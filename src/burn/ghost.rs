// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The ghost filesystem: where a dry-run's pretend writes live.
//!
//! In `--dry-run` every burn is simulated. Without an overlay, a step that
//! reads what an earlier step only pretended to write fails, and the
//! dry-run promise holds only for scripts with no data flow. The ghost is an
//! in-memory overlay the reads consult before the disk: simulated writes,
//! appends, mkdirs, copies and moves land here; simulated deletes leave a
//! tombstone, so a later read of that path is reported as `E520` ("removed
//! earlier in this dry-run by op N") instead of a misleading not-found.
//!
//! The manual's names: an overlay; an in-memory shadow filesystem; a
//! copy-on-write layer. It exists only in dry-run mode. `burn unlit` in a
//! real run records intents and touches neither the disk nor a ghost.

use super::Op;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug)]
enum Node {
    File(Vec<u8>),
    Dir,
    /// A copy of another tree: reads under this path map to reads under
    /// `source`, ghost or disk.
    DirFrom(PathBuf),
}

/// What the ghost knows about a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stat {
    /// Removed by a simulated op: its seq and description.
    Absent {
        seq: u64,
        by: String,
    },
    File {
        len: u64,
    },
    Dir,
    /// The ghost has nothing to say; ask the disk.
    Passthrough,
}

/// What a read through the ghost yields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Read {
    Content(Vec<u8>),
    Absent { seq: u64, by: String },
    IsDir,
    Passthrough,
}

#[derive(Clone, Debug, Default)]
pub struct Ghost {
    cwd: PathBuf,
    entries: BTreeMap<PathBuf, (u64, Node)>,
    tombstones: Vec<(PathBuf, u64, String)>,
}

impl Ghost {
    pub fn new() -> Self {
        Self {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
            ..Default::default()
        }
    }

    #[cfg(test)]
    fn with_cwd(cwd: &Path) -> Self {
        Self {
            cwd: cwd.to_path_buf(),
            ..Default::default()
        }
    }

