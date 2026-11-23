// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
//! Type inference over CIR, across calls (cig-compile's decision 0005,
//! point 3): for each function, under each row of argument types the
//! script calls it with, the type of every value and variable.
//!
//! **What a type here promises.** When a [`Spec`] gives a value, a slot
//! or the result any type but `Dynamic`, then in every run in which that
//! function is entered with arguments of that row, it has that type.
//! `Dynamic` promises nothing and is always a sound answer. Code compiled
//! from a `Spec` holds its values as that type with no check, so a wrong
//! type is a wrong answer: where a rule cannot be shown to hold, the
//! answer is `Dynamic`.
//!
//! The lattice is `Ty` as the IR has it: `Null`, `Bool`, `Int`, `Float`,
//! `Str`, `List`, `Map`, `Func`, with `Dynamic` on top. Inside the
//! analysis one more point sits under the eight concrete types, written
//! `Bot`: "nothing reaches here yet" (a specialization that has been
//! asked about but not yet walked, or code that cannot run). It never
//! leaves the analysis: a value still at `Bot` when the walk ends prints
//! as `Dynamic`. The join of two different concrete types is `Dynamic`;
//! `Bot` joined with anything is that thing.
//!
//! One function keeps at most `MAX_ROWS` rows of argument types. A fourth
//! row collapses the function to its one all-`Dynamic` row and the
//! earlier rows' specs are marked dead and left out of the result. The
//! walk ends because every change moves a slot or the function's `ret`
//! up a lattice of height two (`Bot < concrete < Dynamic`) and the row
//! count is bounded.
//!
//! A call asks for a row once every argument of it has a type: a `Bot`
//! argument asks for nothing, so a row is one some run can enter and the
//! rows a function keeps are not spent on passes that knew less. The
//! result is what the top level reaches through the call tables as the
//! walk left them ([`infer`]).
//!
//! **Captures.** `VarInfo` alone decides a slot's rule across functions:
//! a slot that is `mutable && captured` is `Dynamic` everywhere (owner
//! and every reader); its value is set in the owner's spec before any
//! walk, so no order of the worklist can change it. A load of an
//! immutable slot by a function that does not own it reads the IR's own
//! static type of the slot's one `Decl` value (`Func::vals[val].ty`:
//! the lowering's own rule, so a constant stays its constant type, a
//! closure is `Func`, a list is `List`; anything the lowering could not
//! type statically is `Dynamic`). A function's own loads of a slot it
//! owns read its spec's `slots` entry; V1 (the verifier) keeps every
//! such read after the declaration.
//!
//! The transfer functions for the arithmetic, the comparisons and the
//! range operator are a case-by-case read of `src/interp/ops.rs`; each
//! has a comment with the lines it leans on.

use super::ir::*;
use crate::syntax::ast::{BinOp, UnOp};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// The most rows of argument types one function keeps.
pub const MAX_ROWS: usize = 3;

/// One row of argument types: one `Ty` a parameter, in order.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Row(pub Vec<Ty>);

/// One function under one row of argument types.
#[derive(Debug)]
pub struct Spec {
    pub func: FuncId,
    pub row: Row,
    /// Indexed by `ValueId`: the type of every value of the function
    /// under this row. `Ty::Dynamic` when it is not known, and for a
    /// value in code that cannot run.
    pub vals: Vec<Ty>,
    /// Every slot the function owns (its parameters and locals): its one
    /// type under this row. Slots a function captures from an outer
    /// function do not appear here: they are read with the capture rule
    /// (see `load_slot`).
    pub slots: BTreeMap<VarId, Ty>,
    /// The join of the types it returns.
    pub ret: Ty,
    /// Each `call.user` in it, by its instruction: the specialization
    /// that call enters. A call with no entry is one an argument of
    /// which never got a type: it is in code that cannot run.
    pub calls: BTreeMap<InstrId, (FuncId, Row)>,
}

/// Every specialization reachable from the top level, sorted by
/// `(FuncId, Row)`.
#[derive(Debug)]
pub struct Inference {
    specs: Vec<Spec>,
}

impl Inference {
    pub fn specs(&self) -> &[Spec] {
        &self.specs
    }

    pub fn spec(&self, func: FuncId, row: &Row) -> Option<&Spec> {
        self.specs
            .binary_search_by(|s| (s.func, &s.row).cmp(&(func, row)))
            .ok()
            .map(|i| &self.specs[i])
    }

