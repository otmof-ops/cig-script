// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Adopting another tool's committed transaction as a cig run.
//!
//! `ciginstaller` keeps its own write-ahead journal under
//! `<home>/installer/txn/<id>/` (`journal.jsonl`, one JSON object per line,
//! and the files it saved under `saved/`). It never links cig's crates or
//! keyring, so it cannot write a record cig would accept. This module reads
//! that format from the outside and imports a COMMITTED transaction as a run:
//! every saved file is checked against the sha256 the journal recorded,
//! every path is confined to the installer home the transaction lives in,
//! the snapshots go into the run's folder in the tin, the journal is written
//! by [`Journal`] itself (so it is chained and signed under this machine's
//! key), and its head goes into the ledger as an `origin` line. Nothing
//! outside cig's own state directory is touched: the restore is the ordinary
//! `cig unburn <id>`.
//!
//! What the installer's `store` ops become: nothing. A store object is
//! content addressed and cig cannot know whether another receipt uses it, so
//! it is left (`ciginstaller gc` reclaims an unused one) and counted in the
//! run's record.

use super::journal::{sha256_file, Before, BeforeState, Journal};
use super::runs::{self, RunRecord};
use super::Op;
use serde::Deserialize;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// The transaction was not committed: open, interrupted, cut short, or rolled back.
pub const NOT_COMMITTED: &str = "E716";
/// A saved file is missing, or is not the bytes the journal recorded.
pub const SAVED_MISMATCH: &str = "E717";
/// A path in the journal is outside the installer home the transaction names.
pub const OUTSIDE_HOME: &str = "E718";
/// The folder is not a transaction this cig can read.
pub const NOT_A_TRANSACTION: &str = "E719";

/// The tool whose transactions this reads.
pub const TOOL: &str = "ciginstaller";
/// The journal format this reads (the installer's `JOURNAL_FORMAT`).
const READS_FORMAT: u32 = 1;

/// Why an adoption was refused: a code from the registry, what happened, and what to do.
#[derive(Debug)]
pub struct Refused {
    pub code: &'static str,
    pub what: String,
    pub hint: String,
}

fn refuse(code: &'static str, what: impl Into<String>, hint: impl Into<String>) -> Refused {
    Refused {
        code,
        what: what.into(),
        hint: hint.into(),
    }
}

/// What an adoption did.
#[derive(Debug)]
pub struct Adopted {
    /// The run's id; `None` when the transaction changed nothing cig can put back.
    pub id: Option<String>,
    pub home: PathBuf,
    pub txn: String,
    pub label: String,
    /// Entries in the run's journal.
    pub entries: usize,
    /// `store` ops the installer logged: left in place, not undone.
    pub store_objects_left: usize,
    /// This transaction was adopted before; the run that exists is reported.
    pub already: bool,
}

// ------------------------------------------------- the installer's journal

