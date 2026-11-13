// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Programs: a script appended to the `cig` that runs it.
//!
//! CigCompile's first back end (otmof-ops/cig-compile#2, stage A) makes a
//! program by appending a checked script to the `cig` binary of the same
//! version, built for the machine it is meant for. At start-up `cig` looks
//! at its own end: when a program is there, it runs that program through
//! the same run as `cig run` (the same kernel, journal, plan and rollback)
//! and takes no command.
//!
//! Format 2 (cig-compile's decision 0004) is what [`append2`] writes:
//!
//! ```text
//! ... cig ...        the runtime, as released; its image ends at P
//! record             JSON, `{"format":2,...`, at most 64 KiB
//! zero padding       to the first body's alignment
//! section bodies     in table order, each at a file offset that is a multiple of 16
//! section table      count x 64 bytes
//! "CIGPROG2" ...     the trailer, 56 bytes: payload and record lengths, the
//!                    section count, flags, sha256(record + table)
//! ```
//!
//! A start reads the trailer, the record and the table and checks their
//! sha256: a constant amount, whatever the script's size. A body is read,
//! hashed and decoded only when it is used ([`Loaded::body`]).
//!
//! Format 1, which [`append`] still writes for one release, is one JSON
//! record that holds the script, then a 48-byte trailer:
//!
//! ```text
//! ... cig ...                                  the runtime, as released
//! payload                                      `len` bytes of JSON: the program's record, its source included
//! "CIGPROG1"  len (u64, LE)  sha256(payload)   the trailer, 48 bytes
//! ```
//!
//! [`read`] reads both. A program whose hashes do not match is damaged, and
//! it refuses to run (E1050). The writers put everything in one fixed order
//! and nothing that changes between builds, so the same script, compiler
//! and runtime give the same bytes.
//!
//! The sha256s are integrity, not authenticity: they catch a damaged or
//! truncated program, not an edited one (anyone can recompute them), and
//! the runtime half is not covered. Programs are unsigned until
//! cig-compile#40.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, SeekFrom};

/// The trailer's first eight bytes.
pub const MAGIC: [u8; 8] = *b"CIGPROG1";
/// The trailer's length: magic, payload length, payload sha256.
pub const TRAILER_LEN: u64 = 48;
/// The payload's format.
pub const FORMAT: u32 = 1;
/// The most a payload may hold. A script is text; this is generous.
pub const MAX_PAYLOAD: u64 = 16 * 1024 * 1024;
/// A payload's first bytes: its record's first field (the fields are
/// written in declaration order, `format` first).
const PAYLOAD_START: &[u8] = b"{\"format\":";
/// How far back from the end a trailer is looked for when bytes follow it.
const TAIL: u64 = 1024 * 1024;
/// Set on cig's calls to itself (the ashtray's background send, the box's
/// network forwarder). A binary with a program appended is `cig` for
/// exactly those calls ([`own`]) and runs its program for anything else.
pub const SELF_ENV: &str = "CIG_INTERNAL_SELF";

/// Every cig says its version in its own bytes, where cigcompile reads it
/// without running the binary (a cig built for another machine cannot run
/// here): `\0cig-runtime-version=1.15.0\0`. [`crate::cli::entry`] reads it
/// once, so no linker drops it.
pub static RUNTIME_MARK: &str = concat!("\0cig-runtime-version=", env!("CARGO_PKG_VERSION"), "\0");

/// The version a cig binary's bytes say it is ([`RUNTIME_MARK`]); `None`
/// for a binary without the mark (a cig before 1.15.0, or not a cig).
pub fn runtime_version(bytes: &[u8]) -> Option<String> {
    let needle = b"\0cig-runtime-version=";
    // memchr's SIMD search: an 11 MB runtime is read in about a millisecond.
    for at in memchr::memmem::find_iter(bytes, needle) {
        let start = at + needle.len();
        let rest = &bytes[start..bytes.len().min(start + 32)];
        if let Some(end) = rest.iter().position(|b| *b == 0) {
            let v = &rest[..end];
            let parts: Vec<&[u8]> = v.split(|b| *b == b'.').collect();
            if parts.len() == 3
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.iter().all(u8::is_ascii_digit))
            {
                return Some(String::from_utf8_lossy(v).to_string());
            }
        }
    }
    None
}

static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Note that this process is a compiled program (the front end calls it
/// first). What only `cig` itself should do checks [`running`].
pub fn set_running() {
    RUNNING.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Is this process a compiled program rather than `cig`?
pub fn running() -> bool {
    RUNNING.load(std::sync::atomic::Ordering::Relaxed)
}

/// A compiled program: what it is called, the script it was compiled
/// from, and who compiled it for which cig.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Program {
    /// The payload's format ([`FORMAT`]).
    pub format: u32,
    /// The program's name: its file's stem, or the name it was built as.
    pub name: String,
    /// The script's path as it was compiled. A runtime error points here,
    /// with the same line and column `cig run` gives.
    pub file: String,
    /// The script.
    pub source: String,
    /// sha256 of `source`, hex.
    pub sha256: String,
    /// The cig version the program was compiled for; it runs only on that one.
    pub cig: String,
    /// What compiled it, e.g. `cigcompile 0.1.0`.
    pub compiler: String,
}

impl Program {
    /// A program for this cig, from a script's text.
    pub fn new(name: &str, file: &str, source: &str, compiler: &str) -> Program {
        Program {
            format: FORMAT,
            name: name.to_string(),
            file: file.to_string(),
            source: source.to_string(),
            sha256: hex::encode(Sha256::digest(source.as_bytes())),
            cig: crate::VERSION.to_string(),
            compiler: compiler.to_string(),
        }
    }

    /// The payload: the record as JSON, its fields in declaration order.
    pub fn payload(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a program serialises")
    }

    /// The record without the script, for `--cig-program` and `cig runs`.
    pub fn summary(&self) -> serde_json::Value {
        serde_json::json!({
            "format": self.format,
            "name": self.name,
            "file": self.file,
            "sha256": self.sha256,
            "cig": self.cig,
            "compiler": self.compiler,
        })
    }
}

/// The trailer for a payload.
pub fn trailer(payload: &[u8]) -> [u8; TRAILER_LEN as usize] {
    let mut t = [0u8; TRAILER_LEN as usize];
    t[..8].copy_from_slice(&MAGIC);
    t[8..16].copy_from_slice(&(payload.len() as u64).to_le_bytes());
    t[16..].copy_from_slice(&Sha256::digest(payload));
    t
}

/// A program's executable: `runtime` with `program` appended.
pub fn append(runtime: &[u8], program: &Program) -> Vec<u8> {
    let payload = program.payload();
    let mut out = Vec::with_capacity(runtime.len() + payload.len() + TRAILER_LEN as usize);
    out.extend_from_slice(runtime);
    out.extend_from_slice(&payload);
    out.extend_from_slice(&trailer(&payload));
    out
}

// ----- Format 2: the record, the section table, the bodies -----

/// A format 2 trailer's first eight bytes.
pub const MAGIC2: [u8; 8] = *b"CIGPROG2";
/// A format 2 trailer's length: magic, payload length, record length,
/// section count, flags, head sha256.
pub const TRAILER2_LEN: u64 = 56;
/// The format [`append2`] writes.
pub const FORMAT2: u32 = 2;
/// A section table entry's length.
pub const ENTRY_LEN: u64 = 64;
/// The most a format 2 record may hold.
pub const MAX_RECORD: u64 = 65_536;
/// The most sections a table may list.
pub const MAX_SECTIONS: u64 = 32;
/// The most a format 2 payload may hold, and the most a body may decode to.
pub const MAX_PAYLOAD2: u64 = 64 * 1024 * 1024;
/// Every body starts at a file offset that is a multiple of this.
pub const ALIGN: u64 = 16;
/// A script this long or longer is kept DEFLATE-compressed.
pub const DEFLATE_FROM: usize = 4096;
/// The one DEFLATE level the writer uses, so the same text gives the same bytes.
pub const DEFLATE_LEVEL: u32 = 9;
/// The sections this cig knows, in the order a table lists them; a table
/// lists any other id after these, in byte order.
pub const KNOWN_SECTIONS: [&str; 5] = ["source", "bytecode", "tree", "snap", "sbom"];
/// `CIG_VALIDATE=1`: a start hashes and validates every section before it runs.
pub const VALIDATE_ENV: &str = "CIG_VALIDATE";

/// Which body a format 2 program runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stage {
    /// The `source` section, on the tree-walker.
    #[serde(rename = "a")]
    A,
    /// The `bytecode` section, on the VM.
    #[serde(rename = "b")]
    B,
}

impl Stage {
    /// `a` or `b`, as the record writes it.
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::A => "a",
            Stage::B => "b",
        }
    }

    /// The section a program at this stage cannot run without.
    pub fn needs(self) -> &'static str {
        match self {
            Stage::A => "source",
            Stage::B => "bytecode",
        }
    }
}

/// How a section's body is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    /// As is (`0`).
    Stored,
    /// Raw DEFLATE, RFC 1951 (`1`).
    Deflate,
}

impl Encoding {
    /// The byte the table entry carries.
    pub fn code(self) -> u8 {
        match self {
            Encoding::Stored => 0,
            Encoding::Deflate => 1,
        }
    }

    /// The encoding a table byte names, if this cig reads it.
    pub fn from_code(b: u8) -> Option<Encoding> {
        match b {
            0 => Some(Encoding::Stored),
            1 => Some(Encoding::Deflate),
            _ => None,
        }
    }

    /// `stored` or `deflate`, for `--cig-program`.
    pub fn name(self) -> &'static str {
        match self {
            Encoding::Stored => "stored",
            Encoding::Deflate => "deflate",
        }
    }
}

/// A format 2 program's record: what it is, who made it, for which cig,
/// and how. Its keys are written in this order, without whitespace;
/// readers ignore keys they do not know.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// [`FORMAT2`]; first, so a cut program is still recognized.
    pub format: u32,
    /// The program's name.
    pub name: String,
    /// The script's file name as compiled; errors point here.
    pub file: String,
    /// sha256 of the script's text (decoded), hex: as in format 1.
    pub sha256: String,
    /// The cig version the program runs on.
    pub cig: String,
    /// What compiled it, e.g. `cigcompile 0.4.0`.
    pub compiler: String,
    /// Which body runs.
    pub stage: Stage,
    /// The optimization level it was built at: `0`, `1`, `2`.
    pub opt: String,
    /// The target triple it was built for.
    pub target: String,
    /// sha256 of the bytes before the payload: the runtime as appended to, hex.
    pub runtime_sha256: String,
    /// Every argument is the program's (written only when set).
    #[serde(default, skip_serializing_if = "is_false")]
    pub own_flags: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// One entry of a format 2 section table, as the file holds it. Until the
/// table is checked ([`Loaded::check`]) its fields are only what the bytes
/// say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The id's eight bytes: lower-case ASCII, zero-padded.
    pub raw_id: [u8; 8],
    /// The body's own format: `1` for `source` and `sbom`.
    pub version: u16,
    /// The encoding byte: `0` stored, `1` DEFLATE ([`Entry::encoding`]).
    pub encoding: u8,
    /// Bit 0: required. Other bits are refused.
    pub flags: u8,
    /// The body's length decoded.
    pub decoded_len: u32,
    /// Where the body starts, from the payload's start.
    pub offset: u64,
    /// The body's stored length.
    pub length: u64,
    /// sha256 of the stored bytes.
    pub sha256: [u8; 32],
}

impl Entry {
    fn parse(b: &[u8]) -> Entry {
        let u = |at: usize, n: usize| int(b, at, n, false).unwrap_or(0);
        let mut raw_id = [0u8; 8];
        raw_id.copy_from_slice(&b[..8]);
        let mut sha256 = [0u8; 32];
        sha256.copy_from_slice(&b[32..64]);
        Entry {
            raw_id,
            version: u(8, 2) as u16,
            encoding: b[10],
            flags: b[11],
            decoded_len: u(12, 4) as u32,
            offset: u(16, 8),
            length: u(24, 8),
            sha256,
        }
    }

