// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The recorder: every input the work depended on, noted by whatever
//! mediates the work's reads while it runs. A pass is only as honest as
//! its inputs, and the issuer never declares them by hand.

use crate::encode::{Identity, Input, PresentKind};
use crate::fsid::{self, EntryType};
use crate::issue::IssueError;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

/// One observation of a path, kept so two observations that disagree are
/// caught ("an input changed while the work ran").
#[derive(Clone, Debug, PartialEq, Eq)]
enum Seen {
    File(Identity, bool, [u8; 32]),
    Absent,
    Dir([u8; 32]),
    Present(PresentKind),
}

/// Collects inputs while the work runs.
#[derive(Debug, Default)]
pub struct Recorder {
    paths: BTreeMap<Vec<u8>, Vec<Seen>>,
    env: BTreeMap<Vec<u8>, Option<[u8; 32]>>,
    tools: BTreeMap<String, (String, String)>,
    rules: BTreeMap<String, [u8; 32]>,
    passes: Vec<[u8; 32]>,
    clock: Option<i64>,
    open_world: Option<String>,
    changed: Option<String>,
    declared_reads: bool,
}

impl Recorder {
    pub fn new() -> Self {
        Self::default()
    }

    fn path_key(&mut self, path: &Path) -> Option<Vec<u8>> {
        match fsid::path_bytes(path) {
            Some(b) if crate::encode::is_absolute_bytes(&b) => Some(b),
            Some(_) => {
                self.open_world(format!("{} is not an absolute path", path.display()));
                None
            }
            None => {
                self.open_world(format!("{} is not a valid name here", path.display()));
                None
            }
        }
    }

    fn see(&mut self, key: Vec<u8>, seen: Seen) {
        let list = self.paths.entry(key.clone()).or_default();
        if list.contains(&seen) {
            return;
        }
        // Absent against anything present, or two different contents for
        // one file, or two listings of one folder: the world moved under
        // the work.
        let conflicts = list.iter().any(|s| match (s, &seen) {
            (Seen::Absent, Seen::Absent) => false,
            (Seen::Absent, _) | (_, Seen::Absent) => true,
            (Seen::File(_, _, a), Seen::File(_, _, b)) => a != b,
            (Seen::Dir(a), Seen::Dir(b)) => a != b,
            (Seen::Present(a), Seen::Present(b)) => a != b,
            (Seen::File(..), Seen::Present(k)) | (Seen::Present(k), Seen::File(..)) => {
                *k != PresentKind::File
            }
            (Seen::Dir(_), Seen::Present(k)) | (Seen::Present(k), Seen::Dir(_)) => {
                *k != PresentKind::Dir
            }
            (Seen::File(..), Seen::Dir(_)) | (Seen::Dir(_), Seen::File(..)) => true,
        });
        if conflicts && self.changed.is_none() {
            self.changed = Some(fsid::shown(&key));
        }
        list.push(seen);
    }

    /// The work read this file's bytes through a handle whose metadata is
    /// `meta`. The identity is the handle's, so a swap between the look
    /// and the read is the read's.
    pub fn file_read(&mut self, path: &Path, meta: &fs::Metadata, bytes: &[u8]) {
        self.file_read_at(path, meta, bytes, fsid::wall_ms());
    }

    /// `file_read`, for a read observed at `observed_ms` (the racy rule is
    /// about that moment).
    pub fn file_read_at(
        &mut self,
        path: &Path,
        meta: &fs::Metadata,
        bytes: &[u8],
        observed_ms: i64,
    ) {
        if let Some(k) = self.path_key(path) {
            let id = fsid::identity(meta);
            let racy = fsid::racy_at(&id, observed_ms);
            self.see(k, Seen::File(id, racy, fsid::hash_bytes(bytes)));
        }
    }

