// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! The run index (#149): one compact file under the cig home, updated as
//! runs start, finish, are pruned, forgotten or unburned, so `cig runs` at
//! ten thousand runs no longer walks every run folder. The index is an
//! append-only log of lines: `{"op":"set", ...run record}` for a run
//! added or updated, `{"op":"rm","id":"..."}` for one gone. Readers
//! replay the file front to back; the last line wins per id, and a
//! removed id drops out. A missing file, or a torn or unparseable line,
//! is a cache miss: the index is rebuilt from the run folders (and said
//! so once), then the fresh view is written back.
//!
//! Every write takes an exclusive advisory lock on `runs-index.lock`, so
//! two concurrent runs never interleave. Reads are lockless: an append
//! under the lock is either fully there or not at all, and the tail line
//! of a crashed writer is thrown out on read.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use super::runs::{home_dir, runs_dir, RunRecord};

/// The index file: one JSONL line per edit.
pub fn path() -> PathBuf {
    home_dir().join("runs-index.jsonl")
}

/// The advisory lock that serialises writers.
fn lock_path() -> PathBuf {
    home_dir().join("runs-index.lock")
}

/// What one line of the index carries. The `Set` variant boxes the full
/// `RunRecord` so the enum stays small (clippy's `large_enum_variant`).
///
/// The `mtime_ns` and `size` fields are of the `run.json` file at the
/// time the line was written: a `list()` stats each run's `run.json` and
/// re-reads it from disk only when one of them does not match, so a
/// hand-edited record (the `a_damaged_or_renamed_run_record_still_lists`
/// and `interrupted_run_is_recognised_counted_from_the_journal` tests)
/// is caught without reading every record every time.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Line {
    /// A run was added or updated.
    Set {
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        pinned: bool,
        /// The modified time of `run.json` when this line was written,
        /// as a Unix epoch in nanoseconds (serde_json does not carry
        /// `u128` in its self-describing form, and `u64` is good for
        /// over five hundred years past the epoch). 0 for a line from
        /// a filesystem that could not report one.
        #[serde(default)]
        mtime_ns: u64,
        /// The size of `run.json` when this line was written, in bytes.
        #[serde(default)]
        size: u64,
        #[serde(flatten)]
        rec: Box<RunRecord>,
    },
    /// The run's folder was removed (prune, forget, budget).
    Rm { id: String },
}

/// `list()` said the index was missing or torn and rebuilt it. Said once
/// per process on stderr.
static REBUILT_NOTE: OnceLock<()> = OnceLock::new();

