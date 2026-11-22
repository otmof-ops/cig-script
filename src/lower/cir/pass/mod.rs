// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
//! Passes over CIR (cig-compile#33). Each runs between two checks: the structural
//! verifier, and the transition check against a trace taken before it, so
//! a pass that moves an effect out of its region, reorders effects, drops
//! one, removes an operation that may raise without a proof, or loses a
//! step mark is caught where it happens.
//!
//! What a pass may do with an instruction is its `Class`
//! (`Kind::class`): pure ones may move, merge or vanish; effects,
//! operations that may raise and step marks keep their places and their
//! order (T1, T2), effects and marks are never lost (T3), and an
//! operation that may raise goes only where the verifier proves it cannot
//! (T4).
//!
//! One module a pass, each with its golden folder under `tests/cir/<pass>/`
//! and a broken twin in `broken` that the verifier catches (`broken::twin`).

pub mod broken;
mod copy_prop;
mod dead_pure;
mod fold_consts;
mod sccp;

pub use copy_prop::CopyProp;
pub use dead_pure::DeadPure;
pub use fold_consts::FoldConsts;
pub use sccp::Sccp;

use super::ir::*;
use super::verify::{errors, verify, verify_transition, Trace, VerifyError};
use crate::syntax::span::Span;
use std::collections::HashMap;

pub trait Pass {
    fn name(&self) -> &'static str;
    fn run(&self, m: &mut Module);

    /// A variant that records what the pass did or left alone, for
    /// `--remarks`. The default calls `run` and records nothing: a pass
    /// that has remarks overrides this. The cost is paid only by the
    /// caller that asked for remarks; `run` itself is unchanged.
    fn run_remarking(&self, m: &mut Module, _remarks: &mut Vec<Remark>) {
        self.run(m);
    }
}

/// One thing a pass did or left alone: shown by `--remarks` on stderr.
#[derive(Clone, Debug)]
pub struct Remark {
    pub pass: &'static str,
    pub message: String,
    pub span: Span,
}

#[derive(Debug)]
pub struct PassError {
    /// `lowering`, or the pass that broke a rule.
    pub pass: &'static str,
    pub errors: Vec<VerifyError>,
}

/// Verify, then run each pass between the two checks. The warnings
/// (`V6`) are returned; any error stops the pipeline at the pass that
/// caused it.
pub fn run_passes(m: &mut Module, passes: &[&dyn Pass]) -> Result<Vec<VerifyError>, PassError> {
    run_passes_inner(m, passes, None)
}

/// `run_passes`, and alongside it a remark for each pass: one summary and
/// one line per thing that changed (or a `nothing to do` line when the
/// pass left the IR as it found it). The cost is paid only here; the
/// plain `run_passes` keeps its old shape.
pub fn run_passes_with_remarks(
    m: &mut Module,
    passes: &[&dyn Pass],
) -> Result<(Vec<VerifyError>, Vec<Remark>), PassError> {
    let mut remarks = Vec::new();
    let warnings = run_passes_inner(m, passes, Some(&mut remarks))?;
    Ok((warnings, remarks))
}

fn run_passes_inner(
    m: &mut Module,
    passes: &[&dyn Pass],
    mut remarks: Option<&mut Vec<Remark>>,
) -> Result<Vec<VerifyError>, PassError> {
    let found = verify(m);
    if !errors(&found).is_empty() {
        return Err(PassError {
            pass: "lowering",
            errors: found,
        });
    }
    let mut warnings: Vec<VerifyError> = found;
    for p in passes {
        let before = Trace::of(m);
        match &mut remarks {
            Some(sink) => {
                let start = sink.len();
                // A pass with no remarks of its own is still said, changed
                // or not, by the printed IR around it.
                let text = super::print::print(m);
                p.run_remarking(m, sink);
                if sink.len() == start {
                    let same = super::print::print(m) == text;
                    sink.push(Remark {
                        pass: p.name(),
                        message: if same {
                            format!("{}: nothing to do", p.name())
                        } else {
                            format!("{}: changed the IR", p.name())
                        },
                        span: Span::default(),
                    });
                }
            }
            None => p.run(m),
        }
        let mut found = verify(m);
        found.extend(verify_transition(&before, m));
        if !errors(&found).is_empty() {
            return Err(PassError {
                pass: p.name(),
                errors: found,
            });
        }
        warnings = found;
    }
    Ok(warnings)
}

/// The passes `--emit=cir` runs: `-O1`.
pub fn default_passes() -> Vec<&'static dyn Pass> {
    level(1)
}

/// Every pass `--passes` can name, in the order the levels run them. A
/// pass added here needs its `by_name` arm, its golden folder and its
/// broken twin, or `each_pass_has_a_negative_test_that_is_caught` fails.
pub const NAMES: &[&str] = &["fold-consts", "copy-prop", "sccp", "dead-pure"];

