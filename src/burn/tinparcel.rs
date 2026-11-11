// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The tin parcel (cig-script#141): the whole tin as one verifiable,
//! incremental backup, where `cig parcel` carries a single run.
//!
//! A parcel is a folder (or the tree of an orphan git commit under
//! `refs/cig/backup/<name>`) holding
//!
//! - `runs/<id>/...`: every live run's record and journal (and any files
//!   the run keeps beside them),
//! - `tin/objects`, `tin/recipes`, `tin/sealed`, `tin/keeps`: the objects
//!   those runs' before-states need, byte for byte as the tin keeps them
//!   (a sealed object stays sealed: its ciphertext is copied, never
//!   opened, so a pushed ref never shows a secret in the clear),
//! - `ledger.jsonl`: the ledger, with its head named in the manifest,
//! - `manifest.json`: the SHA-256 of every file the parcel carries, and
//!   the parent parcel it builds on.
//!
//! With `--since <parent>` a directory parcel carries only the objects its
//! parent does not hold; the rest are named in the manifest as inherited
//! and found in the parent chain when it is restored. A git parcel is
//! always a whole tree (git stores an unchanged blob once, so a second
//! backup costs the new blobs only).
//!
//! `unparcel` checks every hash, every journal's chain, the ledger's
//! chain and that every object a journal needs is there before it writes
//! a byte, and it never overwrites a live run that differs from the one in
//! the parcel.

use super::journal::{sha256_file, Before, Journal};
use super::runs::{self, RunRecord};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

/// The shape of a manifest this cig writes.
pub const FORMAT: u32 = 1;
pub const MANIFEST: &str = "manifest.json";
pub const LEDGER: &str = "ledger.jsonl";
/// Where git parcels live: outside every branch.
pub const REF_PREFIX: &str = "refs/cig/backup/";

/// The ways a parcel command stops.
#[derive(Debug)]
pub enum Error {
    /// A command line that cannot be carried out (E803).
    Usage(String),
    /// The parcel is tampered with or incomplete (E722): one line each.
    Unverified(Vec<String>),
    /// A different live run, or a different ledger, is in the way (E723).
    Conflict(Vec<String>),
    /// The disk or git failed (E703).
    Io(String),
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e.to_string())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Origin {
    pub state_dir: PathBuf,
    #[serde(default)]
    pub host: String,
}

