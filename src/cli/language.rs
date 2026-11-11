// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! `cig language`: the legend and the library surface, generated from the
//! same tables the interpreter uses so it can never drift.

use super::{exit, Ctx};
use crate::value::Effect;

pub const LEGEND: &[(&str, &str)] = &[
    ("roll x = e", "declare a variable"),
    ("stick x = e", "declare a constant (a stick never changes)"),
    ("pull name(a, b) { }", "define a function"),
    ("pack(a) => e  /  pack(a) { }", "anonymous function"),
    ("snuff e", "return from a function"),
    ("exhale a, b", "print to stdout"),
    (
        "cough e",
        "raise an error (a string or a map with a message)",
    ),
    (
        "try { } ashtray err { } finally { }",
        "catch an error; err has message, kind, line",
    ),
    ("burn { }", "the only place side effects are allowed"),
    (
        "burn (s) { } unburn { }",
        "a burn with its undo: the compensation runs on rollback with the state map s",
    ),
    (
        "pack { \"./build\" }",
        "first statement: the scope every write must stay inside",
    ),
    (
        "burn unlit { }",
        "declare side effects without ever running them",
    ),
    (
        "chain name { a, b }",
        "an ordered list of sticks to light, in order",
    ),
    (
        "light(chain, opts?)",
        "run a chain; stops at the first failure, returns a report",
    ),
    ("if / else if / else", "conditionals"),
    (
        "while c { }  /  for x in xs { }",
        "loops; break and continue work",
    ),
    ("a >> f(b)", "pipeline: calls f(a, b)"),
    ("m?.key  /  a ?? b", "null-safe access and null coalescing"),
    (
        "\"n = ${n}\"",
        "string interpolation; 'single quotes' are raw",
    ),
    ("# comment", "comments"),
];

pub fn language(ctx: &Ctx) -> i32 {
    let catalogue = crate::stdlib::catalogue();
    if ctx.json {
        let obj = serde_json::json!({
            "version": crate::VERSION,
            "legend": LEGEND.iter().map(|(s, m)| serde_json::json!({"syntax": s, "meaning": m})).collect::<Vec<_>>(),
            "library": catalogue.iter().map(|(group, rows)| serde_json::json!({
                "group": group,
                "functions": rows.iter().map(|(name, effect, min, max)| serde_json::json!({
                    "name": name,
                    "effect": effect_name(*effect),
                    "needs_burn": effect.needs_burn(),
                    "min_args": min,
                    "max_args": max,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        });
        outln!("{}", serde_json::to_string_pretty(&obj).unwrap_or_default());
        return exit::OK;
    }
    outln!(
        "{}",
        ctx.bold(&format!(
            "CigScript {} — nothing real happens unless you burn",
            crate::VERSION
        ))
    );
    outln!();
    for (syntax, meaning) in LEGEND {
        outln!("  {:<34} {}", syntax, ctx.dim(meaning));
    }
    outln!();
    outln!("{}", ctx.bold("standard library"));
    outln!(
        "  {}",
        ctx.dim("functions marked `burn` change the world and must be inside a burn block")
    );
    for (group, rows) in &catalogue {
        outln!();
        outln!("  {}", ctx.bold(group));
        for (name, effect, min, max) in rows {
            let arity = match max {
                Some(m) if m == min => format!("{min}"),
                Some(m) => format!("{min}-{m}"),
                None => format!("{min}+"),
            };
            let tag = match effect {
                Effect::Pure => ctx.dim("pure"),
                Effect::Read => ctx.dim("read"),
                _ => ctx.yellow("burn"),
            };
            outln!("    {:<24} {:<5} {}", name, arity, tag);
        }
    }
    exit::OK
}

fn effect_name(e: Effect) -> &'static str {
    match e {
        Effect::Pure => "pure",
        Effect::Read => "read",
        Effect::Write => "write",
        Effect::Proc => "proc",
        Effect::Env => "env",
    }
}
