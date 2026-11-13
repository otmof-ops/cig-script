// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The burn rules on the region rows: the shapes a lowering writes, each
//! malformed shape a rule names, the region label's three kinds on the
//! wire, and the disassembly of the burn instructions.

use cig_bytecode::{
    disasm::disassemble_module, parse_module, serialize_module, verify_module, BurnRegion,
    Constant, HandlerEntry, Instr, Module, Op, ParseError, RegionLabel, RegionRow, VerifyError,
    RESERVED_U32,
};

/// A top level with `regs` registers, the regions `(class, label)`, the
/// rows `(pc_start, pc_end, region)` and `code`. `K0` is a burn op's
/// callee, `K1` a compensation's source, `K2` its state's name.
fn burns(
    regs: u32,
    regions: &[(u8, RegionLabel)],
    rows: &[(u32, u32, u32)],
    code: Vec<Instr>,
) -> Module {
    let mut m = Module::empty("t.cig");
    m.main.reg_count = regs;
    m.main.constants = vec![
        Constant::Str("fs.write_text".into()),
        Constant::Str("fs.rm(s.path)".into()),
        Constant::Str("s".into()),
    ];
    m.main.regions = regions
        .iter()
        .map(|&(class, label)| BurnRegion { class, label })
        .collect();
    m.main.region_rows = rows
        .iter()
        .map(|&(pc_start, pc_end, region)| RegionRow {
            pc_start,
            pc_end,
            region,
        })
        .collect();
    m.main.instrs = code;
    m
}

fn enter(class: u8, region: u16) -> Instr {
    Instr::abx(Op::BURN_ENTER, class, region)
}
fn leave() -> Instr {
    Instr::abc(Op::BURN_LEAVE, 0, 0, 0)
}
fn comp_enter(state: u8) -> Instr {
    Instr::abc(Op::COMP_ENTER, state, 1, 2)
}
fn comp_leave() -> Instr {
    Instr::abc(Op::COMP_LEAVE, 0, 0, 0)
}
/// `R1 = fs.write_text(R2)`.
fn op() -> Instr {
    Instr::abc(Op::BURN_OP, 1, 0, 0x10)
}
fn jmp(offset: i16) -> Instr {
    Instr::asbx(Op::JMP, 0, offset)
}
fn test(r: u8, offset: i16) -> Instr {
    Instr::asbx(Op::TEST, r, offset)
}
fn ret(r: u8) -> Instr {
    Instr::abc(Op::RET, r, 0, 0)
}
fn loadnull(r: u8) -> Instr {
    Instr::abc(Op::LOADNULL, r, 0, 0)
}

const NORMAL: (u8, RegionLabel) = (0, RegionLabel::None);
const UNLIT: (u8, RegionLabel) = (1, RegionLabel::None);

/// The rule that refused `m`: the `why` of `burn-unbalanced`, and its pc.
fn refused(m: &Module) -> (&'static str, u32) {
    match verify_module(m) {
        Err(VerifyError::BurnUnbalanced { why, pc, func: 0 }) => {
            assert_eq!(
                verify_module(m).unwrap_err().to_string(),
                format!("burn-unbalanced at fn0+{pc}")
            );
            (why, pc)
        }
        Err(other) => panic!("refused for another rule: {other}"),
        Ok(_) => panic!("verified"),
    }
}

// ----- what a lowering writes ------------------------------------------------------

#[test]
fn a_burn_left_by_a_jump_needs_nothing_after_the_jump() {
    // `burn (s) { op; if c { break } } unburn { }`: the way out leaves the
    // compensation and the burn, then jumps; the body's own end follows.
    let m = burns(
        4,
        &[NORMAL],
        &[(1, 10, 0)],
        vec![
            Instr::abc(Op::NEWMAP, 0, 0, 0), // 0
            enter(0, 0),                     // 1
            comp_enter(0),                   // 2
            op(),                            // 3
            test(3, 3),                      // 4 -> 8
            comp_leave(),                    // 5
            leave(),                         // 6
            jmp(2),                          // 7 -> 10
            comp_leave(),                    // 8
            leave(),                         // 9
            loadnull(0),                     // 10
            ret(0),                          // 11
        ],
    );
    verify_module(&m).expect("a burn left by a jump verifies");
}

