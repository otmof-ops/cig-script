// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
//! The text form of CIR (`cigcompile build --emit=cir`): for reading and for
//! golden tests. Every instruction prints its span, so a golden diff shows
//! a span that moved. Instruction ids are not printed.

use super::ir::*;
use crate::syntax::ast::{AssignOp, BinOp, BurnClass, UnOp};
use crate::syntax::span::Span;
use std::fmt::Write as _;

pub fn print(m: &Module) -> String {
    let mut out = format!("cir 1 {:?}\n", m.file);
    // Functions after main, then main, as they read best: callee first.
    let mut order: Vec<&Func> = m.funcs.iter().skip(1).collect();
    order.push(&m.funcs[0]);
    for f in order {
        func(m, f, &mut out);
    }
    out
}

fn var(m: &Module, v: VarId) -> String {
    let info = &m.vars[v.0 as usize];
    format!(
        "${}:{}{}",
        v.0,
        info.name,
        if info.mutable { "!" } else { "" }
    )
}

fn func(m: &Module, f: &Func, out: &mut String) {
    let params: Vec<String> = f.params.iter().map(|p| var(m, *p)).collect();
    let mut attrs = vec![match f.kind {
        FuncKind::Main => "main".to_string(),
        FuncKind::Pull => "pull".to_string(),
        FuncKind::Lambda => "lambda".to_string(),
    }];
    if f.requires_burn {
        attrs.push("requires_burn".to_string());
    }
    if !f.captures.is_empty() {
        let c: Vec<String> = f.captures.iter().map(|v| var(m, *v)).collect();
        attrs.push(format!("captures({})", c.join(", ")));
    }
    let head = format!(
        "func @{}{}({}) {} {{",
        f.id.0,
        f.name.as_ref().map(|n| format!(" {n}")).unwrap_or_default(),
        params.join(", "),
        attrs.join(" ")
    );
    let _ = writeln!(out, "{head:<60} @{}:{}", f.span.line, f.span.col);
    block(m, &f.body, 1, out);
    out.push_str("}\n");
}

fn vs(v: &[ValueId]) -> String {
    v.iter()
        .map(|x| format!("%{}", x.0))
        .collect::<Vec<_>>()
        .join(", ")
}

fn konst(c: &Const) -> String {
    match c {
        Const::Null => "null".to_string(),
        Const::Bool(b) => b.to_string(),
        Const::Int(i) => i.to_string(),
        Const::Float(f) => format!("{f:?}"),
        Const::Str(s) => format!("{s:?}"),
    }
}

fn line(out: &mut String, depth: usize, text: &str, i: &Instr) {
    line_at(out, depth, text, i, &[]);
}

/// A line whose span column also carries the operands' own spans, by
/// name (`idx@3:10`): the positions an error is raised at.
fn line_at(out: &mut String, depth: usize, text: &str, i: &Instr, parts: &[(&str, Span)]) {
    let pad = "  ".repeat(depth);
    let mut at = format!("@{}:{}", i.span.line, i.span.col);
    for (name, s) in parts {
        let _ = write!(at, " {name}@{}:{}", s.line, s.col);
    }
    let _ = writeln!(out, "{:<60} {at}", format!("{pad}{text}"));
}

fn close(out: &mut String, depth: usize, text: &str) {
    let _ = writeln!(out, "{}{text}", "  ".repeat(depth));
}

