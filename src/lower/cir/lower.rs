// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
//! Lowering a checked program to CIR, statement by statement as the
//! interpreter evaluates it (eval.rs in CigScript): the right-hand side of
//! an assignment before its target, a call's callee before its arguments,
//! every `pull` of a block hoisted to the block's head, `?.` evaluating
//! nothing after a null subject, a `burn`'s label before its region.
//! A step mark opens every statement but a `pull`, and every loop body,
//! where the interpreter's `tick` counts.

use super::effects::{kind_of, Resolved, Resolver};
use super::ir::*;
use crate::syntax::ast::{
    self, AssignOp, AssignTarget, BinOp, Expr, ExprKind, LambdaBody, Stmt, StrPiece,
};
use crate::syntax::span::Span;
use std::collections::HashMap;

pub type Error = String;

/// A name in a scope. `pending`: allocated when the block was entered, not
/// declared yet: invisible to the block's own straight-line code, visible
/// to a function body (which runs later, as the interpreter's closures
/// resolve names when they run).
#[derive(Clone, Copy)]
struct Entry {
    var: VarId,
    pull: Option<FuncId>,
    pending: bool,
}

struct Builder {
    id: FuncId,
    scopes: Vec<HashMap<String, Entry>>,
    vals: Vec<ValueInfo>,
    captures: Vec<VarId>,
}

struct Lowerer<'r> {
    file: String,
    funcs: Vec<Option<Func>>,
    vars: Vec<VarInfo>,
    next_instr: u32,
    next_region: u32,
    stack: Vec<Builder>,
    resolver: &'r Resolver,
}

enum Name {
    Var(VarId, Option<FuncId>),
    Prelude,
}

/// Lower a program that has passed the checker.
pub fn lower(program: &ast::Program, file: &str, resolver: &Resolver) -> Result<Module, Error> {
    let mut l = Lowerer {
        file: file.to_string(),
        funcs: vec![None],
        vars: Vec::new(),
        next_instr: 0,
        next_region: 0,
        stack: Vec::new(),
        resolver,
    };
    let span = program
        .body
        .first()
        .map(|s| s.span())
        .unwrap_or(Span::new(0, 0, 1, 1));
    let span = Span::new(span.start, span.end, span.line.max(1), span.col.max(1));
    l.stack.push(Builder {
        id: FuncId(0),
        scopes: Vec::new(),
        vals: Vec::new(),
        captures: Vec::new(),
    });
    let mut body = Block::default();
    l.block(&program.body, &mut body, None)?;
    let b = l.stack.pop().expect("main");
    l.funcs[0] = Some(Func {
        id: FuncId(0),
        kind: FuncKind::Main,
        name: None,
        params: Vec::new(),
        captures: Vec::new(),
        requires_burn: false,
        vals: b.vals,
        body,
        span,
    });
    let mut m = Module {
        file: l.file,
        funcs: l
            .funcs
            .into_iter()
            .map(|f| f.expect("every func is finished"))
            .collect(),
        vars: l.vars,
        next_instr: l.next_instr,
        next_region: l.next_region,
    };
    let flags = super::verify::requires_burn(&m);
    for (f, flag) in m.funcs.iter_mut().zip(flags) {
        f.requires_burn = flag && f.kind != FuncKind::Main;
    }
    Ok(m)
}

