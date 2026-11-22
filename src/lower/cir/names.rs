// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
//! The names a statement mentions, anywhere inside it: in its expressions,
//! in the blocks it holds and in the functions written in it. The lowering
//! asks this to tell whether a function can run before a declaration it
//! reads ([`super::lower`]). A name counts wherever it is written, whatever
//! it resolves to there: the answer may name too much, never too little.

use crate::syntax::ast::{AssignTarget, Block, Expr, ExprKind, LambdaBody, MapKey, Stmt, StrPiece};
use std::collections::HashSet;

/// Add every name `s` mentions to `out`.
pub fn in_stmt<'a>(s: &'a Stmt, out: &mut HashSet<&'a str>) {
    match s {
        Stmt::Declare { value, .. } | Stmt::Cough { value, .. } => in_expr(value, out),
        Stmt::Assign { target, value, .. } => {
            match target {
                AssignTarget::Var(name, _) => {
                    out.insert(name);
                }
                AssignTarget::Index { object, index } => {
                    in_expr(object, out);
                    in_expr(index, out);
                }
                AssignTarget::Member { object, .. } => in_expr(object, out),
            }
            in_expr(value, out);
        }
        Stmt::Pull(f) => in_block(&f.body, out),
        Stmt::Chain(c) => c.steps.iter().for_each(|e| in_expr(e, out)),
        Stmt::Pack(p) => {
            p.roots.iter().for_each(|e| in_expr(e, out));
            p.options.iter().for_each(|o| in_expr(&o.value, out));
        }
        Stmt::Snuff { value, .. } => {
            if let Some(e) = value {
                in_expr(e, out);
            }
        }
        Stmt::Exhale { values, .. } => values.iter().for_each(|e| in_expr(e, out)),
        Stmt::If {
            branches,
            otherwise,
            ..
        } => {
            for (cond, body) in branches {
                in_expr(cond, out);
                in_block(body, out);
            }
            if let Some(b) = otherwise {
                in_block(b, out);
            }
        }
        Stmt::While { cond, body, .. } => {
            in_expr(cond, out);
            in_block(body, out);
        }
        Stmt::For { iter, body, .. } => {
            in_expr(iter, out);
            in_block(body, out);
        }
        Stmt::Break(_) | Stmt::Continue(_) => {}
        Stmt::Try {
            body,
            handler,
            finally,
            ..
        } => {
            in_block(body, out);
            for b in [handler, finally].into_iter().flatten() {
                in_block(b, out);
            }
        }
        Stmt::Burn {
            label,
            body,
            unburn,
            ..
        } => {
            if let Some(e) = label {
                in_expr(e, out);
            }
            in_block(body, out);
            if let Some(b) = unburn {
                in_block(b, out);
            }
        }
        Stmt::Expr(e) => in_expr(e, out),
    }
}

fn in_block<'a>(b: &'a Block, out: &mut HashSet<&'a str>) {
    b.stmts.iter().for_each(|s| in_stmt(s, out));
}

fn in_expr<'a>(e: &'a Expr, out: &mut HashSet<&'a str>) {
    match &e.kind {
        ExprKind::Null | ExprKind::Bool(_) | ExprKind::Int(_) | ExprKind::Float(_) => {}
        ExprKind::Str(pieces) => {
            for p in pieces {
                match p {
                    StrPiece::Lit(_) => {}
                    StrPiece::Expr(e) => in_expr(e, out),
                }
            }
        }
        ExprKind::Ident(name) => {
            out.insert(name);
        }
        ExprKind::List(items) => items.iter().for_each(|e| in_expr(e, out)),
        ExprKind::Map(entries) => {
            for (key, value) in entries {
                match key {
                    MapKey::Static(_) => {}
                    MapKey::Dynamic(e) => in_expr(e, out),
                }
                in_expr(value, out);
            }
        }
        ExprKind::Unary { expr, .. } => in_expr(expr, out),
        ExprKind::Binary { lhs, rhs, .. } | ExprKind::Pipe { lhs, rhs } => {
            in_expr(lhs, out);
            in_expr(rhs, out);
        }
        ExprKind::Call { callee, args } => {
            in_expr(callee, out);
            args.iter().for_each(|e| in_expr(e, out));
        }
        ExprKind::Method { receiver, args, .. } => {
            in_expr(receiver, out);
            args.iter().for_each(|e| in_expr(e, out));
        }
        ExprKind::Member { object, .. } => in_expr(object, out),
        ExprKind::Index { object, index } => {
            in_expr(object, out);
            in_expr(index, out);
        }
        ExprKind::Lambda { body, .. } => match body {
            LambdaBody::Expr(e) => in_expr(e, out),
            LambdaBody::Block(b) => in_block(b, out),
        },
    }
}
