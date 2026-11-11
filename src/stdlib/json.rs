// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! JSON in and out.

use super::b;
use crate::burn::{Decision, Op};
use crate::diagnostics::{runtime, type_error, Diagnostic};
use crate::interp::Interp;
use crate::syntax::span::Span;
use crate::value::{expect_str, Builtin, Effect, Value};
use indexmap::IndexMap;
use std::path::PathBuf;

pub static TABLE: &[Builtin] = &[
    b("json.parse", Effect::Pure, 1, Some(1), parse),
    b("json.stringify", Effect::Pure, 1, Some(2), stringify),
    b("json.load", Effect::Read, 1, Some(1), load),
    b("json.save", Effect::Write, 2, Some(3), save),
];

pub fn to_value(j: serde_json::Value) -> Value {
    match j {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else {
                Value::Float(n.as_f64().unwrap_or(f64::NAN))
            }
        }
        serde_json::Value::String(s) => Value::str(s),
        serde_json::Value::Array(items) => Value::list(items.into_iter().map(to_value).collect()),
        serde_json::Value::Object(map) => {
            let mut out = IndexMap::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k, to_value(v));
            }
            Value::map(out)
        }
    }
}

pub fn from_value(v: &Value, span: Span) -> Result<serde_json::Value, Diagnostic> {
    from_value_in(v, span, &mut Vec::new())
}

/// `seen` holds the lists and maps being encoded further up the walk: a
/// value that contains itself has no JSON, and says so instead of
/// overflowing the stack.
fn from_value_in(
    v: &Value,
    span: Span,
    seen: &mut Vec<usize>,
) -> Result<serde_json::Value, Diagnostic> {
    let contains_itself = || {
        runtime(
            "json: the value contains itself; a list or map that holds itself cannot be encoded",
        )
        .at(span)
        .with_hint("encode the parts, or replace the inner reference with an id")
    };
    Ok(match v {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int(i) => serde_json::Value::from(*i),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .ok_or_else(|| runtime("json: cannot encode a non-finite float").at(span))?,
        Value::Str(s) => serde_json::Value::String(s.to_string()),
        Value::List(l) => {
            let key = std::rc::Rc::as_ptr(l) as *const () as usize;
            if seen.contains(&key) {
                return Err(contains_itself());
            }
            seen.push(key);
            let mut out = Vec::with_capacity(l.borrow().len());
            for item in l.borrow().iter() {
                out.push(from_value_in(item, span, seen)?);
            }
            seen.pop();
            serde_json::Value::Array(out)
        }
        Value::Map(m) => {
            let key = std::rc::Rc::as_ptr(m) as *const () as usize;
            if seen.contains(&key) {
                return Err(contains_itself());
            }
            seen.push(key);
            let mut out = serde_json::Map::with_capacity(m.borrow().len());
            for (k, v) in m.borrow().iter() {
                out.insert(k.clone(), from_value_in(v, span, seen)?);
            }
            seen.pop();
            serde_json::Value::Object(out)
        }
        other => {
            return Err(type_error(format!("json: cannot encode a {}", other.type_name())).at(span))
        }
    })
}

fn parse(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let text = expect_str(a, 0, "json.parse", s)?;
    serde_json::from_str::<serde_json::Value>(text)
        .map(to_value)
        .map_err(|e| runtime(format!("json.parse: {e}")).code("E511").at(s))
}

fn stringify(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let j = from_value(&a[0], s)?;
    let pretty = a.get(1).is_some_and(|p| p.truthy());
    let text = if pretty {
        serde_json::to_string_pretty(&j)
    } else {
        serde_json::to_string(&j)
    }
    .map_err(|e| runtime(format!("json.stringify: {e}")).at(s))?;
    Ok(Value::str(text))
}

fn load(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let p = expect_str(a, 0, "json.load", s)?;
    let text = match super::fs::ghost_text(i, "json.load", std::path::Path::new(p), s)? {
        Some(t) => t,
        None if i.recording() => {
            let b = super::fs::rec_read(i, "json.load", std::path::Path::new(p), s)?;
            String::from_utf8_lossy(&b).into_owned()
        }
        None => {
            super::fs::check_read_size("json.load", std::path::Path::new(p), s)?;
            std::fs::read_to_string(p).map_err(|e| {
                runtime(format!("json.load: {p}: {}", super::fs::describe_io(&e)))
                    .code("E508")
                    .at(s)
                    .with_subject(p.to_string())
            })?
        }
    };
    serde_json::from_str::<serde_json::Value>(&text)
        .map(to_value)
        .map_err(|e| runtime(format!("json.load: {p}: {e}")).code("E511").at(s))
}

fn save(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let p = PathBuf::from(expect_str(a, 0, "json.save", s)?);
    let j = from_value(&a[1], s)?;
    let pretty = a.get(2).is_none_or(|p| p.truthy());
    let mut text = if pretty {
        serde_json::to_string_pretty(&j)
    } else {
        serde_json::to_string(&j)
    }
    .map_err(|e| runtime(format!("json.save: {e}")).at(s))?;
    text.push('\n');
    if i.effect(
        "json.save",
        Op::Write {
            path: p.clone(),
            sensitive: false,
        },
        s,
    )? == Decision::Execute
    {
        if let Some(parent) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| {
                runtime(format!(
                    "json.save: {}: {}",
                    parent.display(),
                    super::fs::describe_io(&e)
                ))
                .at(s)
            })?;
        }
        std::fs::write(&p, &text).map_err(|e| {
            runtime(format!(
                "json.save: {}: {}",
                p.display(),
                super::fs::describe_io(&e)
            ))
            .at(s)
        })?;
    } else {
        i.ghost_put(&p, text.as_bytes(), false);
    }
    Ok(Value::Int(text.len() as i64))
}
