// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! `read-before-write`: after `call A, B, C` every register past the
//! call's result row is undefined until it is written. One module per way
//! to break the rule, and the same shapes that keep it.

use cig_bytecode::{
    verify_module, Constant, FuncKind, Function, HandlerEntry, Instr, Module, Op, UpvalDesc,
    UpvalKind, VerifyError, RESERVED_U32,
};

/// A main of 8 registers running `code`.
fn module(code: Vec<Instr>) -> Module {
    let mut m = Module::empty("t.cig");
    m.main.reg_count = 8;
    m.main.constants.push(Constant::Str("x".to_string()));
    m.main.instrs = code;
    m
}

/// The read the rule refused: its pc, the register and the call's pc.
fn refused(m: &Module) -> Option<(u32, u32, u32)> {
    match verify_module(m) {
        Err(VerifyError::ReadBeforeWrite {
            func,
            pc,
            reg,
            why,
            at,
        }) => {
            assert_eq!((func, why), (0, "after-call"));
            let e = verify_module(m).unwrap_err();
            assert_eq!(e.rule(), "read-before-write");
            assert_eq!(e.func(), Some(0));
            assert_eq!(e.pc(), Some(pc));
            assert_eq!(
                e.to_string(),
                format!(
                    "read-before-write at fn0+{pc}: R{reg} is undefined (after-call at fn0+{at})"
                )
            );
            Some((pc, reg, at))
        }
        Err(other) => panic!("refused for another rule: {other}"),
        Ok(_) => None,
    }
}

/// `R2 = f(R3)` with `f` in `R2`: the window is `R2..R4`.
fn call_at_2() -> Instr {
    Instr::abc(Op::CALL, 2, 1, 0)
}

#[test]
fn a_value_kept_above_a_calls_window_and_read_after_it_is_refused() {
    let m = module(vec![
        Instr::abc(Op::LOADNULL, 2, 0, 0),
        Instr::abc(Op::LOADNULL, 3, 0, 0),
        Instr::asbx(Op::LOADINT, 5, 7), // pc 2: a value above the window
        call_at_2(),                    // pc 3
        Instr::abc(Op::MOV, 0, 5, 0),   // pc 4: read after the call
        Instr::abc(Op::RET, 0, 0, 0),
    ]);
    assert_eq!(refused(&m), Some((4, 5, 3)));

    // An argument register is above the result too: the callee's
    // parameter, not the caller's value.
    let m = module(vec![
        Instr::abc(Op::LOADNULL, 2, 0, 0),
        Instr::abc(Op::LOADNULL, 3, 0, 0),
        call_at_2(),
        Instr::abc(Op::RET, 3, 0, 0), // pc 3
    ]);
    assert_eq!(refused(&m), Some((3, 3, 2)));

    // Every kind of read counts: an operator, a test, a call's window.
    for read in [
        Instr::abc(Op::ADD, 0, 1, 6),
        Instr::asbx(Op::TEST, 6, 0),
        Instr::abc(Op::CALLB, 5, 0, 1),
        Instr::abc(Op::EXHALE, 6, 1, 0),
        Instr::abc(Op::SETINDEX, 1, 1, 6),
    ] {
        let m = module(vec![
            Instr::abc(Op::LOADNULL, 1, 0, 0),
            Instr::abc(Op::LOADNULL, 2, 0, 0),
            Instr::abc(Op::LOADNULL, 3, 0, 0),
            Instr::abc(Op::LOADNULL, 6, 0, 0),
            call_at_2(), // pc 4
            read,        // pc 5
            Instr::abc(Op::RET, 0, 0, 0),
        ]);
        assert_eq!(refused(&m), Some((5, 6, 4)), "{:?}", m.main.instrs[5]);
    }
}

