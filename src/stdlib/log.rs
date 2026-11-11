// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Diagnostics to stderr. Program output belongs on stdout via `exhale`;
//! commentary about the run belongs here.

use super::b;
use crate::diagnostics::Diagnostic;
use crate::interp::Interp;
use crate::syntax::span::Span;
use crate::value::{Builtin, Effect, Value};

pub static TABLE: &[Builtin] = &[
    b("log.debug", Effect::Pure, 1, None, debug),
    b("log.info", Effect::Pure, 1, None, info),
    b("log.warn", Effect::Pure, 1, None, warn),
    b("log.error", Effect::Pure, 1, None, error),
];

fn emit(i: &mut Interp, level: &str, a: &[Value]) -> Result<Value, Diagnostic> {
    let text = a.iter().map(|v| v.display()).collect::<Vec<_>>().join(" ");
    let _ = writeln!(i.err, "{level}: {text}");
    Ok(Value::Null)
}

use std::io::Write as _;

fn debug(i: &mut Interp, a: &[Value], _: Span) -> Result<Value, Diagnostic> {
    if std::env::var_os("CIG_DEBUG").is_none() {
        return Ok(Value::Null);
    }
    emit(i, "debug", a)
}

fn info(i: &mut Interp, a: &[Value], _: Span) -> Result<Value, Diagnostic> {
    emit(i, "info", a)
}

fn warn(i: &mut Interp, a: &[Value], _: Span) -> Result<Value, Diagnostic> {
    emit(i, "warn", a)
}

fn error(i: &mut Interp, a: &[Value], _: Span) -> Result<Value, Diagnostic> {
    emit(i, "error", a)
}
