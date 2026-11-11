// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The box: a hop run over a copy-on-write layer on the pack, so a failing
//! hop's changes never land. On Linux the hop runs in its own user and
//! mount namespaces with each pack root mounted as overlayfs over itself
//! (the real root the read-only lower layer, an upper directory in the run
//! directory taking every write); a hop that succeeds has its upper layer
//! applied to the real pack through the journal, as native burns, so the
//! run is unburnable; a hop that fails has it deleted, and nothing was ever
//! written to the pack. Where the box cannot be made (no user namespaces,
//! a kernel before 5.11, another operating system), the hop runs on the tin
//! as usual and the plan says so.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Where a boxed hop's layers live: one directory per hop under the run.
pub fn layers_dir(run_dir: &Path, seq: u64) -> PathBuf {
    run_dir.join("box").join(seq.to_string())
}

/// The layers of one hop: per root, an upper and a work directory.
#[derive(Debug)]
pub struct Layers {
    pub dir: PathBuf,
    /// (root, upper, work), outer roots first.
    pub roots: Vec<(PathBuf, PathBuf, PathBuf)>,
}

/// Make the directories a boxed hop needs. Outer roots come first so an
/// overlay over a parent is mounted before one over a child inside it.
pub fn prepare(run_dir: &Path, seq: u64, roots: &[PathBuf]) -> io::Result<Layers> {
    let dir = layers_dir(run_dir, seq);
    // The run may leave layers from here on: say so in the run index first,
    // so the next start's sweep finds it without looking at every run.
    if let Some(id) = run_dir.file_name().and_then(|n| n.to_str()) {
        super::runs_index::mark_boxed(id, true);
    }
    let mut ordered: Vec<PathBuf> = roots.to_vec();
    ordered.sort_by_key(|r| r.components().count());
    let mut out = Vec::new();
    for (i, root) in ordered.iter().enumerate() {
        let upper = dir.join(i.to_string()).join("upper");
        let work = dir.join(i.to_string()).join("work");
        fs::create_dir_all(&upper)?;
        fs::create_dir_all(&work)?;
        out.push((root.clone(), upper, work));
    }
    Ok(Layers { dir, roots: out })
}

/// Why the box could not be made, as the child wrote it before giving up.
pub fn why(layers: &Layers) -> Option<String> {
    fs::read_to_string(layers.dir.join("why"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Throw the layers away: nothing of the hop lands. A contained hop's
/// scratch folder (its home layer, its temp) goes with them.
pub fn discard(layers: &Layers) {
    let _ = fs::remove_file(kept_record(&layers.dir));
    #[cfg(target_os = "linux")]
    if let Some(scratch) = Contain::scratch_of(&layers.dir) {
        let _ = remove_tree_force(&scratch);
    }
    let _ = remove_tree_force(&layers.dir);
    // The `box` directory itself, once no hop's layers are left in it.
    if let Some(parent) = layers.dir.parent() {
        let _ = fs::remove_dir(parent);
    }
}

/// Remove a tree the box left, whatever its modes. overlayfs makes the
/// inner `work` directory of a work layer unreadable (mode 0), where a
/// plain `remove_dir_all` stops; the modes are opened first and the tree
/// goes whole, so a run's record can be pruned after a boxed hop.
pub fn remove_tree_force(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => {
            #[cfg(unix)]
            {
                let _ = &e;
                open_modes(path);
                fs::remove_dir_all(path)
            }
            #[cfg(not(unix))]
            {
                Err(e)
            }
        }
    }
}

#[cfg(unix)]
fn open_modes(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                open_modes(&e.path());
            }
        }
    }
}

/// Can this platform make a box at all? Linux, with a kernel that lets an
/// unprivileged user namespace mount overlayfs (5.11 and later). The real
/// answer comes when the mount is tried; this only rules out what cannot
/// work, so the plan can say so before the run.
pub fn possible() -> Result<(), String> {
    // Test hook: the fallback, on a machine where the box would work.
    if std::env::var_os("CIG_INTERNAL_NO_BOX").is_some() {
        return Err("CIG_INTERNAL_NO_BOX is set".to_string());
    }
    #[cfg(target_os = "linux")]
    {
        if !(fs::read_to_string("/proc/filesystems")
            .map(|t| t.lines().any(|l| l.trim_end().ends_with("overlay")))
            .unwrap_or(false)
            || Path::new("/sys/module/overlay").exists())
        {
            return Err("this kernel has no overlayfs".to_string());
        }
        // The layers live under the state directory, and overlayfs refuses
        // an upper directory that sits on an overlay: a container's layer,
        // or another box. The box does not nest, and says so before the run.
        let home = super::runs::home_dir();
        if let Some(at) = overlay_mount_over(&home) {
            return Err(format!(
                "the layers would sit on an overlay filesystem ({} is on one, mounted at {}: a container's layer, or another box), which overlayfs refuses as an upper directory; a CIGSCRIPT_HOME on a plain filesystem lets the box be made",
                home.display(),
                at.display()
            ));
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err("the box is made with overlayfs in a user namespace, which only Linux has".to_string())
    }
}

/// The mount point of the overlay filesystem `path` sits on, if any: the
/// longest mount point above it in `/proc/self/mountinfo` whose type is
/// `overlay`.
#[cfg(target_os = "linux")]
fn overlay_mount_over(path: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string("/proc/self/mountinfo").ok()?;
    let mut best: Option<(PathBuf, String)> = None;
    for line in text.lines() {
        // id parent major:minor root mount_point options ... - fstype source superopts
        let (left, right) = line.split_once(" - ")?;
        let point = left.split(' ').nth(4)?;
        let fstype = right.split(' ').next()?;
        let point = PathBuf::from(
            point
                .replace("\\040", " ")
                .replace("\\011", "\t")
                .replace("\\134", "\\"),
        );
        if !path.starts_with(&point) {
            continue;
        }
        let deeper = best
            .as_ref()
            .map(|(b, _)| point.components().count() > b.components().count())
            .unwrap_or(true);
        if deeper {
            best = Some((point, fstype.to_string()));
        }
    }
    best.filter(|(_, t)| t == "overlay").map(|(p, _)| p)
}

/// What the child does before it becomes the hop's program: new user and
/// mount namespaces, the user mapped to itself, every root mounted as an
/// overlay over itself, then into the working directory again (the
/// directory it stood in is the real root; the mount hid it). Only async-
/// signal-safe calls, since this runs between fork and exec. On any failure
/// the reason is written to `why` and the exec is refused, so the parent
/// runs the hop again without the box.
/// What a hop's `box` asks for beyond the overlay: its own PID namespace
/// (on by default), and the network it gets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoxSpec {
    pub pidns: bool,
    pub network: Network,
    pub allow_hosts: Vec<String>,
    pub apply: Apply,
    /// Everything the hop can write is a layer cig reads afterwards (the
    /// pack's, the home's, a temp of its own); the rest of the machine is
    /// read-only to it, and it opens no Unix socket (`contain`).
    pub contain: bool,
    /// What the hop is expected to write: globs under the pack root, or
    /// starting `~/` for the home. A change matching none is reported
    /// (`outside`) and, under `contain`, holds the layer back.
    pub expect: Vec<String>,
    /// The script reads the layer, `outside`, `refused` and `changed` itself
    /// and says what they mean: cig's own lines about the layer (E780,
    /// E781, applied, dropped) are left out. Nothing else changes.
    pub quiet: bool,
    /// The watch (`watch: true`): what the hop opens for writing, connects
    /// to and runs, as events (`super::watch`). A contained hop always has
    /// the part that gives the io_uring refusal its event (`E776`).
    pub watch: super::watch::Mode,
    /// Credential paths (`decoys:`) the box replaces with realistic fakes,
    /// each read seen by the watch and recorded as `E773`. Resolved
    /// against the caller's home; `~/` is expanded. A path that resolves
    /// outside the pack roots and the home is refused, and symlinks are
    /// not followed.
    pub decoys: Vec<String>,
    /// Programs that may read their one credential (`pass_reads:`); the
    /// watch supervisor answers an open of the path from the named
    /// program with the real file and from any other with the decoy,
    /// recording `E775`. The program is matched by `/proc/<pid>/exe` at
    /// the time of the call. Fail closed: an unresolvable exe, or the
    /// kernel refusing the fd injection, gives the decoy and the event.
    pub pass_reads: Vec<(String, String)>,
}

impl Default for BoxSpec {
    fn default() -> Self {
        BoxSpec {
            pidns: true,
            network: Network::On,
            allow_hosts: Vec::new(),
            apply: Apply::OnSuccess,
            contain: false,
            expect: Vec::new(),
            quiet: false,
            watch: super::watch::Mode::Off,
            decoys: Vec::new(),
            pass_reads: Vec::new(),
        }
    }
}

impl BoxSpec {
    /// The box's terms in a few words, for the plan and the record (#106):
    /// the network always, the rest where it is not the default.
    pub fn terms(&self) -> Vec<String> {
        let mut t = vec![format!(
            "network {}",
            match self.network {
                Network::On => "on",
                Network::Off => "off",
                Network::Registry => "registry",
            }
        )];
        if !self.allow_hosts.is_empty() {
            t.push(format!("allow {}", self.allow_hosts.join(" ")));
        }
        if self.contain {
            t.push("contain".to_string());
        }
        match self.apply {
            Apply::OnSuccess => {}
            Apply::Ask => t.push("apply ask".to_string()),
            Apply::Never => t.push("apply never".to_string()),
        }
        match self.watch {
            super::watch::Mode::Calls => t.push("watched".to_string()),
            super::watch::Mode::Reads => t.push("watched reads".to_string()),
            _ => {}
        }
        if !self.pidns {
            t.push("pid shared".to_string());
        }
        if !self.expect.is_empty() {
            t.push(format!("expect {}", self.expect.join(" ")));
        }
        if !self.decoys.is_empty() {
            t.push(format!("decoys {}", self.decoys.len()));
        }
        if !self.pass_reads.is_empty() {
            t.push(format!("pass_reads {}", self.pass_reads.len()));
        }
        t
    }
}

/// The network a boxed hop gets: the machine's (`on`), none (`off`: a
/// namespace with only `lo`), or the registry alone (`registry`: the same
/// namespace, with one way out through cig's CONNECT proxy, which allows
/// `allow_hosts` and refuses and records the rest).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Network {
    On,
    Off,
    Registry,
}

/// What becomes of a boxed hop's layer: applied when the hop succeeds (the
/// default), applied after a yes at the terminal, or kept and not applied
/// (`cig box apply` does it later).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Apply {
    OnSuccess,
    Ask,
    Never,
}

/// The port inside a boxed hop's network namespace where the forwarder
/// listens (the namespace is the hop's own, so nothing else can hold it).
pub const PROXY_PORT: u16 = 38080;

/// Where the proxy's Unix socket for a hop lives: under the run's `proxy`,
/// a folder no hop may write (the guard leaves only `hops` and the inbox
/// writable, and the box binds this one read-only besides), so a hop cannot
/// put a link in the socket's place for the forwarder to follow; or, when
/// that path is too long for a socket address, under a private folder in
/// the temporary directory, bound read-only the same way. A socket on a
/// read-only mount still takes connections.
pub fn proxy_sock_path(run_dir: &Path, seq: u64) -> PathBuf {
    let dir = run_dir.join("proxy");
    let under = dir.join(format!("{seq}.sock"));
    if under.as_os_str().len() < 100 {
        let _ = fs::create_dir_all(&dir);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
        }
        return under;
    }
    let dir =
        std::env::temp_dir().join(format!("cig-{}-{}", dir_name(run_dir), std::process::id()));
    let _ = fs::create_dir_all(&dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    }
    dir.join(format!("{seq}.sock"))
}

/// Where a hop's events go (what it connected to, what was refused):
/// under the run, outside the folders the guard leaves writable.
pub fn events_path(run_dir: &Path, seq: u64) -> PathBuf {
    run_dir.join("events").join(format!("{seq}.jsonl"))
}

/// The hop's sequence number from its layers directory (`box/<seq>`).
pub fn seq_of(layers: &Layers) -> u64 {
    dir_name(&layers.dir).parse().unwrap_or(0)
}

/// A layer kept rather than applied (`apply: never`, or `ask` declined or
/// unattended): where it is and what it holds.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Kept {
    pub run: String,
    pub seq: u64,
    pub label: String,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    pub roots: Vec<PathBuf>,
    #[serde(default)]
    pub touches_sealed: Vec<String>,
    pub written: usize,
    pub removed: usize,
    pub at: String,
    /// `contain`: the scratch folder holding the hop's home layer, removed
    /// with the layer (the home layer itself is never applied).
    #[serde(default)]
    pub scratch: Option<PathBuf>,
    /// What the layer held when it was judged (`layer_digest`): applying it
    /// later checks it still holds exactly that.
    #[serde(default)]
    pub digest: Option<String>,
    /// What a contained hop's home layer held when it was judged
    /// (`home_digest`): applying some of it later (`proc.apply_layer(layer,
    /// {home: [...]})`) checks it still holds exactly that.
    #[serde(default)]
    pub home_digest: Option<String>,
    #[serde(skip)]
    pub dir: PathBuf,
}

/// Where a kept layer's record lives: `kept/<seq>.json` under the run, not
/// beside the layer. The layers directory is the hop's own while it runs, so
/// a record there could be planted (a "kept" layer naming roots of the hop's
/// choosing) or turned into a link that cig's write would follow; under the
/// guard `kept/` is read-only to every hop.
pub fn kept_record(layers_dir: &Path) -> PathBuf {
    let run_dir = layers_dir
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| layers_dir.to_path_buf());
    run_dir
        .join("kept")
        .join(format!("{}.json", dir_name(layers_dir)))
}

/// Count what a layer would change: paths written (made or replaced) and
/// removed.
pub fn summarize(layers: &Layers) -> io::Result<(usize, usize)> {
    let (mut written, mut removed) = (0, 0);
    for (root, upper, _) in &layers.roots {
        for c in changes(root, upper)? {
            match c {
                Change::Removed { .. } => removed += 1,
                Change::Dir { .. } => {}
                _ => written += 1,
            }
        }
    }
    Ok((written, removed))
}

/// Keep a layer: write its record beside it, so `cig box apply` can apply
/// it later and `cig box list` can name it.
pub fn keep(
    layers: &Layers,
    label: &str,
    cwd: Option<&Path>,
    touches_sealed: &[String],
    scratch: Option<&Path>,
) -> io::Result<Kept> {
    let (written, removed) = summarize(layers)?;
    // The overlays' work folders are needed only while they are mounted (the
    // hop's namespace is gone now), and overlayfs leaves one unreadable (mode
    // 0): a kept layer drops them, so whatever reads the state directory (a
    // backup, rsync, a tool walking the tin) can read all of it.
    for (_, _, work) in &layers.roots {
        let _ = remove_tree_force(work);
    }
    if let Some(s) = scratch {
        let _ = remove_tree_force(&s.join("home-work"));
    }
    let kept = Kept {
        run: dir_name(&Plan::Boxed(layers).run_dir()),
        seq: seq_of(layers),
        label: label.to_string(),
        cwd: cwd.map(Path::to_path_buf),
        roots: layers.roots.iter().map(|(r, _, _)| r.clone()).collect(),
        touches_sealed: touches_sealed.to_vec(),
        written,
        removed,
        at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        scratch: scratch.map(Path::to_path_buf),
        digest: Some(layer_digest(layers)?),
        home_digest: home_digest(&layers.dir)?,
        dir: layers.dir.clone(),
    };
    let text = serde_json::to_string_pretty(&kept).map_err(io::Error::other)?;
    let record = kept_record(&layers.dir);
    let dir = record.parent().map(Path::to_path_buf).unwrap_or_default();
    fs::create_dir_all(&dir)?;
    // Written beside its final name and renamed in: a link planted at the
    // temporary name is replaced by the rename, never written through.
    let tmp = dir.join(format!(".{}.tmp", dir_name(&layers.dir)));
    let _ = fs::remove_file(&tmp);
    {
        use std::io::Write;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(text.as_bytes())?;
    }
    fs::rename(&tmp, &record)?;
    Ok(kept)
}

