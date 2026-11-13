// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Bounds the verifier holds: `interp`'s part range, `call` writing results
//! out of the frame, closure captures never checked, no frame-size or
//! nesting limit. One test per hole, plus a table-driven walk of
//! every register-naming opcode.

use cig_bytecode::{
    parse_module, serialize_module, verify_module, BurnRegion, Constant, FuncKind, Function, Instr,
    Module, Op, RegionLabel, UpvalDesc, UpvalKind, VerifyError, MAX_FUNCTION_DEPTH, MAX_REG_COUNT,
    RESERVED_U32,
};

fn tiny_main() -> Module {
    let mut m = Module::empty("t.cig");
    m.main.reg_count = 4;
    m.main.instrs = vec![
        Instr::abc(Op::LOADNULL, 0, 0, 0),
        Instr::abc(Op::RET, 0, 0, 0),
    ];
    m
}

#[test]
fn an_interp_whose_parts_run_past_the_frame_is_refused() {
    // interp A, B, C: result R(A), parts R(B..B+C+1).
    // Pick A small (in range), C small (in range), B past the frame.
    let mut m = tiny_main();
    m.main.reg_count = 4;
    m.main.instrs = vec![
        Instr::abc(
            Op::INTERP,
            0, /* A */
            3, /* B */
            2, /* C: parts = 3,4,5 */
        ),
        Instr::abc(Op::RET, 0, 0, 0),
    ];
    // Parts touch R(3), R(4), R(5); R(4) and R(5) are past reg_count=4.
    assert!(
        matches!(
            verify_module(&m).unwrap_err(),
            VerifyError::RegisterRangeOutOfRange { .. }
        ),
        "got {:?}",
        verify_module(&m).unwrap_err()
    );
}

#[test]
fn a_call_whose_results_run_past_the_frame_is_refused() {
    // call A, B, C: args R(A+1..A+1+B), results R(A..A+C+1).
    // Keep B small but make C large so results run past the frame.
    let mut m = tiny_main();
    m.main.reg_count = 4;
    m.main.instrs = vec![
        Instr::abc(
            Op::CALL,
            0,
            1,  /* B: one arg */
            10, /* C: 11 results */
        ),
        Instr::abc(Op::RET, 0, 0, 0),
    ];
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::RegisterRangeOutOfRange { .. } | VerifyError::CallArityTooHigh { .. }
    ));
}

