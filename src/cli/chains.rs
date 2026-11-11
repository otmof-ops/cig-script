// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! `cig chains`: list the chains a script declares, without running it.

use super::{exit, Ctx};
use crate::syntax::ast::{ExprKind, Stmt};
use crate::syntax::parse;
use std::path::Path;

pub struct ChainInfo {
    pub name: String,
    pub line: u32,
    pub steps: Vec<String>,
}

/// Top-level chain declarations, in source order.
pub fn declared(program: &crate::syntax::ast::Program) -> Vec<ChainInfo> {
    program
        .body
        .iter()
        .filter_map(|stmt| match stmt {
            Stmt::Chain(c) => Some(ChainInfo {
                name: c.name.clone(),
                line: c.span.line,
                steps: c
                    .steps
                    .iter()
                    .enumerate()
                    .map(|(n, e)| match &e.kind {
                        ExprKind::Ident(name) => name.clone(),
                        ExprKind::Member { name, .. } => name.clone(),
                        ExprKind::Lambda { .. } => format!("step {} (inline pack)", n + 1),
                        ExprKind::Call { .. } => format!("step {} (call)", n + 1),
                        _ => format!("step {}", n + 1),
                    })
                    .collect(),
            }),
            _ => None,
        })
        .collect()
}

pub fn chains(ctx: &Ctx, file: &Path) -> i32 {
    let source = match super::run::read_source(ctx, file) {
        Ok(s) => s,
        Err(code) => return code,
    };
    chains_source(ctx, &file.to_string_lossy(), &source)
}

/// The chains a script declares, from its text (a compiled program's own).
pub fn chains_source(ctx: &Ctx, name: &str, source: &str) -> i32 {
    let name = name.to_string();
    let source = source.to_string();
    let program = match parse(&source) {
        Ok(p) => p,
        Err(d) => {
            super::run::report(ctx, &d, Some(&name), Some(&source));
            return exit::SYNTAX_ERROR;
        }
    };
    let pack = declared_pack(&program);
    let found = declared(&program);
    if ctx.json {
        let items: Vec<serde_json::Value> = found
            .iter()
            .map(|c| serde_json::json!({"name": c.name, "line": c.line, "steps": c.steps}))
            .collect();
        outln!(
            "{}",
            serde_json::json!({"file": name, "pack": pack, "chains": items})
        );
        return exit::OK;
    }
    if found.is_empty() {
        eprintln!("{name}: no chains declared");
        eprintln!("{}", ctx.dim("declare one with: chain name { step, step }"));
        return exit::OK;
    }
    if let Some(roots) = &pack {
        outln!("{} {}", ctx.dim("pack:"), roots.join(", "));
    }
    for c in &found {
        outln!(
            "{}  {}",
            ctx.bold(&c.name),
            ctx.dim(&format!("(line {})", c.line))
        );
        for (n, s) in c.steps.iter().enumerate() {
            outln!("  {:>2}. {s}", n + 1);
        }
    }
    outln!(
        "{}",
        ctx.dim(&format!("light one with: cig light {name} <chain>"))
    );
    exit::OK
}

/// The literal roots of a script's `pack { }`, if it declares one.
pub fn declared_pack(program: &crate::syntax::ast::Program) -> Option<Vec<String>> {
    program.body.iter().find_map(|stmt| match stmt {
        Stmt::Pack(p) => Some(
            p.roots
                .iter()
                .map(|r| match &r.kind {
                    ExprKind::Str(pieces) => pieces
                        .iter()
                        .map(|piece| match piece {
                            crate::syntax::ast::StrPiece::Lit(t) => t.clone(),
                            crate::syntax::ast::StrPiece::Expr(_) => "${...}".to_string(),
                        })
                        .collect::<String>(),
                    _ => "(computed)".to_string(),
                })
                .collect(),
        ),
        _ => None,
    })
}
