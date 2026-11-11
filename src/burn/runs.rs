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
        if let Ok(j) = super::journal::Journal::load(&self.dir()) {
            let burns = j.len();
            let irreversible = j.entries().iter().filter(|e| !e.reversible).count();
            if burns > self.burns {
                self.burns = burns;
                self.irreversible = irreversible;
            }
        }
        if self.status == "running"
            && !self
                .pid
                .map(|pid| pid_alive(pid, self.pid_started.as_deref()))
                .unwrap_or(false)
        {
            self.status = "interrupted".to_string();
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
        // The format is read first: a newer cig may have added fields this
        // one does not know, and that is a newer record, not a damaged one.
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
        let mut rec: Self = serde_json::from_str(&text)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        rec.loaded_from = Some(run_dir.to_path_buf());
        Ok(rec)
    }

    /// The version that wrote this record, for handing the run back to it.
    pub fn written_by(&self) -> Option<crate::update::Version> {
        crate::update::Version::parse(&self.version)
    }
}

/// The error for a record a newer cig wrote: `io::ErrorKind::Unsupported`,
/// so a caller can tell it from damage and hand the run to that version.
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
            use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
            use windows_sys::Win32::System::Threading::{
                GetProcessTimes, OpenProcess, QueryFullProcessImageNameW,
                PROCESS_QUERY_LIMITED_INFORMATION,
            };
            // SAFETY: plain Win32 calls; the handle is closed before returning.
            unsafe {
                let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
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
    let Ok(entries) = fs::read_dir(runs_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        let layers = dir.join("box");
        if !layers.is_dir() {
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
    // Asked before every run, so it reads each run's record alone and
    // never a journal: everything it needs (the script's hash, the
    // status, the mode, the version) is in the record, and a history
    // of a thousand runs must not make every start slower. The run
    // index (#149) does not stand in for this scan because a folder
    // scan here keeps the FIFO invariant (`cig run` opens no other
    // run's journal, even one hand-written beside the index).
    let mut records: Vec<RunRecord> = fs::read_dir(runs_dir())
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| RunRecord::load(&e.path()).ok())
        .filter(|r| r.script_sha256 == script_sha256 && r.status == "ok" && r.mode == "run")
        .collect();
    records.sort_by(|a, b| b.started.cmp(&a.started).then_with(|| b.id.cmp(&a.id)));
    records
        .into_iter()
        .find_map(|r| r.written_by().map(|v| (v, r.id.clone())))
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
