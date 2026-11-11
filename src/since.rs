// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! When each part of the language and library arrived, so a script and its
//! pin can be checked against each other before anything runs, and so
//! `cig pin` can infer the version a script needs.
//!
//! Everything not listed here shipped in 1.0.0. The table is the changelog,
//! read the other way round: a feature joins it in the release that adds it.

use crate::syntax::ast::*;
use crate::syntax::span::Span;
use crate::update::Version;

/// Library functions that arrived after 1.0.0, by qualified name.
const BUILTINS: &[(&str, Version)] = &[
    ("proc.csv", Version(1, 1, 0)),
    ("proc.json", Version(1, 1, 0)),
    ("proc.kv", Version(1, 1, 0)),
    ("proc.lines", Version(1, 1, 0)),
    ("proc.pipe", Version(1, 1, 0)),
    ("proc.shell", Version(1, 1, 0)),
    ("proc.text", Version(1, 1, 0)),
    ("pass.issue", Version(1, 20, 0)),
    ("pass.find", Version(1, 20, 0)),
    ("pass.memo", Version(1, 20, 0)),
    ("pass.check", Version(1, 20, 0)),
    ("pass.narrow", Version(1, 20, 0)),
    ("pass.spend", Version(1, 20, 0)),
    ("pass.show", Version(1, 20, 0)),
    ("pass.recording", Version(1, 20, 0)),
];

/// Statements and clauses that arrived after 1.0.0.
const FEATURES: &[(&str, Version)] = &[
    ("finally", Version(1, 1, 0)),
    ("pack { }", Version(1, 1, 0)),
    ("burn (s) { } unburn { }", Version(1, 1, 0)),
    ("burn (s, label) { }", Version(1, 4, 2)),
    ("touches_sealed", Version(1, 5, 0)),
    ("fs.write_text(path, text, {sensitive})", Version(1, 5, 0)),
    ("box: true", Version(1, 6, 0)),
    ("skip", Version(1, 18, 0)),
];

pub const FIRST: Version = Version(1, 0, 0);

/// The release a qualified builtin (`proc.pipe`) arrived in.
pub fn builtin_since(qualified: &str) -> Version {
    BUILTINS
        .iter()
        .find(|(n, _)| *n == qualified)
        .map(|(_, v)| v.clone())
        .unwrap_or(FIRST)
}

/// One use of something newer than 1.0.0.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Use {
    pub what: String,
    pub since: Version,
    pub span: Span,
}

/// Every use in a program of a feature newer than 1.0.0, in source order,
/// each reported once.
pub fn uses(program: &Program) -> Vec<Use> {
    let mut out = Vec::new();
    for s in &program.body {
        stmt(s, &mut out);
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|u| seen.insert(u.what.clone()));
    out
}

/// The lowest version that has everything the program uses.
pub fn minimum(program: &Program) -> Version {
    uses(program)
        .into_iter()
        .map(|u| u.since)
        .max()
        .unwrap_or(FIRST)
}

fn note(out: &mut Vec<Use>, what: &str, since: Version, span: Span) {
    if since > FIRST {
        out.push(Use {
            what: what.to_string(),
            since,
            span,
        });
    }
}

fn block(b: &Block, out: &mut Vec<Use>) {
    for s in &b.stmts {
        stmt(s, out);
    }
}

fn stmt(s: &Stmt, out: &mut Vec<Use>) {
    match s {
        Stmt::Declare { value, .. } => expr(value, out),
        Stmt::Assign { target, value, .. } => {
            match target {
                AssignTarget::Var(_) => {}
                AssignTarget::Index { object, index } => {
                    expr(object, out);
                    expr(index, out);
                }
                AssignTarget::Member { object, .. } => expr(object, out),
            }
            expr(value, out);
        }
        Stmt::Pull(f) => block(&f.body, out),
        Stmt::Chain(c) => c.steps.iter().for_each(|e| expr(e, out)),
        Stmt::Pack(p) => {
            note(out, "pack { }", feature("pack { }"), p.span);
            p.roots.iter().for_each(|e| expr(e, out));
        }
        Stmt::Snuff { value, .. } => {
            if let Some(v) = value {
                expr(v, out);
            }
        }
        Stmt::Exhale { values, .. } => values.iter().for_each(|e| expr(e, out)),
        Stmt::Cough { value, .. } => expr(value, out),
        Stmt::If {
            branches,
            otherwise,
            ..
        } => {
            for (c, b) in branches {
                expr(c, out);
                block(b, out);
            }
            if let Some(b) = otherwise {
                block(b, out);
            }
        }
        Stmt::While { cond, body, .. } => {
            expr(cond, out);
            block(body, out);
        }
        Stmt::For { iter, body, .. } => {
            expr(iter, out);
            block(body, out);
        }
        Stmt::Break(_) | Stmt::Continue(_) => {}
        Stmt::Try {
            body,
            handler,
            finally,
            span,
            ..
        } => {
            block(body, out);
            if let Some(h) = handler {
                block(h, out);
            }
            if let Some(f) = finally {
                note(out, "finally", feature("finally"), *span);
                block(f, out);
            }
        }
        Stmt::Burn {
            body,
            unburn,
            label,
            span,
            ..
        } => {
            if label.is_some() {
                note(
                    out,
                    "burn (s, label) { }",
                    feature("burn (s, label) { }"),
                    *span,
                );
            }
            if unburn.is_some() {
                note(
                    out,
                    "burn (s) { } unburn { }",
                    feature("burn (s) { } unburn { }"),
                    *span,
                );
            }
            block(body, out);
            if let Some(u) = unburn {
                block(u, out);
            }
        }
        Stmt::Expr(e) => expr(e, out),
    }
}

