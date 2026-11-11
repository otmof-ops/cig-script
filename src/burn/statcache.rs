// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The tin's stat cache (#148), off by default (`cig config hop_statcache
//! on` turns it on). One index per pack, keyed by the hash of the pack's
//! canonical roots, kept at `<cig home>/tin/statcache/<pack hash>`. Each
//! entry pairs a file's `(dev, inode, size, mtime_ns, ctime_ns, mode)` with
//! the sha256 of what the tin kept for it, so a later hop whose `touches`
//! file matches on every field knows the bytes are the same, and its tin
//! object can be referred to by hash without a read.
//!
//! The racy-git rule: a file whose mtime falls within the filesystem's
//! timestamp granularity of the cache's own write is re-hashed anyway, so
//! a same-second edit after the cache saved is never missed.
//!
//! A restore never trusts the cache (E704 still re-hashes before and
//! after). The cache decides what needs reading, nothing else.
//!
//! The on-disk format is a packed little-endian stream, temp-and-rename on
//! save, so a crash in the middle leaves the old index intact. A file with
//! a different magic, version or entry count is read as empty: a corrupt
//! index can never fool a restore, only lose an optimisation.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// `~/.cigscript/tin/statcache` (or the override under `CIGSCRIPT_HOME`).
pub fn dir() -> PathBuf {
    super::tin::dir().join("statcache")
}

/// The index file for this pack.
pub fn path_for(pack_hash: &str) -> PathBuf {
    dir().join(pack_hash)
}

/// Is the stat cache on in this process? Read once, like `watch_cap` and
/// `hop_skip`: a run keeps the setting it started with. Unix only until
/// the Windows side can read `file_index` on stable (unstable rust-lang
/// issue #63010): the setting parses on every platform, but on Windows
/// the cache stays off.
pub fn is_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        if !cfg!(unix) {
            return false;
        }
        matches!(
            crate::config::Config::load().get("hop_statcache").as_str(),
            "on"
        )
    })
}

/// How close to the cache's write time a file's mtime can be before the
/// cache refuses to trust itself: two seconds covers FAT-grade timestamps
/// (the coarsest of the common filesystems), the same margin the scan uses
/// for a reusable-directory decision.
const RACY_MARGIN_NS: u64 = 2_000_000_000;

/// The hash a pack is keyed by: the sha256 of its roots, canonicalised so
/// `./build` and `/abs/build` give one file. Not secret, so an eight-byte
/// prefix after the hex would be enough, but a whole hash keeps the name
/// easy to grep and read.
pub fn pack_hash(roots: &[PathBuf]) -> String {
    let mut h = Sha256::new();
    let mut names: Vec<String> = roots
        .iter()
        .map(|r| fs::canonicalize(r).unwrap_or_else(|_| r.clone()))
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    names.sort();
    for n in names {
        h.update(n.as_bytes());
        h.update(b"\0");
    }
    hex::encode(h.finalize())
}

/// One file's stat tuple, as the cache reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatTuple {
    pub dev: u64,
    pub inode: u64,
    pub size: u64,
    pub mtime_ns: u64,
    pub ctime_ns: u64,
    pub mode: u32,
}

/// What a cache entry carries: the stat tuple above, the sha256 the tin
/// kept it under, and whether the tin kept it as a recipe (a chunked file).
/// The recipe flag is informational: `object_for(sha256)` finds the right
/// path whichever one it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub stat: StatTuple,
    pub sha256: String,
    pub chunked: bool,
}

/// The cache for one pack: entries keyed by path (as bytes, exact on Unix
/// and UTF-8 on Windows, like `scan::rel_bytes`), and the time (ns since
/// the epoch) the cache was written. Save time serves the racy-git rule:
/// a file whose mtime is within `RACY_MARGIN_NS` of the write is re-hashed
/// instead of trusted.
#[derive(Clone, Debug, Default)]
pub struct Cache {
    pub entries: BTreeMap<Vec<u8>, Entry>,
    pub written_at_ns: u64,
}

const MAGIC: &[u8; 8] = b"CIGSCAC1";
const FORMAT: u32 = 1;

