// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Statement execution and expression evaluation.

use super::env::{AssignError, Env, Scope};
use super::ops::{binary, fast_binary, negate, range_fits};
use super::{Interp, MAX_CALL_DEPTH};
use crate::diagnostics::{runtime, type_error, Diagnostic, Kind};
use crate::syntax::ast::*;
use crate::syntax::span::Span;
use crate::value::{FnBody, Function, Value};
use indexmap::IndexMap;
use std::cell::Cell;
use std::rc::Rc;

/// Non-local control flow.
pub enum Signal {
    Error(Diagnostic),
    Return(Value),
    Break(Span),
    Continue(Span),
}

impl From<Diagnostic> for Signal {
    fn from(d: Diagnostic) -> Self {
        Signal::Error(d)
    }
}

type Exec = Result<(), Signal>;
type Eval = Result<Value, Diagnostic>;

impl Interp {
    // ----- blocks and statements ---------------------------------------------

    /// Run `stmts` in `env`, hoisting `pull` declarations first.
    fn run_while(&mut self, cond: &Expr, body: &Block, env: &Env, span: Span) -> Exec {
        while self.eval(cond, env)?.truthy() {
            self.tick(span)?;
            match self.exec_block(body, env) {
                Ok(()) | Err(Signal::Continue(_)) => {}
                Err(Signal::Break(_)) => break,
                Err(other) => return Err(other),
            }
        }
        Ok(())
    }

    fn run_for(
        &mut self,
        var: &str,
        items: Vec<Value>,
        body: &Block,
        env: &Env,
        span: Span,
    ) -> Exec {
        for item in items {
            self.tick(span)?;
            let scope = Scope::child(env);
            scope.declare(var, item, true);
            let result = self.exec_block_in(&body.stmts, &scope);
            Scope::release(scope);
            match result {
                Ok(()) | Err(Signal::Continue(_)) => {}
                Err(Signal::Break(_)) => break,
                Err(other) => return Err(other),
            }
        }
        Ok(())
    }

    /// `run_for` over `a..b` without the list: the same scope per turn, the
    /// same steps, the same values.
    fn run_for_range(
        &mut self,
        var: &str,
        a: i64,
        b: i64,
        body: &Block,
        env: &Env,
        span: Span,
    ) -> Exec {
        for i in a..b {
            self.tick(span)?;
            let scope = Scope::child(env);
            scope.declare(var, Value::Int(i), true);
            let result = self.exec_block_in(&body.stmts, &scope);
            Scope::release(scope);
            match result {
                Ok(()) | Err(Signal::Continue(_)) => {}
                Err(Signal::Break(_)) => break,
                Err(other) => return Err(other),
            }
        }
        Ok(())
    }

    pub(crate) fn exec_block_in(&mut self, stmts: &[Stmt], env: &Env) -> Exec {
        self.hoist(stmts, env)?;
        for stmt in stmts {
            if !matches!(stmt, Stmt::Pull(_)) {
                self.exec(stmt, env)?;
            }
        }
        Ok(())
    }

    /// Like `exec_block_in`, but yields the value of a trailing expression
    /// statement.
    pub(crate) fn eval_block_last(
        &mut self,
        stmts: &[Stmt],
        env: &Env,
    ) -> Result<Option<Value>, Signal> {
        self.hoist(stmts, env)?;
        let mut last = None;
        for stmt in stmts {
            match stmt {
                Stmt::Pull(_) => {}
                Stmt::Expr(e) => last = Some(self.eval(e, env)?),
                other => {
                    self.exec(other, env)?;
                    last = None;
                }
            }
        }
        Ok(last)
    }

    fn hoist(&mut self, stmts: &[Stmt], env: &Env) -> Exec {
        for stmt in stmts {
            if let Stmt::Pull(decl) = stmt {
                let func = Value::Func(Rc::new(Function {
                    name: Some(decl.name.clone()),
                    params: decl.params.clone(),
                    body: FnBody::Block(decl.body.clone()),
                    closure: env.clone(),
                }));
                if !env.declare(&decl.name, func, false) {
                    return Err(Signal::Error(
                        crate::diagnostics::check(format!(
                            "`{}` is already declared in this scope",
                            decl.name
                        ))
                        .code("E306")
                        .at(decl.span),
                    ));
                }
            }
        }
        Ok(())
    }

    fn exec_block(&mut self, block: &Block, env: &Env) -> Exec {
        if !block.declares() {
            // Nothing can be declared here, so no scope is made (the
            // checker counts it the same way): no hoisting to look for either.
            for stmt in &block.stmts {
                self.exec(stmt, env)?;
            }
            return Ok(());
        }
        let scope = Scope::child(env);
        let result = self.exec_block_in(&block.stmts, &scope);
        Scope::release(scope);
        result
    }

    fn exec(&mut self, stmt: &Stmt, env: &Env) -> Exec {
        self.tick(stmt.span())?;
        match stmt {
            Stmt::Declare {
                name,
                mutable,
                value,
                span,
            } => {
                let v = self.operand(value, env)?;
                if !env.declare(name, v, *mutable) {
                    return Err(crate::diagnostics::check(format!(
                        "`{name}` is already declared in this scope"
                    ))
                    .code("E306")
                    .at(*span)
                    .with_hint(format!(
                        "assign to it with `{name} = ...`, or pick another name"
                    ))
                    .into());
                }
                Ok(())
            }
            Stmt::Assign {
                target,
                op,
                value,
                span,
            } => self.assign(target, *op, value, env, *span),
            Stmt::Snuff { value, .. } => {
                let v = match value {
                    Some(e) => self.eval(e, env)?,
                    None => Value::Null,
                };
                Err(Signal::Return(v))
            }
            Stmt::If {
                branches,
                otherwise,
                ..
            } => {
                for (cond, body) in branches {
                    if self.eval(cond, env)?.truthy() {
                        return self.exec_block(body, env);
                    }
                }
                if let Some(block) = otherwise {
                    self.exec_block(block, env)?;
                }
                Ok(())
            }
            Stmt::Expr(e) => {
                self.eval(e, env)?;
                Ok(())
            }
            _ => self.exec_rare(stmt, env),
        }
    }

