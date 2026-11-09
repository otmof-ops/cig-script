// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! Narrowing never widens: each caveat appended keeps the ones before it,
//! round-trips through the encoding, and never makes an invalid pass valid.

#![no_main]

#[path = "common.rs"]
mod common;

use libfuzzer_sys::arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

fn caveat(u: &mut Unstructured<'_>) -> Option<cig_pass::Caveat> {
    use cig_pass::Caveat;
    Some(match u.int_in_range(0u8..=6).ok()? {
        0 => Caveat::Until(u.arbitrary().ok()?),
        1 => Caveat::Once,
        2 => Caveat::Max(u.arbitrary().ok()?),
        3 => Caveat::Boxed,
        4 => Caveat::Agent(u.arbitrary::<String>().ok()?),
        5 => Caveat::Args(vec![u.arbitrary::<String>().ok()?]),
        _ => Caveat::Needs("human-yes".to_string()),
    })
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);
    let mut p = common::issue("check", b"fuzz", Vec::new(), b"");
    let c = common::ctx();
    let mut was_valid = cig_pass::verify(&p, &c).is_ok();
    while let Some(cv) = caveat(&mut u) {
        let before = p.caveats.clone();
        match cig_pass::attenuate(&p, cv) {
            Ok(q) => {
                assert_eq!(q.caveats.len(), before.len() + 1);
                assert_eq!(&q.caveats[..before.len()], &before[..]);
                assert!(cig_pass::Pass::decode(&q.encode()).is_ok_and(|d| d == q));
                let valid = cig_pass::verify(&q, &c).is_ok();
                assert!(!valid || was_valid, "narrowing made a pass valid");
                was_valid = valid;
                p = q;
            }
            Err(_) => break,
        }
    }
});
