// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The vault's store: `<root>/u<uid>/<kind>/<id[..2]>/<id>`, one tree per
//! user. Directories 0700, objects 0400, owned by the account the vault runs
//! as. Nothing here removes an object except [`Store::gc_apply`], which the
//! protocol does not reach.

use crate::{Kind, MAX_OBJECT, MAX_RECIPE};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

/// Why the vault said no: a word for the wire and a sentence.
#[derive(Debug)]
pub struct Refusal {
    pub word: &'static str,
    pub msg: String,
}

impl Refusal {
    pub fn new(word: &'static str, msg: impl Into<String>) -> Self {
        Self {
            word,
            msg: msg.into(),
        }
    }
}

impl From<io::Error> for Refusal {
    fn from(e: io::Error) -> Self {
        Refusal::new("io", e.to_string())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Put {
    Stored,
    Present,
}

pub struct Store {
    root: PathBuf,
    quota: u64,
    used: Mutex<HashMap<u32, u64>>,
}

#[derive(Deserialize)]
struct RecipeDoc {
    format: u32,
    sha256: String,
    chunks: Vec<ChunkDoc>,
}

#[derive(Deserialize)]
struct ChunkDoc {
    sha256: String,
}

impl Store {
    /// Open the store at `root`, which must be a directory this process owns
    /// and nobody else can enter. `quota` is the most one user may keep.
    pub fn open(root: &Path, quota: u64) -> Result<Store, String> {
        let md = fs::symlink_metadata(root)
            .map_err(|e| format!("the vault's store {} cannot be read: {e}", root.display()))?;
        if !md.is_dir() {
            return Err(format!("{} is not a directory", root.display()));
        }
        // SAFETY: geteuid has no preconditions.
        if md.uid() != unsafe { libc::geteuid() } {
            return Err(format!(
                "{} is not owned by the account the vault runs as",
                root.display()
            ));
        }
        if md.mode() & 0o077 != 0 {
            return Err(format!(
                "{} is open to other accounts (mode {:o}); it must be 0700",
                root.display(),
                md.mode() & 0o777
            ));
        }
        Ok(Store {
            root: root.to_path_buf(),
            quota,
            used: Mutex::new(HashMap::new()),
        })
    }

    fn user_dir(&self, uid: u32) -> PathBuf {
        self.root.join(format!("u{uid}"))
    }

    fn path(&self, uid: u32, kind: Kind, id: &str) -> PathBuf {
        self.user_dir(uid)
            .join(kind.letter().to_string())
            .join(&id[..2])
            .join(id)
    }

    fn private_dir(dir: &Path) -> io::Result<()> {
        if !dir.is_dir() {
            let mut b = fs::DirBuilder::new();
            b.recursive(true);
            std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
            b.create(dir)?;
        }
        Ok(())
    }

    fn used_for(&self, uid: u32) -> u64 {
        let mut g = self.used.lock().unwrap_or_else(|p| p.into_inner());
        *g.entry(uid).or_insert_with(|| {
            Kind::ALL
                .iter()
                .flat_map(|k| self.walk(uid, *k))
                .map(|(_, len, _)| len)
                .sum()
        })
    }

    fn charge(&self, uid: u32, n: u64) {
        let mut g = self.used.lock().unwrap_or_else(|p| p.into_inner());
        let e = g.entry(uid).or_insert(0);
        *e = e.saturating_add(n);
    }

    /// Every stored object of `kind` for `uid`: id, length, mtime.
    fn walk(&self, uid: u32, kind: Kind) -> Vec<(String, u64, SystemTime)> {
        let base = self.user_dir(uid).join(kind.letter().to_string());
        let mut out = Vec::new();
        for bucket in fs::read_dir(&base).into_iter().flatten().flatten() {
            for f in fs::read_dir(bucket.path()).into_iter().flatten().flatten() {
                let name = f.file_name().to_string_lossy().to_string();
                if !kind.id_ok(&name) {
                    continue;
                }
                if let Ok(md) = f.metadata() {
                    out.push((
                        name,
                        md.len(),
                        md.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    ));
                }
            }
        }
        out.sort();
        out
    }

    pub fn list(&self, uid: u32, kind: Kind) -> Vec<(String, u64)> {
        self.walk(uid, kind)
            .into_iter()
            .map(|(id, len, _)| (id, len))
            .collect()
    }

    pub fn has(&self, uid: u32, kind: Kind, id: &str) -> bool {
        kind.id_ok(id) && fs::symlink_metadata(self.path(uid, kind, id)).is_ok()
    }

    /// Take the bytes from `src` (an open regular file the client passed),
    /// hash them here, and keep them under `id`. Never replaces an object.
    pub fn put(
        &self,
        uid: u32,
        kind: Kind,
        id: &str,
        len: u64,
        src: BorrowedFd<'_>,
    ) -> Result<Put, Refusal> {
        if !kind.id_ok(id) {
            return Err(Refusal::new(
                "bad-request",
                "that is not an id of this kind",
            ));
        }
        let src = regular_file(src, false)?;
        let size = src.metadata()?.len();
        if size != len {
            return Err(Refusal::new(
                "bad-fd",
                format!("the file holds {size} bytes, the request said {len}"),
            ));
        }
        let cap = if kind == Kind::Recipe {
            MAX_RECIPE
        } else {
            MAX_OBJECT
        };
        if len > cap {
            return Err(Refusal::new(
                "too-large",
                format!("{len} bytes is over the limit"),
            ));
        }
        let final_path = self.path(uid, kind, id);
        let exists = fs::symlink_metadata(&final_path).is_ok();
        if exists && kind != Kind::Sealed {
            return Ok(Put::Present);
        }
        if !exists && self.used_for(uid).saturating_add(len) > self.quota {
            return Err(Refusal::new(
                "quota",
                "this account's share of the vault is full",
            ));
        }
        let tmp_dir = self.user_dir(uid).join("tmp");
        Self::private_dir(&tmp_dir)?;
        let mut rnd = [0u8; 8];
        getrandom::getrandom(&mut rnd).map_err(|e| Refusal::new("io", e.to_string()))?;
        let tmp = tmp_dir.join(format!("t{}", hex::encode(rnd)));
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        let done = (|| -> Result<String, Refusal> {
            let mut hasher = Sha256::new();
            let mut head = Vec::new();
            let mut buf = vec![0u8; 1 << 20];
            let mut at = 0u64;
            while at < len {
                let want = buf.len().min((len - at) as usize);
                let n = src.read_at(&mut buf[..want], at)?;
                if n == 0 {
                    return Err(Refusal::new("bad-fd", "the file ended before its length"));
                }
                hasher.update(&buf[..n]);
                if head.len() < MAX_RECIPE as usize && kind == Kind::Recipe {
                    head.extend_from_slice(&buf[..n]);
                } else if head.len() < 8 {
                    head.extend_from_slice(&buf[..n.min(8)]);
                }
                out.write_all(&buf[..n])?;
                at += n as u64;
            }
            let sum = hex::encode(hasher.finalize());
            match kind {
                Kind::Object if sum != id => {
                    return Err(Refusal::new(
                        "hash-mismatch",
                        "the bytes do not hash to the id named",
                    ))
                }
                Kind::Sealed
                    if !(head.starts_with(b"CIGSEAL1") || head.starts_with(b"CIGSEAL2")) =>
                {
                    return Err(Refusal::new("bad-request", "that is not a sealed object"))
                }
                Kind::Recipe => self.check_recipe(uid, id, &head)?,
                _ => {}
            }
            out.sync_all()?;
            Ok(sum)
        })();
        let sum = match done {
            Ok(s) => s,
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                return Err(e);
            }
        };
        drop(out);
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o400));
        let bucket = final_path.parent().expect("an object has a bucket");
        Self::private_dir(bucket)?;
        if kind == Kind::Sealed {
            if exists {
                let same = fs::read_to_string(sum_path(&final_path)).is_ok_and(|s| s.trim() == sum);
                let _ = fs::remove_file(&tmp);
                return if same {
                    Ok(Put::Present)
                } else {
                    Err(Refusal::new(
                        "exists-different",
                        "a sealed object with that id is already kept, with other bytes",
                    ))
                };
            }
            // The sum first: a sealed object without its sum is never kept.
            write_ro(&sum_path(&final_path), sum.as_bytes())?;
        }
        match fs::hard_link(&tmp, &final_path) {
            Ok(()) => {
                let _ = fs::remove_file(&tmp);
                self.charge(uid, len);
                sync_dir(bucket);
                Ok(Put::Stored)
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(&tmp);
                Ok(Put::Present)
            }
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                Err(e.into())
            }
        }
    }

    fn check_recipe(&self, uid: u32, id: &str, bytes: &[u8]) -> Result<(), Refusal> {
        let doc: RecipeDoc = serde_json::from_slice(bytes)
            .map_err(|e| Refusal::new("bad-request", format!("not a recipe: {e}")))?;
        if doc.format > 1 || doc.sha256 != id {
            return Err(Refusal::new(
                "bad-request",
                "the recipe is for other bytes than its id says, or from a newer cig",
            ));
        }
        for c in &doc.chunks {
            if !Kind::Object.id_ok(&c.sha256) || !self.has(uid, Kind::Object, &c.sha256) {
                return Err(Refusal::new(
                    "missing-chunk",
                    format!(
                        "chunk {} is not in the vault yet; store chunks before the recipe",
                        c.sha256.get(..12).unwrap_or("?")
                    ),
                ));
            }
        }
        Ok(())
    }

    /// Write the object into `dest` (a file the client opened for writing),
    /// checking the hash as it goes. On a bad object `dest` is emptied.
    pub fn get(
        &self,
        uid: u32,
        kind: Kind,
        id: &str,
        dest: BorrowedFd<'_>,
    ) -> Result<u64, Refusal> {
        if !kind.id_ok(id) {
            return Err(Refusal::new(
                "bad-request",
                "that is not an id of this kind",
            ));
        }
        let dest = regular_file(dest, true)?;
        let src = fs::File::open(self.path(uid, kind, id))
            .map_err(|_| Refusal::new("missing", "the vault does not keep that object"))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        let mut at = 0u64;
        loop {
            let n = src.read_at(&mut buf, at)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            dest.write_all_at(&buf[..n], at)?;
            at += n as u64;
        }
        let sum = hex::encode(hasher.finalize());
        let good = match kind {
            Kind::Object => sum == id,
            Kind::Sealed => fs::read_to_string(sum_path(&self.path(uid, kind, id)))
                .is_ok_and(|s| s.trim() == sum),
            Kind::Recipe => true,
        };
        if !good {
            let _ = dest.set_len(0);
            return Err(Refusal::new(
                "corrupt",
                "the stored bytes no longer match their id",
            ));
        }
        let _ = dest.sync_all();
        Ok(at)
    }

    /// Rehash a stored object (a recipe: every chunk too).
    pub fn verify(&self, uid: u32, kind: Kind, id: &str) -> Result<u64, Refusal> {
        if !kind.id_ok(id) {
            return Err(Refusal::new(
                "bad-request",
                "that is not an id of this kind",
            ));
        }
        let p = self.path(uid, kind, id);
        let bytes = fs::read(&p)
            .map_err(|_| Refusal::new("missing", "the vault does not keep that object"))?;
        let sum = hex::encode(Sha256::digest(&bytes));
        match kind {
            Kind::Object if sum != id => Err(Refusal::new(
                "corrupt",
                "the stored bytes no longer match their id",
            )),
            Kind::Sealed if !fs::read_to_string(sum_path(&p)).is_ok_and(|s| s.trim() == sum) => {
                Err(Refusal::new(
                    "corrupt",
                    "the stored bytes changed since they were kept",
                ))
            }
            Kind::Recipe => {
                let doc: RecipeDoc = serde_json::from_slice(&bytes)
                    .map_err(|_| Refusal::new("corrupt", "the stored recipe cannot be read"))?;
                for c in &doc.chunks {
                    if !Kind::Object.id_ok(&c.sha256) {
                        return Err(Refusal::new(
                            "corrupt",
                            "the stored recipe names a bad chunk",
                        ));
                    }
                    self.verify(uid, Kind::Object, &c.sha256).map_err(|e| {
                        Refusal::new("corrupt", format!("a chunk of the recipe: {}", e.msg))
                    })?;
                }
                Ok(bytes.len() as u64)
            }
            _ => Ok(bytes.len() as u64),
        }
    }

    /// What no live id names and is older than `grace`. A live recipe keeps
    /// its chunks. Nothing is removed.
    pub fn gc_plan(
        &self,
        uid: u32,
        live: &HashSet<(Kind, String)>,
        grace: Duration,
    ) -> Vec<(Kind, String)> {
        let mut keep: HashSet<(Kind, String)> = live.clone();
        for (k, id) in live {
            if *k == Kind::Recipe {
                if let Ok(b) = fs::read(self.path(uid, Kind::Recipe, id)) {
                    if let Ok(doc) = serde_json::from_slice::<RecipeDoc>(&b) {
                        for c in doc.chunks {
                            keep.insert((Kind::Object, c.sha256));
                        }
                    }
                }
            }
        }
        let now = SystemTime::now();
        let mut out = Vec::new();
        for kind in Kind::ALL {
            for (id, _, mtime) in self.walk(uid, kind) {
                let young = now.duration_since(mtime).map(|a| a < grace).unwrap_or(true);
                if !young && !keep.contains(&(kind, id.clone())) {
                    out.push((kind, id));
                }
            }
        }
        out
    }

    /// Remove what `gc_plan` named. The administrator's act (`cig-vault gc
    /// --apply`, run as the vault's account); no request reaches it.
    pub fn gc_apply(&self, uid: u32, plan: &[(Kind, String)]) -> (usize, u64) {
        let (mut n, mut bytes) = (0, 0);
        for (kind, id) in plan {
            if !kind.id_ok(id) {
                continue;
            }
            let p = self.path(uid, *kind, id);
            let len = fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            if fs::remove_file(&p).is_ok() {
                n += 1;
                bytes += len;
                let _ = fs::remove_file(sum_path(&p));
            }
        }
        let mut g = self.used.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(u) = g.get_mut(&uid) {
            *u = u.saturating_sub(bytes);
        }
        (n, bytes)
    }

    /// The uids that have a tree here.
    pub fn users(&self) -> Vec<u32> {
        fs::read_dir(&self.root)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                e.file_name()
                    .to_string_lossy()
                    .strip_prefix('u')?
                    .parse()
                    .ok()
            })
            .collect()
    }

    /// Leftover temporary files from an interrupted store.
    pub fn sweep_tmp(&self) {
        for uid in self.users() {
            for f in fs::read_dir(self.user_dir(uid).join("tmp"))
                .into_iter()
                .flatten()
                .flatten()
            {
                let _ = fs::remove_file(f.path());
            }
        }
    }
}

