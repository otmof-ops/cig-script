// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Arithmetic helpers. Deliberately no random numbers: a run must be
//! reproducible from its inputs.

use super::b;
use crate::diagnostics::{runtime, type_error, Diagnostic};
use crate::interp::Interp;
use crate::syntax::span::Span;
use crate::value::{expect_num, Builtin, Effect, Value};

pub static TABLE: &[Builtin] = &[
    b("math.abs", Effect::Pure, 1, Some(1), abs),
    b("math.min", Effect::Pure, 1, None, min),
    b("math.max", Effect::Pure, 1, None, max),
    b("math.floor", Effect::Pure, 1, Some(1), floor),
    b("math.ceil", Effect::Pure, 1, Some(1), ceil),
    b("math.round", Effect::Pure, 1, Some(2), round),
    b("math.sqrt", Effect::Pure, 1, Some(1), sqrt),
    b("math.pow", Effect::Pure, 2, Some(2), pow),
    b("math.clamp", Effect::Pure, 3, Some(3), clamp),
];

fn abs(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    match &a[0] {
        Value::Int(i) => i
            .checked_abs()
            .map(Value::Int)
            .ok_or_else(|| runtime("math.abs: integer overflow").at(s)),
        Value::Float(f) => Ok(Value::Float(f.abs())),
        other => Err(type_error(format!(
            "math.abs: expected a number, got {}",
            other.type_name()
        ))
        .at(s)),
    }
}

fn spread(a: &[Value]) -> Vec<Value> {
    match a {
        [Value::List(l)] => l.borrow().clone(),
        other => other.to_vec(),
    }
}

fn extreme(a: &[Value], name: &str, s: Span, want_min: bool) -> Result<Value, Diagnostic> {
    let items = spread(a);
    if items.is_empty() {
        return Err(runtime(format!("{name}: nothing to compare")).at(s));
    }
    let mut best = items[0].clone();
    for v in &items[1..] {
        if v.as_f64().is_none() {
            return Err(
                type_error(format!("{name}: expected numbers, got {}", v.type_name())).at(s),
            );
        }
        let ord = super::methods::compare(v, &best);
        if (want_min && ord.is_lt()) || (!want_min && ord.is_gt()) {
            best = v.clone();
        }
    }
    if best.as_f64().is_none() {
        return Err(type_error(format!(
            "{name}: expected numbers, got {}",
            best.type_name()
        ))
        .at(s));
    }
    Ok(best)
}

fn min(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    extreme(a, "math.min", s, true)
}

fn max(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    extreme(a, "math.max", s, false)
}

fn to_int_checked(f: f64, name: &str, s: Span) -> Result<Value, Diagnostic> {
    if !f.is_finite() || f.abs() > i64::MAX as f64 {
        return Err(runtime(format!("{name}: result does not fit in an int")).at(s));
    }
    Ok(Value::Int(f as i64))
}

fn floor(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    match &a[0] {
        Value::Int(i) => Ok(Value::Int(*i)),
        _ => to_int_checked(expect_num(a, 0, "math.floor", s)?.floor(), "math.floor", s),
    }
}

fn ceil(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    match &a[0] {
        Value::Int(i) => Ok(Value::Int(*i)),
        _ => to_int_checked(expect_num(a, 0, "math.ceil", s)?.ceil(), "math.ceil", s),
    }
}

/// `math.round(x)` gives an int; `math.round(x, places)` gives a float.
fn round(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let x = expect_num(a, 0, "math.round", s)?;
    match a.get(1) {
        None => match &a[0] {
            Value::Int(i) => Ok(Value::Int(*i)),
            _ => to_int_checked(x.round(), "math.round", s),
        },
        Some(_) => {
            let places = crate::value::expect_int(a, 1, "math.round", s)?.clamp(0, 15);
            let factor = 10f64.powi(places as i32);
            Ok(Value::Float((x * factor).round() / factor))
        }
    }
}

fn sqrt(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let x = expect_num(a, 0, "math.sqrt", s)?;
    if x < 0.0 {
        return Err(runtime("math.sqrt: negative input").at(s));
    }
    Ok(Value::Float(x.sqrt()))
}

fn pow(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    match (&a[0], &a[1]) {
        (Value::Int(base), Value::Int(exp)) if *exp >= 0 => base
            .checked_pow(u32::try_from(*exp).unwrap_or(u32::MAX))
            .map(Value::Int)
            .ok_or_else(|| runtime("math.pow: integer overflow").at(s)),
        _ => Ok(Value::Float(
            expect_num(a, 0, "math.pow", s)?.powf(expect_num(a, 1, "math.pow", s)?),
        )),
    }
}

fn clamp(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    match (&a[0], &a[1], &a[2]) {
        (Value::Int(x), Value::Int(lo), Value::Int(hi)) => {
            if lo > hi {
                return Err(runtime("math.clamp: low bound is above high bound").at(s));
            }
            Ok(Value::Int((*x).clamp(*lo, *hi)))
        }
        _ => {
            let (x, lo, hi) = (
                expect_num(a, 0, "math.clamp", s)?,
                expect_num(a, 1, "math.clamp", s)?,
                expect_num(a, 2, "math.clamp", s)?,
            );
            if lo > hi {
                return Err(runtime("math.clamp: low bound is above high bound").at(s));
            }
            Ok(Value::Float(x.clamp(lo, hi)))
        }
    }
}
