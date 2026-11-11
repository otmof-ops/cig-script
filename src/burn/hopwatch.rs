// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Change notifications for hops (#148, `hop_watch`, off by default). Not the
//! box's watch (`watch.rs`, #106): this one never reports anything to a
//! script. It only lets a hop over a tree nobody touched skip the stat walk.
//!
//! A process that runs several hops over one pack keeps an inotify watch
//! (Linux), an FSEvents stream (macOS) or a `ReadDirectoryChangesW` request
//! per root (Windows) alive between them. The scan taken at the end of one
//! hop is kept; when the next hop begins and the watch has seen nothing since,
//! that scan stands in for the walk, and the stat cache's per-file `lstat`
//! is skipped for every file the watch has not seen change since the cache
//! was written. A hop that changed nothing skips its scan after it too.
//!
//! The rule that keeps it honest: the watch only ever *removes* a walk, never
//! decides a change. Any doubt (a queue overflow, a directory made, moved or
//! removed, a watch that could not be added, a link that leads out of the
//! tree, more dirty paths than the memory guard allows, another pack, other
//! `skip` globs) and the hop does the walk it always did, and arms again.
//!
//! What a change notification cannot see, and the walk can: a file written
//! through a writable `mmap`, and one reached through a hard link kept outside
//! the pack. `hop_watch` is off by default for that reason, and docs/BURN.md
//! says so.

use super::scan::Scan;
use globset::GlobSet;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

/// Is `hop_watch` on in this process? Read once, like `hop_statcache`.
pub fn is_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        matches!(
            crate::config::Config::load().get("hop_watch").as_str(),
            "on"
        )
    })
}

/// A test-only trace: when `CIG_INTERNAL_HOPWATCH` names a file, one byte is
/// appended to it for each walk or `lstat` the watch let a hop skip (`b` a
/// scan before, `e` a scan after, `t` a trusted cache entry). A no-op
/// otherwise, like the tin's read counter.
pub(crate) fn skipped(what: u8) {
    let Some(path) = std::env::var_os("CIG_INTERNAL_HOPWATCH") else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        let _ = f.write_all(&[what]);
    }
}

/// More dirty paths than this and the watch gives up for the arming: it is
/// a memory guard, and a tree that busy gains nothing from a watch.
const DIRTY_CAP: usize = 1_000_000;

#[derive(Default)]
struct Log {
    /// Counts every event kept. Two reads of it that agree mean nothing was
    /// seen between them.
    seq: u64,
    /// The watch can no longer vouch for the tree.
    broken: bool,
    /// The last event's number, by (canonical) path.
    dirty: HashMap<PathBuf, u64>,
}

struct Root {
    /// As the pack names it.
    given: PathBuf,
    /// As the filesystem names it (what events carry).
    canon: PathBuf,
    ignore: Option<GlobSet>,
}

/// What the scan leaves out is left out here, so a build writing into
/// `.git` or a skipped cache does not spoil a hop over the rest.
struct Filter {
    roots: Vec<Root>,
    skip: Option<GlobSet>,
    home: PathBuf,
}

impl Filter {
    fn ignores(&self, path: &Path) -> bool {
        if path.starts_with(&self.home) {
            return true;
        }
        let Some(root) = self.roots.iter().find(|r| path.starts_with(&r.canon)) else {
            return false;
        };
        let Ok(rel) = path.strip_prefix(&root.canon) else {
            return false;
        };
        let mut at = PathBuf::new();
        for c in rel.components() {
            let name = c.as_os_str();
            if super::tin::SKIPPED_ALWAYS.contains(&name.to_string_lossy().as_ref()) {
                return true;
            }
            at.push(name);
            let hit = |set: &Option<GlobSet>| set.as_ref().is_some_and(|s| s.is_match(&at));
            if hit(&root.ignore) || hit(&self.skip) {
                return true;
            }
        }
        false
    }

    /// A path as the pack names it, in the filesystem's own spelling.
    fn canon(&self, path: &Path) -> PathBuf {
        for r in &self.roots {
            if let Ok(rel) = path.strip_prefix(&r.given) {
                return r.canon.join(rel);
            }
        }
        path.to_path_buf()
    }
}

