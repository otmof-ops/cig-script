// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! `cig tin rekey`: make a new master seal key and re-seal every sealed
//! object the tin or a run's snapshots hold under it, then swap the keeper.
//!
//! Crash-safe: while the walk is in flight the old key sits at
//! `seal.key.old` and the new one at `seal.key.new`, so a sealed object is
//! readable under one or the other no matter when a crash lands. After the
//! walk the keeper (keyring or the primary key file, whichever `seal_key`
//! says) is swapped, and only once the keeper is proven to carry the new
//! key are both fallback files wiped and removed. A second `cig tin rekey`
//! after a crash picks up where the walk left off: it does not make a new
//! key, it finishes the one in flight.

use super::seal::{self, Key};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The rekey's own lock file: one per tin, in the keys directory. A
/// shared opener (a `cig run` that will seal or sign) holds this file's
/// shared lock for its whole life, so a rekey's exclusive try-lock fails
/// while any such process lives.
pub const REKEY_LOCK: &str = "rekey.lock";

/// The "a rekey has started and may not have finished" marker. Created
/// at rekey start, removed on a clean finish. Its presence is what
/// lets `seal::secondary_keys` honour the on-disk stashes as fallback
/// keys. A planted stash with no marker on disk is ignored.
pub const REKEY_MARKER: &str = "rekey.in-progress";

/// What a rekey (or a resume of one) did, for the operator and `--json`.
#[derive(Debug, serde::Serialize)]
pub struct Done {
    /// What the keeper now holds: `cig tin verify` opens with this.
    pub new_fingerprint: String,
    /// What the keeper held before this rekey began.
    pub old_fingerprint: String,
    /// Sealed objects and snapshots walked.
    pub walked: usize,
    /// Of those, the ones found already under the new key.
    pub already: usize,
    /// Of those, the ones re-sealed from the old key.
    pub resealed: usize,
    /// True when this run resumed an earlier rekey instead of starting one.
    pub resumed: bool,
}

/// The rekey lock's path.
pub fn rekey_lock_path() -> PathBuf {
    seal::keys_dir().join(REKEY_LOCK)
}

/// Is a rekey marker on disk?
pub fn rekey_in_progress() -> bool {
    seal::keys_dir().join(REKEY_MARKER).exists()
}

/// Why a rekey lock could not be taken.
#[derive(Debug)]
pub enum LockBusy {
    /// A rekey is in flight (marker on disk): the other process holds the
    /// exclusive lock, or it crashed and left the marker.
    OtherRekey,
    /// A run (seal or signed append) is in flight: no marker, but the
    /// shared lock is held somewhere.
    Busy,
    Io(io::Error),
}

/// Take an exclusive, non-blocking lock on the rekey lock file. The
/// returned handle must be kept alive for the lock to hold.
#[cfg(unix)]
fn acquire_rekey_lock() -> Result<fs::File, LockBusy> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::AsRawFd;
    let f = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(rekey_lock_path())
        .map_err(LockBusy::Io)?;
    // SAFETY: flock on a descriptor this handle owns.
    let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        Ok(f)
    } else {
        let e = io::Error::last_os_error();
        match e.raw_os_error() {
            Some(libc::EWOULDBLOCK) => {
                if rekey_in_progress() {
                    Err(LockBusy::OtherRekey)
                } else {
                    Err(LockBusy::Busy)
                }
            }
            _ => Err(LockBusy::Io(e)),
        }
    }
}

#[cfg(windows)]
fn acquire_rekey_lock() -> Result<fs::File, LockBusy> {
    use std::os::windows::fs::OpenOptionsExt;
    match fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .share_mode(0)
        .open(rekey_lock_path())
    {
        Ok(f) => Ok(f),
        Err(e) if e.raw_os_error() == Some(32) => {
            if rekey_in_progress() {
                Err(LockBusy::OtherRekey)
            } else {
                Err(LockBusy::Busy)
            }
        }
        Err(e) => Err(LockBusy::Io(e)),
    }
}

#[cfg(not(any(unix, windows)))]
fn acquire_rekey_lock() -> Result<fs::File, LockBusy> {
    fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(rekey_lock_path())
        .map_err(LockBusy::Io)
}

