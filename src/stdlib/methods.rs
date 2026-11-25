// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Methods on strings, lists and maps. Argument 0 is always the receiver.

use super::b;
use crate::diagnostics::{runtime, type_error, Diagnostic};
use crate::interp::Interp;
use crate::syntax::span::Span;
use crate::value::{
    expect_func, expect_int, expect_list, expect_map, expect_str, Builtin, Effect, Value,
};
use std::cmp::Ordering;
use std::rc::Rc;

pub static STR: &[Builtin] = &[
    b("string.len", Effect::Pure, 1, Some(1), s_len),
    b("string.upper", Effect::Pure, 1, Some(1), s_upper),
    b("string.lower", Effect::Pure, 1, Some(1), s_lower),
    b("string.trim", Effect::Pure, 1, Some(1), s_trim),
    b("string.trim_start", Effect::Pure, 1, Some(1), s_trim_start),
    b("string.trim_end", Effect::Pure, 1, Some(1), s_trim_end),
    b("string.split", Effect::Pure, 1, Some(2), s_split),
    b("string.lines", Effect::Pure, 1, Some(1), s_lines),
    b("string.chars", Effect::Pure, 1, Some(1), s_chars),
    b("string.contains", Effect::Pure, 2, Some(2), s_contains),
    b(
        "string.starts_with",
        Effect::Pure,
        2,
        Some(2),
        s_starts_with,
    ),
    b("string.ends_with", Effect::Pure, 2, Some(2), s_ends_with),
    b("string.replace", Effect::Pure, 3, Some(3), s_replace),
    b("string.find", Effect::Pure, 2, Some(2), s_find),
    b("string.slice", Effect::Pure, 2, Some(3), s_slice),
    b("string.repeat", Effect::Pure, 2, Some(2), s_repeat),
    b("string.pad_left", Effect::Pure, 2, Some(3), s_pad_left),
    b("string.pad_right", Effect::Pure, 2, Some(3), s_pad_right),
    b("string.reverse", Effect::Pure, 1, Some(1), s_reverse),
    b("string.is_empty", Effect::Pure, 1, Some(1), s_is_empty),
    b("string.to_int", Effect::Pure, 1, Some(1), s_to_int),
    b("string.to_float", Effect::Pure, 1, Some(1), s_to_float),
    b("string.count", Effect::Pure, 2, Some(2), s_count),
];

pub static LIST: &[Builtin] = &[
    b("list.len", Effect::Pure, 1, Some(1), l_len),
    b("list.is_empty", Effect::Pure, 1, Some(1), l_is_empty),
    b("list.push", Effect::Pure, 2, None, l_push),
    b("list.pop", Effect::Pure, 1, Some(1), l_pop),
    b("list.insert", Effect::Pure, 3, Some(3), l_insert),
    b("list.remove", Effect::Pure, 2, Some(2), l_remove),
    b("list.first", Effect::Pure, 1, Some(1), l_first),
    b("list.last", Effect::Pure, 1, Some(1), l_last),
    b("list.get", Effect::Pure, 2, Some(3), l_get),
    b("list.slice", Effect::Pure, 2, Some(3), l_slice),
    b("list.contains", Effect::Pure, 2, Some(2), l_contains),
    b("list.index_of", Effect::Pure, 2, Some(2), l_index_of),
    b("list.map", Effect::Pure, 2, Some(2), l_map).calling_back(),
    b("list.filter", Effect::Pure, 2, Some(2), l_filter).calling_back(),
    b("list.reduce", Effect::Pure, 3, Some(3), l_reduce).calling_back(),
    b("list.each", Effect::Pure, 2, Some(2), l_each).calling_back(),
    b("list.any", Effect::Pure, 2, Some(2), l_any).calling_back(),
    b("list.all", Effect::Pure, 2, Some(2), l_all).calling_back(),
    b("list.find", Effect::Pure, 2, Some(2), l_find).calling_back(),
    b("list.sort", Effect::Pure, 1, Some(1), l_sort),
    b("list.sort_by", Effect::Pure, 2, Some(2), l_sort_by).calling_back(),
    b("list.reverse", Effect::Pure, 1, Some(1), l_reverse),
    b("list.join", Effect::Pure, 1, Some(2), l_join),
    b("list.unique", Effect::Pure, 1, Some(1), l_unique),
    b("list.flatten", Effect::Pure, 1, Some(1), l_flatten),
    b("list.sum", Effect::Pure, 1, Some(1), l_sum),
    b("list.min", Effect::Pure, 1, Some(1), l_min),
    b("list.max", Effect::Pure, 1, Some(1), l_max),
    b("list.zip", Effect::Pure, 2, Some(2), l_zip),
    b("list.enumerate", Effect::Pure, 1, Some(1), l_enumerate),
    b("list.chunks", Effect::Pure, 2, Some(2), l_chunks),
    b("list.copy", Effect::Pure, 1, Some(1), l_copy),
    b("list.repeat", Effect::Pure, 2, Some(2), l_repeat),
    b("list.counts", Effect::Pure, 1, Some(1), l_counts),
    b("list.count", Effect::Pure, 2, Some(2), l_count),
    b("list.extend", Effect::Pure, 2, Some(2), l_extend),
];