/// What the events fill and the hops read.
struct Shared {
    log: Mutex<Log>,
    filter: Filter,
    imp: Mutex<Option<imp::Imp>>,
}

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    fn note(&self, path: PathBuf) {
        if self.filter.ignores(&path) {
            return;
        }
        let mut log = locked(&self.log);
        log.seq += 1;
        if log.dirty.len() >= DIRTY_CAP {
            log.broken = true;
            log.dirty.clear();
            return;
        }
        let seq = log.seq;
        log.dirty.insert(path, seq);
    }

    fn mark_broken(&self) {
        locked(&self.log).broken = true;
    }

    /// Take in whatever the OS has queued, then say where the log stands:
    /// `None` when the watch can no longer vouch for anything.
    fn sync(&self) -> Option<u64> {
        if let Some(imp) = locked(&self.imp).as_mut() {
            imp.sync(self);
        }
        let log = locked(&self.log);
        (!log.broken).then_some(log.seq)
    }
}

/// A hop's view of the watch: where the log stood when the hop began.
pub(crate) struct Token {
    shared: Arc<Shared>,
    seq: u64,
}

impl Token {
    /// Has the watch seen nothing at all since the hop began?
    pub(crate) fn quiet(&self) -> bool {
        self.shared.sync() == Some(self.seq)
    }

    /// Where the log stood when the hop began.
    pub(crate) fn seq(&self) -> u64 {
        self.seq
    }

    /// The log's number now, read before a scan so what the scan saw is
    /// covered by it; `None` when the watch is broken.
    pub(crate) fn now(&self) -> Option<u64> {
        self.shared.sync()
    }
}

/// The paths whose stat-cache entries this process wrote and the watch has
/// not seen change since.
pub(crate) struct Trusted {
    shared: Arc<Shared>,
    /// The `written_at_ns` of the cache file these marks belong to.
    written_at: u64,
    marks: HashMap<PathBuf, u64>,
}

impl Trusted {
    /// The marks and the log speak the filesystem's spelling of a path.
    fn key(&self, path: &Path) -> PathBuf {
        self.shared.filter.canon(path)
    }

    /// Is the cache entry for `path` still what the file is? Only when the
    /// cache on disk is the one this process wrote, the entry was refreshed
    /// at a mark, and the watch has not seen the path since.
    pub(crate) fn untouched(&self, path: &Path, cache_written_at: u64) -> bool {
        if cache_written_at != self.written_at {
            return false;
        }
        let key = self.key(path);
        let log = locked(&self.shared.log);
        if log.broken {
            return false;
        }
        match (self.marks.get(&key), log.dirty.get(&key)) {
            (Some(_), None) => true,
            (Some(mark), Some(seen)) => seen <= mark,
            _ => false,
        }
    }
}

/// What `begin` hands a hop.
pub(crate) struct Begun {
    /// The scans from the end of the last hop, when nothing has moved since.
    pub scans: Option<Vec<Scan>>,
    pub token: Option<Token>,
    pub trusted: Option<Trusted>,
}

struct Armed {
    shared: Arc<Shared>,
    roots: Vec<PathBuf>,
    key: String,
    cap: usize,
    snapshot: Option<(u64, Vec<Scan>)>,
    trusted: Option<(u64, HashMap<PathBuf, u64>)>,
}

static STATE: Mutex<Option<Armed>> = Mutex::new(None);

/// A hop is about to begin. Keeps the watch if it is the same pack, the
/// same `skip` and the same cap and is not in doubt; otherwise arms a new
/// one (before any walk, so nothing that walk sees can change unseen).
pub(crate) fn begin(
    roots: &[PathBuf],
    skip_key: &str,
    skip: Option<&GlobSet>,
    cap: usize,
) -> Begun {
    let mut state = locked(&STATE);
    if let Some(a) = state.as_mut() {
        let same = a.roots == roots && a.key == skip_key && a.cap == cap;
        if same {
            if let Some(now) = a.shared.sync() {
                let scans = match a.snapshot.take() {
                    Some((at, scans)) if at == now => Some(scans),
                    _ => None,
                };
                let trusted = a.trusted.take().map(|(written_at, marks)| Trusted {
                    shared: a.shared.clone(),
                    written_at,
                    marks,
                });
                return Begun {
                    scans,
                    token: Some(Token {
                        shared: a.shared.clone(),
                        seq: now,
                    }),
                    trusted,
                };
            }
        }
    }
    *state = None;
    let Some(shared) = arm(roots, skip, cap) else {
        return Begun {
            scans: None,
            token: None,
            trusted: None,
        };
    };
    let Some(seq) = shared.sync() else {
        return Begun {
            scans: None,
            token: None,
            trusted: None,
        };
    };
    *state = Some(Armed {
        shared: shared.clone(),
        roots: roots.to_vec(),
        key: skip_key.to_string(),
        cap,
        snapshot: None,
        trusted: None,
    });
    Begun {
        scans: None,
        token: Some(Token { shared, seq }),
        trusted: None,
    }
}