/// The parcel this one builds on.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Parent {
    /// Where it was: a folder, or a git ref.
    pub location: String,
    /// The SHA-256 of its `manifest.json`.
    pub manifest_sha256: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LedgerNote {
    pub lines: u64,
    /// The MAC of the ledger's last line: what `cig tin head` prints.
    pub head: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub cig: String,
    pub created: String,
    pub origin: Origin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<Parent>,
    /// The runs in the parcel, by id.
    pub runs: Vec<String>,
    /// Every file this parcel carries, relative to the state directory
    /// (`ledger.jsonl` aside), with its SHA-256.
    pub files: BTreeMap<String, String>,
    /// Objects the parent chain holds, with their SHA-256.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inherited: BTreeMap<String, String>,
    pub ledger: LedgerNote,
    /// How many sealed objects the parcel carries, still sealed.
    pub sealed: usize,
    /// A fingerprint of the key that sealed them, when any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// What `cig tin parcel` wrote.
#[derive(Debug, Serialize)]
pub struct Written {
    /// The folder, or the ref.
    pub target: String,
    /// The commit, for a git parcel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    pub runs: usize,
    /// Objects (plain, chunked and sealed) written into this parcel.
    pub objects_carried: usize,
    /// Objects the parent already holds.
    pub objects_inherited: usize,
    pub files: usize,
    pub bytes: u64,
    pub parent: Option<String>,
    pub manifest_sha256: String,
    pub ledger_head: String,
}

/// What `cig tin unparcel` put back.
#[derive(Debug, Serialize)]
pub struct Restored {
    pub source: String,
    pub runs_restored: usize,
    /// Runs already here, identical.
    pub runs_present: usize,
    pub objects_written: usize,
    pub objects_present: usize,
    pub bytes: u64,
    /// `installed`, `extended` (the parcel's carries more lines), `kept`
    /// (the one here carries more) or `unchanged`.
    pub ledger: &'static str,
    pub ledger_head: String,
}

pub enum Target {
    Dir(PathBuf),
    Git { repo: PathBuf, refname: String },
}

pub enum Source {
    Dir(PathBuf),
    Git { repo: PathBuf, refname: String },
}

// ---------------------------------------------------------------- names

/// A git ref a parcel may live under: always below `refs/cig/backup/`, so
/// no branch, tag or remote ref is ever touched. A bare name is put under
/// the prefix.
pub fn normalize_ref(raw: &str) -> Result<String, Error> {
    let full = if raw.starts_with("refs/") {
        raw.to_string()
    } else {
        format!("{REF_PREFIX}{raw}")
    };
    let rest = full.strip_prefix(REF_PREFIX).unwrap_or("");
    let ok = !rest.is_empty()
        && !rest.starts_with('/')
        && !rest.ends_with('/')
        && !rest.ends_with(".lock")
        && !rest.contains("..")
        && !rest.contains("//")
        && !rest.contains("@{")
        && !rest.split('/').any(|c| c.starts_with('.') || c.is_empty())
        && rest
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/' | b'T'));
    if ok {
        Ok(full)
    } else {
        Err(Error::Usage(format!(
            "`{raw}` is not a name a parcel may have: it must sit under {REF_PREFIX} and use letters, digits, dots, dashes, underscores and slashes (no branch or tag is ever written)"
        )))
    }
}

/// The name a git parcel takes when none is asked for: today's date.
pub fn auto_ref() -> String {
    format!("{REF_PREFIX}{}", chrono::Utc::now().format("%Y-%m-%d"))
}

/// A path inside a parcel: `runs/...`, `tin/...` or the ledger, never
/// absolute, never leaving the folder.
fn safe_rel(rel: &str) -> bool {
    if rel == LEDGER {
        return true;
    }
    if rel.is_empty() || rel.contains('\\') || rel.contains(':') {
        return false;
    }
    let p = Path::new(rel);
    let mut parts = 0;
    for (i, c) in p.components().enumerate() {
        match c {
            Component::Normal(n) => {
                if i == 0 && n != "runs" && n != "tin" {
                    return false;
                }
                parts += 1;
            }
            _ => return false,
        }
    }
    parts >= 2
}

fn is_tin_object(rel: &str) -> bool {
    rel.starts_with("tin/objects/")
        || rel.starts_with("tin/recipes/")
        || rel.starts_with("tin/sealed/")
}

fn rel_of(base: &Path, p: &Path) -> Option<String> {
    let r = p.strip_prefix(base).ok()?;
    let mut parts = Vec::new();
    for c in r.components() {
        match c {
            Component::Normal(n) => parts.push(n.to_str()?.to_string()),
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_default()
}

fn sha_bytes(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

// ---------------------------------------------------------------- writing

struct Item {
    rel: String,
    src: PathBuf,
    sha256: String,
    bytes: u64,
}

fn home() -> PathBuf {
    runs::home_dir()
}

/// Read a parent parcel's manifest (a folder, or a ref in `repo`).
fn read_parent(since: &str, repo: Option<&Path>) -> Result<(Manifest, Parent), Error> {
    let as_dir = Path::new(since);
    let (bytes, location) = if as_dir.join(MANIFEST).is_file() {
        let loc = fs::canonicalize(as_dir).unwrap_or_else(|_| as_dir.to_path_buf());
        (
            fs::read(as_dir.join(MANIFEST))?,
            loc.to_string_lossy().to_string(),
        )
    } else if let Some(repo) = repo {
        let r = normalize_ref(since)?;
        let out = git(repo, &["show", &format!("{r}:{MANIFEST}")], None)
            .map_err(|e| Error::Usage(format!("--since {since}: no parcel there ({e})")))?;
        (out, r)
    } else {
        return Err(Error::Usage(format!(
            "--since {since}: no manifest.json in it; give a parcel folder (or a ref with --git)"
        )));
    };
    let m: Manifest = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Usage(format!("--since {since}: its manifest does not read: {e}")))?;
    if m.format > FORMAT {
        return Err(Error::Usage(format!(
            "--since {since}: a parcel format {} written by a newer cig; this one reads up to {FORMAT}",
            m.format
        )));
    }
    let p = Parent {
        location,
        manifest_sha256: sha_bytes(&bytes),
    };
    Ok((m, p))
}

fn collect_run_files(run_dir: &Path, id: &str, out: &mut Vec<(String, PathBuf)>) {
    for e in walkdir::WalkDir::new(run_dir)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !(e.depth() == 1 && e.file_name() == "box"))
        .flatten()
    {
        if !e.file_type().is_file() {
            continue;
        }
        if let Some(rest) = rel_of(run_dir, e.path()) {
            out.push((format!("runs/{id}/{rest}"), e.path().to_path_buf()));
        }
    }
}

/// Every file the parcel will carry, as (relative name, source), and the
/// run ids.
type Gathered = (Vec<(String, PathBuf)>, Vec<String>);

fn gather() -> Result<Gathered, Error> {
    let tin_dir = super::tin::dir();
    let mut files: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut ids = Vec::new();
    for rec in runs::list()? {
        if !safe_rel(&format!("runs/{}/x", rec.id)) {
            continue;
        }
        let mut v = Vec::new();
        collect_run_files(&rec.dir(), &rec.id, &mut v);
        files.extend(v);
        for obj in super::tin::referenced_by(&rec) {
            if let Some(r) = rel_of(&tin_dir, &obj) {
                if obj.is_file() {
                    files.insert(format!("tin/{r}"), obj);
                }
            }
        }
        ids.push(rec.id.clone());
    }
    // What other tools keep here: the records, and the sealed copies they name.
    let keeps = super::tin::keeps_dir();
    for e in walkdir::WalkDir::new(&keeps)
        .follow_links(false)
        .into_iter()
        .flatten()
    {
        if e.file_type().is_file() {
            if let Some(r) = rel_of(&tin_dir, e.path()) {
                files.insert(format!("tin/{r}"), e.path().to_path_buf());
            }
        }
    }
    for k in super::tin::kept_copies() {
        if let Some(r) = rel_of(&tin_dir, &k.object) {
            if k.object.is_file() {
                files.insert(format!("tin/{r}"), k.object.clone());
            }
        }
    }
    ids.sort();
    ids.dedup();
    Ok((files.into_iter().collect(), ids))
}

fn ledger_note(text: &str) -> LedgerNote {
    let mut lines = 0;
    let mut head = String::new();
    for raw in text.lines().filter(|l| !l.trim().is_empty()) {
        lines += 1;
        if let Ok(l) = serde_json::from_str::<super::ledger::Line>(raw) {
            head = l.mac;
        }
    }
    LedgerNote { lines, head }
}

fn read_ledger() -> Result<Option<Vec<u8>>, Error> {
    match fs::read(super::ledger::path()) {
        Ok(b) if !b.is_empty() => Ok(Some(b)),
        Ok(_) => Ok(None),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Io(format!("cannot read the ledger: {e}"))),
    }
}

/// Write the whole tin as a parcel.
pub fn write(target: &Target, since: Option<&str>) -> Result<Written, Error> {
    let (mut files, ids) = gather()?;
    let repo = match target {
        Target::Git { repo, .. } => Some(repo.as_path()),
        Target::Dir(_) => None,
    };
    let parent = match since {
        Some(s) => Some(read_parent(s, repo)?),
        None => None,
    };
    if let (Target::Dir(_), Some((_, p))) = (target, &parent) {
        if !Path::new(&p.location).is_dir() {
            return Err(Error::Usage(
                "a folder parcel can only build on a folder parcel; give --since a folder"
                    .to_string(),
            ));
        }
    }
    // What the parent holds, by name.
    let held: BTreeMap<String, String> = parent
        .as_ref()
        .map(|(m, _)| {
            m.files
                .iter()
                .chain(m.inherited.iter())
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default();

    let mut items: Vec<Item> = Vec::new();
    let mut inherited: BTreeMap<String, String> = BTreeMap::new();
    let mut carried_objects = 0usize;
    let mut inherited_objects = 0usize;
    let mut sealed = 0usize;
    let dir_target = matches!(target, Target::Dir(_));
    files.sort();
    for (rel, src) in files {
        let object = is_tin_object(&rel);
        let sealed_obj = rel.starts_with("tin/sealed/");
        if sealed_obj {
            sealed += 1;
        }
        // A plain object is named by its hash: a parent holding that name
        // holds those bytes, so it is not read again. A sealed object's
        // name is random and a rekey rewrites it in place, so it is
        // always read and compared.
        let mut sha = String::new();
        let mut known = false;
        if object && !sealed_obj {
            if let Some(h) = held.get(&rel) {
                sha = h.clone();
                known = true;
            }
        }
        if !known {
            sha = sha256_file(&src).map_err(|e| Error::Io(format!("{}: {e}", src.display())))?;
        }
        let in_parent = object && held.get(&rel) == Some(&sha);
        if object && in_parent {
            inherited_objects += 1;
            if dir_target {
                inherited.insert(rel, sha);
                continue;
            }
        } else if object {
            carried_objects += 1;
        }
        let bytes = fs::metadata(&src)?.len();
        items.push(Item {
            rel,
            src,
            sha256: sha,
            bytes,
        });
    }
    let ledger = read_ledger()?;
    let note = ledger
        .as_deref()
        .map(|b| ledger_note(&String::from_utf8_lossy(b)))
        .unwrap_or_default();
    let key = if sealed > 0 {
        super::seal::key()
            .ok()
            .map(|(k, _)| super::seal::fingerprint(&k))
    } else {
        None
    };
    let mut manifest = Manifest {
        format: FORMAT,
        cig: env!("CARGO_PKG_VERSION").to_string(),
        created: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        origin: Origin {
            state_dir: home(),
            host: hostname(),
        },
        parent: parent.as_ref().map(|(_, p)| p.clone()),
        runs: ids.clone(),
        files: items
            .iter()
            .map(|i| (i.rel.clone(), i.sha256.clone()))
            .collect(),
        inherited,
        ledger: note.clone(),
        sealed,
        key,
    };
    if let Some(b) = &ledger {
        manifest.files.insert(LEDGER.to_string(), sha_bytes(b));
    }
    let mut total_bytes: u64 = items.iter().map(|i| i.bytes).sum();
    if let Some(b) = &ledger {
        total_bytes += b.len() as u64;
    }
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| Error::Io(format!("cannot write the manifest: {e}")))?;
    let manifest_sha = sha_bytes(&manifest_bytes);
    let nfiles = manifest.files.len() + 1;

    let (target_name, commit) = match target {
        Target::Dir(dest) => {
            emit_dir(dest, &items, ledger.as_deref(), &manifest_bytes)?;
            (dest.display().to_string(), None)
        }
        Target::Git { repo, refname } => {
            let c = emit_git(repo, refname, &items, ledger.as_deref(), &manifest_bytes)?;
            (refname.clone(), Some(c))
        }
    };
    Ok(Written {
        target: target_name,
        commit,
        runs: ids.len(),
        objects_carried: carried_objects,
        objects_inherited: inherited_objects,
        files: nfiles,
        bytes: total_bytes,
        parent: manifest.parent.map(|p| p.location),
        manifest_sha256: manifest_sha,
        ledger_head: note.head,
    })
}

fn copy_into(src: &Path, dst: &Path) -> io::Result<()> {
    if let Some(p) = dst.parent() {
        fs::create_dir_all(p)?;
    }
    let mut from = fs::File::open(src)?;
    let mut to = fs::File::create(dst)?;
    io::copy(&mut from, &mut to)?;
    to.sync_all()
}

fn emit_dir(
    dest: &Path,
    items: &[Item],
    ledger: Option<&[u8]>,
    manifest: &[u8],
) -> Result<(), Error> {
    if dest.exists() {
        let empty = dest.is_dir() && fs::read_dir(dest)?.next().is_none();
        if !empty {
            return Err(Error::Usage(format!(
                "{} already exists and is not an empty folder; a parcel is written into a new one (never merged into another)",
                dest.display()
            )));
        }
    }
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "parcel".to_string());
    let stage = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .join(format!(".{name}.partial"));
    let _ = fs::remove_dir_all(&stage);
    fs::create_dir_all(&stage)?;
    let built = (|| -> Result<(), Error> {
        for i in items {
            copy_into(&i.src, &stage.join(&i.rel))?;
        }
        if let Some(b) = ledger {
            fs::write(stage.join(LEDGER), b)?;
        }
        fs::write(stage.join(MANIFEST), manifest)?;
        // Verified on the way out: what was written is what the manifest says.
        verify_dir(&stage, None).map(|_| ())
    })();
    if let Err(e) = built {
        let _ = fs::remove_dir_all(&stage);
        return Err(e);
    }
    if dest.exists() {
        fs::remove_dir(dest)?;
    }
    fs::rename(&stage, dest).map_err(|e| {
        let _ = fs::remove_dir_all(&stage);
        Error::Io(format!(
            "cannot put the parcel in place at {}: {e}",
            dest.display()
        ))
    })
}

// ---------------------------------------------------------------- git

fn git(repo: &Path, args: &[&str], stdin: Option<&[u8]>) -> Result<Vec<u8>, String> {
    git_env(repo, args, stdin, &[])
}

fn git_env(
    repo: &Path,
    args: &[&str],
    stdin: Option<&[u8]>,
    env: &[(&str, &str)],
) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo).args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("git is not available: {e}"))?;
    if let Some(data) = stdin {
        let mut si = child.stdin.take().ok_or("git took no input")?;
        let data = data.to_vec();
        let t = std::thread::spawn(move || {
            let _ = si.write_all(&data);
        });
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        let _ = t.join();
        return finish_git(args, out);
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    finish_git(args, out)
}

