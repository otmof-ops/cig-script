// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! A section's code, mapped from the file it lies in, and the call into
//! it.
//!
//! Three steps, and the middle one is the caller's word:
//!
//! 1. [`Mapped::map`] maps a section's code read and execute, private.
//!    It is safe: nothing can read the mapping or call into it.
//! 2. [`Mapped::arm`] is `unsafe`: the caller vouches that the bytes are
//!    the code generator's for this machine, and that the file will not
//!    change while the code can be called. It checks what can be
//!    checked: that the mapping holds exactly the bytes the caller read
//!    and verified, and that every entry lies in them.
//! 3. [`Code::call`] is safe: it calls an entry by its index, with as
//!    many argument slots as that entry takes, or does nothing.
//!
//! The mapping is never writable, never anonymous and never changed
//! with `mprotect`: no page is ever both writable and executable. Only
//! the code is mapped; the tables and the footer are read as data.

use crate::format::{Arch, Tables, MAX_ARGS};
use crate::Context;

/// Why there is no mapping, or no [`Code`].
#[derive(Debug)]
pub enum MapError {
    /// The offset is not a multiple of the system's page size.
    NotPageAligned {
        /// The offset asked for.
        offset: u64,
        /// The page size.
        page: u64,
    },
    /// Nothing to map: the length is zero.
    Empty,
    /// The range does not lie inside the file.
    PastEnd {
        /// The offset asked for.
        offset: u64,
        /// The length asked for.
        len: u64,
        /// The file's length.
        file_len: u64,
    },
    /// This target has no loader: Linux on x86-64 and aarch64 have one.
    Unsupported,
    /// The system refused: the file's length could not be read, or
    /// `mmap` failed (a file system mounted `noexec` gives `EPERM`).
    Refused(std::io::Error),
    /// What [`Mapped::arm`] was given does not fit the mapping or this
    /// machine.
    Mismatch(&'static str),
}

impl core::fmt::Display for MapError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            MapError::NotPageAligned { offset, page } => {
                write!(
                    f,
                    "offset {offset} is not a multiple of the page size, {page}"
                )
            }
            MapError::Empty => write!(f, "there is no code to map"),
            MapError::PastEnd {
                offset,
                len,
                file_len,
            } => write!(
                f,
                "{len} bytes at offset {offset} do not lie inside the file's {file_len}"
            ),
            MapError::Unsupported => write!(f, "this target cannot load native code"),
            MapError::Refused(e) => write!(f, "the system refused the mapping: {e}"),
            MapError::Mismatch(what) => write!(f, "{what}"),
        }
    }
}

impl std::error::Error for MapError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            MapError::Refused(e) => Some(e),
            _ => None,
        }
    }
}

/// A section's code mapped read and execute. Nothing reads it and
/// nothing calls it until [`Mapped::arm`].
///
/// It holds a raw pointer, so it is neither `Send` nor `Sync`: the VM
/// that owns one runs on one thread, and nothing here was made to be
/// shared.
pub struct Mapped {
    inner: sys::Inner,
}

impl Mapped {
    /// Map `len` bytes of `file` from `offset`, read and execute,
    /// private. `offset` is a multiple of the page size, `len` is not
    /// zero, and the range lies inside the file: each is checked before
    /// the system is asked. On a target without a loader this is
    /// [`MapError::Unsupported`] and the file is not touched.
    ///
    /// This is safe because it gives nothing back but the mapping's
    /// length: no reference into the mapped bytes, and no way to run
    /// them.
    pub fn map(file: &std::fs::File, offset: u64, len: usize) -> Result<Mapped, MapError> {
        sys::Inner::map(file, offset, len).map(|inner| Mapped { inner })
    }

    /// The mapping's length in bytes.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// A mapping is never empty.
    pub fn is_empty(&self) -> bool {
        self.inner.len() == 0
    }

