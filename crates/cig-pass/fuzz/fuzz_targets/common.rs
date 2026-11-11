// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! What the fuzz targets share: one fixed key, one fixed clock.

#![allow(dead_code)]

pub const ISSUER: [u8; 16] = [9; 16];
pub const NOW: i64 = 1_800_000_000_000;

pub fn key(kind: &str, epoch: u64) -> [u8; 32] {
    blake3::derive_key("cig-pass fuzz", format!("{kind}:{epoch}").as_bytes())
}

pub fn hash_key() -> [u8; 32] {
    *blake3::keyed_hash(&key("check", u64::MAX), b"cigpass1-hash-key").as_bytes()
}

pub fn ctx() -> cig_pass::MemCtx {
    let mut c = cig_pass::MemCtx::with_local(ISSUER, |k, e| Some(key(k, e)));
    c.now = Some(cig_pass::Now {
        wall_ms: NOW,
        mono: None,
    });
    c.allow_signer([3; 32]);
    c.set_env(b"HOME", Some(b"/home/fuzz"));
    c
}

pub fn issue(kind: &str, subject: &[u8], inputs: Vec<cig_pass::Input>, result: &[u8]) -> cig_pass::Pass {
    let draft = cig_pass::Draft {
        kind: kind.to_string(),
        subject: subject.to_vec(),
        inputs,
        result: result.to_vec(),
        epoch: 0,
        ledger_seq: 0,
        issued_ms: NOW,
        mono: None,
        flags: 0,
    };
    cig_pass::issue_local(&draft, ISSUER, &key(kind, 0), &hash_key()).expect("issue")
}