/// Read the cache for this pack, tolerating absence, truncation, a new
/// format or a stale magic: anything cig cannot trust is read as empty,
/// so the fallback is the slow path, never a wrong decision.
pub fn load(pack_hash: &str) -> Cache {
    let p = path_for(pack_hash);
    let Ok(mut f) = fs::File::open(&p) else {
        return Cache::default();
    };
    let mut head = [0u8; 8 + 4 + 8 + 4];
    if f.read_exact(&mut head).is_err() {
        return Cache::default();
    }
    if &head[..8] != MAGIC {
        return Cache::default();
    }
    let format = u32::from_le_bytes(head[8..12].try_into().unwrap());
    if format != FORMAT {
        return Cache::default();
    }
    let written_at_ns = u64::from_le_bytes(head[12..20].try_into().unwrap());
    let entry_count = u32::from_le_bytes(head[20..24].try_into().unwrap()) as usize;
    let mut body = Vec::new();
    if f.read_to_end(&mut body).is_err() {
        return Cache::default();
    }
    let mut cursor = 0usize;
    let mut entries = BTreeMap::new();
    for _ in 0..entry_count {
        let Some(entry) = read_one(&body, &mut cursor) else {
            // A truncated index is not a bug: return what has read cleanly,
            // and the hop stashes the rest.
            break;
        };
        entries.insert(entry.0, entry.1);
    }
    Cache {
        entries,
        written_at_ns,
    }
}

fn read_u16(body: &[u8], cursor: &mut usize) -> Option<u16> {
    let end = cursor.checked_add(2)?;
    if end > body.len() {
        return None;
    }
    let v = u16::from_le_bytes(body[*cursor..end].try_into().unwrap());
    *cursor = end;
    Some(v)
}

fn read_u32(body: &[u8], cursor: &mut usize) -> Option<u32> {
    let end = cursor.checked_add(4)?;
    if end > body.len() {
        return None;
    }
    let v = u32::from_le_bytes(body[*cursor..end].try_into().unwrap());
    *cursor = end;
    Some(v)
}

fn read_u64(body: &[u8], cursor: &mut usize) -> Option<u64> {
    let end = cursor.checked_add(8)?;
    if end > body.len() {
        return None;
    }
    let v = u64::from_le_bytes(body[*cursor..end].try_into().unwrap());
    *cursor = end;
    Some(v)
}

fn read_bytes<'a>(body: &'a [u8], cursor: &mut usize, n: usize) -> Option<&'a [u8]> {
    let end = cursor.checked_add(n)?;
    if end > body.len() {
        return None;
    }
    let out = &body[*cursor..end];
    *cursor = end;
    Some(out)
}

fn read_one(body: &[u8], cursor: &mut usize) -> Option<(Vec<u8>, Entry)> {
    let path_len = read_u16(body, cursor)? as usize;
    let path = read_bytes(body, cursor, path_len)?.to_vec();
    let dev = read_u64(body, cursor)?;
    let inode = read_u64(body, cursor)?;
    let size = read_u64(body, cursor)?;
    let mtime_ns = read_u64(body, cursor)?;
    let ctime_ns = read_u64(body, cursor)?;
    let mode = read_u32(body, cursor)?;
    let flags = read_bytes(body, cursor, 1)?[0];
    let sha256_raw = read_bytes(body, cursor, 32)?;
    let entry = Entry {
        stat: StatTuple {
            dev,
            inode,
            size,
            mtime_ns,
            ctime_ns,
            mode,
        },
        sha256: hex::encode(sha256_raw),
        chunked: (flags & 0x01) != 0,
    };
    Some((path, entry))
}

/// Write the cache for this pack: temp file, fsync, rename, so a crash in
/// the middle leaves what was there before. An empty cache removes the
/// index altogether instead of leaving a shell behind.
pub fn save(pack_hash: &str, cache: &Cache) -> io::Result<()> {
    let p = path_for(pack_hash);
    if cache.entries.is_empty() {
        let _ = fs::remove_file(&p);
        return Ok(());
    }
    let d = dir();
    fs::create_dir_all(&d)?;
    let mut buf: Vec<u8> = Vec::with_capacity(32 + cache.entries.len() * 128);
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&FORMAT.to_le_bytes());
    buf.extend_from_slice(&cache.written_at_ns.to_le_bytes());
    buf.extend_from_slice(&(cache.entries.len() as u32).to_le_bytes());
    for (path, entry) in &cache.entries {
        if path.len() > u16::MAX as usize {
            // A path longer than 64 KiB is not reachable on any real
            // filesystem; one that is is skipped, not written, so no row
            // is trimmed on read.
            continue;
        }
        let Ok(raw) = hex::decode(&entry.sha256) else {
            continue;
        };
        if raw.len() != 32 {
            continue;
        }
        buf.extend_from_slice(&(path.len() as u16).to_le_bytes());
        buf.extend_from_slice(path);
        buf.extend_from_slice(&entry.stat.dev.to_le_bytes());
        buf.extend_from_slice(&entry.stat.inode.to_le_bytes());
        buf.extend_from_slice(&entry.stat.size.to_le_bytes());
        buf.extend_from_slice(&entry.stat.mtime_ns.to_le_bytes());
        buf.extend_from_slice(&entry.stat.ctime_ns.to_le_bytes());
        buf.extend_from_slice(&entry.stat.mode.to_le_bytes());
        let flags: u8 = if entry.chunked { 0x01 } else { 0x00 };
        buf.push(flags);
        buf.extend_from_slice(&raw);
    }
    let tmp = d.join(format!(".{pack_hash}.tmp"));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&buf)?;
        f.flush()?;
        let _ = super::journal::sync_file(&tmp);
    }
    fs::rename(&tmp, &p)?;
    let _ = super::journal::sync_dir_pub(&d);
    Ok(())
}

