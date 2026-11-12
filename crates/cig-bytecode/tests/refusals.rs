// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! One refusal test per verifier rule, plus the issue-named
//! `the_bytecode_verifier_refuses_a_burn_op_outside_its_region`.

use cig_bytecode::{
    verify_module, wide_prefix, BurnRegion, Constant, FuncKind, Function, HandlerEntry, Instr,
    LineEntry, Module, Op, RegionRow, SpanBase, VerifyError, RESERVED_U32,
};

fn base() -> Module {
    let mut m = Module::empty("t.cig");
    m.main.constants.push(Constant::Str("write".to_string()));
    m.main.regions.push(BurnRegion {
        class: 0,
        label: RESERVED_U32,
    });
    m.main.reg_count = 8;
    m.main.instrs = vec![
        Instr::abc(Op::LOADNULL, 0, 0, 0),
        Instr::abc(Op::RET, 0, 0, 0),
    ];
    m
}

#[test]
fn the_bytecode_verifier_refuses_a_burn_op_outside_its_region() {
    let mut m = base();
    m.main.instrs = vec![
        Instr::abc(Op::BURN_OP, 1, 0, 0x10),
        Instr::abc(Op::LOADNULL, 0, 0, 0),
        Instr::abc(Op::RET, 0, 0, 0),
    ];
    let err = verify_module(&m).unwrap_err();
    assert!(
        matches!(err, VerifyError::BurnOpOutsideRegion { .. }),
        "got {err:?}"
    );
    assert_eq!(err.rule(), "burn-op-outside-region");
    assert_eq!(err.pc(), Some(0));
    assert_eq!(err.func(), Some(0));
}

#[test]
fn a_burn_op_inside_a_lexical_region_passes() {
    let mut m = base();
    m.main.instrs = vec![
        Instr::abx(Op::BURN_ENTER, 0, 0),
        Instr::abc(Op::BURN_OP, 1, 0, 0x10),
        Instr::abc(Op::BURN_LEAVE, 0, 0, 0),
        Instr::abc(Op::LOADNULL, 0, 0, 0),
        Instr::abc(Op::RET, 0, 0, 0),
    ];
    verify_module(&m).expect("burn.op inside a region should verify");
}

#[test]
fn a_burn_op_inside_a_region_row_passes_when_function_lacks_lexical_wrap() {
    let mut m = base();
    m.main.region_rows = vec![RegionRow {
        pc_start: 0,
        pc_end: 1,
        region: 0,
    }];
    m.main.instrs = vec![
        Instr::abc(Op::BURN_OP, 1, 0, 0x10),
        Instr::abc(Op::LOADNULL, 0, 0, 0),
        Instr::abc(Op::RET, 0, 0, 0),
    ];
    verify_module(&m).expect("burn.op inside a region row should verify");
}

#[test]
fn register_out_of_range_is_refused() {
    let mut m = base();
    m.main.reg_count = 2;
    m.main.instrs = vec![Instr::abc(Op::MOV, 10, 0, 0), Instr::abc(Op::RET, 0, 0, 0)];
    let err = verify_module(&m).unwrap_err();
    assert!(
        matches!(
            err,
            VerifyError::RegisterOutOfRange {
                reg: 10,
                max: 2,
                ..
            }
        ),
        "got {err:?}"
    );
}

#[test]
fn const_out_of_range_is_refused() {
    let mut m = base();
    m.main.instrs = vec![Instr::abx(Op::LOADK, 0, 99), Instr::abc(Op::RET, 0, 0, 0)];
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::ConstOutOfRange { .. }
    ));
}

#[test]
fn a_non_string_constant_where_a_string_was_wanted_is_refused() {
    let mut m = base();
    m.main.constants.clear();
    m.main.constants.push(Constant::Int(7));
    m.main.instrs = vec![
        Instr::abc(Op::GETFIELD, 0, 0, 0),
        Instr::abc(Op::RET, 0, 0, 0),
    ];
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::ConstNotString { .. }
    ));
}

