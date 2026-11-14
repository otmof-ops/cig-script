// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The module parser (layout 2: `docs/BYTECODE.md`, "File layout").
//!
//! The parser returns a [`Module`] or a [`ParseError`] for every input
//! it is given; it **never panics**. Every offset, length and count is
//! checked against the one place the layout gives it before anything is
//! allocated or read through it.

use crate::error::ParseError;
use crate::instr::Instr;
use crate::module::{
    BurnRegion, Constant, FuncKind, Function, HandlerEntry, LineEntry, Module, RegionLabel,
    RegionRow, SpanBase, UpvalDesc, UpvalKind, FORMAT_VERSION, MAGIC, MAX_FUNCTION_DEPTH,
    RESERVED_U32,
};
use crate::serialize::{
    body_len, CHILD_LEN, CONST_LEN, DIRECTORY_AT, ENTRY_LEN, HANDLER_LEN, HEADER_LEN, INSTR_LEN,
    REGION_LEN, REGION_ROW_LEN, UPVALUE_LEN,
};
use crate::verify::{ParentCounts, Place};

/// Parse a module from bytes. Returns the module, or the first
/// [`ParseError`] encountered.
pub fn parse_module(bytes: &[u8]) -> Result<Module, ParseError> {
    let index = Index::read(bytes)?;
    let n = index.entries.len();

    // Build each function, the last first: a function's children all
    // come after it, so they are built when it is.
    let mut built: Vec<Option<Function>> = Vec::with_capacity(n);
    built.resize_with(n, || None);
    for (i, e) in index.entries.iter().enumerate().rev() {
        let mut f = read_function(bytes, &index.head, e)?;
        let kids = index.children(i);
        let mut children = Vec::with_capacity(kids.len());
        for &c in kids {
            match built.get_mut(c as usize).and_then(Option::take) {
                Some(child) => children.push(child),
                None => {
                    return Err(ParseError::BadTree {
                        function: i as u32,
                        at: "child",
                    })
                }
            }
        }
        f.children = children;
        built[i] = Some(f);
    }
    match built.first_mut().and_then(Option::take) {
        Some(main) => Ok(Module {
            source_name: index.source_name,
            main,
        }),
        None => Err(ParseError::BadCount {
            at: "functions",
            count: 0,
            remaining: bytes.len(),
        }),
    }
}

/// A module opened for reading one function at a time (layout 2 makes it
/// possible: `docs/BYTECODE.md`, "How the VM reads it").
///
/// [`Reader::open`] makes every check that covers the module as a whole:
/// the header (magic, version, flags), the BLAKE3 over every byte after
/// it, the head, the string pool as UTF-8, every directory entry's body
/// and line-row offsets and lengths against the places the layout gives
/// them, the child tables as one tree in pre-order, and the depth. A
/// function's own bytes (its name, kind, tables, constants, padding and
/// line rows) are checked by [`Reader::head`] and [`Reader::function`],
/// each before anything is built from them: the same checks
/// [`parse_module`] makes, with the same errors.
#[derive(Debug)]
pub struct Reader {
    bytes: Vec<u8>,
    index: Index,
}

/// What [`Reader::head`] reads of a function: its directory fields and its
/// upvalue descriptors, without its code, tables or line rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionHead {
    /// The function's name (empty for an anonymous lambda).
    pub name: String,
    /// Its kind.
    pub kind: FuncKind,
    /// Its `requires_burn` flag.
    pub requires_burn: bool,
    /// Its parameter count.
    pub arity: u16,
    /// Its register count.
    pub reg_count: u32,
    /// Its upvalue descriptors.
    pub upvalues: Vec<UpvalDesc>,
}

impl Reader {
    /// Open a module: every check that covers the module as a whole (see
    /// [`Reader`]). No function body is read.
    pub fn open(bytes: Vec<u8>) -> Result<Reader, ParseError> {
        let index = Index::read(&bytes)?;
        Ok(Reader { bytes, index })
    }

    /// The number of functions (at least 1).
    pub fn len(&self) -> usize {
        self.index.entries.len()
    }

    /// Always false: a module has a top level.
    pub fn is_empty(&self) -> bool {
        self.index.entries.is_empty()
    }

