// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The in-memory representation of a bytecode module.
//!
//! The byte-level layout of a module on disk is in
//! `docs/BYTECODE.md`. The structs here mirror that layout one-to-one.
//!
//! Each function is **self-contained**: its own constant pool, its own
//! list of child functions (a `CLOSURE A, Bx` names a child by its
//! index in the parent), its own burn-region table, its own line table
//! relative to its [`SpanBase`]. The function's bytes do not depend on
//! what sits around it in the module, so the per-function build cache
//! stays warm across edits to a sibling.

use crate::instr::Instr;

/// The four bytes that start every bytecode module.
pub const MAGIC: [u8; 4] = *b"CIGB";

/// The current format version. See `docs/BYTECODE.md` for the rule: a
/// later reader opens an earlier module, an earlier reader refuses a
/// later one.
pub const FORMAT_VERSION: u16 = 1;

/// A sentinel value used where an optional `u32` is absent.
pub const RESERVED_U32: u32 = u32::MAX;

/// The maximum register count the verifier accepts for a function.
/// The widest register operand is 16 bits with a `WIDE` prefix, so
/// indices go from 0 to 65_535; a frame of 65_536 registers is the
/// widest the format can address.
pub const MAX_REG_COUNT: u32 = 65_536;

/// The maximum depth of child-function nesting the parser and the
/// verifier accept. The limit catches a denial-of-service through
/// recursion without closing the door on realistic programs: deep
/// closures of 64 stacked lambdas do not arise in CigScript.
pub const MAX_FUNCTION_DEPTH: u32 = 64;

/// One of the four constant kinds the format carries. Rich values
/// (lists, maps, closures) are built by instructions, not stored here.
#[derive(Clone, Debug, PartialEq)]
pub enum Constant {
    /// A boolean constant.
    Bool(bool),
    /// A 64-bit signed integer. CigScript's integer semantics are full
    /// `i64` with overflow errors.
    Int(i64),
    /// A 64-bit IEEE-754 float. On the wire the bits are stored, so
    /// NaN payloads round-trip.
    Float(f64),
    /// A UTF-8 string.
    Str(String),
}

impl Constant {
    /// True for a [`Constant::Str`].
    pub fn is_str(&self) -> bool {
        matches!(self, Constant::Str(_))
    }
}

/// A function's source-position anchor. Line rows store deltas from
/// this point, so the function's bytes do not change when its source
/// is moved up or down a few lines.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpanBase {
    /// The byte offset of the function's first token in the source.
    pub byte_offset: u32,
    /// The 1-based line of the first token.
    pub line: u32,
    /// The 1-based column of the first token.
    pub col: u32,
}

/// One row of a function's line table: at `pc` the running instruction
/// came from this source position, expressed relative to
/// [`Function::span_base`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineEntry {
    /// The instruction pc this row covers (a first-word pc).
    pub pc: u32,
    /// Bytes from `span_base.byte_offset` to the span's start.
    pub span_off: u32,
    /// Bytes from the span's start to its end (half-open).
    pub span_len: u32,
    /// Lines down from `span_base.line` (0 = same line).
    pub line_off: u32,
    /// 1-based column. Absolute (columns reset each line).
    pub col: u32,
}

/// One row of a function's region-table: for every pc in
/// `pc_start..pc_end` (half-open), the surrounding burn region is
/// `region` (an index into [`Function::regions`]). A `region` of
/// [`RESERVED_U32`] marks "outside every burn".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegionRow {
    /// The first pc in this region (inclusive, a first-word pc).
    pub pc_start: u32,
    /// The pc after the last one in this region.
    pub pc_end: u32,
    /// The burn-region id in [`Function::regions`].
    pub region: u32,
}

/// A description of one of a function's upvalues.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpvalDesc {
    /// Which value the upvalue points at.
    pub kind: UpvalKind,
    /// The index into the **parent** function: a register for
    /// [`UpvalKind::ParentLocal`] (bounded by the parent's
    /// `reg_count`, verified) or an upvalue for
    /// [`UpvalKind::ParentUpvalue`] (bounded by the parent's
    /// upvalue count, verified). 16 bits matches the parent's widest
    /// register index after a `WIDE` prefix on the parent's own
    /// register operands; the upvalue descriptor itself is not an
    /// instruction and does not carry a `WIDE`.
    pub index: u16,
}

/// Where a captured value lives: the direct parent's register, or
/// transitively through its own upvalue list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpvalKind {
    /// The upvalue is a register of the parent function.
    ParentLocal,
    /// The upvalue is itself an upvalue of the parent function.
    ParentUpvalue,
}

impl UpvalKind {
    /// Byte tag used on the wire.
    pub fn tag(self) -> u8 {
        match self {
            UpvalKind::ParentLocal => 0,
            UpvalKind::ParentUpvalue => 1,
        }
    }

    /// The reverse of [`UpvalKind::tag`].
    pub fn from_tag(b: u8) -> Option<UpvalKind> {
        Some(match b {
            0 => UpvalKind::ParentLocal,
            1 => UpvalKind::ParentUpvalue,
            _ => return None,
        })
    }
}