#[test]
fn jump_out_of_range_is_refused() {
    let mut m = base();
    m.main.instrs = vec![Instr::asbx(Op::JMP, 0, 50), Instr::abc(Op::RET, 0, 0, 0)];
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::JumpOutOfRange { .. }
    ));
}

#[test]
fn target_equal_to_n_instr_is_refused() {
    // JMP whose target is exactly `n_instr` would run off the end.
    // Our function has 2 instructions; a JMP at pc 0 with offset 1
    // targets pc 2, which equals n_instr: refused.
    let mut m = base();
    m.main.instrs = vec![Instr::asbx(Op::JMP, 0, 1), Instr::abc(Op::RET, 0, 0, 0)];
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::JumpOutOfRange { .. }
    ));
}

#[test]
fn a_negative_target_is_refused() {
    let mut m = base();
    m.main.instrs = vec![Instr::asbx(Op::JMP, 0, -5), Instr::abc(Op::RET, 0, 0, 0)];
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::JumpOutOfRange { .. }
    ));
}

#[test]
fn a_backward_fused_jump_verifies() {
    // if R(0) != R(1) { goto pc-2 } at pc=2 gives target pc = 2+1-2 = 1
    // (the LT at pc 1), which is a valid first-word pc inside the
    // body of the function. Replace with a cleaner setup: pre-load
    // two regs, compare, jump back.
    // The target charges the step budget, so the cycle is charged.
    let mut m = base();
    m.main.reg_count = 2;
    m.main.instrs = vec![
        Instr::abc(Op::LOADNULL, 0, 0, 0), // pc 0
        Instr::abx(Op::STEPS, 0, 1),       // pc 1
        Instr {
            // pc 2: NEJ R0, R1, sC = -2  (target = 2+1-2 = 1)
            raw: (Op::NEJ as u32) | (1u32 << 16) | ((((-2_i8) as u8) as u32) << 24),
        },
        Instr::abc(Op::RET, 0, 0, 0), // pc 3
    ];
    verify_module(&m).expect("backward fused jump should verify");
}

#[test]
fn a_jump_to_the_second_word_of_a_pair_is_refused() {
    // Build: WIDE, LOADK, JMP sBx=0 (targets pc 3), RET.
    // Flip the JMP's offset so it targets pc 1 (the second word of
    // the WIDE pair).
    let mut m = base();
    m.main.constants.push(Constant::Int(0));
    m.main.reg_count = 1;
    m.main.instrs = vec![
        wide_prefix(0, 0),            // pc 0
        Instr::abx(Op::LOADK, 0, 0),  // pc 1 (second word)
        Instr::asbx(Op::JMP, 0, -2),  // pc 2 targets pc 1
        Instr::abc(Op::RET, 0, 0, 0), // pc 3
    ];
    let err = verify_module(&m).unwrap_err();
    assert!(
        matches!(
            err,
            VerifyError::MidPairReference {
                at: "jump.target",
                ..
            }
        ),
        "got {err:?}"
    );
}

#[test]
fn a_handler_pc_on_the_second_word_is_refused() {
    let mut m = base();
    m.main.reg_count = 1;
    m.main.instrs = vec![
        wide_prefix(0, 0),                 // pc 0
        Instr::abc(Op::LOADNULL, 0, 0, 0), // pc 1 (second word)
        Instr::abc(Op::RET, 0, 0, 0),      // pc 2
    ];
    m.main.handlers = vec![HandlerEntry {
        pc_start: 0,
        pc_end: 2,
        handler_pc: 1, // second word of the wide pair
        catch_reg: RESERVED_U32,
        finally_pc: RESERVED_U32,
    }];
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::MidPairReference {
            at: "handler.handler_pc",
            ..
        }
    ));
}

#[test]
fn a_wide_register_out_of_range_is_refused() {
    let mut m = base();
    m.main.reg_count = 2;
    // WIDE sets A high byte = 1, then MOV A, B, _ sees A = 0x0100 = 256.
    m.main.instrs = vec![
        wide_prefix(1, 0),
        Instr::abc(Op::MOV, 0, 0, 0),
        Instr::abc(Op::RET, 0, 0, 0),
    ];
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::RegisterOutOfRange { .. }
    ));
}