    /// The module's source name.
    pub fn source_name(&self) -> &str {
        &self.index.source_name
    }

    /// The pre-order indices of function `i`'s children, in its child
    /// table's order (checked at open); empty past the last function.
    pub fn children(&self, i: u32) -> &[u32] {
        self.index.children(i as usize)
    }

    /// Function `i`'s place in the tree, for the verifier
    /// ([`crate::verify_function`]): its index, depth, child count and
    /// its parent's register and upvalue counts, all from the directory.
    pub fn place(&self, i: u32) -> Option<Place> {
        let e = self.index.entries.get(i as usize)?;
        let parent = match e.parent {
            RESERVED_U32 => None,
            p => {
                let pe = self.index.entries.get(p as usize)?;
                Some(ParentCounts {
                    reg_count: pe.reg_count,
                    upvalues: pe.upvalues,
                })
            }
        };
        Some(Place {
            index: i,
            depth: self.index.depth.get(i as usize).copied()?,
            children: e.children,
            parent,
        })
    }

    /// Function `i`'s directory fields and upvalue descriptors, each
    /// checked as [`parse_module`] checks it.
    pub fn head(&self, i: u32) -> Result<FunctionHead, ParseError> {
        let e = self.entry(i)?;
        let (name, kind) = read_name_kind(&self.bytes, &self.index.head, e)?;
        Ok(FunctionHead {
            name,
            kind,
            requires_burn: e.requires_burn == 1,
            arity: e.arity,
            reg_count: e.reg_count,
            upvalues: read_upvalues(&self.bytes, e)?,
        })
    }

    /// Function `i`, read from its body and its line rows with every
    /// check [`parse_module`] makes of them. Its `children` is empty: its
    /// children are functions of their own ([`Reader::children`]).
    pub fn function(&self, i: u32) -> Result<Function, ParseError> {
        let e = self.entry(i)?;
        read_function(&self.bytes, &self.index.head, e)
    }

    fn entry(&self, i: u32) -> Result<&Entry, ParseError> {
        self.index
            .entries
            .get(i as usize)
            .ok_or(ParseError::BadIndex {
                at: "function",
                index: i,
                len: self.index.entries.len(),
            })
    }
}

/// What opening a module reads: the head, the source name, the directory
/// with every offset and length checked, and the tree.
#[derive(Debug)]
struct Index {
    head: Head,
    source_name: String,
    entries: Vec<Entry>,
    /// Every function's child table, one after another.
    kids: Vec<u32>,
    /// Where function `i`'s children start in `kids` (one more entry than
    /// there are functions).
    kids_at: Vec<usize>,
    /// Each function's depth: 0 for the top level.
    depth: Vec<u32>,
}

impl Index {
    fn read(bytes: &[u8]) -> Result<Index, ParseError> {
        // Header: 4-byte magic, 2-byte version, 2-byte flags, 32-byte hash.
        let magic = take(bytes, 0, 4, "magic")?;
        if magic != MAGIC {
            return Err(ParseError::BadMagic);
        }
        let version = u16_at(bytes, 4, "version")?;
        if version != FORMAT_VERSION {
            return Err(ParseError::BadVersion {
                found: version,
                supported: FORMAT_VERSION,
            });
        }
        // No flag is defined: a module with any bit set is not this revision.
        // With the magic, the version and the BLAKE3 over the payload, this
        // check leaves no byte of the module unchecked.
        let flags = u16_at(bytes, 6, "flags")?;
        if flags != 0 {
            return Err(ParseError::BadFlags { flags });
        }
        let claimed_hash = take(bytes, 8, 32, "integrity hash")?;
        let actual_hash = blake3::hash(&bytes[HEADER_LEN..]);
        if actual_hash.as_bytes()[..] != claimed_hash[..] {
            return Err(ParseError::BadIntegrity);
        }

        let head = Head::read(bytes)?;
        Pool::check(bytes, &head)?;
        let source_name = pool_str(
            bytes,
            &head,
            u32_at(bytes, 44, "source name")?,
            u32_at(bytes, 48, "source name")?,
            "source name",
        )?
        .to_owned();

        let n = head.functions as usize;
        let mut entries = Vec::with_capacity(n);
        let mut body_at = head.bodies_at;
        let mut lines_at = 0u64;
        for i in 0..head.functions {
            let e = Entry::read(bytes, i)?;
            e.expect("body offset", u64::from(e.body_at), body_at)?;
            let want = body_len(
                u64::from(e.instrs),
                u64::from(e.constants),
                u64::from(e.handlers),
                u64::from(e.region_rows),
                u64::from(e.regions),
                u64::from(e.upvalues),
                u64::from(e.children),
            );
            e.expect("body length", u64::from(e.body_len), want)?;
            body_at += want;
            if body_at > head.pool_at {
                return Err(ParseError::BadOffset {
                    at: "body length",
                    function: i,
                    found: u64::from(e.body_len),
                    expected: head.pool_at - u64::from(e.body_at),
                });
            }
            e.expect("line rows offset", u64::from(e.lines_at), lines_at)?;
            lines_at += u64::from(e.lines_len);
            if lines_at > head.lines_len {
                return Err(ParseError::BadOffset {
                    at: "line rows length",
                    function: i,
                    found: u64::from(e.lines_len),
                    expected: head.lines_len - u64::from(e.lines_at),
                });
            }
            entries.push(e);
        }
        expect_head("pool offset", head.pool_at, body_at)?;
        expect_head("line region length", head.lines_len, lines_at)?;

        let (kids, kids_at, depth) = check_tree(bytes, &entries)?;
        Ok(Index {
            head,
            source_name,
            entries,
            kids,
            kids_at,
            depth,
        })
    }

