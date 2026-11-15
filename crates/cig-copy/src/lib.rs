// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
//! `memcpy` and `memmove` for cig's static build on x86_64.
//!
//! A release of cig for Linux is a static binary on musl, and musl's
//! `memcpy` for x86_64 is the string instruction (`rep movsq`). That
//! instruction costs tens of cycles to start however little it copies, and
//! cig copies little, often: a 24-byte value, a short string, a handful of
//! arguments. On the test machine `memcpy` was 41% of the tree-walker's
//! cycles on `regex-ish` and 24% on `strings`, where the same code linked
//! against glibc spent 9% and 13% in its copy (cig-script#243, step 0).
//!
//! These routines have no start-up cost. A size up to 128 bytes is copied
//! with loads that overlap, every one of them made before the first
//! store, so one piece of code serves `memcpy` and `memmove`. A larger
//! size goes 64 bytes at a time, with the far end loaded first; from 2048
//! bytes a forward copy uses `rep movsb` where the processor says that is
//! fast (ERMS). Nothing here needs more than SSE2, which every x86_64 has.
//!
//! **How a build reaches them.** The root crate's build script passes
//! `--wrap=memcpy` and `--wrap=memmove` to the linker on x86_64 musl, so
//! every reference in the program, in the allocator and in the C library
//! itself goes to `__wrap_memcpy` and `__wrap_memmove` below. A build
//! where the symbols are missing does not link: the routines cannot go
//! missing quietly. Every other target keeps its C library's.
//!
//! **`no_builtins`, and always optimized.** The compiler must not turn a
//! loop in this crate into a call to `memcpy`, nor call it to move a
//! temporary: under the wrap that is a call to this very code. The first is
//! this attribute; the second is the root manifest building this crate
//! optimized in every profile, where a load or a store is an instruction.
#![no_std]
#![no_builtins]

#[cfg(target_arch = "x86_64")]
mod x86_64 {
    use core::arch::x86_64::{__cpuid, __cpuid_count, __m128i, _mm_loadu_si128, _mm_storeu_si128};
    use core::sync::atomic::{AtomicU8, Ordering};

    /// Up to this size every load is made before the first store.
    const SMALL: usize = 128;
    /// From this size a forward copy uses the string instruction, where
    /// it is fast.
    const STRING_FROM: usize = 2048;

    /// Sixteen bytes at `p`, at any alignment.
    ///
    /// # Safety
    /// `p` is valid for a read of 16 bytes.
    #[inline(always)]
    unsafe fn load(p: *const u8) -> __m128i {
        // SAFETY: the caller's contract; the load is unaligned.
        unsafe { _mm_loadu_si128(p.cast()) }
    }

    /// Sixteen bytes to `p`, at any alignment.
    ///
    /// # Safety
    /// `p` is valid for a write of 16 bytes.
    #[inline(always)]
    unsafe fn store(p: *mut u8, v: __m128i) {
        // SAFETY: the caller's contract; the store is unaligned.
        unsafe { _mm_storeu_si128(p.cast(), v) }
    }

    /// `n` bytes from `s` to `d`, `n <= SMALL`. Every load is made before
    /// the first store, so the ranges may overlap in any way.
    ///
    /// # Safety
    /// `s` is valid for a read of `n` bytes and `d` for a write of `n`.
    #[inline(always)]
    unsafe fn small(d: *mut u8, s: *const u8, n: usize) {
        // SAFETY: each branch reads and writes inside `[0, n)`: a pair of
        // blocks of one width, the first at 0 and the last ending at `n`,
        // overlapping where `n` is not twice the width.
        unsafe {
            if n >= 16 {
                if n <= 32 {
                    let (a, b) = (load(s), load(s.add(n - 16)));
                    store(d, a);
                    store(d.add(n - 16), b);
                } else if n <= 64 {
                    let (a, b) = (load(s), load(s.add(16)));
                    let (y, z) = (load(s.add(n - 32)), load(s.add(n - 16)));
                    store(d, a);
                    store(d.add(16), b);
                    store(d.add(n - 32), y);
                    store(d.add(n - 16), z);
                } else {
                    let (a, b) = (load(s), load(s.add(16)));
                    let (c, e) = (load(s.add(32)), load(s.add(48)));
                    let (w, x) = (load(s.add(n - 64)), load(s.add(n - 48)));
                    let (y, z) = (load(s.add(n - 32)), load(s.add(n - 16)));
                    store(d, a);
                    store(d.add(16), b);
                    store(d.add(32), c);
                    store(d.add(48), e);
                    store(d.add(n - 64), w);
                    store(d.add(n - 48), x);
                    store(d.add(n - 32), y);
                    store(d.add(n - 16), z);
                }
            } else if n >= 8 {
                let a = s.cast::<u64>().read_unaligned();
                let b = s.add(n - 8).cast::<u64>().read_unaligned();
                d.cast::<u64>().write_unaligned(a);
                d.add(n - 8).cast::<u64>().write_unaligned(b);
            } else if n >= 4 {
                let a = s.cast::<u32>().read_unaligned();
                let b = s.add(n - 4).cast::<u32>().read_unaligned();
                d.cast::<u32>().write_unaligned(a);
                d.add(n - 4).cast::<u32>().write_unaligned(b);
            } else if n >= 2 {
                let a = s.cast::<u16>().read_unaligned();
                let b = s.add(n - 2).cast::<u16>().read_unaligned();
                d.cast::<u16>().write_unaligned(a);
                d.add(n - 2).cast::<u16>().write_unaligned(b);
            } else if n == 1 {
                d.write(s.read());
            }
        }
    }

