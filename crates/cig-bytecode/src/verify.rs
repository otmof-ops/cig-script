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
use crate::instr::{decode, first_word_pcs, DecodeError, DecodedInstr, FirstWordPcs};
use crate::module::{
    Constant, Function, Module, UpvalKind, MAX_FUNCTION_DEPTH, MAX_REG_COUNT, RESERVED_U32,
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
    verify_function(&m.main, None, &mut counter, 0)?;
    Ok(Verified { module: m })
}

fn verify_function(
    f: &Function,
    parent: Option<&Function>,
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

    verify_upvalue_captures(func, f, parent)?;

    // One pass over the instruction stream, so that WIDE pairs and
    // unknown opcodes are caught first and the boundary set is built
    // once.
    let pcs = match first_word_pcs(&f.instrs) {
        Ok(pcs) => pcs,
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
    verify_instructions(func, f, &pcs)?;
    verify_termination(func, f, &pcs)?;
    verify_charged_cycles(func, f, &pcs)?;

    for c in &f.children {
        verify_function(c, Some(f), counter, depth + 1)?;
    }

    Ok(())
}

fn verify_upvalue_captures(
    func: u32,
    f: &Function,
    parent: Option<&Function>,
) -> Result<(), VerifyError> {
    let Some(parent) = parent else {
        // The top-level function has no parent, so it must have no
        // upvalues.
        if !f.upvalues.is_empty() {
            return Err(VerifyError::UpvalueCaptureOutOfRange {
                func,
                upvalue: 0,
                max: 0,
            });
        }
        return Ok(());
    };
    for (i, u) in f.upvalues.iter().enumerate() {
        let (bound, idx) = match u.kind {
            UpvalKind::ParentLocal => (parent.reg_count, u.index as u32),
            UpvalKind::ParentUpvalue => (parent.upvalues.len() as u32, u.index as u32),
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

fn verify_instructions(func: u32, f: &Function, pcs: &FirstWordPcs) -> Result<(), VerifyError> {
    let n_const = f.constants.len() as u32;
    let n_func = f.children.len() as u32;
    let n_regions = f.regions.len() as u32;
    let mut burn_depth: i32 = 0;

    for d in crate::instr::walk(&f.instrs) {
        let d = match d {
            Ok(d) => d,
            Err(DecodeError::BadOpcode { pc, byte }) => {
                return Err(VerifyError::BadOpcode { func, pc, byte })
            }
            Err(DecodeError::DanglingWide { pc } | DecodeError::DoubleWide { pc }) => {
                return Err(VerifyError::WideDangling { func, pc })
            }
            Err(DecodeError::Eof { pc }) => {
                return Err(VerifyError::JumpOutOfRange {
                    func,
                    pc,
                    target: pc as i64,
                    len: pcs.len(),
                })
            }
        };
        let info = d.op.info();

        verify_operand_a(func, &d, info.a, f, n_const, n_func, n_regions)?;
        verify_operand_b(func, &d, info, f, n_const, n_func, n_regions, pcs)?;
        verify_operand_c(func, &d, info, f, n_const, pcs)?;

        verify_reg_ranges(func, &d, f)?;

        match d.op {
            Op::BURN_ENTER => burn_depth += 1,
            Op::BURN_LEAVE => {
                if burn_depth == 0 {
                    return Err(VerifyError::BurnUnbalanced {
                        func,
                        pc: d.pc,
                        why: "leave-without-enter",
                    });
                }
                burn_depth -= 1;
            }
            Op::BURN_OP => {
                let inside_lexical = burn_depth > 0;
                let inside_region_row = f
                    .region_rows
                    .iter()
                    .any(|r| r.pc_start <= d.pc && d.pc < r.pc_end && r.region != RESERVED_U32);
                if !(inside_lexical || inside_region_row || f.requires_burn) {
                    return Err(VerifyError::BurnOpOutsideRegion { func, pc: d.pc });
                }
            }
            _ => {}
        }
    }

    if burn_depth != 0 {
        return Err(VerifyError::BurnUnbalanced {
            func,
            pc: pcs.len(),
            why: "enter-without-leave",
        });
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
        | OperandKind::BurnOpPacked => Ok(()),
        OperandKind::BurnClass => {
            if raw > 1 {
                return Err(VerifyError::RegisterOutOfRange {
                    func,
                    pc: d.pc,
                    reg: raw,
                    max: 2,
                });
            }
            Ok(())
        }
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

// ---- termination -----------------------------------------------------

fn verify_termination(func: u32, f: &Function, pcs: &FirstWordPcs) -> Result<(), VerifyError> {
    // The last decoded instruction in the function must be a
    // terminator, or a JMP (whose target the jump rule has already
    // checked).
    let Some(last_pc) = pcs.last_start() else {
        return Err(VerifyError::NotTerminated { func });
    };
    let Ok(d) = decode(&f.instrs, last_pc) else {
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
/// The check removes the charging `steps`, then peels off every node with
/// no remaining predecessor (Kahn's algorithm, with a queue: no recursion).
/// Whatever is left lies on or after a cycle; walking predecessors from it
/// lands on one, whose lowest pc is reported.
fn verify_charged_cycles(func: u32, f: &Function, pcs: &FirstWordPcs) -> Result<(), VerifyError> {
    let n = f.instrs.len();
    let mut code: Vec<Option<DecodedInstr>> = vec![None; n];
    for pc in pcs.iter() {
        if let (Ok(d), Some(slot)) = (decode(&f.instrs, pc), code.get_mut(pc as usize)) {
            *slot = Some(d);
        }
    }
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
    // and the parent handler. A charging `steps` is out of the graph.
    let live = |u: usize| match code.get(u) {
        Some(Some(d)) => !charges(d),
        Some(None) => false,
        None => u < total,
    };
    let succ = |u: usize, out: &mut Vec<usize>| {
        out.clear();
        if let Some(slot) = code.get(u) {
            let Some(d) = slot else { return };
            if charges(d) {
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
            label: RESERVED_U32,
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