    fn children(&self, i: usize) -> &[u32] {
        match (self.kids_at.get(i), self.kids_at.get(i + 1)) {
            (Some(&a), Some(&b)) => self.kids.get(a..b).unwrap_or(&[]),
            _ => &[],
        }
    }
}

fn take<'a>(
    bytes: &'a [u8],
    at: usize,
    n: usize,
    what: &'static str,
) -> Result<&'a [u8], ParseError> {
    match at.checked_add(n) {
        Some(end) if end <= bytes.len() => Ok(&bytes[at..end]),
        _ => Err(ParseError::Truncated {
            at: what,
            offset: at.min(bytes.len()),
        }),
    }
}

fn u16_at(bytes: &[u8], at: usize, what: &'static str) -> Result<u16, ParseError> {
    let b = take(bytes, at, 2, what)?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

fn u32_at(bytes: &[u8], at: usize, what: &'static str) -> Result<u32, ParseError> {
    let b = take(bytes, at, 4, what)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn u64_at(bytes: &[u8], at: usize, what: &'static str) -> Result<u64, ParseError> {
    let b = take(bytes, at, 8, what)?;
    Ok(u64::from_le_bytes([
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
    ]))
}

fn expect_head(at: &'static str, found: u64, expected: u64) -> Result<(), ParseError> {
    if found == expected {
        Ok(())
    } else {
        Err(ParseError::BadOffset {
            at,
            function: RESERVED_U32,
            found,
            expected,
        })
    }
}

/// The head: the function count and where each region lies.
#[derive(Debug)]
struct Head {
    functions: u32,
    bodies_at: u64,
    pool_at: u64,
    pool_len: u64,
    lines_at: u64,
    lines_len: u64,
}

impl Head {
    fn read(bytes: &[u8]) -> Result<Head, ParseError> {
        let functions = u32_at(bytes, 40, "function count")?;
        let remaining = bytes.len().saturating_sub(DIRECTORY_AT);
        if functions == 0 || u64::from(functions) * ENTRY_LEN as u64 > remaining as u64 {
            return Err(ParseError::BadCount {
                at: "functions",
                count: functions,
                remaining,
            });
        }
        let h = Head {
            functions,
            bodies_at: u64::from(u32_at(bytes, 52, "bodies offset")?),
            pool_at: u64::from(u32_at(bytes, 56, "pool offset")?),
            pool_len: u64::from(u32_at(bytes, 60, "pool length")?),
            lines_at: u64::from(u32_at(bytes, 64, "line region offset")?),
            lines_len: u64::from(u32_at(bytes, 68, "line region length")?),
        };
        let dir_end = DIRECTORY_AT as u64 + u64::from(functions) * ENTRY_LEN as u64;
        expect_head("bodies offset", h.bodies_at, dir_end)?;
        if h.pool_at < h.bodies_at {
            return Err(ParseError::BadOffset {
                at: "pool offset",
                function: RESERVED_U32,
                found: h.pool_at,
                expected: h.bodies_at,
            });
        }
        expect_head("line region offset", h.lines_at, h.pool_at + h.pool_len)?;
        let end = h.lines_at + h.lines_len;
        let len = bytes.len() as u64;
        if end > len {
            return Err(ParseError::Truncated {
                at: "line region",
                offset: bytes.len(),
            });
        }
        if end < len {
            return Err(ParseError::TrailingBytes {
                offset: end as usize,
            });
        }
        Ok(h)
    }
}

/// The string pool, checked as UTF-8 once, when the module is opened.
struct Pool;

impl Pool {
    fn check(bytes: &[u8], h: &Head) -> Result<(), ParseError> {
        let raw = &bytes[h.pool_at as usize..(h.pool_at + h.pool_len) as usize];
        match core::str::from_utf8(raw) {
            Ok(_) => Ok(()),
            Err(e) => Err(ParseError::BadString {
                at: "string pool",
                index: e.valid_up_to() as u32,
            }),
        }
    }
}

/// The pool's string at `off`, `len` bytes long: inside the pool, and
/// starting and ending on a character. The pool is UTF-8 as a whole
/// (checked at open), so the slice is UTF-8 exactly when both of its ends
/// fall on a character.
fn pool_str<'a>(
    bytes: &'a [u8],
    h: &Head,
    off: u32,
    len: u32,
    what: &'static str,
) -> Result<&'a str, ParseError> {
    let end = u64::from(off) + u64::from(len);
    if end > h.pool_len {
        return Err(ParseError::BadIndex {
            at: what,
            index: off,
            len: h.pool_len as usize,
        });
    }
    let start = h.pool_at + u64::from(off);
    let raw = bytes
        .get(start as usize..(h.pool_at + end) as usize)
        .ok_or(ParseError::BadIndex {
            at: what,
            index: off,
            len: h.pool_len as usize,
        })?;
    core::str::from_utf8(raw).map_err(|_| ParseError::BadString {
        at: what,
        index: off,
    })
}

