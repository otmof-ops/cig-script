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
        calls_back: false,
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

/// Method lookup by receiver type: the name matched to its row, no
/// table scanned.
#[inline]
pub fn method(recv: &Value, name: &str) -> Option<&'static Builtin> {
    let (table, row): (&'static [Builtin], usize) = match recv {
        Value::Str(_) => (methods::STR, methods::str_row(name)?),
        Value::List(_) => (methods::LIST, methods::list_row(name)?),
        Value::Map(_) => (methods::MAP, methods::map_row(name)?),
        _ => return None,
    };
    table.get(row)
}

/// The receiver types that have methods, as a call site keeps them.
const SLOT_STR: u32 = 1;
const SLOT_LIST: u32 = 2;
const SLOT_MAP: u32 = 3;

fn slot_kind(recv: &Value) -> Option<(u32, &'static [Builtin])> {
    match recv {
        Value::Str(_) => Some((SLOT_STR, methods::STR)),
        Value::List(_) => Some((SLOT_LIST, methods::LIST)),
        Value::Map(_) => Some((SLOT_MAP, methods::MAP)),
        _ => None,
    }
}

/// Where [`method`] finds `name` on `recv`, as one number a call site can
/// keep (the receiver's type and the method's row, never 0); `None` when
/// it finds nothing. Never inlined: it runs on a `callm`'s general path,
/// whose frame stays small.
#[inline(never)]
pub fn method_slot(recv: &Value, name: &str) -> Option<u32> {
    let (kind, _) = slot_kind(recv)?;
    let row = match kind {
        SLOT_STR => methods::str_row(name)?,
        SLOT_LIST => methods::list_row(name)?,
        _ => methods::map_row(name)?,
    };
    Some((kind << 16) | u32::try_from(row).ok()?)
}

/// The method a slot from [`method_slot`] names, when `recv` is of the
/// type it was taken for; `None` otherwise (and for 0, no slot).
#[inline]
pub fn method_at(recv: &Value, slot: u32) -> Option<&'static Builtin> {
    let (kind, table) = slot_kind(recv)?;
    if slot >> 16 != kind {
        return None;
    }
    table.get((slot & 0xFFFF) as usize)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::burn::{Kernel, Mode};
    use crate::interp::Interp;
    use crate::syntax::span::Span;

    fn receivers() -> [(Value, &'static [Builtin]); 3] {
        [
            (Value::str(""), methods::STR),
            (Value::list(Vec::new()), methods::LIST),
            (Value::map(IndexMap::new()), methods::MAP),
        ]
    }

    #[test]
    fn every_method_row_is_found_by_its_name() {
        for (recv, table) in receivers() {
            for row in table {
                let tail = row.name.rsplit('.').next().unwrap_or(row.name);
                let found = method(&recv, tail).unwrap_or_else(|| panic!("{} not found", row.name));
                assert!(
                    std::ptr::eq(found, row),
                    "{} found {}",
                    row.name,
                    found.name
                );
                let slot = method_slot(&recv, tail).expect("a slot");
                assert_ne!(slot, 0);
                assert!(std::ptr::eq(method_at(&recv, slot).expect("its row"), row));
                // A slot taken for one type names nothing on another.
                for (other, _) in receivers() {
                    if other.type_name() != recv.type_name() {
                        assert!(method_at(&other, slot).is_none(), "{}", row.name);
                    }
                }
            }
            assert_eq!(method_names(&recv).count(), table.len());
        }
        assert!(method(&Value::str(""), "push").is_none());
        assert!(method(&Value::Int(1), "len").is_none());
        assert!(method_slot(&Value::Null, "len").is_none());
        assert!(method_at(&Value::str(""), 0).is_none());
    }

    #[test]
    fn an_unknown_method_is_e404_with_its_hint() {
        let mut i = Interp::new(Kernel::ephemeral(Mode::Run));
        let e = i
            .call_method(&[Value::str("abc")], "uper", Span::default())
            .err()
            .expect("E404");
        assert_eq!(e.code, "E404");
        assert_eq!(e.message, "string has no method `uper`");
        assert_eq!(e.hint.as_deref(), Some("did you mean `.upper()`?"));
        let e = i
            .call_method(&[Value::list(Vec::new())], "push", Span::default())
            .err()
            .expect("arity");
        assert_eq!(e.message, ".push() expects at least 2 arguments, got 1");
    }

    #[test]
    fn exactly_the_builtins_that_call_a_script_function_are_marked() {
        let mut marked: Vec<&str> = builtin_catalogue()
            .iter()
            .chain(
                methods::STR
                    .iter()
                    .chain(methods::LIST)
                    .chain(methods::MAP)
                    .collect::<Vec<_>>()
                    .iter(),
            )
            .filter(|b| b.calls_back)
            .map(|b| b.name)
            .collect();
        marked.sort_unstable();
        assert_eq!(
            marked,
            [
                "light",
                "list.all",
                "list.any",
                "list.each",
                "list.filter",
                "list.find",
                "list.map",
                "list.reduce",
                "list.sort_by",
                "pass.issue",
                "pass.memo",
            ]
        );
    }

    #[test]
    fn join_writes_each_item_as_display_does() {
        let mut i = Interp::new(Kernel::ephemeral(Mode::Run));
        let items = Value::list(vec![
            Value::str("a"),
            Value::Int(-12),
            Value::Float(2.0),
            Value::Null,
            Value::list(vec![Value::str("q")]),
        ]);
        let join = method(&items, "join").expect("join");
        let with = (join.func)(&mut i, &[items.clone(), Value::str(", ")], Span::default());
        assert_eq!(
            with.ok().map(|v| v.display()).as_deref(),
            Some("a, -12, 2.0, null, [\"q\"]")
        );
        let bare = (join.func)(&mut i, &[items], Span::default());
        assert_eq!(
            bare.ok().map(|v| v.display()).as_deref(),
            Some("a-122.0null[\"q\"]")
        );
    }
}
