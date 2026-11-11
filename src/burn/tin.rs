// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The tin: the before-bytes of files a hop may change, kept outside the
//! project under `~/.cigscript/tin/objects/<sha256>`, one object per
//! distinct content. A hop that says what it may `touches` has every
//! existing file under those globs stashed here before it runs, and a file
//! it modified or deleted comes back from the tin on rollback, byte for
//! byte, hash-verified. Objects are shared by content, so a formatter that
//! changes three lines of a file stores that file about once.
//!
//! The tin also keeps what a rollback displaces: a file a hop created, or
//! one it modified, is moved here before the rollback removes or rewrites
//! it, so a save made while the hop ran is never destroyed, only set aside
//! (`cig unburn <id> --give-back` puts it back).

use super::journal::sha256_file;
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

pub fn dir() -> PathBuf {
    super::runs::home_dir().join("tin")
}

fn objects_dir() -> PathBuf {
    dir().join("objects")
}

fn recipes_dir() -> PathBuf {
    dir().join("recipes")
}

/// Where sealed objects live: named by a random id, never shared.
pub fn sealed_dir() -> PathBuf {
    dir().join("sealed")
}

/// The objects' and recipes' roots, for a take-in that moves another
/// tin's into this one.
pub fn objects_root() -> PathBuf {
    objects_dir()
}

pub fn recipes_root() -> PathBuf {
    recipes_dir()
}

/// Is this snapshot path a sealed object in the tin?
pub fn is_sealed(path: &Path) -> bool {
    path.starts_with(sealed_dir())
}

/// Files at least this large are kept as chunks under a recipe rather than
/// as one object, so a small change stores the chunk that changed, not the
/// file again. Smaller files cost exactly what they did before.
pub const CHUNK_FILE_MIN: u64 = 64 * 1024;
/// FastCDC's bounds for one chunk: 16 KiB at least, 64 KiB on average,
/// 256 KiB at most. Boundaries depend on the bytes, not on offsets, so an
/// insert moves one boundary, not every boundary after it.
const CHUNK_MIN: u32 = 16 * 1024;
const CHUNK_AVG: u32 = 64 * 1024;
const CHUNK_MAX: u32 = 256 * 1024;

/// The shape of a recipe file this cig writes.
pub const RECIPE_FORMAT: u32 = 1;

/// A chunked file in the tin: the ordered chunks that make it, each an
/// object named by its own hash, and the hash and size of the whole. Kept
/// under `tin/recipes/<sha256 of the file>`, so the same file stashed
/// twice is one recipe.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recipe {
    pub format: u32,
    pub sha256: String,
    pub bytes: u64,
    pub chunks: Vec<ChunkRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkRef {
    pub sha256: String,
    pub len: u64,
}

pub(crate) fn object_path(hash: &str) -> PathBuf {
    objects_dir().join(&hash[..2]).join(hash)
}

/// Where the object for this hash would be.
pub fn object_path_pub(hash: &str) -> PathBuf {
    object_path(hash)
}

pub(crate) fn recipe_path(hash: &str) -> PathBuf {
    recipes_dir().join(&hash[..2]).join(hash)
}

/// The tin's whole object or recipe for these bytes, when it has one: what
/// a parcel leaves for a file whose bytes git held on the other machine.
pub fn object_for(sha256: &str) -> Option<PathBuf> {
    if sha256.len() < 2 {
        return None;
    }
    let o = object_path(sha256);
    if o.is_file() {
        return Some(o);
    }
    let r = recipe_path(sha256);
    r.is_file().then_some(r)
}

/// Every path in the tin the journal will end up referring to for this
/// sha256: the object alone when it is a whole file in the tin, the
/// recipe and every chunk when it is a chunked one. The stat cache needs
/// this to add a hit's objects to the run's `tin.pending` list, so a
/// `cig tin gc` during the hop keeps them.
pub fn pending_for_hash(sha256: &str) -> Vec<PathBuf> {
    let Some(p) = object_for(sha256) else {
        return Vec::new();
    };
    if is_recipe(&p) {
        let mut out = vec![p.clone()];
        if let Ok(r) = read_recipe(&p) {
            for c in r.chunks {
                out.push(object_path(&c.sha256));
            }
        }
        out
    } else {
        vec![p]
    }
}

/// `pending_for_hash` for an object the caller has already found (a stat
/// cache hit): no second look for it, only the chunks a recipe names.
pub fn pending_for_object(object: &Path) -> Vec<PathBuf> {
    if is_recipe(object) {
        let mut out = vec![object.to_path_buf()];
        if let Ok(r) = read_recipe(object) {
            for c in r.chunks {
                out.push(object_path(&c.sha256));
            }
        }
        out
    } else {
        vec![object.to_path_buf()]
    }
}

/// Is this snapshot path a recipe in the tin (a chunked file)?
pub fn is_recipe(path: &Path) -> bool {
    path.starts_with(recipes_dir())
}

/// Read a recipe and check that it is the one its name says.
pub fn read_recipe(path: &Path) -> io::Result<Recipe> {
    let text = fs::read_to_string(path)?;
    let recipe: Recipe =
        serde_json::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    if recipe.sha256 != name {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "recipe {} is for other bytes than its name says (sha256 {})",
                path.display(),
                &recipe.sha256[..recipe.sha256.len().min(12)]
            ),
        ));
    }
    if recipe.format > RECIPE_FORMAT {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "recipe {} is format {}, written by a newer cig; this one reads up to {RECIPE_FORMAT}",
                path.display(),
                recipe.format
            ),
        ));
    }
    Ok(recipe)
}

/// One path kept in the tin: a file's bytes as an object, with its mode
/// and modification time beside them in the journal; or a symbolic link,
/// kept as its target alone, since a link has no bytes of its own.
#[derive(Clone, Debug)]
pub struct Stashed {
    pub object: PathBuf,
    pub sha256: String,
    pub bytes: u64,
    pub mode: Option<u32>,
    pub mtime_ns: Option<u64>,
    /// The target, when the path is a symbolic link.
    pub link: Option<PathBuf>,
    /// The blob, when git holds the bytes and the tin has no copy.
    pub git: Option<super::journal::GitBlob>,
}

/// A copy of a file waiting beside the objects under temporary names,
/// hashed as it was read: it becomes an object (or, for a large file, a
/// recipe and the chunks the tin did not have yet) when it is committed.
/// The two steps let a run write down which objects it is about to depend
/// on before any of them exists under its final name, so a `cig tin gc` in
/// another terminal never removes one between the two.
#[derive(Debug)]
pub struct Staged {
    /// A whole file's copy under a temporary name; None for a chunked file
    /// or a link.
    tmp: Option<PathBuf>,
    /// A chunked file: every chunk, with a temporary copy of each one the
    /// tin did not already hold.
    chunks: Vec<StagedChunk>,
    /// The recipe's temporary copy, when the tin did not already hold it.
    recipe_tmp: Option<PathBuf>,
    /// A sealed copy waiting under a temporary name, with the random id it
    /// will be named by.
    sealed: Option<(PathBuf, String)>,
    chunked: bool,
    sha256: String,
    bytes: u64,
    mode: Option<u32>,
    mtime_ns: Option<u64>,
    link: Option<PathBuf>,
}

#[derive(Debug)]
struct StagedChunk {
    sha256: String,
    len: u64,
    tmp: Option<PathBuf>,
}

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_name(tag: &str) -> PathBuf {
    temp_name_in(&objects_dir(), tag)
}

fn temp_name_in(dir: &Path, tag: &str) -> PathBuf {
    dir.join(format!(
        ".{tag}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

/// A test-only counter: cig notes every time it reads a pack file's bytes
/// into the tin, so the "an unchanged tree is not read" test can prove a
/// hop with the stat cache on stashes no bytes a second time. The counter
/// is a count of appended `.` bytes in the file at `CIG_INTERNAL_TIN_READS`
/// (one byte per read-open), so two cig processes in a chain both count.
/// Unset outside tests: the call is a cheap no-op.
pub(crate) fn note_file_read() {
    let Some(path) = std::env::var_os("CIG_INTERNAL_TIN_READS") else {
        return;
    };
    if path.is_empty() {
        return;
    }
    if let Ok(mut f) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(std::path::Path::new(&path))
    {
        let _ = f.write_all(b".");
    }
}

/// Copy `from` to `to`, hashing the bytes as they are written: the hash
/// names what was copied, not what the file held a moment earlier. A file
/// that changes while it is read gives a copy that is a moment newer than
/// the scan, and an object that still matches its name.
fn copy_hashing(from: &Path, to: &Path) -> io::Result<(String, u64)> {
    note_file_read();
    let mut src = fs::File::open(from)?;
    let mut dst = fs::File::create(to)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut total = 0u64;
    loop {
        let n = src.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        dst.write_all(&buf[..n])?;
        total += n as u64;
    }
    dst.flush()?;
    Ok((hex::encode(hasher.finalize()), total))
}

pub(crate) fn mode_of(meta: &fs::Metadata) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Some(meta.permissions().mode() & 0o7777)
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
    }
}

pub(crate) fn make_read_only(p: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(p, fs::Permissions::from_mode(0o444))
    }
    #[cfg(not(unix))]
    {
        let mut perms = fs::metadata(p)?.permissions();
        perms.set_readonly(true);
        fs::set_permissions(p, perms)
    }
}

/// Is the object already there, whole and hashing to its name?
fn pressed_ok(object: &Path, len: u64, sha256: &str) -> bool {
    object.is_file()
        && fs::metadata(object).map(|m| m.len()).ok() == Some(len)
        && sha256_file(object).ok().as_deref() == Some(sha256)
}

/// Give a temporary copy its name as an object: pressed, written once. An
/// object already there is trusted only when it hashes to its name; one
/// that does not is replaced, so a corrupt object is never reused by a
/// later stash. The copy is flushed through a handle that works on a
/// read-only file too, then named, and its directory synced, so the journal
/// entry that follows never points at a name the disk has not learned yet.
fn press(tmp: &Path, object: &Path, len: u64, sha256: &str) -> io::Result<()> {
    let bucket = object.parent().expect("an object has a bucket");
    fs::create_dir_all(bucket)?;
    if pressed_ok(object, len, sha256) {
        let _ = fs::remove_file(tmp);
        return Ok(());
    }
    let done = (|| {
        super::journal::sync_file(tmp)?;
        make_read_only(tmp)?;
        if object.exists() {
            lift_read_only(object);
            let _ = fs::remove_file(object);
        }
        fs::rename(tmp, object)?;
        let _ = super::journal::sync_dir_pub(bucket);
        Ok(())
    })();
    if done.is_err() {
        let _ = fs::remove_file(tmp);
    }
    done
}

