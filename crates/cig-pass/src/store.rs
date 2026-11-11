// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The pass store (cig-script#186): opaque encoded passes with the little
//! metadata the store needs, found by (kind, subject) or by id.
//!
//! ```text
//! <dir>/young.log        append-only, many writers, torn tail tolerated
//! <dir>/seg-NNNNNN.seg   immutable, temp + rename, salted hash indexes inside
//! <dir>/hits.log         use hints for eviction, appended without the lock
//! <dir>/store.lock       flock for insert, remove and compaction
//! ```
//!
//! The store never looks inside an encoded pass. A pass is whatever bytes
//! the caller hands in; the caller verifies what `candidates` returns and
//! takes the first that holds. A damaged file only loses an optimization:
//! corrupt = empty, never a panic, never a wrong answer (every record is
//! checked when it is read, and the pass is MAC'd anyway).
//!
//! Concurrency: writers (insert, remove, compact) hold the store lock.
//! Readers never take it. Segments are immutable and the young log is only
//! ever appended to or replaced whole by rename, so a reader sees either
//! the old state or the new one; a reader that lists the folder in the
//! middle of a compaction may see a pass twice, never zero times.
//!
//! Reads are `pread`s (no `mmap`, so a truncated file is an error, not a
//! `SIGBUS`). Every length and offset read from a file is checked against
//! the file's length before anything is allocated.
//!
//! Hash flooding: a segment's indexes are keyed with a random 32-byte salt
//! kept in its header, loaded to at most one half, with probe chains capped
//! at 16. A chain that would run longer means the segment is rebuilt with a
//! new salt; a lookup never follows more than 16 slots.
//!
//! The young log is indexed lazily on a cold handle: one read of the file
//! and a walk of the record lengths, with no hashing. A record's check is
//! verified when the record is read (and for every tombstone, and during
//! compaction), so a flipped byte costs one candidate, not a wrong answer.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

/// The young log is due for compaction past this many records.
pub const YOUNG_COMPACT_RECORDS: usize = 1024;
/// Passes kept per (kind, subject) when records are merged.
pub const KEEP_PER_KEY: usize = 4;
/// The most candidates one lookup returns.
pub const MAX_CANDIDATES: usize = 8;
/// A record's body is at most 4 MiB (the young log's rule); a pass
/// is at most this many bytes less the record's own fields.
pub const MAX_BODY: usize = 4 * 1024 * 1024;
/// The longest encoded pass the store takes (`MAX_BODY` less the record's
/// fixed fields at their longest).
pub const MAX_ENCODED: usize = MAX_BODY - 336;
/// Kinds that never count against the budget and are never evicted by a
/// flood of other kinds; they have a quota of their own. `rules` is here
/// because a list held by one must not lose its pass to a burst of
/// verdicts.
pub const EXEMPT_KINDS: [&str; 4] = ["approval", "human-yes", "revoked", "rules"];
/// The default size of the exempt kinds' own quota.
pub const EXEMPT_QUOTA: u64 = 4 * 1024 * 1024;

const MAX_KIND: usize = 255;
const FRAME_HEAD: usize = 12;
const YOUNG_MAGIC: &[u8; 8] = b"CIGPYNG1";
const YOUNG_HEAD: u64 = 16;
const SEG_MAGIC: &[u8; 8] = b"CIGPSEG2";
const SEG_HEAD: usize = 128;
const SLOT: usize = 24;
/// Longest probe chain a segment index may hold or a lookup will follow.
const PROBE_MAX: usize = 16;
const PROBE_WINDOW: usize = 8;
/// Segments of one size tier merge when this many collect.
const FANOUT: usize = 4;
const TIER_BASE: u64 = 256 * 1024;
const MAX_SEG_BYTES: u64 = 1 << 30;
const MAX_GROUP_BYTES: u32 = (4 * (MAX_BODY + FRAME_HEAD) + 64) as u32;
const MAX_GROUP_COUNT: u32 = 4096;
const MAX_SEGS: usize = 256;
const MAX_SLOTS: u32 = 1 << 28;
const MAX_HITS_READ: u64 = 16 * 1024 * 1024;
const HITS_REWRITE_AT: u64 = 1024 * 1024;
const NIL: u32 = u32::MAX;

/// What the store keeps beside each encoded pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Meta {
    /// The pass id (BLAKE3 over its base and caveats).
    pub id: [u8; 32],
    /// The pass kind; 1 to 255 bytes of UTF-8.
    pub kind: String,
    /// The subject hash.
    pub subject: [u8; 32],
    /// Unix milliseconds the pass was issued.
    pub issued_ms: i64,
    /// Length of the encoded pass; `insert` checks it.
    pub size: u32,
}

/// What a compaction did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompactReport {
    /// Records read from the young log.
    pub young_records: usize,
    /// Segments merged away (their records now live in a new one).
    pub segments_merged: usize,
    /// Segments written.
    pub segments_written: usize,
    /// Passes in the segments written.
    pub kept: usize,
    /// Dropped because `cheap_valid` refused them.
    pub dropped_invalid: usize,
    /// Dropped as duplicates, removed ids, or beyond the newest four.
    pub dropped_extra: usize,
    /// Dropped to fit the budget or the exempt quota, least recently used
    /// first within a kind.
    pub evicted: usize,
    /// The evictions by kind (kind, count), for `stats`.
    pub evicted_by_kind: Vec<(String, usize)>,
    /// Bytes the store's segments take after.
    pub bytes_after: u64,
}

/// The #176 checks, supplied by the caller (cig: `seal::ensure_keys_dir`
/// style). The store asks once, at `open`.
pub trait DirGuard {
    /// Make `dir` exist as a private folder (0700 when created) and refuse
    /// it when it is not ours, not 0700, or is a link.
    fn ensure_dir(&self, dir: &Path) -> io::Result<()>;
}

/// A guard that only creates the folder 0700. For tests; cig passes its own.
pub struct NoGuard;

impl DirGuard for NoGuard {
    fn ensure_dir(&self, dir: &Path) -> io::Result<()> {
        let mut b = fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700);
        }
        b.create(dir)
    }
}

// ---------------------------------------------------------------- files

/// Who owns the private folder; files in it must be ours alike.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(unix), allow(dead_code))]
pub struct Owner(u32);

/// Read the folder's owner after the guard has passed it.
pub fn owner_of(dir: &Path) -> io::Result<Owner> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(Owner(fs::metadata(dir)?.uid()))
    }
    #[cfg(not(unix))]
    {
        fs::metadata(dir)?;
        Ok(Owner(0))
    }
}

/// Open a file in the private folder without following a link, then check
/// the open descriptor (not the path): a regular file, owned like the
/// folder. New files are 0600.
#[cfg_attr(not(unix), allow(unused_mut))]
pub fn open_private(path: &Path, mut opts: OpenOptions, owner: Owner) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        opts.mode(0o600);
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        let f = opts.open(path)?;
        let md = f.metadata()?;
        if !md.file_type().is_file() || md.uid() != owner.0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "E721 burn: {} is not a plain file of ours. Move it aside and run again",
                    path.display()
                ),
            ));
        }
        Ok(f)
    }
    #[cfg(not(unix))]
    {
        let _ = owner;
        opts.open(path)
    }
}

fn read_exact_at(f: &File, buf: &mut [u8], off: u64) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        f.read_exact_at(buf, off)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut done = 0;
        while done < buf.len() {
            let n = f.seek_read(&mut buf[done..], off + done as u64)?;
            if n == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            done += n;
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (f, buf, off);
        Err(io::ErrorKind::Unsupported.into())
    }
}

/// A blocking exclusive advisory lock on `path` (the way `runs_index.rs`
/// takes one). Released when the returned file is dropped.
fn exclusive_lock(path: &Path, owner: Owner) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.create(true).truncate(false).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let f = open_private(path, o, owner)?;
        loop {
            // SAFETY: flock on a descriptor this handle owns.
            let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) };
            if rc == 0 {
                return Ok(f);
            }
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let _ = owner;
        // No blocking flock here: an exclusive open, spun on a sharing
        // violation, as `runs_index.rs` does.
        o.share_mode(0);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match o.open(path) {
                Ok(f) => return Ok(f),
                Err(e) if e.raw_os_error() == Some(32) => {
                    if std::time::Instant::now() >= deadline {
                        return Err(e);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(e) => return Err(e),
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = owner;
        o.open(path)
    }
}

fn random32() -> io::Result<[u8; 32]> {
    let mut b = [0u8; 32];
    getrandom::getrandom(&mut b).map_err(|e| io::Error::other(format!("no random bytes: {e}")))?;
    Ok(b)
}

fn hex16() -> io::Result<String> {
    let r = random32()?;
    Ok(r[..16].iter().map(|b| format!("{b:02x}")).collect())
}

/// Make a rename durable: fsync the folder it happened in.
fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(dir)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

/// Write `bytes` beside `dest` and rename it into place: `create_new`, 0600,
/// no link followed, an unpredictable temp name, fsync of the file and then
/// of the folder. A crash leaves the old file or the new one, never half.
pub fn write_atomic(dest: &Path, bytes: &[u8], owner: Owner) -> io::Result<()> {
    let dir = dest.parent().unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(format!("tmp-{}-{}.tmp", std::process::id(), hex16()?));
    let mut o = OpenOptions::new();
    o.write(true).create_new(true);
    let mut f = open_private(&tmp, o, owner)?;
    let r = f
        .write_all(bytes)
        .and_then(|_| f.sync_all())
        .and_then(|_| fs::rename(&tmp, dest));
    if r.is_err() {
        let _ = fs::remove_file(&tmp);
        return r;
    }
    sync_dir(dir)
}

fn nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn is_soft(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof | io::ErrorKind::NotFound
    )
}

// --------------------------------------------------------------- frames

fn arr32(b: &[u8]) -> [u8; 32] {
    let mut a = [0u8; 32];
    a.copy_from_slice(&b[..32]);
    a
}

fn le_u32(b: &[u8]) -> u32 {
    let mut a = [0u8; 4];
    a.copy_from_slice(&b[..4]);
    u32::from_le_bytes(a)
}

fn le_u64(b: &[u8]) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[..8]);
    u64::from_le_bytes(a)
}

