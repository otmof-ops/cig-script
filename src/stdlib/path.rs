// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Pure path manipulation. Nothing here touches the disk except `abs`,
//! which needs the current directory.

use super::b;
use crate::diagnostics::{runtime, Diagnostic};
use crate::interp::Interp;
use crate::syntax::span::Span;
use crate::value::{expect_str, Builtin, Effect, Value};
use std::path::{Component, Path, PathBuf};

pub static TABLE: &[Builtin] = &[
    b("path.join", Effect::Pure, 1, None, join),
    b("path.base", Effect::Pure, 1, Some(1), base),
    b("path.dir", Effect::Pure, 1, Some(1), dir),
    b("path.ext", Effect::Pure, 1, Some(1), ext),
    b("path.stem", Effect::Pure, 1, Some(1), stem),
    b("path.with_ext", Effect::Pure, 2, Some(2), with_ext),
    b("path.normalize", Effect::Pure, 1, Some(1), normalize),
    b("path.is_abs", Effect::Pure, 1, Some(1), is_abs),
    b("path.abs", Effect::Read, 1, Some(1), abs),
    b("path.parts", Effect::Pure, 1, Some(1), parts),
];

fn join(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let mut out = PathBuf::new();
    for i in 0..a.len() {
        out.push(expect_str(a, i, "path.join", s)?);
    }
    Ok(Value::str(out.to_string_lossy()))
}

fn base(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let p = expect_str(a, 0, "path.base", s)?;
    Ok(Value::str(
        Path::new(p)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
    ))
}

fn dir(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let p = expect_str(a, 0, "path.dir", s)?;
    Ok(Value::str(
        Path::new(p)
            .parent()
            .map(|n| n.to_string_lossy().to_string())
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| ".".to_string()),
    ))
}

fn ext(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let p = expect_str(a, 0, "path.ext", s)?;
    Ok(Value::str(
        Path::new(p)
            .extension()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
    ))
}

fn stem(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let p = expect_str(a, 0, "path.stem", s)?;
    Ok(Value::str(
        Path::new(p)
            .file_stem()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
    ))
}

fn with_ext(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let p = expect_str(a, 0, "path.with_ext", s)?;
    let e = expect_str(a, 1, "path.with_ext", s)?.trim_start_matches('.');
    Ok(Value::str(Path::new(p).with_extension(e).to_string_lossy()))
}

pub(crate) fn normalize_path(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

fn normalize(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let p = expect_str(a, 0, "path.normalize", s)?;
    Ok(Value::str(normalize_path(Path::new(p)).to_string_lossy()))
}

fn is_abs(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    Ok(Value::Bool(
        Path::new(expect_str(a, 0, "path.is_abs", s)?).is_absolute(),
    ))
}

fn abs(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let p = Path::new(expect_str(a, 0, "path.abs", s)?);
    if i.recording() && !p.is_absolute() {
        let _ = super::fs::rec_abs(i, p);
    }
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| runtime(format!("path.abs: {e}")).at(s))?
            .join(p)
    };
    Ok(Value::str(normalize_path(&joined).to_string_lossy()))
}

fn parts(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let p = expect_str(a, 0, "path.parts", s)?;
    Ok(Value::list(
        Path::new(p)
            .components()
            .map(|c| Value::str(c.as_os_str().to_string_lossy()))
            .collect(),
    ))
}