fn finish_git(args: &[&str], out: std::process::Output) -> Result<Vec<u8>, String> {
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

fn text(b: Vec<u8>) -> String {
    String::from_utf8_lossy(&b).trim().to_string()
}

/// Write the parcel as an orphan commit under `refname`, without a work
/// tree, an index of the repository's or a branch: blobs go in with
/// `hash-object -w` straight from the tin's own files (no filter runs), the
/// tree is built in a private index file, and the ref is created only when
/// it does not exist.
fn emit_git(
    repo: &Path,
    refname: &str,
    items: &[Item],
    ledger: Option<&[u8]>,
    manifest: &[u8],
) -> Result<String, Error> {
    let gd = text(
        git(repo, &["rev-parse", "--absolute-git-dir"], None).map_err(|_| {
            Error::Usage(format!(
                "{} is not inside a git repository (--git writes there)",
                repo.display()
            ))
        })?,
    );
    if git(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{refname}^{{commit}}"),
        ],
        None,
    )
    .is_ok()
    {
        return Err(Error::Usage(format!(
            "{refname} already exists; a backup ref is never moved (choose another name)"
        )));
    }
    let mut paths = String::new();
    let mut names: Vec<&str> = Vec::new();
    for i in items {
        let s = i.src.to_string_lossy();
        if s.contains('\n') {
            return Err(Error::Io(format!(
                "{s} has a newline in its name; git cannot be asked about it"
            )));
        }
        paths.push_str(&s);
        paths.push('\n');
        names.push(&i.rel);
    }
    let mut entries: Vec<(String, String)> = Vec::new();
    if !items.is_empty() {
        let out = text(
            git(
                repo,
                &["hash-object", "-w", "--no-filters", "--stdin-paths"],
                Some(paths.as_bytes()),
            )
            .map_err(Error::Io)?,
        );
        let ids: Vec<&str> = out.lines().collect();
        if ids.len() != names.len() {
            return Err(Error::Io(
                "git hashed a different number of files than were given".to_string(),
            ));
        }
        for (n, id) in names.iter().zip(ids) {
            entries.push((n.to_string(), id.to_string()));
        }
    }
    if let Some(b) = ledger {
        let id = text(
            git(
                repo,
                &["hash-object", "-w", "--no-filters", "--stdin"],
                Some(b),
            )
            .map_err(Error::Io)?,
        );
        entries.push((LEDGER.to_string(), id));
    }
    let id = text(
        git(
            repo,
            &["hash-object", "-w", "--no-filters", "--stdin"],
            Some(manifest),
        )
        .map_err(Error::Io)?,
    );
    entries.push((MANIFEST.to_string(), id));
    let mut info = String::new();
    for (n, id) in &entries {
        info.push_str(&format!("100644 {id}\t{n}\n"));
    }
    let index = PathBuf::from(&gd).join(format!("cig-tin-parcel-{}.index", std::process::id()));
    let _ = fs::remove_file(&index);
    let idx = index.to_string_lossy().to_string();
    let built = (|| -> Result<String, String> {
        git_env(
            repo,
            &["update-index", "--add", "--index-info"],
            Some(info.as_bytes()),
            &[("GIT_INDEX_FILE", &idx)],
        )?;
        let tree = text(git_env(
            repo,
            &["write-tree"],
            None,
            &[("GIT_INDEX_FILE", &idx)],
        )?);
        let who = [
            ("GIT_AUTHOR_NAME", "cig"),
            ("GIT_AUTHOR_EMAIL", "cig@localhost"),
            ("GIT_COMMITTER_NAME", "cig"),
            ("GIT_COMMITTER_EMAIL", "cig@localhost"),
        ];
        let commit = text(git_env(
            repo,
            &[
                "commit-tree",
                &tree,
                "-m",
                &format!("cig tin parcel {refname}"),
            ],
            None,
            &who,
        )?);
        // The empty old value: create the ref, never move one.
        git(repo, &["update-ref", refname, &commit, ""], None)?;
        Ok(commit)
    })();
    let _ = fs::remove_file(&index);
    built.map_err(Error::Io)
}