#[test]
fn values_below_the_window_its_result_row_and_written_registers_pass() {
    let m = module(vec![
        Instr::abc(Op::LOADNULL, 0, 0, 0),
        Instr::abc(Op::LOADNULL, 1, 0, 0),
        Instr::abc(Op::LOADNULL, 2, 0, 0),
        Instr::abc(Op::LOADNULL, 3, 0, 0),
        Instr::abc(Op::LOADNULL, 5, 0, 0),
        call_at_2(),
        Instr::abc(Op::ADD, 0, 0, 1), // below the window
        Instr::abc(Op::MOV, 1, 2, 0), // the result
        Instr::asbx(Op::LOADINT, 5, 1),
        Instr::abc(Op::MOV, 1, 5, 0), // written after the call first
        Instr::abc(Op::RET, 1, 0, 0),
    ]);
    assert_eq!(refused(&m), None);

    // A result row of two: `call 2, 1, 1` writes R2 and R3.
    let m = module(vec![
        Instr::abc(Op::LOADNULL, 2, 0, 0),
        Instr::abc(Op::LOADNULL, 3, 0, 0),
        Instr::abc(Op::CALL, 2, 1, 1),
        Instr::abc(Op::MOV, 0, 3, 0),
        Instr::abc(Op::RET, 0, 0, 0),
    ]);
    assert_eq!(refused(&m), None);
    let m = module(vec![
        Instr::abc(Op::LOADNULL, 2, 0, 0),
        Instr::abc(Op::LOADNULL, 3, 0, 0),
        Instr::abc(Op::LOADNULL, 4, 0, 0),
        Instr::abc(Op::CALL, 2, 1, 1), // pc 3
        Instr::abc(Op::MOV, 0, 4, 0),  // pc 4
        Instr::abc(Op::RET, 0, 0, 0),
    ]);
    assert_eq!(refused(&m), Some((4, 4, 3)));

    // `callb`, `callm` and `burn.op` make no frame: nothing is undefined.
    for call in [
        Instr::abc(Op::CALLB, 2, 0, 1),
        Instr::abc(Op::CALLM, 2, 0, 1),
    ] {
        let m = module(vec![
            Instr::abc(Op::LOADNULL, 2, 0, 0),
            Instr::abc(Op::LOADNULL, 3, 0, 0),
            Instr::abc(Op::LOADNULL, 5, 0, 0),
            call,
            Instr::abc(Op::MOV, 0, 5, 0),
            Instr::abc(Op::RET, 3, 0, 0),
        ]);
        assert_eq!(refused(&m), None);
    }
}

#[test]
fn a_cell_above_a_calls_window_is_gone_after_it() {
    // `cellnew` reads its register: the cell would hold what the callee
    // left there.
    let m = module(vec![
        Instr::abc(Op::LOADNULL, 2, 0, 0),
        Instr::abc(Op::LOADNULL, 3, 0, 0),
        Instr::abc(Op::LOADNULL, 5, 0, 0),
        Instr::abc(Op::CELLNEW, 5, 0, 0), // pc 3: a captured value above
        call_at_2(),                      // pc 4
        Instr::abc(Op::MOV, 0, 5, 0),     // pc 5
        Instr::abc(Op::RET, 0, 0, 0),
    ]);
    assert_eq!(refused(&m), Some((5, 5, 4)));
    let m = module(vec![
        Instr::abc(Op::LOADNULL, 2, 0, 0),
        Instr::abc(Op::LOADNULL, 3, 0, 0),
        call_at_2(),
        Instr::abc(Op::CELLNEW, 5, 0, 0), // pc 3
        Instr::abc(Op::RET, 2, 0, 0),
    ]);
    assert_eq!(refused(&m), Some((3, 5, 2)));

    // A closure that captures it reads it.
    let mut m = module(vec![
        Instr::abc(Op::LOADNULL, 2, 0, 0),
        Instr::abc(Op::LOADNULL, 3, 0, 0),
        Instr::abc(Op::LOADNULL, 5, 0, 0),
        Instr::abc(Op::CELLNEW, 5, 0, 0),
        call_at_2(),                   // pc 4
        Instr::abx(Op::CLOSURE, 0, 0), // pc 5
        Instr::abc(Op::RET, 0, 0, 0),
    ]);
    let mut child = Function::empty_main();
    child.kind = FuncKind::Lambda;
    child.reg_count = 1;
    child.upvalues.push(UpvalDesc {
        kind: UpvalKind::ParentLocal,
        index: 5,
    });
    child.instrs = vec![Instr::abc(Op::GETUP, 0, 0, 0), Instr::abc(Op::RET, 0, 0, 0)];
    m.main.children.push(child);
    assert_eq!(refused(&m), Some((5, 5, 4)));

    // Below the window, a cell lives through the call.
    let mut below = m.clone();
    below.main.children[0].upvalues[0].index = 1;
    below.main.instrs[2] = Instr::abc(Op::LOADNULL, 1, 0, 0);
    below.main.instrs[3] = Instr::abc(Op::CELLNEW, 1, 0, 0);
    assert_eq!(refused(&below), None);

    // Written after the call, before the cell is made: fine.
    let m = module(vec![
        Instr::abc(Op::LOADNULL, 2, 0, 0),
        Instr::abc(Op::LOADNULL, 3, 0, 0),
        call_at_2(),
        Instr::abc(Op::LOADNULL, 5, 0, 0),
        Instr::abc(Op::CELLNEW, 5, 0, 0),
        Instr::abc(Op::MOV, 0, 5, 0),
        Instr::abc(Op::RET, 0, 0, 0),
    ]);
    assert_eq!(refused(&m), None);
}