/// A process-wide shared (blocking) lock on the rekey lock file: every
/// cig process that reads the seal key takes this once, through
/// `seal::key()`, and keeps it for the lifetime of the process. A rekey
/// that starts while any such process lives finds the exclusive lock
/// held and refuses (see `rekey()`); a process that starts while a rekey
/// holds the exclusive lock blocks here with one line on stderr until
/// the rekey finishes, and then reads the new key.
///
/// Fail closed (SEC-004 N3): an open or `flock` error returns `Err`,
/// which `seal::hold_shared_rekey_lock` caches and `seal::key` propagates,
/// so every seal/sign call is refused with the reason rather than carrying
/// on unprotected. The reported errno is the one from the blocking call
/// that actually refused, not from the non-blocking probe before it.
#[cfg(unix)]
pub fn take_process_shared_lock() -> Result<fs::File, String> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::AsRawFd;
    // Make or verify the keys folder (#176): private from the start,
    // owned by us. A folder another user owns, or that is group- or
    // world-writable, refuses here with `E721`, so a run or rekey
    // never reads a planted lock, seal.key or ledger.jsonl.
    let _keys_dir = seal::ensure_keys_dir()?;
    // The lock file itself must be ours, and not a symlink someone else
    // can swing at another path. Checked before we open it.
    seal::check_private_state_file(&rekey_lock_path())?;
    let f = match fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(rekey_lock_path())
    {
        Ok(f) => f,
        Err(e) => {
            return Err(format!(
                "could not open the rekey lock {} ({e}); refusing to proceed unprotected against a concurrent cig tin rekey",
                rekey_lock_path().display()
            ));
        }
    };
    // Probe with LOCK_SH|LOCK_NB first: if it would block, say so on
    // stderr before the thread goes to sleep, so a user running `cig
    // run` while a rekey is in flight sees why nothing moved.
    // SAFETY: flock on a descriptor this handle owns.
    let probe = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) };
    if probe == 0 {
        return Ok(f);
    }
    let probe_err = io::Error::last_os_error();
    if probe_err.raw_os_error() == Some(libc::EWOULDBLOCK) {
        eprintln!(
            "note: cig tin rekey is in progress; waiting for it to finish before this run touches the tin"
        );
        // SAFETY: flock on a descriptor this handle owns. Blocking.
        let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_SH) };
        if rc == 0 {
            return Ok(f);
        }
        // The blocking call's own errno, not the probe's.
        let e = io::Error::last_os_error();
        return Err(format!(
            "could not take the rekey lock on {} ({e}); refusing to proceed unprotected against a concurrent cig tin rekey",
            rekey_lock_path().display()
        ));
    }
    Err(format!(
        "could not take the rekey lock on {} ({probe_err}); refusing to proceed unprotected against a concurrent cig tin rekey",
        rekey_lock_path().display()
    ))
}

#[cfg(windows)]
pub fn take_process_shared_lock() -> Result<fs::File, String> {
    use std::os::windows::fs::OpenOptionsExt;
    let _keys_dir = seal::ensure_keys_dir()?;
    // FILE_SHARE_READ | FILE_SHARE_WRITE: other shared holders can open
    // the file too, but a share_mode(0) exclusive open (the rekey) has
    // to wait. A sharing violation (error 32) means a rekey is in flight:
    // wait and retry rather than return unprotected (SEC-004 N3). On a
    // filesystem that cannot share-open the file at all for another
    // reason, fail closed with the OS error.
    const FILE_SHARE_READ: u32 = 1;
    const FILE_SHARE_WRITE: u32 = 2;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    // Up to ~30s of waits, at 100ms each. A rekey holding the file
    // longer than that is an abandoned run that needs operator care;
    // reporting instead of hanging is kinder.
    let mut waited_once = false;
    for attempt in 0..300 {
        match fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(rekey_lock_path())
        {
            Ok(f) => return Ok(f),
            Err(e) if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION) => {
                if !waited_once {
                    eprintln!(
                        "note: cig tin rekey is in progress; waiting for it to finish before this run touches the tin"
                    );
                    waited_once = true;
                }
                let _ = attempt;
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => {
                return Err(format!(
                    "could not open the rekey lock {} ({e}); refusing to proceed unprotected against a concurrent cig tin rekey",
                    rekey_lock_path().display()
                ));
            }
        }
    }
    Err(format!(
        "the rekey lock {} stayed in a sharing violation for 30s; refusing to proceed unprotected against a concurrent cig tin rekey",
        rekey_lock_path().display()
    ))
}

#[cfg(not(any(unix, windows)))]
pub fn take_process_shared_lock() -> Result<fs::File, String> {
    Err("process rekey lock is not implemented on this platform; refusing to proceed unprotected against a concurrent cig tin rekey".to_string())
}

