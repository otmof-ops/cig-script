// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Sealed: tin objects and snapshots encrypted at rest, so a tin that is on
//! for a whole project never keeps an old `.env`, a private key or a
//! kubeconfig in the clear. No crypto of our own: XChaCha20-Poly1305 in the
//! STREAM construction (`aead::stream`), one megabyte a segment, every
//! segment authenticated and the last one marked, so a byte flipped or a
//! tail cut off is refused on read. The key lives in the OS keyring (Secret
//! Service, Keychain, Credential Manager) and, where there is none, in a
//! file beside the tin that only its owner can read. A sealed object is
//! named by a random id, not its hash, and never shared between files, so
//! two equal secrets do not reveal that they are equal.

use chacha20poly1305::aead::stream::{DecryptorBE32, EncryptorBE32};
use chacha20poly1305::aead::{KeyInit, Payload};
use chacha20poly1305::XChaCha20Poly1305;
use globset::{Glob, GlobSet, GlobSetBuilder};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The first bytes of a sealed file this cig writes: version 2, sealed with
/// the seal subkey derived from the master key. Version 1 (`CIGSEAL1`) was
/// sealed with the master key itself and is still read, so objects sealed
/// before the key was split still open.
pub const MAGIC: &[u8; 8] = b"CIGSEAL2";
/// Version 1: the master key used directly, before per-use key separation.
pub const MAGIC_V1: &[u8; 8] = b"CIGSEAL1";

/// HKDF labels: one key never does two jobs. The stored secret is a master
/// key, and each use derives its own 32-byte subkey from it under its label.
/// `tin-seal` seals objects and snapshots; the MAC labels are reserved for
/// the signed journal and ledger (a later step).
pub const LABEL_SEAL: &[u8] = b"tin-seal";
pub const LABEL_JOURNAL_MAC: &[u8] = b"journal-mac";
pub const LABEL_LEDGER_MAC: &[u8] = b"ledger-mac";

/// A 32-byte subkey of the master, for one purpose, by HKDF-SHA256. The
/// returned subkey is a `Key`, so it wipes itself on drop.
pub fn derive(master: &Key, label: &[u8]) -> Key {
    let hk = Hkdf::<Sha256>::new(None, master.as_bytes());
    let mut out = [0u8; 32];
    hk.expand(label, &mut out)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    Key::new(out)
}
/// Plaintext bytes per segment.
const SEGMENT: usize = 1 << 20;
/// Poly1305's tag.
const TAG: usize = 16;
/// XChaCha20's 24-byte nonce less the STREAM counter and last-block flag.
const NONCE: usize = 19;
/// The suffix a sealed snapshot in a run directory (or a tree) carries.
pub const SUFFIX: &str = ".cig-sealed";

/// Names that are secrets until proven otherwise. Matched against the file
/// name alone, in any directory.
pub const SECRET_NAMES: &[&str] = &[
    ".env",
    ".env.*",
    "*.pem",
    "*.key",
    "id_rsa*",
    "id_ed25519*",
    "id_ecdsa*",
    "id_dsa*",
    "*.p12",
    "*.pfx",
    "credentials*",
    "*secret*",
    ".npmrc",
    ".pypirc",
    "kubeconfig",
    "*.kdbx",
    ".netrc",
    ".git-credentials",
    "*.ppk",
    "*.jks",
    "*.keystore",
];

/// What the `seal` setting asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// Secret-shaped names, `.cigseal` and `touches_sealed` (the default).
    Secrets,
    /// Everything the tin keeps.
    All,
    /// Nothing.
    Never,
}

pub fn policy() -> Policy {
    static P: OnceLock<Policy> = OnceLock::new();
    *P.get_or_init(
        || match crate::config::Config::load().get("seal").as_str() {
            "all" => Policy::All,
            "never" => Policy::Never,
            _ => Policy::Secrets,
        },
    )
}

fn secret_set() -> &'static GlobSet {
    static SET: OnceLock<GlobSet> = OnceLock::new();
    SET.get_or_init(|| {
        let mut b = GlobSetBuilder::new();
        for p in SECRET_NAMES {
            if let Ok(g) = Glob::new(p) {
                b.add(g);
            }
        }
        b.build().expect("the built-in secret names are globs")
    })
}

/// Does the file's name alone make it a secret?
pub fn secret_name(path: &Path) -> bool {
    path.file_name()
        .map(|n| secret_set().is_match(Path::new(n)))
        .unwrap_or(false)
}

/// The `.cigseal` of a pack root, as a glob set: one glob per line, `#`
/// comments, relative to the root; `None` when there is none.
pub fn seal_set(root: &Path) -> Option<GlobSet> {
    let text = fs::read_to_string(root.join(".cigseal")).ok()?;
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

/// Should this file be kept sealed? By the setting, by its name, by the
/// pack root's `.cigseal`, or by the hop's `touches_sealed` globs (both
/// matched on the path relative to the root).
pub fn should_seal(path: &Path, root: Option<&Path>, sets: &[&GlobSet]) -> bool {
    match policy() {
        Policy::Never => false,
        Policy::All => true,
        Policy::Secrets => {
            if secret_name(path) {
                return true;
            }
            let Some(root) = root else {
                return false;
            };
            let Ok(rel) = path.strip_prefix(root) else {
                return false;
            };
            sets.iter().any(|s| s.is_match(rel))
        }
    }
}

/// Should this file be kept sealed, judged from the file alone: the
/// setting, its name, and any `.cigseal` in a directory above it (a
/// project's list applies to the whole tree below it). What a rollback
/// displaces, a native op's snapshot and a tree snapshot ask this.
pub fn should_seal_here(path: &Path) -> bool {
    match policy() {
        Policy::Never => false,
        Policy::All => true,
        Policy::Secrets => {
            if secret_name(path) {
                return true;
            }
            let mut dir = path.parent();
            let mut depth = 0;
            while let Some(d) = dir {
                if depth > 64 {
                    break;
                }
                if let Some(set) = cached_seal_set(d) {
                    if path.strip_prefix(d).is_ok_and(|rel| set.is_match(rel)) {
                        return true;
                    }
                }
                dir = d.parent();
                depth += 1;
            }
            false
        }
    }
}

/// A directory's `.cigseal`, compiled once and reused while the file is
/// unchanged (its size and modification time). A tree snapshot asks for
/// every file it copies, up every directory above it; reading and compiling
/// the same list for each one made a large tree slow for nothing.
fn cached_seal_set(dir: &Path) -> Option<std::rc::Rc<GlobSet>> {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;
    type Stamp = (u64, Option<std::time::SystemTime>);
    type Cache = HashMap<PathBuf, (Stamp, Option<Rc<GlobSet>>)>;
    thread_local! {
        static CACHE: RefCell<Cache> = RefCell::new(HashMap::new());
    }
    let meta = fs::metadata(dir.join(".cigseal"))
        .ok()
        .filter(|m| m.is_file())?;
    let stamp: Stamp = (meta.len(), meta.modified().ok());
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if let Some((seen, set)) = c.get(dir) {
            if *seen == stamp {
                return set.clone();
            }
        }
        let set = seal_set(dir).map(Rc::new);
        c.insert(dir.to_path_buf(), (stamp, set.clone()));
        set
    })
}

/// Is this snapshot path a sealed object or snapshot?
pub fn is_sealed_path(path: &Path) -> bool {
    path.starts_with(super::tin::sealed_dir())
        || path
            .file_name()
            .map(|n| n.to_string_lossy().ends_with(SUFFIX))
            .unwrap_or(false)
}

// ----- the key -------------------------------------------------------------------

/// A 32-byte secret: the master, or a subkey derived from it. Not `Copy`
/// (so a stack copy is never left behind when the value is moved), and its
/// `Drop` writes zeroes over the bytes through a volatile store, so the
/// backing memory is wiped rather than left for the allocator to hand out
/// again (#105, L5). `Clone` is kept but explicit, so a copy is a deliberate
/// act.
pub struct Key([u8; 32]);

/// A compile-time refusal: if someone adds `Copy` to `Key`, this const's
/// body stops type-checking. The trick is a blanket impl keyed on `Copy`:
/// with the plain impl it is unambiguous, with the `Copy` one it is a
/// conflict (static_assertions-style, with no new dependency).
const _: fn() = || {
    struct Check<T: ?Sized>(core::marker::PhantomData<T>);
    trait AmbiguousIfImpl<A> {
        fn resolve() {}
    }
    impl<T: ?Sized> AmbiguousIfImpl<()> for Check<T> {}
    #[allow(dead_code)]
    impl<T: Copy> AmbiguousIfImpl<u8> for Check<T> {}
    let _ = <Check<Key> as AmbiguousIfImpl<_>>::resolve;
};

impl Key {
    /// Build a key from 32 bytes. The caller's own copy is still on the
    /// stack: wiping it is the caller's job (`wipe_bytes` does it).
    pub fn new(bytes: [u8; 32]) -> Self {
        Key(bytes)
    }

    /// Borrow the 32 bytes, for a cipher or HMAC that wants a slice. A
    /// borrow never escapes a `Key`, so a copy is a deliberate act.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl Clone for Key {
    fn clone(&self) -> Self {
        Key(self.0)
    }
}

impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        // Constant-time: the rekey's keeper read-back compares a fresh
        // `Key` with the key just written, and a short-circuiting compare
        // would leak the mismatch position to a local observer. Cheap
        // (32 bytes) and the usual-case callers (two values that match)
        // pay nothing on top.
        ct_eq_bytes(&self.0, &other.0)
    }
}
impl Eq for Key {}

/// Constant-time byte-slice equality. Lengths that differ are not equal and
/// return at once; equal-length slices compare every byte.
fn ct_eq_bytes(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Key").field(&"<32 bytes>").finish()
    }
}

impl TryFrom<&[u8]> for Key {
    type Error = ();
    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        let arr: [u8; 32] = bytes.try_into().map_err(|_| ())?;
        Ok(Key(arr))
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        wipe_bytes(&mut self.0);
    }
}

