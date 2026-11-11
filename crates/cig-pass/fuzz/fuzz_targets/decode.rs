// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Arbitrary bytes into `Pass::decode` never panic, and anything that
//! decodes re-encodes to the very same bytes (one encoding per pass). The
//! same bytes go through the token form too.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(p) = cig_pass::Pass::decode(data) {
        assert_eq!(p.encode(), data, "decode accepted a non-canonical encoding");
        let q = cig_pass::Pass::from_token(&p.token()).expect("a decoded pass's token decodes");
        assert_eq!(q, p);
    }
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = cig_pass::Pass::from_token(s);
        let _ = cig_pass::Pass::from_token(&format!("cigpass1.{s}"));
    }
});
