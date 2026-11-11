// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Tripwires: cheap checks of cig's own promises, run after the work is
//! done, that turn a silent bug into a report. A tripwire never changes the
//! run's outcome; it records what it found and says one line.
//!
//! - after a rollback (in a run, or `cig unburn` later): every path it
//!   restored is read back and compared with the state recorded before the
//!   first op that touched it (bytes, mode, link target, absence);
//! - after a run: the journal on disk re-read, its chain intact, its head
//!   and length what the kernel holds;
//! - after a run: a sample of the tin objects it refers to re-hashed;
//! - after a box apply (in the kernel): the applied files hash as the layer's.

use crate::burn::journal::{sha256_file, Before, Journal, RollbackReport};
use crate::burn::tin;
use std::path::Path;

/// One broken promise.
#[derive(Clone, Debug)]
pub struct Trip {
    /// Which check: `rollback`, `journal`, `tin`, `box`.
    pub check: &'static str,
    /// What was found, in one line (paths relative to home).
    pub what: String,
    pub details: serde_json::Value,
}

/// How many bytes a rollback check re-hashes at most; past it the rest of
/// the restored files are compared by size alone and the report says so.
const HASH_BUDGET: u64 = 256 << 20;

fn mode_of(path: &Path) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::symlink_metadata(path)
            .ok()
            .map(|m| m.permissions().mode() & 0o7777)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// How many mismatches a rollback report lists in full; the rest are
/// counted, so a run over hundreds of files still fits an issue.
const MISMATCH_CAP: usize = 25;