pub static MAP: &[Builtin] = &[
    b("map.len", Effect::Pure, 1, Some(1), m_len),
    b("map.is_empty", Effect::Pure, 1, Some(1), m_is_empty),
    b("map.keys", Effect::Pure, 1, Some(1), m_keys),
    b("map.values", Effect::Pure, 1, Some(1), m_values),
    b("map.entries", Effect::Pure, 1, Some(1), m_entries),
    b("map.has", Effect::Pure, 2, Some(2), m_has),
    b("map.get", Effect::Pure, 2, Some(3), m_get),
    b("map.set", Effect::Pure, 3, Some(3), m_set),
    b("map.remove", Effect::Pure, 2, Some(2), m_remove),
    b("map.merge", Effect::Pure, 2, Some(2), m_merge),
    b("map.copy", Effect::Pure, 1, Some(1), m_copy),
    b("map.inc", Effect::Pure, 2, Some(3), m_inc),
];

/// The row of [`STR`] that holds the string method `name`.
pub(super) fn str_row(name: &str) -> Option<usize> {
    Some(match name {
        "len" => 0,
        "upper" => 1,
        "lower" => 2,
        "trim" => 3,
        "trim_start" => 4,
        "trim_end" => 5,
        "split" => 6,
        "lines" => 7,
        "chars" => 8,
        "contains" => 9,
        "starts_with" => 10,
        "ends_with" => 11,
        "replace" => 12,
        "find" => 13,
        "slice" => 14,
        "repeat" => 15,
        "pad_left" => 16,
        "pad_right" => 17,
        "reverse" => 18,
        "is_empty" => 19,
        "to_int" => 20,
        "to_float" => 21,
        "count" => 22,
        _ => return None,
    })
}

/// The row of [`LIST`] that holds the list method `name`.
pub(super) fn list_row(name: &str) -> Option<usize> {
    Some(match name {
        "len" => 0,
        "is_empty" => 1,
        "push" => 2,
        "pop" => 3,
        "insert" => 4,
        "remove" => 5,
        "first" => 6,
        "last" => 7,
        "get" => 8,
        "slice" => 9,
        "contains" => 10,
        "index_of" => 11,
        "map" => 12,
        "filter" => 13,
        "reduce" => 14,
        "each" => 15,
        "any" => 16,
        "all" => 17,
        "find" => 18,
        "sort" => 19,
        "sort_by" => 20,
        "reverse" => 21,
        "join" => 22,
        "unique" => 23,
        "flatten" => 24,
        "sum" => 25,
        "min" => 26,
        "max" => 27,
        "zip" => 28,
        "enumerate" => 29,
        "chunks" => 30,
        "copy" => 31,
        "repeat" => 32,
        "counts" => 33,
        "count" => 34,
        "extend" => 35,
        _ => return None,
    })
}

