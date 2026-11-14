// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The module verifier.
//!
//! Every rule a module must satisfy to be safe to run with no further
//! checks. The verifier reads a parsed module and returns the first
//! [`VerifyError`] it finds. It never panics.
//!
//! Each rule is one function: the names match `docs/BYTECODE.md`'s
//! verifier bullets. The verifier uses [`crate::instr::decode`] for
//! every operand read, so a `WIDE`-prefixed instruction's widened
//! operands are checked against the function's bounds.

use crate::error::VerifyError;
use crate::instr::{decode_table, DecodeError, DecodedInstr, FirstWordPcs};
use crate::module::{
    Constant, Function, Module, RegionLabel, RegionRow, UpvalDesc, UpvalKind, MAX_FUNCTION_DEPTH,
    MAX_REG_COUNT, RESERVED_U32,
};
use crate::opcode::{Fmt, Op, OperandKind};

/// A module the verifier has accepted.
///
/// Only [`verify_module`] makes one: the field is private and there is
/// no other constructor, so a VM that accepts nothing but a `Verified`
/// cannot be handed a module that skipped the verifier.
///
/// ```compile_fail
/// use cig_bytecode::{Module, Verified};
/// let m = Module::empty("x.cig");
/// let v = Verified { module: &m }; // private field: does not compile
/// ```
#[derive(Clone, Copy, Debug)]
pub struct Verified<'m> {
    module: &'m Module,
}

impl<'m> Verified<'m> {
    /// The module the verifier accepted.
    pub fn module(&self) -> &'m Module {
        self.module
    }
}

/// Verify a module. Returns the first failing rule, or the module
/// wrapped as [`Verified`] when every rule holds.
pub fn verify_module(m: &Module) -> Result<Verified<'_>, VerifyError> {
    // Depth-first walk, assigning a running id for the error's `func`
    // field. The id is a pre-order index: 0 is main, each child
    // continues the sequence.
    let mut counter = 0u32;
    verify_tree(&m.main, None, &mut counter, 0)?;
    Ok(Verified { module: m })
}

/// What a function's rules need from outside the function: its pre-order
/// index (`fn<i>` in an error), its nesting depth, its number of children
/// (the bound of `closure`'s operand) and its parent's register and
/// upvalue counts (the bounds of its upvalue descriptors; `None` for the
/// top level, which has no upvalues).
///
/// A module's reader gives each function's place from the module's
/// directory, which the reader checked as one tree when it opened the
/// module (`docs/BYTECODE.md`, "The tree").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Place {
    /// The function's pre-order index.
    pub index: u32,
    /// Its depth: 0 for the top level.
    pub depth: u32,
    /// How many children it has.
    pub children: u32,
    /// Its parent's counts; `None` for the top level.
    pub parent: Option<ParentCounts>,
}

/// The two counts of a function's parent that its upvalue descriptors are
/// checked against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParentCounts {
    /// The parent's register count.
    pub reg_count: u32,
    /// The parent's upvalue count.
    pub upvalues: u32,
}

/// One function the verifier has accepted, with its instructions decoded
/// once: the table every rule read, which the VM loads from instead of
/// decoding the function again.
///
/// Only [`verify_function`] makes one.
#[derive(Debug)]
pub struct VerifiedFunction<'f> {
    function: &'f Function,
    index: u32,
    code: Vec<Option<DecodedInstr>>,
}

impl<'f> VerifiedFunction<'f> {
    /// The function the verifier accepted.
    pub fn function(&self) -> &'f Function {
        self.function
    }

    /// Its pre-order index.
    pub fn index(&self) -> u32 {
        self.index
    }

    /// The decoded instruction at each first-word pc, `None` at the second
    /// word of a `WIDE` pair.
    pub fn code(&self) -> &[Option<DecodedInstr>] {
        &self.code
    }

    /// The decoded table, taken.
    pub fn into_code(self) -> Vec<Option<DecodedInstr>> {
        self.code
    }
}

/// Verify one function at its place in its module: every rule
/// [`verify_module`] checks for that function, in the same order, with
/// the counts it would take from the function's parent and children taken
/// from `place`. The function's children are not verified here: each is
/// verified at its own place (`f.children` is not read; a function a lazy
/// reader gives has none).
pub fn verify_function(f: &Function, place: Place) -> Result<VerifiedFunction<'_>, VerifyError> {
    if place.depth > MAX_FUNCTION_DEPTH {
        return Err(VerifyError::NestedTooDeep {
            depth: place.depth,
            max: MAX_FUNCTION_DEPTH,
        });
    }
    let code = check_function(f, place)?;
    Ok(VerifiedFunction {
        function: f,
        index: place.index,
        code,
    })
}

fn verify_tree(
    f: &Function,
    parent: Option<ParentCounts>,
    counter: &mut u32,
    depth: u32,
) -> Result<(), VerifyError> {
    if depth > MAX_FUNCTION_DEPTH {
        return Err(VerifyError::NestedTooDeep {
            depth,
            max: MAX_FUNCTION_DEPTH,
        });
    }
    let func = *counter;
    *counter += 1;

    check_function(
        f,
        Place {
            index: func,
            depth,
            children: f.children.len() as u32,
            parent,
        },
    )?;

    let counts = ParentCounts {
        reg_count: f.reg_count,
        upvalues: f.upvalues.len() as u32,
    };
    for c in &f.children {
        verify_tree(c, Some(counts), counter, depth + 1)?;
    }

    Ok(())
}

/// Every rule of one function; the decoded table they read.
fn check_function(f: &Function, place: Place) -> Result<Vec<Option<DecodedInstr>>, VerifyError> {
    let func = place.index;
    if f.instrs.is_empty() {
        return Err(VerifyError::EmptyFunction { func });
    }
    if f.reg_count > MAX_REG_COUNT {
        return Err(VerifyError::FrameTooLarge {
            func,
            reg_count: f.reg_count,
            max: MAX_REG_COUNT,
        });
    }
    if (f.arity as u32) > f.reg_count {
        return Err(VerifyError::ArityExceedsRegCount { func });
    }

    verify_upvalues(func, &f.upvalues, place.parent)?;

    // One pass over the instruction stream, so that WIDE pairs and
    // unknown opcodes are caught first and the boundary set and the
    // decoded table every rule reads are built once.
    let (pcs, code) = match decode_table(&f.instrs) {
        Ok(t) => t,
        Err(DecodeError::BadOpcode { pc, byte }) => {
            return Err(VerifyError::BadOpcode { func, pc, byte })
        }
        Err(DecodeError::DanglingWide { pc }) => {
            return Err(VerifyError::WideDangling { func, pc })
        }
        Err(DecodeError::DoubleWide { pc }) => return Err(VerifyError::WideDangling { func, pc }),
        Err(DecodeError::Eof { pc }) => {
            return Err(VerifyError::JumpOutOfRange {
                func,
                pc,
                target: pc as i64,
                len: f.instrs.len() as u32,
            })
        }
    };

    verify_handlers(func, f, &pcs)?;
    verify_regions(func, f, &pcs)?;
    verify_line_table(func, f, &pcs)?;
    verify_instructions(func, f, place.children, &pcs, &code)?;
    verify_burn_rows(func, f, &pcs, &code)?;
    verify_termination(func, &pcs, &code)?;
    verify_charged_cycles(func, f, &code)?;
    verify_reads_after_calls(func, f, &code)?;

    Ok(code)
}

