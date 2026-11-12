// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! # cig-bytecode
//!
//! The register bytecode that CigScript's stage-B VM runs
//! (otmof-ops/cig-compile#31): the instruction set, the module format,
//! a non-panicking parser, a verifier that makes a module safe to run
//! with no further checks, and a text disassembler.
//!
//! The format's rules are documented, as a contract, in
//! `docs/BYTECODE.md`. Every function in this crate implements that
//! contract and nothing else:
//!
//! - no `unsafe`;
//! - no dependency on the interpreter or the burn kernel;
//! - every function that reads bytes returns a [`ParseError`] and
//!   never panics on any input;
//! - every function that checks a module returns a [`VerifyError`]
//!   with the rule and the function and the pc.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![cfg_attr(
    not(test),
    deny(clippy::panic, clippy::unwrap_used, clippy::expect_used)
)]

pub mod disasm;
pub mod error;
pub mod instr;
pub mod module;
pub mod opcode;
pub mod parse;
pub mod serialize;
pub mod verify;

pub use error::{ParseError, VerifyError};
pub use instr::{
    decode, first_word_pcs, wide_prefix, DecodeError, DecodedInstr, FirstWordPcs, Instr,
};
pub use module::{
    BurnRegion, Constant, FuncKind, Function, HandlerEntry, LineEntry, Module, RegionRow, SpanBase,
    UpvalDesc, UpvalKind, FORMAT_VERSION, MAGIC, MAX_FUNCTION_DEPTH, MAX_REG_COUNT, RESERVED_U32,
};
pub use opcode::{Op, OpInfo, OperandKind};
pub use parse::parse_module;
pub use serialize::serialize_module;
pub use verify::verify_module;
