// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Hop watching's scan: every entry of a pack root, read on all cores and
//! kept compact (one arena of relative paths, entries of a few dozen bytes,
//! sorted by path), so a million entries fit in memory a laptop has and the
//! difference between the scan before a hop and the scan after is one
//! merge walk. The scan after a hop reuses the scan before it for every
//! directory whose own modification time has not moved (and is old enough
//! for that to mean something), so it stats the files and reads no
//! directory that cannot have changed.

use globset::GlobSet;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::SystemTime;

/// What hop watching sees of one path: enough to notice a change without
/// reading the file. A link is its target, and the time and size of the
/// file it leads to when it leads to one, so a write through it is seen; a
/// directory is watched for being made, deleted or replaced, and its mode
/// is kept so a deleted one comes back as it was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Seen {
    File { mtime: u64, len: u64, mode: u32 },
    Dir { mode: u32, mtime: u64 },
    Link(Box<LinkSeen>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LinkSeen {
    pub target: PathBuf,
    pub through: Option<(u64, u64)>,
}

impl Seen {
    pub fn is_dir(&self) -> bool {
        matches!(self, Seen::Dir { .. })
    }

    pub fn is_link(&self) -> bool {
        matches!(self, Seen::Link(_))
    }

    /// The (mtime, len) of a regular file, for the `left` a journal entry
    /// records.
    pub fn left(&self) -> Option<super::Left> {
        match self {
            Seen::File { mtime, len, .. } => Some(super::Left {
                mtime_ns: *mtime,
                len: *len,
            }),
            _ => None,
        }
    }
}

struct Entry {
    off: u32,
    len: u32,
    seen: Seen,
    /// What the same `lstat` said beyond `Seen` (inode, device, ctime), kept
    /// so the stat cache can use the scan's stat instead of making its own
    /// (#148). Never part of what two scans are compared on.
    ident: Ident,
}

/// The part of one `lstat` that `Seen` leaves out. All zero where the
/// platform has no stable identity (the stat cache is off there).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Ident {
    dev: u64,
    ino: u64,
    ctime_ns: u64,
}

#[cfg(unix)]
fn ident_of(m: &fs::Metadata) -> Ident {
    use std::os::unix::fs::MetadataExt;
    let ctime_ns = (m.ctime() as i128)
        .saturating_mul(1_000_000_000)
        .saturating_add(m.ctime_nsec() as i128);
    Ident {
        dev: m.dev(),
        ino: m.ino(),
        ctime_ns: if ctime_ns < 0 { 0 } else { ctime_ns as u64 },
    }
}

#[cfg(not(unix))]
fn ident_of(_: &fs::Metadata) -> Ident {
    Ident::default()
}

/// One pass over a tree.
pub(crate) struct Scan {
    pub root: PathBuf,
    arena: Vec<u8>,
    entries: Vec<Entry>,
    /// False when the walk stopped at the cap. What was not seen cannot be
    /// reasoned about, and the kernel says so instead of guessing.
    pub complete: bool,
    /// Links to directories, with where they lead: the kernel decides
    /// whether a target is part of the pack or a door out of it.
    pub dir_links: Vec<(PathBuf, PathBuf)>,
    /// When the scan began; a directory's unchanged mtime counts only when it
    /// is older than this by a margin.
    pub started: SystemTime,
    /// The root's own modification time.
    root_mtime: u64,
}

/// The path of an entry relative to its root, as bytes with `/` between
/// components: exact on Unix, UTF-8 on Windows.
fn rel_bytes(root: &Path, path: &Path) -> Option<Vec<u8>> {
    let rel = path.strip_prefix(root).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let mut out = Vec::with_capacity(64);
        for (i, c) in rel.components().enumerate() {
            if i > 0 {
                out.push(b'/');
            }
            out.extend_from_slice(c.as_os_str().as_bytes());
        }
        Some(out)
    }
    #[cfg(not(unix))]
    {
        let mut out = String::with_capacity(64);
        for (i, c) in rel.components().enumerate() {
            if i > 0 {
                out.push('/');
            }
            out.push_str(&c.as_os_str().to_string_lossy());
        }
        Some(out.into_bytes())
    }
}

