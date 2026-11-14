// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

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
use std::cell::{Cell, OnceCell};
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Binding {
    Mutable,
    Immutable,
    Function,
}

/// A scope's names: each one's binding and its position among the names the
/// scope declares, in the order the evaluator declares them (hoisted pulls
/// first, then each `roll`, `stick` and `chain` as it runs).
type Names = HashMap<String, (Binding, u32)>;

struct Scope<'a> {
    names: Names,
    /// The scope of a `pull` or a lambda: what runs in it runs when called.
    is_fn: bool,
    /// Reads made from inside a function body that passed over this scope
    /// without finding their name, by name. If this scope declares the name
    /// later, a call after that finds the new one, so the read is not
    /// resolved.
    misses: HashMap<String, Vec<&'a Cell<Res>>>,
    /// Where this scope's names go when the walk is done with it, for the
    /// function bodies found inside it and left for their first call, and
    /// the last [`Outer`] made for it.
    kept: Option<(Rc<OnceCell<Names>>, Rc<Outer>)>,
}

impl Scope<'_> {
    fn add(&mut self, name: &str, binding: Binding) {
        let idx = match self.names.get(name) {
            Some((_, i)) => *i,
            None => u32::try_from(self.names.len()).unwrap_or(u32::MAX),
        };
        self.names.insert(name.to_string(), (binding, idx));
    }

    fn new(is_fn: bool) -> Self {
        Scope {
            names: HashMap::new(),
            is_fn,
            misses: HashMap::new(),
            kept: None,
        }
    }
}

/// One of the scopes around a function body that [`resolve`] left for its
/// first call, as the walk that found the body saw it: how many names it had
/// declared by then, and all of its names once the walk is done with it (it
/// is, before the body can run). A name it declared after the body was
/// found is one the body's reads passed over then: [`Checker::resolve`]
/// leaves such a read unresolved, as the walk does when the declaration
/// comes after the read (the scope's `misses`).
pub(crate) struct Outer {
    names: Rc<OnceCell<Names>>,
    seen: usize,
    is_fn: bool,
    parent: Option<Rc<Outer>>,
}

/// `Res::Local`, or `Unresolved` where the depth does not fit.
fn local(depth: usize, idx: u32) -> Res {
    match u16::try_from(depth) {
        Ok(depth) => Res::Local {
            depth,
            idx: u16::try_from(idx).unwrap_or(u16::MAX),
        },
        Err(_) => Res::Unresolved,
    }
}

pub struct Checker<'a> {
    scopes: Vec<Scope<'a>>,
    /// The scopes around the body this walk resolves, when it is one that
    /// [`resolve`] left for its first call ([`enter`]); the outermost of
    /// them is the prelude's.
    outer: Option<Rc<Outer>>,
    /// Off for [`resolve`] and [`enter`]: the same walk, resolving the same
    /// names, with every diagnostic and every check that only leads to one
    /// left out, and a function body's (a block's; a lambda's expression is
    /// walked where it is) left for its first call.
    diagnose: bool,
    /// The bodies left for their first call, for the tests that enter them.
    #[cfg(test)]
    found: Vec<(Rc<Block>, Vec<String>)>,
    /// Names the prelude provides (modules and builtins), which a
    /// compensation may use.
    prelude: std::collections::HashSet<String>,
    /// Inside an `unburn { }` block: the scope index the block starts at.
    /// Names resolved below it (other than the prelude) are E754.
    comp_floor: Option<usize>,
    /// Blocks open right now that have no scope of their own (they declare
    /// nothing); they still count as nesting for `pack`.
    elided: usize,
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

/// Where a `pack { }` may not be.
#[derive(Clone, Copy)]
pub(crate) enum PackPlace {
    /// After another statement.
    NotFirst,
    /// Inside a block.
    InBlock,
    /// A script's second.
    Second,
}

/// E753 for a `pack { }` in `place`: the checker's error, and the VM's for
/// a `pack` instruction a module runs there.
pub(crate) fn pack_misplaced(place: PackPlace, span: Span) -> Diagnostic {
    let (message, hint) = match place {
        PackPlace::NotFirst => (
            "pack { } must be the first statement of the script",
            "move it above everything else; the scope is declared before anything can burn",
        ),
        PackPlace::InBlock => (
            "pack { } cannot be declared inside a block",
            "declare it once, at the top of the script",
        ),
        PackPlace::Second => (
            "a second pack { }; a script declares one scope",
            "merge the roots into the first pack { }",
        ),
    };
    Diagnostic::new(Kind::Check, message)
        .code("E753")
        .at(span)
        .with_hint(hint)
}