    fn bytes(&self) -> [u8; ENTRY_LEN as usize] {
        let mut b = [0u8; ENTRY_LEN as usize];
        b[..8].copy_from_slice(&self.raw_id);
        b[8..10].copy_from_slice(&self.version.to_le_bytes());
        b[10] = self.encoding;
        b[11] = self.flags;
        b[12..16].copy_from_slice(&self.decoded_len.to_le_bytes());
        b[16..24].copy_from_slice(&self.offset.to_le_bytes());
        b[24..32].copy_from_slice(&self.length.to_le_bytes());
        b[32..].copy_from_slice(&self.sha256);
        b
    }

    /// The id as text; empty when its bytes are not an id.
    pub fn id(&self) -> &str {
        id_text(&self.raw_id).unwrap_or("")
    }

    /// Bit 0 of the flags: a reader that does not know this id refuses the program.
    pub fn required(&self) -> bool {
        self.flags & 1 != 0
    }

    /// The encoding, when it is one this cig reads.
    pub fn encoding(&self) -> Option<Encoding> {
        Encoding::from_code(self.encoding)
    }

    /// The sha256 of the stored bytes, hex.
    pub fn sha256_hex(&self) -> String {
        hex::encode(self.sha256)
    }
}

/// An id's text: lower-case ASCII letters, digits and `-`, a letter first,
/// then zero bytes to eight.
fn id_text(raw: &[u8; 8]) -> Option<&str> {
    let n = raw.iter().position(|b| *b == 0).unwrap_or(8);
    let (id, pad) = raw.split_at(n);
    let good = !id.is_empty()
        && id[0].is_ascii_lowercase()
        && id
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        && pad.iter().all(|b| *b == 0);
    good.then(|| std::str::from_utf8(id).expect("ascii"))
}

/// Where an id sits in a table: the known sections first, in their order,
/// then any other by its bytes.
fn order_key(raw: &[u8; 8]) -> (usize, [u8; 8]) {
    let id = id_text(raw).unwrap_or("");
    let at = KNOWN_SECTIONS
        .iter()
        .position(|k| *k == id)
        .unwrap_or(KNOWN_SECTIONS.len());
    (at, *raw)
}

/// A section for [`append2`] to write beside `source`: its body as it is
/// used (the writer encodes it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    /// Lower-case ASCII letters, digits and `-`, a letter first, at most 8.
    pub id: String,
    /// The body's own format.
    pub version: u16,
    /// How the writer keeps it.
    pub encoding: Encoding,
    /// A reader that does not know this id refuses the program.
    pub required: bool,
    /// The body, decoded.
    pub bytes: Vec<u8>,
}

/// What [`append2`] makes a format 2 program from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Build {
    /// The program's name ([`good_name`]).
    pub name: String,
    /// The script's file name as compiled.
    pub file: String,
    /// The script.
    pub source: String,
    /// What compiled it.
    pub compiler: String,
    /// The cig it runs on; [`Build::new`] gives this cig's version.
    pub cig: String,
    /// Which body runs.
    pub stage: Stage,
    /// The optimization level: `0`, `1`, `2`.
    pub opt: String,
    /// The target triple.
    pub target: String,
    /// Every argument is the program's.
    pub own_flags: bool,
    /// The sections after `source`, in table order.
    pub sections: Vec<Section>,
}

impl Build {
    /// A stage `a` program for this cig, at `opt` `0`, with no other section.
    pub fn new(name: &str, file: &str, source: &str, compiler: &str, target: &str) -> Build {
        Build {
            name: name.to_string(),
            file: file.to_string(),
            source: source.to_string(),
            compiler: compiler.to_string(),
            cig: crate::VERSION.to_string(),
            stage: Stage::A,
            opt: "0".to_string(),
            target: target.to_string(),
            own_flags: false,
            sections: Vec::new(),
        }
    }
}

/// A body ready to place: its entry (offset not yet set) and its stored bytes.
struct Placed {
    entry: Entry,
    stored: Vec<u8>,
}

fn deflate(bytes: &[u8]) -> Vec<u8> {
    use std::io::Write as _;
    let mut e =
        flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(DEFLATE_LEVEL));
    e.write_all(bytes).expect("writing to memory");
    e.finish().expect("writing to memory")
}

fn place(
    id: &str,
    version: u16,
    encoding: Encoding,
    required: bool,
    body: &[u8],
) -> Result<Placed, String> {
    let mut raw_id = [0u8; 8];
    if id.len() > 8 {
        return Err(format!("section id `{id}` is longer than 8 bytes"));
    }
    raw_id[..id.len()].copy_from_slice(id.as_bytes());
    if id_text(&raw_id).is_none() {
        return Err(format!(
            "section id `{id}` is not lower-case ASCII letters, digits and `-`"
        ));
    }
    if body.len() as u64 > MAX_PAYLOAD2 {
        return Err(format!("section `{id}` is more than 64 MiB"));
    }
    let stored = match encoding {
        Encoding::Stored => body.to_vec(),
        Encoding::Deflate => deflate(body),
    };
    Ok(Placed {
        entry: Entry {
            raw_id,
            version,
            encoding: encoding.code(),
            flags: u8::from(required),
            decoded_len: body.len() as u32,
            offset: 0,
            length: stored.len() as u64,
            sha256: Sha256::digest(&stored).into(),
        },
        stored,
    })
}

/// A format 2 program's executable: `runtime` with the record, the bodies
/// (`source` first, then `b.sections`), the table and the trailer appended.
/// The same inputs give the same bytes. `Err` names what a reader would
/// refuse: a name, id, order or size outside the format's rules.
pub fn append2(runtime: &[u8], b: &Build) -> Result<Vec<u8>, String> {
    let record = Record {
        format: FORMAT2,
        name: b.name.clone(),
        file: b.file.clone(),
        sha256: hex::encode(Sha256::digest(b.source.as_bytes())),
        cig: b.cig.clone(),
        compiler: b.compiler.clone(),
        stage: b.stage,
        opt: b.opt.clone(),
        target: b.target.clone(),
        runtime_sha256: hex::encode(Sha256::digest(runtime)),
        own_flags: b.own_flags,
    };
    check_record(&record)?;
    let source_encoding = if b.source.len() >= DEFLATE_FROM {
        Encoding::Deflate
    } else {
        Encoding::Stored
    };
    let mut bodies = vec![place(
        "source",
        1,
        source_encoding,
        true,
        b.source.as_bytes(),
    )?];
    for s in &b.sections {
        bodies.push(place(&s.id, s.version, s.encoding, s.required, &s.bytes)?);
    }
    for w in bodies.windows(2) {
        if order_key(&w[0].entry.raw_id) >= order_key(&w[1].entry.raw_id) {
            return Err(format!(
                "section `{}` is repeated or out of order after `{}`",
                w[1].entry.id(),
                w[0].entry.id()
            ));
        }
    }
    let record = serde_json::to_vec(&record).expect("a record serialises");
    assemble(runtime, &record, bodies)
}

/// The record, the bodies at their alignment, the table and the trailer,
/// after `runtime`.
fn assemble(runtime: &[u8], record: &[u8], bodies: Vec<Placed>) -> Result<Vec<u8>, String> {
    if record.len() as u64 > MAX_RECORD {
        return Err("the record is more than 64 KiB".to_string());
    }
    if bodies.is_empty() || bodies.len() as u64 > MAX_SECTIONS {
        return Err(format!(
            "a table lists 1 to {MAX_SECTIONS} sections, not {}",
            bodies.len()
        ));
    }
    let p = runtime.len() as u64;
    let mut payload = record.to_vec();
    let mut table = Vec::with_capacity(bodies.len() * ENTRY_LEN as usize);
    for mut b in bodies {
        let pad = (ALIGN - (p + payload.len() as u64) % ALIGN) % ALIGN;
        payload.resize(payload.len() + pad as usize, 0);
        b.entry.offset = payload.len() as u64;
        payload.extend_from_slice(&b.stored);
        table.extend_from_slice(&b.entry.bytes());
    }
    payload.extend_from_slice(&table);
    if payload.len() as u64 > MAX_PAYLOAD2 {
        return Err("the payload is more than 64 MiB".to_string());
    }
    let mut out = Vec::with_capacity(runtime.len() + payload.len() + TRAILER2_LEN as usize);
    out.extend_from_slice(runtime);
    out.extend_from_slice(&payload);
    out.extend_from_slice(&trailer2(
        payload.len() as u64,
        record.len() as u32,
        (table.len() as u64 / ENTRY_LEN) as u16,
        record,
        &table,
    ));
    Ok(out)
}

/// A format 2 trailer.
fn trailer2(
    payload_len: u64,
    record_len: u32,
    count: u16,
    record: &[u8],
    table: &[u8],
) -> [u8; TRAILER2_LEN as usize] {
    let mut t = [0u8; TRAILER2_LEN as usize];
    t[..8].copy_from_slice(&MAGIC2);
    t[8..16].copy_from_slice(&payload_len.to_le_bytes());
    t[16..20].copy_from_slice(&record_len.to_le_bytes());
    t[20..22].copy_from_slice(&count.to_le_bytes());
    t[24..].copy_from_slice(&head_sha256(record, table));
    t
}

fn head_sha256(record: &[u8], table: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(record);
    h.update(table);
    h.finalize().into()
}