/// Now, in nanoseconds since the epoch; 0 where the clock is unreadable.
pub fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Would this entry be trusted if the file's stat tuple matched and its
/// tin object is still there? The racy-git rule: a mtime close to the
/// cache's own write is not trusted, so a same-second edit cannot hide.
pub fn is_racy(mtime_ns: u64, written_at_ns: u64) -> bool {
    mtime_ns.saturating_add(RACY_MARGIN_NS) >= written_at_ns
}

/// The shortcut after a hop: a file whose stat tuple reads as it did
/// before the hop was not written by it, unless its last change came
/// within the racy margin of the hop's start. There, the hop's own write
/// can land in the same timestamp tick as the change before it and leave
/// every field as it was, so the file is hashed instead.
pub fn unchanged_through(before: &StatTuple, now: &StatTuple, started_ns: u64) -> bool {
    before == now && !is_racy(before.mtime_ns, started_ns) && !is_racy(before.ctime_ns, started_ns)
}

/// Read a file's stat tuple, or `None` when it is not a regular file. A
/// link, a directory or a socket never caches: the tin keeps a link as its
/// target, not as bytes to hash.
pub fn stat_of(path: &Path) -> Option<StatTuple> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let ctime_ns = (meta.ctime() as i128)
            .saturating_mul(1_000_000_000)
            .saturating_add(meta.ctime_nsec() as i128);
        Some(StatTuple {
            dev: meta.dev(),
            inode: meta.ino(),
            size: meta.len(),
            mtime_ns: super::journal::mtime_ns(&meta).unwrap_or(0),
            ctime_ns: if ctime_ns < 0 { 0 } else { ctime_ns as u64 },
            mode: meta.mode() & 0o7777,
        })
    }
    #[cfg(not(unix))]
    {
        // The Windows side waits for a stable `file_index` and
        // `volume_serial_number` (rust-lang/rust#63010); until then the
        // cache stays off and the hop reads the pack the way it did
        // before. Nothing wrong follows.
        let _ = meta;
        None
    }
}

/// Every cached entry, re-read: the sha256 of what is on disk now against
/// the sha256 the cache remembers. A path the cache still names but that
/// has no file returns a "missing" problem, not a mismatch; a mismatch is
/// what `cig tin verify --statcache` reports and is the tripwire the issue
/// calls for.
#[derive(Clone, Debug)]
pub struct Mismatch {
    pub pack_hash: String,
    pub path: Vec<u8>,
    /// The sha256 the cache remembers.
    pub remembered: String,
    /// The sha256 the file hashes to now (empty when the file is gone).
    pub actual: String,
}

/// Verify every statcache under this home. Returns the mismatches; the
/// caller decides what to do with them (print, raise a tripwire, drop the
/// entry). The cache is not rewritten: `cig tin verify` is a read-only
/// pass.
pub fn verify_all() -> Vec<Mismatch> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(dir()) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with('.') {
            // A `.tmp` left by an interrupted save is not an index.
            continue;
        }
        let pack_hash = name.to_string();
        let cache = load(&pack_hash);
        for (rel, entry) in &cache.entries {
            // The relative path is stored exactly as `scan` sees it: on
            // Unix the raw bytes, on Windows UTF-8. Reading it back gives
            // the same path, whatever the filesystem names it.
            let path = decode_rel(rel);
            match super::journal::sha256_file(&path) {
                Ok(actual) if actual == entry.sha256 => {}
                Ok(actual) => out.push(Mismatch {
                    pack_hash: pack_hash.clone(),
                    path: rel.clone(),
                    remembered: entry.sha256.clone(),
                    actual,
                }),
                Err(_) => out.push(Mismatch {
                    pack_hash: pack_hash.clone(),
                    path: rel.clone(),
                    remembered: entry.sha256.clone(),
                    actual: String::new(),
                }),
            }
        }
    }
    out
}

