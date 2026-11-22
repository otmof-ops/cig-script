// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The module cache: lowered, verified modules kept under the tin, so that
//! `cig run` on the VM lowers a script once and loads it from then on
//! (#254).
//!
//! **The key.** An entry is found by the SHA-256 over this cig's version,
//! the lowering's version, the optimization level and the script's own
//! SHA-256. The same bytes on the same cig give the same key; an edit, an
//! update of cig or a change to the lowering gives another, and the old
//! entry is never asked for again ([`sweep`] removes it).
//!
//! **Trust.** The tin is not where trust lives: the ledger sits in the
//! keys directory for the same reason. An entry is used only when its MAC
//! verifies under this machine's module key, a file in the keys directory.
//! The MAC is a keyed BLAKE3 over the entry's header and the module's own
//! header, which carries the BLAKE3 of every byte of the module's body;
//! [`crate::vm::open`] checks the body against it. So bytes written into
//! the tin by something that cannot read the keys directory are never run.
//! Whoever can read the keys directory can already change the script or
//! cig itself; the cache gives them nothing they did not have.
//!
//! **Nothing here fails a run.** An entry that is missing, cut short, for
//! another cig, or that does not verify is a miss: the script is lowered
//! again and the entry rewritten. A cache that cannot be written (no keys
//! directory, a full disk) costs the lowering on every run and nothing
//! else.

use cig_bytecode::serialize::HEADER_LEN as MODULE_HEADER_LEN;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// The folder under the tin that holds the entries.
pub const DIR: &str = "modules";
/// The module key's file name, in the keys directory.
pub const KEY_FILE: &str = "module.key";
/// How long an entry nobody loaded is kept before [`sweep`] removes it.
pub const KEEP: Duration = Duration::from_secs(30 * 24 * 60 * 60);

const MAGIC: [u8; 8] = *b"CIGMOD1\0";
const FORMAT: u32 = 1;
/// The check gave warnings for this script: a run prints them, so it
/// checks the script again instead of taking the entry on its word alone.
const FLAG_WARNED: u32 = 1;
const VERSION_LEN: usize = 16;
/// magic, format, flags, the cig version, the key, the module's length,
/// the MAC.
const HEADER_LEN: usize = 8 + 4 + 4 + VERSION_LEN + 32 + 8 + 32;
const MAC_AT: usize = HEADER_LEN - 32;
/// The optimization level entries are keyed under: the one `cig run`
/// lowers at.
const LEVEL: u8 = crate::lower::DEFAULT_LEVEL;
/// A loaded entry's modification time is moved forward when it is older
/// than this, so [`sweep`] can tell an entry in use from one left behind
/// without a write on every run.
const TOUCH_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// Where the entries live: `<state>/tin/modules`.
pub fn root() -> PathBuf {
    super::tin::dir().join(DIR)
}

/// An entry's key.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Key([u8; 32]);

impl Key {
    pub fn hex(&self) -> String {
        hex::encode(self.0)
    }
}

/// The key for a script, from its SHA-256 as the run record carries it.
pub fn key_for(script_sha256: &str) -> Key {
    let mut h = Sha256::new();
    h.update(b"cig module cache\0");
    h.update(crate::VERSION.as_bytes());
    h.update([0]);
    h.update(crate::lower::VERSION.to_le_bytes());
    h.update([LEVEL]);
    h.update(script_sha256.as_bytes());
    Key(h.finalize().into())
}

/// What a hit gives back.
pub struct Entry {
    /// The module's bytes, as [`crate::vm::open`] takes them.
    pub module: Vec<u8>,
    /// The check gave warnings when the entry was made.
    pub warned: bool,
}

fn path_of(key: &Key) -> PathBuf {
    let name = key.hex();
    root().join(&name[..2]).join(name)
}

fn version_field() -> [u8; VERSION_LEN] {
    let mut out = [0u8; VERSION_LEN];
    let v = crate::VERSION.as_bytes();
    let n = v.len().min(VERSION_LEN);
    out[..n].copy_from_slice(&v[..n]);
    out
}

fn header(key: &Key, flags: u32, len: u64) -> [u8; HEADER_LEN] {
    let mut h = [0u8; HEADER_LEN];
    h[..8].copy_from_slice(&MAGIC);
    h[8..12].copy_from_slice(&FORMAT.to_le_bytes());
    h[12..16].copy_from_slice(&flags.to_le_bytes());
    h[16..16 + VERSION_LEN].copy_from_slice(&version_field());
    h[32..64].copy_from_slice(&key.0);
    h[64..72].copy_from_slice(&len.to_le_bytes());
    h
}