#[test]
fn a_wide_const_out_of_range_is_refused() {
    let mut m = base();
    m.main.reg_count = 1;
    m.main.constants.clear();
    m.main.constants.push(Constant::Int(0));
    // WIDE Bx high 16 = 0x0001, next LOADK Bx = 0x0000 -> combined 0x1_0000.
    m.main.instrs = vec![
        wide_prefix(0, 0x0001),
        Instr::abx(Op::LOADK, 0, 0),
        Instr::abc(Op::RET, 0, 0, 0),
    ];
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::ConstOutOfRange { .. }
    ));
}

#[test]
fn a_wide_jump_target_is_checked() {
    // WIDE + JMP with combined signed sBx = huge positive => target
    // past the end.
    let mut m = base();
    // 4 words total. WIDE+JMP occupies pc 0..=1; we need enough
    // instrs that the target computation runs cleanly.
    m.main.instrs = vec![
        wide_prefix(0, 0x7FFF),       // pc 0, Bx high 16 = max positive
        Instr::asbx(Op::JMP, 0, 0),   // pc 1 (second word)
        Instr::abc(Op::RET, 0, 0, 0), // pc 2
    ];
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::JumpOutOfRange { .. }
    ));
}

#[test]
fn func_out_of_range_is_refused() {
    let mut m = base();
    m.main.instrs = vec![Instr::abx(Op::CLOSURE, 0, 99), Instr::abc(Op::RET, 0, 0, 0)];
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::FuncOutOfRange { .. }
    ));
}

#[test]
fn upvalue_out_of_range_is_refused() {
    let mut m = base();
    m.main.instrs = vec![Instr::abc(Op::GETUP, 0, 0, 0), Instr::abc(Op::RET, 0, 0, 0)];
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::UpvalueOutOfRange { .. }
    ));
}

#[test]
fn handler_out_of_range_is_refused() {
    let mut m = base();
    m.main.instrs = vec![
        Instr::abx(Op::TRY_ENTER, 0, 5),
        Instr::abc(Op::RET, 0, 0, 0),
    ];
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::HandlerOutOfRange { .. }
    ));
}

#[test]
fn handler_with_a_bad_range_is_refused() {
    let mut m = base();
    m.main.handlers.push(HandlerEntry {
        pc_start: 0,
        pc_end: 999,
        handler_pc: 0,
        catch_reg: RESERVED_U32,
        finally_pc: RESERVED_U32,
    });
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::HandlerBadRange { .. }
    ));
}

#[test]
fn interleaved_handlers_are_refused() {
    let mut m = base();
    m.main.instrs = vec![
        Instr::abc(Op::LOADNULL, 0, 0, 0),
        Instr::abc(Op::LOADNULL, 0, 0, 0),
        Instr::abc(Op::LOADNULL, 0, 0, 0),
        Instr::abc(Op::LOADNULL, 0, 0, 0),
        Instr::abc(Op::RET, 0, 0, 0),
    ];
    m.main.handlers = vec![
        HandlerEntry {
            pc_start: 0,
            pc_end: 3,
            handler_pc: 4,
            catch_reg: RESERVED_U32,
            finally_pc: RESERVED_U32,
        },
        HandlerEntry {
            pc_start: 2,
            pc_end: 5,
            handler_pc: 4,
            catch_reg: RESERVED_U32,
            finally_pc: RESERVED_U32,
        },
    ];
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::HandlerRangesInterleave { .. }
    ));
}

#[test]
fn burn_enter_without_leave_is_refused() {
    let mut m = base();
    m.main.instrs = vec![
        Instr::abx(Op::BURN_ENTER, 0, 0),
        Instr::abc(Op::LOADNULL, 0, 0, 0),
        Instr::abc(Op::RET, 0, 0, 0),
    ];
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::BurnUnbalanced { .. }
    ));
}