    /// The statements that are not on every hot path: kept out of `exec`,
    /// whose frame (and prologue) every statement pays for.
    #[inline(never)]
    fn exec_rare(&mut self, stmt: &Stmt, env: &Env) -> Exec {
        match stmt {
            Stmt::Declare { .. }
            | Stmt::Assign { .. }
            | Stmt::Snuff { .. }
            | Stmt::If { .. }
            | Stmt::Expr(_) => unreachable!("handled in exec"),
            Stmt::Pull(_) => Ok(()), // hoisted
            Stmt::Chain(decl) => {
                let mut steps = Vec::with_capacity(decl.steps.len());
                for (n, expr) in decl.steps.iter().enumerate() {
                    let value = self.eval(expr, env)?;
                    let label = match &expr.kind {
                        ExprKind::Ident(name) => Some(name.clone()),
                        ExprKind::Member { name, .. } => Some(name.clone()),
                        _ => None,
                    };
                    steps.push(chain_step(&decl.name, n, label, value, expr.span)?);
                }
                let chain = make_chain(&decl.name, steps);
                if !env.declare(&decl.name, chain, false) {
                    return Err(crate::diagnostics::check(format!(
                        "`{}` is already declared in this scope",
                        decl.name
                    ))
                    .code("E306")
                    .at(decl.span)
                    .into());
                }
                Ok(())
            }
            Stmt::Exhale { values, .. } => {
                let mut line = String::new();
                for (i, e) in values.iter().enumerate() {
                    exhale_part(&mut line, i, &self.eval(e, env)?);
                }
                self.write_exhaled(line)?;
                Ok(())
            }
            Stmt::Cough { value, span } => {
                let v = self.eval(value, env)?;
                Err(self.cough(v, *span).into())
            }
            Stmt::While { cond, body, span } => {
                self.loop_depth += 1;
                let result = self.run_while(cond, body, env, *span);
                self.loop_depth -= 1;
                result
            }
            Stmt::For {
                var,
                iter,
                body,
                span,
            } => {
                // `for i in a..b`: the numbers one at a time, never the list
                // of them, with the range's own checks and errors.
                if let ExprKind::Binary {
                    op: BinOp::Range,
                    lhs,
                    rhs,
                } = &iter.kind
                {
                    let l = self.eval(lhs, env)?;
                    let r = self.eval(rhs, env)?;
                    if let (Value::Int(a), Value::Int(b)) = (&l, &r) {
                        range_fits(*a, *b, iter.span)?;
                        let (a, b) = (*a, *b);
                        self.loop_depth += 1;
                        let result = self.run_for_range(var, a, b, body, env, *span);
                        self.loop_depth -= 1;
                        return result;
                    }
                    let iterable = binary(BinOp::Range, l, r, iter.span)?;
                    let items = self.iterate(iterable, *span)?;
                    self.loop_depth += 1;
                    let result = self.run_for(var, items, body, env, *span);
                    self.loop_depth -= 1;
                    return result;
                }
                let iterable = self.eval(iter, env)?;
                let items = self.iterate(iterable, *span)?;
                self.loop_depth += 1;
                let result = self.run_for(var, items, body, env, *span);
                self.loop_depth -= 1;
                result
            }
            Stmt::Break(span) => Err(Signal::Break(*span)),
            Stmt::Continue(span) => Err(Signal::Continue(*span)),
            Stmt::Try {
                body,
                catch_var,
                handler,
                finally,
                ..
            } => {
                let mut result = self.exec_block(body, env);
                if let Some(handler) = handler {
                    if matches!(result, Err(Signal::Error(_))) && self.exit_requested.is_none() {
                        let Err(Signal::Error(diag)) = result else {
                            unreachable!()
                        };
                        let payload = self.cough_payload.take();
                        let scope = Scope::child(env);
                        if let Some(name) = catch_var {
                            scope.declare(name, error_value(&diag, payload), false);
                        }
                        result = self.exec_block_in(&handler.stmts, &scope);
                    }
                }
                if let Some(fin) = finally {
                    // Runs whatever happened, exit() included. Its own
                    // signal wins; otherwise the pending one carries on,
                    // payload intact.
                    let pending_payload = self.cough_payload.take();
                    match self.exec_block(fin, env) {
                        Ok(()) => {
                            self.cough_payload = pending_payload;
                            result
                        }
                        Err(sig) => Err(sig),
                    }
                } else {
                    result
                }
            }
            Stmt::Pack(decl) => {
                let mut roots = Vec::new();
                for r in &decl.roots {
                    let v = self.eval(r, env)?;
                    roots.push(pack_root(&v, r.span)?);
                }
                self.declare_pack(roots, decl.span)?;
                Ok(())
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
                // The label is read before the block runs, in the enclosing
                // scope: a task name, a string.
                let label_text = match label {
                    Some(e) => {
                        let v = self.eval(e, env)?;
                        match &v {
                            Value::Str(s) => Some(s.to_string()),
                            other => {
                                return Err(type_error(format!(
                                    "a burn's label is a string, got {}",
                                    other.type_name()
                                ))
                                .at(e.span)
                                .with_hint("burn (s, \"deploy\") { ... }: the name the plan and a rollback call this burn by")
                                .into());
                            }
                        }
                    }
                    None => None,
                };
                let scope = Scope::child(env);
                let state_value = state.as_ref().map(|name| {
                    let m = Value::map(IndexMap::new());
                    scope.declare(name, m.clone(), false);
                    m
                });
                let unlit = *class == BurnClass::Unlit;
                let compensating = unburn.is_some() && !unlit;
                self.burn_open(unlit);
                if compensating {
                    self.compensate_open();
                }
                let result = self.exec_block_in(&body.stmts, &scope);
                let mark = if compensating {
                    self.compensate_close()
                } else {
                    0
                };
                self.burn_close(unlit);
                // A block left by snuff, break, continue or exit() completed
                // its effects, so its compensation is recorded before the
                // signal goes on; only an error leaves them uncovered.
                let completed = match &result {
                    Ok(()) => true,
                    Err(Signal::Error(_)) => self.exit_requested.is_some(),
                    Err(_) => true,
                };
                if compensating && completed {
                    self.record_compensation(
                        crate::burn::journal::Compensation {
                            source: unburn_src.clone().unwrap_or_default(),
                            state_name: state.clone(),
                            state: serde_json::Value::Null,
                            line: unburn.as_ref().map(|b| b.span.line).unwrap_or(span.line),
                            label: label_text,
                            command: None,
                            cwd: None,
                        },
                        state_value.as_ref(),
                        mark,
                        *span,
                    )?;
                }
                result?;
                Ok(())
            }
        }
    }