/// A relative path's bytes back as a path under `root`.
pub(crate) fn join_rel(root: &Path, rel: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        root.join(std::ffi::OsStr::from_bytes(rel))
    }
    #[cfg(not(unix))]
    {
        root.join(String::from_utf8_lossy(rel).as_ref())
    }
}

/// A relative path's bytes as something a glob matches.
pub(crate) fn rel_path(rel: &[u8]) -> std::borrow::Cow<'_, Path> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        std::borrow::Cow::Borrowed(Path::new(std::ffi::OsStr::from_bytes(rel)))
    }
    #[cfg(not(unix))]
    {
        std::borrow::Cow::Owned(PathBuf::from(String::from_utf8_lossy(rel).as_ref()))
    }
}

fn mode_bits(m: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        m.permissions().mode() & 0o7777
    }
    #[cfg(not(unix))]
    {
        let _ = m;
        0
    }
}

fn mtime_of(m: &fs::Metadata) -> u64 {
    super::journal::mtime_ns(m).unwrap_or(0)
}

/// How far from the scan's start a directory's mtime must be for "unchanged"
/// to mean no entry came or went: coarse timestamps (FAT, some network
/// filesystems) move in two-second steps.
const MTIME_MARGIN_NS: u64 = 2_000_000_000;

impl Scan {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    fn rel_of(&self, e: &Entry) -> &[u8] {
        &self.arena[e.off as usize..(e.off + e.len) as usize]
    }

    /// Every entry, in path order: (relative path, what was seen).
    pub fn iter(&self) -> impl Iterator<Item = (&[u8], &Seen)> {
        self.entries.iter().map(|e| (self.rel_of(e), &e.seen))
    }

    /// Every file the scan saw, in path order, with the stat tuple the stat
    /// cache keys on, taken from the scan's own `lstat` (no second one).
    /// `None` for the tuple where the platform gives no identity.
    pub fn iter_stat(
        &self,
    ) -> impl Iterator<Item = (&[u8], &Seen, Option<super::statcache::StatTuple>)> {
        self.entries
            .iter()
            .map(|e| (self.rel_of(e), &e.seen, stat_tuple(&e.seen, &e.ident)))
    }

    /// The stat tuple the scan's `lstat` gave a regular file at this
    /// absolute path, or `None` when the scan did not see a file there.
    pub fn stat_of_path(&self, path: &Path) -> Option<super::statcache::StatTuple> {
        let i = rel_bytes(&self.root, path).and_then(|r| self.index_of(&r))?;
        stat_tuple(&self.entries[i].seen, &self.entries[i].ident)
    }

    /// The absolute path of an entry's relative path.
    pub fn path(&self, rel: &[u8]) -> PathBuf {
        join_rel(&self.root, rel)
    }

    fn index_of(&self, rel: &[u8]) -> Option<usize> {
        self.entries
            .binary_search_by(|e| self.rel_of(e).cmp(rel))
            .ok()
    }

    /// What was seen at a relative path, if anything.
    pub fn find(&self, rel: &[u8]) -> Option<&Seen> {
        self.index_of(rel).map(|i| &self.entries[i].seen)
    }

    /// Was this absolute path seen?
    pub fn has(&self, path: &Path) -> bool {
        rel_bytes(&self.root, path).is_some_and(|r| self.index_of(&r).is_some())
    }

    /// The direct children of a directory (by its relative path; empty for
    /// the root), as (name bytes, what was seen).
    fn children(&self, dir_rel: &[u8]) -> Vec<(&[u8], &Seen)> {
        let mut prefix = dir_rel.to_vec();
        if !prefix.is_empty() {
            prefix.push(b'/');
        }
        // Entries with the prefix are contiguous in path order.
        let start = self
            .entries
            .partition_point(|e| self.rel_of(e) < prefix.as_slice());
        let mut out = Vec::new();
        for e in &self.entries[start..] {
            let rel = self.rel_of(e);
            if !rel.starts_with(&prefix) {
                break;
            }
            let name = &rel[prefix.len()..];
            if !name.contains(&b'/') {
                out.push((name, &e.seen));
            }
        }
        out
    }

    /// The mtime a directory had, by relative path (the root's for empty).
    fn dir_mtime(&self, dir_rel: &[u8]) -> Option<u64> {
        if dir_rel.is_empty() {
            return Some(self.root_mtime);
        }
        match self.find(dir_rel)? {
            Seen::Dir { mtime, .. } => Some(*mtime),
            _ => None,
        }
    }
}

