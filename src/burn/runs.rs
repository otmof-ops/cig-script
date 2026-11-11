// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Run records: one directory per `cig run`, under `$CIGSCRIPT_HOME/runs`.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Where CigScript keeps its state. `CIGSCRIPT_HOME` overrides the default
/// of `~/.cigscript`.
pub fn home_dir() -> PathBuf {
    // A unit test that never named a home gets one of its own, so what the
    // kernel keeps in the tin (native snapshots live there now) never lands
    // in the developer's real one.
    #[cfg(test)]
    {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            if std::env::var_os("CIGSCRIPT_HOME").is_none() {
                let dir =
                    std::env::temp_dir().join(format!("cig-unit-home-{}", std::process::id()));
                let _ = fs::create_dir_all(&dir);
                std::env::set_var("CIGSCRIPT_HOME", dir);
            }
        });
    }
    if let Some(p) = std::env::var_os("CIGSCRIPT_HOME") {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".cigscript")
}

pub fn runs_dir() -> PathBuf {
    home_dir().join("runs")
}

/// The marker a pinned run's folder holds (#149): retention and `--prune`
/// keep the run whatever they say. A file, not a field, so the run's
/// record keeps its format and an older cig reads the folder as before.
pub const PIN_FILE: &str = "pinned";

/// The shape of `run.json` this cig writes. A newer cig reads every older
/// shape; an older cig refuses a newer one by its number instead of
/// guessing (E808).
pub const RECORD_FORMAT: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunRecord {
    pub id: String,
    /// The record's format: 0 for records from before 1.2.0, which carried none.
    #[serde(default)]
    pub format: u32,
    pub version: String,
    pub script: String,
    pub script_sha256: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub mode: String,
    pub started: String,
    #[serde(default)]
    pub finished: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub error: Option<String>,
    /// The failing diagnostic in its `--json` form, so `cig doctor <run>`
    /// can diagnose it again later, elsewhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<serde_json::Value>,
    #[serde(default)]
    pub burns: usize,
    /// The run was asked to leave its burns in place on failure
    /// (`--no-rollback`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_rollback: bool,
    #[serde(default)]
    pub irreversible: usize,
    #[serde(default)]
    pub rolled_back: bool,
    /// What the automatic rollback could not restore, one line each; the
    /// status is `rollback_incomplete` when this is not empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rollback_failed: Vec<String>,
    /// How many files the rollback moved into the tin instead of
    /// destroying (`cig unburn <id> --give-back` puts them back).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub rollback_took: usize,
    /// The pack the run declared, so `cig unburn` can take its lock.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pack: Vec<PathBuf>,
    /// The head of the journal's hash chain when this record was last
    /// saved: the hash of its last line. `cig runs <id> --verify` compares
    /// it with the journal on disk, so lines added or removed since show.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_head: Option<String>,
    /// Set when the run came here in a parcel: where from, when, and how
    /// its paths map to this machine (the journal reads through the map).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported: Option<serde_json::Value>,
    /// Set when this run was a cig inside another run's hop: that run's id.
    /// Its changes reached the pack through the hop's layer and are in the
    /// parent's journal; a child is unburned through its parent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Process id of the run, to tell an interrupted run from a live one.
    #[serde(default)]
    pub pid: Option<u32>,
    /// When that process started, as the platform reports it, so a reused
    /// pid never passes for the run that died: Linux's `/proc` start ticks,
    /// `ps -o lstart` on macOS and the BSDs, the creation time on Windows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_started: Option<String>,
    /// What the trial found before this run: the versions compared, whether
    /// their plans agreed, and what was done about it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial: Option<serde_json::Value>,
    /// Set when a compiled program made the run (see `crate::program`): its
    /// name, the script it was compiled from, sha256, the compiler, the cig
    /// it runs on, and the binary. `script` is then the program's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<serde_json::Value>,
    /// The directory this record was read from, which is where its journal
    /// is even when the directory no longer matches the id.
    #[serde(skip)]
    pub loaded_from: Option<PathBuf>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl RunRecord {
    /// A fresh record with a unique, sortable id: `YYYYMMDDTHHMMSS-<hash>`.
    /// A dry run writes no record, so it does not ask the platform for its
    /// own start time (a process to spawn on some of them).
    pub fn begin(script: &Path, source: &str, args: &[String], mode: &str) -> Self {
        use sha2::{Digest, Sha256};
        let now = chrono::Utc::now();
        let sha = hex::encode(Sha256::digest(source.as_bytes()));
        let mut salt = Sha256::new();
        salt.update(now.timestamp_nanos_opt().unwrap_or(0).to_le_bytes());
        salt.update(std::process::id().to_le_bytes());
        salt.update(script.to_string_lossy().as_bytes());
        let salt = hex::encode(salt.finalize());
        Self {
            id: format!("{}-{}", now.format("%Y%m%dT%H%M%S"), &salt[..6]),
            format: RECORD_FORMAT,
            version: crate::VERSION.to_string(),
            script: script.to_string_lossy().to_string(),
            script_sha256: sha,
            args: args.to_vec(),
            cwd: std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default(),
            mode: mode.to_string(),
            started: now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            finished: None,
            status: "running".to_string(),
            error: None,
            burns: 0,
            irreversible: 0,
            diagnostic: None,
            rolled_back: false,
            rollback_failed: Vec::new(),
            rollback_took: 0,
            pack: Vec::new(),
            journal_head: None,
            imported: None,
            parent: None,
            pid: Some(std::process::id()),
            pid_started: if mode == "dry-run" {
                None
            } else {
                process_started(std::process::id())
            },
            trial: None,
            program: None,
            no_rollback: false,
            loaded_from: None,
        }
    }

    /// A record for a run directory whose `run.json` cannot be read. The
    /// journal beside it is what matters; nothing about the run is claimed
    /// beyond its id, and `cig unburn` can still reach it.
    pub fn damaged(id: &str, e: &io::Error) -> Self {
        // The id carries the start time, so the run still sorts in order.
        let started =
            chrono::NaiveDateTime::parse_from_str(id.get(..15).unwrap_or(id), "%Y%m%dT%H%M%S")
                .map(|t| {
                    t.and_utc()
                        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                })
                .unwrap_or_else(|_| id.to_string());
        Self {
            id: id.to_string(),
            format: 0,
            version: String::new(),
            script: String::new(),
            script_sha256: String::new(),
            args: Vec::new(),
            cwd: String::new(),
            mode: "run".to_string(),
            started,
            finished: None,
            status: "damaged".to_string(),
            error: Some(format!("run.json is damaged: {e}")),
            burns: 0,
            irreversible: 0,
            diagnostic: None,
            rolled_back: false,
            rollback_failed: Vec::new(),
            rollback_took: 0,
            pack: Vec::new(),
            journal_head: None,
            imported: None,
            parent: None,
            pid: None,
            pid_started: None,
            trial: None,
            program: None,
            no_rollback: false,
            loaded_from: None,
        }
    }

    /// Count burns from the journal, which is authoritative even when the
    /// run never got to write its totals, and recognise a run whose process
    /// died mid-burn.
    pub fn refresh_from_journal(&mut self) {
        self.count_burns_from_journal();
        if self.status == "running"
            && !self
                .pid
                .map(|pid| pid_alive(pid, self.pid_started.as_deref()))
                .unwrap_or(false)
        {
            self.status = "interrupted".to_string();
        }
    }

    /// `refresh_from_journal` for a record whose process the caller has
    /// already found dead: the process is not asked about a second time.
    pub(crate) fn refresh_from_journal_known_dead(&mut self) {
        self.count_burns_from_journal();
        if self.status == "running" {
            self.status = "interrupted".to_string();
        }
    }

    fn count_burns_from_journal(&mut self) {
        if let Ok(j) = super::journal::Journal::load(&self.dir()) {
            let burns = j.len();
            let irreversible = j.entries().iter().filter(|e| !e.reversible).count();
            if burns > self.burns {
                self.burns = burns;
                self.irreversible = irreversible;
            }
        }
    }

    pub fn is_interrupted(&self) -> bool {
        self.status == "interrupted"
    }

    pub fn dir(&self) -> PathBuf {
        self.loaded_from
            .clone()
            .unwrap_or_else(|| runs_dir().join(&self.id))
    }

    pub fn save(&self) -> io::Result<()> {
        // A record is never written ahead of the snapshots it stands for.
        super::tin::sync_deferred()?;
        let dir = self.dir();
        fs::create_dir_all(&dir)?;
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        // Write, sync, then rename, and sync the directory, so a crash never
        // leaves a torn record and a power cut never loses a finished one.
        let tmp = dir.join("run.json.tmp");
        fs::write(&tmp, json)?;
        super::journal::sync_file(&tmp)?;
        fs::rename(tmp, dir.join("run.json"))?;
        super::journal::sync_dir_pub(&dir)?;
        // Keep the run index in step (#149). A failure here is noted but
        // does not fail the save: the folder is the source of truth, and
        // the index is rebuilt from it when it is missing or torn.
        super::runs_index::upsert(self);
        Ok(())
    }

    pub fn finish(&mut self, status: &str, error: Option<String>) {
        self.finished =
            Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
        self.status = status.to_string();
        self.error = error;
    }

    pub fn load(run_dir: &Path) -> io::Result<Self> {
        let text = fs::read_to_string(run_dir.join("run.json"))?;
        #[cfg(test)]
        LOADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // One parse, into the record. The format is judged on what came
        // out of it: a newer cig may have added fields this one does not
        // know, and that is a newer record, not a damaged one. Only a
        // record that does not read as ours is parsed again, loosely, to
        // tell a newer one from a damaged one.
        match serde_json::from_str::<Self>(&text) {
            Ok(mut rec) if rec.format <= RECORD_FORMAT => {
                rec.loaded_from = Some(run_dir.to_path_buf());
                Ok(rec)
            }
            Ok(rec) => Err(newer_format("run.json", rec.format, &rec.version)),
            Err(e) => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                    let format = v["format"].as_u64().unwrap_or(0) as u32;
                    if format > RECORD_FORMAT {
                        return Err(newer_format(
                            "run.json",
                            format,
                            v["version"].as_str().unwrap_or(""),
                        ));
                    }
                }
                Err(io::Error::new(io::ErrorKind::InvalidData, e))
            }
        }
    }

    /// The version that wrote this record, for handing the run back to it.
    pub fn written_by(&self) -> Option<crate::update::Version> {
        crate::update::Version::parse(&self.version)
    }
}