/// One directory entry, as the file holds it.
#[derive(Debug)]
struct Entry {
    index: u32,
    name_at: u32,
    name_len: u32,
    parent: u32,
    kind: u8,
    requires_burn: u8,
    arity: u16,
    reg_count: u32,
    body_at: u32,
    body_len: u32,
    instrs: u32,
    constants: u32,
    handlers: u32,
    region_rows: u32,
    regions: u32,
    upvalues: u32,
    children: u32,
    lines_at: u32,
    lines_len: u32,
    lines: u32,
    span_base: SpanBase,
}

impl Entry {
    fn read(bytes: &[u8], index: u32) -> Result<Entry, ParseError> {
        let e = DIRECTORY_AT + index as usize * ENTRY_LEN;
        let d = take(bytes, e, ENTRY_LEN, "directory entry")?;
        let w = |k: usize| u32::from_le_bytes([d[k], d[k + 1], d[k + 2], d[k + 3]]);
        Ok(Entry {
            index,
            name_at: w(0),
            name_len: w(4),
            parent: w(8),
            kind: d[12],
            requires_burn: d[13],
            arity: u16::from_le_bytes([d[14], d[15]]),
            reg_count: w(16),
            body_at: w(20),
            body_len: w(24),
            instrs: w(28),
            constants: w(32),
            handlers: w(36),
            region_rows: w(40),
            regions: w(44),
            upvalues: w(48),
            children: w(52),
            lines_at: w(56),
            lines_len: w(60),
            lines: w(64),
            span_base: SpanBase {
                byte_offset: w(68),
                line: w(72),
                col: w(76),
            },
        })
    }

    fn expect(&self, at: &'static str, found: u64, expected: u64) -> Result<(), ParseError> {
        if found == expected {
            Ok(())
        } else {
            Err(ParseError::BadOffset {
                at,
                function: self.index,
                found,
                expected,
            })
        }
    }