/// Copy `path` beside the objects, hashed as it is copied: whole when it is
/// small, as chunks under a recipe when it is `CHUNK_FILE_MIN` or more. A
/// symbolic link needs no copy: it is kept as its target.
pub fn prepare(path: &Path) -> io::Result<Staged> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Ok(Staged {
            tmp: None,
            chunks: Vec::new(),
            recipe_tmp: None,
            sealed: None,
            chunked: false,
            sha256: String::new(),
            bytes: 0,
            mode: None,
            mtime_ns: None,
            link: Some(fs::read_link(path)?),
        });
    }
    fs::create_dir_all(objects_dir())?;
    if meta.len() >= CHUNK_FILE_MIN {
        return prepare_chunked(path, &meta);
    }
    let tmp = temp_name("stash");
    let (sha256, bytes) = match copy_hashing(path, &tmp) {
        Ok(v) => v,
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
    };
    Ok(Staged {
        tmp: Some(tmp),
        chunks: Vec::new(),
        recipe_tmp: None,
        sealed: None,
        chunked: false,
        sha256,
        bytes,
        mode: mode_of(&meta),
        mtime_ns: super::journal::mtime_ns(&meta),
        link: None,
    })
}

/// Read a large file once, cutting it into content-defined chunks as it
/// streams. A chunk the tin already holds (hashing to its name) costs
/// nothing; the rest wait under temporary names. The recipe is written
/// when the tin has none for this exact content.
fn prepare_chunked(path: &Path, meta: &fs::Metadata) -> io::Result<Staged> {
    // A big file is cut and hashed on every core (#148); a failure there
    // (a file that shrank while it was read, say) is retried on the serial
    // path, which gives the error a plain read would have given.
    let workers = chunk_workers(meta.len());
    if workers > 1 {
        if let Ok(staged) = prepare_chunked_parallel(path, meta, workers) {
            return Ok(staged);
        }
    }
    prepare_chunked_serial(path, meta)
}

/// The serial reference: one pass, one core. The parallel path must give
/// exactly what this gives (`parallel_hash_equals_serial_hash`).
fn prepare_chunked_serial(path: &Path, meta: &fs::Metadata) -> io::Result<Staged> {
    note_file_read();
    fs::create_dir_all(recipes_dir())?;
    let file = fs::File::open(path)?;
    let mut whole = Sha256::new();
    let mut total = 0u64;
    let mut chunks: Vec<StagedChunk> = Vec::new();
    let cut = (|| -> io::Result<()> {
        let reader = io::BufReader::with_capacity(1 << 20, file);
        for chunk in fastcdc::v2020::StreamCDC::new(reader, CHUNK_MIN, CHUNK_AVG, CHUNK_MAX) {
            let chunk = match chunk {
                Ok(c) => c,
                Err(fastcdc::v2020::Error::Empty) => break,
                Err(fastcdc::v2020::Error::IoError(e)) => return Err(e),
                Err(e) => return Err(io::Error::other(e.to_string())),
            };
            let sha256 = hex::encode(Sha256::digest(&chunk.data));
            whole.update(&chunk.data);
            let len = chunk.data.len() as u64;
            total += len;
            let tmp = if pressed_ok(&object_path(&sha256), len, &sha256) {
                None
            } else {
                let t = temp_name("chunk");
                fs::write(&t, &chunk.data)?;
                Some(t)
            };
            chunks.push(StagedChunk { sha256, len, tmp });
        }
        Ok(())
    })();
    if let Err(e) = cut {
        for c in &chunks {
            if let Some(t) = &c.tmp {
                let _ = fs::remove_file(t);
            }
        }
        return Err(e);
    }
    stage_recipe(chunks, hex::encode(whole.finalize()), total, meta)
}

/// The end both chunking paths share: the recipe for the chunks, written
/// beside them unless the tin already holds this exact one.
fn stage_recipe(
    chunks: Vec<StagedChunk>,
    sha256: String,
    total: u64,
    meta: &fs::Metadata,
) -> io::Result<Staged> {
    let recipe = Recipe {
        format: RECIPE_FORMAT,
        sha256: sha256.clone(),
        bytes: total,
        chunks: chunks
            .iter()
            .map(|c| ChunkRef {
                sha256: c.sha256.clone(),
                len: c.len,
            })
            .collect(),
    };
    let have = read_recipe(&recipe_path(&sha256)).ok();
    let recipe_tmp = if have.as_ref() == Some(&recipe) {
        None
    } else {
        let t = temp_name_in(&recipes_dir(), "recipe");
        let json = serde_json::to_vec(&recipe)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if let Err(e) = fs::write(&t, json) {
            for c in &chunks {
                if let Some(t) = &c.tmp {
                    let _ = fs::remove_file(t);
                }
            }
            return Err(e);
        }
        Some(t)
    };
    Ok(Staged {
        tmp: None,
        chunks,
        recipe_tmp,
        sealed: None,
        chunked: true,
        sha256,
        bytes: total,
        mode: mode_of(meta),
        mtime_ns: super::journal::mtime_ns(meta),
        link: None,
    })
}

/// A file this big is cut into chunks and hashed on every core. Below it the
/// threads cost more than they save.
const PARALLEL_CHUNK_MIN: u64 = 16 * 1024 * 1024;
/// How much of a file one worker cuts before its stream is joined to the one
/// before it.
const CUT_SEGMENT: u64 = 8 * 1024 * 1024;
/// A recipe this big (and this many chunks) is restored on every core.
const PARALLEL_RESTORE_MIN: u64 = 8 * 1024 * 1024;

fn cores() -> usize {
    std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(8)
}

/// How many threads chunk a file of `len` bytes: one means the serial path.
fn chunk_workers(len: u64) -> usize {
    if len < PARALLEL_CHUNK_MIN {
        1
    } else {
        cores()
    }
}

/// Read exactly `buf.len()` bytes at `offset`, leaving no cursor to share.
fn read_exact_at(f: &fs::File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        f.read_exact_at(buf, offset)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut done = 0usize;
        while done < buf.len() {
            let n = f.seek_read(&mut buf[done..], offset + done as u64)?;
            if n == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            done += n;
        }
        Ok(())
    }
}

/// Ask the kernel to start writing a range back now, without waiting for it,
/// so the `fsync` that ends a restore finds most of the file already on its
/// way to the disk. A hint: where it is unknown or refused, nothing changes.
fn start_writeback(f: &fs::File, offset: u64, len: u64) {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: a plain syscall on a descriptor this function borrows.
        unsafe {
            libc::sync_file_range(
                f.as_raw_fd(),
                offset as libc::off64_t,
                len as libc::off64_t,
                libc::SYNC_FILE_RANGE_WRITE,
            );
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (f, offset, len);
}

/// Write all of `buf` at `offset` (`pwrite`), leaving no cursor to share.
fn write_all_at(f: &fs::File, buf: &[u8], offset: u64) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        f.write_all_at(buf, offset)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut done = 0usize;
        while done < buf.len() {
            let n = f.seek_write(&buf[done..], offset + done as u64)?;
            if n == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            done += n;
        }
        Ok(())
    }
}

struct Lane<T> {
    claimed: usize,
    consumed: usize,
    ready: BTreeMap<usize, io::Result<T>>,
    stop: bool,
}

/// Sets the lane to stop when dropped, so workers never wait on a consumer
/// that has gone (an error, or a panic in the sink).
struct StopOnDrop<'a, T>(&'a std::sync::Mutex<Lane<T>>, &'a std::sync::Condvar);

impl<T> Drop for StopOnDrop<'_, T> {
    fn drop(&mut self) {
        if let Ok(mut g) = self.0.lock() {
            g.stop = true;
        }
        self.1.notify_all();
    }
}

/// Run `work(0..n)` on `workers` threads and hand each result to `sink` in
/// index order, on the calling thread. At most `window` results are ahead
/// of the sink, so memory is bounded. The first error stops the run; what
/// finished but was never sunk goes to `discard`, so a caller can clean up.
fn ordered_map<T: Send>(
    n: usize,
    workers: usize,
    window: usize,
    work: &(dyn Fn(usize) -> io::Result<T> + Sync),
    mut sink: impl FnMut(usize, T),
    mut discard: impl FnMut(T),
) -> io::Result<()> {
    use std::sync::{Condvar, Mutex};
    let lane = Mutex::new(Lane {
        claimed: 0,
        consumed: 0,
        ready: BTreeMap::new(),
        stop: false,
    });
    let cv = Condvar::new();
    let window = window.max(1);
    let mut outcome = Ok(());
    std::thread::scope(|s| {
        let _stop = StopOnDrop(&lane, &cv);
        for _ in 0..workers.max(1) {
            s.spawn(|| loop {
                let i = {
                    let Ok(mut g) = lane.lock() else { return };
                    loop {
                        if g.stop || g.claimed >= n {
                            return;
                        }
                        if g.claimed < g.consumed + window {
                            break;
                        }
                        g = match cv.wait(g) {
                            Ok(g) => g,
                            Err(_) => return,
                        };
                    }
                    let i = g.claimed;
                    g.claimed += 1;
                    i
                };
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(i)))
                    .unwrap_or_else(|_| Err(io::Error::other("a tin worker panicked")));
                if let Ok(mut g) = lane.lock() {
                    g.ready.insert(i, r);
                }
                cv.notify_all();
            });
        }
        for i in 0..n {
            let r = {
                let Ok(mut g) = lane.lock() else {
                    outcome = Err(io::Error::other("a tin worker panicked"));
                    return;
                };
                loop {
                    if let Some(r) = g.ready.remove(&i) {
                        g.consumed = i + 1;
                        break r;
                    }
                    g = match cv.wait(g) {
                        Ok(g) => g,
                        Err(_) => {
                            outcome = Err(io::Error::other("a tin worker panicked"));
                            return;
                        }
                    };
                }
            };
            cv.notify_all();
            match r {
                Ok(t) => sink(i, t),
                Err(e) => {
                    outcome = Err(e);
                    return;
                }
            }
        }
    });
    if let Ok(g) = lane.into_inner() {
        for (_, r) in g.ready {
            if let Ok(t) = r {
                discard(t);
            }
        }
    }
    outcome
}