    #[inline(never)]
    fn assign(
        &mut self,
        target: &AssignTarget,
        op: AssignOp,
        value: &Expr,
        env: &Env,
        span: Span,
    ) -> Exec {
        let rhs = self.operand(value, env)?;
        match target {
            AssignTarget::Var(name, res) => {
                if self.recording() {
                    self.recorded_assign(env, name, span)?;
                }
                // A recording pass goes by name: it needs the owner's serial.
                let at = match res.get() {
                    Res::Local { depth, idx } if !self.recording() => Some((depth, idx)),
                    _ => None,
                };
                let new = match op {
                    AssignOp::Set => rhs,
                    _ => {
                        let resolved = at.and_then(|(d, i)| env.get_at(d, i, name));
                        let current = match resolved {
                            Some(v) => v,
                            None => env.get(name).ok_or_else(|| unknown_name(name, env, span))?,
                        };
                        binary(compound_op(op), current, rhs, span)?
                    }
                };
                let result = match at {
                    Some((d, i)) => match env.assign_at(d, i, name, new) {
                        Ok(r) => r,
                        Err(back) => env.assign(name, back),
                    },
                    None => env.assign(name, new),
                };
                match result {
                    Ok(()) => Ok(()),
                    Err(AssignError::NotFound) => Err(unknown_name(name, env, span)
                        .with_hint(format!("declare it first: roll {name} = ..."))
                        .into()),
                    Err(AssignError::Immutable) => Err(runtime(format!(
                        "`{name}` is a stick and cannot be reassigned"
                    ))
                    .code("E507")
                    .at(span)
                    .with_hint(format!(
                        "declare it with `roll {name} = ...` if it needs to change"
                    ))
                    .into()),
                }
            }
            AssignTarget::Index { object, index } => {
                let obj = self.operand(object, env)?;
                let idx = self.operand(index, env)?;
                let new = match op {
                    AssignOp::Set => rhs,
                    _ => {
                        let current = stored_at(&obj, &idx, span, index.span, object.span)?;
                        binary(compound_op(op), current, rhs, span)?
                    }
                };
                set_index(&obj, &idx, new, span, index.span, object.span)?;
                Ok(())
            }
            AssignTarget::Member { object, name, hint } => {
                let obj = self.operand(object, env)?;
                let new = match (&obj, op) {
                    (_, AssignOp::Set) => rhs,
                    (Value::Map(map), _) => {
                        let current =
                            field(&map.borrow(), name, hint).cloned().ok_or_else(|| {
                                runtime(format!("map has no key `{name}`"))
                                    .code("E505")
                                    .at(span)
                            })?;
                        binary(compound_op(op), current, rhs, span)?
                    }
                    // Not a map: `set_field` says so before anything is read.
                    _ => rhs,
                };
                set_field(&obj, name, new, hint, object.span)?;
                Ok(())
            }
        }
    }

    // ----- expressions ---------------------------------------------------------

    /// `eval`, with the leaves that need no recursion (a number, a name the
    /// checker placed) answered here: `eval`'s frame is large and most nodes
    /// of a hot loop are leaves.
    #[inline(always)]
    fn operand(&mut self, e: &Expr, env: &Env) -> Eval {
        match &e.kind {
            ExprKind::Int(i) => return Ok(Value::Int(*i)),
            ExprKind::Float(f) => return Ok(Value::Float(*f)),
            ExprKind::Ident(name) => {
                if let Res::Local { depth, idx } = e.res.get() {
                    if !self.recording() {
                        if let Some(v) = env.get_at(depth, idx, name) {
                            return Ok(v);
                        }
                    }
                }
            }
            _ => {}
        }
        self.eval(e, env)
    }

