// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Global functions.

use super::b;
use crate::diagnostics::{runtime, type_error, Diagnostic, Kind};
use crate::interp::Interp;
use crate::syntax::span::Span;
use crate::value::{expect_int, expect_map, Builtin, Effect, Value};
use std::io::Read;

pub static GLOBALS: &[Builtin] = &[
    b("len", Effect::Pure, 1, Some(1), len),
    b("str", Effect::Pure, 1, Some(1), to_str),
    b("repr", Effect::Pure, 1, Some(1), repr),
    b("int", Effect::Pure, 1, Some(1), to_int),
    b("float", Effect::Pure, 1, Some(1), to_float),
    b("bool", Effect::Pure, 1, Some(1), to_bool),
    b("type_of", Effect::Pure, 1, Some(1), type_of),
    b("range", Effect::Pure, 1, Some(3), range),
    b("keys", Effect::Pure, 1, Some(1), keys),
    b("values", Effect::Pure, 1, Some(1), values),
    b("entries", Effect::Pure, 1, Some(1), entries),
    b("assert", Effect::Pure, 1, Some(2), assert),
    b("exit", Effect::Pure, 0, Some(1), exit),
    b("stdin", Effect::Read, 0, Some(0), stdin),
    b("light", Effect::Pure, 1, Some(2), super::chain::light).calling_back(),
];

fn len(_: &mut Interp, args: &[Value], span: Span) -> Result<Value, Diagnostic> {
    Ok(Value::Int(match &args[0] {
        Value::Str(s) => s.chars().count() as i64,
        Value::List(l) => l.borrow().len() as i64,
        Value::Map(m) => m.borrow().len() as i64,
        other => {
            return Err(type_error(format!("len: {} has no length", other.type_name())).at(span))
        }
    }))
}

fn to_str(_: &mut Interp, args: &[Value], _: Span) -> Result<Value, Diagnostic> {
    Ok(Value::str(args[0].display()))
}

fn repr(_: &mut Interp, args: &[Value], _: Span) -> Result<Value, Diagnostic> {
    Ok(Value::str(args[0].repr()))
}

/// A float becomes an int only when it fits: a silent saturation turned
/// 1e300 into 9223372036854775807.
fn int_from_float(f: f64, span: Span) -> Result<Value, Diagnostic> {
    let t = f.trunc();
    if !(-9223372036854775808.0..9223372036854775808.0).contains(&t) {
        return Err(runtime(format!(
            "int: {} does not fit in an int",
            crate::value::format_float(f)
        ))
        .code("E503")
        .at(span)
        .with_hint("ints run from -9223372036854775808 to 9223372036854775807; keep it a float, or clamp it first"));
    }
    Ok(Value::Int(t as i64))
}

pub(super) fn to_int(_: &mut Interp, args: &[Value], span: Span) -> Result<Value, Diagnostic> {
    match &args[0] {
        Value::Int(i) => Ok(Value::Int(*i)),
        Value::Float(f) if f.is_finite() => int_from_float(*f, span),
        Value::Float(_) => Err(runtime("int: cannot convert a non-finite float").at(span)),
        Value::Bool(b) => Ok(Value::Int(*b as i64)),
        Value::Str(s) => {
            let t = s.trim();
            if let Ok(i) = t.parse::<i64>() {
                return Ok(Value::Int(i));
            }
            match t.parse::<f64>() {
                Ok(f) if f.is_finite() => int_from_float(f, span),
                _ => Err(runtime(format!(
                    "int: `{}` is not a number",
                    crate::value::truncate(t, 30)
                ))
                .at(span)),
            }
        }
        other => Err(type_error(format!("int: cannot convert {}", other.type_name())).at(span)),
    }
}

