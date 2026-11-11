// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! CSV (RFC 4180 quoting). Rows are lists of strings, or maps when a header
//! row is used.

use super::b;
use crate::burn::{Decision, Op};
use crate::diagnostics::{runtime, type_error, Diagnostic};
use crate::interp::Interp;
use crate::syntax::span::Span;
use crate::value::{expect_list, expect_str, opt_map, Builtin, Effect, Value};
use indexmap::IndexMap;
use std::path::PathBuf;

pub static TABLE: &[Builtin] = &[
    b("csv.parse", Effect::Pure, 1, Some(2), parse),
    b("csv.stringify", Effect::Pure, 1, Some(2), stringify),
    b("csv.read", Effect::Read, 1, Some(2), read),
    b("csv.write", Effect::Write, 2, Some(3), write),
];

struct Opts {
    header: bool,
    sep: char,
}

fn opts(a: &[Value], i: usize, name: &str, s: Span) -> Result<Opts, Diagnostic> {
    let mut o = Opts {
        header: false,
        sep: ',',
    };
    if let Some(m) = opt_map(a, i, name, s)? {
        for (k, v) in m.borrow().iter() {
            match (k.as_str(), v) {
                ("header", v) => o.header = v.truthy(),
                ("sep", Value::Str(sep)) => {
                    o.sep = sep
                        .chars()
                        .next()
                        .ok_or_else(|| runtime(format!("{name}: `sep` is empty")).at(s))?
                }
                (other, _) => {
                    return Err(runtime(format!("{name}: unknown option `{other}`"))
                        .at(s)
                        .with_hint("use header (bool) or sep (string)"))
                }
            }
        }
    }
    Ok(o)
}

pub(crate) fn parse_rows(text: &str, sep: char) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut chars = text.chars().peekable();
    let mut in_quotes = false;
    let mut any = false;
    while let Some(c) = chars.next() {
        any = true;
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' => in_quotes = true,
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            c if c == sep => row.push(std::mem::take(&mut field)),
            c => field.push(c),
        }
    }
    if any && (!field.is_empty() || !row.is_empty()) {
        row.push(field);
        rows.push(row);
    }
    rows
}

pub(crate) fn rows_to_value(rows: Vec<Vec<String>>, header: bool) -> Value {
    if !header {
        return Value::list(
            rows.into_iter()
                .map(|r| Value::list(r.into_iter().map(Value::str).collect()))
                .collect(),
        );
    }
    let mut it = rows.into_iter();
    let Some(head) = it.next() else {
        return Value::list(vec![]);
    };
    Value::list(
        it.map(|r| {
            let mut m = IndexMap::with_capacity(head.len());
            for (i, name) in head.iter().enumerate() {
                m.insert(
                    name.clone(),
                    r.get(i).map(Value::str).unwrap_or(Value::Null),
                );
            }
            Value::map(m)
        })
        .collect(),
    )
}

fn quote(field: &str, sep: char) -> String {
    if field.contains(sep) || field.contains('"') || field.contains('\n') || field.contains('\r') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

pub(crate) fn rows_to_text(
    rows: &Value,
    sep: char,
    header: bool,
    s: Span,
) -> Result<String, Diagnostic> {
    let Value::List(list) = rows else {
        return Err(type_error(format!(
            "csv: expected a list of rows, got {}",
            rows.type_name()
        ))
        .at(s));
    };
    let list = list.borrow();
    let mut out = String::new();
    let mut columns: Option<Vec<String>> = None;
    for (n, row) in list.iter().enumerate() {
        let fields: Vec<String> = match row {
            Value::List(r) => r.borrow().iter().map(|v| v.display()).collect(),
            Value::Map(m) => {
                let m = m.borrow();
                let cols = columns.get_or_insert_with(|| m.keys().cloned().collect());
                if header && n == 0 {
                    out.push_str(
                        &cols
                            .iter()
                            .map(|c| quote(c, sep))
                            .collect::<Vec<_>>()
                            .join(&sep.to_string()),
                    );
                    out.push('\n');
                }
                cols.iter()
                    .map(|c| m.get(c).map(|v| v.display()).unwrap_or_default())
                    .collect()
            }
            other => {
                return Err(type_error(format!(
                    "csv: row {n} must be a list or a map, got {}",
                    other.type_name()
                ))
                .at(s))
            }
        };
        out.push_str(
            &fields
                .iter()
                .map(|f| quote(f, sep))
                .collect::<Vec<_>>()
                .join(&sep.to_string()),
        );
        out.push('\n');
    }
    Ok(out)
}

fn parse(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let text = expect_str(a, 0, "csv.parse", s)?;
    let o = opts(a, 1, "csv.parse", s)?;
    Ok(rows_to_value(parse_rows(text, o.sep), o.header))
}

fn stringify(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    expect_list(a, 0, "csv.stringify", s)?;
    let o = opts(a, 1, "csv.stringify", s)?;
    let header = a.get(1).is_none() || o.header || matches!(a.get(1), Some(Value::Null));
    Ok(Value::str(rows_to_text(&a[0], o.sep, header, s)?))
}

fn read(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let p = expect_str(a, 0, "csv.read", s)?;
    let o = opts(a, 1, "csv.read", s)?;
    let text = match super::fs::ghost_text(i, "csv.read", std::path::Path::new(p), s)? {
        Some(t) => t,
        None if i.recording() => {
            let b = super::fs::rec_read(i, "csv.read", std::path::Path::new(p), s)?;
            String::from_utf8_lossy(&b).into_owned()
        }
        None => {
            super::fs::check_read_size("csv.read", std::path::Path::new(p), s)?;
            std::fs::read_to_string(p).map_err(|e| {
                runtime(format!("csv.read: {p}: {}", super::fs::describe_io(&e)))
                    .code("E508")
                    .at(s)
                    .with_subject(p.to_string())
            })?
        }
    };
    Ok(rows_to_value(parse_rows(&text, o.sep), o.header))
}

fn write(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let p = PathBuf::from(expect_str(a, 0, "csv.write", s)?);
    expect_list(a, 1, "csv.write", s)?;
    let o = opts(a, 2, "csv.write", s)?;
    let header = a.get(2).is_none() || o.header;
    let text = rows_to_text(&a[1], o.sep, header, s)?;
    if i.effect(
        "csv.write",
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
                    "csv.write: {}: {}",
                    parent.display(),
                    super::fs::describe_io(&e)
                ))
                .at(s)
            })?;
        }
        std::fs::write(&p, &text).map_err(|e| {
            runtime(format!(
                "csv.write: {}: {}",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quotes_and_newlines() {
        let rows = parse_rows(
            "a,b\n\"x,1\",\"say \"\"hi\"\"\"\r\nlast,\"multi\nline\"\n",
            ',',
        );
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1], vec!["x,1", "say \"hi\""]);
        assert_eq!(rows[2][1], "multi\nline");
    }

    #[test]
    fn quoting_round_trips() {
        assert_eq!(quote("plain", ','), "plain");
        assert_eq!(quote("a,b", ','), "\"a,b\"");
        assert_eq!(quote("q\"q", ','), "\"q\"\"q\"");
    }
}