/// The row of [`MAP`] that holds the map method `name`.
pub(super) fn map_row(name: &str) -> Option<usize> {
    Some(match name {
        "len" => 0,
        "is_empty" => 1,
        "keys" => 2,
        "values" => 3,
        "entries" => 4,
        "has" => 5,
        "get" => 6,
        "set" => 7,
        "remove" => 8,
        "merge" => 9,
        "copy" => 10,
        "inc" => 11,
        _ => return None,
    })
}

// ----- strings -----------------------------------------------------------------

fn s_len(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    Ok(Value::Int(
        expect_str(a, 0, "len", s)?.chars().count() as i64
    ))
}
fn s_upper(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    Ok(Value::str(expect_str(a, 0, "upper", s)?.to_uppercase()))
}
fn s_lower(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    Ok(Value::str(expect_str(a, 0, "lower", s)?.to_lowercase()))
}
fn s_trim(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    Ok(Value::str(expect_str(a, 0, "trim", s)?.trim()))
}
fn s_trim_start(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    Ok(Value::str(expect_str(a, 0, "trim_start", s)?.trim_start()))
}
fn s_trim_end(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    Ok(Value::str(expect_str(a, 0, "trim_end", s)?.trim_end()))
}
fn s_split(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let text = expect_str(a, 0, "split", s)?;
    let parts: Vec<Value> = match a.get(1) {
        None => text.split_whitespace().map(Value::str).collect(),
        Some(_) => {
            let sep = expect_str(a, 1, "split", s)?;
            if sep.is_empty() {
                text.chars().map(|c| Value::str(c.to_string())).collect()
            } else {
                text.split(sep).map(Value::str).collect()
            }
        }
    };
    Ok(Value::list(parts))
}
fn s_lines(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    Ok(Value::list(
        expect_str(a, 0, "lines", s)?
            .lines()
            .map(Value::str)
            .collect(),
    ))
}
fn s_chars(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    Ok(Value::list(
        expect_str(a, 0, "chars", s)?
            .chars()
            .map(|c| Value::str(c.to_string()))
            .collect(),
    ))
}
fn s_contains(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    Ok(Value::Bool(
        expect_str(a, 0, "contains", s)?.contains(expect_str(a, 1, "contains", s)?),
    ))
}
fn s_starts_with(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    Ok(Value::Bool(
        expect_str(a, 0, "starts_with", s)?.starts_with(expect_str(a, 1, "starts_with", s)?),
    ))
}
fn s_ends_with(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    Ok(Value::Bool(
        expect_str(a, 0, "ends_with", s)?.ends_with(expect_str(a, 1, "ends_with", s)?),
    ))
}
fn s_replace(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    Ok(Value::str(expect_str(a, 0, "replace", s)?.replace(
        expect_str(a, 1, "replace", s)?,
        expect_str(a, 2, "replace", s)?,
    )))
}
fn s_find(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let text = expect_str(a, 0, "find", s)?;
    let needle = expect_str(a, 1, "find", s)?;
    Ok(match text.find(needle) {
        Some(byte) => Value::Int(text[..byte].chars().count() as i64),
        None => Value::Int(-1),
    })
}
fn s_slice(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let text = expect_str(a, 0, "slice", s)?;
    let chars: Vec<char> = text.chars().collect();
    let (start, end) = slice_bounds(a, chars.len(), "slice", s)?;
    Ok(Value::str(chars[start..end].iter().collect::<String>()))
}
fn s_repeat(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let n = expect_int(a, 1, "repeat", s)?;
    if n < 0 {
        return Err(runtime("repeat: count cannot be negative").at(s));
    }
    let text = expect_str(a, 0, "repeat", s)?;
    crate::value::check_alloc(text.len() as u128 * n as u128, "repeat", s)?;
    Ok(Value::str(text.repeat(n as usize)))
}
fn pad(a: &[Value], name: &str, s: Span, left: bool) -> Result<Value, Diagnostic> {
    let text = expect_str(a, 0, name, s)?;
    let width = expect_int(a, 1, name, s)?.max(0) as usize;
    crate::value::check_alloc(width as u128 * 4, name, s)?;
    let fill = match a.get(2) {
        Some(_) => expect_str(a, 2, name, s)?.chars().next().unwrap_or(' '),
        None => ' ',
    };
    let n = text.chars().count();
    if n >= width {
        return Ok(Value::str(text));
    }
    let padding: String = std::iter::repeat_n(fill, width - n).collect();
    Ok(Value::str(if left {
        format!("{padding}{text}")
    } else {
        format!("{text}{padding}")
    }))
}
fn s_pad_left(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    pad(a, "pad_left", s, true)
}
fn s_pad_right(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    pad(a, "pad_right", s, false)
}
fn s_reverse(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    Ok(Value::str(
        expect_str(a, 0, "reverse", s)?
            .chars()
            .rev()
            .collect::<String>(),
    ))
}
fn s_is_empty(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    Ok(Value::Bool(expect_str(a, 0, "is_empty", s)?.is_empty()))
}
fn s_to_int(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    super::core::to_int(i, &a[..1], s)
}
fn s_to_float(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    super::core::to_float(i, &a[..1], s)
}
/// Non-overlapping count of `sub` in the receiver. An empty `sub` is 0:
/// the alternative, as Python has it, is `len(s) + 1`, which is rarely
/// what a count asks for and invites a loop that never finishes.
fn s_count(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let text = expect_str(a, 0, "count", s)?;
    let sub = expect_str(a, 1, "count", s)?;
    Ok(Value::Int(if sub.is_empty() {
        0
    } else {
        text.matches(sub).count() as i64
    }))
}