/// The first chunk of `file` from `pos`: where it ends.
fn next_cut(file: &fs::File, len: u64, pos: u64) -> io::Result<u64> {
    let want = u64::from(CHUNK_MAX).min(len - pos) as usize;
    let mut buf = vec![0u8; want];
    read_exact_at(file, &mut buf, pos)?;
    let c = fastcdc::v2020::FastCDC::new(&buf, CHUNK_MIN, CHUNK_AVG, CHUNK_MAX)
        .next()
        .ok_or_else(|| io::Error::other("no chunk where the file has bytes"))?;
    Ok(pos + (c.offset + c.length) as u64)
}

/// The cuts one segment's worker finds, from its own guess of where a chunk
/// begins (the segment's start; the true start for the first segment): every
/// chunk end up to the first one at or past the segment's end. A cut is only
/// believed when `CHUNK_MAX` bytes follow its chunk's start in what was read
/// (fastcdc cuts differently once fewer remain) or the read reached the end
/// of the file.
fn segment_cuts(file: &fs::File, len: u64, k: usize) -> io::Result<Vec<u64>> {
    let s = k as u64 * CUT_SEGMENT;
    let e = (s + CUT_SEGMENT).min(len);
    let read_to = (e + u64::from(CHUNK_MAX)).min(len);
    let mut buf = vec![0u8; (read_to - s) as usize];
    read_exact_at(file, &mut buf, s)?;
    let at_end = read_to == len;
    let mut out = Vec::new();
    for c in fastcdc::v2020::FastCDC::new(&buf, CHUNK_MIN, CHUNK_AVG, CHUNK_MAX) {
        if !at_end && c.offset + CHUNK_MAX as usize > buf.len() {
            break;
        }
        let end = s + (c.offset + c.length) as u64;
        out.push(end);
        if end >= e {
            break;
        }
    }
    Ok(out)
}

/// Every chunk end of a file, the same ones one pass from the start would
/// find. Each segment is cut on its own thread from a guessed start; the
/// segments are then joined: a stream that shares one cut with the true one
/// shares every cut after it (a cut depends on where its chunk began and on
/// nothing before), so the true list takes the next segment's cuts from the
/// first one both have. Where none is shared, the true stream is walked on
/// until one is, which only costs time.
fn parallel_cuts(file: &fs::File, len: u64, workers: usize) -> io::Result<Vec<u64>> {
    if len == 0 {
        return Ok(Vec::new());
    }
    let segs = len.div_ceil(CUT_SEGMENT) as usize;
    let mut per: Vec<Vec<u64>> = Vec::with_capacity(segs);
    ordered_map(
        segs,
        workers,
        workers * 2,
        &|k| segment_cuts(file, len, k),
        |_, c| per.push(c),
        |_| {},
    )?;
    let mut truth = per[0].clone();
    for (k, mine) in per.iter().enumerate().skip(1) {
        if truth.last() == Some(&len) {
            break;
        }
        let s = k as u64 * CUT_SEGMENT;
        let set: HashSet<u64> = mine.iter().copied().collect();
        let mut at = truth.iter().position(|c| *c > s && set.contains(c));
        if at.is_none() {
            let mut pos = truth.last().copied().unwrap_or(0);
            while pos < len {
                pos = next_cut(file, len, pos)?;
                truth.push(pos);
                if set.contains(&pos) {
                    at = Some(truth.len() - 1);
                    break;
                }
            }
        }
        if let Some(i) = at {
            truth.truncate(i + 1);
            let join = truth[i];
            truth.extend(mine.iter().copied().filter(|c| *c > join));
        }
    }
    Ok(truth)
}

/// `prepare_chunked`, on `workers` threads: the same chunks, the same
/// recipe, byte for byte. The cuts are found in parallel, then each chunk is
/// read, hashed and (if the tin lacks it) written on its own thread, while
/// the whole file's hash, which no thread can split, is fed in order from the
/// very bytes the chunks were hashed from.
fn prepare_chunked_parallel(
    path: &Path,
    meta: &fs::Metadata,
    workers: usize,
) -> io::Result<Staged> {
    note_file_read();
    fs::create_dir_all(recipes_dir())?;
    let file = fs::File::open(path)?;
    let len = file.metadata()?.len();
    let cuts = parallel_cuts(&file, len, workers)?;
    let work = |i: usize| -> io::Result<(StagedChunk, Vec<u8>)> {
        let start = if i == 0 { 0 } else { cuts[i - 1] };
        let mut data = vec![0u8; (cuts[i] - start) as usize];
        read_exact_at(&file, &mut data, start)?;
        let sha256 = hex::encode(Sha256::digest(&data));
        let len = data.len() as u64;
        let tmp = if pressed_ok(&object_path(&sha256), len, &sha256) {
            None
        } else {
            let t = temp_name("chunk");
            if let Err(e) = fs::write(&t, &data) {
                let _ = fs::remove_file(&t);
                return Err(e);
            }
            Some(t)
        };
        Ok((StagedChunk { sha256, len, tmp }, data))
    };
    let mut whole = Sha256::new();
    let mut total = 0u64;
    let mut chunks: Vec<StagedChunk> = Vec::with_capacity(cuts.len());
    let mut stray: Vec<StagedChunk> = Vec::new();
    let ran = ordered_map(
        cuts.len(),
        workers,
        workers * 8,
        &work,
        |_, (c, data)| {
            whole.update(&data);
            total += c.len;
            chunks.push(c);
        },
        |(c, _)| stray.push(c),
    );
    if let Err(e) = ran {
        for c in chunks.iter().chain(stray.iter()) {
            if let Some(t) = &c.tmp {
                let _ = fs::remove_file(t);
            }
        }
        return Err(e);
    }
    stage_recipe(chunks, hex::encode(whole.finalize()), total, meta)
}

/// Seal `path` into the tin: encrypted under a fresh random id, never
/// shared with another file, so two equal secrets stay two objects that
/// tell nothing about each other. A link is kept as a link, as ever.
/// `prepare` (or `prepare_sealed`) for every path, on up to eight threads,
/// the results in the paths' order (#148): a hop that touched many files
/// copies and hashes them on every core, not one at a time. Each staged
/// object has a name of its own (the pid and a counter), and nothing else is
/// shared but the key, which is found once.
pub fn prepare_all(paths: &[PathBuf], sealed: bool) -> Vec<io::Result<Staged>> {
    let one = |p: &Path| {
        if sealed {
            prepare_sealed(p)
        } else {
            prepare(p)
        }
    };
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(8)
        .min(paths.len());
    if workers <= 1 || paths.len() < 4 {
        return paths.iter().map(|p| one(p)).collect();
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut done: Vec<(usize, io::Result<Staged>)> = std::thread::scope(|s| {
        let running: Vec<_> = (0..workers)
            .map(|_| {
                s.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(p) = paths.get(i) else {
                            break;
                        };
                        mine.push((i, one(p)));
                    }
                    mine
                })
            })
            .collect();
        running
            .into_iter()
            .flat_map(|w| w.join().expect("a tin worker panicked"))
            .collect()
    });
    done.sort_by_key(|(i, _)| *i);
    done.into_iter().map(|(_, r)| r).collect()
}

pub fn prepare_sealed(path: &Path) -> io::Result<Staged> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return prepare(path);
    }
    fs::create_dir_all(sealed_dir())?;
    let id = super::seal::random_id()?;
    let tmp = temp_name_in(&sealed_dir(), "seal");
    let (sha256, bytes) = super::seal::seal_file(path, &tmp, id.as_bytes())?;
    Ok(Staged {
        tmp: None,
        chunks: Vec::new(),
        recipe_tmp: None,
        sealed: Some((tmp, id)),
        chunked: false,
        sha256,
        bytes,
        mode: mode_of(&meta),
        mtime_ns: super::journal::mtime_ns(&meta),
        link: None,
    })
}

impl Staged {
    /// What the journal will point at once committed: the object, the
    /// recipe for a chunked file, or the sealed object (empty for a link).
    pub fn object(&self) -> PathBuf {
        if self.link.is_some() {
            PathBuf::new()
        } else if let Some((_, id)) = &self.sealed {
            sealed_dir().join(&id[..2]).join(id)
        } else if self.chunked {
            recipe_path(&self.sha256)
        } else {
            object_path(&self.sha256)
        }
    }

    /// Every path in the tin this stash will depend on: the object, or the
    /// recipe and each of its chunks. A run lists them before any exists
    /// under its name, so a `cig tin gc` during the hop keeps them.
    pub fn pending_paths(&self) -> Vec<PathBuf> {
        if self.link.is_some() {
            return Vec::new();
        }
        let mut out = vec![self.object()];
        for c in &self.chunks {
            out.push(object_path(&c.sha256));
        }
        out
    }

