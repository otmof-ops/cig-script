// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! The ashtray: where a report lands before anything is sent, like the
//! journal before a burn. Sent in the background, retried with backoff,
//! kept while offline, marked filed once the tracker has it. A run never
//! waits on the network: the most it does is start `cig crash flush` and
//! carry on.

use crate::config::Config;
use crate::crash::{self, Consent, CrashReport, SendOutcome};
use std::io;
use std::path::PathBuf;

/// Seconds to wait before trying again after the n-th failure: the first
/// retry is the next run, then longer each time, a day at most.
pub fn backoff_secs(attempts: u32) -> u64 {
    match attempts {
        0 | 1 => 0,
        2 => 300,
        3 => 1_800,
        4 => 7_200,
        5 => 43_200,
        _ => 86_400,
    }
}

/// Put a report in the ashtray. Nothing is sent here.
pub fn queue(report: &mut CrashReport) -> io::Result<PathBuf> {
    crash::write_new(report)
}

/// The repository a report goes to: its own, when the tool that made it
/// said; cig's otherwise.
pub fn repo_for(cfg: &Config, report: &CrashReport) -> String {
    report
        .repo
        .clone()
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| cfg.get("issues_repo"))
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn is_due(r: &CrashReport) -> bool {
    match &r.queue.next_try {
        None => true,
        Some(t) => chrono::DateTime::parse_from_rfc3339(t)
            .map(|t| t <= chrono::Utc::now())
            .unwrap_or(true),
    }
}

/// Reports waiting to go: not filed yet, asked for (by the person, or by
/// `reports = always`), and past their backoff.
pub fn pending(cfg: &Config) -> Vec<CrashReport> {
    let always = crash::consent(cfg) == Consent::Always;
    crash::list()
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.issue.is_none() && r.sent.is_none() && (r.queue.requested || always))
        .collect()
}

pub fn due(cfg: &Config) -> Vec<CrashReport> {
    pending(cfg).into_iter().filter(is_due).collect()
}

/// What one pass of the ashtray did.
#[derive(Debug, Default)]
pub struct Flushed {
    /// (report id, issue url)
    pub filed: Vec<(String, String)>,
    pub noted: Vec<(String, String)>,
    /// (report id, reason)
    pub failed: Vec<(String, String)>,
    /// Pending reports not due yet.
    pub waiting: usize,
    /// Another sender held the ashtray; nothing was tried.
    pub locked: bool,
}

/// Try to send one report now. A failure is recorded on the report (the
/// reason, the attempt count, when to try again) and the report stays.
pub fn try_send(report: &mut CrashReport, cfg: &Config) -> Result<SendOutcome, String> {
    // One sender at a time, really: a send that went ahead without the lock
    // could file a report a flush is filing too. When another sender keeps
    // it past the wait, this report stays queued, untouched, for the next.
    let Some(_lock) = lock(SEND_WAIT) else {
        return Err(
            "another cig is still sending reports; this one stays in the ashtray and goes with the next send"
                .to_string(),
        );
    };
    // A flush that held the lock may have filed this very report meanwhile.
    if let Ok(Some(fresh)) = crash::find(&report.id) {
        if let Some(issue) = fresh.issue.clone() {
            report.issue = Some(issue.clone());
            report.sent = Some(issue.url.clone());
            return Ok(SendOutcome::Commented(issue.url));
        }
    }
    let repo = repo_for(cfg, report);
    // The repro is proved in a sandbox before it travels; one that does not
    // reproduce goes as a note, never as a repro.
    if let Some(x) = report.repro.as_mut() {
        if !x.verified && x.note.is_empty() {
            let scratch = crash::ashtray_dir().join(format!("repro-{}", report.id));
            crate::repro::verify_and_shrink(x, &scratch);
            let _ = crash::write(report);
        }
    }
    let outcome = crash::send(report, &repo);
    match &outcome {
        Ok(SendOutcome::Filed(_)) | Ok(SendOutcome::Commented(_)) => {
            report.queue.last_error = None;
            report.queue.next_try = None;
            let _ = crash::write(report);
        }
        Ok(SendOutcome::Manual(_)) => {
            // Nothing can send from here; the report keeps waiting for a
            // tool to appear, with the reason written on it.
            report.queue.attempts += 1;
            report.queue.last_error = Some(crate::tracker::why_none().to_string());
            report.queue.next_try = Some(
                (chrono::Utc::now()
                    + chrono::Duration::seconds(backoff_secs(report.queue.attempts) as i64))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            );
            let _ = crash::write(report);
        }
        Err(e) => {
            report.queue.attempts += 1;
            report.queue.last_error = Some(e.clone());
            report.queue.next_try = Some(
                (chrono::Utc::now()
                    + chrono::Duration::seconds(backoff_secs(report.queue.attempts) as i64))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            );
            let _ = crash::write(report);
        }
    }
    outcome
}