/// The upvalue rule on its own: each descriptor inside its parent's
/// registers or upvalues, and none at the top level. It is the one rule
/// that reads another function's counts; a reader that defers the other
/// rules to a function's first call can check this one for every function
/// when it opens the module.
pub fn verify_upvalues(
    func: u32,
    upvalues: &[UpvalDesc],
    parent: Option<ParentCounts>,
) -> Result<(), VerifyError> {
    let Some(parent) = parent else {
        // The top-level function has no parent, so it must have no
        // upvalues.
        if !upvalues.is_empty() {
            return Err(VerifyError::UpvalueCaptureOutOfRange {
                func,
                upvalue: 0,
                max: 0,
            });
        }
        return Ok(());
    };
    for (i, u) in upvalues.iter().enumerate() {
        let (bound, idx) = match u.kind {
            UpvalKind::ParentLocal => (parent.reg_count, u.index as u32),
            UpvalKind::ParentUpvalue => (parent.upvalues, u.index as u32),
        };
        if idx >= bound {
            return Err(VerifyError::UpvalueCaptureOutOfRange {
                func,
                upvalue: i as u32,
                max: bound,
            });
        }
    }
    Ok(())
}

// ---- handlers --------------------------------------------------------

fn verify_handlers(func: u32, f: &Function, pcs: &FirstWordPcs) -> Result<(), VerifyError> {
    let n_instr = pcs.len();
    for (i, h) in f.handlers.iter().enumerate() {
        let shape_ok = h.pc_start < h.pc_end
            && h.pc_end <= n_instr
            && h.handler_pc < n_instr
            && (h.catch_reg == RESERVED_U32 || h.catch_reg < f.reg_count)
            && (h.finally_pc == RESERVED_U32 || h.finally_pc < n_instr);
        if !shape_ok {
            return Err(VerifyError::HandlerBadRange {
                func,
                handler: i as u32,
            });
        }
        if !pcs.contains(h.pc_start) {
            return Err(VerifyError::MidPairReference {
                func,
                at: "handler.pc_start",
                pc: h.pc_start,
            });
        }
        if !pcs.is_boundary(h.pc_end) {
            return Err(VerifyError::MidPairReference {
                func,
                at: "handler.pc_end",
                pc: h.pc_end,
            });
        }
        if !pcs.contains(h.handler_pc) {
            return Err(VerifyError::MidPairReference {
                func,
                at: "handler.handler_pc",
                pc: h.handler_pc,
            });
        }
        if h.finally_pc != RESERVED_U32 && !pcs.contains(h.finally_pc) {
            return Err(VerifyError::MidPairReference {
                func,
                at: "handler.finally_pc",
                pc: h.finally_pc,
            });
        }
    }
    for i in 0..f.handlers.len() {
        for j in (i + 1)..f.handlers.len() {
            let a = &f.handlers[i];
            let b = &f.handlers[j];
            let disjoint = a.pc_end <= b.pc_start || b.pc_end <= a.pc_start;
            let a_contains_b = a.pc_start <= b.pc_start && b.pc_end <= a.pc_end;
            let b_contains_a = b.pc_start <= a.pc_start && a.pc_end <= b.pc_end;
            if !(disjoint || a_contains_b || b_contains_a) {
                return Err(VerifyError::HandlerRangesInterleave {
                    func,
                    a: i as u32,
                    b: j as u32,
                });
            }
        }
    }
    Ok(())
}

// ---- line table ------------------------------------------------------

fn verify_line_table(func: u32, f: &Function, pcs: &FirstWordPcs) -> Result<(), VerifyError> {
    let n_instr = pcs.len();
    let mut last_pc: Option<u32> = None;
    for (i, l) in f.lines.iter().enumerate() {
        if l.pc >= n_instr || last_pc.is_some_and(|p| p >= l.pc) {
            return Err(VerifyError::LineTableBadRow {
                func,
                row: i as u32,
            });
        }
        if !pcs.contains(l.pc) {
            return Err(VerifyError::MidPairReference {
                func,
                at: "line.pc",
                pc: l.pc,
            });
        }
        last_pc = Some(l.pc);
    }
    Ok(())
}

// ---- regions ---------------------------------------------------------

fn verify_regions(func: u32, f: &Function, pcs: &FirstWordPcs) -> Result<(), VerifyError> {
    let n_instr = pcs.len();
    let n_regions = f.regions.len() as u32;
    for (i, r) in f.region_rows.iter().enumerate() {
        if !(r.pc_start < r.pc_end && r.pc_end <= n_instr) {
            return Err(VerifyError::RegionBadRange {
                func,
                row: i as u32,
            });
        }
        if r.region != RESERVED_U32 && r.region >= n_regions {
            return Err(VerifyError::RegionOutOfRange {
                func,
                row: i as u32,
                index: r.region,
                max: n_regions,
            });
        }
        if !pcs.contains(r.pc_start) {
            return Err(VerifyError::MidPairReference {
                func,
                at: "region.pc_start",
                pc: r.pc_start,
            });
        }
        if !pcs.is_boundary(r.pc_end) {
            return Err(VerifyError::MidPairReference {
                func,
                at: "region.pc_end",
                pc: r.pc_end,
            });
        }
    }
    Ok(())
}

// ---- instruction stream ---------------------------------------------

fn verify_instructions(
    func: u32,
    f: &Function,
    n_func: u32,
    pcs: &FirstWordPcs,
    code: &[Option<DecodedInstr>],
) -> Result<(), VerifyError> {
    let n_const = f.constants.len() as u32;
    let n_regions = f.regions.len() as u32;

    for d in code.iter().flatten() {
        let info = d.op.info();

        verify_operand_a(func, d, info.a, f, n_const, n_func, n_regions)?;
        verify_operand_b(func, d, info, f, n_const, n_func, n_regions, pcs)?;
        verify_operand_c(func, d, info, f, n_const, pcs)?;

        verify_reg_ranges(func, d, f)?;

        if d.op == Op::BURN_ENTER {
            // The operand checks above put `Bx` inside the region table.
            if let Some(region) = f.regions.get(d.b_u as usize) {
                if u32::from(region.class) != d.a {
                    return Err(VerifyError::BurnClassMismatch { func, pc: d.pc });
                }
                if let RegionLabel::Reg(r) = region.label {
                    if r >= f.reg_count {
                        return Err(VerifyError::RegisterOutOfRange {
                            func,
                            pc: d.pc,
                            reg: r,
                            max: f.reg_count,
                        });
                    }
                }
            }
        }
    }

    Ok(())
}

fn verify_operand_a(
    func: u32,
    d: &DecodedInstr,
    kind: OperandKind,
    f: &Function,
    n_const: u32,
    n_func: u32,
    n_regions: u32,
) -> Result<(), VerifyError> {
    check_scalar(func, d, kind, d.a, f, n_const, n_func, n_regions, false)
}