/// A pass by the name `--passes` gives it.
pub fn by_name(name: &str) -> Option<&'static dyn Pass> {
    match name {
        "fold-consts" => Some(&FoldConsts),
        "copy-prop" => Some(&CopyProp),
        "sccp" => Some(&Sccp),
        "dead-pure" => Some(&DeadPure),
        _ => None,
    }
}

/// The passes an optimization level runs. `-O0` is lowering only; `-O1`
/// and `-O2` run every pass there is: stage B's passes (gvn, licm,
/// inlining) are what will set `-O2` apart (cig-compile#33). `fold-consts` runs
/// twice because `copy-prop` makes new constant operands visible to it
/// (`stick a = 1; if a == 1`: the second fold turns the compare into
/// `const true` and `sccp` then empties the dead branch).
pub fn level(n: u8) -> Vec<&'static dyn Pass> {
    match n {
        0 => vec![],
        _ => vec![&FoldConsts, &CopyProp, &FoldConsts, &Sccp, &DeadPure],
    }
}

/// One instruction's shape at a moment in time: enough for `--remarks`
/// to say what a pass changed or dropped. `kind_tag` is the mnemonic
/// used in the printed IR (`bin +`, `const`, `load`, `call.user`, ...);
/// `func` identifies the function the instruction sits in, by number or
/// name, so the remark can say where the change was made.
struct Summary {
    pub id: InstrId,
    pub span: Span,
    pub kind_tag: &'static str,
    pub const_text: Option<String>,
    pub func: String,
}

/// How a remark names a function: `@0 main`, `@1 greet`, `@2`.
fn func_label(f: &Func) -> String {
    match (&f.kind, &f.name) {
        (_, Some(n)) => format!("@{} {n}", f.id.0),
        (FuncKind::Main, _) => "@0 main".to_string(),
        _ => format!("@{}", f.id.0),
    }
}

fn walk(m: &Module) -> Vec<(String, Summary)> {
    let mut out = Vec::new();
    for f in &m.funcs {
        walk_block(&f.body, &func_label(f), &mut out);
    }
    out
}

fn walk_block(b: &Block, func: &str, out: &mut Vec<(String, Summary)>) {
    for i in &b.instrs {
        out.push((
            func.to_string(),
            Summary {
                id: i.id,
                span: i.span,
                kind_tag: kind_tag(&i.kind),
                const_text: match &i.kind {
                    Kind::Const(c) => Some(const_text(c)),
                    _ => None,
                },
                func: func.to_string(),
            },
        ));
        for inner in i.kind.blocks() {
            walk_block(inner, func, out);
        }
    }
}

fn snapshot(m: &Module) -> HashMap<InstrId, Summary> {
    walk(m).into_iter().map(|(_, s)| (s.id, s)).collect()
}

fn kind_tag(k: &Kind) -> &'static str {
    match k {
        Kind::Step(StepSite::Stmt) => "step",
        Kind::Step(StepSite::Iter) => "step.iter",
        Kind::Const(_) => "const",
        Kind::Prelude(_) => "prelude",
        Kind::Load(_) => "load",
        Kind::Decl(_, _) => "decl",
        Kind::Store(_, _) => "store",
        Kind::Unary(_, _) => "un",
        Kind::Binary(_, _, _) => "bin",
        Kind::Short { .. } => "short",
        Kind::MakeList(_) => "list",
        Kind::MakeMap(_) => "map",
        Kind::Interp(_) => "interp",
        Kind::Member { .. } => "member",
        Kind::Index { .. } => "index",
        Kind::Key(_) => "key",
        Kind::SetIndex { .. } => "set-index",
        Kind::SetMember { .. } => "set-member",
        Kind::Closure(_) => "closure",
        Kind::BurnOp { .. } => "burn.op",
        Kind::CallBuiltin { .. } => "call.builtin",
        Kind::CallUser { .. } => "call.user",
        Kind::CallValue { .. } => "call.value",
        Kind::CallMethod { .. } => "call.method",
        Kind::MakeChain { .. } => "chain",
        Kind::PackDecl(_) => "pack",
        Kind::Exhale(_) => "exhale",
        Kind::Burn(_) => "burn",
        Kind::Try { .. } => "try",
        Kind::If { .. } => "if",
        Kind::Loop { .. } => "loop",
        Kind::ForEach { .. } => "foreach",
        Kind::Yield(_) => "yield",
        Kind::Ret(_) => "ret",
        Kind::Break => "break",
        Kind::Continue => "continue",
        Kind::Throw(_) => "throw",
    }
}

fn const_text(c: &Const) -> String {
    match c {
        Const::Null => "null".to_string(),
        Const::Bool(b) => b.to_string(),
        Const::Int(i) => i.to_string(),
        Const::Float(f) => format!("{f:?}"),
        Const::Str(s) => format!("{s:?}"),
    }
}