#[derive(Debug, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum SrcBefore {
    Absent,
    File {
        #[serde(default)]
        saved: Option<String>,
        #[serde(default)]
        sha256: Option<String>,
    },
    Dir {
        #[serde(default)]
        saved: Option<String>,
    },
    Link {
        target: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
#[allow(dead_code)] // fields the format carries and the adoption does not need
enum SrcOp {
    Begin {
        label: String,
        #[serde(default)]
        pid: u32,
        format: u32,
    },
    Mkdir {
        path: PathBuf,
        before: SrcBefore,
    },
    Store {
        #[serde(default)]
        object: String,
        #[serde(default)]
        exec: bool,
        #[serde(default = "absent")]
        before: SrcBefore,
    },
    Link {
        from: PathBuf,
        to: PathBuf,
        before: SrcBefore,
    },
    Move {
        from: PathBuf,
        to: PathBuf,
        before: SrcBefore,
    },
    Write {
        path: PathBuf,
        before: SrcBefore,
        #[serde(default)]
        after_sha256: String,
    },
    Delete {
        path: PathBuf,
        before: SrcBefore,
    },
    Block {
        path: PathBuf,
        before: SrcBefore,
        #[serde(default)]
        marker: String,
    },
    Flip {
        index_sha256: String,
        previous_sha256: String,
        #[serde(default)]
        saved: Option<String>,
    },
    Commit {
        #[serde(default)]
        recovered: bool,
    },
    #[serde(rename = "rollback-done")]
    RollbackDone,
}

fn absent() -> SrcBefore {
    SrcBefore::Absent
}

#[derive(Debug, Deserialize)]
struct SrcLine {
    seq: u64,
    #[serde(flatten)]
    op: SrcOp,
}

// ---------------------------------------------------------------- the plan

/// A before-state, still pointing at the installer's saved copy.
#[derive(Debug)]
enum Source {
    Absent(PathBuf),
    File {
        path: PathBuf,
        saved: PathBuf,
        sha256: String,
    },
    Dir {
        path: PathBuf,
        saved: PathBuf,
    },
    Link {
        path: PathBuf,
        target: PathBuf,
    },
    Moved {
        path: PathBuf,
        to: PathBuf,
    },
}

struct Planned {
    op: Op,
    states: Vec<Source>,
}

struct Plan {
    items: Vec<Planned>,
    store_objects_left: usize,
    label: String,
}

/// Where a transaction lives: `<home>/installer/txn/<id>`.
#[derive(Debug)]
struct Where {
    dir: PathBuf,
    txn: String,
    /// The home as the transaction folder was spelled to us.
    home: PathBuf,
    /// The home with links resolved.
    home_real: PathBuf,
}

fn locate(dir: &Path) -> Result<Where, Refused> {
    let not = |what: String| {
        refuse(
            NOT_A_TRANSACTION,
            what,
            "give the folder `<installer home>/installer/txn/<id>` that `ciginstaller update` reported as its transaction",
        )
    };
    let abs = std::path::absolute(dir).map_err(|e| not(format!("{}: {e}", dir.display())))?;
    if !abs.is_dir() {
        return Err(not(format!("{} is not a folder", abs.display())));
    }
    let txn = abs
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let shaped = txn.len() == 22
        && txn.as_bytes()[8] == b'T'
        && txn.as_bytes()[15] == b'-'
        && txn
            .bytes()
            .enumerate()
            .all(|(i, b)| i == 8 || i == 15 || b.is_ascii_alphanumeric());
    let name_of = |p: Option<&Path>| {
        p.and_then(Path::file_name)
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    };
    let txn_root = abs.parent();
    let installer = txn_root.and_then(Path::parent);
    let home = installer.and_then(Path::parent);
    if !shaped || name_of(txn_root) != "txn" || name_of(installer) != "installer" || home.is_none()
    {
        return Err(not(format!(
            "{} is not `<home>/installer/txn/<id>`",
            abs.display()
        )));
    }
    let home = home.map(Path::to_path_buf).unwrap_or_default();
    let home_real = fs::canonicalize(&home).unwrap_or_else(|_| home.clone());
    Ok(Where {
        dir: abs,
        txn,
        home,
        home_real,
    })
}

/// A journal path, confined: absolute, no `..`, inside the home, and no link on the way
/// down to it (a restore would write through one). The last component may itself be a link.
fn confine(w: &Where, p: &Path) -> Result<PathBuf, Refused> {
    let out = |why: String| {
        refuse(
            OUTSIDE_HOME,
            format!("{} {why}", p.display()),
            format!(
                "an installer transaction can only name paths inside its own home ({}); nothing was imported",
                w.home.display()
            ),
        )
    };
    if !p.has_root() {
        return Err(out("is not an absolute path".into()));
    }
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(out("has a `..` in it".into()));
    }
    let (base, rel) = if let Ok(r) = p.strip_prefix(&w.home) {
        (&w.home, r)
    } else if let Ok(r) = p.strip_prefix(&w.home_real) {
        (&w.home_real, r)
    } else {
        return Err(out("is outside the installer home".into()));
    };
    if rel.as_os_str().is_empty() {
        return Err(out("is the installer home itself".into()));
    }
    let comps: Vec<&std::ffi::OsStr> = rel.iter().collect();
    let mut cur = base.to_path_buf();
    for c in &comps[..comps.len() - 1] {
        cur.push(c);
        if fs::symlink_metadata(&cur).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(out(format!("goes through a link ({})", cur.display())));
        }
    }
    Ok(p.to_path_buf())
}