fn fnv(b: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for x in b {
        h ^= *x as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// A segment's index key for (kind, subject): keyed by the segment's salt.
fn group_key(salt: &[u8; 32], kind: &str, subject: &[u8; 32]) -> [u8; 32] {
    let mut h = blake3::Hasher::new_keyed(salt);
    h.update(kind.as_bytes());
    h.update(&[0]);
    h.update(subject);
    *h.finalize().as_bytes()
}

/// A segment's id-index key.
fn id_key(salt: &[u8; 32], id: &[u8; 32]) -> [u8; 32] {
    *blake3::keyed_hash(salt, id).as_bytes()
}

fn frame_check(len: &[u8], body: &[u8]) -> [u8; 8] {
    let mut h = blake3::Hasher::new();
    h.update(len);
    h.update(body);
    let d = h.finalize();
    let mut c = [0u8; 8];
    c.copy_from_slice(&d.as_bytes()[..8]);
    c
}

/// `u32 len || [8] check || body`, the check over the length and body.
fn push_frame(out: &mut Vec<u8>, body: &[u8]) {
    let len = (body.len() as u32).to_le_bytes();
    out.extend_from_slice(&len);
    out.extend_from_slice(&frame_check(&len, body));
    out.extend_from_slice(body);
}

fn pass_body(meta: &Meta, enc: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(1 + 32 + 1 + meta.kind.len() + 32 + 8 + 4 + enc.len());
    b.push(0);
    b.extend_from_slice(&meta.id);
    b.push(meta.kind.len() as u8);
    b.extend_from_slice(meta.kind.as_bytes());
    b.extend_from_slice(&meta.subject);
    b.extend_from_slice(&meta.issued_ms.to_le_bytes());
    b.extend_from_slice(&(enc.len() as u32).to_le_bytes());
    b.extend_from_slice(enc);
    b
}

fn tomb_body(id: &[u8; 32]) -> Vec<u8> {
    let mut b = Vec::with_capacity(33);
    b.push(1);
    b.extend_from_slice(id);
    b
}

fn hit_body(id: &[u8; 32], ms: i64) -> Vec<u8> {
    let mut b = Vec::with_capacity(41);
    b.push(2);
    b.extend_from_slice(id);
    b.extend_from_slice(&ms.to_le_bytes());
    b
}

fn body_len(kind_len: usize, enc_len: usize) -> usize {
    1 + 32 + 1 + kind_len + 32 + 8 + 4 + enc_len
}

enum Body<'a> {
    Pass(Meta, &'a [u8]),
    Tomb([u8; 32]),
    Hit([u8; 32], i64),
}

fn parse_body(body: &[u8]) -> Option<Body<'_>> {
    let (&t, rest) = body.split_first()?;
    match t {
        1 if rest.len() == 32 => Some(Body::Tomb(arr32(rest))),
        2 if rest.len() == 40 => Some(Body::Hit(arr32(rest), le_u64(&rest[32..]) as i64)),
        0 => {
            if rest.len() < 33 {
                return None;
            }
            let kl = rest[32] as usize;
            let need = 33 + kl + 32 + 8 + 4;
            if kl == 0 || rest.len() < need {
                return None;
            }
            let kind = std::str::from_utf8(&rest[33..33 + kl]).ok()?.to_string();
            let mut p = 33 + kl;
            let subject = arr32(&rest[p..]);
            p += 32;
            let issued_ms = le_u64(&rest[p..]) as i64;
            p += 8;
            let size = le_u32(&rest[p..]);
            p += 4;
            let enc = &rest[p..];
            if enc.len() != size as usize {
                return None;
            }
            Some(Body::Pass(
                Meta {
                    id: arr32(rest),
                    kind,
                    subject,
                    issued_ms,
                    size,
                },
                enc,
            ))
        }
        _ => None,
    }
}

enum FrameAt {
    /// Not enough bytes for the frame its header promises.
    Incomplete,
    /// A header that cannot be a frame, or a check that fails.
    Bad,
    /// Body start and frame end.
    Good(usize, usize),
}

/// The record at `p`: its length is checked against `MAX_BODY` and against
/// the bytes that are there before anything is read.
fn frame_at(buf: &[u8], p: usize) -> FrameAt {
    let rest = buf.len() - p;
    if rest < FRAME_HEAD {
        return FrameAt::Incomplete;
    }
    let len = le_u32(&buf[p..]) as usize;
    if len > MAX_BODY {
        return FrameAt::Bad;
    }
    if rest < FRAME_HEAD + len {
        return FrameAt::Incomplete;
    }
    let body = &buf[p + FRAME_HEAD..p + FRAME_HEAD + len];
    if frame_check(&buf[p..p + 4], body) == buf[p + 4..p + 12] {
        FrameAt::Good(p + FRAME_HEAD, p + FRAME_HEAD + len)
    } else {
        FrameAt::Bad
    }
}

/// Walk the frames in `buf`, calling `f(frame start, body)` for each good
/// one. A bad frame is skipped by scanning forward for the next that holds.
/// Returns how many bytes were consumed: all of them, or up to a trailing
/// frame that stops short.
fn scan_frames(buf: &[u8], mut f: impl FnMut(usize, &[u8])) -> usize {
    let mut p = 0;
    while p < buf.len() {
        match frame_at(buf, p) {
            FrameAt::Good(bs, end) => {
                f(p, &buf[bs..end]);
                p = end;
            }
            FrameAt::Incomplete => return p,
            FrameAt::Bad => {
                let mut o = p + 1;
                let mut found = None;
                while o < buf.len() {
                    if let FrameAt::Good(..) = frame_at(buf, o) {
                        found = Some(o);
                        break;
                    }
                    o += 1;
                }
                match found {
                    Some(o) => p = o,
                    None => return buf.len(),
                }
            }
        }
    }
    p
}

/// One pass with its bytes, in memory (compaction and `list`).
struct Rec {
    meta: Meta,
    enc: Vec<u8>,
}

fn collect_recs(buf: &[u8], recs: &mut Vec<Rec>, tombs: &mut HashSet<[u8; 32]>) -> usize {
    scan_frames(buf, |_, body| match parse_body(body) {
        Some(Body::Pass(meta, enc)) => {
            tombs.remove(&meta.id);
            recs.push(Rec {
                meta,
                enc: enc.to_vec(),
            });
        }
        Some(Body::Tomb(id)) => {
            tombs.insert(id);
        }
        _ => {}
    })
}

fn sort_recs(recs: &mut [Rec]) {
    recs.sort_by(|a, b| {
        a.meta
            .kind
            .cmp(&b.meta.kind)
            .then(a.meta.subject.cmp(&b.meta.subject))
            .then(b.meta.issued_ms.cmp(&a.meta.issued_ms))
            .then(a.meta.id.cmp(&b.meta.id))
    });
}

fn frame_size(r: &Rec) -> u64 {
    (FRAME_HEAD + body_len(r.meta.kind.len(), r.enc.len())) as u64
}

fn is_exempt(kind: &str) -> bool {
    EXEMPT_KINDS.contains(&kind)
}

// -------------------------------------------------------------- segment

/// A segment file:
///
/// ```text
/// header 128: "CIGPSEG2" || version u32 (2) || slots u32 || probe u32 ||
///   count u32 || groups u32 || id_slots u32 || group_index_off u64 ||
///   id_index_off u64 || data_off u64 || data_len u64 || salt [32] ||
///   zeros || [8] check over the first 120 bytes
/// group index: (slots + 16) slots of 24: tag u64 || off u64 || len u32 || count u32
/// id index:    (id_slots + 16) slots of 24: tag u64 || off u64 || len u32 || 1
/// data: records, sorted by (kind, subject, issued desc)
/// ```
struct Seg {
    seq: u32,
    path: PathBuf,
    file: File,
    len: u64,
    salt: [u8; 32],
    slots: u32,
    id_slots: u32,
    index_off: u64,
    id_off: u64,
    data_off: u64,
    data_len: u64,
}

fn bad(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.to_string())
}

fn table_bytes(slots: u32) -> u64 {
    (slots as u64 + PROBE_MAX as u64) * SLOT as u64
}

impl Seg {
    fn open(path: &Path, seq: u32, owner: Owner) -> io::Result<Seg> {
        let mut o = OpenOptions::new();
        o.read(true);
        let file = open_private(path, o, owner)?;
        let len = file.metadata()?.len();
        if len < SEG_HEAD as u64 {
            return Err(bad("segment too short"));
        }
        let mut h = [0u8; SEG_HEAD];
        read_exact_at(&file, &mut h, 0)?;
        if &h[..8] != SEG_MAGIC || le_u32(&h[8..]) != 2 {
            return Err(bad("segment magic"));
        }
        let mut c = [0u8; 8];
        c.copy_from_slice(&blake3::hash(&h[..120]).as_bytes()[..8]);
        if c != h[120..128] {
            return Err(bad("segment header check"));
        }
        let slots = le_u32(&h[12..]);
        let probe = le_u32(&h[16..]) as usize;
        let count = le_u32(&h[20..]) as u64;
        let id_slots = le_u32(&h[28..]);
        let index_off = le_u64(&h[32..]);
        let id_off = le_u64(&h[40..]);
        let data_off = le_u64(&h[48..]);
        let data_len = le_u64(&h[56..]);
        let ok_slots = |s: u32| s != 0 && s.is_power_of_two() && s <= MAX_SLOTS;
        if !ok_slots(slots)
            || !ok_slots(id_slots)
            || probe != PROBE_MAX
            || index_off != SEG_HEAD as u64
            || data_len > MAX_SEG_BYTES
            || count > data_len / FRAME_HEAD as u64
        {
            return Err(bad("segment header values"));
        }
        if id_off != index_off + table_bytes(slots)
            || data_off != id_off + table_bytes(id_slots)
            || data_off.checked_add(data_len) != Some(len)
        {
            return Err(bad("segment lengths"));
        }
        Ok(Seg {
            seq,
            path: path.to_path_buf(),
            file,
            len,
            salt: arr32(&h[64..]),
            slots,
            id_slots,
            index_off,
            id_off,
            data_off,
            data_len,
        })
    }

    /// The slots of one table that carry `tag`, following a chain of at
    /// most 16 and stopping at the first empty slot: `(off, len, count)`.
    fn probe(
        &self,
        table_off: u64,
        slots: u32,
        key: &[u8; 32],
    ) -> io::Result<Vec<(u64, u32, u32)>> {
        let tag = le_u64(key);
        let pos = (le_u64(&key[8..]) & (slots as u64 - 1)) as usize;
        let mut out = Vec::new();
        let mut i = 0;
        'probe: while i < PROBE_MAX {
            let n = PROBE_WINDOW.min(PROBE_MAX - i);
            let mut buf = [0u8; PROBE_WINDOW * SLOT];
            let at = table_off + ((pos + i) * SLOT) as u64;
            read_exact_at(&self.file, &mut buf[..n * SLOT], at)?;
            for j in 0..n {
                let s = &buf[j * SLOT..(j + 1) * SLOT];
                let count = le_u32(&s[20..]);
                if count == 0 {
                    break 'probe;
                }
                if le_u64(s) == tag {
                    out.push((le_u64(&s[8..]), le_u32(&s[16..]), count));
                }
            }
            i += n;
        }
        Ok(out)
    }

    fn in_data(&self, off: u64, len: u32) -> bool {
        off >= self.data_off
            && off
                .checked_add(len as u64)
                .is_some_and(|e| e <= self.data_off + self.data_len)
    }

    /// Candidates for one (kind, subject): two reads on a hit, one on a miss.
    fn lookup(&self, kind: &str, subject: &[u8; 32], out: &mut Vec<Cand>) -> io::Result<()> {
        let key = group_key(&self.salt, kind, subject);
        for (off, glen, count) in self.probe(self.index_off, self.slots, &key)? {
            if !self.in_data(off, glen) || glen > MAX_GROUP_BYTES || count > MAX_GROUP_COUNT {
                continue;
            }
            let mut buf = vec![0u8; glen as usize];
            read_exact_at(&self.file, &mut buf, off)?;
            scan_frames(&buf, |_, body| {
                if let Some(Body::Pass(m, enc)) = parse_body(body) {
                    if m.kind == kind && m.subject == *subject {
                        out.push(Cand {
                            issued: m.issued_ms,
                            id: m.id,
                            enc: enc.to_vec(),
                        });
                    }
                }
            });
        }
        Ok(())
    }

    /// The pass with this id, if the segment holds it.
    fn find_id(&self, id: &[u8; 32]) -> io::Result<Option<Vec<u8>>> {
        let key = id_key(&self.salt, id);
        for (off, flen, _) in self.probe(self.id_off, self.id_slots, &key)? {
            if !self.in_data(off, flen) || flen as usize > MAX_BODY + FRAME_HEAD {
                continue;
            }
            let buf = match read_frame(&self.file, off, flen - FRAME_HEAD as u32) {
                Ok(b) => b,
                Err(e) if is_soft(&e) => continue,
                Err(e) => return Err(e),
            };
            if let Some(Body::Pass(m, enc)) = parse_body(&buf[FRAME_HEAD..]) {
                if m.id == *id {
                    return Ok(Some(enc.to_vec()));
                }
            }
        }
        Ok(None)
    }

    fn scan_all(&self) -> io::Result<Vec<Rec>> {
        let mut buf = vec![0u8; self.data_len as usize];
        read_exact_at(&self.file, &mut buf, self.data_off)?;
        let mut recs = Vec::new();
        let mut tombs = HashSet::new();
        collect_recs(&buf, &mut recs, &mut tombs);
        Ok(recs)
    }
}

