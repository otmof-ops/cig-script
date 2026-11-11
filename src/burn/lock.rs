// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The pack lock: one run at a time on a pack. Hop watching reads every
//! change in the pack between its two scans as the hop's, so a second run
//! writing in the same pack would have its output rolled back by the first
//! run's failure, and vice versa. A run takes an advisory lock per pack root
//! under `~/.cigscript/locks` for as long as it lives; another run on the
//! same root, or on one that contains it or lies inside it, is refused
//! (E759) with the holder named.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub fn dir() -> PathBuf {
    super::runs::home_dir().join("locks")
}

/// The locks a run holds, released when it is dropped.
#[derive(Debug)]
pub struct PackLock {
    _files: Vec<fs::File>,
}

/// Why a lock could not be taken.
#[derive(Debug)]
pub enum Busy {
    /// Another run holds a lock on `root`, or on a root overlapping it.
    Held {
        root: PathBuf,
        holder: String,
    },
    Io(io::Error),
}

impl From<io::Error> for Busy {
    fn from(e: io::Error) -> Self {
        Busy::Io(e)
    }
}

/// The canonical form a root is locked under: the real directory, so two
/// spellings of one place lock one file.
fn key_of(root: &Path) -> PathBuf {
    super::scope_path(root)
}

fn lock_path(key: &Path) -> PathBuf {
    use sha2::{Digest, Sha256};
    let hash = hex::encode(Sha256::digest(key.to_string_lossy().as_bytes()));
    dir().join(format!("{}.lock", &hash[..32]))
}

/// The holder's note beside the lock, readable while the lock is held.
fn who_path(lock: &Path) -> PathBuf {
    lock.with_extension("who")
}

/// Try the lock on `path` without waiting. `Ok(None)` means someone holds it.
fn try_lock(path: &Path) -> io::Result<Option<fs::File>> {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let f = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?;
        // SAFETY: flock on a descriptor this handle owns.
        let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            Ok(Some(f))
        } else {
            let e = io::Error::last_os_error();
            match e.raw_os_error() {
                Some(libc::EWOULDBLOCK) => Ok(None),
                _ => Err(e),
            }
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // An exclusive open: no sharing at all, so a second open fails with
        // a sharing violation for as long as the first handle is open.
        match fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .share_mode(0)
            .open(path)
        {
            Ok(f) => Ok(Some(f)),
            Err(e) if e.raw_os_error() == Some(32) => Ok(None),
            Err(e) => Err(e),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .map(Some)
    }
}

/// One root contains the other, or they are the same place.
fn overlap(a: &Path, b: &Path) -> bool {
    a == b || a.starts_with(b) || b.starts_with(a)
}

/// The run id a holder's note names (`run <id> (pid ...)`).
fn holder_id(note: &str) -> Option<&str> {
    note.lines().next()?.strip_prefix("run ")?.split(' ').next()
}

/// Is the run holding this lock the one that started us? A cig a hop runs
/// (`cig run inner.cig` as a task) works inside the parent's pack while the
/// parent waits for it: the parent's hop watch brackets the whole child run,
/// so nothing interleaves, and the child's own writes are the hop's.
fn is_parent(note: &str) -> bool {
    match (holder_id(note), std::env::var("CIG_RUN_ID")) {
        (Some(id), Ok(parent)) => !id.is_empty() && id == parent,
        _ => false,
    }
}

/// Take the lock on every root for run `id`. Every lock is released again
/// when any one cannot be taken.
pub fn acquire(roots: &[PathBuf], id: &str) -> Result<PackLock, Busy> {
    fs::create_dir_all(dir())?;
    let keys: Vec<PathBuf> = roots.iter().map(|r| key_of(r)).collect();
    // A run already holding a root that contains or lies inside one of
    // ours is as much in the way as one holding the root itself.
    if let Ok(entries) = fs::read_dir(dir()) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().is_none_or(|x| x != "who") {
                continue;
            }
            let Ok(note) = fs::read_to_string(&p) else {
                continue;
            };
            let Some(their_root) = note.lines().nth(1).map(PathBuf::from) else {
                continue;
            };
            if !keys.iter().any(|k| overlap(k, &their_root)) {
                continue;
            }
            let lock = p.with_extension("lock");
            if try_lock(&lock)?.is_none() {
                if is_parent(&note) {
                    // Our parent holds it; it waits for us.
                    return Ok(PackLock { _files: Vec::new() });
                }
                return Err(Busy::Held {
                    root: their_root,
                    holder: note.lines().next().unwrap_or("another run").to_string(),
                });
            }
        }
    }
    let mut held = Vec::new();
    for key in &keys {
        let path = lock_path(key);
        match try_lock(&path)? {
            Some(f) => {
                let note = format!(
                    "run {id} (pid {}, started {})\n{}\n",
                    std::process::id(),
                    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    key.display()
                );
                if let Ok(mut w) = fs::File::create(who_path(&path)) {
                    let _ = w.write_all(note.as_bytes());
                }
                held.push(f);
            }
            None => {
                let note = fs::read_to_string(who_path(&path)).unwrap_or_default();
                if is_parent(&note) {
                    return Ok(PackLock { _files: Vec::new() });
                }
                let holder = note
                    .lines()
                    .next()
                    .map(str::to_string)
                    .unwrap_or_else(|| "another run".to_string());
                return Err(Busy::Held {
                    root: key.clone(),
                    holder,
                });
            }
        }
    }
    Ok(PackLock { _files: held })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_lock_on_the_same_or_an_overlapping_root_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        // The lock directory follows CIGSCRIPT_HOME; this test keeps to its
        // own by pointing the whole home at the scratch directory, under the
        // lock every test that touches the environment takes.
        let _g = super::super::test_env_lock();
        let old = std::env::var_os("CIGSCRIPT_HOME");
        std::env::set_var("CIGSCRIPT_HOME", tmp.path().join("home"));
        let root = tmp.path().join("p");
        fs::create_dir_all(root.join("sub")).unwrap();
        let first = acquire(std::slice::from_ref(&root), "one").expect("first lock");
        match acquire(std::slice::from_ref(&root), "two") {
            Err(Busy::Held { holder, .. }) => assert!(holder.contains("run one"), "{holder}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
        match acquire(&[root.join("sub")], "three") {
            Err(Busy::Held { .. }) => {}
            other => panic!("an inner root overlaps: {other:?}"),
        }
        drop(first);
        // A child another test forks at this moment holds a copy of the
        // lock's descriptor until it execs (O_CLOEXEC closes it then), and
        // flock lasts while any copy is open: the lock is free within
        // moments of the holder letting go, not always at once.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let four = loop {
            match acquire(&[root.join("sub")], "four") {
                Ok(l) => break l,
                Err(Busy::Held { .. }) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(20))
                }
                Err(e) => panic!("free after the holder let go: {e:?}"),
            }
        };
        drop(four);
        match old {
            Some(v) => std::env::set_var("CIGSCRIPT_HOME", v),
            None => std::env::remove_var("CIGSCRIPT_HOME"),
        }
    }
}