/// Zero a buffer through volatile writes, so the compiler cannot elide
/// them. A compiler fence after is defence in depth (nothing may reorder
/// past it).
pub fn wipe_bytes(buf: &mut [u8]) {
    for b in buf.iter_mut() {
        // SAFETY: a plain write through a valid &mut u8, kept by volatile.
        unsafe { std::ptr::write_volatile(b, 0u8) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

/// Where the master key is kept when the keyring has none: apart from the
/// tin it seals, so a copy of the tin never takes the key with it.
/// `CIG_SEAL_KEY_FILE` names it outright (CI, headless); otherwise it is
/// `seal.key` under the keys directory.
pub fn key_path() -> PathBuf {
    if let Some(f) = std::env::var_os("CIG_SEAL_KEY_FILE") {
        return PathBuf::from(f);
    }
    keys_dir().join("seal.key")
}

/// The directory the fallback key lives in, outside the tin. With
/// `CIGSCRIPT_HOME` set (tests, a custom state root) it sits beside that
/// home, never inside it; otherwise it is the OS config directory, so that
/// copying `~/.cigscript` leaves the key behind. `CIG_KEYS_DIR` overrides.
///
/// Where the parent directory is shared (world-writable, or sticky like
/// `/tmp` or `/var/tmp`), the folder's name gets a `-<uid>` suffix so two
/// users under the same shared parent do not share one `.cigscript-keys`.
/// A non-shared parent keeps the old name, so no existing install moves
/// (#176).
pub fn keys_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("CIG_KEYS_DIR") {
        return PathBuf::from(d);
    }
    if std::env::var_os("CIGSCRIPT_HOME").is_some() {
        let home = super::runs::home_dir();
        let home = std::path::absolute(&home).unwrap_or(home);
        let parent = home
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| home.clone());
        return parent.join(keys_dir_name_in(&parent));
    }
    os_config_dir().join("cigscript").join("keys")
}

/// The keys-folder name to use under `parent`: `.cigscript-keys` when the
/// parent is private, and `.cigscript-keys-<uid>` when the parent is
/// world-writable or has the sticky bit set (so two users under `/tmp`
/// never land in one folder).
#[cfg(unix)]
fn keys_dir_name_in(parent: &Path) -> String {
    use std::os::unix::fs::MetadataExt;
    let shared = fs::metadata(parent)
        .map(|m| (m.mode() & 0o002) != 0 || (m.mode() & 0o1000) != 0)
        .unwrap_or(false);
    if !shared {
        return ".cigscript-keys".to_string();
    }
    // SAFETY: geteuid() has no preconditions and returns the uid.
    let uid = unsafe { libc::geteuid() };
    let own = format!(".cigscript-keys-{uid}");
    // A folder this user made under the old shared name (cig 1.18.0 and
    // older) stays in use while it is still ours and private, so the key
    // and the ledger in it are not left behind. One someone else made is
    // never used: the per-user name is.
    if fs::symlink_metadata(parent.join(&own)).is_err() {
        if let Ok(m) = fs::symlink_metadata(parent.join(".cigscript-keys")) {
            if m.file_type().is_dir() && m.uid() == uid && (m.mode() & 0o022) == 0 {
                return ".cigscript-keys".to_string();
            }
        }
    }
    own
}

#[cfg(not(unix))]
fn keys_dir_name_in(_parent: &Path) -> String {
    // On Windows `%TEMP%` is per-user, so a shared parent does not arise
    // the way `/tmp` does on Unix. Keep the name stable.
    ".cigscript-keys".to_string()
}

/// The OS configuration directory for a user-level tool.
fn os_config_dir() -> PathBuf {
    let home = || {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
    };
    #[cfg(target_os = "macos")]
    {
        home()
            .join("Library")
            .join("Application Support")
            .join("CigScript")
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join("AppData").join("Roaming"))
            .join("CigScript")
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home().join(".config"))
    }
}

/// The key's old home, inside the tin, before it was moved out (1.8.0).
fn legacy_key_path() -> PathBuf {
    super::tin::dir().join("seal.key")
}

/// Where the key came from this time, for the plan and the notes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Keeper {
    Keyring,
    File,
    /// `--seal-key-fd N`: handed over on a descriptor and held in memory
    /// only, never written down.
    Descriptor,
    /// `seal_key = passphrase`: derived from a passphrase each time and
    /// stored nowhere (`passkeep`).
    Passphrase,
}

/// The key `--seal-key-fd` handed over, read once at start-up.
static GIVEN: Slot = Slot::new();

/// The most a key descriptor is read for: a key is 64 hex characters, and
/// a descriptor that streams on must not fill memory.
#[cfg(unix)]
const GIVEN_KEY_LIMIT: u64 = 4096;

/// `--seal-key-fd N`: read the tin's key from descriptor N (the 64 hex
/// characters a key file holds), then close it, so the key sits in no
/// file and no environment variable, and nothing cig starts inherits it.
/// Descriptor 0 is read and then pointed at /dev/null instead of closed.
/// Called once, before anything asks for the key.
#[cfg(unix)]
pub fn take_key_from_fd(fd: i32) -> Result<(), String> {
    use std::io::Read as _;
    use std::os::fd::FromRawFd as _;
    match fd {
        1 => return Err("descriptor 1 is cig's own output; open the key on 3 or above (3<key.hex), or on 0".to_string()),
        2 => return Err("descriptor 2 is cig's own error stream; open the key on 3 or above (3<key.hex), or on 0".to_string()),
        _ => {}
    }
    // SAFETY: F_GETFD only asks whether the descriptor is open.
    if fd < 0 || unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1 {
        return Err(format!(
            "nothing is open on descriptor {fd}; open the key there, for example {fd}<key.hex"
        ));
    }
    harden_process_memory();
    let mut text = Vec::with_capacity(128);
    let read = if fd == 0 {
        std::io::stdin()
            .lock()
            .take(GIVEN_KEY_LIMIT)
            .read_to_end(&mut text)
    } else {
        // SAFETY: the descriptor is open (asked above) and was handed to cig
        // for this alone; the File owns it from here and closes it on drop.
        let file = unsafe { fs::File::from_raw_fd(fd) };
        file.take(GIVEN_KEY_LIMIT).read_to_end(&mut text)
    };
    if fd == 0 {
        // A file redirected onto stdin could be rewound by a child and read
        // again: stdin becomes /dev/null.
        if let Ok(null) = fs::File::open("/dev/null") {
            use std::os::fd::AsRawFd as _;
            // SAFETY: dup2 onto 0 replaces stdin with an open descriptor.
            unsafe { libc::dup2(null.as_raw_fd(), 0) };
        }
    }
    if crate::config::Config::load().get("seal_key") == "passphrase" {
        // The descriptor holds the passphrase, not a key; passkeep owns it
        // from here (and wipes what it was copied from).
        let given = match read {
            Ok(_) => super::passkeep::give(text.clone()),
            Err(e) => Err(e.to_string()),
        };
        wipe_bytes(&mut text);
        return given.map_err(|e| format!("descriptor {fd} did not hold a passphrase ({e})"));
    }
    let decoded = match read {
        Ok(_) => std::str::from_utf8(&text)
            .map_err(|_| "it is not text".to_string())
            .and_then(|t| decode_key(t.trim())),
        Err(e) => Err(e.to_string()),
    };
    // The hex is a copy of the key: wiped before it is let go.
    wipe_bytes(&mut text);
    let key = decoded.map_err(|e| {
        format!(
            "descriptor {fd} did not hold a tin key, the 64 hex characters a key file holds ({e})"
        )
    })?;
    let _ = GIVEN.put(key, Keeper::Descriptor);
    Ok(())
}

/// Was the key handed over with `--seal-key-fd`? Its keeper holds it
/// elsewhere already, so no backup reminder is due.
pub fn key_was_given() -> bool {
    GIVEN.is_set()
}

/// `--seal-key-fd` needs Unix descriptors; on Windows the key file is the
/// headless way (`CIG_SEAL_KEY_FILE`).
#[cfg(not(unix))]
pub fn take_key_from_fd(_fd: i32) -> Result<(), String> {
    Err("--seal-key-fd works on Linux, macOS and the BSDs; on Windows name a key file with CIG_SEAL_KEY_FILE".to_string())
}

/// The key, found once per process: the OS keyring when the setting asks
/// for it and it answers, else the key file (made on first use). `Err` is
/// the reason none could be had, and nothing is sealed or unsealed then.
///
/// At the first call in this process the rekey lock is taken in shared
/// mode and kept for the lifetime of the process (SEC-004 H1): a
/// `cig tin rekey` cannot sign anything out from under a long-lived run,
/// and a run that starts while a rekey holds the exclusive lock waits
/// until it finishes (one notice on stderr) and reads the new key. A
/// rekey is its own lock-holder, so it opts out of the shared lock
/// before calling `key()` (see `rekey::enter_rekey`).
pub fn key() -> Result<(Key, Keeper), String> {
    static K: Slot = Slot::new();
    // Fail closed: a lock that cannot be had leaves every seal/sign call
    // refused with the reason, rather than proceeding unprotected under a
    // rekey a concurrent `cig tin rekey` is about to retire (SEC-004 N3).
    hold_shared_rekey_lock()?;
    K.get_or_init(key_uncached)
}

/// The same as `key()` but without taking the shared rekey lock. For the
/// rekey itself: it holds the exclusive lock and asks for the key under
/// it. Everyone else goes through `key()`.
pub(crate) fn key_no_rekey_lock() -> Result<(Key, Keeper), String> {
    static K: Slot = Slot::new();
    K.get_or_init(key_uncached)
}

