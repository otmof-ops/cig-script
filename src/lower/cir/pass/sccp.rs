// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
//! `sccp`: the arm of an `if` that a constant condition never takes is
//! emptied.

use super::Pass;
use crate::lower::cir::ir::*;
use std::collections::HashMap;

/// Sparse conditional constant propagation, in the form CIR's structured
/// control flow needs: `fold-consts`, run before it, carries the constants,
/// and this pass empties the dead side of an `if` whose condition is a
/// known boolean constant. The lattice through phis comes with stage B's
/// SSA, if it has phis. The `if` itself
/// stays, so the places of every surviving pinned instruction do too
/// (T1); the cond's own instruction stays, so a condition that might have
/// raised still raises at its own span. The emptied arm goes whole, its
/// step marks and its operations that may raise with it: the verifier
/// sees the constant condition and lets a block that never runs go (T3,
/// T4).
///
/// Must not: remove a branch whose condition can raise (the branch
/// instruction stays), drop a `burn.op` (T3 would catch it) or move one
/// out of a region. The dead side is left alone when it carries a
/// `burn.op`, a `burn`, a `closure`, a `pack` or a chain declaration: all
/// visible in the module beyond this block, so their removal is a bigger
/// change than this pass performs at stage A.
pub struct Sccp;

impl Pass for Sccp {
    fn name(&self) -> &'static str {
        "sccp"
    }

    fn run(&self, m: &mut Module) {
        for f in &mut m.funcs {
            let mut known: HashMap<ValueId, bool> = HashMap::new();
            collect_bool_consts(&f.body, &mut known);
            sccp_block(&mut f.body, &known);
        }
    }
}

fn collect_bool_consts(b: &Block, out: &mut HashMap<ValueId, bool>) {
    for i in &b.instrs {
        if let (Kind::Const(Const::Bool(v)), Some(r)) = (&i.kind, i.result) {
            out.insert(r, *v);
        }
        for blk in i.kind.blocks() {
            collect_bool_consts(blk, out);
        }
    }
}

fn sccp_block(b: &mut Block, known: &HashMap<ValueId, bool>) {
    for i in &mut b.instrs {
        for blk in i.kind.blocks_mut() {
            sccp_block(blk, known);
        }
        if let Kind::If {
            cond,
            then,
            otherwise,
        } = &mut i.kind
        {
            if let Some(&taken) = known.get(cond) {
                let arm = if taken {
                    otherwise.as_mut()
                } else {
                    Some(then)
                };
                if let Some(a) = arm.filter(|a| safe_to_clear(a)) {
                    a.instrs.clear();
                }
            }
        }
    }
}

fn safe_to_clear(b: &Block) -> bool {
    for i in &b.instrs {
        if !kind_safe_to_clear(&i.kind) {
            return false;
        }
        for blk in i.kind.blocks() {
            if !safe_to_clear(blk) {
                return false;
            }
        }
    }
    true
}

fn kind_safe_to_clear(k: &Kind) -> bool {
    !matches!(
        k,
        Kind::BurnOp { .. }
            | Kind::Burn(_)
            | Kind::Closure(_)
            | Kind::MakeChain { .. }
            | Kind::PackDecl(_)
    )
}
