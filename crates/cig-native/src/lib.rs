// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! # cig-native
//!
//! What a cig program and the compiler that built it agree on about
//! native code: the `native` section's layout, the context native code
//! shares with the VM, and the loader that maps a section's code from
//! the program's own file and calls it.
//!
//! A program built at stage `c` is a stage `b` program (its script as
//! bytecode, run by the VM) with one more section, `native`: machine
//! code for some of its functions, each compiled for one row of argument
//! types. The VM calls an entry when a call's arguments are of its row,
//! and runs the bytecode otherwise, and whenever the code cannot be
//! loaded.
//!
//! - [`mod@format`]: the section's body ([`Section`], [`Tables`], [`Entry`],
//!   [`Site`], [`Body`], [`Rule`]), written by [`Section::to_bytes`] and
//!   read by [`Tables::parse`] and [`Section::parse`]. A body is hostile
//!   data to the reader. No `unsafe`.
//! - [`Context`]: the 80 bytes native code and the VM share for one
//!   call (interface 2: with the frame, the helpers and the host added
//!   for bodies).
//! - [`mod@value`]: what native code knows of a value in a frame
//!   (interface 2): where its tag and payload are, and the tags of
//!   null, bool, int and float. [`value::agrees`] holds a runtime's
//!   value type to these constants.
//! - [`loader`]: [`Mapped`], a section's code mapped read and execute
//!   from a file, [`Code`], a mapping whose entries and bodies can be
//!   called, and the helpers' table ([`Helpers`], [`Host`]) that stands
//!   between native code and the runtime. Every `unsafe` block of the
//!   crate is in this module and in [`mod@value`].
//!
//! ## Trust
//!
//! No check makes bytes safe to run. So the crate separates what it can
//! do safely from what the caller has to answer for:
//!
//! - **Mapping is safe.** [`Mapped::map`] gives back nothing but the
//!   mapping's length: no reference into it and no way to run it.
//! - **Arming is the caller's word.** [`Mapped::arm`] is `unsafe`: its
//!   caller vouches that the bytes are the code generator's for this
//!   machine and that the file will not change underneath. It checks
//!   what can be checked: the mapping holds exactly the bytes the caller
//!   read and verified, and every entry lies in them on an instruction
//!   boundary.
//! - **Calling is checked.** [`Code::call`] takes an entry's index and
//!   exactly as many argument slots as that entry reads, or runs
//!   nothing.
//!
//! The mapping is read and execute, private, and of the code alone:
//! never writable, never anonymous, never changed with `mprotect`.
//!
//! ## Targets
//!
//! The loader is built for Linux on x86-64 and aarch64. On every other
//! target the crate compiles, the format and the context are whole, and
//! [`Mapped::map`] returns [`MapError::Unsupported`].

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![cfg_attr(
    not(test),
    deny(clippy::panic, clippy::unwrap_used, clippy::expect_used)
)]

mod context;
pub mod format;
#[allow(unsafe_code)]
pub mod loader;
#[allow(unsafe_code)]
pub mod value;

pub use context::Context;
pub use format::{
    bodies_in, op, Arch, Body, Entry, Exit, FormatError, Op, Rule, Section, Site, Tables, Ty,
    ALIGN, BODY_LEN, ENTRY_LEN, FOOTER_LEN, INTERFACE, MAGIC, MAX_ARGS, MAX_BODY_FRAME, MAX_FRAME,
    MAX_ROWS, RESUME_LEN, SITE_LEN, VERSION,
};
pub use loader::{Code, Helpers, Host, MapError, Mapped, Stop};
