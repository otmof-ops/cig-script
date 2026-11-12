// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Pack files (#147): a native op's small before-states, many to a file.
//!
//! A before-state under [`OBJECT_MAX`] bytes is not an object file of its
//! own (one file and one disk block each, 4 KiB for a 300-byte snapshot)
//! but a record appended to a pack, `tin/packs/<id[..2]>/<id>`, where `id`
//! is a random 32-hex name. One process writes one pack at a time and
//! never appends to a pack it did not create, so a pack is a run of
//! records written by one writer, front to back.
//!
//! ```text
//! pack   := "CIGPACK1" record*
//! record := sha256 (32) | form (1) | raw_len (u32 LE) | stored_len (u32 LE)
//!           | check (4) | payload (stored_len)
//! ```
//!
//! - `sha256` is the hash of the object's own bytes, uncompressed: the
//!   same name the object would have as a loose file in `tin/objects`.
//! - `form` is 0 for bytes kept as they are, 1 for zstd.
//! - `check` is the first four bytes of the SHA-256 of the 41 bytes
//!   before it, so a header that was not written whole is told from one
//!   that was.
//!
//! The record headers are the pack's index: one read of the file and one
//! walk over the headers find every object in it, which is how a restore
//! reads a pack (once, then every object it needs from memory). A write
//! cut short (a killed cig) leaves at most one partial record at the end:
//! its header or payload is short, the walk stops there, and nothing the
//! journal names can be in it, because the journal line is written only
//! after the record's write returned. Every object read from a pack is
//! decompressed and hashed against its name before a byte of it is used.

use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Before-states smaller than this go into a pack.
pub const OBJECT_MAX: u64 = 16 * 1024;
/// A pack this large is finished and the next record starts a new one, so
/// a restore never holds more than about this much of one in memory.
pub const ROLL_AT: u64 = 32 * 1024 * 1024;
/// The first bytes of every pack.
pub const MAGIC: &[u8; 8] = b"CIGPACK1";
/// The bytes of a record's header.
pub const HEAD: usize = 45;
/// The file a run lists its packs in, one id a line, before the first
/// record for it lands, so a `cig tin gc` during the run keeps them.
pub const LISTED: &str = "tin.packs";

/// Bytes kept as they are.
pub const FORM_RAW: u8 = 0;
/// Bytes kept as one zstd frame.
pub const FORM_ZSTD: u8 = 1;

pub fn root() -> PathBuf {
    super::tin::dir().join("packs")
}

/// Where the pack named `id` is.
pub fn pack_path(id: &str) -> PathBuf {
    root().join(&id[..2]).join(id)
}

fn is_id(name: &str) -> bool {
    name.len() == 32 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Is this snapshot path a pack in the tin?
pub fn is_packed(path: &Path) -> bool {
    path.starts_with(root()) && path.file_name().and_then(|n| n.to_str()).is_some_and(is_id)
}

/// Every pack in the tin, with its size.
pub fn packs() -> Vec<(PathBuf, u64)> {
    let mut out = Vec::new();
    let Ok(buckets) = fs::read_dir(root()) else {
        return out;
    };
    for b in buckets.flatten() {
        let Ok(files) = fs::read_dir(b.path()) else {
            continue;
        };
        for f in files.flatten() {
            if f.file_name().to_str().is_some_and(is_id) {
                if let Ok(m) = f.metadata() {
                    if m.is_file() {
                        out.push((f.path(), m.len()));
                    }
                }
            }
        }
    }
    out.sort();
    out
}

// ---------------------------------------------------------------- format

/// One record found by a walk over a pack.
#[derive(Clone, Debug)]
pub struct Entry {
    pub sha256: String,
    pub form: u8,
    pub raw: u32,
    pub stored: u32,
    /// Where the payload starts in the pack.
    pub at: usize,
}

/// How a walk over a pack ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum End {
    /// Every byte belongs to a whole record.
    Clean,
    /// The last record was not written whole (a write cut short): its
    /// offset. Nothing after it is read.
    Torn(usize),
    /// A whole header that does not check, at this offset: the pack was
    /// changed after it was written. Nothing after it is read.
    Damaged(usize),
}

fn header_check(head: &[u8]) -> [u8; 4] {
    let d = Sha256::digest(&head[..41]);
    [d[0], d[1], d[2], d[3]]
}

