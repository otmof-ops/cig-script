// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
//! The verifier (cig-compile#3). `verify` is structural and runs after lowering and
//! after every pass; `verify_transition` compares a module with a snapshot
//! taken before a pass, because an output alone cannot show that something
//! was moved or reordered. Every error names its rule, its function and
//! its span.
//!
//! | rule | holds |
//! |---|---|
//! | V1 | ids are unique, and a value is used only where it is defined; a slot read as pure (a `stick` read in the function that owns it) is read only after its declaration |
//! | V2 | every instruction and function has a span; an index, an indexed store and a map's computed keys carry their operands' own spans, each inside the instruction's |
//! | V3 | a terminator ends its block; `yield` ends a value block, and only one; `break` and `continue` sit in loops; the top level does not `ret` |
//! | V4 | a `burn.op` sits in a burn, or in a function that requires one |
//! | V5 | `requires_burn` is what the call graph says |
//! | V6 | (a warning) an effectful pull is called outside every burn |
//! | V7 | a `burn.op`'s kind is its callee's |
//! | V8 | regions are unique; `pack` is declared once, in the top level's own block, before any burn (E753); a compensation names only its state, what it declares and what the functions made in it own, and calls only those functions (E754) |
//! | V9 | a variable is its function's or captured by it; only a mutable one is stored to; a closure's captures resolve where it is made; `captured` is set exactly when another function captures the variable |
//! | V10 | the top level is the first function and is never made; any other is made at one place at most; `call.user` calls a pull that is made |
//! | V11 | every loop body opens with exactly one `step.iter`, and no `step.iter` sits anywhere else; no step mark sits in a value block |
//! | T1 | no pinned instruction (an effect, an operation that may raise, a step mark: anything `Kind::class` does not call pure) changes its place: every block around it, so its burn region, its branch of an `if`, its loop, its `try` handler, and a burn's body or compensation |
//! | T2 | no pinned instruction is reordered |
//! | T3 | no effect (a `burn.op`, an `exhale`, a store, a call, ...) or step mark is lost or cloned; one goes only with its whole block, when that block is an `if` arm its constant condition never takes or its owner went too |
//! | T4 | an operation that may raise is removed only when the verifier proves it cannot (constant operands that `fold_binary` folds, and a fold left in its place is that constant), or with its whole block as T3 allows |
//!
//! Pure instructions are free: T1 to T4 let them move, merge or vanish,
//! and V1 keeps every use after its definition.
//!
//! Not seen by any rule: an effect reached dynamically (through `light`, a
//! method that takes a callback, a function kept in a map) is a call here,
//! not a `burn.op`. The runtime's E701 is its backstop, as it is the
//! interpreter's.

use super::effects::kind_of;
use super::ir::*;
use crate::syntax::span::Span;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warn,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifyError {
    /// The rule: `V1`..`V11`, `T1`..`T4`.
    pub code: &'static str,
    pub severity: Severity,
    pub func: String,
    pub span: Span,
    pub message: String,
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {} in {} at {}:{}: {}",
            if self.severity == Severity::Warn {
                "warning"
            } else {
                "error"
            },
            self.code,
            self.func,
            self.span.line,
            self.span.col,
            self.message
        )
    }
}

fn fname(f: &Func) -> String {
    match (&f.kind, &f.name) {
        (FuncKind::Main, _) => "main".to_string(),
        (_, Some(n)) => n.clone(),
        (_, None) => format!("lambda@{}", f.id.0),
    }
}

/// Which functions require a burn: the least fixpoint of "has an effect
/// outside its own burns, or calls (outside its burns) one that does".
/// A closure's body is its own function: making it is not calling it.
pub fn requires_burn(m: &Module) -> Vec<bool> {
    let n = m.funcs.len();
    let mut direct = vec![false; n];
    let mut calls: Vec<Vec<usize>> = vec![Vec::new(); n];
    for f in &m.funcs {
        let i = f.id.0 as usize;
        scan(&f.body, 0, &mut |instr, depth| {
            if depth == 0 {
                match &instr.kind {
                    Kind::BurnOp { .. } => direct[i] = true,
                    Kind::CallUser { func, .. } => calls[i].push(func.0 as usize),
                    _ => {}
                }
            }
        });
    }
    let mut flag = direct;
    loop {
        let mut changed = false;
        for i in 0..n {
            if !flag[i]
                && calls[i]
                    .iter()
                    .any(|c| flag.get(*c).copied().unwrap_or(false))
            {
                flag[i] = true;
                changed = true;
            }
        }
        if !changed {
            return flag;
        }
    }
}

