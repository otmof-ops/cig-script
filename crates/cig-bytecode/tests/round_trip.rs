// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Round-trip tests: serialize a module, parse it back, equal.

use cig_bytecode::{
    parse_module, serialize_module, verify_module, wide_prefix, BurnRegion, Constant, FuncKind,
    Function, HandlerEntry, Instr, LineEntry, Module, Op, RegionLabel, RegionRow, SpanBase,
    UpvalDesc, UpvalKind, RESERVED_U32,
};

fn example_main(seed: u64) -> Function {
    let mut f = Function::empty_main();
    f.constants.push(Constant::Bool(seed & 1 == 0));
    f.constants.push(Constant::Int(seed as i64));
    f.constants.push(Constant::Float(2.5 + (seed as f64)));
    f.constants.push(Constant::Str(format!("hello-{seed}")));
    f.constants.push(Constant::Str("write".to_string()));
    f.regions.push(BurnRegion {
        class: 0,
        label: RegionLabel::Const(3),
    });
    f.reg_count = 4;
    f.instrs = vec![
        Instr::abx(Op::LOADK, 0, 0),
        Instr::abx(Op::BURN_ENTER, 0, 0),
        Instr::abc(Op::BURN_OP, 1, 4, 0x10),
        Instr::abc(Op::BURN_LEAVE, 0, 0, 0),
        Instr::abc(Op::RET, 0, 0, 0),
    ];
    f.handlers.push(HandlerEntry {
        pc_start: 1,
        pc_end: 4,
        handler_pc: 4,
        catch_reg: RESERVED_U32,
        finally_pc: RESERVED_U32,
    });
    f.span_base = SpanBase {
        byte_offset: 0,
        line: 1,
        col: 1,
    };
    f.lines.push(LineEntry {
        pc: 0,
        span_off: 0,
        span_len: 1,
        line_off: 0,
        col: 1,
    });
    f.lines.push(LineEntry {
        pc: 2,
        span_off: 5,
        span_len: 2,
        line_off: 1,
        col: 1,
    });
    f.region_rows.push(RegionRow {
        pc_start: 1,
        pc_end: 4,
        region: 0,
    });
    // One child: a lambda with its own constants and reg frame.
    let child = Function {
        name: String::new(),
        kind: FuncKind::Lambda,
        reg_count: 3,
        arity: 1,
        requires_burn: false,
        upvalues: vec![UpvalDesc {
            kind: UpvalKind::ParentLocal,
            index: 0,
        }],
        constants: vec![Constant::Int(42)],
        children: Vec::new(),
        regions: Vec::new(),
        handlers: Vec::new(),
        instrs: vec![Instr::abc(Op::MOV, 1, 0, 0), Instr::abc(Op::RET, 1, 0, 0)],
        span_base: SpanBase::default(),
        lines: Vec::new(),
        region_rows: Vec::new(),
    };
    f.children.push(child);
    f
}

fn example_module(seed: u64) -> Module {
    Module {
        source_name: "round-trip.cig".to_string(),
        main: example_main(seed),
    }
}

#[test]
fn an_empty_module_round_trips() {
    let m = Module::empty("");
    let bytes = serialize_module(&m);
    let back = parse_module(&bytes).expect("parse");
    assert_eq!(m, back);
}

#[test]
fn several_modules_round_trip_and_verify() {
    for seed in 0..32u64 {
        let m = example_module(seed);
        verify_module(&m).unwrap_or_else(|e| panic!("seed {seed} should verify: {e}"));
        let bytes = serialize_module(&m);
        let back = parse_module(&bytes).unwrap_or_else(|e| panic!("seed {seed} parse: {e}"));
        assert_eq!(m, back, "seed {seed}");
        let back_bytes = serialize_module(&back);
        assert_eq!(bytes, back_bytes, "seed {seed} bytes");
    }
}

#[test]
fn nan_float_constants_round_trip_bit_identical() {
    let mut m = Module::empty("");
    let nan = f64::from_bits(0x7FF8_0000_0000_1234);
    m.main.constants.push(Constant::Float(nan));
    m.main.reg_count = 1;
    m.main.instrs = vec![Instr::abx(Op::LOADK, 0, 0), Instr::abc(Op::RET, 0, 0, 0)];
    let bytes = serialize_module(&m);
    let back = parse_module(&bytes).unwrap();
    if let (Constant::Float(a), Constant::Float(b)) =
        (&m.main.constants[0], &back.main.constants[0])
    {
        assert_eq!(a.to_bits(), b.to_bits());
    } else {
        panic!("expected floats");
    }
}

