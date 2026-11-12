// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! A single 32-bit instruction word, and the shared decoder.
//!
//! The byte order on the wire is **little-endian**.
//!
//! Layout:
//!
//! ```text
//!   bits 0-7   : opcode
//!   bits 8-15  : A (u8)
//!   bits 16-31 : B+C (two u8 for ABC, one u16 for ABx/sBx)
//! ```
//!
//! [`decode`] is the one function that turns a stream of [`Instr`]
//! words at a pc into a [`DecodedInstr`] with full operands, combining
//! a `WIDE` prefix if one sits in front. The verifier, the
//! disassembler and (later) the VM all go through it. Nothing decodes
//! an instruction elsewhere.

use crate::opcode::{Fmt, Op};

/// A decoded (or ready-to-encode) 32-bit instruction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Instr {
    /// The raw word, little-endian on the wire.
    pub raw: u32,
}

impl Instr {
    /// Build an instruction in ABC form.
    pub fn abc(op: Op, a: u8, b: u8, c: u8) -> Instr {
        Instr {
            raw: (op.as_byte() as u32)
                | ((a as u32) << 8)
                | ((b as u32) << 16)
                | ((c as u32) << 24),
        }
    }

    /// Build an instruction in ABx form.
    pub fn abx(op: Op, a: u8, bx: u16) -> Instr {
        Instr {
            raw: (op.as_byte() as u32) | ((a as u32) << 8) | ((bx as u32) << 16),
        }
    }

    /// Build an instruction in AsBx form (the signed-Bx variant).
    pub fn asbx(op: Op, a: u8, sbx: i16) -> Instr {
        Instr {
            raw: (op.as_byte() as u32) | ((a as u32) << 8) | ((sbx as u16 as u32) << 16),
        }
    }

    /// Low byte of the word.
    pub fn op_byte(self) -> u8 {
        (self.raw & 0xFF) as u8
    }

    /// The decoded opcode, or `None` for an unknown byte.
    pub fn op(self) -> Option<Op> {
        Op::from_byte(self.op_byte())
    }

    /// Raw `A` byte.
    pub fn a(self) -> u8 {
        ((self.raw >> 8) & 0xFF) as u8
    }

    /// Raw `B` byte (ABC form).
    pub fn b(self) -> u8 {
        ((self.raw >> 16) & 0xFF) as u8
    }

    /// Raw `C` byte (ABC form).
    pub fn c(self) -> u8 {
        ((self.raw >> 24) & 0xFF) as u8
    }

    /// Raw `Bx` field (unsigned 16).
    pub fn bx(self) -> u16 {
        ((self.raw >> 16) & 0xFFFF) as u16
    }

    /// Raw `sBx` field (signed 16).
    pub fn sbx(self) -> i16 {
        (((self.raw >> 16) & 0xFFFF) as u16) as i16
    }

    /// Encode to 4 little-endian bytes.
    pub fn to_le_bytes(self) -> [u8; 4] {
        self.raw.to_le_bytes()
    }

    /// Decode from 4 little-endian bytes.
    pub fn from_le_bytes(bytes: [u8; 4]) -> Instr {
        Instr {
            raw: u32::from_le_bytes(bytes),
        }
    }
}

/// A fully decoded instruction: the opcode and its operands at their
/// widened widths, together with the pc of the first word and the
/// number of words consumed (1 or 2). `a`, `b_u` and `c` are
/// zero-extended for unsigned operand kinds; `b_s` is sign-extended
/// for signed kinds (`AsBx`, the signed jump in `C` for `EQJ`/`LTJ`/
/// `LEJ`/`NEJ`/`TESTSET`). The verifier and the disassembler read
/// whichever field the operand kind demands.
#[derive(Clone, Copy, Debug)]
pub struct DecodedInstr {
    /// First-word pc (the pc of the `WIDE` word when there is a
    /// prefix, otherwise the opcode's own word).
    pub pc: u32,
    /// Number of words this decoded instruction covers (1 or 2).
    pub width: u32,
    /// The decoded opcode.
    pub op: Op,
    /// The opcode's format.
    pub fmt: Fmt,
    /// Operand `A`, zero-extended.
    pub a: u32,
    /// Operand `B` (ABC) or `Bx` (ABx) zero-extended.
    pub b_u: u32,
    /// Operand `B` (ABC signed path) or `sBx` sign-extended.
    pub b_s: i32,
    /// Operand `C` (ABC), zero-extended.
    pub c: u32,
    /// Signed `C` (ABC form; only meaningful for jump operands).
    pub c_s: i32,
    /// True when this instruction had a `WIDE` prefix.
    pub wide: bool,
}

