// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Lexing and parsing.

pub mod ast;
pub mod lexer;
pub mod parser;
pub mod span;
pub mod token;

pub use parser::{parse, parse_expr_src};