impl Lowerer<'_> {
    fn cur(&mut self) -> &mut Builder {
        self.stack.last_mut().expect("a function is being built")
    }

    fn new_var(&mut self, name: &str, mutable: bool, span: Span) -> VarId {
        let id = VarId(self.vars.len() as u32);
        let owner = self.stack.last().expect("a function").id;
        self.vars.push(VarInfo {
            name: name.to_string(),
            mutable,
            owner,
            captured: false,
            span: fix(span),
        });
        id
    }

    fn emit(&mut self, out: &mut Block, span: Span, kind: Kind, ty: Option<Ty>) -> Option<ValueId> {
        self.next_instr += 1;
        let id = InstrId(self.next_instr);
        let result = ty.map(|ty| {
            let b = self.cur();
            let v = ValueId(b.vals.len() as u32);
            b.vals.push(ValueInfo { ty, def: id });
            v
        });
        out.instrs.push(Instr {
            id,
            span: fix(span),
            result,
            kind,
        });
        result
    }

    fn value(&mut self, out: &mut Block, span: Span, kind: Kind, ty: Ty) -> ValueId {
        self.emit(out, span, kind, Some(ty)).expect("a result")
    }

    fn konst(&mut self, out: &mut Block, span: Span, c: Const) -> ValueId {
        let ty = match &c {
            Const::Null => Ty::Null,
            Const::Bool(_) => Ty::Bool,
            Const::Int(_) => Ty::Int,
            Const::Float(_) => Ty::Float,
            Const::Str(_) => Ty::Str,
        };
        self.value(out, span, Kind::Const(c), ty)
    }

    /// Resolve a name: this function's declared scopes, then enclosing
    /// functions' (declared or pending: a capture), then the prelude.
    fn resolve(&mut self, name: &str) -> Result<Name, Error> {
        let depth = self.stack.len();
        for level in (0..depth).rev() {
            let own = level == depth - 1;
            let found = self.stack[level]
                .scopes
                .iter()
                .rev()
                .find_map(|s| s.get(name).copied().filter(|e| !(own && e.pending)));
            if let Some(e) = found {
                if !own {
                    // A capture: every function between owner and user carries it.
                    self.vars[e.var.0 as usize].captured = true;
                    for b in &mut self.stack[level + 1..] {
                        if !b.captures.contains(&e.var) {
                            b.captures.push(e.var);
                        }
                    }
                }
                return Ok(Name::Var(e.var, e.pull));
            }
        }
        if self.resolver.get(name).is_some() {
            return Ok(Name::Prelude);
        }
        Err(format!(
            "`{name}` does not resolve (the checker let it through)"
        ))
    }

    /// A block: its declarations allocated (pending), its pulls hoisted,
    /// then each statement in order. `pre` declares names first (a loop
    /// variable, a catch variable, a burn's state).
    fn block(
        &mut self,
        stmts: &[Stmt],
        out: &mut Block,
        pre: Option<(&str, VarId)>,
    ) -> Result<(), Error> {
        self.cur().scopes.push(HashMap::new());
        if let Some((name, var)) = pre {
            self.cur().scopes.last_mut().expect("pushed").insert(
                name.to_string(),
                Entry {
                    var,
                    pull: None,
                    pending: false,
                },
            );
        }
        // Allocate what this block declares, so a hoisted pull's body can
        // name it.
        for s in stmts {
            let (name, mutable, span) = match s {
                Stmt::Declare {
                    name,
                    mutable,
                    span,
                    ..
                } => (name, *mutable, *span),
                Stmt::Chain(c) => (&c.name, false, c.span),
                Stmt::Pull(f) => (&f.name, false, f.span),
                _ => continue,
            };
            if self.cur().scopes.last().expect("pushed").contains_key(name) {
                // E306 in the interpreter; the checker refuses it first.
                return Err(format!("`{name}` is declared twice in one scope"));
            }
            let var = self.new_var(name, mutable, span);
            self.cur().scopes.last_mut().expect("pushed").insert(
                name.clone(),
                Entry {
                    var,
                    pull: None,
                    pending: true,
                },
            );
        }
        // Hoist: every pull is a closure declared at the block's head.
        let pulls: Vec<&ast::FnDecl> = stmts
            .iter()
            .filter_map(|s| match s {
                Stmt::Pull(f) => Some(f),
                _ => None,
            })
            .collect();
        // The pulls' entries are declared before their bodies are lowered,
        // so pulls can call each other (and themselves).
        let mut planned = Vec::new();
        for f in &pulls {
            let fid = FuncId(self.funcs.len() as u32);
            self.funcs.push(None);
            let e = self
                .cur()
                .scopes
                .last_mut()
                .expect("pushed")
                .get_mut(&f.name)
                .expect("allocated");
            e.pull = Some(fid);
            e.pending = false;
            planned.push((f, fid, e.var));
        }
        for (f, fid, var) in planned {
            self.func(
                fid,
                FuncKind::Pull,
                Some(f.name.clone()),
                &f.params,
                FnBody::Block(&f.body.stmts),
                f.span,
            )?;
            let c = self.value(out, f.span, Kind::Closure(fid), Ty::Func);
            self.emit(out, f.span, Kind::Decl(var, c), None);
        }
        for s in stmts {
            self.stmt(s, out)?;
            if out.instrs.last().is_some_and(|i| i.kind.is_terminator()) {
                break;
            }
        }
        self.cur().scopes.pop();
        Ok(())
    }

    fn func(
        &mut self,
        fid: FuncId,
        kind: FuncKind,
        name: Option<String>,
        params: &[String],
        body: FnBody,
        span: Span,
    ) -> Result<(), Error> {
        self.stack.push(Builder {
            id: fid,
            scopes: vec![HashMap::new()],
            vals: Vec::new(),
            captures: Vec::new(),
        });
        let mut ps = Vec::new();
        for p in params {
            let v = self.new_var(p, true, span);
            self.cur().scopes[0].insert(
                p.clone(),
                Entry {
                    var: v,
                    pull: None,
                    pending: false,
                },
            );
            ps.push(v);
        }
        let mut block = Block::default();
        match body {
            FnBody::Block(stmts) => self.block(stmts, &mut block, None)?,
            FnBody::Expr(e) => {
                let v = self.expr(e, &mut block)?;
                self.emit(&mut block, e.span, Kind::Ret(v), None);
            }
        }
        if !block.instrs.last().is_some_and(|i| i.kind.is_terminator()) {
            let n = self.konst(&mut block, span, Const::Null);
            self.emit(&mut block, span, Kind::Ret(n), None);
        }
        let b = self.stack.pop().expect("pushed");
        self.funcs[fid.0 as usize] = Some(Func {
            id: fid,
            kind,
            name,
            params: ps,
            captures: b.captures,
            requires_burn: false,
            vals: b.vals,
            body: block,
            span: fix(span),
        });
        Ok(())
    }

    /// Mark a pending entry of the current function declared.
    fn declare(&mut self, name: &str) -> Result<VarId, Error> {
        for s in self.cur().scopes.iter_mut().rev() {
            if let Some(e) = s.get_mut(name) {
                e.pending = false;
                return Ok(e.var);
            }
        }
        Err(format!("`{name}` was declared without being allocated"))
    }

    fn stmt(&mut self, s: &Stmt, out: &mut Block) -> Result<(), Error> {
        // The interpreter counts a step as each statement starts, before
        // it evaluates anything (eval.rs `exec`); a `pull` is hoisted and
        // never runs as a statement, so it counts nothing.
        if !matches!(s, Stmt::Pull(_)) {
            self.emit(out, s.span(), Kind::Step(StepSite::Stmt), None);
        }
        match s {
            Stmt::Declare {
                name, value, span, ..
            } => {
                let v = self.expr(value, out)?;
                let var = self.declare(name)?;
                self.emit(out, *span, Kind::Decl(var, v), None);
            }
            Stmt::Assign {
                target,
                op,
                value,
                span,
            } => {
                // The value first, then the target (eval.rs).
                let v = self.expr(value, out)?;
                match target {
                    AssignTarget::Var(name, _) => {
                        let Name::Var(var, _) = self.resolve(name)? else {
                            return Err(format!("assignment to the prelude name `{name}`"));
                        };
                        let stored = match compound(*op) {
                            None => v,
                            Some(bop) => {
                                let old = self.value(out, *span, Kind::Load(var), Ty::Dynamic);
                                self.value(out, *span, Kind::Binary(bop, old, v), Ty::Dynamic)
                            }
                        };
                        self.emit(out, *span, Kind::Store(var, stored), None);
                    }
                    AssignTarget::Index { object, index } => {
                        let obj = self.expr(object, out)?;
                        let idx = self.expr(index, out)?;
                        self.emit(
                            out,
                            *span,
                            Kind::SetIndex {
                                obj,
                                idx,
                                op: *op,
                                val: v,
                                obj_at: fix(object.span),
                                idx_at: fix(index.span),
                            },
                            None,
                        );
                    }
                    AssignTarget::Member { object, name, .. } => {
                        let obj = self.expr(object, out)?;
                        self.emit(
                            out,
                            *span,
                            Kind::SetMember {
                                obj,
                                name: name.clone(),
                                op: *op,
                                val: v,
                                obj_at: fix(object.span),
                            },
                            None,
                        );
                    }
                }
            }
            Stmt::Pull(_) => {}
            Stmt::Chain(c) => {
                let mut steps = Vec::new();
                for e in &c.steps {
                    let hint = match &e.kind {
                        ExprKind::Ident(n) => Some(n.clone()),
                        ExprKind::Member { name, .. } => Some(name.clone()),
                        _ => None,
                    };
                    let v = self.expr(e, out)?;
                    steps.push((hint, v, fix(e.span)));
                }
                let var = self.declare(&c.name)?;
                self.emit(out, c.span, Kind::MakeChain { name: var, steps }, None);
            }
            Stmt::Pack(p) => {
                let mut vs = Vec::new();
                for r in &p.roots {
                    vs.push(self.expr(r, out)?);
                }
                self.emit(out, p.span, Kind::PackDecl(vs), None);
            }
            Stmt::Snuff { value, span } => {
                let v = match value {
                    Some(e) => self.expr(e, out)?,
                    None => self.konst(out, *span, Const::Null),
                };
                self.emit(out, *span, Kind::Ret(v), None);
            }
            Stmt::Exhale { values, span } => {
                let mut vs = Vec::new();
                for e in values {
                    vs.push(self.expr(e, out)?);
                }
                self.emit(out, *span, Kind::Exhale(vs), None);
            }
            Stmt::Cough { value, span } => {
                let v = self.expr(value, out)?;
                self.emit(out, *span, Kind::Throw(v), None);
            }
            Stmt::If {
                branches,
                otherwise,
                span,
            } => self.if_chain(branches, otherwise.as_ref(), *span, out)?,
            Stmt::While { cond, body, span } => {
                let mut c = Block::default();
                let cv = self.expr(cond, &mut c)?;
                self.emit(&mut c, cond.span, Kind::Yield(cv), None);
                // One step per iteration, once the condition held.
                let mut b = Block::default();
                self.emit(&mut b, *span, Kind::Step(StepSite::Iter), None);
                self.block(&body.stmts, &mut b, None)?;
                self.emit(out, *span, Kind::Loop { cond: c, body: b }, None);
            }
            Stmt::For {
                var,
                iter,
                body,
                span,
            } => {
                let it = self.expr(iter, out)?;
                let v = self.new_var(var, true, *span);
                // One step per item, before the body.
                let mut b = Block::default();
                self.emit(&mut b, *span, Kind::Step(StepSite::Iter), None);
                self.block(&body.stmts, &mut b, Some((var, v)))?;
                self.emit(
                    out,
                    *span,
                    Kind::ForEach {
                        iter: it,
                        var: v,
                        body: b,
                    },
                    None,
                );
            }
            Stmt::Break(span) => {
                self.emit(out, *span, Kind::Break, None);
            }
            Stmt::Continue(span) => {
                self.emit(out, *span, Kind::Continue, None);
            }
            Stmt::Try {
                body,
                catch_var,
                handler,
                finally,
                span,
            } => {
                let mut b = Block::default();
                self.block(&body.stmts, &mut b, None)?;
                let catch = match handler {
                    Some(h) => {
                        let var = catch_var
                            .as_ref()
                            .map(|n| (n.as_str(), self.new_var(n, false, *span)));
                        let mut hb = Block::default();
                        self.block(&h.stmts, &mut hb, var)?;
                        Some((var.map(|v| v.1), hb))
                    }
                    None => None,
                };
                let finally = match finally {
                    Some(f) => {
                        let mut fb = Block::default();
                        self.block(&f.stmts, &mut fb, None)?;
                        Some(fb)
                    }
                    None => None,
                };
                self.emit(
                    out,
                    *span,
                    Kind::Try {
                        body: b,
                        catch,
                        finally,
                    },
                    None,
                );
            }
            Stmt::Burn {
                class,
                state,
                label,
                body,
                unburn,
                unburn_src,
                span,
            } => {
                // The label is evaluated before the region, outside it.
                let label_at = label.as_ref().map(|e| e.span);
                let label = match label {
                    Some(e) => Some(self.expr(e, out)?),
                    None => None,
                };
                self.next_region += 1;
                let region = RegionId(self.next_region);
                let state_var = state
                    .as_ref()
                    .map(|n| (n.as_str(), self.new_var(n, false, *span)));
                let mut b = Block::default();
                self.block(&body.stmts, &mut b, state_var)?;
                let compensate = match unburn {
                    Some(u) => {
                        let mut ub = Block::default();
                        self.block(&u.stmts, &mut ub, state_var)?;
                        Some(ub)
                    }
                    None => None,
                };
                let node = BurnNode {
                    region,
                    class: *class,
                    state: state_var.map(|s| s.1),
                    label,
                    body: b,
                    compensate,
                    unburn_src: unburn_src.clone(),
                    unburn_at: unburn.as_ref().map(|u| u.span),
                    label_at,
                };
                self.emit(out, *span, Kind::Burn(Box::new(node)), None);
            }
            Stmt::Expr(e) => {
                self.expr(e, out)?;
            }
        }
        Ok(())
    }

    fn if_chain(
        &mut self,
        branches: &[(Expr, ast::Block)],
        otherwise: Option<&ast::Block>,
        span: Span,
        out: &mut Block,
    ) -> Result<(), Error> {
        let Some(((cond, then), rest)) = branches.split_first() else {
            if let Some(o) = otherwise {
                // Only reached through a recursion with no branches left.
                self.block(&o.stmts, out, None)?;
            }
            return Ok(());
        };
        let c = self.expr(cond, out)?;
        let mut t = Block::default();
        self.block(&then.stmts, &mut t, None)?;
        let other = if !rest.is_empty() {
            let mut o = Block::default();
            self.if_chain(rest, otherwise, span, &mut o)?;
            Some(o)
        } else {
            match otherwise {
                Some(o) => {
                    let mut ob = Block::default();
                    self.block(&o.stmts, &mut ob, None)?;
                    Some(ob)
                }
                None => None,
            }
        };
        self.emit(
            out,
            span,
            Kind::If {
                cond: c,
                then: t,
                otherwise: other,
            },
            None,
        );
        Ok(())
    }

    /// Is this expression a name for a prelude module (not shadowed)?
    fn module_ident(&mut self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Ident(m) => {
                matches!(self.resolve(m), Ok(Name::Prelude))
                    && matches!(self.resolver.get(m), Some(Resolved::Module(_)))
            }
            _ => false,
        }
    }

    fn args(
        &mut self,
        args: &[Expr],
        out: &mut Block,
        prefix: Option<ValueId>,
    ) -> Result<Vec<ValueId>, Error> {
        let mut v: Vec<ValueId> = prefix.into_iter().collect();
        for a in args {
            v.push(self.expr(a, out)?);
        }
        Ok(v)
    }

    /// A call of a builtin: an effect becomes a `burn.op`.
    fn builtin(
        &mut self,
        out: &mut Block,
        span: Span,
        name: &str,
        effect: crate::value::Effect,
        args: Vec<ValueId>,
    ) -> Result<ValueId, Error> {
        if effect.needs_burn() {
            let kind =
                kind_of(name).ok_or_else(|| format!("the effect `{name}` has no op kind"))?;
            Ok(self.value(
                out,
                span,
                Kind::BurnOp {
                    kind,
                    callee: name.to_string(),
                    effect,
                    args,
                },
                Ty::Dynamic,
            ))
        } else {
            Ok(self.value(
                out,
                span,
                Kind::CallBuiltin {
                    callee: name.to_string(),
                    args,
                },
                Ty::Dynamic,
            ))
        }
    }

    fn call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        span: Span,
        out: &mut Block,
        prefix: Option<ValueId>,
    ) -> Result<ValueId, Error> {
        if let ExprKind::Ident(name) = &callee.kind {
            match self.resolve(name)? {
                Name::Var(_, Some(fid)) => {
                    let a = self.args(args, out, prefix)?;
                    return Ok(self.value(
                        out,
                        span,
                        Kind::CallUser { func: fid, args: a },
                        Ty::Dynamic,
                    ));
                }
                Name::Prelude => {
                    if let Some(Resolved::Builtin { name, effect }) = self.resolver.get(name) {
                        let (name, effect) = (*name, *effect);
                        let a = self.args(args, out, prefix)?;
                        return self.builtin(out, span, name, effect, a);
                    }
                }
                Name::Var(..) => {}
            }
        }
        let f = self.expr(callee, out)?;
        let a = self.args(args, out, prefix)?;
        Ok(self.value(out, span, Kind::CallValue { f, args: a }, Ty::Dynamic))
    }

    #[allow(clippy::too_many_arguments)]
    fn method(
        &mut self,
        receiver: &Expr,
        name: &str,
        args: &[Expr],
        safe: bool,
        span: Span,
        out: &mut Block,
        prefix: Option<ValueId>,
    ) -> Result<ValueId, Error> {
        // `fs.rm(x)`: a module's member, resolved statically.
        if let ExprKind::Ident(m) = &receiver.kind {
            if let Name::Prelude = self.resolve(m)? {
                if let Some(Resolved::Module(items)) = self.resolver.get(m) {
                    match items.get(name) {
                        Some(Some((full, effect))) => {
                            let (full, effect) = (*full, *effect);
                            let a = self.args(args, out, prefix)?;
                            return self.builtin(out, span, full, effect, a);
                        }
                        Some(None) => {}
                        None => {
                            return Err(format!(
                                "module `{m}` has no `{name}` (the checker let it through)"
                            ))
                        }
                    }
                }
            }
        }
        let recv = self.expr(receiver, out)?;
        if safe {
            let mut rhs = Block::default();
            let a = self.args(args, &mut rhs, prefix)?;
            let m = self.value(
                &mut rhs,
                span,
                Kind::CallMethod {
                    recv,
                    name: name.to_string(),
                    args: a,
                },
                Ty::Dynamic,
            );
            self.emit(&mut rhs, span, Kind::Yield(m), None);
            return Ok(self.value(
                out,
                span,
                Kind::Short {
                    op: ShortOp::Safe,
                    subject: recv,
                    rhs,
                },
                Ty::Dynamic,
            ));
        }
        let a = self.args(args, out, prefix)?;
        Ok(self.value(
            out,
            span,
            Kind::CallMethod {
                recv,
                name: name.to_string(),
                args: a,
            },
            Ty::Dynamic,
        ))
    }

    fn expr(&mut self, e: &Expr, out: &mut Block) -> Result<ValueId, Error> {
        let span = e.span;
        Ok(match &e.kind {
            ExprKind::Null => self.konst(out, span, Const::Null),
            ExprKind::Bool(b) => self.konst(out, span, Const::Bool(*b)),
            ExprKind::Int(i) => self.konst(out, span, Const::Int(*i)),
            ExprKind::Float(f) => self.konst(out, span, Const::Float(*f)),
            ExprKind::Str(pieces) => {
                if pieces.iter().all(|p| matches!(p, StrPiece::Lit(_))) {
                    let s: String = pieces
                        .iter()
                        .map(|p| match p {
                            StrPiece::Lit(s) => s.as_str(),
                            StrPiece::Expr(_) => "",
                        })
                        .collect();
                    self.konst(out, span, Const::Str(s))
                } else {
                    let mut ps = Vec::new();
                    for p in pieces {
                        ps.push(match p {
                            StrPiece::Lit(s) => Piece::Lit(s.clone()),
                            StrPiece::Expr(x) => Piece::Val(self.expr(x, out)?),
                        });
                    }
                    self.value(out, span, Kind::Interp(ps), Ty::Str)
                }
            }
            ExprKind::Ident(name) => match self.resolve(name)? {
                Name::Var(v, pull) => self.value(
                    out,
                    span,
                    Kind::Load(v),
                    if pull.is_some() {
                        Ty::Func
                    } else {
                        Ty::Dynamic
                    },
                ),
                Name::Prelude => self.value(out, span, Kind::Prelude(name.clone()), Ty::Dynamic),
            },
            ExprKind::List(items) => {
                let mut vs = Vec::new();
                for x in items {
                    vs.push(self.expr(x, out)?);
                }
                self.value(out, span, Kind::MakeList(vs), Ty::List)
            }
            ExprKind::Map(entries) => {
                let mut kv = Vec::new();
                for (k, x) in entries {
                    // The tree-walker's order: a computed key, its check,
                    // then the entry's value (eval.rs, the map literal).
                    let key = match k {
                        ast::MapKey::Static(s) => MapKey::Static(s.clone()),
                        ast::MapKey::Dynamic(d) => {
                            let raw = self.expr(d, out)?;
                            let checked = self.value(out, d.span, Kind::Key(raw), Ty::Str);
                            MapKey::Dyn(checked, fix(d.span))
                        }
                    };
                    kv.push((key, self.expr(x, out)?));
                }
                self.value(out, span, Kind::MakeMap(kv), Ty::Map)
            }
            ExprKind::Unary { op, expr } => {
                let v = self.expr(expr, out)?;
                self.value(out, span, Kind::Unary(*op, v), Ty::Dynamic)
            }
            ExprKind::Binary { op, lhs, rhs } => match op {
                BinOp::And | BinOp::Or | BinOp::Coalesce => {
                    let subject = self.expr(lhs, out)?;
                    let mut r = Block::default();
                    let rv = self.expr(rhs, &mut r)?;
                    self.emit(&mut r, rhs.span, Kind::Yield(rv), None);
                    let sop = match op {
                        BinOp::And => ShortOp::And,
                        BinOp::Or => ShortOp::Or,
                        _ => ShortOp::Coalesce,
                    };
                    self.value(
                        out,
                        span,
                        Kind::Short {
                            op: sop,
                            subject,
                            rhs: r,
                        },
                        Ty::Dynamic,
                    )
                }
                _ => {
                    let a = self.expr(lhs, out)?;
                    let b = self.expr(rhs, out)?;
                    self.value(out, span, Kind::Binary(*op, a, b), Ty::Dynamic)
                }
            },
            ExprKind::Call { callee, args } => self.call(callee, args, span, out, None)?,
            ExprKind::Method {
                receiver,
                name,
                args,
                safe,
            } => self.method(receiver, name, args, *safe, span, out, None)?,
            ExprKind::Member {
                object, name, safe, ..
            } => {
                // `a?.k` evaluates nothing after `a`, and is null for a null
                // `a` and for a map without `k` (eval.rs member()): one
                // instruction carries both rules.
                let obj = self.expr(object, out)?;
                self.value(
                    out,
                    span,
                    Kind::Member {
                        obj,
                        name: name.clone(),
                        safe: *safe,
                    },
                    Ty::Dynamic,
                )
            }
            ExprKind::Index { object, index } => {
                let obj = self.expr(object, out)?;
                let idx = self.expr(index, out)?;
                let idx_at = fix(index.span);
                self.value(out, span, Kind::Index { obj, idx, idx_at }, Ty::Dynamic)
            }
            ExprKind::Lambda { params, body } => {
                let fid = FuncId(self.funcs.len() as u32);
                self.funcs.push(None);
                let b = match body {
                    LambdaBody::Expr(x) => FnBody::Expr(x),
                    LambdaBody::Block(b) => FnBody::Block(&b.stmts),
                };
                self.func(fid, FuncKind::Lambda, None, params, b, span)?;
                self.value(out, span, Kind::Closure(fid), Ty::Func)
            }
            ExprKind::Pipe { lhs, rhs } => {
                // The left side first, then the call with it prepended.
                let l = self.expr(lhs, out)?;
                match &rhs.kind {
                    ExprKind::Call { callee, args } => {
                        self.call(callee, args, rhs.span, out, Some(l))?
                    }
                    ExprKind::Method {
                        receiver,
                        name,
                        args,
                        safe,
                    } => self.method(receiver, name, args, *safe, rhs.span, out, Some(l))?,
                    // `x >> fs.rm` and `x >> f`: the bare name resolves as a
                    // call of it would, so an effect stays a burn.op and a
                    // pull a call.user.
                    ExprKind::Ident(_) => self.call(rhs, &[], rhs.span, out, Some(l))?,
                    ExprKind::Member {
                        object,
                        name,
                        safe: false,
                        ..
                    } if self.module_ident(object) => {
                        self.method(object, name, &[], false, rhs.span, out, Some(l))?
                    }
                    _ => {
                        let f = self.expr(rhs, out)?;
                        self.value(out, span, Kind::CallValue { f, args: vec![l] }, Ty::Dynamic)
                    }
                }
            }
        })
    }
}

enum FnBody<'a> {
    Block(&'a [Stmt]),
    Expr(&'a Expr),
}

fn compound(op: AssignOp) -> Option<BinOp> {
    match op {
        AssignOp::Set => None,
        AssignOp::Add => Some(BinOp::Add),
        AssignOp::Sub => Some(BinOp::Sub),
        AssignOp::Mul => Some(BinOp::Mul),
        AssignOp::Div => Some(BinOp::Div),
    }
}

/// A span with no line (synthesised in the parser) still points somewhere.
fn fix(s: Span) -> Span {
    Span::new(s.start, s.end.max(s.start), s.line.max(1), s.col.max(1))
}