/// Keep the scans a hop ended with, taken after the log read `at`: valid for
/// the next hop only while the log still reads `at`.
pub(crate) fn keep_scans(token: &Token, at: u64, scans: Vec<Scan>) {
    let mut state = locked(&STATE);
    if let Some(a) = state.as_mut() {
        if Arc::ptr_eq(&a.shared, &token.shared) {
            a.snapshot = Some((at, scans));
        }
    }
}

/// The stat cache was just written (`written_at`) with entries for `paths`,
/// each stat'ed or confirmed after the log read `token.seq`.
pub(crate) fn keep_marks<'a>(
    token: &Token,
    written_at: u64,
    paths: impl Iterator<Item = &'a Path>,
) {
    let mut state = locked(&STATE);
    if let Some(a) = state.as_mut() {
        if Arc::ptr_eq(&a.shared, &token.shared) {
            let marks = paths
                .map(|p| (token.shared.filter.canon(p), token.seq))
                .collect::<HashMap<_, _>>();
            a.trusted = Some((written_at, marks));
        }
    }
}

fn arm(roots: &[PathBuf], skip: Option<&GlobSet>, cap: usize) -> Option<Arc<Shared>> {
    let mut rs = Vec::new();
    for r in roots {
        rs.push(Root {
            given: r.clone(),
            canon: std::fs::canonicalize(r).ok()?,
            ignore: super::tin::ignore_set(r),
        });
    }
    let shared = Arc::new(Shared {
        log: Mutex::new(Log::default()),
        filter: Filter {
            roots: rs,
            skip: skip.cloned(),
            home: std::fs::canonicalize(super::runs::home_dir())
                .unwrap_or_else(|_| super::runs::home_dir()),
        },
        imp: Mutex::new(None),
    });
    let made = imp::Imp::arm(&shared, cap)?;
    *locked(&shared.imp) = Some(made);
    Some(shared)
}

#[cfg(target_os = "linux")]
mod imp {
    use super::Shared;
    use std::collections::HashMap;
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    const WATCH: u32 = libc::IN_MODIFY
        | libc::IN_ATTRIB
        | libc::IN_CLOSE_WRITE
        | libc::IN_CREATE
        | libc::IN_DELETE
        | libc::IN_MOVED_FROM
        | libc::IN_MOVED_TO
        | libc::IN_DELETE_SELF
        | libc::IN_MOVE_SELF
        | libc::IN_ONLYDIR
        | libc::IN_DONT_FOLLOW;

    pub(super) struct Imp {
        fd: OwnedFd,
        dirs: HashMap<i32, PathBuf>,
    }

    impl Imp {
        /// One inotify watch per directory the scan would read. `None`
        /// when the kernel will not take them all, or a link leads out.
        pub(super) fn arm(shared: &Arc<Shared>, cap: usize) -> Option<Imp> {
            // SAFETY: plain syscall; the result is checked and owned below.
            let raw = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
            if raw < 0 {
                return None;
            }
            // SAFETY: `raw` is a fresh descriptor nothing else owns.
            let fd = unsafe { OwnedFd::from_raw_fd(raw) };
            let mut imp = Imp {
                fd,
                dirs: HashMap::new(),
            };
            let mut todo: Vec<PathBuf> = shared
                .filter
                .roots
                .iter()
                .map(|r| r.canon.clone())
                .collect();
            while let Some(dir) = todo.pop() {
                if imp.dirs.len() >= cap {
                    return None;
                }
                let c = CString::new(dir.as_os_str().as_bytes()).ok()?;
                // SAFETY: `c` is a NUL-terminated path; `fd` is open.
                let wd = unsafe { libc::inotify_add_watch(imp.fd.as_raw_fd(), c.as_ptr(), WATCH) };
                if wd < 0 {
                    return None;
                }
                imp.dirs.insert(wd, dir.clone());
                for e in std::fs::read_dir(&dir).ok()?.flatten() {
                    let path = e.path();
                    let ft = e.file_type().ok()?;
                    if ft.is_symlink() {
                        // A write through a link lands where it leads: fine
                        // inside the watched trees, unseen outside them.
                        let inside = std::fs::canonicalize(&path).ok().is_some_and(|t| {
                            shared.filter.roots.iter().any(|r| t.starts_with(&r.canon))
                        });
                        if !inside && !shared.filter.ignores(&path) {
                            return None;
                        }
                    } else if ft.is_dir() && !shared.filter.ignores(&path) {
                        todo.push(path);
                    }
                }
            }
            Some(imp)
        }