/// Table-driven: every opcode that names a register range is built
/// with the base register exactly past the function's frame, and the
/// verifier must refuse.
#[test]
fn every_register_an_opcode_names_is_bounded() {
    // (name, instruction, expected_min_reg_count_for_success).
    // The template has reg_count = 1, so an operand indexing R(1) or
    // R(5) must trip. The last instruction is always RET R0.
    //
    // Each row below encodes the offending opcode with a base that,
    // combined with the operand's role, writes or reads at least R(1).
    let cases: &[(&str, Instr)] = &[
        // Reg operand kinds (A as single register):
        ("mov A", Instr::abc(Op::MOV, 5, 0, 0)),
        ("mov B", Instr::abc(Op::MOV, 0, 5, 0)),
        ("loadnull A", Instr::abc(Op::LOADNULL, 5, 0, 0)),
        ("loadk A", Instr::abx(Op::LOADK, 5, 0)),
        ("loadbool A", Instr::abc(Op::LOADBOOL, 5, 1, 0)),
        ("loadint A", Instr::asbx(Op::LOADINT, 5, 0)),
        ("loadg A", Instr::abx(Op::LOADG, 5, 0)),
        ("neg A", Instr::abc(Op::NEG, 5, 0, 0)),
        ("add A", Instr::abc(Op::ADD, 5, 0, 0)),
        ("add B", Instr::abc(Op::ADD, 0, 5, 0)),
        ("add C", Instr::abc(Op::ADD, 0, 0, 5)),
        ("not A", Instr::abc(Op::NOT, 5, 0, 0)),
        ("eq A", Instr::abc(Op::EQ, 5, 0, 0)),
        ("eq B", Instr::abc(Op::EQ, 0, 5, 0)),
        ("eq C", Instr::abc(Op::EQ, 0, 0, 5)),
        ("test A", Instr::asbx(Op::TEST, 5, 0)),
        ("testn A", Instr::asbx(Op::TESTN, 5, 0)),
        ("testset A", Instr::abc(Op::TESTSET, 5, 0, 0)),
        ("ret A", Instr::abc(Op::RET, 5, 0, 0)),
        ("iterprep A", Instr::abc(Op::ITERPREP, 5, 0, 0)),
        ("newlist A", Instr::abc(Op::NEWLIST, 5, 0, 0)),
        ("newmap A", Instr::abc(Op::NEWMAP, 5, 0, 0)),
        ("getindex A", Instr::abc(Op::GETINDEX, 5, 0, 0)),
        ("getindex B", Instr::abc(Op::GETINDEX, 0, 5, 0)),
        ("getindex C", Instr::abc(Op::GETINDEX, 0, 0, 5)),
        ("setindex A", Instr::abc(Op::SETINDEX, 5, 0, 0)),
        ("appendlist A", Instr::abc(Op::APPENDLIST, 5, 0, 0)),
        ("appendlist B", Instr::abc(Op::APPENDLIST, 0, 5, 0)),
        ("insertmap B", Instr::abc(Op::INSERTMAP, 0, 5, 0)),
        ("closure A", Instr::abx(Op::CLOSURE, 5, 0)),
        ("getup A", Instr::abc(Op::GETUP, 5, 0, 0)),
        ("setup B", Instr::abc(Op::SETUP, 0, 5, 0)),
        ("cellnew A", Instr::abc(Op::CELLNEW, 5, 0, 0)),
        ("cellget A", Instr::abc(Op::CELLGET, 5, 0, 0)),
        ("cellset B", Instr::abc(Op::CELLSET, 0, 5, 0)),
        ("throw A", Instr::abc(Op::THROW, 5, 0, 0)),
        // Register-range operand kinds:
        ("call base", Instr::abc(Op::CALL, 5, 0, 0)),
        ("call args", Instr::abc(Op::CALL, 0, 5, 0)),
        ("call results", Instr::abc(Op::CALL, 0, 0, 5)),
        ("callb args", Instr::abc(Op::CALLB, 0, 0, 5)),
        ("callm args", Instr::abc(Op::CALLM, 0, 0, 5)),
        ("tailcall", Instr::abc(Op::TAILCALL, 0, 5, 0)),
        ("pack count", Instr::abc(Op::PACK, 0, 5, 0)),
        ("exhale count", Instr::abc(Op::EXHALE, 0, 5, 0)),
        ("chain.make count", Instr::abc(Op::CHAIN_MAKE, 0, 5, 0)),
        // The named interp bug: B past the frame.
        ("interp B", Instr::abc(Op::INTERP, 0, 5, 0)),
        // Burn op: packed C = (argc << 4) | kind.
        ("burn.op args", Instr::abc(Op::BURN_OP, 0, 0, 5u8 << 4)),
        // FORPREP/FORLOOP/ITERNEXT: A+4 must fit.
        ("forprep A+4", Instr::asbx(Op::FORPREP, 1, 0)),
        ("forloop A+4", Instr::asbx(Op::FORLOOP, 1, 0)),
        ("iternext A+4", Instr::asbx(Op::ITERNEXT, 1, 0)),
        ("iterprep A+4", Instr::abc(Op::ITERPREP, 1, 0, 0)),
    ];

    // GETFIELD/SETFIELD need a Str constant for their StrConst slot;
    // build a dedicated template that includes one. CALLM also needs
    // a Str. The straightforward way: include a string constant in
    // every module, and let the non-string tests still work.
    for (name, instr) in cases {
        let mut m = Module::empty("tdt.cig");
        m.main.reg_count = 1;
        m.main.constants.push(Constant::Str("x".into()));
        // For burn.op we need a region + a region row, OR the
        // function marked requires_burn. Easiest is requires_burn for
        // this test; it does not matter for a register-bounds trip.
        if *name == "burn.op args" {
            m.main.requires_burn = true;
            m.main.kind = FuncKind::Pull;
        }
        m.main.instrs = vec![*instr, Instr::abc(Op::RET, 0, 0, 0)];
        let err = verify_module(&m);
        assert!(err.is_err(), "case {name}: expected a refusal, got {err:?}");
    }
}