/// Is this layers directory a kept layer?
pub fn is_kept(dir: &Path) -> bool {
    kept_record(dir).is_file()
}

/// Every kept layer under every run, oldest first.
pub fn kept_layers() -> Vec<Kept> {
    let mut out = Vec::new();
    let Ok(runs) = fs::read_dir(super::runs::runs_dir()) else {
        return out;
    };
    let mut run_dirs: Vec<PathBuf> = runs.flatten().map(|e| e.path()).collect();
    run_dirs.sort();
    for run in run_dirs {
        let Ok(seqs) = fs::read_dir(run.join("box")) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = seqs.flatten().map(|e| e.path()).collect();
        dirs.sort_by_key(|d| dir_name(d).parse::<u64>().unwrap_or(0));
        for dir in dirs {
            if let Ok(text) = fs::read_to_string(kept_record(&dir)) {
                if let Ok(mut k) = serde_json::from_str::<Kept>(&text) {
                    // The record names its own layer; one that names another
                    // run's or another hop's is not this layer's record.
                    if k.run == dir_name(&run) && k.seq.to_string() == dir_name(&dir) {
                        k.dir = dir.clone();
                        out.push(k);
                    }
                }
            }
        }
    }
    out
}

/// The layers of a kept layer, as `box_apply` wants them.
pub fn layers_of(kept: &Kept) -> Layers {
    Layers {
        dir: kept.dir.clone(),
        roots: kept
            .roots
            .iter()
            .enumerate()
            .map(|(i, r)| {
                (
                    r.clone(),
                    kept.dir.join(i.to_string()).join("upper"),
                    kept.dir.join(i.to_string()).join("work"),
                )
            })
            .collect(),
    }
}

/// Remove the layers an interrupted run never got to apply, keeping the
/// ones kept on purpose. Returns how many went.
pub fn drop_unapplied(run_dir: &Path) -> usize {
    let mut n = 0;
    if let Ok(rd) = fs::read_dir(run_dir.join("box")) {
        for e in rd.flatten() {
            let dir = e.path();
            if dir.is_dir() && !is_kept(&dir) {
                let _ = remove_tree_force(&dir);
                n += 1;
            }
        }
    }
    let _ = fs::remove_dir(run_dir.join("box"));
    // A contained hop's scratch folder whose layer is not kept.
    #[cfg(target_os = "linux")]
    sweep_scratch(run_dir);
    // A record whose layer is gone keeps nothing.
    if let Ok(rd) = fs::read_dir(run_dir.join("kept")) {
        for e in rd.flatten() {
            let seq = e
                .path()
                .file_stem()
                .map(|x| x.to_string_lossy().to_string())
                .unwrap_or_default();
            if !run_dir.join("box").join(&seq).is_dir() {
                let _ = fs::remove_file(e.path());
            }
        }
    }
    let _ = fs::remove_dir(run_dir.join("kept"));
    n
}

/// An environment for an exec in a child that may not allocate: the
/// strings and the pointer array, both built in the parent.
#[cfg(target_os = "linux")]
pub struct ExecEnv {
    _strings: Vec<std::ffi::CString>,
    ptrs: Vec<*const libc::c_char>,
}

// SAFETY: the pointers point into `_strings`, owned by the same value and
// never changed after it is built (moving the value moves no heap buffer);
// the forked child only reads them, for execve.
#[cfg(target_os = "linux")]
unsafe impl Send for ExecEnv {}
#[cfg(target_os = "linux")]
unsafe impl Sync for ExecEnv {}

#[cfg(target_os = "linux")]
impl ExecEnv {
    /// This process's environment with cig's self-call marker set.
    pub fn marked_self() -> ExecEnv {
        use std::os::unix::ffi::OsStrExt;
        let mut strings: Vec<std::ffi::CString> = std::env::vars_os()
            .filter(|(k, _)| k.as_os_str() != crate::program::SELF_ENV)
            .filter_map(|(k, v)| {
                let mut b = k.as_bytes().to_vec();
                b.push(b'=');
                b.extend_from_slice(v.as_bytes());
                std::ffi::CString::new(b).ok()
            })
            .collect();
        strings.push(
            std::ffi::CString::new(format!("{}=1", crate::program::SELF_ENV)).expect("plain"),
        );
        let mut ptrs: Vec<*const libc::c_char> = strings.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        ExecEnv {
            _strings: strings,
            ptrs,
        }
    }

    /// The `envp` for execve.
    pub fn envp(&self) -> *const *const libc::c_char {
        self.ptrs.as_ptr()
    }
}

#[cfg(target_os = "linux")]
pub struct Setup {
    pub uid_map: Vec<u8>,
    pub gid_map: Vec<u8>,
    /// (mount point, overlay options), each NUL-terminated.
    pub mounts: Vec<(std::ffi::CString, std::ffi::CString)>,
    /// The build and dependency trees hop watching never looks into (`.git`,
    /// `node_modules`, `target`, ...) that are there under a root: bound
    /// through the overlay from the real root, so what a hop writes there
    /// lands as it always did and is never journaled. A tree that is not
    /// there before the hop is made through the box like the rest.
    pub binds: Vec<std::ffi::CString>,
    pub cwd: std::ffi::CString,
    pub why: std::ffi::CString,
    /// The tin guard, when the setting asks for it (`tin_guard` auto or on).
    pub guard: Option<Guard>,
    /// The hop in a PID namespace of its own: a relay in the outer one, an
    /// init of cig's as pid 1 inside, the hop under it.
    pub pidns: bool,
    /// A network namespace of its own, with `lo` alone.
    pub netns: bool,
    /// `/dev/null` over `/etc/resolv.conf` inside: no resolver.
    pub resolv_null: bool,
    /// The folder of the proxy's socket, bound read-only inside, with the
    /// flags its mount must keep on a remount.
    pub sock_dir: Option<(std::ffi::CString, libc::c_ulong)>,
    /// The forwarder to start inside the namespace (`cig internal-forward
    /// <sock> <port>`), as the argv to exec.
    pub forward: Option<[std::ffi::CString; 4]>,
    /// The forwarder's environment: cig's own, marked as a call to itself
    /// (`crate::program::SELF_ENV`), so a program's binary is `cig` for it.
    pub forward_env: Option<ExecEnv>,
    /// `contain`: the home layer, the read-only rest, a /dev of its own,
    /// the hop's syscall filter.
    pub contain: Option<Contain>,
    /// The watch, once the parent has started its supervisor (the caller
    /// sets it between `new` and the fork).
    pub watch: Option<super::watch::WatchChild>,
    /// Decoys and pass_reads as read-only bind mounts: the decoy file
    /// (source, in the run's scratch) shadows the real path in the box's
    /// mount namespace. A path missing at mount time is skipped (nothing
    /// to protect); a bind that fails for any other reason refuses the
    /// hop, the way the covers do. The real file is never bound anywhere
    /// in the box: the supervisor serves pass_reads itself, from outside
    /// the namespace.
    pub decoy_mounts: Vec<DecoyMount>,
}

/// One decoy to bind-mount in the box, with both ends held before the
/// fork so `apply` only reads async-signal-safe pointers.
#[cfg(target_os = "linux")]
#[derive(Clone, Debug)]
pub struct DecoyMount {
    /// The decoy's host path (in the run's scratch).
    pub source: std::ffi::CString,
    /// The real credential's path inside the box (the overlays and
    /// contain's home have already landed by the time of the bind, so
    /// the target here is what the hop sees).
    pub target: std::ffi::CString,
}

/// What `contain` adds to the box, planned in the parent and applied in the
/// child (see BURN.md, "Rehearsing code you did not write").
#[cfg(target_os = "linux")]
pub struct Contain {
    /// The box's scratch folder, outside the home and every pack root:
    /// the home layer's upper and work, and the hop's temp.
    pub scratch: PathBuf,
    /// The home, and the overlay to mount on it, when no pack root holds
    /// the home or lies above it.
    pub home: Option<(std::ffi::CString, std::ffi::CString)>,
    /// What lives under the home and is put back over the home layer, held
    /// open before it and bound again after: the pack roots there, the
    /// state root, mounts that were there. Outermost first.
    pub rebinds: Vec<std::ffi::CString>,
    /// The hop's temp, and whether it goes over /tmp (not when anything cig
    /// needs lives under /tmp).
    pub tmp: std::ffi::CString,
    pub tmp_over: bool,
    /// The key folder (an empty tmpfs over it) and the other ways to the
    /// key (each covered with /dev/null), in the hop's final view.
    pub keys: Option<std::ffi::CString>,
    pub covers: Vec<std::ffi::CString>,
    /// Mount points made writable again after the whole tree went
    /// read-only: the pack roots, the home, the temp (each required), and
    /// /dev and its parts.
    pub writable: Vec<(std::ffi::CString, bool)>,
    /// The hop's syscall filter (classic BPF), installed last in the hop.
    pub filter: Vec<libc::sock_filter>,
    /// io_uring's refusal alone: the watch answers it with an event; where
    /// there is no watch this is installed instead, and the refusal stands.
    pub refusal: Vec<libc::sock_filter>,
}

#[cfg(target_os = "linux")]
impl Contain {
    /// The paths outside the run that the box made for this hop: removed
    /// when the hop's layer goes (`discard`) or is applied.
    pub fn scratch_of(layers_dir: &Path) -> Option<PathBuf> {
        let run = layers_dir.parent()?.parent()?;
        let uid = unsafe { libc::geteuid() };
        let name = format!("cig-box-{uid}-{}-{}", dir_name(run), dir_name(layers_dir));
        scratch_bases()
            .into_iter()
            .map(|b| b.join(&name))
            .find(|p| p.exists())
    }
}

/// Where a contained hop's scratch folder may live: `/var/tmp` (on disk on
/// most machines; a home layer full of package caches should not sit in
/// memory), else the temporary directory.
#[cfg(target_os = "linux")]
fn scratch_bases() -> Vec<PathBuf> {
    let mut out = vec![PathBuf::from("/var/tmp")];
    let t = std::env::temp_dir();
    if !out.contains(&t) {
        out.push(t);
    }
    out
}

/// The real home: `HOME`, else the password database.
#[cfg(target_os = "linux")]
fn real_home() -> Option<PathBuf> {
    let h = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|h| h.is_absolute() && h.is_dir())?;
    Some(fs::canonicalize(&h).unwrap_or(h))
}

/// Mount points under `under` (strictly), from this process's mount table,
/// decoded (`\040` is a space).
#[cfg(target_os = "linux")]
fn mounts_under(under: &Path) -> Vec<PathBuf> {
    let Ok(text) = fs::read_to_string("/proc/self/mountinfo") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(raw) = line.split(' ').nth(4) else {
            continue;
        };
        let mut bytes = Vec::with_capacity(raw.len());
        let b = raw.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'\\' && i + 3 < b.len() {
                if let Ok(n) = u8::from_str_radix(&raw[i + 1..i + 4], 8) {
                    bytes.push(n);
                    i += 4;
                    continue;
                }
            }
            bytes.push(b[i]);
            i += 1;
        }
        let p = PathBuf::from(String::from_utf8_lossy(&bytes).into_owned());
        if p != under && p.starts_with(under) && !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// `mount_setattr(2)` (Linux 5.12): set or clear attributes on a mount, or
/// on a whole tree with `AT_RECURSIVE`, in one call and without walking
/// paths. Async-signal-safe (a raw syscall).
#[cfg(target_os = "linux")]
unsafe fn mount_setattr(path: &std::ffi::CStr, recursive: bool, set: u64, clear: u64) -> bool {
    #[repr(C)]
    struct MountAttr {
        attr_set: u64,
        attr_clr: u64,
        propagation: u64,
        userns_fd: u64,
    }
    const AT_RECURSIVE: libc::c_long = 0x8000;
    let attr = MountAttr {
        attr_set: set,
        attr_clr: clear,
        propagation: 0,
        userns_fd: 0,
    };
    libc::syscall(
        libc::SYS_mount_setattr,
        libc::AT_FDCWD as libc::c_long,
        path.as_ptr(),
        if recursive { AT_RECURSIVE } else { 0 },
        &attr as *const MountAttr,
        std::mem::size_of::<MountAttr>(),
    ) == 0
}

#[cfg(target_os = "linux")]
const MOUNT_ATTR_RDONLY: u64 = 0x1;

/// Plan `contain` for one hop: the scratch folder, the home layer and what
/// is put back over it, the temp, the key folder and covers, the mounts made
/// writable again after the rest goes read-only, and the syscall filter.
#[cfg(target_os = "linux")]
fn plan_contain(
    layers: &Layers,
    cwd: &Path,
    run_dir: &Path,
    sock: Option<&Path>,
) -> io::Result<Contain> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let other = |m: &str| io::Error::other(m.to_string());
    let cstr = |p: &Path| -> io::Result<std::ffi::CString> {
        std::ffi::CString::new(p.as_os_str().as_bytes())
            .map_err(|_| io::Error::other("a path holds a NUL byte"))
    };
    let euid = unsafe { libc::geteuid() };
    if euid == 0 {
        return Err(other("contain runs the hop as you, not as root: a root hop keeps its capabilities in the box and could make the rest writable again"));
    }
    let filter = hop_filter_with(false).map_err(io::Error::other)?;
    let refusal = refusal_filter().map_err(io::Error::other)?;
    let roots: Vec<PathBuf> = layers.roots.iter().map(|(r, _, _)| r.clone()).collect();
    let state =
        std::path::absolute(super::runs::home_dir()).unwrap_or_else(|_| super::runs::home_dir());
    let home = real_home();
    let inside_any = |p: &Path| {
        home.as_ref().map(|h| p.starts_with(h)).unwrap_or(false)
            || roots.iter().any(|r| p.starts_with(r))
    };
    // The scratch folder: outside the home (an overlay's upper may not sit
    // in its lower) and outside every pack root; made fresh, the user's
    // alone, and checked to be so.
    let seq = dir_name(&layers.dir);
    let name = format!("cig-box-{euid}-{}-{seq}", dir_name(run_dir));
    let base = scratch_bases()
        .into_iter()
        .find(|b| b.is_dir() && !inside_any(b))
        .ok_or_else(|| other("no place for the box's scratch folder outside your home and the pack (/var/tmp, or the temporary directory)"))?;
    let scratch = base.join(&name);
    if fs::symlink_metadata(&scratch).is_ok() {
        remove_tree_force(&scratch)?;
    }
    fs::create_dir(&scratch)?;
    fs::set_permissions(&scratch, fs::Permissions::from_mode(0o700))?;
    let meta = fs::symlink_metadata(&scratch)?;
    if !meta.is_dir() || meta.uid() != euid {
        return Err(other("the box's scratch folder is not yours alone"));
    }
    for d in ["home-upper", "home-work", "tmp"] {
        fs::create_dir(scratch.join(d))?;
    }
    // The home layer, unless a pack root holds the home (its writes are in
    // that layer already).
    let home_mount = match &home {
        Some(h) if !roots.iter().any(|r| h.starts_with(r)) => {
            let opts = format!(
                "lowerdir={},upperdir={},workdir={},userxattr",
                escape(h),
                escape(&scratch.join("home-upper")),
                escape(&scratch.join("home-work"))
            );
            Some((
                cstr(h)?,
                std::ffi::CString::new(opts).map_err(|_| other("NUL in options"))?,
            ))
        }
        _ => None,
    };
    // What lives under the home and goes back over its layer: an overlay's
    // lower does not cross mounts, so without this the pack roots there
    // would show their real, unlayered selves and the state root its raw
    // folder. Outermost only.
    let mut rebinds: Vec<PathBuf> = Vec::new();
    if let (Some(h), Some(_)) = (&home, &home_mount) {
        let mut cands: Vec<PathBuf> = roots
            .iter()
            .filter(|r| r.starts_with(h) && *r != h)
            .cloned()
            .collect();
        if state.starts_with(h) && &state != h {
            cands.push(state.clone());
        }
        for m in mounts_under(h) {
            cands.push(m);
        }
        cands.sort_by_key(|p| p.components().count());
        for c in cands {
            if !rebinds.iter().any(|r| c.starts_with(r)) {
                rebinds.push(c);
            }
        }
    }
    if rebinds.len() > MAX_BINDS {
        return Err(other(
            "too many mounts under your home to put back over its layer",
        ));
    }
    // The temp: over /tmp, unless something cig needs lives there (a pack
    // root, the state root, the working directory, the proxy's socket, or
    // your home: a home under /tmp would vanish under a new /tmp, and making
    // it writable again would then find nothing).
    let under_tmp = |p: &Path| p.starts_with("/tmp");
    let tmp_over = !(roots.iter().any(|r| under_tmp(r))
        || under_tmp(&state)
        || under_tmp(&scratch)
        || under_tmp(cwd)
        || home.as_ref().map(|h| under_tmp(h)).unwrap_or(false)
        || sock.map(under_tmp).unwrap_or(false));
    let hidden = |p: &Path| tmp_over && under_tmp(p);
    // The key folder and the other ways to the key, in the final view.
    let keys = super::seal::keys_dir();
    fs::create_dir_all(&keys)?;
    let _ = fs::set_permissions(&keys, fs::Permissions::from_mode(0o700));
    let keys_c = if hidden(&keys) {
        None
    } else {
        Some(cstr(&keys)?)
    };
    let mut covers = Vec::new();
    for p in key_routes(&keys, true) {
        if !hidden(&p) {
            covers.push(cstr(&p)?);
        }
    }
    let tmp = scratch.join("tmp");
    let mut writable: Vec<(std::ffi::CString, bool)> = Vec::new();
    for r in &roots {
        writable.push((cstr(r)?, true));
    }
    if let Some((h, _)) = &home_mount {
        writable.push((h.clone(), true));
    }
    writable.push((
        if tmp_over {
            std::ffi::CString::new("/tmp").expect("plain")
        } else {
            cstr(&tmp)?
        },
        true,
    ));
    for d in DEV_PARTS {
        writable.push((std::ffi::CString::new(*d).expect("plain"), false));
    }
    Ok(Contain {
        scratch,
        home: home_mount,
        rebinds: rebinds.iter().map(|p| cstr(p)).collect::<io::Result<_>>()?,
        tmp: cstr(&tmp)?,
        tmp_over,
        keys: keys_c,
        covers,
        writable,
        filter,
        refusal,
    })
}

