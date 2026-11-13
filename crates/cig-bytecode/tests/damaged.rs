// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Damaged-module tests: flip every bit of every byte of a sample,
//! truncate at every offset, and check that the parser or the verifier
//! rejects it with an error and never panics. The VM wraps either in
//! `E1050`; this test catches the panic-free rule on the low-level
//! side.
//!
//! The issue-named test is `a_program_with_damaged_bytecode_is_e1050`.

use cig_bytecode::{
    parse_module, serialize_module, verify_module, BurnRegion, Constant, Function, Instr, Module,
    Op, RegionLabel, RegionRow,
};

fn sample_bytes() -> Vec<u8> {
    let mut m = Module::empty("damaged.cig");
    m.main.constants.push(Constant::Str("write".to_string()));
    m.main.regions.push(BurnRegion {
        class: 0,
        label: RegionLabel::None,
    });
    m.main.region_rows.push(RegionRow {
        pc_start: 0,
        pc_end: 3,
        region: 0,
    });
    m.main.reg_count = 4;
    m.main.instrs = vec![
        Instr::abx(Op::BURN_ENTER, 0, 0),
        Instr::abc(Op::BURN_OP, 1, 0, 0x10),
        Instr::abc(Op::BURN_LEAVE, 0, 0, 0),
        Instr::abc(Op::LOADNULL, 0, 0, 0),
        Instr::abc(Op::RET, 0, 0, 0),
    ];
    // A child to widen the surface.
    let mut child = Function::empty_main();
    child.kind = cig_bytecode::FuncKind::Lambda;
    child.name = "lambda".to_string();
    child.reg_count = 2;
    child.arity = 1;
    child.instrs = vec![Instr::abc(Op::MOV, 1, 0, 0), Instr::abc(Op::RET, 1, 0, 0)];
    m.main.children.push(child);
    verify_module(&m).unwrap();
    serialize_module(&m)
}

#[test]
fn a_program_with_damaged_bytecode_is_e1050() {
    let bytes = sample_bytes();
    let m = parse_module(&bytes).expect("fresh bytes parse");
    verify_module(&m).expect("fresh bytes verify");

    for cut in 0..bytes.len() {
        let _ = parse_module(&bytes[..cut]);
    }

    for i in 0..bytes.len() {
        for bit in 0..8 {
            let mut copy = bytes.clone();
            copy[i] ^= 1 << bit;
            if copy == bytes {
                continue;
            }
            if let Ok(m) = parse_module(&copy) {
                let _ = verify_module(&m);
            }
        }
    }
}

#[test]
fn a_bad_jump_target_is_refused_by_the_verifier() {
    let mut m = Module::empty("");
    m.main.reg_count = 2;
    m.main.instrs = vec![Instr::asbx(Op::JMP, 0, 500), Instr::abc(Op::RET, 0, 0, 0)];
    assert!(verify_module(&m).is_err());
    let bytes = serialize_module(&m);
    let parsed = parse_module(&bytes).expect("parse fine");
    assert!(verify_module(&parsed).is_err());
}

#[test]
fn an_unbalanced_burn_leave_is_refused_by_the_verifier() {
    let mut m = Module::empty("");
    m.main.reg_count = 2;
    m.main.instrs = vec![
        Instr::abc(Op::BURN_LEAVE, 0, 0, 0),
        Instr::abc(Op::RET, 0, 0, 0),
    ];
    assert!(verify_module(&m).is_err());
}

#[test]
fn an_appended_byte_is_rejected_by_the_integrity_hash() {
    let mut bytes = sample_bytes();
    bytes.push(0);
    let err = parse_module(&bytes).unwrap_err();
    assert!(
        matches!(
            err,
            cig_bytecode::ParseError::BadIntegrity | cig_bytecode::ParseError::TrailingBytes { .. }
        ),
        "got {err:?}"
    );
}

#[test]
fn a_prepended_byte_is_rejected_at_the_magic() {
    let mut bytes = sample_bytes();
    bytes.insert(0, b'X');
    let err = parse_module(&bytes).unwrap_err();
    assert!(
        matches!(err, cig_bytecode::ParseError::BadMagic),
        "got {err:?}"
    );
}

#[test]
fn a_bumped_format_version_is_refused() {
    let mut bytes = sample_bytes();
    bytes[4] = 0xFF;
    bytes[5] = 0xFF;
    let err = parse_module(&bytes).unwrap_err();
    assert!(
        matches!(
            err,
            cig_bytecode::ParseError::BadVersion { .. } | cig_bytecode::ParseError::BadIntegrity
        ),
        "got {err:?}"
    );
}

/// No byte of a module goes unchecked by `parse_module`: the magic, the
/// version and the flags are compared, the stored hash and every byte after
/// the header are covered by the BLAKE3. A start relies on this to check a
/// program's `bytecode` section without hashing it a second time.
#[test]
fn every_changed_byte_of_a_module_is_refused_by_the_parser() {
    let bytes = sample_bytes();
    for i in 0..bytes.len() {
        for bit in 0..8 {
            let mut copy = bytes.clone();
            copy[i] ^= 1 << bit;
            let err = parse_module(&copy).expect_err("a changed byte must not parse");
            let want = match i {
                0..=3 => matches!(err, cig_bytecode::ParseError::BadMagic),
                4..=5 => matches!(err, cig_bytecode::ParseError::BadVersion { .. }),
                6..=7 => matches!(err, cig_bytecode::ParseError::BadFlags { .. }),
                _ => matches!(err, cig_bytecode::ParseError::BadIntegrity),
            };
            assert!(want, "byte {i} bit {bit}: {err:?}");
        }
    }
}

#[test]
fn a_header_flag_is_refused() {
    let mut bytes = sample_bytes();
    bytes[7] = 0x80;
    assert_eq!(
        parse_module(&bytes).unwrap_err(),
        cig_bytecode::ParseError::BadFlags { flags: 0x8000 }
    );
}
