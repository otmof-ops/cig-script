// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
//! `dead-pure`: what nothing uses and nothing needs goes.

use super::{snapshot, walk, Pass, Remark};
use crate::lower::cir::ir::*;
use crate::syntax::span::Span;
use std::collections::{HashMap, HashSet};

/// Removes pure instructions whose value nothing uses: those `Kind::class`
/// calls `Class::Pure` (constants, prelude names, closures, fresh lists,
/// interpolations, reads of a slot that never changes, and operators whose
/// operand types cannot make them fail).
///
/// Must not: remove an operation that may raise (an unused `a / b` still
/// raises E5xx at its span; T4 catches its removal), an effect (T3) or a
/// step mark (T3).
pub struct DeadPure;

impl Pass for DeadPure {
    fn name(&self) -> &'static str {
        "dead-pure"
    }

    fn run(&self, m: &mut Module) {
        let vars = &m.vars;
        for f in &mut m.funcs {
            loop {
                let mut used = HashSet::new();
                uses(&f.body, &mut used);
                let mut dead = HashSet::new();
                find_dead(f, &f.body, vars, &used, &mut dead);
                if dead.is_empty() {
                    break;
                }
                prune(&mut f.body, &dead);
            }
        }
    }

    fn run_remarking(&self, m: &mut Module, remarks: &mut Vec<Remark>) {
        let before = snapshot(m);
        self.run(m);
        let after: HashMap<InstrId, ()> = walk(m).into_iter().map(|(_, s)| (s.id, ())).collect();
        let mut pruned = 0usize;
        let mut details: Vec<Remark> = Vec::new();
        for (id, b) in &before {
            if !after.contains_key(id) {
                pruned += 1;
                details.push(Remark {
                    pass: self.name(),
                    message: format!("dead-pure: pruned a {} in {}", b.kind_tag, b.func),
                    span: b.span,
                });
            }
        }
        // The snapshot is a map: the lines go in source order, so the same
        // build says the same thing.
        details.sort_by_key(|r| (r.span.line, r.span.col, r.message.clone()));
        remarks.push(Remark {
            pass: self.name(),
            message: match pruned {
                0 => "dead-pure: nothing to prune".to_string(),
                1 => "dead-pure: pruned 1 pure value".to_string(),
                n => format!("dead-pure: pruned {n} pure values"),
            },
            span: Span::default(),
        });
        remarks.extend(details);
    }
}

fn uses(b: &Block, used: &mut HashSet<ValueId>) {
    for i in &b.instrs {
        used.extend(i.kind.uses());
        for blk in i.kind.blocks() {
            uses(blk, used);
        }
    }
}

/// The pure instructions of `b` (nested blocks included) whose value
/// nothing uses.
fn find_dead(
    f: &Func,
    b: &Block,
    vars: &[VarInfo],
    used: &HashSet<ValueId>,
    dead: &mut HashSet<InstrId>,
) {
    for i in &b.instrs {
        if i.result.is_some_and(|r| !used.contains(&r)) && i.kind.class(f, vars) == Class::Pure {
            dead.insert(i.id);
        }
        for blk in i.kind.blocks() {
            find_dead(f, blk, vars, used, dead);
        }
    }
}

fn prune(b: &mut Block, dead: &HashSet<InstrId>) {
    b.instrs.retain(|i| !dead.contains(&i.id));
    for i in &mut b.instrs {
        for blk in i.kind.blocks_mut() {
            prune(blk, dead);
        }
    }
}
