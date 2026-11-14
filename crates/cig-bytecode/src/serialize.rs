// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The module serializer. Writes a [`Module`] as a versioned, integrity-
//! hashed byte sequence in layout 2: a head, a directory of fixed-width
//! function entries, the bodies, one string pool and the line region.
//! Pairs with [`crate::parse::parse_module`].
//!
//! The format is documented, as a contract, in `docs/BYTECODE.md`.

use crate::module::{Constant, Function, Module, FORMAT_VERSION, MAGIC, RESERVED_U32};
use crate::opcode::FORMAT_VERSION_LATEST;
use std::collections::HashMap;

/// The header's length: magic, version, flags, BLAKE3.
pub const HEADER_LEN: usize = 40;
/// The head's length, after the header: the function count and the
/// offsets and lengths of the module's regions.
pub const HEAD_LEN: usize = 32;
/// Where the directory starts: after the header and the head.
pub const DIRECTORY_AT: usize = HEADER_LEN + HEAD_LEN;
/// The length of one directory entry.
pub const ENTRY_LEN: usize = 80;
/// The width of each per-function table's entry, in body order:
/// instructions, constant values, handlers, region rows, children,
/// regions, upvalues, constant tags.
pub const INSTR_LEN: usize = 4;
/// A constant's value slot (its tag is in the tag table).
pub const CONST_LEN: usize = 8;
/// A handler: five `u32`.
pub const HANDLER_LEN: usize = 20;
/// A region row: three `u32`.
pub const REGION_ROW_LEN: usize = 12;
/// A child: its directory index.
pub const CHILD_LEN: usize = 4;
/// A burn region: class, label kind, two zero bytes, label.
pub const REGION_LEN: usize = 8;
/// An upvalue: kind, a zero byte, index.
pub const UPVALUE_LEN: usize = 4;

/// Serialize a module to bytes. Infallible (every [`Module`]
/// round-trips through [`crate::parse_module`] to an equal module, as
/// long as the module is under 4 GiB).
pub fn serialize_module(m: &Module) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // flags: none defined
    out.extend_from_slice(&[0u8; 32]); // the hash, written last
    write_payload(&mut out, m);
    let hash = blake3::hash(&out[HEADER_LEN..]);
    out[8..HEADER_LEN].copy_from_slice(hash.as_bytes());
    debug_assert_eq!(FORMAT_VERSION, FORMAT_VERSION_LATEST);
    out
}

/// One function in directory order, with its parent's and its children's
/// directory indices.
struct Flat<'a> {
    f: &'a Function,
    parent: u32,
    children: Vec<u32>,
}

/// The functions in pre-order: a function, then each child's subtree in
/// order. The index in this list is the function's `fn<i>`.
fn flatten(main: &Function) -> Vec<Flat<'_>> {
    let mut out = vec![Flat {
        f: main,
        parent: RESERVED_U32,
        children: Vec::new(),
    }];
    // An explicit stack: (function index, next child to visit).
    let mut stack: Vec<(usize, usize)> = vec![(0, 0)];
    while let Some(top) = stack.last_mut() {
        let (i, next) = *top;
        let f = out[i].f;
        if next == f.children.len() {
            stack.pop();
            continue;
        }
        top.1 += 1;
        let j = out.len();
        out.push(Flat {
            f: &f.children[next],
            parent: u32_of(i),
            children: Vec::new(),
        });
        out[i].children.push(u32_of(j));
        stack.push((j, 0));
    }
    out
}

fn u32_of(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The module's strings, each stored once, in the order first used.
#[derive(Default)]
struct Pool<'a> {
    bytes: Vec<u8>,
    seen: HashMap<&'a str, (u32, u32)>,
}

impl<'a> Pool<'a> {
    fn add(&mut self, s: &'a str) -> (u32, u32) {
        if let Some(r) = self.seen.get(s) {
            return *r;
        }
        let r = (u32_of(self.bytes.len()), u32_of(s.len()));
        self.bytes.extend_from_slice(s.as_bytes());
        self.seen.insert(s, r);
        r
    }
}

/// A function body's length: every table at its width, then the tags,
/// padded to a multiple of 4.
pub(crate) fn body_len(
    instrs: u64,
    constants: u64,
    handlers: u64,
    region_rows: u64,
    regions: u64,
    upvalues: u64,
    children: u64,
) -> u64 {
    let n = instrs * INSTR_LEN as u64
        + constants * (CONST_LEN as u64 + 1)
        + handlers * HANDLER_LEN as u64
        + region_rows * REGION_ROW_LEN as u64
        + children * CHILD_LEN as u64
        + regions * REGION_LEN as u64
        + upvalues * UPVALUE_LEN as u64;
    n.div_ceil(4) * 4
}

fn put_u32(p: &mut Vec<u8>, v: u32) {
    p.extend_from_slice(&v.to_le_bytes());
}

fn set_u32(p: &mut [u8], at: usize, v: u32) {
    p[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// A `u64` as LEB128: seven bits a byte, low first, the top bit set on
/// every byte but the last.
fn put_varint(p: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            p.push(b);
            return;
        }
        p.push(b | 0x80);
    }
}

/// A signed delta as an unsigned number: 0, -1, 1, -2, 2 ... as 0, 1, 2,
/// 3, 4 ...
fn zigzag(d: i64) -> u64 {
    ((d << 1) ^ (d >> 63)) as u64
}