/// Decode the instruction at `pc` in `instrs`. Returns the decoded
/// form and the pc that follows it. Errors are purely structural: a
/// bad opcode byte, a dangling `WIDE`, or `WIDE` followed by `WIDE`.
pub fn decode(instrs: &[Instr], pc: u32) -> Result<DecodedInstr, DecodeError> {
    let first = instrs
        .get(pc as usize)
        .copied()
        .ok_or(DecodeError::Eof { pc })?;
    let first_op = first.op().ok_or(DecodeError::BadOpcode {
        pc,
        byte: first.op_byte(),
    })?;

    if first_op != Op::WIDE {
        // Simple decode. For signed operands, sign-extend from their
        // native width.
        let fmt = first_op.info().fmt;
        let (a, b_u, b_s, c, c_s) = match fmt {
            Fmt::ABC => (
                first.a() as u32,
                first.b() as u32,
                (first.b() as i8) as i32,
                first.c() as u32,
                (first.c() as i8) as i32,
            ),
            Fmt::ABx => (first.a() as u32, first.bx() as u32, first.bx() as i32, 0, 0),
            Fmt::AsBx => (
                first.a() as u32,
                first.bx() as u32,
                first.sbx() as i32,
                0,
                0,
            ),
        };
        return Ok(DecodedInstr {
            pc,
            width: 1,
            op: first_op,
            fmt,
            a,
            b_u,
            b_s,
            c,
            c_s,
            wide: false,
        });
    }

    // WIDE prefix. The next word must exist and must not be WIDE.
    let next_pc = pc + 1;
    let next = instrs
        .get(next_pc as usize)
        .copied()
        .ok_or(DecodeError::DanglingWide { pc })?;
    let next_op = next.op().ok_or(DecodeError::BadOpcode {
        pc: next_pc,
        byte: next.op_byte(),
    })?;
    if next_op == Op::WIDE {
        return Err(DecodeError::DoubleWide { pc });
    }
    let fmt = next_op.info().fmt;
    let (a, b_u, b_s, c, c_s) = match fmt {
        Fmt::ABC => {
            // A: wide.A << 8 | next.A (16-bit).
            let a = ((first.a() as u32) << 8) | (next.a() as u32);
            // B: wide.Bx low byte << 8 | next.B (16-bit).
            let b_hi = (first.bx() & 0x00FF) as u32;
            let b = (b_hi << 8) | (next.b() as u32);
            let b_signed = (((b_hi << 8) | (next.b() as u32)) as i16) as i32;
            // C: wide.Bx high byte << 8 | next.C (16-bit).
            let c_hi = ((first.bx() >> 8) & 0x00FF) as u32;
            let c = (c_hi << 8) | (next.c() as u32);
            let c_signed = (((c_hi << 8) | (next.c() as u32)) as i16) as i32;
            (a, b, b_signed, c, c_signed)
        }
        Fmt::ABx => {
            let a = ((first.a() as u32) << 8) | (next.a() as u32);
            let bx = ((first.bx() as u32) << 16) | (next.bx() as u32);
            (a, bx, bx as i32, 0, 0)
        }
        Fmt::AsBx => {
            let a = ((first.a() as u32) << 8) | (next.a() as u32);
            let bx = ((first.bx() as u32) << 16) | (next.bx() as u32);
            // The whole 32-bit value is signed.
            (a, bx, bx as i32, 0, 0)
        }
    };
    Ok(DecodedInstr {
        pc,
        width: 2,
        op: next_op,
        fmt,
        a,
        b_u,
        b_s,
        c,
        c_s,
        wide: true,
    })
}

