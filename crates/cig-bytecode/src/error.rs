// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The two error kinds this crate produces: a parse error (the bytes could
//! not be read as a module) and a verify error (the module reads, but
//! would not be safe to run). `cig` maps both to `E1050`: a damaged
//! program.

use core::fmt;

/// Why the bytes given to [`crate::parse_module`] are not a module. A
/// parser that reads bytes never panics on any input: every failing case
/// returns one of these variants instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// The file is shorter than its own header or truncates mid-field.
    Truncated {
        /// Which field or section was being read when the bytes ran out.
        at: &'static str,
        /// The byte offset reached.
        offset: usize,
    },
    /// The magic bytes at the start of the file are not `CIGB`.
    BadMagic,
    /// The format version is not one this parser knows. A later parser
    /// reads an earlier module; an earlier parser refuses a later one.
    BadVersion {
        /// The version the file claims.
        found: u16,
        /// The highest version this parser understands.
        supported: u16,
    },
    /// The BLAKE3 hash in the header does not match the hash of the
    /// payload: the file was cut short, edited, or had bytes appended.
    BadIntegrity,
    /// A field's value is a tag this format does not define (an opcode
    /// byte that is not an [`crate::Op`], a constant tag that is not one
    /// of the four, a function kind that is not main/pull/lambda).
    BadTag {
        /// Which field held the tag.
        at: &'static str,
        /// The tag's raw value.
        value: u32,
    },
    /// A count field claims more entries than the remaining bytes allow.
    /// A reader that trusted the count could allocate without bound.
    BadCount {
        /// Which section's count.
        at: &'static str,
        /// The count field's value.
        count: u32,
        /// The bytes left in the payload.
        remaining: usize,
    },
    /// A UTF-8 string's bytes are not valid UTF-8.
    BadString {
        /// Which string: a string-pool entry or a function's name.
        at: &'static str,
        /// The index in its table.
        index: u32,
    },
    /// A referenced index points past the end of its table. The verifier
    /// also checks this; the parser catches it when the reference is in
    /// the header or a global table that can be checked without the
    /// instruction stream.
    BadIndex {
        /// Which table.
        at: &'static str,
        /// The out-of-range value.
        index: u32,
        /// The table's length.
        len: usize,
    },
    /// The payload has bytes after the last declared section. A format
    /// with trailing data is a format a later reader cannot trust not to
    /// have meaning, so this parser refuses it.
    TrailingBytes {
        /// The byte offset where the trailing data starts.
        offset: usize,
    },
    /// The nesting of child functions exceeded this reader's maximum
    /// depth. Catches a denial-of-service through recursion.
    TooDeep {
        /// The depth at which the limit tripped.
        depth: u32,
        /// The reader's maximum.
        max: u32,
    },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Truncated { at, offset } => {
                write!(f, "bytecode truncated at `{at}` (offset {offset})")
            }
            ParseError::BadMagic => f.write_str("bytecode magic is not `CIGB`"),
            ParseError::BadVersion { found, supported } => write!(
                f,
                "bytecode format version {found} is not supported (highest known: {supported})"
            ),
            ParseError::BadIntegrity => f.write_str("bytecode integrity hash does not match"),
            ParseError::BadTag { at, value } => {
                write!(f, "bytecode has an unknown {at} tag {value:#x}")
            }
            ParseError::BadCount {
                at,
                count,
                remaining,
            } => {
                write!(
                    f,
                    "bytecode `{at}` claims {count} entries but only {remaining} bytes remain"
                )
            }
            ParseError::BadString { at, index } => {
                write!(f, "bytecode `{at}` string at index {index} is not UTF-8")
            }
            ParseError::BadIndex { at, index, len } => {
                write!(
                    f,
                    "bytecode `{at}` index {index} is past its table of length {len}"
                )
            }
            ParseError::TrailingBytes { offset } => {
                write!(
                    f,
                    "bytecode has trailing bytes after the last section (offset {offset})"
                )
            }
            ParseError::TooDeep { depth, max } => {
                write!(
                    f,
                    "bytecode's function nesting exceeded the limit: depth {depth} > {max}"
                )
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// Why a module parsed but would not be safe to run. Every variant names
/// the rule from `docs/BYTECODE.md`'s verifier section and the function
/// (`fn`) and instruction offset (`pc`) where it failed, so the VM can
/// surface an `E1050` text in the form the issue names:
/// `the program's bytecode does not verify: <rule> at <fn>+<pc>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyError {
    /// An instruction's register operand is outside the function's
    /// declared register count.
    RegisterOutOfRange {
        /// The function this instruction lives in.
        func: u32,
        /// The instruction's pc.
        pc: u32,
        /// The register that is out of range.
        reg: u32,
        /// The function's register count.
        max: u32,
    },
    /// A range of registers (for a call, a list literal, a pack) runs
    /// past the end of the frame.
    RegisterRangeOutOfRange {
        /// The function this instruction lives in.
        func: u32,
        /// The instruction's pc.
        pc: u32,
        /// The first register of the range.
        base: u32,
        /// The count of registers in the range.
        count: u32,
        /// The function's register count.
        max: u32,
    },
    /// A constant-pool index points past the end of the pool.
    ConstOutOfRange {
        /// The function.
        func: u32,
        /// The pc.
        pc: u32,
        /// The index given.
        index: u32,
        /// The pool's length.
        max: u32,
    },
    /// A constant-pool index was used where a string constant was
    /// required (field names, method names, chain step names).
    ConstNotString {
        /// The function.
        func: u32,
        /// The pc.
        pc: u32,
        /// The index.
        index: u32,
    },
    /// A jump instruction targets an offset that is not inside the
    /// function's instruction stream or is not on an instruction
    /// boundary (jumps are always to the start of a 32-bit word in this
    /// format).
    JumpOutOfRange {
        /// The function.
        func: u32,
        /// The pc of the jump.
        pc: u32,
        /// The target pc.
        target: i64,
        /// The function's instruction count.
        len: u32,
    },
    /// A function-prototype index (`CLOSURE proto`) points past the end
    /// of the module's function table.
    FuncOutOfRange {
        /// The function this instruction lives in.
        func: u32,
        /// The pc.
        pc: u32,
        /// The index.
        index: u32,
        /// The module's function count.
        max: u32,
    },
    /// An upvalue index used by `GETUP`, `SETUP`, `CELLGET` or `CELLSET`
    /// points past the function's upvalue descriptor list.
    UpvalueOutOfRange {
        /// The function.
        func: u32,
        /// The pc.
        pc: u32,
        /// The index.
        index: u32,
        /// The function's upvalue count.
        max: u32,
    },
    /// A handler-table index (`TRY.ENTER`) points past the function's
    /// handler-table slice.
    HandlerOutOfRange {
        /// The function.
        func: u32,
        /// The pc of the `TRY.ENTER`.
        pc: u32,
        /// The index.
        index: u32,
        /// The function's handler count.
        max: u32,
    },
    /// A handler-table entry's pc range or its handler_pc is not inside
    /// the function, or the catch register is not in the frame, or the
    /// finally pc is not on an instruction boundary.
    HandlerBadRange {
        /// The function.
        func: u32,
        /// The handler index in the function's slice.
        handler: u32,
    },
    /// Two handler ranges overlap but neither contains the other.
    /// Handlers nest; they do not interleave.
    HandlerRangesInterleave {
        /// The function.
        func: u32,
        /// The outer handler in the function's slice.
        a: u32,
        /// The inner handler in the function's slice.
        b: u32,
    },
    /// `BURN.ENTER`/`BURN.LEAVE` are not balanced on every path: a
    /// forward walk through the function ends with a non-empty burn
    /// stack, or `BURN.LEAVE` is reached without a matching
    /// `BURN.ENTER`.
    BurnUnbalanced {
        /// The function.
        func: u32,
        /// The pc where the imbalance was found.
        pc: u32,
        /// What is wrong, in one word (`leave-without-enter` or
        /// `enter-without-leave`).
        why: &'static str,
    },
    /// The verifier's main rule (V4/T1-T3): a `BURN.OP` sits outside
    /// every `BURN.ENTER`/`BURN.LEAVE` range in its function, and the
    /// function is not marked `requires_burn`.
    BurnOpOutsideRegion {
        /// The function.
        func: u32,
        /// The pc of the `BURN.OP`.
        pc: u32,
    },
    /// A region-table row names a region id past the end of the
    /// module's region table.
    RegionOutOfRange {
        /// The function.
        func: u32,
        /// The row index in the function's region table.
        row: u32,
        /// The region id.
        index: u32,
        /// The module's region count.
        max: u32,
    },
    /// A region-table row's pc bounds are not inside the function, or
    /// end < start.
    RegionBadRange {
        /// The function.
        func: u32,
        /// The row index.
        row: u32,
    },
    /// A call's operand that encodes an argument count exceeds the
    /// fixed arity the function or builtin advertises. (For builtins,
    /// checked against the K-string callee name only if the catalogue
    /// index is out of range; the real catalogue arity is enforced by
    /// the VM at a lower cost than reading a catalogue here.)
    CallArityTooHigh {
        /// The function.
        func: u32,
        /// The pc of the call.
        pc: u32,
        /// The count given.
        given: u32,
        /// The maximum for that opcode's A register plus arg range to
        /// still fit in the frame.
        max: u32,
    },
    /// The function's instruction stream ends on an instruction that is
    /// not a terminator (`RET`, `TAILCALL`, `THROW`, or an unconditional
    /// `JMP` to another in-range pc). A function must terminate on
    /// every path; the simple check is that the last instruction is one
    /// of these.
    NotTerminated {
        /// The function.
        func: u32,
    },
    /// A line-table row names a pc past the end of the function's
    /// instructions, or two rows are not in strictly ascending pc
    /// order.
    LineTableBadRow {
        /// The function.
        func: u32,
        /// The row index.
        row: u32,
    },
    /// A function's `arity` field is greater than its `reg_count`: the
    /// parameters cannot all fit in the frame.
    ArityExceedsRegCount {
        /// The function.
        func: u32,
    },
    /// An instruction uses the `WIDE` prefix but is the last instruction
    /// in the function (the prefix has no instruction to extend), or is
    /// followed by another `WIDE`.
    WideDangling {
        /// The function.
        func: u32,
        /// The pc of the dangling `WIDE`.
        pc: u32,
    },
    /// An instruction's opcode byte is not one of the defined
    /// [`crate::Op`] values. The parser accepts any 32-bit word; the
    /// verifier catches an unknown opcode here.
    BadOpcode {
        /// The function.
        func: u32,
        /// The pc of the offending instruction.
        pc: u32,
        /// The raw opcode byte.
        byte: u8,
    },
    /// A jump, handler, region or line reference points at a pc that
    /// is not the first word of an instruction (it points at the
    /// second word of a `WIDE` pair, or past the end of the function).
    MidPairReference {
        /// The function.
        func: u32,
        /// Which field held the bad pc.
        at: &'static str,
        /// The offending pc.
        pc: u32,
    },
    /// A function has no instructions. The termination rule needs at
    /// least one.
    EmptyFunction {
        /// The function.
        func: u32,
    },
    /// A function's `reg_count` is larger than the widest register
    /// operand can address (16 bits with a `WIDE` prefix, so
    /// `reg_count` must be `<= 65_536`).
    FrameTooLarge {
        /// The function.
        func: u32,
        /// The requested `reg_count`.
        reg_count: u32,
        /// The maximum.
        max: u32,
    },
    /// A child function's upvalue captures a parent register or a
    /// parent upvalue whose index is past the parent's bounds, or the
    /// module's top-level function declares an upvalue (it has no
    /// parent).
    UpvalueCaptureOutOfRange {
        /// The child function.
        func: u32,
        /// The index in the child's upvalue list.
        upvalue: u32,
        /// The parent bound (parent's `reg_count` for a parent-local
        /// capture, or parent's upvalue count for a parent-upvalue
        /// capture, or `0` for the top-level function).
        max: u32,
    },
    /// The function nesting exceeded the verifier's maximum depth.
    /// The parser enforces the same limit on the wire.
    NestedTooDeep {
        /// The depth at which the limit tripped.
        depth: u32,
        /// The maximum.
        max: u32,
    },
}