#[allow(clippy::too_many_arguments)]
fn verify_operand_b(
    func: u32,
    d: &DecodedInstr,
    info: crate::opcode::OpInfo,
    f: &Function,
    n_const: u32,
    n_func: u32,
    n_regions: u32,
    pcs: &FirstWordPcs,
) -> Result<(), VerifyError> {
    if info.b == OperandKind::Jump {
        return check_jump(func, d, d.b_s as i64, pcs);
    }
    // Choose the unsigned or signed reading per operand kind.
    let value = match info.fmt {
        Fmt::ABC => d.b_u,
        Fmt::ABx => d.b_u,
        Fmt::AsBx => d.b_u, // the kind-specific check doesn't care
    };
    check_scalar(func, d, info.b, value, f, n_const, n_func, n_regions, false)
}

fn verify_operand_c(
    func: u32,
    d: &DecodedInstr,
    info: crate::opcode::OpInfo,
    f: &Function,
    n_const: u32,
    pcs: &FirstWordPcs,
) -> Result<(), VerifyError> {
    if info.c == OperandKind::Jump {
        return check_jump(func, d, d.c_s as i64, pcs);
    }
    check_scalar(func, d, info.c, d.c, f, n_const, 0, 0, false)
}

#[allow(clippy::too_many_arguments)]
fn check_scalar(
    func: u32,
    d: &DecodedInstr,
    kind: OperandKind,
    raw: u32,
    f: &Function,
    n_const: u32,
    n_func: u32,
    n_regions: u32,
    _signed: bool,
) -> Result<(), VerifyError> {
    match kind {
        OperandKind::None
        | OperandKind::Immediate
        | OperandKind::Any
        | OperandKind::Jump
        | OperandKind::Wide
        | OperandKind::Builtin
        | OperandKind::Arity
        | OperandKind::RegRange
        | OperandKind::RegRangeCount
        | OperandKind::BurnOpPacked
        // `burn.enter`'s class is checked against its region's class.
        | OperandKind::BurnClass => Ok(()),
        OperandKind::Reg => {
            if raw >= f.reg_count {
                return Err(VerifyError::RegisterOutOfRange {
                    func,
                    pc: d.pc,
                    reg: raw,
                    max: f.reg_count,
                });
            }
            Ok(())
        }
        OperandKind::Const => {
            if raw >= n_const {
                return Err(VerifyError::ConstOutOfRange {
                    func,
                    pc: d.pc,
                    index: raw,
                    max: n_const,
                });
            }
            Ok(())
        }
        OperandKind::StrConst => {
            if raw >= n_const {
                return Err(VerifyError::ConstOutOfRange {
                    func,
                    pc: d.pc,
                    index: raw,
                    max: n_const,
                });
            }
            if !matches!(&f.constants[raw as usize], Constant::Str(_)) {
                return Err(VerifyError::ConstNotString {
                    func,
                    pc: d.pc,
                    index: raw,
                });
            }
            Ok(())
        }
        OperandKind::Func => {
            if raw >= n_func {
                return Err(VerifyError::FuncOutOfRange {
                    func,
                    pc: d.pc,
                    index: raw,
                    max: n_func,
                });
            }
            Ok(())
        }
        OperandKind::Upvalue => {
            let n = f.upvalues.len() as u32;
            if raw >= n {
                return Err(VerifyError::UpvalueOutOfRange {
                    func,
                    pc: d.pc,
                    index: raw,
                    max: n,
                });
            }
            Ok(())
        }
        OperandKind::Handler => {
            let n = f.handlers.len() as u32;
            if raw >= n {
                return Err(VerifyError::HandlerOutOfRange {
                    func,
                    pc: d.pc,
                    index: raw,
                    max: n,
                });
            }
            Ok(())
        }
        OperandKind::Region => {
            if raw >= n_regions {
                return Err(VerifyError::RegionOutOfRange {
                    func,
                    row: d.pc,
                    index: raw,
                    max: n_regions,
                });
            }
            Ok(())
        }
    }
}

fn check_jump(
    func: u32,
    d: &DecodedInstr,
    offset: i64,
    pcs: &FirstWordPcs,
) -> Result<(), VerifyError> {
    // `pc` is the first-word pc; the jump base per the spec is also
    // the first-word pc of the (possibly prefixed) instruction, so a
    // `WIDE`+`JMP` pair measures its offset from the `WIDE` word.
    let target_i64 = (d.pc as i64) + 1 + offset;
    if target_i64 < 0 || target_i64 >= pcs.len() as i64 {
        return Err(VerifyError::JumpOutOfRange {
            func,
            pc: d.pc,
            target: target_i64,
            len: pcs.len(),
        });
    }
    let target = target_i64 as u32;
    if !pcs.contains(target) {
        return Err(VerifyError::MidPairReference {
            func,
            at: "jump.target",
            pc: target,
        });
    }
    Ok(())
}

/// A "role" is one half-open register window the opcode accesses:
/// `[base, base + count)`. Reads and writes are both checked.
///
/// The list for each opcode is walked row-by-row against
/// `docs/BYTECODE.md` and is the single source of truth for which
/// registers the VM may touch.
struct Role {
    base: u32,
    count: u32,
    /// `true` if this role is a call's args + callee bundle, so the
    /// error reads as `call-arity-too-high` rather than the generic
    /// register-range error.
    call_shape: bool,
}
impl Role {
    fn new(base: u32, count: u32) -> Self {
        Self {
            base,
            count,
            call_shape: false,
        }
    }
    fn call(base: u32, count: u32) -> Self {
        Self {
            base,
            count,
            call_shape: true,
        }
    }
}