    pub(crate) fn eval(&mut self, expr: &Expr, env: &Env) -> Eval {
        let span = expr.span;
        match &expr.kind {
            ExprKind::Null => Ok(Value::Null),
            ExprKind::Bool(b) => Ok(Value::Bool(*b)),
            ExprKind::Int(i) => Ok(Value::Int(*i)),
            ExprKind::Float(f) => Ok(Value::Float(*f)),
            ExprKind::Ident(name) => {
                if self.recording() {
                    return match self.recorded_get(env, name, span) {
                        Some(r) => r,
                        None => Err(unknown_name(name, env, span)),
                    };
                }
                match expr.res.get() {
                    Res::Local { depth, idx } => {
                        if let Some(v) = env.get_at(depth, idx, name) {
                            return Ok(v);
                        }
                    }
                    Res::Prelude => {
                        if let Some(v) = self.prelude.get(name) {
                            #[cfg(debug_assertions)]
                            env.cross_check_owner(name, self.prelude.serial());
                            return Ok(v);
                        }
                    }
                    Res::Unresolved => {}
                }
                env.get(name).ok_or_else(|| unknown_name(name, env, span))
            }
            ExprKind::Binary { op, lhs, rhs } => match op {
                BinOp::And => {
                    let l = self.eval(lhs, env)?;
                    if !l.truthy() {
                        return Ok(l);
                    }
                    self.eval(rhs, env)
                }
                BinOp::Or => {
                    let l = self.eval(lhs, env)?;
                    if l.truthy() {
                        return Ok(l);
                    }
                    self.eval(rhs, env)
                }
                BinOp::Coalesce => {
                    let l = self.eval(lhs, env)?;
                    if !l.is_null() {
                        return Ok(l);
                    }
                    self.eval(rhs, env)
                }
                _ => {
                    let l = self.operand(lhs, env)?;
                    let r = self.operand(rhs, env)?;
                    if let Some(v) = fast_binary(*op, &l, &r) {
                        return Ok(v);
                    }
                    binary(*op, l, r, span)
                }
            },
            ExprKind::Call { callee, args } => {
                let f = self.eval(callee, env)?;
                if let Value::Func(func) = &f {
                    // A compiled function takes its arguments as values, not
                    // a scope: it goes the general way, to `call_function`.
                    if func.params.len() == args.len() && !matches!(func.body, FnBody::Compiled(_))
                    {
                        // The arguments go straight into the callee's frame:
                        // no list, no allocation (cig-script#146).
                        let scope = Scope::child(&func.closure);
                        for (param, a) in func.params.iter().zip(args) {
                            let v = self.operand(a, env)?;
                            scope.declare(param, v, true);
                        }
                        return self.run_call(func, scope, span);
                    }
                }
                let mut argv = Vec::with_capacity(args.len());
                for a in args {
                    argv.push(self.eval(a, env)?);
                }
                self.call(&f, argv, span)
            }
            ExprKind::Member {
                object,
                name,
                safe,
                hint,
            } => {
                let obj = self.operand(object, env)?;
                if let Value::Map(map) = &obj {
                    if let Some(v) = field(&map.borrow(), name, hint) {
                        return Ok(v.clone());
                    }
                }
                self.member(obj, name, *safe, span)
            }
            ExprKind::Index { object, index } => {
                let obj = self.operand(object, env)?;
                let idx = self.operand(index, env)?;
                self.index(obj, idx, span, index.span)
            }
            ExprKind::Str(pieces) => {
                let mut s = String::new();
                for piece in pieces {
                    match piece {
                        StrPiece::Lit(l) => s.push_str(l),
                        StrPiece::Expr(e) => s.push_str(&self.eval(e, env)?.display()),
                    }
                }
                Ok(Value::str(s))
            }
            ExprKind::List(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(self.eval(item, env)?);
                }
                Ok(Value::list(out))
            }
            ExprKind::Map(entries) => {
                let mut map = IndexMap::with_capacity(entries.len());
                for (key, value) in entries {
                    let k = match key {
                        MapKey::Static(s) => s.clone(),
                        MapKey::Dynamic(e) => map_key(self.eval(e, env)?, e.span)?,
                    };
                    let v = self.eval(value, env)?;
                    map.insert(k, v);
                }
                Ok(Value::map(map))
            }
            ExprKind::Unary { op, expr: inner } => {
                let v = self.eval(inner, env)?;
                match op {
                    UnOp::Not => Ok(Value::Bool(!v.truthy())),
                    UnOp::Neg => negate(v, span),
                }
            }
            ExprKind::Method {
                receiver,
                name,
                args,
                safe,
            } => {
                let recv = self.eval(receiver, env)?;
                if *safe && recv.is_null() {
                    return Ok(Value::Null);
                }
                let mut argv = Vec::with_capacity(args.len());
                for a in args {
                    argv.push(self.eval(a, env)?);
                }
                self.call_method(recv, name, argv, span)
            }
            ExprKind::Lambda { params, body } => Ok(Value::Func(Rc::new(Function {
                name: None,
                params: params.clone(),
                body: match body {
                    LambdaBody::Expr(e) => FnBody::Expr(e.clone()),
                    LambdaBody::Block(b) => FnBody::Block(b.clone()),
                },
                closure: env.clone(),
            }))),
            ExprKind::Pipe { lhs, rhs } => {
                let piped = self.eval(lhs, env)?;
                match &rhs.kind {
                    ExprKind::Call { callee, args } => {
                        let f = self.eval(callee, env)?;
                        let mut argv = Vec::with_capacity(args.len() + 1);
                        argv.push(piped);
                        for a in args {
                            argv.push(self.eval(a, env)?);
                        }
                        self.call(&f, argv, span)
                    }
                    ExprKind::Method {
                        receiver,
                        name,
                        args,
                        safe,
                    } => {
                        let recv = self.eval(receiver, env)?;
                        if *safe && recv.is_null() {
                            return Ok(Value::Null);
                        }
                        let mut argv = Vec::with_capacity(args.len() + 1);
                        argv.push(piped);
                        for a in args {
                            argv.push(self.eval(a, env)?);
                        }
                        self.call_method(recv, name, argv, span)
                    }
                    _ => {
                        let f = self.eval(rhs, env)?;
                        self.call(&f, vec![piped], span)
                    }
                }
            }
        }
    }

    #[inline(never)]
    pub(crate) fn member(&mut self, obj: Value, name: &str, safe: bool, span: Span) -> Eval {
        match &obj {
            Value::Null if safe => Ok(Value::Null),
            Value::Map(map) => match map.borrow().get(name) {
                Some(v) => Ok(v.clone()),
                None if safe => Ok(Value::Null),
                None => Err(runtime(format!("map has no key `{name}`"))
                    .code("E505")
                    .at(span)
                    .with_hint(format!(
                        "use `?.{name}` to get null instead, or check `has(\"{name}\")`"
                    ))),
            },
            Value::Chain(c) => match name {
                "name" => Ok(Value::str(&c.name)),
                "steps" => Ok(Value::list(
                    c.steps.iter().map(|s| Value::str(&s.label)).collect(),
                )),
                "len" => Ok(Value::Int(c.steps.len() as i64)),
                _ => Err(runtime(format!("chain has no member `{name}`"))
                    .at(span)
                    .with_hint("chains have .name, .steps and .len; run one with light(chain)")),
            },
            Value::Module(m) => match m.items.get(name) {
                Some(v) => Ok(v.clone()),
                None => {
                    let names: Vec<&str> = m.items.keys().copied().collect();
                    let mut d = runtime(format!("module `{}` has no `{name}`", m.name)).at(span);
                    if let Some(s) = closest(name, names.iter().copied()) {
                        d = d.with_hint(format!("did you mean `{}.{s}`?", m.name));
                    }
                    Err(d)
                }
            },
            Value::Null => Err(runtime(format!("cannot read `.{name}` of null"))
                .at(span)
                .with_hint(format!("use `?.{name}` if null is expected"))),
            other => Err(
                type_error(format!("{} has no member `{name}`", other.type_name()))
                    .at(span)
                    .with_hint("add the parentheses: `.len()`"),
            ),
        }
    }

    #[inline(never)]
    pub(crate) fn index(&mut self, obj: Value, idx: Value, span: Span, idx_span: Span) -> Eval {
        match (&obj, &idx) {
            (Value::List(list), Value::Int(i)) => {
                let list = list.borrow();
                normalize_index(*i, list.len())
                    .map(|p| list[p].clone())
                    .ok_or_else(|| {
                        runtime(format!(
                            "index {i} is out of range for a list of {}",
                            list.len()
                        ))
                        .code("E504")
                        .at(span)
                    })
            }
            (Value::Str(s), Value::Int(i)) => {
                let count = s.chars().count();
                normalize_index(*i, count)
                    .and_then(|p| s.chars().nth(p))
                    .map(|c| Value::str(c.to_string()))
                    .ok_or_else(|| {
                        runtime(format!(
                            "index {i} is out of range for a string of {count} characters"
                        ))
                        .code("E504")
                        .at(span)
                    })
            }
            (Value::Map(map), Value::Str(key)) => {
                map.borrow().get(&**key).cloned().ok_or_else(|| {
                    runtime(format!("map has no key `{key}`"))
                        .code("E505")
                        .at(span)
                })
            }
            (Value::List(_), other) | (Value::Str(_), other) => Err(type_error(format!(
                "index must be an int, got {}",
                other.type_name()
            ))
            .at(idx_span)),
            (Value::Map(_), other) => Err(type_error(format!(
                "map key must be a string, got {}",
                other.type_name()
            ))
            .at(idx_span)),
            (other, _) => Err(
                type_error(format!("cannot index into {}", other.type_name()))
                    .code("E405")
                    .at(span),
            ),
        }
    }

    pub(crate) fn iterate(&mut self, v: Value, span: Span) -> Result<Vec<Value>, Diagnostic> {
        match v {
            Value::List(l) => Ok(l.borrow().clone()),
            Value::Map(m) => Ok(m.borrow().keys().map(Value::str).collect()),
            Value::Str(s) => Ok(s.chars().map(|c| Value::str(c.to_string())).collect()),
            other => Err(
                type_error(format!("cannot iterate over {}", other.type_name()))
                    .code("E405")
                    .at(span)
                    .with_hint(
                        "loop over a list, a map (its keys), a string (its characters) or a range",
                    ),
            ),
        }
    }

    // ----- calls -----------------------------------------------------------------

    pub fn call(&mut self, f: &Value, args: Vec<Value>, span: Span) -> Eval {
        match f {
            Value::Func(func) => self.call_function(func, args, span),
            Value::Builtin(b) => self.call_builtin(b, &args, span),
            Value::Module(m) => Err(type_error(format!(
                "`{}` is a module, not a function",
                m.name
            ))
            .code("E404")
            .at(span)
            .with_hint(format!(
                "call one of its functions, e.g. `{}.{}(...)`",
                m.name,
                m.items.keys().next().unwrap_or(&"")
            ))),
            other => Err(type_error(format!("cannot call {}", other.type_name()))
                .code("E404")
                .at(span)),
        }
    }

    /// Call a builtin: its arity, the pass recorder's admission, then the
    /// builtin itself, with the call's position on any error that has none.
    pub(crate) fn call_builtin(
        &mut self,
        b: &crate::value::Builtin,
        args: &[Value],
        span: Span,
    ) -> Eval {
        b.check_arity(args.len(), span)?;
        if self.recording() {
            self.recording_admits(b, span)?;
        }
        (b.func)(self, args, span).map_err(|e| e.or_at(span))
    }

    #[inline(never)]
    fn call_function(&mut self, func: &Rc<Function>, args: Vec<Value>, span: Span) -> Eval {
        if args.len() != func.params.len() {
            return Err(arity_mismatch(func, args.len(), span));
        }
        if let FnBody::Compiled(cf) = &func.body {
            return crate::vm::call_compiled(self, func, cf, args, span);
        }
        let scope = Scope::child(&func.closure);
        for (param, arg) in func.params.iter().zip(args) {
            scope.declare(param, arg, true);
        }
        self.run_call(func, scope, span)
    }

    /// Run `func`'s body in `scope`, which already holds its arguments.
    fn run_call(&mut self, func: &Rc<Function>, scope: Env, span: Span) -> Eval {
        self.enter_call(span)?;
        let result = match &func.body {
            FnBody::Expr(e) => self.eval(e, &scope).map_err(Signal::Error),
            FnBody::Block(b) => self.exec_block_in(&b.stmts, &scope).map(|()| Value::Null),
            // `call_function` sends a compiled function to the VM before a
            // scope is made, and the fast call path leaves it to that.
            FnBody::Compiled(_) => Err(Signal::Error(crate::vm::misrouted(span))),
        };
        self.leave_call();
        Scope::release(scope);
        match result {
            Ok(v) => Ok(v),
            Err(Signal::Return(v)) => Ok(v),
            Err(Signal::Error(d)) => Err(d),
            Err(Signal::Break(s)) => Err(runtime("`break` outside of a loop").at(s)),
            Err(Signal::Continue(s)) => Err(runtime("`continue` outside of a loop").at(s)),
        }
    }

    /// One more script call on the stack, or E506 at `span` when the
    /// stack is already [`MAX_CALL_DEPTH`] deep. Pairs with
    /// [`Interp::leave_call`].
    pub(crate) fn enter_call(&mut self, span: Span) -> Result<(), Diagnostic> {
        if self.call_depth >= MAX_CALL_DEPTH {
            return Err(runtime(format!(
                "call depth exceeded {MAX_CALL_DEPTH}; is there unbounded recursion?"
            ))
            .code("E506")
            .at(span));
        }
        self.call_depth += 1;
        Ok(())
    }

    /// The call [`Interp::enter_call`] counted has returned.
    pub(crate) fn leave_call(&mut self) {
        self.call_depth -= 1;
    }

    #[inline(never)]
    pub(crate) fn call_method(
        &mut self,
        recv: Value,
        name: &str,
        args: Vec<Value>,
        span: Span,
    ) -> Eval {
        // A map holding a function under that key is called as a plain function.
        if let Value::Map(map) = &recv {
            let f = map.borrow().get(name).cloned();
            if let Some(f @ (Value::Func(_) | Value::Builtin(_))) = f {
                return self.call(&f, args, span);
            }
        }
        if let Value::Module(m) = &recv {
            return match m.items.get(name) {
                Some(f) => self.call(f, args, span),
                None => self.member(recv.clone(), name, false, span),
            };
        }
        match crate::stdlib::method(&recv, name) {
            Some(b) => {
                let mut argv = Vec::with_capacity(args.len() + 1);
                argv.push(recv);
                argv.extend(args);
                b.check_arity(argv.len(), span).map_err(|d| {
                    // Report arity without the implicit receiver.
                    Diagnostic::new(
                        d.kind,
                        d.message.replace(
                            &format!("{} expects", b.name),
                            &format!(".{name}() expects"),
                        ),
                    )
                    .at(span)
                })?;
                (b.func)(self, &argv, span).map_err(|e| e.or_at(span))
            }
            None => {
                let mut d = type_error(format!("{} has no method `{name}`", recv.type_name()))
                    .code("E404")
                    .at(span);
                if let Some(s) = closest(name, crate::stdlib::method_names(&recv)) {
                    d = d.with_hint(format!("did you mean `.{s}()`?"));
                }
                Err(d)
            }
        }
    }
}

