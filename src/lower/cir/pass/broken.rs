// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
//! Passes that break the rules on purpose, so the tests can show the
//! verifier catches each: every pass in `NAMES` has one (`twin`).

use super::*;

/// Moves the first `burn.op` in a burn's body to just before the burn.
pub struct HoistBurnOp;

impl Pass for HoistBurnOp {
    fn name(&self) -> &'static str {
        "hoist-burn-op"
    }

    fn run(&self, m: &mut Module) {
        for f in &mut m.funcs {
            if hoist(&mut f.body) {
                return;
            }
        }
    }
}

fn hoist(b: &mut Block) -> bool {
    for k in 0..b.instrs.len() {
        if let Kind::Burn(node) = &mut b.instrs[k].kind {
            if let Some(at) = node
                .body
                .instrs
                .iter()
                .position(|i| matches!(i.kind, Kind::BurnOp { .. }))
            {
                let op = node.body.instrs.remove(at);
                b.instrs.insert(k, op);
                return true;
            }
        }
        for blk in b.instrs[k].kind.blocks_mut() {
            if hoist(blk) {
                return true;
            }
        }
    }
    false
}

/// Swaps two adjacent `burn.op`s in one region: structurally fine, so
/// only the transition check sees it.
pub struct SwapBurnOps;

impl Pass for SwapBurnOps {
    fn name(&self) -> &'static str {
        "swap-burn-ops"
    }

    fn run(&self, m: &mut Module) {
        for f in &mut m.funcs {
            if swap(&mut f.body) {
                return;
            }
        }
    }
}

/// Moves the first `burn.op` of a block past the second one and the
/// instructions that compute the second's arguments. Every use still
/// follows its definition (when nothing in between uses the first
/// op's result), so only the order of effects has changed.
fn swap(b: &mut Block) -> bool {
    let ops: Vec<usize> = b
        .instrs
        .iter()
        .enumerate()
        .filter(|(_, i)| matches!(i.kind, Kind::BurnOp { .. }))
        .map(|(k, _)| k)
        .collect();
    if ops.len() >= 2 {
        let (a, c) = (ops[0], ops[1]);
        let first = b.instrs[a].result;
        let used_between = b.instrs[a + 1..=c]
            .iter()
            .any(|i| first.is_some_and(|r| i.kind.uses().contains(&r)));
        if !used_between {
            b.instrs[a..=c].rotate_left(1);
            return true;
        }
    }
    for i in &mut b.instrs {
        for blk in i.kind.blocks_mut() {
            if swap(blk) {
                return true;
            }
        }
    }
    false
}

/// Removes the first `burn.op` it finds.
pub struct DropBurnOp;

impl Pass for DropBurnOp {
    fn name(&self) -> &'static str {
        "drop-burn-op"
    }

    fn run(&self, m: &mut Module) {
        for f in &mut m.funcs {
            if drop_one(&mut f.body) {
                return;
            }
        }
    }
}

fn drop_one(b: &mut Block) -> bool {
    if let Some(at) = b
        .instrs
        .iter()
        .position(|i| matches!(i.kind, Kind::BurnOp { .. }) && i.result.is_none_or(|_| true))
    {
        b.instrs.remove(at);
        return true;
    }
    for i in &mut b.instrs {
        for blk in i.kind.blocks_mut() {
            if drop_one(blk) {
                return true;
            }
        }
    }
    false
}

/// Moves the then-branch of the first `if` holding a `burn.op` out to
/// just after the `if`. The order and the region stay the same and
/// every use still follows its definition, but the effect is no longer
/// conditional: only T1's places see it.
pub struct HoistOutOfIf;

impl Pass for HoistOutOfIf {
    fn name(&self) -> &'static str {
        "hoist-out-of-if"
    }

    fn run(&self, m: &mut Module) {
        for f in &mut m.funcs {
            if unwrap_if(&mut f.body) {
                return;
            }
        }
    }
}