#[test]
fn a_capture_of_a_parent_register_out_of_range_is_refused() {
    let mut m = Module::empty("");
    m.main.reg_count = 2;
    m.main.instrs = vec![Instr::abx(Op::CLOSURE, 0, 0), Instr::abc(Op::RET, 0, 0, 0)];
    let mut child = Function::empty_main();
    child.kind = FuncKind::Lambda;
    // Parent's reg_count is 2; this captures index 5, out of range.
    child.upvalues.push(UpvalDesc {
        kind: UpvalKind::ParentLocal,
        index: 5,
    });
    m.main.children.push(child);
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::UpvalueCaptureOutOfRange { .. }
    ));
}

#[test]
fn a_capture_of_a_parent_upvalue_out_of_range_is_refused() {
    let mut m = Module::empty("");
    m.main.reg_count = 1;
    m.main.instrs = vec![Instr::abx(Op::CLOSURE, 0, 0), Instr::abc(Op::RET, 0, 0, 0)];
    // Main has no upvalues. A child that claims to capture the
    // parent's upvalue 0 must be refused.
    let mut child = Function::empty_main();
    child.kind = FuncKind::Lambda;
    child.upvalues.push(UpvalDesc {
        kind: UpvalKind::ParentUpvalue,
        index: 0,
    });
    m.main.children.push(child);
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::UpvalueCaptureOutOfRange { .. }
    ));
}

#[test]
fn the_top_level_function_has_no_upvalues() {
    let mut m = Module::empty("");
    m.main.upvalues.push(UpvalDesc {
        kind: UpvalKind::ParentLocal,
        index: 0,
    });
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::UpvalueCaptureOutOfRange { .. }
    ));
}

#[test]
fn a_frame_larger_than_the_register_operand_is_refused() {
    let mut m = Module::empty("");
    m.main.reg_count = MAX_REG_COUNT + 1;
    m.main.instrs = vec![
        Instr::abc(Op::LOADNULL, 0, 0, 0),
        Instr::abc(Op::RET, 0, 0, 0),
    ];
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::FrameTooLarge { .. }
    ));
}

#[test]
fn a_module_nested_past_the_limit_is_refused_and_does_not_overflow() {
    // A chain of `MAX_FUNCTION_DEPTH + 10` lambdas, one inside the
    // other, serialized (the writer does not recurse) and parsed: the
    // directory's parents give each function's depth.
    let over = MAX_FUNCTION_DEPTH + 10;
    let mut bottom = Function::empty_main();
    bottom.kind = FuncKind::Lambda;
    for _ in 0..over {
        let mut parent = Function::empty_main();
        parent.kind = FuncKind::Lambda;
        parent.children.push(bottom);
        bottom = parent;
    }
    let bytes = serialize_module(&Module {
        source_name: String::new(),
        main: bottom,
    });

    let err = parse_module(&bytes).unwrap_err();
    assert!(
        matches!(err, cig_bytecode::ParseError::TooDeep { .. }),
        "expected TooDeep; got {err:?}"
    );

    // Also prove the verifier catches an in-memory depth excess. Build
    // a module with MAX+2 nesting in memory; the test stays well below
    // Rust's default stack, so this check does not depend on the
    // parser's depth limit. Assembled nest keeps working through
    // depths that a real compiler would never produce.
    let mut bottom = Function::empty_main();
    bottom.kind = FuncKind::Lambda;
    for _ in 0..(MAX_FUNCTION_DEPTH + 2) {
        let mut parent = Function::empty_main();
        parent.kind = FuncKind::Lambda;
        parent.children.push(bottom);
        bottom = parent;
    }
    let mut top = Function::empty_main();
    top.children.push(bottom);
    let m = Module {
        source_name: String::new(),
        main: top,
    };
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::NestedTooDeep { .. }
    ));
}

/// `RESERVED_U32` is still an exported sentinel and shows up in
/// several tests; keep it in the import so it stays reachable.
#[test]
fn reserved_sentinel_stays_exported() {
    assert_eq!(RESERVED_U32, u32::MAX);
    let _ = BurnRegion {
        class: 0,
        label: RegionLabel::None,
    };
}
