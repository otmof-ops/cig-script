// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The opcode table.
//!
//! Each opcode's **mnemonic** is the name writers and readers use; its
//! **discriminant** is the byte on the wire; its **[`OpInfo`]** says how
//! to read its operands and what to verify about them.
//!
//! The format: a fixed 32-bit instruction, bits laid out as
//!
//! ```text
//!   [ C: u8 ][ B: u8 ][ A: u8 ][ op: u8 ]      (ABC)
//!   [     Bx: u16    ][ A: u8 ][ op: u8 ]      (ABx)
//!   [     sBx: i16   ][ A: u8 ][ op: u8 ]      (AsBx)
//! ```
//!
//! A `WIDE` prefix is itself an ABx-layout word whose `A` and `Bx` are
//! the **high bits** of the next instruction's operands. Prefixed
//! operands widen to:
//!
//! - ABC next: `A = wide.A << 8 | next.A`, `B = wide.Bx[0..8] << 8 | next.B`,
//!   `C = wide.Bx[8..16] << 8 | next.C` (three 16-bit operands);
//! - ABx next: `A = wide.A << 8 | next.A`, `Bx = wide.Bx << 16 | next.Bx`
//!   (one 16-bit `A` and one 32-bit `Bx`);
//! - AsBx next: the same, with the 32-bit value read as signed.
//!
//! All operand decoding goes through [`crate::instr::decode`], which the
//! verifier, the disassembler and the VM share. `WIDE` is never the last
//! word of a function; `WIDE` may not be followed by another `WIDE`;
//! and a jump target, a handler's `pc_start`, `handler_pc` or
//! `finally_pc`, a region row's `pc_start`, or a line row's `pc` may
//! never point at the second word of a prefixed pair.

use core::fmt;

/// A bytecode opcode. The discriminants are **stable** parts of the
/// format: a value that is not one of these on the wire is a bad tag.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
#[allow(non_camel_case_types)]
#[allow(missing_docs)]
pub enum Op {
    // --- moves and constants (0x00-0x0F) ---
    NOP = 0x00,
    MOV = 0x01,
    LOADK = 0x02,
    LOADNULL = 0x03,
    LOADBOOL = 0x04,
    LOADINT = 0x05,
    LOADG = 0x06,

    // --- arithmetic (0x10-0x1F) ---
    ADD = 0x10,
    SUB = 0x11,
    MUL = 0x12,
    DIV = 0x13,
    MOD = 0x14,
    POW = 0x15,
    NEG = 0x16,
    ADDK = 0x17,
    SUBK = 0x18,
    MULK = 0x19,
    DIVK = 0x1A,
    MODK = 0x1B,
    POWK = 0x1C,

    // --- comparison and logic (0x20-0x2F) ---
    EQ = 0x20,
    LT = 0x21,
    LE = 0x22,
    NOT = 0x23,
    EQK = 0x24,
    LTK = 0x25,
    LEK = 0x26,
    EQJ = 0x27,
    LTJ = 0x28,
    LEJ = 0x29,
    NEJ = 0x2A,

    // --- control (0x30-0x3F) ---
    JMP = 0x30,
    TEST = 0x31,
    TESTSET = 0x32,
    FORPREP = 0x33,
    FORLOOP = 0x34,
    LOOP = 0x35,
    ITERPREP = 0x36,
    ITERNEXT = 0x37,
    TESTN = 0x38,

    // --- data (0x40-0x4F) ---
    NEWLIST = 0x40,
    NEWMAP = 0x41,
    GETFIELD = 0x42,
    SETFIELD = 0x43,
    GETINDEX = 0x44,
    SETINDEX = 0x45,
    APPENDLIST = 0x46,
    INSERTMAP = 0x47,
    INTERP = 0x48,

    // --- calls (0x50-0x5F) ---
    CALL = 0x50,
    CALLB = 0x51,
    CALLM = 0x52,
    RET = 0x53,
    TAILCALL = 0x54,

    // --- closures (0x60-0x6F) ---
    CLOSURE = 0x60,
    GETUP = 0x61,
    SETUP = 0x62,
    CELLNEW = 0x63,
    CELLGET = 0x64,
    CELLSET = 0x65,

    // --- errors (0x70-0x7F) ---
    TRY_ENTER = 0x70,
    TRY_LEAVE = 0x71,
    THROW = 0x72,