    /// Absolute and lexically normalised, without touching the disk.
    /// The ghost's key for a path: absolute, `.` and `..` folded, and the
    /// part that exists on disk resolved through its symlinks, so that a
    /// write through `link/x` and a read of `real/x` meet at one entry.
    fn abs(&self, p: &Path) -> PathBuf {
        let joined = if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.cwd.join(p)
        };
        let mut out = PathBuf::new();
        for c in joined.components() {
            match c {
                Component::CurDir => {}
                Component::ParentDir => {
                    out.pop();
                }
                other => out.push(other.as_os_str()),
            }
        }
        super::scope_path(&out)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.tombstones.is_empty()
    }

    /// The newest tombstone covering `p` (itself or an ancestor) among
    /// those older than `as_of`.
    fn tombstone_at(&self, p: &Path, as_of: u64) -> Option<(u64, &str)> {
        self.tombstones
            .iter()
            .filter(|(t, seq, _)| (p == t || p.starts_with(t)) && *seq < as_of)
            .map(|(_, seq, by)| (*seq, by.as_str()))
            .max_by_key(|(seq, _)| *seq)
    }

    fn entry_at(&self, p: &Path, as_of: u64) -> Option<(u64, &Node)> {
        self.entries
            .get(p)
            .filter(|(seq, _)| *seq < as_of)
            .map(|(seq, node)| (*seq, node))
    }

    /// A `DirFrom` ancestor newer than any tombstone on `p`: the mapped
    /// source path, and the copy's seq, which becomes the new `as_of` so the
    /// copy reads the source as it was at that moment.
    fn mapped_at(&self, p: &Path, as_of: u64) -> Option<(PathBuf, u64)> {
        let tomb = self.tombstone_at(p, as_of).map(|(s, _)| s).unwrap_or(0);
        for ancestor in p.ancestors().skip(1) {
            if let Some((seq, Node::DirFrom(src))) = self.entry_at(ancestor, as_of) {
                if seq > tomb {
                    let rel = p.strip_prefix(ancestor).ok()?;
                    return Some((src.join(rel), seq));
                }
            }
        }
        None
    }

    fn disk_exists(p: &Path) -> bool {
        fs::symlink_metadata(p).is_ok()
    }

    /// The ghost's view of a path. Never reads file contents from disk.
    pub fn stat(&self, p: &Path) -> Stat {
        self.stat_abs(&self.abs(p), u64::MAX, 0)
    }

    fn stat_abs(&self, p: &Path, as_of: u64, depth: u8) -> Stat {
        if depth > 16 {
            return Stat::Passthrough;
        }
        let tomb = self.tombstone_at(p, as_of);
        if let Some((seq, node)) = self.entry_at(p, as_of) {
            if tomb.is_none_or(|(t, _)| seq > t) {
                return match node {
                    Node::File(b) => Stat::File {
                        len: b.len() as u64,
                    },
                    Node::Dir | Node::DirFrom(_) => Stat::Dir,
                };
            }
        }
        // A directory that exists only because ghost children were created
        // under it (a tombstoned parent re-populated by a later write).
        let has_live_child = self.entries.iter().any(|(k, (seq, _))| {
            k.starts_with(p) && k != p && *seq < as_of && tomb.is_none_or(|(t, _)| *seq > t)
        });
        if has_live_child {
            return Stat::Dir;
        }
        if let Some((seq, by)) = tomb {
            return Stat::Absent {
                seq,
                by: by.to_string(),
            };
        }
        if let Some((mapped, at)) = self.mapped_at(p, as_of) {
            return match self.stat_abs(&mapped, at, depth + 1) {
                Stat::Passthrough => match fs::symlink_metadata(&mapped) {
                    Ok(m) if m.is_dir() => Stat::Dir,
                    Ok(m) => Stat::File { len: m.len() },
                    Err(_) => Stat::Passthrough,
                },
                other => other,
            };
        }
        Stat::Passthrough
    }

    /// Read a file through the ghost.
    pub fn read(&self, p: &Path) -> Read {
        self.read_abs(&self.abs(p), u64::MAX, 0)
    }

    fn read_abs(&self, p: &Path, as_of: u64, depth: u8) -> Read {
        if depth > 16 {
            return Read::Passthrough;
        }
        let tomb = self.tombstone_at(p, as_of);
        if let Some((seq, node)) = self.entry_at(p, as_of) {
            if tomb.is_none_or(|(t, _)| seq > t) {
                return match node {
                    Node::File(b) => Read::Content(b.clone()),
                    Node::Dir | Node::DirFrom(_) => Read::IsDir,
                };
            }
        }
        if let Some((seq, by)) = tomb {
            return Read::Absent {
                seq,
                by: by.to_string(),
            };
        }
        if let Some((mapped, at)) = self.mapped_at(p, as_of) {
            return match self.read_abs(&mapped, at, depth + 1) {
                Read::Passthrough => match fs::read(&mapped) {
                    Ok(b) => Read::Content(b),
                    Err(_) => Read::Passthrough,
                },
                other => other,
            };
        }
        Read::Passthrough
    }

    /// Ghost content, or the disk's, for ops that build on what is there.
    fn read_through(&self, abs: &Path) -> Vec<u8> {
        match self.read_abs(abs, u64::MAX, 0) {
            Read::Content(b) => b,
            Read::Passthrough => fs::read(abs).unwrap_or_default(),
            Read::Absent { .. } | Read::IsDir => Vec::new(),
        }
    }

    /// Every ancestor a write creates gets its own entry, not only the
    /// deepest: `fs.list` enumerates a directory from its entry, and a
    /// parent known only through a live child would list as empty.
    fn ensure_dirs(&mut self, abs: &Path, seq: u64) {
        for ancestor in abs.ancestors().skip(1) {
            if ancestor.as_os_str().is_empty() || ancestor.parent().is_none() {
                break;
            }
            let tomb = self.tombstone_at(ancestor, u64::MAX).map(|(s, _)| s);
            let live_entry = self
                .entry_at(ancestor, u64::MAX)
                .is_some_and(|(seq, _)| tomb.is_none_or(|t| seq > t));
            if live_entry {
                break;
            }
            if tomb.is_none() && Self::disk_exists(ancestor) {
                break;
            }
            self.entries
                .insert(ancestor.to_path_buf(), (seq, Node::Dir));
        }
    }

    fn remove_subtree(&mut self, abs: &Path) {
        self.entries
            .retain(|k, _| !(k == abs || k.starts_with(abs)));
    }

    /// Record a simulated op. Writes and appends get their bytes later, via
    /// [`Ghost::put`], from the function that has them.
    pub fn apply(&mut self, seq: u64, op: &Op) {
        match op {
            Op::Write { path, .. } => {
                let abs = self.abs(path);
                self.ensure_dirs(&abs, seq);
                self.entries.insert(abs, (seq, Node::File(Vec::new())));
            }
            Op::Append { path, .. } => {
                let abs = self.abs(path);
                let current = self.read_through(&abs);
                self.ensure_dirs(&abs, seq);
                self.entries.insert(abs, (seq, Node::File(current)));
            }
            Op::Mkdir { path } => {
                let abs = self.abs(path);
                self.ensure_dirs(&abs, seq);
                self.entries.insert(abs, (seq, Node::Dir));
            }
            Op::Delete { path } => {
                let abs = self.abs(path);
                self.remove_subtree(&abs);
                self.tombstones.push((abs, seq, op.describe()));
            }
            Op::Copy { from, to } => {
                let (from, to) = (self.abs(from), self.abs(to));
                self.copy_into(&from, &to, seq);
            }
            Op::Move { from, to } => {
                let (from, to) = (self.abs(from), self.abs(to));
                self.copy_into(&from, &to, seq);
                self.remove_subtree(&from);
                self.tombstones.push((from, seq, op.describe()));
            }
            Op::Proc { .. }
            | Op::EnvSet { .. }
            | Op::EnvUnset { .. }
            | Op::HopCreated { .. }
            | Op::HopChanged { .. }
            | Op::Compensate { .. } => {}
        }
    }

    fn copy_into(&mut self, from: &Path, to: &Path, seq: u64) {
        self.ensure_dirs(to, seq);
        let is_dir = match self.stat_abs(from, u64::MAX, 0) {
            Stat::Dir => true,
            Stat::File { .. } => false,
            Stat::Absent { .. } => false,
            Stat::Passthrough => fs::metadata(from).map(|m| m.is_dir()).unwrap_or(false),
        };
        self.remove_subtree(to);
        if is_dir {
            // Ghost children of `from` are copied by value; the rest map back.
            let children: Vec<(PathBuf, (u64, Node))> = self
                .entries
                .iter()
                .filter(|(k, _)| k.starts_with(from) && *k != from)
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            self.entries
                .insert(to.to_path_buf(), (seq, Node::DirFrom(from.to_path_buf())));
            for (k, (_, node)) in children {
                if let Ok(rel) = k.strip_prefix(from) {
                    self.entries.insert(to.join(rel), (seq, node));
                }
            }
        } else {
            let content = self.read_through(from);
            self.entries
                .insert(to.to_path_buf(), (seq, Node::File(content)));
        }
    }

    /// Give a simulated write or append its bytes.
    pub fn put(&mut self, p: &Path, bytes: &[u8], append: bool, seq: u64) {
        let abs = self.abs(p);
        let content = if append {
            let mut c = match self.entries.get(&abs) {
                Some((_, Node::File(b))) => b.clone(),
                _ => Vec::new(),
            };
            c.extend_from_slice(bytes);
            c
        } else {
            bytes.to_vec()
        };
        self.ensure_dirs(&abs, seq);
        self.entries.insert(abs, (seq, Node::File(content)));
    }

    /// The names in a directory as the ghost sees them: the disk's entries
    /// minus the removed ones, plus the ghost's. `None` means the ghost has
    /// no opinion about this directory; list the disk.
    pub fn list(&self, dir: &Path) -> Option<Vec<PathBuf>> {
        let abs = self.abs(dir);
        let stat = self.stat_abs(&abs, u64::MAX, 0);
        let touches_dir = self
            .entries
            .keys()
            .any(|k| k.parent() == Some(abs.as_path()))
            || self
                .tombstones
                .iter()
                .any(|(t, _, _)| t.parent() == Some(abs.as_path()))
            || stat != Stat::Passthrough;
        if !touches_dir {
            return None;
        }
        let mut names: Vec<PathBuf> = Vec::new();
        let source_dir = match &stat {
            Stat::Dir => match self.entries.get(&abs) {
                Some((_, Node::DirFrom(src))) => Some(src.clone()),
                _ => None,
            },
            Stat::Passthrough => Some(abs.clone()),
            _ => None,
        };
        if let Some(src) = source_dir {
            if let Ok(rd) = fs::read_dir(&src) {
                for e in rd.flatten() {
                    names.push(abs.join(e.file_name()));
                }
            }
        }
        for k in self.entries.keys() {
            if k.parent() == Some(abs.as_path()) && !names.contains(k) {
                names.push(k.clone());
            }
        }
        names.retain(|n| !matches!(self.stat_abs(n, u64::MAX, 0), Stat::Absent { .. }));
        names.sort();
        Some(names)
    }

    /// Every path the ghost currently holds as present (absolute).
    pub fn live_paths(&self) -> Vec<PathBuf> {
        self.entries
            .keys()
            .filter(|k| !matches!(self.stat_abs(k, u64::MAX, 0), Stat::Absent { .. }))
            .cloned()
            .collect()
    }

    /// Is this absolute path removed as far as the ghost is concerned?
    pub fn is_removed(&self, p: &Path) -> bool {
        matches!(self.stat(p), Stat::Absent { .. })
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op_write(p: &str) -> Op {
        Op::Write {
            path: PathBuf::from(p),
            sensitive: false,
        }
    }

    #[test]
    fn writes_are_readable_and_deletes_leave_tombstones() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("real.txt"), "disk").unwrap();
        let mut g = Ghost::with_cwd(tmp.path());
        g.apply(1, &op_write("out/a.txt"));
        g.put(Path::new("out/a.txt"), b"hello", false, 1);
        assert_eq!(
            g.read(Path::new("out/a.txt")),
            Read::Content(b"hello".to_vec())
        );
        assert_eq!(g.stat(Path::new("out")), Stat::Dir);
        assert_eq!(g.stat(Path::new("real.txt")), Stat::Passthrough);
        g.apply(
            2,
            &Op::Delete {
                path: PathBuf::from("real.txt"),
            },
        );
        assert!(matches!(
            g.read(Path::new("real.txt")),
            Read::Absent { seq: 2, .. }
        ));
        // A later write brings it back.
        g.apply(3, &op_write("real.txt"));
        g.put(Path::new("real.txt"), b"again", false, 3);
        assert_eq!(
            g.read(Path::new("real.txt")),
            Read::Content(b"again".to_vec())
        );
        // Append builds on the disk's content.
        fs::write(tmp.path().join("log.txt"), "one\n").unwrap();
        g.apply(
            4,
            &Op::Append {
                path: PathBuf::from("log.txt"),
                sensitive: false,
            },
        );
        g.put(Path::new("log.txt"), b"two\n", true, 4);
        assert_eq!(
            g.read(Path::new("log.txt")),
            Read::Content(b"one\ntwo\n".to_vec())
        );
    }

    #[test]
    fn moves_copies_and_listings_follow() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("src/sub")).unwrap();
        fs::write(tmp.path().join("src/f.txt"), "f").unwrap();
        fs::write(tmp.path().join("src/sub/g.txt"), "g").unwrap();
        let mut g = Ghost::with_cwd(tmp.path());
        g.apply(
            1,
            &Op::Move {
                from: "src".into(),
                to: "dst".into(),
            },
        );
        assert!(matches!(g.stat(Path::new("src")), Stat::Absent { .. }));
        assert!(matches!(
            g.stat(Path::new("src/f.txt")),
            Stat::Absent { .. }
        ));
        assert_eq!(g.read(Path::new("dst/f.txt")), Read::Content(b"f".to_vec()));
        assert_eq!(
            g.read(Path::new("dst/sub/g.txt")),
            Read::Content(b"g".to_vec())
        );
        assert_eq!(g.stat(Path::new("dst/sub")), Stat::Dir);
        let listed = g.list(Path::new("dst")).unwrap();
        let names: Vec<String> = listed
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["f.txt", "sub"]);
        g.apply(
            2,
            &Op::Delete {
                path: "dst/f.txt".into(),
            },
        );
        let listed = g.list(Path::new("dst")).unwrap();
        assert_eq!(listed.len(), 1);
        g.apply(
            3,
            &Op::Copy {
                from: "dst/sub/g.txt".into(),
                to: "copy.txt".into(),
            },
        );
        assert_eq!(g.read(Path::new("copy.txt")), Read::Content(b"g".to_vec()));
        assert!(g.list(Path::new("elsewhere")).is_none());
    }
}