        /// Read every queued event.
        pub(super) fn sync(&mut self, shared: &Shared) {
            let mut buf = vec![0u8; 1 << 16];
            loop {
                // SAFETY: `buf` is a live buffer of the length given.
                let n = unsafe {
                    libc::read(
                        self.fd.as_raw_fd(),
                        buf.as_mut_ptr().cast::<libc::c_void>(),
                        buf.len(),
                    )
                };
                if n <= 0 {
                    // EAGAIN: drained. Anything else: the watch is unreadable.
                    if n < 0
                        && std::io::Error::last_os_error().kind() != std::io::ErrorKind::WouldBlock
                    {
                        shared.mark_broken();
                    }
                    return;
                }
                parse(&buf[..n as usize], &self.dirs, shared);
            }
        }
    }

    /// The events in one `read`: a 16-byte header (wd, mask, cookie, name
    /// length) and the NUL-padded name.
    pub(super) fn parse(buf: &[u8], dirs: &HashMap<i32, PathBuf>, shared: &Shared) {
        let mut at = 0usize;
        while at + 16 <= buf.len() {
            let word = |o: usize| u32::from_ne_bytes(buf[at + o..at + o + 4].try_into().unwrap());
            let wd = word(0) as i32;
            let mask = word(4);
            let len = word(12) as usize;
            let name_end = (at + 16 + len).min(buf.len());
            let raw = &buf[at + 16..name_end];
            let name = &raw[..raw.iter().position(|b| *b == 0).unwrap_or(raw.len())];
            at = name_end;
            if mask & libc::IN_Q_OVERFLOW != 0 {
                shared.mark_broken();
                continue;
            }
            if mask
                & (libc::IN_IGNORED | libc::IN_UNMOUNT | libc::IN_DELETE_SELF | libc::IN_MOVE_SELF)
                != 0
            {
                shared.mark_broken();
                continue;
            }
            let Some(dir) = dirs.get(&wd) else {
                shared.mark_broken();
                continue;
            };
            let path = if name.is_empty() {
                dir.clone()
            } else {
                dir.join(Path::new(std::ffi::OsStr::from_bytes(name)))
            };
            let dir_change = mask & libc::IN_ISDIR != 0
                && mask
                    & (libc::IN_CREATE | libc::IN_MOVED_TO | libc::IN_MOVED_FROM | libc::IN_DELETE)
                    != 0;
            if dir_change {
                // A directory made or gone has contents no watch has seen.
                if !shared.filter.ignores(&path) {
                    shared.mark_broken();
                }
                continue;
            }
            shared.note(path);
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    //! FSEvents: one stream over every root, events kept by path. The
    //! stream is flushed before each read, so what the kernel has seen has
    //! been delivered to the callback by the time `sync` returns.
    use super::Shared;
    use std::ffi::{c_char, c_void, CStr};
    use std::path::PathBuf;
    use std::sync::{Arc, Weak};

    type Ref = *const c_void;
    const SINCE_NOW: u64 = u64::MAX;
    const FILE_EVENTS: u32 = 0x10;
    const NO_DEFER: u32 = 0x02;
    const UTF8: u32 = 0x0800_0100;
    const BROKEN: u32 = 0x1 | 0x2 | 0x4 | 0x20 | 0x40 | 0x80;

    #[repr(C)]
    struct Context {
        version: isize,
        info: *mut c_void,
        retain: Option<extern "C" fn(*const c_void) -> *const c_void>,
        release: Option<extern "C" fn(*const c_void)>,
        copy_description: Option<extern "C" fn(*const c_void) -> Ref>,
    }

    #[link(name = "CoreServices", kind = "framework")]
    extern "C" {
        fn FSEventStreamCreate(
            allocator: Ref,
            callback: extern "C" fn(Ref, *mut c_void, usize, *mut c_void, *const u32, *const u64),
            context: *const Context,
            paths: Ref,
            since: u64,
            latency: f64,
            flags: u32,
        ) -> Ref;
        fn FSEventStreamScheduleWithRunLoop(stream: Ref, run_loop: Ref, mode: Ref);
        fn FSEventStreamStart(stream: Ref) -> u8;
        fn FSEventStreamFlushSync(stream: Ref);
        fn FSEventStreamStop(stream: Ref);
        fn FSEventStreamInvalidate(stream: Ref);
        fn FSEventStreamRelease(stream: Ref);
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFAllocatorDefault: Ref;
        static kCFTypeArrayCallBacks: c_void;
        static kCFRunLoopDefaultMode: Ref;
        fn CFStringCreateWithBytes(
            alloc: Ref,
            bytes: *const u8,
            n: isize,
            enc: u32,
            ext: u8,
        ) -> Ref;
        fn CFArrayCreate(alloc: Ref, values: *const Ref, n: isize, cb: *const c_void) -> Ref;
        fn CFRelease(r: Ref);
        fn CFRunLoopGetCurrent() -> Ref;
        fn CFRunLoopRun();
        fn CFRunLoopStop(r: Ref);
    }

    struct Handle(Ref);
    // SAFETY: the stream and run loop refs are only used through the
    // thread-safe FSEvents/CFRunLoop calls below.
    unsafe impl Send for Handle {}

    pub(super) struct Imp {
        stream: Handle,
        run_loop: Handle,
        weak: *const Shared,
    }

    // SAFETY: `weak` is only read in `drop`.
    unsafe impl Send for Imp {}

    extern "C" fn on_events(
        _stream: Ref,
        info: *mut c_void,
        n: usize,
        paths: *mut c_void,
        flags: *const u32,
        _ids: *const u64,
    ) {
        // SAFETY: `info` is the `Weak<Shared>` leaked in `arm` and freed by
        // `Imp::drop`; it is upgraded without taking its count. FSEvents
        // gives `n` C strings and `n` flag words.
        let weak = std::mem::ManuallyDrop::new(unsafe { Weak::from_raw(info as *const Shared) });
        let Some(shared) = weak.upgrade() else {
            return;
        };
        let paths = paths as *const *const c_char;
        for i in 0..n {
            let flag = unsafe { *flags.add(i) };
            if flag & BROKEN != 0 {
                shared.mark_broken();
                continue;
            }
            let p = unsafe { CStr::from_ptr(*paths.add(i)) };
            shared.note(PathBuf::from(p.to_string_lossy().to_string()));
        }
    }

    impl Imp {
        pub(super) fn arm(shared: &Arc<Shared>, _cap: usize) -> Option<Imp> {
            let roots: Vec<String> = shared
                .filter
                .roots
                .iter()
                .map(|r| r.canon.to_string_lossy().to_string())
                .collect();
            // SAFETY: CoreFoundation calls on values made here; the stream
            // keeps a pointer to the leaked `Arc` for the process's life.
            unsafe {
                let strs: Vec<Ref> = roots
                    .iter()
                    .map(|r| {
                        CFStringCreateWithBytes(
                            kCFAllocatorDefault,
                            r.as_ptr(),
                            r.len() as isize,
                            UTF8,
                            0,
                        )
                    })
                    .collect();
                let arr = CFArrayCreate(
                    kCFAllocatorDefault,
                    strs.as_ptr(),
                    strs.len() as isize,
                    &kCFTypeArrayCallBacks,
                );
                let weak = Weak::into_raw(Arc::downgrade(shared));
                let ctx = Context {
                    version: 0,
                    info: weak as *mut c_void,
                    retain: None,
                    release: None,
                    copy_description: None,
                };
                let stream = FSEventStreamCreate(
                    kCFAllocatorDefault,
                    on_events,
                    &ctx,
                    arr,
                    SINCE_NOW,
                    0.0,
                    FILE_EVENTS | NO_DEFER,
                );
                CFRelease(arr);
                for s in strs {
                    CFRelease(s);
                }
                if stream.is_null() {
                    drop(Weak::from_raw(weak));
                    return None;
                }
                let (tx, rx) = std::sync::mpsc::channel::<Handle>();
                let s2 = Handle(stream);
                std::thread::spawn(move || {
                    let s = s2;
                    let rl = CFRunLoopGetCurrent();
                    FSEventStreamScheduleWithRunLoop(s.0, rl, kCFRunLoopDefaultMode);
                    let ok = FSEventStreamStart(s.0) != 0;
                    let _ = tx.send(Handle(if ok { rl } else { std::ptr::null() }));
                    if ok {
                        CFRunLoopRun();
                    }
                });
                let run_loop = rx.recv().ok()?;
                if run_loop.0.is_null() {
                    drop(Weak::from_raw(weak));
                    return None;
                }
                Some(Imp {
                    stream: Handle(stream),
                    run_loop,
                    weak,
                })
            }
        }

        pub(super) fn sync(&mut self, _shared: &Shared) {
            // SAFETY: the stream is live while `self` is.
            unsafe { FSEventStreamFlushSync(self.stream.0) };
        }
    }

    impl Drop for Imp {
        fn drop(&mut self) {
            // SAFETY: stop, invalidate and release the stream this owns.
            unsafe {
                FSEventStreamStop(self.stream.0);
                FSEventStreamInvalidate(self.stream.0);
                FSEventStreamRelease(self.stream.0);
                CFRunLoopStop(self.run_loop.0);
                drop(Weak::from_raw(self.weak));
            }
        }
    }
}

#[cfg(windows)]
mod imp {
    //! `ReadDirectoryChangesW`, one overlapped request per root, whole
    //! subtree. Nothing waits: `sync` asks whether the request has
    //! completed (the filesystem completes it while the write that caused
    //! it is still returning, so a write that is done has been reported),
    //! reads what it carries and asks again. A buffer overflow (zero bytes
    //! returned) breaks the watch.
    use super::Shared;
    use std::ffi::c_void;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::PathBuf;
    use std::sync::Arc;
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_IO_INCOMPLETE, GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, ReadDirectoryChangesW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED,
        FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_CREATION,
        FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
        FILE_NOTIFY_CHANGE_SECURITY, FILE_NOTIFY_CHANGE_SIZE, FILE_NOTIFY_INFORMATION,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

    const FILTER: u32 = FILE_NOTIFY_CHANGE_FILE_NAME
        | FILE_NOTIFY_CHANGE_DIR_NAME
        | FILE_NOTIFY_CHANGE_ATTRIBUTES
        | FILE_NOTIFY_CHANGE_SIZE
        | FILE_NOTIFY_CHANGE_LAST_WRITE
        | FILE_NOTIFY_CHANGE_CREATION
        | FILE_NOTIFY_CHANGE_SECURITY;

    struct Dir {
        root: PathBuf,
        handle: HANDLE,
        ov: Box<OVERLAPPED>,
        buf: Box<[u32]>,
    }

    pub(super) struct Imp {
        dirs: Vec<Dir>,
    }

    // SAFETY: the handles and buffers belong to this value alone and are
    // only touched through `&mut self`.
    unsafe impl Send for Imp {}

    impl Dir {
        fn ask(&mut self) -> bool {
            // SAFETY: the buffer and OVERLAPPED are boxed and outlive the
            // request (`Drop` cancels it and waits before freeing them).
            unsafe {
                *self.ov = std::mem::zeroed();
                ReadDirectoryChangesW(
                    self.handle,
                    self.buf.as_mut_ptr().cast::<c_void>(),
                    (self.buf.len() * 4) as u32,
                    1,
                    FILTER,
                    std::ptr::null_mut(),
                    &mut *self.ov,
                    None,
                ) != 0
            }
        }
    }

    impl Imp {
        pub(super) fn arm(shared: &Arc<Shared>, _cap: usize) -> Option<Imp> {
            let mut dirs = Vec::new();
            for r in &shared.filter.roots {
                let wide: Vec<u16> = r.canon.as_os_str().encode_wide().chain([0]).collect();
                // SAFETY: `wide` is NUL-terminated and lives through the call.
                let handle = unsafe {
                    CreateFileW(
                        wide.as_ptr(),
                        FILE_LIST_DIRECTORY | GENERIC_READ,
                        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                        std::ptr::null(),
                        OPEN_EXISTING,
                        FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                        std::ptr::null_mut(),
                    )
                };
                if handle == INVALID_HANDLE_VALUE {
                    return None;
                }
                // SAFETY: an all-zero OVERLAPPED is its documented initial state.
                let ov: Box<OVERLAPPED> = Box::new(unsafe { std::mem::zeroed() });
                let mut d = Dir {
                    root: r.canon.clone(),
                    handle,
                    ov,
                    buf: vec![0u32; 1 << 14].into_boxed_slice(),
                };
                if !d.ask() {
                    // SAFETY: the handle was just opened here.
                    unsafe { CloseHandle(handle) };
                    return None;
                }
                dirs.push(d);
            }
            Some(Imp { dirs })
        }

        pub(super) fn sync(&mut self, shared: &Shared) {
            for d in &mut self.dirs {
                loop {
                    let mut bytes = 0u32;
                    // SAFETY: `ov` is the request made in `ask`; no wait.
                    let done = unsafe { GetOverlappedResult(d.handle, &*d.ov, &mut bytes, 0) };
                    if done == 0 {
                        // Still pending is the quiet answer; anything else
                        // (a cancelled or failed request) is doubt.
                        let err = std::io::Error::last_os_error().raw_os_error();
                        if err != Some(ERROR_IO_INCOMPLETE as i32) {
                            shared.mark_broken();
                        }
                        break;
                    }
                    if bytes == 0 {
                        shared.mark_broken();
                        break;
                    }
                    let base = d.buf.as_ptr().cast::<u8>();
                    let mut off = 0usize;
                    loop {
                        // SAFETY: the kernel wrote a chain of
                        // FILE_NOTIFY_INFORMATION records in `bytes` bytes.
                        let (next, name) = unsafe {
                            let info = base.add(off).cast::<FILE_NOTIFY_INFORMATION>();
                            let n = (*info).FileNameLength as usize / 2;
                            let name = std::slice::from_raw_parts((*info).FileName.as_ptr(), n);
                            (
                                (*info).NextEntryOffset as usize,
                                std::ffi::OsString::from_wide(name),
                            )
                        };
                        shared.note(d.root.join(name));
                        if next == 0 || off + next >= bytes as usize {
                            break;
                        }
                        off += next;
                    }
                    if !d.ask() {
                        shared.mark_broken();
                        break;
                    }
                }
            }
        }
    }

    impl Drop for Imp {
        fn drop(&mut self) {
            for d in &mut self.dirs {
                let mut bytes = 0u32;
                // SAFETY: cancel the pending request and wait for it so the
                // buffer is not freed under the kernel; then close.
                unsafe {
                    CancelIoEx(d.handle, &*d.ov);
                    GetOverlappedResult(d.handle, &*d.ov, &mut bytes, 1);
                    CloseHandle(d.handle);
                }
            }
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod imp {
    use super::Shared;
    use std::sync::Arc;

    /// No change notifications on this platform: hops walk as before.
    pub(super) struct Imp;

    impl Imp {
        pub(super) fn arm(_shared: &Arc<Shared>, _cap: usize) -> Option<Imp> {
            None
        }
        pub(super) fn sync(&mut self, _shared: &Shared) {}
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::fs;

    /// `begin` keeps one watch for the process: tests that use it take turns.
    fn one_at_a_time() -> MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        locked(&LOCK)
    }

    fn tree(n: usize) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for i in 0..n {
            fs::create_dir_all(d.path().join(format!("d{}", i % 4))).unwrap();
            fs::write(d.path().join(format!("d{}/f{i}.txt", i % 4)), "x\n").unwrap();
        }
        d
    }

    fn roots(d: &tempfile::TempDir) -> Vec<PathBuf> {
        vec![fs::canonicalize(d.path()).unwrap()]
    }

    #[test]
    fn a_write_is_seen_and_a_quiet_tree_stays_quiet() {
        let d = tree(8);
        let r = roots(&d);
        let w = arm(&r, None, 1000).expect("inotify");
        let at = w.sync().unwrap();
        assert_eq!(w.sync(), Some(at), "nothing happened");
        fs::write(r[0].join("d1/f1.txt"), "changed\n").unwrap();
        let after = w.sync().unwrap();
        assert!(after > at, "a write moved the log");
        assert!(locked(&w.log).dirty.contains_key(&r[0].join("d1/f1.txt")));
        // A new file, a removed one and a rename are seen too.
        fs::write(r[0].join("d2/new.txt"), "n").unwrap();
        fs::remove_file(r[0].join("d3/f3.txt")).unwrap();
        fs::rename(r[0].join("d0/f0.txt"), r[0].join("d0/g0.txt")).unwrap();
        assert!(w.sync().unwrap() > after);
        let log = locked(&w.log);
        for p in ["d2/new.txt", "d3/f3.txt", "d0/f0.txt", "d0/g0.txt"] {
            assert!(log.dirty.contains_key(&r[0].join(p)), "{p}");
        }
    }

    #[test]
    fn a_directory_made_or_removed_breaks_the_watch() {
        let d = tree(4);
        let r = roots(&d);
        let w = arm(&r, None, 1000).unwrap();
        assert!(w.sync().is_some());
        fs::create_dir(r[0].join("fresh")).unwrap();
        assert_eq!(w.sync(), None, "a directory no watch has seen is doubt");
    }

    #[test]
    fn what_the_scan_leaves_out_is_left_out_here() {
        let d = tree(4);
        let r = roots(&d);
        fs::create_dir_all(r[0].join(".git")).unwrap();
        fs::write(r[0].join("debug.log"), "").unwrap();
        let skip = super::super::tin::skip_set(&["*.log".to_string()]);
        let w = arm(&r, skip.as_ref(), 1000).unwrap();
        let at = w.sync().unwrap();
        fs::write(r[0].join(".git/index"), "i").unwrap();
        fs::write(r[0].join("debug.log"), "more").unwrap();
        assert_eq!(w.sync(), Some(at), "ignored paths do not count");
        fs::write(r[0].join("d0/f0.txt"), "real").unwrap();
        assert!(w.sync().unwrap() > at);
    }

    #[test]
    fn a_link_that_leads_out_of_the_tree_is_doubt_at_arming() {
        let d = tree(4);
        let elsewhere = tempfile::tempdir().unwrap();
        fs::write(elsewhere.path().join("t.txt"), "t").unwrap();
        let r = roots(&d);
        std::os::unix::fs::symlink(elsewhere.path().join("t.txt"), r[0].join("out")).unwrap();
        assert!(arm(&r, None, 1000).is_none());
        // One that stays inside is fine: the write lands where the watch is.
        fs::remove_file(r[0].join("out")).unwrap();
        std::os::unix::fs::symlink(r[0].join("d0/f0.txt"), r[0].join("in")).unwrap();
        assert!(arm(&r, None, 1000).is_some());
    }

    #[test]
    fn hop_watch_falls_back_on_overflow() {
        let _one = one_at_a_time();
        let limit = fs::read_to_string("/proc/sys/fs/inotify/max_queued_events")
            .ok()
            .and_then(|t| t.trim().parse::<usize>().ok())
            .unwrap_or(16384);
        let d = tree(8);
        let r = roots(&d);
        let first = begin(&r, "", None, 1_000_000);
        let token = first.token.expect("inotify");
        // The scan this hop ended with is kept, and is good while nothing moves.
        let scans: Vec<Scan> = r
            .iter()
            .map(|p| super::super::scan::scan(p, 64, 1_000_000, None))
            .collect();
        keep_scans(&token, token.now().unwrap(), scans);
        let again = begin(&r, "", None, 1_000_000);
        assert!(
            again.scans.is_some(),
            "nothing moved: the kept scans stand in"
        );
        let token = again.token.unwrap();
        keep_scans(
            &token,
            token.now().unwrap(),
            r.iter()
                .map(|p| super::super::scan::scan(p, 64, 1_000_000, None))
                .collect(),
        );
        // More events than the kernel will queue, none of them read.
        if limit > 200_000 {
            return;
        }
        for i in 0..(limit / 2 + 2_000) {
            fs::write(r[0].join(format!("d0/burst{i}.txt")), "").unwrap();
        }
        assert!(
            token.now().is_none(),
            "the queue overflowed: the watch is in doubt"
        );
        assert!(!token.quiet());
        let next = begin(&r, "", None, 1_000_000);
        assert!(
            next.scans.is_none(),
            "after an overflow the kept scans are not used: the walk is done"
        );
        assert!(next.trusted.is_none());
        let token = next.token.expect("armed again");
        assert!(token.quiet(), "the new watch starts clean");
    }

    #[test]
    fn kept_scans_are_used_only_while_the_log_has_not_moved() {
        let _one = one_at_a_time();
        let d = tree(8);
        let r = roots(&d);
        let first = begin(&r, "", None, 1_000_000);
        let token = first.token.expect("inotify");
        let take = || -> Vec<Scan> {
            r.iter()
                .map(|p| super::super::scan::scan(p, 64, 1_000_000, None))
                .collect()
        };
        keep_scans(&token, token.now().unwrap(), take());
        fs::write(r[0].join("d1/f1.txt"), "moved\n").unwrap();
        let next = begin(&r, "", None, 1_000_000);
        assert!(next.scans.is_none(), "a write since: the walk is done");
        // Another pack or other skip globs is another watch.
        let token = next.token.unwrap();
        keep_scans(&token, token.now().unwrap(), take());
        let other = begin(&r, "skip-this", None, 1_000_000);
        assert!(other.scans.is_none());
    }

    #[test]
    fn marks_vouch_for_a_cache_entry_until_its_path_is_touched() {
        let _one = one_at_a_time();
        let d = tree(8);
        let r = roots(&d);
        let first = begin(&r, "", None, 1_000_000);
        let token = first.token.unwrap();
        let a = r[0].join("d1/f1.txt");
        let b = r[0].join("d2/f2.txt");
        keep_marks(&token, 77, [a.as_path(), b.as_path()].into_iter());
        let second = begin(&r, "", None, 1_000_000);
        let trusted = second.trusted.expect("marks kept");
        assert!(trusted.untouched(&a, 77));
        assert!(
            !trusted.untouched(&a, 78),
            "another cache file is not this one"
        );
        assert!(
            !trusted.untouched(&r[0].join("d0/f0.txt"), 77),
            "never marked"
        );
        fs::write(&a, "edit\n").unwrap();
        assert!(trusted.shared.sync().is_some());
        assert!(!trusted.untouched(&a, 77), "touched since the mark");
        assert!(trusted.untouched(&b, 77), "its neighbor is not");
    }
}
