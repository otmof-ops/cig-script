// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Regular expressions and text templating.

use super::b;
use crate::diagnostics::{runtime, Diagnostic};
use crate::interp::Interp;
use crate::syntax::span::Span;
use crate::value::{expect_map, expect_str, Builtin, Effect, Value};
use regex::Regex;
use std::cell::RefCell;
use std::collections::HashMap;

pub static TABLE: &[Builtin] = &[
    b("text.matches", Effect::Pure, 2, Some(2), matches),
    b("text.find", Effect::Pure, 2, Some(2), find),
    b("text.find_all", Effect::Pure, 2, Some(2), find_all),
    b("text.captures", Effect::Pure, 2, Some(2), captures),
    b("text.replace", Effect::Pure, 3, Some(3), replace),
    b("text.split", Effect::Pure, 2, Some(2), split),
    b("text.template", Effect::Pure, 2, Some(2), template),
];

thread_local! {
    static CACHE: RefCell<HashMap<String, Regex>> = RefCell::new(HashMap::new());
}

fn regex(pattern: &str, s: Span) -> Result<Regex, Diagnostic> {
    CACHE.with(|c| {
        if let Some(r) = c.borrow().get(pattern) {
            return Ok(r.clone());
        }
        let r = Regex::new(pattern).map_err(|e| {
            runtime(format!("invalid regex `{pattern}`: {e}"))
                .code("E510")
                .at(s)
        })?;
        c.borrow_mut().insert(pattern.to_string(), r.clone());
        Ok(r)
    })
}

fn matches(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let text = expect_str(a, 0, "text.matches", s)?;
    let re = regex(expect_str(a, 1, "text.matches", s)?, s)?;
    Ok(Value::Bool(re.is_match(text)))
}

fn find(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let text = expect_str(a, 0, "text.find", s)?;
    let re = regex(expect_str(a, 1, "text.find", s)?, s)?;
    Ok(re
        .find(text)
        .map(|m| Value::str(m.as_str()))
        .unwrap_or(Value::Null))
}

fn find_all(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let text = expect_str(a, 0, "text.find_all", s)?;
    let re = regex(expect_str(a, 1, "text.find_all", s)?, s)?;
    Ok(Value::list(
        re.find_iter(text).map(|m| Value::str(m.as_str())).collect(),
    ))
}

/// Capture groups of the first match: `[whole, group1, group2, ...]`, with
/// named groups also available in a trailing map when the pattern has any.
fn captures(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let text = expect_str(a, 0, "text.captures", s)?;
    let re = regex(expect_str(a, 1, "text.captures", s)?, s)?;
    let Some(caps) = re.captures(text) else {
        return Ok(Value::Null);
    };
    let groups: Vec<Value> = (0..caps.len())
        .map(|i| {
            caps.get(i)
                .map(|m| Value::str(m.as_str()))
                .unwrap_or(Value::Null)
        })
        .collect();
    Ok(Value::list(groups))
}

fn replace(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let text = expect_str(a, 0, "text.replace", s)?;
    let re = regex(expect_str(a, 1, "text.replace", s)?, s)?;
    let repl = expect_str(a, 2, "text.replace", s)?;
    Ok(Value::str(re.replace_all(text, repl)))
}

fn split(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let text = expect_str(a, 0, "text.split", s)?;
    let re = regex(expect_str(a, 1, "text.split", s)?, s)?;
    Ok(Value::list(re.split(text).map(Value::str).collect()))
}

/// `{{name}}` substitution from a map; unknown names are an error so typos
/// never ship silently.
fn template(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let text = expect_str(a, 0, "text.template", s)?;
    let vars = expect_map(a, 1, "text.template", s)?;
    let re = regex(r"\{\{\s*([A-Za-z_][A-Za-z0-9_.]*)\s*\}\}", s)?;
    let mut missing = None;
    let out = re.replace_all(text, |caps: &regex::Captures| {
        let key = &caps[1];
        match vars.borrow().get(key) {
            Some(v) => v.display(),
            None => {
                missing.get_or_insert_with(|| key.to_string());
                String::new()
            }
        }
    });
    if let Some(key) = missing {
        return Err(runtime(format!("text.template: no value for `{{{{{key}}}}}`")).at(s));
    }
    Ok(Value::str(out))
}