    /// Give the copies their names. A chunk the tin already held is checked
    /// to be there still; the rest are pressed, then the recipe.
    pub fn commit(self) -> io::Result<Stashed> {
        if let Some(target) = self.link {
            return Ok(Stashed {
                object: PathBuf::new(),
                sha256: String::new(),
                bytes: 0,
                mode: None,
                mtime_ns: None,
                link: Some(target),
                git: None,
            });
        }
        let object = self.object();
        if let Some((tmp, _)) = &self.sealed {
            let bucket = object.parent().expect("a sealed object has a bucket");
            fs::create_dir_all(bucket)?;
            let done = (|| -> io::Result<()> {
                super::journal::sync_file(tmp)?;
                make_read_only(tmp)?;
                fs::rename(tmp, &object)?;
                let _ = super::journal::sync_dir_pub(bucket);
                Ok(())
            })();
            if done.is_err() {
                let _ = fs::remove_file(tmp);
            }
            done?;
            if let Some((_, id)) = &self.sealed {
                super::vault::keep_all(&[(
                    super::vault::VKind::Sealed,
                    id.clone(),
                    object.clone(),
                )]);
            }
            return Ok(Stashed {
                object,
                sha256: self.sha256,
                bytes: self.bytes,
                mode: self.mode,
                mtime_ns: self.mtime_ns,
                link: None,
                git: None,
            });
        }
        // What the vault is handed once the copies have their names: the
        // chunks, then the recipe that names them.
        let mut for_vault: Vec<super::vault::Item> = Vec::new();
        if self.chunked {
            for c in &self.chunks {
                for_vault.push((
                    super::vault::VKind::Object,
                    c.sha256.clone(),
                    object_path(&c.sha256),
                ));
            }
            for_vault.push((
                super::vault::VKind::Recipe,
                self.sha256.clone(),
                object.clone(),
            ));
        } else if self.tmp.is_some() {
            for_vault.push((
                super::vault::VKind::Object,
                self.sha256.clone(),
                object.clone(),
            ));
        }
        if self.chunked {
            let mut chunks = self.chunks.into_iter();
            let mut failed: Option<io::Error> = None;
            for c in chunks.by_ref() {
                let target = object_path(&c.sha256);
                let r = match &c.tmp {
                    Some(tmp) => press(tmp, &target, c.len, &c.sha256),
                    // Held by the tin when the file was read; a `cig tin gc`
                    // that raced the stash is the one thing that takes it.
                    None if target.is_file() => Ok(()),
                    None => Err(io::Error::other(format!(
                        "chunk {} left the tin while the file was being kept (a gc ran meanwhile)",
                        &c.sha256[..12]
                    ))),
                };
                if let Err(e) = r {
                    failed = Some(e);
                    break;
                }
            }
            if let Some(e) = failed {
                for c in chunks {
                    if let Some(t) = &c.tmp {
                        let _ = fs::remove_file(t);
                    }
                }
                if let Some(t) = &self.recipe_tmp {
                    let _ = fs::remove_file(t);
                }
                return Err(e);
            }
            if let Some(tmp) = &self.recipe_tmp {
                let bucket = object.parent().expect("a recipe has a bucket");
                fs::create_dir_all(bucket)?;
                super::journal::sync_file(tmp)?;
                if object.exists() {
                    lift_read_only(&object);
                    let _ = fs::remove_file(&object);
                }
                fs::rename(tmp, &object)?;
                let _ = make_read_only(&object);
                let _ = super::journal::sync_dir_pub(bucket);
            }
        } else if let Some(tmp) = &self.tmp {
            press(tmp, &object, self.bytes, &self.sha256)?;
        }
        super::vault::keep_all(&for_vault);
        Ok(Stashed {
            object,
            sha256: self.sha256,
            bytes: self.bytes,
            mode: self.mode,
            mtime_ns: self.mtime_ns,
            link: None,
            git: None,
        })
    }
}

pub(crate) fn lift_read_only(p: &Path) {
    if let Ok(m) = fs::metadata(p) {
        let mut perms = m.permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            perms.set_mode(0o644);
        }
        #[cfg(not(unix))]
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        let _ = fs::set_permissions(p, perms);
    }
}

/// Keep a copy of `path` in the tin, named by its content. The same bytes
/// stashed twice cost nothing. The object is read-only; the file's own
/// mode and modification time travel in the journal and are put back on
/// restore. A symbolic link is kept as a link.
pub fn stash(path: &Path) -> io::Result<Stashed> {
    prepare(path)?.commit()
}

/// Move a file into the tin: what a rollback is about to remove or rewrite
/// goes here first, so nothing a rollback displaces is lost. The file
/// leaves its place (a rename where the tin is on the same filesystem, a
/// copy and a remove where it is not) and becomes an object named by the
/// hash of what arrived.
pub fn take(path: &Path) -> io::Result<Stashed> {
    take_with(path, false)
}

/// `take`, sealing the file when `seal` says so (what replaced a secret is
/// a secret) or when the file alone says so.
pub fn take_with(path: &Path, seal: bool) -> io::Result<Stashed> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() {
        return Err(io::Error::other(format!(
            "{} is not a regular file; the tin keeps files",
            path.display()
        )));
    }
    fs::create_dir_all(objects_dir())?;
    if (seal && super::seal::policy() != super::seal::Policy::Never)
        || super::seal::should_seal_here(path)
    {
        // A secret the rollback displaces is kept sealed like any other.
        let st = prepare_sealed(path)?.commit()?;
        let _ = fs::remove_file(path);
        return Ok(st);
    }
    if meta.len() >= CHUNK_FILE_MIN {
        // A large file is read and kept as chunks, so a hop's output that
        // is the original with one change shares every other chunk with it.
        let st = prepare_chunked(path, &meta)?.commit()?;
        // A file another program holds open (Windows) stays where it is for
        // now; its bytes are kept all the same.
        let _ = fs::remove_file(path);
        return Ok(st);
    }
    let tmp = temp_name("take");
    match fs::rename(path, &tmp) {
        Ok(()) => {}
        // Across filesystems, or a file Windows will not let go of yet: copy.
        // A file another program holds open stays where it is for now; its
        // bytes are kept all the same, and the caller decides what a file
        // still at the path means (a restore writes over it in place).
        Err(_) => {
            copy_hashing(path, &tmp).inspect_err(|_| {
                let _ = fs::remove_file(&tmp);
            })?;
            let _ = fs::remove_file(path);
        }
    }
    lift_read_only(&tmp);
    let sha256 = sha256_file(&tmp)?;
    let bytes = fs::metadata(&tmp)?.len();
    let staged = Staged {
        tmp: Some(tmp),
        chunks: Vec::new(),
        recipe_tmp: None,
        sealed: None,
        chunked: false,
        sha256,
        bytes,
        mode: mode_of(&meta),
        mtime_ns: super::journal::mtime_ns(&meta),
        link: None,
    };
    staged.commit()
}

/// Write the bytes a snapshot holds to `dest`, verified: a whole tin
/// object is hashed as it is copied and must match its name, a recipe's
/// chunks each match theirs and add up to the recipe's hash, and either
/// must match the hash the journal recorded. A snapshot in a run's own
/// directory is copied with its mode, as it always was. On any failure
/// `dest` is removed, so nothing that does not hash as named is ever left
/// where a file is about to be restored.
pub fn materialize(snapshot: &Path, expect_sha256: &str, dest: &Path) -> io::Result<()> {
    let mut result = materialize_inner(snapshot, expect_sha256, dest);
    // A tin object that is gone or no longer hashes to its name: the vault
    // (when there is one) has the copy nothing running as the user could
    // delete. Put back, the restore is tried once more.
    if result.is_err() && super::vault::heal(snapshot) {
        result = materialize_inner(snapshot, expect_sha256, dest);
    }
    if result.is_err() {
        let _ = fs::remove_file(dest);
    }
    result
}

fn materialize_inner(snapshot: &Path, expect_sha256: &str, dest: &Path) -> io::Result<()> {
    if super::seal::is_sealed_path(snapshot) {
        return super::seal::unseal_file(
            snapshot,
            &super::seal::aad_of(snapshot),
            expect_sha256,
            dest,
        );
    }
    if is_recipe(snapshot) {
        let recipe = read_recipe(snapshot)?;
        if !expect_sha256.is_empty() && recipe.sha256 != expect_sha256 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "recipe {} is for other bytes than the journal recorded (sha256 {} against {}); refusing to restore from it",
                    snapshot.display(),
                    &recipe.sha256[..12],
                    &expect_sha256[..expect_sha256.len().min(12)]
                ),
            ));
        }
        // A big recipe is restored on every core (#148): chunks are read,
        // checked and written at their own offsets in a file of the right
        // size, and the whole hash is fed in order from what was checked.
        let workers = restore_workers(&recipe);
        if workers > 1 {
            return restore_parallel(&recipe, snapshot, dest, workers);
        }
        return restore_serial(&recipe, snapshot, dest);
    }
    if holds(snapshot) && !expect_sha256.is_empty() {
        // A shared object is checked against its name every time it is
        // read back: a corrupt tin never writes garbage.
        let (actual, _) = copy_hashing(snapshot, dest)?;
        if actual != expect_sha256 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "tin object {} is corrupt (sha256 {} on disk); refusing to restore from it",
                    snapshot.display(),
                    &actual[..12]
                ),
            ));
        }
        return Ok(());
    }
    fs::copy(snapshot, dest)?;
    #[cfg(unix)]
    fs::set_permissions(dest, fs::metadata(snapshot)?.permissions())?;
    Ok(())
}

/// One chunk of a recipe, read from the tin and checked against the length
/// and hash the recipe names: a missing or corrupt one refuses the restore.
fn read_chunk_checked(recipe: &Recipe, i: usize) -> io::Result<Vec<u8>> {
    let c = &recipe.chunks[i];
    let object = object_path(&c.sha256);
    let data = fs::read(&object).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!(
                "chunk {} of {} (tin object {}) is missing: {e}; refusing to restore from the recipe",
                i + 1,
                recipe.chunks.len(),
                &c.sha256[..12]
            ),
        )
    })?;
    if data.len() as u64 != c.len || hex::encode(Sha256::digest(&data)) != c.sha256 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "tin object {} is corrupt (chunk {} of {} of the recipe); refusing to restore from it",
                &c.sha256[..12],
                i + 1,
                recipe.chunks.len()
            ),
        ));
    }
    Ok(data)
}

/// The serial restore: one chunk at a time, in order. What the parallel one
/// must write, byte for byte.
fn restore_serial(recipe: &Recipe, snapshot: &Path, dest: &Path) -> io::Result<()> {
    let mut out = io::BufWriter::new(fs::File::create(dest)?);
    let mut whole = Sha256::new();
    for i in 0..recipe.chunks.len() {
        let data = read_chunk_checked(recipe, i)?;
        whole.update(&data);
        out.write_all(&data)?;
    }
    out.flush()?;
    out.get_ref().sync_all()?;
    if hex::encode(whole.finalize()) != recipe.sha256 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "the chunks of recipe {} do not add up to its hash; refusing to restore from it",
                snapshot.display()
            ),
        ));
    }
    Ok(())
}

/// How many threads restore this recipe: one means the serial loop.
fn restore_workers(recipe: &Recipe) -> usize {
    if recipe.bytes < PARALLEL_RESTORE_MIN || recipe.chunks.len() < 8 {
        1
    } else {
        cores()
    }
}

