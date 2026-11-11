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
//! The end of a program's executable:
//!
//! ```text
//! ... cig ...                                  the runtime, as released
//! payload                                      `len` bytes of JSON: the program's record, its source included
//! "CIGPROG1"  len (u64, LE)  sha256(payload)   the trailer, 48 bytes
//! ```
//!
//! A trailer whose payload does not hash to the sha256 it carries is a
//! damaged program, and it refuses to run (E1050). The payload is written
//! with its fields in one fixed order and nothing that changes between
//! builds, so the same script, compiler and runtime give the same bytes.
//!
//! The sha256s are integrity, not authenticity: they catch a damaged or
//! truncated program, not an edited one (anyone can recompute them), and
//! the runtime half is not covered. Programs are unsigned until
//! cig-compile#10.

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

/// Does `bytes` end in a trailer (a program, damaged or not)?
pub fn carries_program(bytes: &[u8]) -> bool {
    bytes.len() >= TRAILER_LEN as usize && bytes[bytes.len() - TRAILER_LEN as usize..][..8] == MAGIC
}

/// The program at the end of `f`, if one is there. `Ok(None)`: none is;
/// `Err`: one is there and it is damaged.
///
/// A program's payload starts where its runtime's image ends ([`image_of`]),
/// so a binary built as a program knows it is one even when its trailer is
/// gone (#135): cut short, or with bytes added after it, it is damaged, never
/// `cig`. Two things may follow a trailer and leave the program whole: zero
/// padding, and a PE certificate table (a Windows program signed after it
/// was built).
pub fn read<F: Read + Seek>(f: &mut F) -> Result<Option<Program>, String> {
    let end = f.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
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
        if at.checked_add(size) == Some(end) && at >= image.end + TRAILER_LEN {
            let t = read_at(f, at - TRAILER_LEN, TRAILER_LEN)?;
            if t[..8] == MAGIC && trailer_len(&t).checked_add(image.end) == Some(at - TRAILER_LEN) {
                return at_trailer(f, at - TRAILER_LEN, &t).map(Some);
            }
        }
    }
    // The newest trailer whose payload runs from the image's end to it.
    let from = end.saturating_sub(TAIL).max(image.end);
    let tail = read_at(f, from, end - from)?;
    for q in memchr::memmem::rfind_iter(&tail, &MAGIC) {
        let Some(t) = tail.get(q..q + TRAILER_LEN as usize) else {
            continue;
        };
        let at = from + q as u64;
        if trailer_len(t).checked_add(image.end) != Some(at) {
            continue;
        }
        let p = at_trailer(f, at, t)?;
        let after = &tail[q + TRAILER_LEN as usize..];
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

/// The payload length a trailer gives.
fn trailer_len(t: &[u8]) -> u64 {
    u64::from_le_bytes(t[8..16].try_into().expect("eight bytes"))
}

/// The program whose trailer `t` is at `at`.
fn at_trailer<F: Read + Seek>(f: &mut F, at: u64, t: &[u8]) -> Result<Program, String> {
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
    check_words(&p)?;
    Ok(p)
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
fn check_words(p: &Program) -> Result<(), String> {
    if !good_name(&p.name) {
        return Err(
            "the program's name is not a plain word (letters, digits, `.`, `_`, `-`)".to_string(),
        );
    }
    for (what, v, max) in [
        ("file", &p.file, 4096),
        ("compiler", &p.compiler, 128),
        ("cig", &p.cig, 32),
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
pub fn own() -> Option<Result<Program, Unrunnable>> {
    let marked = std::env::var_os(SELF_ENV).is_some();
    if marked {
        // Before any thread starts: nothing else reads the environment yet.
        std::env::remove_var(SELF_ENV);
    }
    let Some(mut f) = open_self() else {
        let name = self_name();
        return (!named_as_cig(&name)).then_some(Err(Unrunnable::Unreadable { name }));
    };
    let p = match read(&mut f) {
        Ok(None) => return None,
        Ok(Some(p)) => p,
        Err(e) => return Some(Err(Unrunnable::Damaged(e))),
    };
    // A setuid program would run its caller's PATH, and keep its runs and
    // keys where its caller's environment says, with the rights of the
    // file's owner: it stops before it reads any of that.
    if privileged() {
        return Some(Err(Unrunnable::Raised { name: p.name }));
    }
    if marked {
        let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
        if internal_call(&args) {
            return None;
        }
    }
    if p.cig != crate::VERSION {
        return Some(Err(Unrunnable::OtherCig {
            name: p.name,
            cig: p.cig,
        }));
    }
    Some(Ok(p))
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
        read(&mut Cursor::new(b))
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
}