// ----- helpers -----------------------------------------------------------------

/// Where `name` is in `map`, trying the position the node found it at last
/// time first. The hint is only a guess: it is believed when the key at that
/// position is `name`, and otherwise the lookup runs and the hint moves, so a
/// key removed, added or moved since cannot give a wrong answer.
#[inline]
fn field_index(map: &IndexMap<String, Value>, name: &str, hint: &Cell<u32>) -> Option<usize> {
    let guess = hint.get() as usize;
    if let Some((k, _)) = map.get_index(guess) {
        if k == name {
            return Some(guess);
        }
    }
    let i = map.get_index_of(name)?;
    hint.set(i as u32);
    Some(i)
}

#[inline]
fn field<'m>(map: &'m IndexMap<String, Value>, name: &str, hint: &Cell<u32>) -> Option<&'m Value> {
    field_index(map, name, hint).map(|i| &map[i])
}

/// One root of `pack { }`, as the tree-walker and the VM's `pack` check it:
/// a non-empty string (`E755`), normalized. Errors are at the root, `span`.
pub(crate) fn pack_root(v: &Value, span: Span) -> Result<std::path::PathBuf, Diagnostic> {
    let Value::Str(text) = v else {
        return Err(type_error(format!(
            "pack roots are paths (strings), got {}",
            v.type_name()
        ))
        .at(span));
    };
    if text.trim().is_empty() {
        return Err(crate::diagnostics::burn("a pack root is empty")
            .code("E755")
            .at(span)
            .with_hint("name the directory the script may write in, pack { \"./build\" }; an empty root would mean the whole working directory"));
    }
    Ok(crate::burn::normalize_root(text))
}