    /// Sixty-four bytes from `s` to `d`, loaded whole before any is stored.
    ///
    /// # Safety
    /// `s` is valid for a read of 64 bytes and `d` for a write of 64.
    #[inline(always)]
    unsafe fn block(d: *mut u8, s: *const u8) {
        // SAFETY: the caller's contract.
        unsafe {
            let (a, b) = (load(s), load(s.add(16)));
            let (c, e) = (load(s.add(32)), load(s.add(48)));
            store(d, a);
            store(d.add(16), b);
            store(d.add(32), c);
            store(d.add(48), e);
        }
    }

    /// `n` bytes from `s` to `d`, upward, `n > SMALL`. The ranges do not
    /// overlap, or `d` is below `s`.
    ///
    /// # Safety
    /// `s` is valid for a read of `n` bytes and `d` for a write of `n`.
    #[inline(always)]
    unsafe fn forward(d: *mut u8, s: *const u8, n: usize) {
        if n >= STRING_FROM && string_is_fast() {
            // SAFETY: `rep movsb` copies `rcx` bytes from `[rsi]` to
            // `[rdi]`, upward (the ABI keeps the direction flag clear):
            // inside both ranges by the caller's contract, and right for
            // a destination below its source.
            unsafe {
                core::arch::asm!(
                    "rep movsb",
                    inout("rcx") n => _,
                    inout("rdi") d => _,
                    inout("rsi") s => _,
                    options(nostack, preserves_flags)
                );
            }
            return;
        }
        // SAFETY: the last 64 bytes are loaded before anything is stored
        // and stored last, so a block of the loop may run over them. Each
        // block is loaded whole before it is stored, and with `d` below
        // `s` a store reaches only source bytes already loaded.
        unsafe {
            let end = s.add(n - 64);
            let (w, x) = (load(end), load(end.add(16)));
            let (y, z) = (load(end.add(32)), load(end.add(48)));
            let mut i = 0;
            while i + 64 <= n {
                block(d.add(i), s.add(i));
                i += 64;
            }
            let end = d.add(n - 64);
            store(end, w);
            store(end.add(16), x);
            store(end.add(32), y);
            store(end.add(48), z);
        }
    }

    /// `n` bytes from `s` to `d`, downward, `n > SMALL`: for a `d` that is
    /// above `s` and inside the source.
    ///
    /// # Safety
    /// `s` is valid for a read of `n` bytes and `d` for a write of `n`.
    #[inline(always)]
    unsafe fn backward(d: *mut u8, s: *const u8, n: usize) {
        // SAFETY: the first 64 bytes are loaded before anything is stored
        // and stored last, so the lowest block of the loop may run over
        // them. Each block is loaded whole before it is stored, and with
        // `d` above `s` a store reaches only source bytes already loaded.
        // `i` stays at 64 or over until its last step, so it never wraps.
        unsafe {
            let (a, b) = (load(s), load(s.add(16)));
            let (c, e) = (load(s.add(32)), load(s.add(48)));
            let mut i = n;
            while i >= 64 {
                i -= 64;
                block(d.add(i), s.add(i));
            }
            store(d, a);
            store(d.add(16), b);
            store(d.add(32), c);
            store(d.add(48), e);
        }
    }

    /// 0 not asked yet, 1 no, 2 yes.
    static STRING_FAST: AtomicU8 = AtomicU8::new(0);