    // Where each table starts, from the body's start (the order of
    // `docs/BYTECODE.md`, "Function body").
    fn consts_at(&self) -> usize {
        self.instrs as usize * INSTR_LEN
    }
    fn handlers_at(&self) -> usize {
        self.consts_at() + self.constants as usize * CONST_LEN
    }
    fn region_rows_at(&self) -> usize {
        self.handlers_at() + self.handlers as usize * HANDLER_LEN
    }
    fn child_table_at(&self) -> usize {
        self.region_rows_at() + self.region_rows as usize * REGION_ROW_LEN
    }
    fn regions_at(&self) -> usize {
        self.child_table_at() + self.children as usize * CHILD_LEN
    }
    fn upvalues_at(&self) -> usize {
        self.regions_at() + self.regions as usize * REGION_LEN
    }
    fn tags_at(&self) -> usize {
        self.upvalues_at() + self.upvalues as usize * UPVALUE_LEN
    }
    fn padding_at(&self) -> usize {
        self.tags_at() + self.constants as usize
    }
}

/// The directory describes one tree in pre-order: fn 0 has no parent;
/// every later function's parent is fn 0 or a function on the path from
/// fn 0 to the function before it; each function lists as its children
/// exactly the functions that name it as their parent, in increasing
/// order; no function is deeper than [`MAX_FUNCTION_DEPTH`].
/// Returns every child table, one after another, where each function's
/// starts, and each function's depth.
#[allow(clippy::type_complexity)]
fn check_tree(
    bytes: &[u8],
    entries: &[Entry],
) -> Result<(Vec<u32>, Vec<usize>, Vec<u32>), ParseError> {
    let parents: Vec<u32> = entries.iter().map(|e| e.parent).collect();
    let mut depths: Vec<u32> = Vec::with_capacity(entries.len());
    // The path from fn 0 to the function before.
    let mut path: Vec<u32> = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        let i = i as u32;
        if i == 0 {
            if e.parent != RESERVED_U32 {
                return Err(ParseError::BadTree {
                    function: 0,
                    at: "parent",
                });
            }
        } else {
            if e.parent >= i {
                return Err(ParseError::BadTree {
                    function: i,
                    at: "parent",
                });
            }
            while path.last().is_some_and(|p| *p != e.parent) {
                path.pop();
            }
            if path.is_empty() {
                return Err(ParseError::BadTree {
                    function: i,
                    at: "pre-order",
                });
            }
        }
        path.push(i);
        let depth = path.len() as u32 - 1;
        if depth > MAX_FUNCTION_DEPTH {
            return Err(ParseError::TooDeep {
                depth,
                max: MAX_FUNCTION_DEPTH,
            });
        }
        depths.push(depth);
    }
    let mut listed = 0u64;
    let mut kids: Vec<u32> = Vec::with_capacity(entries.len().saturating_sub(1));
    let mut kids_at: Vec<usize> = Vec::with_capacity(entries.len() + 1);
    for e in entries {
        kids_at.push(kids.len());
        let at = e.body_at as usize + e.child_table_at();
        let mut prev: Option<u32> = None;
        for k in 0..e.children as usize {
            let c = u32_at(bytes, at + k * CHILD_LEN, "child")?;
            let Some(p) = parents.get(c as usize) else {
                return Err(ParseError::BadIndex {
                    at: "child",
                    index: c,
                    len: entries.len(),
                });
            };
            if *p != e.index {
                return Err(ParseError::BadTree {
                    function: e.index,
                    at: "child",
                });
            }
            if prev.is_some_and(|q| q >= c) {
                return Err(ParseError::BadTree {
                    function: e.index,
                    at: "child order",
                });
            }
            prev = Some(c);
            kids.push(c);
        }
        listed += u64::from(e.children);
    }
    kids_at.push(kids.len());
    if listed != entries.len() as u64 - 1 {
        return Err(ParseError::BadTree {
            function: 0,
            at: "child count",
        });
    }
    Ok((kids, kids_at, depths))
}

/// A function's name, kind and `requires_burn` flag, checked.
fn read_name_kind(bytes: &[u8], head: &Head, e: &Entry) -> Result<(String, FuncKind), ParseError> {
    let name = pool_str(bytes, head, e.name_at, e.name_len, "function name")?.to_owned();
    let kind = FuncKind::from_tag(e.kind).ok_or(ParseError::BadTag {
        at: "function kind",
        value: u32::from(e.kind),
    })?;
    if e.requires_burn > 1 {
        return Err(ParseError::BadTag {
            at: "requires_burn",
            value: u32::from(e.requires_burn),
        });
    }
    Ok((name, kind))
}