/// What a program says about itself, with anything that is not printable
/// shown escaped: a reader's message may quote a record's bytes.
fn shown(s: &str) -> String {
    s.chars()
        .map(|c| {
            if printable(&c.to_string()) {
                c.to_string()
            } else {
                c.escape_unicode().to_string()
            }
        })
        .collect()
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// The record's own rules: format 1's for its words, a sha256 for each
/// hash, and short printable text for `opt` and `target`.
fn check_record(r: &Record) -> Result<(), String> {
    if r.format != FORMAT2 {
        return Err(format!(
            "the record says format {}, and its trailer is format 2",
            r.format
        ));
    }
    check_words(&r.name, &r.file, &r.compiler, &r.cig)?;
    for (what, v) in [("sha256", &r.sha256), ("runtime_sha256", &r.runtime_sha256)] {
        if !is_sha256_hex(v) {
            return Err(format!(
                "the record's `{what}` is not a sha256 (64 lower-case hex digits)"
            ));
        }
    }
    for (what, v, max) in [("opt", &r.opt, 8), ("target", &r.target, 64)] {
        if v.is_empty() || v.chars().count() > max || !printable(v) {
            return Err(format!(
                "the record's `{what}` is not printable text of 1 to {max} characters"
            ));
        }
    }
    Ok(())
}

/// The format 2 program whose trailer `t` is at `at`: its trailer's fields,
/// its record and the head's sha256 checked; its table read, not yet checked.
fn at_trailer2<F: Read + Seek>(f: &mut F, at: u64, t: &[u8]) -> Result<Loaded, String> {
    let len = trailer_len(t);
    let record_len = int(t, 16, 4, false).expect("in the trailer");
    let count = int(t, 20, 2, false).expect("in the trailer");
    let flags = int(t, 22, 2, false).expect("in the trailer");
    if flags != 0 {
        return Err(format!(
            "the trailer's flags are {flags:#06x}, and this cig knows none"
        ));
    }
    if len > MAX_PAYLOAD2 {
        return Err(format!(
            "the trailer says the payload is {len} bytes, more than 64 MiB"
        ));
    }
    if len > at {
        return Err(format!(
            "the trailer says the program is {len} bytes, which this file cannot hold"
        ));
    }
    if record_len == 0 || record_len > MAX_RECORD {
        return Err(format!(
            "the trailer says the record is {record_len} bytes; a record is 1 to 65,536 bytes"
        ));
    }
    if count == 0 || count > MAX_SECTIONS {
        return Err(format!(
            "the trailer says the section table has {count} entries; a table has 1 to {MAX_SECTIONS}"
        ));
    }
    let table_len = count * ENTRY_LEN;
    if record_len + table_len > len {
        return Err(
            "the record and the section table do not fit in the payload the trailer gives"
                .to_string(),
        );
    }
    let start = at - len;
    let record = read_at(f, start, record_len)?;
    let table = read_at(f, at - table_len, table_len)?;
    if head_sha256(&record, &table)[..] != t[24..56] {
        return Err(
            "the record and the section table do not match the sha256 in the trailer".to_string(),
        );
    }
    if !record.starts_with(PAYLOAD_START) {
        return Err("the record does not start with {\"format\":".to_string());
    }
    let r: Record = serde_json::from_slice(&record).map_err(|e| {
        format!(
            "the program's record does not read: {}",
            shown(&e.to_string())
        )
    })?;
    check_record(&r)?;
    Ok(Loaded {
        start,
        inner: Inner::Two(Two {
            record: r,
            entries: table
                .chunks_exact(ENTRY_LEN as usize)
                .map(Entry::parse)
                .collect(),
            record_len,
            table_at: len - table_len,
            checked: false,
        }),
    })
}

/// A program as a start reads it: its trailer, its record and, for format
/// 2, its section table. No body is read until one is used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loaded {
    /// Where the payload starts in its file (P): the runtime's length.
    pub start: u64,
    inner: Inner,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Inner {
    One(Program),
    Two(Two),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Two {
    record: Record,
    entries: Vec<Entry>,
    record_len: u64,
    /// The table's offset from P.
    table_at: u64,
    checked: bool,
}

impl Loaded {
    /// `1` or `2`.
    pub fn format(&self) -> u32 {
        match &self.inner {
            Inner::One(p) => p.format,
            Inner::Two(t) => t.record.format,
        }
    }

    /// A format 1 program's record, its script included.
    pub fn format1(&self) -> Option<&Program> {
        match &self.inner {
            Inner::One(p) => Some(p),
            Inner::Two(_) => None,
        }
    }

    /// A format 2 program's record.
    pub fn record(&self) -> Option<&Record> {
        match &self.inner {
            Inner::One(_) => None,
            Inner::Two(t) => Some(&t.record),
        }
    }

    /// A format 2 program's section table; empty for format 1.
    pub fn table(&self) -> &[Entry] {
        match &self.inner {
            Inner::One(_) => &[],
            Inner::Two(t) => &t.entries,
        }
    }

    /// The table entry for `id`.
    pub fn entry(&self, id: &str) -> Option<&Entry> {
        self.table().iter().find(|e| e.id() == id)
    }

    fn words(&self) -> (&str, &str, &str, &str, &str) {
        match &self.inner {
            Inner::One(p) => (&p.name, &p.file, &p.sha256, &p.cig, &p.compiler),
            Inner::Two(t) => {
                let r = &t.record;
                (&r.name, &r.file, &r.sha256, &r.cig, &r.compiler)
            }
        }
    }

    /// The program's name.
    pub fn name(&self) -> &str {
        self.words().0
    }

    /// The script's file name as compiled.
    pub fn file(&self) -> &str {
        self.words().1
    }

    /// sha256 of the script's text, hex.
    pub fn sha256(&self) -> &str {
        self.words().2
    }

    /// The cig it runs on.
    pub fn cig(&self) -> &str {
        self.words().3
    }

    /// What compiled it.
    pub fn compiler(&self) -> &str {
        self.words().4
    }

    /// Which body runs; format 1 is stage `a`.
    pub fn stage(&self) -> Stage {
        self.record().map_or(Stage::A, |r| r.stage)
    }

    /// Every argument is the program's; never for format 1.
    pub fn own_flags(&self) -> bool {
        self.record().is_some_and(|r| r.own_flags)
    }

    /// The record without the script, for `--cig-program` and `cig runs`:
    /// format 1's six keys, or format 2's record as written.
    pub fn summary(&self) -> serde_json::Value {
        match &self.inner {
            Inner::One(p) => p.summary(),
            Inner::Two(t) => serde_json::to_value(&t.record).expect("a record serialises"),
        }
    }

    /// The table as `--cig-program --json` lists it.
    pub fn sections_json(&self) -> serde_json::Value {
        serde_json::Value::Array(
            self.table()
                .iter()
                .map(|e| {
                    serde_json::json!({
                        "id": e.id(),
                        "version": e.version,
                        "bytes": e.length,
                        "decoded": e.decoded_len,
                        "encoding": e.encoding().map_or("unknown", Encoding::name),
                        "required": e.required(),
                        "sha256": e.sha256_hex(),
                    })
                })
                .collect(),
        )
    }

    /// The table's rules and the stage's sections (a format 1 program was
    /// checked whole when it was read). Each rule refused names itself.
    /// Reads only the zero padding between the record, the bodies and the
    /// table: at most 15 bytes before each body and before the table.
    pub fn check<F: Read + Seek>(&mut self, f: &mut F) -> Result<(), String> {
        let start = self.start;
        let Inner::Two(t) = &mut self.inner else {
            return Ok(());
        };
        if t.checked {
            return Ok(());
        }
        let mut prev_end = t.record_len;
        let mut prev_key: Option<(usize, [u8; 8])> = None;
        for (n, e) in t.entries.iter().enumerate() {
            let Some(id) = id_text(&e.raw_id) else {
                return Err(format!(
                    "section {} of the table has an id that is not lower-case ASCII, zero-padded",
                    n + 1
                ));
            };
            let key = order_key(&e.raw_id);
            if let Some(prev) = prev_key {
                if key == prev {
                    return Err(format!("section `{id}` appears twice in the table"));
                }
                if key < prev {
                    return Err(format!(
                        "section `{id}` is out of order in the table (source, bytecode, tree, snap, sbom, then any other by name)"
                    ));
                }
            }
            prev_key = Some(key);
            let Some(encoding) = e.encoding() else {
                return Err(format!(
                    "section `{id}` has encoding {}; this cig reads 0 (stored) and 1 (DEFLATE)",
                    e.encoding
                ));
            };
            if e.flags & !1 != 0 {
                return Err(format!(
                    "section `{id}` sets flag bits this cig does not know ({:#04x})",
                    e.flags
                ));
            }
            if u64::from(e.decoded_len) > MAX_PAYLOAD2 {
                return Err(format!(
                    "section `{id}` decodes to {} bytes, more than 64 MiB",
                    e.decoded_len
                ));
            }
            if encoding == Encoding::Stored && u64::from(e.decoded_len) != e.length {
                return Err(format!(
                    "section `{id}` is stored as is, and its decoded length ({}) is not its length ({})",
                    e.decoded_len, e.length
                ));
            }
            if e.offset < t.record_len {
                return Err(format!("section `{id}` starts inside the record"));
            }
            if e.offset < prev_end {
                return Err(format!("section `{id}` overlaps the section before it"));
            }
            let end = e
                .offset
                .checked_add(e.length)
                .filter(|end| *end <= t.table_at);
            let Some(end) = end else {
                return Err(format!("section `{id}` runs into the section table"));
            };
            if start.checked_add(e.offset).map(|o| o % ALIGN) != Some(0) {
                return Err(format!(
                    "section `{id}` does not start at a file offset that is a multiple of 16"
                ));
            }
            zero_gap(f, start, prev_end, e.offset, &format!("section `{id}`"))?;
            prev_end = end;
            if id == "source" && e.version != 1 {
                return Err(format!(
                    "section `source` is version {}; this cig reads version 1",
                    e.version
                ));
            }
        }
        zero_gap(f, start, prev_end, t.table_at, "the section table")?;
        for e in &t.entries {
            if e.required() && !KNOWN_SECTIONS.contains(&e.id()) {
                return Err(format!(
                    "the program needs section `{}`, which this cig does not read",
                    e.id()
                ));
            }
        }
        let needs = t.record.stage.needs();
        if !t.entries.iter().any(|e| e.id() == needs) {
            return Err(format!(
                "a stage `{}` program needs section `{needs}`, and this one has none",
                t.record.stage.as_str()
            ));
        }
        t.checked = true;
        Ok(())
    }

    /// The stored bytes of section `id`, checked against the table's sha256.
    pub fn stored<F: Read + Seek>(&self, f: &mut F, id: &str) -> Result<Vec<u8>, String> {
        let Inner::Two(t) = &self.inner else {
            return Err(format!("a format 1 program has no section `{id}`"));
        };
        if !t.checked {
            return Err("the section table has not been checked".to_string());
        }
        let Some(e) = t.entries.iter().find(|e| e.id() == id) else {
            return Err(format!("the program has no section `{id}`"));
        };
        // Checked: the body lies inside the payload, at most 64 MiB.
        let stored = read_at(f, self.start + e.offset, e.length)?;
        if Sha256::digest(&stored)[..] != e.sha256 {
            return Err(format!(
                "section `{id}`'s bytes do not match the sha256 in the section table"
            ));
        }
        Ok(stored)
    }

    /// Section `id`'s body, read, checked against the table's sha256 and
    /// decoded to exactly its decoded length.
    pub fn body<F: Read + Seek>(&self, f: &mut F, id: &str) -> Result<Vec<u8>, String> {
        let stored = self.stored(f, id)?;
        let e = self.entry(id).expect("stored found it");
        match e.encoding() {
            Some(Encoding::Deflate) => inflate(&stored, u64::from(e.decoded_len), id),
            _ => Ok(stored),
        }
    }

    /// The script, the one way to it: format 1's from its record; format
    /// 2's from section `source`, its bytes checked against the table and
    /// its text against the record's `sha256`.
    pub fn source<F: Read + Seek>(&self, f: &mut F) -> Result<String, String> {
        match &self.inner {
            Inner::One(p) => Ok(p.source.clone()),
            Inner::Two(t) => {
                let body = self.body(f, "source")?;
                let text = String::from_utf8(body)
                    .map_err(|_| "section `source` is not UTF-8 text".to_string())?;
                if hex::encode(Sha256::digest(text.as_bytes())) != t.record.sha256 {
                    return Err(
                        "the script in section `source` does not match the sha256 the record gives"
                            .to_string(),
                    );
                }
                Ok(text)
            }
        }
    }

    /// Every section read, hashed and decoded, and the script checked
    /// against the record (`CIG_VALIDATE=1`, and `cigcompile verify`).
    pub fn validate<F: Read + Seek>(&self, f: &mut F) -> Result<(), String> {
        for e in self.table() {
            self.body(f, e.id())?;
        }
        if self.entry("source").is_some() {
            self.source(f)?;
        }
        Ok(())
    }
}

/// The zero padding from `from` to `to` (offsets from `start`): shorter
/// than [`ALIGN`], and all zeros.
fn zero_gap<F: Read + Seek>(
    f: &mut F,
    start: u64,
    from: u64,
    to: u64,
    before: &str,
) -> Result<(), String> {
    let gap = to - from;
    if gap >= ALIGN {
        return Err(format!(
            "{gap} bytes lie before {before}, more than its alignment needs"
        ));
    }
    if gap > 0 && read_at(f, start + from, gap)?.iter().any(|b| *b != 0) {
        return Err(format!("the padding before {before} is not zero"));
    }
    Ok(())
}

/// Raw DEFLATE, refused when it does not inflate to exactly `len` bytes:
/// it never inflates past them.
fn inflate(stored: &[u8], len: u64, id: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    flate2::read::DeflateDecoder::new(stored)
        .take(len + 1)
        .read_to_end(&mut out)
        .map_err(|e| format!("section `{id}` does not inflate: {e}"))?;
    match (out.len() as u64).cmp(&len) {
        std::cmp::Ordering::Equal => Ok(out),
        std::cmp::Ordering::Greater => Err(format!(
            "section `{id}` inflates past the {len} bytes its table entry gives"
        )),
        std::cmp::Ordering::Less => Err(format!(
            "section `{id}` inflates to {} bytes, not the {len} its table entry gives",
            out.len()
        )),
    }
}

/// A program this executable carries, with its file kept open: a body is
/// read from the same file the record came from, and the script once.
#[derive(Debug)]
pub struct Opened {
    program: Loaded,
    file: std::fs::File,
    source: std::cell::OnceCell<Result<String, String>>,
}

impl std::ops::Deref for Opened {
    type Target = Loaded;
    fn deref(&self) -> &Loaded {
        &self.program
    }
}

impl Opened {
    /// The program as read.
    pub fn loaded(&self) -> &Loaded {
        &self.program
    }

    /// The script ([`Loaded::source`]), read and checked the first time
    /// it is asked for.
    pub fn source(&self) -> Result<&str, String> {
        self.source
            .get_or_init(|| self.program.source(&mut &self.file))
            .as_deref()
            .map_err(Clone::clone)
    }

    /// Section `id`'s body ([`Loaded::body`]).
    pub fn body(&self, id: &str) -> Result<Vec<u8>, String> {
        self.program.body(&mut &self.file, id)
    }
}

/// `CIG_VALIDATE=1` is set.
pub fn validate_by_env() -> bool {
    std::env::var_os(VALIDATE_ENV).is_some_and(|v| v == "1")
}

/// Does `bytes` end in a trailer of either format (a program, damaged or not)?
pub fn carries_program(bytes: &[u8]) -> bool {
    let ends_in = |magic: &[u8; 8], len: u64| {
        bytes.len() >= len as usize && bytes[bytes.len() - len as usize..][..8] == *magic
    };
    ends_in(&MAGIC, TRAILER_LEN) || ends_in(&MAGIC2, TRAILER2_LEN)
}

/// The program at the end of `f`, if one is there, with every rule of its
/// format checked. `Ok(None)`: none is; `Err`: one is there and it is
/// damaged. A format 2 program's bodies are not read: [`Loaded::body`]
/// reads one when it is used, and [`Loaded::validate`] reads them all.
///
/// The trailer is `CIGPROG2` (56 bytes) or `CIGPROG1` (48 bytes) at the
/// end. A program's payload starts where its runtime's image ends
/// ([`image_of`]), so a binary built as a program knows it is one even when
/// its trailer is gone (#135): cut short, or with bytes added after it, it
/// is damaged, never `cig`. Two things may follow a trailer and leave the
/// program whole: zero padding, and a PE certificate table (a Windows
/// program signed after it was built).
pub fn read<F: Read + Seek>(f: &mut F) -> Result<Option<Loaded>, String> {
    let Some(mut p) = open(f)? else {
        return Ok(None);
    };
    p.check(f)?;
    Ok(Some(p))
}

/// The first step of a start: find the trailer, read the record (and a
/// format 2 program's table), check the head's sha256. A format 1 program
/// is read and checked whole here, as it always was; a format 2 program's
/// table rules wait for [`Loaded::check`].
fn open<F: Read + Seek>(f: &mut F) -> Result<Option<Loaded>, String> {
    let end = f.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    if end >= TRAILER2_LEN {
        let t = read_at(f, end - TRAILER2_LEN, TRAILER2_LEN)?;
        if t[..8] == MAGIC2 {
            return at_trailer2(f, end - TRAILER2_LEN, &t).map(Some);
        }
    }
    if end >= TRAILER_LEN {
        let t = read_at(f, end - TRAILER_LEN, TRAILER_LEN)?;
        if t[..8] == MAGIC {
            return at_trailer(f, end - TRAILER_LEN, &t).map(Some);
        }
    }
    let Some(image) = image_of(f, end) else {
        return Ok(None);
    };
    let starts_a_payload = image.end < end
        && read_at(
            f,
            image.end,
            (PAYLOAD_START.len() as u64).min(end - image.end),
        )
        .is_ok_and(|b| PAYLOAD_START.starts_with(&b));
    if !starts_a_payload {
        return Ok(None);
    }
    // Signed after it was built: the certificate table follows the trailer.
    if let Some((at, size)) = image.certificates {
        if at.checked_add(size) == Some(end) {
            for (magic, len) in [(MAGIC2, TRAILER2_LEN), (MAGIC, TRAILER_LEN)] {
                let Some(t_at) = at.checked_sub(len).filter(|t| *t >= image.end) else {
                    continue;
                };
                let t = read_at(f, t_at, len)?;
                if t[..8] == magic && trailer_len(&t).checked_add(image.end) == Some(t_at) {
                    return at_either(f, t_at, &t).map(Some);
                }
            }
        }
    }
    // The newest trailer whose payload runs from the image's end to it.
    let from = end.saturating_sub(TAIL).max(image.end);
    let tail = read_at(f, from, end - from)?;
    for q in memchr::memmem::rfind_iter(&tail, &MAGIC[..7]) {
        let len = match tail.get(q + 7) {
            Some(b'1') => TRAILER_LEN,
            Some(b'2') => TRAILER2_LEN,
            _ => continue,
        };
        let Some(t) = tail.get(q..q + len as usize) else {
            continue;
        };
        let at = from + q as u64;
        if trailer_len(t).checked_add(image.end) != Some(at) {
            continue;
        }
        let p = at_either(f, at, t)?;
        let after = &tail[q + len as usize..];
        if after.iter().all(|b| *b == 0) {
            return Ok(Some(p));
        }
        return Err(format!("{} bytes follow its trailer", after.len()));
    }
    Err(
        "its trailer is not at its end: the file was cut short, or something was written after it"
            .to_string(),
    )
}

/// The program whose trailer, of either format, is `t` at `at`.
fn at_either<F: Read + Seek>(f: &mut F, at: u64, t: &[u8]) -> Result<Loaded, String> {
    if t[..8] == MAGIC2 {
        at_trailer2(f, at, t)
    } else {
        at_trailer(f, at, t)
    }
}

/// The payload length a trailer gives.
fn trailer_len(t: &[u8]) -> u64 {
    u64::from_le_bytes(t[8..16].try_into().expect("eight bytes"))
}

/// The format 1 program whose trailer `t` is at `at`, read and checked whole.
fn at_trailer<F: Read + Seek>(f: &mut F, at: u64, t: &[u8]) -> Result<Loaded, String> {
    let len = trailer_len(t);
    if len > MAX_PAYLOAD || len > at {
        return Err(format!(
            "the trailer says the program is {len} bytes, which this file cannot hold"
        ));
    }
    let payload = read_at(f, at - len, len)?;
    if Sha256::digest(&payload)[..] != t[16..] {
        return Err("the program's bytes do not match the sha256 in its trailer".to_string());
    }
    let p: Program = serde_json::from_slice(&payload)
        .map_err(|e| format!("the program's record does not read: {e}"))?;
    if p.format != FORMAT {
        return Err(format!(
            "the program is in format {}; this cig reads format {FORMAT}",
            p.format
        ));
    }
    if hex::encode(Sha256::digest(p.source.as_bytes())) != p.sha256 {
        return Err("the script does not match the sha256 the program records".to_string());
    }
    check_words(&p.name, &p.file, &p.compiler, &p.cig)?;
    Ok(Loaded {
        start: at - len,
        inner: Inner::One(p),
    })
}

/// `len` bytes of `f` from `at`, all of them.
fn read_at<F: Read + Seek>(f: &mut F, at: u64, len: u64) -> Result<Vec<u8>, String> {
    f.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
    let mut b = Vec::new();
    f.by_ref()
        .take(len)
        .read_to_end(&mut b)
        .map_err(|e| e.to_string())?;
    if b.len() as u64 != len {
        return Err("the program ends before its trailer says it does".to_string());
    }
    Ok(b)
}

/// Where an executable's own image ends in its file, and where a PE's
/// certificate table is (it follows the image, so it is not part of it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Image {
    /// The end of the last thing the headers place in the file.
    pub end: u64,
    /// A PE's certificate table: its offset and size.
    pub certificates: Option<(u64, u64)>,
}

/// The most of a header table read to find an image's end.
const HEADERS: u64 = 1024 * 1024;

/// Where the executable in `f` ends, from its headers: ELF's segments,
/// sections and section header table; a Mach-O's segments and code
/// signature, or a universal binary's last slice; a PE's headers, sections
/// and COFF symbols. `None` for a file that is none of these, or whose
/// headers do not read.
pub fn image_of<F: Read + Seek>(f: &mut F, end: u64) -> Option<Image> {
    let head = read_at(f, 0, end.min(64)).ok()?;
    let plain = |e: u64| Image {
        end: e,
        certificates: None,
    };
    match head.get(..4)? {
        [0x7f, b'E', b'L', b'F'] => elf_end(f, &head, end).map(plain),
        [0xcf, 0xfa, 0xed, 0xfe] => macho_end(f, &head, end).map(plain),
        [0xca, 0xfe, 0xba, 0xbe] => fat_end(f, end).map(plain),
        [b'M', b'Z', ..] => pe_image(f, &head, end),
        _ => None,
    }
}

/// An unsigned integer of `n` bytes at `at` in `b`, little or big endian.
fn int(b: &[u8], at: usize, n: usize, big: bool) -> Option<u64> {
    let s = b.get(at..at.checked_add(n)?)?;
    let v = |acc: u64, x: &u8| acc << 8 | u64::from(*x);
    Some(if big {
        s.iter().fold(0, v)
    } else {
        s.iter().rev().fold(0, v)
    })
}

/// `count` entries of `size` bytes at `off`, when they fit the file.
fn table<F: Read + Seek>(f: &mut F, off: u64, size: u64, count: u64, end: u64) -> Option<Vec<u8>> {
    let len = size.checked_mul(count)?;
    if len > HEADERS || off.checked_add(len)? > end {
        return None;
    }
    read_at(f, off, len).ok()
}

fn elf_end<F: Read + Seek>(f: &mut F, h: &[u8], end: u64) -> Option<u64> {
    let wide = *h.get(4)? == 2;
    let big = *h.get(5)? == 2;
    let n = |at, len| int(h, at, len, big);
    let (phoff, phsize, phnum, shoff, shsize, shnum) = if wide {
        (
            n(32, 8)?,
            n(54, 2)?,
            n(56, 2)?,
            n(40, 8)?,
            n(58, 2)?,
            n(60, 2)?,
        )
    } else {
        (
            n(28, 4)?,
            n(42, 2)?,
            n(44, 2)?,
            n(32, 4)?,
            n(46, 2)?,
            n(48, 2)?,
        )
    };
    let mut last = n(if wide { 52 } else { 40 }, 2)?;
    if phnum > 0 {
        if phsize < if wide { 56 } else { 32 } {
            return None;
        }
        let t = table(f, phoff, phsize, phnum, end)?;
        last = last.max(phoff + phsize * phnum);
        for e in t.chunks_exact(phsize as usize) {
            let (off, len) = if wide {
                (int(e, 8, 8, big)?, int(e, 32, 8, big)?)
            } else {
                (int(e, 4, 4, big)?, int(e, 16, 4, big)?)
            };
            last = last.max(off.checked_add(len)?);
        }
    }
    if shnum > 0 {
        if shsize < if wide { 64 } else { 40 } {
            return None;
        }
        let t = table(f, shoff, shsize, shnum, end)?;
        last = last.max(shoff + shsize * shnum);
        for e in t.chunks_exact(shsize as usize) {
            // SHT_NOBITS (.bss and friends) holds nothing in the file.
            if int(e, 4, 4, big)? == 8 {
                continue;
            }
            let (off, len) = if wide {
                (int(e, 24, 8, big)?, int(e, 32, 8, big)?)
            } else {
                (int(e, 16, 4, big)?, int(e, 20, 4, big)?)
            };
            last = last.max(off.checked_add(len)?);
        }
    }
    Some(last)
}

fn macho_end<F: Read + Seek>(f: &mut F, h: &[u8], end: u64) -> Option<u64> {
    let ncmds = int(h, 16, 4, false)?;
    let size = int(h, 20, 4, false)?;
    let cmds = table(f, 32, size, 1, end)?;
    let mut last = 32 + size;
    let mut at = 0usize;
    for _ in 0..ncmds.min(65_536) {
        let cmd = int(&cmds, at, 4, false)?;
        let len = int(&cmds, at + 4, 4, false)? as usize;
        if len < 8 {
            return None;
        }
        match cmd {
            // LC_SEGMENT_64: fileoff, filesize.
            0x19 => {
                last = last.max(int(&cmds, at + 40, 8, false)?.checked_add(int(
                    &cmds,
                    at + 48,
                    8,
                    false,
                )?)?)
            }
            // LC_CODE_SIGNATURE: dataoff, datasize.
            0x1d => last = last.max(int(&cmds, at + 8, 4, false)? + int(&cmds, at + 12, 4, false)?),
            _ => {}
        }
        at = at.checked_add(len)?;
    }
    Some(last)
}

fn fat_end<F: Read + Seek>(f: &mut F, end: u64) -> Option<u64> {
    let h = read_at(f, 0, end.min(8)).ok()?;
    let count = int(&h, 4, 4, true)?;
    if count == 0 || count > 64 {
        return None;
    }
    let t = table(f, 8, 20, count, end)?;
    let mut last = 8 + 20 * count;
    for e in t.chunks_exact(20) {
        last = last.max(int(e, 8, 4, true)? + int(e, 12, 4, true)?);
    }
    Some(last)
}

fn pe_image<F: Read + Seek>(f: &mut F, h: &[u8], end: u64) -> Option<Image> {
    let pe = int(h, 0x3c, 4, false)?;
    let hdr = table(f, pe, 24, 1, end)?;
    if hdr.get(..4)? != b"PE\0\0" {
        return None;
    }
    let sections = int(&hdr, 6, 2, false)?;
    let symbols = int(&hdr, 12, 4, false)?;
    let nsyms = int(&hdr, 16, 4, false)?;
    let opt_size = int(&hdr, 20, 2, false)?;
    let opt = table(f, pe + 24, opt_size, 1, end)?;
    let (dirs, count_at) = match int(&opt, 0, 2, false)? {
        0x10b => (96, 92),
        0x20b => (112, 108),
        _ => return None,
    };
    let mut last = int(&opt, 60, 4, false)?;
    let t = table(f, pe + 24 + opt_size, 40, sections, end)?;
    for e in t.chunks_exact(40) {
        last = last.max(int(e, 20, 4, false)? + int(e, 16, 4, false)?);
    }
    if symbols != 0 {
        let strings_at = symbols.checked_add(nsyms.checked_mul(18)?)?;
        let strings = int(&read_at(f, strings_at, 4).ok()?, 0, 4, false)?;
        last = last.max(strings_at + strings);
    }
    let certificates = (int(&opt, count_at, 4, false)? > 4)
        .then(|| {
            Some((
                int(&opt, dirs + 32, 4, false)?,
                int(&opt, dirs + 36, 4, false)?,
            ))
        })
        .flatten()
        .filter(|(at, size)| *at != 0 && *size != 0);
    Some(Image {
        end: last,
        certificates,
    })
}

/// What a program says about itself is printed and recorded: its name is
/// a plain word, and nothing it carries holds a control character or an
/// invisible one ([`printable`]): no escape sequence reaches a terminal
/// from a payload, and no name reads backwards (#137).
fn check_words(name: &str, file: &str, compiler: &str, cig: &str) -> Result<(), String> {
    if !good_name(name) {
        return Err(
            "the program's name is not a plain word (letters, digits, `.`, `_`, `-`)".to_string(),
        );
    }
    for (what, v, max) in [
        ("file", file, 4096),
        ("compiler", compiler, 128),
        ("cig", cig, 32),
    ] {
        if v.is_empty() || v.chars().count() > max || !printable(v) {
            return Err(format!("the program's `{what}` is not printable text"));
        }
    }
    Ok(())
}

/// Text that shows as what it says: no control characters (C0, C1), and
/// none of Unicode's invisible ones: the format characters (general
/// category Cf), the bidi controls among them (U+202E shows `tidy\u{202e}gpj.cig`
/// as `tidygic.jpg`), and the line and paragraph separators.
pub fn printable(s: &str) -> bool {
    !s.chars().any(|c| c.is_control() || invisible(c))
}

/// Unicode's format characters (Cf, Unicode 15.1) and its two separators.
fn invisible(c: char) -> bool {
    matches!(c as u32,
        0x00AD | 0x0600..=0x0605 | 0x061C | 0x06DD | 0x070F | 0x0890..=0x0891 | 0x08E2
        | 0x180E | 0x200B..=0x200F | 0x2028..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F
        | 0xFEFF | 0xFFF9..=0xFFFB | 0x110BD | 0x110CD | 0x13430..=0x1343F
        | 0x1BCA0..=0x1BCA3 | 0x1D173..=0x1D17A | 0xE0001 | 0xE0020..=0xE007F)
}

/// A program's name: a letter or digit, then letters, digits, `.`, `_` or
/// `-`, 64 at most.
pub fn good_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

/// Why a binary that carries a program will not run it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unrunnable {
    /// A trailer is there and what it points at is damaged (E1050).
    Damaged(String),
    /// The program was compiled for another cig (E1051).
    OtherCig {
        /// The program's name.
        name: String,
        /// The cig it was compiled for.
        cig: String,
    },
    /// The system started it with more rights than its caller's: setuid,
    /// setgid or file capabilities (E1054, #134).
    Raised {
        /// The program's name.
        name: String,
    },
    /// It cannot read its own file, so it cannot tell whether it is a
    /// program, and it is not named as `cig` is (E1055, #136).
    Unreadable {
        /// Its file's name.
        name: String,
    },
}