    /// Read a file for the work, and record it: one handle, opened without
    /// blocking, `fstat` before and after the read. A file that changed
    /// while it was read gives two answers (E734), and its torn bytes are
    /// never vouched for. A missing file is recorded absent, and the error
    /// is returned for the work to report as usual.
    pub fn read_file(&mut self, path: &Path) -> io::Result<Vec<u8>> {
        use std::io::Read;
        let mut opts = fs::OpenOptions::new();
        opts.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.custom_flags(libc::O_NONBLOCK);
        }
        let mut f = match opts.open(path) {
            Ok(f) => f,
            Err(e) => {
                if e.kind() == io::ErrorKind::NotFound {
                    self.absent(path);
                } else {
                    self.open_world(format!("{} could not be read ({e})", path.display()));
                }
                return Err(e);
            }
        };
        let before = f.metadata()?;
        if !before.is_file() {
            self.open_world(format!("{} is not a regular file", path.display()));
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
        let mut bytes = Vec::with_capacity(before.len().min(1 << 24) as usize);
        f.read_to_end(&mut bytes)?;
        let after = f.metadata()?;
        let (b, a) = (fsid::identity(&before), fsid::identity(&after));
        if b != a || a.size != bytes.len() as u64 {
            if self.changed.is_none() {
                self.changed = Some(path.display().to_string());
            }
            return Ok(bytes);
        }
        if let Some(k) = self.path_key(path) {
            let racy = fsid::racy_at(&a, fsid::wall_ms());
            self.see(k, Seen::File(a, racy, fsid::hash_bytes(&bytes)));
        }
        Ok(bytes)
    }

    /// The work's result depends on this file's bytes (its size, its time):
    /// stat it and hash it now. A missing file is recorded absent.
    pub fn file_seen(&mut self, path: &Path) -> io::Result<()> {
        let Some(k) = self.path_key(path) else {
            return Ok(());
        };
        let mut f = match fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.see(k, Seen::Absent);
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let meta = f.metadata()?;
        if !meta.is_file() {
            self.see(
                k,
                Seen::Present(if meta.is_dir() {
                    PresentKind::Dir
                } else {
                    PresentKind::Other
                }),
            );
            return Ok(());
        }
        let digest = fsid::hash_reader(&mut f)?;
        let after = f.metadata()?;
        let (b, a) = (fsid::identity(&meta), fsid::identity(&after));
        if b != a {
            if self.changed.is_none() {
                self.changed = Some(fsid::shown(&k));
            }
            return Ok(());
        }
        let racy = fsid::racy_at(&a, fsid::wall_ms());
        self.see(k, Seen::File(a, racy, digest));
        Ok(())
    }

    /// The work looked for this path and did not find it.
    pub fn absent(&mut self, path: &Path) {
        if let Some(k) = self.path_key(path) {
            self.see(k, Seen::Absent);
        }
    }

    /// The work checked only that something is at this path, and what.
    pub fn present(&mut self, path: &Path, kind: PresentKind) {
        if let Some(k) = self.path_key(path) {
            self.see(k, Seen::Present(kind));
        }
    }

    /// The work listed this folder and saw these entries.
    pub fn dir_listed(&mut self, path: &Path, entries: &[(Vec<u8>, EntryType)]) {
        if let Some(k) = self.path_key(path) {
            self.see(k, Seen::Dir(fsid::listing_digest(entries)));
        }
    }

    /// The work read a variable (`None`: it was not set).
    pub fn env(&mut self, name: &[u8], value: Option<&[u8]>) {
        if name.is_empty() || name.contains(&b'=') || name.contains(&0) {
            self.open_world(format!(
                "the variable `{}` cannot be recorded",
                String::from_utf8_lossy(name)
            ));
            return;
        }
        let v = value.map(fsid::env_digest);
        match self.env.get(name) {
            Some(old) if *old != v => {
                if self.changed.is_none() {
                    self.changed = Some(format!("${}", String::from_utf8_lossy(name)));
                }
            }
            Some(_) => {}
            None => {
                self.env.insert(name.to_vec(), v);
            }
        }
    }

    /// The result depends on this tool, by version and build.
    pub fn tool(&mut self, name: &str, version: &str, build: &str) {
        let v = (version.to_string(), build.to_string());
        match self.tools.get(name) {
            Some(old) if *old != v => {
                if self.changed.is_none() {
                    self.changed = Some(format!("the tool {name}"));
                }
            }
            Some(_) => {}
            None => {
                self.tools.insert(name.to_string(), v);
            }
        }
    }

    /// The result depends on this rule data.
    pub fn rules(&mut self, name: &str, digest: [u8; 32]) {
        match self.rules.get(name) {
            Some(old) if *old != digest => {
                if self.changed.is_none() {
                    self.changed = Some(format!("the rules {name}"));
                }
            }
            Some(_) => {}
            None => {
                self.rules.insert(name.to_string(), digest);
            }
        }
    }