    // --- burns (0x80-0x8F) ---
    BURN_ENTER = 0x80,
    BURN_OP = 0x81,
    BURN_LEAVE = 0x82,
    COMP_ENTER = 0x83,
    COMP_LEAVE = 0x84,

    // --- the rest (0x90-0x9F) ---
    PACK = 0x90,
    EXHALE = 0x91,
    CHAIN_MAKE = 0x92,

    // --- the step budget (0xA0-0xAF) ---
    STEPS = 0xA0,

    // --- the wide prefix (0xF0) ---
    WIDE = 0xF0,
}

impl Op {
    /// Try to decode a raw opcode byte as an [`Op`]. `None` for a byte
    /// that is not one of the defined values.
    pub fn from_byte(b: u8) -> Option<Op> {
        use Op::*;
        Some(match b {
            0x00 => NOP,
            0x01 => MOV,
            0x02 => LOADK,
            0x03 => LOADNULL,
            0x04 => LOADBOOL,
            0x05 => LOADINT,
            0x06 => LOADG,
            0x10 => ADD,
            0x11 => SUB,
            0x12 => MUL,
            0x13 => DIV,
            0x14 => MOD,
            0x15 => POW,
            0x16 => NEG,
            0x17 => ADDK,
            0x18 => SUBK,
            0x19 => MULK,
            0x1A => DIVK,
            0x1B => MODK,
            0x1C => POWK,
            0x20 => EQ,
            0x21 => LT,
            0x22 => LE,
            0x23 => NOT,
            0x24 => EQK,
            0x25 => LTK,
            0x26 => LEK,
            0x27 => EQJ,
            0x28 => LTJ,
            0x29 => LEJ,
            0x2A => NEJ,
            0x30 => JMP,
            0x31 => TEST,
            0x32 => TESTSET,
            0x33 => FORPREP,
            0x34 => FORLOOP,
            0x35 => LOOP,
            0x36 => ITERPREP,
            0x37 => ITERNEXT,
            0x38 => TESTN,
            0x40 => NEWLIST,
            0x41 => NEWMAP,
            0x42 => GETFIELD,
            0x43 => SETFIELD,
            0x44 => GETINDEX,
            0x45 => SETINDEX,
            0x46 => APPENDLIST,
            0x47 => INSERTMAP,
            0x48 => INTERP,
            0x50 => CALL,
            0x51 => CALLB,
            0x52 => CALLM,
            0x53 => RET,
            0x54 => TAILCALL,
            0x60 => CLOSURE,
            0x61 => GETUP,
            0x62 => SETUP,
            0x63 => CELLNEW,
            0x64 => CELLGET,
            0x65 => CELLSET,
            0x70 => TRY_ENTER,
            0x71 => TRY_LEAVE,
            0x72 => THROW,
            0x80 => BURN_ENTER,
            0x81 => BURN_OP,
            0x82 => BURN_LEAVE,
            0x83 => COMP_ENTER,
            0x84 => COMP_LEAVE,
            0x90 => PACK,
            0x91 => EXHALE,
            0x92 => CHAIN_MAKE,
            0xA0 => STEPS,
            0xF0 => WIDE,
            _ => return None,
        })
    }

    /// The opcode's byte on the wire.
    pub fn as_byte(self) -> u8 {
        self as u8
    }