fn block(m: &Module, b: &Block, depth: usize, out: &mut String) {
    for i in &b.instrs {
        let res = i.result.map(|r| format!("%{} = ", r.0)).unwrap_or_default();
        let simple = |text: String| format!("{res}{text}");
        match &i.kind {
            Kind::Step(StepSite::Stmt) => line(out, depth, "step", i),
            Kind::Step(StepSite::Iter) => line(out, depth, "step.iter", i),
            Kind::Const(c) => line(out, depth, &simple(format!("const {}", konst(c))), i),
            Kind::Prelude(n) => line(out, depth, &simple(format!("prelude {n}")), i),
            Kind::Load(v) => line(out, depth, &simple(format!("load {}", var(m, *v))), i),
            Kind::Decl(v, x) => line(out, depth, &format!("decl {} %{}", var(m, *v), x.0), i),
            Kind::Store(v, x) => line(out, depth, &format!("store {} %{}", var(m, *v), x.0), i),
            Kind::Unary(op, x) => {
                let o = match op {
                    UnOp::Neg => "neg",
                    UnOp::Not => "not",
                };
                line(out, depth, &simple(format!("un {o} %{}", x.0)), i)
            }
            Kind::Binary(op, a, b) => line(
                out,
                depth,
                &simple(format!("bin {} %{}, %{}", bin(*op), a.0, b.0)),
                i,
            ),
            Kind::Short { op, subject, rhs } => {
                let o = match op {
                    ShortOp::And => "and",
                    ShortOp::Or => "or",
                    ShortOp::Coalesce => "coalesce",
                    ShortOp::Safe => "safe",
                };
                line(
                    out,
                    depth,
                    &simple(format!("short.{o} %{} {{", subject.0)),
                    i,
                );
                block(m, rhs, depth + 1, out);
                close(out, depth, "}");
            }
            Kind::MakeList(v) => line(out, depth, &simple(format!("list [{}]", vs(v))), i),
            Kind::MakeMap(kv) => {
                let parts: Vec<String> = kv
                    .iter()
                    .map(|(k, v)| match k {
                        MapKey::Static(s) => format!("{s:?}: %{}", v.0),
                        MapKey::Dyn(d, _) => format!("[%{}]: %{}", d.0, v.0),
                    })
                    .collect();
                let keys: Vec<(&str, Span)> = kv
                    .iter()
                    .filter_map(|(k, _)| match k {
                        MapKey::Dyn(_, at) => Some(("key", *at)),
                        MapKey::Static(_) => None,
                    })
                    .collect();
                line_at(
                    out,
                    depth,
                    &simple(format!("map {{{}}}", parts.join(", "))),
                    i,
                    &keys,
                )
            }
            Kind::Key(k) => line(out, depth, &simple(format!("key %{}", k.0)), i),
            Kind::Interp(p) => {
                let parts: Vec<String> = p
                    .iter()
                    .map(|p| match p {
                        Piece::Lit(s) => format!("{s:?}"),
                        Piece::Val(v) => format!("%{}", v.0),
                    })
                    .collect();
                line(
                    out,
                    depth,
                    &simple(format!("interp {}", parts.join(" "))),
                    i,
                )
            }
            Kind::Member { obj, name, safe } => line(
                out,
                depth,
                &simple(format!(
                    "member %{}{}{name}",
                    obj.0,
                    if *safe { "?." } else { "." }
                )),
                i,
            ),
            Kind::Index { obj, idx, idx_at } => line_at(
                out,
                depth,
                &simple(format!("index %{}[%{}]", obj.0, idx.0)),
                i,
                &[("idx", *idx_at)],
            ),
            Kind::SetIndex {
                obj,
                idx,
                op,
                val,
                obj_at,
                idx_at,
            } => line_at(
                out,
                depth,
                &format!(
                    "set.index %{}[%{}] {} %{}",
                    obj.0,
                    idx.0,
                    assign(*op),
                    val.0
                ),
                i,
                &[("obj", *obj_at), ("idx", *idx_at)],
            ),
            Kind::SetMember {
                obj,
                name,
                op,
                val,
                obj_at,
            } => line_at(
                out,
                depth,
                &format!("set.member %{}.{name} {} %{}", obj.0, assign(*op), val.0),
                i,
                &[("obj", *obj_at)],
            ),
            Kind::Closure(f) => line(out, depth, &simple(format!("closure @{}", f.0)), i),
            Kind::BurnOp {
                kind, callee, args, ..
            } => line(
                out,
                depth,
                &simple(format!("burn.op {} {callee}({})", kind.name(), vs(args))),
                i,
            ),
            Kind::CallBuiltin { callee, args } => line(
                out,
                depth,
                &simple(format!("call.builtin {callee}({})", vs(args))),
                i,
            ),
            Kind::CallUser { func, args } => line(
                out,
                depth,
                &simple(format!("call.user @{}({})", func.0, vs(args))),
                i,
            ),
            Kind::CallValue { f, args } => line(
                out,
                depth,
                &simple(format!("call.value %{}({})", f.0, vs(args))),
                i,
            ),
            Kind::CallMethod { recv, name, args } => line(
                out,
                depth,
                &simple(format!("call.method %{}.{name}({})", recv.0, vs(args))),
                i,
            ),
            Kind::MakeChain { name, steps } => {
                let s: Vec<String> = steps
                    .iter()
                    .map(|(h, v, _)| match h {
                        Some(h) => format!("{h}=%{}", v.0),
                        None => format!("%{}", v.0),
                    })
                    .collect();
                line(
                    out,
                    depth,
                    &format!("chain {} [{}]", var(m, *name), s.join(", ")),
                    i,
                )
            }
            Kind::PackDecl(v) => line(out, depth, &format!("pack [{}]", vs(v)), i),
            Kind::Exhale(v) => line(out, depth, &format!("exhale {}", vs(v)), i),
            Kind::Burn(b) => {
                let class = match b.class {
                    BurnClass::Normal => "normal",
                    BurnClass::Unlit => "unlit",
                };
                let mut head = format!("burn.begin r{} {class}", b.region.0);
                if let Some(s) = b.state {
                    head += &format!(" state={}", var(m, s));
                }
                if let Some(l) = b.label {
                    head += &format!(" label=%{}", l.0);
                }
                line(out, depth, &format!("{head} {{"), i);
                block(m, &b.body, depth + 1, out);
                if let Some(c) = &b.compensate {
                    close(out, depth, "} burn.compensate {");
                    block(m, c, depth + 1, out);
                }
                close(out, depth, "} burn.end");
            }
            Kind::Try {
                body,
                catch,
                finally,
            } => {
                line(out, depth, "try {", i);
                block(m, body, depth + 1, out);
                if let Some((v, c)) = catch {
                    close(
                        out,
                        depth,
                        &format!(
                            "}} catch{} {{",
                            v.map(|v| format!(" {}", var(m, v))).unwrap_or_default()
                        ),
                    );
                    block(m, c, depth + 1, out);
                }
                if let Some(f) = finally {
                    close(out, depth, "} finally {");
                    block(m, f, depth + 1, out);
                }
                close(out, depth, "}");
            }
            Kind::If {
                cond,
                then,
                otherwise,
            } => {
                line(out, depth, &format!("if %{} {{", cond.0), i);
                block(m, then, depth + 1, out);
                if let Some(o) = otherwise {
                    close(out, depth, "} else {");
                    block(m, o, depth + 1, out);
                }
                close(out, depth, "}");
            }
            Kind::Loop { cond, body } => {
                line(out, depth, "loop.cond {", i);
                block(m, cond, depth + 1, out);
                close(out, depth, "} loop.body {");
                block(m, body, depth + 1, out);
                close(out, depth, "}");
            }
            Kind::ForEach { iter, var: v, body } => {
                line(
                    out,
                    depth,
                    &format!("foreach {} in %{} {{", var(m, *v), iter.0),
                    i,
                );
                block(m, body, depth + 1, out);
                close(out, depth, "}");
            }
            Kind::Yield(v) => line(out, depth, &format!("yield %{}", v.0), i),
            Kind::Ret(v) => line(out, depth, &format!("ret %{}", v.0), i),
            Kind::Break => line(out, depth, "break", i),
            Kind::Continue => line(out, depth, "continue", i),
            Kind::Throw(v) => line(out, depth, &format!("throw %{}", v.0), i),
        }
    }
}

fn bin(op: BinOp) -> &'static str {
    op.symbol()
}

fn assign(op: AssignOp) -> &'static str {
    match op {
        AssignOp::Set => "=",
        AssignOp::Add => "+=",
        AssignOp::Sub => "-=",
        AssignOp::Mul => "*=",
        AssignOp::Div => "/=",
    }
}