fn verify_reg_ranges(func: u32, d: &DecodedInstr, f: &Function) -> Result<(), VerifyError> {
    // Per-opcode roles, by what the spec says each one does.
    //
    // Every opcode that names a register range (via `RegRange`,
    // `RegRangeCount`, or a per-op convention like FORPREP's 4-slot
    // state) is listed here. Rows for opcodes whose only register
    // operands are plain `Reg` (A, B, C) are left to `check_scalar`.
    let roles: Vec<Role> = match d.op {
        // `call A, B, C`: callee R(A), args R(A+1..A+1+B), results
        // R(A..A+C+1). The widest access is max(B, C) + 1 regs from A.
        Op::CALL => vec![
            Role::call(d.a, d.b_u + 1), // callee + args
            Role::new(d.a, d.c + 1),    // results (never lower than 1)
        ],
        // `callb A, B, C`: builtin B, args R(A+1..A+1+C), result R(A).
        // One role covers the C args plus the single-reg result.
        Op::CALLB => vec![Role::call(d.a, d.c + 1)],
        // `callm A, K, C`: recv R(A), args R(A+1..A+1+C), result R(A).
        Op::CALLM => vec![Role::call(d.a, d.c + 1)],
        // `tailcall A, B`: callee R(A), args R(A+1..A+1+B); no result
        // written here (it leaves the frame).
        Op::TAILCALL => vec![Role::call(d.a, d.b_u + 1)],
        // `pack A, B`: packs R(A..A+B).
        Op::PACK => vec![Role::new(d.a, d.b_u)],
        // `exhale A, B`: exhales R(A..A+B).
        Op::EXHALE => vec![Role::new(d.a, d.b_u)],
        // `chain.make A, B`: result R(A); step regs R(A+1..A+1+B).
        Op::CHAIN_MAKE => vec![Role::new(d.a, d.b_u + 1)],
        // `interp A, B, C`: result R(A); parts R(B..B+C+1).
        Op::INTERP => vec![Role::new(d.a, 1), Role::new(d.b_u, d.c + 1)],
        // `burn.op A, K, packed`: result R(A); args R(A+1..A+1+argc).
        // Argc sits in the high nibble of C unwidened, or in the high
        // byte of C when a WIDE prefix widens it.
        Op::BURN_OP => {
            let argc = if d.wide { d.c >> 8 } else { d.c >> 4 };
            vec![Role::new(d.a, argc + 1)]
        }
        // `forprep`/`forloop A, sBx`: the state is a 4-slot window
        // R(A..A+4) (start, limit, step, induction variable).
        Op::FORPREP | Op::FORLOOP => vec![Role::new(d.a, 4)],
        // `iterprep A` and `iternext A, sBx`: 4-slot window R(A..A+4)
        // (source, state, control, item).
        Op::ITERPREP | Op::ITERNEXT => vec![Role::new(d.a, 4)],
        _ => Vec::new(),
    };

    for role in &roles {
        let end = role.base.saturating_add(role.count);
        if end > f.reg_count {
            if role.call_shape && role.base < f.reg_count {
                return Err(VerifyError::CallArityTooHigh {
                    func,
                    pc: d.pc,
                    given: role.count.saturating_sub(1),
                    max: f.reg_count.saturating_sub(role.base + 1),
                });
            }
            return Err(VerifyError::RegisterRangeOutOfRange {
                func,
                pc: d.pc,
                base: role.base,
                count: role.count,
                max: f.reg_count,
            });
        }
    }
    Ok(())
}

// ---- burn rows -------------------------------------------------------

/// The burn rules. A **burn row** is a region row that names a region (not
/// the sentinel): the pcs of one burn, from its `burn.enter` to the pc
/// after its last instruction. The rules are on the rows, so a burn left
/// by a jump (`comp.leave`, `burn.leave`, then the jump) needs nothing
/// after the jump, and a body that ends in a terminator needs no
/// `burn.leave` after it:
///
/// - `rows-interleave`: burn rows nest (two rows are disjoint or one holds
///   the other) and no two start at the same pc;
/// - `row-without-enter`: a row starts with a `burn.enter` of its region;
/// - `row-falls-out`: a row's last instruction is a `burn.leave` or does
///   not fall through (`ret`, `tailcall`, `throw`, `jmp`, `loop`);
/// - `enter-outside-row`: a `burn.enter` lies inside a row of its region
///   (at its start, or anywhere in it: a re-entry no run reaches);
/// - `leave-outside-row`: a run of `burn.leave`s, with only `comp.leave`
///   between them, is no longer than the number of rows around its first;
/// - `jump-into-row`: a jump operand, or a handler's `handler_pc` or
///   `finally_pc`, lands inside a row only at its start, or from inside it
///   (the jump's pc, the handler's whole range);
/// - `burn-op-outside-region`: a `burn.op` lies inside a row, or the
///   function is `requires_burn`.
///
/// These refuse a malformed module early; the VM's burn stack at run time
/// stays the guarantee. Linear in the function's size, after a sort of
/// the rows.
fn verify_burn_rows(
    func: u32,
    f: &Function,
    pcs: &FirstWordPcs,
    code: &[Option<DecodedInstr>],
) -> Result<(), VerifyError> {
    let n = f.instrs.len();
    let unbalanced = |pc: u32, why: &'static str| VerifyError::BurnUnbalanced { func, pc, why };
    let at = |pc: u32| code.get(pc as usize).copied().flatten();

    // The rows, outer first; each one's parent, the row around it.
    let mut rows: Vec<RegionRow> = f
        .region_rows
        .iter()
        .copied()
        .filter(|r| r.region != RESERVED_U32)
        .collect();
    rows.sort_by_key(|r| (r.pc_start, core::cmp::Reverse(r.pc_end)));
    let mut parent: Vec<Option<usize>> = vec![None; rows.len()];
    let mut open: Vec<usize> = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        if i > 0 && rows[i - 1].pc_start == r.pc_start {
            return Err(unbalanced(r.pc_start, "rows-interleave"));
        }
        while open.last().is_some_and(|&o| rows[o].pc_end <= r.pc_start) {
            open.pop();
        }
        if open.last().is_some_and(|&o| rows[o].pc_end < r.pc_end) {
            return Err(unbalanced(r.pc_start, "rows-interleave"));
        }
        parent[i] = open.last().copied();
        open.push(i);
    }

    for r in &rows {
        if !at(r.pc_start).is_some_and(|d| d.op == Op::BURN_ENTER && d.b_u == r.region) {
            return Err(unbalanced(r.pc_start, "row-without-enter"));
        }
        let last = pcs.last_before(r.pc_end).and_then(at);
        let closed = last.is_some_and(|d| {
            matches!(
                d.op,
                Op::BURN_LEAVE | Op::RET | Op::TAILCALL | Op::THROW | Op::JMP | Op::LOOP
            )
        });
        if !closed {
            let pc = last.map_or(r.pc_start, |d| d.pc);
            return Err(unbalanced(pc, "row-falls-out"));
        }
    }

    // Each pc's innermost row, and how many rows hold it.
    let mut inner: Vec<Option<usize>> = vec![None; n];
    let mut depth: Vec<u32> = vec![0; n];
    open.clear();
    let mut next = 0usize;
    for pc in 0..n {
        let p = pc as u32;
        while open.last().is_some_and(|&o| rows[o].pc_end <= p) {
            open.pop();
        }
        if rows.get(next).is_some_and(|r| r.pc_start == p) {
            open.push(next);
            next += 1;
        }
        inner[pc] = open.last().copied();
        depth[pc] = open.len() as u32;
    }

    // Each region's pcs, as sorted disjoint spans: the union of its rows.
    let mut spans: Vec<Vec<(u32, u32)>> = vec![Vec::new(); f.regions.len()];
    for r in &rows {
        if let Some(s) = spans.get_mut(r.region as usize) {
            match s.last_mut() {
                Some(last) if r.pc_start < last.1 => last.1 = last.1.max(r.pc_end),
                _ => s.push((r.pc_start, r.pc_end)),
            }
        }
    }
    let in_region = |region: u32, pc: u32| {
        spans.get(region as usize).is_some_and(|s| {
            let i = s.partition_point(|&(start, _)| start <= pc);
            i > 0 && s.get(i - 1).is_some_and(|&(_, end)| pc < end)
        })
    };

    // The row a landing at `t` must come from inside: the innermost row
    // holding `t`, or the one around it when `t` is that row's start.
    let entered = |t: u32| -> Option<usize> {
        let r = inner.get(t as usize).copied().flatten()?;
        if rows[r].pc_start == t {
            parent[r]
        } else {
            Some(r)
        }
    };

    let mut leaves = 0u32;
    let mut room = 0u32;
    for pc in pcs.iter() {
        let Some(d) = at(pc) else { continue };
        let here = depth.get(pc as usize).copied().unwrap_or(0);
        match d.op {
            Op::BURN_ENTER if !in_region(d.b_u, pc) => {
                return Err(unbalanced(pc, "enter-outside-row"));
            }
            Op::BURN_LEAVE => {
                if leaves == 0 {
                    room = here;
                }
                leaves += 1;
                if leaves > room {
                    return Err(unbalanced(pc, "leave-outside-row"));
                }
            }
            Op::BURN_OP if here == 0 && !f.requires_burn => {
                return Err(VerifyError::BurnOpOutsideRegion { func, pc });
            }
            _ => {}
        }
        if !matches!(d.op, Op::BURN_LEAVE | Op::COMP_LEAVE) {
            leaves = 0;
        }
        let info = d.op.info();
        let targets = [
            (info.b == OperandKind::Jump).then(|| i64::from(pc) + 1 + i64::from(d.b_s)),
            (info.c == OperandKind::Jump).then(|| i64::from(pc) + 1 + i64::from(d.c_s)),
        ];
        for t in targets.into_iter().flatten() {
            let Ok(t) = u32::try_from(t) else { continue };
            if let Some(r) = entered(t) {
                if !(rows[r].pc_start <= pc && pc < rows[r].pc_end) {
                    return Err(unbalanced(pc, "jump-into-row"));
                }
            }
        }
    }

    for h in &f.handlers {
        for t in [h.handler_pc, h.finally_pc] {
            if t == RESERVED_U32 {
                continue;
            }
            if let Some(r) = entered(t) {
                if !(rows[r].pc_start <= h.pc_start && h.pc_end <= rows[r].pc_end) {
                    return Err(unbalanced(t, "jump-into-row"));
                }
            }
        }
    }
    Ok(())
}