/// The one sender at a time: a run's background flush and a `cig crash
/// send` typed meanwhile must not both file the same report. The lock is a
/// file made atomically, holding its sender's pid and start time; it is a
/// dead sender's when that process is gone, or when it is older than ten
/// minutes.
/// Within one process it is re-entrant: a flush holds it while each report
/// goes through `try_send`, which takes it again at once.
pub struct Lock {
    path: PathBuf,
    owner: bool,
}

impl Drop for Lock {
    fn drop(&mut self) {
        if self.owner {
            let _ = std::fs::remove_file(&self.path);
            if let Ok(mut held) = HELD.lock() {
                held.retain(|p| p != &self.path);
            }
        }
    }
}

/// The lock files this process holds.
static HELD: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// How long a sender waits for another to finish: long enough for a flush
/// to prove a few repros in their sandboxes.
pub const SEND_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

pub fn lock(wait: std::time::Duration) -> Option<Lock> {
    let _ = std::fs::create_dir_all(crash::ashtray_dir());
    lock_at(&crash::ashtray_dir().join(".flush.lock"), wait)
}

/// The lock at `path` (see `Lock`), waiting up to `wait` for another
/// process to let it go; `None` when it did not.
pub fn lock_at(path: &std::path::Path, wait: std::time::Duration) -> Option<Lock> {
    let path = path.to_path_buf();
    if HELD.lock().map(|h| h.contains(&path)).unwrap_or(false) {
        return Some(Lock { path, owner: false });
    }
    let started = std::time::Instant::now();
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                use std::io::Write;
                let me = std::process::id();
                let started = crate::burn::runs::process_started(me).unwrap_or_default();
                let _ = writeln!(f, "{me} {started}");
                if let Ok(mut held) = HELD.lock() {
                    held.push(path.clone());
                }
                return Some(Lock { path, owner: true });
            }
            Err(_) => {
                let old = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .map(|t| t.elapsed().map(|d| d.as_secs() > 600).unwrap_or(false))
                    .unwrap_or(true);
                if old || holder_dead(&path) {
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
                if started.elapsed() >= wait {
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
}

/// Is the process that wrote the lock at `path` gone? The file holds its
/// pid and, since 1.15.1, the pid's start time, so a pid that was reused is
/// not taken for the sender. A sender killed mid-send otherwise kept every
/// later send waiting on its lock (#127). The name is not checked: a
/// compiled program sends reports under its own name, not cig's.
fn holder_dead(path: &std::path::Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    // The pid, then the start time: the rest of the line, which on macOS
    // and the BSDs is a date with spaces in it (`ps -o lstart`).
    let line = text.lines().next().unwrap_or("").trim();
    let (pid, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let Ok(pid) = pid.parse::<u32>() else {
        return false;
    };
    let rest = rest.trim();
    let recorded = (!rest.is_empty()).then_some(rest);
    if pid == std::process::id() {
        // Ours only if it is from this process, not an earlier one that had
        // the same pid.
        return recorded.is_some_and(|r| {
            crate::burn::runs::process_started(pid).is_some_and(|live| live != r)
        });
    }
    match crate::burn::runs::process_started(pid) {
        None => true,
        Some(live) => recorded.is_some_and(|r| r != live),
    }
}

/// One pass: every due report is tried once; with `every`, the ones still
/// in their backoff too (the person typed `cig crash flush`, which is a
/// request for now). Skipped, with `locked` set, while another sender
/// holds the ashtray.
pub fn flush(cfg: &Config, every: bool) -> Flushed {
    let mut out = Flushed::default();
    let Some(_lock) = lock(std::time::Duration::from_millis(200)) else {
        out.locked = true;
        return out;
    };
    let all = pending(cfg);
    for mut r in all {
        if !every && !is_due(&r) {
            out.waiting += 1;
            continue;
        }
        match try_send(&mut r, cfg) {
            Ok(SendOutcome::Filed(url)) => out.filed.push((r.id.clone(), url)),
            Ok(SendOutcome::Commented(url)) => out.noted.push((r.id.clone(), url)),
            Ok(SendOutcome::Manual(_)) | Err(_) => out
                .failed
                .push((r.id.clone(), r.queue.last_error.clone().unwrap_or_default())),
        }
    }
    out
}

/// Start `cig crash flush --quiet` in the background when anything is due,
/// so the run that queued a report never waits on the network.
pub fn spawn_flush(cfg: &Config) {
    if due(cfg).is_empty() {
        return;
    }
    // Marked as cig's call to itself: a program's binary is `cig` for it.
    let Ok(mut c) = crate::program::self_command() else {
        return;
    };
    c.args(["crash", "flush", "--quiet"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
        c.creation_flags(0x0000_0008 | 0x0000_0200);
    }
    let _ = c.spawn();
}

/// Does this text name issue `number` as `#number`, as a whole token?
fn mentions(text: &str, number: u64) -> bool {
    let needle = format!("#{number}");
    let mut from = 0;
    while let Some(i) = text[from..].find(&needle) {
        let end = from + i + needle.len();
        let next = text[end..].chars().next();
        if !next.is_some_and(|c| c.is_ascii_digit()) {
            return true;
        }
        from = end;
    }
    false
}

/// Close the loop: for every report this machine filed, ask the tracker
/// whether its issue is closed and which release says it fixed it; the
/// first time that is known, say so in one line. `releases` are the
/// repository's, newest first, with their notes.
pub fn announce_fixed(cfg: &Config, releases: &[crate::update::Release]) -> Vec<String> {
    let mut lines = Vec::new();
    let Some(tracker) = crate::tracker::pick() else {
        return lines;
    };
    let current = crate::update::Version::current();
    let mut looked = 0;
    for mut r in crash::list().unwrap_or_default() {
        let Some(issue) = r.issue.clone() else {
            continue;
        };
        if r.fixed_in.is_some() || looked >= 10 {
            continue;
        }
        // Reports from another tool's repository are that tool's to announce.
        if issue.repo != repo_for(cfg, &r) {
            continue;
        }
        looked += 1;
        let Ok(state) = tracker.get(&issue.repo, issue.number) else {
            continue;
        };
        if state.state != "closed" {
            continue;
        }
        let fixed_in = releases
            .iter()
            .filter(|rel| mentions(&rel.body, issue.number))
            .filter_map(|rel| rel.version())
            .min();
        let Some(v) = fixed_in else {
            continue;
        };
        r.fixed_in = Some(v.to_string());
        if !r.announced {
            r.announced = true;
            let headline = crash::truncate(r.headline(), 60);
            lines.push(if v > current {
                format!(
                    "a bug you reported (#{}, {headline}) is fixed in {v}: ciginstaller update",
                    issue.number
                )
            } else {
                format!(
                    "a bug you reported (#{}, {headline}) is fixed in {v}, which you run",
                    issue.number
                )
            });
        }
        let _ = crash::write(&r);
    }
    lines
}

/// The last time this process queued a report: `cig run` asks once about it.
pub fn stamp_now() -> String {
    now_rfc3339()
}

#[cfg(test)]
mod lock_tests {
    use super::*;

    #[test]
    fn the_lock_is_one_sender_at_a_time_and_reentrant_within_one() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join(".flush.lock");
        let outer = lock_at(&path, std::time::Duration::ZERO).expect("free");
        assert!(path.exists());
        // The same process takes it again at once (a flush's try_send).
        let started = std::time::Instant::now();
        let inner = lock_at(&path, std::time::Duration::from_secs(5)).expect("re-entrant");
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        drop(inner);
        assert!(path.exists(), "the inner guard let go of the outer's lock");
        // Another holder (the file, made by another process) is waited for,
        // and not taken while it stays.
        drop(outer);
        assert!(!path.exists());
        // A live holder: written as another process would write it, with a
        // pid and start time that are alive (this process's; not in HELD).
        let me = std::process::id();
        let started = crate::burn::runs::process_started(me).unwrap_or_default();
        std::fs::write(&path, format!("{me} {started}\n")).unwrap();
        assert!(lock_at(&path, std::time::Duration::from_millis(150)).is_none());
        let p2 = path.clone();
        let freer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            std::fs::remove_file(&p2).unwrap();
        });
        let got = lock_at(&path, std::time::Duration::from_secs(10));
        freer.join().unwrap();
        assert!(got.is_some(), "the lock was not taken once it was let go");
    }

    /// #127: a sender killed mid-send leaves its lock; the next send sees
    /// that its pid is gone (or belongs to a later process) and takes it.
    #[test]
    fn a_dead_senders_lock_is_taken_over() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join(".flush.lock");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--list")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let dead = child.id();
        child.wait().unwrap();
        // A start time no live process has: a reused pid is not the sender.
        std::fs::write(&path, format!("{dead} ticks:1\n")).unwrap();
        let got = lock_at(&path, std::time::Duration::from_secs(2));
        assert!(
            got.is_some(),
            "a dead sender's lock kept the next send waiting"
        );
        // A lock from before 1.15.1 (a pid alone) whose pid is gone, too.
        drop(got);
        std::fs::write(&path, format!("{dead}\n")).unwrap();
        if crate::burn::runs::process_started(dead).is_none() {
            assert!(lock_at(&path, std::time::Duration::from_secs(2)).is_some());
        }
    }
}