/// The record for `bytes`, named `sha256` (hex): compressed when that
/// saves at least a tenth.
pub fn encode(sha256: &str, bytes: &[u8]) -> io::Result<Vec<u8>> {
    let raw_sha = hex::decode(sha256)
        .ok()
        .filter(|s| s.len() == 32)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "an object name is 64 hex"))?;
    let raw_len = u32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "too large for a pack"))?;
    // zstd level 3, kept only where it saves at least a tenth.
    let packed = super::tin::squeeze(bytes);
    let (form, payload): (u8, &[u8]) = match &packed {
        Some(c) => (FORM_ZSTD, c),
        None => (FORM_RAW, bytes),
    };
    let mut rec = Vec::with_capacity(HEAD + payload.len());
    rec.extend_from_slice(&raw_sha);
    rec.push(form);
    rec.extend_from_slice(&raw_len.to_le_bytes());
    rec.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    let check = header_check(&rec);
    rec.extend_from_slice(&check);
    rec.extend_from_slice(payload);
    Ok(rec)
}

/// Walk a pack's records. An error only when it is not a pack at all.
pub fn walk(pack: &[u8]) -> io::Result<(Vec<Entry>, End)> {
    if pack.len() < MAGIC.len() || &pack[..MAGIC.len()] != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a pack file (no CIGPACK1 at its start)",
        ));
    }
    let mut out = Vec::new();
    let mut at = MAGIC.len();
    loop {
        if at == pack.len() {
            return Ok((out, End::Clean));
        }
        if pack.len() - at < HEAD {
            return Ok((out, End::Torn(at)));
        }
        let head = &pack[at..at + HEAD];
        let form = head[32];
        let raw = u32::from_le_bytes([head[33], head[34], head[35], head[36]]);
        let stored = u32::from_le_bytes([head[37], head[38], head[39], head[40]]);
        if head[41..45] != header_check(head) || form > FORM_ZSTD || u64::from(raw) >= OBJECT_MAX {
            return Ok((out, End::Damaged(at)));
        }
        let start = at + HEAD;
        if pack.len() - start < stored as usize {
            return Ok((out, End::Torn(at)));
        }
        out.push(Entry {
            sha256: hex::encode(&head[..32]),
            form,
            raw,
            stored,
            at: start,
        });
        at = start + stored as usize;
    }
}

/// The object a record holds, decompressed and checked against its name.
pub fn object_of(pack: &[u8], e: &Entry) -> io::Result<Vec<u8>> {
    let payload = &pack[e.at..e.at + e.stored as usize];
    let bytes = match e.form {
        FORM_RAW => payload.to_vec(),
        _ => zstd::bulk::decompress(payload, e.raw as usize).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "packed object {} does not decompress: {err}",
                    &e.sha256[..12]
                ),
            )
        })?,
    };
    if bytes.len() != e.raw as usize || hex::encode(Sha256::digest(&bytes)) != e.sha256 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "packed object {} is corrupt (does not hash to its name)",
                &e.sha256[..12]
            ),
        ));
    }
    Ok(bytes)
}

// ---------------------------------------------------------------- writing

struct Writer {
    id: String,
    path: PathBuf,
    file: fs::File,
    len: u64,
    held: HashSet<String>,
    listed: HashSet<PathBuf>,
}

static WRITER: Mutex<Option<Writer>> = Mutex::new(None);

/// Name the pack in a run's list. Not flushed: the list only keeps a
/// `cig tin gc` in another terminal from taking a pack the journal is
/// about to name, and a killed cig leaves it in the page cache.
fn list_in(run_dir: &Path, id: &str) -> io::Result<()> {
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(run_dir.join(LISTED))?;
    f.write_all(format!("{id}\n").as_bytes())
}