/// The MAC over an entry's header (without the MAC's own bytes) and the
/// module's header, which carries the hash of the module's body.
fn mac(mac_key: &[u8; 32], header: &[u8; HEADER_LEN], module_head: &[u8]) -> blake3::Hash {
    let mut h = blake3::Hasher::new_keyed(mac_key);
    h.update(&header[..MAC_AT]);
    h.update(module_head);
    h.finalize()
}

/// The module key: 32 random bytes in the keys directory, owner-only, made
/// the first time an entry is stored. `None` where there is no usable key:
/// the cache is then off for this run.
fn mac_key(create: bool) -> Option<[u8; 32]> {
    let dir = super::seal::ensure_keys_dir().ok()?;
    let path = dir.join(KEY_FILE);
    if let Some(k) = read_key(&path) {
        return Some(k);
    }
    if !create || fs::symlink_metadata(&path).is_ok() {
        // Something is there and it is not a key this cig will use.
        return None;
    }
    let mut k = [0u8; 32];
    getrandom::getrandom(&mut k).ok()?;
    // Written under another name and linked into place: the key appears
    // whole or not at all, and two cigs starting together agree on the one
    // that got there first.
    let tmp = dir.join(format!(".{KEY_FILE}.{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let written = private_file(&tmp).and_then(|mut f| f.write_all(&k));
    if written.is_ok() {
        let _ = fs::hard_link(&tmp, &path);
    }
    let _ = fs::remove_file(&tmp);
    read_key(&path)
}

/// The key in `path`, when it is a plain file of exactly 32 bytes that is
/// this user's alone.
fn read_key(path: &Path) -> Option<[u8; 32]> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() || meta.len() != 32 {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: `geteuid` takes nothing and cannot fail.
        let me = unsafe { libc::geteuid() };
        if meta.uid() != me || meta.mode() & 0o077 != 0 {
            return None;
        }
    }
    let bytes = fs::read(path).ok()?;
    bytes.try_into().ok()
}

/// A new file only its owner can read, refusing to follow a link.
fn private_file(path: &Path) -> io::Result<fs::File> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    opts.open(path)
}

fn private_dir(path: &Path) -> io::Result<()> {
    let mut b = fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(path)
}

/// The entry for `key`, when there is one this machine made for this cig.
pub fn lookup(key: &Key) -> Option<Entry> {
    let path = path_of(key);
    let mut file = fs::File::open(&path).ok()?;
    let meta = file.metadata().ok()?;
    let total = usize::try_from(meta.len()).ok()?;
    if !meta.is_file() || total < HEADER_LEN + MODULE_HEADER_LEN {
        return None;
    }
    let mut head = [0u8; HEADER_LEN];
    file.read_exact(&mut head).ok()?;
    let len = u64::from_le_bytes(head[64..72].try_into().ok()?);
    let flags = u32::from_le_bytes(head[12..16].try_into().ok()?);
    if head[..8] != MAGIC
        || head[8..12] != FORMAT.to_le_bytes()
        || head[16..16 + VERSION_LEN] != version_field()
        || head[32..64] != key.0
        || len != (total - HEADER_LEN) as u64
        || flags & !FLAG_WARNED != 0
    {
        return None;
    }
    let mut module = vec![0u8; total - HEADER_LEN];
    file.read_exact(&mut module).ok()?;
    let mac_key = mac_key(false)?;
    let want = mac(&mac_key, &head, &module[..MODULE_HEADER_LEN]);
    // `blake3::Hash` compares in constant time.
    if want != blake3::Hash::from_bytes(head[MAC_AT..].try_into().ok()?) {
        return None;
    }
    touch_if_old(&path, &meta);
    Some(Entry {
        module,
        warned: flags & FLAG_WARNED != 0,
    })
}

fn touch_if_old(path: &Path, meta: &fs::Metadata) {
    let old = meta
        .modified()
        .ok()
        .and_then(|m| SystemTime::now().duration_since(m).ok())
        .is_some_and(|age| age > TOUCH_AFTER);
    if old {
        if let Ok(f) = fs::OpenOptions::new().write(true).open(path) {
            let _ = f.set_modified(SystemTime::now());
        }
    }
}