impl VerifyError {
    /// The rule's short name, for the `E1050` message. Matches the
    /// bullet names in `docs/BYTECODE.md`'s verifier section.
    pub fn rule(&self) -> &'static str {
        match self {
            VerifyError::RegisterOutOfRange { .. }
            | VerifyError::RegisterRangeOutOfRange { .. } => "register-out-of-range",
            VerifyError::ConstOutOfRange { .. } => "const-out-of-range",
            VerifyError::ConstNotString { .. } => "const-not-string",
            VerifyError::JumpOutOfRange { .. } => "jump-out-of-range",
            VerifyError::FuncOutOfRange { .. } => "func-out-of-range",
            VerifyError::UpvalueOutOfRange { .. } => "upvalue-out-of-range",
            VerifyError::HandlerOutOfRange { .. } => "handler-out-of-range",
            VerifyError::HandlerBadRange { .. } => "handler-bad-range",
            VerifyError::HandlerRangesInterleave { .. } => "handler-ranges-interleave",
            VerifyError::BurnUnbalanced { .. } => "burn-unbalanced",
            VerifyError::BurnOpOutsideRegion { .. } => "burn-op-outside-region",
            VerifyError::RegionOutOfRange { .. } => "region-out-of-range",
            VerifyError::RegionBadRange { .. } => "region-bad-range",
            VerifyError::CallArityTooHigh { .. } => "call-arity-too-high",
            VerifyError::NotTerminated { .. } => "not-terminated",
            VerifyError::LineTableBadRow { .. } => "line-table-bad-row",
            VerifyError::ArityExceedsRegCount { .. } => "arity-exceeds-reg-count",
            VerifyError::WideDangling { .. } => "wide-dangling",
            VerifyError::BadOpcode { .. } => "bad-opcode",
            VerifyError::MidPairReference { .. } => "mid-pair-reference",
            VerifyError::EmptyFunction { .. } => "empty-function",
            VerifyError::FrameTooLarge { .. } => "frame-too-large",
            VerifyError::UpvalueCaptureOutOfRange { .. } => "upvalue-capture-out-of-range",
            VerifyError::NestedTooDeep { .. } => "nested-too-deep",
        }
    }

    /// Which function the error is in, when the error carries one.
    pub fn func(&self) -> Option<u32> {
        match self {
            VerifyError::RegisterOutOfRange { func, .. }
            | VerifyError::RegisterRangeOutOfRange { func, .. }
            | VerifyError::ConstOutOfRange { func, .. }
            | VerifyError::ConstNotString { func, .. }
            | VerifyError::JumpOutOfRange { func, .. }
            | VerifyError::FuncOutOfRange { func, .. }
            | VerifyError::UpvalueOutOfRange { func, .. }
            | VerifyError::HandlerOutOfRange { func, .. }
            | VerifyError::HandlerBadRange { func, .. }
            | VerifyError::HandlerRangesInterleave { func, .. }
            | VerifyError::BurnUnbalanced { func, .. }
            | VerifyError::BurnOpOutsideRegion { func, .. }
            | VerifyError::RegionOutOfRange { func, .. }
            | VerifyError::RegionBadRange { func, .. }
            | VerifyError::CallArityTooHigh { func, .. }
            | VerifyError::NotTerminated { func }
            | VerifyError::LineTableBadRow { func, .. }
            | VerifyError::ArityExceedsRegCount { func }
            | VerifyError::WideDangling { func, .. }
            | VerifyError::BadOpcode { func, .. }
            | VerifyError::MidPairReference { func, .. }
            | VerifyError::EmptyFunction { func }
            | VerifyError::FrameTooLarge { func, .. }
            | VerifyError::UpvalueCaptureOutOfRange { func, .. } => Some(*func),
            VerifyError::NestedTooDeep { .. } => None,
        }
    }

    /// Which pc the error is at, when the error carries one.
    pub fn pc(&self) -> Option<u32> {
        match self {
            VerifyError::RegisterOutOfRange { pc, .. }
            | VerifyError::RegisterRangeOutOfRange { pc, .. }
            | VerifyError::ConstOutOfRange { pc, .. }
            | VerifyError::ConstNotString { pc, .. }
            | VerifyError::JumpOutOfRange { pc, .. }
            | VerifyError::FuncOutOfRange { pc, .. }
            | VerifyError::UpvalueOutOfRange { pc, .. }
            | VerifyError::HandlerOutOfRange { pc, .. }
            | VerifyError::BurnUnbalanced { pc, .. }
            | VerifyError::BurnOpOutsideRegion { pc, .. }
            | VerifyError::CallArityTooHigh { pc, .. }
            | VerifyError::WideDangling { pc, .. }
            | VerifyError::BadOpcode { pc, .. }
            | VerifyError::MidPairReference { pc, .. } => Some(*pc),
            _ => None,
        }
    }
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rule = self.rule();
        match (self.func(), self.pc()) {
            (Some(func), Some(pc)) => write!(f, "{rule} at fn{func}+{pc}"),
            (Some(func), None) => write!(f, "{rule} in fn{func}"),
            (None, Some(pc)) => write!(f, "{rule} at pc {pc}"),
            (None, None) => f.write_str(rule),
        }
    }
}

impl std::error::Error for VerifyError {}
