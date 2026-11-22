// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
//! `fold-consts`: `2 * 3` becomes `6`.

use super::{snapshot, walk, Pass, Remark};
use crate::lower::cir::ir::*;
use crate::syntax::span::Span;
use std::collections::HashMap;

/// Folds a unary or binary operator over constants, only where the
/// interpreter cannot fail (`fold_binary`, `fold_unary`): integer `+ - *`
/// that do not overflow, comparisons of like constants, string `+`, `not`.
/// The folded instruction keeps its id and its span.
///
/// Must not: fold an operation that raises (an overflow stays a runtime
/// E503 at its span; `/`, `%`, `..` and mixed numbers are never folded).
/// T4 checks every fold against `fold_binary` over the same operands.
pub struct FoldConsts;

impl Pass for FoldConsts {
    fn name(&self) -> &'static str {
        "fold-consts"
    }

    fn run(&self, m: &mut Module) {
        for f in &mut m.funcs {
            let mut known: HashMap<ValueId, Const> = HashMap::new();
            fold_block(&mut f.body, &mut known, &mut f.vals);
        }
    }

    fn run_remarking(&self, m: &mut Module, remarks: &mut Vec<Remark>) {
        let before = snapshot(m);
        self.run(m);
        let mut folded = 0usize;
        let mut details: Vec<Remark> = Vec::new();
        for (f, after) in walk(m) {
            if let Some(b) = before.get(&after.id) {
                if b.kind_tag != "const" && after.kind_tag == "const" {
                    folded += 1;
                    let what = b.kind_tag;
                    let val = after
                        .const_text
                        .clone()
                        .unwrap_or_else(|| "a constant".to_string());
                    details.push(Remark {
                        pass: self.name(),
                        message: format!("fold-consts: folded {what} in {f} to const {val}"),
                        span: after.span,
                    });
                }
            }
        }
        remarks.push(Remark {
            pass: self.name(),
            message: match folded {
                0 => "fold-consts: nothing to fold".to_string(),
                1 => "fold-consts: folded 1 constant".to_string(),
                n => format!("fold-consts: folded {n} constants"),
            },
            span: Span::default(),
        });
        remarks.extend(details);
    }
}

fn fold_block(b: &mut Block, known: &mut HashMap<ValueId, Const>, vals: &mut [ValueInfo]) {
    for i in &mut b.instrs {
        for blk in i.kind.blocks_mut() {
            fold_block(blk, known, vals);
        }
        let folded = match &i.kind {
            Kind::Const(c) => {
                if let Some(r) = i.result {
                    known.insert(r, c.clone());
                }
                None
            }
            Kind::Unary(op, x) => known.get(x).and_then(|x| fold_unary(*op, x)),
            Kind::Binary(op, a, b) => match (known.get(a), known.get(b)) {
                (Some(x), Some(y)) => fold_binary(*op, x, y),
                _ => None,
            },
            _ => None,
        };
        if let (Some(c), Some(r)) = (folded, i.result) {
            vals[r.0 as usize].ty = c.ty();
            known.insert(r, c.clone());
            i.kind = Kind::Const(c);
        }
    }
}