    pub fn of(&self, func: FuncId) -> impl Iterator<Item = &Spec> {
        self.specs.iter().filter(move |s| s.func == func)
    }
}

/// Infer every specialization reachable from the top level.
pub fn infer(m: &Module) -> Inference {
    let mut s = State::new(m);
    s.ask(FuncId(0), Row(Vec::new()));
    while let Some((func, row)) = s.work.pop_front() {
        s.walk(func, row);
    }
    s.collapsed_sweep();
    let mut specs: Vec<Spec> = s.into_specs();
    specs.sort_by(|a, b| (a.func, &a.row).cmp(&(b.func, &b.row)));
    Inference {
        specs: reached(specs),
    }
}

/// The specializations the top level reaches through the call tables as
/// the walk left them. A row asked for by a walk that knew less (before
/// a callee's result rose to `Dynamic`, say) is one no call enters any
/// more: it is left out, so the result is the same whatever order the
/// walk took.
fn reached(mut specs: Vec<Spec>) -> Vec<Spec> {
    let mut keep: BTreeSet<(FuncId, Row)> = BTreeSet::new();
    let mut next = vec![(FuncId(0), Row(Vec::new()))];
    while let Some(key) = next.pop() {
        let found = specs.binary_search_by(|s| (s.func, &s.row).cmp(&(key.0, &key.1)));
        if let (Ok(at), true) = (found, keep.insert(key)) {
            next.extend(specs[at].calls.values().cloned());
        }
    }
    specs.retain(|s| keep.contains(&(s.func, s.row.clone())));
    specs
}

/// The inference as text, for `--emit=types` and for golden files:
/// deterministic. Reads `Func::name` for the function's name and
/// `VarInfo::name` for a slot's.
pub fn print(m: &Module, inf: &Inference) -> String {
    let mut out = format!("types 1 {:?}\n", m.file);
    for s in inf.specs() {
        let f = match m.funcs.get(s.func.0 as usize) {
            Some(f) => f,
            None => continue,
        };
        let args: Vec<String> = s.row.0.iter().map(ty).collect();
        let name = match (f.kind, f.name.as_deref()) {
            (FuncKind::Main, _) => "main".to_string(),
            (_, Some(n)) => n.to_string(),
            (FuncKind::Pull, None) => format!("pull@{}", s.func.0),
            (FuncKind::Lambda, None) => format!("lambda@{}", s.func.0),
        };
        out.push_str(&format!(
            "spec @{} {} ({}) -> {}\n",
            s.func.0,
            name,
            args.join(", "),
            ty(&s.ret),
        ));
        for (v, t) in &s.slots {
            let info = match m.vars.get(v.0 as usize) {
                Some(x) => x,
                None => continue,
            };
            out.push_str(&format!(
                "  slot ${}:{}{} {}\n",
                v.0,
                info.name,
                if info.mutable { "!" } else { "" },
                ty(t),
            ));
        }
        for (i, t) in s.vals.iter().enumerate() {
            out.push_str(&format!("  val %{} {}\n", i, ty(t)));
        }
        for (ins, (g, row)) in &s.calls {
            let args: Vec<String> = row.0.iter().map(ty).collect();
            out.push_str(&format!(
                "  call #{} -> @{}({})\n",
                ins.0,
                g.0,
                args.join(", "),
            ));
        }
    }
    out
}

fn ty(t: &Ty) -> String {
    match t {
        Ty::Dynamic => "dynamic",
        Ty::Null => "null",
        Ty::Bool => "bool",
        Ty::Int => "int",
        Ty::Float => "float",
        Ty::Str => "str",
        Ty::List => "list",
        Ty::Map => "map",
        Ty::Func => "func",
    }
    .to_string()
}

// --- internal lattice ----------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Lat {
    Bot,
    T(Ty),
}

impl Lat {
    fn dynamic() -> Lat {
        Lat::T(Ty::Dynamic)
    }

    fn join(self, other: Lat) -> Lat {
        match (self, other) {
            (Lat::Bot, x) | (x, Lat::Bot) => x,
            (Lat::T(a), Lat::T(b)) if a == b => Lat::T(a),
            _ => Lat::dynamic(),
        }
    }