// ---- termination -----------------------------------------------------

fn verify_termination(
    func: u32,
    pcs: &FirstWordPcs,
    code: &[Option<DecodedInstr>],
) -> Result<(), VerifyError> {
    // The last decoded instruction in the function must be a
    // terminator, or a JMP (whose target the jump rule has already
    // checked).
    let Some(last_pc) = pcs.last_start() else {
        return Err(VerifyError::NotTerminated { func });
    };
    let Some(Some(d)) = code.get(last_pc as usize) else {
        return Err(VerifyError::NotTerminated { func });
    };
    match d.op {
        op if op.is_terminator() => Ok(()),
        Op::JMP => Ok(()),
        _ => Err(VerifyError::NotTerminated { func }),
    }
}

// ---- every cycle is charged ------------------------------------------

/// Within a function, the instructions that are not a `steps` with
/// `Bx >= 1` form no cycle: every way back to an instruction already run
/// passes a `steps` that charges the step budget.
///
/// The graph's nodes are the function's pcs plus one node per handler.
/// Its edges are every transfer the VM can make: the fall-through to the
/// next instruction (except after `ret`, `tailcall`, `throw`, `jmp` and
/// `loop`), each jump operand (the plain and fused jumps, `forprep`,
/// `forloop`, `iternext`), and an edge from every pc inside a handler's
/// range to that handler's node, which leads to its `handler_pc`, its
/// `finally_pc` and the next handler out. A pc reaches only its innermost
/// handler's node, and the nodes chain outward, so the edge count stays
/// linear in the function's size.
///
/// A charging `steps` ends every path through it but one: once the budget
/// is spent it raises every time it runs, so inside a handler's range it
/// keeps its edge to that handler. A handler that leads back into its own
/// range therefore needs a charge outside the range on the way.
///
/// The check peels off every node with no remaining predecessor (Kahn's
/// algorithm, with a queue: no recursion).
/// Whatever is left lies on or after a cycle; walking predecessors from it
/// lands on one, whose lowest pc is reported.
fn verify_charged_cycles(
    func: u32,
    f: &Function,
    code: &[Option<DecodedInstr>],
) -> Result<(), VerifyError> {
    let n = f.instrs.len();
    let charges = |d: &DecodedInstr| d.op == Op::STEPS && d.b_u >= 1;

    // Handlers by (start ascending, end descending): an outer one comes
    // before the ones it contains (they nest: verified above). One sweep
    // gives each pc its innermost handler and each handler its parent.
    let n_handlers = f.handlers.len();
    let mut order: Vec<usize> = (0..n_handlers).collect();
    order.sort_by_key(|&i| {
        let h = &f.handlers[i];
        (h.pc_start, core::cmp::Reverse(h.pc_end), i)
    });
    let mut inner: Vec<Option<usize>> = vec![None; n];
    let mut parent: Vec<Option<usize>> = vec![None; n_handlers];
    let mut open: Vec<usize> = Vec::new();
    let mut next = order.iter().peekable();
    for (pc, slot) in inner.iter_mut().enumerate() {
        let pc = pc as u32;
        while open.last().is_some_and(|&h| f.handlers[h].pc_end <= pc) {
            open.pop();
        }
        while let Some(&&h) = next.peek() {
            if f.handlers[h].pc_start != pc {
                break;
            }
            next.next();
            while open.last().is_some_and(|&o| f.handlers[o].pc_end <= pc) {
                open.pop();
            }
            parent[h] = open.last().copied();
            open.push(h);
        }
        *slot = open.last().copied();
    }

    let total = n + n_handlers;
    // A node's successors, at most three: pc nodes have a fall-through,
    // one jump and one handler; handler nodes have handler_pc, finally_pc
    // and the parent handler. A charging `steps` has only its handler.
    let live = |u: usize| match code.get(u) {
        Some(Some(_)) => true,
        Some(None) => false,
        None => u < total,
    };
    let succ = |u: usize, out: &mut Vec<usize>| {
        out.clear();
        if let Some(slot) = code.get(u) {
            let Some(d) = slot else { return };
            if charges(d) {
                if let Some(Some(h)) = inner.get(u) {
                    out.push(n + h);
                }
                return;
            }
            let pc = i64::from(d.pc);
            let ends = matches!(
                d.op,
                Op::RET | Op::TAILCALL | Op::THROW | Op::JMP | Op::LOOP
            );
            let info = d.op.info();
            let targets = [
                (!ends).then(|| pc + i64::from(d.width)),
                (info.b == OperandKind::Jump).then(|| pc + 1 + i64::from(d.b_s)),
                (info.c == OperandKind::Jump).then(|| pc + 1 + i64::from(d.c_s)),
            ];
            for t in targets.into_iter().flatten() {
                if (0..n as i64).contains(&t) {
                    out.push(t as usize);
                }
            }
            if let Some(Some(h)) = inner.get(u) {
                out.push(n + h);
            }
        } else if let Some(h) = f.handlers.get(u - n) {
            out.push(h.handler_pc as usize);
            if h.finally_pc != RESERVED_U32 {
                out.push(h.finally_pc as usize);
            }
            if let Some(Some(p)) = parent.get(u - n) {
                out.push(n + p);
            }
        }
        out.retain(|&v| v < total && live(v));
    };

    let mut indeg = vec![0u32; total];
    let mut buf = Vec::with_capacity(3);
    for u in (0..total).filter(|&u| live(u)) {
        succ(u, &mut buf);
        for &v in &buf {
            indeg[v] += 1;
        }
    }
    let mut queue: std::collections::VecDeque<usize> =
        (0..total).filter(|&u| live(u) && indeg[u] == 0).collect();
    while let Some(u) = queue.pop_front() {
        succ(u, &mut buf);
        for &v in &buf {
            indeg[v] -= 1;
            if indeg[v] == 0 {
                queue.push_back(v);
            }
        }
    }
    let Some(start) = (0..total).find(|&u| live(u) && indeg[u] > 0) else {
        return Ok(());
    };

    // What is left: each node has a predecessor that is left too. Walk
    // predecessors past the node count to land on a cycle, then once
    // round it for its lowest pc.
    let left = |u: usize| live(u) && indeg[u] > 0;
    let mut pred: Vec<Vec<usize>> = vec![Vec::new(); total];
    for u in (0..total).filter(|&u| left(u)) {
        succ(u, &mut buf);
        for &v in buf.iter().filter(|&&v| left(v)) {
            pred[v].push(u);
        }
    }
    let back = |u: usize| pred[u].first().copied().unwrap_or(u);
    let mut on_cycle = start;
    for _ in 0..=total {
        on_cycle = back(on_cycle);
    }
    let mut lowest = on_cycle;
    let mut u = back(on_cycle);
    while u != on_cycle {
        if u < n && (lowest >= n || u < lowest) {
            lowest = u;
        }
        u = back(u);
    }
    Err(VerifyError::UnchargedCycle {
        func,
        pc: lowest as u32,
    })
}