/// Build a `WIDE` prefix word carrying high bits for the next
/// instruction. `a_hi` is the high byte of the 16-bit `A`; `b_hi` is
/// the high 16 bits of a 32-bit `Bx` (for ABx/AsBx next) or two
/// 8-bit high bytes packed `(c_hi << 8) | b_hi` (for ABC next).
pub fn wide_prefix(a_hi: u8, b_hi: u16) -> Instr {
    Instr::abx(Op::WIDE, a_hi, b_hi)
}

/// What can go wrong when decoding an instruction. Structural only;
/// semantic checks (register bounds, const bounds, etc.) are in the
/// verifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// No instruction word at this pc.
    Eof {
        /// The pc that was asked for.
        pc: u32,
    },
    /// An instruction byte is not one of the defined [`Op`] values.
    BadOpcode {
        /// The pc of the offending word.
        pc: u32,
        /// The raw byte.
        byte: u8,
    },
    /// A `WIDE` prefix is the last word of the function.
    DanglingWide {
        /// The pc of the dangling `WIDE`.
        pc: u32,
    },
    /// A `WIDE` is followed by another `WIDE`.
    DoubleWide {
        /// The pc of the first `WIDE`.
        pc: u32,
    },
}

/// Walk a function's instructions, yielding one decoded instruction
/// per step (a decoded instruction is 1 or 2 words). Errors stop the
/// walk; the first-word pc of every instruction is reachable through
/// `DecodedInstr::pc`.
pub fn walk(instrs: &[Instr]) -> InstrWalk<'_> {
    InstrWalk {
        instrs,
        pc: 0,
        done: false,
    }
}

/// Iterator returned by [`walk`].
pub struct InstrWalk<'a> {
    instrs: &'a [Instr],
    pc: u32,
    done: bool,
}

impl<'a> Iterator for InstrWalk<'a> {
    type Item = Result<DecodedInstr, DecodeError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.done || (self.pc as usize) >= self.instrs.len() {
            return None;
        }
        match decode(self.instrs, self.pc) {
            Ok(d) => {
                self.pc = d.pc + d.width;
                Some(Ok(d))
            }
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }
}

/// The set of first-word pcs in a function (every pc that starts a
/// decoded instruction, plus the one-past-the-end pc). Used by the
/// verifier to check that a jump target or a handler bound never
/// points at the second word of a `WIDE` pair.
pub fn first_word_pcs(instrs: &[Instr]) -> Result<FirstWordPcs, DecodeError> {
    let mut starts = Vec::with_capacity(instrs.len());
    let mut pc = 0u32;
    while (pc as usize) < instrs.len() {
        starts.push(pc);
        let d = decode(instrs, pc)?;
        pc = d.pc + d.width;
    }
    Ok(FirstWordPcs {
        starts,
        len: instrs.len() as u32,
    })
}

/// The set of first-word pcs returned by [`first_word_pcs`].
#[derive(Clone, Debug)]
pub struct FirstWordPcs {
    starts: Vec<u32>,
    len: u32,
}

impl FirstWordPcs {
    /// True iff `pc` is the pc of a first-word of some instruction.
    pub fn contains(&self, pc: u32) -> bool {
        self.starts.binary_search(&pc).is_ok()
    }

    /// A pc is "a valid boundary" when it is a first-word pc or when
    /// it equals the function's length (used by exclusive `pc_end`
    /// fields).
    pub fn is_boundary(&self, pc: u32) -> bool {
        pc == self.len || self.contains(pc)
    }

    /// The function's instruction count.
    pub fn len(&self) -> u32 {
        self.len
    }

    /// True iff the function has no instructions. An empty function
    /// never satisfies the verifier; this method exists to pair with
    /// [`FirstWordPcs::len`].
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The last first-word pc of this function.
    pub fn last_start(&self) -> Option<u32> {
        self.starts.last().copied()
    }