#[test]
fn burn_leave_without_enter_is_refused() {
    let mut m = base();
    m.main.instrs = vec![
        Instr::abc(Op::BURN_LEAVE, 0, 0, 0),
        Instr::abc(Op::RET, 0, 0, 0),
    ];
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::BurnUnbalanced { .. }
    ));
}

#[test]
fn region_bad_range_is_refused() {
    let mut m = base();
    m.main.region_rows.push(RegionRow {
        pc_start: 10,
        pc_end: 20,
        region: 0,
    });
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::RegionBadRange { .. }
    ));
}

#[test]
fn region_out_of_range_is_refused() {
    let mut m = base();
    m.main.region_rows.push(RegionRow {
        pc_start: 0,
        pc_end: 1,
        region: 42,
    });
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::RegionOutOfRange { .. }
    ));
}

#[test]
fn call_arity_overflowing_the_frame_is_refused() {
    let mut m = base();
    m.main.reg_count = 4;
    m.main.instrs = vec![Instr::abc(Op::CALL, 2, 20, 0), Instr::abc(Op::RET, 0, 0, 0)];
    let err = verify_module(&m).unwrap_err();
    assert!(
        matches!(
            err,
            VerifyError::CallArityTooHigh { .. } | VerifyError::RegisterRangeOutOfRange { .. }
        ),
        "got {err:?}"
    );
}

#[test]
fn not_terminated_is_refused() {
    let mut m = base();
    m.main.instrs = vec![Instr::abc(Op::LOADNULL, 0, 0, 0)];
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::NotTerminated { .. }
    ));
}

#[test]
fn line_table_out_of_order_is_refused() {
    let mut m = base();
    m.main.instrs = vec![
        Instr::abc(Op::LOADNULL, 0, 0, 0),
        Instr::abc(Op::LOADNULL, 0, 0, 0),
        Instr::abc(Op::LOADNULL, 0, 0, 0),
        Instr::abc(Op::RET, 0, 0, 0),
    ];
    m.main.lines = vec![
        LineEntry {
            pc: 2,
            span_off: 0,
            span_len: 0,
            line_off: 0,
            col: 1,
        },
        LineEntry {
            pc: 1,
            span_off: 0,
            span_len: 0,
            line_off: 0,
            col: 1,
        },
    ];
    m.main.span_base = SpanBase {
        byte_offset: 0,
        line: 1,
        col: 1,
    };
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::LineTableBadRow { .. }
    ));
}

#[test]
fn arity_over_reg_count_is_refused() {
    let mut m = base();
    m.main.reg_count = 1;
    m.main.arity = 5;
    m.main.kind = FuncKind::Pull;
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::ArityExceedsRegCount { .. }
    ));
}

#[test]
fn a_bad_opcode_is_refused() {
    let mut m = base();
    m.main.instrs = vec![Instr { raw: 0xFFFF_FFFF }, Instr::abc(Op::RET, 0, 0, 0)];
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::BadOpcode { byte: 0xFF, .. }
    ));
}

#[test]
fn a_dangling_wide_prefix_is_refused() {
    let mut m = base();
    // Function is a single WIDE word: there is nothing to prefix.
    m.main.instrs = vec![wide_prefix(0, 0)];
    let err = verify_module(&m).unwrap_err();
    assert!(
        matches!(
            err,
            VerifyError::WideDangling { .. }
                | VerifyError::NotTerminated { .. }
                | VerifyError::EmptyFunction { .. }
        ),
        "got {err:?}"
    );
}

#[test]
fn a_child_function_is_verified_too() {
    // A main that verifies, holding a child that does not. The child
    // has a MOV past its reg_count.
    let mut m = base();
    let mut child = Function::empty_main();
    child.kind = FuncKind::Lambda;
    child.reg_count = 1;
    child.instrs = vec![Instr::abc(Op::MOV, 9, 0, 0), Instr::abc(Op::RET, 0, 0, 0)];
    m.main.children.push(child);
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::RegisterOutOfRange { .. }
    ));
}

