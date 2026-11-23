// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The section's reader never panics on any body the fuzzer makes up,
//! and a body it takes is the one encoding of its section: written back,
//! it is the same bytes. Version 2 (interface 2): the body and resume
//! tables, the rule, and the extra footer fields are covered by the
//! same round trip.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let tables = cig_native::Tables::parse(data);
    let section = cig_native::Section::parse(data);
    assert_eq!(tables.is_ok(), section.is_ok());
    if let Ok(s) = section {
        assert_eq!(s.to_bytes().as_deref(), Ok(data));
    }
});