/// The registers an instruction reads, whichever way it goes on: pushed
/// onto `out` (cleared first). `f` is the instruction's function (a
/// `closure` reads the registers its child captures, a `burn.enter` its
/// register label, a `comp.enter` its state register when it names one).
fn reads(d: &DecodedInstr, f: &Function, out: &mut Vec<u32>) {
    out.clear();
    let (a, b, c) = (d.a, d.b_u, d.c);
    let range = |out: &mut Vec<u32>, first: u32, n: u32| out.extend(first..first.saturating_add(n));
    match d.op {
        Op::MOV | Op::NEG | Op::NOT | Op::TESTSET | Op::GETFIELD => out.push(b),
        Op::ADD
        | Op::SUB
        | Op::MUL
        | Op::DIV
        | Op::MOD
        | Op::POW
        | Op::EQ
        | Op::LT
        | Op::LE
        | Op::GETINDEX => out.extend([b, c]),
        Op::ADDK
        | Op::SUBK
        | Op::MULK
        | Op::DIVK
        | Op::MODK
        | Op::POWK
        | Op::EQK
        | Op::LTK
        | Op::LEK => out.push(b),
        Op::EQJ | Op::NEJ | Op::LTJ | Op::LEJ | Op::APPENDLIST => out.extend([a, b]),
        Op::TEST | Op::TESTN | Op::RET | Op::THROW | Op::CELLNEW | Op::ITERPREP => out.push(a),
        Op::FORPREP | Op::FORLOOP => range(out, a, 3),
        Op::ITERNEXT => range(out, a.saturating_add(1), 2),
        Op::SETFIELD => out.extend([a, c]),
        Op::SETINDEX | Op::INSERTMAP => out.extend([a, b, c]),
        Op::INTERP => range(out, b, c.saturating_add(1)),
        Op::CALL | Op::TAILCALL => range(out, a, b.saturating_add(1)),
        Op::CALLB => range(out, a.saturating_add(1), c),
        Op::CALLM => range(out, a, c.saturating_add(1)),
        Op::SETUP | Op::CELLSET => out.push(b),
        Op::CLOSURE => {
            if let Some(child) = f.children.get(b as usize) {
                for u in &child.upvalues {
                    if u.kind == UpvalKind::ParentLocal {
                        out.push(u32::from(u.index));
                    }
                }
            }
        }
        Op::BURN_ENTER => {
            if let Some(RegionLabel::Reg(r)) = f.regions.get(b as usize).map(|g| g.label) {
                out.push(r);
            }
        }
        Op::BURN_OP => {
            let argc = if d.wide { c >> 8 } else { c >> 4 };
            range(out, a.saturating_add(1), argc);
        }
        Op::COMP_ENTER => {
            if matches!(f.constants.get(c as usize), Some(Constant::Str(n)) if !n.is_empty()) {
                out.push(a);
            }
        }
        Op::PACK | Op::EXHALE => range(out, a, b),
        Op::CHAIN_MAKE => range(out, a.saturating_add(1), b),
        _ => {}
    }
}

/// The register an instruction writes on every way on but its jump, if
/// any: `R(A)` for the instructions that make a value there.
fn writes_a(op: Op) -> bool {
    matches!(
        op,
        Op::MOV
            | Op::LOADK
            | Op::LOADNULL
            | Op::LOADBOOL
            | Op::LOADINT
            | Op::LOADG
            | Op::ADD
            | Op::SUB
            | Op::MUL
            | Op::DIV
            | Op::MOD
            | Op::POW
            | Op::NEG
            | Op::ADDK
            | Op::SUBK
            | Op::MULK
            | Op::DIVK
            | Op::MODK
            | Op::POWK
            | Op::EQ
            | Op::LT
            | Op::LE
            | Op::NOT
            | Op::EQK
            | Op::LTK
            | Op::LEK
            | Op::NEWLIST
            | Op::NEWMAP
            | Op::GETFIELD
            | Op::GETINDEX
            | Op::INTERP
            | Op::CALLB
            | Op::CALLM
            | Op::CLOSURE
            | Op::GETUP
            | Op::CELLGET
            | Op::BURN_OP
            | Op::CHAIN_MAKE
    )
}

/// A try the run has open at a pc, as the VM keeps it: the handler, the
/// stage, and for a finally what goes on when it ends.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct OpenTry {
    handler: u32,
    stage: u8,
    /// For a finally: the pc it goes on at, or [`RAISE`].
    pending: u32,
}

const BODY: u8 = 0;
const CATCH: u8 = 1;
const FINALLY: u8 = 2;
/// A finally's pending: raise its error again.
const RAISE: u32 = u32::MAX;

/// The registers that may be undefined at a pc, from `lo` up: one bit
/// each.
type Undef = Vec<u64>;

/// A hasher for the pass's small integer keys: the standard one's
/// SipHash showed in cigo's verify profile.
#[derive(Default)]
struct Mix(u64);

