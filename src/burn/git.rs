// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Git blob reuse: a file under `touches` that git already holds (tracked,
//! and clean against the index) is referenced from the repository's object
//! store instead of copied into the tin, so the tin never doubles what
//! `.git/objects` keeps. The run pins what it refers to under
//! `refs/cig/runs/<run id>` (one tree naming every blob) until the run is
//! pruned, so a `git gc` after a rebase cannot take the bytes with it. Git
//! is only ever read, the pin aside; nothing touches the index or the
//! working tree.

use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The ref a run pins its blobs under.
pub fn pin_ref(run_id: &str) -> String {
    format!("refs/cig/runs/{run_id}")
}

/// The file in a run directory naming the repositories the run pinned in,
/// one work tree per line, so `cig runs --prune` can drop the pins.
pub const PINS: &str = "git-pins.txt";

/// Is git on PATH? Asked once per process.
fn available() -> bool {
    static ONCE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ONCE.get_or_init(|| {
        Command::new("git")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

fn git(work_tree: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("-C").arg(work_tree);
    c.env("GIT_OPTIONAL_LOCKS", "0");
    c.stdin(Stdio::null());
    c
}

/// A repository a pack root lies in, with its index read once.
#[derive(Clone, Debug)]
pub struct Repo {
    /// The work tree, as the disk spells it.
    pub work_tree: PathBuf,
    /// `sha1` or `sha256`: how this repository names its objects.
    pub object_format: String,
    /// Every tracked regular file at stage 0: its path relative to the work
    /// tree (as git prints it) and the blob the index holds for it.
    index: HashMap<PathBuf, String>,
}

/// The repository `dir` is inside, or None: git is not installed, or the
/// directory is not in a work tree.
pub fn repo_for(dir: &Path) -> Option<Repo> {
    if !available() {
        return None;
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let top = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    let work_tree = fs::canonicalize(&top).unwrap_or(top);
    let object_format = git(&work_tree)
        .args(["rev-parse", "--show-object-format"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|f| !f.is_empty())
        .unwrap_or_else(|| "sha1".to_string());
    let ls = git(&work_tree)
        .args(["ls-files", "-s", "-z"])
        .output()
        .ok()?;
    if !ls.status.success() {
        return None;
    }
    let mut index = HashMap::new();
    for entry in ls.stdout.split(|b| *b == 0) {
        if entry.is_empty() {
            continue;
        }
        let text = String::from_utf8_lossy(entry);
        // `<mode> <object> <stage>\t<path>`
        let Some((meta, path)) = text.split_once('\t') else {
            continue;
        };
        let mut parts = meta.split(' ');
        let (Some(mode), Some(object), Some(stage)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if stage != "0" || !(mode == "100644" || mode == "100755") {
            continue;
        }
        index.insert(PathBuf::from(path), object.to_string());
    }
    Some(Repo {
        work_tree,
        object_format,
        index,
    })
}

impl Repo {
    /// The blob the index holds for `path`, when it is a tracked regular file.
    pub fn indexed(&self, path: &Path) -> Option<&str> {
        let parent = fs::canonicalize(path.parent()?).ok()?;
        let real = parent.join(path.file_name()?);
        let rel = real.strip_prefix(&self.work_tree).ok()?;
        self.index.get(rel).map(String::as_str)
    }

    /// `(blob id, sha256)` of a file that is tracked and clean against the
    /// index: the bytes on disk hash to the blob the index names. None for
    /// an untracked file, or one whose bytes differ (dirty), which goes to
    /// the tin as before.
    pub fn clean_blob(&self, path: &Path, len: u64) -> Option<(String, String)> {
        let indexed = self.indexed(path)?;
        let (blob, sha256) = hash_blob(path, &self.object_format, len).ok()?;
        (blob == indexed).then_some((blob, sha256))
    }

    /// Pin every blob under the run's ref: one tree naming them all, so a
    /// `git gc` keeps them until `cig runs --prune` drops the ref.
    pub fn pin(&self, run_id: &str, blobs: &BTreeSet<String>) -> io::Result<()> {
        let mut input = String::new();
        for b in blobs {
            input.push_str(&format!("100644 blob {b}\t{b}\n"));
        }
        let mut child = git(&self.work_tree)
            .arg("mktree")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(input.as_bytes())?;
        }
        let out = child.wait_with_output()?;
        if !out.status.success() {
            return Err(io::Error::other(format!(
                "git mktree: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        let tree = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let out = git(&self.work_tree)
            .args(["update-ref", &pin_ref(run_id), &tree])
            .output()?;
        if !out.status.success() {
            return Err(io::Error::other(format!(
                "git update-ref: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(())
    }
}

/// Drop a run's pin in a repository, if git and the repository are there.
pub fn unpin(work_tree: &Path, run_id: &str) {
    if !available() {
        return;
    }
    let _ = git(work_tree)
        .args(["update-ref", "-d", &pin_ref(run_id)])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Read a file once for both names it has: git's blob id (`blob <len>\0`
/// and the content, hashed as the repository hashes) and our SHA-256.
pub fn hash_blob(path: &Path, object_format: &str, len: u64) -> io::Result<(String, String)> {
    let mut file = fs::File::open(path)?;
    let header = format!("blob {len}\0");
    let mut ours = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    if object_format == "sha256" {
        let mut theirs = Sha256::new();
        theirs.update(header.as_bytes());
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            theirs.update(&buf[..n]);
            ours.update(&buf[..n]);
        }
        Ok((hex::encode(theirs.finalize()), hex::encode(ours.finalize())))
    } else {
        use sha1::Sha1;
        let mut theirs = Sha1::new();
        theirs.update(header.as_bytes());
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            theirs.update(&buf[..n]);
            ours.update(&buf[..n]);
        }
        Ok((hex::encode(theirs.finalize()), hex::encode(ours.finalize())))
    }
}

/// Does the repository still have the blob, at the size the journal
/// recorded? One line per problem.
pub fn check_blob(work_tree: &Path, blob: &str, bytes: u64) -> Vec<String> {
    if !available() {
        return vec!["git is not installed, so the blob cannot be read".to_string()];
    }
    if !work_tree.is_dir() {
        return vec![format!(
            "the repository {} is gone, so git cannot give the blob back",
            work_tree.display()
        )];
    }
    let out = git(work_tree).args(["cat-file", "-s", blob]).output();
    match out {
        Ok(o) if o.status.success() => {
            let size: u64 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap_or(0);
            if size != bytes {
                vec![format!(
                    "git blob {} in {} is {size} bytes, the journal recorded {bytes}",
                    &blob[..blob.len().min(12)],
                    work_tree.display()
                )]
            } else {
                Vec::new()
            }
        }
        Ok(o) => vec![format!(
            "git no longer has blob {} in {} ({}); the run's pin ({}) may have been removed, or the repository replaced",
            &blob[..blob.len().min(12)],
            work_tree.display(),
            String::from_utf8_lossy(&o.stderr).trim(),
            "refs/cig/runs/<run>"
        )],
        Err(e) => vec![format!("git could not be run in {}: {e}", work_tree.display())],
    }
}

/// Stream a blob out of the repository into `dest`, hashing it as it is
/// written and refusing (nothing left at `dest`) when it is not the bytes
/// the journal recorded.
pub fn materialize(
    work_tree: &Path,
    blob: &str,
    expect_sha256: &str,
    dest: &Path,
) -> io::Result<()> {
    let result = (|| -> io::Result<()> {
        if !available() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "git is not installed, so the blob cannot be read",
            ));
        }
        let mut child = git(work_tree)
            .args(["cat-file", "blob", blob])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut out = child.stdout.take().expect("piped stdout");
        let mut file = fs::File::create(dest)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 16];
        loop {
            let n = out.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n])?;
        }
        file.sync_all()?;
        let status = child.wait()?;
        if !status.success() {
            let mut err = String::new();
            if let Some(mut e) = child.stderr.take() {
                let _ = e.read_to_string(&mut err);
            }
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "git no longer has blob {} in {} ({}); the run's pin may have been removed, or the repository replaced",
                    &blob[..blob.len().min(12)],
                    work_tree.display(),
                    err.trim()
                ),
            ));
        }
        let actual = hex::encode(hasher.finalize());
        if !expect_sha256.is_empty() && actual != expect_sha256 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "git blob {} in {} holds other bytes than the journal recorded (sha256 {} against {}); refusing to restore from it",
                    &blob[..blob.len().min(12)],
                    work_tree.display(),
                    &actual[..12],
                    &expect_sha256[..expect_sha256.len().min(12)]
                ),
            ));
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(dest);
    }
    result
}