#[test]
fn testn_falsy_branch_verifies_and_disassembles() {
    let mut m = base();
    m.main.reg_count = 1;
    m.main.instrs = vec![
        Instr::abc(Op::LOADNULL, 0, 0, 0), // pc 0
        Instr::asbx(Op::TESTN, 0, 1),      // pc 1: if falsy(R0) skip
        Instr::abc(Op::RET, 0, 0, 0),      // pc 2
        Instr::abc(Op::RET, 0, 0, 0),      // pc 3
    ];
    verify_module(&m).expect("testn should verify");
    let dis = cig_bytecode::disasm::disassemble_module(&m);
    assert!(dis.contains("testn"), "disasm missing testn: {dis}");
}

fn uncharged_pc(m: &Module) -> Option<u32> {
    match verify_module(m) {
        Err(VerifyError::UnchargedCycle { func, pc }) => {
            assert_eq!(func, 0);
            let e = verify_module(m).unwrap_err();
            assert_eq!(e.rule(), "uncharged-cycle");
            assert_eq!(e.to_string(), format!("uncharged-cycle at fn0+{pc}"));
            Some(pc)
        }
        Err(other) => panic!("refused for another rule: {other}"),
        Ok(_) => None,
    }
}

#[test]
fn a_cycle_with_no_charged_step_is_refused() {
    // The plain `jmp -1`.
    let mut m = base();
    m.main.instrs = vec![
        Instr::abc(Op::LOADNULL, 0, 0, 0),
        Instr::asbx(Op::JMP, 0, -1), // pc 1 -> 1
        Instr::abc(Op::RET, 0, 0, 0),
    ];
    assert_eq!(uncharged_pc(&m), Some(1));

    // A cycle through a fused compare-and-jump: pc 1 -> 3 -> (eqj) 1.
    let mut m = base();
    m.main.instrs = vec![
        Instr::abc(Op::LOADNULL, 0, 0, 0),
        Instr::abc(Op::LOADNULL, 1, 0, 0),       // pc 1
        Instr::asbx(Op::JMP, 0, 0),              // pc 2 -> 3
        Instr::abc(Op::EQJ, 0, 1, (-3i8) as u8), // pc 3 -> 1
        Instr::abc(Op::RET, 0, 0, 0),
    ];
    assert_eq!(uncharged_pc(&m), Some(1));

    // A cycle through a handler edge: the try body (pc 1..3) can fault to
    // its handler at pc 1, which falls into the body again.
    let mut m = base();
    m.main.instrs = vec![
        Instr::abx(Op::STEPS, 0, 1),
        Instr::abc(Op::LOADNULL, 0, 0, 0), // pc 1
        Instr::abc(Op::NOT, 1, 0, 0),      // pc 2
        Instr::abc(Op::RET, 0, 0, 0),      // pc 3
    ];
    m.main.handlers.push(HandlerEntry {
        pc_start: 1,
        pc_end: 3,
        handler_pc: 1,
        catch_reg: RESERVED_U32,
        finally_pc: RESERVED_U32,
    });
    assert_eq!(uncharged_pc(&m), Some(1));
    // The same through an outer handler's finally, reached from an inner
    // handler's range.
    let mut m = base();
    m.main.instrs = vec![
        Instr::abx(Op::STEPS, 0, 1),
        Instr::abc(Op::LOADNULL, 0, 0, 0), // pc 1
        Instr::abc(Op::NOT, 1, 0, 0),      // pc 2
        Instr::abc(Op::RET, 0, 0, 0),      // pc 3
        Instr::abc(Op::RET, 0, 0, 0),      // pc 4
    ];
    m.main.handlers = vec![
        HandlerEntry {
            pc_start: 1,
            pc_end: 4,
            handler_pc: 4,
            catch_reg: RESERVED_U32,
            finally_pc: 2,
        },
        HandlerEntry {
            pc_start: 2,
            pc_end: 3,
            handler_pc: 4,
            catch_reg: RESERVED_U32,
            finally_pc: RESERVED_U32,
        },
    ];
    assert_eq!(uncharged_pc(&m), Some(2));

    // A cycle whose only `steps` charges 0.
    let mut m = base();
    m.main.instrs = vec![
        Instr::abx(Op::STEPS, 1, 0),  // pc 0
        Instr::asbx(Op::LOOP, 0, -2), // pc 1 -> 0
        Instr::abc(Op::RET, 0, 0, 0),
    ];
    assert_eq!(uncharged_pc(&m), Some(0));

    // The iterating loops' back-edges count too.
    for op in [Op::FORLOOP, Op::ITERNEXT] {
        let mut m = base();
        m.main.instrs = vec![
            Instr::abc(Op::LOADNULL, 0, 0, 0),
            Instr::abc(Op::LOADNULL, 1, 0, 0), // pc 1
            Instr::asbx(op, 0, -2),            // pc 2 -> 1
            Instr::abc(Op::RET, 0, 0, 0),
        ];
        assert_eq!(uncharged_pc(&m), Some(1), "{op}");
    }

    // A wide back-edge measures from its first word.
    let mut m = base();
    m.main.instrs = vec![
        Instr::abc(Op::LOADNULL, 0, 0, 0),
        wide_prefix(0, 0xFFFF),      // pc 1
        Instr::asbx(Op::JMP, 0, -2), // pc 1 -> 0 (32-bit -2)
        Instr::abc(Op::RET, 0, 0, 0),
    ];
    assert_eq!(uncharged_pc(&m), Some(0));
}