/// Restore a recipe's chunks on `workers` threads with `pwrite`: the file is
/// made its full size first, each chunk lands at the sum of the lengths
/// before it once it hashes to its name, and the whole hash is fed in order
/// from the checked bytes. The result is what the serial loop writes.
fn restore_parallel(
    recipe: &Recipe,
    snapshot: &Path,
    dest: &Path,
    workers: usize,
) -> io::Result<()> {
    let out = fs::File::create(dest)?;
    let total: u64 = recipe.chunks.iter().map(|c| c.len).sum();
    out.set_len(total)?;
    let mut offsets = Vec::with_capacity(recipe.chunks.len());
    let mut at = 0u64;
    for c in &recipe.chunks {
        offsets.push(at);
        at += c.len;
    }
    let mut whole = Sha256::new();
    ordered_map(
        recipe.chunks.len(),
        workers,
        workers * 8,
        &|i| {
            let data = read_chunk_checked(recipe, i)?;
            write_all_at(&out, &data, offsets[i])?;
            start_writeback(&out, offsets[i], data.len() as u64);
            Ok(data)
        },
        |_, data| whole.update(&data),
        |_| {},
    )?;
    out.sync_all()?;
    if hex::encode(whole.finalize()) != recipe.sha256 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "the chunks of recipe {} do not add up to its hash; refusing to restore from it",
                snapshot.display()
            ),
        ));
    }
    Ok(())
}

/// Does the sealed snapshot open with the tin's key and hold the bytes the
/// journal recorded? Nothing is written. One line per problem.
pub fn check_sealed(snapshot: &Path, expect_sha256: &str) -> Vec<String> {
    match super::seal::unseal_to(
        snapshot,
        &super::seal::aad_of(snapshot),
        expect_sha256,
        &mut io::sink(),
    ) {
        Ok(_) => Vec::new(),
        Err(e) => vec![e.to_string()],
    }
}

/// Everything a restore from this snapshot would need, checked without
/// writing: the recipe reads and names the right bytes, every chunk is
/// there and hashes to its name. One line per problem.
pub fn check_recipe(snapshot: &Path, expect_sha256: &str) -> Vec<String> {
    let recipe = match read_recipe(snapshot) {
        Ok(r) => r,
        Err(e) => return vec![format!("recipe {} cannot be read: {e}", snapshot.display())],
    };
    let mut problems = Vec::new();
    if !expect_sha256.is_empty() && recipe.sha256 != expect_sha256 {
        problems.push(format!(
            "recipe {} is for other bytes than the journal recorded",
            snapshot.display()
        ));
    }
    for (i, c) in recipe.chunks.iter().enumerate() {
        let object = object_path(&c.sha256);
        if !object.is_file() {
            problems.push(format!(
                "chunk {} of {} (tin object {}) is missing",
                i + 1,
                recipe.chunks.len(),
                &c.sha256[..12]
            ));
            continue;
        }
        match sha256_file(&object) {
            Ok(actual) if actual == c.sha256 => {}
            Ok(actual) => problems.push(format!(
                "tin object {} is corrupt (chunk {} of {}; sha256 {} on disk)",
                &c.sha256[..12],
                i + 1,
                recipe.chunks.len(),
                &actual[..12]
            )),
            Err(e) => problems.push(format!("tin object {}: cannot read: {e}", &c.sha256[..12])),
        }
    }
    problems
}

/// Is this snapshot path an object in the tin (shared by content), as
/// opposed to a run's own snapshot?
pub fn holds(path: &Path) -> bool {
    path.starts_with(objects_dir())
}

/// A set of `touches` globs, matched against a path relative to the pack
/// root it is under. `*` is the root's own files, `**` everything, `src/**`
/// a tree, `package.json` one file. A leading `?` (`?yarn.lock`) marks a
/// glob that may match nothing without a word; it is not part of the glob.
pub fn globs(patterns: &[String]) -> Result<GlobSet, String> {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        let bare = p.strip_prefix('?').unwrap_or(p).trim_start_matches("./");
        let g = Glob::new(bare).map_err(|e| format!("touches: `{p}` is not a glob: {e}"))?;
        b.add(g);
    }
    b.build().map_err(|e| format!("touches: {e}"))
}

/// The file a running hop lists the objects it stashed in, inside its run
/// directory, before its journal refers to them. `referenced` reads it, so
/// a `cig tin gc` during the hop keeps what the hop's rollback will need.
pub const PENDING: &str = "tin.pending";

/// Every object a live run still refers to, by path: from its journal, and
/// from the pending list of a hop still running.
pub fn referenced() -> HashSet<PathBuf> {
    let mut out = HashSet::new();
    for rec in super::runs::list().unwrap_or_default() {
        out.extend(referenced_by(&rec));
    }
    // What another tool keeps here (`cig tin keep`) is referred to by its
    // record, whatever the runs say.
    for k in kept_copies() {
        out.insert(k.object);
    }
    out
}

/// The objects one run refers to: from its journal, and from the pending
/// list of a hop still running. The history budget (#149) counts these per
/// run, so it knows which bytes a prune frees.
pub fn referenced_by(rec: &super::runs::RunRecord) -> HashSet<PathBuf> {
    let mut out = HashSet::new();
    {
        if let Ok(j) = super::journal::Journal::load(&rec.dir()) {
            for e in j.entries() {
                for b in e.before.iter().chain(e.took.iter()) {
                    match &b.before {
                        super::journal::Before::File {
                            sha256,
                            git: Some(_),
                            ..
                        } => {
                            if let Some(o) = object_for(sha256) {
                                if is_recipe(&o) {
                                    if let Ok(r) = read_recipe(&o) {
                                        for c in &r.chunks {
                                            out.insert(object_path(&c.sha256));
                                        }
                                    }
                                }
                                out.insert(o);
                            }
                        }
                        super::journal::Before::File { snapshot, .. } if is_recipe(snapshot) => {
                            out.insert(snapshot.clone());
                            if let Ok(r) = read_recipe(snapshot) {
                                for c in &r.chunks {
                                    out.insert(object_path(&c.sha256));
                                }
                            }
                        }
                        super::journal::Before::File { snapshot, .. }
                        | super::journal::Before::Dir { snapshot }
                            if holds(snapshot) || is_sealed(snapshot) =>
                        {
                            out.insert(snapshot.clone());
                        }
                        _ => {}
                    }
                }
            }
        }
        if let Ok(text) = fs::read_to_string(rec.dir().join(PENDING)) {
            for line in text.lines() {
                let line = line.trim();
                if !line.is_empty() {
                    out.insert(PathBuf::from(line));
                }
            }
        }
    }
    out
}

/// `n` objects drawn at random from the whole store, for the check after a
/// burn (#105, L6, `verify_sample`): a random bucket, then a random object
/// in it, so a big tin costs no more to sample than a small one.
pub fn random_objects(n: usize) -> Vec<PathBuf> {
    let Ok(buckets) = fs::read_dir(objects_dir()) else {
        return Vec::new();
    };
    let buckets: Vec<PathBuf> = buckets
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    if buckets.is_empty() || n == 0 {
        return Vec::new();
    }
    let mut seed = [0u8; 8];
    let _ = getrandom::getrandom(&mut seed);
    let mut x = u64::from_le_bytes(seed) | 1;
    let mut next = move |below: usize| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        (x % below as u64) as usize
    };
    let mut out: Vec<PathBuf> = Vec::new();
    // A few draws more than asked: a bucket can be empty, a draw repeated.
    for _ in 0..n.saturating_mul(4) {
        if out.len() >= n {
            break;
        }
        let Ok(entries) = fs::read_dir(&buckets[next(buckets.len())]) else {
            continue;
        };
        let names: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.len() == 64 && n.chars().all(|c| c.is_ascii_hexdigit()))
            })
            .collect();
        if names.is_empty() {
            continue;
        }
        let pick = names[next(names.len())].clone();
        if !out.contains(&pick) {
            out.push(pick);
        }
    }
    out
}

/// Every entry of one bucketed store (`objects/`, `recipes/`, `sealed/`)
/// named by `name_len` hex digits, with its size.
fn hashed_entries_named(store: &Path, name_len: usize) -> Vec<(PathBuf, u64)> {
    let mut out = Vec::new();
    let Ok(buckets) = fs::read_dir(store) else {
        return out;
    };
    for bucket in buckets.flatten() {
        let Ok(entries) = fs::read_dir(bucket.path()) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if name.len() == name_len && name.chars().all(|c| c.is_ascii_hexdigit()) {
                if let Ok(m) = e.metadata() {
                    out.push((p, m.len()));
                }
            }
        }
    }
    out.sort();
    out
}

fn hashed_entries(store: &Path) -> Vec<(PathBuf, u64)> {
    hashed_entries_named(store, 64)
}

/// Every object in the tin (whole files and chunks alike), with its size.
pub fn objects() -> Vec<(PathBuf, u64)> {
    hashed_entries(&objects_dir())
}

/// Every recipe in the tin, with the size of the recipe file itself.
pub fn recipes() -> Vec<(PathBuf, u64)> {
    hashed_entries(&recipes_dir())
}

/// Every sealed object in the tin, with its size on disk.
pub fn sealed() -> Vec<(PathBuf, u64)> {
    hashed_entries_named(&sealed_dir(), 32)
}

/// What the tin holds, stored and logical.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Usage {
    /// Objects on disk: whole files and chunks.
    pub objects: usize,
    /// Bytes the objects take.
    pub stored: u64,
    /// Recipes: chunked files.
    pub recipes: usize,
    /// Bytes the kept files would take written out: every recipe's file,
    /// plus every object that is not a chunk of one, plus the sealed ones.
    pub logical: u64,
    /// Sealed objects (encrypted at rest), counted apart.
    pub sealed: usize,
    pub sealed_bytes: u64,
}

/// What the tin holds: objects and bytes stored, next to what the files
/// they keep would take, so the saving from chunking is visible.
pub fn du() -> Usage {
    let all = objects();
    let mut chunk_names: HashSet<String> = HashSet::new();
    let mut logical = 0u64;
    let recipes = recipes();
    for (p, _) in &recipes {
        if let Ok(r) = read_recipe(p) {
            logical += r.bytes;
            for c in r.chunks {
                chunk_names.insert(c.sha256);
            }
        }
    }
    for (p, size) in &all {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if !chunk_names.contains(&name) {
            logical += size;
        }
    }
    let sealed = sealed();
    let sealed_bytes: u64 = sealed.iter().map(|(_, b)| b).sum();
    Usage {
        objects: all.len(),
        stored: all.iter().map(|(_, b)| b).sum(),
        recipes: recipes.len(),
        logical: logical + sealed_bytes,
        sealed: sealed.len(),
        sealed_bytes,
    }
}