/// The program appended to this executable. `None`: no program, act as
/// `cig`; and, for a binary that carries one, cig's own internal calls
/// ([`SELF_ENV`] with exactly their command line), so a program's binary is
/// never a general `cig`. The marker is taken out of the environment either
/// way, so nothing this process starts inherits it. A program refuses to
/// run when it was compiled for another cig, when the system started it
/// with more rights than its caller's (#134), and when it cannot read
/// itself unless it is named as `cig` is (#136).
///
/// The order is the format's start: the trailer, the record and (format 2)
/// the table are read and the head's sha256 checked; then raised rights,
/// cig's internal calls and another cig are decided; then the table's rules
/// and the stage's sections. No body is read here, unless `CIG_VALIDATE=1`
/// asks for every one to be checked first.
pub fn own() -> Option<Result<Opened, Unrunnable>> {
    let marked = std::env::var_os(SELF_ENV).is_some();
    if marked {
        // Before any thread starts: nothing else reads the environment yet.
        std::env::remove_var(SELF_ENV);
    }
    let Some(mut f) = open_self() else {
        let name = self_name();
        return (!named_as_cig(&name)).then_some(Err(Unrunnable::Unreadable { name }));
    };
    let mut p = match open(&mut f) {
        Ok(None) => return None,
        Ok(Some(p)) => p,
        Err(e) => return Some(Err(Unrunnable::Damaged(e))),
    };
    // A setuid program would run its caller's PATH, and keep its runs and
    // keys where its caller's environment says, with the rights of the
    // file's owner: it stops before it reads any of that.
    if privileged() {
        return Some(Err(Unrunnable::Raised {
            name: p.name().to_string(),
        }));
    }
    if marked {
        let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
        if internal_call(&args) {
            return None;
        }
    }
    if p.cig() != crate::VERSION {
        return Some(Err(Unrunnable::OtherCig {
            name: p.name().to_string(),
            cig: p.cig().to_string(),
        }));
    }
    if let Err(e) = p.check(&mut f) {
        return Some(Err(Unrunnable::Damaged(e)));
    }
    if validate_by_env() {
        if let Err(e) = p.validate(&mut f) {
            return Some(Err(Unrunnable::Damaged(e)));
        }
    }
    Some(Ok(Opened {
        program: p,
        file: f,
        source: std::cell::OnceCell::new(),
    }))
}