/// A contained hop's /dev: these nodes, bound from the machine's, and
/// nothing else (no input devices, cameras, sound or disks).
#[cfg(target_os = "linux")]
const DEV_NODES: &[&str] = &["null", "zero", "full", "random", "urandom", "tty"];

/// The parts of a contained hop's /dev made writable again after the rest
/// goes read-only.
#[cfg(target_os = "linux")]
const DEV_PARTS: &[&str] = &[
    "/dev",
    "/dev/shm",
    "/dev/pts",
    "/dev/null",
    "/dev/zero",
    "/dev/full",
    "/dev/random",
    "/dev/urandom",
    "/dev/tty",
];

/// The contained hop's syscall filter (classic BPF, for seccomp): only
/// this machine's own architecture (no 32-bit or x32 entry, whose numbers
/// differ); `socket` for the internet and netlink alone, so no Unix socket
/// is opened (a session bus, systemd's manager, docker.sock, an agent),
/// while `socketpair`, which child pipes use, stays; no new user namespace
/// (`clone`, `unshare` with `CLONE_NEWUSER`; `clone3`, whose flags a filter
/// cannot read, says it does not exist and the C library falls back); no
/// pushing input into a terminal; and nothing that reaches past the
/// namespaces: io_uring (its operations escape a syscall filter), ptrace,
/// another process's memory or descriptors, the kernel keyrings, BPF,
/// performance counters, userfaultfd, the mount calls.
#[cfg(target_os = "linux")]
pub fn hop_filter() -> Result<Vec<libc::sock_filter>, String> {
    hop_filter_with(true)
}

/// `hop_filter`, with or without the io_uring refusal in it: without, the
/// refusal is `refusal_filter`'s, and the watch gives it an event (`E776`).
#[cfg(target_os = "linux")]
pub fn hop_filter_with(io_uring: bool) -> Result<Vec<libc::sock_filter>, String> {
    #[cfg(target_arch = "x86_64")]
    const ARCH: u32 = 0xC000_003E;
    #[cfg(target_arch = "aarch64")]
    const ARCH: u32 = 0xC000_00B7;
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        return Err(
            "contain needs x86_64 or aarch64; this cig was built for another architecture"
                .to_string(),
        );
    }
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    {
        const LD_W_ABS: u16 = 0x20;
        const JEQ: u16 = 0x15;
        const JSET: u16 = 0x45;
        const RET: u16 = 0x06;
        const ALLOW: u32 = 0x7fff_0000;
        const KILL: u32 = 0x8000_0000;
        let errno = |e: i32| 0x0005_0000u32 | (e as u32 & 0xffff);
        let st = |code: u16, k: u32| libc::sock_filter {
            code,
            jt: 0,
            jf: 0,
            k,
        };
        let jmp = |code: u16, k: u32, jt: u8, jf: u8| libc::sock_filter { code, jt, jf, k };
        const NR: u32 = 0;
        const ARCH_OFF: u32 = 4;
        const ARG0: u32 = 16;
        const ARG1: u32 = 24;
        let mut f = vec![
            st(LD_W_ABS, ARCH_OFF),
            jmp(JEQ, ARCH, 1, 0),
            st(RET, KILL),
            st(LD_W_ABS, NR),
        ];
        #[cfg(target_arch = "x86_64")]
        {
            const JGE: u16 = 0x35;
            // x32: the same numbers with this bit set, a way around the rest.
            f.push(jmp(JGE, 0x4000_0000, 0, 1));
            f.push(st(RET, KILL));
        }
        let mut denied: Vec<libc::c_long> = Vec::new();
        if io_uring {
            denied.extend([
                libc::SYS_io_uring_setup,
                libc::SYS_io_uring_enter,
                libc::SYS_io_uring_register,
            ]);
        }
        denied.extend([
            libc::SYS_ptrace,
            libc::SYS_process_vm_readv,
            libc::SYS_process_vm_writev,
            libc::SYS_pidfd_getfd,
            libc::SYS_keyctl,
            libc::SYS_add_key,
            libc::SYS_request_key,
            libc::SYS_bpf,
            libc::SYS_perf_event_open,
            libc::SYS_userfaultfd,
            libc::SYS_mount,
            libc::SYS_umount2,
            libc::SYS_pivot_root,
            libc::SYS_move_mount,
            libc::SYS_open_tree,
            libc::SYS_fsopen,
            libc::SYS_fsmount,
            libc::SYS_fspick,
            libc::SYS_mount_setattr,
        ]);
        for nr in denied {
            f.push(jmp(JEQ, nr as u32, 0, 1));
            f.push(st(RET, errno(libc::EPERM)));
        }
        f.push(jmp(JEQ, libc::SYS_clone3 as u32, 0, 1));
        f.push(st(RET, errno(libc::ENOSYS)));
        // socket: the internet and netlink only.
        f.push(jmp(JEQ, libc::SYS_socket as u32, 0, 6));
        f.push(st(LD_W_ABS, ARG0));
        f.push(jmp(JEQ, libc::AF_INET as u32, 3, 0));
        f.push(jmp(JEQ, libc::AF_INET6 as u32, 2, 0));
        f.push(jmp(JEQ, libc::AF_NETLINK as u32, 1, 0));
        f.push(st(RET, errno(libc::EACCES)));
        f.push(st(RET, ALLOW));
        // A new user namespace, by clone or unshare.
        for nr in [libc::SYS_clone, libc::SYS_unshare] {
            f.push(jmp(JEQ, nr as u32, 0, 4));
            f.push(st(LD_W_ABS, ARG0));
            f.push(jmp(JSET, libc::CLONE_NEWUSER as u32, 0, 1));
            f.push(st(RET, errno(libc::EPERM)));
            f.push(st(RET, ALLOW));
        }
        // Input pushed into a terminal.
        f.push(jmp(JEQ, libc::SYS_ioctl as u32, 0, 5));
        f.push(st(LD_W_ABS, ARG1));
        f.push(jmp(JEQ, libc::TIOCSTI as u32, 2, 0));
        f.push(jmp(JEQ, 0x541C, 1, 0));
        f.push(st(RET, ALLOW));
        f.push(st(RET, errno(libc::EPERM)));
        f.push(st(RET, ALLOW));
        Ok(f)
    }
}

/// Only io_uring's refusal (`EPERM`), for a contained hop whose watch could
/// not be made: the call stays refused, only its event is missing.
#[cfg(target_os = "linux")]
pub fn refusal_filter() -> Result<Vec<libc::sock_filter>, String> {
    #[cfg(target_arch = "x86_64")]
    const ARCH: u32 = 0xC000_003E;
    #[cfg(target_arch = "aarch64")]
    const ARCH: u32 = 0xC000_00B7;
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        return Err("contain needs x86_64 or aarch64".to_string());
    }
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    {
        let st = |code: u16, k: u32| libc::sock_filter {
            code,
            jt: 0,
            jf: 0,
            k,
        };
        let jeq = |k: u32| libc::sock_filter {
            code: 0x15,
            jt: 0,
            jf: 1,
            k,
        };
        let mut f = vec![
            st(0x20, 4),
            jeq_arch(ARCH),
            st(0x06, 0x8000_0000),
            st(0x20, 0),
        ];
        for nr in [
            libc::SYS_io_uring_setup,
            libc::SYS_io_uring_enter,
            libc::SYS_io_uring_register,
        ] {
            f.push(jeq(nr as u32));
            f.push(st(0x06, 0x0005_0000 | libc::EPERM as u32));
        }
        f.push(st(0x06, 0x7fff_0000));
        Ok(f)
    }
}

#[cfg(target_os = "linux")]
fn jeq_arch(arch: u32) -> libc::sock_filter {
    libc::sock_filter {
        code: 0x15,
        jt: 1,
        jf: 0,
        k: arch,
    }
}

/// The tin read-only to the hop, and the key out of its sight. The state
/// root is bound over itself and remounted read-only, after the subtrees a
/// hop legitimately writes (its own layers, its protocol file) were bound
/// writable underneath, which the non-recursive remount leaves alone; an
/// empty tmpfs goes over the key folder. A guard that cannot be made is
/// noted in `why`, and the hop runs boxed without it (`tin_guard = on`
/// refuses instead).
///
/// From 1.19.0 the guard also hides `tin/` and `runs/` entirely: an empty
/// tmpfs goes over each, and this hop's own burn-protocol journal is bound
/// back into the fresh `runs/` tmpfs so a hop that speaks the protocol can
/// still record what it changed. The child run's inbox is outside `runs/`,
/// so it is still reached unchanged; a cig started inside the hop keeps
/// the inbox as its own state root.
#[cfg(target_os = "linux")]
pub struct Guard {
    pub home: std::ffi::CString,
    /// The flags the state root's mount must keep when it is remounted
    /// read-only (`locked_flags`).
    pub home_flags: libc::c_ulong,
    pub keys: std::ffi::CString,
    pub writable: Vec<std::ffi::CString>,
    pub why: std::ffi::CString,
    /// `tin_guard = on`: a guard that cannot be made refuses the hop.
    pub required: bool,
    /// The inbox: the one writable place under the state directory, and a
    /// cig inside the hop's own state root.
    pub inbox: PathBuf,
    /// `CIG_INTERNAL_NO_GUARD`: behave as if the mounts were refused.
    pub pretend_unavailable: bool,
    /// The other ways to the key, each covered with `/dev/null`: a key file
    /// named outright (`CIG_SEAL_KEY_FILE`) outside the key folder, and,
    /// while the key may be in the OS keyring, the session bus and systemd's
    /// private socket (see `key_routes`).
    pub covers: Vec<std::ffi::CString>,
    /// The session bus is cut: the hop's environment does not name it.
    pub bus_cut: bool,
    /// `tin/` and `runs/` hidden by two empty tmpfs mounts, with this hop's
    /// own journal bound back through so the burn protocol keeps working.
    pub tin_dir: std::ffi::CString,
    pub runs_dir: std::ffi::CString,
    pub hop_journal: std::ffi::CString,
    /// The proxy's socket folder, when `network: registry` named one (the
    /// run's `proxy/` lives under `runs/`, so the fresh tmpfs would cover
    /// it; the real folder is bound back through at the same path, and the
    /// netns block that follows binds it read-only as it always did).
    pub hide_proxy_dir: Option<std::ffi::CString>,
    /// The directories to create inside the fresh `runs/` tmpfs so the
    /// journal and the proxy folder have a path, outermost first.
    pub hide_mkdirs: Vec<std::ffi::CString>,
}

/// What confines a hop: the box (an overlay over each pack root, with the
/// guard), or the guard alone (`tin_guard = on`, for a hop outside the box).
#[derive(Debug)]
pub enum Plan<'a> {
    Boxed(&'a Layers),
    GuardOnly { run_dir: PathBuf, seq: u64 },
}

impl Plan<'_> {
    /// Where this hop's notes go (`why`, `guard-why`): the layers directory,
    /// or a folder of the hop's own under the run's `hops`.
    pub fn base(&self) -> PathBuf {
        match self {
            Plan::Boxed(l) => l.dir.clone(),
            Plan::GuardOnly { run_dir, seq } => run_dir.join("hops").join(format!("{seq}-guard")),
        }
    }

    /// The run this hop belongs to.
    pub fn run_dir(&self) -> PathBuf {
        match self {
            Plan::Boxed(l) => l
                .dir
                .parent()
                .and_then(Path::parent)
                .map(Path::to_path_buf)
                .unwrap_or_else(|| l.dir.clone()),
            Plan::GuardOnly { run_dir, .. } => run_dir.clone(),
        }
    }

    pub fn run_id(&self) -> String {
        dir_name(&self.run_dir())
    }

    /// The hop's sequence number in the run, carried in the layer directory
    /// for a boxed hop and explicit for an unboxed one under `tin_guard = on`.
    pub fn seq(&self) -> u64 {
        match self {
            Plan::Boxed(l) => seq_of(l),
            Plan::GuardOnly { seq, .. } => *seq,
        }
    }
}

/// What the guard hides from a boxed or guarded hop and what it keeps
/// writable: the two folders covered with an empty tmpfs (`tin/` and
/// `runs/`), the directories to create inside the fresh `runs/` tmpfs so
/// the hop's burn-protocol journal has a path again (outermost first), and
/// the one file under `runs/` that stays writable (that journal, the file
/// `CIG_JOURNAL` names).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HidePlan {
    pub tin_dir: PathBuf,
    pub runs_dir: PathBuf,
    pub mkdirs: Vec<PathBuf>,
    pub hop_journal: PathBuf,
}

/// Plan what a hop sees when the guard hides the tin: an empty `tin/`, an
/// empty `runs/` holding only the path to this hop's own journal, and the
/// file bound back through the tmpfs for the burn protocol to write. The
/// child run's inbox lives outside `runs/`, so it is not touched here and
/// the guard binds it writable as before.
pub fn hide_plan(home: &Path, run_dir: &Path, seq: u64) -> HidePlan {
    let tin_dir = home.join("tin");
    let runs_dir = home.join("runs");
    let run = runs_dir.join(dir_name(run_dir));
    let hops = run.join("hops");
    let hop_journal = hops.join(format!("{seq}.jsonl"));
    HidePlan {
        tin_dir,
        runs_dir,
        mkdirs: vec![run, hops],
        hop_journal,
    }
}