thread_local! {
    /// Set on the rekey's own thread: `key()` skips the shared lock when it
    /// is set, so the rekey (which holds the exclusive lock) does not
    /// deadlock against itself.
    static REKEY_OWNS_LOCK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Mark this thread as the rekey's: subsequent `key()` calls skip the
/// shared lock. Called by `rekey::rekey` after it takes the exclusive
/// lock and cleared again at the end of that scope.
pub(crate) fn mark_rekey_thread(on: bool) {
    REKEY_OWNS_LOCK.with(|c| c.set(on));
}

/// Take the process-wide shared lock on the rekey lock file once, and keep
/// it for the rest of the process. A `cig tin rekey` cannot then take the
/// exclusive lock while this process lives, so nothing is signed or
/// sealed under a key the rekey is about to retire. A run started while
/// a rekey holds the exclusive lock blocks here (with one line on stderr
/// the first time) until the rekey finishes.
///
/// Fail closed (SEC-004 N3): when the lock cannot be had (the keys
/// directory cannot be made, the lock file cannot be opened, or `flock`
/// refuses for a reason other than contention), the first call caches
/// the error on `HELD` and every later call returns it too. Callers
/// (`seal::key` and the ledger/journal pre-write path) propagate the
/// error rather than carry on unprotected.
pub(crate) fn hold_shared_rekey_lock() -> Result<(), String> {
    if REKEY_OWNS_LOCK.with(|c| c.get()) {
        return Ok(());
    }
    static HELD: OnceLock<Result<fs::File, String>> = OnceLock::new();
    match HELD.get_or_init(super::rekey::take_process_shared_lock) {
        Ok(_) => {
            // StrictModes (#176): the lock has been taken, which already
            // verifies the keys folder is ours. Every file next to it
            // (seal.key, ledger.jsonl) must be ours too before a run
            // proceeds, so a planted file never ends up read or signed
            // against. These checks run once per process (OnceLock below
            // caches the outcome).
            static FILES: OnceLock<Result<(), String>> = OnceLock::new();
            FILES
                .get_or_init(check_keys_contents)
                .as_ref()
                .map_err(Clone::clone)?;
            Ok(())
        }
        Err(e) => Err(e.clone()),
    }
}

/// Check every file in the keys folder we read or append to: `seal.key`,
/// `ledger.jsonl`, and anything `CIG_SEAL_KEY_FILE` points at. A missing
/// file is not an error (cig will make it). Called once per process by
/// `hold_shared_rekey_lock`.
fn check_keys_contents() -> Result<(), String> {
    // The seal key is checked where it is read (`file_key`), so a leftover
    // file does not stop a run whose key comes from the keyring or a
    // descriptor.
    #[cfg(unix)]
    check_private_state_file(&super::ledger::path())?;
    Ok(())
}

/// Where the key lives for the life of the process (#105, L5): a page of
/// its own, mapped anonymously, locked out of swap where the platform allows
/// it, left out of core dumps, and marked `MADV_WIPEONFORK`, so a child
/// forked from cig (a boxed hop before its exec) sees zeros there, not the
/// key. Where the kernel lacks that (before 4.14) the page is marked
/// `MADV_DONTFORK` instead: the child has no such page at all. Elsewhere,
/// the key is held in ordinary memory as before. A read from a child that
/// finds zeros is an error, never a zero key.
pub struct Slot {
    cell: OnceLock<Result<Held, String>>,
}

enum Held {
    #[cfg(target_os = "linux")]
    Page(PagePtr, Keeper),
    Plain(Key, Keeper),
}

#[cfg(target_os = "linux")]
struct PagePtr(*mut u8);

// SAFETY: the page is written once, before the pointer is shared, and only
// read after.
#[cfg(target_os = "linux")]
unsafe impl Send for PagePtr {}
#[cfg(target_os = "linux")]
unsafe impl Sync for PagePtr {}

/// How the key page is kept from a fork: 0 none, 1 wiped, 2 not inherited.
#[cfg(target_os = "linux")]
static PROTECTION: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

#[cfg(target_os = "linux")]
const MADV_WIPEONFORK: libc::c_int = 18;

/// A fresh page for a secret, and how it is kept from a fork.
#[cfg(target_os = "linux")]
fn secret_page() -> Option<(*mut u8, usize, u8)> {
    // SAFETY: an anonymous private mapping of one page, then advice on it.
    unsafe {
        let len = (libc::sysconf(libc::_SC_PAGESIZE).max(4096)) as usize;
        let p = libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        );
        if p == libc::MAP_FAILED {
            return None;
        }
        let how = if libc::madvise(p, len, MADV_WIPEONFORK) == 0 {
            1
        } else if libc::madvise(p, len, libc::MADV_DONTFORK) == 0 {
            2
        } else {
            0
        };
        libc::madvise(p, len, libc::MADV_DONTDUMP);
        // Best effort: a small RLIMIT_MEMLOCK may say no.
        libc::mlock(p, len);
        Some((p.cast(), len, how))
    }
}

/// What this kernel does for a secret page across a fork, for doctor:
/// `wipe-on-fork`, `dont-fork` (before Linux 4.14) or `none`.
pub fn fork_protection() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        match secret_page() {
            Some((p, len, how)) => {
                // SAFETY: unmapping the probe page just made.
                unsafe { libc::munmap(p.cast(), len) };
                match how {
                    1 => "wipe-on-fork",
                    2 => "dont-fork",
                    _ => "none",
                }
            }
            None => "none",
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        "none"
    }
}

impl Held {
    fn store(key: Key, keeper: Keeper) -> Held {
        #[cfg(target_os = "linux")]
        if let Some((p, _len, how)) = secret_page() {
            PROTECTION.store(how, std::sync::atomic::Ordering::Relaxed);
            // SAFETY: the page is 4096 bytes or more and ours alone.
            unsafe { std::ptr::copy_nonoverlapping(key.as_bytes().as_ptr(), p, 32) };
            return Held::Page(PagePtr(p), keeper);
        }
        Held::Plain(key, keeper)
    }

    fn read(&self) -> Option<(Key, Keeper)> {
        match self {
            #[cfg(target_os = "linux")]
            Held::Page(p, keeper) => {
                let mut a = [0u8; 32];
                // SAFETY: 32 bytes of the live page.
                unsafe { std::ptr::copy_nonoverlapping(p.0, a.as_mut_ptr(), 32) };
                let zero = a.iter().all(|b| *b == 0);
                let k = Key::new(a);
                wipe_bytes(&mut a);
                if zero {
                    None
                } else {
                    Some((k, keeper.clone()))
                }
            }
            Held::Plain(k, keeper) => Some((k.clone(), keeper.clone())),
        }
    }
}

impl Slot {
    pub const fn new() -> Slot {
        Slot {
            cell: OnceLock::new(),
        }
    }

    /// Keep `key`, once; false when a key is already held.
    pub fn put(&self, key: Key, keeper: Keeper) -> bool {
        self.cell.set(Ok(Held::store(key, keeper))).is_ok()
    }

    pub fn is_set(&self) -> bool {
        matches!(self.cell.get(), Some(Ok(_)))
    }

    /// A copy of the key held, if there is one and it can be read here.
    pub fn get(&self) -> Option<Key> {
        match self.cell.get() {
            Some(Ok(h)) => h.read().map(|(k, _)| k),
            _ => None,
        }
    }

    /// The key held, made by `init` the first time.
    pub fn get_or_init(
        &self,
        init: impl FnOnce() -> Result<(Key, Keeper), String>,
    ) -> Result<(Key, Keeper), String> {
        let held = self
            .cell
            .get_or_init(|| init().map(|(k, keeper)| Held::store(k, keeper)));
        match held {
            Ok(h) => h.read().ok_or_else(|| {
                "the key is not held in this process (a forked child sees zeros there)".to_string()
            }),
            Err(e) => Err(e.clone()),
        }
    }
}

impl Default for Slot {
    fn default() -> Self {
        Slot::new()
    }
}

/// Text that held a key (its hex, a keyring's answer), wiped before it is
/// let go (#105, L5): the bytes are zeroed where they are, not left for the
/// allocator to hand out again.
fn wipe(s: String) {
    let mut b = s.into_bytes();
    wipe_bytes(&mut b);
}

fn key_uncached() -> Result<(Key, Keeper), String> {
    harden_process_memory();
    if let Some(k) = GIVEN.get() {
        return Ok((k, Keeper::Descriptor));
    }
    let cfg = crate::config::Config::load();
    if cfg.get("seal_key") == "passphrase" {
        return super::passkeep::key(sealed_in_use()).map(|k| (k, Keeper::Passphrase));
    }
    if cfg.get("seal_key") != "file" {
        match keyring_key() {
            Ok(k) => return Ok((k, Keeper::Keyring)),
            Err(e) => {
                let first = fs::metadata(key_path()).is_err();
                if first {
                    eprintln!(
                        "note: the OS keyring is not available ({e}); the tin's key is kept in {} instead, readable by you alone (cig config seal_key file makes that the rule)",
                        crate::crash::tilde(&key_path())
                    );
                }
            }
        }
    }
    file_key().map(|k| (k, Keeper::File))
}

const SERVICE: &str = "cigscript";
const USER: &str = "tin-seal";

/// The key from the OS keyring, made and stored on first use. Asked on a
/// thread with a deadline: a D-Bus activation that hangs must not hang
/// the run.
fn keyring_key() -> Result<Key, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let r = (|| -> Result<Key, String> {
            let entry = keyring::Entry::new(SERVICE, USER).map_err(|e| e.to_string())?;
            match entry.get_password() {
                Ok(hex_key) => {
                    let k = decode_key(hex_key.trim());
                    wipe(hex_key);
                    k
                }
                Err(keyring::Error::NoEntry) => {
                    if sealed_in_use() {
                        return Err(format!(
                            "the keyring has no {SERVICE}/{USER} entry, and this machine holds sealed objects only the key that sealed them opens; a new key would lock them out for good"
                        ));
                    }
                    let k = random_key()?;
                    let text = hex::encode(k.as_bytes());
                    let stored = entry.set_password(&text).map_err(|e| e.to_string());
                    wipe(text);
                    stored?;
                    Ok(k)
                }
                Err(e) => Err(e.to_string()),
            }
        })();
        let _ = tx.send(r);
    });
    match rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(r) => r,
        Err(_) => Err("the keyring did not answer within 5 seconds".to_string()),
    }
}