#[test]
fn a_loop_that_charges_each_turn_verifies() {
    // while: steps 1,0; top: cond; testn -> out; steps 1,1; body; loop top.
    let mut m = base();
    m.main.instrs = vec![
        Instr::abx(Op::STEPS, 1, 0),       // pc 0
        Instr::abc(Op::NOT, 0, 0, 0),      // pc 1: top
        Instr::asbx(Op::TESTN, 0, 3),      // pc 2 -> 6
        Instr::abx(Op::STEPS, 1, 1),       // pc 3: one turn
        Instr::abc(Op::LOADNULL, 1, 0, 0), // pc 4
        Instr::asbx(Op::LOOP, 0, -5),      // pc 5 -> 1
        Instr::abx(Op::STEPS, 0, 0),       // pc 6
        Instr::abc(Op::RET, 0, 0, 0),
    ];
    verify_module(&m).expect("a charged while verifies");

    // for i in a..b: forprep -> out; body: steps 1,1; ...; forloop -> body.
    let mut m = base();
    m.main.instrs = vec![
        Instr::asbx(Op::FORPREP, 0, 3),  // pc 0 -> 4
        Instr::abx(Op::STEPS, 1, 1),     // pc 1: one item
        Instr::abc(Op::MOV, 5, 3, 0),    // pc 2
        Instr::asbx(Op::FORLOOP, 0, -3), // pc 3 -> 1
        Instr::abc(Op::RET, 0, 0, 0),    // pc 4
    ];
    verify_module(&m).expect("a charged for verifies");

    // A try inside a charged loop: the handler falls back to the head.
    let mut m = base();
    m.main.instrs = vec![
        Instr::abx(Op::STEPS, 1, 1),       // pc 0: head
        Instr::abc(Op::LOADNULL, 0, 0, 0), // pc 1: try body
        Instr::abc(Op::NOT, 1, 0, 0),      // pc 2: handler
        Instr::asbx(Op::TEST, 1, -4),      // pc 3 -> 0
        Instr::abc(Op::RET, 0, 0, 0),
    ];
    m.main.handlers.push(HandlerEntry {
        pc_start: 1,
        pc_end: 2,
        handler_pc: 2,
        catch_reg: RESERVED_U32,
        finally_pc: RESERVED_U32,
    });
    verify_module(&m).expect("a charged loop around a try verifies");
}