pub(super) fn to_float(_: &mut Interp, args: &[Value], span: Span) -> Result<Value, Diagnostic> {
    match &args[0] {
        Value::Int(i) => Ok(Value::Float(*i as f64)),
        Value::Float(f) => Ok(Value::Float(*f)),
        Value::Bool(b) => Ok(Value::Float(if *b { 1.0 } else { 0.0 })),
        Value::Str(s) => s.trim().parse::<f64>().map(Value::Float).map_err(|_| {
            runtime(format!(
                "float: `{}` is not a number",
                crate::value::truncate(s.trim(), 30)
            ))
            .at(span)
        }),
        other => Err(type_error(format!("float: cannot convert {}", other.type_name())).at(span)),
    }
}

fn to_bool(_: &mut Interp, args: &[Value], _: Span) -> Result<Value, Diagnostic> {
    Ok(Value::Bool(args[0].truthy()))
}

fn type_of(_: &mut Interp, args: &[Value], _: Span) -> Result<Value, Diagnostic> {
    Ok(Value::str(args[0].type_name()))
}

fn range(_: &mut Interp, args: &[Value], span: Span) -> Result<Value, Diagnostic> {
    let (start, end, step) = match args.len() {
        1 => (0, expect_int(args, 0, "range", span)?, 1),
        2 => (
            expect_int(args, 0, "range", span)?,
            expect_int(args, 1, "range", span)?,
            1,
        ),
        _ => (
            expect_int(args, 0, "range", span)?,
            expect_int(args, 1, "range", span)?,
            expect_int(args, 2, "range", span)?,
        ),
    };
    if step == 0 {
        return Err(runtime("range: step cannot be 0").at(span));
    }
    let count = if step > 0 {
        (end - start).max(0) as u64 / step as u64 + u64::from((end - start).max(0) % step != 0)
    } else {
        (start - end).max(0) as u64 / step.unsigned_abs()
            + u64::from((start - end).max(0) % step.unsigned_abs() as i64 != 0)
    };
    if count > 10_000_000 {
        return Err(runtime("range: too large (limit 10,000,000)")
            .code("E512")
            .at(span));
    }
    let mut out = Vec::with_capacity(count as usize);
    let mut i = start;
    while (step > 0 && i < end) || (step < 0 && i > end) {
        out.push(Value::Int(i));
        i += step;
    }
    Ok(Value::list(out))
}

fn keys(_: &mut Interp, args: &[Value], span: Span) -> Result<Value, Diagnostic> {
    let m = expect_map(args, 0, "keys", span)?;
    let v = m.borrow().keys().map(Value::str).collect();
    Ok(Value::list(v))
}

fn values(_: &mut Interp, args: &[Value], span: Span) -> Result<Value, Diagnostic> {
    let m = expect_map(args, 0, "values", span)?;
    let v = m.borrow().values().cloned().collect();
    Ok(Value::list(v))
}

fn entries(_: &mut Interp, args: &[Value], span: Span) -> Result<Value, Diagnostic> {
    let m = expect_map(args, 0, "entries", span)?;
    let v = m
        .borrow()
        .iter()
        .map(|(k, v)| Value::list(vec![Value::str(k), v.clone()]))
        .collect();
    Ok(Value::list(v))
}

fn assert(interp: &mut Interp, args: &[Value], span: Span) -> Result<Value, Diagnostic> {
    if args[0].truthy() {
        return Ok(Value::Null);
    }
    let message = args
        .get(1)
        .map(|m| m.display())
        .unwrap_or_else(|| "assertion failed".to_string());
    interp.cough_payload = None;
    Err(Diagnostic::new(Kind::Cough, message).code("E601").at(span))
}

fn exit(interp: &mut Interp, args: &[Value], span: Span) -> Result<Value, Diagnostic> {
    let code = if args.is_empty() {
        0
    } else {
        expect_int(args, 0, "exit", span)?
    };
    interp.exit_requested = Some(code.clamp(0, 255) as i32);
    // The interpreter recognises the exit request and treats this as a
    // clean stop rather than an error.
    Err(Diagnostic::new(Kind::Runtime, format!("exit({code})")).at(span))
}

fn stdin(_: &mut Interp, _: &[Value], span: Span) -> Result<Value, Diagnostic> {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .map_err(|e| runtime(format!("stdin: {e}")).at(span))?;
    Ok(Value::str(buf))
}