/// Did nothing at all change between these two scans of one root, down to
/// each file's inode and ctime, and is nothing too new to tell? A file whose
/// whole stat tuple reads as it did was not written to (a write moves its
/// ctime), which is all the hash check after a hop looks for; this answers it
/// for every file in one pass over the two arrays. Anything else (an entry
/// made or gone, a tuple that moved, a time inside the racy margin of
/// `started_ns`, a scan that stopped at the cap, a platform with no identity)
/// is `false`, and the file-by-file check runs as it always did.
pub(crate) fn same_stat(before: &Scan, after: &Scan, started_ns: u64) -> bool {
    if !cfg!(unix)
        || !before.complete
        || !after.complete
        || before.entries.len() != after.entries.len()
    {
        return false;
    }
    before.entries.iter().zip(&after.entries).all(|(b, a)| {
        if b.seen != a.seen || b.ident != a.ident || before.rel_of(b) != after.rel_of(a) {
            return false;
        }
        match &b.seen {
            Seen::File { mtime, .. } => {
                !super::statcache::is_racy(*mtime, started_ns)
                    && !super::statcache::is_racy(b.ident.ctime_ns, started_ns)
            }
            _ => true,
        }
    })
}

/// The stat cache's tuple for a regular file from what a scan saw. The fields
/// are the ones `statcache::stat_of` reads, from the same calls.
#[cfg(unix)]
fn stat_tuple(seen: &Seen, id: &Ident) -> Option<super::statcache::StatTuple> {
    match seen {
        Seen::File { mtime, len, mode } => Some(super::statcache::StatTuple {
            dev: id.dev,
            inode: id.ino,
            size: *len,
            mtime_ns: *mtime,
            ctime_ns: id.ctime_ns,
            mode: *mode,
        }),
        _ => None,
    }
}

#[cfg(not(unix))]
fn stat_tuple(_: &Seen, _: &Ident) -> Option<super::statcache::StatTuple> {
    None
}

/// What a worker collected: its own arena and entries into it.
struct Chunk {
    arena: Vec<u8>,
    entries: Vec<Entry>,
}

impl Chunk {
    fn push(&mut self, rel: &[u8], seen: Seen, ident: Ident) {
        let off = self.arena.len() as u32;
        self.arena.extend_from_slice(rel);
        self.entries.push(Entry {
            off,
            len: rel.len() as u32,
            seen,
            ident,
        });
    }
}

/// What a worker makes of one directory entry.
fn look(
    path: &Path,
    rel: &[u8],
    chunk: &mut Chunk,
    dir_links: &mut Vec<(PathBuf, PathBuf)>,
) -> bool {
    super::statcache::SCAN_LSTAT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let Ok(m) = fs::symlink_metadata(path) else {
        return false;
    };
    let ft = m.file_type();
    if ft.is_symlink() {
        let Ok(target) = fs::read_link(path) else {
            return false;
        };
        // A dangling link resolves to nothing and is a link all the same.
        let through = match fs::metadata(path) {
            Ok(t) if t.is_file() => Some((mtime_of(&t), t.len())),
            Ok(t) if t.is_dir() => {
                dir_links.push((
                    path.to_path_buf(),
                    super::journal::resolve_symlink_chain(path),
                ));
                None
            }
            _ => None,
        };
        chunk.push(
            rel,
            Seen::Link(Box::new(LinkSeen { target, through })),
            Ident::default(),
        );
        return false;
    }
    if ft.is_dir() {
        chunk.push(
            rel,
            Seen::Dir {
                mode: mode_bits(&m),
                mtime: mtime_of(&m),
            },
            Ident::default(),
        );
        return true;
    }
    if ft.is_file() {
        chunk.push(
            rel,
            Seen::File {
                mtime: mtime_of(&m),
                len: m.len(),
                mode: mode_bits(&m),
            },
            ident_of(&m),
        );
    }
    false
}

/// Read a tree under `root`, `max_depth` levels down, stopping at `cap`
/// entries (the memory guard; the kernel's rule at the cap still holds).
/// With `reference`, a directory whose mtime has not moved since that scan
/// (and is older than it by the margin) is not read again: its entries are
/// taken from the reference and each is stat'ed, which is what finds a
/// file changed in place.
pub(crate) fn scan(root: &Path, max_depth: usize, cap: usize, reference: Option<&Scan>) -> Scan {
    scan_skipping(root, max_depth, cap, reference, None)
}