/// Visit every instruction with its burn depth.
fn scan(b: &Block, depth: usize, f: &mut dyn FnMut(&Instr, usize)) {
    for i in &b.instrs {
        f(i, depth);
        let inner = if matches!(i.kind, Kind::Burn(_)) {
            depth + 1
        } else {
            depth
        };
        for blk in i.kind.blocks() {
            scan(blk, inner, f);
        }
    }
}

/// Every instruction of a block, nested blocks included; not the bodies of
/// the functions it makes, which are their own.
fn each<'a>(b: &'a Block, f: &mut dyn FnMut(&'a Instr)) {
    for i in &b.instrs {
        f(i);
        for blk in i.kind.blocks() {
            each(blk, f);
        }
    }
}

/// The operand spans an instruction carries, by name: where the
/// tree-walker raises an error about that operand.
fn operand_spans(k: &Kind) -> Vec<(&'static str, Span)> {
    match k {
        Kind::Index { idx_at, .. } => vec![("index", *idx_at)],
        Kind::SetIndex { obj_at, idx_at, .. } => vec![("object", *obj_at), ("index", *idx_at)],
        Kind::SetMember { obj_at, .. } => vec![("object", *obj_at)],
        Kind::MakeMap(kv) => kv
            .iter()
            .filter_map(|(k, _)| match k {
                MapKey::Dyn(_, at) => Some(("key", *at)),
                MapKey::Static(_) => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// The variables an instruction declares.
fn declared(k: &Kind) -> Vec<VarId> {
    match k {
        Kind::Decl(v, _) => vec![*v],
        Kind::ForEach { var, .. } => vec![*var],
        Kind::MakeChain { name, .. } => vec![*name],
        Kind::Try {
            catch: Some((Some(v), _)),
            ..
        } => vec![*v],
        Kind::Burn(b) => b.state.into_iter().collect(),
        _ => Vec::new(),
    }
}

/// The variables an instruction names: what it reads, writes or declares.
fn named(k: &Kind) -> Vec<VarId> {
    match k {
        Kind::Load(v) | Kind::Store(v, _) => vec![*v],
        other => declared(other),
    }
}

struct Walk<'m> {
    m: &'m Module,
    f: &'m Func,
    errors: Vec<VerifyError>,
    seen_instr: &'m mut HashSet<InstrId>,
    defined: HashSet<ValueId>,
    regions: &'m mut HashSet<RegionId>,
    flags: &'m [bool],
    /// How many `closure` instructions make each function.
    made: &'m HashMap<FuncId, usize>,
    /// The slots declared on the way here: parameters, `decl`s and chains
    /// before this point, a loop variable, a catch variable, a state.
    slots: HashSet<VarId>,
}

impl Walk<'_> {
    fn err(&mut self, code: &'static str, span: Span, message: String) {
        self.errors.push(VerifyError {
            code,
            severity: Severity::Error,
            func: fname(self.f),
            span,
            message,
        });
    }

    fn warn(&mut self, code: &'static str, span: Span, message: String) {
        self.errors.push(VerifyError {
            code,
            severity: Severity::Warn,
            func: fname(self.f),
            span,
            message,
        });
    }

    /// The variable is this function's, or captured by it.
    fn reaches(&self, v: VarId) -> bool {
        self.m
            .vars
            .get(v.0 as usize)
            .is_some_and(|x| x.owner == self.f.id)
            || self.f.captures.contains(&v)
    }

    fn var_ok(&mut self, v: VarId, span: Span) {
        let Some(info) = self.m.vars.get(v.0 as usize) else {
            self.err("V9", span, format!("${} is not a variable", v.0));
            return;
        };
        if !self.reaches(v) {
            self.err(
                "V9",
                span,
                format!(
                    "${}:{} is neither this function's nor captured by it",
                    v.0, info.name
                ),
            );
        }
    }

    /// V8: a compensation runs later, in another process, from the journal
    /// alone (E754). It may name its burn's state, what it declares itself
    /// and what the functions made inside it own, and call only those
    /// functions.
    fn compensation(&mut self, state: Option<VarId>, comp: &Block) {
        let m = self.m;
        // The functions made inside it, and inside those.
        let mut inner: HashSet<FuncId> = HashSet::new();
        let mut todo: Vec<&Block> = vec![comp];
        while let Some(b) = todo.pop() {
            each(b, &mut |i| {
                if let Kind::Closure(g) = i.kind {
                    if inner.insert(g) {
                        if let Some(func) = m.funcs.get(g.0 as usize) {
                            todo.push(&func.body);
                        }
                    }
                }
            });
        }
        let mut allowed: HashSet<VarId> = state.into_iter().collect();
        each(comp, &mut |i| allowed.extend(declared(&i.kind)));
        allowed.extend(
            m.vars
                .iter()
                .enumerate()
                .filter(|(_, v)| inner.contains(&v.owner))
                .map(|(k, _)| VarId(k as u32)),
        );
        let name = |v: VarId| m.vars.get(v.0 as usize).map_or("?", |x| x.name.as_str());
        let mut found: Vec<(Span, String)> = Vec::new();
        each(comp, &mut |i| {
            for v in named(&i.kind) {
                if !allowed.contains(&v) {
                    found.push((
                        i.span,
                        format!(
                            "the compensation names ${}:{}, which is neither its state nor its own",
                            v.0,
                            name(v)
                        ),
                    ));
                }
            }
            if let Kind::CallUser { func, .. } = i.kind {
                if !inner.contains(&func) {
                    found.push((
                        i.span,
                        format!("the compensation calls @{}, which it does not make", func.0),
                    ));
                }
            }
        });
        let mut made_inside: Vec<FuncId> = inner.iter().copied().collect();
        made_inside.sort();
        for g in made_inside {
            let Some(func) = m.funcs.get(g.0 as usize) else {
                continue;
            };
            for v in &func.captures {
                if !allowed.contains(v) {
                    found.push((
                        func.span,
                        format!(
                            "@{} in the compensation captures ${}:{}, which is neither its state nor its own",
                            g.0,
                            v.0,
                            name(*v)
                        ),
                    ));
                }
            }
            each(&func.body, &mut |i| {
                if let Kind::CallUser { func: c, .. } = i.kind {
                    if !inner.contains(&c) {
                        found.push((
                            i.span,
                            format!(
                                "@{} in the compensation calls @{}, which it does not make",
                                g.0, c.0
                            ),
                        ));
                    }
                }
            });
        }
        for (span, message) in found {
            self.err("V8", span, message);
        }
    }

    /// V11: a loop body opens with its one `step.iter`; nothing else has
    /// one; a value block (a condition, a short-circuit's right side) has
    /// no step mark at all, since it holds no statement.
    fn steps(&mut self, b: &Block, loop_body: bool, yields: bool) {
        let mut iters = 0usize;
        let mut ordered_before = false;
        for i in &b.instrs {
            match i.kind {
                Kind::Step(_) if yields => {
                    self.err("V11", i.span, "a step mark in a value block".to_string())
                }
                Kind::Step(StepSite::Iter) if !loop_body => self.err(
                    "V11",
                    i.span,
                    "a step.iter outside a loop body's head".to_string(),
                ),
                Kind::Step(StepSite::Iter) => {
                    iters += 1;
                    if iters > 1 {
                        self.err(
                            "V11",
                            i.span,
                            "a second step.iter in one loop body".to_string(),
                        );
                    } else if ordered_before {
                        self.err(
                            "V11",
                            i.span,
                            "the loop body's step.iter is not its first pinned instruction"
                                .to_string(),
                        );
                    }
                }
                _ => {}
            }
            if i.kind.class(self.f, &self.m.vars).is_pinned() {
                ordered_before = true;
            }
        }
        if loop_body && iters == 0 {
            let span = b.instrs.first().map(|i| i.span).unwrap_or(self.f.span);
            self.err(
                "V11",
                span,
                "a loop body without its step.iter: every iteration counts a step".to_string(),
            );
        }
    }

    /// `visible`: the values in scope here. `yields`: this block must end
    /// in `yield` (and only such a block may). `loop_body`: the body of a
    /// `loop` or `foreach`, which opens with its `step.iter`.
    fn block(
        &mut self,
        b: &Block,
        visible: &HashSet<ValueId>,
        burn_depth: usize,
        loop_depth: usize,
        yields: bool,
        loop_body: bool,
    ) {
        let m = self.m;
        let mut scope = visible.clone();
        let entry = self.slots.clone();
        let n = b.instrs.len();
        self.steps(b, loop_body, yields);
        for (k, i) in b.instrs.iter().enumerate() {
            let last = k + 1 == n;
            // V1: ids.
            if !self.seen_instr.insert(i.id) {
                self.err(
                    "V1",
                    i.span,
                    format!("instruction #{} appears twice", i.id.0),
                );
            }
            for u in i.kind.uses() {
                if !scope.contains(&u) {
                    self.err(
                        "V1",
                        i.span,
                        format!("%{} is used where it is not defined", u.0),
                    );
                }
            }
            // V1: a pure read of a slot follows its declaration, since a
            // pass may move it anywhere its value is in scope.
            if let Kind::Load(v) = &i.kind {
                if i.kind.class(self.f, &m.vars) == Class::Pure && !self.slots.contains(v) {
                    self.err(
                        "V1",
                        i.span,
                        format!("${} is read before its declaration", v.0),
                    );
                }
            }
            // V2: spans.
            if i.span.line < 1 || i.span.start > i.span.end {
                self.err("V2", i.span, "an instruction without a span".to_string());
            }
            for (name, at) in operand_spans(&i.kind) {
                let inside = at.start >= i.span.start && at.end <= i.span.end;
                if at.line < 1 || at.start > at.end || !inside {
                    self.err(
                        "V2",
                        i.span,
                        format!(
                            "the {name} span {}:{} is not a span inside the instruction's",
                            at.line, at.col
                        ),
                    );
                }
            }
            // V3: terminators.
            if i.kind.is_terminator() && !last {
                self.err(
                    "V3",
                    i.span,
                    "a terminator before the end of its block".to_string(),
                );
            }
            match &i.kind {
                Kind::Yield(_) if !yields || !last => {
                    self.err("V3", i.span, "a yield outside a value block".to_string())
                }
                Kind::Break | Kind::Continue if loop_depth == 0 => {
                    self.err("V3", i.span, "break or continue outside a loop".to_string())
                }
                Kind::Ret(_) if self.f.kind == FuncKind::Main => {
                    self.err("V3", i.span, "a return in the top level".to_string())
                }
                _ => {}
            }
            // V4, V7: effects only in regions, and as the table says.
            if let Kind::BurnOp {
                kind,
                callee,
                effect,
                ..
            } = &i.kind
            {
                // Outside the top level, an effect outside every burn is
                // what makes its function require one (V5 derives the flag
                // from it), so this cannot fail there: a pass that moves an
                // effect out of a burn inside a pull is T1's to catch.
                let allowed = burn_depth > 0
                    || (self.f.kind != FuncKind::Main && self.flags[self.f.id.0 as usize]);
                if !allowed {
                    self.err(
                        "V4",
                        i.span,
                        format!("burn.op {} ({callee}) outside every burn", kind.name()),
                    );
                }
                if kind_of(callee) != Some(*kind) || !effect.needs_burn() {
                    self.err(
                        "V7",
                        i.span,
                        format!("burn.op {} does not match {callee}", kind.name()),
                    );
                }
            }
            // V6: a lint, never an error: an effectful pull called outside
            // any burn reaches E701 at run time, as the interpreter's does.
            if let Kind::CallUser { func, .. } = &i.kind {
                if burn_depth == 0
                    && self.f.kind == FuncKind::Main
                    && self.flags.get(func.0 as usize).copied().unwrap_or(false)
                {
                    self.warn(
                        "V6",
                        i.span,
                        format!("calls @{} (which effects) outside any burn", func.0),
                    );
                }
                // V10: what it calls is a pull, made somewhere.
                match m.funcs.get(func.0 as usize) {
                    None => self.err("V10", i.span, format!("@{} is not a function", func.0)),
                    Some(g) if g.kind != FuncKind::Pull => self.err(
                        "V10",
                        i.span,
                        format!("call.user of @{}, which is not a pull", func.0),
                    ),
                    Some(_) if !self.made.contains_key(func) => self.err(
                        "V10",
                        i.span,
                        format!("calls @{}, which nothing makes", func.0),
                    ),
                    Some(_) => {}
                }
            }
            // V9: every variable it names.
            for v in named(&i.kind) {
                self.var_ok(v, i.span);
            }
            if let Kind::Store(v, _) = &i.kind {
                if !m.vars.get(v.0 as usize).is_some_and(|x| x.mutable) {
                    self.err("V9", i.span, format!("a store to the immutable ${}", v.0));
                }
            }
            // V8, V9, V10.
            match &i.kind {
                Kind::Burn(b) => {
                    if b.region == RegionId::ROOT || !self.regions.insert(b.region) {
                        self.err(
                            "V8",
                            i.span,
                            format!("region r{} is not unique", b.region.0),
                        );
                    }
                    if let Some(c) = &b.compensate {
                        self.compensation(b.state, c);
                    }
                }
                Kind::Closure(g) => match m.funcs.get(g.0 as usize) {
                    None => self.err(
                        "V10",
                        i.span,
                        format!("closure of @{}, which is not a function", g.0),
                    ),
                    Some(func) if func.kind == FuncKind::Main => {
                        self.err("V10", i.span, "a closure of the top level".to_string())
                    }
                    Some(func) => {
                        for v in &func.captures {
                            if !self.reaches(*v) {
                                self.err(
                                    "V9",
                                    i.span,
                                    format!(
                                        "closure of @{} captures ${}, which this function neither owns nor captures",
                                        g.0, v.0
                                    ),
                                );
                            }
                        }
                    }
                },
                _ => {}
            }
            // Nested blocks.
            let inner_burn = if matches!(i.kind, Kind::Burn(_)) {
                burn_depth + 1
            } else {
                burn_depth
            };
            let outer = self.slots.clone();
            if !matches!(i.kind, Kind::Decl(..) | Kind::MakeChain { .. }) {
                self.slots.extend(declared(&i.kind));
            }
            match &i.kind {
                Kind::Short { rhs, .. } => {
                    self.block(rhs, &scope, inner_burn, loop_depth, true, false)
                }
                Kind::Loop { cond, body } => {
                    self.block(cond, &scope, inner_burn, loop_depth, true, false);
                    self.block(body, &scope, inner_burn, loop_depth + 1, false, true);
                }
                Kind::ForEach { body, .. } => {
                    self.block(body, &scope, inner_burn, loop_depth + 1, false, true)
                }
                other => {
                    for blk in other.blocks() {
                        self.block(blk, &scope, inner_burn, loop_depth, false, false);
                    }
                }
            }
            self.slots = outer;
            if matches!(i.kind, Kind::Decl(..) | Kind::MakeChain { .. }) {
                self.slots.extend(declared(&i.kind));
            }
            if let Some(r) = i.result {
                if !self.defined.insert(r) {
                    self.err("V1", i.span, format!("%{} is defined twice", r.0));
                }
                if self.f.vals.get(r.0 as usize).is_none_or(|v| v.def != i.id) {
                    self.err(
                        "V1",
                        i.span,
                        format!("%{}'s record does not name this instruction", r.0),
                    );
                }
                scope.insert(r);
            }
        }
        self.slots = entry;
        if yields
            && !b
                .instrs
                .last()
                .is_some_and(|i| matches!(i.kind, Kind::Yield(_)))
        {
            let span = b.instrs.last().map(|i| i.span).unwrap_or(self.f.span);
            self.err(
                "V3",
                span,
                "a value block that does not end in yield".to_string(),
            );
        }
    }
}

/// V8: `pack` is declared once, in the top level's own block, before any
/// burn (E753): the scope is declared before anything can burn.
fn pack(m: &Module, errors: &mut Vec<VerifyError>) {
    let mut all = 0;
    for f in &m.funcs {
        each(&f.body, &mut |i| {
            if matches!(i.kind, Kind::PackDecl(_)) {
                all += 1;
            }
        });
    }
    let main = &m.funcs[0];
    let top: Vec<usize> = main
        .body
        .instrs
        .iter()
        .enumerate()
        .filter(|(_, i)| matches!(i.kind, Kind::PackDecl(_)))
        .map(|(k, _)| k)
        .collect();
    let mk = |span: Span, message: &str| VerifyError {
        code: "V8",
        severity: Severity::Error,
        func: "main".to_string(),
        span,
        message: message.to_string(),
    };
    if all > top.len() {
        errors.push(mk(main.span, "pack declared inside a block or a function"));
    }
    if let Some(k) = top.get(1) {
        errors.push(mk(main.body.instrs[*k].span, "a second pack"));
    }
    if let Some(k) = top.first() {
        let mut burned = None;
        for i in &main.body.instrs[..*k] {
            each_with(i, &mut |x| {
                if burned.is_none() && matches!(x.kind, Kind::Burn(_) | Kind::BurnOp { .. }) {
                    burned = Some(x.span);
                }
            });
        }
        if let Some(span) = burned {
            errors.push(mk(span, "a burn before the pack is declared"));
        }
    }
}

/// One instruction and everything nested in it.
fn each_with<'a>(i: &'a Instr, f: &mut dyn FnMut(&'a Instr)) {
    f(i);
    for blk in i.kind.blocks() {
        each(blk, f);
    }
}

/// Every structural rule, every error (never only the first).
pub fn verify(m: &Module) -> Vec<VerifyError> {
    let mut errors = Vec::new();
    let v10 = |message: String| VerifyError {
        code: "V10",
        severity: Severity::Error,
        func: "main".to_string(),
        span: Span::new(0, 0, 1, 1),
        message,
    };
    if m.funcs.first().is_none_or(|f| f.kind != FuncKind::Main) {
        errors.push(v10("the first function is not the top level".to_string()));
        return errors;
    }
    if let Some((k, f)) = m
        .funcs
        .iter()
        .enumerate()
        .find(|(k, f)| f.id.0 as usize != *k)
    {
        errors.push(v10(format!("function {k} carries the id @{}", f.id.0)));
        return errors;
    }
    let flags = requires_burn(m);
    let mut made: HashMap<FuncId, usize> = HashMap::new();
    for f in &m.funcs {
        each(&f.body, &mut |i| {
            if let Kind::Closure(g) = i.kind {
                *made.entry(g).or_default() += 1;
            }
        });
    }
    let mut seen_instr = HashSet::new();
    let mut regions = HashSet::new();
    for f in &m.funcs {
        // V5: the stored flag is the derived one; the top level never.
        let want = flags[f.id.0 as usize] && f.kind != FuncKind::Main;
        if f.requires_burn != want {
            errors.push(VerifyError {
                code: "V5",
                severity: Severity::Error,
                func: fname(f),
                span: f.span,
                message: format!(
                    "requires_burn is {}, the call graph says {want}",
                    f.requires_burn
                ),
            });
        }
        // V10: one function, one place that makes it.
        if let Some(n) = made.get(&f.id).filter(|n| **n > 1) {
            errors.push(VerifyError {
                code: "V10",
                severity: Severity::Error,
                func: fname(f),
                span: f.span,
                message: format!("@{} is made at {n} places", f.id.0),
            });
        }
        let mut w = Walk {
            m,
            f,
            errors: Vec::new(),
            seen_instr: &mut seen_instr,
            defined: HashSet::new(),
            regions: &mut regions,
            flags: &flags,
            made: &made,
            slots: f.params.iter().copied().collect(),
        };
        if f.span.line < 1 {
            w.err("V2", f.span, "a function without a span".to_string());
        }
        w.block(&f.body, &HashSet::new(), 0, 0, false, false);
        errors.extend(w.errors);
    }
    // V9: `captured` is set exactly when another function captures it.
    let mut captured: HashSet<VarId> = HashSet::new();
    for f in &m.funcs {
        for v in &f.captures {
            if m.vars.get(v.0 as usize).is_some_and(|x| x.owner != f.id) {
                captured.insert(*v);
            }
        }
    }
    for (k, v) in m.vars.iter().enumerate() {
        let want = captured.contains(&VarId(k as u32));
        if v.captured != want {
            errors.push(VerifyError {
                code: "V9",
                severity: Severity::Error,
                func: m
                    .funcs
                    .get(v.owner.0 as usize)
                    .map(fname)
                    .unwrap_or_default(),
                span: v.span,
                message: format!(
                    "${k}:{} is marked captured: {}, and the functions say {want}",
                    v.name, v.captured
                ),
            });
        }
    }
    pack(m, &mut errors);
    errors
}

/// The errors only (not the V6 warnings).
pub fn errors(found: &[VerifyError]) -> Vec<&VerifyError> {
    found
        .iter()
        .filter(|e| e.severity == Severity::Error)
        .collect()
}

/// Where an instruction sits: every structured instruction around it and
/// which of its blocks, outermost first.
type Place = Vec<(InstrId, &'static str)>;

struct Spot {
    id: InstrId,
    place: Place,
    class: Class,
    burn_op: bool,
    /// For an operation that may raise: what the interpreter computes
    /// from its constant operands, where it cannot fail (T4's proof).
    proof: Option<Const>,
    tag: &'static str,
}

/// One function's trace.
#[derive(Default)]
struct FuncTrace {
    /// Its pinned instructions in order, with their places.
    seq: Vec<Spot>,
    /// Every instruction of each block, by place.
    blocks: HashMap<Place, Vec<InstrId>>,
    /// Every instruction there is, with the constant it is when it is one.
    all: HashMap<InstrId, Option<Const>>,
    /// The arms of an `if` that never run: the condition is a constant
    /// that picks the other one.
    dead_arms: HashSet<(InstrId, &'static str)>,
}

/// A module's trace, taken before a pass and again after it.
pub struct Trace {
    funcs: HashMap<FuncId, FuncTrace>,
}

impl Trace {
    pub fn of(m: &Module) -> Trace {
        let mut funcs = HashMap::new();
        for f in &m.funcs {
            // Every value that is a constant, or that the interpreter's
            // arithmetic computes from constants without failing: T4's
            // proof follows a fold through a chain (`2 + 3 * 4`).
            let mut consts: HashMap<ValueId, Const> = HashMap::new();
            each(&f.body, &mut |i| {
                let Some(r) = i.result else {
                    return;
                };
                let c = match &i.kind {
                    Kind::Const(c) => Some(c.clone()),
                    Kind::Binary(op, a, b) => match (consts.get(a), consts.get(b)) {
                        (Some(x), Some(y)) => fold_binary(*op, x, y),
                        _ => None,
                    },
                    Kind::Unary(op, x) => consts.get(x).and_then(|x| fold_unary(*op, x)),
                    _ => None,
                };
                if let Some(c) = c {
                    consts.insert(r, c);
                }
            });
            let mut t = FuncTrace::default();
            trace(m, f, &consts, &f.body, &mut Vec::new(), &mut t);
            funcs.insert(f.id, t);
        }
        Trace { funcs }
    }
}

fn trace(
    m: &Module,
    f: &Func,
    consts: &HashMap<ValueId, Const>,
    b: &Block,
    place: &mut Place,
    out: &mut FuncTrace,
) {
    out.blocks
        .entry(place.clone())
        .or_default()
        .extend(b.instrs.iter().map(|i| i.id));
    for i in &b.instrs {
        let class = i.kind.class(f, &m.vars);
        out.all.insert(
            i.id,
            match &i.kind {
                Kind::Const(c) => Some(c.clone()),
                _ => None,
            },
        );
        if class.is_pinned() {
            let proof = match &i.kind {
                Kind::Binary(op, a, b) if class == Class::MayRaise => {
                    match (consts.get(a), consts.get(b)) {
                        (Some(x), Some(y)) => fold_binary(*op, x, y),
                        _ => None,
                    }
                }
                Kind::Unary(op, x) if class == Class::MayRaise => {
                    consts.get(x).and_then(|x| fold_unary(*op, x))
                }
                _ => None,
            };
            out.seq.push(Spot {
                id: i.id,
                place: place.clone(),
                class,
                burn_op: matches!(i.kind, Kind::BurnOp { .. }),
                proof,
                tag: tag(&i.kind),
            });
        }
        if let Kind::If { cond, .. } = &i.kind {
            match consts.get(cond) {
                Some(Const::Bool(true)) => {
                    out.dead_arms.insert((i.id, "if.else"));
                }
                Some(Const::Bool(false)) => {
                    out.dead_arms.insert((i.id, "if.then"));
                }
                _ => {}
            }
        }
        for (slot, blk) in i.kind.slots() {
            place.push((i.id, slot));
            trace(m, f, consts, blk, place, out);
            place.pop();
        }
    }
}

/// The mnemonic an error names an instruction by.
fn tag(k: &Kind) -> &'static str {
    match k {
        Kind::Step(StepSite::Stmt) => "step",
        Kind::Step(StepSite::Iter) => "step.iter",
        Kind::Load(_) => "load",
        Kind::Decl(..) => "decl",
        Kind::Store(..) => "store",
        Kind::Unary(..) => "un",
        Kind::Binary(..) => "bin",
        Kind::Short { .. } => "short",
        Kind::MakeMap(_) => "map",
        Kind::Member { .. } => "member",
        Kind::Index { .. } => "index",
        Kind::Key(_) => "key",
        Kind::SetIndex { .. } => "set.index",
        Kind::SetMember { .. } => "set.member",
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
        Kind::Const(_) => "const",
        Kind::Prelude(_) => "prelude",
        Kind::MakeList(_) => "list",
        Kind::Interp(_) => "interp",
        Kind::Closure(_) => "closure",
    }
}

fn show(p: &Place) -> String {
    if p.is_empty() {
        return "the function's own block".to_string();
    }
    p.iter()
        .map(|(id, slot)| format!("#{} {slot}", id.0))
        .collect::<Vec<_>>()
        .join(" > ")
}

/// Did the block at `place` go as a whole, legitimately? Every
/// instruction it held is gone, and either it is an `if` arm its constant
/// condition never takes, or the instruction that owned it went too (whose
/// own removal the same rules judge, one block out). A function's own
/// block never goes.
fn block_went(place: &Place, before: &FuncTrace, after: &FuncTrace) -> bool {
    let Some(owner) = place.last() else {
        return false;
    };
    let emptied = before
        .blocks
        .get(place)
        .is_none_or(|ids| ids.iter().all(|id| !after.all.contains_key(id)));
    emptied && (after.dead_arms.contains(owner) || !after.all.contains_key(&owner.0))
}

/// What a pass may not do, judged against the trace taken before it:
///
/// - T1: move a pinned instruction (anything but a pure one) to another
///   place;
/// - T2: reorder pinned instructions;
/// - T3: lose or clone an effect or a step mark; one goes only with its
///   whole block, when that block never runs or its owner went;
/// - T4: remove an operation that may raise, unless the verifier proves it
///   cannot (its operands are constants and `fold_binary` agrees, and a
///   fold left in its place is that same constant) or its whole block went
///   as T3 allows.
///
/// Pure instructions are free: they may move, merge or vanish.
pub fn verify_transition(before: &Trace, m: &Module) -> Vec<VerifyError> {
    let after = Trace::of(m);
    let mut errors = Vec::new();
    for f in &m.funcs {
        let (Some(bt), Some(at)) = (before.funcs.get(&f.id), after.funcs.get(&f.id)) else {
            continue;
        };
        let (b, a) = (&bt.seq, &at.seq);
        let mk = |code: &'static str, message: String| VerifyError {
            code,
            severity: Severity::Error,
            func: fname(f),
            span: f.span,
            message,
        };
        let before_ids: HashMap<InstrId, &Spot> = b.iter().map(|s| (s.id, s)).collect();
        let after_ids: HashMap<InstrId, &Spot> = a.iter().map(|s| (s.id, s)).collect();
        // T3, T4: what is no longer pinned where it was.
        for s in b {
            if after_ids.contains_key(&s.id) {
                continue;
            }
            let now = at.all.get(&s.id);
            let went = block_went(&s.place, bt, at);
            match s.class {
                Class::MayRaise => {
                    let fine = match (now, &s.proof) {
                        // Still there, and its types now say it cannot raise.
                        (Some(None), _) => true,
                        _ if went => true,
                        (None, Some(_)) => true,
                        (Some(Some(c)), Some(p)) if c == p => true,
                        (Some(Some(c)), Some(p)) => {
                            errors.push(mk(
                                "T4",
                                format!(
                                    "#{} ({}) became const {c:?}; the interpreter computes {p:?}",
                                    s.id.0, s.tag
                                ),
                            ));
                            continue;
                        }
                        _ => false,
                    };
                    if !fine {
                        errors.push(mk(
                            "T4",
                            format!(
                                "#{} ({}) may raise and was removed without a proof that it cannot",
                                s.id.0, s.tag
                            ),
                        ));
                    }
                }
                _ if went => {}
                Class::Step => {
                    let kept = bt
                        .blocks
                        .get(&s.place)
                        .and_then(|ids| ids.iter().find(|id| at.all.contains_key(id)));
                    errors.push(mk(
                        "T3",
                        match kept {
                            Some(k) => format!(
                                "step mark #{} was removed from {}, which keeps #{}",
                                s.id.0,
                                show(&s.place),
                                k.0
                            ),
                            None => format!(
                                "step mark #{} went with {}, a block that still runs",
                                s.id.0,
                                show(&s.place)
                            ),
                        },
                    ));
                }
                _ if s.burn_op => {
                    errors.push(mk("T3", format!("burn.op #{} was removed", s.id.0)));
                }
                _ => {
                    errors.push(mk(
                        "T3",
                        format!(
                            "#{} ({}) was removed from {}",
                            s.id.0,
                            s.tag,
                            show(&s.place)
                        ),
                    ));
                }
            }
        }
        let mut count: HashMap<InstrId, usize> = HashMap::new();
        for s in a {
            *count.entry(s.id).or_default() += 1;
        }
        let mut cloned: Vec<(InstrId, usize, &'static str)> = count
            .into_iter()
            .filter_map(|(id, n)| {
                let was = before_ids.get(&id)?;
                let what = match was.class {
                    Class::Step => "step mark",
                    _ if was.burn_op => "burn.op",
                    _ => was.tag,
                };
                (n > 1).then_some((id, n, what))
            })
            .collect();
        cloned.sort();
        for (id, n, what) in cloned {
            errors.push(mk("T3", format!("{what} #{} appears {n} times", id.0)));
        }
        // T1.
        for s in a {
            if let Some(was) = before_ids.get(&s.id) {
                if was.place != s.place {
                    errors.push(mk(
                        "T1",
                        format!(
                            "#{} moved from {} to {}",
                            s.id.0,
                            show(&was.place),
                            show(&s.place)
                        ),
                    ));
                }
            }
        }
        // T2: the common instructions in the same order.
        let common_b: Vec<InstrId> = b
            .iter()
            .map(|s| s.id)
            .filter(|i| after_ids.contains_key(i))
            .collect();
        let common_a: Vec<InstrId> = a
            .iter()
            .map(|s| s.id)
            .filter(|i| before_ids.contains_key(i))
            .collect();
        if common_b != common_a {
            let at = common_b
                .iter()
                .zip(&common_a)
                .position(|(x, y)| x != y)
                .unwrap_or(0);
            errors.push(mk(
                "T2",
                format!(
                    "pinned instructions were reordered (first at #{})",
                    common_b.get(at).map(|i| i.0).unwrap_or(0)
                ),
            ));
        }
    }
    errors
}
