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

/// How a body, or [`Host::run`], came back: the low 32 bits are a pc,
/// and the bits from 32 up say what happened there.
///
/// | bits 32.. | name          | meaning                                                                                                  |
/// |---:|---|---|
/// | 0 | [`Stop::At`]     | the bytecode goes on at this pc. The frame is whole and `steps_left` and `nest` in the context are current |
/// | 1 | [`Stop::Raised`] | a helper raised while the frame's pc was this one. The host holds the error                              |
/// | 2 | [`Stop::Defect`] | a register was not of its proven type when native code read it from the frame at this pc: a compiler bug |
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stop {
    /// The bytecode goes on at this pc.
    At(u32),
    /// A helper raised while the frame's pc was this one.
    Raised(u32),
    /// A register was not of its proven type when native code read it
    /// from the frame at this pc: a defect in the compiler.
    Defect(u32),
}

impl Stop {
    /// The stop's bits: the pc in the low 32, the kind in bits 32 and
    /// up.
    pub const fn bits(self) -> u64 {
        match self {
            Stop::At(pc) => pc as u64,
            Stop::Raised(pc) => pc as u64 | (1u64 << 32),
            Stop::Defect(pc) => pc as u64 | (2u64 << 32),
        }
    }

    /// The stop a `u64` names, if it names one: [`None`] when bits 32
    /// and up are not 0, 1 or 2.
    pub const fn from_bits(bits: u64) -> Option<Stop> {
        let pc = bits as u32;
        match bits >> 32 {
            0 => Some(Stop::At(pc)),
            1 => Some(Stop::Raised(pc)),
            2 => Some(Stop::Defect(pc)),
            _ => None,
        }
    }
}

/// The runtime's functions a body calls, through the context. Interface
/// 2 has three.
///
/// Each field's offset in this table is part of the interface: a body
/// loads a function pointer by its offset here and calls through it in
/// the target's C convention.
#[repr(C)]
pub struct Helpers {
    /// Offset 0. `run(ctx, from, to)`: run the frame's bytecode from
    /// pc `from` while the pc stays in `from..to`. Returns a stop's
    /// bits: [`Stop::At`] with the pc it stands at now (outside the
    /// range, or inside it at an instruction `run` does not run), or
    /// [`Stop::Raised`]. A call in the range that the runtime can
    /// carry is made and its callee run to its return before `run`
    /// goes on; at one it cannot carry, and at what ends the frame (a
    /// return, a tail call), `run` stands at the instruction.
    pub run: unsafe extern "C" fn(ctx: *mut Context, from: u32, to: u32) -> u64,
    /// Offset 8. `list(ctx, reg)`: 1 when register `reg` holds a list
    /// whose elements native code may read and write where they lie
    /// until its next call of a helper, with `ctx.left` set to the
    /// address of the first and `ctx.right` to how many there are; 0
    /// otherwise, with nothing written.
    pub list: unsafe extern "C" fn(ctx: *mut Context, reg: u32) -> u32,
    /// Offset 16. `again(ctx, pc)`: the instruction at `pc` has machine
    /// code in the body for some operands, and these are not among
    /// them: run that one instruction. Returns a stop's bits:
    /// [`Stop::At`] with the pc the bytecode goes on at, or
    /// [`Stop::Raised`]. When the stop is [`Stop::At`]`(pc)` itself the
    /// runtime ran nothing and takes the frame back: the body returns
    /// that stop as it is. The runtime counts these calls, which is how
    /// it finds a body whose guesses keep failing.
    pub again: unsafe extern "C" fn(ctx: *mut Context, pc: u32) -> u64,
}

// The layout the body is compiled against.
const _: () = {
    assert!(core::mem::size_of::<Helpers>() == 24);
    assert!(core::mem::align_of::<Helpers>() == core::mem::align_of::<usize>());
    assert!(core::mem::offset_of!(Helpers, run) == 0);
    assert!(core::mem::offset_of!(Helpers, list) == 8);
    assert!(core::mem::offset_of!(Helpers, again) == 16);
};