    /// `Bot` becomes `Dynamic`: a value never reached is in code that
    /// cannot run, and that is sound as `Dynamic`.
    fn out(self) -> Ty {
        match self {
            Lat::Bot => Ty::Dynamic,
            Lat::T(t) => t,
        }
    }
}

// --- state ---------------------------------------------------------------

struct State<'m> {
    m: &'m Module,
    /// Every row recorded for a function. After a collapse this holds the
    /// one all-`Dynamic` row and nothing else; `collapsed.contains(&func)`
    /// is then true.
    rows: BTreeMap<FuncId, Vec<Row>>,
    collapsed: BTreeSet<FuncId>,
    /// `(FuncId, Row) -> SpecState`.
    specs: BTreeMap<(FuncId, Row), SpecState>,
    /// Pending work.
    work: VecDeque<(FuncId, Row)>,
    /// Who depends on each callee: when the callee's `ret` moves, every
    /// dependent is re-walked.
    rdeps: BTreeMap<(FuncId, Row), BTreeSet<(FuncId, Row)>>,
    /// Pre-pass: every `VarId` with `mutable && captured`.
    cap_mut_dyn: BTreeSet<VarId>,
    /// Pre-pass: for every slot, the IR's own static type for the
    /// value its one `Decl` instruction declared. A `Decl` the lowering
    /// gave `Dynamic`, or no `Decl` at all (a parameter), gives
    /// `Dynamic` here — the fallback is always `Dynamic`. Only loads
    /// of an immutable slot by a function that does not own it read
    /// this; it is sound because it depends on no row and no spec.
    cap_decl_ty: BTreeMap<VarId, Ty>,
}

struct SpecState {
    func: FuncId,
    row: Row,
    /// `ValueId` -> lattice point.
    vals: Vec<Lat>,
    /// `VarId` -> lattice point for every slot the function owns.
    slots: BTreeMap<VarId, Lat>,
    ret: Lat,
    /// Each `call.user` -> the specialization it enters.
    calls: BTreeMap<InstrId, (FuncId, Row)>,
    /// Collapsed-out: the function has gone to its all-`Dynamic` row and
    /// this earlier spec no longer appears in the result. Live code
    /// still runs its walk out to the end on the shell, but the shell
    /// never grows and the result is filtered.
    dead: bool,
}