/// Re-hash every object against its name, check every recipe names chunks
/// the tin holds, and open every sealed object with the tin's key (with
/// `quick`, only look at its shape). Returns one line per problem.
pub fn verify(quick: bool) -> Vec<String> {
    let mut problems = Vec::new();
    for (p, size) in sealed() {
        if quick {
            let shaped = fs::File::open(&p)
                .and_then(|mut f| {
                    let mut head = [0u8; 8];
                    f.read_exact(&mut head).map(|()| head)
                })
                .map(|h| &h == super::seal::MAGIC)
                .unwrap_or(false);
            if !shaped || size < 8 + 19 + 16 {
                problems.push(format!(
                    "{} is not shaped like a sealed object",
                    p.display()
                ));
            }
            continue;
        }
        if let Err(e) = super::seal::opens(&p, &super::seal::aad_of(&p)) {
            problems.push(format!("{e}"));
        }
    }
    for (p, _) in objects() {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        match sha256_file(&p) {
            Ok(actual) if actual == name => {}
            Ok(actual) => problems.push(format!(
                "{} does not hash to its name (sha256 {} on disk); a restore from it would be refused",
                p.display(),
                &actual[..12]
            )),
            Err(e) => problems.push(format!("{}: cannot read: {e}", p.display())),
        }
    }
    for (p, _) in recipes() {
        match read_recipe(&p) {
            Ok(r) => {
                for (i, c) in r.chunks.iter().enumerate() {
                    if !object_path(&c.sha256).is_file() {
                        problems.push(format!(
                            "{} names chunk {} of {} (object {}), which is missing; a restore from it would be refused",
                            p.display(),
                            i + 1,
                            r.chunks.len(),
                            &c.sha256[..12]
                        ));
                    }
                }
            }
            Err(e) => problems.push(format!("{}: {e}", p.display())),
        }
    }
    problems
}

/// Remove every object, recipe and sealed object no live run refers to,
/// and, with `sealed_orphans`, every sealed object the tin's key cannot
/// open (sealed under a key that is gone). Returns (removed, bytes).
pub fn gc(sealed_orphans: bool) -> (usize, u64) {
    let keep = referenced();
    let mut removed = 0;
    let mut bytes = 0;
    for (p, size) in objects().into_iter().chain(recipes()).chain(sealed()) {
        let orphan = sealed_orphans
            && is_sealed(&p)
            && super::seal::opens(&p, &super::seal::aad_of(&p)).is_err();
        if keep.contains(&p) && !orphan {
            continue;
        }
        lift_read_only(&p);
        if fs::remove_file(&p).is_ok() {
            removed += 1;
            bytes += size;
        }
    }
    (removed, bytes)
}

/// The names hop watching never looks into: build and dependency trees
/// that are rebuilt, not restored, plus whatever a pack root's
/// `.cigignore` lists (one glob per line, `#` comments, relative to the root).
pub const SKIPPED_ALWAYS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".cigscript",
    ".venv",
    "venv",
    "__pycache__",
    ".gradle",
    ".terraform",
];

/// The `.cigignore` of a pack root, as a glob set; `None` when there is none.
pub fn ignore_set(root: &Path) -> Option<GlobSet> {
    let text = fs::read_to_string(root.join(".cigignore")).ok()?;
    let mut b = GlobSetBuilder::new();
    let mut any = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let pat = line.trim_start_matches("./").trim_end_matches('/');
        for p in [pat.to_string(), format!("{pat}/**")] {
            if let Ok(g) = Glob::new(&p) {
                b.add(g);
                any = true;
            }
        }
    }
    if !any {
        return None;
    }
    b.build().ok()
}

/// What a watched hop leaves out (#148): its own `skip` globs and the
/// `hop_skip` setting, each matched like a `.cigignore` line (a directory's
/// glob covers what is under it). A path under one is never read, kept or
/// journaled, so the hop cannot undo its changes there (E310). `None` when
/// nothing is skipped.
pub fn skip_set(hop: &[String]) -> Option<GlobSet> {
    let setting = hop_skip_setting();
    let mut b = GlobSetBuilder::new();
    let mut any = false;
    for pat in hop
        .iter()
        .map(String::as_str)
        .chain(skip_setting_globs(setting))
    {
        let pat = pat.trim().trim_start_matches("./").trim_end_matches('/');
        if pat.is_empty() {
            continue;
        }
        for p in [pat.to_string(), format!("{pat}/**")] {
            if let Ok(g) = Glob::new(&p) {
                b.add(g);
                any = true;
            }
        }
    }
    if !any {
        return None;
    }
    b.build().ok()
}

/// The globs a `hop_skip` setting lists: comma-separated.
pub fn skip_setting_globs(value: &str) -> impl Iterator<Item = &str> {
    value.split(',').map(str::trim).filter(|g| !g.is_empty())
}

