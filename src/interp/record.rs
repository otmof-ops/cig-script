// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! A pass's recording, inside the interpreter (cig-script#186).
//!
//! While `pass.issue` runs its work, every input the work depends on must
//! be recorded, and nothing the recording cannot see may reach the result:
//!
//! - every builtin is classified in one table: recorded (the builtin notes
//!   what it read), pure, or open-world. A builtin not in the table is
//!   open-world, so one added later cannot slip past a recording;
//! - a builtin that changes the world (a burn inside the work) is refused;
//! - a variable of any scope made before the work began (the function
//!   around it, a closure's captured scope) is refused, and so is a global
//!   that is not a constant: the work is a function of its subject and of
//!   what it reads, nothing else.

use super::env::Env;
use super::Interp;
use crate::diagnostics::{burn, Diagnostic};
use crate::syntax::ast::{
    AssignTarget, Expr, ExprKind, LambdaBody, MapKey, Program, Stmt, StrPiece,
};
use crate::syntax::span::Span;
use crate::value::{Builtin, Effect, Value};
use std::collections::HashSet;

/// One `pass.issue` being recorded.
pub(crate) struct RecFrame {
    pub rec: cig_pass::Recorder,
    /// Scopes with a serial below this existed before the work began.
    pub barrier: u64,
    /// The pass will carry a time limit, so the work may read the clock.
    pub clock: bool,
}

/// Builtins that record what they read (each notes its inputs itself).
pub(crate) const RECORDED: &[&str] = &[
    "fs.read_text",
    "fs.read_lines",
    "fs.exists",
    "fs.is_file",
    "fs.is_dir",
    "fs.size",
    "fs.list",
    "fs.glob",
    "fs.cwd",
    "fs.home",
    "env.get",
    "env.has",
    "env.all",
    "hash.sha256_file",
    "proc.which",
    "path.abs",
    "json.load",
    "csv.read",
    "proc.run",
    "proc.text",
    "proc.lines",
    "proc.json",
    "proc.csv",
    "proc.kv",
    "proc.pipe",
    "proc.shell",
    "pass.find",
    "pass.check",
    "pass.show",
    "pass.issue",
    "pass.memo",
    "pass.narrow",
    "pass.recording",
];

/// Builtins that depend on nothing outside their arguments.
pub(crate) const PURE: &[&str] = &[
    "len",
    "str",
    "repr",
    "int",
    "float",
    "bool",
    "type_of",
    "range",
    "keys",
    "values",
    "entries",
    "assert",
    "exit",
    "light",
    "log.debug",
    "log.info",
    "log.warn",
    "log.error",
    "cig.version",
    "cig.os",
    "cig.arch",
    "path.join",
    "path.base",
    "path.dir",
    "path.ext",
    "path.stem",
    "path.with_ext",
    "path.normalize",
    "path.is_abs",
    "path.parts",
    "math.abs",
    "math.min",
    "math.max",
    "math.floor",
    "math.ceil",
    "math.round",
    "math.sqrt",
    "math.pow",
    "math.clamp",
    "csv.parse",
    "csv.stringify",
    "json.parse",
    "json.stringify",
    "text.matches",
    "text.find",
    "text.find_all",
    "text.captures",
    "text.replace",
    "text.split",
    "text.template",
    "time.sleep_ms",
    "time.format_ms",
    "hash.sha256",
    "hash.short",
];

/// Clock reads: allowed only when the pass carries a time limit.
pub(crate) const CLOCK: &[&str] = &["time.now_ms", "time.now_iso", "time.stamp"];

/// How a recording treats a builtin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    /// It notes its own inputs.
    Recorded,
    /// It depends on its arguments alone.
    Pure,
    /// It reads the clock.
    Clock,
    /// Nothing a recording can see into.
    OpenWorld,
    /// It changes the world.
    Refused,
}