#[test]
fn a_burn_whose_body_ends_in_a_terminator_needs_no_leave_after_it() {
    for last in [ret(1), Instr::abc(Op::THROW, 1, 0, 0), jmp(0)] {
        let code = vec![enter(0, 0), op(), last, loadnull(0), ret(0)];
        let m = burns(4, &[NORMAL], &[(0, 3, 0)], code);
        verify_module(&m).unwrap_or_else(|e| panic!("{last:?}: {e}"));
    }
}

#[test]
fn nested_burns_left_two_at_a_time_verify() {
    // `burn { burn unlit { if c { break } } }` inside a loop: both are left
    // on the way out, innermost first.
    let m = burns(
        4,
        &[NORMAL, UNLIT],
        &[(0, 8, 0), (1, 7, 1)],
        vec![
            enter(0, 0), // 0
            enter(1, 1), // 1
            test(3, 3),  // 2 -> 6
            leave(),     // 3
            leave(),     // 4
            jmp(2),      // 5 -> 8
            leave(),     // 6: the inner burn's end
            leave(),     // 7: the outer burn's end
            loadnull(0), // 8
            ret(0),      // 9
        ],
    );
    verify_module(&m).expect("two burns left at once verify");
}

#[test]
fn a_re_entry_no_run_reaches_still_verifies() {
    // What a lowering wrote before the rules followed the rows: after the
    // jump out, one `burn.enter` per burn left, inside its own row.
    let m = burns(
        4,
        &[NORMAL, NORMAL],
        &[(0, 10, 0), (1, 9, 1)],
        vec![
            enter(0, 0), // 0
            enter(0, 1), // 1
            test(3, 5),  // 2 -> 8
            leave(),     // 3
            leave(),     // 4
            jmp(4),      // 5 -> 10
            enter(0, 0), // 6: never runs
            enter(0, 1), // 7: never runs
            leave(),     // 8
            leave(),     // 9
            loadnull(0), // 10
            ret(0),      // 11
        ],
    );
    verify_module(&m).expect("the old lowering's shape verifies");
}

#[test]
fn a_jump_to_a_rows_start_enters_it() {
    let m = burns(
        4,
        &[NORMAL],
        &[(2, 5, 0)],
        vec![
            test(3, 1),  // 0 -> 2
            loadnull(0), // 1
            enter(0, 0), // 2
            op(),        // 3
            leave(),     // 4
            ret(0),      // 5
        ],
    );
    verify_module(&m).expect("a jump to a burn's burn.enter verifies");
}

#[test]
fn an_unlit_burn_in_a_one_register_frame_verifies() {
    let m = burns(
        1,
        &[UNLIT],
        &[(0, 2, 0)],
        vec![enter(1, 0), leave(), loadnull(0), ret(0)],
    );
    verify_module(&m).expect("the class is not a register");
}

// ----- one refusal per rule --------------------------------------------------------

#[test]
fn burn_rows_that_interleave_are_refused() {
    let code = vec![enter(0, 0), enter(0, 1), leave(), leave(), ret(0)];
    let m = burns(4, &[NORMAL, NORMAL], &[(0, 3, 0), (1, 4, 1)], code.clone());
    assert_eq!(refused(&m), ("rows-interleave", 1));
    // Two rows on one `burn.enter`.
    let m = burns(4, &[NORMAL, NORMAL], &[(0, 3, 0), (0, 4, 0)], code);
    assert_eq!(refused(&m), ("rows-interleave", 0));
}

#[test]
fn a_row_that_does_not_start_at_its_burn_enter_is_refused() {
    let m = burns(
        4,
        &[NORMAL],
        &[(0, 3, 0)],
        vec![loadnull(0), enter(0, 0), leave(), ret(0)],
    );
    assert_eq!(refused(&m), ("row-without-enter", 0));
    // A `burn.enter` of another region.
    let m = burns(
        4,
        &[NORMAL, NORMAL],
        &[(0, 2, 0)],
        vec![enter(0, 1), leave(), ret(0)],
    );
    assert_eq!(refused(&m), ("row-without-enter", 0));
}