/// The `hop_skip` setting, read once per process like `watch_cap`.
fn hop_skip_setting() -> &'static str {
    static SKIP: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SKIP.get_or_init(|| crate::config::Config::load().get("hop_skip"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #148: a hop's `skip` globs match like `.cigignore` lines: a
    /// directory's glob covers what is under it, `./` and a trailing `/`
    /// are dropped, and nothing given is nothing skipped.
    #[test]
    fn skip_globs_match_like_cigignore_lines() {
        let s = skip_set(&["./cache/".to_string(), "*.log".to_string()]).unwrap();
        assert!(s.is_match("cache"));
        assert!(s.is_match("cache/a/b.bin"));
        assert!(s.is_match("build.log"));
        assert!(!s.is_match("src/main.rs"));
        let v: Vec<&str> = skip_setting_globs(" a/** , ,*.tmp").collect();
        assert_eq!(v, ["a/**", "*.tmp"]);
    }

    /// #148: staging on every core gives what staging one file at a time
    /// gives, in the paths' order: small files, chunked ones, links and a
    /// path that is not there (its error in its place).
    #[test]
    fn parallel_prepare_equals_serial_prepare() {
        with_home(|root| {
            let dir = root.join("files");
            fs::create_dir_all(&dir).unwrap();
            let mut paths = Vec::new();
            for i in 0..40u32 {
                let p = dir.join(format!("f{i}"));
                let len = if i % 7 == 0 {
                    CHUNK_FILE_MIN as usize + 4096 * i as usize
                } else {
                    37 * i as usize
                };
                let body: Vec<u8> = (0..len).map(|k| (k as u32 * 31 + i) as u8).collect();
                fs::write(&p, body).unwrap();
                paths.push(p);
            }
            paths.insert(17, dir.join("not-there"));
            #[cfg(unix)]
            {
                let link = dir.join("link");
                std::os::unix::fs::symlink("f3", &link).unwrap();
                paths.push(link);
            }
            let serial: Vec<io::Result<Staged>> = paths.iter().map(|p| prepare(p)).collect();
            let parallel = prepare_all(&paths, false);
            assert_eq!(serial.len(), parallel.len());
            for (i, (a, b)) in serial.iter().zip(&parallel).enumerate() {
                match (a, b) {
                    (Ok(a), Ok(b)) => {
                        assert_eq!(
                            (&a.sha256, a.bytes, a.chunked, &a.link, a.mode),
                            (&b.sha256, b.bytes, b.chunked, &b.link, b.mode),
                            "{}",
                            paths[i].display()
                        );
                        assert_eq!(a.chunks.len(), b.chunks.len());
                    }
                    (Err(a), Err(b)) => assert_eq!(a.kind(), b.kind()),
                    _ => panic!(
                        "{}: one way failed and the other did not",
                        paths[i].display()
                    ),
                }
            }
            assert!(
                parallel[17].is_err(),
                "the missing path's error is not in its place"
            );
            // Both sets commit, and the objects are the files' bytes.
            for st in parallel.into_iter().chain(serial).flatten() {
                st.commit().unwrap();
            }
        });
    }

    #[test]
    fn touches_globs_match_relative_paths() {
        let g = globs(&["src/**".to_string(), "package.json".to_string()]).unwrap();
        assert!(g.is_match("src/a/b.ts"));
        assert!(g.is_match("package.json"));
        assert!(!g.is_match("README.md"));
        let all = globs(&["**".to_string()]).unwrap();
        assert!(all.is_match("deep/er/file"));
        assert!(globs(&["[".to_string()]).is_err());
    }

    /// The tests below point the tin at a scratch home. Every test in this
    /// crate that sets `CIGSCRIPT_HOME` takes the same lock, since they
    /// share one process and one environment.
    fn with_home<T>(f: impl FnOnce(&Path) -> T) -> T {
        let _g = super::super::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let old = std::env::var_os("CIGSCRIPT_HOME");
        std::env::set_var("CIGSCRIPT_HOME", tmp.path().join("home"));
        let out = f(tmp.path());
        match old {
            Some(v) => std::env::set_var("CIGSCRIPT_HOME", v),
            None => std::env::remove_var("CIGSCRIPT_HOME"),
        }
        out
    }

    #[test]
    fn an_object_is_named_by_the_hash_of_the_copy_and_a_corrupt_one_is_not_reused() {
        with_home(|root| {
            let f = root.join("f.txt");
            fs::write(&f, "content one\n").unwrap();
            let first = stash(&f).unwrap();
            assert_eq!(
                first.sha256,
                sha256_file(&first.object).unwrap(),
                "the object hashes to its name"
            );
            assert_eq!(fs::read_to_string(&first.object).unwrap(), "content one\n");
            // Corrupt the object in place: same size, other bytes.
            lift_read_only(&first.object);
            fs::write(&first.object, "content two\n").unwrap();
            assert_eq!(verify(false).len(), 1);
            // A later stash of the same content does not trust the name.
            let again = stash(&f).unwrap();
            assert_eq!(again.object, first.object);
            assert_eq!(fs::read_to_string(&again.object).unwrap(), "content one\n");
            assert!(verify(false).is_empty(), "the corrupt object was replaced");
        });
    }

    #[test]
    fn take_moves_a_file_into_the_tin_and_names_it_by_content() {
        with_home(|root| {
            let f = root.join("notes.txt");
            fs::write(&f, "my notes\n").unwrap();
            let taken = take(&f).unwrap();
            assert!(!f.exists(), "the file left its place");
            assert_eq!(fs::read_to_string(&taken.object).unwrap(), "my notes\n");
            assert_eq!(taken.sha256, sha256_file(&taken.object).unwrap());
            assert_eq!(taken.bytes, 9);
            // The same content taken twice is one object.
            fs::write(&f, "my notes\n").unwrap();
            let twice = take(&f).unwrap();
            assert_eq!(twice.object, taken.object);
            assert_eq!(objects().len(), 1);
        });
    }

    /// Deterministic pseudo-random bytes: chunk boundaries depend on the
    /// bytes, so the same seed gives the same chunks every time.
    fn noise(len: usize, seed: u64) -> Vec<u8> {
        let mut x = (seed << 1) | 1;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect()
    }

    /// What one pass from the start cuts: the reference the parallel cuts
    /// must equal.
    fn serial_cuts(data: &[u8]) -> Vec<u64> {
        let mut out = Vec::new();
        let mut end = 0u64;
        for c in fastcdc::v2020::StreamCDC::new(data, CHUNK_MIN, CHUNK_AVG, CHUNK_MAX) {
            end += c.unwrap().data.len() as u64;
            out.push(end);
        }
        out
    }

    fn written(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
        let f = dir.join(name);
        fs::write(&f, data).unwrap();
        f
    }

    #[test]
    fn parallel_hash_equals_serial_hash() {
        with_home(|root| {
            // Noise; noise then a long run of one byte (chunks that all hit
            // the maximum, so the streams of two segments may never meet and
            // the true one is walked); and a length just past a segment.
            fs::create_dir_all(objects_dir()).unwrap();
            let seg = CUT_SEGMENT as usize;
            let mut runs = noise(seg + 9_001, 11);
            runs.extend(std::iter::repeat_n(7u8, seg + 1_000));
            let cases = [noise(2 * seg + 123_457, 5), runs, noise(seg + 1, 9)];
            for (n, data) in cases.iter().enumerate() {
                let f = written(root, &format!("big{n}.bin"), data);
                let meta = fs::metadata(&f).unwrap();
                // The cut points alone.
                let file = fs::File::open(&f).unwrap();
                for workers in [2, 5] {
                    assert_eq!(
                        parallel_cuts(&file, data.len() as u64, workers).unwrap(),
                        serial_cuts(data),
                        "case {n}, {workers} workers"
                    );
                }
                // And what is staged from them: same chunks, same recipe.
                let serial = prepare_chunked_serial(&f, &meta).unwrap();
                let parallel = prepare_chunked_parallel(&f, &meta, 4).unwrap();
                let names = |s: &Staged| -> Vec<(String, u64)> {
                    s.chunks.iter().map(|c| (c.sha256.clone(), c.len)).collect()
                };
                assert_eq!(names(&serial), names(&parallel), "case {n}");
                assert_eq!(serial.sha256, parallel.sha256, "case {n}");
                assert_eq!(serial.bytes, parallel.bytes, "case {n}");
                assert_eq!(serial.object(), parallel.object(), "case {n}");
                let want = hex::encode(Sha256::digest(data));
                assert_eq!(parallel.sha256, want, "case {n}");
                // Both leave the same recipe text behind once committed.
                let a = serial.commit().unwrap();
                let b = parallel.commit().unwrap();
                assert_eq!(a.object, b.object);
                assert_eq!(a.sha256, b.sha256);
                assert_eq!(
                    fs::read(recipe_path(&a.sha256)).unwrap(),
                    fs::read(recipe_path(&b.sha256)).unwrap()
                );
            }
        });
    }

    /// A worker that cannot write stops the run, with no thread left waiting
    /// and no temporary copy left in the tin.
    #[cfg(unix)]
    #[test]
    fn a_parallel_prepare_that_fails_stops_cleanly() {
        use std::os::unix::fs::PermissionsExt;
        with_home(|root| {
            fs::create_dir_all(objects_dir()).unwrap();
            let f = written(root, "big.bin", &noise(CUT_SEGMENT as usize + 5, 21));
            let meta = fs::metadata(&f).unwrap();
            fs::set_permissions(objects_dir(), fs::Permissions::from_mode(0o555)).unwrap();
            let denied = fs::write(objects_dir().join(".probe"), b"x").is_err();
            let ran = prepare_chunked_parallel(&f, &meta, 3);
            fs::set_permissions(objects_dir(), fs::Permissions::from_mode(0o755)).unwrap();
            if denied {
                assert!(
                    ran.is_err(),
                    "a tin that cannot be written to was written to"
                );
            }
            let left: Vec<_> = fs::read_dir(objects_dir())
                .unwrap()
                .flatten()
                .map(|e| e.path())
                .collect();
            assert!(left.is_empty(), "stray temp files: {left:?}");
        });
    }

    fn big_restore(len: usize, reps: usize) {
        with_home(|root| {
            let data = noise(len, 77);
            let f = written(root, "big.bin", &data);
            let st = stash(&f).unwrap();
            let recipe = read_recipe(&st.object).unwrap();
            assert_eq!(recipe.bytes, len as u64);
            let dest = root.join("restored.bin");
            // The serial reference, and the way restore chooses to go; the
            // times (`--nocapture`) are the best of `reps` alternating runs.
            let (mut serial, mut chosen) = (std::time::Duration::MAX, std::time::Duration::MAX);
            for _ in 0..reps {
                let t = std::time::Instant::now();
                restore_serial(&recipe, &st.object, &dest).unwrap();
                serial = serial.min(t.elapsed());
                assert!(fs::read(&dest).unwrap() == data, "serial restore differs");
                fs::remove_file(&dest).unwrap();
                let t = std::time::Instant::now();
                materialize(&st.object, &st.sha256, &dest).unwrap();
                chosen = chosen.min(t.elapsed());
                assert!(fs::read(&dest).unwrap() == data, "restore differs");
                fs::remove_file(&dest).unwrap();
            }
            eprintln!(
                "restore of {len} bytes, best of {reps}: serial {serial:?}, chosen {chosen:?}"
            );
            // Forced on four threads, whatever this machine has.
            restore_parallel(&recipe, &st.object, &dest, 4).unwrap();
            assert!(fs::read(&dest).unwrap() == data, "parallel restore differs");
            fs::remove_file(&dest).unwrap();
            // A corrupt chunk refuses the parallel restore, and nothing is
            // left where the file was to go.
            let victim = object_path(&recipe.chunks[recipe.chunks.len() / 2].sha256);
            lift_read_only(&victim);
            let good = fs::read(&victim).unwrap();
            let mut bad = good.clone();
            bad[0] ^= 1;
            fs::write(&victim, &bad).unwrap();
            let err = materialize_with_workers(&st.object, &st.sha256, &dest, 4).unwrap_err();
            assert!(err.to_string().contains("corrupt"), "{err}");
            assert!(!dest.exists());
            fs::write(&victim, &good).unwrap();
        });
    }

    /// `materialize`, with the parallel restore forced on.
    fn materialize_with_workers(
        snapshot: &Path,
        expect: &str,
        dest: &Path,
        workers: usize,
    ) -> io::Result<()> {
        let recipe = read_recipe(snapshot)?;
        let r = restore_parallel(&recipe, snapshot, dest, workers);
        let _ = expect;
        if r.is_err() {
            let _ = fs::remove_file(dest);
        }
        r
    }

    #[test]
    fn big_file_restore_is_exact() {
        // Debug builds hash slowly: a file two segments long is big enough
        // to be cut and restored in parallel.
        big_restore(
            if cfg!(debug_assertions) {
                24 * 1024 * 1024
            } else {
                64 * 1024 * 1024
            },
            1,
        );
    }

    /// The nightly's size: `cargo test --release -- --ignored big_file`.
    #[test]
    #[ignore]
    fn big_file_restore_is_exact_256_mb() {
        big_restore(256 * 1024 * 1024, 5);
    }

    #[test]
    fn a_one_byte_change_in_a_large_file_stores_one_new_chunk() {
        with_home(|root| {
            let f = root.join("big.bin");
            let mut data = noise(1 << 20, 7);
            fs::write(&f, &data).unwrap();
            let first = stash(&f).unwrap();
            assert!(is_recipe(&first.object), "{}", first.object.display());
            let n = objects().len();
            assert!((4..=64).contains(&n), "{n} chunks for 1 MiB");
            assert_eq!(recipes().len(), 1);
            let u = du();
            assert_eq!(u.logical, 1 << 20);
            assert_eq!(u.stored, 1 << 20);
            data[500_000] ^= 0xff;
            fs::write(&f, &data).unwrap();
            let second = stash(&f).unwrap();
            assert_ne!(second.sha256, first.sha256);
            assert_eq!(objects().len(), n + 1, "one chunk changed: one new object");
            assert_eq!(recipes().len(), 2);
            let u = du();
            assert_eq!(u.logical, 2 << 20);
            assert!(
                u.stored < (1 << 20) + u64::from(CHUNK_MAX),
                "stored {} for two versions of 1 MiB",
                u.stored
            );
            // The same content again costs nothing.
            stash(&f).unwrap();
            assert_eq!(objects().len(), n + 1);
            assert_eq!(recipes().len(), 2);
        });
    }

    #[test]
    fn an_insert_at_the_start_of_a_file_changes_one_chunk() {
        with_home(|root| {
            let f = root.join("big.bin");
            let data = noise(1 << 20, 11);
            fs::write(&f, &data).unwrap();
            stash(&f).unwrap();
            let n = objects().len();
            let mut shifted = b"seven b".to_vec();
            shifted.extend_from_slice(&data);
            fs::write(&f, &shifted).unwrap();
            stash(&f).unwrap();
            // The first chunk changes; a boundary right after it may move.
            assert!(
                objects().len() <= n + 2,
                "{} new objects after a 7-byte insert at the start",
                objects().len() - n
            );
        });
    }

    #[test]
    fn a_restore_from_chunks_is_byte_identical_and_hash_checked() {
        with_home(|root| {
            let f = root.join("big.bin");
            let data = noise(300_000, 3);
            fs::write(&f, &data).unwrap();
            let st = stash(&f).unwrap();
            let dest = root.join("restored.bin");
            materialize(&st.object, &st.sha256, &dest).unwrap();
            assert_eq!(fs::read(&dest).unwrap(), data);
            fs::remove_file(&dest).unwrap();
            // Corrupt one chunk in place: same length, other bytes.
            let recipe = read_recipe(&st.object).unwrap();
            let victim = object_path(&recipe.chunks[1].sha256);
            lift_read_only(&victim);
            let good = fs::read(&victim).unwrap();
            let mut bad = good.clone();
            bad[0] ^= 1;
            fs::write(&victim, &bad).unwrap();
            let err = materialize(&st.object, &st.sha256, &dest).unwrap_err();
            assert!(err.to_string().contains("corrupt"), "{err}");
            assert!(!dest.exists(), "nothing is left where the restore failed");
            assert_eq!(check_recipe(&st.object, &st.sha256).len(), 1);
            assert_eq!(verify(false).len(), 1);
            fs::write(&victim, &good).unwrap();
            assert!(verify(false).is_empty());
            // A journal that recorded other bytes under this recipe is refused too.
            let err = materialize(&st.object, "0000000000000000", &dest).unwrap_err();
            assert!(
                err.to_string()
                    .contains("other bytes than the journal recorded"),
                "{err}"
            );
            assert!(!dest.exists());
        });
    }

    #[test]
    fn a_missing_chunk_refuses_the_restore_and_writes_nothing() {
        with_home(|root| {
            let f = root.join("big.bin");
            fs::write(&f, noise(200_000, 5)).unwrap();
            let st = stash(&f).unwrap();
            let recipe = read_recipe(&st.object).unwrap();
            let gone = object_path(&recipe.chunks[0].sha256);
            lift_read_only(&gone);
            fs::remove_file(&gone).unwrap();
            let dest = root.join("restored.bin");
            let err = materialize(&st.object, &st.sha256, &dest).unwrap_err();
            assert!(err.to_string().contains("is missing"), "{err}");
            assert!(!dest.exists());
            let problems = check_recipe(&st.object, &st.sha256);
            assert_eq!(problems.len(), 1, "{problems:?}");
            assert!(problems[0].contains("missing"), "{problems:?}");
            assert_eq!(verify(false).len(), 1);
            // The next stash of the same file puts the chunk back.
            stash(&f).unwrap();
            assert!(gone.is_file());
            materialize(&st.object, &st.sha256, &dest).unwrap();
        });
    }

    #[test]
    fn gc_keeps_every_chunk_a_live_recipe_names() {
        with_home(|root| {
            use super::super::journal::{Before, BeforeState, Journal};
            use super::super::runs::RunRecord;
            use super::super::Op;
            let live = root.join("live.bin");
            fs::write(&live, noise(400_000, 9)).unwrap();
            let kept = stash(&live).unwrap();
            let stray = root.join("stray.bin");
            fs::write(&stray, noise(400_000, 10)).unwrap();
            let gone = stash(&stray).unwrap();
            let small = root.join("small.txt");
            fs::write(&small, "small\n").unwrap();
            let gone_small = stash(&small).unwrap();
            // A run whose journal refers to the live recipe.
            let rec = RunRecord::begin(Path::new("s.cig"), "", &[], "run");
            rec.save().unwrap();
            let mut j = Journal::open(Some(rec.dir())).unwrap();
            j.record_with_before(
                1,
                Op::HopChanged {
                    path: live.clone(),
                    change: "modified".into(),
                    tinned: true,
                    kept: Some("tin".into()),
                    left: None,
                },
                vec![BeforeState {
                    path: live.clone(),
                    before: Before::File {
                        snapshot: kept.object.clone(),
                        sha256: kept.sha256.clone(),
                        bytes: kept.bytes,
                        mode: kept.mode,
                        mtime_ns: kept.mtime_ns,
                        git: None,
                    },
                }],
                None,
                false,
            )
            .unwrap();
            let live_chunks = read_recipe(&kept.object).unwrap().chunks.len();
            let (removed, _) = gc(false);
            assert!(removed >= 3, "{removed}");
            assert!(kept.object.is_file());
            for c in read_recipe(&kept.object).unwrap().chunks {
                assert!(object_path(&c.sha256).is_file());
            }
            assert_eq!(objects().len(), live_chunks);
            assert!(!gone.object.exists() && !gone_small.object.exists());
            assert_eq!(recipes().len(), 1);
            assert!(verify(false).is_empty());
        });
    }

    /// An object a 1.4 cig wrote (a large file kept whole, named by its
    /// hash) restores as it always did.
    #[test]
    fn a_1_4_whole_file_object_still_restores() {
        with_home(|root| {
            let data = noise(200_000, 12);
            let sha = hex::encode(Sha256::digest(&data));
            let object = object_path(&sha);
            fs::create_dir_all(object.parent().unwrap()).unwrap();
            fs::write(&object, &data).unwrap();
            make_read_only(&object).unwrap();
            assert!(holds(&object) && !is_recipe(&object));
            let dest = root.join("restored.bin");
            materialize(&object, &sha, &dest).unwrap();
            assert_eq!(fs::read(&dest).unwrap(), data);
            assert_eq!(du().logical, 200_000);
            fs::remove_file(&dest).unwrap();
            // Still named by its content: a corrupt one is refused.
            lift_read_only(&object);
            fs::write(&object, noise(200_000, 13)).unwrap();
            assert!(materialize(&object, &sha, &dest).is_err());
            assert!(!dest.exists());
        });
    }

    #[test]
    fn prepare_then_commit_leaves_no_object_until_commit() {
        with_home(|root| {
            let f = root.join("f.txt");
            fs::write(&f, "staged\n").unwrap();
            let staged = prepare(&f).unwrap();
            let object = staged.object();
            assert!(!object.exists(), "nothing under the final name yet");
            assert!(objects().is_empty(), "a temporary copy is not an object");
            let st = staged.commit().unwrap();
            assert_eq!(st.object, object);
            assert!(object.is_file());
            assert_eq!(objects().len(), 1);
        });
    }
}

/// A copy another tool keeps in the tin under a label of its own (`cig tin
/// keep`): the last list it verified, the entry it wrote somewhere else. The
/// bytes are a sealed object, the record names it, and the ledger has a
/// line for each keep, so a copy deleted along with the file it backs is
/// missed rather than forgotten.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct KeptCopy {
    pub label: String,
    pub name: String,
    pub sha256: String,
    pub object: PathBuf,
    pub bytes: u64,
    pub at: String,
}