/// After a rollback: is every path it restored in the state the journal
/// recorded before the run? A path touched twice is judged by the state
/// before the first op that touched it; the later before-states are the
/// run's own intermediate states, which a full rollback walks back
/// through. A path whose first entry was not restored was not promised.
pub fn after_rollback(journal: &Journal, report: &RollbackReport) -> Option<Trip> {
    let mut mismatches: Vec<serde_json::Value> = Vec::new();
    let mut more = 0usize;
    let mut hashed = 0u64;
    let mut checked = 0usize;
    let mut sampled = false;
    // The first before-state of every path, in journal order. The script
    // names a path from the working directory and a hop's effect names it
    // whole; both are one path here.
    let cwd = std::env::current_dir().unwrap_or_default();
    let whole = |p: &Path| -> std::path::PathBuf {
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            cwd.join(p)
        }
    };
    let mut first: Vec<(
        &crate::burn::journal::Entry,
        &crate::burn::journal::BeforeState,
    )> = Vec::new();
    let mut seen: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
    for entry in journal.entries() {
        for state in &entry.before {
            if seen.insert(whole(&state.path)) {
                first.push((entry, state));
            }
        }
    }
    let was_there = |p: &Path| {
        let p = whole(p);
        first.iter().any(|(_, s)| {
            whole(&s.path) == p
                && matches!(
                    s.before,
                    Before::File { .. }
                        | Before::Dir { .. }
                        | Before::Directory { .. }
                        | Before::Symlink { .. }
                )
        })
    };
    // A path under a folder that did not exist before the run is absent
    // after a full rollback, whatever a later entry recorded for the path
    // itself: the folder's own entry, the earliest to cover it, says so.
    let made_folder_over = |p: &Path| -> Option<&crate::burn::journal::Entry> {
        let p = whole(p);
        first
            .iter()
            .filter(|(_, s)| {
                let q = whole(&s.path);
                q != p && p.starts_with(&q) && matches!(s.before, Before::Absent)
            })
            .min_by_key(|(e, _)| e.seq)
            .filter(|(e, s)| {
                // Only when nothing earlier recorded the path itself.
                let own = first
                    .iter()
                    .find(|(_, t)| whole(&t.path) == p)
                    .map(|(f, _)| f.seq);
                own.is_none_or(|seq| e.seq < seq) && !s.path.as_os_str().is_empty()
            })
            .map(|(e, _)| *e)
    };
    for (entry, state) in &first {
        let path = &state.path;
        if crate::burn::journal::left_for_older_run(&journal.run_dir_of(), path) {
            continue;
        }
        let shown = path.display().to_string();
        let meta = std::fs::symlink_metadata(path);
        let folder = made_folder_over(path);
        let judged_by = folder.unwrap_or(entry);
        if !(judged_by.restored || judged_by.undone) {
            continue;
        }
        let problem: Option<String> = if folder.is_some() {
            meta.is_ok().then(|| {
                "should be absent (its folder did not exist before the run), but something is there"
                    .to_string()
            })
        } else {
            match &state.before {
                Before::Absent => meta
                    .is_ok()
                    .then(|| "should be absent, but something is there".to_string()),
                Before::File {
                    snapshot,
                    sha256,
                    bytes,
                    mode,
                    ..
                } => match meta {
                    Err(_) => Some("should be a file, but nothing is there".to_string()),
                    Ok(m) if !m.is_file() => Some("should be a file, but is not one".to_string()),
                    Ok(m) => {
                        let mut p = None;
                        if m.len() != *bytes {
                            p = Some(format!("should hold {bytes} bytes, holds {}", m.len()));
                        } else if hashed + *bytes <= HASH_BUDGET {
                            hashed += *bytes;
                            match sha256_file(path) {
                                Ok(h) if h != *sha256 => {
                                    p = Some(format!(
                                        "should hash to {}, hashes to {}",
                                        &sha256[..12],
                                        &h[..12]
                                    ))
                                }
                                Err(e) => p = Some(format!("could not be read back: {e}")),
                                _ => {}
                            }
                        } else {
                            sampled = true;
                        }
                        if p.is_none() {
                            // The mode the journal recorded, or the one the
                            // run's own snapshot still carries (none where
                            // the platform has no modes).
                            let want = mode.or_else(|| {
                                (!snapshot.as_os_str().is_empty()
                                    && !tin::holds(snapshot)
                                    && !crate::burn::seal::is_sealed_path(snapshot))
                                .then(|| mode_of(snapshot))
                                .flatten()
                            });
                            if let (Some(want), Some(have)) = (want, mode_of(path)) {
                                if want & 0o7777 != have {
                                    p = Some(format!(
                                        "should have mode {:o}, has {have:o}",
                                        want & 0o7777
                                    ));
                                }
                            }
                        }
                        p
                    }
                },
                Before::Dir { .. } | Before::Directory { .. } => match meta {
                    Ok(m) if m.is_dir() => None,
                    Ok(_) => Some("should be a directory, but is not one".to_string()),
                    Err(_) => Some("should be a directory, but nothing is there".to_string()),
                },
                Before::Symlink { target } => match std::fs::read_link(path) {
                    Ok(t) if t == *target => None,
                    Ok(t) => Some(format!(
                        "should link to {}, links to {}",
                        target.display(),
                        t.display()
                    )),
                    Err(_) => Some("should be a link, but is not one".to_string()),
                },
                Before::Moved { to } => {
                    if meta.is_err() {
                        Some("should be back, but nothing is there".to_string())
                    } else if std::fs::symlink_metadata(to).is_ok() && !was_there(to) {
                        Some(format!(
                            "was moved back, yet {} is still there",
                            to.display()
                        ))
                    } else {
                        None
                    }
                }
            }
        };
        checked += 1;
        if let Some(why) = problem {
            if mismatches.len() < MISMATCH_CAP {
                mismatches.push(serde_json::json!({
                    "seq": entry.seq,
                    "op": entry.op.describe(),
                    "path": shown,
                    "problem": why,
                }));
            } else {
                more += 1;
            }
        }
    }
    if mismatches.is_empty() {
        return None;
    }
    let total = mismatches.len() + more;
    let first_problem = mismatches[0]["problem"].as_str().unwrap_or("").to_string();
    Some(Trip {
        check: "rollback",
        what: format!(
            "the rollback said it restored {} path{}, but {} {} not as recorded: {}",
            checked,
            if checked == 1 { "" } else { "s" },
            total,
            if total == 1 { "is" } else { "are" },
            first_problem
        ),
        details: serde_json::json!({
            "checked": checked,
            "mismatches": mismatches,
            "more": more,
            "hash_budget_spent": sampled,
            "rollback_failed": report.failed.len(),
        }),
    })
}

