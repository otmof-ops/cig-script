// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! If the parser accepts a module, the verifier never panics on it.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(m) = cig_bytecode::parse_module(data) {
        let _ = cig_bytecode::verify_module(&m);
    }
});