/// The table: every builtin's class while recording. Anything not in it is
/// open-world.
pub(crate) fn class_of(b: &Builtin) -> Class {
    if matches!(b.effect, Effect::Write | Effect::Env) || b.name == "pass.spend" {
        return Class::Refused;
    }
    if RECORDED.contains(&b.name) {
        Class::Recorded
    } else if PURE.contains(&b.name) {
        Class::Pure
    } else if CLOCK.contains(&b.name) {
        Class::Clock
    } else {
        Class::OpenWorld
    }
}

fn e732(msg: String, span: Span) -> Diagnostic {
    burn(msg).code("E732").at(span).with_hint(
        "put what the work needs in its subject (pack(s) reads s.name), or read it inside the work",
    )
}

impl Interp {
    /// Is a pass's work being recorded?
    pub(crate) fn recording(&self) -> bool {
        !self.recs.is_empty()
    }

    /// The innermost recorder, while recording.
    pub(crate) fn recorder(&mut self) -> Option<&mut cig_pass::Recorder> {
        self.recs.last_mut().map(|f| &mut f.rec)
    }

    /// A name's value, checked against the recording: a variable from
    /// before the work began, or a global that is not a constant, is E732.
    pub(crate) fn recorded_get(
        &self,
        env: &Env,
        name: &str,
        span: Span,
    ) -> Option<Result<Value, Diagnostic>> {
        let (value, serial, mutable) = env.get_traced(name)?;
        let Some(frame) = self.recs.last() else {
            return Some(Ok(value));
        };
        if serial >= frame.barrier {
            return Some(Ok(value));
        }
        if serial == self.prelude.serial() {
            if name == "args" {
                return Some(Err(e732(
                    "the work read `args`: a pass found later would vouch for other arguments"
                        .to_string(),
                    span,
                )));
            }
            return Some(Ok(value));
        }
        if serial == self.globals.serial() {
            if matches!(
                value,
                Value::Func(_) | Value::Builtin(_) | Value::Module(_) | Value::Chain(_)
            ) || (!mutable && self.const_globals.contains(name))
            {
                return Some(Ok(value));
            }
            return Some(Err(e732(
                format!("the work read the global `{name}`, which is not a constant: a pass found later would vouch for whatever it held then"),
                span,
            )));
        }
        Some(Err(e732(
            format!("the work read `{name}` from outside it: a pass found later would vouch for whatever it held then"),
            span,
        )))
    }

    /// May the work assign to this name? Only to its own variables.
    pub(crate) fn recorded_assign(
        &self,
        env: &Env,
        name: &str,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let Some(frame) = self.recs.last() else {
            return Ok(());
        };
        match env.owner_serial(name) {
            Some(s) if s < frame.barrier => Err(e732(
                format!("the work changed `{name}`, which lives outside it: a pass found later would skip that change"),
                span,
            )),
            _ => Ok(()),
        }
    }

    /// May this builtin run inside the work? Open-world calls are noted
    /// (the pass will not be issued); world changes are refused.
    pub(crate) fn recording_admits(&mut self, b: &Builtin, span: Span) -> Result<(), Diagnostic> {
        let clock = self.recs.last().is_some_and(|f| f.clock);
        match class_of(b) {
            Class::Recorded | Class::Pure => Ok(()),
            Class::Clock if clock => Ok(()),
            Class::Clock => {
                if let Some(r) = self.recorder() {
                    r.open_world(format!("`{}` read the clock and the pass has no `until`", b.name));
                }
                Ok(())
            }
            Class::OpenWorld => {
                if let Some(r) = self.recorder() {
                    r.open_world(format!("`{}` was called, and a pass cannot see what it depends on", b.name));
                }
                Ok(())
            }
            Class::Refused => Err(burn(format!(
                "the work called `{}`, which changes the world: a pass found later would skip that change",
                b.name
            ))
            .code("E733")
            .at(span)
            .with_hint("keep the work to reads; change the world outside pass.issue")),
        }
    }
}