/// A saved copy's path inside the transaction folder.
fn saved_path(w: &Where, rel: &str) -> Result<PathBuf, Refused> {
    let r = Path::new(rel);
    let plain = !rel.is_empty()
        && r.components().all(|c| matches!(c, Component::Normal(_)))
        && r.components().count() >= 1;
    if !plain {
        return Err(refuse(
            SAVED_MISMATCH,
            format!("the journal names a saved copy `{rel}` that is not inside its transaction"),
            "the journal was edited; nothing was imported",
        ));
    }
    Ok(w.dir.join(r))
}

/// A saved file, present and exactly as recorded.
fn saved_file(w: &Where, rel: &str, want: &str) -> Result<PathBuf, Refused> {
    let p = saved_path(w, rel)?;
    match fs::symlink_metadata(&p) {
        Ok(m) if m.is_file() => {}
        _ => return Err(refuse(
            SAVED_MISMATCH,
            format!("the saved copy {rel} is missing (or is not a plain file)"),
            "the transaction folder was changed after the installer wrote it; nothing was imported",
        )),
    }
    let got = sha256_file(&p).map_err(|e| {
        refuse(
            SAVED_MISMATCH,
            format!("the saved copy {rel} cannot be read: {e}"),
            "nothing was imported",
        )
    })?;
    if got != want {
        return Err(refuse(
            SAVED_MISMATCH,
            format!(
                "the saved copy {rel} is not what the journal recorded (sha256 {} on disk, {} recorded)",
                &got[..got.len().min(12)],
                &want[..want.len().min(12)]
            ),
            "the saved file was changed after the installer wrote it; restoring from it would put back the wrong bytes, so nothing was imported",
        ));
    }
    Ok(p)
}

fn saved_dir(w: &Where, rel: &str) -> Result<PathBuf, Refused> {
    let p = saved_path(w, rel)?;
    match fs::symlink_metadata(&p) {
        Ok(m) if m.is_dir() => Ok(p),
        _ => Err(refuse(
            SAVED_MISMATCH,
            format!("the saved folder {rel} is missing"),
            "the transaction folder was changed after the installer wrote it; nothing was imported",
        )),
    }
}

/// The before-state of a file the installer replaced: the saved bytes put back
/// into a fresh file, never in place (the path may share its inode with a store object).
fn file_states(w: &Where, path: &Path, before: &SrcBefore) -> Result<Option<Vec<Source>>, Refused> {
    match before {
        SrcBefore::Absent => Ok(Some(vec![Source::Absent(path.to_path_buf())])),
        SrcBefore::File {
            saved: Some(rel),
            sha256: Some(sha),
        } => {
            let saved = saved_file(w, rel, sha)?;
            Ok(Some(vec![
                Source::File {
                    path: path.to_path_buf(),
                    saved,
                    sha256: sha.clone(),
                },
                // restored first: the name goes, so the bytes come back as a new file
                Source::Absent(path.to_path_buf()),
            ]))
        }
        SrcBefore::File { .. } => Err(refuse(
            SAVED_MISMATCH,
            format!(
                "the journal records a saved copy of {} with no hash to check it by",
                path.display()
            ),
            "the journal was edited; nothing was imported",
        )),
        _ => Err(refuse(
            NOT_A_TRANSACTION,
            format!("an op on {} has a before-state this cig does not read", path.display()),
            "the transaction came from a newer installer; update cig, or use `ciginstaller rollback`",
        )),
    }
}

fn is_partial(home: &Path, from: &Path, to: &Path) -> bool {
    to.starts_with(home.join("toolchains"))
        && from.file_name().is_some_and(|n| {
            let n = n.to_string_lossy();
            n.starts_with('.') && n.contains(".partial-")
        })
}