/// Put a ref's tree into a staging folder to be verified like any other.
fn export_ref(repo: &Path, refname: &str, stage: &Path) -> Result<(), Error> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["archive", "--format=tar", &format!("{refname}^{{commit}}")])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::Usage(format!("git is not available: {e}")))?;
    let mut out = child
        .stdout
        .take()
        .ok_or_else(|| Error::Io("no output from git".to_string()))?;
    fs::create_dir_all(stage)?;
    let mut ar = tar::Archive::new(&mut out);
    let unpacked = ar.unpack(stage);
    let mut sink = Vec::new();
    let _ = out.read_to_end(&mut sink);
    let status = child.wait()?;
    if !status.success() {
        let mut err = String::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut err);
        }
        return Err(Error::Usage(format!(
            "{refname}: no parcel there ({})",
            err.trim()
        )));
    }
    unpacked
        .map_err(|e| Error::Unverified(vec![format!("{refname}: its tree does not unpack: {e}")]))
}

// ---------------------------------------------------------------- verifying

/// A manifest and the folder it was read from.
struct Link {
    dir: PathBuf,
    manifest: Manifest,
}

pub struct Verified {
    pub manifest: Manifest,
    chain: Vec<Link>,
}

fn hash_ok(cache: &mut HashMap<PathBuf, bool>, path: &Path, want: &str) -> bool {
    if let Some(v) = cache.get(path) {
        return *v;
    }
    let ok = sha256_file(path).map(|h| h == want).unwrap_or(false);
    cache.insert(path.to_path_buf(), ok);
    ok
}

