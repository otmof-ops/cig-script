// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
#![forbid(unsafe_code)]
//! The lowering: a checked script to CIR ([`cir`], with its passes and its
//! verifier), and CIR to the register bytecode the VM runs ([`bc`]).
//!
//! `cig run` and cig-compile share this one copy. `cig run --engine vm`
//! lowers a script here before the VM opens it, and `cigcompile build
//! --stage b` puts the same bytes in a program's `bytecode` section, both
//! through [`module_bytes`], so the two cannot drift. Nothing here depends
//! on cig-compile.

pub mod bc;
pub mod cir;

use crate::syntax::ast::Program;
use crate::syntax::span::Span;

/// The lowering's version: bump it whenever the bytes it makes for a given
/// script can change; it is part of the key lowered modules are cached
/// under.
pub const VERSION: u32 = 1;

/// The optimization level [`module_bytes`] lowers at: what `cigcompile
/// build --stage b` uses without `-O`. Level 0 is lowering only; every
/// level above runs the same passes ([`cir::pass::level`]).
pub const DEFAULT_LEVEL: u8 = 1;

/// Why a script has no module.
#[derive(Clone, Debug, PartialEq)]
pub enum Failure {
    /// A construct the lowering does not take yet (the text names it): the
    /// caller runs the script on the tree-walker.
    Refused(String),
    /// The lowering or the verifier found its own bug.
    Internal(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Refused(s) | Failure::Internal(s) => f.write_str(s),
        }
    }
}

/// The first thing that stopped a lowering, with its place in the script
/// when it has one. `bug` marks a stop that is the lowering's own fault
/// rather than a construct it declines; the caller words that.
#[derive(Clone, Debug, PartialEq)]
pub struct Stop {
    pub what: String,
    pub span: Option<Span>,
    pub bug: bool,
}

impl From<Stop> for Failure {
    fn from(s: Stop) -> Failure {
        let at = match s.span {
            Some(sp) => format!("{}:{}: ", sp.line, sp.col),
            None => String::new(),
        };
        if s.bug {
            Failure::Internal(format!("{at}{}", s.what))
        } else {
            Failure::Refused(format!("{at}{}", s.what))
        }
    }
}

/// A checked script as the verified module's bytes: what `cigcompile build
/// --stage b` puts in a program's `bytecode` section at its default level,
/// and what `cig run` loads with `vm::open`.
pub fn module_bytes(program: &Program, file: &str) -> Result<Vec<u8>, Failure> {
    module_bytes_at(program, file, DEFAULT_LEVEL).map_err(Failure::from)
}

/// [`module_bytes`] at optimization level `opt`, with the stop in full.
/// The script is lowered to CIR, run through the passes of level `opt`,
/// lowered to bytecode, verified, and loaded by this cig's VM (its
/// builtins, prelude names and burn-op kinds), with no instruction the VM
/// lists as not built. The bytes are the module as
/// [`cig_bytecode::serialize_module`] writes it.
pub fn module_bytes_at(program: &Program, file: &str, opt: u8) -> Result<Vec<u8>, Stop> {
    use bc::{compile_program, CompileError, Error};
    use cir::LowerError;
    let passes = cir::pass::level(opt);
    let m = compile_program(program, file, &passes).map_err(|e| match e {
        CompileError::Lower(LowerError::Diagnostic(d)) => Stop {
            what: d.message,
            span: d.span,
            bug: false,
        },
        CompileError::Lower(LowerError::Internal(e)) => Stop {
            what: format!("the lowering to IR failed ({e})"),
            span: None,
            bug: true,
        },
        CompileError::Verify(e) => Stop {
            what: format!(
                "the IR broke a rule after {} ({})",
                e.pass,
                e.errors
                    .iter()
                    .map(|x| x.to_string())
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
            span: None,
            bug: true,
        },
        CompileError::Bytecode(Error::Refused { construct, span }) => Stop {
            what: format!("{construct} is not lowered to bytecode"),
            span: Some(span),
            bug: false,
        },
        CompileError::Bytecode(Error::Limit { what, span }) => Stop {
            what,
            span: Some(span),
            bug: false,
        },
        CompileError::Bytecode(Error::Internal(e)) => Stop {
            what: e,
            span: None,
            bug: true,
        },
    })?;
    if let Some(n) = not_built(&m.main) {
        return Err(n);
    }
    let verified = cig_bytecode::verify_module(&m).map_err(|e| Stop {
        what: format!("the bytecode does not verify: {e}"),
        span: None,
        bug: true,
    })?;
    crate::vm::load(verified).map_err(|d| Stop {
        what: format!("this cig's VM does not load it: {}", d.message),
        span: d.span,
        bug: false,
    })?;
    Ok(cig_bytecode::serialize_module(&m))
}

/// The first instruction, in pre-order, that this cig's VM lists as not
/// built (`vm::NOT_BUILT`), with its line from the function's line table.
fn not_built(f: &cig_bytecode::Function) -> Option<Stop> {
    use cig_bytecode::decode;
    let mut pc = 0u32;
    while (pc as usize) < f.instrs.len() {
        let Ok(d) = decode(&f.instrs, pc) else {
            break;
        };
        if crate::vm::NOT_BUILT.contains(&d.op) {
            let row = f.lines.iter().take_while(|r| r.pc <= pc).last();
            let span =
                row.map(|r| Span::new(0, 0, f.span_base.line.saturating_add(r.line_off), r.col));
            return Some(Stop {
                what: format!(
                    "it needs the instruction `{:?}`, which this cig's VM does not run",
                    d.op
                ),
                span,
                bug: false,
            });
        }
        pc = d.pc + d.width;
    }
    f.children.iter().find_map(not_built)
}