fn place(t: &mut [u8], key: &[u8; 32], slots: u32, off: u64, len: u32, count: u32) -> bool {
    let pos = (le_u64(&key[8..]) & (slots as u64 - 1)) as usize;
    for i in 0..PROBE_MAX {
        let s = &mut t[(pos + i) * SLOT..(pos + i + 1) * SLOT];
        if le_u32(&s[20..]) == 0 {
            s[..8].copy_from_slice(&key[..8]);
            s[8..16].copy_from_slice(&off.to_le_bytes());
            s[16..20].copy_from_slice(&len.to_le_bytes());
            s[20..24].copy_from_slice(&count.to_le_bytes());
            return true;
        }
    }
    false
}

fn pow2_slots(n: usize) -> u32 {
    (n * 2).next_power_of_two().max(16) as u32
}

/// The bytes of a segment for `recs`, sorted by (kind, subject, issued desc).
/// The salt is fresh; a chain over 16 means another salt, and only after
/// several the tables grow.
fn build_segment(recs: &[Rec]) -> io::Result<Vec<u8>> {
    struct G {
        first: usize,
        off: u64,
        len: u32,
        count: u32,
    }
    let mut groups: Vec<G> = Vec::new();
    let mut offs: Vec<(u64, u32)> = Vec::with_capacity(recs.len());
    let mut rel = 0u64;
    for (i, r) in recs.iter().enumerate() {
        let fs = frame_size(r);
        let same = i > 0
            && recs[i - 1].meta.kind == r.meta.kind
            && recs[i - 1].meta.subject == r.meta.subject;
        if same {
            if let Some(g) = groups.last_mut() {
                g.len += fs as u32;
                g.count += 1;
            }
        } else {
            groups.push(G {
                first: i,
                off: rel,
                len: fs as u32,
                count: 1,
            });
        }
        offs.push((rel, fs as u32));
        rel += fs;
    }
    let mut slots = pow2_slots(groups.len());
    let mut id_slots = pow2_slots(recs.len());
    let mut attempts = 0u32;
    let (salt, gt, it, data_off) = loop {
        let salt = random32()?;
        let data_off = SEG_HEAD as u64 + table_bytes(slots) + table_bytes(id_slots);
        let mut gt = vec![0u8; table_bytes(slots) as usize];
        let mut it = vec![0u8; table_bytes(id_slots) as usize];
        let mut ok = true;
        for g in &groups {
            let r = &recs[g.first];
            let key = group_key(&salt, &r.meta.kind, &r.meta.subject);
            if !place(&mut gt, &key, slots, data_off + g.off, g.len, g.count) {
                ok = false;
                break;
            }
        }
        if ok {
            for (r, (off, len)) in recs.iter().zip(&offs) {
                if !place(
                    &mut it,
                    &id_key(&salt, &r.meta.id),
                    id_slots,
                    data_off + off,
                    *len,
                    1,
                ) {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            break (salt, gt, it, data_off);
        }
        attempts += 1;
        if attempts % 4 == 0 {
            slots = slots
                .checked_mul(2)
                .filter(|s| *s <= MAX_SLOTS)
                .ok_or_else(|| bad("index"))?;
            id_slots = id_slots
                .checked_mul(2)
                .filter(|s| *s <= MAX_SLOTS)
                .ok_or_else(|| bad("index"))?;
        }
        if attempts > 64 {
            return Err(bad("segment index cannot be placed"));
        }
    };
    let mut head = [0u8; SEG_HEAD];
    head[..8].copy_from_slice(SEG_MAGIC);
    head[8..12].copy_from_slice(&2u32.to_le_bytes());
    head[12..16].copy_from_slice(&slots.to_le_bytes());
    head[16..20].copy_from_slice(&(PROBE_MAX as u32).to_le_bytes());
    head[20..24].copy_from_slice(&(recs.len() as u32).to_le_bytes());
    head[24..28].copy_from_slice(&(groups.len() as u32).to_le_bytes());
    head[28..32].copy_from_slice(&id_slots.to_le_bytes());
    head[32..40].copy_from_slice(&(SEG_HEAD as u64).to_le_bytes());
    head[40..48].copy_from_slice(&(SEG_HEAD as u64 + gt.len() as u64).to_le_bytes());
    head[48..56].copy_from_slice(&data_off.to_le_bytes());
    head[56..64].copy_from_slice(&rel.to_le_bytes());
    head[64..96].copy_from_slice(&salt);
    let c = blake3::hash(&head[..120]);
    head[120..128].copy_from_slice(&c.as_bytes()[..8]);
    let mut out = Vec::with_capacity(data_off as usize + rel as usize);
    out.extend_from_slice(&head);
    out.extend_from_slice(&gt);
    out.extend_from_slice(&it);
    for r in recs {
        push_frame(&mut out, &pass_body(&r.meta, &r.enc));
    }
    Ok(out)
}

fn seg_name(seq: u32) -> String {
    format!("seg-{seq:06}.seg")
}

/// Segment files in `dir`, newest (highest number) first.
fn list_seg_files(dir: &Path) -> io::Result<Vec<(u32, PathBuf)>> {
    let mut v = Vec::new();
    for e in fs::read_dir(dir)? {
        let e = e?;
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(num) = name
            .strip_prefix("seg-")
            .and_then(|n| n.strip_suffix(".seg"))
        else {
            continue;
        };
        if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        if let Ok(seq) = num.parse::<u32>() {
            v.push((seq, e.path()));
        }
    }
    v.sort_by_key(|a| std::cmp::Reverse(a.0));
    Ok(v)
}

// ---------------------------------------------------------------- cache

struct Cand {
    issued: i64,
    id: [u8; 32],
    enc: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Fp {
    ino: u64,
    len: u64,
    mtime: Option<u128>,
}

fn fp_of(md: &fs::Metadata) -> Fp {
    #[cfg(unix)]
    let ino = {
        use std::os::unix::fs::MetadataExt;
        md.ino()
    };
    #[cfg(not(unix))]
    let ino = 0;
    Fp {
        ino,
        len: md.len(),
        mtime: md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_nanos()),
    }
}

/// One young-log record as the lazy scan sees it: where it is and what it
/// claims to be. Nothing here is trusted until the record is read back.
struct YEntry {
    tag: u64,
    kindh: u64,
    id: [u8; 32],
    issued: i64,
    off: u64,
    len: u32,
    next: u32,
}

/// What this handle knows of the young log and the segments. Rebuilt when
/// the log is replaced, extended when it grows.
struct Cache {
    inited: bool,
    fp: Option<Fp>,
    gen: u64,
    consumed: u64,
    file: Option<File>,
    entries: Vec<YEntry>,
    heads: HashMap<u64, u32>,
    ids: Option<HashMap<[u8; 32], u32>>,
    tombs: HashSet<[u8; 32]>,
    records: usize,
    segs: Vec<Seg>,
    segs_stale: bool,
}

impl Cache {
    fn new() -> Cache {
        Cache {
            inited: false,
            fp: None,
            gen: 0,
            consumed: YOUNG_HEAD,
            file: None,
            entries: Vec::new(),
            heads: HashMap::new(),
            ids: None,
            tombs: HashSet::new(),
            records: 0,
            segs: Vec::new(),
            segs_stale: false,
        }
    }

    fn clear_young(&mut self) {
        self.fp = None;
        self.gen = 0;
        self.consumed = YOUNG_HEAD;
        self.file = None;
        self.entries.clear();
        self.heads.clear();
        self.ids = None;
        self.tombs.clear();
        self.records = 0;
    }

    fn add_entry(&mut self, e: YEntry) {
        let tag = e.tag;
        let id = e.id;
        let at = self.entries.len() as u32;
        let mut e = e;
        e.next = self.heads.insert(tag, at).unwrap_or(NIL);
        self.entries.push(e);
        if let Some(ids) = self.ids.as_mut() {
            ids.insert(id, at);
        }
        self.tombs.remove(&id);
    }

    /// A frame whose check has been verified.
    fn add_verified(&mut self, off: u64, body: &[u8]) {
        self.records += 1;
        match parse_body(body) {
            Some(Body::Pass(m, _)) => self.add_entry(YEntry {
                tag: le_u64(&m.subject),
                kindh: fnv(m.kind.as_bytes()),
                id: m.id,
                issued: m.issued_ms,
                off,
                len: body.len() as u32,
                next: NIL,
            }),
            Some(Body::Tomb(id)) => {
                self.tombs.insert(id);
            }
            _ => {}
        }
    }

    fn ensure_ids(&mut self) {
        if self.ids.is_none() {
            let mut m = HashMap::with_capacity(self.entries.len());
            for (i, e) in self.entries.iter().enumerate() {
                m.insert(e.id, i as u32);
            }
            self.ids = Some(m);
        }
    }

    /// The lazy scan: walk the record lengths and read each pass record's
    /// own fields, with no hashing of its bytes. A tombstone is always
    /// verified, and a record that does not look right hands the rest of
    /// the buffer to the verifying scan. Returns the bytes consumed.
    fn scan_lazy(&mut self, buf: &[u8], base: u64) -> usize {
        let mut p = 0usize;
        while p < buf.len() {
            let rest = buf.len() - p;
            if rest < FRAME_HEAD {
                return p;
            }
            let len = le_u32(&buf[p..]) as usize;
            if len <= MAX_BODY && rest < FRAME_HEAD + len {
                return p;
            }
            if len <= MAX_BODY {
                let body = &buf[p + FRAME_HEAD..p + FRAME_HEAD + len];
                match body.first() {
                    Some(0) => {
                        if let Some(e) = peek_pass(body, base + p as u64) {
                            self.records += 1;
                            self.add_entry(e);
                            p += FRAME_HEAD + len;
                            continue;
                        }
                    }
                    Some(1) if len == 33 => {
                        if frame_check(&buf[p..p + 4], body) == buf[p + 4..p + 12] {
                            self.add_verified(base + p as u64, body);
                            p += FRAME_HEAD + len;
                            continue;
                        }
                    }
                    Some(2) if len == 41 => {
                        // A use hint in the wrong file: skip it whole.
                        p += FRAME_HEAD + len;
                        continue;
                    }
                    _ => {}
                }
            }
            let adv = scan_frames(&buf[p..], |off, body| {
                self.add_verified(base + (p + off) as u64, body)
            });
            return p + adv;
        }
        p
    }
}

/// A pass record's claims, read off its fixed fields; `None` when the
/// lengths do not add up.
fn peek_pass(body: &[u8], off: u64) -> Option<YEntry> {
    if body.len() < 34 {
        return None;
    }
    let kl = body[33] as usize;
    let p = 34 + kl;
    let need = p + 44;
    if kl == 0 || body.len() < need || (body.len() - need) as u64 != le_u32(&body[p + 40..]) as u64
    {
        return None;
    }
    Some(YEntry {
        tag: le_u64(&body[p..]),
        kindh: fnv(&body[34..34 + kl]),
        id: arr32(&body[1..]),
        issued: le_u64(&body[p + 32..]) as i64,
        off,
        len: body.len() as u32,
        next: NIL,
    })
}

fn read_frame(file: &File, off: u64, body_len: u32) -> io::Result<Vec<u8>> {
    if body_len as usize > MAX_BODY {
        return Err(bad("record too long"));
    }
    let total = FRAME_HEAD + body_len as usize;
    let mut buf = vec![0u8; total];
    read_exact_at(file, &mut buf, off)?;
    match frame_at(&buf, 0) {
        FrameAt::Good(_, end) if end == total => Ok(buf),
        _ => Err(bad("frame check")),
    }
}

fn new_gen(prev: u64) -> u64 {
    nanos().max(prev.saturating_add(1))
}

fn young_header(gen: u64) -> [u8; 16] {
    let mut h = [0u8; 16];
    h[..8].copy_from_slice(YOUNG_MAGIC);
    h[8..].copy_from_slice(&gen.to_le_bytes());
    h
}

// ---------------------------------------------------------------- store

/// A handle on the store folder. Cheap to open; many handles, in one
/// process or many, may insert and read at once.
pub struct Store {
    dir: PathBuf,
    owner: Owner,
    exempt_quota: u64,
    cache: Mutex<Cache>,
    touched: Mutex<(HashSet<[u8; 32]>, Option<File>)>,
}

impl Store {
    /// Open (creating if need be) the store at `dir`. `guard` is asked once
    /// to make the folder private and to refuse one that is not.
    pub fn open(dir: &Path, guard: &dyn DirGuard) -> io::Result<Store> {
        guard.ensure_dir(dir)?;
        let owner = owner_of(dir)?;
        Ok(Store {
            dir: dir.to_path_buf(),
            owner,
            exempt_quota: EXEMPT_QUOTA,
            cache: Mutex::new(Cache::new()),
            touched: Mutex::new((HashSet::new(), None)),
        })
    }

    /// Set the exempt kinds' own quota (default 4 MiB).
    pub fn with_exempt_quota(mut self, bytes: u64) -> Store {
        self.exempt_quota = bytes;
        self
    }

    fn young_path(&self) -> PathBuf {
        self.dir.join("young.log")
    }

    fn hits_path(&self) -> PathBuf {
        self.dir.join("hits.log")
    }

    fn cache(&self) -> MutexGuard<'_, Cache> {
        match self.cache.lock() {
            Ok(g) => g,
            Err(p) => {
                // A panic left it half-updated: start it over.
                let mut g = p.into_inner();
                *g = Cache::new();
                g
            }
        }
    }

    fn lock(&self) -> io::Result<File> {
        exclusive_lock(&self.dir.join("store.lock"), self.owner)
    }

    /// Bring the cache level with the folder: one `stat` when nothing
    /// changed.
    fn refresh(&self, c: &mut Cache) -> io::Result<()> {
        let md = match fs::metadata(self.young_path()) {
            Ok(m) => Some(m),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        let fp = md.as_ref().map(fp_of);
        if c.inited && fp == c.fp && !c.segs_stale {
            return Ok(());
        }
        let mut relist = !c.inited || c.segs_stale;
        if !c.inited || fp != c.fp {
            if md.is_none() {
                c.clear_young();
                relist = true;
            } else if self.load_young(c)? {
                relist = true;
            }
        }
        if relist {
            self.load_segs(c)?;
        }
        c.inited = true;
        Ok(())
    }

    /// Read the young log into the cache. `true` when it was replaced since
    /// the cache last looked (the segments may have changed with it).
    fn load_young(&self, c: &mut Cache) -> io::Result<bool> {
        let mut o = OpenOptions::new();
        o.read(true);
        let file = match open_private(&self.young_path(), o, self.owner) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                c.clear_young();
                return Ok(true);
            }
            Err(e) => return Err(e),
        };
        let md = file.metadata()?;
        let fp = fp_of(&md);
        let len = md.len();
        let mut head = [0u8; 16];
        let ok = len >= YOUNG_HEAD
            && read_exact_at(&file, &mut head, 0).is_ok()
            && &head[..8] == YOUNG_MAGIC;
        if !ok {
            // Corrupt = empty. The next writer rewrites the header.
            c.clear_young();
            c.fp = Some(fp);
            c.consumed = len.max(YOUNG_HEAD);
            return Ok(true);
        }
        let gen = le_u64(&head[8..]);
        let prev_ino = c.fp.map(|f| f.ino);
        let reset =
            gen != c.gen || prev_ino != Some(fp.ino) || len < c.consumed || c.file.is_none();
        if reset {
            c.clear_young();
            c.gen = gen;
        }
        if len > c.consumed {
            let base = c.consumed;
            let mut buf = vec![0u8; (len - base) as usize];
            match read_exact_at(&file, &mut buf, base) {
                Ok(()) => {}
                Err(e) if is_soft(&e) => {
                    // Shortened under us (a writer healing a tail): look
                    // again next time.
                    c.fp = None;
                    return Ok(false);
                }
                Err(e) => return Err(e),
            }
            let adv = c.scan_lazy(&buf, base);
            c.consumed = base + adv as u64;
        }
        c.file = Some(file);
        c.fp = Some(fp);
        Ok(reset)
    }

    fn load_segs(&self, c: &mut Cache) -> io::Result<()> {
        for _ in 0..3 {
            let mut segs = Vec::new();
            let mut raced = false;
            for (seq, path) in list_seg_files(&self.dir)?.into_iter().take(MAX_SEGS) {
                match Seg::open(&path, seq, self.owner) {
                    Ok(s) => segs.push(s),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => raced = true,
                    Err(e) if is_soft(&e) => {}
                    Err(e) => return Err(e),
                }
            }
            c.segs = segs;
            c.segs_stale = false;
            if !raced {
                return Ok(());
            }
        }
        // Still racing a compaction: look again on the next call.
        c.segs_stale = true;
        Ok(())
    }

    /// Append passes to the young log under the store lock.
    pub fn insert(&self, meta: &Meta, encoded: &[u8]) -> io::Result<()> {
        self.insert_many(&[(meta, encoded)])
    }

    /// Append several passes under one lock: one write, one wait.
    pub fn insert_many(&self, items: &[(&Meta, &[u8])]) -> io::Result<()> {
        let mut buf = Vec::new();
        for (meta, enc) in items {
            if meta.kind.is_empty()
                || meta.kind.len() > MAX_KIND
                || enc.len() > MAX_ENCODED
                || meta.size as usize != enc.len()
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "pass store: a pass with a bad kind or size",
                ));
            }
            push_frame(&mut buf, &pass_body(meta, enc));
        }
        self.append(&buf)
    }

    fn append(&self, frames: &[u8]) -> io::Result<()> {
        if frames.is_empty() {
            return Ok(());
        }
        let _lock = self.lock()?;
        let mut c = self.cache();
        let mut f = self.prepare_log(&mut c)?;
        let before = f.metadata()?.len();
        if let Err(e) = f.write_all(frames) {
            // Do not leave half a record for the next writer to cut.
            let _ = f.set_len(before);
            return Err(e);
        }
        Ok(())
    }

    /// Under the lock: the young log open for append, with a good header
    /// and no torn tail.
    fn prepare_log(&self, c: &mut Cache) -> io::Result<File> {
        let path = self.young_path();
        // Read and write, not append: Windows lets a handle opened only to
        // append neither cut a torn tail nor take back a half-written
        // record (no write-data right). The store lock makes writers take
        // turns, so seeking to the end before each write is an append.
        let mut o = OpenOptions::new();
        o.read(true).write(true).create(true).truncate(false);
        let mut f = open_private(&path, o, self.owner)?;
        let len = f.metadata()?.len();
        let mut head = [0u8; 16];
        let ok = len >= YOUNG_HEAD
            && read_exact_at(&f, &mut head, 0).is_ok()
            && &head[..8] == YOUNG_MAGIC;
        if !ok {
            f.set_len(0)?;
            f.seek(SeekFrom::Start(0))?;
            f.write_all(&young_header(new_gen(c.gen)))?;
            c.fp = None;
        }
        self.refresh(c)?;
        let len = f.metadata()?.len();
        if c.consumed < len {
            f.set_len(c.consumed)?;
        }
        f.seek(SeekFrom::End(0))?;
        Ok(f)
    }

    /// The passes for (kind, subject), newest issued first, at most eight.
    /// The caller verifies each and takes the first that holds. This never
    /// writes; call `touch` with the id of the pass that was used.
    pub fn candidates(&self, kind: &str, subject: &[u8; 32]) -> io::Result<Vec<Vec<u8>>> {
        if kind.is_empty() || kind.len() > MAX_KIND {
            return Ok(Vec::new());
        }
        let mut guard = self.cache();
        self.refresh(&mut guard)?;
        let c = &*guard;
        let mut found: Vec<Cand> = Vec::new();
        if let (Some(&head), Some(file)) = (c.heads.get(&le_u64(subject)), c.file.as_ref()) {
            let kindh = fnv(kind.as_bytes());
            let mut chain: Vec<&YEntry> = Vec::new();
            let mut at = head;
            while at != NIL {
                let e = &c.entries[at as usize];
                if e.kindh == kindh && !c.tombs.contains(&e.id) {
                    chain.push(e);
                }
                at = e.next;
            }
            // Newest issued first; the chain runs newest written first, and
            // the sort is stable, so ties keep that order.
            chain.sort_by_key(|e| std::cmp::Reverse(e.issued));
            for e in chain.into_iter().take(MAX_CANDIDATES) {
                let Ok(buf) = read_frame(file, e.off, e.len) else {
                    continue;
                };
                if let Some(Body::Pass(m, enc)) = parse_body(&buf[FRAME_HEAD..]) {
                    if m.kind == kind && m.subject == *subject {
                        found.push(Cand {
                            issued: m.issued_ms,
                            id: m.id,
                            enc: enc.to_vec(),
                        });
                    }
                }
            }
        }
        for s in &c.segs {
            match s.lookup(kind, subject, &mut found) {
                Ok(()) => {}
                Err(e) if is_soft(&e) => {}
                Err(e) => return Err(e),
            }
        }
        found.retain(|f| !c.tombs.contains(&f.id));
        found.sort_by_key(|a| std::cmp::Reverse(a.issued));
        let mut seen: Vec<[u8; 32]> = Vec::new();
        let mut out = Vec::new();
        for f in found {
            if seen.contains(&f.id) {
                continue;
            }
            seen.push(f.id);
            out.push(f.enc);
            if out.len() >= MAX_CANDIDATES {
                break;
            }
        }
        Ok(out)
    }

    fn lookup_id(&self, c: &mut Cache, id: &[u8; 32]) -> io::Result<Option<Vec<u8>>> {
        if c.tombs.contains(id) {
            return Ok(None);
        }
        c.ensure_ids();
        let at = c.ids.as_ref().and_then(|m| m.get(id)).copied();
        if let (Some(at), Some(file)) = (at, c.file.as_ref()) {
            let e = &c.entries[at as usize];
            if let Ok(buf) = read_frame(file, e.off, e.len) {
                if let Some(Body::Pass(m, enc)) = parse_body(&buf[FRAME_HEAD..]) {
                    if m.id == *id {
                        return Ok(Some(enc.to_vec()));
                    }
                }
            }
        }
        for s in &c.segs {
            match s.find_id(id) {
                Ok(Some(enc)) => return Ok(Some(enc)),
                Ok(None) => {}
                Err(e) if is_soft(&e) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(None)
    }

    /// The pass with this id, for a nested `Pass` input.
    pub fn find_by_id(&self, id: &[u8; 32]) -> io::Result<Option<Vec<u8>>> {
        let mut guard = self.cache();
        self.refresh(&mut guard)?;
        self.lookup_id(&mut guard, id)
    }

    /// Note that the pass `id` was used, for least-recently-used eviction.
    /// At most one small append per id per process, with no lock and no
    /// read of anything; failures are ignored (it is only a hint).
    pub fn touch(&self, id: &[u8; 32]) {
        let mut g = match self.touched.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if !g.0.insert(*id) {
            return;
        }
        if g.1.is_none() {
            let mut o = OpenOptions::new();
            o.create(true).append(true);
            g.1 = open_private(&self.hits_path(), o, self.owner).ok();
        }
        if let Some(f) = g.1.as_mut() {
            let mut b = Vec::with_capacity(53);
            push_frame(&mut b, &hit_body(id, now_ms()));
            let _ = f.write_all(&b);
        }
    }

    /// id -> the last time it was used, from the hit log.
    fn fold_hits(&self) -> HashMap<[u8; 32], i64> {
        let mut m: HashMap<[u8; 32], i64> = HashMap::new();
        let mut o = OpenOptions::new();
        o.read(true);
        let Ok(f) = open_private(&self.hits_path(), o, self.owner) else {
            return m;
        };
        let Ok(md) = f.metadata() else { return m };
        let start = md.len().saturating_sub(MAX_HITS_READ);
        let mut buf = vec![0u8; (md.len() - start) as usize];
        if read_exact_at(&f, &mut buf, start).is_err() {
            return m;
        }
        scan_frames(&buf, |_, body| {
            if let Some(Body::Hit(id, ms)) = parse_body(body) {
                let e = m.entry(id).or_insert(ms);
                *e = (*e).max(ms);
            }
        });
        m
    }

    fn young_recs(c: &Cache) -> io::Result<Vec<Rec>> {
        let Some(file) = c.file.as_ref() else {
            return Ok(Vec::new());
        };
        if c.consumed <= YOUNG_HEAD {
            return Ok(Vec::new());
        }
        let mut buf = vec![0u8; (c.consumed - YOUNG_HEAD) as usize];
        read_exact_at(file, &mut buf, YOUNG_HEAD)?;
        let mut recs = Vec::new();
        let mut tombs = HashSet::new();
        collect_recs(&buf, &mut recs, &mut tombs);
        recs.retain(|r| !c.tombs.contains(&r.meta.id));
        Ok(recs)
    }

    /// Every live pass (of one kind, or all), newest issued first.
    pub fn list(&self, kind: Option<&str>) -> io::Result<Vec<(Meta, Vec<u8>)>> {
        let mut guard = self.cache();
        self.refresh(&mut guard)?;
        let c = &*guard;
        let mut recs =
            Self::young_recs(c).or_else(|e| if is_soft(&e) { Ok(Vec::new()) } else { Err(e) })?;
        for s in &c.segs {
            match s.scan_all() {
                Ok(r) => recs.extend(r),
                Err(e) if is_soft(&e) => {}
                Err(e) => return Err(e),
            }
        }
        recs.retain(|r| !c.tombs.contains(&r.meta.id) && kind.is_none_or(|k| r.meta.kind == k));
        recs.sort_by_key(|a| std::cmp::Reverse(a.meta.issued_ms));
        let mut seen = HashSet::new();
        recs.retain(|r| seen.insert(r.meta.id));
        Ok(recs.into_iter().map(|r| (r.meta, r.enc)).collect())
    }

    /// Remove a pass: a tombstone in the young log, applied for good at
    /// compaction. `false` when no live pass has that id.
    pub fn remove(&self, id: &[u8; 32]) -> io::Result<bool> {
        let _lock = self.lock()?;
        let mut c = self.cache();
        let mut f = self.prepare_log(&mut c)?;
        if self.lookup_id(&mut c, id)?.is_none() {
            return Ok(false);
        }
        let mut buf = Vec::new();
        push_frame(&mut buf, &tomb_body(id));
        let before = f.metadata()?.len();
        if let Err(e) = f.write_all(&buf) {
            let _ = f.set_len(before);
            return Err(e);
        }
        Ok(true)
    }

    /// Records in the young log (passes and tombstones).
    pub fn young_records(&self) -> io::Result<usize> {
        let mut c = self.cache();
        self.refresh(&mut c)?;
        Ok(c.records)
    }

    /// The young log has passed 1,024 records.
    pub fn should_compact(&self) -> io::Result<bool> {
        Ok(self.young_records()? > YOUNG_COMPACT_RECORDS)
    }

    /// Bytes the store's files take.
    pub fn disk_bytes(&self) -> io::Result<u64> {
        let mut n = fs::metadata(self.young_path())
            .map(|m| m.len())
            .unwrap_or(0);
        for (_, p) in list_seg_files(&self.dir)? {
            n += fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        }
        Ok(n)
    }

    /// The routine compaction: the young log becomes a segment, segments of
    /// one size tier merge, passes `cheap_valid` refuses are dropped from
    /// what is rewritten, and only the newest four per (kind, subject)
    /// stay. When the store is over `budget_bytes` (0 = no budget) it
    /// becomes a full compaction. The budget covers every kind but
    /// `EXEMPT_KINDS`, which have their own quota.
    pub fn compact(
        &self,
        budget_bytes: u64,
        cheap_valid: &dyn Fn(&[u8]) -> bool,
    ) -> io::Result<CompactReport> {
        self.compact_impl(budget_bytes, cheap_valid, false)
    }

    /// `cig pass gc`: rewrite everything into one segment, applying every
    /// check, every tombstone, the budget and the exempt quota.
    pub fn compact_full(
        &self,
        budget_bytes: u64,
        cheap_valid: &dyn Fn(&[u8]) -> bool,
    ) -> io::Result<CompactReport> {
        self.compact_impl(budget_bytes, cheap_valid, true)
    }

    fn compact_impl(
        &self,
        budget: u64,
        valid: &dyn Fn(&[u8]) -> bool,
        full_asked: bool,
    ) -> io::Result<CompactReport> {
        struct Group {
            young: bool,
            segs: Vec<usize>,
            size: u64,
        }
        let _lock = self.lock()?;
        let mut guard = self.cache();
        let c = &mut *guard;
        // Under the lock nobody appends: read the folder as it is now.
        *c = Cache::new();
        // Every tmp file is a crashed writer's: all writers hold the lock.
        if let Ok(rd) = fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                let n = e.file_name();
                let n = n.to_string_lossy();
                if n.starts_with("tmp-") && n.ends_with(".tmp") {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
        self.refresh(c)?;
        let mut report = CompactReport::default();

        let mut young =
            Self::young_recs(c).or_else(|e| if is_soft(&e) { Ok(Vec::new()) } else { Err(e) })?;
        report.young_records = young.len();
        let carry_in: Vec<[u8; 32]> = c.tombs.iter().copied().collect();

        // Files that do not open as segments are cache debris: they go.
        let mut debris: Vec<PathBuf> = Vec::new();
        let mut max_seq = 0u32;
        for (seq, path) in list_seg_files(&self.dir)? {
            max_seq = max_seq.max(seq);
            if !c.segs.iter().any(|s| s.seq == seq) {
                debris.push(path);
            }
        }
        let young_size: u64 = young.iter().map(frame_size).sum();
        let total_est: u64 = young_size + c.segs.iter().map(|s| s.len).sum::<u64>();
        let full = full_asked || (budget > 0 && total_est > budget);

        let mut groups: Vec<Group> = Vec::new();
        if full {
            groups.push(Group {
                young: true,
                segs: (0..c.segs.len()).collect(),
                size: total_est,
            });
        } else {
            if !young.is_empty() {
                groups.push(Group {
                    young: true,
                    segs: Vec::new(),
                    size: young_size,
                });
            }
            for (i, s) in c.segs.iter().enumerate() {
                groups.push(Group {
                    young: false,
                    segs: vec![i],
                    size: s.len,
                });
            }
            loop {
                let mut by_tier: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
                for (i, g) in groups.iter().enumerate() {
                    by_tier.entry(tier(g.size)).or_default().push(i);
                }
                let Some((_, idx)) = by_tier.into_iter().find(|(_, v)| v.len() >= FANOUT) else {
                    break;
                };
                let mut merged = Group {
                    young: false,
                    segs: Vec::new(),
                    size: 0,
                };
                for i in idx.iter().rev() {
                    let g = groups.remove(*i);
                    merged.young |= g.young;
                    merged.segs.extend(g.segs);
                    merged.size += g.size;
                }
                groups.push(merged);
            }
        }

        let hits = if full {
            self.fold_hits()
        } else {
            HashMap::new()
        };
        let mut next = max_seq + 1;
        let mut to_delete: Vec<PathBuf> = debris;
        let mut wrote: Vec<(PathBuf, Vec<u8>)> = Vec::new();
        let mut kept_ids: HashSet<[u8; 32]> = HashSet::new();
        let seg_tombs = c.tombs.clone();
        let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
        for g in &groups {
            let rewrite = g.young || g.segs.len() >= 2 || full;
            if !rewrite {
                continue;
            }
            let mut recs: Vec<Rec> = Vec::new();
            if g.young {
                recs.append(&mut young);
            }
            for &i in &g.segs {
                match c.segs[i].scan_all() {
                    Ok(r) => recs.extend(r),
                    Err(e) if is_soft(&e) => {}
                    Err(e) => return Err(e),
                }
                to_delete.push(c.segs[i].path.clone());
                report.segments_merged += 1;
            }
            let before = recs.len();
            recs.retain(|r| !seg_tombs.contains(&r.meta.id));
            let mut seen = HashSet::new();
            recs.retain(|r| seen.insert(r.meta.id));
            let n_unique = recs.len();
            recs.retain(|r| valid(&r.enc));
            let n_valid = recs.len();
            report.dropped_invalid += n_unique - n_valid;
            sort_recs(&mut recs);
            // Newest four per (kind, subject): the sort puts them first.
            let mut run = 0usize;
            let mut kept: Vec<Rec> = Vec::with_capacity(recs.len());
            for r in recs {
                let same = kept.last().is_some_and(|l| {
                    l.meta.kind == r.meta.kind && l.meta.subject == r.meta.subject
                });
                run = if same { run + 1 } else { 1 };
                if run <= KEEP_PER_KEY {
                    kept.push(r);
                }
            }
            let mut recs = kept;
            report.dropped_extra += (before - n_unique) + (n_valid - recs.len());
            if full {
                let dropped = evict(&recs, budget, self.exempt_quota, &hits);
                if !dropped.is_empty() {
                    for &i in &dropped {
                        *by_kind.entry(recs[i].meta.kind.clone()).or_default() += 1;
                    }
                    report.evicted += dropped.len();
                    let mut idx = 0usize;
                    recs.retain(|_| {
                        let keep = !dropped.contains(&idx);
                        idx += 1;
                        keep
                    });
                }
            }
            if recs.is_empty() {
                continue;
            }
            report.kept += recs.len();
            kept_ids.extend(recs.iter().map(|r| r.meta.id));
            let bytes = build_segment(&recs)?;
            wrote.push((self.dir.join(seg_name(next)), bytes));
            next += 1;
        }
        report.evicted_by_kind = by_kind.into_iter().collect();
        let changed = !wrote.is_empty() || !to_delete.is_empty() || report.young_records > 0;
        if !changed && !(full && !carry_in.is_empty()) {
            report.bytes_after = self.disk_bytes()?;
            return Ok(report);
        }
        // New segments first, then the old ones go, then the log is
        // replaced: a reader sees a pass twice at worst, never not at all.
        for (path, bytes) in &wrote {
            write_atomic(path, bytes, self.owner)?;
            report.segments_written += 1;
        }
        let mut delete_failed = false;
        for p in &to_delete {
            if fs::remove_file(p).is_err() && p.exists() {
                delete_failed = true;
            }
        }
        // Tombstones can only be dropped when every segment that could hold
        // their target was rewritten and removed.
        let carry: Vec<[u8; 32]> = if full && !delete_failed {
            Vec::new()
        } else {
            carry_in
        };
        let mut log = young_header(new_gen(c.gen)).to_vec();
        for id in &carry {
            push_frame(&mut log, &tomb_body(id));
        }
        write_atomic(&self.young_path(), &log, self.owner)?;
        self.tidy_hits(full, &kept_ids)?;
        *c = Cache::new();
        self.refresh(c)?;
        report.bytes_after = c.segs.iter().map(|s| s.len).sum();
        Ok(report)
    }

    /// Keep the hit log small: after a full compaction only the ids that
    /// survived, otherwise (when it has grown) the last time per id.
    fn tidy_hits(&self, full: bool, kept: &HashSet<[u8; 32]>) -> io::Result<()> {
        let len = fs::metadata(self.hits_path()).map(|m| m.len()).unwrap_or(0);
        if len == 0 || (!full && len < HITS_REWRITE_AT) {
            return Ok(());
        }
        let folded = self.fold_hits();
        let mut ids: Vec<_> = folded
            .into_iter()
            .filter(|(id, _)| !full || kept.contains(id))
            .collect();
        ids.sort();
        let mut b = Vec::new();
        for (id, ms) in ids {
            push_frame(&mut b, &hit_body(&id, ms));
        }
        write_atomic(&self.hits_path(), &b, self.owner)?;
        // Our own append handle points at the file just replaced.
        if let Ok(mut t) = self.touched.lock() {
            t.1 = None;
        }
        Ok(())
    }
}

/// The records to drop, by index. The exempt kinds are held to their own
/// quota; the rest to the budget (0 = none), the segment's own index bytes
/// counted against it. Within each set the kind holding the most bytes
/// gives up its least recently used pass first (last use, else issued), so
/// a flood of one kind cannot push another out.
fn evict(
    recs: &[Rec],
    budget: u64,
    exempt_quota: u64,
    hits: &HashMap<[u8; 32], i64>,
) -> HashSet<usize> {
    let mut dropped = HashSet::new();
    let last = |i: usize| {
        let m = &recs[i].meta;
        hits.get(&m.id)
            .copied()
            .unwrap_or(i64::MIN)
            .max(m.issued_ms)
    };
    let mut sets: [(Vec<usize>, u64, u64); 2] = [
        (Vec::new(), 0, exempt_quota),
        (Vec::new(), 0, if budget == 0 { u64::MAX } else { budget }),
    ];
    let groups = recs
        .windows(2)
        .filter(|w| w[0].meta.kind != w[1].meta.kind || w[0].meta.subject != w[1].meta.subject)
        .count()
        + usize::from(!recs.is_empty());
    sets[1].1 =
        SEG_HEAD as u64 + table_bytes(pow2_slots(groups)) + table_bytes(pow2_slots(recs.len()));
    for (i, r) in recs.iter().enumerate() {
        let s = usize::from(!is_exempt(&r.meta.kind));
        sets[s].0.push(i);
        sets[s].1 += frame_size(r);
    }
    for (idxs, total, limit) in sets {
        if total <= limit {
            continue;
        }
        let mut total = total;
        // Per kind: indexes, least recently used first, and the bytes held.
        let mut kinds: BTreeMap<&str, (Vec<usize>, u64)> = BTreeMap::new();
        for i in idxs {
            let e = kinds.entry(recs[i].meta.kind.as_str()).or_default();
            e.0.push(i);
            e.1 += frame_size(&recs[i]);
        }
        for (v, _) in kinds.values_mut() {
            v.sort_by(|a, b| {
                last(*b)
                    .cmp(&last(*a))
                    .then(recs[*b].meta.id.cmp(&recs[*a].meta.id))
            });
        }
        while total > limit {
            // The kind with the most bytes (ties: by name); pop its LRU.
            let Some((_, (v, bytes))) = kinds
                .iter_mut()
                .filter(|(_, (v, _))| !v.is_empty())
                .max_by_key(|(k, (_, b))| (*b, std::cmp::Reverse(*k)))
            else {
                break;
            };
            let Some(i) = v.pop() else { break };
            let fsz = frame_size(&recs[i]);
            *bytes -= fsz;
            total -= fsz;
            dropped.insert(i);
        }
    }
    dropped
}

fn tier(size: u64) -> u32 {
    let mut t = 0;
    let mut lim = TIER_BASE;
    while size > lim {
        t += 1;
        lim = lim.saturating_mul(FANOUT as u64);
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn subj(n: u32) -> [u8; 32] {
        *blake3::hash(&n.to_le_bytes()).as_bytes()
    }

    /// A pass: first byte is a tag (0xFF = "cheap check refuses"), then the
    /// issued time, then filler to `len` bytes.
    fn mk(kind: &str, s: u32, issued: i64, tag: u8, len: usize) -> (Meta, Vec<u8>) {
        let mut enc = vec![tag; len.max(9)];
        enc[1..9].copy_from_slice(&issued.to_le_bytes());
        let mut h = blake3::Hasher::new();
        h.update(kind.as_bytes());
        h.update(&subj(s));
        h.update(&issued.to_le_bytes());
        h.update(&[tag]);
        h.update(&(len as u64).to_le_bytes());
        let meta = Meta {
            id: *h.finalize().as_bytes(),
            kind: kind.to_string(),
            subject: subj(s),
            issued_ms: issued,
            size: enc.len() as u32,
        };
        (meta, enc)
    }

    fn issued_of(enc: &[u8]) -> i64 {
        le_u64(&enc[1..9]) as i64
    }

    fn open(d: &Path) -> Store {
        Store::open(d, &NoGuard).unwrap()
    }

    fn all_valid(_: &[u8]) -> bool {
        true
    }

    #[test]
    fn insert_then_find_newest_first() {
        let d = tempfile::tempdir().unwrap();
        let st = open(d.path());
        for t in [30, 10, 20] {
            let (m, e) = mk("verdict", 1, t, 1, 64);
            st.insert(&m, &e).unwrap();
        }
        let (m, e) = mk("verdict", 2, 99, 1, 64);
        st.insert(&m, &e).unwrap();
        let (m, e) = mk("run", 1, 98, 1, 64);
        st.insert(&m, &e).unwrap();
        let got = st.candidates("verdict", &subj(1)).unwrap();
        let times: Vec<i64> = got.iter().map(|e| issued_of(e)).collect();
        assert_eq!(times, vec![30, 20, 10]);
        assert!(st.candidates("verdict", &subj(3)).unwrap().is_empty());
        assert_eq!(st.candidates("run", &subj(1)).unwrap().len(), 1);
        // A second handle sees them too.
        assert_eq!(
            open(d.path())
                .candidates("verdict", &subj(2))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(st.list(Some("verdict")).unwrap().len(), 4);
        assert_eq!(st.list(None).unwrap().len(), 5);
    }

    #[test]
    fn insert_refuses_a_bad_record() {
        let d = tempfile::tempdir().unwrap();
        let st = open(d.path());
        let (mut m, e) = mk("verdict", 1, 1, 1, 64);
        m.size += 1;
        assert!(st.insert(&m, &e).is_err());
        let (mut m, e) = mk("verdict", 1, 1, 1, 64);
        m.kind = String::new();
        assert!(st.insert(&m, &e).is_err());
        m.kind = "k".repeat(256);
        assert!(st.insert(&m, &e).is_err());
        assert!(st.candidates("verdict", &subj(1)).unwrap().is_empty());
    }

    #[test]
    fn torn_young_tail_is_ignored_and_healed() {
        let d = tempfile::tempdir().unwrap();
        let st = open(d.path());
        let (m, e) = mk("verdict", 1, 1, 1, 64);
        st.insert(&m, &e).unwrap();
        // A crashed append: the header promises more than was written.
        let mut tail = Vec::new();
        tail.extend_from_slice(&5000u32.to_le_bytes());
        tail.extend_from_slice(&[7u8; 40]);
        let p = d.path().join("young.log");
        let mut f = OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(&tail).unwrap();
        drop(f);
        let fresh = open(d.path());
        assert_eq!(fresh.candidates("verdict", &subj(1)).unwrap().len(), 1);
        // The next writer cuts the tail before it appends: its record shows.
        let (m2, e2) = mk("verdict", 1, 2, 1, 64);
        fresh.insert(&m2, &e2).unwrap();
        let got = open(d.path()).candidates("verdict", &subj(1)).unwrap();
        assert_eq!(
            got.iter().map(|e| issued_of(e)).collect::<Vec<_>>(),
            vec![2, 1]
        );
        // And a tail of fewer than twelve bytes.
        let mut f = OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(&[1, 2, 3]).unwrap();
        drop(f);
        assert_eq!(
            open(d.path())
                .candidates("verdict", &subj(1))
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn a_bad_record_in_the_middle_is_skipped() {
        let d = tempfile::tempdir().unwrap();
        let st = open(d.path());
        for t in 1..=3 {
            let (m, e) = mk("verdict", 1, t, 1, 100);
            st.insert(&m, &e).unwrap();
        }
        let p = d.path().join("young.log");
        let mut b = fs::read(&p).unwrap();
        let frame = FRAME_HEAD + body_len(7, 100);
        // Flip a byte inside the second record's pass.
        let at = 16 + frame + frame - 10;
        b[at] ^= 0xFF;
        fs::write(&p, &b).unwrap();
        let got = open(d.path()).candidates("verdict", &subj(1)).unwrap();
        assert_eq!(
            got.iter().map(|e| issued_of(e)).collect::<Vec<_>>(),
            vec![3, 1]
        );
        // A record appended after the damage is still found.
        let (m, e) = mk("verdict", 1, 4, 1, 100);
        open(d.path()).insert(&m, &e).unwrap();
        let got = open(d.path()).candidates("verdict", &subj(1)).unwrap();
        assert_eq!(
            got.iter().map(|e| issued_of(e)).collect::<Vec<_>>(),
            vec![4, 3, 1]
        );
    }

    #[test]
    fn a_corrupt_young_header_reads_as_empty_and_recovers() {
        let d = tempfile::tempdir().unwrap();
        fs::write(
            d.path().join("young.log"),
            b"garbage garbage garbage garbage",
        )
        .unwrap();
        let st = open(d.path());
        assert!(st.candidates("verdict", &subj(1)).unwrap().is_empty());
        let (m, e) = mk("verdict", 1, 1, 1, 64);
        st.insert(&m, &e).unwrap();
        assert_eq!(
            open(d.path())
                .candidates("verdict", &subj(1))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn compaction_keeps_newest_four_and_drops_refused() {
        let d = tempfile::tempdir().unwrap();
        let st = open(d.path());
        for t in 1..=7 {
            let (m, e) = mk("verdict", 1, t, 1, 64);
            st.insert(&m, &e).unwrap();
        }
        // Three bad ones, one of them the newest.
        for t in [8, 3, 4] {
            let (m, e) = mk("verdict", 2, t, 0xFF, 64);
            st.insert(&m, &e).unwrap();
        }
        let (m, e) = mk("verdict", 2, 5, 1, 64);
        st.insert(&m, &e).unwrap();
        let r = st.compact(0, &|e: &[u8]| e[0] != 0xFF).unwrap();
        assert_eq!(r.young_records, 11);
        assert_eq!(r.dropped_invalid, 3);
        assert_eq!(r.dropped_extra, 3);
        assert_eq!(r.kept, 5);
        assert_eq!(r.segments_written, 1);
        let fresh = open(d.path());
        let got = fresh.candidates("verdict", &subj(1)).unwrap();
        assert_eq!(
            got.iter().map(|e| issued_of(e)).collect::<Vec<_>>(),
            vec![7, 6, 5, 4]
        );
        let got = fresh.candidates("verdict", &subj(2)).unwrap();
        assert_eq!(
            got.iter().map(|e| issued_of(e)).collect::<Vec<_>>(),
            vec![5]
        );
        assert_eq!(fresh.young_records().unwrap(), 0);
        // The handle that did the work sees the same.
        assert_eq!(st.list(None).unwrap().len(), 5);
        // Compacting again with nothing new changes nothing.
        let r = st.compact(0, &all_valid).unwrap();
        assert_eq!((r.young_records, r.segments_written), (0, 0));
    }

    #[test]
    fn a_handle_open_across_a_compaction_sees_the_merge() {
        let d = tempfile::tempdir().unwrap();
        let reader = open(d.path());
        let writer = open(d.path());
        let (m, e) = mk("verdict", 1, 1, 1, 64);
        writer.insert(&m, &e).unwrap();
        assert_eq!(reader.candidates("verdict", &subj(1)).unwrap().len(), 1);
        writer.compact(0, &all_valid).unwrap();
        assert_eq!(reader.candidates("verdict", &subj(1)).unwrap().len(), 1);
        let (m, e) = mk("verdict", 1, 2, 1, 64);
        writer.insert(&m, &e).unwrap();
        assert_eq!(reader.candidates("verdict", &subj(1)).unwrap().len(), 2);
    }

    #[test]
    fn tombstones_hide_and_are_applied_at_compaction() {
        let d = tempfile::tempdir().unwrap();
        let st = open(d.path());
        let (a, ea) = mk("verdict", 1, 1, 1, 64);
        let (b, eb) = mk("verdict", 1, 2, 1, 64);
        st.insert(&a, &ea).unwrap();
        st.insert(&b, &eb).unwrap();
        assert!(!st.remove(&[9u8; 32]).unwrap());
        assert!(st.remove(&b.id).unwrap());
        assert!(!st.remove(&b.id).unwrap());
        let got = open(d.path()).candidates("verdict", &subj(1)).unwrap();
        assert_eq!(got, vec![ea.clone()]);
        // Into a segment (tombstone carried), then removed from the segment.
        st.compact(0, &all_valid).unwrap();
        assert_eq!(
            st.candidates("verdict", &subj(1)).unwrap(),
            vec![ea.clone()]
        );
        assert!(st.remove(&a.id).unwrap());
        assert!(st.candidates("verdict", &subj(1)).unwrap().is_empty());
        assert!(open(d.path()).list(None).unwrap().is_empty());
        // A full compaction drops the dead for good, tombstones included.
        let r = st.compact_full(0, &all_valid).unwrap();
        assert_eq!(r.kept, 0);
        assert_eq!(st.young_records().unwrap(), 0);
        assert!(st.list(None).unwrap().is_empty());
        assert!(list_seg_files(d.path()).unwrap().is_empty());
        // Removing then re-inserting the same pass brings it back.
        st.insert(&a, &ea).unwrap();
        assert!(st.remove(&a.id).unwrap());
        st.insert(&a, &ea).unwrap();
        assert_eq!(st.candidates("verdict", &subj(1)).unwrap().len(), 1);
    }

    #[test]
    fn budget_evicts_oldest_issued_first() {
        let d = tempfile::tempdir().unwrap();
        let st = open(d.path());
        // Distinct subjects so only the budget can drop them.
        let items: Vec<(Meta, Vec<u8>)> = (0..50u32)
            .map(|i| mk("verdict", i, 1000 + i as i64, 1, 1000))
            .collect();
        let refs: Vec<(&Meta, &[u8])> = items.iter().map(|(m, e)| (m, e.as_slice())).collect();
        st.insert_many(&refs).unwrap();
        let budget = 25_000;
        let r = st.compact(budget, &all_valid).unwrap();
        assert!(r.evicted > 0 && r.evicted < 50, "{r:?}");
        assert!(r.bytes_after <= budget, "{r:?}");
        let left = st.list(None).unwrap();
        assert_eq!(left.len(), 50 - r.evicted);
        // What stayed is the newest run.
        let oldest_kept = left.iter().map(|(m, _)| m.issued_ms).min().unwrap();
        assert_eq!(oldest_kept, 1000 + r.evicted as i64);
        assert!(st.disk_bytes().unwrap() <= budget + 64);
    }

    #[test]
    fn segments_merge_by_tier_and_stay_few() {
        let d = tempfile::tempdir().unwrap();
        let st = open(d.path());
        let mut n = 0u32;
        for round in 0..40 {
            let items: Vec<(Meta, Vec<u8>)> = (0..20)
                .map(|_| {
                    n += 1;
                    mk("verdict", n, n as i64, 1, 200)
                })
                .collect();
            let refs: Vec<(&Meta, &[u8])> = items.iter().map(|(m, e)| (m, e.as_slice())).collect();
            st.insert_many(&refs).unwrap();
            st.compact(0, &all_valid).unwrap();
            let segs = list_seg_files(d.path()).unwrap().len();
            assert!(segs <= 3 * 3, "round {round}: {segs} segments");
        }
        assert_eq!(st.list(None).unwrap().len(), 800);
        for i in (1..=800).step_by(37) {
            assert_eq!(st.candidates("verdict", &subj(i)).unwrap().len(), 1, "{i}");
        }
    }

    #[test]
    fn many_keys_in_one_segment_all_found_and_probes_stay_bounded() {
        let d = tempfile::tempdir().unwrap();
        let st = open(d.path());
        let items: Vec<(Meta, Vec<u8>)> = (0..6000u32)
            .map(|i| mk("verdict", i, i as i64, 1, 20))
            .collect();
        let refs: Vec<(&Meta, &[u8])> = items.iter().map(|(m, e)| (m, e.as_slice())).collect();
        st.insert_many(&refs).unwrap();
        st.compact(0, &all_valid).unwrap();
        let fresh = open(d.path());
        for i in 0..6000u32 {
            assert_eq!(fresh.candidates("verdict", &subj(i)).unwrap().len(), 1);
        }
        assert!(fresh.candidates("verdict", &subj(6001)).unwrap().is_empty());
        assert!(fresh.candidates("run", &subj(5)).unwrap().is_empty());
    }

    #[test]
    fn many_processes_inserting_at_once_lose_nothing() {
        let d = tempfile::tempdir().unwrap();
        let dir = Arc::new(d.path().to_path_buf());
        let mut hs = Vec::new();
        for t in 0..6u32 {
            let dir = Arc::clone(&dir);
            hs.push(std::thread::spawn(move || {
                // Each "process" has its own handle.
                let st = Store::open(&dir, &NoGuard).unwrap();
                for i in 0..150u32 {
                    let (m, e) = mk("verdict", t * 1000 + i, i as i64, 1, 120);
                    st.insert(&m, &e).unwrap();
                    if t == 5 && i % 40 == 0 {
                        // One of them compacts in the middle of it.
                        st.compact(0, &|_: &[u8]| true).unwrap();
                    }
                    // Readers never block and never miss their own.
                    assert_eq!(
                        st.candidates("verdict", &subj(t * 1000 + i)).unwrap().len(),
                        1
                    );
                }
            }));
        }
        for h in hs {
            h.join().unwrap();
        }
        let st = open(&dir);
        assert_eq!(st.list(None).unwrap().len(), 900);
        for t in 0..6u32 {
            for i in 0..150u32 {
                assert_eq!(
                    st.candidates("verdict", &subj(t * 1000 + i)).unwrap().len(),
                    1,
                    "{t} {i}"
                );
            }
        }
    }

    #[test]
    fn a_corrupt_segment_reads_as_empty_and_never_panics() {
        let d = tempfile::tempdir().unwrap();
        let st = open(d.path());
        let items: Vec<(Meta, Vec<u8>)> = (0..200u32)
            .map(|i| mk("verdict", i, i as i64, 1, 50))
            .collect();
        let refs: Vec<(&Meta, &[u8])> = items.iter().map(|(m, e)| (m, e.as_slice())).collect();
        st.insert_many(&refs).unwrap();
        st.compact(0, &all_valid).unwrap();
        let (_, path) = list_seg_files(d.path()).unwrap().remove(0);
        let good = fs::read(&path).unwrap();
        let mut cases: Vec<Vec<u8>> = vec![
            Vec::new(),
            good[..10].to_vec(),
            good[..SEG_HEAD].to_vec(),
            good[..good.len() - 1].to_vec(),
            vec![0xAB; good.len()],
        ];
        for at in [
            0usize,
            9,
            13,
            20,
            33,
            41,
            50,
            58,
            70,
            400,
            2000,
            good.len() - 5,
        ] {
            let mut b = good.clone();
            b[at] ^= 0xFF;
            cases.push(b);
        }
        // Every byte of the index zeroed, and every byte of it 0xFF.
        let index_end = le_u64(&good[40..]) as usize;
        for fill in [0u8, 0xFF] {
            let mut b = good.clone();
            for x in &mut b[SEG_HEAD..index_end] {
                *x = fill;
            }
            cases.push(b);
        }
        for bad_file in cases {
            fs::write(&path, &bad_file).unwrap();
            let h = open(d.path());
            for i in 0..200u32 {
                let r = h.candidates("verdict", &subj(i)).unwrap();
                assert!(r.len() <= 1);
                for e in r {
                    // Whatever comes back is a whole, checked record.
                    assert_eq!(issued_of(&e), i as i64);
                }
            }
            let _ = h.list(None).unwrap();
            // And the store still takes new passes and compacts.
            let (m, e) = mk("verdict", 7777, 1, 1, 40);
            h.insert(&m, &e).unwrap();
            h.compact(0, &all_valid).unwrap();
            assert_eq!(h.candidates("verdict", &subj(7777)).unwrap().len(), 1);
            fs::remove_file(&path).ok();
            for (_, p) in list_seg_files(d.path()).unwrap() {
                fs::remove_file(p).ok();
            }
            fs::write(&path, &good).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_link_in_place_of_the_log_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let other = d.path().join("elsewhere");
        fs::write(&other, b"x").unwrap();
        let dir = d.path().join("passes");
        let st = open(&dir);
        std::os::unix::fs::symlink(&other, dir.join("young.log")).unwrap();
        let (m, e) = mk("verdict", 1, 1, 1, 64);
        assert!(st.insert(&m, &e).is_err());
        assert!(st.candidates("verdict", &subj(1)).is_err());
        assert_eq!(fs::read(&other).unwrap(), b"x");
    }

    #[test]
    fn find_among_100000_passes() {
        let d = tempfile::tempdir().unwrap();
        let st = open(d.path());
        const N: u32 = 100_000;
        let mut i = 0u32;
        while i < N {
            let items: Vec<(Meta, Vec<u8>)> = (i..(i + 5000).min(N))
                .map(|j| mk("verdict", j, j as i64, 1, 300))
                .collect();
            let refs: Vec<(&Meta, &[u8])> = items.iter().map(|(m, e)| (m, e.as_slice())).collect();
            st.insert_many(&refs).unwrap();
            i += 5000;
        }
        let t = std::time::Instant::now();
        let r = st.compact(0, &all_valid).unwrap();
        println!("compact 100000: {:?} {r:?}", t.elapsed());
        assert_eq!(r.kept, N as usize);

        // A cold handle: first call reads the folder.
        let h = open(d.path());
        let t = std::time::Instant::now();
        assert_eq!(h.candidates("verdict", &subj(1)).unwrap().len(), 1);
        println!("cold first lookup: {:?}", t.elapsed());

        // Warm: hits at spread-out keys, then misses.
        let probes: Vec<[u8; 32]> = (0..20_000u32).map(|k| subj((k * 4999) % N)).collect();
        let t = std::time::Instant::now();
        for s in &probes {
            let r = h.candidates("verdict", s).unwrap();
            assert_eq!(r.len(), 1);
        }
        let per_hit = t.elapsed().as_secs_f64() * 1e6 / probes.len() as f64;
        let misses: Vec<[u8; 32]> = (0..20_000u32).map(|k| subj(N + 1 + k)).collect();
        let t = std::time::Instant::now();
        for s in &misses {
            assert!(h.candidates("verdict", s).unwrap().is_empty());
        }
        let per_miss = t.elapsed().as_secs_f64() * 1e6 / misses.len() as f64;
        println!("find in 100000 passes: hit {per_hit:.1} us, miss {per_miss:.1} us (warm)");
        // The target is a release-build number.
        if !cfg!(debug_assertions) {
            assert!(per_hit <= 50.0, "hit took {per_hit:.1} us");
        }

        // With a hundred fresh passes in the young log on top.
        let items: Vec<(Meta, Vec<u8>)> = (0..100u32)
            .map(|j| mk("verdict", j, 1_000_000 + j as i64, 1, 300))
            .collect();
        let refs: Vec<(&Meta, &[u8])> = items.iter().map(|(m, e)| (m, e.as_slice())).collect();
        st.insert_many(&refs).unwrap();
        assert_eq!(h.candidates("verdict", &subj(5)).unwrap().len(), 2);
    }

    #[test]
    fn cold_handle_on_a_full_young_log() {
        let d = tempfile::tempdir().unwrap();
        let st = open(d.path());
        let n = YOUNG_COMPACT_RECORDS as u32;
        let items: Vec<(Meta, Vec<u8>)> =
            (0..n).map(|j| mk("verdict", j, j as i64, 1, 600)).collect();
        let refs: Vec<(&Meta, &[u8])> = items.iter().map(|(m, e)| (m, e.as_slice())).collect();
        st.insert_many(&refs).unwrap();
        assert!(!st.should_compact().unwrap());
        let (m, e) = mk("verdict", 1, 9999, 1, 600);
        st.insert(&m, &e).unwrap();
        assert!(st.should_compact().unwrap());
        // A few cold handles in turn.
        let mut worst = std::time::Duration::ZERO;
        let mut best = std::time::Duration::MAX;
        for _ in 0..5 {
            let h = open(d.path());
            let t = std::time::Instant::now();
            assert_eq!(h.candidates("verdict", &subj(2)).unwrap().len(), 1);
            let took = t.elapsed();
            worst = worst.max(took);
            best = best.min(took);
        }
        println!(
            "cold first candidates(), {} young records: {worst:?}",
            n + 1
        );
        if !cfg!(debug_assertions) {
            // Best of five: the tests run in parallel and a busy core is not the store.
            assert!(best < std::time::Duration::from_millis(1), "{best:?}");
        }
        let h = open(d.path());
        assert_eq!(h.candidates("verdict", &subj(2)).unwrap().len(), 1);
        let t = std::time::Instant::now();
        for j in 0..1000u32 {
            assert!(!h.candidates("verdict", &subj(j)).unwrap().is_empty());
        }
        println!(
            "warm lookup in a full young log: {:.1} us",
            t.elapsed().as_secs_f64() * 1e6 / 1000.0
        );
        // One more insert is picked up incrementally by the warm handle.
        let (m, e) = mk("verdict", 2, 10_000, 1, 600);
        st.insert(&m, &e).unwrap();
        assert_eq!(h.candidates("verdict", &subj(2)).unwrap().len(), 2);
        // A cold scan of a 4x longer log, for the record.
        let more: Vec<(Meta, Vec<u8>)> = (0..3000u32)
            .map(|j| mk("verdict", 100_000 + j, j as i64, 1, 600))
            .collect();
        let refs: Vec<(&Meta, &[u8])> = more.iter().map(|(m, e)| (m, e.as_slice())).collect();
        st.insert_many(&refs).unwrap();
        let h = open(d.path());
        let t = std::time::Instant::now();
        assert_eq!(h.candidates("verdict", &subj(2)).unwrap().len(), 2);
        println!(
            "cold first candidates(), 4000+ young records: {:?}",
            t.elapsed()
        );
    }

    #[test]
    fn find_by_id_in_young_log_segments_and_after_removal() {
        let d = tempfile::tempdir().unwrap();
        let st = open(d.path());
        let items: Vec<(Meta, Vec<u8>)> = (0..300u32)
            .map(|i| mk("check", i, i as i64, 1, 40))
            .collect();
        let refs: Vec<(&Meta, &[u8])> = items.iter().map(|(m, e)| (m, e.as_slice())).collect();
        st.insert_many(&refs[..150]).unwrap();
        st.compact(0, &all_valid).unwrap();
        st.insert_many(&refs[150..]).unwrap();
        let h = open(d.path());
        for (m, e) in &items {
            assert_eq!(h.find_by_id(&m.id).unwrap().as_deref(), Some(e.as_slice()));
        }
        assert_eq!(h.find_by_id(&[7u8; 32]).unwrap(), None);
        assert!(h.remove(&items[10].0.id).unwrap());
        assert!(h.remove(&items[200].0.id).unwrap());
        assert_eq!(h.find_by_id(&items[10].0.id).unwrap(), None);
        assert_eq!(h.find_by_id(&items[200].0.id).unwrap(), None);
        h.compact_full(0, &all_valid).unwrap();
        assert_eq!(h.find_by_id(&items[10].0.id).unwrap(), None);
        assert!(h.find_by_id(&items[11].0.id).unwrap().is_some());
        assert!(open(d.path())
            .find_by_id(&items[299].0.id)
            .unwrap()
            .is_some());
    }

    #[test]
    fn each_segment_has_its_own_salt_and_a_long_chain_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let st = open(d.path());
        let mut salts = Vec::new();
        for round in 0..3u32 {
            let (m, e) = mk("verdict", round, 1, 1, 40);
            st.insert(&m, &e).unwrap();
            st.compact(0, &all_valid).unwrap();
        }
        for (_, p) in list_seg_files(d.path()).unwrap() {
            let b = fs::read(p).unwrap();
            salts.push(b[64..96].to_vec());
        }
        assert_eq!(salts.len(), 3);
        salts.sort();
        salts.dedup();
        assert_eq!(salts.len(), 3);
        // A chain of 16 fills the window; the 17th key at that position
        // does not fit, which is what makes a rebuild with a new salt.
        let mut t = vec![0u8; table_bytes(16) as usize];
        let mut key = [0u8; 32];
        key[8] = 3;
        for i in 0..PROBE_MAX {
            key[0] = i as u8;
            assert!(place(&mut t, &key, 16, 0, 1, 1));
        }
        key[0] = 99;
        assert!(!place(&mut t, &key, 16, 0, 1, 1));
    }

    #[test]
    fn a_probe_never_reads_past_the_cap() {
        // A segment whose index is all full still answers a miss, and in
        // bounded time, because a chain is followed at most 16 slots.
        let d = tempfile::tempdir().unwrap();
        let st = open(d.path());
        let items: Vec<(Meta, Vec<u8>)> = (0..40u32)
            .map(|i| mk("verdict", i, i as i64, 1, 30))
            .collect();
        let refs: Vec<(&Meta, &[u8])> = items.iter().map(|(m, e)| (m, e.as_slice())).collect();
        st.insert_many(&refs).unwrap();
        st.compact(0, &all_valid).unwrap();
        let (_, path) = list_seg_files(d.path()).unwrap().remove(0);
        let mut b = fs::read(&path).unwrap();
        let slots = le_u32(&b[12..]) as usize;
        let end = SEG_HEAD + (slots + PROBE_MAX) * SLOT;
        // Make every slot look occupied with a tag nobody asks for.
        for s in b[SEG_HEAD..end].chunks_mut(SLOT) {
            s[..8].copy_from_slice(&[0xEE; 8]);
            s[20..24].copy_from_slice(&1u32.to_le_bytes());
        }
        fs::write(&path, &b).unwrap();
        let h = open(d.path());
        for i in 0..40u32 {
            assert!(h.candidates("verdict", &subj(i)).unwrap().is_empty());
        }
    }

    #[test]
    fn an_absurd_record_length_is_not_allocated() {
        let d = tempfile::tempdir().unwrap();
        let st = open(d.path());
        let (m, e) = mk("verdict", 1, 1, 1, 64);
        st.insert(&m, &e).unwrap();
        let p = d.path().join("young.log");
        let mut f = OpenOptions::new().append(true).open(&p).unwrap();
        for len in [u32::MAX, (MAX_BODY as u32) + 1, 0] {
            f.write_all(&len.to_le_bytes()).unwrap();
            f.write_all(&[0u8; 40]).unwrap();
        }
        drop(f);
        let h = open(d.path());
        assert_eq!(h.candidates("verdict", &subj(1)).unwrap().len(), 1);
        let (m2, e2) = mk("verdict", 1, 2, 1, 64);
        h.insert(&m2, &e2).unwrap();
        assert_eq!(
            open(d.path())
                .candidates("verdict", &subj(1))
                .unwrap()
                .len(),
            2
        );
        let big = vec![1u8; MAX_ENCODED + 1];
        let (mut m3, _) = mk("verdict", 1, 3, 1, 64);
        m3.size = big.len() as u32;
        assert!(h.insert(&m3, &big).is_err());
        let ok = vec![1u8; MAX_ENCODED];
        m3.size = ok.len() as u32;
        h.insert(&m3, &ok).unwrap();
        assert_eq!(
            h.candidates("verdict", &subj(1)).unwrap()[0].len(),
            MAX_ENCODED
        );
    }

    #[test]
    fn a_flipped_byte_in_a_pass_costs_that_pass_only() {
        let d = tempfile::tempdir().unwrap();
        let st = open(d.path());
        for t in 1..=3 {
            let (m, e) = mk("verdict", 1, t, 1, 100);
            st.insert(&m, &e).unwrap();
        }
        let p = d.path().join("young.log");
        let mut b = fs::read(&p).unwrap();
        let frame = FRAME_HEAD + body_len(7, 100);
        // Inside the third record's pass bytes: its lengths stay right, so
        // the lazy scan indexes it and the check refuses it at read time.
        b[16 + 2 * frame + frame - 5] ^= 1;
        fs::write(&p, &b).unwrap();
        let got = open(d.path()).candidates("verdict", &subj(1)).unwrap();
        assert_eq!(
            got.iter().map(|e| issued_of(e)).collect::<Vec<_>>(),
            vec![2, 1]
        );
    }

    #[test]
    fn a_flood_of_one_kind_cannot_evict_the_exempt_kinds() {
        let d = tempfile::tempdir().unwrap();
        let st = open(d.path());
        let mut items = Vec::new();
        for i in 0..5u32 {
            items.push(mk("approval", i, 1 + i as i64, 1, 1000));
        }
        for i in 0..200u32 {
            items.push(mk("verdict", i, 100 + i as i64, 1, 1000));
        }
        let refs: Vec<(&Meta, &[u8])> = items.iter().map(|(m, e)| (m, e.as_slice())).collect();
        st.insert_many(&refs).unwrap();
        let r = st.compact(30_000, &all_valid).unwrap();
        assert!(r.evicted > 0, "{r:?}");
        assert_eq!(r.evicted_by_kind.len(), 1);
        assert_eq!(r.evicted_by_kind[0].0, "verdict");
        assert_eq!(st.list(Some("approval")).unwrap().len(), 5);
        // Over its own quota, an exempt kind gives up its oldest.
        let st = open(d.path()).with_exempt_quota(2500);
        let r = st.compact_full(30_000, &all_valid).unwrap();
        let ev: Vec<_> = r
            .evicted_by_kind
            .iter()
            .filter(|(k, _)| k == "approval")
            .collect();
        assert_eq!(ev.len(), 1, "{r:?}");
        let left = st.list(Some("approval")).unwrap();
        assert!(left.len() < 5 && !left.is_empty());
        let oldest = left.iter().map(|(m, _)| m.issued_ms).min().unwrap();
        assert!(oldest > 1);
    }

    #[test]
    fn eviction_within_a_kind_is_least_recently_used() {
        let d = tempfile::tempdir().unwrap();
        let st = open(d.path());
        let items: Vec<(Meta, Vec<u8>)> = (0..40u32)
            .map(|i| mk("verdict", i, 10 + i as i64, 1, 1000))
            .collect();
        let refs: Vec<(&Meta, &[u8])> = items.iter().map(|(m, e)| (m, e.as_slice())).collect();
        st.insert_many(&refs).unwrap();
        // The very oldest pass is used just now (in another process).
        let other = open(d.path());
        other.touch(&items[0].0.id);
        other.touch(&items[0].0.id);
        let hits = fs::metadata(d.path().join("hits.log")).unwrap().len();
        assert_eq!(hits as usize, FRAME_HEAD + 41, "once per process per id");
        let r = st.compact_full(20_000, &all_valid).unwrap();
        assert!(r.evicted >= 10, "{r:?}");
        let left = st.list(None).unwrap();
        assert!(
            left.iter().any(|(m, _)| m.id == items[0].0.id),
            "the used one stays"
        );
        assert!(
            !left.iter().any(|(m, _)| m.id == items[1].0.id),
            "an unused old one goes"
        );
        assert!(left.iter().any(|(m, _)| m.id == items[39].0.id));
    }

    #[test]
    fn store_files_are_private_and_temp_names_are_not_guessable() {
        let d = tempfile::tempdir().unwrap();
        let st = open(d.path());
        let (m, e) = mk("verdict", 1, 1, 1, 64);
        st.insert(&m, &e).unwrap();
        st.compact(0, &all_valid).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for e in fs::read_dir(d.path()).unwrap() {
                let e = e.unwrap();
                let mode = e.metadata().unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600, "{:?}", e.file_name());
            }
        }
        let o = owner_of(d.path()).unwrap();
        let a = d.path().join("x");
        // A planted file at a guessable name does not matter, and two
        // writes name their temp files differently.
        write_atomic(&a, b"1", o).unwrap();
        write_atomic(&a, b"2", o).unwrap();
        assert_eq!(fs::read(&a).unwrap(), b"2");
        assert_ne!(hex16().unwrap(), hex16().unwrap());
        let names: Vec<_> = fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert!(!names.iter().any(|n| n.starts_with("tmp-")), "{names:?}");
    }
}