fn read_manifest(dir: &Path) -> Result<(Manifest, String), String> {
    let bytes = fs::read(dir.join(MANIFEST))
        .map_err(|e| format!("{}: no readable manifest.json ({e})", dir.display()))?;
    let m: Manifest = serde_json::from_slice(&bytes)
        .map_err(|e| format!("{}: manifest.json does not read ({e})", dir.display()))?;
    if m.format > FORMAT {
        return Err(format!(
            "{}: parcel format {} was written by a newer cig; this one reads up to {FORMAT}",
            dir.display(),
            m.format
        ));
    }
    Ok((m, sha_bytes(&bytes)))
}

/// Check a parcel folder completely, writing nothing. `live` is the state
/// directory whose tin may stand in for an object an incremental parcel
/// leaves to its parent (the restoring machine's own), `None` when the
/// parcel is judged on its own and its parents.
pub fn verify_dir(dir: &Path, live: Option<&Path>) -> Result<Verified, Error> {
    let mut problems: Vec<String> = Vec::new();
    let (manifest, _) = read_manifest(dir).map_err(|e| Error::Unverified(vec![e]))?;
    let mut chain = vec![Link {
        dir: dir.to_path_buf(),
        manifest: manifest.clone(),
    }];
    // The parents, each checked against the digest its child recorded.
    let mut want = manifest.parent.clone();
    let mut depth = 0;
    while let Some(p) = want {
        depth += 1;
        if depth > 64 {
            problems.push(
                "the chain of parents is longer than 64 parcels; it is not followed further"
                    .to_string(),
            );
            break;
        }
        let here = chain.last().map(|l| l.dir.clone()).unwrap_or_default();
        let mut cands = vec![PathBuf::from(&p.location)];
        if let Some(name) = Path::new(&p.location).file_name() {
            if let Some(par) = here.parent() {
                cands.push(par.join(name));
            }
        }
        let found = cands.into_iter().find(|c| c.join(MANIFEST).is_file());
        let Some(pdir) = found else {
            problems.push(format!(
                "the parent parcel {} is not here; an incremental parcel needs it (or a tin that already holds its objects)",
                p.location
            ));
            break;
        };
        match read_manifest(&pdir) {
            Ok((pm, sha)) => {
                if sha != p.manifest_sha256 {
                    problems.push(format!(
                        "the parent parcel {} is not the one this parcel was built on (its manifest hashes to {}, the child recorded {})",
                        pdir.display(),
                        &sha[..12],
                        &p.manifest_sha256[..12.min(p.manifest_sha256.len())]
                    ));
                    break;
                }
                want = pm.parent.clone();
                chain.push(Link {
                    dir: pdir,
                    manifest: pm,
                });
            }
            Err(e) => {
                problems.push(e);
                break;
            }
        }
    }
    let mut cache: HashMap<PathBuf, bool> = HashMap::new();

    // 1. Every file this parcel carries is named, safe and as hashed; no
    //    file is there that the manifest does not name.
    for (rel, sha) in &manifest.files {
        if !safe_rel(rel) {
            problems.push(format!("{rel}: not a path a parcel may carry"));
            continue;
        }
        let p = dir.join(rel);
        if !p.is_file() {
            problems.push(format!(
                "{rel}: named in the manifest and missing (an incomplete parcel)"
            ));
        } else if !hash_ok(&mut cache, &p, sha) {
            problems.push(format!(
                "{rel}: does not hash to what the manifest says (changed since it was written)"
            ));
        } else if rel.starts_with("tin/objects/") && rel.rsplit('/').next() != Some(sha.as_str()) {
            problems.push(format!(
                "{rel}: an object is named by its own hash and this one is not"
            ));
        }
    }
    for e in walkdir::WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .flatten()
    {
        if !e.file_type().is_file() && !e.file_type().is_dir() {
            problems.push(format!("{}: not a file or folder", e.path().display()));
            continue;
        }
        if !e.file_type().is_file() {
            continue;
        }
        let Some(rel) = rel_of(dir, e.path()) else {
            continue;
        };
        if rel != MANIFEST && !manifest.files.contains_key(&rel) {
            problems.push(format!(
                "{rel}: in the parcel and not named in its manifest"
            ));
        }
    }

    // 2. What is left to the parents is there, as hashed.
    let mut effective: BTreeSet<String> = manifest.files.keys().cloned().collect();
    for (rel, sha) in &manifest.inherited {
        effective.insert(rel.clone());
        if !safe_rel(rel) || !is_tin_object(rel) {
            problems.push(format!("{rel}: not an object a parent may hold"));
            continue;
        }
        let from_chain = chain.iter().skip(1).find_map(|l| {
            l.manifest
                .files
                .get(rel)
                .map(|s| (l.dir.join(rel), s.clone()))
        });
        let ok = match from_chain {
            Some((p, s)) => s == *sha && hash_ok(&mut cache, &p, sha),
            None => false,
        };
        if ok {
            continue;
        }
        let in_live = live
            .map(|h| h.join(rel))
            .filter(|p| p.is_file())
            .map(|p| hash_ok(&mut cache, &p, sha))
            .unwrap_or(false);
        if !in_live {
            problems.push(format!(
                "{rel}: left to the parent and found in neither the parent chain nor this tin (an incomplete parcel)"
            ));
        }
    }
    let present = |rel: &str| -> bool {
        effective.contains(rel) || live.map(|h| h.join(rel).is_file()).unwrap_or(false)
    };

    // 3. Recipes are the ones their names say and every chunk is held.
    for rel in manifest
        .files
        .keys()
        .filter(|r| r.starts_with("tin/recipes/"))
    {
        match super::tin::read_recipe(&dir.join(rel)) {
            Ok(r) => {
                for (i, c) in r.chunks.iter().enumerate() {
                    let need = format!(
                        "tin/objects/{}/{}",
                        &c.sha256[..2.min(c.sha256.len())],
                        c.sha256
                    );
                    if !present(&need) {
                        problems.push(format!("{rel}: chunk {} of {} ({}) is not in the parcel (an incomplete parcel)", i + 1, r.chunks.len(), &c.sha256[..12.min(c.sha256.len())]));
                    }
                }
            }
            Err(e) => problems.push(format!("{rel}: {e}")),
        }
    }

    // 4. Every run reads, its chain holds, and what its journal needs is here.
    let origin_tin = manifest.origin.state_dir.join("tin");
    let origin_runs = manifest.origin.state_dir.join("runs");
    for id in &manifest.runs {
        let rd = dir.join("runs").join(id);
        let rec = match RunRecord::load(&rd) {
            Ok(r) => r,
            Err(e) => {
                problems.push(format!("run {id}: record does not read ({e})"));
                continue;
            }
        };
        if rec.id != *id {
            problems.push(format!("run {id}: its record says it is {}", rec.id));
        }
        let j = match Journal::load(&rd) {
            Ok(j) => j,
            Err(e) => {
                problems.push(format!("run {id}: journal does not read ({e})"));
                continue;
            }
        };
        if let Some(b) = &j.chain().broken_at {
            problems.push(format!(
                "run {id}: the journal's chain breaks at line {} ({})",
                b.line, b.reason
            ));
        }
        if let Some(h) = &rec.journal_head {
            if h != j.head() && !j.chain().unverified && !j.chain().other_key {
                problems.push(format!(
                    "run {id}: the record's journal head is not the journal's"
                ));
            }
        }
        for e in j.entries() {
            for b in e.before.iter().chain(e.took.iter()) {
                let snap = match &b.before {
                    Before::File {
                        snapshot,
                        git: None,
                        ..
                    } => snapshot,
                    Before::Dir { snapshot } => snapshot,
                    _ => continue,
                };
                if snap.as_os_str().is_empty() {
                    continue;
                }
                let rel = if let Some(r) = rel_of(&origin_tin, snap) {
                    format!("tin/{r}")
                } else if let Some(r) = rel_of(&origin_runs, snap) {
                    format!("runs/{r}")
                } else {
                    continue;
                };
                if !is_tin_object(&rel) && !rel.starts_with(&format!("runs/{id}/")) {
                    continue;
                }
                if !present(&rel) {
                    problems.push(format!("run {id}: its journal needs {rel}, which the parcel does not hold (an incomplete parcel)"));
                }
            }
        }
    }

    // 5. The ledger's chain and head.
    if manifest.ledger.lines > 0 {
        match fs::read_to_string(dir.join(LEDGER)) {
            Ok(t) => match super::ledger::verify_text(&t) {
                Ok(l) => {
                    if let Some(at) = l.broken_at {
                        problems.push(format!("the ledger's line {at} does not carry cig's signature (E709): it was edited, removed or reordered"));
                    } else if l.head != manifest.ledger.head
                        || l.lines.len() as u64 != manifest.ledger.lines
                    {
                        problems.push("the ledger's head is not the one the manifest names: it was cut short or swapped".to_string());
                    }
                }
                Err(e) => problems.push(format!(
                    "the ledger cannot be verified: {e} (cig tin key import puts the key back)"
                )),
            },
            Err(_) => problems.push(
                "ledger.jsonl: named by the manifest and missing (an incomplete parcel)"
                    .to_string(),
            ),
        }
    }

    if problems.is_empty() {
        Ok(Verified { manifest, chain })
    } else {
        Err(Error::Unverified(problems))
    }
}

