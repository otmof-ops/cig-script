// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

// musl's allocator is slow under an interpreter's allocation pattern; musl
// builds (the released Linux binaries) use mimalloc instead (#146).
#[cfg(all(target_env = "musl", not(feature = "bench-stats")))]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

// `--features bench-stats` wraps whichever allocator the platform uses in one
// that counts, for `cig bench --stats` (#179). Off in releases.
#[cfg(all(target_env = "musl", feature = "bench-stats"))]
#[global_allocator]
static ALLOCATOR: cigscript::benchstats::Counting<mimalloc::MiMalloc> =
    cigscript::benchstats::Counting(mimalloc::MiMalloc);
#[cfg(all(not(target_env = "musl"), feature = "bench-stats"))]
#[global_allocator]
static ALLOCATOR: cigscript::benchstats::Counting<std::alloc::System> =
    cigscript::benchstats::Counting(std::alloc::System);

// The heap's first megabytes in small pages (cig-compile#30; carton 0.2.0
// rows 9 and 10). Left alone, mimalloc 3 reserves a 1 GiB arena at the
// first allocation and asks for all of it to be backed with 2 MiB pages. A
// short program touches a few hundred KB spread over its first 6 MB, so on a
// kernel whose transparent huge pages are `madvise` (Ubuntu) or `always`
// (Debian, Arch) it zeroed and kept four 2 MiB pages: a compiled hello went
// from 5.2 MB to 13.9 MB at its peak and started about 0.7 ms later. A long
// one gains from them (json runs 24% faster than in 4 KiB pages).
//
// Before `main` and the first allocation this maps the 1 GiB arena itself,
// marks its first 8 MiB `MADV_NOHUGEPAGE` and the rest `MADV_HUGEPAGE`, and
// only then gives it to mimalloc, so nothing in the head is ever touched as a
// huge page, even on an `always` kernel. A script that outgrows it gets
// mimalloc's next arena as before. mimalloc's page map is mapped and first
// touched while transparent huge pages are off for the process
// (`PR_SET_THP_DISABLE`, from the first constructor to the end of this one);
// the switch is put back before `main`, so nothing passes to a hop's
// processes. Only musl builds link mimalloc; other targets are unchanged.
// A 32-bit build keeps mimalloc's own arena (a gigabyte is a third of its
// address space), and so does a kernel that refuses to overcommit.
#[cfg(all(target_env = "musl", target_pointer_width = "64"))]
mod heap {
    use std::ffi::{c_int, c_void};
    use std::sync::atomic::{AtomicBool, Ordering};

    extern "C" {
        fn mi_manage_os_memory_ex(
            start: *mut c_void,
            size: usize,
            is_committed: bool,
            is_pinned: bool,
            is_zero: bool,
            numa_node: c_int,
            exclusive: bool,
            arena_id: *mut *mut c_void,
        ) -> bool;
        fn mi_malloc(size: usize) -> *mut c_void;
        fn mi_free(p: *mut c_void);
    }

    /// The arena's address space: reserved, nothing resident until touched.
    pub const ARENA_BYTES: usize = 1 << 30;
    /// Its head in 4 KiB pages: a hello's heap is in its first 6 MiB.
    pub const SMALL_HEAD_BYTES: usize = 8 << 20;
    const HUGE: usize = 2 << 20;

    /// Whether `before_mimalloc` turned transparent huge pages off, so
    /// `small_head` turns them back on (and never one the parent set).
    static THP_OFF_HERE: AtomicBool = AtomicBool::new(false);

    extern "C" fn before_mimalloc() {
        // SAFETY: prctl with documented options and no pointers.
        unsafe {
            if libc::prctl(libc::PR_GET_THP_DISABLE, 0, 0, 0, 0) == 0
                && libc::prctl(libc::PR_SET_THP_DISABLE, 1, 0, 0, 0) == 0
            {
                THP_OFF_HERE.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Does the kernel refuse to overcommit (`vm.overcommit_memory = 2`)?
    /// There a writable mapping is charged in full when it is made, so a
    /// 1 GiB arena would be charged to every process whether or not it is
    /// touched. mimalloc commits as the heap grows on such a kernel: the
    /// heap is left to it. Read with `open` and `read` on the stack, since
    /// nothing may allocate before the arena is handed over.
    fn strict_overcommit() -> bool {
        // SAFETY: a read of a few bytes of a /proc file into a stack buffer.
        unsafe {
            let fd = libc::open(
                c"/proc/sys/vm/overcommit_memory".as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC,
            );
            if fd < 0 {
                return false;
            }
            let mut b = [0u8; 4];
            let n = libc::read(fd, b.as_mut_ptr().cast(), b.len());
            libc::close(fd);
            n > 0 && b[0] == b'2'
        }
    }

    extern "C" fn small_head() {
        // SAFETY: an anonymous private mapping of our own, trimmed to a
        // 2 MiB-aligned 1 GiB; `madvise` only changes how it is backed;
        // mimalloc is handed memory nothing else uses, mapped readable and
        // writable and zero, and keeps it for the life of the process. The
        // allocation and free that follow are mimalloc's own pair.
        unsafe {
            let len = ARENA_BYTES + HUGE;
            let raw = if strict_overcommit() {
                libc::MAP_FAILED
            } else {
                libc::mmap(
                    std::ptr::null_mut(),
                    len,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                    -1,
                    0,
                )
            };
            if raw != libc::MAP_FAILED {
                let lead = (HUGE - raw as usize % HUGE) % HUGE;
                if lead > 0 {
                    libc::munmap(raw, lead);
                }
                let start = raw.cast::<u8>().add(lead);
                let tail = HUGE - lead;
                if tail > 0 {
                    libc::munmap(start.add(ARENA_BYTES).cast(), tail);
                }
                libc::madvise(start.cast(), SMALL_HEAD_BYTES, libc::MADV_NOHUGEPAGE);
                libc::madvise(
                    start.add(SMALL_HEAD_BYTES).cast(),
                    ARENA_BYTES - SMALL_HEAD_BYTES,
                    libc::MADV_HUGEPAGE,
                );
                let mut id: *mut c_void = std::ptr::null_mut();
                if !mi_manage_os_memory_ex(
                    start.cast(),
                    ARENA_BYTES,
                    true,
                    false,
                    true,
                    -1,
                    false,
                    &mut id,
                ) {
                    libc::munmap(start.cast(), ARENA_BYTES);
                }
            }
            // The page map's first touch, while huge pages are still off.
            mi_free(mi_malloc(16));
            if THP_OFF_HERE.load(Ordering::Relaxed) {
                libc::prctl(libc::PR_SET_THP_DISABLE, 0, 0, 0, 0);
            }
        }
    }

    /// First: before mimalloc's own constructor (priority 101) maps the page
    /// map.
    #[used]
    #[link_section = ".init_array.00100"]
    static BEFORE_MIMALLOC: extern "C" fn() = before_mimalloc;

    /// Then, after it and before `main` and the first allocation.
    #[used]
    #[link_section = ".init_array"]
    static SMALL_HEAD: extern "C" fn() = small_head;
}

fn main() {
    std::process::exit(cigscript::cli::entry());
}