/// A function's upvalue descriptors, checked.
fn read_upvalues(bytes: &[u8], e: &Entry) -> Result<Vec<UpvalDesc>, ParseError> {
    let body = e.body_at as usize;
    let mut upvalues = Vec::with_capacity(e.upvalues as usize);
    for k in 0..e.upvalues as usize {
        let at = body + e.upvalues_at() + k * UPVALUE_LEN;
        let b = take(bytes, at, UPVALUE_LEN, "upvalue")?;
        if b[1] != 0 {
            return Err(ParseError::BadPadding {
                at: "upvalue padding",
                offset: at + 1,
            });
        }
        let kind = UpvalKind::from_tag(b[0]).ok_or(ParseError::BadTag {
            at: "upvalue kind",
            value: u32::from(b[0]),
        })?;
        upvalues.push(UpvalDesc {
            kind,
            index: u16::from_le_bytes([b[2], b[3]]),
        });
    }
    Ok(upvalues)
}

fn read_function(bytes: &[u8], head: &Head, e: &Entry) -> Result<Function, ParseError> {
    let (name, kind) = read_name_kind(bytes, head, e)?;
    let body = e.body_at as usize;

    let mut instrs = Vec::with_capacity(e.instrs as usize);
    for k in 0..e.instrs as usize {
        let b = take(bytes, body + k * INSTR_LEN, INSTR_LEN, "instruction")?;
        instrs.push(Instr::from_le_bytes([b[0], b[1], b[2], b[3]]));
    }

    let mut constants = Vec::with_capacity(e.constants as usize);
    for k in 0..e.constants as usize {
        let tag = take(bytes, body + e.tags_at() + k, 1, "constant tag")?[0];
        let v = u64_at(bytes, body + e.consts_at() + k * CONST_LEN, "constant")?;
        constants.push(match tag {
            1 if v <= 1 => Constant::Bool(v == 1),
            1 => {
                return Err(ParseError::BadTag {
                    at: "bool",
                    value: v.min(u64::from(u32::MAX)) as u32,
                })
            }
            2 => Constant::Int(v as i64),
            3 => Constant::Float(f64::from_bits(v)),
            4 => Constant::Str(
                pool_str(bytes, head, v as u32, (v >> 32) as u32, "string constant")?.to_owned(),
            ),
            _ => {
                return Err(ParseError::BadTag {
                    at: "constant",
                    value: u32::from(tag),
                })
            }
        });
    }

    let mut handlers = Vec::with_capacity(e.handlers as usize);
    for k in 0..e.handlers as usize {
        let at = body + e.handlers_at() + k * HANDLER_LEN;
        handlers.push(HandlerEntry {
            pc_start: u32_at(bytes, at, "handler pc_start")?,
            pc_end: u32_at(bytes, at + 4, "handler pc_end")?,
            handler_pc: u32_at(bytes, at + 8, "handler handler_pc")?,
            catch_reg: u32_at(bytes, at + 12, "handler catch_reg")?,
            finally_pc: u32_at(bytes, at + 16, "handler finally_pc")?,
        });
    }

    let mut region_rows = Vec::with_capacity(e.region_rows as usize);
    for k in 0..e.region_rows as usize {
        let at = body + e.region_rows_at() + k * REGION_ROW_LEN;
        region_rows.push(RegionRow {
            pc_start: u32_at(bytes, at, "region row pc_start")?,
            pc_end: u32_at(bytes, at + 4, "region row pc_end")?,
            region: u32_at(bytes, at + 8, "region row region")?,
        });
    }

    let mut regions = Vec::with_capacity(e.regions as usize);
    for k in 0..e.regions as usize {
        let at = body + e.regions_at() + k * REGION_LEN;
        let b = take(bytes, at, 4, "region")?;
        let (class, tag) = (b[0], b[1]);
        if b[2] != 0 || b[3] != 0 {
            return Err(ParseError::BadPadding {
                at: "region padding",
                offset: at + 2,
            });
        }
        if class > 1 {
            return Err(ParseError::BadTag {
                at: "region class",
                value: u32::from(class),
            });
        }
        let index = u32_at(bytes, at + 4, "region label")?;
        let Some(label) = RegionLabel::from_wire(tag, index) else {
            return Err(ParseError::BadTag {
                at: "region label kind",
                value: u32::from(tag),
            });
        };
        regions.push(BurnRegion { class, label });
    }

    let upvalues = read_upvalues(bytes, e)?;

    let pad_at = body + e.padding_at();
    let pad_end = body + e.body_len as usize;
    if let Some(k) = take(bytes, pad_at, pad_end - pad_at, "body padding")?
        .iter()
        .position(|b| *b != 0)
    {
        return Err(ParseError::BadPadding {
            at: "body padding",
            offset: pad_at + k,
        });
    }

    let lines = read_lines(bytes, head, e)?;

    Ok(Function {
        name,
        kind,
        reg_count: e.reg_count,
        arity: e.arity,
        requires_burn: e.requires_burn == 1,
        upvalues,
        constants,
        children: Vec::new(),
        regions,
        handlers,
        instrs,
        span_base: e.span_base,
        lines,
        region_rows,
    })
}