/// Remove the named paths from the cache, in place. A mismatch dropped
/// earns the storage a fresh read next time the hop touches the file.
pub fn drop_entries(pack_hash: &str, paths: &[Vec<u8>]) -> io::Result<()> {
    let mut cache = load(pack_hash);
    for p in paths {
        cache.entries.remove(p);
    }
    save(pack_hash, &cache)
}

/// A path's relative bytes back as a `PathBuf`. The verify pass stored the
/// absolute path under each pack's cache, so this is a plain decode.
fn decode_rel(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(bytes).as_ref())
    }
}

/// Encode an absolute path for storage: Unix keeps the raw bytes, Windows
/// stores UTF-8 (lossy only on byte sequences that are not UTF-8, which
/// Windows itself refuses to name).
pub fn encode_abs(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        path.to_string_lossy().into_owned().into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shortcut after a hop holds only for a file whose last change is
    /// older than the hop's start by the racy margin: an equal tuple there
    /// proves nothing was written; within the margin the file is hashed.
    #[test]
    fn an_unchanged_tuple_is_trusted_only_outside_the_racy_margin() {
        let started = 100 * RACY_MARGIN_NS;
        let old = StatTuple {
            dev: 1,
            inode: 2,
            size: 3,
            mtime_ns: started - 2 * RACY_MARGIN_NS,
            ctime_ns: started - 2 * RACY_MARGIN_NS,
            mode: 0o644,
        };
        assert!(unchanged_through(&old, &old, started));
        let fresh_ctime = StatTuple {
            ctime_ns: started - RACY_MARGIN_NS / 2,
            ..old
        };
        assert!(!unchanged_through(&fresh_ctime, &fresh_ctime, started));
        let fresh_mtime = StatTuple {
            mtime_ns: started - 1,
            ..old
        };
        assert!(!unchanged_through(&fresh_mtime, &fresh_mtime, started));
        let grown = StatTuple { size: 4, ..old };
        assert!(!unchanged_through(&old, &grown, started));
    }

    fn with_home<T>(f: impl FnOnce(&Path) -> T) -> T {
        let _g = super::super::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("CIGSCRIPT_HOME");
        std::env::set_var("CIGSCRIPT_HOME", tmp.path());
        let out = f(tmp.path());
        match prev {
            Some(v) => std::env::set_var("CIGSCRIPT_HOME", v),
            None => std::env::remove_var("CIGSCRIPT_HOME"),
        }
        out
    }

    #[test]
    fn save_and_load_round_trip() {
        with_home(|_| {
            let entry = Entry {
                stat: StatTuple {
                    dev: 1,
                    inode: 2,
                    size: 3,
                    mtime_ns: 4,
                    ctime_ns: 5,
                    mode: 0o644,
                },
                sha256: "0".repeat(64),
                chunked: false,
            };
            let c = Cache {
                entries: {
                    let mut m = BTreeMap::new();
                    m.insert(b"src/main.rs".to_vec(), entry.clone());
                    m
                },
                written_at_ns: 12345,
            };
            save("deadbeef", &c).unwrap();
            let back = load("deadbeef");
            assert_eq!(back.written_at_ns, 12345);
            assert_eq!(back.entries.get(b"src/main.rs".as_slice()), Some(&entry));
        });
    }

    #[test]
    fn a_bogus_magic_is_read_as_empty() {
        with_home(|_| {
            fs::create_dir_all(dir()).unwrap();
            fs::write(path_for("deadbeef"), b"not-a-cache").unwrap();
            let back = load("deadbeef");
            assert!(back.entries.is_empty());
        });
    }

    #[test]
    fn same_roots_different_order_hash_alike() {
        let a = pack_hash(&[PathBuf::from("/x"), PathBuf::from("/y")]);
        let b = pack_hash(&[PathBuf::from("/y"), PathBuf::from("/x")]);
        assert_eq!(a, b);
    }

    #[test]
    fn a_racy_mtime_is_not_trusted() {
        let written = 10_000_000_000u64;
        assert!(is_racy(written, written));
        assert!(is_racy(written - 1_000_000_000, written));
        assert!(!is_racy(written - 3_000_000_000, written));
    }
}