/// Read the index and return the runs it names, newest first.
///
/// The read is fast at ten thousand runs (#149): one JSONL file is
/// parsed to get the record of every known run; the runs directory is
/// then listed (one syscall) and each run's `run.json` is stat'ed (one
/// syscall) to catch hand-edits and hand-deletes. A record is re-read
/// from disk only when the index's (mtime, size) do not match it. A
/// still-running record's journal is opened only to update the burn
/// count when its process has died; the FIFO test, and `cig run`'s
/// start, open no journal of any other run.
///
/// A torn index (an append crashed mid-write), an unparseable line, or
/// a missing file is a cache miss: the view is built from a folder
/// scan (`RunRecord::load` on each) and the fresh index is written
/// back. Said so once per process.
pub fn list() -> io::Result<Vec<RunRecord>> {
    let indexed = match read_index_as_map() {
        Ok(m) => Some(m),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        // A torn or unparseable index is a cache miss, not a hard
        // error: fall through to a full folder scan.
        Err(e) => {
            REBUILT_NOTE.get_or_init(|| {
                eprintln!(
                    "note: the runs index was torn or unparseable ({e}); rebuilding from the run folders"
                );
            });
            None
        }
    };
    // The folder scan: one `read_dir`, then one `stat` per entry. A
    // run id that appears here but not in the index is a hand-write
    // or a run from an older cig; a run id in the index but not here
    // was hand-deleted.
    let runs = runs_dir();
    let dir_entries = match fs::read_dir(&runs) {
        Ok(it) => it,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out: Vec<RunRecord> = Vec::new();
    let mut reloaded = false;
    for entry in dir_entries {
        let Ok(entry) = entry else { continue };
        let id = entry.file_name().to_string_lossy().to_string();
        if id.starts_with('.') {
            continue;
        }
        let dir = entry.path();
        let rj = dir.join("run.json");
        let meta = match fs::metadata(&rj) {
            Ok(m) => m,
            // A folder without a `run.json` is nothing of ours (stray
            // scratch, half-made by a crash before the first save).
            Err(_) => continue,
        };
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        let size = meta.len();
        let cached = indexed.as_ref().and_then(|m| m.get(&id));
        let in_sync = cached.is_some_and(|c| c.mtime_ns == mtime_ns && c.size == size);
        let mut rec = if in_sync {
            let mut rec = *cached.unwrap().rec.clone();
            rec.loaded_from = Some(dir.clone());
            rec
        } else {
            reloaded = true;
            match RunRecord::load(&dir) {
                Ok(rec) => rec,
                Err(e) if e.kind() == io::ErrorKind::Unsupported => {
                    // A record from a newer cig: show it as such, keep
                    // its id and version so `cig runs` names them.
                    let mut rec = RunRecord::damaged(&id, &e);
                    rec.status = "newer".to_string();
                    if let Ok(text) = fs::read_to_string(&rj) {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                            rec.version = v["version"].as_str().unwrap_or("").to_string();
                            rec.format = v["format"].as_u64().unwrap_or(0) as u32;
                            rec.script = v["script"].as_str().unwrap_or("").to_string();
                        }
                    }
                    rec.loaded_from = Some(dir.clone());
                    rec
                }
                Err(e) => {
                    let mut rec = RunRecord::damaged(&id, &e);
                    rec.loaded_from = Some(dir.clone());
                    rec
                }
            }
        };
        // A still-running record whose process has died flips to
        // `interrupted` and reads its journal for the real burn count.
        // We do this only here, for the small handful of such records,
        // not on every indexed entry.
        let may_be_interrupted = matches!(rec.status.as_str(), "running" | "damaged");
        let alive = rec
            .pid
            .map(|pid| super::runs::pid_alive(pid, rec.pid_started.as_deref()))
            .unwrap_or(false);
        if may_be_interrupted && !alive {
            rec.refresh_from_journal();
        }
        out.push(rec);
    }
    // Newest first; `started` carries milliseconds, ids only seconds.
    out.sort_by(|a, b| b.started.cmp(&a.started).then_with(|| b.id.cmp(&a.id)));
    // If we reloaded any record, or found folders not in the index,
    // rewrite the index so the next list is faster. Best effort; a
    // failure means a longer file next time, not a wrong view.
    if reloaded || indexed.as_ref().is_none_or(|m| m.len() != out.len()) {
        let _ = write_fresh(&out);
    }
    Ok(out)
}

/// The parsed index as a map from id to its entry. `NotFound` means no
/// file; `InvalidData` means a torn or unparseable file.
#[derive(Debug)]
struct CachedEntry {
    rec: Box<RunRecord>,
    mtime_ns: u64,
    size: u64,
}

fn read_index_as_map() -> io::Result<std::collections::HashMap<String, CachedEntry>> {
    let text = fs::read_to_string(path())?;
    if text.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    if !text.ends_with('\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the runs index ends mid-line (a crashed append)",
        ));
    }
    let mut map: std::collections::HashMap<String, CachedEntry> =
        std::collections::HashMap::with_capacity(1024);
    for raw in text.lines() {
        if raw.trim().is_empty() {
            continue;
        }
        let line: Line =
            serde_json::from_str(raw).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        match line {
            Line::Set {
                rec,
                mtime_ns,
                size,
                ..
            } => {
                map.insert(
                    rec.id.clone(),
                    CachedEntry {
                        rec,
                        mtime_ns,
                        size,
                    },
                );
            }
            Line::Rm { id } => {
                map.remove(&id);
            }
        }
    }
    Ok(map)
}

/// Add or update a run in the index. Best effort: a failure here is
/// logged but never fails the save of `run.json` itself (the folder is
/// the source of truth; the index is a cache).
pub fn upsert(rec: &RunRecord) {
    let dir = rec
        .loaded_from
        .clone()
        .unwrap_or_else(|| runs_dir().join(&rec.id));
    let pinned = dir.join(crate::burn::runs::PIN_FILE).exists();
    let (mtime_ns, size) = stat_run_json(&dir).unwrap_or((0, 0));
    if let Err(e) = append(&Line::Set {
        pinned,
        mtime_ns,
        size,
        rec: Box::new(rec.clone()),
    }) {
        eprintln!("note: could not update the runs index: {e}");
    }
}

/// `(mtime_ns, size)` of the run's `run.json`, to be stored in the index
/// and checked on the next read. Returns None if the file cannot be
/// stat'ed (the caller treats that as "unknown, re-read next time").
fn stat_run_json(run_dir: &Path) -> Option<(u64, u64)> {
    let meta = fs::metadata(run_dir.join("run.json")).ok()?;
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    Some((mtime_ns, meta.len()))
}

