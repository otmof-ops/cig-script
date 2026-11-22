// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
//! `copy-prop`: `stick a = b; use a` becomes `use b`.

use super::Pass;
use crate::lower::cir::ir::*;
use std::collections::{HashMap, HashSet};

/// Copy propagation: a use of a value that is a plain copy of another is
/// replaced by the original. `stick a = b` uses `b`: a later load of `a`
/// reads the same value the declaration stored, so the use becomes a use
/// of that value directly and the load is dead.
///
/// Must not: rewrite across a store to the same slot, or across the
/// boundary into a function that captures the slot. Both are side-stepped
/// by considering only slots that are neither `roll` (mutable) nor shared
/// with another function (`captured`): the declaration's source is the
/// slot's only value in this function's lifetime, and nowhere else is a
/// load of it visible.
pub struct CopyProp;

impl Pass for CopyProp {
    fn name(&self) -> &'static str {
        "copy-prop"
    }

    fn run(&self, m: &mut Module) {
        let flag: Vec<bool> = m.vars.iter().map(|v| !v.mutable && !v.captured).collect();
        for f in &mut m.funcs {
            copy_prop_func(f, &flag);
        }
    }
}

fn copy_prop_func(f: &mut Func, flag: &[bool]) {
    let mut src: HashMap<VarId, ValueId> = HashMap::new();
    collect_decl_sources(&f.body, flag, &mut src);
    if src.is_empty() {
        return;
    }
    let mut rename: HashMap<ValueId, ValueId> = HashMap::new();
    collect_load_renames(&f.body, &src, &mut rename);
    if rename.is_empty() {
        return;
    }
    // Resolve chains: a load of `a` replaced by a load of `b` which is
    // itself a copy of `c` (`stick a = b; stick c = a`) walks to `c`'s
    // source. Capped to guard against a cycle the lowering should not
    // produce.
    let keys: Vec<ValueId> = rename.keys().copied().collect();
    for k in keys {
        let mut cur = rename[&k];
        let mut steps = 0;
        while let Some(&nx) = rename.get(&cur) {
            if nx == cur || steps > 1024 {
                break;
            }
            cur = nx;
            steps += 1;
        }
        rename.insert(k, cur);
    }
    rewrite_block_uses(&mut f.body, &rename);
    let renamed: HashSet<ValueId> = rename.keys().copied().collect();
    remove_renamed_loads(&mut f.body, &renamed);
}

fn collect_decl_sources(b: &Block, flag: &[bool], out: &mut HashMap<VarId, ValueId>) {
    for i in &b.instrs {
        if let Kind::Decl(v, x) = &i.kind {
            if flag.get(v.0 as usize).copied().unwrap_or(false) {
                out.insert(*v, *x);
            }
        }
        for blk in i.kind.blocks() {
            collect_decl_sources(blk, flag, out);
        }
    }
}

fn collect_load_renames(
    b: &Block,
    src: &HashMap<VarId, ValueId>,
    rename: &mut HashMap<ValueId, ValueId>,
) {
    for i in &b.instrs {
        if let (Kind::Load(v), Some(r)) = (&i.kind, i.result) {
            if let Some(x) = src.get(v) {
                rename.insert(r, *x);
            }
        }
        for blk in i.kind.blocks() {
            collect_load_renames(blk, src, rename);
        }
    }
}

fn rewrite_block_uses(b: &mut Block, rename: &HashMap<ValueId, ValueId>) {
    for i in &mut b.instrs {
        rewrite_kind_uses(&mut i.kind, rename);
        for blk in i.kind.blocks_mut() {
            rewrite_block_uses(blk, rename);
        }
    }
}

fn rewrite_kind_uses(k: &mut Kind, rename: &HashMap<ValueId, ValueId>) {
    let rn = |v: ValueId| rename.get(&v).copied().unwrap_or(v);
    match k {
        Kind::Step(_)
        | Kind::Const(_)
        | Kind::Prelude(_)
        | Kind::Load(_)
        | Kind::Closure(_)
        | Kind::Break
        | Kind::Continue
        | Kind::Loop { .. }
        | Kind::Try { .. } => {}
        Kind::Decl(_, v) | Kind::Store(_, v) | Kind::Unary(_, v) | Kind::Key(v) => {
            *v = rn(*v);
        }
        Kind::Binary(_, a, b) => {
            *a = rn(*a);
            *b = rn(*b);
        }
        Kind::Short { subject, .. } => {
            *subject = rn(*subject);
        }
        Kind::MakeList(vs) | Kind::PackDecl(vs) | Kind::Exhale(vs) => {
            for v in vs {
                *v = rn(*v);
            }
        }
        Kind::MakeMap(kvs) => {
            for (k, v) in kvs {
                if let MapKey::Dyn(d, _) = k {
                    *d = rn(*d);
                }
                *v = rn(*v);
            }
        }
        Kind::Interp(ps) => {
            for p in ps {
                if let Piece::Val(v) = p {
                    *v = rn(*v);
                }
            }
        }
        Kind::Member { obj, .. } => {
            *obj = rn(*obj);
        }
        Kind::Index { obj, idx, .. } => {
            *obj = rn(*obj);
            *idx = rn(*idx);
        }
        Kind::SetIndex { obj, idx, val, .. } => {
            *obj = rn(*obj);
            *idx = rn(*idx);
            *val = rn(*val);
        }
        Kind::SetMember { obj, val, .. } => {
            *obj = rn(*obj);
            *val = rn(*val);
        }
        Kind::BurnOp { args, .. }
        | Kind::CallBuiltin { args, .. }
        | Kind::CallUser { args, .. } => {
            for a in args {
                *a = rn(*a);
            }
        }
        Kind::CallValue { f, args } => {
            *f = rn(*f);
            for a in args {
                *a = rn(*a);
            }
        }
        Kind::CallMethod { recv, args, .. } => {
            *recv = rn(*recv);
            for a in args {
                *a = rn(*a);
            }
        }
        Kind::MakeChain { steps, .. } => {
            for s in steps {
                s.1 = rn(s.1);
            }
        }
        Kind::Burn(b) => {
            if let Some(l) = &mut b.label {
                *l = rn(*l);
            }
        }
        Kind::If { cond, .. } => {
            *cond = rn(*cond);
        }
        Kind::ForEach { iter, .. } => {
            *iter = rn(*iter);
        }
        Kind::Yield(v) | Kind::Ret(v) | Kind::Throw(v) => {
            *v = rn(*v);
        }
    }
}

fn remove_renamed_loads(b: &mut Block, renamed: &HashSet<ValueId>) {
    b.instrs.retain(|i| match (&i.kind, i.result) {
        (Kind::Load(_), Some(r)) => !renamed.contains(&r),
        _ => true,
    });
    for i in &mut b.instrs {
        for blk in i.kind.blocks_mut() {
            remove_renamed_loads(blk, renamed);
        }
    }
}
