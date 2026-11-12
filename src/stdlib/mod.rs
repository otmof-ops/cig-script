// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The standard library: global functions, modules (`fs`, `path`, `json`,
//! `text`, `proc`, `time`, `hash`, `math`, `env`, `csv`, `log`) and the
//! methods available on strings, lists and maps.
//!
//! Every function declares its [`Effect`]; the interpreter refuses
//! world-changing ones outside a `burn` block.

mod chain;
mod cig;
mod core;
mod csv;
mod env;
mod fs;
mod hash;
pub mod json;
mod log;
mod math;
mod methods;
mod pass;
mod path;
mod proc;
mod text;
mod time;

use crate::value::{Builtin, Effect, Module, Value};
use indexmap::IndexMap;
use std::rc::Rc;

/// Shorthand for a builtin table entry.
pub(crate) const fn b(
    name: &'static str,
    effect: Effect,
    min_args: usize,
    max_args: Option<usize>,
    func: crate::value::BuiltinFn,
) -> Builtin {
    Builtin {
        name,
        effect,
        min_args,
        max_args,
        func,
    }
}

pub(crate) fn module(
    name: &'static str,
    table: &'static [Builtin],
    consts: &[(&'static str, Value)],
) -> Value {
    let mut items: IndexMap<&'static str, Value> = IndexMap::new();
    for entry in table {
        // Table names are qualified (`fs.read_text`); the item key is the tail.
        let key = entry.name.rsplit('.').next().unwrap_or(entry.name);
        items.insert(key, Value::Builtin(Rc::new(*entry)));
    }
    for (k, v) in consts {
        items.insert(k, v.clone());
    }
    Value::Module(Rc::new(Module { name, items }))
}

/// Everything visible in every script.
pub fn prelude() -> Vec<(&'static str, Value)> {
    let mut out: Vec<(&'static str, Value)> = core::GLOBALS
        .iter()
        .map(|entry| (entry.name, Value::Builtin(Rc::new(*entry))))
        .collect();
    out.push(("fs", module("fs", fs::TABLE, &[])));
    out.push(("path", module("path", path::TABLE, &[])));
    out.push(("json", module("json", json::TABLE, &[])));
    out.push(("text", module("text", text::TABLE, &[])));
    out.push(("proc", module("proc", proc::TABLE, &[])));
    out.push(("time", module("time", time::TABLE, &[])));
    out.push(("hash", module("hash", hash::TABLE, &[])));
    out.push(("cig", module("cig", cig::TABLE, &[])));
    out.push(("pass", module("pass", pass::TABLE, &[])));
    out.push((
        "math",
        module(
            "math",
            math::TABLE,
            &[
                ("pi", Value::Float(std::f64::consts::PI)),
                ("e", Value::Float(std::f64::consts::E)),
            ],
        ),
    ));
    out.push(("env", module("env", env::TABLE, &[])));
    out.push(("csv", module("csv", csv::TABLE, &[])));
    out.push(("log", module("log", log::TABLE, &[])));
    out
}

/// The builtin catalogue a bytecode `callb` indexes (cig-compile#31): the
/// global functions, then each module's functions in the order the
/// prelude declares the modules. An index is valid for the cig it was
/// compiled against; the VM refuses a module naming an index past the
/// end (E1050) when it loads it.
pub fn builtin_catalogue() -> &'static [&'static Builtin] {
    static CATALOGUE: std::sync::OnceLock<Vec<&'static Builtin>> = std::sync::OnceLock::new();
    CATALOGUE.get_or_init(|| {
        let tables: [&'static [Builtin]; 14] = [
            core::GLOBALS,
            fs::TABLE,
            path::TABLE,
            json::TABLE,
            text::TABLE,
            proc::TABLE,
            time::TABLE,
            hash::TABLE,
            cig::TABLE,
            pass::TABLE,
            math::TABLE,
            env::TABLE,
            csv::TABLE,
            log::TABLE,
        ];
        tables.iter().flat_map(|t| t.iter()).collect()
    })
}

/// Label for an ad-hoc chain step, for the interpreter.
pub fn chain_label(v: &Value, index: usize) -> String {
    chain::label_for(v, index)
}

/// Method lookup by receiver type.
pub fn method(recv: &Value, name: &str) -> Option<&'static Builtin> {
    let table: &[Builtin] = match recv {
        Value::Str(_) => methods::STR,
        Value::List(_) => methods::LIST,
        Value::Map(_) => methods::MAP,
        _ => return None,
    };
    table
        .iter()
        .find(|m| m.name.rsplit('.').next() == Some(name))
}

pub fn method_names(recv: &Value) -> impl Iterator<Item = &'static str> {
    let table: &'static [Builtin] = match recv {
        Value::Str(_) => methods::STR,
        Value::List(_) => methods::LIST,
        Value::Map(_) => methods::MAP,
        _ => &[],
    };
    table
        .iter()
        .map(|m| m.name.rsplit('.').next().unwrap_or(m.name))
}

/// One line of the library catalogue: name, effect, min and max arity.
pub type CatalogueRow = (&'static str, Effect, usize, Option<usize>);

/// The complete surface, for `cig language` and the docs.
pub fn catalogue() -> Vec<(&'static str, Vec<CatalogueRow>)> {
    fn rows(t: &'static [Builtin]) -> Vec<CatalogueRow> {
        t.iter()
            .map(|b| (b.name, b.effect, b.min_args, b.max_args))
            .collect()
    }
    vec![
        ("globals", rows(core::GLOBALS)),
        ("fs", rows(fs::TABLE)),
        ("path", rows(path::TABLE)),
        ("json", rows(json::TABLE)),
        ("text", rows(text::TABLE)),
        ("proc", rows(proc::TABLE)),
        ("time", rows(time::TABLE)),
        ("hash", rows(hash::TABLE)),
        ("cig", rows(cig::TABLE)),
        ("math", rows(math::TABLE)),
        ("env", rows(env::TABLE)),
        ("csv", rows(csv::TABLE)),
        ("log", rows(log::TABLE)),
        ("string methods", rows(methods::STR)),
        ("list methods", rows(methods::LIST)),
        ("map methods", rows(methods::MAP)),
    ]
}