/// The error for a record a newer cig wrote: `io::ErrorKind::Unsupported`,
/// so a caller can tell it from damage and hand the run to that version.
/// `run.json` files read, counted for the tests that pin how many a start
/// may read.
#[cfg(test)]
pub static LOADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn newer_format(what: &str, format: u32, version: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        format!(
            "{what} is format {format}, written by cig {}; this cig ({}) reads up to format {}{}",
            if version.is_empty() { "?" } else { version },
            crate::VERSION,
            RECORD_FORMAT,
            if what == "journal.jsonl" {
                " (E711)"
            } else {
                ""
            }
        ),
    )
}

/// The name and start time of a live process, as this platform tells them:
/// `(name, started)`, or None when there is no such process (or it cannot
/// be asked). Read-only: no signal is sent. The start time is an opaque
/// string compared for equality.
fn process_identity(pid: u32) -> Option<(String, String)> {
    #[cfg(target_os = "linux")]
    {
        let comm = fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // Field 22 (starttime) comes after the parenthesised name, which may
        // itself hold spaces and parentheses.
        let after = stat.rsplit_once(')')?.1;
        let started = after.split_whitespace().nth(19)?.to_string();
        Some((comm.trim().to_string(), format!("ticks:{started}")))
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        // Two questions, two answers: the name and the start time each on
        // a line of their own, whatever ps's column layout on this BSD.
        let ask = |column: &str| -> Option<String> {
            // One locale and one zone, so the start time reads the same
            // whichever shell asks: `lstart` is printed through strftime.
            let out = std::process::Command::new("ps")
                .args(["-p", &pid.to_string(), "-o", column])
                .env("LC_ALL", "C")
                .env("TZ", "UTC")
                .output()
                .ok()?;
            if !out.status.success() {
                return None;
            }
            let text = String::from_utf8_lossy(&out.stdout);
            let line = text
                .lines()
                .find(|l| !l.trim().is_empty())?
                .trim()
                .to_string();
            if line.is_empty() {
                None
            } else {
                Some(line)
            }
        };
        let comm = ask("comm=")?;
        let name = std::path::Path::new(&comm)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or(comm.clone());
        let started = ask("lstart=").unwrap_or_default();
        Some((
            name,
            if started.is_empty() {
                String::new()
            } else {
                format!("lstart:{started}")
            },
        ))
    }
    #[cfg(windows)]
    {
        // The kernel is asked directly: no PowerShell to start for every
        // run. The creation time is the FILETIME .NET's `ToFileTimeUtc`
        // gives, so records 1.3.x wrote with PowerShell compare equal.
        {
            use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, FILETIME};
            use windows_sys::Win32::System::Threading::{
                GetProcessTimes, OpenProcess, QueryFullProcessImageNameW,
                PROCESS_QUERY_LIMITED_INFORMATION,
            };
            // SAFETY: plain Win32 calls; the handle is closed before returning.
            unsafe {
                let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
                // No such process is the kernel's answer for a pid that has
                // exited: that is a dead run, and neither PowerShell nor
                // tasklist can say otherwise. Only a refusal (access denied
                // for another user's process) goes on to ask them.
                if h.is_null() && no_such_process(GetLastError()) {
                    return None;
                }
                if !h.is_null() {
                    let mut created: FILETIME = std::mem::zeroed();
                    let mut exited: FILETIME = std::mem::zeroed();
                    let mut kernel: FILETIME = std::mem::zeroed();
                    let mut user: FILETIME = std::mem::zeroed();
                    let times_ok =
                        GetProcessTimes(h, &mut created, &mut exited, &mut kernel, &mut user) != 0;
                    let mut buf = vec![0u16; 2048];
                    let mut len = buf.len() as u32;
                    let name_ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len) != 0;
                    CloseHandle(h);
                    // A process that has exited but whose handle someone still
                    // holds is dead for this purpose.
                    if times_ok && (exited.dwLowDateTime != 0 || exited.dwHighDateTime != 0) {
                        return None;
                    }
                    if name_ok {
                        let full = String::from_utf16_lossy(&buf[..len as usize]);
                        let name = std::path::Path::new(&full)
                            .file_stem()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or(full.clone());
                        let started = if times_ok {
                            let ft = (u64::from(created.dwHighDateTime) << 32)
                                | u64::from(created.dwLowDateTime);
                            format!("filetime:{ft}")
                        } else {
                            String::new()
                        };
                        return Some((name, started));
                    }
                }
            }
        }
        // PowerShell knows a process's name and start time; tasklist only
        // its name. Either is better than a bare pid.
        let script = format!(
            "$p = Get-Process -Id {pid} -ErrorAction SilentlyContinue; if ($p) {{ $p.ProcessName + '|' + $p.StartTime.ToFileTimeUtc() }}"
        );
        let out = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output();
        if let Ok(o) = out {
            let text = String::from_utf8_lossy(&o.stdout);
            let line = text.trim();
            if let Some((name, started)) = line.split_once('|') {
                return Some((
                    name.trim().to_string(),
                    format!("filetime:{}", started.trim()),
                ));
            }
            if o.status.success() && line.is_empty() {
                return None;
            }
        }
        let out = std::process::Command::new("tasklist")
            .args(["/FO", "CSV", "/NH", "/FI", &format!("PID eq {pid}")])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let line = text.lines().find(|l| l.starts_with('"'))?;
        let name = line.split('"').nth(1)?.to_string();
        Some((name, String::new()))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        None
    }
}