#[test]
fn a_row_whose_end_falls_through_is_refused() {
    let m = burns(
        4,
        &[NORMAL],
        &[(0, 3, 0)],
        vec![enter(0, 0), op(), loadnull(0), ret(0)],
    );
    assert_eq!(refused(&m), ("row-falls-out", 2));
}

#[test]
fn a_burn_enter_outside_its_regions_rows_is_refused() {
    // Inside a row, but of another region.
    let m = burns(
        4,
        &[NORMAL, NORMAL],
        &[(0, 4, 0)],
        vec![enter(0, 0), enter(0, 1), leave(), leave(), ret(0)],
    );
    assert_eq!(refused(&m), ("enter-outside-row", 1));
}

#[test]
fn more_leaves_than_rows_around_them_are_refused() {
    let m = burns(
        4,
        &[NORMAL],
        &[(0, 3, 0)],
        vec![enter(0, 0), leave(), comp_leave(), leave(), ret(0)],
    );
    // The row ends at pc 3 after the run: rows [0, 4).
    let mut m = m;
    m.main.region_rows[0].pc_end = 4;
    assert_eq!(refused(&m), ("leave-outside-row", 3));
}

#[test]
fn a_jump_into_a_row_from_outside_it_is_refused() {
    let code = vec![
        test(3, 1),  // 0 -> 2
        enter(0, 0), // 1
        op(),        // 2
        leave(),     // 3
        loadnull(0), // 4
        ret(0),      // 5
    ];
    let m = burns(4, &[NORMAL], &[(1, 4, 0)], code);
    assert_eq!(refused(&m), ("jump-into-row", 0));
    // A handler whose range lies outside the row, landing inside it.
    let code = vec![loadnull(0), enter(0, 0), op(), leave(), ret(0)];
    let mut m = burns(4, &[NORMAL], &[(1, 4, 0)], code);
    m.main.handlers.push(HandlerEntry {
        pc_start: 0,
        pc_end: 1,
        handler_pc: 2,
        catch_reg: RESERVED_U32,
        finally_pc: RESERVED_U32,
    });
    assert_eq!(refused(&m), ("jump-into-row", 2));
}

#[test]
fn a_burn_op_outside_every_row_is_refused() {
    let m = burns(
        4,
        &[NORMAL],
        &[(0, 2, 0)],
        vec![enter(0, 0), leave(), op(), ret(0)],
    );
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::BurnOpOutsideRegion { pc: 2, .. }
    ));
}

#[test]
fn a_burn_enter_whose_class_is_not_its_regions_is_refused() {
    let m = burns(
        4,
        &[NORMAL],
        &[(0, 2, 0)],
        vec![enter(1, 0), leave(), ret(0)],
    );
    let e = verify_module(&m).unwrap_err();
    assert_eq!(e, VerifyError::BurnClassMismatch { func: 0, pc: 0 });
    assert_eq!(e.to_string(), "burn-class-mismatch at fn0+0");
}

#[test]
fn a_label_register_past_the_frame_is_refused() {
    let code = vec![enter(0, 0), leave(), ret(0)];
    let m = burns(4, &[(0, RegionLabel::Reg(3))], &[(0, 2, 0)], code.clone());
    verify_module(&m).expect("a label register in the frame verifies");
    let m = burns(4, &[(0, RegionLabel::Reg(4))], &[(0, 2, 0)], code);
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::RegisterOutOfRange { pc: 0, reg: 4, .. }
    ));
}