/// Step `n` (from 0) of the chain `chain`, as the tree-walker and the VM's
/// `chain.make` build it: a function or a chain, labeled `label` (the
/// step's name in the source) or, without one, as the chain runner labels
/// a value. Errors are at the step, `span`.
pub(crate) fn chain_step(
    chain: &str,
    n: usize,
    label: Option<String>,
    value: Value,
    span: Span,
) -> Result<crate::value::ChainStep, Diagnostic> {
    let label = label.unwrap_or_else(|| crate::stdlib::chain_label(&value, n));
    match value {
        Value::Func(_) | Value::Builtin(_) | Value::Chain(_) => {}
        other => {
            return Err(type_error(format!(
                "chain `{}`: step {} is a {}, not a function or chain",
                chain,
                n + 1,
                other.type_name()
            ))
            .at(span)
            .with_hint("make each step a stick holding a pack, a pull, or another chain"))
        }
    }
    Ok(crate::value::ChainStep { label, value, span })
}

/// The chain value `chain name { steps }` declares.
pub(crate) fn make_chain(name: &str, steps: Vec<crate::value::ChainStep>) -> Value {
    Value::Chain(Rc::new(crate::value::Chain {
        name: name.to_string(),
        steps,
    }))
}

/// One value of an `exhale` line: a space before every value but the first,
/// then the value as `display` shows it. The tree-walker and the VM build
/// the line with this, and write it with [`Interp::write_exhaled`].
pub(crate) fn exhale_part(line: &mut String, i: usize, v: &Value) {
    if i > 0 {
        line.push(' ');
    }
    line.push_str(&v.display());
}