/// The program's constant globals: top-level `stick`s whose value is a
/// constant expression (literals, lists and maps of them, arithmetic on
/// them, other constants) and that nothing in the program mutates (an
/// index or field assignment, a mutating method). A work may read these.
pub(crate) fn const_globals_of(program: &Program) -> HashSet<String> {
    let mut consts: HashSet<String> = HashSet::new();
    for stmt in &program.body {
        if let Stmt::Declare {
            name,
            mutable: false,
            value,
            ..
        } = stmt
        {
            if is_const(value, &consts) {
                consts.insert(name.clone());
            }
        }
    }
    let mut mutated = HashSet::new();
    for stmt in &program.body {
        mutations_in(stmt, &mut mutated);
    }
    consts.retain(|n| !mutated.contains(n));
    consts
}

fn is_const(e: &Expr, consts: &HashSet<String>) -> bool {
    match &e.kind {
        ExprKind::Null | ExprKind::Bool(_) | ExprKind::Int(_) | ExprKind::Float(_) => true,
        ExprKind::Str(pieces) => pieces.iter().all(|p| match p {
            StrPiece::Lit(_) => true,
            StrPiece::Expr(x) => is_const(x, consts),
        }),
        ExprKind::List(items) => items.iter().all(|x| is_const(x, consts)),
        ExprKind::Map(entries) => entries.iter().all(|(k, v)| {
            (match k {
                MapKey::Static(_) => true,
                MapKey::Dynamic(x) => is_const(x, consts),
            }) && is_const(v, consts)
        }),
        ExprKind::Unary { expr, .. } => is_const(expr, consts),
        ExprKind::Binary { lhs, rhs, .. } => is_const(lhs, consts) && is_const(rhs, consts),
        ExprKind::Ident(n) => consts.contains(n),
        _ => false,
    }
}

/// Methods that change a list or a map in place.
const MUTATING: &[&str] = &[
    "push", "pop", "insert", "remove", "clear", "extend", "set", "delete", "sort", "reverse",
    "append", "shift", "unshift", "splice", "fill", "update",
];

fn root_name(e: &Expr) -> Option<&str> {
    match &e.kind {
        ExprKind::Ident(n) => Some(n),
        ExprKind::Index { object, .. } | ExprKind::Member { object, .. } => root_name(object),
        _ => None,
    }
}

fn mutations_in(s: &Stmt, out: &mut HashSet<String>) {
    let mut on_expr = |e: &Expr| {
        if let ExprKind::Method { receiver, name, .. } = &e.kind {
            if MUTATING.contains(&name.as_str()) {
                if let Some(n) = root_name(receiver) {
                    out.insert(n.to_string());
                }
            }
        }
    };
    let mut assigned = Vec::new();
    walk_stmt(s, &mut on_expr, &mut |st: &Stmt| {
        if let Stmt::Assign {
            target: AssignTarget::Index { object, .. } | AssignTarget::Member { object, .. },
            ..
        } = st
        {
            if let Some(n) = root_name(object) {
                assigned.push(n.to_string());
            }
        }
    });
    out.extend(assigned);
}