fn read_lines(w: &Where) -> Result<(Vec<SrcLine>, Vec<u8>), Refused> {
    let jp = w.dir.join("journal.jsonl");
    let bytes = fs::read(&jp).map_err(|e| {
        refuse(
            NOT_A_TRANSACTION,
            format!("{}: {e}", jp.display()),
            "the folder holds no installer journal",
        )
    })?;
    let bad = |why: String| {
        refuse(
            NOT_A_TRANSACTION,
            format!("{}: {why}", jp.display()),
            "the journal is damaged or is not an installer journal; nothing was imported",
        )
    };
    let text = std::str::from_utf8(&bytes).map_err(|_| bad("not UTF-8".into()))?;
    let mut lines = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let Some(i) = rest.find('\n') else {
            return Err(refuse(
                NOT_COMMITTED,
                "the journal ends in a half-written line: the transaction was cut short",
                "`ciginstaller` settles an interrupted transaction on its next run; adopt it once it has committed or rolled back",
            ));
        };
        let line: SrcLine = serde_json::from_str(&rest[..i])
            .map_err(|e| bad(format!("line {}: {e}", lines.len() + 1)))?;
        if line.seq != lines.len() as u64 {
            return Err(bad(format!(
                "line {} has seq {}",
                lines.len() + 1,
                line.seq
            )));
        }
        lines.push(line);
        rest = &rest[i + 1..];
    }
    Ok((lines, bytes))
}

fn plan(w: &Where, lines: &[SrcLine]) -> Result<Plan, Refused> {
    let label = match lines.first() {
        Some(SrcLine {
            op: SrcOp::Begin { label, format, .. },
            ..
        }) => {
            if *format > READS_FORMAT {
                return Err(refuse(
                    NOT_A_TRANSACTION,
                    format!("the journal is format {format}; this cig reads format {READS_FORMAT}"),
                    "update cig, or use `ciginstaller rollback` for this transaction",
                ));
            }
            label.clone()
        }
        _ => {
            return Err(refuse(
                NOT_A_TRANSACTION,
                "the journal's first line is not `begin`",
                "this is not an installer journal",
            ))
        }
    };
    if lines.iter().any(|l| matches!(l.op, SrcOp::RollbackDone)) {
        return Err(refuse(
            NOT_COMMITTED,
            "the transaction was rolled back; there is nothing of it left to undo",
            "adopt a transaction that committed (`ciginstaller update` reports its id as `txn`)",
        ));
    }
    let last_is_commit = matches!(lines.last().map(|l| &l.op), Some(SrcOp::Commit { .. }));
    if !last_is_commit {
        return Err(refuse(
            NOT_COMMITTED,
            "the transaction never committed",
            "`ciginstaller` rolls an open transaction back (or forward) on its next run; adopt it after that",
        ));
    }
    let commits = lines
        .iter()
        .filter(|l| matches!(l.op, SrcOp::Commit { .. }))
        .count();
    if commits != 1
        || lines
            .iter()
            .filter(|l| matches!(l.op, SrcOp::Begin { .. }))
            .count()
            != 1
    {
        return Err(refuse(
            NOT_A_TRANSACTION,
            "the journal has more than one `begin` or `commit`",
            "this is not a journal the installer writes",
        ));
    }
    let home = &w.home;
    let mut items: Vec<Planned> = Vec::new();
    let mut store_objects_left = 0usize;
    for l in &lines[1..lines.len() - 1] {
        match &l.op {
            SrcOp::Begin { .. } | SrcOp::Commit { .. } | SrcOp::RollbackDone => {}
            SrcOp::Mkdir { path, before } => {
                let path = confine(w, path)?;
                if matches!(before, SrcBefore::Absent) {
                    items.push(Planned {
                        op: Op::Mkdir { path: path.clone() },
                        states: vec![Source::Absent(path)],
                    });
                }
            }
            SrcOp::Store { .. } => store_objects_left += 1,
            SrcOp::Link { from, to, before } => {
                let from = confine(w, from)?;
                let to = confine(w, to)?;
                if let Some(states) = file_states(w, &to, before)? {
                    items.push(Planned {
                        op: Op::Copy { from, to },
                        states,
                    });
                }
            }
            SrcOp::Move { from, to, before } => {
                let from = confine(w, from)?;
                let to = confine(w, to)?;
                if !matches!(before, SrcBefore::Absent) {
                    continue;
                }
                // a toolchain's directory flip is undone by removing what arrived
                let states = if is_partial(home, &from, &to) || is_partial(&w.home_real, &from, &to)
                {
                    vec![Source::Absent(to.clone())]
                } else {
                    vec![Source::Moved {
                        path: from.clone(),
                        to: to.clone(),
                    }]
                };
                items.push(Planned {
                    op: Op::Move { from, to },
                    states,
                });
            }
            SrcOp::Write { path, before, .. } | SrcOp::Block { path, before, .. } => {
                let path = confine(w, path)?;
                if let Some(states) = file_states(w, &path, before)? {
                    items.push(Planned {
                        op: Op::Write {
                            path,
                            sensitive: false,
                        },
                        states,
                    });
                }
            }
            SrcOp::Delete { path, before } => {
                let path = confine(w, path)?;
                let state = match before {
                    SrcBefore::Absent => continue,
                    SrcBefore::File {
                        saved: Some(rel),
                        sha256: Some(sha),
                    } => Source::File {
                        path: path.clone(),
                        saved: saved_file(w, rel, sha)?,
                        sha256: sha.clone(),
                    },
                    SrcBefore::Dir { saved: Some(rel) } => Source::Dir {
                        path: path.clone(),
                        saved: saved_dir(w, rel)?,
                    },
                    SrcBefore::Link { target } => Source::Link {
                        path: path.clone(),
                        target: PathBuf::from(target),
                    },
                    _ => {
                        return Err(refuse(
                            SAVED_MISMATCH,
                            format!(
                                "the journal deletes {} and records nothing to put back",
                                path.display()
                            ),
                            "the journal was edited; nothing was imported",
                        ))
                    }
                };
                items.push(Planned {
                    op: Op::Delete { path },
                    states: vec![state],
                });
            }
            SrcOp::Flip {
                index_sha256,
                previous_sha256,
                saved,
            } => {
                // the rename of installer/shim-index becomes a write of the saved index
                if index_sha256 == previous_sha256 {
                    continue;
                }
                let idx = confine(w, &home.join("installer").join("shim-index"))?;
                let states = match saved {
                    Some(rel) => {
                        let saved = saved_file(w, rel, previous_sha256)?;
                        vec![
                            Source::File {
                                path: idx.clone(),
                                saved,
                                sha256: previous_sha256.clone(),
                            },
                            Source::Absent(idx.clone()),
                        ]
                    }
                    None if previous_sha256 == "absent" => vec![Source::Absent(idx.clone())],
                    None => {
                        return Err(refuse(
                            SAVED_MISMATCH,
                            "the flip replaced a shim-index and kept no copy of it",
                            "the journal was edited; nothing was imported",
                        ))
                    }
                };
                items.push(Planned {
                    op: Op::Write {
                        path: idx,
                        sensitive: false,
                    },
                    states,
                });
            }
        }
    }
    Ok(Plan {
        items,
        store_objects_left,
        label,
    })
}