/// Does this machine hold anything only the current key opens: an object
/// under the tin's sealed directory, or a sealed snapshot in a run's record?
/// Asked before a new key is made where none was found: a key that went
/// missing is put back, never replaced behind the user's back.
fn sealed_in_use() -> bool {
    let any_object = fs::read_dir(super::tin::sealed_dir())
        .map(|buckets| {
            buckets.flatten().any(|b| {
                fs::read_dir(b.path())
                    .map(|mut objects| objects.next().is_some())
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    if any_object {
        return true;
    }
    fs::read_dir(super::runs::runs_dir())
        .map(|runs| {
            runs.flatten().any(|r| {
                fs::read_dir(r.path().join("snapshots"))
                    .map(|snaps| {
                        snaps
                            .flatten()
                            .any(|f| f.file_name().to_string_lossy().ends_with(SUFFIX))
                    })
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn file_key() -> Result<Key, String> {
    let path = key_path();
    // Keys live in the keys folder; even when CIG_SEAL_KEY_FILE names a
    // file elsewhere, that file must still be owned by us and private
    // (SEC-176). The folder check only fires for the default location.
    let explicit = std::env::var_os("CIG_SEAL_KEY_FILE").is_some();
    if !explicit {
        let _ = ensure_keys_dir()?;
    }
    migrate_legacy_key(&path);
    match open_key_file(&path)? {
        Some(mut f) => {
            let mut text = String::new();
            if let Err(e) = io::Read::read_to_string(&mut f, &mut text) {
                wipe(text);
                return Err(format!("cannot read the key file {}: {e}", path.display()));
            }
            let k = decode_key(text.trim());
            wipe(text);
            k
        }
        None => {
            if sealed_in_use() {
                return Err(format!(
                    "the tin's key is gone ({} is not there), and this machine holds sealed objects only that key opens; a new key would lock them out for good. Put the key back (the keyring entry {SERVICE}/{USER}, or that file), or move {} and the sealed snapshots under {} away to start over",
                    crate::crash::tilde(&path),
                    crate::crash::tilde(&super::tin::sealed_dir()),
                    crate::crash::tilde(&super::runs::runs_dir())
                ));
            }
            let k = random_key()?;
            if explicit {
                // `CIG_SEAL_KEY_FILE` outside the keys folder: its parent is
                // made 0700 when it is missing, and left as it is when it is
                // there (never a chmod of a folder cig did not make).
                if let Some(parent) = path.parent() {
                    make_private_dir_if_missing(parent).map_err(|e| e.to_string())?;
                }
            }
            let mut opts = fs::OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut f = opts
                .open(&path)
                .map_err(|e| format!("cannot create the key file {}: {e}", path.display()))?;
            f.write_all(hex::encode(k.as_bytes()).as_bytes())
                .and_then(|()| f.write_all(b"\n"))
                .and_then(|()| f.sync_all())
                .map_err(|e| e.to_string())?;
            Ok(k)
        }
    }
}

/// Make `dir` 0700 when it is missing; leave one that is there as it is.
fn make_private_dir_if_missing(dir: &Path) -> io::Result<()> {
    if dir.as_os_str().is_empty() || fs::symlink_metadata(dir).is_ok() {
        return Ok(());
    }
    let mut b = fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir)
}

/// Open the key file to read it, without following a link and without
/// waiting on a pipe, and check the file actually opened: ours, a regular
/// file with one name, closed to others. Checking the open descriptor,
/// not the path, leaves no gap between the look and the read for a file
/// swapped in between (#176). `Ok(None)`: there is no key file.
fn open_key_file(path: &Path) -> Result<Option<fs::File>, String> {
    let mut o = fs::OpenOptions::new();
    o.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let f = match o.open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            // ELOOP (EMLINK on FreeBSD): the path is a symbolic link.
            #[cfg(unix)]
            if matches!(e.raw_os_error(), Some(libc::ELOOP) | Some(libc::EMLINK)) {
                return Err(format!(
                    "E721 burn: the seal key {} is a symlink, so a read may be redirected. Put the real key file back in its place",
                    path.display()
                ));
            }
            return Err(format!("cannot read the key file {}: {e}", path.display()));
        }
    };
    #[cfg(unix)]
    {
        let md = f
            .metadata()
            .map_err(|e| format!("cannot read the key file {}: {e}", path.display()))?;
        check_key_meta(path, &md)?;
    }
    Ok(Some(f))
}

/// The seal key's own rules, on metadata from the file itself (an open
/// descriptor) or from a look at the path that does not follow a link.
#[cfg(unix)]
fn check_key_meta(path: &Path, md: &fs::Metadata) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    if !md.file_type().is_file() {
        return Err(format!(
            "E721 burn: the seal key {} is not a regular file. Put the real key file back in its place",
            path.display()
        ));
    }
    // SAFETY: geteuid() has no preconditions.
    let euid = unsafe { libc::geteuid() };
    if md.uid() != euid {
        return Err(format!(
            "E721 burn: the seal key {} is owned by {}, not you (uid {euid}). If it is yours (made under sudo), chown it back to yourself; if it is not, it is not your key: set CIG_KEYS_DIR / CIG_SEAL_KEY_FILE to a place only you can write",
            path.display(),
            fmt_owner(md.uid()),
        ));
    }
    if md.nlink() > 1 {
        return Err(format!(
            "E721 burn: the seal key {} has {} names (hard links), so it can be read or changed through another one. Give it a name of its own: copy it beside itself and move the copy over it",
            path.display(),
            md.nlink(),
        ));
    }
    if (md.mode() & 0o077) != 0 {
        return Err(format!(
            "E721 burn: the seal key {} has mode {:04o}: others could read it. chmod 600 it, then, if anyone else could have read it, cig tin rekey for a key nobody else has seen",
            path.display(),
            md.mode() & 0o7777,
        ));
    }
    Ok(())
}

/// Move a pre-1.8 key out of the tin into the keys directory, once, with a
/// note. The old file is removed only after the new one is in place.
fn migrate_legacy_key(new_path: &Path) {
    if new_path.exists() {
        return;
    }
    let legacy = legacy_key_path();
    if legacy == *new_path || fs::metadata(&legacy).is_err() {
        return;
    }
    let Ok(text) = fs::read_to_string(&legacy) else {
        return;
    };
    if let Some(parent) = new_path.parent() {
        // `ensure_keys_dir` already made the default parent 0700; a
        // `CIG_SEAL_KEY_FILE` parent is made 0700 only when missing.
        if make_private_dir_if_missing(parent).is_err() {
            return;
        }
    }
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    if let Ok(mut f) = opts.open(new_path) {
        if f.write_all(text.as_bytes())
            .and_then(|()| f.sync_all())
            .is_ok()
        {
            let _ = fs::remove_file(&legacy);
            eprintln!(
                "note: the tin's key moved out of the tin to {}, so a copy of the tin no longer takes it (cig tin key export backs it up)",
                crate::crash::tilde(new_path)
            );
        } else {
            let _ = fs::remove_file(new_path);
        }
    }
}

// ----- the keys directory and strict-mode checks (#176) -------------------------

/// Make the keys directory and return its path. On Unix the directory is
/// created `0700` from the start (not `create_dir_all` followed by a
/// `chmod`), so there is no window where it exists with wider
/// permissions. When the directory already exists, it is checked instead
/// of touched: the owner and mode must be safe (`check_keys_dir`), or
/// the call refuses with the `E721` message.
///
/// Every path that can create the folder (the key reader, the rekey lock,
/// the ledger) goes through this helper, so the lock cannot be the one
/// that creates it with the umask's mode (#176).
pub fn ensure_keys_dir() -> Result<PathBuf, String> {
    let dir = keys_dir();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        match fs::symlink_metadata(&dir) {
            Ok(m) if m.file_type().is_dir() => {
                check_keys_dir(&dir)?;
            }
            _ => {
                // Doesn't exist, or something else is in the way (a file,
                // a dangling symlink). Let `DirBuilder` report the OS
                // error in the "unprotected" wording that `cig run`
                // already catches as E802: this is a filesystem issue,
                // not a strict-mode one.
                let mut b = fs::DirBuilder::new();
                b.recursive(true).mode(0o700);
                b.create(&dir).map_err(|e| {
                    format!(
                        "could not create the keys directory {} ({e}); refusing to proceed unprotected against a concurrent cig tin rekey",
                        dir.display()
                    )
                })?;
                // A recursive create says yes to a folder that appeared
                // between the look above and this call, whoever made it:
                // check what is there now, as for one that was there
                // before.
                check_keys_dir(&dir)?;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let fresh = !dir.exists();
        fs::create_dir_all(&dir).map_err(|e| {
            format!(
                "could not create the keys directory {} ({e}); refusing to proceed unprotected against a concurrent cig tin rekey",
                dir.display()
            )
        })?;
        // Made now: protected from the start, the user and SYSTEM alone (#105, K2).
        #[cfg(windows)]
        if fresh {
            let _ = super::winacl::protect_dir(&dir);
        }
        #[cfg(not(windows))]
        let _ = fresh;
    }
    Ok(dir)
}

/// The keys folder, checked. Owned by this uid; not a symlink; not a
/// regular file; not group- or world-writable. Refuses with the `E721`
/// wording the operator reads if any of those fails.
#[cfg(unix)]
pub fn check_keys_dir(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let md = fs::symlink_metadata(path).map_err(|e| {
        format!(
            "E721 burn: cannot look at the keys folder {}: {e}; move it aside or set CIG_KEYS_DIR to a folder only you can write",
            path.display()
        )
    })?;
    if md.file_type().is_symlink() {
        return Err(format!(
            "E721 burn: the keys folder {} is a symlink; a read may be redirected. Move it aside or set CIG_KEYS_DIR to a folder only you can write",
            path.display()
        ));
    }
    if !md.file_type().is_dir() {
        return Err(format!(
            "E721 burn: the keys folder {} is not a directory. Move it aside or set CIG_KEYS_DIR to a folder only you can write",
            path.display()
        ));
    }
    // SAFETY: geteuid() has no preconditions.
    let euid = unsafe { libc::geteuid() };
    if md.uid() != euid {
        return Err(format!(
            "E721 burn: the keys folder {} is owned by {}, not you (uid {euid}); whoever owns it could plant the seal key or delete the ledger. If it is yours (made under sudo), chown it back to yourself; if it is not, move it aside, or set CIG_KEYS_DIR to a folder only you can write",
            path.display(),
            fmt_owner(md.uid()),
        ));
    }
    if (md.mode() & 0o022) != 0 {
        return Err(format!(
            "E721 burn: the keys folder {} has mode {:04o}: others can write to it, and could plant the seal key or delete the ledger. chmod 700 it, or set CIG_KEYS_DIR to a folder only you can write",
            path.display(),
            md.mode() & 0o7777,
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn check_keys_dir(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// The seal key, checked before it is read: owned by this uid, a regular
/// file (not a symlink: `symlink_metadata`), and not readable or writable
/// by group or world. Missing is not an error: the caller will create it.
/// A key others could read may have been read, so a key that is ours but
/// too open is refused, as OpenSSH refuses such a private key, with the
/// fix that keeps it (never "move it aside": a new key opens nothing the
/// old one sealed).
#[cfg(unix)]
pub fn check_private_key_file(path: &Path) -> Result<(), String> {
    let md = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(format!(
                "E721 burn: cannot look at the seal key {}: {e}",
                path.display()
            ))
        }
    };
    if md.file_type().is_symlink() {
        return Err(format!(
            "E721 burn: the seal key {} is a symlink, so a read may be redirected. Put the real key file back in its place",
            path.display()
        ));
    }
    check_key_meta(path, &md)
}

/// The ledger and the rekey lock, checked before they are opened: owned by
/// this uid and a regular file, or refused. Neither holds a secret, and
/// the keys folder (0700) keeps others out, so one that is ours but more
/// open than 0600 (a lock an early build made with the umask's mode) is
/// made 0600 and used.
#[cfg(unix)]
pub fn check_private_state_file(path: &Path) -> Result<(), String> {
    check_own_file(path)
}

#[cfg(unix)]
fn check_own_file(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let md = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(format!(
                "E721 burn: cannot look at {}: {e}; move it aside or set CIG_KEYS_DIR to a folder only you can write",
                path.display()
            ));
        }
    };
    if md.file_type().is_symlink() {
        return Err(format!(
            "E721 burn: {} is a symlink, so a read may be redirected. Remove the link and put the file back in place",
            path.display()
        ));
    }
    if !md.file_type().is_file() {
        return Err(format!(
            "E721 burn: {} is not a regular file. Move it aside and run again",
            path.display()
        ));
    }
    // SAFETY: geteuid() has no preconditions.
    let euid = unsafe { libc::geteuid() };
    if md.uid() != euid {
        return Err(format!(
            "E721 burn: {} is owned by {}, not you (uid {euid}): it may have been planted, and yours may be gone. If it is yours (made under sudo), chown it back to yourself; if it is not, the folder was not private: move it aside, then check the history with cig runs --verify --deep",
            path.display(),
            fmt_owner(md.uid()),
        ));
    }
    if (md.mode() & 0o077) != 0 {
        use std::os::unix::fs::PermissionsExt;
        return fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|e| {
            format!(
                "E721 burn: {} has mode {:04o} and cannot be made 0600 ({e}). chmod 600 it",
                path.display(),
                md.mode() & 0o7777,
            )
        });
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn check_private_key_file(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(not(unix))]
pub fn check_private_state_file(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// What a run would refuse about the ledger or the lock, found without
/// changing anything (doctor's look): another owner, a link, not a file.
/// A file that is ours and only too open is not a problem: the next run
/// makes it 0600.
#[cfg(unix)]
pub fn state_file_problem(path: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let md = fs::symlink_metadata(path).ok()?;
    // SAFETY: geteuid() has no preconditions.
    let euid = unsafe { libc::geteuid() };
    if md.file_type().is_symlink() || !md.file_type().is_file() || md.uid() != euid {
        // Each of these is refused before the tightening step, so this
        // changes nothing.
        return check_own_file(path).err();
    }
    None
}

#[cfg(not(unix))]
pub fn state_file_problem(_path: &Path) -> Option<String> {
    None
}

/// Who owns a file, by uid: no user-database lookup (NSS can load
/// modules, and `getpwuid` is not thread-safe) in a process that holds
/// the key. `getent passwd <uid>` names them.
#[cfg(unix)]
fn fmt_owner(uid: u32) -> String {
    format!("uid {uid}")
}

/// Stop a debugger or another process of the same user from reading the
/// key out of cig's memory, once a key has been loaded. Reset for every
/// program cig starts, so hops are unaffected. `CIG_ALLOW_DEBUG=1` skips
/// it, with a note on every run so it is not left on by accident.
pub fn harden_process_memory() {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| {
        if std::env::var_os("CIG_ALLOW_DEBUG").is_some() {
            eprintln!("note: CIG_ALLOW_DEBUG=1: a debugger can read cig's keys; turn it off when you are done (E715)");
        } else {
            #[cfg(target_os = "linux")]
            unsafe {
                // PR_SET_DUMPABLE 0: blocks same-user ptrace and /proc/<pid>/mem,
                // and turns off core dumps (cig has its own crash reports).
                libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
            }
            #[cfg(target_os = "macos")]
            unsafe {
                // PT_DENY_ATTACH (31): refuse a debugger and core dumps.
                libc::ptrace(31, 0, std::ptr::null_mut(), 0);
            }
            #[cfg(windows)]
            {
                // Another process of the same user may not open cig to read
                // or write its memory (#105, K2).
                let _ = super::winacl::protect_process();
            }
        }
    });
}

// ----- carrying the key between machines ----------------------------------------

const KEY_EXPORT_MAGIC: &[u8; 8] = b"CIGKEYX1";

/// Where the "key was exported" marker lives: beside the key, so a restored
/// backup carries the knowledge that the key is kept elsewhere too.
pub fn export_marker() -> PathBuf {
    keys_dir().join(".exported")
}

/// Record that the key has been exported at least once.
pub fn mark_exported() {
    // Through the one helper that makes the folder private and checks it:
    // a folder that fails the checks gets no marker.
    if ensure_keys_dir().is_ok() {
        let _ = fs::write(export_marker(), b"");
    }
}

/// Has the key ever been exported from this machine?
pub fn was_exported() -> bool {
    export_marker().exists()
}

/// Does the tin hold anything only the current key opens? (public wrapper)
pub fn has_sealed() -> bool {
    sealed_in_use()
}

/// Write the master key wrapped with a passphrase (Argon2id, then
/// XChaCha20-Poly1305), so a backup can carry it. The file is useless
/// without the passphrase.
pub fn export_key(dest: &Path, passphrase: &str) -> Result<(), String> {
    use argon2::Argon2;
    use chacha20poly1305::aead::Aead;
    let (master, _) = key()?;
    let mut salt = [0u8; 16];
    let mut nonce = [0u8; 24];
    getrandom::getrandom(&mut salt).map_err(|e| e.to_string())?;
    getrandom::getrandom(&mut nonce).map_err(|e| e.to_string())?;
    let mut wrap = [0u8; 32];
    Argon2::default()
        .hash_password_into(passphrase.as_bytes(), &salt, &mut wrap)
        .map_err(|e| format!("deriving the wrapping key failed: {e}"))?;
    let cipher = XChaCha20Poly1305::new((&wrap).into());
    let ct = cipher
        .encrypt((&nonce).into(), master.as_bytes().as_slice())
        .map_err(|_| "wrapping the key failed".to_string())?;
    // Wipe the derived wrapping key: Argon2 is costly, its output is secret.
    wipe_bytes(&mut wrap);
    let mut out = Vec::with_capacity(8 + 16 + 24 + ct.len());
    out.extend_from_slice(KEY_EXPORT_MAGIC);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(dest).map_err(|e| e.to_string())?;
    f.write_all(&out)
        .and_then(|()| f.sync_all())
        .map_err(|e| e.to_string())
}

/// Put a wrapped key back as the fallback key file. Refuses when the tin
/// already holds a different key and sealed objects that only it opens.
pub fn import_key(src: &Path, passphrase: &str) -> Result<(), String> {
    use argon2::Argon2;
    use chacha20poly1305::aead::Aead;
    let raw = fs::read(src).map_err(|e| e.to_string())?;
    if raw.len() < 8 + 16 + 24 + 16 || &raw[..8] != KEY_EXPORT_MAGIC {
        return Err(format!("{} is not a cig key export", src.display()));
    }
    let salt = &raw[8..24];
    let nonce = &raw[24..48];
    let ct = &raw[48..];
    let mut wrap = [0u8; 32];
    Argon2::default()
        .hash_password_into(passphrase.as_bytes(), salt, &mut wrap)
        .map_err(|e| format!("deriving the wrapping key failed: {e}"))?;
    let cipher = XChaCha20Poly1305::new((&wrap).into());
    let mut pt = cipher
        .decrypt(nonce.into(), ct)
        .map_err(|_| "wrong passphrase, or the export was changed".to_string())?;
    wipe_bytes(&mut wrap);
    let master: Key = Key::try_from(pt.as_slice())
        .map_err(|_| "the export does not hold a 32-byte key".to_string())?;
    // The plaintext buffer still holds the key bytes: wipe before letting go.
    wipe_bytes(&mut pt);
    let path = key_path();
    // A different key already in place, with sealed objects that only it
    // opens: importing over it would lock them out. Refused, with the way
    // round it.
    if let Ok(existing) = fs::read_to_string(&path) {
        if let Ok(current) = decode_key(existing.trim()) {
            if current != master && sealed_in_use() {
                return Err(format!(
                    "the tin already has a key ({}) that sealed objects depend on, and this export holds a different one; importing it would lock them out. Export the current key first (cig tin key export), move it aside, then import",
                    crate::crash::tilde(&path)
                ));
            }
        }
    }
    // The folder through the one private helper; a CIG_SEAL_KEY_FILE
    // parent made 0700 only when missing. The key is written without
    // following a link at its path, so it never lands where a link points.
    if std::env::var_os("CIG_SEAL_KEY_FILE").is_none() {
        ensure_keys_dir()?;
    } else if let Some(parent) = path.parent() {
        make_private_dir_if_missing(parent).map_err(|e| e.to_string())?;
    }
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut f = opts.open(&path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        // An existing file it opened: still ours, one name, closed to others.
        let md = f.metadata().map_err(|e| e.to_string())?;
        if let Err(e) = check_key_meta(&path, &md) {
            // The mode alone is fixable here: the import writes a key, so
            // the file is made 0600 before the key goes in.
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            // SAFETY: geteuid() has no preconditions.
            let euid = unsafe { libc::geteuid() };
            if md.uid() == euid && md.nlink() == 1 && md.file_type().is_file() {
                f.set_permissions(fs::Permissions::from_mode(0o600))
                    .map_err(|e| e.to_string())?;
            } else {
                return Err(e);
            }
        }
    }
    f.write_all(hex::encode(master.as_bytes()).as_bytes())
        .and_then(|()| f.write_all(b"\n"))
        .and_then(|()| f.sync_all())
        .map_err(|e| e.to_string())?;
    // Where the keyring is the keeper, the imported key goes there as well,
    // so the next run finds it there and never makes a fresh one instead.
    // Best-effort: the primary key file above is already in place, so a
    // keyring that is not around now is not a reason to refuse the import.
    let _ = store_in_keyring(&master);
    Ok(())
}

/// Put a key into the OS keyring and prove the keyring now carries it: the
/// value is written, then read back, and the two compared in constant time.
/// Runs on a thread with a deadline, so a hanging D-Bus activation cannot
/// hang the command. `Ok` only when the read-back equalled the value
/// written; any other outcome is `Err`, and the caller holds the stashes.
///
/// `seal_key = file` leaves the keyring alone and returns `Ok`.
pub fn store_in_keyring(k: &Key) -> Result<(), String> {
    if matches!(
        crate::config::Config::load().get("seal_key").as_str(),
        "file" | "passphrase"
    ) {
        return Ok(());
    }
    // The hex copy is let go after the compare: both the one sent to the
    // thread and the one the thread receives from the keyring are wiped.
    let want = k.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut hex_key = hex::encode(want.as_bytes());
        let outcome = (|| -> Result<(), String> {
            let entry = keyring::Entry::new(SERVICE, USER).map_err(|e| e.to_string())?;
            entry.set_password(&hex_key).map_err(|e| e.to_string())?;
            match entry.get_password() {
                Ok(got) => {
                    let parsed = decode_key(got.trim());
                    wipe(got);
                    let round_tripped = parsed?;
                    if round_tripped == want {
                        Ok(())
                    } else {
                        Err(
                            "the keyring accepted the write but gave back other bytes on the read-back"
                                .to_string(),
                        )
                    }
                }
                Err(e) => Err(format!("read-back from the keyring failed: {e}")),
            }
        })();
        // The hex Vec<u8> held the key for a moment: wipe before it is let go.
        // SAFETY: wipe_bytes takes a &mut [u8] from the String's bytes.
        // SAFETY: using the String buffer directly would need unsafe; the
        // simpler path is to build a Vec from the String, wipe, drop.
        super::seal::wipe_bytes(unsafe { hex_key.as_bytes_mut() });
        drop(hex_key);
        let _ = tx.send(outcome);
    });
    match rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(r) => r,
        Err(_) => Err("the keyring did not answer within 5 seconds".to_string()),
    }
}

/// Where the key is kept, without making one: the keyring when it holds an
/// entry, else the key file when it exists, else nowhere yet. For doctor,
/// which must not create a key as a side effect of looking.
pub fn peek_keeper() -> Option<Keeper> {
    if GIVEN.is_set() {
        return Some(Keeper::Descriptor);
    }
    if crate::config::Config::load().get("seal_key") == "passphrase" {
        return super::passkeep::record_path()
            .exists()
            .then_some(Keeper::Passphrase);
    }
    if crate::config::Config::load().get("seal_key") != "file" && keyring_has_entry() {
        return Some(Keeper::Keyring);
    }
    if key_path().exists() {
        return Some(Keeper::File);
    }
    None
}

fn keyring_has_entry() -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let ok = keyring::Entry::new(SERVICE, USER)
            .and_then(|e| e.get_password())
            .is_ok();
        let _ = tx.send(ok);
    });
    rx.recv_timeout(std::time::Duration::from_secs(5))
        .unwrap_or(false)
}

fn decode_key(text: &str) -> Result<Key, String> {
    let mut bytes = hex::decode(text).map_err(|_| "the stored key is not hex".to_string())?;
    let arr: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| "the stored key is not 32 bytes".to_string())?;
    // The intermediate hex-decoded Vec held the key for a moment: wipe it.
    wipe_bytes(&mut bytes);
    Ok(Key::new(arr))
}

/// A fresh 32-byte random master key, wipe-on-drop.
pub fn random_key() -> Result<Key, String> {
    let mut k = [0u8; 32];
    getrandom::getrandom(&mut k).map_err(|e| format!("no randomness: {e}"))?;
    let out = Key::new(k);
    // The local stack copy held the key for a moment: wipe it.
    wipe_bytes(&mut k);
    Ok(out)
}

/// A short, public fingerprint of a key, for messages and manifests.
pub fn fingerprint(key: &Key) -> String {
    hex::encode(Sha256::digest(key.as_bytes()))[..16].to_string()
}

/// A fresh random id for a sealed object: 16 bytes, hex.
pub fn random_id() -> io::Result<String> {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(hex::encode(b))
}

// ----- sealing and unsealing -----------------------------------------------------

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

fn key_or_refuse() -> io::Result<Key> {
    key().map(|(k, _)| k).map_err(|e| {
        io::Error::other(format!(
            "the key that seals the tin is not available ({e}); nothing was written"
        ))
    })
}

/// Seal `src` into `dest`: the magic, a random nonce, then the segments.
/// `aad` binds the file to its name. Returns the plaintext's SHA-256 and
/// size, which the journal records so the bytes are checked on the way
/// back whatever the ciphertext says.
///
/// `seal_inner` reaches `key()` first, which takes the rekey's shared
/// lock for the whole life of the process (SEC-004 H1): a `cig tin
/// rekey` cannot sign anything out from under a run that lives, and a
/// run started during a rekey waits on the key call and then seals
/// under the new key.
pub fn seal_file(src: &Path, dest: &Path, aad: &[u8]) -> io::Result<(String, u64)> {
    let result = seal_inner(src, dest, aad);
    if result.is_err() {
        let _ = fs::remove_file(dest);
    }
    result
}

fn seal_inner(src: &Path, dest: &Path, aad: &[u8]) -> io::Result<(String, u64)> {
    let key = derive(&key_or_refuse()?, LABEL_SEAL);
    let mut nonce = [0u8; NONCE];
    getrandom::getrandom(&mut nonce).map_err(|e| io::Error::other(e.to_string()))?;
    let cipher = XChaCha20Poly1305::new(key.as_bytes().into());
    let mut enc = EncryptorBE32::from_aead(cipher, (&nonce).into());
    let mut input = fs::File::open(src)?;
    let mut out = io::BufWriter::new(fs::File::create(dest)?);
    out.write_all(MAGIC)?;
    out.write_all(&nonce)?;
    let mut buf = vec![0u8; SEGMENT];
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    loop {
        let n = read_full(&mut input, &mut buf)?;
        hasher.update(&buf[..n]);
        total += n as u64;
        let payload = Payload {
            msg: &buf[..n],
            aad,
        };
        if n == SEGMENT {
            let ct = enc
                .encrypt_next(payload)
                .map_err(|_| io::Error::other("sealing failed"))?;
            out.write_all(&ct)?;
        } else {
            let ct = enc
                .encrypt_last(payload)
                .map_err(|_| io::Error::other("sealing failed"))?;
            out.write_all(&ct)?;
            break;
        }
    }
    out.flush()?;
    out.get_ref().sync_all()?;
    Ok((hex::encode(hasher.finalize()), total))
}

/// Unseal `src` into `dst`, refusing a byte flipped or a tail cut off (a
/// segment that does not authenticate), and, when `expect_sha256` is
/// given, bytes other than the ones the journal recorded.
pub fn unseal_to(
    src: &Path,
    aad: &[u8],
    expect_sha256: &str,
    dst: &mut dyn Write,
) -> io::Result<u64> {
    let master = key().map(|(k, _)| k).map_err(|e| {
        io::Error::other(format!(
            "the key that sealed it is not available ({e}); nothing was written"
        ))
    })?;
    // A rekey in progress keeps the old key (and the new key, before the
    // keeper is swapped) on disk, so a crash leaves every object readable
    // under one of them. Pick the right key by trial-decrypting one segment
    // before streaming the whole file into `dst` (`dst` cannot be rewound).
    // The stashes are honoured only when they prove the primary (H2).
    let secondaries = secondary_keys(&master);
    let picked = if secondaries.is_empty() || probe_key(src, aad, &master).is_ok() {
        master
    } else if let Some(k) = secondaries
        .into_iter()
        .find(|k| probe_key(src, aad, k).is_ok())
    {
        k
    } else {
        // Neither master nor any stash opens: fall through with master so
        // `unseal_to_with` reports the real tag failure to the caller.
        master
    };
    unseal_to_with(&picked, src, aad, expect_sha256, dst)
}

/// The fallback keys a rekey leaves on disk while it works: `seal.key.old`
/// (the key that sealed everything before the rekey) and `seal.key.new`
/// (the key the keeper will carry once the walk is through). Either is
/// present only while a rekey is in flight; after it finishes, both are
/// removed. Empty on a steady tin.
///
/// Honoured only when every one of the following holds (SEC-004 H2):
///
/// - the rekey marker `rekey.in-progress` is on disk, and
/// - each stash file is a regular file, mode 0600, owned by this user, and
/// - the keeper's primary key equals one of the stash keys, by a
///   constant-time compare. Every legitimate mid-rekey state satisfies
///   this (the primary is still `seal.key.old` before the keeper swap and
///   `seal.key.new` after it). A planted marker + planted stash made by
///   someone who does not know the primary key cannot satisfy it, so a
///   signed journal or sealed object produced under a planted key will
///   not verify or open.
///
/// Returns the stashes that are NOT equal to the primary, so a caller
/// never tries the primary key twice.
pub fn secondary_keys(primary: &Key) -> Vec<Key> {
    let dir = keys_dir();
    if !dir.join(super::rekey::REKEY_MARKER).exists() {
        return Vec::new();
    }
    // Read both stashes, honouring only private 0600 regular files.
    let read = |name: &str| -> Option<Key> {
        let p = dir.join(name);
        if !is_private_regular_file(&p) {
            return None;
        }
        key_in_file(&p)
    };
    let stashed_new = read("seal.key.new");
    let stashed_old = read("seal.key.old");
    // The primary MUST equal at least one of the stashes (constant-time).
    // This is what forgery cannot satisfy: a planter who does not know
    // the primary key cannot write a `seal.key.old`/`seal.key.new` that
    // equals it.
    let primary_matches = stashed_new.as_ref().map(|k| k == primary).unwrap_or(false)
        || stashed_old.as_ref().map(|k| k == primary).unwrap_or(false);
    if !primary_matches {
        return Vec::new();
    }
    let mut out = Vec::new();
    // Order: new first, since after a successful swap the active keeper
    // still carries old until the next process starts.
    for s in [stashed_new, stashed_old].into_iter().flatten() {
        if &s != primary {
            out.push(s);
        }
    }
    out
}

/// A key stash is trusted only when it is a regular file, mode 0600, owned
/// by this effective user. A world-readable or someone-else's file at the
/// path is ignored, not an error: the probe falls back on the primary key.
#[cfg(unix)]
fn is_private_regular_file(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(md) = fs::symlink_metadata(path) else {
        return false;
    };
    if !md.file_type().is_file() {
        return false;
    }
    if (md.mode() & 0o777) != 0o600 {
        return false;
    }
    // SAFETY: geteuid() has no preconditions and returns a uid.
    let euid = unsafe { libc::geteuid() };
    md.uid() == euid
}

#[cfg(not(unix))]
fn is_private_regular_file(path: &Path) -> bool {
    // Where POSIX mode and uid do not apply, rely on the keys-dir ACL
    // (0700-equivalent on Windows under %APPDATA%\CigScript\keys): a stash
    // in there is a user-owned file, and the directory's ACL kept others
    // out already.
    fs::metadata(path).is_ok_and(|m| m.is_file())
}

/// Does this key open the first segment of `src`? One cheap trial read,
/// so a streaming `unseal_to` can pick the right key before writing bytes
/// to a `dst` that cannot be rewound.
fn probe_key(src: &Path, aad: &[u8], master: &Key) -> io::Result<()> {
    let mut input = io::BufReader::new(fs::File::open(src)?);
    let mut head = [0u8; MAGIC.len() + NONCE];
    let n = read_full(&mut input, &mut head)?;
    if n != head.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a sealed object",
        ));
    }
    let magic = &head[..MAGIC.len()];
    let key = if magic == MAGIC {
        derive(master, LABEL_SEAL)
    } else if magic == MAGIC_V1 {
        master.clone()
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a sealed object",
        ));
    };
    let nonce: [u8; NONCE] = head[MAGIC.len()..].try_into().expect("nonce length");
    let cipher = XChaCha20Poly1305::new(key.as_bytes().into());
    let mut dec = DecryptorBE32::from_aead(cipher, (&nonce).into());
    let mut buf = vec![0u8; SEGMENT + TAG];
    let n = read_full(&mut input, &mut buf)?;
    let payload = Payload {
        msg: &buf[..n],
        aad,
    };
    if n == SEGMENT + TAG {
        dec.decrypt_next(payload)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "wrong key"))?;
    } else {
        if n < TAG {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "truncated"));
        }
        dec.decrypt_last(payload)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "wrong key"))?;
    }
    Ok(())
}