/// The internal calls a program's binary answers as `cig`: the ashtray's
/// background send and the box's network forwarder, exactly as cig makes
/// them.
fn internal_call(args: &[std::ffi::OsString]) -> bool {
    let words: Vec<&str> = args.iter().filter_map(|a| a.to_str()).collect();
    if words.len() != args.len() {
        return false;
    }
    match words.as_slice() {
        ["crash", "flush", "--quiet"] => true,
        // The box, and so its forwarder, is Linux's.
        #[cfg(target_os = "linux")]
        ["internal-forward", sock, port] => {
            std::path::Path::new(sock).is_absolute() && port.parse::<u16>().is_ok()
        }
        _ => false,
    }
}

/// A process the system started with more than its caller's rights
/// (setuid, setgid, file capabilities): a program refuses to run in one.
#[cfg(target_os = "linux")]
fn privileged() -> bool {
    // SAFETY: getauxval reads the process's auxiliary vector; no pointers.
    unsafe { libc::getauxval(libc::AT_SECURE) != 0 }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn privileged() -> bool {
    // SAFETY: plain id queries.
    unsafe { libc::getuid() != libc::geteuid() || libc::getgid() != libc::getegid() }
}

#[cfg(not(unix))]
fn privileged() -> bool {
    false
}

/// This executable's file name, without `.exe`.
fn self_name() -> String {
    std::env::current_exe()
        .ok()
        .or_else(|| std::env::args_os().next().map(std::path::PathBuf::from))
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().to_string()))
        .unwrap_or_else(|| "this program".to_string())
}

/// The names `cig` itself goes by: `cig`, cigcompile's `cig-runtime`, and
/// a release asset as downloaded (`cig-v1.15.1-linux-x86_64`).
fn named_as_cig(name: &str) -> bool {
    matches!(name, "cig" | "cig-runtime") || name.starts_with("cig-v")
}

/// This executable, as it was started: on Linux the running image itself
/// (`/proc/self/exe` opens it even after its path was replaced or removed).
fn open_self() -> Option<std::fs::File> {
    #[cfg(target_os = "linux")]
    if let Ok(f) = std::fs::File::open("/proc/self/exe") {
        return Some(f);
    }
    std::fs::File::open(std::env::current_exe().ok()?).ok()
}