fn unwrap_if(b: &mut Block) -> bool {
    for k in 0..b.instrs.len() {
        if let Kind::If { then, .. } = &mut b.instrs[k].kind {
            let effect = then
                .instrs
                .iter()
                .any(|i| matches!(i.kind, Kind::BurnOp { .. }));
            if effect && !then.instrs.iter().any(|i| i.kind.is_terminator()) {
                let moved = std::mem::take(&mut then.instrs);
                b.instrs.splice(k + 1..k + 1, moved);
                return true;
            }
        }
        for blk in b.instrs[k].kind.blocks_mut() {
            if unwrap_if(blk) {
                return true;
            }
        }
    }
    false
}

/// Clears the first `if.then` that holds a `burn.op`, ignoring the
/// condition. The broken twin of `sccp`: a correct pass leaves a dead
/// branch alone when its removal would delete an effect, and the
/// transition check's T3 catches this shape where the pass itself
/// failed to.
pub struct DropBranchWithEffect;

impl Pass for DropBranchWithEffect {
    fn name(&self) -> &'static str {
        "drop-branch-with-effect"
    }

    fn run(&self, m: &mut Module) {
        for f in &mut m.funcs {
            if drop_effect_branch(&mut f.body) {
                return;
            }
        }
    }
}

fn drop_effect_branch(b: &mut Block) -> bool {
    for i in &mut b.instrs {
        if let Kind::If { then, .. } = &mut i.kind {
            if then
                .instrs
                .iter()
                .any(|ii| matches!(ii.kind, Kind::BurnOp { .. }))
            {
                then.instrs.clear();
                return true;
            }
        }
        for blk in i.kind.blocks_mut() {
            if drop_effect_branch(blk) {
                return true;
            }
        }
    }
    false
}

/// Rewrites the first use after the first `if` with a non-empty
/// `then`, replacing it with a value defined inside that then-branch.
/// The broken twin of `copy-prop`: a correct pass never propagates a
/// value out of the scope it is defined in, and V1 catches the shape
/// where it slipped (`%r used where it is not defined`).
pub struct PropOutOfNestedScope;

impl Pass for PropOutOfNestedScope {
    fn name(&self) -> &'static str {
        "prop-out-of-nested-scope"
    }

    fn run(&self, m: &mut Module) {
        for f in &mut m.funcs {
            if prop_out_once(&mut f.body) {
                return;
            }
        }
    }
}

fn first_result_in(b: &Block) -> Option<ValueId> {
    for i in &b.instrs {
        if let Some(r) = i.result {
            return Some(r);
        }
        for blk in i.kind.blocks() {
            if let Some(r) = first_result_in(blk) {
                return Some(r);
            }
        }
    }
    None
}

fn prop_out_once(b: &mut Block) -> bool {
    // Find the first If with a non-empty then, remember a value it
    // defines, and rewrite the first instruction AFTER that if whose
    // first use we can reach.
    let mut inner: Option<ValueId> = None;
    let mut after: Option<usize> = None;
    for (k, i) in b.instrs.iter().enumerate() {
        if let Kind::If { then, .. } = &i.kind {
            if let Some(r) = first_result_in(then) {
                inner = Some(r);
                after = Some(k);
                break;
            }
        }
    }
    if let (Some(r), Some(k)) = (inner, after) {
        for later in b.instrs.iter_mut().skip(k + 1) {
            if replace_first_use(&mut later.kind, r) {
                return true;
            }
        }
    }
    for i in &mut b.instrs {
        for blk in i.kind.blocks_mut() {
            if prop_out_once(blk) {
                return true;
            }
        }
    }
    false
}