// ---------------------------------------------------------------- restoring

fn raw_macs(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str::<super::ledger::Line>(l)
                .map(|x| x.mac)
                .unwrap_or_default()
        })
        .collect()
}

fn install(src: &Path, dst: &Path, read_only: bool) -> io::Result<()> {
    if let Some(p) = dst.parent() {
        fs::create_dir_all(p)?;
    }
    let name = dst
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let tmp = dst.with_file_name(format!(".{name}.restoring"));
    let _ = fs::remove_file(&tmp);
    copy_into(src, &tmp)?;
    if read_only {
        let mut perm = fs::metadata(&tmp)?.permissions();
        perm.set_readonly(true);
        fs::set_permissions(&tmp, perm)?;
    }
    if dst.exists() {
        if let Ok(m) = fs::metadata(dst) {
            let mut perm = m.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perm.set_readonly(false);
            let _ = fs::set_permissions(dst, perm);
        }
    }
    fs::rename(&tmp, dst)
}

fn locate(v: &Verified, rel: &str) -> Option<PathBuf> {
    v.chain
        .iter()
        .find(|l| l.manifest.files.contains_key(rel))
        .map(|l| l.dir.join(rel))
}

/// Verify a parcel, then put back what is missing.
pub fn restore(source: &Source) -> Result<Restored, Error> {
    let home = home();
    let (dir, staged, label) = match source {
        Source::Dir(d) => (d.clone(), None, d.display().to_string()),
        Source::Git { repo, refname } => {
            let stage = home.join(format!(".tin-parcel-staging-{}", std::process::id()));
            let _ = fs::remove_dir_all(&stage);
            if let Err(e) = export_ref(repo, refname, &stage) {
                let _ = fs::remove_dir_all(&stage);
                return Err(e);
            }
            (stage.clone(), Some(stage), refname.clone())
        }
    };
    let out = restore_from(&dir, &home, label);
    if let Some(s) = staged {
        let _ = fs::remove_dir_all(s);
    }
    out
}