    /// The opcode's lower-snake mnemonic (for the disassembler).
    pub fn mnemonic(self) -> &'static str {
        use Op::*;
        match self {
            NOP => "nop",
            MOV => "mov",
            LOADK => "loadk",
            LOADNULL => "loadnull",
            LOADBOOL => "loadbool",
            LOADINT => "loadint",
            LOADG => "loadg",
            ADD => "add",
            SUB => "sub",
            MUL => "mul",
            DIV => "div",
            MOD => "mod",
            POW => "pow",
            NEG => "neg",
            ADDK => "addk",
            SUBK => "subk",
            MULK => "mulk",
            DIVK => "divk",
            MODK => "modk",
            POWK => "powk",
            EQ => "eq",
            LT => "lt",
            LE => "le",
            NOT => "not",
            EQK => "eqk",
            LTK => "ltk",
            LEK => "lek",
            EQJ => "eqj",
            LTJ => "ltj",
            LEJ => "lej",
            NEJ => "nej",
            JMP => "jmp",
            TEST => "test",
            TESTN => "testn",
            TESTSET => "testset",
            FORPREP => "forprep",
            FORLOOP => "forloop",
            LOOP => "loop",
            ITERPREP => "iterprep",
            ITERNEXT => "iternext",
            NEWLIST => "newlist",
            NEWMAP => "newmap",
            GETFIELD => "getfield",
            SETFIELD => "setfield",
            GETINDEX => "getindex",
            SETINDEX => "setindex",
            APPENDLIST => "appendlist",
            INSERTMAP => "insertmap",
            INTERP => "interp",
            CALL => "call",
            CALLB => "callb",
            CALLM => "callm",
            RET => "ret",
            TAILCALL => "tailcall",
            CLOSURE => "closure",
            GETUP => "getup",
            SETUP => "setup",
            CELLNEW => "cellnew",
            CELLGET => "cellget",
            CELLSET => "cellset",
            TRY_ENTER => "try.enter",
            TRY_LEAVE => "try.leave",
            THROW => "throw",
            BURN_ENTER => "burn.enter",
            BURN_OP => "burn.op",
            BURN_LEAVE => "burn.leave",
            COMP_ENTER => "comp.enter",
            COMP_LEAVE => "comp.leave",
            PACK => "pack",
            EXHALE => "exhale",
            CHAIN_MAKE => "chain.make",
            STEPS => "steps",
            WIDE => "wide",
        }
    }

    /// The opcode's operand metadata.
    pub fn info(self) -> OpInfo {
        info_for(self)
    }

    /// True for instructions that end a function on their fall-through
    /// path: `RET`, `TAILCALL`, `THROW`. (`JMP` ends the fall-through
    /// too, but to another in-range pc; the function-termination check
    /// treats it positively.)
    pub fn is_terminator(self) -> bool {
        matches!(self, Op::RET | Op::TAILCALL | Op::THROW)
    }
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.mnemonic())
    }
}

/// The three word layouts.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fmt {
    /// `op, A, B, C` (three 8-bit operands).
    ABC,
    /// `op, A, Bx` (one 8-bit and one unsigned 16-bit).
    ABx,
    /// `op, A, sBx` (one 8-bit and one signed 16-bit).
    AsBx,
}

/// What each operand slot carries. Used by the verifier and the
/// disassembler to interpret a decoded operand without a per-opcode
/// match.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OperandKind {
    /// The operand is unused.
    None,
    /// A register index into the function's frame.
    Reg,
    /// A base register for a range of adjacent registers; the range's
    /// length is in the other operand named here.
    RegRange,
    /// The count of registers the paired base covers.
    RegRangeCount,
    /// A constant-pool index into the function's own pool.
    Const,
    /// A constant-pool index that must be a [`crate::Constant::Str`].
    StrConst,
    /// A child-function index into this function's own `children`.
    Func,
    /// An upvalue index (into this function's descriptor list).
    Upvalue,
    /// A handler-table index (into this function's handler slice).
    Handler,
    /// A pc-relative, **signed** jump offset in instruction words.
    /// The target is `pc + 1 + offset`, where `pc` is the word that
    /// holds the offset (the prefixed word when there is a `WIDE`).
    Jump,
    /// A small signed integer (used by `LOADINT`).
    Immediate,
    /// A burn class: 0 = normal, 1 = unlit. (Checked as an immediate.)
    BurnClass,
    /// A packed value where the low 4 bits are the burn-op kind and
    /// the high 4 bits are the argument count (used by `BURN_OP`,
    /// unwidened; wide carries them in the high byte).
    BurnOpPacked,
    /// A region-table index into this function's own `regions`.
    Region,
    /// A builtin-catalogue index. The verifier accepts any value: the
    /// VM checks the catalogue once at module-load time and refuses a
    /// module with an unknown builtin.
    Builtin,
    /// Only valid in `WIDE`: the operand carries high bits for the
    /// next word's operands. The decoder consumes it before the
    /// verifier ever sees a kind-based check.
    Wide,
    /// An argument count (checked to fit the frame via a range check
    /// on the base register).
    Arity,
    /// A free u8/u16/u32 with no verifier rule attached.
    Any,
}

