// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The module parser.
//!
//! The parser returns a [`Module`] or a [`ParseError`] for every input
//! it is given; it **never panics**.

use crate::error::ParseError;
use crate::instr::Instr;
use crate::module::{
    BurnRegion, Constant, FuncKind, Function, HandlerEntry, LineEntry, Module, RegionLabel,
    RegionRow, SpanBase, UpvalDesc, UpvalKind, FORMAT_VERSION, MAGIC, MAX_FUNCTION_DEPTH,
};

/// Parse a module from bytes. Returns the module, or the first
/// [`ParseError`] encountered.
pub fn parse_module(bytes: &[u8]) -> Result<Module, ParseError> {
    let mut r = Reader::new(bytes);

    // Header: 4-byte magic, 2-byte version, 2-byte flags, 32-byte hash.
    let magic = r.take(4, "magic")?;
    if magic != MAGIC {
        return Err(ParseError::BadMagic);
    }
    let version = r.read_u16("version")?;
    if version != FORMAT_VERSION {
        return Err(ParseError::BadVersion {
            found: version,
            supported: FORMAT_VERSION,
        });
    }
    // No flag is defined: a module with any bit set is not this revision.
    // With the magic, the version and the BLAKE3 over the payload, this
    // check leaves no byte of the module unchecked.
    let flags = r.read_u16("flags")?;
    if flags != 0 {
        return Err(ParseError::BadFlags { flags });
    }
    let hash_bytes = r.take(32, "integrity hash")?;
    let mut claimed_hash = [0u8; 32];
    claimed_hash.copy_from_slice(hash_bytes);

    let payload = r.rest();
    let actual_hash = blake3::hash(payload);
    if actual_hash.as_bytes() != &claimed_hash {
        return Err(ParseError::BadIntegrity);
    }

    let mut r = Reader::new(payload);
    let source_name = r.read_string("source name")?;
    let main = read_function(&mut r, 0)?;
    if r.remaining() != 0 {
        return Err(ParseError::TrailingBytes { offset: r.pos });
    }

    Ok(Module { source_name, main })
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Reader { bytes, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    fn rest(&self) -> &'a [u8] {
        &self.bytes[self.pos..]
    }

    fn take(&mut self, n: usize, at: &'static str) -> Result<&'a [u8], ParseError> {
        if self.remaining() < n {
            return Err(ParseError::Truncated {
                at,
                offset: self.pos,
            });
        }
        let out = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    fn read_u8(&mut self, at: &'static str) -> Result<u8, ParseError> {
        let b = self.take(1, at)?;
        Ok(b[0])
    }

    fn read_u16(&mut self, at: &'static str) -> Result<u16, ParseError> {
        let b = self.take(2, at)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn read_u32(&mut self, at: &'static str) -> Result<u32, ParseError> {
        let b = self.take(4, at)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn read_i64(&mut self, at: &'static str) -> Result<i64, ParseError> {
        let b = self.take(8, at)?;
        Ok(i64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn read_f64(&mut self, at: &'static str) -> Result<f64, ParseError> {
        let b = self.take(8, at)?;
        Ok(f64::from_bits(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ])))
    }

    fn read_string(&mut self, at: &'static str) -> Result<String, ParseError> {
        let len = self.read_u32(at)?;
        if (len as usize) > self.remaining() {
            return Err(ParseError::BadCount {
                at,
                count: len,
                remaining: self.remaining(),
            });
        }
        let bytes = self.take(len as usize, at)?;
        match core::str::from_utf8(bytes) {
            Ok(s) => Ok(s.to_owned()),
            Err(_) => Err(ParseError::BadString { at, index: 0 }),
        }
    }
}

fn min_entry_bytes(at: &'static str) -> usize {
    match at {
        "constants" => 1,
        "regions" => 1 + 1 + 4,
        "handlers" => 5 * 4,
        "lines" => 5 * 4,
        "region rows" => 3 * 4,
        "upvalues" => 1 + 2,
        "instructions" => 4,
        "children" => 4 + 1 + 4 + 2 + 1 + 2 + 4 + 4 + 4 + 4 + 4 + 4 + 4 + 4 + 4,
        _ => 1,
    }
}

fn read_count(r: &mut Reader<'_>, at: &'static str) -> Result<u32, ParseError> {
    let n = r.read_u32(at)?;
    let min = min_entry_bytes(at);
    let need = (n as u64).saturating_mul(min as u64);
    if need > r.remaining() as u64 {
        return Err(ParseError::BadCount {
            at,
            count: n,
            remaining: r.remaining(),
        });
    }
    Ok(n)
}

fn read_constants(r: &mut Reader<'_>) -> Result<Vec<Constant>, ParseError> {
    let n = read_count(r, "constants")?;
    let mut out = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let tag = r.read_u8("constant tag")?;
        let c = match tag {
            1 => Constant::Bool(r.read_u8("bool")? != 0),
            2 => Constant::Int(r.read_i64("int")?),
            3 => Constant::Float(r.read_f64("float")?),
            4 => Constant::Str(r.read_string("string constant")?),
            _ => {
                return Err(ParseError::BadTag {
                    at: "constant",
                    value: tag as u32,
                })
            }
        };
        out.push(c);
    }
    Ok(out)
}

fn read_regions(r: &mut Reader<'_>) -> Result<Vec<BurnRegion>, ParseError> {
    let n = read_count(r, "regions")?;
    let mut out = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let class = r.read_u8("region class")?;
        if class > 1 {
            return Err(ParseError::BadTag {
                at: "region class",
                value: class as u32,
            });
        }
        let tag = r.read_u8("region label kind")?;
        let index = r.read_u32("region label")?;
        let Some(label) = RegionLabel::from_wire(tag, index) else {
            return Err(ParseError::BadTag {
                at: "region label kind",
                value: tag as u32,
            });
        };
        out.push(BurnRegion { class, label });
    }
    Ok(out)
}

fn read_function(r: &mut Reader<'_>, depth: u32) -> Result<Function, ParseError> {
    if depth > MAX_FUNCTION_DEPTH {
        return Err(ParseError::TooDeep {
            depth,
            max: MAX_FUNCTION_DEPTH,
        });
    }
    let name = r.read_string("function name")?;
    let kind_byte = r.read_u8("function kind")?;
    let kind = FuncKind::from_tag(kind_byte).ok_or(ParseError::BadTag {
        at: "function kind",
        value: kind_byte as u32,
    })?;
    let reg_count = r.read_u32("reg count")?;
    let arity = r.read_u16("arity")?;
    let requires_burn = r.read_u8("requires_burn")? != 0;

    let n_upvals = r.read_u16("upvalue count")? as u32;
    let min = min_entry_bytes("upvalues");
    if (n_upvals as u64).saturating_mul(min as u64) > r.remaining() as u64 {
        return Err(ParseError::BadCount {
            at: "upvalues",
            count: n_upvals,
            remaining: r.remaining(),
        });
    }
    let mut upvalues = Vec::with_capacity(n_upvals as usize);
    for _ in 0..n_upvals {
        let k = r.read_u8("upvalue kind")?;
        let idx = r.read_u16("upvalue index")?;
        let kind = UpvalKind::from_tag(k).ok_or(ParseError::BadTag {
            at: "upvalue kind",
            value: k as u32,
        })?;
        upvalues.push(UpvalDesc { kind, index: idx });
    }

    let constants = read_constants(r)?;
    let regions = read_regions(r)?;

    let n_handlers = read_count(r, "handlers")?;
    let mut handlers = Vec::with_capacity(n_handlers as usize);
    for _ in 0..n_handlers {
        let pc_start = r.read_u32("handler pc_start")?;
        let pc_end = r.read_u32("handler pc_end")?;
        let handler_pc = r.read_u32("handler handler_pc")?;
        let catch_reg = r.read_u32("handler catch_reg")?;
        let finally_pc = r.read_u32("handler finally_pc")?;
        handlers.push(HandlerEntry {
            pc_start,
            pc_end,
            handler_pc,
            catch_reg,
            finally_pc,
        });
    }

    let n_instrs = read_count(r, "instructions")?;
    let mut instrs = Vec::with_capacity(n_instrs as usize);
    for _ in 0..n_instrs {
        let b = r.take(4, "instruction")?;
        instrs.push(Instr::from_le_bytes([b[0], b[1], b[2], b[3]]));
    }

    let span_base = SpanBase {
        byte_offset: r.read_u32("span_base byte_offset")?,
        line: r.read_u32("span_base line")?,
        col: r.read_u32("span_base col")?,
    };

    let n_lines = read_count(r, "lines")?;
    let mut lines = Vec::with_capacity(n_lines as usize);
    for _ in 0..n_lines {
        let pc = r.read_u32("line pc")?;
        let span_off = r.read_u32("line span_off")?;
        let span_len = r.read_u32("line span_len")?;
        let line_off = r.read_u32("line line_off")?;
        let col = r.read_u32("line col")?;
        lines.push(LineEntry {
            pc,
            span_off,
            span_len,
            line_off,
            col,
        });
    }

    let n_regions = read_count(r, "region rows")?;
    let mut region_rows = Vec::with_capacity(n_regions as usize);
    for _ in 0..n_regions {
        let pc_start = r.read_u32("region row pc_start")?;
        let pc_end = r.read_u32("region row pc_end")?;
        let region = r.read_u32("region row region")?;
        region_rows.push(RegionRow {
            pc_start,
            pc_end,
            region,
        });
    }

    let n_children = read_count(r, "children")?;
    let mut children = Vec::with_capacity(n_children as usize);
    for _ in 0..n_children {
        children.push(read_function(r, depth + 1)?);
    }

    Ok(Function {
        name,
        kind,
        reg_count,
        arity,
        requires_burn,
        upvalues,
        constants,
        children,
        regions,
        handlers,
        instrs,
        span_base,
        lines,
        region_rows,
    })
}