    /// Make the mapping callable: `code` is the section's code as the
    /// caller read and verified it, `tables` its tables.
    ///
    /// Checked here, each a [`MapError::Mismatch`] that drops the
    /// mapping: the tables are for this machine; `code` is as long as
    /// the tables say and as the mapping is; the mapping holds exactly
    /// `code`'s bytes; every entry has at most [`MAX_ARGS`] arguments
    /// and an offset inside the code, on an instruction boundary.
    ///
    /// # Safety
    ///
    /// The caller vouches for two things nothing here can check.
    ///
    /// **The file does not change under the mapping.** From the moment
    /// the caller read `code` from the file until the returned [`Code`]
    /// is dropped, the mapped range of the file is not written and the
    /// file is not shortened. A program's own executable is such a file
    /// on Linux: the system refuses to open a running executable for
    /// writing.
    ///
    /// **The bytes are the code generator's.** `code` and `tables` are
    /// the code and the tables of one native section made by cig's code
    /// generator for this machine. So each entry's offset is the first
    /// instruction of a function that follows the entry convention,
    /// `extern "C" fn(*mut Context, *const u64, *mut u64) -> u32`; reads
    /// exactly as many argument slots as the entry has arguments; writes
    /// at most the one result slot; touches no other memory than the
    /// context, those slots and its own stack, of which one call uses at
    /// most [`MAX_FRAME`](crate::MAX_FRAME) bytes and calls nest no
    /// deeper than the context's `depth_left`; and returns.
    ///
    /// The caller's evidence is where the bytes came from: a section of
    /// the program's own file, whose sha256 the program's section table
    /// gives and whose footer names the bytecode module it was compiled
    /// from.
    pub unsafe fn arm(self, code: &[u8], tables: &Tables) -> Result<Code, MapError> {
        if Arch::host() != Some(tables.arch) {
            return Err(MapError::Mismatch("the code is for another machine"));
        }
        if code.len() != tables.code_len as usize {
            return Err(MapError::Mismatch(
                "the code is not as long as its tables say",
            ));
        }
        if code.len() != self.inner.len() {
            return Err(MapError::Mismatch("the mapping is not as long as the code"));
        }
        // SAFETY: the caller vouched that the file does not change under
        // the mapping, which is what reading it needs.
        if !unsafe { self.inner.holds(code) } {
            return Err(MapError::Mismatch(
                "the mapping does not hold the code that was read",
            ));
        }
        let align = tables.arch.code_align();
        let mut entries = Vec::with_capacity(tables.entries.len());
        for e in &tables.entries {
            let Ok(args) = u8::try_from(e.args.len()) else {
                return Err(MapError::Mismatch("an entry has too many arguments"));
            };
            if usize::from(args) > MAX_ARGS {
                return Err(MapError::Mismatch("an entry has too many arguments"));
            }
            if e.offset >= tables.code_len {
                return Err(MapError::Mismatch("an entry lies outside the code"));
            }
            if e.offset % align != 0 {
                return Err(MapError::Mismatch(
                    "an entry is not on an instruction boundary",
                ));
            }
            entries.push(Armed {
                offset: e.offset,
                args,
            });
        }
        Ok(Code {
            inner: self.inner,
            entries,
        })
    }
}

/// What a call needs of an entry.
#[derive(Clone, Copy)]
struct Armed {
    offset: u32,
    args: u8,
}

/// A section's code, callable: a mapping its caller vouched for
/// ([`Mapped::arm`]).
pub struct Code {
    inner: sys::Inner,
    entries: Vec<Armed>,
}