/// A command for cig to call itself with: this executable (on Linux the
/// running image, whatever its path holds now), marked ([`SELF_ENV`]) so a
/// program's binary answers it as `cig`.
pub fn self_command() -> std::io::Result<std::process::Command> {
    #[cfg(target_os = "linux")]
    let me = if std::path::Path::new("/proc/self/exe").exists() {
        std::path::PathBuf::from("/proc/self/exe")
    } else {
        std::env::current_exe()?
    };
    #[cfg(not(target_os = "linux"))]
    let me = std::env::current_exe()?;
    let mut c = std::process::Command::new(me);
    c.env(SELF_ENV, "1");
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn sample() -> Program {
        Program::new("hello", "examples/hello.cig", "exhale \"hi\"\n", "test 0")
    }

    #[test]
    fn a_program_reads_back_from_the_end_of_its_runtime() {
        let bytes = append(b"\x7fELF runtime bytes", &sample());
        assert!(carries_program(&bytes));
        let back = read(&mut Cursor::new(&bytes)).unwrap().unwrap();
        assert_eq!(back.format(), 1);
        let back = back.format1().unwrap().clone();
        assert_eq!(back, sample());
        assert_eq!(back.cig, crate::VERSION);
    }

    #[test]
    fn a_runtime_says_its_version_in_its_bytes() {
        let mut bytes = b"junk \0cig-runtime-version=\x01 more".to_vec();
        bytes.extend_from_slice(RUNTIME_MARK.as_bytes());
        bytes.extend_from_slice(b"tail");
        assert_eq!(runtime_version(&bytes).as_deref(), Some(crate::VERSION));
        assert_eq!(runtime_version(b"no mark here"), None);
        assert_eq!(runtime_version(b"\0cig-runtime-version=1.2\0"), None);
    }

    #[test]
    fn a_runtime_without_a_program_is_none() {
        assert!(read(&mut Cursor::new(b"just cig".to_vec()))
            .unwrap()
            .is_none());
        assert!(read(&mut Cursor::new(Vec::new())).unwrap().is_none());
        assert!(!carries_program(b"just cig"));
    }

    #[test]
    fn the_same_inputs_give_the_same_bytes() {
        assert_eq!(append(b"rt", &sample()), append(b"rt", &sample()));
        for b in [build2(), big2(), with_extra(false)] {
            let one = append2(&tiny_elf(), &b).unwrap();
            assert_eq!(one, append2(&tiny_elf(), &b).unwrap());
            // And a rebuild from what was read gives them again.
            let l = read2(&one).unwrap();
            let mut again = Build::new(
                l.name(),
                l.file(),
                &l.source(&mut Cursor::new(&one)).unwrap(),
                l.compiler(),
                &l.record().unwrap().target,
            );
            again.sections = b.sections.clone();
            assert_eq!(append2(&tiny_elf(), &again).unwrap(), one);
        }
    }

    #[test]
    fn a_damaged_payload_is_refused() {
        let mut bytes = append(b"runtime", &sample());
        let at = b"runtime".len() + 5;
        bytes[at] ^= 1;
        let e = read(&mut Cursor::new(&bytes)).unwrap_err();
        assert!(e.contains("sha256"), "{e}");
    }

    #[test]
    fn a_length_past_the_start_is_refused() {
        let mut bytes = append(b"rt", &sample());
        let n = bytes.len();
        bytes[n - 40..n - 32].copy_from_slice(&(u64::MAX).to_le_bytes());
        assert!(read(&mut Cursor::new(&bytes))
            .unwrap_err()
            .contains("cannot hold"));
    }

    #[test]
    fn an_edited_script_with_a_recomputed_trailer_still_fails_its_own_sha256() {
        let mut p = sample();
        p.source = "exhale \"changed\"\n".to_string();
        let payload = serde_json::to_vec(&p).unwrap();
        let mut bytes = b"rt".to_vec();
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(&trailer(&payload));
        let e = read(&mut Cursor::new(&bytes)).unwrap_err();
        assert!(
            e.contains("does not match the sha256 the program records"),
            "{e}"
        );
    }

    #[test]
    fn a_hostile_trailer_is_refused_without_reading_past_the_file() {
        let good = append(b"rt", &sample());
        for len in [MAX_PAYLOAD + 1, good.len() as u64, u64::MAX] {
            let mut b = good.clone();
            let n = b.len();
            b[n - 40..n - 32].copy_from_slice(&len.to_le_bytes());
            assert!(read(&mut Cursor::new(&b)).is_err(), "len {len}");
        }
        // The magic alone, in a file too short to hold a trailer, and a zero length.
        assert!(read(&mut Cursor::new(MAGIC.to_vec())).unwrap().is_none());
        let mut zero = b"rt".to_vec();
        zero.extend_from_slice(&trailer(b""));
        assert!(read(&mut Cursor::new(&zero)).is_err());
        // A payload truncated before its trailer.
        let mut cut = good.clone();
        cut.drain(2..10);
        assert!(read(&mut Cursor::new(&cut)).is_err());
    }

    #[test]
    fn deep_nesting_is_refused_not_overflowed() {
        let payload = "[".repeat(1_000_000).into_bytes();
        let mut b = b"rt".to_vec();
        b.extend_from_slice(&payload);
        b.extend_from_slice(&trailer(&payload));
        assert!(read(&mut Cursor::new(&b)).is_err());
    }

    #[test]
    fn a_name_or_file_with_control_characters_is_refused() {
        for bad in [
            "he\x1b[2Jllo",
            "",
            ".hidden",
            "a/b",
            "x".repeat(65).as_str(),
        ] {
            let mut p = sample();
            p.name = bad.to_string();
            let e = read(&mut Cursor::new(&append(b"rt", &p))).unwrap_err();
            assert!(e.contains("plain word"), "{bad:?}: {e}");
        }
        let mut p = sample();
        p.file = "a\nb.cig".to_string();
        assert!(read(&mut Cursor::new(&append(b"rt", &p)))
            .unwrap_err()
            .contains("`file`"));
    }

    #[test]
    fn only_cigs_own_internal_calls_count() {
        let w = |a: &[&str]| a.iter().map(std::ffi::OsString::from).collect::<Vec<_>>();
        assert!(internal_call(&w(&["crash", "flush", "--quiet"])));
        assert_eq!(
            internal_call(&w(&["internal-forward", "/run/x.sock", "3128"])),
            cfg!(target_os = "linux"),
            "the forwarder is the box's, and the box is Linux's"
        );
        assert!(!internal_call(&w(&["internal-forward", "x.sock", "3128"])));
        assert!(!internal_call(&w(&["internal-forward", "/x", "port"])));
        assert!(!internal_call(&w(&["run", "evil.cig"])));
        assert!(!internal_call(&w(&["update"])));
        assert!(!internal_call(&w(&["crash", "flush"])));
        assert!(!internal_call(&w(&[])));
    }

    /// The smallest ELF header: no segments, no sections; its image is the
    /// header's 64 bytes.
    fn tiny_elf() -> Vec<u8> {
        let mut h = vec![0u8; 64];
        h[..6].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1]);
        h[52..54].copy_from_slice(&64u16.to_le_bytes());
        h
    }

    /// A PE32+ with one section of 512 bytes: its image ends at 0x400. The
    /// security directory is patched in by [`sign`].
    fn tiny_pe() -> Vec<u8> {
        let mut b = vec![0u8; 0x400];
        b[..2].copy_from_slice(b"MZ");
        b[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        b[0x40..0x44].copy_from_slice(b"PE\0\0");
        let coff = 0x44;
        b[coff..coff + 2].copy_from_slice(&0x8664u16.to_le_bytes());
        b[coff + 2..coff + 4].copy_from_slice(&1u16.to_le_bytes());
        b[coff + 16..coff + 18].copy_from_slice(&240u16.to_le_bytes());
        let opt = coff + 20;
        b[opt..opt + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        b[opt + 60..opt + 64].copy_from_slice(&0x200u32.to_le_bytes());
        b[opt + 108..opt + 112].copy_from_slice(&16u32.to_le_bytes());
        let sec = opt + 240;
        b[sec..sec + 5].copy_from_slice(b".text");
        b[sec + 16..sec + 20].copy_from_slice(&0x200u32.to_le_bytes());
        b[sec + 20..sec + 24].copy_from_slice(&0x200u32.to_le_bytes());
        b
    }

    /// What Authenticode does to a PE: a certificate table at its end, and
    /// the security directory pointing at it.
    fn sign(mut b: Vec<u8>) -> Vec<u8> {
        let at = b.len() as u32;
        let cert = [
            16u32.to_le_bytes(),
            [0x00, 0x02, 0x02, 0x00],
            [7; 4],
            [9; 4],
        ]
        .concat();
        let dir = 0x44 + 20 + 112 + 32;
        b[dir..dir + 4].copy_from_slice(&at.to_le_bytes());
        b[dir + 4..dir + 8].copy_from_slice(&(cert.len() as u32).to_le_bytes());
        b.extend_from_slice(&cert);
        b
    }

    fn read_bytes(b: &[u8]) -> Result<Option<Program>, String> {
        read(&mut Cursor::new(b)).map(|o| o.map(|l| l.format1().expect("format 1").clone()))
    }

    #[test]
    fn this_binarys_image_ends_where_its_file_does() {
        let me = std::env::current_exe().unwrap();
        let len = std::fs::metadata(&me).unwrap().len();
        let mut f = std::fs::File::open(&me).unwrap();
        let image = image_of(&mut f, len).expect("this test binary's headers read");
        assert_eq!(image.end, len, "{}", me.display());
        assert_eq!(image_of(&mut Cursor::new(tiny_elf()), 64).unwrap().end, 64);
        assert_eq!(
            image_of(&mut Cursor::new(tiny_pe()), 0x400).unwrap().end,
            0x400
        );
        assert_eq!(
            image_of(&mut Cursor::new(b"#!/bin/sh\n".to_vec()), 10),
            None
        );
    }

    #[test]
    fn a_program_cut_short_is_damaged_never_cig() {
        let whole = append(&tiny_elf(), &sample());
        assert_eq!(read_bytes(&whole).unwrap(), Some(sample()));
        for cut in [1, 8, 30, 47, 48, 49, 100, whole.len() - 70] {
            let e = read_bytes(&whole[..whole.len() - cut]).unwrap_err();
            assert!(e.contains("not at its end"), "{cut} cut: {e}");
        }
        // The runtime alone is cig, and so is one with something else after
        // its image.
        assert_eq!(read_bytes(&tiny_elf()).unwrap(), None);
        let mut other = tiny_elf();
        other.extend_from_slice(b"an overlay that is not a program");
        assert_eq!(read_bytes(&other).unwrap(), None);
    }

    #[test]
    fn bytes_after_the_trailer_are_damage_and_zeros_are_not() {
        let whole = append(&tiny_elf(), &sample());
        let mut junk = whole.clone();
        junk.extend_from_slice(b"XX");
        let e = read_bytes(&junk).unwrap_err();
        assert!(e.contains("2 bytes follow its trailer"), "{e}");
        let mut padded = whole.clone();
        padded.extend_from_slice(&[0; 4096]);
        assert_eq!(read_bytes(&padded).unwrap(), Some(sample()));
        // A script that names the magic does not fool the search.
        let mut p = sample();
        p.source = "exhale \"CIGPROG1 is the trailer\"\n".to_string();
        p.sha256 = hex::encode(Sha256::digest(p.source.as_bytes()));
        let named = append(&tiny_elf(), &p);
        let e = read_bytes(&named[..named.len() - 48]).unwrap_err();
        assert!(e.contains("not at its end"), "{e}");
        let mut damaged = append(&tiny_elf(), &sample());
        damaged[100] ^= 1;
        damaged.extend_from_slice(b"XX");
        assert!(read_bytes(&damaged).unwrap_err().contains("sha256"));
    }

    #[test]
    fn a_windows_program_signed_after_it_was_built_still_reads() {
        let program = append(&tiny_pe(), &sample());
        assert_eq!(read_bytes(&sign(program.clone())).unwrap(), Some(sample()));
        // The same bytes after it, with no security directory naming them,
        // are damage.
        let mut unsigned = program;
        unsigned.extend_from_slice(&sign(tiny_pe())[0x400..]);
        assert!(read_bytes(&unsigned)
            .unwrap_err()
            .contains("16 bytes follow its trailer"));
        // A signed cig is cig.
        assert_eq!(read_bytes(&sign(tiny_pe())).unwrap(), None);
    }

    #[test]
    fn invisible_characters_in_what_a_program_says_are_damage() {
        for c in [
            '\u{202e}',
            '\u{202a}',
            '\u{2066}',
            '\u{2069}',
            '\u{200e}',
            '\u{200b}',
            '\u{061c}',
            '\u{feff}',
            '\u{2028}',
            '\u{2029}',
            '\u{00ad}',
            '\u{e0041}',
        ] {
            for field in ["file", "compiler", "cig"] {
                let mut p = sample();
                let v = format!("tidy{c}gpj.cig");
                match field {
                    "file" => p.file = v,
                    "compiler" => p.compiler = v,
                    _ => p.cig = v,
                }
                let e = read_bytes(&append(b"rt", &p)).unwrap_err();
                assert!(e.contains(&format!("`{field}`")), "{c:?} in {field}: {e}");
            }
        }
        for good in [
            "tidy.cig",
            "naïve/tidy.cig",
            "日本語.cig",
            "C:\\tools\\tidy.cig",
        ] {
            assert!(printable(good), "{good}");
        }
    }

    #[test]
    fn only_cigs_own_names_run_as_cig_when_unreadable() {
        for name in ["cig", "cig-runtime", "cig-v1.15.1-linux-x86_64"] {
            assert!(named_as_cig(name), "{name}");
        }
        for name in ["tidy", "cigar", "cigo", "Cig", "this program"] {
            assert!(!named_as_cig(name), "{name}");
        }
    }

    #[test]
    fn another_format_is_refused() {
        let mut p = sample();
        p.format = 9;
        let bytes = append(b"rt", &p);
        assert!(read(&mut Cursor::new(&bytes))
            .unwrap_err()
            .contains("format 9"));
    }

    // ----- Format 2 -----

    const TARGET: &str = "x86_64-unknown-linux-musl";

    fn build2() -> Build {
        Build::new(
            "hello",
            "examples/hello.cig",
            "exhale \"hi\"\n",
            "test 0",
            TARGET,
        )
    }

    /// A script long enough to be kept DEFLATE-compressed.
    fn big2() -> Build {
        let source: String = (0..400).map(|n| format!("exhale \"line {n}\"\n")).collect();
        assert!(source.len() >= DEFLATE_FROM);
        Build::new("big", "big.cig", &source, "test 0", TARGET)
    }

    /// `build2` with a test-only section after `source`.
    fn with_extra(required: bool) -> Build {
        let mut b = build2();
        b.sections.push(Section {
            id: "zz-test".to_string(),
            version: 1,
            encoding: Encoding::Stored,
            required,
            bytes: b"a test-only section's body, 41 bytes long".to_vec(),
        });
        b
    }

    fn make2(b: &Build) -> Vec<u8> {
        append2(&tiny_elf(), b).unwrap()
    }

    fn read2(b: &[u8]) -> Result<Loaded, String> {
        read(&mut Cursor::new(b)).and_then(|o| o.ok_or_else(|| "no program".to_string()))
    }

    fn refused(b: &[u8], sentence: &str) {
        let e = read2(b).unwrap_err();
        assert!(e.contains(sentence), "wanted {sentence:?}, got {e:?}");
    }

    /// Where a format 2 program's parts are: the trailer, the payload's
    /// start, the record's length and the table.
    struct Parts {
        t: usize,
        p: usize,
        record_len: usize,
        table: usize,
    }

    fn parts(b: &[u8]) -> Parts {
        let t = b.len() - TRAILER2_LEN as usize;
        let len = u64::from_le_bytes(b[t + 8..t + 16].try_into().unwrap()) as usize;
        let record_len = u32::from_le_bytes(b[t + 16..t + 20].try_into().unwrap()) as usize;
        let count = u16::from_le_bytes(b[t + 20..t + 22].try_into().unwrap()) as usize;
        Parts {
            t,
            p: t - len,
            record_len,
            table: t - count * ENTRY_LEN as usize,
        }
    }

    /// The head sha256 recomputed after an edit to the record or the table.
    fn reseal(b: &mut [u8]) {
        let x = parts(b);
        let head = head_sha256(&b[x.p..x.p + x.record_len], &b[x.table..x.t]);
        b[x.t + 24..x.t + 56].copy_from_slice(&head);
    }

    /// Byte `at` of table entry `n`.
    fn entry_byte(b: &[u8], n: usize, at: usize) -> usize {
        parts(b).table + n * ENTRY_LEN as usize + at
    }

    fn set_u64(b: &mut [u8], at: usize, v: u64) {
        b[at..at + 8].copy_from_slice(&v.to_le_bytes());
    }

    fn get_u64(b: &[u8], at: usize) -> u64 {
        u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
    }

    fn set_u32(b: &mut [u8], at: usize, v: u32) {
        b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }

    /// The record's text replaced by `edit` (same length), resealed.
    fn edit_record(b: &mut [u8], from: &str, to: &str) {
        assert_eq!(from.len(), to.len());
        let x = parts(b);
        let rec = String::from_utf8(b[x.p..x.p + x.record_len].to_vec()).unwrap();
        assert!(rec.contains(from), "{rec}");
        let rec = rec.replacen(from, to, 1);
        b[x.p..x.p + x.record_len].copy_from_slice(rec.as_bytes());
        reseal(b);
    }

    #[test]
    fn a_format_2_program_reads_back_with_its_record_and_table() {
        let bytes = make2(&with_extra(false));
        assert!(carries_program(&bytes));
        let l = read2(&bytes).unwrap();
        assert_eq!(l.format(), 2);
        assert_eq!(l.start, tiny_elf().len() as u64);
        assert_eq!(
            (l.name(), l.file(), l.cig(), l.compiler(), l.stage()),
            (
                "hello",
                "examples/hello.cig",
                crate::VERSION,
                "test 0",
                Stage::A
            )
        );
        let r = l.record().unwrap();
        assert_eq!(r.target, TARGET);
        assert_eq!(r.opt, "0");
        assert_eq!(r.runtime_sha256, hex::encode(Sha256::digest(tiny_elf())));
        let ids: Vec<&str> = l.table().iter().map(Entry::id).collect();
        assert_eq!(ids, ["source", "zz-test"]);
        assert!(l.entry("source").unwrap().required());
        assert!(!l.entry("zz-test").unwrap().required());
        for e in l.table() {
            assert_eq!((l.start + e.offset) % ALIGN, 0, "{}", e.id());
        }
        assert_eq!(
            l.source(&mut Cursor::new(&bytes)).unwrap(),
            "exhale \"hi\"\n"
        );
        assert_eq!(
            l.body(&mut Cursor::new(&bytes), "zz-test").unwrap(),
            b"a test-only section's body, 41 bytes long"
        );
        // The record: keys in order, no whitespace, no `own_flags` unless set.
        let x = parts(&bytes);
        let rec = std::str::from_utf8(&bytes[x.p..x.p + x.record_len]).unwrap();
        assert!(
            rec.starts_with("{\"format\":2,\"name\":\"hello\",\"file\":"),
            "{rec}"
        );
        assert!(
            rec.contains("\"stage\":\"a\",\"opt\":\"0\",\"target\":"),
            "{rec}"
        );
        assert!(
            !rec.contains("\": ") && !rec.contains(", ") && !rec.contains("own_flags"),
            "{rec}"
        );
        let mut own = build2();
        own.own_flags = true;
        let l = read2(&make2(&own)).unwrap();
        assert!(l.own_flags());
        assert_eq!(l.summary()["own_flags"], true);
    }

    #[test]
    fn a_long_script_is_kept_deflated_and_a_short_one_as_is() {
        let big = make2(&big2());
        let l = read2(&big).unwrap();
        let e = l.entry("source").unwrap();
        assert_eq!(e.encoding(), Some(Encoding::Deflate));
        assert_eq!(e.decoded_len as usize, big2().source.len());
        assert!(
            e.length < e.decoded_len as u64 / 3,
            "{} of {}",
            e.length,
            e.decoded_len
        );
        assert_eq!(l.source(&mut Cursor::new(&big)).unwrap(), big2().source);
        let small = make2(&build2());
        let e = read2(&small).unwrap().entry("source").unwrap().clone();
        assert_eq!(e.encoding(), Some(Encoding::Stored));
        assert_eq!(e.length, 12);
    }

    #[test]
    fn the_writer_refuses_what_a_reader_would() {
        let mut b = with_extra(false);
        b.sections[0].id = "Upper".to_string();
        assert!(append2(b"rt", &b).unwrap_err().contains("lower-case"));
        b.sections[0].id = "toolongid".to_string();
        assert!(append2(b"rt", &b).unwrap_err().contains("longer than 8"));
        b.sections[0].id = "source".to_string();
        assert!(append2(b"rt", &b).unwrap_err().contains("out of order"));
        let mut b = build2();
        for id in ["sbom", "bytecode"] {
            b.sections.push(Section {
                id: id.to_string(),
                version: 1,
                encoding: Encoding::Stored,
                required: false,
                bytes: Vec::new(),
            });
        }
        assert!(append2(b"rt", &b).unwrap_err().contains("out of order"));
        let mut b = build2();
        b.name = ".hidden".to_string();
        assert!(append2(b"rt", &b).unwrap_err().contains("plain word"));
        let mut b = build2();
        b.target = "x".repeat(65);
        assert!(append2(b"rt", &b).unwrap_err().contains("`target`"));
    }

    // One case per rule of the format's section 1, each with its sentence.

    #[test]
    fn rule_trailer_flags() {
        let mut b = make2(&build2());
        let t = parts(&b).t;
        b[t + 22] = 1;
        refused(&b, "the trailer's flags are 0x0001");
    }

    #[test]
    fn rule_payload_length() {
        let good = make2(&build2());
        let t = parts(&good).t;
        let mut b = good.clone();
        set_u64(&mut b, t + 8, MAX_PAYLOAD2 + 1);
        refused(&b, "more than 64 MiB");
        let mut b = good.clone();
        set_u64(&mut b, t + 8, t as u64 + 1);
        refused(&b, "which this file cannot hold");
        let mut b = good;
        set_u64(&mut b, t + 8, u64::MAX);
        refused(&b, "more than 64 MiB");
    }

    #[test]
    fn rule_record_length() {
        let good = make2(&build2());
        let t = parts(&good).t;
        for n in [0, 65_537, u32::MAX] {
            let mut b = good.clone();
            set_u32(&mut b, t + 16, n);
            refused(&b, "a record is 1 to 65,536 bytes");
        }
        let mut b = good;
        let len = get_u64(&b, t + 8);
        set_u32(&mut b, t + 16, len as u32);
        refused(&b, "do not fit in the payload");
    }

    #[test]
    fn rule_section_count() {
        let good = make2(&build2());
        let t = parts(&good).t;
        for n in [0u16, 33, u16::MAX] {
            let mut b = good.clone();
            b[t + 20..t + 22].copy_from_slice(&n.to_le_bytes());
            refused(&b, "a table has 1 to 32");
        }
    }

    #[test]
    fn rule_head_sha256() {
        let good = make2(&with_extra(false));
        let x = parts(&good);
        for at in [x.p, x.p + x.record_len - 1, x.table, x.t - 1, x.t + 30] {
            let mut b = good.clone();
            b[at] ^= 0x01;
            refused(&b, "do not match the sha256 in the trailer");
        }
    }

    #[test]
    fn rule_the_record() {
        let good = make2(&build2());
        let mut b = good.clone();
        let p = parts(&b).p;
        b[p] = b' ';
        reseal(&mut b);
        // A record that does not start as one is not where the image ends,
        // either; at the trailer it is refused by its own rule.
        refused(&b, "does not start with {\"format\":");
        let mut b = good.clone();
        edit_record(&mut b, "\"stage\":\"a\"", "\"stage\":\"c\"");
        refused(&b, "the program's record does not read");
        let mut b = good.clone();
        edit_record(&mut b, "{\"format\":2,", "{\"format\":3,");
        refused(&b, "the record says format 3, and its trailer is format 2");
        let mut b = good.clone();
        edit_record(&mut b, "\"name\":\"hello\"", "\"name\":\".ello\"");
        refused(&b, "plain word");
        let mut b = good.clone();
        let sha = read2(&good).unwrap().sha256().to_string();
        edit_record(&mut b, &sha[..2], "GG");
        refused(&b, "`sha256` is not a sha256");
        let mut b = good.clone();
        edit_record(&mut b, "x86_64", "\\u202e");
        refused(&b, "`target` is not printable");
        // A key it does not know is ignored.
        let x = parts(&good);
        let rec = std::str::from_utf8(&good[x.p..x.p + x.record_len]).unwrap();
        let rec = rec.replacen("{\"format\":2,", "{\"format\":2,\"later\":{\"a\":1},", 1);
        let src = place("source", 1, Encoding::Stored, true, b"exhale \"hi\"\n").unwrap();
        let b = assemble(&tiny_elf(), rec.as_bytes(), vec![src]).unwrap();
        let l = read2(&b).unwrap();
        assert_eq!(l.source(&mut Cursor::new(&b)).unwrap(), "exhale \"hi\"\n");
    }

    #[test]
    fn rule_section_ids() {
        let good = make2(&with_extra(false));
        let mut b = good.clone();
        let at = entry_byte(&b, 1, 0);
        b[at] = b'Z';
        reseal(&mut b);
        refused(
            &b,
            "section 2 of the table has an id that is not lower-case ASCII, zero-padded",
        );
        let mut b = good.clone();
        let at = entry_byte(&b, 1, 0);
        b[at + 2] = 0;
        reseal(&mut b);
        refused(&b, "not lower-case ASCII, zero-padded");
        let mut b = good.clone();
        let at = entry_byte(&b, 1, 0);
        b[at..at + 8].copy_from_slice(b"source\0\0");
        reseal(&mut b);
        refused(&b, "section `source` appears twice in the table");
        let mut b = good;
        let (a0, a1) = (entry_byte(&b, 0, 0), entry_byte(&b, 1, 0));
        b[a0..a0 + 8].copy_from_slice(b"zz-test\0");
        b[a1..a1 + 8].copy_from_slice(b"source\0\0");
        reseal(&mut b);
        refused(&b, "section `source` is out of order in the table");
    }

    #[test]
    fn rule_encodings_and_flag_bits() {
        let good = make2(&build2());
        let mut b = good.clone();
        let at = entry_byte(&b, 0, 10);
        b[at] = 2;
        reseal(&mut b);
        refused(
            &b,
            "section `source` has encoding 2; this cig reads 0 (stored) and 1 (DEFLATE)",
        );
        let mut b = good;
        let at = entry_byte(&b, 0, 11);
        b[at] = 0x81;
        reseal(&mut b);
        refused(
            &b,
            "section `source` sets flag bits this cig does not know (0x81)",
        );
    }

    #[test]
    fn rule_decoded_sizes() {
        let mut b = make2(&big2());
        let at = entry_byte(&b, 0, 12);
        set_u32(&mut b, at, MAX_PAYLOAD2 as u32 + 1);
        reseal(&mut b);
        refused(
            &b,
            "section `source` decodes to 67108865 bytes, more than 64 MiB",
        );
        let mut b = make2(&build2());
        let at = entry_byte(&b, 0, 12);
        set_u32(&mut b, at, 13);
        reseal(&mut b);
        refused(
            &b,
            "section `source` is stored as is, and its decoded length (13) is not its length (12)",
        );
    }

    #[test]
    fn rule_bounds_and_overlap() {
        let good = make2(&with_extra(false));
        let mut b = good.clone();
        let at = entry_byte(&b, 0, 16);
        set_u64(&mut b, at, 0);
        reseal(&mut b);
        refused(&b, "section `source` starts inside the record");
        let mut b = good.clone();
        let src = get_u64(&b, entry_byte(&b, 0, 16));
        let at = entry_byte(&b, 1, 16);
        set_u64(&mut b, at, src);
        reseal(&mut b);
        refused(&b, "section `zz-test` overlaps the section before it");
        let mut b = good.clone();
        for (field, v) in [(12, 4000u64), (24, 4000)] {
            let at = entry_byte(&b, 1, field);
            if field == 12 {
                set_u32(&mut b, at, v as u32);
            } else {
                set_u64(&mut b, at, v);
            }
        }
        reseal(&mut b);
        refused(&b, "section `zz-test` runs into the section table");
        let mut b = good;
        let at = entry_byte(&b, 1, 24);
        set_u64(&mut b, at, u64::MAX);
        let at = entry_byte(&b, 1, 12);
        set_u32(&mut b, at, u32::MAX);
        reseal(&mut b);
        refused(&b, "decodes to");
    }

    #[test]
    fn rule_alignment_and_padding() {
        let good = make2(&with_extra(false));
        let l = read2(&good).unwrap();
        let (src, extra) = (
            l.entry("source").unwrap().clone(),
            l.entry("zz-test").unwrap().clone(),
        );
        let gap = (extra.offset - (src.offset + src.length)) as usize;
        assert!(gap > 0, "the test needs padding between the two bodies");
        // A body moved off its alignment, its end where it was.
        let mut b = good.clone();
        set_u64(&mut b, entry_byte(&good, 1, 16), extra.offset + 1);
        set_u64(&mut b, entry_byte(&good, 1, 24), extra.length - 1);
        set_u32(&mut b, entry_byte(&good, 1, 12), extra.decoded_len - 1);
        reseal(&mut b);
        refused(
            &b,
            "section `zz-test` does not start at a file offset that is a multiple of 16",
        );
        // Moved by 16: aligned, but more padding than alignment needs.
        let mut b = good.clone();
        set_u64(&mut b, entry_byte(&good, 1, 16), extra.offset + 16);
        set_u64(&mut b, entry_byte(&good, 1, 24), extra.length - 16);
        set_u32(&mut b, entry_byte(&good, 1, 12), extra.decoded_len - 16);
        reseal(&mut b);
        refused(
            &b,
            "bytes lie before section `zz-test`, more than its alignment needs",
        );
        // Padding is not hashed: it must be zero.
        let mut b = good;
        b[l.start as usize + extra.offset as usize - 1] = 1;
        refused(&b, "the padding before section `zz-test` is not zero");
    }

    #[test]
    fn an_unknown_required_section_is_refused() {
        refused(
            &make2(&with_extra(true)),
            "the program needs section `zz-test`, which this cig does not read",
        );
    }

    #[test]
    fn an_unknown_optional_section_is_skipped() {
        let b = make2(&with_extra(false));
        let l = read2(&b).unwrap();
        assert_eq!(l.source(&mut Cursor::new(&b)).unwrap(), "exhale \"hi\"\n");
        l.validate(&mut Cursor::new(&b)).unwrap();
    }

    #[test]
    fn a_stage_without_its_section_is_refused() {
        let mut b = build2();
        b.stage = Stage::B;
        refused(
            &make2(&b),
            "a stage `b` program needs section `bytecode`, and this one has none",
        );
        b.sections.push(Section {
            id: "bytecode".to_string(),
            version: 1,
            encoding: Encoding::Stored,
            required: true,
            bytes: b"CIGB".to_vec(),
        });
        assert_eq!(read2(&make2(&b)).unwrap().stage(), Stage::B);
    }

    #[test]
    fn archive_tamper_refused() {
        let good = make2(&with_extra(false));
        let l = read2(&good).unwrap();
        for id in ["source", "zz-test"] {
            let e = l.entry(id).unwrap();
            let mut b = good.clone();
            b[(l.start + e.offset) as usize + 2] ^= 0x20;
            // The start reads no body, so it does not see the change...
            let back = read2(&b).unwrap();
            // ...and a use of the body does, as does CIG_VALIDATE's pass.
            let want =
                format!("section `{id}`'s bytes do not match the sha256 in the section table");
            assert_eq!(back.body(&mut Cursor::new(&b), id).unwrap_err(), want);
            assert_eq!(back.validate(&mut Cursor::new(&b)).unwrap_err(), want);
            if id == "zz-test" {
                assert!(back.source(&mut Cursor::new(&b)).is_ok());
            } else {
                assert_eq!(back.source(&mut Cursor::new(&b)).unwrap_err(), want);
            }
        }
    }

    #[test]
    fn a_body_is_held_to_its_decoded_length_and_its_text_to_the_record() {
        let good = make2(&big2());
        let len = read2(&good).unwrap().entry("source").unwrap().decoded_len;
        for (n, sentence) in [
            (
                len - 1,
                format!("section `source` inflates past the {} bytes", len - 1),
            ),
            (
                len + 1,
                format!(
                    "section `source` inflates to {len} bytes, not the {}",
                    len + 1
                ),
            ),
        ] {
            let mut b = good.clone();
            set_u32(&mut b, entry_byte(&good, 0, 12), n);
            reseal(&mut b);
            let l = read2(&b).unwrap();
            let e = l.source(&mut Cursor::new(&b)).unwrap_err();
            assert!(e.contains(&sentence), "{e}");
        }
        // A body that is not DEFLATE, with a table that says it is.
        let mut b = make2(&build2());
        let at = entry_byte(&b, 0, 10);
        b[at] = 1;
        reseal(&mut b);
        let l = read2(&b).unwrap();
        let e = l.source(&mut Cursor::new(&b)).unwrap_err();
        assert!(e.contains("section `source`"), "{e}");
        // Bytes that match the table, and a text that does not match the record.
        let mut b = make2(&build2());
        let other = hex::encode(Sha256::digest(b"something else"));
        let sha = read2(&b).unwrap().sha256().to_string();
        edit_record(&mut b, &sha, &other);
        let l = read2(&b).unwrap();
        assert_eq!(
            l.source(&mut Cursor::new(&b)).unwrap_err(),
            "the script in section `source` does not match the sha256 the record gives"
        );
        // Bytes that are not UTF-8, with the table's sha256 made to match.
        let mut b = make2(&build2());
        let l = read2(&b).unwrap();
        let at = (l.start + l.entry("source").unwrap().offset) as usize;
        b[at] = 0xff;
        let sha: [u8; 32] = Sha256::digest(&b[at..at + 12]).into();
        let e = entry_byte(&b, 0, 32);
        b[e..e + 32].copy_from_slice(&sha);
        reseal(&mut b);
        let l = read2(&b).unwrap();
        assert_eq!(
            l.source(&mut Cursor::new(&b)).unwrap_err(),
            "section `source` is not UTF-8 text"
        );
    }

    /// A reader that notes every byte it is asked for.
    struct Counting<'a> {
        c: Cursor<&'a [u8]>,
        reads: Vec<(u64, u64)>,
    }

    impl Read for Counting<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let at = self.c.position();
            let n = self.c.read(buf)?;
            if n > 0 {
                self.reads.push((at, at + n as u64));
            }
            Ok(n)
        }
    }

    impl Seek for Counting<'_> {
        fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
            self.c.seek(to)
        }
    }

    #[test]
    fn the_head_is_all_a_start_hashes() {
        for b in [big2(), with_extra(false), with_extra(false)] {
            let bytes = make2(&b);
            let mut c = Counting {
                c: Cursor::new(&bytes),
                reads: Vec::new(),
            };
            let l = read(&mut c).unwrap().unwrap();
            let bodies: Vec<(u64, u64)> = l
                .table()
                .iter()
                .map(|e| (l.start + e.offset, l.start + e.offset + e.length))
                .collect();
            for (a, z) in &c.reads {
                for (ba, bz) in &bodies {
                    assert!(
                        z <= ba || a >= bz,
                        "read {a}..{z} touches a body at {ba}..{bz}"
                    );
                }
            }
            let x = parts(&bytes);
            let read_total: u64 = c.reads.iter().map(|(a, z)| z - a).sum();
            let head = TRAILER2_LEN as usize + x.record_len + (x.t - x.table);
            let most = head + (ALIGN as usize - 1) * (l.table().len() + 1);
            assert!(read_total as usize <= most, "{read_total} > {most}");
        }
    }

    #[test]
    fn a_damaged_format_2_program_never_panics() {
        for b in [with_extra(false), big2()] {
            let good = make2(&b);
            let p = tiny_elf().len();
            for at in p..good.len() {
                for change in [0x01u8, 0x80, 0xff] {
                    let mut bad = good.clone();
                    bad[at] ^= change;
                    match read(&mut Cursor::new(&bad)) {
                        Err(_) => {}
                        Ok(None) => panic!("byte {at} ^ {change:#x}: read as no program"),
                        Ok(Some(l)) => {
                            // Only a body's bytes pass the start; using it refuses them.
                            let in_body = l.table().iter().any(|e| {
                                (l.start + e.offset..l.start + e.offset + e.length)
                                    .contains(&(at as u64))
                            });
                            assert!(in_body, "byte {at} ^ {change:#x} passed outside a body");
                            assert!(
                                l.validate(&mut Cursor::new(&bad)).is_err(),
                                "byte {at} ^ {change:#x}: changed bytes validated"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_format_2_program_cut_short_is_damaged_never_cig() {
        let whole = make2(&with_extra(false));
        assert!(read2(&whole).is_ok());
        for cut in [1, 8, 55, 56, 57, 100, whole.len() - 70] {
            let e = read_bytes2(&whole[..whole.len() - cut]).unwrap_err();
            assert!(e.contains("not at its end"), "{cut} cut: {e}");
        }
        let mut junk = whole.clone();
        junk.extend_from_slice(b"XX");
        assert!(read_bytes2(&junk)
            .unwrap_err()
            .contains("2 bytes follow its trailer"));
        let mut padded = whole.clone();
        padded.extend_from_slice(&[0; 4096]);
        assert_eq!(read2(&padded).unwrap(), read2(&whole).unwrap());
    }

    fn read_bytes2(b: &[u8]) -> Result<Option<Loaded>, String> {
        read(&mut Cursor::new(b))
    }

    #[test]
    fn a_signed_format_2_windows_program_still_reads() {
        let program = append2(&tiny_pe(), &with_extra(false)).unwrap();
        let signed = sign(program.clone());
        let l = read2(&signed).unwrap();
        assert_eq!(
            l.source(&mut Cursor::new(&signed)).unwrap(),
            "exhale \"hi\"\n"
        );
        let mut unsigned = program;
        unsigned.extend_from_slice(&sign(tiny_pe())[0x400..]);
        assert!(read_bytes2(&unsigned)
            .unwrap_err()
            .contains("16 bytes follow its trailer"));
    }

    #[test]
    fn a_trailer_with_another_magic_is_not_a_trailer() {
        let mut b = make2(&build2());
        let t = parts(&b).t;
        b[t + 7] = b'3';
        refused(&b, "not at its end");
    }
}