/// What the helpers are on the runtime's side: safe code the crate's
/// `extern "C"` glue calls through [`Code::enter`]. A panic in any
/// method is caught there, the body is told [`Stop::Raised`] and
/// [`Code::enter`] resumes the panic once the body has returned.
pub trait Host {
    /// Run the frame's bytecode from `from` while the pc stays in
    /// `from..to`. Returns the stop the body sees. Never
    /// [`Stop::Defect`]. A host may carry a call through to its return
    /// here, on frames above the body's; when it comes back from one,
    /// the body's frame is on top again and whole.
    fn run(&mut self, ctx: &mut Context, from: u32, to: u32) -> Stop;
    /// Give native code a view on register `reg`'s list, when it holds
    /// one: set `ctx.left` to the address of its first element and
    /// `ctx.right` to how many there are, and return `true`. Return
    /// `false` with nothing written otherwise.
    fn list(&mut self, ctx: &mut Context, reg: u32) -> bool;
    /// Run the one instruction at `pc`, whose machine code in the body
    /// did not cover the operands it met. Returns the stop the body
    /// sees: [`Stop::At`] with the pc the bytecode goes on at, or with
    /// `pc` itself when the host ran nothing and takes the frame back.
    /// Never [`Stop::Defect`].
    fn again(&mut self, ctx: &mut Context, pc: u32) -> Stop;
}

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
    /// and an offset inside the code, on an instruction boundary; every
    /// body's offset is inside the code, on an instruction boundary.
    ///
    /// # Safety
    ///
    /// The caller vouches for two things nothing here can check.
    ///
    /// **The file does not change under the mapping.** From the moment
    /// the caller read `code` from the file until the returned [`Code`]
    /// is dropped, the mapped range of the file is not written and the
    /// file is not shortened. The image a process is running is such a
    /// file to exactly the extent the process's own code is: the system
    /// maps that code from the same file in the same way, and Linux
    /// refuses to open a running executable for writing.
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
        let mut bodies = Vec::with_capacity(tables.bodies.len());
        for b in &tables.bodies {
            if b.offset >= tables.code_len {
                return Err(MapError::Mismatch("a body lies outside the code"));
            }
            if b.offset % align != 0 {
                return Err(MapError::Mismatch(
                    "a body is not on an instruction boundary",
                ));
            }
            bodies.push(ArmedBody {
                offset: b.offset,
                resumes: u32::try_from(b.resumes.len()).unwrap_or(u32::MAX),
            });
        }
        Ok(Code {
            inner: self.inner,
            entries,
            bodies,
        })
    }
}

/// What a call needs of an entry.
#[derive(Clone, Copy)]
struct Armed {
    offset: u32,
    args: u8,
}

/// What `enter` needs of a body: where its code starts and how many
/// resume points it has.
#[derive(Clone, Copy)]
struct ArmedBody {
    offset: u32,
    resumes: u32,
}

/// A section's code, callable: a mapping its caller vouched for
/// ([`Mapped::arm`]).
pub struct Code {
    inner: sys::Inner,
    entries: Vec<Armed>,
    bodies: Vec<ArmedBody>,
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