/// Run the rekey. Refuses when the current key is not available (nothing
/// safe to do: a key gone is #103's problem, not this command's).
pub fn rekey() -> Result<Done, String> {
    // The keys folder must be ours and private before any lock or key
    // read; a planted folder or lock refuses here with E721 (#176).
    let _ = seal::ensure_keys_dir()?;
    seal::check_private_state_file(&rekey_lock_path())?;
    // The rekey holds the exclusive lock. First take it so no run that
    // seals or signs can slip in between the key reads and the walk; the
    // rekey's own `seal::key()` call below must opt out of the shared
    // lock, or it would block waiting on this same lock.
    let _lock = acquire_rekey_lock().map_err(|b| match b {
        LockBusy::OtherRekey => "another cig tin rekey is in progress on this tin; wait for it to finish before starting a new one".to_string(),
        LockBusy::Busy => "a run is in progress on this tin (a seal, a journal append or a ledger append holds the shared rekey lock); wait for it to finish before starting a rekey".to_string(),
        LockBusy::Io(e) => format!("cannot take the rekey lock ({}): {e}", rekey_lock_path().display()),
    })?;
    // The rekey OWNS the lock; its own key reads must skip the shared
    // lock the normal path takes, or they would self-deadlock.
    seal::mark_rekey_thread(true);
    let result = rekey_inner();
    seal::mark_rekey_thread(false);
    result
}