/// The key kept in a file (hex), when it reads as one. The text the file
/// held is wiped before being let go.
pub fn key_in_file(path: &Path) -> Option<Key> {
    let text = fs::read_to_string(path).ok()?;
    let decoded = decode_key(text.trim()).ok();
    wipe(text);
    decoded
}

/// Re-seal `src`, sealed under `from`, into `dest` under this machine's
/// key, bound to the same name. `dest` may be `src`. The plaintext stays
/// in memory the whole time; nothing lands beside the file in the clear,
/// so a hard crash cannot leave the secret on disk.
pub fn reseal(src: &Path, dest: &Path, aad: &[u8], from: &Key) -> io::Result<()> {
    let to = key_or_refuse()?;
    reseal_with(src, dest, aad, from, &to)
}

/// Shared body of `reseal` and `reseal_under`: decrypt into a Vec, seal
/// from that Vec into a sealed temp, verify by decrypting the new copy,
/// rename onto `dest`. The plaintext buffer wipes itself on drop, and
/// the Vec is pre-sized to the plaintext's rough length so a `push` or
/// `extend_from_slice` does not reallocate and leave the earlier copy
/// in unwiped memory (SEC-004 L3).
fn reseal_with(src: &Path, dest: &Path, aad: &[u8], from: &Key, to: &Key) -> io::Result<()> {
    // The sealed file is a magic + nonce + segments where each segment is
    // plaintext + tag; the plaintext never exceeds the sealed file's
    // size, which is a cheap stat.
    let budget = fs::metadata(src).map(|m| m.len() as usize).unwrap_or(0);
    let mut plaintext = ZeroingBuf::with_capacity(budget);
    unseal_to_with(from, src, aad, "", &mut plaintext)?;
    let sealed = dest.with_extension("reseal-sealed");
    let _ = fs::remove_file(&sealed);
    let result = (|| -> io::Result<()> {
        seal_bytes(plaintext.as_slice(), &sealed, aad, to)?;
        // Prove the sealed temp opens with the target key before the
        // rename overwrites `dest`: a cosmic-ray-flipped output byte is
        // caught here, not at the next `cig unburn`.
        opens_with(&sealed, aad, to)?;
        fs::rename(&sealed, dest)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&sealed);
    }
    result
}