    /// Iterate first-word pcs in order.
    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        self.starts.iter().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opcode::Op;

    #[test]
    fn abc_round_trips() {
        let i = Instr::abc(Op::ADD, 1, 2, 3);
        assert_eq!(i.op(), Some(Op::ADD));
        assert_eq!(i.a(), 1);
        assert_eq!(i.b(), 2);
        assert_eq!(i.c(), 3);
    }

    #[test]
    fn abx_round_trips() {
        let i = Instr::abx(Op::LOADK, 7, 0x1234);
        assert_eq!(i.op(), Some(Op::LOADK));
        assert_eq!(i.a(), 7);
        assert_eq!(i.bx(), 0x1234);
    }

    #[test]
    fn asbx_round_trips_negative() {
        let i = Instr::asbx(Op::JMP, 0, -3);
        assert_eq!(i.op(), Some(Op::JMP));
        assert_eq!(i.sbx(), -3);
    }

    #[test]
    fn unknown_byte_is_none() {
        let bad = Instr { raw: 0xDEAD_BEEF };
        assert_eq!(bad.op(), None);
    }

    #[test]
    fn wide_prefix_widens_loadk_constant() {
        let w = wide_prefix(0, 0x0001); // Bx high 16 = 1
        let k = Instr::abx(Op::LOADK, 0, 0x2345);
        let d = decode(&[w, k], 0).unwrap();
        assert_eq!(d.op, Op::LOADK);
        assert!(d.wide);
        assert_eq!(d.width, 2);
        // Combined Bx = 0x0001_2345.
        assert_eq!(d.b_u, 0x0001_2345);
    }

    #[test]
    fn wide_prefix_widens_abc_three_operands() {
        // Pack B_hi=0x12, C_hi=0x34 into Bx = (0x34 << 8) | 0x12 = 0x3412.
        let w = wide_prefix(0x55, (0x34u16 << 8) | 0x12u16);
        let a = Instr::abc(Op::ADD, 0x66, 0x77, 0x88);
        let d = decode(&[w, a], 0).unwrap();
        assert!(d.wide);
        assert_eq!(d.a, (0x55u32 << 8) | 0x66u32);
        assert_eq!(d.b_u, (0x12u32 << 8) | 0x77u32);
        assert_eq!(d.c, (0x34u32 << 8) | 0x88u32);
    }

    #[test]
    fn wide_asbx_sign_extends() {
        // Bx = 0xFFFF, next sBx = 0xFFFF => combined 0xFFFF_FFFF as i32 = -1
        let w = wide_prefix(0, 0xFFFF);
        let j = Instr {
            raw: (Op::JMP as u32) | ((0xFFFF_u32) << 16),
        };
        let d = decode(&[w, j], 0).unwrap();
        assert_eq!(d.op, Op::JMP);
        assert_eq!(d.b_s, -1);
    }

    #[test]
    fn dangling_wide_is_an_error() {
        let w = wide_prefix(0, 0);
        let err = decode(&[w], 0).unwrap_err();
        assert!(matches!(err, DecodeError::DanglingWide { .. }));
    }

    #[test]
    fn double_wide_is_an_error() {
        let w = wide_prefix(0, 0);
        let err = decode(&[w, w, Instr::abc(Op::RET, 0, 0, 0)], 0).unwrap_err();
        assert!(matches!(err, DecodeError::DoubleWide { .. }));
    }

    #[test]
    fn first_word_pcs_skips_second_words() {
        let w = wide_prefix(0, 0);
        let loadk = Instr::abx(Op::LOADK, 0, 0);
        let ret = Instr::abc(Op::RET, 0, 0, 0);
        let instrs = vec![w, loadk, ret];
        let pcs = first_word_pcs(&instrs).unwrap();
        assert!(pcs.contains(0));
        assert!(!pcs.contains(1)); // second word of the pair
        assert!(pcs.contains(2));
        assert!(pcs.is_boundary(3)); // length boundary
    }
}