impl Interp {
    /// Write a finished `exhale` line, with its newline, to the output. A
    /// reader that went away (`| head`) stops the script quietly (E513).
    pub(crate) fn write_exhaled(&mut self, mut line: String) -> Result<(), Diagnostic> {
        line.push('\n');
        if let Err(e) = self.out.write_all(line.as_bytes()) {
            if e.kind() == std::io::ErrorKind::BrokenPipe {
                // The reader went away (e.g. `| head`): stop quietly.
                self.exit_requested = Some(0);
                return Err(runtime("output pipe closed").code("E513"));
            }
            return Err(runtime(format!("could not write output: {e}")));
        }
        Ok(())
    }
}

/// A map literal's computed key: a string as itself, an int as its digits,
/// anything else a type error at the key.
pub(crate) fn map_key(v: Value, span: Span) -> Result<String, Diagnostic> {
    match v {
        Value::Str(s) => Ok(s.to_string()),
        Value::Int(i) => Ok(i.to_string()),
        other => Err(type_error(format!(
            "map keys must be strings, got {}",
            other.type_name()
        ))
        .at(span)),
    }
}

/// `obj.name = new`: a map's key set in place, or added at the end; on
/// anything else, the type error at `obj_span`.
pub(crate) fn set_field(
    obj: &Value,
    name: &str,
    new: Value,
    hint: &Cell<u32>,
    obj_span: Span,
) -> Result<(), Diagnostic> {
    match obj {
        Value::Map(map) => {
            let mut map = map.borrow_mut();
            match field_index(&map, name, hint) {
                Some(i) => map[i] = new,
                None => {
                    map.insert(name.to_string(), new);
                }
            }
            Ok(())
        }
        other => {
            Err(type_error(format!("cannot set `.{name}` on {}", other.type_name())).at(obj_span))
        }
    }
}

/// The value an indexed assignment target holds now, for `+=` and the like:
/// E504 past a list's end, E505 for a missing key, and the store's own type
/// errors otherwise.
fn stored_at(
    obj: &Value,
    idx: &Value,
    span: Span,
    idx_span: Span,
    obj_span: Span,
) -> Result<Value, Diagnostic> {
    match (obj, idx) {
        (Value::List(list), Value::Int(i)) => {
            let list = list.borrow();
            let pos = list_slot(*i, list.len(), span)?;
            Ok(list[pos].clone())
        }
        (Value::Map(map), Value::Str(key)) => map.borrow().get(&**key).cloned().ok_or_else(|| {
            runtime(format!("map has no key `{key}`"))
                .code("E505")
                .at(span)
        }),
        _ => Err(store_type_error(obj, idx, idx_span, obj_span)),
    }
}