/// Check a whole program. `extra_globals` are names the host will provide
/// (for the REPL, everything already defined).
pub fn check(program: &Program, extra_globals: &[String]) -> Report {
    let mut c = Checker::new(extra_globals, true);
    for (i, stmt) in program.body.iter().enumerate() {
        if let Stmt::Pack(p) = stmt {
            if i != 0 {
                c.errors.push(pack_misplaced(PackPlace::NotFirst, p.span));
            }
        }
    }
    c.block_stmts(&program.body);
    Report {
        errors: c.errors,
        warnings: c.warnings,
    }
}

/// Resolve the names of a program that is not checked here (a compiled
/// program, which was checked when it was compiled, and `--no-check`): the
/// walk [`check`] makes, which sets the same [`Res`] on every node, and
/// nothing else. No diagnostic is made and nothing is printed. A name it
/// cannot place, in a script [`check`] would refuse as in any other, is
/// left `Unresolved` and looked up by name when it is read.
///
/// The top level is resolved here; a function body (a block) is resolved
/// the first time it runs ([`enter`]), with what it needs from the scopes
/// around it kept from this walk, so a start does the work of the
/// functions its run uses.
pub fn resolve(program: &Program) {
    let mut c = Checker::new(&[], false);
    c.block_stmts(&program.body);
    c.finish();
}

/// Before a function body runs: resolve its names, the first time, if
/// [`resolve`] left it for now. Nothing to do for any other body.
#[inline]
pub fn enter(params: &[String], body: &Block) {
    if let Some(outer) = body.pending.take() {
        enter_now(outer, params, body);
    }
}

#[cold]
#[inline(never)]
fn enter_now<'b>(outer: Rc<Outer>, params: &[String], body: &'b Block) -> Checker<'b> {
    let mut c = Checker::around(outer);
    c.function(params, &body.stmts);
    c.finish();
    c
}