/// Where the guard writes why it could not be made.
pub fn guard_why_path(base: &Path) -> PathBuf {
    base.join("guard-why")
}

/// What a hop's setup wrote when it could not be made at all (`why`).
pub fn why_at(base: &Path) -> Option<String> {
    fs::read_to_string(base.join("why"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The inbox of a run: under the state directory, named by the run, the
/// one place a guarded hop may write. A cig started inside the hop has
/// `home` under it as its state root and `keys` as its key folder.
pub fn inbox_dir(run_dir: &Path) -> PathBuf {
    let home = super::runs::home_dir();
    let home = std::path::absolute(&home).unwrap_or(home);
    home.join("inbox").join(dir_name(run_dir))
}

/// Make a run's inbox, owner-only, with the parent's settings copied in
/// for the cig inside and the key kept in a file of the inbox's own: the
/// hop must not reach the parent's key through the keyring.
pub fn prepare_inbox(run_dir: &Path) -> io::Result<PathBuf> {
    let inbox = inbox_dir(run_dir);
    let home = inbox.join("home");
    let keys = inbox.join("keys");
    for d in [&inbox, &home, &keys] {
        fs::create_dir_all(d)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(d, fs::Permissions::from_mode(0o700));
        }
    }
    let mut text: String = fs::read_to_string(crate::config::path())
        .unwrap_or_default()
        .lines()
        .filter(|l| {
            l.split_once('=')
                .map(|(k, _)| k.trim() != "seal_key")
                .unwrap_or(true)
        })
        .map(|l| format!("{l}\n"))
        .collect();
    text.push_str("seal_key = file\n");
    fs::write(home.join("config"), text)?;
    Ok(inbox)
}

/// Is the session bus cut from guarded hops? While the key may be in the OS
/// keyring (`seal_key` is not `file`): on Linux that keyring is the Secret
/// Service on the session bus, which hands a secret to any process of the
/// user's that asks for it, and systemd's user manager on the same bus would
/// start a process outside the box that could ask.
pub fn bus_cut() -> bool {
    crate::config::Config::load().get("seal_key") != "file"
}

/// The ways to the key a guarded hop must not have, beyond the key folder
/// the tmpfs hides: a key file named outright by `CIG_SEAL_KEY_FILE` that
/// lies outside that folder, and, when `bus_cut`, the session bus (the
/// `path=` of `DBUS_SESSION_BUS_ADDRESS`, `$XDG_RUNTIME_DIR/bus`) and
/// systemd's private socket. Each is real (canonical), present and not a
/// directory; an abstract socket has no path to cover, and is out of reach
/// only in a network namespace of the hop's own.
#[cfg(target_os = "linux")]
pub fn key_routes(keys: &Path, bus_cut: bool) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    let mut add = |p: PathBuf| {
        let Ok(meta) = fs::symlink_metadata(&p) else {
            return;
        };
        let real = fs::canonicalize(&p).unwrap_or(p);
        let is_dir = if meta.file_type().is_symlink() {
            real.is_dir()
        } else {
            meta.is_dir()
        };
        if !is_dir && !found.contains(&real) {
            found.push(real);
        }
    };
    if let Some(f) = std::env::var_os("CIG_SEAL_KEY_FILE") {
        let f = PathBuf::from(f);
        let f = std::path::absolute(&f).unwrap_or(f);
        let keys = fs::canonicalize(keys).unwrap_or_else(|_| keys.to_path_buf());
        let real = fs::canonicalize(&f).unwrap_or_else(|_| f.clone());
        if !real.starts_with(&keys) {
            add(f);
        }
    }
    if bus_cut {
        if let Some(addr) = std::env::var_os("DBUS_SESSION_BUS_ADDRESS") {
            for p in bus_paths(&addr.to_string_lossy()) {
                add(p);
            }
        }
        // The runtime folder named, and the uid's own whether named or not:
        // the bus sits at a fixed path, so a hop needs no variable to find it.
        let mut runtime: Vec<PathBuf> = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .into_iter()
            .collect();
        runtime.push(PathBuf::from(format!("/run/user/{}", unsafe {
            libc::geteuid()
        })));
        for rt in runtime {
            add(rt.join("bus"));
            add(rt.join("systemd").join("private"));
        }
    }
    found
}

/// The socket paths in a D-Bus address (`unix:path=/run/user/1000/bus`,
/// alternatives separated by `;`, values %-escaped).
#[cfg(target_os = "linux")]
fn bus_paths(addr: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for alt in addr.split(';') {
        let Some(("unix", params)) = alt.split_once(':') else {
            continue;
        };
        for kv in params.split(',') {
            if let Some(v) = kv.strip_prefix("path=") {
                out.push(PathBuf::from(unescape_bus(v)));
            }
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn unescape_bus(v: &str) -> String {
    let b = v.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
            if let Some(n) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(n);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What the `tin_guard` setting says: `auto` (the default), `on` or `off`;
/// `CIG_TIN_GUARD=0` is `off` for one run.
pub fn guard_setting() -> String {
    if std::env::var_os("CIG_TIN_GUARD").is_some_and(|v| v == "0") {
        return "off".to_string();
    }
    let s = crate::config::Config::load().get("tin_guard");
    if s == "on" || s == "off" {
        s
    } else {
        "auto".to_string()
    }
}

/// How many trees the child binds at most (a stack array holds their
/// descriptors between fork and exec).
#[cfg(target_os = "linux")]
const MAX_BINDS: usize = 64;

/// A listening TCP socket on `127.0.0.1:port`, close-on-exec, or -1.
///
/// # Safety
///
/// Async-signal-safe (socket, setsockopt, bind, listen, close): it is
/// called between fork and exec.
#[cfg(target_os = "linux")]
unsafe fn listen_loopback(port: u16) -> libc::c_int {
    let s = libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0);
    if s < 0 {
        return -1;
    }
    let one: libc::c_int = 1;
    libc::setsockopt(
        s,
        libc::SOL_SOCKET,
        libc::SO_REUSEADDR,
        &one as *const libc::c_int as *const libc::c_void,
        std::mem::size_of::<libc::c_int>() as libc::socklen_t,
    );
    let mut addr: libc::sockaddr_in = std::mem::zeroed();
    addr.sin_family = libc::AF_INET as libc::sa_family_t;
    addr.sin_port = port.to_be();
    addr.sin_addr.s_addr = u32::from_be_bytes([127, 0, 0, 1]).to_be();
    if libc::bind(
        s,
        &addr as *const libc::sockaddr_in as *const libc::sockaddr,
        std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
    ) != 0
        || libc::listen(s, 128) != 0
    {
        libc::close(s);
        return -1;
    }
    s
}

#[cfg(target_os = "linux")]
impl Setup {
    pub fn new(plan: &Plan, cwd: &Path, spec: &BoxSpec) -> io::Result<Setup> {
        use std::os::unix::ffi::OsStrExt;
        let uid = unsafe { libc::geteuid() };
        let gid = unsafe { libc::getegid() };
        let cstr = |p: &Path| -> io::Result<std::ffi::CString> {
            std::ffi::CString::new(p.as_os_str().as_bytes())
                .map_err(|_| io::Error::other("a path holds a NUL byte"))
        };
        let mut mounts = Vec::new();
        let mut binds = Vec::new();
        let layers = match plan {
            Plan::Boxed(l) => Some(*l),
            Plan::GuardOnly { .. } => None,
        };
        // `contain`: nothing passes through to the real disk, the guard is
        // the contained one below, not the setting's.
        let contained = spec.contain && layers.is_some();
        for (root, upper, work) in layers.map(|l| l.roots.as_slice()).unwrap_or(&[]) {
            let opts = format!(
                "lowerdir={},upperdir={},workdir={},userxattr",
                escape(root),
                escape(upper),
                escape(work)
            );
            mounts.push((
                cstr(root)?,
                std::ffi::CString::new(opts).map_err(|_| io::Error::other("NUL in options"))?,
            ));
            for name in super::tin::SKIPPED_ALWAYS {
                let dir = root.join(name);
                if !contained
                    && fs::symlink_metadata(&dir)
                        .map(|m| m.is_dir())
                        .unwrap_or(false)
                    && binds.len() < MAX_BINDS
                {
                    binds.push(cstr(&dir)?);
                }
            }
        }
        // The guard: the state root, the key folder, and what stays writable.
        let base = plan.base();
        fs::create_dir_all(&base)?;
        let run_dir = plan.run_dir();
        // The proxy socket's folder when `network: registry` asks for one,
        // worked out before the guard so the hide step can plan a path for
        // it inside the fresh `runs/` tmpfs it mounts (`sock_dir` below is
        // computed from the same conditions and binds the folder read-only
        // in the netns block, after the hide).
        let boxed_flag = layers.is_some();
        let netns_flag = boxed_flag && spec.network != Network::On;
        let proxy_dir = if netns_flag && spec.network == Network::Registry {
            let seq = layers.map(seq_of).unwrap_or(0);
            proxy_sock_path(&run_dir, seq)
                .parent()
                .map(Path::to_path_buf)
        } else {
            None
        };
        let guard = if guard_setting() == "off" || contained {
            None
        } else {
            let home = std::path::absolute(super::runs::home_dir())
                .unwrap_or_else(|_| super::runs::home_dir());
            // The key folder is made before the hop runs, so that the empty
            // tmpfs has something to cover: a hop must neither read a key
            // nor plant one for the first seal to pick up.
            let keys = super::seal::keys_dir();
            fs::create_dir_all(&keys)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&keys, fs::Permissions::from_mode(0o700));
            }
            let hops = run_dir.join("hops");
            fs::create_dir_all(&hops)?;
            let mut writable = vec![cstr(&hops)?];
            if let Some(l) = layers {
                writable.push(cstr(&l.dir)?);
            }
            // The inbox: a cig inside the hop has it as its own state root.
            // Outside `runs/`, so the hide step below never covers it.
            let inbox = prepare_inbox(&run_dir)?;
            writable.push(cstr(&inbox)?);
            let bus_cut = bus_cut();
            let mut covers = Vec::new();
            for p in key_routes(&keys, bus_cut) {
                covers.push(cstr(&p)?);
            }
            // What the hide step covers, and the one file under `runs/`
            // that stays writable through it: this hop's own burn-protocol
            // journal, pre-touched so the bind has something to land on.
            // The tin folder is made (0700) if a run has yet to write
            // there, so the tmpfs always has a mountpoint to cover. When
            // the registry proxy is on, its folder under `runs/` is
            // pre-planned too so the fresh tmpfs has a mountpoint for the
            // real socket folder the netns block binds read-only next.
            let hide = hide_plan(&home, &run_dir, plan.seq());
            fs::create_dir_all(&hide.tin_dir)?;
            fs::create_dir_all(&hide.runs_dir)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&hide.tin_dir, fs::Permissions::from_mode(0o700));
            }
            if let Some(parent) = hide.hop_journal.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&hide.hop_journal)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&hide.hop_journal, fs::Permissions::from_mode(0o600));
            }
            let mut mkdirs = hide.mkdirs.clone();
            let hide_proxy_dir = if let Some(p) = &proxy_dir {
                fs::create_dir_all(p)?;
                mkdirs.push(p.clone());
                Some(cstr(p)?)
            } else {
                None
            };
            let hide_mkdirs = mkdirs
                .iter()
                .map(|p| cstr(p))
                .collect::<io::Result<Vec<_>>>()?;
            Some(Guard {
                home_flags: locked_flags(&home),
                home: cstr(&home)?,
                keys: cstr(&keys)?,
                writable,
                why: cstr(&guard_why_path(&base))?,
                required: guard_setting() == "on",
                inbox,
                pretend_unavailable: std::env::var_os("CIG_INTERNAL_NO_GUARD").is_some(),
                covers,
                bus_cut,
                tin_dir: cstr(&hide.tin_dir)?,
                runs_dir: cstr(&hide.runs_dir)?,
                hop_journal: cstr(&hide.hop_journal)?,
                hide_proxy_dir,
                hide_mkdirs,
            })
        };
        // The namespaces beyond the mounts: only with the box itself.
        let boxed = boxed_flag;
        let netns = netns_flag;
        let mut forward = None;
        let mut forward_env = None;
        let mut sock_dir = None;
        if netns && spec.network == Network::Registry {
            let seq = layers.map(seq_of).unwrap_or(0);
            let sock = proxy_sock_path(&run_dir, seq);
            if let Some(dir) = sock.parent() {
                sock_dir = Some((cstr(dir)?, locked_flags(dir)));
            }
            let exe = std::env::current_exe()?;
            forward = Some([
                cstr(&exe)?,
                std::ffi::CString::new("internal-forward").expect("plain"),
                cstr(&sock)?,
                std::ffi::CString::new(PROXY_PORT.to_string()).expect("digits"),
            ]);
            forward_env = Some(ExecEnv::marked_self());
        }
        let contain = match (contained, layers) {
            (true, Some(l)) => {
                let sock = forward
                    .as_ref()
                    .map(|f| PathBuf::from(std::ffi::OsStr::from_bytes(f[2].as_bytes())));
                Some(plan_contain(l, cwd, &run_dir, sock.as_deref())?)
            }
            _ => None,
        };
        Ok(Setup {
            uid_map: format!("{uid} {uid} 1\n").into_bytes(),
            gid_map: format!("{gid} {gid} 1\n").into_bytes(),
            mounts,
            binds,
            cwd: cstr(cwd)?,
            why: cstr(&base.join("why"))?,
            guard,
            pidns: boxed && spec.pidns,
            netns,
            resolv_null: netns,
            sock_dir,
            forward,
            forward_env,
            contain,
            watch: None,
            decoy_mounts: Vec::new(),
        })
    }

    /// Run in the child, between fork and exec.
    ///
    /// # Safety
    ///
    /// Only async-signal-safe calls are made (unshare, open, write, mount,
    /// chdir, close), on data prepared before the fork (the descriptors the
    /// binds name were opened there); the caller runs it in a `pre_exec`
    /// closure and nowhere else.
    pub unsafe fn apply(&self) -> io::Result<()> {
        let mut flags = libc::CLONE_NEWUSER | libc::CLONE_NEWNS;
        if self.pidns {
            flags |= libc::CLONE_NEWPID;
        }
        if self.netns {
            flags |= libc::CLONE_NEWNET;
        }
        if self.contain.is_some() {
            // System V IPC, the host name and the cgroup view of its own.
            flags |= libc::CLONE_NEWIPC | libc::CLONE_NEWUTS | libc::CLONE_NEWCGROUP;
        }
        if libc::unshare(flags) != 0 {
            return self.fail(b"unshare(CLONE_NEWUSER|CLONE_NEWNS) refused");
        }
        // Once cig has loaded the tin's key it is non-dumpable
        // (seal::harden_process_memory), and the fork inherits that. A
        // non-dumpable process's /proc/self files are owned by root, so this
        // child could not open its own uid_map to write it. The child is
        // about to exec, which makes it dumpable again regardless, so it is
        // made dumpable now, inside its own user namespace; the parent cig
        // holding the key stays protected.
        libc::prctl(libc::PR_SET_DUMPABLE, 1, 0, 0, 0);
        if !write_file(c"/proc/self/uid_map", &self.uid_map) {
            return self.fail(b"writing /proc/self/uid_map refused");
        }
        write_file(c"/proc/self/setgroups", b"deny\n");
        if !write_file(c"/proc/self/gid_map", &self.gid_map) {
            return self.fail(b"writing /proc/self/gid_map refused");
        }
        // The maps are written: back to non-dumpable. The relay and the init
        // below never exec, so they stay copies of cig's memory, the key
        // among it when one was loaded; non-dumpable, nothing of the user's
        // can trace or read them. The hop's exec makes it dumpable again.
        libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
        // The forwarder's program, held before any mount changes the view:
        // under `contain` the box's temp goes over /tmp, and a cig that lives
        // there (a build tree, a CI runner's tool folder) would be gone.
        let exe_fd = match &self.forward {
            Some(f) => libc::open(f[0].as_ptr(), libc::O_PATH | libc::O_CLOEXEC),
            None => -1,
        };
        // Nothing mounted here must reach the parent's namespace.
        if libc::mount(
            std::ptr::null(),
            c"/".as_ptr(),
            std::ptr::null(),
            libc::MS_REC | libc::MS_PRIVATE,
            std::ptr::null(),
        ) != 0
        {
            return self.fail(b"making the mount tree private refused");
        }
        // The tin guard, before the overlays: the state root bound over
        // itself (read-write), the hop's own subtrees bound writable under
        // it, then the root remounted read-only (not recursively, so those
        // stay writable), and an empty tmpfs over the key folder. A failure
        // here is noted and the hop goes on boxed without the guard; the
        // parent decides what `tin_guard = on` makes of it.
        if let Some(g) = &self.guard {
            let null = std::ptr::null::<libc::c_char>();
            let mut failed: Option<&[u8]> = None;
            if g.pretend_unavailable {
                failed = Some(b"CIG_INTERNAL_NO_GUARD is set");
            }
            if failed.is_none()
                && libc::mount(
                    g.home.as_ptr(),
                    g.home.as_ptr(),
                    null,
                    libc::MS_BIND,
                    std::ptr::null(),
                ) != 0
            {
                failed = Some(b"binding the state directory over itself refused");
            }
            if failed.is_none() {
                for w in &g.writable {
                    if libc::mount(
                        w.as_ptr(),
                        w.as_ptr(),
                        null,
                        libc::MS_BIND,
                        std::ptr::null(),
                    ) != 0
                    {
                        failed = Some(b"binding a writable subtree of the state directory refused");
                        break;
                    }
                }
            }
            if failed.is_none()
                && libc::mount(
                    null,
                    g.home.as_ptr(),
                    null,
                    libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY | g.home_flags,
                    std::ptr::null(),
                ) != 0
            {
                failed = Some(b"remounting the state directory read-only refused");
            }
            if failed.is_none()
                && libc::mount(
                    c"tmpfs".as_ptr(),
                    g.keys.as_ptr(),
                    c"tmpfs".as_ptr(),
                    libc::MS_NOSUID | libc::MS_NODEV,
                    c"mode=0700,size=16k".as_ptr() as *const libc::c_void,
                ) != 0
            {
                failed = Some(b"hiding the key folder refused");
            }
            if let Some(what) = failed {
                self.note(&g.why, what);
                if g.required {
                    // Asked for, not available: the hop does not run.
                    return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
                }
            }
        }
        // The build and dependency trees, held open before the overlay hides
        // them. Opened here, after the namespace split: a descriptor from
        // before it names the parent namespace's mounts, which this one
        // cannot bind (EINVAL).
        let mut held: [libc::c_int; MAX_BINDS] = [-1; MAX_BINDS];
        for (i, at) in self.binds.iter().enumerate() {
            held[i] = libc::open(
                at.as_ptr(),
                libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
            );
        }
        for (at, opts) in &self.mounts {
            if libc::mount(
                c"overlay".as_ptr(),
                at.as_ptr(),
                c"overlay".as_ptr(),
                0,
                opts.as_ptr() as *const libc::c_void,
            ) != 0
            {
                return self.fail(b"mounting overlayfs over a root refused (a kernel before 5.11, or a filesystem overlayfs cannot layer over)");
            }
        }
        // Those trees come through from the real root: what a hop writes
        // there is not the box's business, as it is not the watch's.
        for (i, at) in self.binds.iter().enumerate() {
            let fd = held[i];
            if fd < 0 {
                // Gone since the plan was made: nothing to bind.
                continue;
            }
            let prefix = b"/proc/self/fd/";
            let mut from = [0u8; 48];
            from[..prefix.len()].copy_from_slice(prefix);
            let mut digits = [0u8; 32];
            let k = itoa(fd, &mut digits);
            from[prefix.len()..prefix.len() + k].copy_from_slice(&digits[..k]);
            if libc::mount(
                from.as_ptr() as *const libc::c_char,
                at.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND | libc::MS_REC,
                std::ptr::null(),
            ) != 0
            {
                return self.fail(b"binding a build tree through the overlay refused");
            }
            libc::close(fd);
        }
        // `contain`: the home layer, what goes back over it, the key folder
        // and the other ways to the key, the temp, a /dev of its own.
        if let Some(c) = &self.contain {
            self.contain_mounts(c)?;
        }
        // The other ways to the key, covered with /dev/null after the
        // overlays, so the cover is in the view the hop has: a key file read
        // gives nothing, a socket refuses the connection. The hop keeps its
        // own uid in the namespace, so after exec it has no capability to
        // take a cover off, and a namespace it makes of its own inherits the
        // cover locked.
        if let Some(g) = &self.guard {
            for c in &g.covers {
                if let Err(errno) = cover_path(c) {
                    write_why(&g.why, b"covering a way to the key refused", Some(c), errno);
                    if g.required {
                        return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
                    }
                }
            }
        }
        // Decoys (and pass_reads' decoy half): bind-mount the planted
        // fake read-only over each real credential path, in this box's
        // mount namespace only. The real file is then unreachable by
        // any path in the box — a symlink, a dirfd, /proc/self/root/...
        // — because the inode at this path is the decoy's. A path
        // missing at mount time has nothing to protect and is skipped;
        // a bind that fails for any other reason refuses the hop, the
        // way the covers do. The real file is never bound anywhere in
        // the box: the supervisor serves pass_reads from outside the
        // namespace. Done BEFORE the hide step (which tmpfs-covers
        // `runs/`, under which the decoy sources sit), so the sources
        // are still reachable here.
        for dm in &self.decoy_mounts {
            match decoy_cover(&dm.source, &dm.target) {
                DecoyOutcome::Covered | DecoyOutcome::Skipped => {}
                DecoyOutcome::Err(errno) => {
                    let why_file = match &self.guard {
                        Some(g) => &g.why,
                        None => &self.why,
                    };
                    write_why(
                        why_file,
                        b"binding a decoy over a credential refused",
                        Some(&dm.target),
                        errno,
                    );
                    return Err(io::Error::from_raw_os_error(errno));
                }
            }
        }
        // Hide `tin/` and `runs/` from the hop entirely, with the hop's own
        // burn-protocol journal bound back through so the protocol keeps
        // working. Done last so every setup `self.note`/`self.fail` before
        // this reaches the real `why` file, and any failure here is noted
        // through a descriptor opened before the tmpfs covers the path.
        if let Some(g) = &self.guard {
            self.hide_tin_and_runs(g)?;
        }
        // Into the working directory again, through the overlay this time.
        if libc::chdir(self.cwd.as_ptr()) != 0 {
            return self.fail(b"entering the working directory through the overlay refused");
        }
        // The network namespace has `lo` down and no resolver: bring `lo`
        // up (the forwarder listens on it), and bind an empty file over
        // resolv.conf so a lookup has no one to ask.
        if self.netns {
            let s = libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0);
            if s >= 0 {
                let mut ifr: libc::ifreq = std::mem::zeroed();
                ifr.ifr_name[0] = b'l' as libc::c_char;
                ifr.ifr_name[1] = b'o' as libc::c_char;
                ifr.ifr_ifru.ifru_flags = (libc::IFF_UP | libc::IFF_RUNNING) as libc::c_short;
                libc::ioctl(s, libc::SIOCSIFFLAGS as _, &ifr);
                libc::close(s);
            }
            if self.resolv_null {
                libc::mount(
                    c"/dev/null".as_ptr(),
                    c"/etc/resolv.conf".as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND,
                    std::ptr::null(),
                );
            }
            // The proxy's socket folder, read-only inside whatever the guard
            // managed: the forwarder connects by path, so the hop must not be
            // able to put a link to another socket in its place.
            if let Some((dir, flags)) = &self.sock_dir {
                if libc::mount(
                    dir.as_ptr(),
                    dir.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND,
                    std::ptr::null(),
                ) != 0
                    || libc::mount(
                        std::ptr::null(),
                        dir.as_ptr(),
                        std::ptr::null(),
                        libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY | *flags,
                        std::ptr::null(),
                    ) != 0
                {
                    return self.fail(b"making the proxy's socket folder read-only refused");
                }
            }
        }
        // `contain`: the whole tree read-only in one call, then the places
        // whose writes are layers (or scratch) writable again. Overlayfs
        // writes its upper through a private clone of the upper's mount,
        // taken when it was mounted above, so the layers keep working.
        if let Some(c) = &self.contain {
            if !mount_setattr(c"/", true, MOUNT_ATTR_RDONLY, 0) {
                return self.fail(b"making the rest of the machine read-only refused (mount_setattr needs Linux 5.12)");
            }
            for (w, required) in &c.writable {
                if !mount_setattr(w, false, 0, MOUNT_ATTR_RDONLY) && *required {
                    return self.fail(b"making a layer writable again refused");
                }
            }
        }
        // The PID namespace: unshare(CLONE_NEWPID) puts only children in
        // it, so this process (R) stays outside as a relay, forks the
        // namespace's init (A, pid 1 inside), which forks the hop (B) and
        // reaps for it. Each dies with its parent (PR_SET_PDEATHSIG), so a
        // cig that dies takes the whole namespace with it, and the layer is
        // applied only by a live cig after the hop. R and A ignore the
        // termination signals cig sends the group, so the hop gets its
        // grace; the SIGKILL after it reaches everyone.
        if self.pidns {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0);
            libc::signal(libc::SIGTERM, libc::SIG_IGN);
            libc::signal(libc::SIGINT, libc::SIG_IGN);
            let a = libc::fork();
            if a < 0 {
                return self.fail(b"forking the box's init refused");
            }
            if a > 0 {
                // The relay is a forked copy of cig: the parent's signal
                // handlers came with it (the child-exit handler of the
                // waiting crate writes to a pipe of the parent's), and so
                // did every descriptor, among them the standard library's
                // exec-status pipe, which the parent reads to the end
                // before `spawn` returns. The handler goes back to the
                // default and every descriptor above stderr is closed, so
                // nothing here holds `spawn`, or the hop's timeout, until
                // the hop ends.
                libc::signal(libc::SIGCHLD, libc::SIG_DFL);
                close_from_3();
                libc::_exit(wait_for(a));
            }
            // A: pid 1 inside, the same forked copy; the same resets.
            libc::signal(libc::SIGCHLD, libc::SIG_DFL);
            // Its own /proc, best effort (a container may refuse it; the
            // namespace still hides the pids from signals).
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0);
            libc::mount(
                c"proc".as_ptr(),
                c"/proc".as_ptr(),
                c"proc".as_ptr(),
                libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
                std::ptr::null(),
            );
            // The forwarder's listener, made before the hop exists: a hop
            // that connects at once waits in its backlog, never refused
            // because the forwarder (a fresh process; a compiled program
            // reads its own payload first) has not bound the port yet. It
            // reaches the forwarder as fd 3; the hop's exec closes it.
            let listener = match &self.forward {
                Some(_) => listen_loopback(PROXY_PORT),
                None => -1,
            };
            if let Some(f) = &self.forward {
                let p = libc::fork();
                if p == 0 {
                    // The forwarder's own stdio to /dev/null: it must not
                    // hold the hop's pipes open after the hop has ended.
                    let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC);
                    if null >= 0 {
                        libc::dup2(null, 0);
                        libc::dup2(null, 1);
                        libc::dup2(null, 2);
                    }
                    // The program's descriptor off 3, then the listener on
                    // it, kept across the exec.
                    let exe_fd = if exe_fd == 3 {
                        libc::fcntl(exe_fd, libc::F_DUPFD_CLOEXEC, 4)
                    } else {
                        exe_fd
                    };
                    if listener == 3 {
                        libc::fcntl(3, libc::F_SETFD, 0);
                    } else if listener >= 0 {
                        libc::dup2(listener, 3);
                    }
                    let argv = [
                        f[0].as_ptr(),
                        f[1].as_ptr(),
                        f[2].as_ptr(),
                        f[3].as_ptr(),
                        std::ptr::null(),
                    ];
                    // Through the descriptor held above, whatever covers
                    // the path now; by the path where none could be held.
                    // Marked as cig's call to itself (built in the parent).
                    let envp = match &self.forward_env {
                        Some(e) => e.envp(),
                        None => libc::_exit(127),
                    };
                    if exe_fd >= 0 {
                        let from = fd_path(exe_fd);
                        libc::execve(from.as_ptr() as *const libc::c_char, argv.as_ptr(), envp);
                    }
                    libc::execve(f[0].as_ptr(), argv.as_ptr(), envp);
                    libc::_exit(127);
                }
            }
            let b = libc::fork();
            if b < 0 {
                libc::_exit(127);
            }
            if b > 0 {
                // The init needs no descriptor either (see the relay).
                close_from_3();
                // The init loop: reap everything; the hop's exit is ours.
                let mut status: libc::c_int = 0;
                loop {
                    let r = libc::waitpid(-1, &mut status, 0);
                    if r == b {
                        libc::_exit(exit_code(status));
                    }
                    if r < 0 {
                        let e = *libc::__errno_location();
                        if e == libc::EINTR {
                            continue;
                        }
                        libc::_exit(127);
                    }
                }
            }
            // B: the hop. Signals as the hop expects them.
            libc::signal(libc::SIGTERM, libc::SIG_DFL);
            libc::signal(libc::SIGINT, libc::SIG_DFL);
        }
        // Whatever descriptor cig held without close-on-exec (a library's,
        // never cig's own) is closed by the exec: one that named a folder
        // before the mounts above would still write through the old view.
        cloexec_from_3();
        // `contain`, last of all in the hop itself: a session of its own (no
        // controlling terminal to push input into), no gaining privileges
        // through a setuid program, and the syscall filter.
        if let Some(c) = &self.contain {
            libc::setsid();
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                return self.fail(b"setting no_new_privs refused");
            }
            let prog = libc::sock_fprog {
                len: c.filter.len() as libc::c_ushort,
                filter: c.filter.as_ptr() as *mut libc::sock_filter,
            };
            if libc::prctl(
                libc::PR_SET_SECCOMP,
                libc::SECCOMP_MODE_FILTER,
                &prog as *const libc::sock_fprog,
                0,
                0,
            ) != 0
            {
                return self.fail(b"installing the hop's syscall filter refused");
            }
            // No watch: io_uring is refused here, without an event.
            if self.watch.is_none() && !super::watch::install(&c.refusal) {
                return self.fail(b"installing the hop's io_uring refusal refused");
            }
        }
        if let Some(w) = &self.watch {
            return self.install_watch(w);
        }
        Ok(())
    }

    /// The watch's filter in the hop, last of all: by seccomp user
    /// notification when the kernel has it (the descriptor goes to cig over
    /// the socket), else, for a watch of calls, by ptrace (cig seizes this
    /// process and answers once it is held); else the hop is refused when a
    /// watch was asked for, and runs with the io_uring refusal alone when
    /// the watch was only the contained box's own.
    ///
    /// # Safety
    ///
    /// Called from `apply`, between fork and exec: only async-signal-safe
    /// calls, on data planned in the parent.
    unsafe fn install_watch(&self, w: &super::watch::WatchChild) -> io::Result<()> {
        use super::watch as wt;
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            return self.fail(b"setting no_new_privs refused");
        }
        let fd = if w.force_ptrace {
            -1
        } else {
            wt::try_listener(&w.notify)
        };
        if fd >= 0 {
            let sent = wt::send_msg(w.sock, b'N', fd);
            libc::close(fd);
            libc::close(w.sock);
            if !sent {
                return self.fail(b"handing the watch to cig refused");
            }
            return Ok(());
        }
        // The ptrace route only: cig must attach to this process, and the
        // mm of a hop forked from a cig that holds the tin's key is not
        // dumpable, so a cig outside this user namespace may not. It is
        // set here, as late as it can be and only on this route, and the
        // exec a few calls on resets it. This process still holds a copy
        // of cig's memory until then (nothing in the child wipes it), so
        // the fast route, which needs no attach, never sets it.
        if w.mode >= wt::Mode::Calls {
            libc::prctl(libc::PR_SET_DUMPABLE, 1, 0, 0, 0);
        }
        if w.mode >= wt::Mode::Calls
            && wt::send_msg(w.sock, b'P', -1)
            && wt::wait_ack(w.sock)
            && wt::install(&w.trace)
        {
            libc::close(w.sock);
            return Ok(());
        }
        libc::close(w.sock);
        if w.required {
            return self.fail(
                b"the watch cannot be made here (no seccomp user notification and no ptrace)",
            );
        }
        if let Some(c) = &self.contain {
            if !wt::install(&c.refusal) {
                return self.fail(b"installing the hop's io_uring refusal refused");
            }
        }
        Ok(())
    }

    /// `contain`'s mounts, after the pack's overlays (see `Contain`).
    ///
    /// # Safety
    ///
    /// Called from `apply`, between fork and exec: only async-signal-safe
    /// calls, on data planned in the parent.
    unsafe fn contain_mounts(&self, c: &Contain) -> io::Result<()> {
        let null = std::ptr::null::<libc::c_char>();
        // What lives under the home, held before the home layer hides it.
        let mut held: [libc::c_int; MAX_BINDS] = [-1; MAX_BINDS];
        for (i, at) in c.rebinds.iter().enumerate() {
            held[i] = libc::open(
                at.as_ptr(),
                libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
            );
            if held[i] < 0 {
                return self.fail(b"holding what lives under the home refused");
            }
        }
        if let Some((home, opts)) = &c.home {
            if libc::mount(
                c"overlay".as_ptr(),
                home.as_ptr(),
                c"overlay".as_ptr(),
                0,
                opts.as_ptr() as *const libc::c_void,
            ) != 0
            {
                return self.fail(b"mounting the home layer refused");
            }
        }
        for (i, at) in c.rebinds.iter().enumerate() {
            let from = fd_path(held[i]);
            if libc::mount(
                from.as_ptr() as *const libc::c_char,
                at.as_ptr(),
                null,
                libc::MS_BIND | libc::MS_REC,
                std::ptr::null(),
            ) != 0
            {
                return self.fail(b"putting back what lives under the home refused");
            }
            libc::close(held[i]);
        }
        if let Some(keys) = &c.keys {
            if libc::mount(
                c"tmpfs".as_ptr(),
                keys.as_ptr(),
                c"tmpfs".as_ptr(),
                libc::MS_NOSUID | libc::MS_NODEV,
                c"mode=0700,size=16k".as_ptr() as *const libc::c_void,
            ) != 0
            {
                return self.fail(b"hiding the key folder refused");
            }
        }
        for cover in &c.covers {
            if let Err(errno) = cover_path(cover) {
                write_why(
                    &self.why,
                    b"covering a way to the key refused",
                    Some(cover),
                    errno,
                );
                return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
            }
        }
        let tmp_at = if c.tmp_over {
            c"/tmp"
        } else {
            c.tmp.as_c_str()
        };
        if libc::mount(
            c.tmp.as_ptr(),
            tmp_at.as_ptr(),
            null,
            libc::MS_BIND,
            std::ptr::null(),
        ) != 0
        {
            return self.fail(b"binding the box's temp refused");
        }
        // A /dev of its own: an empty tmpfs, the plain nodes bound in from
        // the machine's, a pty instance of its own, a fresh /dev/shm.
        let mut nodes: [libc::c_int; 8] = [-1; 8];
        for (i, n) in DEV_NODES.iter().enumerate() {
            let mut path = [0u8; 32];
            let pre = b"/dev/";
            path[..pre.len()].copy_from_slice(pre);
            path[pre.len()..pre.len() + n.len()].copy_from_slice(n.as_bytes());
            nodes[i] = libc::open(
                path.as_ptr() as *const libc::c_char,
                libc::O_PATH | libc::O_CLOEXEC,
            );
        }
        if libc::mount(
            c"tmpfs".as_ptr(),
            c"/dev".as_ptr(),
            c"tmpfs".as_ptr(),
            libc::MS_NOSUID | libc::MS_NOEXEC,
            c"mode=0755,size=64k".as_ptr() as *const libc::c_void,
        ) != 0
        {
            return self.fail(b"giving the hop a /dev of its own refused");
        }
        for (i, n) in DEV_NODES.iter().enumerate() {
            if nodes[i] < 0 {
                continue;
            }
            let mut path = [0u8; 32];
            let pre = b"/dev/";
            path[..pre.len()].copy_from_slice(pre);
            path[pre.len()..pre.len() + n.len()].copy_from_slice(n.as_bytes());
            let at = path.as_ptr() as *const libc::c_char;
            let made = libc::open(at, libc::O_WRONLY | libc::O_CREAT | libc::O_CLOEXEC, 0o666);
            if made >= 0 {
                libc::close(made);
            }
            let from = fd_path(nodes[i]);
            if libc::mount(
                from.as_ptr() as *const libc::c_char,
                at,
                null,
                libc::MS_BIND,
                std::ptr::null(),
            ) != 0
            {
                return self.fail(b"binding a device into the hop's /dev refused");
            }
            libc::close(nodes[i]);
        }
        libc::mkdir(c"/dev/pts".as_ptr(), 0o755);
        libc::mkdir(c"/dev/shm".as_ptr(), 0o1777);
        // A pty instance of its own, best effort (some kernels refuse it to a
        // user namespace; the hop then has no pseudo-terminals).
        if libc::mount(
            c"devpts".as_ptr(),
            c"/dev/pts".as_ptr(),
            c"devpts".as_ptr(),
            libc::MS_NOSUID | libc::MS_NOEXEC,
            c"newinstance,ptmxmode=0666,mode=0620".as_ptr() as *const libc::c_void,
        ) == 0
        {
            libc::symlink(c"pts/ptmx".as_ptr(), c"/dev/ptmx".as_ptr());
        }
        if libc::mount(
            c"tmpfs".as_ptr(),
            c"/dev/shm".as_ptr(),
            c"tmpfs".as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV,
            c"mode=1777,size=256m".as_ptr() as *const libc::c_void,
        ) != 0
        {
            return self.fail(b"giving the hop a /dev/shm of its own refused");
        }
        libc::symlink(c"/proc/self/fd".as_ptr(), c"/dev/fd".as_ptr());
        libc::symlink(c"/proc/self/fd/0".as_ptr(), c"/dev/stdin".as_ptr());
        libc::symlink(c"/proc/self/fd/1".as_ptr(), c"/dev/stdout".as_ptr());
        libc::symlink(c"/proc/self/fd/2".as_ptr(), c"/dev/stderr".as_ptr());
        Ok(())
    }

    /// Hide `tin/` and `runs/` with two empty tmpfs mounts, then rebind
    /// this hop's own burn-protocol journal through the fresh `runs/` so
    /// the protocol keeps working. A descriptor on `why` is opened before
    /// any tmpfs covers the path, so a failure here is still noted on the
    /// real file; `tin_guard = on` refuses the hop on a failure, `auto`
    /// falls back to the tin read-only (where the state root remount and
    /// the overlay already put it).
    ///
    /// # Safety
    ///
    /// Only async-signal-safe calls (open, close, mkdir, mount, write): it
    /// runs in the pre-exec child.
    unsafe fn hide_tin_and_runs(&self, g: &Guard) -> io::Result<()> {
        // A descriptor on the why file first, since the hide covers the
        // path a few mounts from now: any later note writes through this,
        // not through `self.note` which would open the hidden tmpfs.
        let why_fd = libc::open(
            g.why.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND | libc::O_CLOEXEC,
            0o644,
        );
        // Descriptors on the real hop journal file and, if the registry
        // proxy named one, its socket folder: the binds that follow land
        // the real inodes on the paths inside the fresh tmpfs.
        let journal_fd = libc::open(g.hop_journal.as_ptr(), libc::O_PATH | libc::O_CLOEXEC);
        if journal_fd < 0 {
            note_fd(why_fd, b"holding the hop journal refused");
            if why_fd >= 0 {
                libc::close(why_fd);
            }
            if g.required {
                return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
            }
            return Ok(());
        }
        let proxy_fd = match &g.hide_proxy_dir {
            Some(p) => libc::open(
                p.as_ptr(),
                libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
            ),
            None => -1,
        };
        if g.hide_proxy_dir.is_some() && proxy_fd < 0 {
            note_fd(why_fd, b"holding the proxy folder refused");
            libc::close(journal_fd);
            if why_fd >= 0 {
                libc::close(why_fd);
            }
            if g.required {
                return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
            }
            return Ok(());
        }
        let mut failed: Option<&[u8]> = None;
        // What was mounted, so a failure part way undoes it: a half-hidden
        // runs/ without the hop's journal bound back would swallow the
        // protocol's writes; undone, the hop sees the tin read-only as
        // before. Flags, not a list: this runs between fork and exec, where
        // nothing may allocate.
        let (mut tin_hidden, mut runs_hidden, mut journal_bound) = (false, false, false);
        if libc::mount(
            c"tmpfs".as_ptr(),
            g.tin_dir.as_ptr(),
            c"tmpfs".as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
            c"mode=0700,size=4k".as_ptr() as *const libc::c_void,
        ) != 0
        {
            failed = Some(b"hiding the tin refused");
        } else {
            tin_hidden = true;
        }
        if failed.is_none()
            && libc::mount(
                c"tmpfs".as_ptr(),
                g.runs_dir.as_ptr(),
                c"tmpfs".as_ptr(),
                libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
                c"mode=0700,size=16k".as_ptr() as *const libc::c_void,
            ) != 0
        {
            failed = Some(b"hiding runs refused");
        } else if failed.is_none() {
            runs_hidden = true;
        }
        if failed.is_none() {
            for p in &g.hide_mkdirs {
                if libc::mkdir(p.as_ptr(), 0o700) != 0 && *libc::__errno_location() != libc::EEXIST
                {
                    failed = Some(b"making a path inside the hidden runs refused");
                    break;
                }
            }
        }
        if failed.is_none() {
            let fd = libc::open(
                g.hop_journal.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_CLOEXEC,
                0o600,
            );
            if fd < 0 {
                failed = Some(b"touching the hop journal in the hidden runs refused");
            } else {
                libc::close(fd);
            }
        }
        if failed.is_none() {
            let from = fd_path(journal_fd);
            if libc::mount(
                from.as_ptr() as *const libc::c_char,
                g.hop_journal.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND,
                std::ptr::null(),
            ) != 0
            {
                failed = Some(b"binding the hop journal through the hidden runs refused");
            } else {
                journal_bound = true;
            }
        }
        if failed.is_none() {
            if let Some(p) = &g.hide_proxy_dir {
                let from = fd_path(proxy_fd);
                if libc::mount(
                    from.as_ptr() as *const libc::c_char,
                    p.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND,
                    std::ptr::null(),
                ) != 0
                {
                    failed = Some(b"binding the proxy folder through the hidden runs refused");
                }
            }
        }
        libc::close(journal_fd);
        if proxy_fd >= 0 {
            libc::close(proxy_fd);
        }
        if let Some(what) = failed {
            note_fd(why_fd, what);
            if journal_bound {
                libc::umount2(g.hop_journal.as_ptr(), libc::MNT_DETACH);
            }
            if runs_hidden {
                libc::umount2(g.runs_dir.as_ptr(), libc::MNT_DETACH);
            }
            if tin_hidden {
                libc::umount2(g.tin_dir.as_ptr(), libc::MNT_DETACH);
            }
            if why_fd >= 0 {
                libc::close(why_fd);
            }
            if g.required {
                return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
            }
            return Ok(());
        }
        if why_fd >= 0 {
            libc::close(why_fd);
        }
        Ok(())
    }

    /// Write a reason with the errno to `at`; the exec goes on.
    unsafe fn note(&self, at: &std::ffi::CString, what: &[u8]) {
        let errno = *libc::__errno_location();
        let fd = libc::open(
            at.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_CLOEXEC,
            0o644,
        );
        if fd >= 0 {
            let _ = libc::write(fd, what.as_ptr() as *const libc::c_void, what.len());
            let mut buf = [0u8; 32];
            let n = itoa(errno, &mut buf);
            let _ = libc::write(fd, b" (errno ".as_ptr() as *const libc::c_void, 8);
            let _ = libc::write(fd, buf.as_ptr() as *const libc::c_void, n);
            let _ = libc::write(fd, b")\n".as_ptr() as *const libc::c_void, 2);
            libc::close(fd);
        }
    }

    /// Write the reason with the errno, then refuse the exec.
    unsafe fn fail(&self, what: &[u8]) -> io::Result<()> {
        let errno = *libc::__errno_location();
        let fd = libc::open(
            self.why.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_CLOEXEC,
            0o644,
        );
        if fd >= 0 {
            let _ = libc::write(fd, what.as_ptr() as *const libc::c_void, what.len());
            let mut buf = [0u8; 32];
            let n = itoa(errno, &mut buf);
            let _ = libc::write(fd, b" (errno ".as_ptr() as *const libc::c_void, 8);
            let _ = libc::write(fd, buf.as_ptr() as *const libc::c_void, n);
            let _ = libc::write(fd, b")\n".as_ptr() as *const libc::c_void, 2);
            libc::close(fd);
        }
        Err(io::Error::from_raw_os_error(libc::ENOTSUP))
    }
}