pub fn keeps_dir() -> PathBuf {
    dir().join("keeps")
}

fn keep_record(label: &str, name: &str) -> PathBuf {
    keeps_dir().join(label).join(format!("{name}.json"))
}

/// A label or a name a keep may use: letters, digits, dashes and dots
/// (not leading), up to 64.
pub fn good_keep_word(w: &str) -> bool {
    !w.is_empty()
        && w.len() <= 64
        && !w.starts_with('.')
        && w.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.' || b == b'_')
}

/// Every copy kept in the tin, as their records say.
pub fn kept_copies() -> Vec<KeptCopy> {
    let mut out = Vec::new();
    let Ok(labels) = fs::read_dir(keeps_dir()) else {
        return out;
    };
    for l in labels.flatten() {
        let Ok(names) = fs::read_dir(l.path()) else {
            continue;
        };
        for n in names.flatten() {
            if let Ok(text) = fs::read_to_string(n.path()) {
                if let Ok(k) = serde_json::from_str::<KeptCopy>(&text) {
                    out.push(k);
                }
            }
        }
    }
    out
}

/// Keep a sealed copy of `file` under `label` and `name`, replacing the one
/// kept before, and say so in the ledger.
pub fn keep_copy(file: &Path, label: &str, name: &str) -> io::Result<KeptCopy> {
    if !good_keep_word(label) || !good_keep_word(name) {
        return Err(io::Error::other(
            "a label and a name are letters, digits, dashes, dots and underscores, up to 64",
        ));
    }
    let meta = fs::metadata(file)?;
    if !meta.is_file() {
        return Err(io::Error::other(format!(
            "{} is not a file",
            file.display()
        )));
    }
    let st = prepare_sealed(file)?.commit()?;
    let rec = KeptCopy {
        label: label.to_string(),
        name: name.to_string(),
        sha256: st.sha256.clone(),
        object: st.object.clone(),
        bytes: st.bytes,
        at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    };
    let path = keep_record(label, name);
    let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
    fs::create_dir_all(&parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(keeps_dir(), fs::Permissions::from_mode(0o700));
        let _ = fs::set_permissions(&parent, fs::Permissions::from_mode(0o700));
    }
    let tmp = parent.join(format!(".{name}.tmp"));
    let _ = fs::remove_file(&tmp);
    fs::write(
        &tmp,
        serde_json::to_string_pretty(&rec).map_err(io::Error::other)?,
    )?;
    fs::rename(&tmp, &path)?;
    super::ledger::append(
        "keep",
        &format!("{label}/{name}"),
        &st.sha256,
        "kept",
        st.bytes,
    )
    .map_err(io::Error::other)?;
    Ok(rec)
}

/// What `cig tin give` finds for a label and a name.
pub enum Given {
    /// The copy, as it was kept.
    Copy(Vec<u8>, KeptCopy),
    /// Nothing was ever kept under that name.
    Nothing,
    /// The ledger says a copy was kept, and the tin no longer holds it as
    /// kept (the record or the object is gone, or does not match).
    Gone(String),
}

pub fn give_copy(label: &str, name: &str) -> Given {
    let last = super::ledger::load().ok().and_then(|l| {
        l.lines
            .iter()
            .rev()
            .find(|x| x.kind == "keep" && x.run == format!("{label}/{name}"))
            .cloned()
    });
    let rec = fs::read_to_string(keep_record(label, name))
        .ok()
        .and_then(|t| serde_json::from_str::<KeptCopy>(&t).ok());
    let Some(rec) = rec else {
        return match last {
            Some(l) => Given::Gone(format!(
                "the ledger says {name} was kept under {label} on {}, and the tin no longer holds its record",
                l.t
            )),
            None => Given::Nothing,
        };
    };
    if let Some(l) = &last {
        if l.head != rec.sha256 {
            return Given::Gone(format!(
                "the record of {name} under {label} names other bytes than the ledger's last keep ({})",
                l.t
            ));
        }
    } else {
        return Given::Gone(format!(
            "the record of {name} under {label} has no keep in the ledger: it was not written by cig tin keep"
        ));
    }
    let tmp = std::env::temp_dir().join(format!(
        "cig-give-{}-{}",
        std::process::id(),
        rec.sha256.get(..12).unwrap_or("x")
    ));
    let got = materialize(&rec.object, &rec.sha256, &tmp).and_then(|()| fs::read(&tmp));
    let _ = fs::remove_file(&tmp);
    match got {
        Ok(bytes) => Given::Copy(bytes, rec),
        Err(e) => Given::Gone(format!(
            "the tin no longer holds the copy of {name} under {label} as it was kept: {e}"
        )),
    }
}
