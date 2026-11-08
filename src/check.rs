// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! Static checks that run before any code executes (`cig check`, and
//! always before `cig run`).
//!
//! The checker resolves names lexically, so it catches typos, assignment to
//! sticks, `snuff` outside a function, `break` outside a loop, and effectful
//! calls that can never be inside a burn.

use crate::diagnostics::{Diagnostic, Kind};
use crate::syntax::ast::*;
use crate::syntax::span::Span;
use crate::value::{Effect, Value};
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Binding {
    Mutable,
    Immutable,
    Function,
}

struct Scope {
    names: HashMap<String, Binding>,
}

pub struct Checker {
    scopes: Vec<Scope>,
    /// Names the prelude provides (modules and builtins), which a
    /// compensation may use.
    prelude: std::collections::HashSet<String>,
    /// Inside an `unburn { }` block: the scope index the block starts at.
    /// Names resolved below it (other than the prelude) are E754.
    comp_floor: Option<usize>,
    /// The pack's literal roots, for E750.
    pack_roots: Vec<std::path::PathBuf>,
    pack_seen: bool,
    /// Chains declared in the blocks being checked: usable inside a function
    /// body before the declaration, since the body resolves names when it
    /// runs, by which time the chain exists.
    hoisted_chains: Vec<String>,
    /// Qualified builtin names with an effect that needs a burn, e.g. `fs.rm`.
    effectful: HashMap<String, Effect>,
    modules: HashMap<String, Vec<String>>,
    burn_depth: u32,
    fn_depth: u32,
    loop_depth: u32,
    pub errors: Vec<Diagnostic>,
    pub warnings: Vec<Diagnostic>,
}