/// Close every descriptor from 3 up: `close_range` where the kernel has
/// it (5.9), one by one where it does not. Async-signal-safe.
#[cfg(target_os = "linux")]
unsafe fn close_from_3() {
    if libc::syscall(
        libc::SYS_close_range,
        3 as libc::c_long,
        libc::c_uint::MAX as libc::c_long,
        0 as libc::c_long,
    ) < 0
    {
        for fd in 3..4096 {
            libc::close(fd);
        }
    }
}

/// Write a reason with the errno to an open descriptor; used after a
/// tmpfs has covered the path `self.note` would open.
///
/// # Safety
///
/// Async-signal-safe (`write` only on a descriptor opened before this).
#[cfg(target_os = "linux")]
unsafe fn note_fd(fd: libc::c_int, what: &[u8]) {
    if fd < 0 {
        return;
    }
    let errno = *libc::__errno_location();
    let _ = libc::write(fd, what.as_ptr() as *const libc::c_void, what.len());
    let mut buf = [0u8; 32];
    let n = itoa(errno, &mut buf);
    let _ = libc::write(fd, b" (errno ".as_ptr() as *const libc::c_void, 8);
    let _ = libc::write(fd, buf.as_ptr() as *const libc::c_void, n);
    let _ = libc::write(fd, b")\n".as_ptr() as *const libc::c_void, 2);
}