fn create(run_dir: Option<&Path>, grouped: bool) -> io::Result<Writer> {
    let id = super::seal::random_id()?;
    let path = pack_path(&id);
    let bucket = path.parent().expect("a pack has a bucket").to_path_buf();
    fs::create_dir_all(&bucket)?;
    let mut listed = HashSet::new();
    // Listed before it exists: a gc that does not see the list yet does
    // not see the pack yet either.
    if let Some(d) = run_dir {
        list_in(d, &id)?;
        listed.insert(d.to_path_buf());
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    file.write_all(MAGIC)?;
    if grouped {
        super::tin::defer_file(&path);
        super::tin::defer_dir(&bucket);
    } else {
        super::journal::full_sync_data(&file)?;
        super::journal::sync_dir_pub(&bucket)?;
    }
    Ok(Writer {
        id,
        path,
        file,
        len: MAGIC.len() as u64,
        held: HashSet::new(),
        listed,
    })
}

/// Keep `bytes` (named `sha256`) in this process's pack and return the
/// pack's path, which is what the journal names. The same bytes kept
/// twice by one process are one record. `grouped` joins the flush to the
/// group (`tin::sync_deferred`); otherwise the record is flushed now.
pub fn keep(
    bytes: &[u8],
    sha256: &str,
    run_dir: Option<&Path>,
    grouped: bool,
) -> io::Result<PathBuf> {
    let mut g = WRITER.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(w) = g.as_mut() {
        if w.held.contains(sha256) {
            if let Some(d) = run_dir {
                if !w.listed.contains(d) {
                    list_in(d, &w.id)?;
                    w.listed.insert(d.to_path_buf());
                }
            }
            return Ok(w.path.clone());
        }
    }
    // A pack that is full, or that a gc removed, is not written to again.
    if g.as_ref()
        .is_none_or(|w| w.len >= ROLL_AT || !w.path.is_file())
    {
        *g = Some(create(run_dir, grouped)?);
    }
    let w = g.as_mut().expect("a writer");
    if let Some(d) = run_dir {
        if !w.listed.contains(d) {
            list_in(d, &w.id)?;
            w.listed.insert(d.to_path_buf());
        }
    }
    let rec = encode(sha256, bytes)?;
    if let Err(e) = w.file.write_all(&rec) {
        // Part of a record may have landed: this pack ends there, torn,
        // and the next record starts a new one.
        *g = None;
        return Err(e);
    }
    w.len += rec.len() as u64;
    w.held.insert(sha256.to_string());
    let path = w.path.clone();
    if grouped {
        super::tin::defer_file(&path);
    } else {
        super::journal::full_sync_data(&w.file)?;
    }
    Ok(path)
}

// ---------------------------------------------------------------- reading

struct Loaded {
    path: PathBuf,
    bytes: Vec<u8>,
    at: HashMap<String, Entry>,
}

/// The pack read last: an unburn restores newest first, and the objects of
/// one run sit together in one pack, so it is read once.
static READ: Mutex<Option<Loaded>> = Mutex::new(None);

fn load(pack: &Path) -> io::Result<Loaded> {
    super::tin::note_file_read();
    let bytes = fs::read(pack)?;
    let (entries, _) =
        walk(&bytes).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", pack.display())))?;
    let mut at = HashMap::with_capacity(entries.len());
    for e in entries {
        at.entry(e.sha256.clone()).or_insert(e);
    }
    Ok(Loaded {
        path: pack.to_path_buf(),
        bytes,
        at,
    })
}

/// The object named `sha256` from `pack`, checked against its name.
pub fn read(pack: &Path, sha256: &str) -> io::Result<Vec<u8>> {
    let mut g = READ.lock().unwrap_or_else(|p| p.into_inner());
    let fresh = match g.as_ref() {
        Some(l) if l.path == pack => {
            // A pack still being written (a rollback in the process that
            // wrote it) may have grown since it was read.
            !l.at.contains_key(sha256)
                && fs::metadata(pack).map(|m| m.len()).ok() != Some(l.bytes.len() as u64)
        }
        _ => true,
    };
    if fresh {
        *g = None;
        *g = Some(load(pack)?);
    }
    let l = g.as_ref().expect("a loaded pack");
    match l.at.get(sha256) {
        Some(e) => object_of(&l.bytes, e),
        None => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "pack {} holds no object {}",
                pack.display(),
                &sha256[..sha256.len().min(12)]
            ),
        )),
    }
}

/// Every object a pack holds, by name, with its size written out.
pub fn ids(pack: &Path) -> Vec<(String, u64)> {
    let Ok(bytes) = fs::read(pack) else {
        return Vec::new();
    };
    walk(&bytes)
        .map(|(es, _)| {
            es.into_iter()
                .map(|e| (e.sha256, u64::from(e.raw)))
                .collect()
        })
        .unwrap_or_default()
}

/// The object named `sha256` from whichever pack holds it: for the vault,
/// which keeps objects by name and does not know where the tin put them.
pub fn find_anywhere(sha256: &str) -> Option<Vec<u8>> {
    for (p, _) in packs() {
        let Ok(bytes) = fs::read(&p) else {
            continue;
        };
        let Ok((entries, _)) = walk(&bytes) else {
            continue;
        };
        if let Some(e) = entries.iter().find(|e| e.sha256 == sha256) {
            if let Ok(b) = object_of(&bytes, e) {
                return Some(b);
            }
        }
    }
    None
}