/// `ERROR_INVALID_PARAMETER`, what `OpenProcess` sets for a process id that
/// names no process (it exited, or never was one).
const ERROR_INVALID_PARAMETER: u32 = 87;

/// Is this `OpenProcess` error the kernel saying the process is gone?
#[cfg_attr(not(windows), allow(dead_code))]
fn no_such_process(last_error: u32) -> bool {
    last_error == ERROR_INVALID_PARAMETER
}

/// How many times `pid_alive` was asked, for the tests that pin how often a
/// listing asks it.
#[cfg(test)]
pub static PID_CHECKS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The start time of a process, for the run record (see `pid_alive`).
pub fn process_started(pid: u32) -> Option<String> {
    process_identity(pid)
        .map(|(_, started)| started)
        .filter(|s| !s.is_empty())
}

/// Is the run that recorded `pid` still running? The pid alone is not
/// enough on any platform: it is reused, quickly on Windows. The live
/// process has to be a cig, and when the record kept the start time, the
/// same start time; otherwise the run died and the record is interrupted.
pub fn pid_alive(pid: u32, started: Option<&str>) -> bool {
    #[cfg(test)]
    PID_CHECKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let Some((name, live_started)) = process_identity(pid) else {
        // The asking process is alive whatever the platform says about it.
        return pid == std::process::id();
    };
    if pid == std::process::id() {
        // Our own pid, but a record from another boot or another container
        // may have had it too: the start time decides when both are known.
        return match started {
            Some(recorded) if !recorded.is_empty() && !live_started.is_empty() => {
                recorded == live_started
            }
            _ => true,
        };
    }
    let is_cig = name.to_ascii_lowercase().starts_with("cig");
    match started {
        Some(recorded) if !recorded.is_empty() && !live_started.is_empty() => {
            is_cig && recorded == live_started
        }
        _ => is_cig,
    }
}