/// A `Write`-shaped buffer that zeroes its bytes on drop, so a reseal
/// plaintext never sits cold in memory (#105, L5). Grows by reserving
/// up front so a later push does not reallocate: a reallocated Vec
/// leaves the old capacity in freed memory, where `wipe_bytes` on drop
/// never reaches it (SEC-004 L3).
struct ZeroingBuf(Vec<u8>);
impl ZeroingBuf {
    fn with_capacity(n: usize) -> Self {
        Self(Vec::with_capacity(n))
    }
    fn as_slice(&self) -> &[u8] {
        &self.0
    }
}
impl io::Write for ZeroingBuf {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // If we would have to grow the backing Vec, build a fresh one at
        // the needed size, copy the old bytes in, and wipe the OLD Vec
        // before dropping it. `Vec::reserve` on its own would hand the
        // old allocation back to the allocator with the plaintext in it,
        // where `wipe_bytes` on Drop never reaches it (SEC-004 L3).
        if self.0.capacity() < self.0.len() + buf.len() {
            let want = (self.0.len() + buf.len()).max(self.0.capacity().saturating_mul(2));
            let mut new = Vec::with_capacity(want);
            new.extend_from_slice(&self.0);
            wipe_bytes(&mut self.0);
            self.0 = new;
        }
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Drop for ZeroingBuf {
    fn drop(&mut self) {
        wipe_bytes(&mut self.0);
    }
}

/// Seal `bytes` (in memory) into `dest` under `to`, with `aad`. The on-disk
/// format matches `seal_file`.
fn seal_bytes(bytes: &[u8], dest: &Path, aad: &[u8], to: &Key) -> io::Result<()> {
    let seal_sub = derive(to, LABEL_SEAL);
    let mut nonce = [0u8; NONCE];
    getrandom::getrandom(&mut nonce).map_err(|e| io::Error::other(e.to_string()))?;
    let cipher = XChaCha20Poly1305::new(seal_sub.as_bytes().into());
    let mut enc = EncryptorBE32::from_aead(cipher, (&nonce).into());
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut out = io::BufWriter::new(opts.open(dest)?);
    out.write_all(MAGIC)?;
    out.write_all(&nonce)?;
    let mut i = 0;
    while i < bytes.len() || (bytes.is_empty() && i == 0) {
        let end = (i + SEGMENT).min(bytes.len());
        let msg = &bytes[i..end];
        let payload = Payload { msg, aad };
        if end == bytes.len() {
            let ct = enc
                .encrypt_last(payload)
                .map_err(|_| io::Error::other("sealing failed"))?;
            out.write_all(&ct)?;
            break;
        } else {
            let ct = enc
                .encrypt_next(payload)
                .map_err(|_| io::Error::other("sealing failed"))?;
            out.write_all(&ct)?;
        }
        i = end;
    }
    out.flush()?;
    out.get_ref().sync_all()?;
    Ok(())
}

/// `unseal_to` under the master key given: this machine's, or another's
/// whose objects are being taken in.
pub fn unseal_to_with(
    master: &Key,
    src: &Path,
    aad: &[u8],
    expect_sha256: &str,
    dst: &mut dyn Write,
) -> io::Result<u64> {
    let mut input = io::BufReader::new(fs::File::open(src)?);
    let mut head = [0u8; MAGIC.len() + NONCE];
    let n = read_full(&mut input, &mut head)?;
    let magic = &head[..MAGIC.len()];
    let key = if magic == MAGIC {
        derive(master, LABEL_SEAL)
    } else if magic == MAGIC_V1 {
        master.clone()
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is not a sealed object", src.display()),
        ));
    };
    if n != head.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is not a sealed object", src.display()),
        ));
    }
    let nonce: [u8; NONCE] = head[MAGIC.len()..].try_into().expect("nonce length");
    let cipher = XChaCha20Poly1305::new(key.as_bytes().into());
    let mut dec = DecryptorBE32::from_aead(cipher, (&nonce).into());
    let mut buf = vec![0u8; SEGMENT + TAG];
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let bad = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} does not open with the tin's key: a byte was changed, a tail cut off, or it was sealed with another key; refusing to restore from it",
                src.display()
            ),
        )
    };
    loop {
        let n = read_full(&mut input, &mut buf)?;
        let payload = Payload {
            msg: &buf[..n],
            aad,
        };
        if n == SEGMENT + TAG {
            let pt = dec.decrypt_next(payload).map_err(|_| bad())?;
            hasher.update(&pt);
            dst.write_all(&pt)?;
            total += pt.len() as u64;
        } else {
            if n < TAG {
                return Err(bad());
            }
            let pt = dec.decrypt_last(payload).map_err(|_| bad())?;
            hasher.update(&pt);
            dst.write_all(&pt)?;
            total += pt.len() as u64;
            break;
        }
    }
    let actual = hex::encode(hasher.finalize());
    if !expect_sha256.is_empty() && actual != expect_sha256 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} opens, but holds other bytes than the journal recorded (sha256 {} against {}); refusing to restore from it",
                src.display(),
                &actual[..12],
                &expect_sha256[..expect_sha256.len().min(12)]
            ),
        ));
    }
    Ok(total)
}

