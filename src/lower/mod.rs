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
    /// The lowering panicked: a bug it did not find. Only
    /// [`module_loaded`] gives this; the panic's message is the text.
    Crashed(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Refused(s) | Failure::Internal(s) | Failure::Crashed(s) => f.write_str(s),
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
    lowered_at(program, file, opt).map(|l| l.bytes)
}

/// A script lowered, with what native code is compiled from: the IR the
/// module was lowered from, and which IR function each of the module's
/// functions is.
pub struct Lowered {
    /// The module's bytes, as [`module_bytes_at`] gives them.
    pub bytes: Vec<u8>,
    /// The IR after the passes of the level: what `bytes` was lowered
    /// from, so native code compiled from it and the bytecode mean the
    /// same thing.
    pub cir: cir::Module,
    /// The IR function behind each function of the module, in the
    /// module's numbering ([`bc::lower_indexed`]).
    pub funcs: Vec<cir::ir::FuncId>,
}

/// [`module_bytes_at`], keeping the IR: what `cigcompile build --stage c`
/// compiles a program's native code from.
pub fn lowered_with_cir(program: &Program, file: &str, opt: u8) -> Result<Lowered, Stop> {
    lowered_at(program, file, opt).map(|l| Lowered {
        bytes: l.bytes,
        cir: l.cir,
        funcs: l.funcs,
    })
}

/// [`module_bytes`], with the module as this cig's VM loaded it while it
/// was checked: a run that has just lowered a script runs what was loaded
/// and keeps the bytes, without opening them a second time.
///
/// This is `cig run`'s way in, and a run must not end because the lowering
/// has a bug the tree-walker does not: a panic in here comes back as
/// [`Failure::Crashed`], and the caller runs the script on the tree-walker.
pub fn module_loaded(
    program: &Program,
    file: &str,
) -> Result<(Vec<u8>, std::rc::Rc<crate::vm::Program>), Failure> {
    module_loaded_timed(program, file, None)
}

/// [`module_loaded`], writing a phase line to `timings` for each stage of
/// the lowering (`compile-cir`, `compile-passes`, `compile-bytecode`,
/// `compile-verify`, `compile-load`). `cig run --timings` passes a
/// [`Timings`] in; a caller that is not timing passes `None`. The
/// `compile-keep` phase (storing the module in the tin) is timed by the
/// caller, as it is the caller's step.
///
/// [`Timings`]: crate::timings::Timings
pub fn module_loaded_timed(
    program: &Program,
    file: &str,
    timings: Option<&crate::timings::Timings>,
) -> Result<(Vec<u8>, std::rc::Rc<crate::vm::Program>), Failure> {
    guarded(|| {
        if std::env::var("CIG_INTERNAL_PANIC").as_deref() == Ok("lower") {
            // Test hook: a panic from inside the lowering, for the run
            // that must go on without it.
            panic!("deliberate test panic in the lowering (CIG_INTERNAL_PANIC=lower)");
        }
        lowered_at_timed(program, file, DEFAULT_LEVEL, timings)
            .map(|l| (l.bytes, l.loaded))
            .map_err(Failure::from)
    })
}

/// `f`, with a panic inside it turned into [`Failure::Crashed`]. The
/// lowering only reads the tree it is given and builds its own, so the
/// tree is whole whatever happened in here. The panic hook has run by the
/// time this returns: the crash is said and its report saved as for any
/// other.
fn guarded<T>(f: impl FnOnce() -> Result<T, Failure>) -> Result<T, Failure> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|panic| {
        let said = panic
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "a panic without a message".to_string());
        Err(Failure::Crashed(said))
    })
}

/// Everything one lowering makes.
struct Whole {
    bytes: Vec<u8>,
    loaded: std::rc::Rc<crate::vm::Program>,
    cir: cir::Module,
    funcs: Vec<cir::ir::FuncId>,
}

fn lowered_at(program: &Program, file: &str, opt: u8) -> Result<Whole, Stop> {
    lowered_at_timed(program, file, opt, None)
}