impl core::hash::Hasher for Mix {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(u64::from(b));
        }
    }
    fn write_u32(&mut self, x: u32) {
        self.write_u64(u64::from(x));
    }
    fn write_u64(&mut self, x: u64) {
        self.0 = (self.0.rotate_left(5) ^ x).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
    fn write_usize(&mut self, x: usize) {
        self.write_u64(x as u64);
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

type MixMap<K, V> = std::collections::HashMap<K, V, core::hash::BuildHasherDefault<Mix>>;

struct Reads<'f> {
    f: &'f Function,
    lo: u32,
    /// The open tries each state is under, by number.
    stacks: Vec<Vec<OpenTry>>,
    stack_of: MixMap<Vec<OpenTry>, u32>,
    /// A state per (pc, stack) the run reaches.
    state_of: MixMap<(u32, u32), usize>,
    states: Vec<(u32, u32, Undef)>,
    queued: Vec<bool>,
    work: Vec<usize>,
    words: usize,
}

impl Reads<'_> {
    fn stack(&mut self, s: Vec<OpenTry>) -> u32 {
        if let Some(&i) = self.stack_of.get(&s) {
            return i;
        }
        let i = self.stacks.len() as u32;
        self.stacks.push(s.clone());
        self.stack_of.insert(s, i);
        i
    }

    /// Join `u` into the state at `(pc, stack)`, queueing it if it grew.
    fn flow(&mut self, pc: u32, stack: u32, u: &Undef) {
        let i = match self.state_of.get(&(pc, stack)) {
            Some(&i) => {
                let mut grew = false;
                for (w, x) in self.states[i].2.iter_mut().zip(u) {
                    let y = *w | x;
                    grew |= y != *w;
                    *w = y;
                }
                if !grew {
                    return;
                }
                i
            }
            None => {
                let i = self.states.len();
                self.states.push((pc, stack, u.clone()));
                self.state_of.insert((pc, stack), i);
                self.queued.push(false);
                i
            }
        };
        if !self.queued[i] {
            self.queued[i] = true;
            self.work.push(i);
        }
    }

    fn set(&self, u: &mut Undef, r: u32) {
        if r >= self.lo {
            let i = (r - self.lo) as usize;
            if let Some(w) = u.get_mut(i / 64) {
                *w |= 1 << (i % 64);
            }
        }
    }

    fn clear(&self, u: &mut Undef, r: u32) {
        if r >= self.lo {
            let i = (r - self.lo) as usize;
            if let Some(w) = u.get_mut(i / 64) {
                *w &= !(1 << (i % 64));
            }
        }
    }

    fn has(&self, u: &Undef, r: u32) -> bool {
        r >= self.lo && {
            let i = (r - self.lo) as usize;
            u.get(i / 64).is_some_and(|w| w & (1 << (i % 64)) != 0)
        }
    }

    /// An error raised at `pc` under `stack`, offered to the open tries
    /// innermost first exactly as the VM's `offer` does: the states it
    /// can go on in. Both ways of a catch are taken (with `exit()` asked
    /// for, a catch takes nothing).
    fn raise(&mut self, pc: u32, stack: u32, u: &Undef) {
        let mut open = self.stacks[stack as usize].clone();
        while let Some(t) = open.pop() {
            let Some(h) = self.f.handlers.get(t.handler as usize) else {
                return;
            };
            let in_range = (h.pc_start..h.pc_end).contains(&pc);
            let catches = h.handler_pc != h.finally_pc;
            let finally = h.finally_pc != RESERVED_U32;
            if t.stage == BODY && !in_range {
                continue;
            }
            if t.stage == BODY && catches {
                let mut caught = u.clone();
                if h.catch_reg != RESERVED_U32 {
                    self.clear(&mut caught, h.catch_reg);
                }
                let mut s = open.clone();
                s.push(OpenTry { stage: CATCH, ..t });
                let s = self.stack(s);
                self.flow(h.handler_pc, s, &caught);
            }
            if (t.stage == BODY || t.stage == CATCH) && finally {
                let mut s = open.clone();
                s.push(OpenTry {
                    stage: FINALLY,
                    pending: RAISE,
                    ..t
                });
                let s = self.stack(s);
                self.flow(h.finally_pc, s, u);
                return;
            }
        }
    }
}