fn rekey_inner() -> Result<Done, String> {
    let (current, keeper) = seal::key_no_rekey_lock().map_err(|e| {
        format!(
            "the tin's key is not available ({e}); nothing can be re-sealed. Put the key back (cig tin key import), or move the sealed objects aside to start over"
        )
    })?;
    match keeper {
        seal::Keeper::Descriptor => {
            return Err(
                "the key was handed over on a descriptor for this run only (--seal-key-fd); a rekey cannot write it back there. Store it in the keyring or a key file first (cig config seal_key keyring, or cig tin key import)"
                    .to_string(),
            );
        }
        seal::Keeper::Keyring | seal::Keeper::File => {}
    }

    let keys_dir = seal::keys_dir();
    let old_path = keys_dir.join("seal.key.old");
    let new_path = keys_dir.join("seal.key.new");
    let marker_path = keys_dir.join(REKEY_MARKER);

    // Clean temp leftovers before the walk's probe counts them (finding 5).
    // The "rekey in progress" marker is created only after verification
    // below passes (SEC-004 N5): a refused rekey must not leave one, or a
    // later contention with a running cig reads it as "another rekey is
    // in progress" (rekey.rs:239) instead of "a run is in progress".
    clean_reseal_temp_files()?;

    // Resume path: an earlier rekey left stashes on disk. Three distinguishable
    // states, decided by what is still on disk and which key the keeper
    // currently carries.
    //
    //   a. both stashes present: the walk started. Pick them up as-is.
    //   b. only seal.key.old present, keeper still holds that key: the
    //      previous rekey crashed before writing seal.key.new, so
    //      nothing on disk was changed. Wipe-remove the stash and start
    //      fresh.
    //   c. only seal.key.old present, keeper holds another key: the
    //      cleanup step crashed after the swap.
    //
    // Every resume path refuses unless the keeper's current key equals
    // one of the stash keys (SEC-004 H3): a planted old+new pair with
    // no prior rekey cannot satisfy it, so an attacker cannot make the
    // rekey adopt its key without knowing the primary. Legitimate
    // mid-rekey state always satisfies it.
    let resumed = old_path.exists() || new_path.exists();
    let (new_key, old_key) = if resumed {
        let stashed_old = seal::key_in_file(&old_path);
        let stashed_new = seal::key_in_file(&new_path);
        // H3: the keeper must match a stash, or we refuse.
        let keeper_matches = stashed_old.as_ref().map(|k| k == &current).unwrap_or(false)
            || stashed_new.as_ref().map(|k| k == &current).unwrap_or(false);
        if !keeper_matches {
            return Err(format!(
                "the rekey stashes on disk ({} / {}) do not match the tin's current key, so nothing here was left by a rekey this tin started. Either both files were planted, or the keeper has been changed since; refusing to re-sign anything. Move the stashes aside, then rerun cig tin rekey",
                new_path.display(),
                old_path.display(),
            ));
        }
        // H3 (cont'd): verify every chain under the current/stash keys
        // before re-signing. The chain check already trials each
        // candidate the way a load would.
        verify_everything_under_current_key()?;
        // Verification passed. On resume the marker is already on disk
        // from the rekey we are picking up; make_marker is idempotent.
        // Keeping the call here (rather than the pre-verify slot) keeps
        // N5's invariant: only a rekey that passes verification leaves
        // a marker that `rekey_in_progress` later reads as truthful.
        make_marker(&marker_path)?;
        match (stashed_old, stashed_new) {
            (Some(o), Some(n)) => {
                // State (a).
                (n, o)
            }
            (Some(o), None) => {
                if o == current {
                    // State (b): nothing changed on disk; wipe-remove
                    // the dangling stash and start fresh.
                    wipe_and_remove_stash(&old_path)?;
                    let new = seal::random_key().map_err(|e| format!("no randomness: {e}"))?;
                    stash_key(&old_path, &current)?;
                    stash_key(&new_path, &new)?;
                    (new, current)
                } else {
                    // State (c): keeper already carries the new key; the
                    // old stash is a fallback for a rekey that completed
                    // its walk and keeper swap.
                    stash_key(&new_path, &current)?;
                    (current, o)
                }
            }
            (None, Some(n)) => {
                // Half-set: new present, no old. Keeper is new, no fallback needed.
                wipe_and_remove_stash(&new_path)?;
                (n, current)
            }
            (None, None) => unreachable!("one of the stashes must exist, resumed==true"),
        }
    } else {
        // Fresh rekey: every signed journal chain and the ledger must
        // verify under the current key. A chain with a break, or signed
        // by another key (H4), is caught here and the rekey refuses —
        // re-signing it would launder a chain this tin never vouched
        // for, and `cig tin verify` after would be clean, leaving the
        // operator none the wiser.
        verify_everything_under_current_key()?;
        // Verification passed. Create the "rekey in progress" marker
        // AFTER verify, so a refusal leaves no marker to confuse a
        // later contention's message (SEC-004 N5). The marker is what
        // lets `seal::secondary_keys` honour the on-disk stashes as
        // fallback keys through a crash.
        make_marker(&marker_path)?;
        let new = seal::random_key().map_err(|e| format!("no randomness: {e}"))?;
        stash_key(&old_path, &current)?;
        stash_key(&new_path, &new)?;
        (new, current)
    };
    // Make the two stashes durable before the walk changes a sealed file.
    super::journal::sync_dir_pub(&keys_dir).map_err(|e| {
        format!(
            "cannot fsync the keys directory {}: {e}; the stashes may not survive a power cut, refusing to go on",
            keys_dir.display()
        )
    })?;

    // The walk.
    let walked = resealed_paths()?;
    let mut done = Done {
        new_fingerprint: seal::fingerprint(&new_key),
        old_fingerprint: seal::fingerprint(&old_key),
        walked: walked.len(),
        already: 0,
        resealed: 0,
        resumed,
    };
    // Which parent directories need an fsync after the renames (finding 10).
    let mut touched_dirs: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
    for (i, path) in walked.iter().enumerate() {
        inject_fault(i, done.resealed)?;
        let aad = seal::aad_of(path);
        match probe_or_reseal(path, &aad, &new_key, &old_key)? {
            Outcome::Already => done.already += 1,
            Outcome::Resealed => {
                done.resealed += 1;
                if let Some(parent) = path.parent() {
                    touched_dirs.insert(parent.to_path_buf());
                }
            }
        }
    }

    // Journals and the ledger were signed under the old master's MAC
    // subkeys (format 3, from 1.9.0). The new master has its own. Rewrite
    // every chain under the new subkey — `rechain` recomputes each line's
    // `prev` from the seed, so this is idempotent.
    //
    // An unsigned (format-2) journal is left as-is: an old record was
    // never signed under the tin's key, so there is no signature to
    // rotate and we must not quietly upgrade it to signed (finding 3).
    let new_journal_key = seal::derive(&new_key, seal::LABEL_JOURNAL_MAC);
    let new_ledger_key = seal::derive(&new_key, seal::LABEL_LEDGER_MAC);
    let mut new_heads: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    let runs_dir = super::runs::runs_dir();
    if let Ok(entries) = fs::read_dir(&runs_dir) {
        for r in entries.flatten() {
            let dir = r.path();
            if !dir.is_dir() {
                continue;
            }
            let new_head = super::journal::resign_signed_in_place_under(&dir, &new_journal_key)
                .map_err(|e| format!("re-signing {}: {e}", dir.display()))?;
            if let (Some(head), Some(id)) = (
                new_head,
                dir.file_name().map(|n| n.to_string_lossy().to_string()),
            ) {
                // The run.json's `journal_head` follows the journal's new
                // head, so `cig tin verify --head` sees the same value the
                // ledger line below will carry. A temp + rename, so an
                // interrupted write never leaves half a record (finding 9).
                write_run_journal_head(&dir.join("run.json"), &head)?;
                new_heads.insert(id, head);
            }
        }
        // The runs directory holds the renamed run.json files; make them durable.
        super::journal::sync_dir_pub(&runs_dir).map_err(|e| {
            format!(
                "cannot fsync the runs directory {}: {e}; a crash here could leave a run.json half-replaced",
                runs_dir.display()
            )
        })?;
    }
    super::ledger::resign_under(&new_ledger_key, &new_heads)?;
    super::journal::sync_dir_pub(&keys_dir).map_err(|e| {
        format!(
            "cannot fsync the keys directory {}: {e}; the ledger rewrite may not survive a power cut",
            keys_dir.display()
        )
    })?;

    // The keeper swap. Returns Err if the keeper cannot be proven to
    // carry the new key: both stashes stay on disk, every sealed object
    // is still readable under one of them, and a later cig tin rekey
    // finishes the job (finding 1).
    swap_keeper_if_needed(&new_key, keeper)?;

    // Fsync every tin bucket whose sealed file we renamed, so the
    // renames outlive a power cut (finding 10).
    for d in &touched_dirs {
        super::journal::sync_dir_pub(d).map_err(|e| {
            format!(
                "cannot fsync the tin bucket {}: {e}; a sealed object rename may not survive a power cut",
                d.display()
            )
        })?;
    }

    // The keeper is proven. Wipe-and-remove both stashes, fsync the
    // keys directory, then delete the rekey marker. An I/O error that
    // leaves a stash or the marker behind is reported, never swallowed
    // (findings 4, L1).
    wipe_and_remove_stash(&new_path)?;
    wipe_and_remove_stash(&old_path)?;
    match fs::remove_file(&marker_path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(format!(
                "could not remove the rekey marker {}: {e}; secondary_keys will keep honouring the stashes on the next run until the file is removed by hand",
                marker_path.display()
            ))
        }
    }
    super::journal::sync_dir_pub(&keys_dir).map_err(|e| {
        format!(
            "cannot fsync the keys directory {}: {e}; the clean-up of the stashes may not survive a power cut",
            keys_dir.display()
        )
    })?;

    Ok(done)
}