/// One function's line rows, each field a delta from the row before
/// (the first row's from zero) where a delta is small: `pc`, `span_off`
/// and `line_off` as zigzag deltas, `span_len` and `col` as they are.
fn put_lines(p: &mut Vec<u8>, f: &Function) {
    let (mut pc, mut span_off, mut line_off) = (0i64, 0i64, 0i64);
    for l in &f.lines {
        put_varint(p, zigzag(i64::from(l.pc) - pc));
        put_varint(p, zigzag(i64::from(l.span_off) - span_off));
        put_varint(p, u64::from(l.span_len));
        put_varint(p, zigzag(i64::from(l.line_off) - line_off));
        put_varint(p, u64::from(l.col));
        pc = i64::from(l.pc);
        span_off = i64::from(l.span_off);
        line_off = i64::from(l.line_off);
    }
}

fn write_body<'a>(p: &mut Vec<u8>, f: &'a Function, children: &[u32], pool: &mut Pool<'a>) {
    let start = p.len();
    for i in &f.instrs {
        p.extend_from_slice(&i.to_le_bytes());
    }
    for c in &f.constants {
        let v: u64 = match c {
            Constant::Bool(b) => u64::from(*b),
            Constant::Int(i) => *i as u64,
            Constant::Float(x) => x.to_bits(),
            Constant::Str(s) => {
                let (off, len) = pool.add(s);
                u64::from(off) | (u64::from(len) << 32)
            }
        };
        p.extend_from_slice(&v.to_le_bytes());
    }
    for h in &f.handlers {
        for v in [
            h.pc_start,
            h.pc_end,
            h.handler_pc,
            h.catch_reg,
            h.finally_pc,
        ] {
            put_u32(p, v);
        }
    }
    for r in &f.region_rows {
        for v in [r.pc_start, r.pc_end, r.region] {
            put_u32(p, v);
        }
    }
    for c in children {
        put_u32(p, *c);
    }
    for r in &f.regions {
        p.extend_from_slice(&[r.class, r.label.tag(), 0, 0]);
        put_u32(p, r.label.index());
    }
    for u in &f.upvalues {
        p.extend_from_slice(&[u.kind.tag(), 0]);
        p.extend_from_slice(&u.index.to_le_bytes());
    }
    for c in &f.constants {
        p.push(match c {
            Constant::Bool(_) => 1,
            Constant::Int(_) => 2,
            Constant::Float(_) => 3,
            Constant::Str(_) => 4,
        });
    }
    while (p.len() - start) % 4 != 0 {
        p.push(0);
    }
}

fn write_payload(out: &mut Vec<u8>, m: &Module) {
    let flat = flatten(&m.main);
    let n = flat.len();
    let mut pool = Pool::default();
    let source_name = pool.add(&m.source_name);

    // The head and the directory, filled in as the bodies are written.
    let dir_end = DIRECTORY_AT + n * ENTRY_LEN;
    out.resize(dir_end, 0);
    let mut lines = Vec::new();
    for (i, fl) in flat.iter().enumerate() {
        let f = fl.f;
        let name = pool.add(&f.name);
        let body_at = out.len();
        write_body(out, f, &fl.children, &mut pool);
        let lines_at = lines.len();
        put_lines(&mut lines, f);
        let e = DIRECTORY_AT + i * ENTRY_LEN;
        let d = &mut out[e..e + ENTRY_LEN];
        let fields = [
            name.0,
            name.1,
            fl.parent,
            u32::from(f.kind.tag())
                | (u32::from(u8::from(f.requires_burn)) << 8)
                | (u32::from(f.arity) << 16),
            f.reg_count,
            u32_of(body_at),
            0, // the body's length, below
            u32_of(f.instrs.len()),
            u32_of(f.constants.len()),
            u32_of(f.handlers.len()),
            u32_of(f.region_rows.len()),
            u32_of(f.regions.len()),
            u32_of(f.upvalues.len()),
            u32_of(fl.children.len()),
            u32_of(lines_at),
            u32_of(lines.len() - lines_at),
            u32_of(f.lines.len()),
            f.span_base.byte_offset,
            f.span_base.line,
            f.span_base.col,
        ];
        for (k, v) in fields.iter().enumerate() {
            set_u32(d, k * 4, *v);
        }
        let body_len = out.len() - body_at;
        debug_assert_eq!(
            body_len as u64,
            self::body_len(
                f.instrs.len() as u64,
                f.constants.len() as u64,
                f.handlers.len() as u64,
                f.region_rows.len() as u64,
                f.regions.len() as u64,
                f.upvalues.len() as u64,
                fl.children.len() as u64,
            )
        );
        set_u32(&mut out[e..], 24, u32_of(body_len));
    }
    let pool_at = out.len();
    out.extend_from_slice(&pool.bytes);
    let lines_at = out.len();
    out.extend_from_slice(&lines);

    let head = [
        u32_of(n),
        source_name.0,
        source_name.1,
        u32_of(dir_end),
        u32_of(pool_at),
        u32_of(pool.bytes.len()),
        u32_of(lines_at),
        u32_of(lines.len()),
    ];
    for (k, v) in head.iter().enumerate() {
        set_u32(out, HEADER_LEN + k * 4, *v);
    }
}