impl<'m> State<'m> {
    fn new(m: &'m Module) -> State<'m> {
        // Pre-pass over the module: mut+captured vars and the IR's own
        // static type for each slot's `Decl` value.
        let mut cap_mut_dyn = BTreeSet::new();
        for (i, info) in m.vars.iter().enumerate() {
            if info.mutable && info.captured {
                cap_mut_dyn.insert(VarId(i as u32));
            }
        }
        let mut cap_decl_ty: BTreeMap<VarId, Ty> = BTreeMap::new();
        for f in &m.funcs {
            collect_decl_tys(f, &f.body, &mut cap_decl_ty);
        }
        State {
            m,
            rows: BTreeMap::new(),
            collapsed: BTreeSet::new(),
            specs: BTreeMap::new(),
            work: VecDeque::new(),
            rdeps: BTreeMap::new(),
            cap_mut_dyn,
            cap_decl_ty,
        }
    }

    /// Ask for a specialization. Records the row (collapsing when a
    /// fourth distinct row is asked for) and enqueues the walk.
    ///
    /// Returns the row the caller should record for its own `calls`
    /// table: the all-`Dynamic` row once the function has collapsed.
    fn ask(&mut self, func: FuncId, row: Row) -> Row {
        if self.collapsed.contains(&func) {
            let d = dyn_row_of(self.m, func);
            self.ensure(func, d.clone());
            return d;
        }
        let already;
        let bucket_len;
        {
            let bucket = self.rows.entry(func).or_default();
            already = bucket.contains(&row);
            bucket_len = bucket.len();
        }
        if already {
            self.ensure(func, row.clone());
            return row;
        }
        if bucket_len >= MAX_ROWS {
            // The fourth row collapses the function. Every earlier spec
            // is marked dead (not removed: a dead spec on the walker's
            // stack must still answer its own lookups without panic);
            // its dependents are re-walked on the one all-`Dynamic` row.
            let old: Vec<Row> = self
                .rows
                .get_mut(&func)
                .map(std::mem::take)
                .unwrap_or_default();
            self.collapsed.insert(func);
            let d = dyn_row_of(self.m, func);
            self.rows.entry(func).or_default().push(d.clone());
            let mut wake: BTreeSet<(FuncId, Row)> = BTreeSet::new();
            for r in old {
                let key = (func, r);
                if let Some(s) = self.specs.get_mut(&key) {
                    s.dead = true;
                }
                if let Some(deps) = self.rdeps.remove(&key) {
                    wake.extend(deps);
                }
            }
            self.ensure(func, d.clone());
            for w in wake {
                self.enqueue(w);
            }
            return d;
        }
        self.rows.entry(func).or_default().push(row.clone());
        self.ensure(func, row.clone());
        row
    }

    fn ensure(&mut self, func: FuncId, row: Row) {
        let key = (func, row.clone());
        if self.specs.contains_key(&key) {
            return;
        }
        let f = match self.m.funcs.get(func.0 as usize) {
            Some(f) => f,
            None => return,
        };
        let vals = vec![Lat::Bot; f.vals.len()];
        let mut slots: BTreeMap<VarId, Lat> = BTreeMap::new();
        // Parameters from the row; a `mutable && captured` parameter is
        // `Dynamic` by the rule above.
        for (p, t) in f.params.iter().zip(row.0.iter()) {
            let pt = if self.cap_mut_dyn.contains(p) {
                Lat::dynamic()
            } else {
                Lat::T(*t)
            };
            slots.insert(*p, pt);
        }
        // Owned mut+captured locals: pre-set to `Dynamic` before any
        // walk. Decided from `VarInfo` alone, so no worklist order can
        // change it.
        for v in &self.cap_mut_dyn {
            if let Some(info) = self.m.vars.get(v.0 as usize) {
                if info.owner == func {
                    slots.entry(*v).or_insert(Lat::dynamic());
                }
            }
        }
        self.specs.insert(
            key.clone(),
            SpecState {
                func,
                row,
                vals,
                slots,
                ret: Lat::Bot,
                calls: BTreeMap::new(),
                dead: false,
            },
        );
        self.enqueue(key);
    }

    fn enqueue(&mut self, key: (FuncId, Row)) {
        if !self.work.iter().any(|k| k == &key) {
            self.work.push_back(key);
        }
    }

    /// One walk of `(func, row)`. Loops around while a slot's type moved
    /// during the walk; the whole function re-walks if its `ret` moved,
    /// through the dependent set. The walk ends because every change
    /// moves a slot or the function's `ret` up a lattice of height two
    /// (`Bot < concrete < Dynamic`) and `MAX_ROWS` bounds the row
    /// count. A spec that collapses mid-walk is marked dead; the walker
    /// detects that before each iteration and abandons.
    fn walk(&mut self, func: FuncId, row: Row) {
        let key = (func, row);
        let f_ptr = match self.m.funcs.get(key.0 .0 as usize) {
            Some(f) => f,
            None => return,
        };
        let ret_before_walk = match self.specs.get(&key) {
            Some(s) if !s.dead => s.ret,
            _ => return,
        };
        loop {
            let (slots_before, ret_before, dead) = match self.specs.get(&key) {
                Some(s) => (s.slots.clone(), s.ret, s.dead),
                None => return,
            };
            if dead {
                break;
            }
            // Clear the call table: the walk rebuilds it. `vals`, slots
            // and `ret` are joined monotonically across iterations, so
            // leaving them alone preserves everything the previous walk
            // learned; a value's type can only rise to `Dynamic`.
            if let Some(s) = self.specs.get_mut(&key) {
                s.calls.clear();
            }
            self.walk_block(&key, f_ptr, &f_ptr.body, true);
            let (slots_after, ret_after) = match self.specs.get(&key) {
                Some(s) => (s.slots.clone(), s.ret),
                None => return,
            };
            if slots_after == slots_before && ret_after == ret_before {
                break;
            }
        }
        // Wake dependents only when `ret` moved during this whole walk.
        let moved = match self.specs.get(&key) {
            Some(s) => s.ret != ret_before_walk,
            None => false,
        };
        if moved {
            if let Some(deps) = self.rdeps.get(&key).cloned() {
                for d in deps {
                    self.enqueue(d);
                }
            }
        }
    }

    /// Walk a block in its linear order. `reachable` is true when
    /// control has not yet passed a terminator in this linear sequence.
    /// Values after a terminator are still typed by their operands; a
    /// `Ret` after a terminator does not contribute to the function's
    /// `ret` (it cannot run).
    fn walk_block(&mut self, key: &(FuncId, Row), f: &'m Func, b: &Block, mut reachable: bool) {
        for i in &b.instrs {
            self.walk_instr(key, f, i, reachable);
            for blk in i.kind.blocks() {
                // A nested block starts reachable from its own entry.
                self.walk_block(key, f, blk, true);
            }
            if i.kind.is_terminator() {
                reachable = false;
            }
        }
    }

    fn walk_instr(&mut self, key: &(FuncId, Row), f: &'m Func, i: &Instr, reachable: bool) {
        let t = self.transfer(key, f, i);
        if let (Some(r), Some(t)) = (i.result, t) {
            if let Some(s) = self.specs.get_mut(key) {
                if let Some(slot) = s.vals.get_mut(r.0 as usize) {
                    *slot = slot.join(t);
                }
            }
        }
        // Slot updates for `Decl`, `Store`, `foreach` and `catch`.
        match &i.kind {
            Kind::Decl(v, x) | Kind::Store(v, x) => {
                // Capture: the slot lives in another function. Never
                // update this spec's slot map with a non-owned var; the
                // owner's spec has its own slot entry (pre-set to
                // `Dynamic` when the slot is `mutable && captured`).
                let info = match self.m.vars.get(v.0 as usize) {
                    Some(info) => info,
                    None => return,
                };
                if info.owner != f.id {
                    return;
                }
                if self.cap_mut_dyn.contains(v) {
                    // Pre-set to `Dynamic` and stays `Dynamic`: skip the
                    // join (it would be a no-op anyway).
                    return;
                }
                let from = self.val(key, *x);
                self.join_slot(key, *v, from);
            }
            Kind::ForEach { iter, var, .. } => {
                // The iterator is a `bin ..` of two `Int`s under this
                // spec's row: the item is `Int`. Otherwise `Dynamic`.
                // A fresh `foreach` cell each iteration: no decl/load
                // race across iterations.
                let t = match range_operands(f, *iter) {
                    Some((a, b))
                        if self.val(key, a) == Lat::T(Ty::Int)
                            && self.val(key, b) == Lat::T(Ty::Int) =>
                    {
                        Lat::T(Ty::Int)
                    }
                    _ => Lat::dynamic(),
                };
                // A `foreach` variable is owned by its function (V9), so
                // the join lands in this spec's slots.
                self.join_slot(key, *var, t);
            }
            Kind::Try {
                catch: Some((Some(v), _)),
                ..
            } => {
                self.join_slot(key, *v, Lat::dynamic());
            }
            _ => {}
        }
        // `ret`: the join of every `ret v` the control flow can reach.
        // A `ret` after a terminator in its own block cannot run and
        // does not contribute.
        if let Kind::Ret(v) = &i.kind {
            if reachable {
                let rv = self.val(key, *v);
                if let Some(s) = self.specs.get_mut(key) {
                    s.ret = s.ret.join(rv);
                }
            }
        }
    }

    /// The transfer function for an instruction's result value. Returns
    /// `None` for an instruction without a result value.
    ///
    /// Each operator's rule cites `src/interp/ops.rs` by line.
    fn transfer(&mut self, key: &(FuncId, Row), f: &'m Func, i: &Instr) -> Option<Lat> {
        match &i.kind {
            Kind::Step(_) => None,
            Kind::Const(c) => Some(Lat::T(c.ty())),
            Kind::Prelude(_) => Some(Lat::dynamic()),
            Kind::Load(v) => Some(self.load_slot(key, f, *v)),
            Kind::Decl(..) | Kind::Store(..) => None,
            // ops.rs:158-170: int to Int, float to Float, else E403.
            Kind::Unary(UnOp::Neg, x) => Some(match self.val(key, *x) {
                Lat::T(Ty::Int) => Lat::T(Ty::Int),
                Lat::T(Ty::Float) => Lat::T(Ty::Float),
                _ => Lat::dynamic(),
            }),
            // Value::truthy always gives a bool, so `not x` is always Bool.
            Kind::Unary(UnOp::Not, _) => Some(Lat::T(Ty::Bool)),
            Kind::Binary(op, a, b) => Some(bin(*op, self.val(key, *a), self.val(key, *b))),
            // `&&`/`||`: the join of the subject's type and the rhs
            // block's yield. `??`: the rhs when the subject is `Null`,
            // the subject when it is any other concrete type, the join
            // when it is `Dynamic`. `?.`: `Dynamic`.
            Kind::Short { op, subject, rhs } => {
                let sub = self.val(key, *subject);
                let y = self.yield_of(key, f, rhs);
                Some(match op {
                    ShortOp::And | ShortOp::Or => sub.join(y),
                    ShortOp::Coalesce => match sub {
                        Lat::T(Ty::Null) => y,
                        Lat::T(Ty::Dynamic) => sub.join(y),
                        Lat::Bot => y,
                        other => other,
                    },
                    ShortOp::Safe => Lat::dynamic(),
                })
            }
            Kind::MakeList(_) => Some(Lat::T(Ty::List)),
            Kind::MakeMap(_) => Some(Lat::T(Ty::Map)),
            Kind::Key(_) => Some(Lat::T(Ty::Str)),
            Kind::Interp(_) => Some(Lat::T(Ty::Str)),
            Kind::Closure(_) => Some(Lat::T(Ty::Func)),
            Kind::Member { .. }
            | Kind::Index { .. }
            | Kind::SetIndex { .. }
            | Kind::SetMember { .. }
            | Kind::CallBuiltin { .. }
            | Kind::CallValue { .. }
            | Kind::CallMethod { .. }
            | Kind::BurnOp { .. }
            | Kind::MakeChain { .. }
            | Kind::PackDecl(_)
            | Kind::Exhale(_) => Some(Lat::dynamic()),
            Kind::CallUser { func: g, args } => {
                let types: Vec<Lat> = args.iter().map(|a| self.val(key, *a)).collect();
                // An argument nothing has typed yet (the result of a call
                // whose function is not walked yet, a slot a later
                // statement of a loop stores): there is no row to ask
                // for. Asking with `Dynamic` in its place would make a
                // row no run enters, and count it against `MAX_ROWS`.
                // The walk comes back here once it is typed: this spec
                // is walked again when a slot of it or a callee's result
                // moves. An argument that never is typed is in code that
                // cannot run, and the call has no entry in `calls`.
                if types.contains(&Lat::Bot) {
                    return Some(Lat::Bot);
                }
                let row = Row(types.into_iter().map(Lat::out).collect());
                // A `Func` whose arity is not the call's argument count
                // is not specialized: the run raises E401.
                let arity = self
                    .m
                    .funcs
                    .get(g.0 as usize)
                    .map(|fn_| fn_.params.len())
                    .unwrap_or(0);
                if args.len() != arity {
                    if let Some(s) = self.specs.get_mut(key) {
                        s.calls.insert(i.id, (*g, row));
                    }
                    return Some(Lat::dynamic());
                }
                let effective = self.ask(*g, row);
                // Record the dependency before reading the callee.
                self.rdeps
                    .entry((*g, effective.clone()))
                    .or_default()
                    .insert(key.clone());
                let ret = self
                    .specs
                    .get(&(*g, effective.clone()))
                    .map(|s| s.ret)
                    .unwrap_or(Lat::Bot);
                if let Some(s) = self.specs.get_mut(key) {
                    s.calls.insert(i.id, (*g, effective));
                }
                Some(ret)
            }
            Kind::Burn(_)
            | Kind::Try { .. }
            | Kind::If { .. }
            | Kind::Loop { .. }
            | Kind::ForEach { .. } => None,
            Kind::Yield(_) | Kind::Ret(_) | Kind::Break | Kind::Continue | Kind::Throw(_) => None,
        }
    }

    fn val(&self, key: &(FuncId, Row), v: ValueId) -> Lat {
        self.specs
            .get(key)
            .and_then(|s| s.vals.get(v.0 as usize).copied())
            .unwrap_or(Lat::Bot)
    }

    fn join_slot(&mut self, key: &(FuncId, Row), v: VarId, t: Lat) {
        if let Some(s) = self.specs.get_mut(key) {
            let cur = s.slots.get(&v).copied().unwrap_or(Lat::Bot);
            s.slots.insert(v, cur.join(t));
        }
    }

    /// The yield of a short-circuit rhs block (a value block whose last
    /// instruction is a `Yield`). Walk it first (it is also walked by
    /// the outer walk), then read the yielded value's type.
    fn yield_of(&mut self, key: &(FuncId, Row), f: &'m Func, b: &Block) -> Lat {
        self.walk_block(key, f, b, true);
        for i in b.instrs.iter().rev() {
            if let Kind::Yield(v) = &i.kind {
                return self.val(key, *v);
            }
        }
        Lat::Bot
    }

    /// The type of a slot read:
    /// - any slot that is `mutable && captured` (`VarInfo`) is
    ///   `Dynamic` everywhere — the first check below wins on both
    ///   owner and non-owner reads;
    /// - owned by `f`: the spec's `slots` entry. V1 (the verifier) and
    ///   `Builder::early_use` (`src/lower/cir/lower.rs`) make every
    ///   load of a slot in its owner run after the slot's `Decl`, so
    ///   the entry is the right value for this row;
    /// - not owned by `f` (a capture): the IR's own static type for the
    ///   slot's one `Decl` value (`Func::vals[val].ty` of that `Decl`
    ///   in the owner). That type depends on no row and no spec, so it
    ///   cannot go stale; anything the lowering could not type
    ///   statically is `Dynamic`.
    fn load_slot(&self, key: &(FuncId, Row), f: &'m Func, v: VarId) -> Lat {
        if self.cap_mut_dyn.contains(&v) {
            return Lat::dynamic();
        }
        let info = match self.m.vars.get(v.0 as usize) {
            Some(info) => info,
            None => return Lat::dynamic(),
        };
        if info.owner == f.id {
            return self
                .specs
                .get(key)
                .and_then(|s| s.slots.get(&v).copied())
                .unwrap_or(Lat::Bot);
        }
        // Non-owner: it is a capture. `mutable && captured` was handled
        // above, so this is immutable. The IR's static type of its one
        // `Decl` value is a row-free, spec-free fact.
        Lat::T(self.cap_decl_ty.get(&v).copied().unwrap_or(Ty::Dynamic))
    }

    /// Any `(FuncId, Row)` still referenced by a surviving spec's
    /// `calls` table but whose callee row has collapsed is rewritten to
    /// the one all-`Dynamic` row.
    fn collapsed_sweep(&mut self) {
        let keys: Vec<(FuncId, Row)> = self.specs.keys().cloned().collect();
        for k in keys {
            let calls = match self.specs.get(&k) {
                Some(s) => s.calls.clone(),
                None => continue,
            };
            let mut out = BTreeMap::new();
            for (ins, (g, row)) in calls {
                if self.collapsed.contains(&g) {
                    out.insert(ins, (g, dyn_row_of(self.m, g)));
                } else {
                    out.insert(ins, (g, row));
                }
            }
            if let Some(s) = self.specs.get_mut(&k) {
                s.calls = out;
            }
        }
    }

    fn into_specs(self) -> Vec<Spec> {
        self.specs
            .into_values()
            .filter(|s| !s.dead)
            .map(|s| Spec {
                func: s.func,
                row: s.row,
                vals: s.vals.into_iter().map(Lat::out).collect(),
                slots: s.slots.into_iter().map(|(v, t)| (v, t.out())).collect(),
                ret: s.ret.out(),
                calls: s.calls,
            })
            .collect()
    }
}

fn dyn_row_of(m: &Module, func: FuncId) -> Row {
    let arity = m
        .funcs
        .get(func.0 as usize)
        .map(|f| f.params.len())
        .unwrap_or(0);
    Row(vec![Ty::Dynamic; arity])
}

/// If `v` is defined in `f` as `Binary(Range, a, b)`, return `(a, b)`.
fn range_operands(f: &Func, v: ValueId) -> Option<(ValueId, ValueId)> {
    fn find(b: &Block, v: ValueId) -> Option<(ValueId, ValueId)> {
        for i in &b.instrs {
            if i.result == Some(v) {
                if let Kind::Binary(BinOp::Range, a, b) = &i.kind {
                    return Some((*a, *b));
                }
            }
            for sub in i.kind.blocks() {
                if let Some(x) = find(sub, v) {
                    return Some(x);
                }
            }
        }
        None
    }
    find(&f.body, v)
}

/// Pre-pass: fill `out` with the IR's own static type for each slot's
/// `Decl` value in `f`. A slot with no `Decl` in `f` is not touched
/// here (a parameter's `Decl` is elsewhere: it has none in its own
/// function). A value the lowering could not type statically gives
/// `Ty::Dynamic`.
fn collect_decl_tys(f: &Func, b: &Block, out: &mut BTreeMap<VarId, Ty>) {
    for i in &b.instrs {
        if let Kind::Decl(v, val) = &i.kind {
            let t = f
                .vals
                .get(val.0 as usize)
                .map(|x| x.ty)
                .unwrap_or(Ty::Dynamic);
            // V1: one `Decl` per slot; if two walk paths disagree, the
            // first wins — the result is the lowering's own state.
            out.entry(*v).or_insert(t);
        }
        for sub in i.kind.blocks() {
            collect_decl_tys(f, sub, out);
        }
    }
}

/// Binary transfer, read case by case from `src/interp/ops.rs`.
///
/// - `Add` (ops.rs:31-54): Int+Int -> Int (overflow -> E503); Str+Str -> Str;
///   List+List -> List; Int+Float / Float+Int / Float+Float -> Float; else
///   E403.
/// - `Sub`, `Mul` (ops.rs:55-87): Int+Int -> Int; Int/Float/Float -> Float;
///   Mul of Str+Int or Int+Str -> Str (left as Dynamic here because either
///   side can be Str without being the string one); else E403.
/// - `Div` (ops.rs:88-109): Int/Int -> Dynamic (Int when exact, Float when
///   not); Float/Float / Int+Float -> Float (division by 0 -> E502).
/// - `Rem` (ops.rs:110-123): Int%Int -> Int (0 -> E502); Float%Float ->
///   Float; else E403.
/// - `Eq`, `Ne` (ops.rs:124-125): always Bool, whatever the operands.
/// - `Lt`/`Le`/`Gt`/`Ge` (ops.rs:126-143): when they return, Bool; the
///   general `Int,Int`/`Str,Str`/mixed-numeric case; else E403 or NaN.
/// - `Range` (ops.rs:144-150): Int..Int -> List; else E403.
fn bin(op: BinOp, a: Lat, b: Lat) -> Lat {
    use Ty::*;
    let dynamic = Lat::dynamic();
    let (a, b) = match (a, b) {
        (Lat::Bot, _) | (_, Lat::Bot) => return Lat::Bot,
        (Lat::T(Dynamic), _) | (_, Lat::T(Dynamic)) => {
            // One side is Dynamic. A few ops still fix the result type.
            return match op {
                BinOp::Eq | BinOp::Ne => Lat::T(Bool),
                _ => dynamic,
            };
        }
        (Lat::T(a), Lat::T(b)) => (a, b),
    };
    match op {
        BinOp::Add => match (a, b) {
            (Int, Int) => Lat::T(Int),
            (Str, Str) => Lat::T(Str),
            (List, List) => Lat::T(List),
            (Int, Float) | (Float, Int) | (Float, Float) => Lat::T(Float),
            _ => dynamic,
        },
        BinOp::Sub => match (a, b) {
            (Int, Int) => Lat::T(Int),
            (Int, Float) | (Float, Int) | (Float, Float) => Lat::T(Float),
            _ => dynamic,
        },
        BinOp::Mul => match (a, b) {
            (Int, Int) => Lat::T(Int),
            (Int, Float) | (Float, Int) | (Float, Float) => Lat::T(Float),
            // Str*Int or Int*Str -> Str; conservative dynamic covers the
            // mixed cases that do not fit a single result type.
            _ => dynamic,
        },
        BinOp::Div => match (a, b) {
            (Int, Int) => dynamic, // Int when exact, Float when not.
            (Int, Float) | (Float, Int) | (Float, Float) => Lat::T(Float),
            _ => dynamic,
        },
        BinOp::Rem => match (a, b) {
            (Int, Int) => Lat::T(Int),
            (Int, Float) | (Float, Int) | (Float, Float) => Lat::T(Float),
            _ => dynamic,
        },
        BinOp::Eq | BinOp::Ne => Lat::T(Bool),
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => match (a, b) {
            (Int, Int) | (Str, Str) => Lat::T(Bool),
            (Int, Float) | (Float, Int) | (Float, Float) => Lat::T(Bool),
            _ => dynamic,
        },
        BinOp::Range => match (a, b) {
            (Int, Int) => Lat::T(List),
            _ => dynamic,
        },
        // The short-circuit operators are not reached here: lowering
        // emits them as `Kind::Short`, not `Kind::Binary`.
        BinOp::And | BinOp::Or | BinOp::Coalesce => dynamic,
    }
}