/// Create (or touch, with no change in content) the rekey marker. 0600
/// so only the owner can see it, same as the stashes.
fn make_marker(path: &Path) -> Result<(), String> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let f = opts
        .open(path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    f.sync_all()
        .map_err(|e| format!("cannot sync {}: {e}", path.display()))?;
    Ok(())
}

enum Outcome {
    Already,
    Resealed,
}

/// For one sealed file, confirm it opens with the new key (already done),
/// otherwise open it with the old key, re-seal it with the new (through
/// a `.rekey` temp beside it), and rename onto the original. The reseal
/// keeps the plaintext in memory (never on disk) and verifies the new
/// copy by decrypting it with the new key before the rename.
fn probe_or_reseal(
    path: &Path,
    aad: &[u8],
    new_key: &Key,
    old_key: &Key,
) -> Result<Outcome, String> {
    // Already under the new key?
    if seal::opens_with(path, aad, new_key).is_ok() {
        return Ok(Outcome::Already);
    }
    // Otherwise, old must open it.
    seal::opens_with(path, aad, old_key).map_err(|e| {
        format!(
            "{}: neither the new key nor the old key opens it ({e}); the tin is in a state this rekey cannot repair",
            path.display()
        )
    })?;
    // Reseal into a `.rekey` sibling, then rename onto the original.
    // `reseal_under(src, dest, …)` produces `dest`: here `dest` is the
    // temp, the rename does the atomic replace.
    let tmp = path.with_extension("rekey");
    let _ = fs::remove_file(&tmp);
    let outcome =
        seal::reseal_under(path, &tmp, aad, old_key, new_key).and_then(|()| fs::rename(&tmp, path));
    if let Err(e) = outcome {
        let _ = fs::remove_file(&tmp);
        return Err(format!("{}: re-sealing failed: {e}", path.display()));
    }
    Ok(Outcome::Resealed)
}