#[test]
fn a_loop_that_reads_one_on_its_second_turn_is_refused() {
    // R5 is written before the loop and read at the top of each turn; the
    // call in the body leaves it undefined for the second.
    let m = module(vec![
        Instr::abc(Op::LOADNULL, 5, 0, 0), // pc 0
        Instr::abx(Op::STEPS, 1, 1),       // pc 1: the turn
        Instr::abc(Op::MOV, 0, 5, 0),      // pc 2: the read
        Instr::abc(Op::LOADNULL, 2, 0, 0),
        Instr::abc(Op::LOADNULL, 3, 0, 0),
        call_at_2(),                  // pc 5
        Instr::asbx(Op::TEST, 2, 1),  // pc 6 -> 8
        Instr::asbx(Op::LOOP, 0, -7), // pc 7 -> 1
        Instr::abc(Op::RET, 0, 0, 0), // pc 8
    ]);
    assert_eq!(refused(&m), Some((2, 5, 5)));

    // Written again in the body after the call: every turn reads a value.
    let mut fine = m.clone();
    fine.main.instrs[6] = Instr::asbx(Op::LOADINT, 5, 1);
    fine.main.instrs.insert(7, Instr::asbx(Op::TEST, 2, 1)); // pc 7 -> 9
    fine.main.instrs[8] = Instr::asbx(Op::LOOP, 0, -8); // pc 8 -> 1
    assert_eq!(refused(&fine), None);

    // A `for` loop's window below the call's lives through it; its item
    // is written by `forloop` on the way back.
    let m = module(vec![
        Instr::asbx(Op::LOADINT, 0, 0),
        Instr::asbx(Op::LOADINT, 1, 3),
        Instr::asbx(Op::LOADINT, 2, 1),
        Instr::asbx(Op::FORPREP, 0, 5), // pc 3 -> 9
        Instr::abx(Op::STEPS, 1, 1),    // pc 4
        Instr::abc(Op::MOV, 4, 3, 0),   // the item
        Instr::abc(Op::LOADNULL, 5, 0, 0),
        Instr::abc(Op::CALL, 4, 1, 0),   // pc 7: the window R4..R6
        Instr::asbx(Op::FORLOOP, 0, -5), // pc 8 -> 4
        Instr::abc(Op::RET, 0, 0, 0),    // pc 9
    ]);
    assert_eq!(refused(&m), None);
}

