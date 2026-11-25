// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The module cache: lowered, verified modules kept under the tin, so that
//! `cig run` on the VM lowers a script once and loads it from then on
//! (#254).
//!
//! **The key.** An entry is found by the SHA-256 over this cig's build
//! (its version, and its executable's size and time), the lowering's
//! version, the optimization level and the script's own SHA-256. The same
//! bytes on the same cig give the same key; an edit, an update of cig, a
//! rebuild or a change to the lowering gives another, and the old entry is
//! never asked for again ([`sweep`] removes it).
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
//! **What an entry holds.** The script in another form: its names and its
//! literals are readable in it. It is kept as the tin keeps any file a
//! burn changed: owner-only, not encrypted. An entry is not sealed because
//! opening it would then cost time for every byte on every run, and
//! because a later cache of native code must be mapped as it lies.
//!
//! **Nothing here fails a run, or holds one up.** An entry that is
//! missing, cut short, larger than a module may be, not a plain file, for
//! another build, or that does not verify is a miss: the script is lowered
//! again and the entry rewritten. Only a plain file is opened, and no more
//! of it is read than its two headers until the MAC over them verifies. A
//! cache that cannot be written (no keys directory, a full disk, a folder
//! that is not its own) costs the lowering on every run and nothing else.

use cig_bytecode::serialize::HEADER_LEN as MODULE_HEADER_LEN;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

/// The folder under the tin that holds the entries.
pub const DIR: &str = "modules";
/// The module key's file name, in the keys directory.
pub const KEY_FILE: &str = "module.key";
/// How long an entry nobody loaded is kept before [`sweep`] removes it.
pub const KEEP: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// The largest module kept or loaded: what a compiled program may carry
/// ([`crate::program::MAX_PAYLOAD2`]). A larger one is lowered on every
/// run, and a larger file in an entry's place is never read.
pub const MAX_MODULE: u64 = crate::program::MAX_PAYLOAD2;

const MAGIC: [u8; 8] = *b"CIGMOD1\0";
const FORMAT: u32 = 1;
/// The check gave warnings for this script: a run prints them, so it
/// checks the script again instead of taking the entry on its word alone.
const FLAG_WARNED: u32 = 1;
/// The entry's payload is not a module but a refusal: the UTF-8 reason the
/// lowering did not take this script, so a later run under `auto` picks
/// the tree-walker without trying it again.
const FLAG_REFUSED: u32 = 1 << 1;
/// Every flag the format knows: a bit this build does not recognise is a
/// miss.
const KNOWN_FLAGS: u32 = FLAG_WARNED | FLAG_REFUSED;
const BUILD_LEN: usize = 16;
/// magic, format, flags, the build, the key, the payload's length, the MAC.
const HEADER_LEN: usize = 8 + 4 + 4 + BUILD_LEN + 32 + 8 + 32;
const MAC_AT: usize = HEADER_LEN - 32;
/// The optimization level entries are keyed under: the one `cig run`
/// lowers at.
const LEVEL: u8 = crate::lower::DEFAULT_LEVEL;
/// A loaded entry's modification time is moved forward when it is older
/// than this, so [`sweep`] can tell an entry in use from one left behind
/// without a write on every run.
const TOUCH_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
/// A refusal message is a short line, not a module: keep an upper bound so
/// a file put in an entry's place cannot ask for much memory.
const MAX_REFUSAL: usize = 4096;
/// The name of the marker [`claim_lowering`] writes beside a module while a
/// `__lower` subprocess is working on the script. One at a time per script.
const LOWERING_SUFFIX: &str = ".lowering";
/// A marker older than this is treated as stale: a `__lower` that crashed,
/// was killed, or was abandoned should not keep the cache quiet for ever.
pub const LOWERING_STALE_AFTER: Duration = Duration::from_secs(10 * 60);
/// The name of the mark `cig run --engine auto` writes at a script's first
/// sighting. Its presence says the script has been seen before, so the
/// next run on `auto` spawns the lowering instead of writing another
/// mark. Holds nothing.
const SEEN_SUFFIX: &str = ".seen";

/// Where the entries live: `<state>/tin/modules`.
pub fn root() -> PathBuf {
    super::tin::dir().join(DIR)
}

/// This cig's build: its version, and its executable's size and time. Two
/// builds of one version (a checkout built again after a change) are two
/// builds, so one never loads what the other lowered. Where the executable
/// cannot be asked, the version stands alone.
fn build() -> [u8; BUILD_LEN] {
    static BUILD: OnceLock<[u8; BUILD_LEN]> = OnceLock::new();
    *BUILD.get_or_init(|| {
        let mut h = Sha256::new();
        h.update(b"cig build\0");
        h.update(crate::VERSION.as_bytes());
        h.update([0]);
        if let Ok(meta) = std::env::current_exe().and_then(fs::metadata) {
            h.update(meta.len().to_le_bytes());
            let made = meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok());
            if let Some(made) = made {
                h.update(made.as_secs().to_le_bytes());
                h.update(made.subsec_nanos().to_le_bytes());
            }
        }
        let digest = h.finalize();
        let mut out = [0u8; BUILD_LEN];
        out.copy_from_slice(&digest[..BUILD_LEN]);
        out
    })
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
    h.update(build());
    h.update(crate::lower::VERSION.to_le_bytes());
    h.update([LEVEL]);
    h.update(script_sha256.as_bytes());
    Key(h.finalize().into())
}