fn sum_path(object: &Path) -> PathBuf {
    let mut s = object.as_os_str().to_os_string();
    s.push(".sum");
    PathBuf::from(s)
}

fn write_ro(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let _ = fs::remove_file(path);
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o400)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

fn sync_dir(dir: &Path) {
    if let Ok(f) = fs::File::open(dir) {
        let _ = f.sync_all();
    }
}

/// A regular file the client passed, checked and duplicated (so it is
/// closed with the copy). `for_write` wants it open for writing and not in
/// append mode, else for reading.
fn regular_file(fd: BorrowedFd<'_>, for_write: bool) -> Result<fs::File, Refusal> {
    let own = fd
        .try_clone_to_owned()
        .map_err(|e| Refusal::new("bad-fd", e.to_string()))?;
    let file = fs::File::from(own);
    let md = file
        .metadata()
        .map_err(|e| Refusal::new("bad-fd", e.to_string()))?;
    if !md.is_file() {
        return Err(Refusal::new(
            "bad-fd",
            "the descriptor is not a regular file",
        ));
    }
    // SAFETY: F_GETFL reads the descriptor's open flags.
    let fl = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if fl < 0 {
        return Err(Refusal::new("bad-fd", "the descriptor cannot be inspected"));
    }
    let acc = fl & libc::O_ACCMODE;
    if for_write {
        if acc == libc::O_RDONLY || fl & libc::O_APPEND != 0 {
            return Err(Refusal::new(
                "bad-fd",
                "the descriptor is not open for writing at an offset",
            ));
        }
    } else if acc == libc::O_WRONLY {
        return Err(Refusal::new(
            "bad-fd",
            "the descriptor is not open for reading",
        ));
    }
    Ok(file)
}