/// The per-opcode operand metadata.
#[derive(Clone, Copy, Debug)]
pub struct OpInfo {
    /// The word layout.
    pub fmt: Fmt,
    /// What operand `A` holds.
    pub a: OperandKind,
    /// What operand `B` or the whole `Bx`/`sBx` field holds. For
    /// `ABx`/`AsBx` formats, `b` describes the whole field and `c` is
    /// [`OperandKind::None`].
    pub b: OperandKind,
    /// What operand `C` holds (for `ABC` layouts only).
    pub c: OperandKind,
}

/// Table of operand kinds per opcode.
fn info_for(op: Op) -> OpInfo {
    use Fmt::*;
    use Op::*;
    use OperandKind::*;
    let abc = |a, b, c| OpInfo { fmt: ABC, a, b, c };
    let abx = |a, b| OpInfo {
        fmt: ABx,
        a,
        b,
        c: None,
    };
    let asbx = |a, b| OpInfo {
        fmt: AsBx,
        a,
        b,
        c: None,
    };
    match op {
        NOP => abc(None, None, None),
        MOV => abc(Reg, Reg, None),
        LOADK => abx(Reg, Const),
        LOADNULL => abc(Reg, None, None),
        LOADBOOL => abc(Reg, Immediate, None),
        LOADINT => asbx(Reg, Immediate),
        LOADG => abx(Reg, StrConst),

        ADD | SUB | MUL | DIV | MOD | POW => abc(Reg, Reg, Reg),
        NEG => abc(Reg, Reg, None),
        ADDK | SUBK | MULK | DIVK | MODK | POWK => abc(Reg, Reg, Const),

        EQ | LT | LE => abc(Reg, Reg, Reg),
        NOT => abc(Reg, Reg, None),
        EQK | LTK | LEK => abc(Reg, Reg, Const),
        EQJ | LTJ | LEJ | NEJ => abc(Reg, Reg, Jump),

        JMP => asbx(Any, Jump),
        TEST | TESTN => asbx(Reg, Jump),
        TESTSET => abc(Reg, Reg, Jump),
        FORPREP => asbx(Reg, Jump),
        FORLOOP => asbx(Reg, Jump),
        LOOP => asbx(Any, Jump),
        ITERPREP => abc(Reg, None, None),
        ITERNEXT => asbx(Reg, Jump),

        NEWLIST => abc(Reg, Immediate, None),
        NEWMAP => abc(Reg, Immediate, None),
        GETFIELD => abc(Reg, Reg, StrConst),
        SETFIELD => abc(Reg, StrConst, Reg),
        GETINDEX => abc(Reg, Reg, Reg),
        SETINDEX => abc(Reg, Reg, Reg),
        APPENDLIST => abc(Reg, Reg, None),
        INSERTMAP => abc(Reg, Reg, Reg),
        INTERP => abc(Reg, RegRange, RegRangeCount),

        CALL => abc(RegRange, Arity, Arity),
        CALLB => abc(RegRange, Builtin, Arity),
        CALLM => abc(RegRange, StrConst, Arity),
        RET => abc(Reg, None, None),
        TAILCALL => abc(RegRange, Arity, None),

        CLOSURE => abx(Reg, Func),
        GETUP => abc(Reg, Upvalue, None),
        SETUP => abc(Upvalue, Reg, None),
        CELLNEW => abc(Reg, None, None),
        CELLGET => abc(Reg, Upvalue, None),
        CELLSET => abc(Upvalue, Reg, None),

        TRY_ENTER => abx(Any, Handler),
        TRY_LEAVE => abc(Any, None, None),
        THROW => abc(Reg, None, None),

        BURN_ENTER => abx(BurnClass, Region),
        BURN_OP => abc(RegRange, StrConst, BurnOpPacked),
        BURN_LEAVE => abc(None, None, None),
        COMP_ENTER => abc(Reg, StrConst, StrConst),
        COMP_LEAVE => abc(None, None, None),

        PACK => abc(RegRange, RegRangeCount, None),
        EXHALE => abc(RegRange, RegRangeCount, None),
        CHAIN_MAKE => abc(Reg, RegRangeCount, None),

        STEPS => abx(Any, Any),
        WIDE => abx(Wide, Wide),
    }
}

/// The highest value of the format version this crate reads and writes.
pub const FORMAT_VERSION_LATEST: u16 = 1;