    /// The work relied on another pass.
    pub fn pass(&mut self, id: [u8; 32]) {
        if !self.passes.contains(&id) {
            self.passes.push(id);
        }
    }

    /// The result holds only until this time: the earliest one wins.
    pub fn clock(&mut self, not_after_ms: i64) {
        self.clock = Some(self.clock.map_or(not_after_ms, |c| c.min(not_after_ms)));
    }

    /// The work did something no recording can see into. The first reason
    /// is the one given.
    pub fn open_world(&mut self, why: String) {
        if self.open_world.is_none() {
            self.open_world = Some(why);
        }
    }

    /// A hop ran with declared `reads`: some inputs are the issuer's
    /// declaration, not observed, and the pass says so.
    pub fn declared_reads(&mut self) {
        self.declared_reads = true;
    }

    /// The pass's flags for what was recorded.
    pub fn flags(&self) -> u8 {
        if self.declared_reads {
            crate::encode::FLAG_DECLARED_READS
        } else {
            0
        }
    }

    /// Has the work gone open-world?
    pub fn is_open_world(&self) -> Option<&str> {
        self.open_world.as_deref()
    }

    /// Merge what another recorder saw (a nested issue's reads count for
    /// the outer one too).
    pub fn absorb(&mut self, other: &Recorder) {
        for (k, list) in &other.paths {
            for s in list {
                self.see(k.clone(), s.clone());
            }
        }
        for (k, v) in &other.env {
            let v = *v;
            match self.env.get(k) {
                Some(old) if *old != v => {
                    if self.changed.is_none() {
                        self.changed = Some(format!("${}", String::from_utf8_lossy(k)));
                    }
                }
                Some(_) => {}
                None => {
                    self.env.insert(k.clone(), v);
                }
            }
        }
        for (k, (v, b)) in &other.tools {
            self.tool(k, v, b);
        }
        for (k, d) in &other.rules {
            self.rules(k, *d);
        }
        for id in &other.passes {
            self.pass(*id);
        }
        if let Some(c) = other.clock {
            self.clock(c);
        }
        if let Some(w) = &other.open_world {
            self.open_world(w.clone());
        }
        if self.changed.is_none() {
            self.changed.clone_from(&other.changed);
        }
        self.declared_reads |= other.declared_reads;
    }

    /// The inputs, sorted and distinct, or why there is no pass.
    pub fn finish(self) -> Result<Vec<Input>, IssueError> {
        if let Some(w) = self.open_world {
            return Err(IssueError::OpenWorld(w));
        }
        if let Some(c) = self.changed {
            return Err(IssueError::Changed(c));
        }
        let mut out: Vec<Input> = Vec::new();
        for (path, list) in self.paths {
            // Keep the strongest distinct observation of each kind: a file
            // read beats a presence check of a file; a listing beats a
            // presence check of a folder.
            let has_file = list.iter().any(|s| matches!(s, Seen::File(..)));
            let any_racy = list.iter().any(|s| matches!(s, Seen::File(_, true, _)));
            let mut file_done = false;
            let has_dir = list.iter().any(|s| matches!(s, Seen::Dir(_)));
            for s in list {
                match s {
                    Seen::File(identity, _, blake3) => {
                        if !file_done {
                            file_done = true;
                            out.push(Input::File {
                                path: path.clone(),
                                identity,
                                racy: any_racy,
                                blake3,
                            });
                        }
                    }
                    Seen::Absent => out.push(Input::Absent { path: path.clone() }),
                    Seen::Dir(listing) => out.push(Input::Dir {
                        path: path.clone(),
                        listing,
                    }),
                    Seen::Present(PresentKind::File) if has_file => {}
                    Seen::Present(PresentKind::Dir) if has_dir => {}
                    Seen::Present(kind) => out.push(Input::Present {
                        path: path.clone(),
                        kind,
                    }),
                }
            }
        }
        for (name, value) in self.env {
            out.push(Input::Env { name, value });
        }
        for (name, (version, build)) in self.tools {
            out.push(Input::Tool {
                name,
                version,
                build,
            });
        }
        for (name, blake3) in self.rules {
            out.push(Input::Rules { name, blake3 });
        }
        for id in self.passes {
            out.push(Input::Pass { id });
        }
        if let Some(c) = self.clock {
            out.push(Input::Clock { not_after_ms: c });
        }
        out.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
        out.dedup_by(|a, b| a.sort_key() == b.sort_key());
        Ok(out)
    }
}