/// What a hit gives back: the module, or a refusal the lowering recorded.
pub enum Hit {
    /// The module's bytes, for [`crate::vm::open`] and nothing else: the
    /// MAC covers the module's header, and it is `vm::open` that checks
    /// the body against the hash that header carries.
    Module {
        module: Vec<u8>,
        /// The check gave warnings when the entry was made.
        warned: bool,
    },
    /// The lowering did not take this script, and nothing has changed about
    /// the script or this cig since; the caller runs it on the tree-walker
    /// and never tries the lowering again until the script or the build
    /// changes.
    Refused { reason: String },
}

fn path_of(key: &Key) -> PathBuf {
    let name = key.hex();
    root().join(&name[..2]).join(name)
}

fn header(key: &Key, flags: u32, len: u64) -> [u8; HEADER_LEN] {
    let mut h = [0u8; HEADER_LEN];
    h[..8].copy_from_slice(&MAGIC);
    h[8..12].copy_from_slice(&FORMAT.to_le_bytes());
    h[12..16].copy_from_slice(&flags.to_le_bytes());
    h[16..16 + BUILD_LEN].copy_from_slice(&build());
    h[32..64].copy_from_slice(&key.0);
    h[64..72].copy_from_slice(&len.to_le_bytes());
    h
}

/// The MAC over an entry's header (without the MAC's own bytes) and the
/// cover bytes that follow it. For a module the cover is the module's own
/// header, which carries the hash of the module's body; for a refusal it is
/// the whole payload (the reason text), so no byte of a refusal is left
/// unchecked.
fn mac(mac_key: &[u8; 32], header: &[u8; HEADER_LEN], cover: &[u8]) -> blake3::Hash {
    let mut h = blake3::Hasher::new_keyed(mac_key);
    h.update(&header[..MAC_AT]);
    h.update(cover);
    h.finalize()
}

/// The module key: 32 random bytes in the keys directory, owner-only, made
/// the first time an entry is stored. `None` where there is no usable key:
/// the cache is then off for this run. A lookup makes nothing: where no
/// key was ever stored, it does not so much as create the keys directory.
fn mac_key(create: bool) -> Option<[u8; 32]> {
    let path = super::seal::keys_dir().join(KEY_FILE);
    if !create && fs::symlink_metadata(&path).is_err() {
        return None;
    }
    let dir = super::seal::ensure_keys_dir().ok()?;
    let path = dir.join(KEY_FILE);
    if let Some(k) = read_key(&path) {
        return Some(k);
    }
    if !create {
        return None;
    }
    if fs::symlink_metadata(&path).is_ok() {
        // Something is there. A key another cig linked in a moment ago
        // reads now; anything else is not a key this cig will use, and it
        // is never replaced.
        return read_key(&path);
    }
    let mut k = [0u8; 32];
    getrandom::getrandom(&mut k).ok()?;
    // Written under another name and linked into place: the key appears
    // whole or not at all, and two cigs starting together agree on the one
    // that got there first.
    let tmp = dir.join(format!(".{KEY_FILE}.{}", nonce()?));
    let written = private_file(&tmp).and_then(|mut f| f.write_all(&k));
    if written.is_ok() {
        let _ = fs::hard_link(&tmp, &path);
    }
    let _ = fs::remove_file(&tmp);
    read_key(&path)
}

/// A number no other writer is using, for a temporary file's name: two
/// cigs in two containers can share a tin and a process id.
fn nonce() -> Option<u64> {
    let mut n = [0u8; 8];
    getrandom::getrandom(&mut n).ok()?;
    Some(u64::from_le_bytes(n))
}

/// Why the file in the module key's place is not used, if one is there
/// and it is not.
fn key_refusal(path: &Path) -> Option<&'static str> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() {
        return Some("it is not a plain file");
    }
    if meta.len() != 32 {
        return Some("it is not 32 bytes long");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != euid() {
            return Some("it is not yours");
        }
        if meta.mode() & 0o077 != 0 {
            return Some("others can read it");
        }
    }
    None
}

/// The key in `path`, when it is a plain file of exactly 32 bytes that is
/// this user's alone.
fn read_key(path: &Path) -> Option<[u8; 32]> {
    fs::symlink_metadata(path).ok()?;
    if key_refusal(path).is_some() {
        return None;
    }
    let mut key = [0u8; 32];
    open_plain(path).ok()?.read_exact(&mut key).ok()?;
    Some(key)
}

#[cfg(unix)]
fn euid() -> u32 {
    // SAFETY: `geteuid` takes nothing and cannot fail.
    unsafe { libc::geteuid() }
}