fn replace_first_use(k: &mut Kind, with: ValueId) -> bool {
    match k {
        Kind::Decl(_, v)
        | Kind::Store(_, v)
        | Kind::Unary(_, v)
        | Kind::Yield(v)
        | Kind::Ret(v)
        | Kind::Throw(v) => {
            *v = with;
            true
        }
        Kind::Binary(_, a, _) => {
            *a = with;
            true
        }
        Kind::Short { subject, .. } => {
            *subject = with;
            true
        }
        Kind::MakeList(vs) | Kind::PackDecl(vs) | Kind::Exhale(vs) => {
            if let Some(first) = vs.first_mut() {
                *first = with;
                true
            } else {
                false
            }
        }
        Kind::Member { obj, .. } | Kind::Index { obj, .. } => {
            *obj = with;
            true
        }
        Kind::If { cond, .. } => {
            *cond = with;
            true
        }
        _ => false,
    }
}

/// Removes the first step mark of a function's own block while the
/// rest of the block stays: a run of that block would count one step
/// fewer than the interpreter. T3 catches it.
pub struct DropStepMark;

impl Pass for DropStepMark {
    fn name(&self) -> &'static str {
        "drop-step-mark"
    }

    fn run(&self, m: &mut Module) {
        for f in &mut m.funcs {
            let b = &mut f.body.instrs;
            if b.len() > 1 {
                if let Some(at) = b.iter().position(|i| matches!(i.kind, Kind::Step(_))) {
                    b.remove(at);
                    return;
                }
            }
        }
    }
}

/// Empties the first loop body it finds, its `step.iter` with it. The
/// loop still iterates and the interpreter still counts each turn, so
/// V11 wants the mark back even though the whole block went.
pub struct EmptyLoopBody;

impl Pass for EmptyLoopBody {
    fn name(&self) -> &'static str {
        "empty-loop-body"
    }

    fn run(&self, m: &mut Module) {
        for f in &mut m.funcs {
            if empty_loop(&mut f.body) {
                return;
            }
        }
    }
}