#[test]
fn comp_enter_names_a_register_and_two_strings() {
    let code = |c: Instr| vec![enter(0, 0), c, comp_leave(), leave(), ret(0)];
    let m = burns(4, &[NORMAL], &[(0, 4, 0)], code(comp_enter(3)));
    verify_module(&m).expect("comp.enter verifies");
    let m = burns(4, &[NORMAL], &[(0, 4, 0)], code(comp_enter(4)));
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::RegisterOutOfRange { pc: 1, .. }
    ));
    let mut m = burns(4, &[NORMAL], &[(0, 4, 0)], code(comp_enter(0)));
    m.main.constants[2] = Constant::Int(1);
    assert!(matches!(
        verify_module(&m).unwrap_err(),
        VerifyError::ConstNotString {
            pc: 1,
            index: 2,
            ..
        }
    ));
}

// ----- the region label on the wire ------------------------------------------------

#[test]
fn each_kind_of_region_label_round_trips() {
    let m = burns(
        4,
        &[
            (0, RegionLabel::None),
            (1, RegionLabel::Const(2)),
            (0, RegionLabel::Reg(3)),
        ],
        &[],
        vec![ret(0)],
    );
    let bytes = serialize_module(&m);
    assert_eq!(parse_module(&bytes).expect("parses"), m);
}

/// The bytes of `m` with the region table's first entry at `region`
/// (class, label kind, label) replaced, and the integrity hash redone.
fn with_region(m: &Module, region: [u8; 6]) -> Vec<u8> {
    let mut bytes = serialize_module(m);
    let want = {
        let mut w = Vec::new();
        w.extend_from_slice(&1u32.to_le_bytes());
        w.extend_from_slice(&[0, 2]);
        w.extend_from_slice(&3u32.to_le_bytes());
        w
    };
    let at = bytes
        .windows(want.len())
        .position(|w| w == want.as_slice())
        .expect("the region table");
    bytes[at + 4..at + 10].copy_from_slice(&region);
    let hash = blake3::hash(&bytes[40..]);
    bytes[8..40].copy_from_slice(hash.as_bytes());
    bytes
}

#[test]
fn a_region_label_has_one_encoding() {
    let m = burns(4, &[(0, RegionLabel::Reg(3))], &[], vec![ret(0)]);
    let ok = with_region(&m, [0, 2, 3, 0, 0, 0]);
    assert_eq!(parse_module(&ok).expect("parses"), m);
    for (region, tag) in [
        ([0, 3, 0, 0, 0, 0], 3),          // no such kind
        ([0, 0, 0, 0, 0, 0], 0),          // none, with an index
        ([0, 0, 0xFF, 0xFF, 0xFF, 0], 0), // none, with an index
    ] {
        assert_eq!(
            parse_module(&with_region(&m, region)).unwrap_err(),
            ParseError::BadTag {
                at: "region label kind",
                value: tag
            }
        );
    }
}

// ----- the disassembler ------------------------------------------------------------

#[test]
fn the_burn_instructions_disassemble_whole() {
    let mut m = burns(
        4,
        &[(0, RegionLabel::Const(3)), (1, RegionLabel::Reg(3))],
        &[(1, 5, 0), (5, 7, 1)],
        vec![
            Instr::abc(Op::NEWMAP, 0, 0, 0),
            enter(0, 0),
            comp_enter(0),
            comp_leave(),
            leave(),
            enter(1, 1),
            leave(),
            Instr::abc(Op::CHAIN_MAKE, 0, 1, 0),
            ret(0),
        ],
    );
    m.main.constants.push(Constant::Str("deploy".into()));
    verify_module(&m).expect("verifies");
    let want = "; module: t.cig

fn 0 <anonymous> (0 args, 4 regs, main, 4 consts, 0 children, 0 upvals, 0 handlers, 0 lines, 2 regions, 2 region rows):
  0000   newmap    R0, 0
  0001   burn.enter class=normal, Rg0 (label K3 (\"deploy\"))
  0002   comp.enter R0, K1 (\"fs.rm(s.path)\"), K2 (\"s\")
  0003   comp.leave
  0004   burn.leave
  0005   burn.enter class=unlit, Rg1 (label R3)
  0006   burn.leave
  0007   chain.make R0, x1
  0008   ret       R0
";
    assert_eq!(disassemble_module(&m), want);
}