// ------------------------------------------------------------- the import

fn io_refusal(what: &str, e: impl std::fmt::Display) -> Refused {
    refuse(
        NOT_A_TRANSACTION,
        format!("{what}: {e}"),
        "nothing was imported; the state directory is as it was",
    )
}

/// The id a transaction is adopted under: its own timestamp and a hash of where it
/// lives, so the same transaction is always the same run (adopting twice is one run).
fn run_id(w: &Where) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(w.home_real.to_string_lossy().as_bytes());
    h.update([0]);
    h.update(w.txn.as_bytes());
    let hex = hex::encode(h.finalize());
    format!("{}-{}", &w.txn[..15], &hex[..6])
}

fn materialize(
    run_dir: &Path,
    seq: u64,
    slot: usize,
    src: &Source,
) -> Result<BeforeState, Refused> {
    let snaps = run_dir.join("snapshots");
    let name = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "root".to_string())
    };
    Ok(match src {
        Source::Absent(p) => BeforeState {
            path: p.clone(),
            before: Before::Absent,
        },
        Source::Link { path, target } => BeforeState {
            path: path.clone(),
            before: Before::Symlink {
                target: target.clone(),
            },
        },
        Source::Moved { path, to } => BeforeState {
            path: path.clone(),
            before: Before::Moved { to: to.clone() },
        },
        Source::File {
            path,
            saved,
            sha256,
        } => {
            let snap = snaps.join(format!("{seq:05}-{slot}-{}", name(path)));
            fs::copy(saved, &snap).map_err(|e| io_refusal("cannot keep a snapshot", e))?;
            super::journal::sync_file(&snap)
                .map_err(|e| io_refusal("cannot sync a snapshot", e))?;
            // checked again on the copy: what is kept is what was recorded
            let got = sha256_file(&snap).map_err(|e| io_refusal("cannot read a snapshot", e))?;
            if &got != sha256 {
                return Err(refuse(
                    SAVED_MISMATCH,
                    format!(
                        "a saved copy changed while it was being imported ({})",
                        saved.display()
                    ),
                    "nothing was imported; run it again once nothing else is writing in the transaction folder",
                ));
            }
            let bytes = fs::metadata(&snap).map(|m| m.len()).unwrap_or(0);
            BeforeState {
                path: path.clone(),
                before: Before::File {
                    snapshot: snap,
                    sha256: sha256.clone(),
                    bytes,
                    mode: None,
                    mtime_ns: None,
                    git: None,
                },
            }
        }
        Source::Dir { path, saved } => {
            let snap = snaps.join(format!("{seq:05}-{slot}-{}", name(path)));
            super::journal::copy_tree(saved, &snap)
                .map_err(|e| io_refusal("cannot keep a folder snapshot", e))?;
            BeforeState {
                path: path.clone(),
                before: Before::Dir { snapshot: snap },
            }
        }
    })
}