/// The layers a boxed hop left when its run died in the hop: overlayfs
/// leaves a directory nobody can read under them, in the way of anything
/// that walks the home (an outer run's snapshot of a tree holding it, a
/// backup). They were never the pack's. Swept when a run starts, for every
/// run that is not alive; a live run's layers are its own.
pub fn sweep_dead_layers() {
    // Only the runs the index flags (written before a run's layers are
    // made), so a start costs the same at ten runs as at ten thousand. An
    // index that cannot be trusted (missing, cut short, or a record added
    // to the folder behind its back) sends the sweep through every folder,
    // as it always did.
    let dirs: Vec<std::path::PathBuf> = match super::runs_index::boxed_runs() {
        Some(ids) => ids.into_iter().map(|id| runs_dir().join(id)).collect(),
        None => match fs::read_dir(runs_dir()) {
            Ok(entries) => entries.flatten().map(|e| e.path()).collect(),
            Err(_) => return,
        },
    };
    for dir in dirs {
        let layers = dir.join("box");
        if !layers.is_dir() {
            // Flagged, and nothing is left: the flag comes off.
            if let Some(id) = dir.file_name().and_then(|n| n.to_str()) {
                if dir.is_dir() {
                    super::runs_index::mark_boxed(id, false);
                }
            }
            continue;
        }
        let alive = RunRecord::load(&dir)
            .map(|mut rec| {
                rec.refresh_from_journal();
                rec.status == "running"
            })
            .unwrap_or(false);
        if !alive {
            // Only what the run never got to apply: a layer kept on purpose
            // (`apply: "never"`, an `ask` declined) waits for `cig box apply`.
            super::overlay::drop_unapplied(&dir);
        }
    }
}