/// Why the cache is off on this machine, when it is: something stands in
/// the place of its key or of its folder that cig will not use. `None`
/// when nothing is wrong, which includes a cache that was never used. For
/// `cig doctor`: nothing else says so, because an unusable cache fails no
/// run.
pub fn problem() -> Option<String> {
    let key = super::seal::keys_dir().join(KEY_FILE);
    if let Some(why) = key_refusal(&key) {
        return Some(format!(
            "the module key {} is not used ({why}); remove it and the next run on the vm makes a new one",
            crate::crash::tilde(&key)
        ));
    }
    let root = root();
    if fs::symlink_metadata(&root).is_ok() && !is_own_dir(&root) {
        return Some(format!(
            "{} is not a folder of your own, so no module is kept there; remove it and the next run on the vm makes it",
            crate::crash::tilde(&root)
        ));
    }
    None
}

/// Open a file to read, and to move its time forward through the same
/// handle. A link is refused, and something that is not a file is never
/// waited on: a pipe put in a file's place would hold a plain open until
/// someone wrote to it.
fn open_plain(path: &Path) -> io::Result<fs::File> {
    let mut opts = fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // GENERIC_READ, and FILE_WRITE_ATTRIBUTES for the file's time.
        opts.access_mode(0x8000_0000 | 0x0100);
    }
    let file = opts.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a plain file",
        ));
    }
    Ok(file)
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

fn private_dirs() -> fs::DirBuilder {
    // Only Unix has a mode to set.
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut b = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b
}

/// Is `path` a real folder (not a link to one) that is this user's?
fn is_own_dir(path: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != euid() {
            return false;
        }
    }
    meta.file_type().is_dir()
}

/// `path` as a folder of the cache's own, made owner-only when it is not
/// there. A link or anything else in its place is refused, never followed:
/// nothing is written through it.
fn own_dir(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path).is_err() {
        match private_dirs().create(path) {
            // Another cig made it between the look and the make.
            Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
            _ => {}
        }
    }
    if is_own_dir(path) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a folder of the cache's own",
        ))
    }
}

/// The entry for `key`, when there is one this machine made for this
/// build.
pub fn lookup(key: &Key) -> Option<Hit> {
    let path = path_of(key);
    if !fs::symlink_metadata(&path).ok()?.file_type().is_file() {
        return None;
    }
    let mut file = open_plain(&path).ok()?;
    let meta = file.metadata().ok()?;
    let total = meta.len();
    let mut head = [0u8; HEADER_LEN];
    if total < HEADER_LEN as u64 {
        return None;
    }
    file.read_exact(&mut head).ok()?;
    let len = u64::from_le_bytes(head[64..72].try_into().ok()?);
    let flags = u32::from_le_bytes(head[12..16].try_into().ok()?);
    let refused = flags & FLAG_REFUSED != 0;
    let payload_max = if refused {
        MAX_REFUSAL as u64
    } else {
        MAX_MODULE
    };
    let payload_min = if refused { 0 } else { MODULE_HEADER_LEN as u64 };
    if head[..8] != MAGIC
        || head[8..12] != FORMAT.to_le_bytes()
        || head[16..16 + BUILD_LEN] != build()
        || head[32..64] != key.0
        || len != total - HEADER_LEN as u64
        || len < payload_min
        || len > payload_max
        || flags & !KNOWN_FLAGS != 0
    {
        return None;
    }
    let mac_key = mac_key(false)?;
    if refused {
        // The MAC covers the header and the whole refusal payload: a
        // refusal's text is short, so the file is read in full before the
        // MAC is checked.
        let mut body = vec![0u8; usize::try_from(len).ok()?];
        file.read_exact(&mut body).ok()?;
        let want = mac(&mac_key, &head, &body);
        if want != blake3::Hash::from_bytes(head[MAC_AT..].try_into().ok()?) {
            return None;
        }
        let reason = String::from_utf8(body).ok()?;
        touch_if_old(&file, &meta);
        Some(Hit::Refused { reason })
    } else {
        // The MAC before anything else: it covers both headers, so until it
        // verifies no more of the file is read and no memory is asked for on
        // the file's word.
        let mut module = vec![0u8; MODULE_HEADER_LEN];
        file.read_exact(&mut module).ok()?;
        let want = mac(&mac_key, &head, &module);
        // `blake3::Hash` compares in constant time.
        if want != blake3::Hash::from_bytes(head[MAC_AT..].try_into().ok()?) {
            return None;
        }
        module.resize(usize::try_from(len).ok()?, 0);
        file.read_exact(&mut module[MODULE_HEADER_LEN..]).ok()?;
        touch_if_old(&file, &meta);
        Some(Hit::Module {
            module,
            warned: flags & FLAG_WARNED != 0,
        })
    }
}

/// Move an old entry's time forward, through the handle it was read by:
/// only an entry that verified is ever touched, and never by its name.
fn touch_if_old(file: &fs::File, meta: &fs::Metadata) {
    if age(meta).is_some_and(|age| age > TOUCH_AFTER) {
        let _ = file.set_modified(SystemTime::now());
    }
}

/// Keep `module` under `key`. The entry appears whole or not at all: it is
/// written beside its place and renamed into it.
pub fn store(key: &Key, module: &[u8], warned: bool) -> io::Result<()> {
    if module.len() < MODULE_HEADER_LEN || module.len() as u64 > MAX_MODULE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a module the cache keeps",
        ));
    }
    let flags = if warned { FLAG_WARNED } else { 0 };
    write_entry(key, flags, module, &module[..MODULE_HEADER_LEN])
}

