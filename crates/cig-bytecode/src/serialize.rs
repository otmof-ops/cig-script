// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The module serializer. Writes a [`Module`] as a versioned, integrity-
//! hashed byte sequence. Pairs with [`crate::parse::parse_module`].
//!
//! The format is documented, as a contract, in `docs/BYTECODE.md`.

use crate::module::{Constant, Function, Module, UpvalDesc, FORMAT_VERSION, MAGIC};
use crate::opcode::FORMAT_VERSION_LATEST;

/// Serialize a module to bytes. Infallible (every [`Module`]
/// round-trips through [`crate::parse_module`] to an equal module).
pub fn serialize_module(m: &Module) -> Vec<u8> {
    let payload = build_payload(m);
    let mut out = Vec::with_capacity(4 + 2 + 2 + 32 + payload.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // flags, reserved
    let hash = blake3::hash(&payload);
    out.extend_from_slice(hash.as_bytes());
    out.extend_from_slice(&payload);
    debug_assert_eq!(FORMAT_VERSION, FORMAT_VERSION_LATEST);
    out
}

fn build_payload(m: &Module) -> Vec<u8> {
    let mut p = Vec::new();
    write_str(&mut p, &m.source_name);
    write_function(&mut p, &m.main);
    p
}

fn write_u8(p: &mut Vec<u8>, v: u8) {
    p.push(v);
}

fn write_u16(p: &mut Vec<u8>, v: u16) {
    p.extend_from_slice(&v.to_le_bytes());
}

fn write_u32(p: &mut Vec<u8>, v: u32) {
    p.extend_from_slice(&v.to_le_bytes());
}

fn write_i64(p: &mut Vec<u8>, v: i64) {
    p.extend_from_slice(&v.to_le_bytes());
}

fn write_f64(p: &mut Vec<u8>, v: f64) {
    p.extend_from_slice(&v.to_bits().to_le_bytes());
}

fn write_str(p: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    let len = bytes.len().min(u32::MAX as usize) as u32;
    write_u32(p, len);
    p.extend_from_slice(&bytes[..len as usize]);
}

fn write_constants(p: &mut Vec<u8>, cs: &[Constant]) {
    write_u32(p, cs.len().min(u32::MAX as usize) as u32);
    for c in cs {
        match c {
            Constant::Bool(b) => {
                write_u8(p, 1);
                write_u8(p, u8::from(*b));
            }
            Constant::Int(i) => {
                write_u8(p, 2);
                write_i64(p, *i);
            }
            Constant::Float(f) => {
                write_u8(p, 3);
                write_f64(p, *f);
            }
            Constant::Str(s) => {
                write_u8(p, 4);
                write_str(p, s);
            }
        }
    }
}

fn write_regions(p: &mut Vec<u8>, rs: &[crate::module::BurnRegion]) {
    write_u32(p, rs.len().min(u32::MAX as usize) as u32);
    for r in rs {
        write_u8(p, r.class);
        write_u8(p, r.label.tag());
        write_u32(p, r.label.index());
    }
}

fn write_upvalue(p: &mut Vec<u8>, u: &UpvalDesc) {
    write_u8(p, u.kind.tag());
    write_u16(p, u.index);
}

fn write_function(p: &mut Vec<u8>, f: &Function) {
    write_str(p, &f.name);
    write_u8(p, f.kind.tag());
    write_u32(p, f.reg_count);
    write_u16(p, f.arity);
    write_u8(p, u8::from(f.requires_burn));

    write_u16(p, (f.upvalues.len().min(u16::MAX as usize)) as u16);
    for u in &f.upvalues {
        write_upvalue(p, u);
    }

    write_constants(p, &f.constants);
    write_regions(p, &f.regions);

    // Handlers.
    write_u32(p, f.handlers.len().min(u32::MAX as usize) as u32);
    for h in &f.handlers {
        write_u32(p, h.pc_start);
        write_u32(p, h.pc_end);
        write_u32(p, h.handler_pc);
        write_u32(p, h.catch_reg);
        write_u32(p, h.finally_pc);
    }

    // Instructions.
    write_u32(p, f.instrs.len().min(u32::MAX as usize) as u32);
    for i in &f.instrs {
        p.extend_from_slice(&i.to_le_bytes());
    }

    // Span base.
    write_u32(p, f.span_base.byte_offset);
    write_u32(p, f.span_base.line);
    write_u32(p, f.span_base.col);

    // Line rows.
    write_u32(p, f.lines.len().min(u32::MAX as usize) as u32);
    for l in &f.lines {
        write_u32(p, l.pc);
        write_u32(p, l.span_off);
        write_u32(p, l.span_len);
        write_u32(p, l.line_off);
        write_u32(p, l.col);
    }

    // Region rows.
    write_u32(p, f.region_rows.len().min(u32::MAX as usize) as u32);
    for r in &f.region_rows {
        write_u32(p, r.pc_start);
        write_u32(p, r.pc_end);
        write_u32(p, r.region);
    }

    // Child functions.
    write_u32(p, f.children.len().min(u32::MAX as usize) as u32);
    for c in &f.children {
        write_function(p, c);
    }
}