fn origin_text(w: &Where) -> String {
    format!("{TOOL} {} txn {}", w.home.display(), w.txn)
}

/// Make sure the ledger has the `origin` line of an adopted run (a crash between the
/// record and the ledger leaves the run without one).
fn ensure_ledger(rec: &RunRecord) -> Result<(), Refused> {
    let Some(head) = rec.journal_head.as_deref() else {
        return Ok(());
    };
    let loaded = super::ledger::load().map_err(|e| io_refusal("cannot read the ledger", e))?;
    if super::ledger::last_record_of(&loaded, &rec.id).is_some() {
        return Ok(());
    }
    let lines = Journal::load(&rec.dir())
        .map(|j| j.chain().lines as u64)
        .unwrap_or(0);
    super::ledger::append("origin", &rec.id, head, &rec.status, lines)
        .map_err(|e| io_refusal("cannot write the ledger", e))
}

/// Adopt a committed installer transaction as a run. Reads and verifies the whole
/// transaction first; writes only under cig's own state directory.
pub fn adopt(txn_dir: &Path) -> Result<Adopted, Refused> {
    let w = locate(txn_dir)?;
    let (lines, journal_bytes) = read_lines(&w)?;
    let planned = plan(&w, &lines)?;
    let mut out = Adopted {
        id: None,
        home: w.home.clone(),
        txn: w.txn.clone(),
        label: planned.label.clone(),
        entries: planned.items.len(),
        store_objects_left: planned.store_objects_left,
        already: false,
    };
    if planned.items.is_empty() {
        return Ok(out);
    }
    let id = run_id(&w);
    let run_dir = runs::runs_dir().join(&id);
    if run_dir.join("run.json").is_file() {
        // adopted before: the same run, and its ledger line made good if a crash lost it
        let rec = RunRecord::load(&run_dir).map_err(|e| io_refusal("the adopted run", e))?;
        ensure_ledger(&rec)?;
        out.id = Some(id);
        out.already = true;
        return Ok(out);
    }
    if run_dir.exists() {
        // our own name, no record: an adoption that was cut off
        fs::remove_dir_all(&run_dir)
            .map_err(|e| io_refusal("cannot clear a cut-off adoption", e))?;
    }
    fs::create_dir_all(runs::runs_dir()).map_err(|e| io_refusal("cannot make runs/", e))?;
    fs::create_dir(&run_dir).map_err(|e| io_refusal("cannot make the run folder", e))?;
    match write_run(&w, &planned, &journal_bytes, &id, &run_dir) {
        Ok(()) => {
            out.id = Some(id);
            Ok(out)
        }
        Err(e) => {
            let _ = fs::remove_dir_all(&run_dir);
            Err(e)
        }
    }
}