/// `(start, end)` for `.slice(start[, end])` with negative indexes counting
/// from the end, clamped into range.
fn slice_bounds(
    a: &[Value],
    len: usize,
    name: &str,
    s: Span,
) -> Result<(usize, usize), Diagnostic> {
    let norm = |i: i64| -> usize {
        let l = len as i64;
        let v = if i < 0 { l + i } else { i };
        v.clamp(0, l) as usize
    };
    let start = norm(expect_int(a, 1, name, s)?);
    let end = match a.get(2) {
        Some(Value::Null) | None => len,
        Some(_) => norm(expect_int(a, 2, name, s)?),
    };
    Ok((start, end.max(start)))
}

// ----- lists -------------------------------------------------------------------

fn l_len(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    Ok(Value::Int(
        expect_list(a, 0, "len", s)?.borrow().len() as i64
    ))
}
fn l_is_empty(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    Ok(Value::Bool(
        expect_list(a, 0, "is_empty", s)?.borrow().is_empty(),
    ))
}
fn l_push(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let list = expect_list(a, 0, "push", s)?;
    list.borrow_mut().extend(a[1..].iter().cloned());
    Ok(a[0].clone())
}
fn l_pop(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let list = expect_list(a, 0, "pop", s)?;
    let v = list.borrow_mut().pop();
    Ok(v.unwrap_or(Value::Null))
}
fn l_insert(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let list = expect_list(a, 0, "insert", s)?;
    let i = expect_int(a, 1, "insert", s)?;
    let len = list.borrow().len();
    let pos = if i == len as i64 {
        len
    } else {
        crate::interp::normalize_index(i, len).ok_or_else(|| {
            runtime(format!(
                "insert: index {i} is out of range for a list of {len}"
            ))
            .at(s)
        })?
    };
    list.borrow_mut().insert(pos, a[2].clone());
    Ok(a[0].clone())
}
fn l_remove(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let list = expect_list(a, 0, "remove", s)?;
    let i = expect_int(a, 1, "remove", s)?;
    let len = list.borrow().len();
    let pos = crate::interp::normalize_index(i, len).ok_or_else(|| {
        runtime(format!(
            "remove: index {i} is out of range for a list of {len}"
        ))
        .at(s)
    })?;
    let v = list.borrow_mut().remove(pos);
    Ok(v)
}
fn l_first(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    Ok(expect_list(a, 0, "first", s)?
        .borrow()
        .first()
        .cloned()
        .unwrap_or(Value::Null))
}
fn l_last(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    Ok(expect_list(a, 0, "last", s)?
        .borrow()
        .last()
        .cloned()
        .unwrap_or(Value::Null))
}
fn l_get(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let list = expect_list(a, 0, "get", s)?;
    let i = expect_int(a, 1, "get", s)?;
    let list = list.borrow();
    Ok(crate::interp::normalize_index(i, list.len())
        .map(|p| list[p].clone())
        .unwrap_or_else(|| a.get(2).cloned().unwrap_or(Value::Null)))
}
fn l_slice(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let list = expect_list(a, 0, "slice", s)?;
    let list = list.borrow();
    let (start, end) = slice_bounds(a, list.len(), "slice", s)?;
    Ok(Value::list(list[start..end].to_vec()))
}
fn l_contains(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    Ok(Value::Bool(
        expect_list(a, 0, "contains", s)?
            .borrow()
            .iter()
            .any(|v| v.equals(&a[1])),
    ))
}
fn l_index_of(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    Ok(Value::Int(
        expect_list(a, 0, "index_of", s)?
            .borrow()
            .iter()
            .position(|v| v.equals(&a[1]))
            .map(|p| p as i64)
            .unwrap_or(-1),
    ))
}
fn l_map(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let items = expect_list(a, 0, "map", s)?.borrow().clone();
    let f = expect_func(a, 1, "map", s)?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        out.push(i.call(&f, vec![item], s)?);
    }
    Ok(Value::list(out))
}
fn l_filter(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let items = expect_list(a, 0, "filter", s)?.borrow().clone();
    let f = expect_func(a, 1, "filter", s)?;
    let mut out = Vec::new();
    for item in items {
        if i.call(&f, vec![item.clone()], s)?.truthy() {
            out.push(item);
        }
    }
    Ok(Value::list(out))
}
fn l_reduce(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let items = expect_list(a, 0, "reduce", s)?.borrow().clone();
    let f = expect_func(a, 1, "reduce", s)?;
    let mut acc = a[2].clone();
    for item in items {
        acc = i.call(&f, vec![acc, item], s)?;
    }
    Ok(acc)
}
fn l_each(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let items = expect_list(a, 0, "each", s)?.borrow().clone();
    let f = expect_func(a, 1, "each", s)?;
    for item in items {
        i.call(&f, vec![item], s)?;
    }
    Ok(Value::Null)
}
fn l_any(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let items = expect_list(a, 0, "any", s)?.borrow().clone();
    let f = expect_func(a, 1, "any", s)?;
    for item in items {
        if i.call(&f, vec![item], s)?.truthy() {
            return Ok(Value::Bool(true));
        }
    }
    Ok(Value::Bool(false))
}
fn l_all(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let items = expect_list(a, 0, "all", s)?.borrow().clone();
    let f = expect_func(a, 1, "all", s)?;
    for item in items {
        if !i.call(&f, vec![item], s)?.truthy() {
            return Ok(Value::Bool(false));
        }
    }
    Ok(Value::Bool(true))
}
fn l_find(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let items = expect_list(a, 0, "find", s)?.borrow().clone();
    let f = expect_func(a, 1, "find", s)?;
    for item in items {
        if i.call(&f, vec![item.clone()], s)?.truthy() {
            return Ok(item);
        }
    }
    Ok(Value::Null)
}