fn empty_loop(b: &mut Block) -> bool {
    for i in &mut b.instrs {
        match &mut i.kind {
            Kind::Loop { body, .. } | Kind::ForEach { body, .. } => {
                body.instrs.clear();
                return true;
            }
            k => {
                for blk in k.blocks_mut() {
                    if empty_loop(blk) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Moves the first loop body's `step.iter` to just before the loop:
/// one step in all instead of one per turn.
pub struct HoistStepIter;

impl Pass for HoistStepIter {
    fn name(&self) -> &'static str {
        "hoist-step-iter"
    }

    fn run(&self, m: &mut Module) {
        for f in &mut m.funcs {
            if hoist_iter(&mut f.body) {
                return;
            }
        }
    }
}

fn hoist_iter(b: &mut Block) -> bool {
    for k in 0..b.instrs.len() {
        if let Kind::Loop { body, .. } | Kind::ForEach { body, .. } = &mut b.instrs[k].kind {
            if let Some(at) = body
                .instrs
                .iter()
                .position(|i| matches!(i.kind, Kind::Step(StepSite::Iter)))
            {
                let mark = body.instrs.remove(at);
                b.instrs.insert(k, mark);
                return true;
            }
        }
        for blk in b.instrs[k].kind.blocks_mut() {
            if hoist_iter(blk) {
                return true;
            }
        }
    }
    false
}

/// Folds integer `+ - *` with wrapping arithmetic, so an overflow that
/// the interpreter raises (E503) becomes a constant. The broken twin of
/// `fold-consts`: T4 finds no proof for the fold.
pub struct WrappingFold;

impl Pass for WrappingFold {
    fn name(&self) -> &'static str {
        "wrapping-fold"
    }

    fn run(&self, m: &mut Module) {
        for f in &mut m.funcs {
            let mut known: HashMap<ValueId, i64> = HashMap::new();
            wrap_block(&mut f.body, &mut known);
        }
    }
}

fn wrap_block(b: &mut Block, known: &mut HashMap<ValueId, i64>) {
    use crate::syntax::ast::BinOp;
    for i in &mut b.instrs {
        for blk in i.kind.blocks_mut() {
            wrap_block(blk, known);
        }
        let folded = match &i.kind {
            Kind::Const(Const::Int(n)) => {
                if let Some(r) = i.result {
                    known.insert(r, *n);
                }
                None
            }
            Kind::Binary(op, a, b) => match (known.get(a), known.get(b), op) {
                (Some(x), Some(y), BinOp::Add) => Some(x.wrapping_add(*y)),
                (Some(x), Some(y), BinOp::Sub) => Some(x.wrapping_sub(*y)),
                (Some(x), Some(y), BinOp::Mul) => Some(x.wrapping_mul(*y)),
                _ => None,
            },
            _ => None,
        };
        if let (Some(n), Some(r)) = (folded, i.result) {
            known.insert(r, n);
            i.kind = Kind::Const(Const::Int(n));
        }
    }
}

/// Removes every instruction whose value nothing uses and that is not an
/// effect or a step mark, operations that may raise included. The broken
/// twin of `dead-pure`: an unused `a / b` still raises at run time, and
/// T4 catches its removal.
pub struct DropUnusedRaising;

impl Pass for DropUnusedRaising {
    fn name(&self) -> &'static str {
        "drop-unused-raising"
    }

    fn run(&self, m: &mut Module) {
        let vars = &m.vars;
        for f in &mut m.funcs {
            let mut used = std::collections::HashSet::new();
            used_in(&f.body, &mut used);
            let mut dead = std::collections::HashSet::new();
            unused(f, &f.body, vars, &used, &mut dead);
            drop_ids(&mut f.body, &dead);
        }
    }
}

fn used_in(b: &Block, used: &mut std::collections::HashSet<ValueId>) {
    for i in &b.instrs {
        used.extend(i.kind.uses());
        for blk in i.kind.blocks() {
            used_in(blk, used);
        }
    }
}

fn unused(
    f: &Func,
    b: &Block,
    vars: &[VarInfo],
    used: &std::collections::HashSet<ValueId>,
    dead: &mut std::collections::HashSet<InstrId>,
) {
    for i in &b.instrs {
        let class = i.kind.class(f, vars);
        if i.result.is_some_and(|r| !used.contains(&r))
            && matches!(class, Class::Pure | Class::MayRaise)
        {
            dead.insert(i.id);
        }
        for blk in i.kind.blocks() {
            unused(f, blk, vars, used, dead);
        }
    }
}

fn drop_ids(b: &mut Block, dead: &std::collections::HashSet<InstrId>) {
    b.instrs.retain(|i| !dead.contains(&i.id));
    for i in &mut b.instrs {
        for blk in i.kind.blocks_mut() {
            drop_ids(blk, dead);
        }
    }
}

/// A pass's broken twin: the pass it breaks the rules of, a script that
/// shows it, and the rule the verifier catches it by.
pub struct Twin {
    pub pass: &'static dyn Pass,
    pub script: &'static str,
    pub rule: &'static str,
}

/// The broken twin of each pass in `NAMES`.
pub fn twin(name: &str) -> Option<Twin> {
    let (pass, script, rule): (&'static dyn Pass, &'static str, &'static str) = match name {
        "fold-consts" => (&WrappingFold, "exhale 9223372036854775807 + 1\n", "T4"),
        "copy-prop" => (
            &PropOutOfNestedScope,
            "stick a = 1\nif true {\n  stick b = 2\n  exhale b\n}\nexhale a\n",
            "V1",
        ),
        "sccp" => (
            &DropBranchWithEffect,
            "burn {\n  if true {\n    fs.rm(\"gone\")\n  }\n}\n",
            "T3",
        ),
        "dead-pure" => (&DropUnusedRaising, "roll a = 1\nroll b = 0\na / b\n", "T4"),
        _ => return None,
    };
    Some(Twin { pass, script, rule })
}