fn restore_from(dir: &Path, home: &Path, label: String) -> Result<Restored, Error> {
    let v = verify_dir(dir, Some(home))?;
    let m = &v.manifest;

    // Nothing is written yet: find what is in the way.
    let mut conflicts: Vec<String> = Vec::new();
    let mut same_runs: BTreeSet<String> = BTreeSet::new();
    for id in &m.runs {
        let live = home.join("runs").join(id);
        if !live.exists() {
            continue;
        }
        let theirs = sha256_file(&dir.join("runs").join(id).join("journal.jsonl")).ok();
        let mine = sha256_file(&live.join("journal.jsonl")).ok();
        if theirs.is_some() && theirs == mine {
            same_runs.insert(id.clone());
        } else {
            conflicts.push(format!(
                "run {id} is here with a different journal; it is not overwritten (cig unburn it or cig runs --prune it first, then unparcel again)"
            ));
        }
    }
    for (rel, sha) in m.files.iter().chain(m.inherited.iter()) {
        if !rel.starts_with("tin/sealed/") && !rel.starts_with("tin/keeps/") {
            continue;
        }
        let live = home.join(rel);
        if live.is_file() && sha256_file(&live).ok().as_deref() != Some(sha.as_str()) {
            conflicts.push(format!(
                "{rel} is here with other bytes (a sealed object or a kept copy is never replaced)"
            ));
        }
    }
    let theirs_ledger = if m.ledger.lines > 0 {
        fs::read_to_string(dir.join(LEDGER)).unwrap_or_default()
    } else {
        String::new()
    };
    let mine_ledger = fs::read_to_string(super::ledger::path()).unwrap_or_default();
    let (tm, mm) = (raw_macs(&theirs_ledger), raw_macs(&mine_ledger));
    let ledger_action: &'static str = if tm.is_empty() || tm == mm {
        "unchanged"
    } else if mm.is_empty() {
        "installed"
    } else if tm.starts_with(&mm) {
        "extended"
    } else if mm.starts_with(&tm) {
        "kept"
    } else {
        conflicts.push(
            "the ledger here and the parcel's have different histories; neither is replaced"
                .to_string(),
        );
        "unchanged"
    };
    if !conflicts.is_empty() {
        return Err(Error::Conflict(conflicts));
    }

    // Write: objects first, then the keeps, then each run whole, the ledger last.
    let mut written = 0usize;
    let mut present = 0usize;
    let mut bytes = 0u64;
    let mut all: BTreeMap<&String, &String> = BTreeMap::new();
    for (k, s) in m.files.iter().chain(m.inherited.iter()) {
        all.insert(k, s);
    }
    for (rel, sha) in &all {
        if !is_tin_object(rel) && !rel.starts_with("tin/keeps/") {
            continue;
        }
        let dst = home.join(rel.as_str());
        if dst.is_file() && sha256_file(&dst).ok().as_deref() == Some(sha.as_str()) {
            present += 1;
            continue;
        }
        let src = locate(&v, rel).ok_or_else(|| {
            Error::Unverified(vec![format!("{rel}: nowhere in the parcel chain")])
        })?;
        bytes += fs::metadata(&src)?.len();
        install(&src, &dst, is_tin_object(rel))?;
        written += 1;
    }
    let mut restored = 0usize;
    let remap = m.origin.state_dir != home;
    for id in &m.runs {
        if same_runs.contains(id) {
            continue;
        }
        let src = dir.join("runs").join(id);
        let stage = home.join("runs").join(format!(".{id}.restoring"));
        let _ = fs::remove_dir_all(&stage);
        for e in walkdir::WalkDir::new(&src)
            .follow_links(false)
            .into_iter()
            .flatten()
        {
            if !e.file_type().is_file() {
                continue;
            }
            if let Some(r) = rel_of(&src, e.path()) {
                bytes += fs::metadata(e.path())?.len();
                copy_into(e.path(), &stage.join(r))?;
            }
        }
        if remap {
            map_paths(&stage.join("run.json"), &m.origin.state_dir, home)?;
        }
        fs::rename(&stage, home.join("runs").join(id))?;
        if let Ok(rec) = RunRecord::load(&home.join("runs").join(id)) {
            super::runs_index::upsert(&rec);
        }
        restored += 1;
    }
    if ledger_action == "installed" || ledger_action == "extended" {
        let _guard = super::ledger::lock().map_err(Error::Io)?;
        write_ledger(&theirs_ledger)?;
    }
    Ok(Restored {
        source: label,
        runs_restored: restored,
        runs_present: same_runs.len(),
        objects_written: written,
        objects_present: present,
        bytes,
        ledger: ledger_action,
        ledger_head: if tm.is_empty() {
            mm.last().cloned().unwrap_or_default()
        } else {
            m.ledger.head.clone()
        },
    })
}