/// Keep `module` under `key`. The entry appears whole or not at all: it is
/// written beside its place and renamed into it.
pub fn store(key: &Key, module: &[u8], warned: bool) -> io::Result<()> {
    if module.len() < MODULE_HEADER_LEN {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a module"));
    }
    let mac_key =
        mac_key(true).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no module key"))?;
    let path = path_of(key);
    let dir = path.parent().unwrap_or(Path::new("."));
    private_dir(dir)?;
    let flags = if warned { FLAG_WARNED } else { 0 };
    let mut head = header(key, flags, module.len() as u64);
    let tag = mac(&mac_key, &head, &module[..MODULE_HEADER_LEN]);
    head[MAC_AT..].copy_from_slice(tag.as_bytes());
    let tmp = dir.join(format!(".{}.{}.tmp", key.hex(), std::process::id()));
    let _ = fs::remove_file(&tmp);
    let written = private_file(&tmp).and_then(|mut f| {
        f.write_all(&head)?;
        f.write_all(module)
    });
    let placed = written.and_then(|()| fs::rename(&tmp, &path));
    if placed.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    placed
}

/// What the cache holds: how many entries, and their bytes on disk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub entries: usize,
    pub bytes: u64,
}

/// The cache's own files: an entry, or what a write that never finished
/// left beside one. Only real folders are entered and only plain files
/// are listed, never through a link: nothing outside the cache is ever
/// counted or removed, whatever is put in its place.
fn files() -> Vec<(PathBuf, fs::Metadata)> {
    let mut out = Vec::new();
    let is_dir = |p: &Path| fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_dir());
    if !is_dir(&root()) {
        return out;
    }
    let Ok(shards) = fs::read_dir(root()) else {
        return out;
    };
    for shard in shards.flatten() {
        if !is_dir(&shard.path()) {
            continue;
        }
        let Ok(inner) = fs::read_dir(shard.path()) else {
            continue;
        };
        for f in inner.flatten() {
            let path = f.path();
            if !(is_entry(&path) || is_leftover(&path)) {
                continue;
            }
            if let Ok(meta) = fs::symlink_metadata(&path) {
                if meta.file_type().is_file() {
                    out.push((path, meta));
                }
            }
        }
    }
    out
}

fn is_entry(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.len() == 64 && n.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The name [`store`] writes under before it renames: `.<key>.<pid>.tmp`.
fn is_leftover(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix('.'))
        .and_then(|n| n.strip_suffix(".tmp"))
        .and_then(|n| n.split_once('.'))
        .is_some_and(|(key, pid)| {
            key.len() == 64
                && key.bytes().all(|b| b.is_ascii_hexdigit())
                && !pid.is_empty()
                && pid.bytes().all(|b| b.is_ascii_digit())
        })
}

/// The entries and their size, for `cig tin` and `cig doctor`.
pub fn stats() -> Stats {
    let mut s = Stats::default();
    for (path, meta) in files() {
        if is_entry(&path) {
            s.entries += 1;
            s.bytes += meta.len();
        }
    }
    s
}

/// Was this entry made by this cig? Read from its header alone.
fn made_by_this_cig(path: &Path) -> bool {
    let mut head = [0u8; HEADER_LEN];
    fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok()
        && head[..8] == MAGIC
        && head[16..16 + VERSION_LEN] == version_field()
}

/// Remove what nothing will load again: entries another cig made, entries
/// nobody loaded for [`KEEP`], and files a stopped write left behind.
/// Returns what was removed. Every entry is safe to remove: its script is
/// lowered again on its next run.
pub fn sweep() -> Stats {
    remove_where(|path, meta| {
        if !is_entry(path) {
            // A write that never finished; a live one is seconds old.
            return age(meta).is_some_and(|a| a > Duration::from_secs(3600));
        }
        age(meta).is_some_and(|a| a > KEEP) || !made_by_this_cig(path)
    })
}

/// Remove every entry. Returns what was removed.
pub fn clear() -> Stats {
    remove_where(|_, _| true)
}

fn age(meta: &fs::Metadata) -> Option<Duration> {
    SystemTime::now().duration_since(meta.modified().ok()?).ok()
}

