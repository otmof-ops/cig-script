// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! CigScript: a small scripting language where nothing real happens unless
//! you `burn`.
//!
//! The crate is organised as a pipeline:
//!
//! * [`syntax`] turns source text into an AST,
//! * [`check`] walks the AST for static mistakes,
//! * [`interp`] evaluates it, calling into the [`stdlib`],
//! * [`burn`] is the kernel: it gates side effects, journals them and rolls
//!   them back.
//!
//! The `cig` command line is [`cli`]; `src/main.rs` only starts it.

pub mod ashtray;
pub mod benchstats;
pub mod burn;
pub mod check;
pub mod cli;
pub mod config;
pub mod crash;
pub mod diagnostics;
pub mod doctor;
pub mod errors;
pub mod humidor;
pub mod installer;
pub mod interp;
pub mod pass;
pub mod pin;
pub mod program;
pub mod repro;
pub mod since;
pub mod stdlib;
pub mod syntax;
pub mod timings;
pub mod tracker;
pub mod trial;
pub mod tripwire;
pub mod update;
pub mod value;
pub mod vm;

/// The bytecode crate the VM runs, under this crate's name: a compiler
/// that lowers to bytecode builds its modules with the same copy of the
/// crate the VM loads, so a `Verified` it makes is one `vm::load` takes.
pub use cig_bytecode;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The one line every failed run leads with.
pub const NO_CIGARETTES: &str = "Don't see any cigarettes.";