#[test]
fn a_catch_and_a_finally_see_what_the_callee_left() {
    // The call's error reaches the catch with the callee's frame run: a
    // value above the window is undefined there; the catch's binding is
    // written.
    let mut m = module(vec![
        Instr::abc(Op::LOADNULL, 2, 0, 0),
        Instr::abc(Op::LOADNULL, 3, 0, 0),
        Instr::abc(Op::LOADNULL, 5, 0, 0),
        Instr::abx(Op::TRY_ENTER, 0, 0), // pc 3
        call_at_2(),                     // pc 4
        Instr::abc(Op::TRY_LEAVE, 0, 0, 0),
        Instr::asbx(Op::JMP, 0, 2),   // pc 6 -> 9
        Instr::abc(Op::MOV, 0, 6, 0), // pc 7: the catch reads its binding
        Instr::abc(Op::TRY_LEAVE, 0, 0, 0),
        Instr::abc(Op::RET, 0, 0, 0), // pc 9
    ]);
    m.main.handlers.push(HandlerEntry {
        pc_start: 4,
        pc_end: 6,
        handler_pc: 7,
        catch_reg: 6,
        finally_pc: RESERVED_U32,
    });
    assert_eq!(refused(&m), None);
    let mut bad = m.clone();
    bad.main.instrs[7] = Instr::abc(Op::MOV, 0, 5, 0);
    assert_eq!(refused(&bad), Some((7, 5, 4)));

    // A call in a finally: the run goes on after the `try.leave` that
    // entered it with what the finally left.
    let mut m = module(vec![
        Instr::abc(Op::LOADNULL, 5, 0, 0),
        Instr::abx(Op::TRY_ENTER, 0, 0),    // pc 1
        Instr::abc(Op::TRY_LEAVE, 0, 0, 0), // pc 2: into the finally
        Instr::abc(Op::MOV, 0, 5, 0),       // pc 3: after it
        Instr::abc(Op::RET, 0, 0, 0),
        Instr::abc(Op::LOADNULL, 2, 0, 0), // pc 5: the finally
        Instr::abc(Op::LOADNULL, 3, 0, 0),
        call_at_2(),                        // pc 7
        Instr::abc(Op::TRY_LEAVE, 0, 0, 0), // pc 8: its end
        Instr::abc(Op::RET, 0, 0, 0),
    ]);
    m.main.handlers.push(HandlerEntry {
        pc_start: 2,
        pc_end: 3,
        handler_pc: 5,
        catch_reg: RESERVED_U32,
        finally_pc: 5,
    });
    assert_eq!(refused(&m), Some((3, 5, 7)));
    let mut fine = m.clone();
    fine.main.instrs[3] = Instr::abc(Op::MOV, 0, 1, 0);
    assert_eq!(refused(&fine), None);
}

#[test]
fn a_write_on_one_way_only_counts_on_that_way() {
    // `testset` writes R5 only when R1 is truthy: the jump keeps R5
    // undefined.
    let m = module(vec![
        Instr::abc(Op::LOADNULL, 1, 0, 0),
        Instr::abc(Op::LOADNULL, 2, 0, 0),
        Instr::abc(Op::LOADNULL, 3, 0, 0),
        call_at_2(),                      // pc 3
        Instr::abc(Op::TESTSET, 5, 1, 1), // pc 4 -> 6 when falsy
        Instr::abc(Op::MOV, 0, 5, 0),     // pc 5: written
        Instr::abc(Op::RET, 5, 0, 0),     // pc 6: not on the jump
    ]);
    assert_eq!(refused(&m), Some((6, 5, 3)));

    // An instruction the run never reaches is not refused.
    let m = module(vec![
        Instr::abc(Op::LOADNULL, 2, 0, 0),
        Instr::abc(Op::LOADNULL, 3, 0, 0),
        call_at_2(),
        Instr::abc(Op::RET, 2, 0, 0),
        Instr::abc(Op::RET, 5, 0, 0),
    ]);
    assert_eq!(refused(&m), None);
}