/// Total order used by `sort`: numbers, then strings, then everything else
/// by type name, so sorting never fails on mixed lists.
pub(crate) fn compare(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        _ => match (a.as_f64(), b.as_f64()) {
            (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
            _ => rank(a).cmp(&rank(b)).then_with(|| a.repr().cmp(&b.repr())),
        },
    }
}
fn rank(v: &Value) -> u8 {
    match v {
        Value::Null => 0,
        Value::Bool(_) => 1,
        Value::Int(_) | Value::Float(_) => 2,
        Value::Str(_) => 3,
        Value::List(_) => 4,
        Value::Map(_) => 5,
        _ => 6,
    }
}
fn l_sort(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let mut items = expect_list(a, 0, "sort", s)?.borrow().clone();
    items.sort_by(compare);
    Ok(Value::list(items))
}
fn l_sort_by(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let items = expect_list(a, 0, "sort_by", s)?.borrow().clone();
    let f = expect_func(a, 1, "sort_by", s)?;
    let mut keyed = Vec::with_capacity(items.len());
    for item in items {
        let key = i.call(&f, vec![item.clone()], s)?;
        keyed.push((key, item));
    }
    keyed.sort_by(|x, y| compare(&x.0, &y.0));
    Ok(Value::list(keyed.into_iter().map(|(_, v)| v).collect()))
}
fn l_reverse(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let mut items = expect_list(a, 0, "reverse", s)?.borrow().clone();
    items.reverse();
    Ok(Value::list(items))
}
fn l_join(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let list = expect_list(a, 0, "join", s)?;
    let sep = match a.get(1) {
        Some(_) => expect_str(a, 1, "join", s)?,
        None => "",
    };
    let items = list.borrow();
    Ok(crate::value::build_str(|out| {
        for (n, v) in items.iter().enumerate() {
            if n > 0 {
                out.push_str(sep);
            }
            v.write_display(out);
        }
    }))
}
fn l_unique(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let items = expect_list(a, 0, "unique", s)?.borrow().clone();
    let mut out: Vec<Value> = Vec::new();
    for item in items {
        if !out.iter().any(|v| v.equals(&item)) {
            out.push(item);
        }
    }
    Ok(Value::list(out))
}
fn l_flatten(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let items = expect_list(a, 0, "flatten", s)?.borrow().clone();
    let mut out = Vec::new();
    for item in items {
        match item {
            Value::List(inner) => out.extend(inner.borrow().iter().cloned()),
            other => out.push(other),
        }
    }
    Ok(Value::list(out))
}
fn l_sum(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let items = expect_list(a, 0, "sum", s)?;
    let mut int_sum: i64 = 0;
    let mut float_sum = 0.0;
    let mut any_float = false;
    for v in items.borrow().iter() {
        match v {
            Value::Int(i) => {
                int_sum = int_sum
                    .checked_add(*i)
                    .ok_or_else(|| runtime("sum: integer overflow").at(s))?
            }
            Value::Float(f) => {
                any_float = true;
                float_sum += f;
            }
            other => return Err(type_error(format!("sum: cannot add {}", other.type_name())).at(s)),
        }
    }
    Ok(if any_float {
        Value::Float(float_sum + int_sum as f64)
    } else {
        Value::Int(int_sum)
    })
}
fn l_min(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let items = expect_list(a, 0, "min", s)?;
    let items = items.borrow();
    Ok(items
        .iter()
        .min_by(|x, y| compare(x, y))
        .cloned()
        .unwrap_or(Value::Null))
}
fn l_max(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let items = expect_list(a, 0, "max", s)?;
    let items = items.borrow();
    Ok(items
        .iter()
        .max_by(|x, y| compare(x, y))
        .cloned()
        .unwrap_or(Value::Null))
}
fn l_zip(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let x = expect_list(a, 0, "zip", s)?;
    let y = expect_list(a, 1, "zip", s)?;
    let out = x
        .borrow()
        .iter()
        .zip(y.borrow().iter())
        .map(|(p, q)| Value::list(vec![p.clone(), q.clone()]))
        .collect();
    Ok(Value::list(out))
}
fn l_enumerate(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let x = expect_list(a, 0, "enumerate", s)?;
    let out = x
        .borrow()
        .iter()
        .enumerate()
        .map(|(i, v)| Value::list(vec![Value::Int(i as i64), v.clone()]))
        .collect();
    Ok(Value::list(out))
}
fn l_chunks(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let x = expect_list(a, 0, "chunks", s)?;
    let n = expect_int(a, 1, "chunks", s)?;
    if n <= 0 {
        return Err(runtime("chunks: size must be positive").at(s));
    }
    let out = x
        .borrow()
        .chunks(n as usize)
        .map(|c| Value::list(c.to_vec()))
        .collect();
    Ok(Value::list(out))
}
fn l_copy(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    Ok(Value::list(expect_list(a, 0, "copy", s)?.borrow().clone()))
}
/// A new list of `n` copies of the receiver end to end, as the string's
/// `repeat` does to its characters. A negative `n` is the error a
/// string's `repeat` gives; the value ceiling (E514) stops a request
/// that would need too much memory before anything is built.
fn l_repeat(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let n = expect_int(a, 1, "repeat", s)?;
    if n < 0 {
        return Err(runtime("repeat: count cannot be negative").at(s));
    }
    let list = expect_list(a, 0, "repeat", s)?;
    let items = list.borrow();
    // Nothing to copy, or no copies asked for: an empty list at once,
    // however large the count (a loop of `n` turns over no elements
    // would otherwise run outside the step budget).
    if n == 0 || items.is_empty() {
        return Ok(Value::list(Vec::new()));
    }
    let n = n as u128;
    let len = items.len() as u128;
    let bytes = len
        .saturating_mul(n)
        .saturating_mul(std::mem::size_of::<Value>() as u128);
    crate::value::check_alloc(bytes, "repeat", s)?;
    // `.saturating_mul` might have clamped: the ceiling check already
    // refused anything that big, but a pedantic cast here keeps the
    // allocation itself from overflowing `usize` on a 32-bit build.
    let total = (len * n) as usize;
    let n = n as usize;
    let mut out = Vec::with_capacity(total);
    for _ in 0..n {
        out.extend(items.iter().cloned());
    }
    Ok(Value::list(out))
}
/// A map from each value in the receiver to how many times it appears,
/// in first-seen order (cig's maps keep insertion order). A list
/// element that cannot be a map key is the same type error map keys
/// give elsewhere (`map keys must be strings, got X`).
fn l_counts(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let list = expect_list(a, 0, "counts", s)?;
    let items = list.borrow();
    let mut out = indexmap::IndexMap::<String, Value>::new();
    for item in items.iter() {
        let key = crate::interp::map_key(item.clone(), s)?;
        match out.get_mut(&key) {
            // A tally cannot pass the number of elements a list holds.
            Some(Value::Int(n)) => *n += 1,
            _ => {
                out.insert(key, Value::Int(1));
            }
        }
    }
    Ok(Value::map(out))
}
/// How many elements equal `x`, by cig's own equality (ints and floats
/// compare numerically, as `list.contains` does).
fn l_count(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    Ok(Value::Int(
        expect_list(a, 0, "count", s)?
            .borrow()
            .iter()
            .filter(|v| v.equals(&a[1]))
            .count() as i64,
    ))
}
/// Append every element of `ys` to the receiver in place, sized once.
/// `xs.extend(xs)` works: the elements to append are snapshotted first,
/// so the loop that places them does not see what it is appending. The
/// receiver is returned.
fn l_extend(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let xs = expect_list(a, 0, "extend", s)?;
    let ys = expect_list(a, 1, "extend", s)?;
    // The to-append side has to be read first to work with
    // `xs.extend(xs)`: a `borrow_mut` on `xs` below would refuse the
    // second borrow, and a `borrow()` on `ys` after it would refuse the
    // other way round.
    let appended: Vec<Value> = if Rc::ptr_eq(&xs, &ys) {
        xs.borrow().clone()
    } else {
        ys.borrow().iter().cloned().collect()
    };
    let have = xs.borrow().len() as u128;
    let bytes =
        (have + appended.len() as u128).saturating_mul(std::mem::size_of::<Value>() as u128);
    crate::value::check_alloc(bytes, "extend", s)?;
    let mut inner = xs.borrow_mut();
    inner.reserve(appended.len());
    inner.extend(appended);
    drop(inner);
    Ok(a[0].clone())
}