/// Every run, newest first (#149).
///
/// Reads the run index (`$CIGSCRIPT_HOME/runs-index.jsonl`) — one
/// compact file updated as runs start, finish, are pruned, forgotten or
/// unburned — instead of opening every run folder. A hand-edited or
/// hand-deleted record is caught on a cheap stat; the journal of
/// another run is never opened by the fast path.
pub fn list() -> io::Result<Vec<RunRecord>> {
    super::runs_index::list()
}

/// The last good version of a script: the cig that lit the newest run of
/// this exact source (by hash) that went through, with that run's id.
/// Asked before every run, so it reads each run's record alone and never a
/// journal: everything it needs (the script's hash, the status, the mode,
/// the version) is in the record, and a history of a thousand runs must not
/// make every start slower.
pub fn last_good(script_sha256: &str) -> Option<(crate::update::Version, String)> {
    // From the run index (#184): the hash, status and mode of every run
    // are on its lines, so the candidates come off a text scan and only
    // the one that wins has its own `run.json` read, to confirm it. No
    // other run's record or journal is opened. When the index cannot
    // stand in for the folder (see `runs_index::trusted_text`), the
    // folder scan below answers, one parse per record, and the index is
    // brought up to date from what it read.
    let confirm = |id: &str| -> Option<(crate::update::Version, String)> {
        let rec = RunRecord::load(&runs_dir().join(id)).ok()?;
        if rec.script_sha256 != script_sha256 || rec.status != "ok" || rec.mode != "run" {
            return None;
        }
        rec.written_by().map(|v| (v, rec.id.clone()))
    };
    if let Some(answer) = super::runs_index::last_good_candidates(script_sha256, confirm) {
        return answer;
    }
    let all: Vec<RunRecord> = fs::read_dir(runs_dir())
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| RunRecord::load(&e.path()).ok())
        .collect();
    let mut records: Vec<&RunRecord> = all
        .iter()
        .filter(|r| r.script_sha256 == script_sha256 && r.status == "ok" && r.mode == "run")
        .collect();
    records.sort_by(|a, b| b.started.cmp(&a.started).then_with(|| b.id.cmp(&a.id)));
    let hit = records
        .into_iter()
        .find_map(|r| r.written_by().map(|v| (v, r.id.clone())));
    // Bring the index level with the folder, so the next start takes the
    // short way. Best effort.
    let _ = super::runs_index::write_fresh(&all);
    hit
}