impl<'a> Checker<'a> {
    fn new(extra_globals: &[String], diagnose: bool) -> Self {
        let mut root = Scope::new(false);
        let mut effectful = HashMap::new();
        let mut modules = HashMap::new();
        for (name, value) in crate::stdlib::prelude() {
            root.add(name, Binding::Immutable);
            if !diagnose {
                continue;
            }
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
        root.add("args", Binding::Immutable);
        for g in extra_globals {
            root.add(g, Binding::Mutable);
        }
        let prelude: std::collections::HashSet<String> = if diagnose {
            root.names.keys().cloned().collect()
        } else {
            std::collections::HashSet::new()
        };
        Self {
            scopes: vec![root, Scope::new(false)],
            outer: None,
            diagnose,
            #[cfg(test)]
            found: Vec::new(),
            prelude,
            comp_floor: None,
            elided: 0,
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

    /// The walk of a body [`resolve`] left for its first call, inside the
    /// scopes kept for it.
    fn around(outer: Rc<Outer>) -> Self {
        Self {
            scopes: Vec::new(),
            outer: Some(outer),
            diagnose: false,
            #[cfg(test)]
            found: Vec::new(),
            prelude: std::collections::HashSet::new(),
            comp_floor: None,
            elided: 0,
            pack_roots: Vec::new(),
            pack_seen: false,
            hoisted_chains: Vec::new(),
            effectful: HashMap::new(),
            modules: HashMap::new(),
            burn_depth: 0,
            fn_depth: 0,
            loop_depth: 0,
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// The walk is done: every scope still open is closed (its names go to
    /// the bodies that keep it).
    fn finish(&mut self) {
        while !self.scopes.is_empty() {
            self.pop();
        }
    }

    /// Leave a function body for its first call ([`enter`]), with the
    /// scopes open now, as they are now, kept for it.
    fn defer(&mut self, body: &Rc<Block>, params: &[String]) {
        let mut parent = self.outer.clone();
        for s in &mut self.scopes {
            let reuse = s.kept.as_ref().and_then(|(_, o)| {
                let same_parent = match (&o.parent, &parent) {
                    (Some(a), Some(b)) => Rc::ptr_eq(a, b),
                    (None, None) => true,
                    _ => false,
                };
                (o.seen == s.names.len() && same_parent).then(|| Rc::clone(o))
            });
            let node = match reuse {
                Some(o) => o,
                None => {
                    let cell = s
                        .kept
                        .as_ref()
                        .map(|(c, _)| Rc::clone(c))
                        .unwrap_or_default();
                    let o = Rc::new(Outer {
                        names: Rc::clone(&cell),
                        seen: s.names.len(),
                        is_fn: s.is_fn,
                        parent,
                    });
                    s.kept = Some((cell, Rc::clone(&o)));
                    o
                }
            };
            parent = Some(node);
        }
        if let Some(outer) = parent {
            body.pending.set(outer);
        }
        #[cfg(test)]
        self.found.push((Rc::clone(body), params.to_vec()));
        #[cfg(not(test))]
        let _ = params;
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
        self.scopes.push(Scope::new(false));
    }

    /// The scope of a function body: the one `call_function` makes.
    fn push_fn(&mut self) {
        self.scopes.push(Scope::new(true));
    }

    fn pop(&mut self) {
        if let Some(Scope {
            names,
            kept: Some((cell, _)),
            ..
        }) = self.scopes.pop()
        {
            let _ = cell.set(names);
        }
    }

    fn declare(&mut self, name: &str, binding: Binding, span: Span) {
        let scope = self.scopes.last_mut().expect("scope stack is never empty");
        if scope.names.contains_key(name) {
            if !self.diagnose {
                return;
            }
            self.error_code(
                "E306",
                format!("`{name}` is already declared in this scope"),
                span,
            )
            .hint = Some(format!("assign with `{name} = ...`, or pick another name"));
            return;
        }
        let idx = u32::try_from(scope.names.len()).unwrap_or(u32::MAX);
        scope.names.insert(name.to_string(), (binding, idx));
        // A function body that read this name from further out, before it
        // was declared here, finds this one when it is called after.
        if scope.misses.is_empty() {
            return;
        }
        if let Some(cells) = scope.misses.remove(name) {
            for cell in cells {
                cell.set(Res::Unresolved);
            }
        }
    }

    fn lookup(&self, name: &str) -> Option<Binding> {
        self.scopes
            .iter()
            .rev()
            .find_map(|s| s.names.get(name).map(|(b, _)| *b))
    }

    /// Record where a name is found: how many scopes up and which of the
    /// names declared there (#146). The scopes here are the ones the
    /// evaluator makes, one for one; what could differ at run time is left
    /// `Unresolved`, and the evaluator looks it up by name.
    fn resolve(&mut self, name: &str, cell: &'a Cell<Res>) {
        if self.comp_floor.is_some() {
            // An `unburn { }` runs later from the journal, never from this tree.
            return;
        }
        let live = self.scopes.len();
        let mut inside_fn = false;
        for i in (0..live).rev() {
            if let Some((_, idx)) = self.scopes[i].names.get(name) {
                let found = if i == 0 && self.outer.is_none() {
                    Res::Prelude
                } else {
                    local(live - 1 - i, *idx)
                };
                cell.set(found);
                return;
            }
            if inside_fn {
                let misses = &mut self.scopes[i].misses;
                match misses.get_mut(name) {
                    Some(cells) => cells.push(cell),
                    None => {
                        misses.insert(name.to_string(), vec![cell]);
                    }
                }
            }
            inside_fn |= self.scopes[i].is_fn;
        }
        // The scopes kept around a body left for its first call: each with
        // all its names now, of which the first `seen` were declared when
        // the body was found.
        let mut depth = live;
        let mut at = self.outer.as_deref();
        while let Some(o) = at {
            let Some(names) = o.names.get() else {
                // Not done with (never, before the body runs): not placed.
                cell.set(Res::Unresolved);
                return;
            };
            if let Some((_, idx)) = names.get(name) {
                if usize::try_from(*idx).is_ok_and(|i| i < o.seen) {
                    cell.set(if o.parent.is_none() {
                        Res::Prelude
                    } else {
                        local(depth, *idx)
                    });
                    return;
                }
                if inside_fn {
                    // Declared there after the body was found: the read
                    // missed it then and finds it on a later call.
                    cell.set(Res::Unresolved);
                    return;
                }
            }
            inside_fn |= o.is_fn;
            depth += 1;
            at = o.parent.as_deref();
        }
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

    fn block_stmts(&mut self, stmts: &'a [Stmt]) {
        // Hoist functions so calls before the definition resolve.
        for stmt in stmts {
            if let Stmt::Pull(f) = stmt {
                self.declare(&f.name, Binding::Function, f.span);
            }
            if let Stmt::Chain(c) = stmt {
                if self.diagnose {
                    self.hoisted_chains.push(c.name.clone());
                }
            }
        }
        for stmt in stmts {
            self.stmt(stmt);
        }
    }

    fn block(&mut self, block: &'a Block) {
        // The evaluator makes no scope for a block that declares nothing.
        if !block.declares() {
            self.elided += 1;
            self.block_stmts(&block.stmts);
            self.elided -= 1;
            return;
        }
        self.push();
        self.block_stmts(&block.stmts);
        self.pop();
    }

    fn stmt(&mut self, stmt: &'a Stmt) {
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
                    AssignTarget::Var(name, cell) => {
                        self.resolve(name, cell);
                        if !self.diagnose {
                            return;
                        }
                        match self.lookup(name) {
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
                        }
                    }
                    AssignTarget::Index { object, index } => {
                        self.expr(object);
                        self.expr(index);
                    }
                    AssignTarget::Member { object, .. } => self.expr(object),
                }
            }
            Stmt::Pull(f) if !self.diagnose => self.defer(&f.body, &f.params),
            Stmt::Pull(f) => self.function(&f.params, &f.body.stmts),
            Stmt::Chain(ch) => {
                for step in &ch.steps {
                    self.expr(step);
                }
                self.declare(&ch.name, Binding::Immutable, ch.span);
            }
            Stmt::Snuff { value, span } => {
                if self.diagnose && self.fn_depth == 0 {
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
                if self.diagnose && self.loop_depth == 0 {
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
                // Nothing in a compensation is resolved (see `resolve`), so
                // without diagnostics there is nothing to walk there.
                if let Some(u) = unburn.as_ref().filter(|_| self.diagnose) {
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
                if !self.diagnose {
                } else if self.scopes.len() + self.elided > 2 {
                    self.errors.push(pack_misplaced(PackPlace::InBlock, p.span));
                } else if self.pack_seen {
                    self.errors.push(pack_misplaced(PackPlace::Second, p.span));
                }
                self.pack_seen = true;
                if self.diagnose {
                    self.pack_options(&p.options);
                }
                for r in &p.roots {
                    match literal_text(r) {
                        Some(_) if !self.diagnose => {}
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

    /// A pack's options (E766): each one cig knows, given once, its value
    /// the literal `true` or `false`. They change what a compiled program
    /// does, never the scope, so nothing else here reads them.
    fn pack_options(&mut self, options: &[PackOption]) {
        for (i, o) in options.iter().enumerate() {
            if !PACK_OPTIONS.contains(&o.name.as_str()) {
                self.error_code(
                    "E766",
                    format!("`{}` is not a pack option", o.name),
                    o.name_span,
                )
                .hint = Some(format!(
                    "the pack's option is {PACK_OWN_FLAGS}: true; a root is a string, pack {{ \"./build\" }}"
                ));
                continue;
            }
            if options[..i].iter().any(|e| e.name == o.name) {
                self.error_code(
                    "E766",
                    format!("the pack gives `{}` twice", o.name),
                    o.name_span,
                )
                .hint = Some("keep one".to_string());
                continue;
            }
            if !matches!(o.value.kind, ExprKind::Bool(_)) {
                self.error_code(
                    "E766",
                    format!("`{}` takes true or false, written as it is", o.name),
                    o.value.span,
                )
                .hint = Some(format!("pack {{ {}: true }}", o.name));
            }
        }
    }

    fn function(&mut self, params: &[String], body: &'a [Stmt]) {
        self.push_fn();
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

    fn expr(&mut self, expr: &'a Expr) {
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
                self.resolve(name, &expr.res);
                if !self.diagnose {
                } else if self.lookup(name).is_none() {
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
                if self.diagnose {
                    self.effect_check(callee);
                }
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
                if let (true, ExprKind::Ident(module)) = (self.diagnose, &receiver.kind) {
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
                if let (true, ExprKind::Ident(module)) = (self.diagnose, &object.kind) {
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
                    self.push_fn();
                    self.fn_depth += 1;
                    for p in params {
                        self.declare(p, Binding::Mutable, Span::default());
                    }
                    self.expr(e);
                    self.fn_depth -= 1;
                    self.pop();
                }
                LambdaBody::Block(b) if !self.diagnose => self.defer(b, params),
                LambdaBody::Block(b) => self.function(params, &b.stmts),
            },
            ExprKind::Pipe { lhs, rhs } => {
                self.expr(lhs);
                if let ExprKind::Call { callee, args } = &rhs.kind {
                    if self.diagnose {
                        self.effect_check(callee);
                    }
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

    #[test]
    fn pack_options_are_checked_where_they_are_written() {
        assert!(run("pack { own_flags: true }\n").ok());
        assert!(run("pack { \".\", own_flags: false }\n").ok());
        let at = |src: &str| -> Vec<(String, u32, u32, String)> {
            run(src)
                .errors
                .iter()
                .map(|e| {
                    let s = e.span.expect("a position");
                    (e.code.to_string(), s.line, s.col, e.message.clone())
                })
                .collect()
        };
        let e = at("pack { \".\", own_flag: true }\n");
        assert_eq!(e.len(), 1, "{e:?}");
        assert_eq!((e[0].0.as_str(), e[0].1, e[0].2), ("E766", 1, 13), "{e:?}");
        assert!(e[0].3.contains("`own_flag` is not a pack option"), "{e:?}");
        let e = at("pack {\n  own_flags: true\n  own_flags: false\n}\n");
        assert_eq!(e.len(), 1, "{e:?}");
        assert_eq!((e[0].0.as_str(), e[0].1, e[0].2), ("E766", 3, 3), "{e:?}");
        assert!(e[0].3.contains("twice"), "{e:?}");
        for value in ["\"true\"", "1", "yes", "not false", "null"] {
            let e = at(&format!("roll yes = true\npack {{ own_flags: {value} }}\n"));
            let e: Vec<_> = e.into_iter().filter(|e| e.0 == "E766").collect();
            assert_eq!(e.len(), 1, "{value}: {e:?}");
            assert_eq!((e[0].1, e[0].2), (2, 19), "{value}: {e:?}");
            assert!(e[0].3.contains("true or false"), "{e:?}");
        }
        // The pack's other rules hold for a rootless pack as for any.
        let e = at("roll x = 1\npack { own_flags: true }\n");
        assert_eq!(e[0].0, "E753", "{e:?}");
    }

    /// Every `.cig` file under `dir`, at any depth, in a stable order.
    fn scripts_under(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for p in paths {
            if p.is_dir() {
                scripts_under(&p, out);
            } else if p.extension().is_some_and(|e| e == "cig") {
                out.push(p);
            }
        }
    }

    /// The names that interleave function bodies with later declarations,
    /// shadowing, loops, burns and compensations, beside the suite's files.
    const RESOLVE_CASES: [&str; 6] = [
        "pull f() { snuff g() + x }\nroll x = 1\npull g() { snuff x }\nexhale f()\nroll late = 2\n",
        "roll a = 1\npull f() {\n  roll b = a\n  pull h() { snuff a + b + c }\n  roll c = 3\n  snuff h\n}\nroll a2 = f()\nexhale a2()\n",
        "roll xs = []\nfor i in 0..3 {\n  roll j = i\n  xs.push(pack() => i + j)\n}\nexhale xs.map(pack(f) => f())\n",
        "burn s {\n  s.v = 1\n  roll inner = 2\n} unburn s {\n  exhale s.v\n}\nroll print2 = print\nprint2(len([1]))\n",
        "try {\n  cough \"x\"\n} catch e {\n  exhale e\n} finally {\n  roll z = 1\n  exhale z\n}\n",
        "chain c = [pack() => 1]\npull use() { snuff later }\nroll later = c\nwhile false { break }\n",
    ];

    /// [`resolve`], then every function body entered as a run enters it
    /// (an outer body before the bodies inside it): what [`enter`] does on
    /// a body's first call. The number of bodies entered.
    fn resolve_and_enter_all(p: &Program) -> usize {
        let mut c = Checker::new(&[], false);
        c.block_stmts(&p.body);
        c.finish();
        let mut todo = std::mem::take(&mut c.found);
        let mut entered = 0;
        while let Some((body, params)) = todo.pop() {
            let outer = body.pending.take().expect("left for its first call");
            let mut c = enter_now(outer, &params, &body);
            todo.append(&mut c.found);
            entered += 1;
        }
        entered
    }

    /// `resolve`, with every function body entered, sets the `Res`
    /// `check` sets, on every node: the trees' debug forms, which show every
    /// node's `Res`, are the same, for every script the suite has (the
    /// examples, the bench set, every script under `tests/`) and for the
    /// cases above, those `check` refuses among them.
    #[test]
    fn resolve_sets_what_check_sets_on_every_node() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        for dir in ["examples", "bench", "tests"] {
            scripts_under(&root.join(dir), &mut files);
        }
        let mut sources: Vec<(String, String)> = files
            .iter()
            .map(|p| {
                let text = std::fs::read_to_string(p).expect("reads");
                (p.display().to_string(), text)
            })
            .collect();
        assert!(sources.len() > 60, "{} scripts found", sources.len());
        for (i, case) in RESOLVE_CASES.iter().enumerate() {
            sources.push((format!("case {i}"), case.to_string()));
        }
        let (mut compared, mut refused, mut resolved_some) = (0, 0, 0);
        for (name, src) in &sources {
            let Ok(checked) = parse(src) else { continue };
            let unresolved = format!("{checked:?}");
            if !check(&checked, &[]).ok() {
                refused += 1;
            }
            let quiet = parse(src).expect("parses twice");
            resolve_and_enter_all(&quiet);
            let want = format!("{checked:?}");
            assert_eq!(
                format!("{quiet:?}"),
                want,
                "{name}: resolve differs from check"
            );
            resolved_some += usize::from(want != unresolved);
            compared += 1;
        }
        assert!(
            compared > 60 && refused > 3,
            "{compared} compared, {refused} refused"
        );
        assert!(resolved_some > 60, "{resolved_some} resolved anything");
    }

    /// `resolve` leaves a function body as it is until the body is entered,
    /// and `enter` resolves it once, from the scopes as the walk found them:
    /// a name the enclosing scope declares after the function is left to
    /// the lookup by name, as `check` leaves it.
    #[test]
    fn a_body_is_resolved_when_it_is_first_entered() {
        let src = "roll a = 1\nroll b = 2\nif true {\n  pull f(p) {\n    snuff a + p + b\n  }\n  roll b = 3\n  exhale f(0)\n}\n";
        let pull = |p: &Program| -> FnDecl {
            let Stmt::If { branches, .. } = &p.body[2] else {
                panic!("an if")
            };
            let Stmt::Pull(f) = &branches[0].1.stmts[0] else {
                panic!("a pull")
            };
            f.clone()
        };
        let p = parse(src).unwrap();
        resolve(&p);
        let f = pull(&p);
        assert!(f.body.pending.is_pending());
        let before = format!("{:?}", f.body.stmts);
        assert!(!before.contains("Local"), "{before}");
        enter(&f.params, &f.body);
        assert!(!f.body.pending.is_pending());
        let after = format!("{:?}", f.body.stmts);
        let checked = parse(src).unwrap();
        assert!(check(&checked, &[]).ok());
        assert_eq!(after, format!("{:?}", pull(&checked).body.stmts));
        // `a` two scopes up, `p` in the body's own, `b` by name.
        assert_eq!(after.matches("Local").count(), 2, "{after}");
        assert!(after.contains("Unresolved"), "{after}");
        // A second entry changes nothing.
        enter(&f.params, &f.body);
        assert_eq!(format!("{:?}", f.body.stmts), after);
    }

    /// `resolve` makes no diagnostic: a script full of what `check` refuses
    /// goes through it, and the same tree checked still reports all of it.
    #[test]
    fn resolve_reports_nothing() {
        let src = "exhale nope\nroll a = 1\nroll a = 2\nstick s = 1\ns = 2\nbreak\nsnuff 1\nfs.rm(\"x\")\nexhale fs.read_txt(\"x\")\n";
        let p = parse(src).unwrap();
        let mut c = Checker::new(&[], false);
        c.block_stmts(&p.body);
        assert!(c.errors.is_empty() && c.warnings.is_empty());
        assert!(run(src).errors.len() >= 6);
    }
}