/// Every record of a pack hashes to its name and every header checks. A
/// last record cut short is not a problem: no journal names it. One line
/// per problem.
pub fn verify(pack: &Path) -> Vec<String> {
    let bytes = match fs::read(pack) {
        Ok(b) => b,
        Err(e) => return vec![format!("{}: cannot read: {e}", pack.display())],
    };
    let (entries, end) = match walk(&bytes) {
        Ok(v) => v,
        Err(e) => return vec![format!("{}: {e}", pack.display())],
    };
    let mut problems = Vec::new();
    for e in &entries {
        if let Err(err) = object_of(&bytes, e) {
            problems.push(format!(
                "{}: {err}; a restore from it would be refused",
                pack.display()
            ));
        }
    }
    if let End::Damaged(at) = end {
        problems.push(format!(
            "{}: the record header at byte {at} does not check; the {} bytes from there on cannot be read",
            pack.display(),
            bytes.len() - at
        ));
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack_of(records: &[(&str, &[u8])]) -> Vec<u8> {
        let mut p = MAGIC.to_vec();
        for (sha, b) in records {
            p.extend(encode(sha, b).unwrap());
        }
        p
    }

    fn name(b: &[u8]) -> String {
        hex::encode(Sha256::digest(b))
    }

    #[test]
    fn a_pack_walks_back_to_what_was_written() {
        let text = "a line of text that repeats\n".repeat(200);
        let tiny = b"x".to_vec();
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        let noise: Vec<u8> = (0..4000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect();
        let objs = [
            (name(text.as_bytes()), text.as_bytes().to_vec()),
            (name(&tiny), tiny),
            (name(&noise), noise),
        ];
        let recs: Vec<(&str, &[u8])> = objs
            .iter()
            .map(|(s, b)| (s.as_str(), b.as_slice()))
            .collect();
        let pack = pack_of(&recs);
        let (entries, end) = walk(&pack).unwrap();
        assert_eq!(end, End::Clean);
        assert_eq!(entries.len(), 3);
        // Text is kept compressed; noise and a byte are kept as they are.
        assert_eq!(entries[0].form, FORM_ZSTD);
        assert!(
            entries[0].stored * 10 < entries[0].raw * 3,
            "{:?}",
            entries[0]
        );
        assert_eq!(entries[1].form, FORM_RAW);
        assert_eq!(entries[2].form, FORM_RAW);
        for (e, (s, b)) in entries.iter().zip(objs.iter()) {
            assert_eq!(&e.sha256, s);
            assert_eq!(&object_of(&pack, e).unwrap(), b);
        }
    }

    #[test]
    fn a_torn_record_is_never_read_and_the_ones_before_it_are() {
        let a = b"first object".to_vec();
        let b = "second object, long enough to matter "
            .repeat(30)
            .into_bytes();
        let whole = pack_of(&[(&name(&a), &a), (&name(&b), &b)]);
        let first_end = MAGIC.len() + HEAD + a.len();
        // Every cut inside the second record: the first still reads, the
        // second is never found.
        for cut in first_end + 1..whole.len() {
            let (entries, end) = walk(&whole[..cut]).unwrap();
            assert_eq!(entries.len(), 1, "cut at {cut}");
            assert_eq!(end, End::Torn(first_end), "cut at {cut}");
            assert_eq!(object_of(&whole[..cut], &entries[0]).unwrap(), a);
        }
    }

    #[test]
    fn a_changed_byte_is_refused() {
        let a = "some text kept in a pack\n".repeat(20).into_bytes();
        let pack = pack_of(&[(&name(&a), &a)]);
        // In the payload: the record is found and refused.
        let mut bad = pack.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        let (entries, _) = walk(&bad).unwrap();
        assert!(object_of(&bad, &entries[0]).is_err());
        // In the header: the walk stops there.
        let mut bad = pack.clone();
        bad[MAGIC.len() + 34] ^= 1;
        let (entries, end) = walk(&bad).unwrap();
        assert!(entries.is_empty());
        assert_eq!(end, End::Damaged(MAGIC.len()));
        // Not a pack at all.
        assert!(walk(b"CIGPACK0").is_err());
    }
}