/// What a call's arguments use that arrived after the builtin itself: a
/// hop option (`{box: true}`, `touches_sealed`), or `fs.write_text`'s
/// third argument.
fn newer_arguments(qualified: &str, args: &[Expr], span: Span, out: &mut Vec<Use>) {
    if qualified.starts_with("proc.") {
        if let Some(Expr {
            kind: ExprKind::Map(entries),
            ..
        }) = args.last()
        {
            for (k, _) in entries {
                let MapKey::Static(key) = k else { continue };
                match key.as_str() {
                    "touches_sealed" => {
                        note(out, "touches_sealed", feature("touches_sealed"), span)
                    }
                    "box" => note(out, "box: true", feature("box: true"), span),
                    "skip" => note(out, "skip", feature("skip"), span),
                    _ => {}
                }
            }
        }
    } else if matches!(qualified, "fs.write_text" | "fs.append_text") && args.len() >= 3 {
        let what = "fs.write_text(path, text, {sensitive})";
        note(out, what, feature(what), span);
    }
}

fn feature(name: &str) -> Version {
    FEATURES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, v)| v.clone())
        .unwrap_or(FIRST)
}

fn expr(e: &Expr, out: &mut Vec<Use>) {
    match &e.kind {
        ExprKind::Null | ExprKind::Bool(_) | ExprKind::Int(_) | ExprKind::Float(_) => {}
        ExprKind::Ident(_) => {}
        ExprKind::Str(pieces) => {
            for p in pieces {
                if let StrPiece::Expr(x) = p {
                    expr(x, out);
                }
            }
        }
        ExprKind::List(items) => items.iter().for_each(|i| expr(i, out)),
        ExprKind::Map(entries) => {
            for (k, v) in entries {
                if let MapKey::Dynamic(x) = k {
                    expr(x, out);
                }
                expr(v, out);
            }
        }
        ExprKind::Unary { expr: inner, .. } => expr(inner, out),
        ExprKind::Binary { lhs, rhs, .. } | ExprKind::Pipe { lhs, rhs } => {
            expr(lhs, out);
            expr(rhs, out);
        }
        ExprKind::Call { callee, args } => {
            expr(callee, out);
            args.iter().for_each(|a| expr(a, out));
        }
        ExprKind::Method {
            receiver,
            name,
            args,
            ..
        } => {
            // `proc.pipe(...)` parses as a method call on the module ident.
            if let ExprKind::Ident(module) = &receiver.kind {
                let qualified = format!("{module}.{name}");
                note(out, &qualified, builtin_since(&qualified), e.span);
                newer_arguments(&qualified, args, e.span, out);
            }
            expr(receiver, out);
            args.iter().for_each(|a| expr(a, out));
        }
        ExprKind::Member { object, name, .. } => {
            if let ExprKind::Ident(module) = &object.kind {
                let qualified = format!("{module}.{name}");
                note(out, &qualified, builtin_since(&qualified), e.span);
            }
            expr(object, out);
        }
        ExprKind::Index { object, index } => {
            expr(object, out);
            expr(index, out);
        }
        ExprKind::Lambda { body, .. } => match body {
            LambdaBody::Expr(x) => expr(x, out),
            LambdaBody::Block(b) => block(b, out),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parse;

    #[test]
    fn the_table_infers_what_a_script_needs() {
        let p = parse("exhale 1\nstick s = fs.read_text(\"a\")\n").unwrap();
        assert_eq!(minimum(&p), Version(1, 0, 0));
        assert!(uses(&p).is_empty());
        let p = parse("burn { stick r = proc.pipe([[\"ls\"], [\"wc\"]]) }\n").unwrap();
        let u = uses(&p);
        assert_eq!(u.len(), 1);
        assert_eq!(u[0].what, "proc.pipe");
        assert_eq!(minimum(&p), Version(1, 1, 0));
        let p = parse("try { exhale 1 } finally { exhale 2 }\n").unwrap();
        assert_eq!(uses(&p)[0].what, "finally");
        let p =
            parse("pack { \"./build\" }\nburn (s) { s.x = 1 } unburn { exhale s.x }\n").unwrap();
        let found = uses(&p);
        let names: Vec<&str> = found.iter().map(|u| u.what.as_str()).collect();
        assert_eq!(names, vec!["pack { }", "burn (s) { } unburn { }"]);
        // A lambda's own `pack(x) =>` is not the pack statement.
        let p = parse("stick f = pack(x) => x + 1\n").unwrap();
        assert!(uses(&p).is_empty());
    }

    #[test]
    fn hop_options_and_the_third_argument_count_too() {
        let p = parse(
            "pack { \"./p\" }\nburn {\n  proc.run(\"x\", [], {cwd: \"p\", touches_sealed: [\"*.db\"]})\n  fs.write_text(\"a\", \"b\", {sensitive: true})\n}\n",
        )
        .unwrap();
        let names: Vec<String> = uses(&p).into_iter().map(|u| u.what).collect();
        assert!(names.iter().any(|n| n == "touches_sealed"), "{names:?}");
        assert!(
            names
                .iter()
                .any(|n| n == "fs.write_text(path, text, {sensitive})"),
            "{names:?}"
        );
        assert_eq!(minimum(&p), Version(1, 5, 0));
        let p = parse("pack { \"./p\" }\nburn { proc.run(\"x\", [], {box: true}) }\n").unwrap();
        assert_eq!(minimum(&p), Version(1, 6, 0));
        // The same keys on a map that is not a hop's options are nothing.
        let p = parse("stick m = {box: true, touches_sealed: 1}\n").unwrap();
        assert!(uses(&p).is_empty());
    }
}