/// Resolve a full id or a unique prefix to a run directory.
pub fn find(id_or_prefix: &str) -> io::Result<Option<RunRecord>> {
    let all = list()?;
    if let Some(exact) = all.iter().find(|r| r.id == id_or_prefix) {
        return Ok(Some(exact.clone()));
    }
    let matches: Vec<&RunRecord> = all
        .iter()
        .filter(|r| r.id.starts_with(id_or_prefix))
        .collect();
    match matches.len() {
        0 => Ok(None),
        1 => Ok(Some(matches[0].clone())),
        n => Err(io::Error::other(format!(
            "`{id_or_prefix}` matches {n} runs; give more of the id"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn rec(n: usize, sha: &str, status: &str) -> RunRecord {
        let id = format!("20260101T{:06}-{:06x}", n, n);
        let mut r = RunRecord::damaged(&id, &io::Error::other("stub"));
        r.format = RECORD_FORMAT;
        r.version = "1.20.0".to_string();
        r.script_sha256 = sha.to_string();
        r.status = status.to_string();
        r.started = format!("2026-01-01T00:{:02}:{:02}.000Z", n / 60 % 60, n % 60);
        r.error = None;
        r
    }

    /// Only `ERROR_INVALID_PARAMETER` is "no such process": a refusal
    /// (access denied, 5) means the process is there and the fallbacks are
    /// asked, as before.
    #[test]
    fn only_invalid_parameter_means_the_process_is_gone() {
        assert!(no_such_process(87));
        assert!(!no_such_process(5));
        assert!(!no_such_process(0));
    }

    /// Starting a run reads the same number of run records whatever the
    /// history holds: the index names the candidate and one `run.json` is
    /// read to confirm it. The old scan read every record (#184).
    #[test]
    fn run_start_is_constant_in_history() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = crate::burn::test_env_lock();
        let old = std::env::var_os("CIGSCRIPT_HOME");
        std::env::set_var("CIGSCRIPT_HOME", tmp.path().join("home"));
        let mut reads = Vec::new();
        let mut next = 0;
        for size in [5usize, 60, 240] {
            while next < size {
                // Every record is one of this script's good runs: the worst
                // case for a scan that parses what it matches.
                rec(next, "abc", "ok").save().unwrap();
                next += 1;
            }
            let before = LOADS.load(Ordering::Relaxed);
            let (_, id) = last_good("abc").expect("a good run");
            reads.push(LOADS.load(Ordering::Relaxed) - before);
            assert_eq!(id, format!("20260101T{:06}-{:06x}", size - 1, size - 1));
        }
        assert_eq!(reads, vec![1, 1, 1], "run.json reads by history size");
        // A record an older cig wrote into the folder, never told to the
        // index, still wins: the folder's stamp no longer fits, so the
        // folder is scanned.
        let mut later = rec(999, "abc", "ok");
        later.version = "1.19.0".to_string();
        let dir = runs_dir().join(&later.id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("run.json"), serde_json::to_string(&later).unwrap()).unwrap();
        let (v, id) = last_good("abc").unwrap();
        assert_eq!(
            (v.to_string(), id),
            ("1.19.0".to_string(), later.id.clone())
        );
        // That scan brought the index level: the next start takes the short way.
        let before = LOADS.load(Ordering::Relaxed);
        assert_eq!(last_good("abc").unwrap().1, later.id);
        assert_eq!(LOADS.load(Ordering::Relaxed) - before, 1);
        match old {
            Some(v) => std::env::set_var("CIGSCRIPT_HOME", v),
            None => std::env::remove_var("CIGSCRIPT_HOME"),
        }
    }

    /// The flag that says a run may hold box layers is in the index before
    /// the layers are made, so a run that dies in the hop is still found.
    /// Here the layers cannot be made at all (`box` is a file): the flag is
    /// there anyway, and the sweep visits only that run.
    #[test]
    fn box_flag_before_layers() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = crate::burn::test_env_lock();
        let old = std::env::var_os("CIGSCRIPT_HOME");
        std::env::set_var("CIGSCRIPT_HOME", tmp.path().join("home"));
        for n in 0..4 {
            rec(n, "abc", "ok").save().unwrap();
        }
        assert_eq!(super::super::runs_index::boxed_runs(), Some(vec![]));
        let flagged = rec(2, "abc", "ok");
        let dir = flagged.dir();
        fs::write(dir.join("box"), "not a folder").unwrap();
        let made = super::super::overlay::prepare(&dir, 1, &[tmp.path().to_path_buf()]);
        assert!(made.is_err(), "the layers could not be made");
        assert_eq!(
            super::super::runs_index::boxed_runs(),
            Some(vec![flagged.id.clone()])
        );
        // A real layer under a run that is not running is swept, and the
        // flag comes off with it.
        fs::remove_file(dir.join("box")).unwrap();
        let layers = dir.join("box/1/0/work");
        fs::create_dir_all(&layers).unwrap();
        sweep_dead_layers();
        assert!(!dir.join("box").exists());
        sweep_dead_layers();
        assert_eq!(super::super::runs_index::boxed_runs(), Some(vec![]));
        match old {
            Some(v) => std::env::set_var("CIGSCRIPT_HOME", v),
            None => std::env::remove_var("CIGSCRIPT_HOME"),
        }
    }
}