/// `read-before-write`: no instruction reads a register a `call` left
/// undefined. After `call A, B, C` the callee's frame started at
/// `R(A+1)`, so every register from `R(A+C+1)` up holds nothing the run
/// may read until an instruction writes it (the result row
/// `R(A..A+C+1)` is written by the call). The rule follows every way a
/// run can go, the VM's tries included: an error offered to a catch or
/// a finally (from a call, with the callee's frame having run), a
/// `try.leave` into its finally, and a finally's end back to the pc its
/// `try.leave` goes on at. It is a forward pass to a fixed point over
/// (pc, open tries) states, linear in practice.
fn verify_reads_after_calls(
    func: u32,
    f: &Function,
    code: &[Option<DecodedInstr>],
) -> Result<(), VerifyError> {
    let n = f.instrs.len();
    let mut lo = u32::MAX;
    for d in code.iter().flatten() {
        if d.op == Op::CALL {
            lo = lo.min(d.a.saturating_add(d.c).saturating_add(1));
        }
    }
    // No call, or none that leaves a register of the frame undefined.
    if lo >= f.reg_count {
        return Ok(());
    }
    let words = ((f.reg_count - lo) as usize).div_ceil(64);
    let mut r = Reads {
        f,
        lo,
        stacks: Vec::new(),
        stack_of: MixMap::default(),
        state_of: MixMap::default(),
        states: Vec::new(),
        queued: Vec::new(),
        work: Vec::new(),
        words,
    };
    // A state is kept at the start of each block: pc 0, every jump's
    // target, a handler's pcs and the pc after any instruction that is not
    // `straight` (it branches, ends, or changes the open tries). Inside a
    // block one state is carried along.
    let straight = |op: Op| {
        !matches!(
            op,
            Op::RET
                | Op::TAILCALL
                | Op::THROW
                | Op::JMP
                | Op::LOOP
                | Op::TESTSET
                | Op::EQJ
                | Op::NEJ
                | Op::LTJ
                | Op::LEJ
                | Op::TEST
                | Op::TESTN
                | Op::FORPREP
                | Op::FORLOOP
                | Op::ITERPREP
                | Op::ITERNEXT
                | Op::TRY_ENTER
                | Op::TRY_LEAVE
        )
    };
    let mut leader = vec![false; n + 1];
    leader[0] = true;
    for d in code.iter().flatten() {
        let info = d.op.info();
        for (kind, off) in [(info.b, d.b_s), (info.c, d.c_s)] {
            if kind == OperandKind::Jump {
                if let Some(t) = usize::try_from(i64::from(d.pc) + 1 + i64::from(off))
                    .ok()
                    .and_then(|t| leader.get_mut(t))
                {
                    *t = true;
                }
            }
        }
        if !straight(d.op) {
            if let Some(t) = leader.get_mut((d.pc + d.width) as usize) {
                *t = true;
            }
        }
    }
    for h in &f.handlers {
        for t in [h.handler_pc, h.finally_pc] {
            if let Some(t) = leader.get_mut(t as usize) {
                *t = true;
            }
        }
    }
    let none = r.stack(Vec::new());
    r.flow(0, none, &vec![0; words]);
    let mut read = Vec::new();
    while let Some(i) = r.work.pop() {
        r.queued[i] = false;
        let (mut pc, stack) = (r.states[i].0, r.states[i].1);
        let mut u = r.states[i].2.clone();
        let tried = stack != none;
        let d = loop {
            let Some(Some(d)) = code.get(pc as usize).copied() else {
                break None;
            };
            reads(&d, f, &mut read);
            if let Some(&reg) = read.iter().find(|&&x| r.has(&u, x)) {
                return Err(read_after_call(func, code, pc, reg));
            }
            if !straight(d.op) {
                break Some(d);
            }
            if d.op == Op::CALL {
                let first = d.a.saturating_add(d.c).saturating_add(1).max(lo);
                for x in first..f.reg_count {
                    r.set(&mut u, x);
                }
                if tried {
                    r.raise(pc, stack, &u);
                }
                for x in d.a..=d.a.saturating_add(d.c) {
                    r.clear(&mut u, x);
                }
            } else {
                if tried {
                    r.raise(pc, stack, &u);
                }
                if writes_a(d.op) {
                    r.clear(&mut u, d.a);
                }
            }
            pc += d.width;
            if leader.get(pc as usize).copied().unwrap_or(true) {
                r.flow(pc, stack, &u);
                break None;
            }
        };
        let Some(d) = d else {
            continue;
        };
        let next = pc + d.width;
        let jump = |off: i32| u32::try_from(i64::from(pc) + 1 + i64::from(off)).ok();
        let a = d.a;
        // Any instruction may raise (a call's error comes with the callee's
        // frame having run: the straight walk above).
        if tried {
            r.raise(pc, stack, &u);
        }
        let mut on = u;
        match d.op {
            Op::RET | Op::TAILCALL | Op::THROW => {}
            Op::JMP | Op::LOOP => {
                if let Some(t) = jump(d.b_s) {
                    r.flow(t, stack, &on);
                }
            }
            Op::TESTSET => {
                if let Some(t) = jump(d.c_s) {
                    r.flow(t, stack, &on);
                }
                r.clear(&mut on, a);
                r.flow(next, stack, &on);
            }
            Op::EQJ | Op::NEJ | Op::LTJ | Op::LEJ => {
                if let Some(t) = jump(d.c_s) {
                    r.flow(t, stack, &on);
                }
                r.flow(next, stack, &on);
            }
            Op::TEST | Op::TESTN => {
                if let Some(t) = jump(d.b_s) {
                    r.flow(t, stack, &on);
                }
                r.flow(next, stack, &on);
            }
            Op::FORPREP => {
                if let Some(t) = jump(d.b_s) {
                    r.flow(t, stack, &on);
                }
                r.clear(&mut on, a.saturating_add(3));
                r.flow(next, stack, &on);
            }
            Op::FORLOOP => {
                r.flow(next, stack, &on);
                r.clear(&mut on, a);
                r.clear(&mut on, a.saturating_add(3));
                if let Some(t) = jump(d.b_s) {
                    r.flow(t, stack, &on);
                }
            }
            Op::ITERPREP => {
                r.clear(&mut on, a.saturating_add(1));
                r.clear(&mut on, a.saturating_add(2));
                r.flow(next, stack, &on);
            }
            Op::ITERNEXT => {
                if let Some(t) = jump(d.b_s) {
                    r.flow(t, stack, &on);
                }
                r.clear(&mut on, a.saturating_add(2));
                r.clear(&mut on, a.saturating_add(3));
                r.flow(next, stack, &on);
            }
            Op::TRY_ENTER => {
                let mut s = r.stacks[stack as usize].clone();
                if let Some(at) = s.iter().position(|t| t.handler == d.b_u) {
                    s.truncate(at);
                }
                s.push(OpenTry {
                    handler: d.b_u,
                    stage: BODY,
                    pending: RAISE,
                });
                let s = r.stack(s);
                r.flow(next, s, &on);
            }
            Op::TRY_LEAVE => {
                let mut s = r.stacks[stack as usize].clone();
                let Some(t) = s.pop() else {
                    continue;
                };
                let h = f.handlers.get(t.handler as usize);
                if a != 0 {
                    let s = r.stack(s);
                    r.flow(next, s, &on);
                } else if t.stage == FINALLY {
                    if t.pending == RAISE {
                        let s = r.stack(s);
                        r.raise(pc, s, &on);
                    } else {
                        let s = r.stack(s);
                        r.flow(t.pending, s, &on);
                    }
                } else if let Some(h) = h.filter(|h| h.finally_pc != RESERVED_U32) {
                    s.push(OpenTry {
                        stage: FINALLY,
                        pending: next,
                        ..t
                    });
                    let s = r.stack(s);
                    r.flow(h.finally_pc, s, &on);
                } else {
                    let s = r.stack(s);
                    r.flow(next, s, &on);
                }
            }
            _ => r.flow(next, stack, &on),
        }
    }
    debug_assert!(r.states.iter().all(|s| s.2.len() == r.words));
    Ok(())
}

/// The refusal of a read of `reg` at `pc`, naming a call that leaves it
/// undefined: the last one before the read, else the last one.
fn read_after_call(func: u32, code: &[Option<DecodedInstr>], pc: u32, reg: u32) -> VerifyError {
    let calls = || {
        code.iter()
            .flatten()
            .filter(move |c| c.op == Op::CALL && c.a.saturating_add(c.c) < reg)
            .map(|c| c.pc)
    };
    let at = calls()
        .filter(|&c| c < pc)
        .max()
        .or_else(|| calls().max())
        .unwrap_or(pc);
    VerifyError::ReadBeforeWrite {
        func,
        pc,
        reg,
        why: "after-call",
        at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instr::Instr;
    use crate::module::{BurnRegion, Constant, FuncKind};
    use crate::opcode::Op;

    fn minimal_module() -> Module {
        Module::empty("t.cig")
    }

    #[test]
    fn an_empty_module_verifies() {
        verify_module(&minimal_module()).expect("empty module should verify");
    }

    #[test]
    fn an_empty_function_is_refused() {
        let mut m = minimal_module();
        m.main.instrs.clear();
        assert!(matches!(
            verify_module(&m).unwrap_err(),
            VerifyError::EmptyFunction { .. }
        ));
    }

    #[test]
    fn a_burn_op_in_a_region_verifies() {
        let mut m = minimal_module();
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
        verify_module(&m).expect("burn.op inside a region should verify");
    }

    #[test]
    fn requires_burn_allows_a_bare_burn_op() {
        let mut m = minimal_module();
        m.main.constants.push(Constant::Str("write".to_string()));
        m.main.reg_count = 4;
        m.main.requires_burn = true;
        m.main.kind = FuncKind::Pull;
        m.main.instrs = vec![
            Instr::abc(Op::BURN_OP, 1, 0, 0x10),
            Instr::abc(Op::LOADNULL, 0, 0, 0),
            Instr::abc(Op::RET, 0, 0, 0),
        ];
        verify_module(&m).expect("requires_burn should allow a bare BURN_OP");
    }
}