pub struct Report {
    pub errors: Vec<Diagnostic>,
    pub warnings: Vec<Diagnostic>,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Check a whole program. `extra_globals` are names the host will provide
/// (for the REPL, everything already defined).
pub fn check(program: &Program, extra_globals: &[String]) -> Report {
    let mut c = Checker::new(extra_globals);
    for (i, stmt) in program.body.iter().enumerate() {
        if let Stmt::Pack(p) = stmt {
            if i != 0 {
                c.error_code(
                    "E753",
                    "pack { } must be the first statement of the script",
                    p.span,
                )
                .hint = Some(
                    "move it above everything else; the scope is declared before anything can burn"
                        .to_string(),
                );
            }
        }
    }
    c.block_stmts(&program.body);
    Report {
        errors: c.errors,
        warnings: c.warnings,
    }
}

impl Checker {
    fn new(extra_globals: &[String]) -> Self {
        let mut root = Scope {
            names: HashMap::new(),
        };
        let mut effectful = HashMap::new();
        let mut modules = HashMap::new();
        for (name, value) in crate::stdlib::prelude() {
            root.names.insert(name.to_string(), Binding::Immutable);
            match &value {
                Value::Module(m) => {
                    let mut items = Vec::new();
                    for (k, v) in &m.items {
                        items.push(k.to_string());
                        if let Value::Builtin(b) = v {
                            if b.effect.needs_burn() {
                                effectful.insert(format!("{name}.{k}"), b.effect);
                            }
                        }
                    }
                    modules.insert(name.to_string(), items);
                }
                Value::Builtin(b) if b.effect.needs_burn() => {
                    effectful.insert(name.to_string(), b.effect);
                }
                _ => {}
            }
        }
        root.names.insert("args".to_string(), Binding::Immutable);
        for g in extra_globals {
            root.names.insert(g.clone(), Binding::Mutable);
        }
        let prelude: std::collections::HashSet<String> = root.names.keys().cloned().collect();
        Self {
            scopes: vec![
                root,
                Scope {
                    names: HashMap::new(),
                },
            ],
            prelude,
            comp_floor: None,
            pack_roots: Vec::new(),
            pack_seen: false,
            hoisted_chains: Vec::new(),
            effectful,
            modules,
            burn_depth: 0,
            fn_depth: 0,
            loop_depth: 0,
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// `proc.run("sh", ["-c", built])` with a command string built at run
    /// time is where injection lives; a literal string is somebody's own.
    fn shell_check(&mut self, args: &[Expr], span: Span) {
        let Some(program) = args.first().and_then(literal_text) else {
            return;
        };
        let shell = matches!(
            program.rsplit('/').next().unwrap_or(&program),
            "sh" | "bash"
                | "zsh"
                | "dash"
                | "ksh"
                | "fish"
                | "cmd"
                | "cmd.exe"
                | "powershell"
                | "pwsh"
        );
        if !shell {
            return;
        }
        let Some(ExprKind::List(items)) = args.get(1).map(|a| &a.kind) else {
            return;
        };
        let flag = items.first().and_then(literal_text).unwrap_or_default();
        if !matches!(flag.as_str(), "-c" | "/C" | "/c" | "-Command") {
            return;
        }
        let built = items.get(1).is_some_and(|e| literal_text(e).is_none());
        if built {
            self.warn_code(
                "E308",
                format!("`{program} {flag}` runs a shell with a command string built at run time"),
                span,
            )
            .hint = Some(
                "call the program directly with its arguments as a list, proc.pipe for a pipeline, or proc.shell if you mean the shell and the string is yours"
                    .to_string(),
            );
        }
    }

    /// A hop's `skip` globs (#148): what they cover is never kept, so the
    /// hop cannot undo its changes there. It is opt-in, so it is said on the
    /// hop's line; in the box, where `skip` changes nothing (the box's layer
    /// keeps every change), that is said instead. E310.
    fn skip_check(&mut self, args: &[Expr], span: Span) {
        let Some(Expr {
            kind: ExprKind::Map(entries),
            ..
        }) = args.last()
        else {
            return;
        };
        let mut globs: Option<Vec<String>> = None;
        let mut boxed = false;
        for (k, v) in entries {
            let MapKey::Static(key) = k else { continue };
            match key.as_str() {
                "skip" => {
                    let shown = |e: &Expr| literal_text(e).unwrap_or_else(|| "…".to_string());
                    globs = Some(match &v.kind {
                        ExprKind::List(items) => items.iter().map(shown).collect(),
                        _ => vec![shown(v)],
                    });
                }
                "box" => boxed = matches!(v.kind, ExprKind::Bool(true) | ExprKind::Map(_)),
                _ => {}
            }
        }
        let Some(globs) = globs.filter(|g| !g.is_empty()) else {
            return;
        };
        let shown = globs
            .iter()
            .map(|g| format!("`{g}`"))
            .collect::<Vec<_>>()
            .join(", ");
        if boxed {
            self.warn_code(
                "E310",
                format!("skip {shown} does nothing in the box: the box's layer keeps every change the hop makes"),
                span,
            )
            .hint = Some("drop `skip`, or drop `box` if the hop should leave those paths out of the tin".to_string());
        } else {
            self.warn_code(
                "E310",
                format!(
                    "skip {shown}: the hop's changes there are not kept, so they cannot be undone"
                ),
                span,
            )
            .hint = Some(
                "skip only what the hop rebuilds anyway (a cache); drop `skip` to keep those files"
                    .to_string(),
            );
        }
    }

    /// A literal path whose name is a Windows device (`nul`, `con`,
    /// `com1`...): the script will not run there. An error where it is a
    /// device, a warning where it is only unportable. E758.
    fn device_check(&mut self, qualified: &str, args: &[Expr]) {
        let positions: &[usize] = match qualified {
            "fs.write_text" | "fs.append_text" | "fs.mkdir" | "fs.rm" | "json.save"
            | "csv.write" => &[0],
            "fs.cp" => &[1],
            "fs.mv" => &[0, 1],
            _ => return,
        };
        for &pos in positions {
            let Some(text) = args.get(pos).and_then(literal_text) else {
                continue;
            };
            let Some(device) = crate::burn::reserved_device_name(std::path::Path::new(&text))
            else {
                continue;
            };
            let msg = format!("`{text}` is the {device} device on Windows, not a file");
            let hint = "name the file something else; con, prn, aux, nul, com1-9 and lpt1-9 are devices on Windows whatever the extension";
            if cfg!(windows) {
                self.error_code("E758", msg, args[pos].span).hint = Some(hint.to_string());
            } else {
                self.warn_code("E758", msg, args[pos].span).hint = Some(hint.to_string());
            }
        }
    }

    /// A literal path handed to a write, outside the declared pack: E750.
    fn pack_check(&mut self, qualified: &str, args: &[Expr]) {
        if self.pack_roots.is_empty() {
            return;
        }
        let positions: &[usize] = match qualified {
            "fs.write_text" | "fs.append_text" | "fs.mkdir" | "fs.rm" | "json.save"
            | "csv.write" => &[0],
            "fs.cp" => &[1],
            "fs.mv" => &[0, 1],
            _ => return,
        };
        for &pos in positions {
            let Some(text) = args.get(pos).and_then(literal_text) else {
                continue;
            };
            let inside = crate::burn::inside_roots(std::path::Path::new(&text), &self.pack_roots);
            if !inside {
                let roots = self
                    .pack_roots
                    .iter()
                    .map(|r| r.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                self.error_code(
                    "E750",
                    format!("`{text}` is outside the pack ({roots})"),
                    args[pos].span,
                )
                .hint = Some(
                    "move the path inside a declared root, or add its root to pack { }".to_string(),
                );
            }
        }
    }

    fn error_code(
        &mut self,
        code: &'static str,
        msg: impl Into<String>,
        span: Span,
    ) -> &mut Diagnostic {
        self.errors
            .push(Diagnostic::new(Kind::Check, msg).code(code).at(span));
        self.errors.last_mut().expect("just pushed")
    }

    fn warn_code(
        &mut self,
        code: &'static str,
        msg: impl Into<String>,
        span: Span,
    ) -> &mut Diagnostic {
        self.warnings
            .push(Diagnostic::new(Kind::Check, msg).code(code).at(span));
        self.warnings.last_mut().expect("just pushed")
    }

    fn push(&mut self) {
        self.scopes.push(Scope {
            names: HashMap::new(),
        });
    }

    fn pop(&mut self) {
        self.scopes.pop();
    }

    fn declare(&mut self, name: &str, binding: Binding, span: Span) {
        let scope = self.scopes.last_mut().expect("scope stack is never empty");
        if scope.names.contains_key(name) {
            self.error_code(
                "E306",
                format!("`{name}` is already declared in this scope"),
                span,
            )
            .hint = Some(format!("assign with `{name} = ...`, or pick another name"));
            return;
        }
        scope.names.insert(name.to_string(), binding);
    }

    fn lookup(&self, name: &str) -> Option<Binding> {
        self.scopes
            .iter()
            .rev()
            .find_map(|s| s.names.get(name).copied())
    }

    fn all_names(&self) -> Vec<String> {
        self.scopes
            .iter()
            .flat_map(|s| s.names.keys().cloned())
            .collect()
    }

    fn unknown(&mut self, name: &str, span: Span) {
        let names = self.all_names();
        let hint = crate::interp::closest(name, names.iter().map(String::as_str))
            .map(|s| format!("did you mean `{s}`?"));
        let d = self.error_code("E301", format!("unknown name `{name}`"), span);
        d.hint = hint;
    }

    fn block_stmts(&mut self, stmts: &[Stmt]) {
        // Hoist functions so calls before the definition resolve.
        for stmt in stmts {
            if let Stmt::Pull(f) = stmt {
                self.declare(&f.name, Binding::Function, f.span);
            }
            if let Stmt::Chain(c) = stmt {
                self.hoisted_chains.push(c.name.clone());
            }
        }
        for stmt in stmts {
            self.stmt(stmt);
        }
    }

    fn block(&mut self, block: &Block) {
        self.push();
        self.block_stmts(&block.stmts);
        self.pop();
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Declare {
                name,
                mutable,
                value,
                span,
            } => {
                self.expr(value);
                self.declare(
                    name,
                    if *mutable {
                        Binding::Mutable
                    } else {
                        Binding::Immutable
                    },
                    *span,
                );
            }
            Stmt::Assign {
                target,
                value,
                span,
                ..
            } => {
                self.expr(value);
                match target {
                    AssignTarget::Var(name) => match self.lookup(name) {
                        None => {
                            self.unknown(name, *span);
                            if let Some(last) = self.errors.last_mut() {
                                if last.hint.is_none() {
                                    last.hint =
                                        Some(format!("declare it first: roll {name} = ..."));
                                }
                            }
                        }
                        Some(Binding::Immutable) => {
                            self.error_code(
                                "E302",
                                format!("`{name}` is a stick and cannot be reassigned"),
                                *span,
                            )
                            .hint = Some(format!(
                                "declare it with `roll {name} = ...` if it needs to change"
                            ));
                        }
                        Some(Binding::Function) => {
                            self.error_code(
                                "E302",
                                format!("`{name}` is a pull and cannot be reassigned"),
                                *span,
                            );
                        }
                        Some(Binding::Mutable) => {}
                    },
                    AssignTarget::Index { object, index } => {
                        self.expr(object);
                        self.expr(index);
                    }
                    AssignTarget::Member { object, .. } => self.expr(object),
                }
            }
            Stmt::Pull(f) => self.function(&f.params, &f.body.stmts),
            Stmt::Chain(ch) => {
                for step in &ch.steps {
                    self.expr(step);
                }
                self.declare(&ch.name, Binding::Immutable, ch.span);
            }
            Stmt::Snuff { value, span } => {
                if self.fn_depth == 0 {
                    self.error_code("E304", "`snuff` outside of a pull or pack", *span)
                        .hint = Some("to stop the whole script use exit(code)".to_string());
                }
                if let Some(v) = value {
                    self.expr(v);
                }
            }
            Stmt::Exhale { values, .. } => values.iter().for_each(|v| self.expr(v)),
            Stmt::Cough { value, .. } => self.expr(value),
            Stmt::If {
                branches,
                otherwise,
                ..
            } => {
                for (cond, body) in branches {
                    self.expr(cond);
                    self.block(body);
                }
                if let Some(b) = otherwise {
                    self.block(b);
                }
            }
            Stmt::While { cond, body, .. } => {
                self.expr(cond);
                self.loop_depth += 1;
                self.block(body);
                self.loop_depth -= 1;
            }
            Stmt::For {
                var,
                iter,
                body,
                span,
            } => {
                self.expr(iter);
                self.loop_depth += 1;
                self.push();
                self.declare(var, Binding::Mutable, *span);
                self.block_stmts(&body.stmts);
                self.pop();
                self.loop_depth -= 1;
            }
            Stmt::Break(span) | Stmt::Continue(span) => {
                if self.loop_depth == 0 {
                    let what = if matches!(stmt, Stmt::Break(_)) {
                        "break"
                    } else {
                        "continue"
                    };
                    self.error_code("E305", format!("`{what}` outside of a loop"), *span);
                }
            }
            Stmt::Try {
                body,
                catch_var,
                handler,
                finally,
                span,
            } => {
                self.block(body);
                if let Some(handler) = handler {
                    self.push();
                    if let Some(v) = catch_var {
                        self.declare(v, Binding::Immutable, *span);
                    }
                    self.block_stmts(&handler.stmts);
                    self.pop();
                }
                if let Some(f) = finally {
                    self.block(f);
                }
            }
            Stmt::Burn {
                state,
                label,
                body,
                unburn,
                span,
                ..
            } => {
                if let Some(l) = label {
                    self.expr(l);
                }
                self.push();
                if let Some(name) = state {
                    self.declare(name, Binding::Immutable, *span);
                }
                self.burn_depth += 1;
                self.block_stmts(&body.stmts);
                self.burn_depth -= 1;
                self.pop();
                if let Some(u) = unburn {
                    // The compensation runs later, from the journal alone:
                    // only the state map, the prelude and its own names.
                    let floor = self.scopes.len();
                    self.comp_floor = Some(floor);
                    self.push();
                    if let Some(name) = state {
                        self.declare(name, Binding::Immutable, *span);
                    }
                    self.burn_depth += 1;
                    self.block_stmts(&u.stmts);
                    self.burn_depth -= 1;
                    self.pop();
                    self.comp_floor = None;
                }
            }
            Stmt::Pack(p) => {
                // Two scopes are the top level: the prelude and the script's own.
                if self.scopes.len() > 2 {
                    self.error_code("E753", "pack { } cannot be declared inside a block", p.span)
                        .hint = Some("declare it once, at the top of the script".to_string());
                } else if self.pack_seen {
                    self.error_code(
                        "E753",
                        "a second pack { }; a script declares one scope",
                        p.span,
                    )
                    .hint = Some("merge the roots into the first pack { }".to_string());
                }
                self.pack_seen = true;
                for r in &p.roots {
                    match literal_text(r) {
                        Some(text) if text.trim().is_empty() => {
                            self.error_code("E755", "a pack root is empty", r.span).hint = Some(
                                "name the directory the script may write in, pack { \"./build\" }; an empty root would mean the whole working directory".to_string(),
                            );
                        }
                        Some(text) => self.pack_roots.push(crate::burn::normalize_root(&text)),
                        None => self.expr(r),
                    }
                }
            }
            Stmt::Expr(e) => self.expr(e),
        }
    }

    fn function(&mut self, params: &[String], body: &[Stmt]) {
        self.push();
        self.fn_depth += 1;
        let saved_loop = self.loop_depth;
        let saved_burn = self.burn_depth;
        self.loop_depth = 0;
        // A function body may run inside a burn at the call site, so the
        // burn check inside it can only warn.
        for p in params {
            self.declare(p, Binding::Mutable, Span::default());
        }
        self.block_stmts(body);
        self.loop_depth = saved_loop;
        self.burn_depth = saved_burn;
        self.fn_depth -= 1;
        self.pop();
    }

    fn expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Null | ExprKind::Bool(_) | ExprKind::Int(_) | ExprKind::Float(_) => {}
            ExprKind::Str(pieces) => {
                for p in pieces {
                    if let StrPiece::Expr(e) = p {
                        self.expr(e);
                    }
                }
            }
            ExprKind::Ident(name) => {
                if self.lookup(name).is_none() {
                    if !(self.fn_depth > 0 && self.hoisted_chains.iter().any(|c| c == name)) {
                        self.unknown(name, expr.span);
                    }
                } else if let Some(floor) = self.comp_floor {
                    let idx = self
                        .scopes
                        .iter()
                        .rposition(|sc| sc.names.contains_key(name))
                        .unwrap_or(0);
                    let outer = idx < floor && !(idx == 0 && self.prelude.contains(name));
                    if outer {
                        self.error_code(
                            "E754",
                            format!("the compensation uses `{name}` from outside its state"),
                            expr.span,
                        )
                        .hint = Some(format!(
                            "put it in the state map inside the burn block (state.{name} = {name}) and read it from there"
                        ));
                    }
                }
            }
            ExprKind::List(items) => items.iter().for_each(|i| self.expr(i)),
            ExprKind::Map(entries) => {
                for (k, v) in entries {
                    if let MapKey::Dynamic(e) = k {
                        self.expr(e);
                    }
                    self.expr(v);
                }
            }
            ExprKind::Unary { expr: inner, .. } => self.expr(inner),
            ExprKind::Binary { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            ExprKind::Call { callee, args } => {
                self.effect_check(callee);
                self.expr(callee);
                args.iter().for_each(|a| self.expr(a));
            }
            ExprKind::Method {
                receiver,
                name,
                args,
                ..
            } => {
                // `fs.rm(...)` parses as a method call on the module ident.
                if let ExprKind::Ident(module) = &receiver.kind {
                    if module == "proc" && name == "run" {
                        self.shell_check(args, expr.span);
                    }
                    if module == "proc" {
                        self.skip_check(args, expr.span);
                    }
                    if self.modules.contains_key(module)
                        && self.lookup(module) == Some(Binding::Immutable)
                    {
                        let qualified = format!("{module}.{name}");
                        self.pack_check(&qualified, args);
                        self.device_check(&qualified, args);
                        if let Some(items) = self.modules.get(module) {
                            if !items.contains(name) {
                                let hint =
                                    crate::interp::closest(name, items.iter().map(String::as_str))
                                        .map(|s| format!("did you mean `{module}.{s}`?"));
                                self.error_code(
                                    "E307",
                                    format!("module `{module}` has no `{name}`"),
                                    expr.span,
                                )
                                .hint = hint;
                            }
                        }
                        self.effect_by_name(&qualified, expr.span);
                    }
                }
                self.expr(receiver);
                args.iter().for_each(|a| self.expr(a));
            }
            ExprKind::Member { object, name, .. } => {
                if let ExprKind::Ident(module) = &object.kind {
                    if let Some(items) = self.modules.get(module) {
                        if self.lookup(module) == Some(Binding::Immutable) && !items.contains(name)
                        {
                            let hint =
                                crate::interp::closest(name, items.iter().map(String::as_str))
                                    .map(|s| format!("did you mean `{module}.{s}`?"));
                            self.error_code(
                                "E307",
                                format!("module `{module}` has no `{name}`"),
                                expr.span,
                            )
                            .hint = hint;
                        }
                    }
                }
                self.expr(object);
            }
            ExprKind::Index { object, index } => {
                self.expr(object);
                self.expr(index);
            }
            ExprKind::Lambda { params, body } => match body {
                LambdaBody::Expr(e) => {
                    self.push();
                    self.fn_depth += 1;
                    for p in params {
                        self.declare(p, Binding::Mutable, Span::default());
                    }
                    self.expr(e);
                    self.fn_depth -= 1;
                    self.pop();
                }
                LambdaBody::Block(b) => self.function(params, &b.stmts),
            },
            ExprKind::Pipe { lhs, rhs } => {
                self.expr(lhs);
                if let ExprKind::Call { callee, args } = &rhs.kind {
                    self.effect_check(callee);
                    self.expr(callee);
                    args.iter().for_each(|a| self.expr(a));
                } else {
                    self.expr(rhs);
                }
            }
        }
    }

    /// A direct call of an effectful global outside any burn.
    fn effect_check(&mut self, callee: &Expr) {
        if let ExprKind::Ident(name) = &callee.kind {
            if self.lookup(name) == Some(Binding::Immutable) {
                self.effect_by_name(name, callee.span);
            }
        }
    }

    fn effect_by_name(&mut self, qualified: &str, span: Span) {
        if !self.effectful.contains_key(qualified) || self.burn_depth > 0 {
            return;
        }
        let msg = format!("{qualified} changes the world, so it must be inside a burn block");
        let hint = format!("wrap it: burn {{ {qualified}(...) }}");
        if self.fn_depth > 0 {
            self.warn_code("E303", msg, span).hint =
                Some(format!("{hint}, or call this function from inside one"));
        } else {
            self.error_code("E303", msg, span).hint = Some(hint);
        }
    }
}

/// The text of a string literal with no interpolation, if that is what `e` is.
fn literal_text(e: &Expr) -> Option<String> {
    match &e.kind {
        ExprKind::Str(pieces) => {
            let mut out = String::new();
            for p in pieces {
                match p {
                    StrPiece::Lit(t) => out.push_str(t),
                    StrPiece::Expr(_) => return None,
                }
            }
            Some(out)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parse;

    fn run(src: &str) -> Report {
        check(&parse(src).unwrap(), &[])
    }

    #[test]
    fn catches_unknown_names_and_stick_assignment() {
        let r = run("roll a = 1\nexhale b\nstick c = 2\nc = 3\n");
        assert_eq!(r.errors.len(), 2);
        assert!(r.errors[0].message.contains("unknown name `b`"));
        assert!(r.errors[1].message.contains("stick"));
    }

    #[test]
    fn effects_outside_burn_are_errors_at_top_level_and_warnings_in_functions() {
        let r = run("fs.rm(\"x\")\npull f() {\n  fs.rm(\"y\")\n}\nburn {\n  fs.rm(\"z\")\n}\n");
        assert_eq!(r.errors.len(), 1, "{:?}", r.errors);
        assert_eq!(r.warnings.len(), 1);
        assert!(run("fs.read_text(\"x\")").ok());
    }

    #[test]
    fn hoisting_and_scopes() {
        assert!(run("exhale f()\npull f() {\n  snuff 1\n}\n").ok());
        let r = run("if true {\n  roll inner = 1\n}\nexhale inner\n");
        assert_eq!(r.errors.len(), 1);
        let r = run("snuff 1\n");
        assert!(r.errors[0].message.contains("snuff"));
        let r = run("break\n");
        assert!(r.errors[0].message.contains("break"));
    }

    #[test]
    fn module_typos_are_caught() {
        let r = run("exhale fs.read_txt(\"x\")");
        assert!(r.errors[0].hint.as_deref().unwrap().contains("read_text"));
    }
}