// ----- maps ----------------------------------------------------------------------

fn m_len(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    Ok(Value::Int(expect_map(a, 0, "len", s)?.borrow().len() as i64))
}
fn m_is_empty(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    Ok(Value::Bool(
        expect_map(a, 0, "is_empty", s)?.borrow().is_empty(),
    ))
}
fn m_keys(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    Ok(Value::list(
        expect_map(a, 0, "keys", s)?
            .borrow()
            .keys()
            .map(Value::str)
            .collect(),
    ))
}
fn m_values(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    Ok(Value::list(
        expect_map(a, 0, "values", s)?
            .borrow()
            .values()
            .cloned()
            .collect(),
    ))
}
fn m_entries(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    Ok(Value::list(
        expect_map(a, 0, "entries", s)?
            .borrow()
            .iter()
            .map(|(k, v)| Value::list(vec![Value::str(k), v.clone()]))
            .collect(),
    ))
}
fn m_has(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let m = expect_map(a, 0, "has", s)?;
    let k = expect_str(a, 1, "has", s)?;
    let has = m.borrow().contains_key(k);
    Ok(Value::Bool(has))
}
fn m_get(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let m = expect_map(a, 0, "get", s)?;
    let k = expect_str(a, 1, "get", s)?;
    let found = m.borrow().get(k).cloned();
    Ok(found.unwrap_or_else(|| a.get(2).cloned().unwrap_or(Value::Null)))
}
fn m_set(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let m = expect_map(a, 0, "set", s)?;
    let k = expect_str(a, 1, "set", s)?;
    m.borrow_mut().insert(k.to_string(), a[2].clone());
    Ok(a[0].clone())
}
fn m_remove(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let m = expect_map(a, 0, "remove", s)?;
    let k = expect_str(a, 1, "remove", s)?;
    let v = m.borrow_mut().shift_remove(k);
    Ok(v.unwrap_or(Value::Null))
}
fn m_merge(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let x = expect_map(a, 0, "merge", s)?;
    let y = expect_map(a, 1, "merge", s)?;
    let mut out = x.borrow().clone();
    for (k, v) in y.borrow().iter() {
        out.insert(k.clone(), v.clone());
    }
    Ok(Value::record(out))
}
fn m_copy(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    Ok(Value::record(expect_map(a, 0, "copy", s)?.borrow().clone()))
}
/// `m.inc(key)` adds 1, `m.inc(key, by)` adds `by`, to the entry at
/// `key`. A missing entry is created at 0 first, so one lookup covers
/// `has`, the read and the write. The new value is returned. An
/// existing entry that is not a number is the type error map arithmetic
/// gives elsewhere.
fn m_inc(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let map = expect_map(a, 0, "inc", s)?;
    let key = expect_str(a, 1, "inc", s)?;
    // By default, 1. `by` may be an int or a float.
    enum By {
        Int(i64),
        Float(f64),
    }
    let by = match a.get(2) {
        None => By::Int(1),
        Some(Value::Int(n)) => By::Int(*n),
        Some(Value::Float(f)) => By::Float(*f),
        Some(other) => {
            return Err(type_error(format!(
                "inc: argument 3 must be a number, got {} ({})",
                other.type_name(),
                crate::value::truncate(&other.repr(), 40)
            ))
            .code("E402")
            .at(s));
        }
    };
    // One lookup: the entry is found (or made, at the end, where a new
    // key goes) and then read and written by its index.
    let mut inner = map.borrow_mut();
    let idx = match inner.get_index_of(key) {
        Some(idx) => idx,
        None => inner.insert_full(key.to_string(), Value::Int(0)).0,
    };
    let (_, slot) = inner.get_index_mut(idx).expect("just-inserted entry");
    let new = match (&slot, &by) {
        (Value::Int(n), By::Int(d)) => Value::Int(
            n.checked_add(*d)
                .ok_or_else(|| runtime("inc: integer overflow").code("E503").at(s))?,
        ),
        (Value::Int(n), By::Float(d)) => Value::Float(*n as f64 + d),
        (Value::Float(n), By::Int(d)) => Value::Float(n + *d as f64),
        (Value::Float(n), By::Float(d)) => Value::Float(n + d),
        (other, _) => {
            return Err(type_error(format!(
                "inc: the entry at {} is {}, not a number ({})",
                crate::value::truncate(&Value::str(key).repr(), 40),
                other.type_name(),
                crate::value::truncate(&other.repr(), 40)
            ))
            .code("E402")
            .at(s));
        }
    };
    *slot = new.clone();
    Ok(new)
}