impl Code {
    /// How many entries it has.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// It has no entry.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Call entry `entry` (its index in the tables' entries) with
    /// `args`, one 64-bit slot an argument: the status it returned and
    /// its result slot. `None`, having run nothing, when there is no
    /// such entry or `args` is not as many slots as it takes.
    ///
    /// `ctx.depth_left` is how deep the code's own calls may nest: the
    /// caller sets it to what its stack has room for, at
    /// [`MAX_FRAME`](crate::MAX_FRAME) bytes a call.
    pub fn call(&self, entry: usize, ctx: &mut Context, args: &[u64]) -> Option<(u32, u64)> {
        let e = *self.entries.get(entry)?;
        if args.len() != usize::from(e.args) {
            return None;
        }
        // SAFETY: `e.offset` is an entry of the tables this mapping was
        // armed with, so `arm`'s caller vouched that it starts a
        // function of the entry convention that reads `e.args` argument
        // slots, and `args` holds exactly that many.
        Some(unsafe { self.inner.call(e.offset, ctx, args) })
    }
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod sys {
    //! Linux on x86-64 and aarch64: `mmap`, the comparison, the call
    //! and `munmap`. Every `unsafe` block of the crate is here.

    use super::MapError;
    use crate::Context;
    use std::os::unix::io::AsRawFd;

    /// An entry, in the target's C convention.
    type Entry = unsafe extern "C" fn(*mut Context, *const u64, *mut u64) -> u32;

    /// A mapping: `len` bytes at `ptr`, read and execute, until it is
    /// dropped.
    pub(super) struct Inner {
        ptr: *mut libc::c_void,
        len: usize,
    }

    impl Inner {
        pub(super) fn map(
            file: &std::fs::File,
            offset: u64,
            len: usize,
        ) -> Result<Inner, MapError> {
            if len == 0 {
                return Err(MapError::Empty);
            }
            // SAFETY: `sysconf` only reads a constant of the system.
            let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
            // A system that does not say is taken at the smallest page
            // there is: `mmap` itself refuses an offset its real page
            // size does not divide.
            let page = u64::try_from(page).ok().filter(|p| *p > 0).unwrap_or(4096);
            if offset % page != 0 {
                return Err(MapError::NotPageAligned { offset, page });
            }
            let file_len = file.metadata().map_err(MapError::Refused)?.len();
            let past = || MapError::PastEnd {
                offset,
                len: len as u64,
                file_len,
            };
            match offset.checked_add(len as u64) {
                Some(end) if end <= file_len => {}
                _ => return Err(past()),
            }
            let start = libc::off_t::try_from(offset).map_err(|_| past())?;
            // SAFETY: a new mapping at an address the system picks
            // (`addr` null), of `len` (not zero) bytes of an open file
            // from an offset inside it; read and execute, never
            // writable; private. The descriptor is `file`'s, open for
            // the length of the call, and the mapping does not need it
            // afterwards. Failure is `MAP_FAILED`, tested before the
            // pointer is used.
            let ptr = unsafe {
                libc::mmap(
                    core::ptr::null_mut(),
                    len,
                    libc::PROT_READ | libc::PROT_EXEC,
                    libc::MAP_PRIVATE,
                    file.as_raw_fd(),
                    start,
                )
            };
            if ptr == libc::MAP_FAILED {
                return Err(MapError::Refused(std::io::Error::last_os_error()));
            }
            Ok(Inner { ptr, len })
        }

        pub(super) fn len(&self) -> usize {
            self.len
        }

        /// The mapping holds exactly `bytes`.
        ///
        /// # Safety
        ///
        /// The file is not written or shortened under the mapping while
        /// this runs.
        pub(super) unsafe fn holds(&self, bytes: &[u8]) -> bool {
            // SAFETY: `ptr` is the start of `len` mapped, readable
            // bytes, alive as long as `self`; the caller vouched that
            // they do not change while they are read.
            let mapped = unsafe { core::slice::from_raw_parts(self.ptr.cast::<u8>(), self.len) };
            mapped == bytes
        }

        /// Call the function at `offset`.
        ///
        /// # Safety
        ///
        /// `offset` is inside the mapping and is the first instruction
        /// of a function of the entry convention that reads
        /// `args.len()` argument slots, as [`super::Mapped::arm`]'s
        /// caller vouched; and the file has not changed under the
        /// mapping.
        pub(super) unsafe fn call(
            &self,
            offset: u32,
            ctx: &mut Context,
            args: &[u64],
        ) -> (u32, u64) {
            let mut result = 0u64;
            // SAFETY: `offset` is inside the mapping, so the address
            // is; a function pointer is an address, and the caller
            // vouched that a function of this signature starts there.
            let entry: Entry = unsafe {
                core::mem::transmute::<*mut u8, Entry>(self.ptr.cast::<u8>().add(offset as usize))
            };
            // SAFETY: the entry convention's own terms. `ctx` is one
            // context, exclusively borrowed for the call; `args` points
            // to as many slots as the function reads (when it reads
            // none, the pointer is not used); `result` is one slot.
            let status = unsafe { entry(ctx, args.as_ptr(), &mut result) };
            (status, result)
        }
    }

    impl Drop for Inner {
        fn drop(&mut self) {
            // SAFETY: `ptr` and `len` are one mapping `mmap` made, owned
            // by `self` alone, and nothing borrows from it: no reference
            // into it outlives the call that made one. A failure leaves
            // the mapping in place, which harms nothing.
            unsafe {
                libc::munmap(self.ptr, self.len);
            }
        }
    }
}

#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
mod sys {
    //! Every other target: there is no mapping, so there is nothing to
    //! compare or call.

    use super::MapError;
    use crate::Context;

    /// No value of it exists: `map` never makes one.
    pub(super) enum Inner {}

    impl Inner {
        pub(super) fn map(
            _file: &std::fs::File,
            _offset: u64,
            _len: usize,
        ) -> Result<Inner, MapError> {
            Err(MapError::Unsupported)
        }

        pub(super) fn len(&self) -> usize {
            match *self {}
        }

        pub(super) unsafe fn holds(&self, _bytes: &[u8]) -> bool {
            match *self {}
        }

        pub(super) unsafe fn call(
            &self,
            _offset: u32,
            _ctx: &mut Context,
            _args: &[u64],
        ) -> (u32, u64) {
            match *self {}
        }
    }
}

#[cfg(test)]
mod tests;