    /// Does the processor say `rep movsb` is fast (ERMS: leaf 7, EBX bit
    /// 9)? Asked once. Without it the 64-byte loop copies every size.
    #[inline(always)]
    fn string_is_fast() -> bool {
        match STRING_FAST.load(Ordering::Relaxed) {
            0 => ask_the_processor(),
            answer => answer == 2,
        }
    }

    #[cold]
    #[inline(never)]
    #[allow(unused_unsafe)] // `__cpuid` is safe to call on newer toolchains
    fn ask_the_processor() -> bool {
        // SAFETY: every x86_64 has `cpuid`, and leaf 7 is read only where
        // leaf 0 says it exists.
        let fast = unsafe { __cpuid(0).eax >= 7 && (__cpuid_count(7, 0).ebx & (1 << 9)) != 0 };
        STRING_FAST.store(if fast { 2 } else { 1 }, Ordering::Relaxed);
        fast
    }

    /// Use the string instruction or not, whatever the processor says:
    /// both ways of a large forward copy are tested on every machine.
    #[cfg(test)]
    pub(crate) fn set_string_fast(fast: bool) {
        STRING_FAST.store(if fast { 2 } else { 1 }, Ordering::Relaxed);
    }

    /// `memcpy`: `n` bytes from `src` to `dst`, which do not overlap.
    ///
    /// # Safety
    /// `src` is valid for a read of `n` bytes, `dst` for a write of `n`,
    /// and the two ranges do not overlap.
    #[inline(always)]
    pub unsafe fn copy(dst: *mut u8, src: *const u8, n: usize) {
        // SAFETY: the caller's contract; ranges that do not overlap are
        // right for a copy in either direction.
        unsafe {
            if n <= SMALL {
                small(dst, src, n);
            } else {
                forward(dst, src, n);
            }
        }
    }

    /// `memmove`: `n` bytes from `src` to `dst`, which may overlap.
    ///
    /// # Safety
    /// `src` is valid for a read of `n` bytes and `dst` for a write of `n`.
    #[inline(always)]
    pub unsafe fn copy_overlapping(dst: *mut u8, src: *const u8, n: usize) {
        // SAFETY: the caller's contract. `dst - src`, as an unsigned
        // number, is `n` or more exactly when `dst` is below `src` or at
        // or past its end: the two cases an upward copy is right for.
        unsafe {
            if n <= SMALL {
                small(dst, src, n);
            } else if (dst as usize).wrapping_sub(src as usize) >= n {
                forward(dst, src, n);
            } else {
                backward(dst, src, n);
            }
        }
    }
}

/// `memcpy`'s job: `n` bytes from `src` to `dst`, which do not overlap.
///
/// # Safety
/// `src` is valid for a read of `n` bytes, `dst` for a write of `n`, and
/// the two ranges do not overlap.
#[cfg(target_arch = "x86_64")]
pub unsafe fn copy(dst: *mut u8, src: *const u8, n: usize) {
    // SAFETY: the caller's contract is this function's.
    unsafe { x86_64::copy(dst, src, n) }
}

/// `memmove`'s job: `n` bytes from `src` to `dst`, which may overlap.
///
/// # Safety
/// `src` is valid for a read of `n` bytes and `dst` for a write of `n`.
#[cfg(target_arch = "x86_64")]
pub unsafe fn copy_overlapping(dst: *mut u8, src: *const u8, n: usize) {
    // SAFETY: the caller's contract is this function's.
    unsafe { x86_64::copy_overlapping(dst, src, n) }
}

/// What the linker makes of every `memcpy` in a static x86_64 build
/// (`--wrap=memcpy`, from the root crate's build script).
///
/// # Safety
/// `memcpy`'s contract.
#[cfg(all(target_arch = "x86_64", target_env = "musl"))]
#[no_mangle]
pub unsafe extern "C" fn __wrap_memcpy(
    dst: *mut core::ffi::c_void,
    src: *const core::ffi::c_void,
    n: usize,
) -> *mut core::ffi::c_void {
    // SAFETY: `memcpy`'s contract is `copy`'s.
    unsafe { x86_64::copy(dst.cast(), src.cast(), n) };
    dst
}