/// After a run: the journal on disk is the one the kernel wrote.
pub fn journal_reread(journal: &Journal) -> Option<Trip> {
    let dir = journal.run_dir()?;
    let loaded = match Journal::load(dir) {
        Ok(j) => j,
        Err(e) => {
            return Some(Trip {
                check: "journal",
                what: format!("the run's own journal does not load back: {e}"),
                details: serde_json::json!({"error": e.to_string()}),
            })
        }
    };
    let mut problems = Vec::new();
    if !loaded.chain().intact() {
        problems.push(format!(
            "its chain is broken at line {}",
            loaded
                .chain()
                .broken_at
                .as_ref()
                .map(|b| b.line.to_string())
                .unwrap_or_default()
        ));
    }
    if loaded.len() != journal.len() {
        problems.push(format!(
            "it holds {} entries where the kernel wrote {}",
            loaded.len(),
            journal.len()
        ));
    }
    if loaded.head() != journal.head() {
        problems.push("its head is not the hash the kernel holds".to_string());
    }
    if problems.is_empty() {
        return None;
    }
    Some(Trip {
        check: "journal",
        what: format!("the run's own journal, read back: {}", problems.join("; ")),
        details: serde_json::json!({"problems": problems, "entries": loaded.len(), "written": journal.len()}),
    })
}

/// How many objects `verify_sample` asks for: `auto` is 3, `0` none.
pub fn sample_size(setting: &str) -> usize {
    match setting.trim() {
        "auto" | "" => 3,
        n => n.parse().unwrap_or(3),
    }
}

/// After a burn: `n` objects drawn at random from the whole tin hash to
/// their names (#105, L6), beside the run's own ([`tin_sample`]). A few
/// milliseconds, however big the tin.
pub fn tin_random_sample(n: usize) -> Option<Trip> {
    let mut seen = 0;
    let mut bad = Vec::new();
    for p in tin::random_objects(n) {
        let Some(name) = p.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
            continue;
        };
        seen += 1;
        match sha256_file(&p) {
            Ok(h) if h == name => {}
            Ok(h) => bad.push(serde_json::json!({"object": name, "hashes_to": h})),
            Err(err) => bad.push(serde_json::json!({"object": name, "error": err.to_string()})),
        }
    }
    if bad.is_empty() {
        return None;
    }
    Some(Trip {
        check: "tin",
        what: format!(
            "{} of {seen} tin object{} drawn at random do{} not hash to {} name",
            bad.len(),
            if seen == 1 { "" } else { "s" },
            if bad.len() == 1 { "es" } else { "" },
            if bad.len() == 1 { "its" } else { "their" }
        ),
        details: serde_json::json!({"objects": bad, "sampled": seen}),
    })
}

/// After a run: up to three tin objects the journal refers to hash to
/// their names.
pub fn tin_sample(journal: &Journal) -> Option<Trip> {
    let mut seen = 0;
    let mut bad = Vec::new();
    for e in journal.entries() {
        for b in &e.before {
            let Before::File { snapshot, .. } = &b.before else {
                continue;
            };
            if snapshot.as_os_str().is_empty()
                || !tin::holds(snapshot)
                || tin::is_recipe(snapshot)
                || tin::is_sealed(snapshot)
            {
                continue;
            }
            let Some(name) = snapshot.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.len() != 64 {
                continue;
            }
            seen += 1;
            match sha256_file(snapshot) {
                Ok(h) if h == name => {}
                Ok(h) => bad.push(serde_json::json!({"object": name, "hashes_to": h})),
                Err(err) => bad.push(serde_json::json!({"object": name, "error": err.to_string()})),
            }
            if seen >= 3 {
                break;
            }
        }
        if seen >= 3 {
            break;
        }
    }
    if bad.is_empty() {
        return None;
    }
    Some(Trip {
        check: "tin",
        what: format!(
            "{} of {seen} tin object{} the run refers to do{} not hash to {} name",
            bad.len(),
            if seen == 1 { "" } else { "s" },
            if bad.len() == 1 { "es" } else { "" },
            if bad.len() == 1 { "its" } else { "their" }
        ),
        details: serde_json::json!({"sampled": seen, "bad": bad}),
    })
}

/// The test hook that makes a promise break on purpose, so the tripwires
/// are proved on a real fault: `CIG_INTERNAL_FAULT=restore-mode` restores
/// every file with one mode bit wrong; `restore-mode-once:<marker>` does it
/// until the marker file exists, and makes it.
pub fn injected(name: &str) -> bool {
    let Ok(v) = std::env::var("CIG_INTERNAL_FAULT") else {
        return false;
    };
    if v == name {
        return true;
    }
    if let Some(marker) = v.strip_prefix(&format!("{name}-once:")) {
        let m = Path::new(marker);
        if m.exists() {
            return false;
        }
        let _ = std::fs::write(m, b"fired\n");
        return true;
    }
    false
}