/// `scan`, leaving out what `skip` matches (a hop's `skip` globs and the
/// `hop_skip` setting, #148) the way a `.cigignore` line is left out.
pub(crate) fn scan_skipping(
    root: &Path,
    max_depth: usize,
    cap: usize,
    reference: Option<&Scan>,
    skip: Option<&GlobSet>,
) -> Scan {
    let started = SystemTime::now();
    let root_mtime = fs::symlink_metadata(root)
        .map(|m| mtime_of(&m))
        .unwrap_or(0);
    let ignore = super::tin::ignore_set(root);
    // The state directory is never part of a pack, wherever CIGSCRIPT_HOME
    // put it: the tin watching itself would stash its own objects.
    let home = super::runs::home_dir();
    let skipped: HashSet<&str> = super::tin::SKIPPED_ALWAYS.iter().copied().collect();
    let reference = reference.filter(|r| r.root == root && r.complete);
    // A directory is read again unless its mtime is what the reference saw
    // and was already older than the reference's start by the margin: a
    // change in the same coarse tick as that scan could hide otherwise.
    let reference_ns = reference
        .and_then(|r| r.started.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let reuse_ok = |rel: &[u8], mtime_now: u64| -> bool {
        match reference.and_then(|r| r.dir_mtime(rel)) {
            Some(then) => {
                then == mtime_now && mtime_now.saturating_add(MTIME_MARGIN_NS) < reference_ns
            }
            None => false,
        }
    };
    // Directories still to read: (relative path, depth).
    let todo: Mutex<Vec<(Vec<u8>, usize)>> = Mutex::new(vec![(Vec::new(), 0)]);
    let busy = AtomicUsize::new(0);
    let count = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let threads = std::thread::available_parallelism()
        .map(|n| n.get().clamp(1, 8))
        .unwrap_or(1);
    let chunks: Mutex<Vec<Chunk>> = Mutex::new(Vec::new());
    let links: Mutex<Vec<(PathBuf, PathBuf)>> = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                let mut chunk = Chunk {
                    arena: Vec::new(),
                    entries: Vec::new(),
                };
                let mut dir_links = Vec::new();
                loop {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let next = {
                        let mut q = todo.lock().unwrap_or_else(|e| e.into_inner());
                        match q.pop() {
                            Some(d) => {
                                busy.fetch_add(1, Ordering::SeqCst);
                                Some(d)
                            }
                            None => None,
                        }
                    };
                    let Some((dir_rel, depth)) = next else {
                        // Nothing queued: done when no worker is still reading
                        // a directory that could queue more.
                        if busy.load(Ordering::SeqCst) == 0 {
                            break;
                        }
                        // A short wait, not a spin: idle workers burning a
                        // core each while one reads a wide directory helped
                        // nobody.
                        std::thread::sleep(std::time::Duration::from_micros(200));
                        continue;
                    };
                    let dir = join_rel(root, &dir_rel);
                    let dir_mtime = fs::symlink_metadata(&dir)
                        .map(|m| mtime_of(&m))
                        .unwrap_or(0);
                    let mut subdirs: Vec<(Vec<u8>, usize)> = Vec::new();
                    let mut consider = |name: &std::ffi::OsStr, path: PathBuf| {
                        let n = name.to_string_lossy();
                        if skipped.contains(n.as_ref()) || path == home {
                            return;
                        }
                        let mut rel = dir_rel.clone();
                        if !rel.is_empty() {
                            rel.push(b'/');
                        }
                        #[cfg(unix)]
                        {
                            use std::os::unix::ffi::OsStrExt;
                            rel.extend_from_slice(name.as_bytes());
                        }
                        #[cfg(not(unix))]
                        rel.extend_from_slice(n.as_bytes());
                        if let Some(set) = &ignore {
                            if set.is_match(rel_path(&rel).as_ref()) {
                                return;
                            }
                        }
                        if let Some(set) = skip {
                            if set.is_match(rel_path(&rel).as_ref()) {
                                return;
                            }
                        }
                        if count.fetch_add(1, Ordering::Relaxed) >= cap {
                            stop.store(true, Ordering::Relaxed);
                            return;
                        }
                        let is_dir = look(&path, &rel, &mut chunk, &mut dir_links);
                        if is_dir && depth + 1 < max_depth {
                            subdirs.push((rel, depth + 1));
                        }
                    };
                    if reuse_ok(&dir_rel, dir_mtime) {
                        if let Some(r) = reference {
                            for (name, _) in r.children(&dir_rel) {
                                let p = join_rel(&dir, name);
                                let n = p.file_name().map(|n| n.to_os_string()).unwrap_or_default();
                                consider(&n, p);
                            }
                        }
                    } else if let Ok(rd) = fs::read_dir(&dir) {
                        for e in rd.flatten() {
                            consider(&e.file_name(), e.path());
                        }
                    }
                    if !subdirs.is_empty() && !stop.load(Ordering::Relaxed) {
                        todo.lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .extend(subdirs);
                    }
                    busy.fetch_sub(1, Ordering::SeqCst);
                }
                chunks.lock().unwrap_or_else(|e| e.into_inner()).push(chunk);
                links
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .extend(dir_links);
            });
        }
    });
    let complete = !stop.load(Ordering::Relaxed);
    // One arena, one sorted list.
    let mut chunks = chunks.into_inner().unwrap_or_else(|e| e.into_inner());
    let total_bytes: usize = chunks.iter().map(|c| c.arena.len()).sum();
    let total_entries: usize = chunks.iter().map(|c| c.entries.len()).sum();
    let mut arena = Vec::with_capacity(total_bytes);
    let mut entries = Vec::with_capacity(total_entries);
    // Each worker's chunk is let go as soon as it is copied in, so the peak
    // is one arena and a bit, not two.
    while let Some(c) = chunks.pop() {
        let base = arena.len() as u32;
        arena.extend_from_slice(&c.arena);
        drop(c.arena);
        for e in c.entries {
            entries.push(Entry {
                off: e.off + base,
                len: e.len,
                seen: e.seen,
                ident: e.ident,
            });
        }
    }
    entries.sort_unstable_by(|a, b| {
        arena[a.off as usize..(a.off + a.len) as usize]
            .cmp(&arena[b.off as usize..(b.off + b.len) as usize])
    });
    let mut dir_links = links.into_inner().unwrap_or_else(|e| e.into_inner());
    dir_links.sort();
    Scan {
        root: root.to_path_buf(),
        arena,
        entries,
        complete,
        dir_links,
        started,
        root_mtime,
    }
}