    /// How many bodies it has.
    pub fn bodies(&self) -> usize {
        self.bodies.len()
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

    /// Enter body `body` (its index in the tables) at its resume point
    /// `at` (that point's index in the body's `resumes`), with `host`
    /// behind the helpers. [`None`], with
    /// nothing run, when there is no such body or that body has no such
    /// resume point. Sets `ctx.helpers` and `ctx.host`; the caller sets
    /// every other field.
    ///
    /// A panic in a method of `host` is caught, the body is told
    /// [`Stop::Raised`] and the panic is resumed once the body has
    /// returned. A stop whose bits are not one of the three kinds comes
    /// back as [`Stop::Defect`]`(0)`. On a target without a loader this
    /// is always [`None`].
    pub fn enter<H: Host>(
        &self,
        body: usize,
        at: u32,
        ctx: &mut Context,
        host: &mut H,
    ) -> Option<Stop> {
        let b = self.bodies.get(body)?;
        if at >= b.resumes {
            return None;
        }
        let helpers = Helpers {
            run: sys::host_run_c::<H>,
            list: sys::host_list_c::<H>,
            again: sys::host_again_c::<H>,
        };
        ctx.helpers = &helpers as *const Helpers;
        ctx.host = host as *mut H as *mut core::ffi::c_void;
        // SAFETY: `b.offset` is a body of the tables this mapping was
        // armed with, so `arm`'s caller vouched that it starts a body
        // of the signature `BodyFn`. The helpers struct lives on this
        // stack frame until `enter_at` returns, and `ctx.host` points
        // to `host`, which the caller borrows exclusively for the
        // length of this call.
        let bits = unsafe { self.inner.enter_at(b.offset, ctx, at) };
        // Whatever the body returned, clear the pointers so that the
        // next call of `enter` does not see stale ones.
        ctx.helpers = core::ptr::null();
        ctx.host = core::ptr::null_mut();
        // Resume the panic a helper caught, if any.
        if let Some(payload) = sys::take_panic() {
            std::panic::resume_unwind(payload);
        }
        Some(Stop::from_bits(bits).unwrap_or(Stop::Defect(0)))
    }
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod sys {
    //! Linux on x86-64 and aarch64: `mmap`, the comparison, the call,
    //! `munmap`, and the two `extern "C"` glue functions between a body
    //! and a [`super::Host`]. Every `unsafe` block of the crate but one
    //! (the `value::agrees` helper) is here.

    use super::MapError;
    use super::{Host, Stop};
    use crate::Context;
    use std::any::Any;
    use std::cell::RefCell;
    use std::os::unix::io::AsRawFd;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    /// An entry, in the target's C convention.
    type Entry = unsafe extern "C" fn(*mut Context, *const u64, *mut u64) -> u32;

    /// A body, in the target's C convention: entered at resume index
    /// `at` of its function, returns a stop's bits.
    type BodyFn = unsafe extern "C" fn(*mut Context, u32) -> u64;

    thread_local! {
        /// Where a helper's panic is kept for [`super::Code::enter`] to
        /// resume once the body has returned. One payload at a time:
        /// a second panic is dropped (the body still sees it as
        /// [`Stop::Raised`]).
        static CAUGHT: RefCell<Option<Box<dyn Any + Send + 'static>>> =
            const { RefCell::new(None) };
    }

    /// Put `payload` aside so [`super::Code::enter`] resumes it after
    /// the body returns.
    fn store_panic(payload: Box<dyn Any + Send + 'static>) {
        CAUGHT.with(|cell| {
            let mut slot = cell.borrow_mut();
            // Keep the first; a second panic reaches [`super::Code::enter`]
            // through the first one's resume.
            if slot.is_none() {
                *slot = Some(payload);
            }
        });
    }

    /// Take the panic payload a helper left behind, if any.
    pub(super) fn take_panic() -> Option<Box<dyn Any + Send + 'static>> {
        CAUGHT.with(|cell| cell.borrow_mut().take())
    }

    /// `extern "C"` glue for `Host::run`. Reads `ctx.host` as `&mut H`,
    /// catches a panic and returns [`Stop::Raised`]`(from)` with the
    /// panic stashed for [`super::Code::enter`] to resume.
    pub(super) unsafe extern "C" fn host_run_c<H: Host>(
        ctx: *mut Context,
        from: u32,
        to: u32,
    ) -> u64 {
        // SAFETY: `enter`'s caller borrows `host` exclusively for the
        // length of the body's run, and the body has not returned yet,
        // so `ctx.host` is a valid `*mut H` to a live `H` (set by
        // `enter` before the body was entered). `ctx` itself is the
        // same exclusive borrow.
        let host_ptr = unsafe { (*ctx).host as *mut H };
        let ctx_ptr = ctx;
        let result = catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: `host_ptr` is a valid `&mut H` for the length of
            // the call, as above; `ctx_ptr` is `&mut Context`.
            let host = unsafe { &mut *host_ptr };
            let cx = unsafe { &mut *ctx_ptr };
            host.run(cx, from, to)
        }));
        match result {
            Ok(stop) => match stop {
                Stop::Defect(_) => Stop::Defect(0).bits(),
                other => other.bits(),
            },
            Err(payload) => {
                store_panic(payload);
                Stop::Raised(from).bits()
            }
        }
    }

    /// `extern "C"` glue for `Host::list`. On panic, `0` with the
    /// panic stashed.
    pub(super) unsafe extern "C" fn host_list_c<H: Host>(ctx: *mut Context, reg: u32) -> u32 {
        // SAFETY: as in `host_run_c`.
        let host_ptr = unsafe { (*ctx).host as *mut H };
        let ctx_ptr = ctx;
        let result = catch_unwind(AssertUnwindSafe(|| {
            let host = unsafe { &mut *host_ptr };
            let cx = unsafe { &mut *ctx_ptr };
            host.list(cx, reg)
        }));
        match result {
            Ok(true) => 1,
            Ok(false) => 0,
            Err(payload) => {
                store_panic(payload);
                0
            }
        }
    }

    /// `extern "C"` glue for `Host::again`: as [`host_run_c`], and a
    /// panic comes back as [`Stop::Raised`]`(pc)`.
    pub(super) unsafe extern "C" fn host_again_c<H: Host>(ctx: *mut Context, pc: u32) -> u64 {
        // SAFETY: as in `host_run_c`.
        let host_ptr = unsafe { (*ctx).host as *mut H };
        let ctx_ptr = ctx;
        let result = catch_unwind(AssertUnwindSafe(|| {
            let host = unsafe { &mut *host_ptr };
            let cx = unsafe { &mut *ctx_ptr };
            host.again(cx, pc)
        }));
        match result {
            Ok(stop) => match stop {
                Stop::Defect(_) => Stop::Defect(0).bits(),
                other => other.bits(),
            },
            Err(payload) => {
                store_panic(payload);
                Stop::Raised(pc).bits()
            }
        }
    }

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

        /// Enter the body at `offset` at resume index `at`.
        ///
        /// # Safety
        ///
        /// `offset` is inside the mapping and is the first instruction
        /// of a body of signature `BodyFn`, as [`super::Mapped::arm`]'s
        /// caller vouched; `ctx` is borrowed exclusively for the call
        /// and has `helpers` and `host` set; and the file has not
        /// changed under the mapping.
        pub(super) unsafe fn enter_at(&self, offset: u32, ctx: &mut Context, at: u32) -> u64 {
            // SAFETY: `offset` is inside the mapping, and a function
            // of the body signature starts there, as above.
            let body: BodyFn = unsafe {
                core::mem::transmute::<*mut u8, BodyFn>(self.ptr.cast::<u8>().add(offset as usize))
            };
            // SAFETY: the body signature's own terms.
            unsafe { body(ctx, at) }
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

    use super::{Host, MapError};
    use crate::Context;
    use std::any::Any;

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

        pub(super) unsafe fn enter_at(&self, _offset: u32, _ctx: &mut Context, _at: u32) -> u64 {
            match *self {}
        }
    }

    pub(super) fn take_panic() -> Option<Box<dyn Any + Send + 'static>> {
        None
    }

    /// Only ever read as a function pointer on a target that has a
    /// loader; its address stays, so this one is never called. The
    /// type parameter is kept so the signature matches the real glue's.
    #[allow(clippy::extra_unused_type_parameters)]
    pub(super) unsafe extern "C" fn host_run_c<H: Host>(
        _ctx: *mut Context,
        _from: u32,
        _to: u32,
    ) -> u64 {
        0
    }

    #[allow(clippy::extra_unused_type_parameters)]
    pub(super) unsafe extern "C" fn host_list_c<H: Host>(_ctx: *mut Context, _reg: u32) -> u32 {
        0
    }

    #[allow(clippy::extra_unused_type_parameters)]
    pub(super) unsafe extern "C" fn host_again_c<H: Host>(_ctx: *mut Context, _pc: u32) -> u64 {
        0
    }
}

#[cfg(test)]
mod tests;