/// Unseal `src` into the file `dest`; nothing is left there on failure.
pub fn unseal_file(src: &Path, aad: &[u8], expect_sha256: &str, dest: &Path) -> io::Result<()> {
    let result = (|| -> io::Result<()> {
        let mut out = io::BufWriter::new(fs::File::create(dest)?);
        unseal_to(src, aad, expect_sha256, &mut out)?;
        out.flush()?;
        out.get_ref().sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(dest);
    }
    result
}

/// Does the sealed file open with the tin's key, whole? Nothing is written.
pub fn opens(src: &Path, aad: &[u8]) -> io::Result<u64> {
    unseal_to(src, aad, "", &mut io::sink())
}

/// Does the sealed file open with the given master key, whole? Nothing is
/// written. Used by `cig tin rekey` to tell new-sealed from old-sealed and
/// by the parcel import to confirm a carried key opens its sealed objects.
pub fn opens_with(src: &Path, aad: &[u8], master: &Key) -> io::Result<u64> {
    unseal_to_with(master, src, aad, "", &mut io::sink())
}

/// Re-seal `src` (sealed under `from`) into `dest` under `to`. `dest` may
/// equal `src`. The plaintext stays in memory the whole way through: no
/// `.reseal-plain` file lands on disk, so a hard crash during a reseal
/// cannot leave the secret in the clear. Used by the rekey walk, where
/// `to` is this tin's new key.
pub fn reseal_under(src: &Path, dest: &Path, aad: &[u8], from: &Key, to: &Key) -> io::Result<()> {
    reseal_with(src, dest, aad, from, to)
}

/// The name a sealed file is bound to: its own file name.
pub fn aad_of(path: &Path) -> Vec<u8> {
    path.file_name()
        .map(|n| n.to_string_lossy().as_bytes().to_vec())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rewritten_cigseal_is_read_again() {
        let _g = super::super::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("proj");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(".cigseal"), "a.txt\n").unwrap();
        assert!(should_seal_here(&dir.join("a.txt")));
        assert!(!should_seal_here(&dir.join("bb.txt")));
        fs::write(dir.join(".cigseal"), "bb.txt\n").unwrap();
        assert!(should_seal_here(&dir.join("bb.txt")));
        assert!(!should_seal_here(&dir.join("a.txt")));
        fs::remove_file(dir.join(".cigseal")).unwrap();
        assert!(!should_seal_here(&dir.join("bb.txt")));
    }

    fn with_key_file<T>(f: impl FnOnce(&Path) -> T) -> T {
        let _g = super::super::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let old = std::env::var_os("CIGSCRIPT_HOME");
        std::env::set_var("CIGSCRIPT_HOME", tmp.path().join("home"));
        std::env::set_var("CIG_SEAL_KEY", "file");
        let out = f(tmp.path());
        match old {
            Some(v) => std::env::set_var("CIGSCRIPT_HOME", v),
            None => std::env::remove_var("CIGSCRIPT_HOME"),
        }
        out
    }

    #[test]
    fn a_sealed_file_round_trips_and_a_flipped_byte_is_refused() {
        with_key_file(|root| {
            let plain = root.join("id_rsa");
            let data: Vec<u8> = (0..(3 * SEGMENT + 777)).map(|i| (i % 251) as u8).collect();
            fs::write(&plain, &data).unwrap();
            let sealed = root.join("obj");
            let (sha, n) = seal_file(&plain, &sealed, b"obj").unwrap();
            assert_eq!(n as usize, data.len());
            let bytes = fs::read(&sealed).unwrap();
            assert!(bytes.starts_with(MAGIC));
            // Not a byte of the plaintext in the clear.
            assert!(!bytes
                .windows(64)
                .any(|w| data.windows(64).next() == Some(w)));
            let back = root.join("back");
            unseal_file(&sealed, b"obj", &sha, &back).unwrap();
            assert_eq!(fs::read(&back).unwrap(), data);
            // A flipped byte, a cut tail, the wrong name: each refused, nothing left.
            let mut bad = bytes.clone();
            bad[MAGIC.len() + NONCE + 100] ^= 1;
            fs::write(&sealed, &bad).unwrap();
            assert!(unseal_file(&sealed, b"obj", &sha, &back).is_err());
            assert!(!back.exists());
            fs::write(&sealed, &bytes[..bytes.len() - 20]).unwrap();
            assert!(unseal_file(&sealed, b"obj", &sha, &back).is_err());
            fs::write(&sealed, &bytes).unwrap();
            assert!(unseal_file(&sealed, b"other", &sha, &back).is_err());
            assert!(opens(&sealed, b"obj").is_ok());
            // An empty file seals and opens too.
            fs::write(&plain, b"").unwrap();
            let (sha, n) = seal_file(&plain, &sealed, b"obj").unwrap();
            assert_eq!(n, 0);
            unseal_file(&sealed, b"obj", &sha, &back).unwrap();
            assert_eq!(fs::read(&back).unwrap(), b"");
            assert!(
                secret_name(Path::new("/x/.env.local")) && secret_name(Path::new("id_rsa.pub"))
            );
            assert!(secret_name(Path::new("deploy.pem")) && !secret_name(Path::new("main.rs")));
        });
    }

    /// `wipe_bytes` writes zeroes over its buffer through a volatile store,
    /// so the compiler cannot elide the write. A `Key` runs this on drop:
    /// the task's own rule bans reading freed memory through a raw pointer
    /// to prove that end of it, so this test exercises the wipe function
    /// directly instead, which is what `Drop` calls.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_forked_child_sees_zeros_where_the_key_is_kept() {
        let slot = Slot::new();
        let key = Key::new([7u8; 32]);
        assert!(slot.put(key, Keeper::File));
        if fork_protection() == "none" {
            eprintln!("no fork protection for a secret page here; skipped");
            return;
        }
        // SAFETY: the child reads the slot and exits at once, touching no
        // lock and no allocator.
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            let seen = slot.get().is_some();
            // SAFETY: leaving the child without running destructors.
            unsafe { libc::_exit(if seen { 1 } else { 0 }) };
        }
        assert!(pid > 0);
        let mut status = 0;
        // SAFETY: waiting for the child just made.
        unsafe { libc::waitpid(pid, &mut status, 0) };
        // Wiped (zeros, so no key) or not inherited at all (the read
        // faults): either way the child never got the key.
        let clean = (libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0)
            || (libc::WIFSIGNALED(status) && libc::WTERMSIG(status) == libc::SIGSEGV);
        assert!(clean, "the child read the key: status {status:#x}");
        // The parent still has it, and seals and unseals with it.
        let k = slot.get().expect("the parent's key");
        assert_eq!(k.as_bytes(), &[7u8; 32]);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("a.sealed");
        seal_bytes(b"secret text", &dest, b"aad", &k).unwrap();
        let mut out = Vec::new();
        unseal_to_with(&k, &dest, b"aad", "", &mut out).unwrap();
        assert_eq!(out, b"secret text");
    }

    #[test]
    fn wipe_bytes_zeroes_through_a_volatile_store() {
        let mut buf = [0xabu8; 32];
        wipe_bytes(&mut buf);
        assert_eq!(buf, [0u8; 32]);

        // The same on a Vec<u8>, which the key readers hand us (hex text,
        // the keyring's answer).
        let mut v = vec![0x5a; 128];
        wipe_bytes(&mut v);
        assert!(v.iter().all(|b| *b == 0));

        // And on a buffer not aligned to a convenient size.
        let mut odd = vec![0xffu8; 17];
        wipe_bytes(&mut odd);
        assert_eq!(odd, vec![0u8; 17]);
    }

    /// A `Key` is `Clone` but not `Copy`: cloning is an explicit act, and a
    /// move leaves no stack copy behind that `Drop` would miss. The
    /// compile-time assertion at the top of this module (the ambiguity
    /// trick) refuses a `Copy` impl on `Key`; this test confirms that
    /// `Clone` still works and behaves as it should.
    #[test]
    fn key_is_clone_and_the_clone_carries_the_same_bytes() {
        let a = Key::new([0x11u8; 32]);
        let b = a.clone();
        assert_eq!(a, b);
        assert_eq!(a.as_bytes(), &[0x11u8; 32]);
        // Each key has its own backing memory: dropping one does not
        // touch the other. (We can see the surviving one's bytes: wiping
        // happens on the dropped `a`, not `b`.)
        drop(a);
        assert_eq!(b.as_bytes(), &[0x11u8; 32]);
    }
}