fn write_run(
    w: &Where,
    planned: &Plan,
    journal_bytes: &[u8],
    id: &str,
    run_dir: &Path,
) -> Result<(), Refused> {
    use sha2::{Digest, Sha256};
    let mut journal = Journal::open(Some(run_dir.to_path_buf()))
        .map_err(|e| io_refusal("cannot open a journal", e))?;
    for (i, item) in planned.items.iter().enumerate() {
        let seq = i as u64 + 1;
        let mut states = Vec::new();
        for (slot, s) in item.states.iter().enumerate() {
            states.push(materialize(run_dir, seq, slot, s)?);
        }
        journal
            .record_with_before(seq, item.op.clone(), states, None, false)
            .map_err(|e| io_refusal("cannot write the journal", e))?;
    }
    drop(journal);
    super::journal::sync_dir_pub(&run_dir.join("snapshots"))
        .map_err(|e| io_refusal("cannot sync the snapshots", e))?;
    let loaded =
        Journal::load(run_dir).map_err(|e| io_refusal("cannot read the journal back", e))?;
    if !loaded.chain().intact() {
        return Err(io_refusal(
            "the journal just written does not chain",
            "internal",
        ));
    }
    let head = loaded.head().to_string();
    let lines = loaded.chain().lines as u64;
    let mut rec = RunRecord::begin(
        Path::new(&format!("{TOOL}: {} [txn {}]", planned.label, w.txn)),
        "",
        &[],
        "run",
    );
    rec.id = id.to_string();
    rec.script_sha256 = hex::encode(Sha256::digest(journal_bytes));
    rec.cwd = w.home.to_string_lossy().to_string();
    rec.started = chrono::NaiveDateTime::parse_from_str(&w.txn[..15], "%Y%m%dT%H%M%S")
        .map(|t| {
            t.and_utc()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        })
        .unwrap_or_else(|_| rec.started.clone());
    rec.finish("ok", None);
    rec.burns = planned.items.len();
    rec.pid = None;
    rec.pid_started = None;
    rec.journal_head = Some(head.clone());
    rec.imported = Some(serde_json::json!({
        "from": origin_text(w),
        "at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "tool": TOOL,
        "foreign": {
            "tool": TOOL,
            "home": w.home,
            "txn": w.txn,
            "label": planned.label,
            "journal_sha256": rec.script_sha256,
            "store_objects_left": planned.store_objects_left,
        },
        "path_map": [],
    }));
    rec.loaded_from = Some(run_dir.to_path_buf());
    rec.save()
        .map_err(|e| io_refusal("cannot write the run record", e))?;
    super::ledger::append("origin", id, &head, "ok", lines)
        .map_err(|e| io_refusal("cannot write the ledger", e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn txn_dir(root: &Path) -> PathBuf {
        let d = root.join("home/installer/txn/20261101T120000-a1b2c3");
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_transaction_folder_names_its_home_and_nothing_else_does() {
        let t = tempfile::tempdir().unwrap();
        let d = txn_dir(t.path());
        let w = locate(&d).unwrap();
        assert_eq!(w.home, t.path().join("home"));
        assert_eq!(w.txn, "20261101T120000-a1b2c3");
        let other = t.path().join("home/installer/20261101T120000-a1b2c3");
        fs::create_dir_all(&other).unwrap();
        assert_eq!(locate(&other).unwrap_err().code, NOT_A_TRANSACTION);
        let odd = t.path().join("home/installer/txn/not-an-id");
        fs::create_dir_all(&odd).unwrap();
        assert_eq!(locate(&odd).unwrap_err().code, NOT_A_TRANSACTION);
    }

    #[test]
    fn paths_are_confined_to_the_home() {
        let t = tempfile::tempdir().unwrap();
        let w = locate(&txn_dir(t.path())).unwrap();
        let h = &w.home;
        assert!(confine(&w, &h.join("toolchains/cig")).is_ok());
        for bad in [
            h.clone(),
            h.join("../x"),
            t.path().join("elsewhere"),
            PathBuf::from("relative/path"),
        ] {
            assert_eq!(confine(&w, &bad).unwrap_err().code, OUTSIDE_HOME, "{bad:?}");
        }
    }

    #[test]
    fn one_transaction_is_always_one_run_id() {
        let t = tempfile::tempdir().unwrap();
        let w = locate(&txn_dir(t.path())).unwrap();
        assert_eq!(run_id(&w), run_id(&w));
        assert!(run_id(&w).starts_with("20261101T120000-"));
    }
}