/// Keep a refusal under `key`: the reason the lowering did not take this
/// script, so a later run under `--engine auto` picks the tree-walker
/// without starting another lowering until the script or this cig changes.
/// The reason is trimmed to [`MAX_REFUSAL`] bytes; a shorter one goes in as
/// it is.
pub fn store_refusal(key: &Key, reason: &str) -> io::Result<()> {
    let bytes = reason.as_bytes();
    let body = if bytes.len() > MAX_REFUSAL {
        // UTF-8 safe truncation: step back to the last character boundary.
        let mut end = MAX_REFUSAL;
        while end > 0 && (bytes[end] & 0xC0) == 0x80 {
            end -= 1;
        }
        &bytes[..end]
    } else {
        bytes
    };
    write_entry(key, FLAG_REFUSED, body, body)
}

/// Write an entry (a module or a refusal) atomically: tmp file, rename
/// into place. `cover` is what the MAC covers after the header: the
/// module's header for a module, the whole payload for a refusal.
fn write_entry(key: &Key, flags: u32, payload: &[u8], cover: &[u8]) -> io::Result<()> {
    let mac_key =
        mac_key(true).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no module key"))?;
    let path = path_of(key);
    let shard = path.parent().unwrap_or(Path::new("."));
    private_dirs().recursive(true).create(super::tin::dir())?;
    own_dir(&root())?;
    own_dir(shard)?;
    let mut head = header(key, flags, payload.len() as u64);
    let tag = mac(&mac_key, &head, cover);
    head[MAC_AT..].copy_from_slice(tag.as_bytes());
    let unique = nonce().ok_or_else(|| io::Error::other("no random number"))?;
    let tmp = shard.join(format!(".{}.{unique}.tmp", key.hex()));
    let written = private_file(&tmp).and_then(|mut f| {
        f.write_all(&head)?;
        f.write_all(payload)
    });
    let placed = written.and_then(|()| fs::rename(&tmp, &path));
    if placed.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    placed
}

/// A marker beside the modules that says a `__lower` child is working
/// on this key, taken with `create_new` and `private_file` (the same way
/// an entry's temp file is made) so twenty runs that start together
/// never start twenty lowerings. Gives back a [`LoweringLock`] whose
/// `Drop` removes the marker.
///
/// A marker older than [`LOWERING_STALE_AFTER`] is treated as stale and
/// taken over: a `__lower` that crashed or was killed never keeps the
/// cache quiet for ever. A marker whose modified time cannot be read, or
/// whose modified time is in the future (a clock that went back), is
/// treated as stale too. Where the state folder cannot be made, this
/// returns `None` and the caller does not try to lower.
pub fn claim_lowering(key: &Key) -> Option<LoweringLock> {
    let path = marker_of(key);
    let shard = path.parent()?;
    private_dirs()
        .recursive(true)
        .create(super::tin::dir())
        .ok()?;
    own_dir(&root()).ok()?;
    own_dir(shard).ok()?;
    match private_file(&path) {
        Ok(_) => Some(LoweringLock { path }),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            // The marker is there; take it over if it is older than the
            // staleness limit or its modified time cannot be read.
            let stale = match fs::symlink_metadata(&path) {
                Ok(m) => match age(&m) {
                    Some(a) => a > LOWERING_STALE_AFTER,
                    // `age` is `None` for a time in the future (a clock
                    // that moved back) or one the OS cannot give: treat
                    // the marker as stale, since a marker that is not
                    // read is a marker the sweep would clean up anyway.
                    None => true,
                },
                Err(_) => true,
            };
            if stale {
                // Remove it and try once more; a concurrent `create_new`
                // wins the race if it got there first, and we give up.
                let _ = fs::remove_file(&path);
                match private_file(&path) {
                    Ok(_) => Some(LoweringLock { path }),
                    Err(_) => None,
                }
            } else {
                None
            }
        }
        Err(_) => None,
    }
}

/// A marker whose `Drop` removes the file it took.
pub struct LoweringLock {
    path: PathBuf,
}

impl Drop for LoweringLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn marker_of(key: &Key) -> PathBuf {
    let mut name = key.hex();
    name.push_str(LOWERING_SUFFIX);
    root().join(&key.hex()[..2]).join(name)
}