/// `/proc/self/fd/<fd>`, NUL-terminated, built without allocating.
#[cfg(target_os = "linux")]
fn fd_path(fd: libc::c_int) -> [u8; 48] {
    let prefix = b"/proc/self/fd/";
    let mut from = [0u8; 48];
    from[..prefix.len()].copy_from_slice(prefix);
    let mut digits = [0u8; 32];
    let k = itoa(fd, &mut digits);
    from[prefix.len()..prefix.len() + k].copy_from_slice(&digits[..k]);
    from
}

/// Mark every descriptor above stderr close-on-exec (`close_range` with
/// `CLOSE_RANGE_CLOEXEC`, Linux 5.11, which the box needs anyway; else one
/// at a time). The standard library's exec-status pipe is close-on-exec
/// already, so the exec still reports to the parent.
#[cfg(target_os = "linux")]
unsafe fn cloexec_from_3() {
    const CLOSE_RANGE_CLOEXEC: libc::c_long = 1 << 2;
    if libc::syscall(
        libc::SYS_close_range,
        3 as libc::c_long,
        libc::c_uint::MAX as libc::c_long,
        CLOSE_RANGE_CLOEXEC,
    ) < 0
    {
        for fd in 3..4096 {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
    }
}

/// The flags a bind remount of `path` must carry: a mount copied into a
/// user namespace keeps its nosuid, nodev, noexec and atime settings
/// locked, and a remount that would change one is refused (EPERM).
#[cfg(target_os = "linux")]
pub fn locked_flags(path: &Path) -> libc::c_ulong {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return 0;
    };
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return 0;
    }
    // The kernel's statfs flag for relatime (not in the libc crate's musl
    // bindings, though the kernel sets it there too).
    const ST_RELATIME: libc::c_ulong = 4096;
    let f = st.f_flag as libc::c_ulong;
    let mut out: libc::c_ulong = 0;
    for (st_bit, ms_bit) in [
        (libc::ST_NOSUID as libc::c_ulong, libc::MS_NOSUID),
        (libc::ST_NODEV as libc::c_ulong, libc::MS_NODEV),
        (libc::ST_NOEXEC as libc::c_ulong, libc::MS_NOEXEC),
        (libc::ST_NOATIME as libc::c_ulong, libc::MS_NOATIME),
        (libc::ST_NODIRATIME as libc::c_ulong, libc::MS_NODIRATIME),
        (ST_RELATIME, libc::MS_RELATIME),
    ] {
        if f & st_bit != 0 {
            out |= ms_bit;
        }
    }
    if f & (libc::ST_NOATIME as libc::c_ulong | ST_RELATIME) == 0 {
        out |= libc::MS_STRICTATIME;
    }
    out
}

