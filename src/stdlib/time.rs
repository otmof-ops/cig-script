// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Clocks. Reading the time is an input, not an effect.

use super::b;
use crate::diagnostics::Diagnostic;
use crate::interp::Interp;
use crate::syntax::span::Span;
use crate::value::{expect_int, Builtin, Effect, Value};

pub static TABLE: &[Builtin] = &[
    b("time.now_ms", Effect::Read, 0, Some(0), now_ms),
    b("time.now_iso", Effect::Read, 0, Some(0), now_iso),
    b("time.stamp", Effect::Read, 0, Some(0), stamp),
    b("time.sleep_ms", Effect::Pure, 1, Some(1), sleep_ms),
    b("time.format_ms", Effect::Pure, 1, Some(2), format_ms),
];

fn now_ms(_: &mut Interp, _: &[Value], _: Span) -> Result<Value, Diagnostic> {
    Ok(Value::Int(chrono::Utc::now().timestamp_millis()))
}

fn now_iso(_: &mut Interp, _: &[Value], _: Span) -> Result<Value, Diagnostic> {
    Ok(Value::str(
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    ))
}

/// A filesystem-safe local timestamp: `20260926T021500`.
fn stamp(_: &mut Interp, _: &[Value], _: Span) -> Result<Value, Diagnostic> {
    Ok(Value::str(
        chrono::Local::now().format("%Y%m%dT%H%M%S").to_string(),
    ))
}

fn sleep_ms(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let ms = expect_int(a, 0, "time.sleep_ms", s)?.max(0) as u64;
    std::thread::sleep(std::time::Duration::from_millis(ms));
    Ok(Value::Null)
}

/// Format a millisecond timestamp with a strftime pattern (UTC), default
/// `%Y-%m-%d %H:%M:%S`.
fn format_ms(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let ms = expect_int(a, 0, "time.format_ms", s)?;
    let pattern = match a.get(1) {
        Some(_) => crate::value::expect_str(a, 1, "time.format_ms", s)?,
        None => "%Y-%m-%d %H:%M:%S",
    };
    let dt = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms).ok_or_else(|| {
        crate::diagnostics::runtime("time.format_ms: timestamp out of range").at(s)
    })?;
    Ok(Value::str(dt.format(pattern).to_string()))
}