fn remove_where(gone: impl Fn(&Path, &fs::Metadata) -> bool) -> Stats {
    let mut removed = Stats::default();
    for (path, meta) in files() {
        if gone(&path, &meta) && fs::remove_file(&path).is_ok() && is_entry(&path) {
            removed.entries += 1;
            removed.bytes += meta.len();
        }
    }
    // Shards left empty go too; one that still holds something stays,
    // and so does anything that is not a folder of the cache's own.
    if let Ok(shards) = fs::read_dir(root()) {
        for shard in shards.flatten() {
            let path = shard.path();
            if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_dir()) {
                let _ = fs::remove_dir(&path);
            }
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch state directory and keys directory for one test.
    struct Home {
        _tmp: tempfile::TempDir,
        _lock: std::sync::MutexGuard<'static, ()>,
        old: [Option<std::ffi::OsString>; 2],
    }

    impl Home {
        fn new() -> Home {
            let lock = crate::burn::test_env_lock();
            let tmp = tempfile::tempdir().unwrap();
            let old = [
                std::env::var_os("CIGSCRIPT_HOME"),
                std::env::var_os("CIG_KEYS_DIR"),
            ];
            std::env::set_var("CIGSCRIPT_HOME", tmp.path().join("home"));
            std::env::set_var("CIG_KEYS_DIR", tmp.path().join("keys"));
            Home {
                _tmp: tmp,
                _lock: lock,
                old,
            }
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            for (name, old) in ["CIGSCRIPT_HOME", "CIG_KEYS_DIR"].iter().zip(&self.old) {
                match old {
                    Some(v) => std::env::set_var(name, v),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    /// Bytes long enough to stand in for a module: the cache reads only
    /// the module's header, which `vm::open` checks against the body.
    fn module(fill: u8) -> Vec<u8> {
        vec![fill; MODULE_HEADER_LEN + 100]
    }

    #[test]
    fn a_stored_module_comes_back_with_its_flag() {
        let _home = Home::new();
        let key = key_for("aa");
        assert!(lookup(&key).is_none(), "nothing stored yet");
        store(&key, &module(7), false).unwrap();
        let e = lookup(&key).expect("a hit");
        assert_eq!(e.module, module(7));
        assert!(!e.warned);
        store(&key, &module(8), true).unwrap();
        let e = lookup(&key).expect("a hit");
        assert_eq!(e.module, module(8));
        assert!(e.warned);
        assert_eq!(stats().entries, 1);
    }

    #[test]
    fn another_script_is_another_key() {
        let _home = Home::new();
        store(&key_for("aa"), &module(1), false).unwrap();
        assert!(lookup(&key_for("ab")).is_none());
        assert!(key_for("aa") == key_for("aa"));
        assert!(key_for("aa") != key_for("ab"));
    }

    #[test]
    fn a_changed_byte_anywhere_is_a_miss() {
        let _home = Home::new();
        let key = key_for("aa");
        store(&key, &module(3), false).unwrap();
        let path = path_of(&key);
        let good = fs::read(&path).unwrap();
        // The header's fields, the MAC, and the module's own header: each
        // is under the MAC or checked against the key. (The module's body
        // is covered by the hash in its header, which `vm::open` checks.)
        for at in [0, 9, 12, 17, 33, 64, MAC_AT, HEADER_LEN, HEADER_LEN + 39] {
            let mut bad = good.clone();
            bad[at] ^= 1;
            fs::write(&path, &bad).unwrap();
            assert!(lookup(&key).is_none(), "byte {at} changed and still a hit");
        }
        fs::write(&path, &good[..good.len() - 1]).unwrap();
        assert!(lookup(&key).is_none(), "cut short and still a hit");
        fs::write(&path, &good).unwrap();
        assert!(lookup(&key).is_some());
    }

    #[test]
    fn an_entry_under_another_key_does_not_verify() {
        let _home = Home::new();
        let (a, b) = (key_for("aa"), key_for("ab"));
        store(&a, &module(1), false).unwrap();
        // Moved to another script's place: the key inside says whose it is.
        let to = path_of(&b);
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::copy(path_of(&a), &to).unwrap();
        assert!(lookup(&b).is_none());
    }

    #[test]
    fn an_entry_made_under_another_machines_key_is_a_miss() {
        let _home = Home::new();
        let key = key_for("aa");
        store(&key, &module(1), false).unwrap();
        let key_path = crate::burn::seal::keys_dir().join(KEY_FILE);
        fs::remove_file(&key_path).unwrap();
        assert!(lookup(&key).is_none(), "no key, no hit");
        // A new key is made by the next store; the old entry stays a miss
        // until it is rewritten under it.
        let other = key_for("ab");
        store(&other, &module(2), false).unwrap();
        assert!(lookup(&key).is_none());
        assert!(lookup(&other).is_some());
    }

    #[cfg(unix)]
    #[test]
    fn a_key_others_can_read_is_not_used() {
        use std::os::unix::fs::PermissionsExt;
        let _home = Home::new();
        let key = key_for("aa");
        store(&key, &module(1), false).unwrap();
        let key_path = crate::burn::seal::keys_dir().join(KEY_FILE);
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(lookup(&key).is_none());
        assert!(store(&key, &module(1), false).is_err(), "and not replaced");
    }

    /// Whatever is put where the cache's folders go, the sweep removes
    /// only the cache's own files: it enters no link, and a file with
    /// another name stays.
    #[cfg(unix)]
    #[test]
    fn sweep_and_clear_touch_nothing_that_is_not_the_caches_own() {
        let home = Home::new();
        let key = key_for("aa");
        store(&key, &module(1), false).unwrap();
        // A folder elsewhere, with an old file, linked in as a shard.
        let outside = home._tmp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        let precious = outside.join("notes.txt");
        fs::write(&precious, "keep me").unwrap();
        let looks_like_ours = outside.join(key_for("ab").hex());
        fs::write(&looks_like_ours, "not an entry of this cache").unwrap();
        for f in [&precious, &looks_like_ours] {
            let h = fs::OpenOptions::new().write(true).open(f).unwrap();
            h.set_modified(SystemTime::now() - KEEP - Duration::from_secs(60))
                .unwrap();
        }
        std::os::unix::fs::symlink(&outside, root().join("zz")).unwrap();
        // A stray file inside a real shard, with a name that is not the
        // cache's.
        let shard = path_of(&key).parent().unwrap().to_path_buf();
        let stray = shard.join("README");
        fs::write(&stray, "someone's").unwrap();
        let h = fs::OpenOptions::new().write(true).open(&stray).unwrap();
        h.set_modified(SystemTime::now() - KEEP - Duration::from_secs(60))
            .unwrap();
        drop(h);
        assert_eq!(stats().entries, 1, "the link's files are not counted");
        assert_eq!(sweep(), Stats::default(), "nothing of the cache's is stale");
        assert_eq!(clear().entries, 1);
        assert!(precious.exists() && looks_like_ours.exists() && stray.exists());
        assert!(
            fs::symlink_metadata(root().join("zz")).is_ok(),
            "the link stays"
        );
    }

    /// What a write that never finished leaves is removed once it is old.
    #[test]
    fn sweep_removes_an_old_leftover_of_a_write() {
        let _home = Home::new();
        let key = key_for("aa");
        store(&key, &module(1), false).unwrap();
        let shard = path_of(&key).parent().unwrap().to_path_buf();
        let fresh = shard.join(format!(".{}.123.tmp", key.hex()));
        let old = shard.join(format!(".{}.456.tmp", key.hex()));
        fs::write(&fresh, "half").unwrap();
        fs::write(&old, "half").unwrap();
        let h = fs::OpenOptions::new().write(true).open(&old).unwrap();
        h.set_modified(SystemTime::now() - Duration::from_secs(2 * 3600))
            .unwrap();
        drop(h);
        sweep();
        assert!(fresh.exists(), "a write in flight is left alone");
        assert!(!old.exists());
        assert!(lookup(&key).is_some());
    }

    #[test]
    fn sweep_drops_what_another_cig_made_and_what_is_old() {
        let _home = Home::new();
        let (mine, theirs, old) = (key_for("aa"), key_for("ab"), key_for("ac"));
        for k in [&mine, &theirs, &old] {
            store(k, &module(1), false).unwrap();
        }
        // Another cig's entry: its version field differs.
        let p = path_of(&theirs);
        let mut bytes = fs::read(&p).unwrap();
        bytes[16] ^= 0x20;
        fs::write(&p, &bytes).unwrap();
        // An entry nobody loaded for longer than it is kept.
        let f = fs::OpenOptions::new()
            .write(true)
            .open(path_of(&old))
            .unwrap();
        f.set_modified(SystemTime::now() - KEEP - Duration::from_secs(60))
            .unwrap();
        drop(f);
        let removed = sweep();
        assert_eq!(removed.entries, 2);
        assert!(lookup(&mine).is_some());
        assert_eq!(stats().entries, 1);
        assert_eq!(clear().entries, 1);
        assert_eq!(stats(), Stats::default());
    }
}
