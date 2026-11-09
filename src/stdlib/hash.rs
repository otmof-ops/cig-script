// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! Content hashing.

use super::b;
use crate::diagnostics::{runtime, Diagnostic};
use crate::interp::Interp;
use crate::syntax::span::Span;
use crate::value::{expect_str, Builtin, Effect, Value};
use sha2::{Digest, Sha256};

pub static TABLE: &[Builtin] = &[
    b("hash.sha256", Effect::Pure, 1, Some(1), sha256),
    b("hash.sha256_file", Effect::Read, 1, Some(1), sha256_file),
    b("hash.short", Effect::Pure, 1, Some(2), short),
];

fn sha256(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let text = expect_str(a, 0, "hash.sha256", s)?;
    Ok(Value::str(hex::encode(Sha256::digest(text.as_bytes()))))
}

fn sha256_file(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let p = expect_str(a, 0, "hash.sha256_file", s)?;
    if let Some(bytes) = super::fs::ghost_read(i, "hash.sha256_file", std::path::Path::new(p), s)? {
        return Ok(Value::str(hex::encode(Sha256::digest(&bytes))));
    }
    if i.recording() {
        let bytes = super::fs::rec_read(i, "hash.sha256_file", std::path::Path::new(p), s)?;
        return Ok(Value::str(hex::encode(Sha256::digest(&bytes))));
    }
    crate::burn::journal::sha256_file(std::path::Path::new(p))
        .map(Value::str)
        .map_err(|e| {
            runtime(format!(
                "hash.sha256_file: {p}: {}",
                super::fs::describe_io(&e)
            ))
            .at(s)
        })
}

/// The first `n` (default 8) hex characters of the SHA-256 of a string:
/// handy for stable file names.
fn short(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let text = expect_str(a, 0, "hash.short", s)?;
    let n = match a.get(1) {
        Some(_) => crate::value::expect_int(a, 1, "hash.short", s)?.clamp(1, 64) as usize,
        None => 8,
    };
    let full = hex::encode(Sha256::digest(text.as_bytes()));
    Ok(Value::str(&full[..n]))
}