/// A run that came from another state directory reads through a path map,
/// as one that came in a single-run parcel does: the journal stays as
/// written (its chain holds), the tin's paths are this machine's.
fn map_paths(run_json: &Path, from: &Path, to: &Path) -> Result<(), Error> {
    let t = fs::read_to_string(run_json)?;
    let mut v: serde_json::Value =
        serde_json::from_str(&t).map_err(|e| Error::Io(format!("{}: {e}", run_json.display())))?;
    let pair = serde_json::json!([from, to]);
    if let Some(obj) = v.as_object_mut() {
        let imp = obj
            .entry("imported")
            .or_insert_with(|| serde_json::json!({"from": "tin parcel", "path_map": []}));
        if let Some(map) = imp
            .as_object_mut()
            .map(|o| o.entry("path_map").or_insert_with(|| serde_json::json!([])))
            .and_then(|m| m.as_array_mut())
        {
            map.push(pair);
        }
    }
    fs::write(
        run_json,
        serde_json::to_string_pretty(&v).unwrap_or_default(),
    )?;
    Ok(())
}

fn write_ledger(text: &str) -> Result<(), Error> {
    let keys = super::seal::ensure_keys_dir().map_err(Error::Io)?;
    let tmp = keys.join("ledger.jsonl.restoring");
    let _ = fs::remove_file(&tmp);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        fs::write(&tmp, text)?;
    }
    fs::rename(&tmp, super::ledger::path())?;
    Ok(())
}