/// One of a function's try/catch/finally regions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HandlerEntry {
    /// First pc in the try body (inclusive, a first-word pc).
    pub pc_start: u32,
    /// Pc after the last one in the try body.
    pub pc_end: u32,
    /// Pc the handler jumps to when the try body throws. Must be a
    /// first-word pc.
    pub handler_pc: u32,
    /// Register that receives the caught value, or [`RESERVED_U32`]
    /// when there is no catch binding.
    pub catch_reg: u32,
    /// Pc of the finally block's first instruction (a first-word pc),
    /// or [`RESERVED_U32`] when there is no finally.
    pub finally_pc: u32,
}

/// A burn region declared on a function: its class
/// (`Normal` or `Unlit`) and an optional label (a string-constant
/// index into the function's own constant pool, else [`RESERVED_U32`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BurnRegion {
    /// Burn class: 0 = normal, 1 = unlit.
    pub class: u8,
    /// Label constant index into the function's own pool, or
    /// [`RESERVED_U32`] for none.
    pub label: u32,
}

/// A function's kind: the module's top level, a named pull, or a lambda.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FuncKind {
    /// The module's top-level script.
    Main,
    /// A named function (a `pull`).
    Pull,
    /// An anonymous function (a lambda).
    Lambda,
}

impl FuncKind {
    /// Byte tag used on the wire.
    pub fn tag(self) -> u8 {
        match self {
            FuncKind::Main => 0,
            FuncKind::Pull => 1,
            FuncKind::Lambda => 2,
        }
    }

    /// The reverse of [`FuncKind::tag`].
    pub fn from_tag(b: u8) -> Option<FuncKind> {
        Some(match b {
            0 => FuncKind::Main,
            1 => FuncKind::Pull,
            2 => FuncKind::Lambda,
            _ => return None,
        })
    }
}

/// One function of a module (a `pull`, a `lambda`, or the script's top
/// level). Self-contained: its operand tables and line rows are its
/// own, so the function's bytes do not change when a sibling is
/// edited.
#[derive(Clone, Debug, PartialEq)]
pub struct Function {
    /// The function's pretty name. Used by the disassembler; by
    /// backtraces at run time; empty for an anonymous lambda.
    pub name: String,
    /// The function's kind.
    pub kind: FuncKind,
    /// Number of registers this frame uses.
    pub reg_count: u32,
    /// Parameter count (first `arity` registers are parameters).
    pub arity: u16,
    /// True when the function may only be called inside an open burn
    /// region (CIR's derived rule V5): its `BURN_OP` instructions are
    /// legal without a surrounding `BURN_ENTER` in this function. The
    /// VM refuses a call from outside a burn, with the interpreter's
    /// own error.
    pub requires_burn: bool,
    /// The upvalue descriptor list.
    pub upvalues: Vec<UpvalDesc>,
    /// The function's own constant pool. `LOADK`, `GETFIELD`,
    /// `SETFIELD`, `CALLM`, `BURN_OP` index this.
    pub constants: Vec<Constant>,
    /// The function's own child functions. `CLOSURE A, Bx` names
    /// `children[Bx]`.
    pub children: Vec<Function>,
    /// The function's own burn-region table.
    pub regions: Vec<BurnRegion>,
    /// Try/catch/finally handler table.
    pub handlers: Vec<HandlerEntry>,
    /// The instruction stream.
    pub instrs: Vec<Instr>,
    /// The first-token anchor for line rows.
    pub span_base: SpanBase,
    /// The line table (sorted strictly ascending by pc, each pc a
    /// first-word pc).
    pub lines: Vec<LineEntry>,
    /// The region table: which pc ranges belong to which entry of
    /// `regions`.
    pub region_rows: Vec<RegionRow>,
}

/// A whole module.
#[derive(Clone, Debug, PartialEq)]
pub struct Module {
    /// The source file name this module was compiled from. Empty when
    /// the module has none (a REPL snippet, a generated test).
    pub source_name: String,
    /// The top-level function. All other functions live under it as
    /// children (recursively).
    pub main: Function,
}

impl Module {
    /// An empty module whose main function returns null.
    pub fn empty(source_name: impl Into<String>) -> Module {
        Module {
            source_name: source_name.into(),
            main: Function::empty_main(),
        }
    }
}

impl Function {
    /// An empty-but-valid main: `LOADNULL R0; RET R0`.
    pub fn empty_main() -> Function {
        Function {
            name: String::new(),
            kind: FuncKind::Main,
            reg_count: 1,
            arity: 0,
            requires_burn: false,
            upvalues: Vec::new(),
            constants: Vec::new(),
            children: Vec::new(),
            regions: Vec::new(),
            handlers: Vec::new(),
            instrs: vec![
                Instr::abc(crate::opcode::Op::LOADNULL, 0, 0, 0),
                Instr::abc(crate::opcode::Op::RET, 0, 0, 0),
            ],
            span_base: SpanBase::default(),
            lines: Vec::new(),
            region_rows: Vec::new(),
        }
    }
}