/// Wait for one child (through EINTR) and give its exit code.
#[cfg(target_os = "linux")]
unsafe fn wait_for(pid: libc::pid_t) -> libc::c_int {
    let mut status: libc::c_int = 0;
    loop {
        let r = libc::waitpid(pid, &mut status, 0);
        if r == pid {
            return exit_code(status);
        }
        if r < 0 && *libc::__errno_location() != libc::EINTR {
            return 127;
        }
    }
}

/// A wait status as an exit code: the code, or 128 plus the signal.
#[cfg(target_os = "linux")]
fn exit_code(status: libc::c_int) -> libc::c_int {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        127
    }
}

/// A directory's own name (a run's id), as text.
pub(crate) fn dir_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Overlayfs options separate directories with `,` and `:`, so those are
/// escaped with a backslash in a path that holds them.
#[cfg(target_os = "linux")]
fn escape(p: &Path) -> String {
    p.to_string_lossy()
        .replace('\\', "\\\\")
        .replace(',', "\\,")
        .replace(':', "\\:")
}

#[cfg(target_os = "linux")]
unsafe fn write_file(path: &std::ffi::CStr, bytes: &[u8]) -> bool {
    let fd = libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
    if fd < 0 {
        return false;
    }
    let n = libc::write(fd, bytes.as_ptr() as *const libc::c_void, bytes.len());
    libc::close(fd);
    n == bytes.len() as isize
}

/// The result of a decoy cover: `Covered` if the bind took, `Skipped` if
/// the real credential was never there in the first place (nothing to
/// protect), or `Err(errno)` if it still exists and could not be covered.
#[cfg(target_os = "linux")]
pub enum DecoyOutcome {
    Covered,
    Skipped,
    Err(i32),
}

/// Bind-mount `source` (a decoy file in the run's scratch) read-only
/// over `target` (a real credential path inside the box). Same retry
/// logic as `cover_path`: a target that has gone by the time of the
/// mount is `Skipped` (nothing to protect). After the bind takes, the
/// mount is remounted read-only + nosuid + nodev + noexec, **carrying
/// the source mount's locked flags** (statvfs on the source gives
/// MS_NOATIME / MS_NODEV / MS_NOSUID flags that the kernel locks in a
/// user namespace: a remount that drops one fails EPERM, and the
/// decoy would stay writable). A failed remount refuses the hop: the
/// mount is then unreliable as a boundary.
///
/// # Safety
///
/// Async-signal-safe: `mount`, `lstat`, `statvfs`.
#[cfg(target_os = "linux")]
unsafe fn decoy_cover(source: &std::ffi::CString, target: &std::ffi::CString) -> DecoyOutcome {
    let mut errno = 0;
    // Locked flags on the source mount: in a user namespace a bind
    // remount must carry MS_NODEV / MS_NOSUID / MS_NOATIME etc that the
    // kernel locked on the parent, or EPERM. statvfs gives them as
    // f_flag bits, which map 1:1 to the MS_* constants. The ST_* values
    // are the kernel's (fs.h): glibc exposes most but not RELATIME /
    // NODIRATIME under every libc (musl hides them), so each bit is
    // written out here.
    const ST_NOSUID_: libc::c_ulong = 0x0002;
    const ST_NODEV_: libc::c_ulong = 0x0004;
    const ST_NOEXEC_: libc::c_ulong = 0x0008;
    const ST_NOATIME_: libc::c_ulong = 0x0400;
    const ST_NODIRATIME_: libc::c_ulong = 0x0800;
    const ST_RELATIME_: libc::c_ulong = 0x1000;
    let locked: libc::c_ulong = {
        let mut st: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(source.as_ptr(), &mut st) == 0 {
            let f = st.f_flag;
            let mut m: libc::c_ulong = 0;
            if f & ST_NODEV_ != 0 {
                m |= libc::MS_NODEV;
            }
            if f & ST_NOSUID_ != 0 {
                m |= libc::MS_NOSUID;
            }
            if f & ST_NOEXEC_ != 0 {
                m |= libc::MS_NOEXEC;
            }
            if f & ST_NOATIME_ != 0 {
                m |= libc::MS_NOATIME;
            }
            if f & ST_NODIRATIME_ != 0 {
                m |= libc::MS_NODIRATIME;
            }
            if f & ST_RELATIME_ != 0 {
                m |= libc::MS_RELATIME;
            }
            m
        } else {
            0
        }
    };
    for _ in 0..5 {
        let r = libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND,
            std::ptr::null(),
        );
        if r == 0 {
            // A decoy never needs device nodes, setuid bits or exec
            // permission; the credential file never did either.
            let remount_flags = libc::MS_BIND
                | libc::MS_REMOUNT
                | libc::MS_RDONLY
                | libc::MS_NOSUID
                | libc::MS_NODEV
                | libc::MS_NOEXEC
                | locked;
            if libc::mount(
                std::ptr::null(),
                target.as_ptr(),
                std::ptr::null(),
                remount_flags,
                std::ptr::null(),
            ) != 0
            {
                return DecoyOutcome::Err(*libc::__errno_location());
            }
            return DecoyOutcome::Covered;
        }
        errno = *libc::__errno_location();
        let mut st: libc::stat = std::mem::zeroed();
        if libc::lstat(target.as_ptr(), &mut st) != 0 {
            // The real credential is gone: nothing to protect, so skip.
            return DecoyOutcome::Skipped;
        }
    }
    DecoyOutcome::Err(errno)
}

/// Cover `path` with /dev/null (a bind mount). A path that is gone by the
/// time of the mount, or whose parent mount is (a login session's runtime
/// folder when its last session ends, between the plan and this call), has
/// nothing left to hide: it is skipped, whatever errno the bind gave. A path
/// that still exists and could not be covered (after a few tries) is the
/// error, with the errno.
///
/// # Safety
///
/// Async-signal-safe: `mount` and `lstat`.
#[cfg(target_os = "linux")]
unsafe fn cover_path(path: &std::ffi::CString) -> Result<(), i32> {
    let mut errno = 0;
    // A path made again between the failed bind and the look (a new login
    // session's socket) is bound again, a few times.
    for _ in 0..5 {
        if libc::mount(
            c"/dev/null".as_ptr(),
            path.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND,
            std::ptr::null(),
        ) == 0
        {
            return Ok(());
        }
        errno = *libc::__errno_location();
        let mut st: libc::stat = std::mem::zeroed();
        if libc::lstat(path.as_ptr(), &mut st) != 0 {
            return Ok(());
        }
    }
    Err(errno)
}

/// Write why the box could not be made: the reason, the path it concerns
/// (control bytes shown as `?`) and the errno, so a bare "Operation not
/// supported" never hides the cause.
///
/// # Safety
///
/// Async-signal-safe (`open`, `write` on stack buffers).
#[cfg(target_os = "linux")]
unsafe fn write_why(
    at: &std::ffi::CString,
    what: &[u8],
    path: Option<&std::ffi::CString>,
    errno: i32,
) {
    let fd = libc::open(
        at.as_ptr(),
        libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_CLOEXEC,
        0o644,
    );
    if fd < 0 {
        return;
    }
    let put = |b: &[u8]| {
        let _ = libc::write(fd, b.as_ptr() as *const libc::c_void, b.len());
    };
    put(what);
    if let Some(p) = path {
        put(b": ");
        for &c in p.as_bytes() {
            put(&[if c < 0x20 || c == 0x7f { b'?' } else { c }]);
        }
    }
    let mut buf = [0u8; 32];
    let n = itoa(errno, &mut buf);
    put(b" (errno ");
    put(&buf[..n]);
    put(b")\n");
    libc::close(fd);
}

#[cfg(target_os = "linux")]
fn itoa(mut n: i32, buf: &mut [u8; 32]) -> usize {
    if n == 0 {
        buf[0] = b'0';
        return 1;
    }
    let neg = n < 0;
    if neg {
        n = -n;
    }
    let mut digits = [0u8; 16];
    let mut k = 0;
    while n > 0 {
        digits[k] = b'0' + (n % 10) as u8;
        n /= 10;
        k += 1;
    }
    let mut len = 0;
    if neg {
        buf[0] = b'-';
        len = 1;
    }
    for i in (0..k).rev() {
        buf[len] = digits[i];
        len += 1;
    }
    len
}

/// One change the upper layer holds, in the order it is applied.
#[derive(Debug)]
pub enum Change {
    /// A directory the hop made (or wrote into): made if missing.
    Dir { real: PathBuf, mode: u32 },
    /// A directory the hop replaced whole (overlayfs marks it opaque): the
    /// real one goes, the upper's contents come.
    Replaced { real: PathBuf },
    /// A file the hop wrote: the real one takes the upper's bytes.
    File { real: PathBuf, upper: PathBuf },
    /// A link the hop made or retargeted.
    Link { real: PathBuf, target: PathBuf },
    /// A path the hop removed (a whiteout).
    Removed { real: PathBuf },
}

impl Change {
    pub fn real(&self) -> &Path {
        match self {
            Change::Dir { real, .. }
            | Change::Replaced { real }
            | Change::File { real, .. }
            | Change::Link { real, .. }
            | Change::Removed { real } => real,
        }
    }
}

/// Put one change of a layer in place on the real disk: a directory made
/// with its mode, a path removed, a file's bytes copied with its mode and
/// modification time, a link made. Shared by the pack's apply (journaled
/// first) and a contained hop's home writes applied on request.
pub fn place(c: &Change) -> io::Result<()> {
    let real = c.real().to_path_buf();
    match c {
        Change::Dir { mode, .. } => {
            fs::create_dir_all(&real)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&real, fs::Permissions::from_mode(*mode))?;
            }
            #[cfg(not(unix))]
            let _ = mode;
            Ok(())
        }
        Change::Replaced { .. } | Change::Removed { .. } => {
            crate::burn::journal::remove_any_pub(&real)
        }
        Change::File { upper, .. } => {
            if fs::symlink_metadata(&real)
                .map(|m| !m.is_file())
                .unwrap_or(false)
            {
                crate::burn::journal::remove_any_pub(&real)?;
            }
            if let Some(parent) = real.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(upper, &real)?;
            let meta = fs::metadata(upper)?;
            #[cfg(unix)]
            fs::set_permissions(&real, meta.permissions())?;
            if let Some(ns) = crate::burn::journal::mtime_ns(&meta) {
                let _ = crate::burn::journal::set_mtime(&real, ns);
            }
            Ok(())
        }
        Change::Link { target, .. } => {
            crate::burn::journal::remove_any_pub(&real)?;
            crate::burn::journal::make_symlink_pub(target, &real)
        }
    }
}

/// Is this entry overlayfs's whiteout: a character device 0/0?
#[cfg(target_os = "linux")]
fn is_whiteout(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    meta.file_type().is_char_device() && meta.rdev() == 0
}

/// Does overlayfs mark this upper directory opaque (the lower hidden)?
#[cfg(target_os = "linux")]
fn is_opaque(dir: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
        return false;
    };
    let mut buf = [0u8; 8];
    let n = unsafe {
        libc::getxattr(
            c.as_ptr(),
            c"user.overlay.opaque".as_ptr(),
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len(),
        )
    };
    n > 0 && buf[0] == b'y'
}

/// Read one root's upper layer into the changes to apply, parents before
/// children, in path order.
pub fn changes(root: &Path, upper: &Path) -> io::Result<Vec<Change>> {
    let mut out = Vec::new();
    walk_upper(root, upper, upper, &mut out)?;
    Ok(out)
}

fn walk_upper(root: &Path, upper: &Path, dir: &Path, out: &mut Vec<Change>) -> io::Result<()> {
    let mut entries: Vec<fs::DirEntry> = fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        let rel = p.strip_prefix(upper).unwrap_or(&p);
        let real = root.join(rel);
        let meta = fs::symlink_metadata(&p)?;
        #[cfg(target_os = "linux")]
        if is_whiteout(&meta) {
            out.push(Change::Removed { real });
            continue;
        }
        if meta.file_type().is_symlink() {
            out.push(Change::Link {
                real,
                target: fs::read_link(&p)?,
            });
        } else if meta.is_dir() {
            #[cfg(target_os = "linux")]
            let opaque = is_opaque(&p);
            #[cfg(not(target_os = "linux"))]
            let opaque = false;
            if opaque {
                // The real directory goes whole; the upper's is made in its
                // place (empty, when the hop left it so) and filled below.
                out.push(Change::Replaced { real: real.clone() });
            }
            out.push(Change::Dir {
                real: real.clone(),
                mode: mode_of(&meta),
            });
            walk_upper(root, upper, &p, out)?;
        } else if meta.is_file() {
            out.push(Change::File { real, upper: p });
        }
        // Anything else (a socket, a pipe) a hop left in its layer is not a
        // file of the pack and is dropped.
    }
    Ok(())
}

fn mode_of(meta: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o7777
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        0o755
    }
}

/// A path a hop chose, fit to print: control characters and the invisible
/// ones that reorder or hide text (bidi controls, zero-widths) shown as `?`,
/// and capped, so a file name cannot move the cursor or disguise itself.
pub fn printable_path(p: &str) -> String {
    let mut out: String = p
        .chars()
        .take(200)
        .map(|c| {
            let hidden = c.is_control()
                || matches!(c as u32, 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2069 | 0xFEFF | 0xE0000..=0xE007F);
            if hidden {
                '?'
            } else {
                c
            }
        })
        .collect();
    if p.chars().count() > 200 {
        out.push('…');
    }
    out
}

/// The home layer of a contained hop: the real home and the layer's upper,
/// when the box made one.
#[cfg(target_os = "linux")]
pub fn home_layer_of(layers_dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let upper = Contain::scratch_of(layers_dir)?.join("home-upper");
    let home = real_home()?;
    upper.is_dir().then_some((home, upper))
}

#[cfg(not(target_os = "linux"))]
pub fn home_layer_of(_layers_dir: &Path) -> Option<(PathBuf, PathBuf)> {
    None
}