/// Every sealed object the tin holds and every sealed snapshot a run's
/// folder keeps: a path for each, sorted for determinism. Temp suffixes
/// left behind by a crashed earlier rekey (`.rekey`, `.reseal-plain`,
/// `.reseal-sealed`) are not included — they are cleaned up before the
/// walk begins (finding 5).
pub fn resealed_paths() -> Result<Vec<PathBuf>, String> {
    let mut out: Vec<PathBuf> = Vec::new();
    // Sealed objects in the tin.
    let sealed = super::tin::sealed_dir();
    if let Ok(buckets) = fs::read_dir(&sealed) {
        for b in buckets.flatten() {
            let Ok(files) = fs::read_dir(b.path()) else {
                continue;
            };
            for f in files.flatten() {
                if !f.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    continue;
                }
                if is_reseal_temp(&f.path()) {
                    continue;
                }
                out.push(f.path());
            }
        }
    }
    // Sealed snapshots under each run.
    let runs = super::runs::runs_dir();
    if let Ok(entries) = fs::read_dir(&runs) {
        for r in entries.flatten() {
            let snaps = r.path().join("snapshots");
            if let Ok(rd) = fs::read_dir(&snaps) {
                for f in rd.flatten() {
                    let p = f.path();
                    let name = f.file_name().to_string_lossy().to_string();
                    if !name.ends_with(seal::SUFFIX) {
                        continue;
                    }
                    if is_reseal_temp(&p) {
                        continue;
                    }
                    out.push(p);
                }
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Is this path a leftover from a crashed reseal or rekey? Named by
/// extension: `.rekey` (rekey walk), `.reseal-plain` (old reseal path,
/// before the in-memory switch; still cleaned up on upgrade), and
/// `.reseal-sealed`.
fn is_reseal_temp(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("rekey") | Some("reseal-plain") | Some("reseal-sealed")
    )
}

/// Wipe-remove every reseal/rekey temp left in the sealed bucket and
/// under each run's `snapshots`, before the walk begins. A kill during a
/// (pre-1.18) reseal left a `.reseal-plain` with the secret in the clear;
/// this both unblocks a later rekey and removes the plaintext (finding 5).
fn clean_reseal_temp_files() -> Result<(), String> {
    let sealed = super::tin::sealed_dir();
    if let Ok(buckets) = fs::read_dir(&sealed) {
        for b in buckets.flatten() {
            let Ok(files) = fs::read_dir(b.path()) else {
                continue;
            };
            for f in files.flatten() {
                let p = f.path();
                if f.file_type().map(|t| t.is_file()).unwrap_or(false) && is_reseal_temp(&p) {
                    wipe_and_remove_file(&p)?;
                }
            }
        }
    }
    let runs = super::runs::runs_dir();
    if let Ok(entries) = fs::read_dir(&runs) {
        for r in entries.flatten() {
            let snaps = r.path().join("snapshots");
            if let Ok(rd) = fs::read_dir(&snaps) {
                for f in rd.flatten() {
                    let p = f.path();
                    if f.file_type().map(|t| t.is_file()).unwrap_or(false) && is_reseal_temp(&p) {
                        wipe_and_remove_file(&p)?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Before a rekey touches a signature: every signed journal chain and
/// the ledger must verify under the current key (which is still the old
/// key in-process, or one of the stash keys on resume). A chain that
/// does not fit — a break, another key's signature, an unverifiable
/// one — is tampering or an unadopted import, and the rekey refuses:
/// re-signing it would launder it (findings 3, H4).
fn verify_everything_under_current_key() -> Result<(), String> {
    if let Ok(entries) = fs::read_dir(super::runs::runs_dir()) {
        for r in entries.flatten() {
            let dir = r.path();
            if !dir.is_dir() {
                continue;
            }
            if !dir.join("journal.jsonl").exists() {
                continue;
            }
            let j = super::journal::Journal::load(&dir).map_err(|e| {
                format!(
                    "the journal of {} cannot be read: {e}; nothing was re-signed",
                    dir.display()
                )
            })?;
            let id = dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let chain = j.chain();
            if let Some(brk) = &chain.broken_at {
                return Err(format!(
                    "run {id}'s journal does not verify under the current key: line {} does not fit the one before it ({}); re-signing it would launder the edit, so the rekey refuses. Nothing was changed",
                    brk.line, brk.reason
                ));
            }
            if chain.other_key {
                return Err(format!(
                    "run {id}'s journal is signed by another key (another machine's, or a key since replaced); re-signing it would launder a chain this tin never vouched for. Nothing was changed. If you want to adopt it, re-import the run (cig parcel on the source, cig unparcel here) which re-signs its journal under this key"
                ));
            }
            if chain.unverified {
                return Err(format!(
                    "run {id}'s signed journal cannot be verified under any key this tin has, so re-signing it would laundered an untrusted chain. Nothing was changed"
                ));
            }
        }
    }
    match super::ledger::load() {
        Ok(l) => {
            if let Some(line) = l.broken_at {
                return Err(format!(
                    "the ledger does not verify under the current key: line {line} does not fit the one before it; re-signing it would launder the edit, so the rekey refuses. Nothing was changed"
                ));
            }
        }
        Err(e) => {
            return Err(format!(
                "the ledger cannot be read ({e}); nothing was re-signed"
            ))
        }
    }
    Ok(())
}

/// Write `run.json` back with a new `journal_head`, through a temp +
/// rename, so an interrupted write never leaves half a record. Errors
/// propagate (finding 9).
fn write_run_journal_head(rec_path: &Path, head: &str) -> Result<(), String> {
    let Ok(text) = fs::read_to_string(rec_path) else {
        return Ok(());
    };
    let Ok(mut rec) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Ok(());
    };
    rec["journal_head"] = serde_json::Value::String(head.to_string());
    let new = serde_json::to_string_pretty(&rec)
        .map_err(|e| format!("cannot serialise {}: {e}", rec_path.display()))?;
    let tmp = rec_path.with_extension("json.rekey");
    let _ = fs::remove_file(&tmp);
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    f.write_all(new.as_bytes())
        .and_then(|()| f.sync_all())
        .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, rec_path)
        .map_err(|e| format!("cannot replace {}: {e}", rec_path.display()))?;
    if let Some(parent) = rec_path.parent() {
        super::journal::sync_dir_pub(parent).map_err(|e| {
            format!(
                "cannot fsync the run directory {}: {e}; the new run.json may not survive a power cut",
                parent.display()
            )
        })?;
    }
    Ok(())
}

/// A test hook that stops the rekey mid-walk, to prove an interruption
/// leaves every object readable. `CIG_INTERNAL_FAULT=rekey-stop-after:N`
/// fails on the Nth reseal; the walk has already renamed the earlier ones
/// and `seal.key.old` / `seal.key.new` are still on disk.
fn inject_fault(_index: usize, done_so_far: usize) -> Result<(), String> {
    let Ok(v) = std::env::var("CIG_INTERNAL_FAULT") else {
        return Ok(());
    };
    if let Some(n) = v.strip_prefix("rekey-stop-after:") {
        if let Ok(n) = n.parse::<usize>() {
            if done_so_far >= n {
                return Err(format!(
                    "stopped at reseal {n} on purpose (CIG_INTERNAL_FAULT)"
                ));
            }
        }
    }
    Ok(())
}

/// A test hook on the keeper swap: `CIG_INTERNAL_FAULT=rekey-keeper-fail`
/// makes the keeper write refuse, so the test can prove that every
/// sealed object is still readable and a second cig tin rekey finishes
/// the job (finding 1).
fn fault_keeper_fail() -> Option<String> {
    let v = std::env::var("CIG_INTERNAL_FAULT").ok()?;
    if v == "rekey-keeper-fail" {
        Some("deliberate keeper-write failure (CIG_INTERNAL_FAULT=rekey-keeper-fail)".to_string())
    } else {
        None
    }
}

fn stash_key(path: &Path, key: &Key) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("rekey.tmp");
    let _ = fs::remove_file(&tmp);
    write_key_file(&tmp, key).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

fn write_key_file(path: &Path, key: &Key) -> io::Result<()> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    // The hex text is a copy of the key: wipe before letting go (L3).
    let mut hex_bytes = hex::encode(key.as_bytes()).into_bytes();
    let result = f
        .write_all(&hex_bytes)
        .and_then(|()| f.write_all(b"\n"))
        .and_then(|()| f.sync_all());
    seal::wipe_bytes(&mut hex_bytes);
    result
}

/// Overwrite a stash's bytes with zeros, fsync, then unlink. A failed
/// write or unlink is an error: a stash that survives in the clear
/// keeps a key readable to a later process (finding 4).
fn wipe_and_remove_stash(path: &Path) -> Result<(), String> {
    match wipe_and_remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) => Err(format!(
            "could not clear the stash at {}: {e}; move it aside by hand, then run cig tin rekey again",
            path.display()
        )),
    }
}

/// Overwrite with zeros, fsync, unlink. `NotFound` is not an error.
///
/// Refuses anything but a regular file (SEC-004 N1(b)): `symlink_metadata`
/// is used so a symlink is seen AS a symlink rather than as its target,
/// and on unix the open goes through `O_NOFOLLOW` so a symlink that
/// appeared between the stat and the open cannot steer the write to a
/// different file. A hostile parcel whose `manifest.json` is a symlink to
/// a user's `seal.key` then refuses here instead of zeroing the target
/// (parcel.rs:510 is the import-cleanup caller).
pub(crate) fn wipe_and_remove_file(path: &Path) -> Result<(), String> {
    use std::io::Seek;
    let md = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.to_string()),
    };
    if !md.file_type().is_file() {
        return Err(format!(
            "{} is not a regular file; refusing to wipe-remove it",
            path.display()
        ));
    }
    let len = md.len() as usize;
    // Open RW, overwrite in place with zeros, sync. `O_NOFOLLOW` on unix
    // refuses to follow a symlink that is placed between the stat above
    // and this open — `ELOOP` returns, and nothing is written to any
    // target.
    let mut opts = fs::OpenOptions::new();
    opts.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    match opts.open(path) {
        Ok(mut f) => {
            if len > 0 {
                let zeros = vec![0u8; len.min(64 * 1024)];
                let mut written = 0usize;
                while written < len {
                    let n = (len - written).min(zeros.len());
                    f.write_all(&zeros[..n]).map_err(|e| e.to_string())?;
                    written += n;
                }
                f.flush().map_err(|e| e.to_string())?;
                f.sync_all().map_err(|e| e.to_string())?;
                // Rewind, so a later fsync inside the same process does not read
                // past end.
                let _ = f.seek(io::SeekFrom::Start(0));
            }
            drop(f);
        }
        Err(e) => return Err(e.to_string()),
    }
    fs::remove_file(path).map_err(|e| e.to_string())?;
    Ok(())
}

fn swap_keeper_if_needed(new_key: &Key, keeper: seal::Keeper) -> Result<(), String> {
    if let Some(reason) = fault_keeper_fail() {
        return Err(format!(
            "the keeper would not carry the new key: {reason}. The old key is still in {} and the new one in {}; every sealed object is still readable. Run cig tin rekey again to finish",
            seal::keys_dir().join("seal.key.old").display(),
            seal::keys_dir().join("seal.key.new").display(),
        ));
    }
    match keeper {
        seal::Keeper::Keyring => {
            // Store, then read back and compare in constant time.
            seal::store_in_keyring(new_key).map_err(|e| {
                format!(
                    "the keyring would not accept the new key ({e}); the stashes {} and {} are kept, every sealed object is still readable. Run cig tin rekey again to finish",
                    seal::keys_dir().join("seal.key.new").display(),
                    seal::keys_dir().join("seal.key.old").display(),
                )
            })?;
        }
        seal::Keeper::File => {
            let path = seal::key_path();
            if seal::key_in_file(&path).as_ref() == Some(new_key) {
                return Ok(());
            }
            let tmp = path.with_extension("rekey.tmp");
            let _ = fs::remove_file(&tmp);
            write_key_file(&tmp, new_key)
                .map_err(|e| format!("cannot write the new key to {}: {e}", tmp.display()))?;
            fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
            // Prove the keeper now holds the new key (read-back + ct-compare).
            match seal::key_in_file(&path) {
                Some(k) if &k == new_key => {}
                _ => {
                    return Err(format!(
                        "the key file {} did not read back as the new key after the write; both stashes are kept and every sealed object is still readable. Run cig tin rekey again to finish",
                        path.display()
                    ))
                }
            }
            // Keyring is also set where it is the default keeper (same as
            // first-use): best-effort, a failure does not keep the swap
            // from succeeding — `secondary_keys` picks up the stash on
            // the next read, and the file is already the source of truth.
            let _ = seal::store_in_keyring(new_key);
        }
        seal::Keeper::Descriptor => unreachable!("refused up front"),
    }
    Ok(())
}

#[cfg(all(unix, test))]
mod tests {
    use super::*;
    use std::os::unix::fs as unix_fs;

    /// SEC-004 N1(b). `wipe_and_remove_file` refuses a symlink and
    /// leaves the target bytes intact. Without `symlink_metadata` +
    /// `O_NOFOLLOW`, a hostile parcel whose scratch `manifest.json`
    /// is a symlink to the tin's seal key would route the zero-write
    /// through the symlink and lock every sealed object out for good.
    #[test]
    fn wipe_and_remove_file_refuses_a_symlink_and_leaves_the_target() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("target");
        let bytes = b"these bytes must not be zeroed".to_vec();
        fs::write(&target, &bytes).expect("write target");
        let link = tmp.path().join("link");
        unix_fs::symlink(&target, &link).expect("symlink");

        let r = wipe_and_remove_file(&link);
        assert!(
            r.is_err(),
            "wipe_and_remove_file(symlink) should refuse, got {r:?}"
        );
        // Whatever happened to the link itself, the target is untouched.
        assert_eq!(fs::read(&target).expect("re-read target"), bytes);
    }

    /// `wipe_and_remove_file` on an ordinary file still wipes and
    /// removes it, as its callers inside a rekey rely on.
    #[test]
    fn wipe_and_remove_file_wipes_and_removes_a_regular_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let p = tmp.path().join("ordinary");
        fs::write(&p, b"secret bytes").expect("write");
        wipe_and_remove_file(&p).expect("wipe");
        assert!(!p.exists(), "file should be removed");
    }
}