/// Visit every statement and expression under `s`, lambda and function
/// bodies included.
fn walk_stmt(s: &Stmt, fe: &mut dyn FnMut(&Expr), fs: &mut dyn FnMut(&Stmt)) {
    fs(s);
    let block =
        |b: &crate::syntax::ast::Block, fe: &mut dyn FnMut(&Expr), fs: &mut dyn FnMut(&Stmt)| {
            for st in &b.stmts {
                walk_stmt(st, fe, fs);
            }
        };
    match s {
        Stmt::Declare { value, .. } => walk_expr(value, fe, fs),
        Stmt::Assign { target, value, .. } => {
            match target {
                AssignTarget::Var(_) => {}
                AssignTarget::Index { object, index } => {
                    walk_expr(object, fe, fs);
                    walk_expr(index, fe, fs);
                }
                AssignTarget::Member { object, .. } => walk_expr(object, fe, fs),
            }
            walk_expr(value, fe, fs);
        }
        Stmt::Pull(f) => block(&f.body, fe, fs),
        Stmt::Chain(c) => {
            for e in &c.steps {
                walk_expr(e, fe, fs);
            }
        }
        Stmt::Pack(p) => {
            for e in &p.roots {
                walk_expr(e, fe, fs);
            }
        }
        Stmt::Snuff { value, .. } => {
            if let Some(v) = value {
                walk_expr(v, fe, fs);
            }
        }
        Stmt::Exhale { values, .. } => {
            for v in values {
                walk_expr(v, fe, fs);
            }
        }
        Stmt::Cough { value, .. } => walk_expr(value, fe, fs),
        Stmt::If {
            branches,
            otherwise,
            ..
        } => {
            for (c, b) in branches {
                walk_expr(c, fe, fs);
                block(b, fe, fs);
            }
            if let Some(b) = otherwise {
                block(b, fe, fs);
            }
        }
        Stmt::While { cond, body, .. } => {
            walk_expr(cond, fe, fs);
            block(body, fe, fs);
        }
        Stmt::For { iter, body, .. } => {
            walk_expr(iter, fe, fs);
            block(body, fe, fs);
        }
        Stmt::Try {
            body,
            handler,
            finally,
            ..
        } => {
            block(body, fe, fs);
            if let Some(b) = handler {
                block(b, fe, fs);
            }
            if let Some(b) = finally {
                block(b, fe, fs);
            }
        }
        Stmt::Burn {
            label,
            body,
            unburn,
            ..
        } => {
            if let Some(l) = label {
                walk_expr(l, fe, fs);
            }
            block(body, fe, fs);
            if let Some(b) = unburn {
                block(b, fe, fs);
            }
        }
        Stmt::Expr(e) => walk_expr(e, fe, fs),
        Stmt::Break(_) | Stmt::Continue(_) => {}
    }
}

fn walk_expr(e: &Expr, fe: &mut dyn FnMut(&Expr), fs: &mut dyn FnMut(&Stmt)) {
    fe(e);
    match &e.kind {
        ExprKind::Str(pieces) => {
            for p in pieces {
                if let StrPiece::Expr(x) = p {
                    walk_expr(x, fe, fs);
                }
            }
        }
        ExprKind::List(items) => {
            for x in items {
                walk_expr(x, fe, fs);
            }
        }
        ExprKind::Map(entries) => {
            for (k, v) in entries {
                if let MapKey::Dynamic(x) = k {
                    walk_expr(x, fe, fs);
                }
                walk_expr(v, fe, fs);
            }
        }
        ExprKind::Unary { expr, .. } => walk_expr(expr, fe, fs),
        ExprKind::Binary { lhs, rhs, .. } | ExprKind::Pipe { lhs, rhs } => {
            walk_expr(lhs, fe, fs);
            walk_expr(rhs, fe, fs);
        }
        ExprKind::Call { callee, args } => {
            walk_expr(callee, fe, fs);
            for a in args {
                walk_expr(a, fe, fs);
            }
        }
        ExprKind::Method { receiver, args, .. } => {
            walk_expr(receiver, fe, fs);
            for a in args {
                walk_expr(a, fe, fs);
            }
        }
        ExprKind::Member { object, .. } => walk_expr(object, fe, fs),
        ExprKind::Index { object, index } => {
            walk_expr(object, fe, fs);
            walk_expr(index, fe, fs);
        }
        ExprKind::Lambda { body, .. } => match body {
            LambdaBody::Expr(x) => walk_expr(x, fe, fs),
            LambdaBody::Block(b) => {
                for st in &b.stmts {
                    walk_stmt(st, fe, fs);
                }
            }
        },
        ExprKind::Null
        | ExprKind::Bool(_)
        | ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Ident(_) => {}
    }
}