/// A contained hop's scratch folder, when it has one.
#[cfg(target_os = "linux")]
pub fn scratch_of(layers_dir: &Path) -> Option<PathBuf> {
    Contain::scratch_of(layers_dir)
}

#[cfg(not(target_os = "linux"))]
pub fn scratch_of(_layers_dir: &Path) -> Option<PathBuf> {
    None
}

/// A change a hop's layer holds that it was not expected to make.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unexpected {
    /// Relative to its pack root, or `~/...` for the home.
    pub path: String,
    /// `write`, `link`, `delete`, `replace`; `link-out` for a link that
    /// leaves every pack root, `setuid` for a set-id file, `unreadable` for
    /// a folder the hop made that cannot be read (and so cannot be judged).
    pub kind: &'static str,
}

/// What a hop's layers changed, judged against `expect`.
#[derive(Clone, Debug, Default)]
pub struct Judged {
    pub outside: Vec<Unexpected>,
    pub written: usize,
    pub removed: usize,
    pub home_written: usize,
    pub home_removed: usize,
}

/// Judge a hop's layers against `expect` (globs relative to a pack root, or
/// `~/` for the home; `*` stays within one folder, `**` crosses them), and,
/// when there is one, the home layer (`home`: the real home and the layer's
/// upper). A link that leaves every pack root and a set-id file are reported
/// whatever `expect` says; a folder that cannot be read is reported, never
/// skipped, so a hop cannot hide its writes behind its own modes.
pub fn judge(layers: &Layers, home: Option<(&Path, &Path)>, expect: &[String]) -> Judged {
    let build = |pats: Vec<&str>| {
        let mut b = globset::GlobSetBuilder::new();
        for p in pats {
            if let Ok(g) = globset::GlobBuilder::new(p.trim_start_matches("./"))
                .literal_separator(true)
                .build()
            {
                b.add(g);
            }
        }
        b.build().unwrap_or_else(|_| globset::GlobSet::empty())
    };
    let pack = build(
        expect
            .iter()
            .filter(|e| !e.starts_with("~/"))
            .map(String::as_str)
            .collect(),
    );
    let home_set = build(expect.iter().filter_map(|e| e.strip_prefix("~/")).collect());
    let roots: Vec<PathBuf> = layers.roots.iter().map(|(r, _, _)| r.clone()).collect();
    let home_dir: Option<PathBuf> = home
        .map(|(h, _)| h.to_path_buf())
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from));
    let mut j = Judged::default();
    let look =
        |root: &Path, upper: &Path, set: &globset::GlobSet, in_home: bool, j: &mut Judged| {
            // A link may land in this tree, in any pack root, in the home
            // where `expect` names (a package manager's store), or outside
            // the home altogether (a venv's python, a toolchain): what it
            // must not do is point into the rest of your home, where the
            // secrets are.
            let fine = |p: &Path| {
                if p.starts_with(root) || roots.iter().any(|r| p.starts_with(r)) {
                    return true;
                }
                match &home_dir {
                    Some(h) if p.starts_with(h) => p
                        .strip_prefix(h)
                        .map(|rel| home_set.is_match(rel))
                        .unwrap_or(false),
                    _ => true,
                }
            };
            let mut entries = Vec::new();
            walk_tolerant(root, upper, upper, &fine, &mut entries);
            for (real, kind) in entries {
                let rel = real.strip_prefix(root).unwrap_or(&real).to_path_buf();
                let shown = if in_home {
                    format!("~/{}", rel.display())
                } else {
                    rel.display().to_string()
                };
                match kind {
                    "dir" => continue,
                    "delete" => {
                        if in_home {
                            j.home_removed += 1
                        } else {
                            j.removed += 1
                        }
                    }
                    _ => {
                        if in_home {
                            j.home_written += 1
                        } else {
                            j.written += 1
                        }
                    }
                }
                let always = matches!(kind, "link-out" | "setuid" | "unreadable");
                if always || !set.is_match(&rel) {
                    j.outside.push(Unexpected { path: shown, kind });
                }
            }
        };
    for (root, upper, _) in &layers.roots {
        look(root, upper, &pack, false, &mut j);
    }
    if let Some((h, upper)) = home {
        look(h, upper, &home_set, true, &mut j);
    }
    j
}

/// Walk an upper layer into (real path, kind), never stopping at a folder
/// it cannot read: that folder is itself the finding.
fn walk_tolerant(
    root: &Path,
    upper: &Path,
    dir: &Path,
    fine: &dyn Fn(&Path) -> bool,
    out: &mut Vec<(PathBuf, &'static str)>,
) {
    let rel = dir.strip_prefix(upper).unwrap_or(dir);
    let Ok(rd) = fs::read_dir(dir) else {
        out.push((root.join(rel), "unreadable"));
        return;
    };
    let mut entries: Vec<fs::DirEntry> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        let rel = p.strip_prefix(upper).unwrap_or(&p);
        let real = root.join(rel);
        let Ok(meta) = fs::symlink_metadata(&p) else {
            out.push((real, "unreadable"));
            continue;
        };
        #[cfg(target_os = "linux")]
        if is_whiteout(&meta) {
            out.push((real, "delete"));
            continue;
        }
        if meta.file_type().is_symlink() {
            let target = fs::read_link(&p).unwrap_or_default();
            let lands = if target.is_absolute() {
                lexical(&target)
            } else {
                lexical(&real.parent().unwrap_or(root).join(&target))
            };
            out.push((real, if fine(&lands) { "link" } else { "link-out" }));
        } else if meta.is_dir() {
            #[cfg(target_os = "linux")]
            if is_opaque(&p) {
                out.push((real.clone(), "replace"));
            }
            out.push((real, "dir"));
            walk_tolerant(root, upper, &p, fine, out);
        } else if meta.is_file() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if meta.permissions().mode() & 0o6000 != 0 {
                    out.push((real, "setuid"));
                    continue;
                }
            }
            out.push((real, "write"));
        }
    }
}

/// A path with `.` and `..` worked out, without touching the disk.
fn lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// What a layer holds, as one SHA-256: each change's kind, path, and its
/// bytes, link target or mode, in path order. A kept layer is applied only
/// while it still holds exactly what was judged.
pub fn layer_digest(layers: &Layers) -> io::Result<String> {
    let pairs: Vec<(&Path, &Path)> = layers
        .roots
        .iter()
        .map(|(r, u, _)| (r.as_path(), u.as_path()))
        .collect();
    let fine = |p: &Path| layers.roots.iter().any(|(r, _, _)| p.starts_with(r));
    digest_of(&pairs, &fine)
}

/// A contained hop's home layer, digested the way `layer_digest` digests
/// the pack's: none when the hop had no home layer (not contained, or not
/// Linux).
pub fn home_digest(layers_dir: &Path) -> io::Result<Option<String>> {
    let Some((home, upper)) = home_layer_of(layers_dir) else {
        return Ok(None);
    };
    let fine = |p: &Path| p.starts_with(&home);
    digest_of(&[(home.as_path(), upper.as_path())], &fine).map(Some)
}

fn digest_of(pairs: &[(&Path, &Path)], fine: &dyn Fn(&Path) -> bool) -> io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for (i, (root, upper)) in pairs.iter().enumerate() {
        let (root, upper) = (*root, *upper);
        let mut entries = Vec::new();
        walk_tolerant(root, upper, upper, fine, &mut entries);
        for (real, kind) in entries {
            if kind == "unreadable" {
                return Err(io::Error::other(format!(
                    "{} cannot be read, so the layer cannot be judged",
                    real.display()
                )));
            }
            let rel = real.strip_prefix(root).unwrap_or(&real);
            h.update(format!("{i}\0{kind}\0{}\0", rel.display()).as_bytes());
            let up = upper.join(rel);
            match kind {
                "write" | "setuid" => {
                    let mut f = fs::File::open(&up)?;
                    io::copy(&mut f, &mut h)?;
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        h.update(
                            fs::symlink_metadata(&up)?
                                .permissions()
                                .mode()
                                .to_le_bytes(),
                        );
                    }
                }
                "link" | "link-out" => {
                    h.update(fs::read_link(&up)?.as_os_str().as_encoded_bytes());
                }
                _ => {}
            }
            h.update(b"\n");
        }
    }
    Ok(hex::encode(h.finalize()))
}

/// Remove the scratch folders of a run's contained hops whose layers are
/// not kept (the hop is over, or its cig died).
#[cfg(target_os = "linux")]
pub fn sweep_scratch(run_dir: &Path) {
    let uid = unsafe { libc::geteuid() };
    let prefix = format!("cig-box-{uid}-{}-", dir_name(run_dir));
    for base in scratch_bases() {
        let Ok(rd) = fs::read_dir(&base) else {
            continue;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if let Some(seq) = name.strip_prefix(&prefix) {
                if !is_kept(&run_dir.join("box").join(seq)) {
                    let _ = remove_tree_force(&e.path());
                }
            }
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn a_layer_is_judged_against_its_footprint() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("p");
        let upper = t.path().join("upper");
        let home = t.path().join("h");
        let hup = t.path().join("hup");
        for d in [&root, &upper, &home, &hup] {
            fs::create_dir_all(d).unwrap();
        }
        fs::create_dir_all(upper.join("node_modules/x")).unwrap();
        fs::write(upper.join("node_modules/x/index.js"), "m").unwrap();
        fs::create_dir_all(upper.join(".git/hooks")).unwrap();
        fs::write(upper.join(".git/hooks/pre-commit"), "evil").unwrap();
        fs::write(upper.join("a.json"), "{}").unwrap();
        fs::create_dir_all(upper.join("sub")).unwrap();
        fs::write(upper.join("sub/b.json"), "{}").unwrap();
        symlink(
            home.join(".ssh/id_ed25519"),
            upper.join("node_modules/escape"),
        )
        .unwrap();
        symlink("/usr/bin/python3", upper.join("node_modules/venv-python")).unwrap();
        symlink(
            home.join(".npm/_cacache/x"),
            upper.join("node_modules/store"),
        )
        .unwrap();
        symlink("x/index.js", upper.join("node_modules/inside")).unwrap();
        fs::write(upper.join("node_modules/suid"), "s").unwrap();
        fs::set_permissions(
            upper.join("node_modules/suid"),
            fs::Permissions::from_mode(0o4755),
        )
        .unwrap();
        fs::write(hup.join(".bashrc"), "evil").unwrap();
        fs::create_dir_all(hup.join(".npm")).unwrap();
        fs::write(hup.join(".npm/cache"), "c").unwrap();
        let layers = Layers {
            dir: t.path().join("box/1"),
            roots: vec![(root.clone(), upper.clone(), t.path().join("work"))],
        };
        let expect: Vec<String> = ["node_modules/**", "*.json", "~/.npm/**"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let j = judge(&layers, Some((&home, &hup)), &expect);
        let got: Vec<(String, &str)> = j.outside.iter().map(|u| (u.path.clone(), u.kind)).collect();
        assert!(
            got.contains(&(".git/hooks/pre-commit".to_string(), "write")),
            "{got:?}"
        );
        assert!(got.contains(&("~/.bashrc".to_string(), "write")), "{got:?}");
        assert!(
            got.contains(&("node_modules/escape".to_string(), "link-out")),
            "{got:?}"
        );
        assert!(
            got.contains(&("node_modules/suid".to_string(), "setuid")),
            "{got:?}"
        );
        // `*` stays in one folder: sub/b.json is not `*.json`.
        assert!(
            got.contains(&("sub/b.json".to_string(), "write")),
            "{got:?}"
        );
        assert!(
            !got.iter()
                .any(|(p, _)| p == "a.json" || p == "node_modules/inside" || p == "~/.npm/cache"),
            "{got:?}"
        );
        // A link to a system path (a venv's python) or into an expected store is fine.
        assert!(
            !got.iter()
                .any(|(p, _)| p == "node_modules/venv-python" || p == "node_modules/store"),
            "{got:?}"
        );
        assert_eq!(j.home_written, 2);
    }

    #[test]
    fn a_layer_digest_changes_with_its_contents() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("p");
        let upper = t.path().join("upper");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&upper).unwrap();
        fs::write(upper.join("a.txt"), "one").unwrap();
        let layers = Layers {
            dir: t.path().join("box/1"),
            roots: vec![(root, upper.clone(), t.path().join("work"))],
        };
        let first = layer_digest(&layers).unwrap();
        assert_eq!(first, layer_digest(&layers).unwrap());
        fs::write(upper.join("a.txt"), "two").unwrap();
        assert_ne!(first, layer_digest(&layers).unwrap());
    }

    #[test]
    fn a_hop_chosen_path_prints_plainly() {
        assert_eq!(
            printable_path("node_modules/@scope/pkg/a b.js"),
            "node_modules/@scope/pkg/a b.js"
        );
        assert_eq!(printable_path("x\u{1b}[2Jy"), "x?[2Jy");
        assert_eq!(printable_path("evil\u{202E}gpj.exe"), "evil?gpj.exe");
    }

    #[test]
    fn the_hops_filter_checks_the_architecture_first() {
        let f = hop_filter().expect("a filter on this architecture");
        // Load the architecture, compare it, kill on a mismatch.
        assert_eq!(f[0].code, 0x20);
        assert_eq!(f[0].k, 4);
        assert_eq!(f[1].code, 0x15);
        assert_eq!(f[2].k, 0x8000_0000);
        // Every jump lands inside the program.
        for (i, ins) in f.iter().enumerate() {
            if ins.code & 0x07 == 0x05 {
                assert!(
                    i + 1 + (ins.jt.max(ins.jf) as usize) < f.len(),
                    "jump {i} leaves the program"
                );
            }
        }
        assert!(f.len() < 255);
    }

    #[test]
    fn the_guard_hides_the_tin_and_the_runs_and_keeps_the_hop_journal() {
        let home = PathBuf::from("/home/u/.cigscript");
        let run_dir = home.join("runs/20261108T120000-abcdef");
        let h = hide_plan(&home, &run_dir, 3);
        // `tin/` and `runs/` go under empty tmpfs mounts.
        assert_eq!(h.tin_dir, PathBuf::from("/home/u/.cigscript/tin"));
        assert_eq!(h.runs_dir, PathBuf::from("/home/u/.cigscript/runs"));
        // The one file still writable under them is the burn-protocol
        // journal cig is about to point the hop at (`CIG_JOURNAL`), and
        // its parent directories are made in the fresh tmpfs outermost
        // first so a bind of the real file has a path to land on.
        assert_eq!(
            h.hop_journal,
            PathBuf::from("/home/u/.cigscript/runs/20261108T120000-abcdef/hops/3.jsonl")
        );
        assert_eq!(
            h.mkdirs,
            vec![
                PathBuf::from("/home/u/.cigscript/runs/20261108T120000-abcdef"),
                PathBuf::from("/home/u/.cigscript/runs/20261108T120000-abcdef/hops"),
            ]
        );
        // The inbox lives outside `runs/`, so the hide step never covers
        // it: a cig started inside the hop keeps the inbox as its own
        // state root. The guard binds the inbox writable, separately.
        let inbox = inbox_dir(&run_dir);
        assert!(
            !inbox.starts_with(&h.runs_dir) && !inbox.starts_with(&h.tin_dir),
            "the inbox {} would be hidden by the tmpfs over {}",
            inbox.display(),
            h.runs_dir.display(),
        );
    }

    #[test]
    fn the_bus_paths_come_out_of_an_address() {
        assert_eq!(
            bus_paths("unix:path=/run/user/1000/bus"),
            vec![PathBuf::from("/run/user/1000/bus")]
        );
        assert_eq!(
            bus_paths("unix:abstract=/tmp/dbus-x,guid=1;unix:path=/tmp/a%20b/bus,guid=2"),
            vec![PathBuf::from("/tmp/a b/bus")]
        );
        assert!(bus_paths("tcp:host=localhost,port=1").is_empty());
    }
}
