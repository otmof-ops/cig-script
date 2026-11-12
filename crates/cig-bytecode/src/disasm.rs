// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The text disassembler.
//!
//! Turns a [`Module`] into a human-readable listing. The disassembler
//! uses the shared decoder, so a `WIDE` prefix prints as one line with
//! the full operands and the prefix pc hidden behind the `w` marker in
//! the width column.

use crate::instr::{walk, DecodedInstr};
use crate::module::{Constant, Function, Module};
use crate::opcode::{Fmt, OperandKind};
use core::fmt::Write as _;

/// Disassemble a module.
pub fn disassemble_module(m: &Module) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "; module: {}",
        if m.source_name.is_empty() {
            "<anonymous>"
        } else {
            &m.source_name
        }
    );
    disasm_function(&mut out, &m.main, 0, &mut 0);
    out
}

fn disasm_function(out: &mut String, f: &Function, depth: usize, counter: &mut u32) {
    let idx = *counter;
    *counter += 1;
    let _ = writeln!(out);
    let indent = "  ".repeat(depth);
    let mut kind = match f.kind {
        crate::module::FuncKind::Main => "main",
        crate::module::FuncKind::Pull => "pull",
        crate::module::FuncKind::Lambda => "lambda",
    }
    .to_string();
    if f.requires_burn {
        kind.push_str(", requires_burn");
    }
    let name: &str = if f.name.is_empty() {
        "<anonymous>"
    } else {
        &f.name
    };
    let _ = writeln!(
        out,
        "{indent}fn {idx} {name} ({} args, {} regs, {}, {} consts, {} children, {} upvals, {} handlers, {} lines, {} regions, {} region rows):",
        f.arity,
        f.reg_count,
        kind,
        f.constants.len(),
        f.children.len(),
        f.upvalues.len(),
        f.handlers.len(),
        f.lines.len(),
        f.regions.len(),
        f.region_rows.len(),
    );
    for d in walk(&f.instrs) {
        match d {
            Ok(d) => disasm_instr(out, f, &d, depth),
            Err(e) => {
                let _ = writeln!(out, "{indent}  <decode error: {e:?}>");
                break;
            }
        }
    }
    for c in &f.children {
        disasm_function(out, c, depth + 1, counter);
    }
}

fn disasm_instr(out: &mut String, f: &Function, d: &DecodedInstr, depth: usize) {
    let indent = "  ".repeat(depth);
    let width_mark = if d.wide { "w" } else { " " };
    let info = d.op.info();
    let mut line = format!(
        "{indent}  {:04} {} {:<10}",
        d.pc,
        width_mark,
        d.op.mnemonic()
    );
    let mut first = true;
    let mut push = |line: &mut String, s: String| {
        if !s.is_empty() {
            if !first {
                line.push_str(", ");
            }
            line.push_str(&s);
            first = false;
        }
    };
    push(
        &mut line,
        format_operand(f, info.a, info.fmt, d.a as i64, d, OperandSlot::A),
    );
    push(
        &mut line,
        format_operand(f, info.b, info.fmt, b_value(d, info), d, OperandSlot::B),
    );
    push(
        &mut line,
        format_operand(f, info.c, info.fmt, c_value(d, info), d, OperandSlot::C),
    );
    let _ = writeln!(out, "{line}");
}

enum OperandSlot {
    A,
    B,
    C,
}

fn b_value(d: &DecodedInstr, info: crate::opcode::OpInfo) -> i64 {
    match (info.fmt, info.b) {
        (Fmt::AsBx, _) => d.b_s as i64,
        (_, OperandKind::Jump) => d.b_s as i64,
        _ => d.b_u as i64,
    }
}

fn c_value(d: &DecodedInstr, info: crate::opcode::OpInfo) -> i64 {
    match info.c {
        OperandKind::Jump => d.c_s as i64,
        _ => d.c as i64,
    }
}

fn format_operand(
    f: &Function,
    kind: OperandKind,
    _fmt: Fmt,
    v: i64,
    d: &DecodedInstr,
    _slot: OperandSlot,
) -> String {
    match kind {
        OperandKind::None => String::new(),
        OperandKind::Reg => format!("R{v}"),
        OperandKind::RegRange => format!("R{v}"),
        OperandKind::RegRangeCount => format!("x{v}"),
        OperandKind::Const => pretty_const(f, v),
        OperandKind::StrConst => pretty_const(f, v),
        OperandKind::Func => format!("Fn{v}"),
        OperandKind::Upvalue => format!("U{v}"),
        OperandKind::Handler => format!("H{v}"),
        OperandKind::Region => format!("Rg{v}"),
        OperandKind::Builtin => format!("B{v}"),
        OperandKind::Jump => {
            let target = (d.pc as i64) + 1 + v;
            format!("-> {target}")
        }
        OperandKind::Immediate => format!("{v}"),
        OperandKind::BurnClass => match v {
            0 => "class=normal".to_string(),
            1 => "class=unlit".to_string(),
            _ => format!("class={v}"),
        },
        OperandKind::BurnOpPacked => {
            let (kind_bits, argc) = if d.wide {
                (v & 0xFF, v >> 8)
            } else {
                (v & 0xF, v >> 4)
            };
            format!("kind={kind_bits}, argc={argc}")
        }
        OperandKind::Arity => format!("n={v}"),
        OperandKind::Wide => format!("wide={v:#x}"),
        OperandKind::Any => format!("{v}"),
    }
}

fn pretty_const(f: &Function, idx: i64) -> String {
    if idx < 0 || idx as usize >= f.constants.len() {
        return format!("K{idx} <out of range>");
    }
    let c = &f.constants[idx as usize];
    let pretty = match c {
        Constant::Bool(b) => format!("bool {b}"),
        Constant::Int(i) => format!("int {i}"),
        Constant::Float(fl) => format!("float {fl}"),
        Constant::Str(s) => {
            let mut short = s.clone();
            if short.len() > 48 {
                short.truncate(45);
                short.push_str("...");
            }
            format!("\"{}\"", short.replace('\\', "\\\\").replace('"', "\\\""))
        }
    };
    format!("K{idx} ({pretty})")
}
