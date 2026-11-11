// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Nothing the fuzzer makes up verifies: under a fixed key, only the one
//! real pass does. And `verify` never panics.

#![no_main]

#[path = "common.rs"]
mod common;

use libfuzzer_sys::fuzz_target;
use std::sync::OnceLock;

fn real() -> &'static cig_pass::Pass {
    static P: OnceLock<cig_pass::Pass> = OnceLock::new();
    P.get_or_init(|| {
        let mut rec = cig_pass::Recorder::new();
        rec.env(b"HOME", Some(b"/home/fuzz"));
        common::issue("check", b"fuzz", rec.finish().expect("inputs"), b"ok")
    })
}

fuzz_target!(|data: &[u8]| {
    let ctx = common::ctx();
    if let Ok(p) = cig_pass::Pass::decode(data) {
        if cig_pass::verify(&p, &ctx).is_ok() {
            assert_eq!(p.encode(), real().encode(), "the fuzzer produced a pass that verifies");
        }
    }
    let _ = cig_pass::verify(real(), &ctx);
});