/// A name the sweep knows: `<hex>.lowering`.
fn is_marker(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(LOWERING_SUFFIX))
        .is_some_and(|key| key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Where the first-sighting mark for `key` lives.
fn seen_path_of(key: &Key) -> PathBuf {
    let mut name = key.hex();
    name.push_str(SEEN_SUFFIX);
    root().join(&key.hex()[..2]).join(name)
}

/// A name the sweep knows: `<hex>.seen`.
fn is_seen_mark(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(SEEN_SUFFIX))
        .is_some_and(|key| key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Is the first-sighting mark for `key` on disk? The first run of a
/// script on `auto` writes one; the second run reads it and spawns the
/// `__lower` child; `__lower` removes it when it keeps a module (or a
/// refusal). A mark lost to a crash costs one more tree-walker run, so
/// nothing here is synced.
pub fn is_seen(key: &Key) -> bool {
    fs::symlink_metadata(seen_path_of(key)).is_ok_and(|m| m.file_type().is_file())
}

/// Write the first-sighting mark for `key`, atomically: if a mark is
/// already there, this is a no-op success; a missing state folder or a
/// shard that is not a folder of the cache's own is an error. The file
/// holds nothing and is not synced.
pub fn mark_seen(key: &Key) -> io::Result<()> {
    let path = seen_path_of(key);
    let shard = path.parent().ok_or_else(|| io::Error::other("no shard"))?;
    private_dirs().recursive(true).create(super::tin::dir())?;
    own_dir(&root())?;
    own_dir(shard)?;
    match private_file(&path) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

/// Remove the first-sighting mark for `key`, if there is one. `__lower`
/// calls this after it keeps a module or a refusal: nothing is then
/// left of the mark on disk. A missing mark is not an error.
pub fn clear_seen(key: &Key) {
    let _ = fs::remove_file(seen_path_of(key));
}

/// What the cache holds: how many modules and refusals, and their bytes
/// on disk. `entries` counts every entry (modules and refusals), kept so
/// a reader of the field across cig versions sees the same total; the
/// two parts are in `modules` and `refusals` for a caller that wants to
/// say "modules" or distinguish the two.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub entries: usize,
    pub bytes: u64,
    pub modules: usize,
    pub refusals: usize,
}

/// The cache's own files: an entry, a lowering marker, or what a write
/// that never finished left beside one. Only real folders are entered and
/// only plain files are listed, never through a link: nothing outside the
/// cache is ever counted or removed, whatever is put in its place.
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
            if !(is_entry(&path) || is_leftover(&path) || is_marker(&path) || is_seen_mark(&path)) {
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

/// The name [`store`] writes under before it renames: `.<key>.<n>.tmp`.
fn is_leftover(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix('.'))
        .and_then(|n| n.strip_suffix(".tmp"))
        .and_then(|n| n.split_once('.'))
        .is_some_and(|(key, n)| {
            key.len() == 64
                && key.bytes().all(|b| b.is_ascii_hexdigit())
                && !n.is_empty()
                && n.bytes().all(|b| b.is_ascii_digit())
        })
}

/// The entries and their size, for `cig tin` and `cig doctor`. Modules
/// and refusals are counted apart; `entries` is their sum.
pub fn stats() -> Stats {
    let mut s = Stats::default();
    for (path, meta) in files() {
        if !is_entry(&path) {
            continue;
        }
        s.entries += 1;
        s.bytes += meta.len();
        // Read only the header to tell a module from a refusal: the
        // body is not touched here, and a file whose header will not
        // read counts as neither (but its bytes still count).
        let mut head = [0u8; HEADER_LEN];
        if open_plain(&path)
            .and_then(|mut f| f.read_exact(&mut head))
            .is_ok()
            && head[..8] == MAGIC
        {
            let flags = u32::from_le_bytes(head[12..16].try_into().unwrap_or([0u8; 4]));
            if flags & FLAG_REFUSED != 0 {
                s.refusals += 1;
            } else {
                s.modules += 1;
            }
        }
    }
    s
}

/// Was this entry made by this build? Read from its header alone.
fn made_by_this_build(path: &Path) -> bool {
    let mut head = [0u8; HEADER_LEN];
    open_plain(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok()
        && head[..8] == MAGIC
        && head[16..16 + BUILD_LEN] == build()
}

/// Remove what this cig will not load again: entries another build made,
/// entries nobody loaded for [`KEEP`], files a stopped write left behind,
/// and lowering markers older than an hour (a `__lower` that never
/// finished its work). Returns what was removed. Every entry is safe to
/// remove: its script is lowered again on its next run.
pub fn sweep() -> Stats {
    // The entries already here, by hex name, so the mark pass knows
    // which marks sit beside a kept module or refusal.
    let have_entry: std::collections::HashSet<String> = files()
        .iter()
        .filter(|(p, _)| is_entry(p))
        .filter_map(|(p, _)| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
        })
        .collect();
    remove_where(|path, meta| {
        if is_marker(path) || is_leftover(path) {
            // A marker or a write that never finished; a live one is
            // seconds old.
            return age(meta).is_some_and(|a| a > Duration::from_secs(3600));
        }
        if is_seen_mark(path) {
            // A mark beside a kept entry means nothing (an overlap of
            // a first run and a second: the child cleared the mark,
            // the first run wrote one again); remove it whatever its
            // age. Otherwise the mark is kept as long as an unloaded
            // module would be: a script run once a month is seen
            // once each month, and the lowering waits for the sighting
            // after that.
            let key = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(SEEN_SUFFIX))
                .unwrap_or("");
            if have_entry.contains(key) {
                return true;
            }
            return age(meta).is_some_and(|a| a > KEEP);
        }
        if !is_entry(path) {
            return false;
        }
        age(meta).is_some_and(|a| a > KEEP) || !made_by_this_build(path)
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
        tmp: tempfile::TempDir,
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
                tmp,
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

    /// A real module: a script lowered as `cig run` lowers it.
    fn lowered(source: &str) -> Vec<u8> {
        let passes = crate::lower::cir::pass::level(LEVEL);
        let m = crate::lower::bc::compile(source, "", &passes).expect("lowers");
        cig_bytecode::serialize_module(&m)
    }

    fn set_age(path: &Path, age: Duration) {
        let f = fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(SystemTime::now() - age).unwrap();
    }

    fn key_path() -> PathBuf {
        crate::burn::seal::keys_dir().join(KEY_FILE)
    }

    #[test]
    fn a_stored_module_comes_back_with_its_flag() {
        let _home = Home::new();
        let key = key_for("aa");
        assert!(lookup(&key).is_none(), "nothing stored yet");
        store(&key, &module(7), false).unwrap();
        let (bytes, warned) = match lookup(&key).expect("a hit") {
            Hit::Module { module, warned } => (module, warned),
            Hit::Refused { .. } => panic!("a refusal where a module was stored"),
        };
        assert_eq!(bytes, module(7));
        assert!(!warned);
        store(&key, &module(8), true).unwrap();
        let (bytes, warned) = match lookup(&key).expect("a hit") {
            Hit::Module { module, warned } => (module, warned),
            Hit::Refused { .. } => panic!("a refusal where a module was stored"),
        };
        assert_eq!(bytes, module(8));
        assert!(warned);
        assert_eq!(stats().entries, 1);
        assert!(problem().is_none());
    }

    #[test]
    fn a_refusal_comes_back_as_a_refusal_and_replaces_a_module() {
        let _home = Home::new();
        let key = key_for("aa");
        store_refusal(&key, "a construct the lowering does not take").unwrap();
        match lookup(&key).expect("a hit") {
            Hit::Refused { reason } => {
                assert_eq!(reason, "a construct the lowering does not take");
            }
            Hit::Module { .. } => panic!("a module where a refusal was stored"),
        }
        // A later run of the same lowering finds the construct lowers after
        // all, and the entry is overwritten with the module.
        store(&key, &module(3), false).unwrap();
        assert!(matches!(lookup(&key), Some(Hit::Module { .. })));
        assert_eq!(stats().entries, 1);
    }

    #[test]
    fn the_claim_is_one_at_a_time_and_old_markers_are_taken_over() {
        let _home = Home::new();
        let key = key_for("aa");
        let first = claim_lowering(&key).expect("a first claim");
        assert!(claim_lowering(&key).is_none(), "a second is refused");
        drop(first);
        let again = claim_lowering(&key).expect("after the first is dropped");
        // A stale marker is taken over, not kept for ever.
        let path = marker_of(&key);
        set_age(&path, LOWERING_STALE_AFTER + Duration::from_secs(60));
        drop(again); // Removes the marker; make another, mark it stale.
        let shard = path.parent().unwrap();
        fs::create_dir_all(shard).unwrap();
        fs::write(&path, "").unwrap();
        set_age(&path, LOWERING_STALE_AFTER + Duration::from_secs(60));
        assert!(claim_lowering(&key).is_some(), "stale is taken over");
    }

    #[test]
    fn a_lookup_makes_nothing() {
        let home = Home::new();
        assert!(lookup(&key_for("aa")).is_none());
        assert!(
            !home.tmp.path().join("keys").exists() && !home.tmp.path().join("home").exists(),
            "a miss left something behind"
        );
        assert!(problem().is_none(), "a cache never used is not a problem");
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
    fn a_changed_byte_in_either_header_is_a_miss() {
        let _home = Home::new();
        let key = key_for("aa");
        store(&key, &module(3), false).unwrap();
        let path = path_of(&key);
        let good = fs::read(&path).unwrap();
        // Every byte of the entry's header (its fields and the MAC) and of
        // the module's own header: each is under the MAC or checked
        // against the key.
        for at in 0..HEADER_LEN + MODULE_HEADER_LEN {
            let mut bad = good.clone();
            bad[at] ^= 1;
            fs::write(&path, &bad).unwrap();
            assert!(lookup(&key).is_none(), "byte {at} changed and still a hit");
        }
        fs::write(&path, &good[..good.len() - 1]).unwrap();
        assert!(lookup(&key).is_none(), "cut short and still a hit");
        let mut longer = good.clone();
        longer.push(0);
        fs::write(&path, &longer).unwrap();
        assert!(lookup(&key).is_none(), "a byte added and still a hit");
        fs::write(&path, &good).unwrap();
        assert!(lookup(&key).is_some());
    }

    /// The other half of the chain: a byte of the module's body is not
    /// under the MAC but under the hash in the module's header, and it is
    /// the VM that checks it. No changed byte anywhere in an entry runs.
    #[test]
    fn a_changed_byte_in_the_body_is_refused_by_the_vm() {
        let _home = Home::new();
        let key = key_for("aa");
        let bytes =
            lowered("roll total = 0\nfor n in [1, 2, 3] { total = total + n }\nexhale total\n");
        store(&key, &bytes, false).unwrap();
        let path = path_of(&key);
        let good = fs::read(&path).unwrap();
        match lookup(&key).expect("a hit") {
            Hit::Module { module, .. } => assert!(crate::vm::open(module).is_ok()),
            Hit::Refused { .. } => panic!("a refusal where a module was stored"),
        }
        for at in HEADER_LEN + MODULE_HEADER_LEN..good.len() {
            let mut bad = good.clone();
            bad[at] ^= 1;
            fs::write(&path, &bad).unwrap();
            let opened = match lookup(&key) {
                Some(Hit::Module { module, .. }) => Some(crate::vm::open(module).is_ok()),
                Some(Hit::Refused { .. }) | None => None,
            };
            assert_ne!(
                opened,
                Some(true),
                "byte {at} changed and the module opened"
            );
        }
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
        fs::remove_file(key_path()).unwrap();
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
    fn a_key_others_can_read_is_not_used_and_doctor_can_say_so() {
        use std::os::unix::fs::PermissionsExt;
        let _home = Home::new();
        let key = key_for("aa");
        store(&key, &module(1), false).unwrap();
        fs::set_permissions(key_path(), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(lookup(&key).is_none());
        assert!(store(&key, &module(1), false).is_err(), "and not replaced");
        let said = problem().expect("the cache is off, and that is said");
        assert!(said.contains("others can read it"), "{said}");
    }

    /// A pipe in an entry's place would hold an open for ever, and a file
    /// of any size could be put there: neither is read.
    #[cfg(unix)]
    #[test]
    fn what_is_not_a_plain_file_of_a_modules_size_is_never_read() {
        let _home = Home::new();
        let key = key_for("aa");
        store(&key, &module(1), false).unwrap();
        let path = path_of(&key);
        let good = fs::read(&path).unwrap();
        fs::remove_file(&path).unwrap();
        let made = std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .expect("mkfifo");
        assert!(made.success());
        assert!(lookup(&key).is_none(), "a pipe is a miss, at once");
        fs::remove_file(&path).unwrap();
        // The right header on a file far past any module's size: its
        // length says so before a byte of it is read.
        let f = fs::File::create(&path).unwrap();
        (&f).write_all(&good[..HEADER_LEN]).unwrap();
        f.set_len(HEADER_LEN as u64 + MAX_MODULE + 1).unwrap();
        drop(f);
        assert!(lookup(&key).is_none());
        // Within the size, a header whose length is a lie about a file
        // with nothing behind it: the MAC is asked before the body.
        let f = fs::File::create(&path).unwrap();
        let mut head = good[..HEADER_LEN].to_vec();
        head[64..72].copy_from_slice(&MAX_MODULE.to_le_bytes());
        (&f).write_all(&head).unwrap();
        f.set_len(HEADER_LEN as u64 + MAX_MODULE).unwrap();
        drop(f);
        assert!(lookup(&key).is_none());
        // A link to a real entry is not followed either.
        fs::remove_file(&path).unwrap();
        let elsewhere = path.with_extension("real");
        fs::write(&elsewhere, &good).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &path).unwrap();
        assert!(lookup(&key).is_none());
        // And the next store puts a real entry back in the link's place.
        store(&key, &module(1), false).unwrap();
        assert!(matches!(lookup(&key), Some(Hit::Module { .. })));
        assert_eq!(fs::read(&elsewhere).unwrap(), good, "written through");
    }

    /// Nothing is written through a link put where a folder of the
    /// cache's goes.
    #[cfg(unix)]
    #[test]
    fn a_link_in_a_folders_place_is_not_written_through() {
        let home = Home::new();
        let key = key_for("aa");
        let outside = home.tmp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        let shard = path_of(&key).parent().unwrap().to_path_buf();
        fs::create_dir_all(shard.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&outside, &shard).unwrap();
        assert!(store(&key, &module(1), false).is_err());
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
        assert!(problem().is_none(), "one shard is not the whole cache");
        // And the same for the cache's root.
        fs::remove_file(&shard).unwrap();
        fs::remove_dir(root()).unwrap();
        std::os::unix::fs::symlink(&outside, root()).unwrap();
        assert!(store(&key, &module(1), false).is_err());
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
        let said = problem().expect("the cache is off, and that is said");
        assert!(said.contains("not a folder of your own"), "{said}");
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
        let outside = home.tmp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        let precious = outside.join("notes.txt");
        fs::write(&precious, "keep me").unwrap();
        let looks_like_ours = outside.join(key_for("ab").hex());
        fs::write(&looks_like_ours, "not an entry of this cache").unwrap();
        // A stray file inside a real shard, with a name that is not the
        // cache's.
        let stray = path_of(&key).parent().unwrap().join("README");
        fs::write(&stray, "someone's").unwrap();
        for f in [&precious, &looks_like_ours, &stray] {
            set_age(f, KEEP + Duration::from_secs(60));
        }
        std::os::unix::fs::symlink(&outside, root().join("zz")).unwrap();
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
        set_age(&old, Duration::from_secs(2 * 3600));
        sweep();
        assert!(fresh.exists(), "a write in flight is left alone");
        assert!(!old.exists());
        assert!(lookup(&key).is_some());
    }

    #[test]
    fn sweep_drops_what_another_build_made_and_what_is_old() {
        let _home = Home::new();
        let (mine, theirs, old) = (key_for("aa"), key_for("ab"), key_for("ac"));
        for k in [&mine, &theirs, &old] {
            store(k, &module(1), false).unwrap();
        }
        // Another build's entry: its build field differs.
        let p = path_of(&theirs);
        let mut bytes = fs::read(&p).unwrap();
        bytes[16] ^= 0x20;
        fs::write(&p, &bytes).unwrap();
        // An entry nobody loaded for longer than it is kept.
        set_age(&path_of(&old), KEEP + Duration::from_secs(60));
        let removed = sweep();
        assert_eq!(removed.entries, 2);
        assert!(lookup(&mine).is_some());
        assert_eq!(stats().entries, 1);
        assert_eq!(clear().entries, 1);
        assert_eq!(stats(), Stats::default());
    }

    /// Loading an entry is what keeps it: one in use is never swept for
    /// its age.
    #[test]
    fn a_loaded_entry_that_was_old_is_kept_by_the_sweep() {
        let _home = Home::new();
        let key = key_for("aa");
        store(&key, &module(1), false).unwrap();
        set_age(&path_of(&key), KEEP + Duration::from_secs(60));
        assert!(
            matches!(lookup(&key), Some(Hit::Module { .. })),
            "an old entry still loads"
        );
        assert_eq!(sweep(), Stats::default(), "and loading it kept it");
    }

    #[test]
    fn a_temporary_name_is_one_the_sweep_knows() {
        let key = key_for("aa");
        let name = PathBuf::from(format!(".{}.{}.tmp", key.hex(), u64::MAX));
        assert!(is_leftover(&name));
        assert!(!is_entry(&name));
        assert!(is_entry(Path::new(&key.hex())));
        let marker = PathBuf::from(format!("{}{LOWERING_SUFFIX}", key.hex()));
        assert!(is_marker(&marker));
        assert!(!is_entry(&marker));
    }

    #[test]
    fn an_old_marker_is_swept() {
        let _home = Home::new();
        let key = key_for("aa");
        let locked = claim_lowering(&key).expect("a claim");
        // Writing the module here would drop the claim, but the sweep is
        // what we are measuring: age the marker past the hour.
        let path = marker_of(&key);
        set_age(&path, Duration::from_secs(2 * 3600));
        let removed = sweep();
        // The marker is not an entry, so the Stats are empty, but the
        // file is gone.
        assert_eq!(removed, Stats::default());
        assert!(!path.exists(), "the stale marker was swept");
        drop(locked); // Already gone; drop is a no-op.
    }

    #[test]
    fn a_seen_mark_is_made_and_removed() {
        let _home = Home::new();
        let key = key_for("aa");
        assert!(!is_seen(&key));
        mark_seen(&key).expect("mark");
        assert!(is_seen(&key), "the mark is there");
        // A second mark is a no-op; its presence does not become its
        // absence.
        mark_seen(&key).expect("idempotent");
        assert!(is_seen(&key));
        // Not counted as a module or a refusal.
        assert_eq!(stats(), Stats::default());
        clear_seen(&key);
        assert!(!is_seen(&key));
        clear_seen(&key); // no-op when nothing to clear
    }

    #[test]
    fn the_sweep_drops_an_old_mark_and_keeps_a_fresh_one() {
        let _home = Home::new();
        let fresh = key_for("aa");
        let old = key_for("ab");
        mark_seen(&fresh).unwrap();
        mark_seen(&old).unwrap();
        set_age(&seen_path_of(&old), KEEP + Duration::from_secs(60));
        let removed = sweep();
        assert_eq!(removed, Stats::default(), "a mark is not an entry");
        assert!(is_seen(&fresh), "a fresh mark stays");
        assert!(!is_seen(&old), "an old mark is swept");
    }

    #[test]
    fn the_sweep_removes_a_mark_beside_an_entry_whatever_its_age() {
        let _home = Home::new();
        let key = key_for("aa");
        store(&key, &module(1), false).unwrap();
        // Write a mark right beside the module. (In the real flow a
        // first-run write that overlaps the second is what makes
        // this; the sweep cleans it up.)
        mark_seen(&key).unwrap();
        assert!(is_seen(&key), "the mark is there");
        // The mark is fresh; the sweep removes it anyway.
        sweep();
        assert!(!is_seen(&key), "a mark beside an entry is swept");
        assert!(matches!(lookup(&key), Some(Hit::Module { .. })));
    }

    #[test]
    fn a_seen_mark_is_not_a_module() {
        let _home = Home::new();
        let key = key_for("aa");
        mark_seen(&key).unwrap();
        // The lookup is for modules and refusals; a mark does not open.
        assert!(lookup(&key).is_none());
        // `--engine vm` with only a mark will still lower (the mark is
        // not a module); the store overwrites nothing.
        store(&key, &module(1), false).unwrap();
        assert!(matches!(lookup(&key), Some(Hit::Module { .. })));
    }
}