/// What the linker makes of every `memmove` in a static x86_64 build
/// (`--wrap=memmove`, from the root crate's build script).
///
/// # Safety
/// `memmove`'s contract.
#[cfg(all(target_arch = "x86_64", target_env = "musl"))]
#[no_mangle]
pub unsafe extern "C" fn __wrap_memmove(
    dst: *mut core::ffi::c_void,
    src: *const core::ffi::c_void,
    n: usize,
) -> *mut core::ffi::c_void {
    // SAFETY: `memmove`'s contract is `copy_overlapping`'s.
    unsafe { x86_64::copy_overlapping(dst.cast(), src.cast(), n) };
    dst
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    extern crate std;
    use super::{copy, copy_overlapping, x86_64::set_string_fast};
    use std::vec::Vec;

    /// Bytes that differ from their neighbors and from the guard.
    fn pattern(n: usize, seed: u8) -> Vec<u8> {
        (0..n)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed) | 1)
            .collect()
    }

    /// Every size the routines branch on, both sides of each branch, and
    /// sizes well past the last.
    fn sizes() -> Vec<usize> {
        let mut v: Vec<usize> = (0..=400).collect();
        v.extend([
            511, 512, 513, 1023, 1024, 1025, 2047, 2048, 2049, 2111, 4095, 4096, 4097, 8191,
            65_535, 65_536, 65_537, 300_001,
        ]);
        v
    }

    const GUARD: usize = 80;

    /// One test at a time sets which way a large forward copy goes.
    static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A copy writes its `n` bytes, the right ones, and not one more: at
    /// every size, at every alignment of the source and of the
    /// destination within 16.
    #[test]
    fn every_size_at_every_alignment_is_copied_exactly() {
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        for fast in [false, true] {
            set_string_fast(fast);
            for n in sizes() {
                // Large sizes at four alignments each; the rest at all 256.
                let step = if n > 2200 { 5 } else { 1 };
                for so in (0..16).step_by(step) {
                    for to in (0..16).step_by(step) {
                        let src = pattern(so + n, n as u8);
                        let mut dst = std::vec![0xA4u8; to + GUARD + n + GUARD];
                        // SAFETY: both ranges are inside their vectors, and
                        // they are two vectors.
                        unsafe { copy(dst.as_mut_ptr().add(to + GUARD), src.as_ptr().add(so), n) };
                        let at = to + GUARD;
                        assert!(
                            dst[at..at + n] == src[so..],
                            "size {n}, source +{so}, to +{to}"
                        );
                        assert!(
                            dst[..at].iter().chain(&dst[at + n..]).all(|b| *b == 0xA4),
                            "size {n}, source +{so}, to +{to}: wrote outside"
                        );
                    }
                }
            }
        }
    }

    /// A move inside one buffer gives what a copy through a second buffer
    /// gives: both directions, every distance from 1 to past the largest
    /// block, and distances at and around the size itself.
    #[test]
    fn overlapping_moves_are_what_a_copy_through_a_second_buffer_gives() {
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        for fast in [false, true] {
            set_string_fast(fast);
            for n in sizes() {
                if n == 0 {
                    continue;
                }
                let mut distances: Vec<usize> = (1..=150).collect();
                distances.extend([n / 2, n - 1, n, n + 1, n + 63, n + 64, n + 65]);
                distances.retain(|d| *d >= 1);
                // A large size is moved at fewer distances.
                let step = if n > 2200 { 13 } else { 1 };
                for dist in distances.into_iter().step_by(step) {
                    for up in [false, true] {
                        let len = GUARD + dist + n + GUARD;
                        let original = pattern(len, dist as u8);
                        // Up: from `GUARD` to `GUARD + dist`. Down: the other way.
                        let (from, to) = if up {
                            (GUARD, GUARD + dist)
                        } else {
                            (GUARD + dist, GUARD)
                        };
                        let mut want = original.clone();
                        want[to..to + n].copy_from_slice(&original[from..from + n]);
                        let mut got = original.clone();
                        // SAFETY: both ranges are inside the one vector.
                        unsafe {
                            let p = got.as_mut_ptr();
                            copy_overlapping(p.add(to), p.add(from), n);
                        }
                        assert!(got == want, "size {n}, distance {dist}, up {up}");
                    }
                }
            }
        }
    }

    /// A move onto itself changes nothing.
    #[test]
    fn a_move_onto_itself_changes_nothing() {
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        for n in sizes() {
            let original = pattern(n + 8, 3);
            let mut got = original.clone();
            // SAFETY: the range is inside the vector.
            unsafe {
                let p = got.as_mut_ptr().add(4);
                copy_overlapping(p, p, n);
            }
            assert!(got == original, "size {n}");
        }
    }
}