/// [`lowered_at`] with each stage timed on `timings` (`compile-cir`,
/// `compile-passes`, `compile-bytecode`, `compile-verify`, `compile-load`).
/// With `timings = None` no clock is read that was not read before: every
/// call to [`crate::timings::Timings::start`] returns `None` on a
/// [`Timings::off`]-like sink.
///
/// [`Timings::off`]: crate::timings::Timings::off
fn lowered_at_timed(
    program: &Program,
    file: &str,
    opt: u8,
    timings: Option<&crate::timings::Timings>,
) -> Result<Whole, Stop> {
    use bc::{CompileError, Error};
    use cir::LowerError;
    // One phase of a run of the lowering: open it at a start, close it
    // at the end, write nothing when no [`Timings`] is handed in.
    let open = || timings.and_then(|t| t.start());
    let close = |name: &str, started: Option<std::time::Instant>| {
        if let Some(t) = timings {
            t.end(name, started);
        }
    };
    let stop_of = |e: CompileError| -> Stop {
        match e {
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
            CompileError::Lower(LowerError::Refused { what, span }) => Stop {
                what,
                span: Some(span),
                bug: false,
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
        }
    };
    let passes = cir::pass::level(opt);
    // CIR: the source tree to IR.
    let cir_t = open();
    let ir = cir::lower_program(program, file).map_err(CompileError::Lower);
    close("compile-cir", cir_t);
    let mut ir = ir.map_err(stop_of)?;
    // Passes: the IR after the passes of the level.
    let passes_t = open();
    let ran = cir::pass::run_passes(&mut ir, &passes).map_err(CompileError::Verify);
    close("compile-passes", passes_t);
    ran.map_err(stop_of)?;
    // Bytecode: IR to the bytes the VM reads.
    let bc_t = open();
    let bc_r = bc::lower_indexed(&ir).map_err(CompileError::Bytecode);
    close("compile-bytecode", bc_t);
    let (m, funcs) = bc_r.map_err(stop_of)?;
    if let Some(n) = not_built(&m.main) {
        return Err(n);
    }
    // Verify: the bytes against the VM's rules.
    let verify_t = open();
    let verify_r = cig_bytecode::verify_module(&m);
    close("compile-verify", verify_t);
    let verified = verify_r.map_err(|e| Stop {
        what: format!("the bytecode does not verify: {e}"),
        span: None,
        bug: true,
    })?;
    // Load: the Program for the VM, from the verified module.
    let load_t = open();
    let load_r = crate::vm::load(verified);
    close("compile-load", load_t);
    let loaded = load_r.map_err(|d| Stop {
        what: format!("this cig's VM does not load it: {}", d.message),
        span: d.span,
        bug: false,
    })?;
    Ok(Whole {
        bytes: cig_bytecode::serialize_module(&m),
        loaded,
        cir: ir,
        funcs,
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ir_function_behind_each_module_function_is_in_the_modules_order() {
        // Functions that nest, one made in a loop, and one nothing calls.
        let source = "\
pull outer(x) {
  pull inner(y) { snuff y + 1 }
  snuff inner(x)
}
pull second(x) { snuff x }
stick double = pack(x) { snuff x * 2 }
for i in 0..2 {
  pull looped(z) { snuff z + i }
  exhale looped(i)
}
pull unused(x) { snuff x }
exhale outer(1), second(2), double(3)
";
        let ast = crate::syntax::parse(source).unwrap();
        let l = lowered_with_cir(&ast, "t.cig", DEFAULT_LEVEL).unwrap();
        assert_eq!(l.bytes, module_bytes(&ast, "t.cig").unwrap());
        fn names(f: &cig_bytecode::Function, out: &mut Vec<String>) {
            out.push(f.name.clone());
            for c in &f.children {
                names(c, out);
            }
        }
        let mut module = Vec::new();
        names(
            &cig_bytecode::parse_module(&l.bytes).unwrap().main,
            &mut module,
        );
        let ir: Vec<String> = l
            .funcs
            .iter()
            .map(|f| l.cir.funcs[f.0 as usize].name.clone().unwrap_or_default())
            .collect();
        assert_eq!(ir, module);
        assert_eq!(l.funcs.first().map(|f| f.0), Some(0));
        for name in ["outer", "inner", "second", "looped"] {
            assert_eq!(ir.iter().filter(|n| *n == name).count(), 1, "{name}");
        }
        // Each function of the module is one function of the IR.
        let mut seen = l.funcs.clone();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), l.funcs.len());
    }

    #[test]
    fn a_panic_in_the_lowering_comes_back_as_a_failure() {
        let crashed: Result<(), Failure> = guarded(|| panic!("the len is 3 but the index is 4"));
        assert_eq!(
            crashed,
            Err(Failure::Crashed(
                "the len is 3 but the index is 4".to_string()
            ))
        );
        let slot = 7;
        let formatted: Result<(), Failure> = guarded(|| panic!("no slot {slot}"));
        assert_eq!(formatted, Err(Failure::Crashed("no slot 7".to_string())));
        // What does not panic comes back as it was.
        assert_eq!(guarded(|| Ok(5)), Ok(5));
        let refused: Result<(), Failure> = guarded(|| Err(Failure::Refused("x".to_string())));
        assert_eq!(refused, Err(Failure::Refused("x".to_string())));
    }
}