/// `obj[idx] = new`: a list's element (E504 past its end) or a map's key.
pub(crate) fn set_index(
    obj: &Value,
    idx: &Value,
    new: Value,
    span: Span,
    idx_span: Span,
    obj_span: Span,
) -> Result<(), Diagnostic> {
    match (obj, idx) {
        (Value::List(list), Value::Int(i)) => {
            let mut list = list.borrow_mut();
            let pos = list_slot(*i, list.len(), span)?;
            list[pos] = new;
            Ok(())
        }
        (Value::Map(map), Value::Str(key)) => {
            map.borrow_mut().insert(key.to_string(), new);
            Ok(())
        }
        _ => Err(store_type_error(obj, idx, idx_span, obj_span)),
    }
}

fn list_slot(i: i64, len: usize, span: Span) -> Result<usize, Diagnostic> {
    normalize_index(i, len).ok_or_else(|| {
        runtime(format!("index {i} is out of range for a list of {len}"))
            .code("E504")
            .at(span)
    })
}

fn store_type_error(obj: &Value, idx: &Value, idx_span: Span, obj_span: Span) -> Diagnostic {
    match obj {
        Value::List(_) => type_error(format!(
            "list index must be an int, got {}",
            idx.type_name()
        ))
        .at(idx_span),
        Value::Map(_) => {
            type_error(format!("map key must be a string, got {}", idx.type_name())).at(idx_span)
        }
        other => type_error(format!("cannot index into {}", other.type_name()))
            .code("E405")
            .at(obj_span),
    }
}

/// E401 for a script function called with the wrong number of arguments.
pub(crate) fn arity_mismatch(func: &Function, got: usize, span: Span) -> Diagnostic {
    let name = func.name.as_deref().unwrap_or("this pack");
    type_error(format!(
        "{name} expects {} argument{}, got {}",
        func.params.len(),
        if func.params.len() == 1 { "" } else { "s" },
        got
    ))
    .code("E401")
    .at(span)
}

fn compound_op(op: AssignOp) -> BinOp {
    match op {
        AssignOp::Add => BinOp::Add,
        AssignOp::Sub => BinOp::Sub,
        AssignOp::Mul => BinOp::Mul,
        AssignOp::Div => BinOp::Div,
        AssignOp::Set => unreachable!(),
    }
}

pub fn normalize_index(i: i64, len: usize) -> Option<usize> {
    let len = len as i64;
    let idx = if i < 0 { len + i } else { i };
    (0..len).contains(&idx).then_some(idx as usize)
}

impl Interp {
    /// `cough v`: the error it raises at `span`, with `v` kept as the
    /// payload the nearest `ashtray` reads. A map's `message` is the
    /// message; any other value is shown.
    pub(crate) fn cough(&mut self, v: Value, span: Span) -> Diagnostic {
        let message = match &v {
            Value::Map(m) => m
                .borrow()
                .get("message")
                .map(|m| m.display())
                .unwrap_or_else(|| v.repr()),
            other => other.display(),
        };
        self.cough_payload = Some(v);
        Diagnostic::new(Kind::Cough, message).at(span)
    }

    /// The value a prelude name holds (a builtin, a module, `args`): the
    /// same value a script that names it gets. E501 when the name is not
    /// there (`args` before the script's arguments are set).
    pub(crate) fn prelude_value(&self, name: &str, span: Span) -> Eval {
        self.prelude
            .get(name)
            .ok_or_else(|| unknown_name(name, &self.prelude, span))
    }
}

/// Build the map bound by `ashtray err`.
pub fn error_value(diag: &Diagnostic, payload: Option<Value>) -> Value {
    let mut map = IndexMap::new();
    if let Some(Value::Map(p)) = &payload {
        for (k, v) in p.borrow().iter() {
            map.insert(k.clone(), v.clone());
        }
    }
    map.insert("message".to_string(), Value::str(&diag.message));
    map.insert("kind".to_string(), Value::str(diag.kind.as_str()));
    map.insert(
        "line".to_string(),
        diag.line
            .map(|l| Value::Int(l as i64))
            .unwrap_or(Value::Null),
    );
    if let Some(p) = payload {
        if !matches!(p, Value::Map(_)) {
            map.insert("value".to_string(), p);
        }
    }
    Value::map(map)
}

fn unknown_name(name: &str, env: &Env, span: Span) -> Diagnostic {
    let mut d = runtime(format!("unknown name `{name}`"))
        .code("E501")
        .at(span);
    let names = env.visible_names();
    if let Some(s) = closest(name, names.iter().map(String::as_str)) {
        d = d.with_hint(format!("did you mean `{s}`?"));
    }
    d
}

/// The closest candidate within a small edit distance, for hints. Ties go
/// to the shorter name, then the alphabetically first, so the hint never
/// depends on iteration order.
pub fn closest<'a>(name: &str, candidates: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let limit = if name.len() <= 4 { 1 } else { 2 };
    candidates
        .filter(|c| *c != name)
        .map(|c| (edit_distance(name, c), c))
        .filter(|(d, _)| *d <= limit)
        .min_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.len().cmp(&b.1.len()))
                .then_with(|| a.1.cmp(b.1))
        })
        .map(|(_, c)| c)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            cur.push((prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closest_is_deterministic_on_ties() {
        assert_eq!(closest("b", ["f", "a"].into_iter()), Some("a"));
        assert_eq!(closest("b", ["a", "f"].into_iter()), Some("a"));
        assert_eq!(closest("lenn", ["length", "len"].into_iter()), Some("len"));
        assert_eq!(closest("zzzz", ["a", "b"].into_iter()), None);
        assert_eq!(
            closest("read_txt", ["read_text", "read_lines"].into_iter()),
            Some("read_text")
        );
    }
}