/// One function's line rows from the line region: exactly `e.lines` rows
/// in exactly `e.lines_len` bytes.
fn read_lines(bytes: &[u8], head: &Head, e: &Entry) -> Result<Vec<LineEntry>, ParseError> {
    let start = (head.lines_at + u64::from(e.lines_at)) as usize;
    let end = start + e.lines_len as usize;
    // Each row is at least five bytes: a count that claims more rows than
    // that allows is refused before anything is allocated.
    if u64::from(e.lines) * 5 > u64::from(e.lines_len) {
        return Err(ParseError::BadCount {
            at: "lines",
            count: e.lines,
            remaining: e.lines_len as usize,
        });
    }
    let mut r = Varints {
        bytes: take(bytes, start, end - start, "line rows")?,
        pos: 0,
        base: start,
        function: e.index,
    };
    let mut out = Vec::with_capacity(e.lines as usize);
    let (mut pc, mut span_off, mut line_off) = (0u32, 0u32, 0u32);
    for _ in 0..e.lines {
        pc = r.delta(pc)?;
        span_off = r.delta(span_off)?;
        let span_len = r.plain()?;
        line_off = r.delta(line_off)?;
        let col = r.plain()?;
        out.push(LineEntry {
            pc,
            span_off,
            span_len,
            line_off,
            col,
        });
    }
    if r.pos != r.bytes.len() {
        return Err(ParseError::BadOffset {
            at: "line rows length",
            function: e.index,
            found: u64::from(e.lines_len),
            expected: r.pos as u64,
        });
    }
    Ok(out)
}

/// A reader of one function's line rows.
struct Varints<'a> {
    bytes: &'a [u8],
    pos: usize,
    base: usize,
    function: u32,
}

impl Varints<'_> {
    fn bad(&self, at: usize) -> ParseError {
        ParseError::BadVarint {
            function: self.function,
            offset: self.base + at,
        }
    }

    /// A LEB128 number of at most five bytes, in its shortest form.
    fn varint(&mut self) -> Result<u64, ParseError> {
        let start = self.pos;
        let mut v = 0u64;
        for k in 0..5 {
            let Some(b) = self.bytes.get(self.pos).copied() else {
                return Err(ParseError::Truncated {
                    at: "line rows",
                    offset: self.base + self.pos,
                });
            };
            self.pos += 1;
            v |= u64::from(b & 0x7f) << (7 * k);
            if b & 0x80 == 0 {
                if b == 0 && k > 0 {
                    return Err(self.bad(start));
                }
                return Ok(v);
            }
        }
        Err(self.bad(start))
    }

    fn plain(&mut self) -> Result<u32, ParseError> {
        let start = self.pos;
        let v = self.varint()?;
        u32::try_from(v).map_err(|_| self.bad(start))
    }

    fn delta(&mut self, prev: u32) -> Result<u32, ParseError> {
        let start = self.pos;
        let z = self.varint()?;
        let d = (z >> 1) as i64 ^ -((z & 1) as i64);
        u32::try_from(i64::from(prev) + d).map_err(|_| self.bad(start))
    }
}
