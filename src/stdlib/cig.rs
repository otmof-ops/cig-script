// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The cig a script is running under: its version, the platform it was
//! built for, and the run in progress. A tool written in CigScript (an
//! orchestrator, a launcher) uses these to check the cig it was handed
//! against what it was written for, and to name its own run.

use super::b;
use crate::diagnostics::Diagnostic;
use crate::interp::Interp;
use crate::syntax::span::Span;
use crate::value::{Builtin, Effect, Value};

pub static TABLE: &[Builtin] = &[
    b("cig.version", Effect::Pure, 0, Some(0), version),
    b("cig.os", Effect::Pure, 0, Some(0), os),
    b("cig.arch", Effect::Pure, 0, Some(0), arch),
    b("cig.run_id", Effect::Read, 0, Some(0), run_id),
];

/// The running cig's version, `1.2.0`, as `cig --version` prints it.
fn version(_: &mut Interp, _: &[Value], _: Span) -> Result<Value, Diagnostic> {
    Ok(Value::str(crate::VERSION))
}

/// `linux`, `macos` or `windows`, as the crash report names it.
fn os(_: &mut Interp, _: &[Value], _: Span) -> Result<Value, Diagnostic> {
    Ok(Value::str(std::env::consts::OS))
}

/// `x86_64` or `aarch64`, as the crash report names it.
fn arch(_: &mut Interp, _: &[Value], _: Span) -> Result<Value, Diagnostic> {
    Ok(Value::str(std::env::consts::ARCH))
}

/// The id of the run this script is lighting, the one `cig unburn` takes;
/// null in a dry run, under `cig check`, and before a run has a record.
fn run_id(_: &mut Interp, _: &[Value], _: Span) -> Result<Value, Diagnostic> {
    Ok(match crate::crash::active_run() {
        Some(id) => Value::str(&id),
        None => Value::Null,
    })
}