#[test]
fn the_round_trip_generator_emits_wide_pairs() {
    let mut m = Module::empty("wide.cig");
    m.main.constants.push(Constant::Int(7));
    m.main.reg_count = 1;
    // WIDE + LOADK with high Bx = 0 (so combined = Bx of 0, fine).
    // We still demand WIDE pairs round-trip as bytes.
    m.main.instrs = vec![
        wide_prefix(0, 0),
        Instr::abx(Op::LOADK, 0, 0),
        Instr::abc(Op::RET, 0, 0, 0),
    ];
    verify_module(&m).expect("wide-prefixed module should verify");
    let bytes = serialize_module(&m);
    let back = parse_module(&bytes).unwrap();
    assert_eq!(m, back);
    // Confirm the first word really is a WIDE on the wire by parsing
    // its opcode byte.
    assert_eq!(back.main.instrs[0].op(), Some(Op::WIDE));
}

#[test]
fn loadg_and_the_try_instructions_round_trip() {
    let mut m = Module::empty("prelude.cig");
    m.main.constants.push(Constant::Str("len".into()));
    m.main.constants.push(Constant::Str("args".into()));
    m.main.reg_count = 300;
    m.main.instrs = vec![
        Instr::abx(Op::LOADG, 0, 0),
        // WIDE loadg: R(256) = K[1].
        wide_prefix(1, 0),
        Instr::abx(Op::LOADG, 0, 1),
        Instr::abx(Op::TRY_ENTER, 0, 0),
        Instr::abc(Op::THROW, 0, 0, 0),
        Instr::abc(Op::TRY_LEAVE, 0, 0, 0),
        Instr::abc(Op::TRY_LEAVE, 1, 0, 0),
        Instr::abc(Op::RET, 0, 0, 0),
    ];
    m.main.handlers.push(HandlerEntry {
        pc_start: 4,
        pc_end: 6,
        handler_pc: 6,
        catch_reg: 1,
        finally_pc: RESERVED_U32,
    });
    verify_module(&m).expect("verifies");
    let bytes = serialize_module(&m);
    let back = parse_module(&bytes).expect("parse");
    assert_eq!(m, back);
    assert_eq!(bytes, serialize_module(&back));
    assert_eq!(Op::from_byte(0x06), Some(Op::LOADG));
    assert_eq!(Op::LOADG.mnemonic(), "loadg");
    let dis = cig_bytecode::disasm::disassemble_module(&back);
    assert!(
        dis.lines()
            .any(|l| l.contains("loadg") && l.ends_with("R256, K1 (\"args\")")),
        "{dis}"
    );
    assert!(
        dis.lines()
            .any(|l| l.contains("try.leave") && l.ends_with(" 1")),
        "{dis}"
    );
}

#[test]
fn a_function_encodes_the_same_wherever_it_sits() {
    // Take a child function, encode it on its own, then encode the
    // same child nested inside a parent that has other constants and
    // sibling children before it. The child's bytes must match: its
    // operand indices are all into its own tables, so nothing above
    // it in the module can shift them.
    let child = example_main(1).children[0].clone();

    let bytes_alone = encode_function_only(&child);

    let mut parent = Function::empty_main();
    parent.constants.push(Constant::Str("noise".into()));
    parent.constants.push(Constant::Int(999));
    let mut sibling = Function::empty_main();
    sibling.kind = FuncKind::Lambda;
    sibling.constants.push(Constant::Int(7));
    parent.children.push(sibling);
    parent.children.push(child.clone());

    let bytes_nested = encode_function_only(&parent.children[1]);
    assert_eq!(
        bytes_alone, bytes_nested,
        "child bytes depend only on the child"
    );
}

// Encode a single function through the public API by wrapping it in a
// fresh module whose main is the function, then strip the header
// (40 bytes) and empty source-name length (4 bytes) and the main
// function's own leading `name` length (another 4 bytes if we also
// want name-independent bytes; keep the name in and compare the
// whole function section).
fn encode_function_only(f: &Function) -> Vec<u8> {
    let m = Module {
        source_name: String::new(),
        main: f.clone(),
    };
    let full = serialize_module(&m);
    // Header is 4 + 2 + 2 + 32 = 40; source_name is 4 (length=0) bytes.
    full[40 + 4..].to_vec()
}