/// Mark the run gone (prune, forget, budget). Best effort.
pub fn remove(id: &str) {
    if let Err(e) = append(&Line::Rm { id: id.to_string() }) {
        eprintln!("note: could not update the runs index: {e}");
    }
}

/// Write the full view (from a folder scan), replacing whatever was
/// there. Used when `--prune` or the retention pass has already reshaped
/// the history and we want one line per run again.
pub fn write_fresh(view: &[RunRecord]) -> io::Result<()> {
    let p = path();
    let home = home_dir();
    fs::create_dir_all(&home)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&home, fs::Permissions::from_mode(0o700));
    }
    let _lock = exclusive_lock(&lock_path())?;
    let tmp = p.with_extension("jsonl.tmp");
    let mut f = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&tmp)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
    }
    for rec in view {
        let dir = rec
            .loaded_from
            .clone()
            .unwrap_or_else(|| runs_dir().join(&rec.id));
        let pinned = dir.join(crate::burn::runs::PIN_FILE).exists();
        let (mtime_ns, size) = stat_run_json(&dir).unwrap_or((0, 0));
        let line = Line::Set {
            pinned,
            mtime_ns,
            size,
            rec: Box::new(rec.clone()),
        };
        let text = serde_json::to_string(&line)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        f.write_all(text.as_bytes())?;
        f.write_all(b"\n")?;
    }
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, &p)?;
    super::journal::sync_dir_pub(&home)?;
    Ok(())
}

/// Append one line under the exclusive index lock, fsync, release.
fn append(line: &Line) -> io::Result<()> {
    let p = path();
    let home = home_dir();
    fs::create_dir_all(&home)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&home, fs::Permissions::from_mode(0o700));
    }
    let _lock = exclusive_lock(&lock_path())?;
    let mut opts = fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&p)?;
    let text =
        serde_json::to_string(line).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    f.write_all(text.as_bytes())?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}

/// A blocking exclusive advisory lock on `lock_path`. The file is created
/// if it does not exist.
fn exclusive_lock(path: &Path) -> io::Result<fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let f = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?;
        // SAFETY: flock on a descriptor this handle owns.
        let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) };
        if rc == 0 {
            Ok(f)
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Spin on a sharing violation: Windows has no blocking flock
        // equivalent and we are serialising index writers, not waiting
        // on a human.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .share_mode(0)
                .open(path)
            {
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
        fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A torn index file (no trailing newline: a crashed append) is a
    /// cache miss — `read_index_as_map` returns `InvalidData` — not a
    /// silent accept of the lines before the tear. The full folder
    /// scan the `list()` caller falls through to is covered by the CLI
    /// test `runs_index_survives_a_crash`.
    #[test]
    fn a_torn_tail_line_reads_as_invalid_data() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = crate::burn::test_env_lock();
        let old = std::env::var_os("CIGSCRIPT_HOME");
        std::env::set_var("CIGSCRIPT_HOME", tmp.path().join("home"));
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let p = path();
        fs::write(&p, "{\"op\":\"set\",\"id\":\"half").unwrap();
        let e = read_index_as_map().expect_err("torn index is a cache miss");
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        match old {
            Some(v) => std::env::set_var("CIGSCRIPT_HOME", v),
            None => std::env::remove_var("CIGSCRIPT_HOME"),
        }
    }

    /// A `set` line followed by an `rm` of the same id leaves the
    /// cache without that id. Replay semantics.
    #[test]
    fn set_then_rm_drops_a_run_from_the_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = crate::burn::test_env_lock();
        let old = std::env::var_os("CIGSCRIPT_HOME");
        std::env::set_var("CIGSCRIPT_HOME", tmp.path().join("home"));
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let p = path();
        let mut rec = RunRecord::damaged("20260101T000000-aaaaaa", &io::Error::other("stub"));
        rec.status = "ok".to_string();
        rec.started = "2026-01-01T00:00:00.000Z".to_string();
        let a = serde_json::to_string(&Line::Set {
            pinned: false,
            mtime_ns: 0,
            size: 0,
            rec: Box::new(rec),
        })
        .unwrap();
        let rm = serde_json::to_string(&Line::Rm {
            id: "20260101T000000-aaaaaa".to_string(),
        })
        .unwrap();
        fs::write(&p, format!("{a}\n{rm}\n")).unwrap();
        let map = read_index_as_map().unwrap();
        assert!(map.is_empty(), "rm after set should drop the id");
        match old {
            Some(v) => std::env::set_var("CIGSCRIPT_HOME", v),
            None => std::env::remove_var("CIGSCRIPT_HOME"),
        }
    }
}