/// One step of the merge of two scans of one root, in path order.
pub(crate) enum Step<'a> {
    /// Only in the scan after.
    Created(&'a [u8], &'a Seen),
    /// Only in the scan before.
    Gone(&'a [u8], &'a Seen),
    /// In both.
    Both(&'a [u8], &'a Seen, &'a Seen),
}

/// Walk two sorted scans together, one step at a time: nothing of the
/// merge is held beyond the two scans themselves.
pub(crate) fn merge<'a>(before: &'a Scan, after: &'a Scan) -> Merge<'a> {
    Merge {
        before,
        after,
        i: 0,
        j: 0,
    }
}

pub(crate) struct Merge<'a> {
    before: &'a Scan,
    after: &'a Scan,
    i: usize,
    j: usize,
}

impl<'a> Iterator for Merge<'a> {
    type Item = Step<'a>;

    fn next(&mut self) -> Option<Step<'a>> {
        let (before, after) = (self.before, self.after);
        match (before.entries.get(self.i), after.entries.get(self.j)) {
            (Some(b), Some(a)) => {
                let (br, ar) = (before.rel_of(b), after.rel_of(a));
                match br.cmp(ar) {
                    std::cmp::Ordering::Less => {
                        self.i += 1;
                        Some(Step::Gone(br, &b.seen))
                    }
                    std::cmp::Ordering::Greater => {
                        self.j += 1;
                        Some(Step::Created(ar, &a.seen))
                    }
                    std::cmp::Ordering::Equal => {
                        self.i += 1;
                        self.j += 1;
                        Some(Step::Both(br, &b.seen, &a.seen))
                    }
                }
            }
            (Some(b), None) => {
                self.i += 1;
                Some(Step::Gone(before.rel_of(b), &b.seen))
            }
            (None, Some(a)) => {
                self.j += 1;
                Some(Step::Created(after.rel_of(a), &a.seen))
            }
            (None, None) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #148: `same_stat` is the one-pass proof that no file was written to
    /// between two scans: any moved tuple, entry made or gone, or time too
    /// new to tell, says no.
    #[cfg(unix)]
    #[test]
    fn same_stat_proves_a_quiet_tree_and_nothing_else() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("d")).unwrap();
        for f in ["d/a.txt", "d/b.txt", "c.txt"] {
            fs::write(root.join(f), f).unwrap();
        }
        let first = scan(root, 64, 1_000_000, None);
        let second = scan(root, 64, 1_000_000, None);
        let far = u64::MAX;
        assert!(
            same_stat(&first, &second, far),
            "two scans of a quiet tree agree"
        );
        let now = super::super::statcache::now_ns();
        assert!(
            !same_stat(&first, &second, now),
            "files written a moment ago are too new to vouch for"
        );
        // A same-size write that puts the time back still moves the ctime.
        let a = root.join("d/a.txt");
        let m = fs::metadata(&a).unwrap().modified().unwrap();
        fs::write(&a, "d/a.txt").unwrap();
        fs::File::options()
            .write(true)
            .open(&a)
            .unwrap()
            .set_modified(m)
            .unwrap();
        let third = scan(root, 64, 1_000_000, None);
        assert!(!same_stat(&first, &third, far), "a write moves the ctime");
        fs::write(root.join("new.txt"), "n").unwrap();
        let fourth = scan(root, 64, 1_000_000, None);
        assert!(
            !same_stat(&second, &fourth, far),
            "an entry made is a change"
        );
        // The stat tuple a scan keeps is the one the cache reads itself.
        let t = second.stat_of_path(&root.join("c.txt")).unwrap();
        assert_eq!(
            Some(t),
            super::super::statcache::stat_of(&root.join("c.txt"))
        );
    }

    #[test]
    fn a_scan_is_sorted_complete_and_diffs_by_merge() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for d in ["a", "a/b", "c", "node_modules/x"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        for f in [
            "a/one.txt",
            "a/b/two.txt",
            "c/three.txt",
            "node_modules/x/dep.js",
            "top.txt",
        ] {
            fs::write(root.join(f), f).unwrap();
        }
        let before = scan(root, 64, 1_000_000, None);
        assert!(before.complete);
        let rels: Vec<String> = before
            .iter()
            .map(|(r, _)| String::from_utf8_lossy(r).to_string())
            .collect();
        assert_eq!(
            rels,
            vec![
                "a",
                "a/b",
                "a/b/two.txt",
                "a/one.txt",
                "c",
                "c/three.txt",
                "top.txt"
            ]
        );
        assert!(before.find(b"a/one.txt").is_some_and(|s| !s.is_dir()));
        assert!(before.find(b"a/b").is_some_and(Seen::is_dir));
        assert!(before.has(&root.join("top.txt")) && !before.has(&root.join("nope")));
        // Change, create, delete; then the merge says exactly that.
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(root.join("a/one.txt"), "changed and longer").unwrap();
        fs::write(root.join("c/new.txt"), "n").unwrap();
        fs::remove_file(root.join("top.txt")).unwrap();
        let after = scan(root, 64, 1_000_000, Some(&before));
        let mut created = Vec::new();
        let mut gone = Vec::new();
        let mut changed = Vec::new();
        for step in merge(&before, &after) {
            match step {
                Step::Created(r, _) => created.push(String::from_utf8_lossy(r).to_string()),
                Step::Gone(r, _) => gone.push(String::from_utf8_lossy(r).to_string()),
                Step::Both(r, b, a) if b != a && !b.is_dir() => {
                    changed.push(String::from_utf8_lossy(r).to_string())
                }
                Step::Both(..) => {}
            }
        }
        assert_eq!(created, vec!["c/new.txt"]);
        assert_eq!(gone, vec!["top.txt"]);
        assert_eq!(changed, vec!["a/one.txt"]);
        // The cap stops the walk and says so.
        let capped = scan(root, 64, 3, None);
        assert!(!capped.complete);
        assert!(capped.len() <= 4);
    }
}
