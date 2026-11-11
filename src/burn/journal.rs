// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Append-only journal of executed ops with before-state snapshots, and
//! the rollback that replays it in reverse.
//!
//! The journal is write-ahead: the entry and its snapshot are on disk, and
//! synced, before the effect happens. A process killed mid-burn leaves a
//! record `cig unburn` can replay. Symlinks are recorded as links (their
//! target), never as copies of what they point at.

use super::Op;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// What a path looked like before an op touched it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Before {
    Absent,
    File {
        snapshot: PathBuf,
        sha256: String,
        bytes: u64,
        /// The file's own mode, when the snapshot is a shared tin object
        /// whose mode is not the file's.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<u32>,
        /// The file's modification time, nanoseconds since the epoch, put
        /// back on restore so mtime-driven tools see what they saw before.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mtime_ns: Option<u64>,
        /// Where the bytes are when git holds them: the repository and the
        /// blob, instead of a copy in the tin (`snapshot` is empty then).
        /// Our own hash travels too, so the bytes are checked on the way
        /// back whatever git says.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        git: Option<GitBlob>,
    },
    Dir {
        snapshot: PathBuf,
    },
    /// A directory stood here, its contents journaled on their own: a hop
    /// deleted it, or replaced it with a file. Rollback makes the directory
    /// again, with its mode, before the entries for what was inside it.
    Directory {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mtime_ns: Option<u64>,
    },
    /// A symbolic link: restored as a link to the same target.
    Symlink {
        target: PathBuf,
    },
    /// The path was moved; rollback moves it back from `to`.
    Moved {
        to: PathBuf,
    },
}

impl Before {
    /// The (mtime, size) a restore of this state leaves, when it is a file
    /// whose time was recorded.
    pub fn stamp(&self) -> Option<(u64, u64)> {
        match self {
            Before::File {
                mtime_ns: Some(m),
                bytes,
                ..
            } => Some((*m, *bytes)),
            _ => None,
        }
    }

    /// This state as `after.json` would record it, for the kinds a
    /// changed-since check can compare.
    pub fn as_path_state(&self) -> Option<PathState> {
        match self {
            Before::Absent => Some(PathState::Absent),
            Before::File { sha256, bytes, .. } => Some(PathState::File {
                sha256: sha256.clone(),
                bytes: *bytes,
            }),
            _ => None,
        }
    }
}

/// A blob in a git repository: the bytes a run refers to without copying.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitBlob {
    /// The work tree, as the disk spells it.
    pub repo: PathBuf,
    /// The object id, as the repository hashes (SHA-1, or SHA-256).
    pub blob: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BeforeState {
    pub path: PathBuf,
    #[serde(flatten)]
    pub before: Before,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub seq: u64,
    pub at: String,
    pub reversible: bool,
    pub op: Op,
    pub before: Vec<BeforeState>,
    /// Rolled back already, by a retry inside the run; skipped by `unburn`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub undone: bool,
    /// Put back whole by a rollback that left other entries failed, so the
    /// plain `unburn` that finishes the job skips it. Kept as a marker line
    /// after the entry, never in the entry itself.
    #[serde(skip)]
    pub restored: bool,
    /// What a rollback of this entry moved into the tin instead of
    /// destroying: a file saved while the hop ran, or the hop's own output.
    /// `cig unburn <id> --give-back` puts them back. Kept as a marker line
    /// after the entry; the newest marker is the current list.
    #[serde(skip)]
    pub took: Vec<BeforeState>,
    /// What the newest `--give-back` of this entry left at each path it
    /// put back, so the next one can tell an edit made since from its own
    /// work. Kept in the same marker line as `took`.
    #[serde(skip)]
    pub left_after: Vec<LeftState>,
    /// An irreversible op inside a `burn { } unburn { }` block: the
    /// compensation covers it, so the plan says `compensated`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub compensated: bool,
    /// For `Op::Compensate`: the compensation itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compensation: Option<Compensation>,
}

/// A compensation: the `unburn { }` block's source and the state map it
/// was journaled with. It runs later, in another process, from this alone.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Compensation {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_name: Option<String>,
    #[serde(default)]
    pub state: serde_json::Value,
    pub line: u32,
    /// The burn's name (`burn (s, "deploy")`), used in place of the line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// A command instead of source: what a hop said undoes it, through the
    /// burn protocol (`docs/PROTOCOL.md`). Run with no shell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    /// Where that command runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
}

/// The shape of `journal.jsonl` and `after.json` this cig writes. The
/// journal's first line says which; a cig that reads an older format reads
/// it as it always did, and a newer one is refused by its number (E808).
/// Format 3 (1.9.0) signs the chain: `prev` is an HMAC-SHA256 under the
/// `journal-mac` subkey of the line before it, so a script that rewrites the
/// journal cannot recompute it without the key; `journal_sign = off` keeps
/// writing format 2 for anyone who needs older cigs to read new runs.
/// Format 2 (1.5.0) chains the lines: each carries `prev`, the SHA-256 of
/// the line before it as written, and the first line that of the run's id,
/// so a line edited, removed or reordered after the run no longer fits.
pub const JOURNAL_FORMAT: u32 = 3;

/// The state of a journal's hash chain, found when it was loaded. A format
/// 1 journal has no chain and is never checked.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Chain {
    /// The format the journal declared (0 for one from before 1.2.0).
    pub format: u32,
    /// The run id the first line names (format 2), which the chain seeds from.
    pub run: Option<String>,
    /// Lines read, the format line included and a damaged last line aside.
    pub lines: usize,
    /// The first line whose `prev` is not the hash of the line before it.
    pub broken_at: Option<ChainBreak>,
    /// A signed journal (format 3) whose very first link does not verify
    /// under this machine's key: signed by another key. Not tampering,
    /// which breaks the chain part-way (E708, not E760).
    #[serde(default)]
    pub other_key: bool,
    /// A signed journal this cig could not check: the tin's key is not
    /// available. Nothing is judged; a note, never a refusal.
    #[serde(default)]
    pub unverified: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ChainBreak {
    /// The line in `journal.jsonl`, counted from 1.
    pub line: usize,
    pub reason: String,
}

impl Chain {
    /// Does this journal carry a chain at all?
    pub fn checked(&self) -> bool {
        self.format >= 2
    }

    /// Every line fits the one before it (true for a journal with no chain).
    pub fn intact(&self) -> bool {
        self.broken_at.is_none()
    }
}

/// HMAC-SHA256 under the journal's MAC key, hex.
fn hmac_hex(key: &[u8; 32], bytes: &[u8]) -> String {
    use hmac::Mac;
    let mut m = hmac::Hmac::<sha2::Sha256>::new_from_slice(key).expect("HMAC takes any key length");
    m.update(bytes);
    hex::encode(m.finalize().into_bytes())
}

/// One link of the chain: keyed (format 3) or plain (format 2). A `Key`
/// here is the format-3 journal MAC subkey.
fn link(key: Option<&super::seal::Key>, bytes: &[u8]) -> String {
    match key {
        Some(k) => hmac_hex(k.as_bytes(), bytes),
        None => sha256_hex(bytes),
    }
}

/// The journal MAC subkey, when the master key can be had. The key wipes
/// itself when it is let go.
fn journal_key() -> Option<super::seal::Key> {
    super::seal::key()
        .ok()
        .map(|(m, _)| super::seal::derive(&m, super::seal::LABEL_JOURNAL_MAC))
}

/// Every candidate journal MAC subkey: the current one, plus one derived
/// from each stash key a rekey left on disk. A rekey that has re-signed a
/// journal under the NEW master but not yet swapped the keeper leaves the
/// journal under NEW while `seal::key()` still gives OLD; the stash's
/// derivation covers that gap (and the reverse one). The stashes are
/// honoured only when they prove the primary (SEC-004 H2).
fn candidate_journal_keys() -> Vec<super::seal::Key> {
    let mut keys = Vec::new();
    let Ok((master, _)) = super::seal::key() else {
        return keys;
    };
    keys.push(super::seal::derive(&master, super::seal::LABEL_JOURNAL_MAC));
    for sk in super::seal::secondary_keys(&master) {
        keys.push(super::seal::derive(&sk, super::seal::LABEL_JOURNAL_MAC));
    }
    keys
}

/// Pick the journal MAC subkey this journal was signed with, by trying
/// each candidate against the header line's `prev`. The header's `prev`
/// is `link(mac_key, id_bytes)`, so the key whose derivation matches is
/// the one that signed the chain. Returns `None` when no candidate is
/// available (no primary and no stashes), or when no candidate matches.
fn pick_journal_mac_key(header: &serde_json::Value) -> Option<super::seal::Key> {
    let id = header.get("run").and_then(|r| r.as_str())?;
    let header_prev = header.get("prev").and_then(|p| p.as_str())?;
    // No candidate match: the journal is from another machine, or the
    // key is simply not available here. Returning None leaves the
    // chain-check path to mark the journal `other_key`, same as before.
    candidate_journal_keys()
        .into_iter()
        .find(|k| link(Some(k), id.as_bytes()) == header_prev)
}

/// Does the setting ask for signed journals? (`journal_sign`, on by default.)
fn signing_wanted() -> bool {
    crate::config::Config::load().get("journal_sign") != "off"
}

/// What `tail_head` found in a journal file without reading every line
/// (#177). The computed `Head` is `link(mac_key, last_line_bytes)` for
/// the last complete line on disk: the hash a run.json records when the
/// run ends, so it compares directly with `rec.journal_head` and with
/// the ledger's `head` for the run.
pub enum TailHead {
    /// The last line's head: compare with `rec.journal_head` and with the
    /// ledger line's `head` for this run.
    Head(String),
    /// Format 1 (from before 1.5.0): no chain, nothing to compare.
    NoChain,
    /// Format 3 header does not fit any key this cig has: the journal was
    /// signed by another key (another machine's, one since replaced).
    /// Comparing the head would mean nothing, and `cig unburn` would
    /// refuse this run with E708 as it stands.
    OtherKey,
    /// Format 3 but no MAC key is available here (no primary, no
    /// stash): nothing is judged, as `load()` does.
    Unverifiable,
    /// The journal is empty or has only its header line (no body): a
    /// run that burned nothing, or one whose header was written but no
    /// burn landed. A record with `journal_head = None` is consistent
    /// with this; a record with a head is not, and the caller flags it.
    Empty,
    /// The journal is missing (`NotFound`).
    Missing,
}

/// Compute a journal's chain head from its tail alone: the first line
/// gives the format and (for format 3) picks the MAC key, and the file
/// is read back from the end to find the last complete line, whose
/// bytes are hashed under that key. One cheap read per run — kept
/// bounded by `TAIL_WINDOW` and doubled when a journal's last line is
/// long enough to cross the window boundary — so a whole-history check
/// (`cig runs --verify`) stays quick at ten thousand runs (#177, #149).
pub fn tail_head(run_dir: &Path) -> io::Result<TailHead> {
    let path = run_dir.join("journal.jsonl");
    let mut f = match fs::File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(TailHead::Missing),
        Err(e) => return Err(e),
    };
    let len = f.metadata()?.len();
    if len == 0 {
        return Ok(TailHead::Empty);
    }
    // --- the first line: format, and the MAC key for a signed journal ---
    // A journal's header line is comfortably shorter than 4 KiB.
    let mut head_buf = vec![0u8; 4096];
    f.seek(SeekFrom::Start(0))?;
    let read = f.read(&mut head_buf)?;
    let head_bytes = &head_buf[..read];
    let first_end = head_bytes
        .iter()
        .position(|&b| b == b'\n')
        .unwrap_or(head_bytes.len());
    let first_line = &head_bytes[..first_end];
    let header: serde_json::Value = match serde_json::from_slice(first_line) {
        Ok(v) => v,
        // No parseable header: not a chain this tool can judge.
        Err(_) => return Ok(TailHead::NoChain),
    };
    let format = header.get("format").and_then(|f| f.as_u64()).unwrap_or(0) as u32;
    if format < 2 {
        return Ok(TailHead::NoChain);
    }
    let mac_key: Option<super::seal::Key> = if format >= 3 {
        match pick_journal_mac_key(&header) {
            Some(k) => Some(k),
            // The header does not fit any candidate key. When this
            // machine has no key at all (no primary, no stash), the
            // journal is simply unverifiable here. When it has a key
            // but none fits, the journal was signed by another key.
            None => {
                if candidate_journal_keys().is_empty() {
                    return Ok(TailHead::Unverifiable);
                }
                return Ok(TailHead::OtherKey);
            }
        }
    } else {
        None
    };
    // The file has lines beyond the header: find the last complete one.
    // Read back from the end in TAIL_WINDOW-sized bites; double the
    // window if a line is long enough to cross it, so a very long
    // last line still fits in at most a handful of reads.
    const TAIL_WINDOW: u64 = 64 * 1024;
    let mut window = TAIL_WINDOW;
    loop {
        let off = len.saturating_sub(window);
        f.seek(SeekFrom::Start(off))?;
        let mut buf = Vec::with_capacity((len - off) as usize);
        io::Read::by_ref(&mut f)
            .take(len - off)
            .read_to_end(&mut buf)?;
        // Strip a torn tail (a crashed append left half a line without
        // a newline). A journal that ends mid-line is truncated; the
        // last *complete* line is the one we report.
        if !buf.is_empty() && buf[buf.len() - 1] != b'\n' {
            if let Some(i) = buf.iter().rposition(|&b| b == b'\n') {
                buf.truncate(i + 1);
            } else {
                // The whole window is one torn line. Widen if we can.
                if off == 0 {
                    return Ok(TailHead::Empty);
                }
                window = window.saturating_mul(2);
                continue;
            }
        }
        // buf now ends on '\n' (or is empty). Find the last line by
        // the penultimate newline.
        if buf.is_empty() {
            return Ok(TailHead::Empty);
        }
        let last_line_end = buf.len() - 1; // drop trailing '\n'
        let before = &buf[..last_line_end];
        let line_start = before
            .iter()
            .rposition(|&b| b == b'\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        if line_start == 0 && off > 0 {
            // The last line reaches further back than this window;
            // widen and try again.
            window = window.saturating_mul(2);
            continue;
        }
        let line = &buf[line_start..last_line_end];
        // The header's own hash is not an "end of run" head: a
        // header-only file reads as Empty.
        if off == 0 && line_start == 0 && line == first_line {
            return Ok(TailHead::Empty);
        }
        return Ok(TailHead::Head(link(mac_key.as_ref(), line)));
    }
}

/// Rewrite a journal's chain under another link: this machine's keyed chain
/// (format 3) when a parcel's run is taken in, or the plain chain (format 2)
/// when `to_key` is `None`. Every line's `prev` is recomputed from the
/// header's seed; the header's `format` is set to match; nothing else in a
/// line changes. Returns the new text and the new head.
pub fn rechain(text: &str, to_key: Option<&super::seal::Key>) -> (String, String) {
    let mut out = String::new();
    let mut prev: Option<String> = None;
    for raw in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(mut v) = serde_json::from_str::<serde_json::Value>(raw) else {
            out.push_str(raw);
            out.push('\n');
            continue;
        };
        if v.get("format").is_some() && v.get("op").is_none() {
            v["format"] =
                serde_json::Value::from(if to_key.is_some() { JOURNAL_FORMAT } else { 2 });
            if let Some(id) = v.get("run").and_then(|r| r.as_str()).map(str::to_string) {
                prev = Some(link(to_key, id.as_bytes()));
            }
        }
        if let Some(p) = &prev {
            v["prev"] = serde_json::Value::String(p.clone());
        }
        let line = serde_json::to_string(&v).unwrap_or_else(|_| raw.to_string());
        prev = Some(link(to_key, line.as_bytes()));
        out.push_str(&line);
        out.push('\n');
    }
    (out, prev.unwrap_or_default())
}

/// Re-sign a run's journal under this machine's key, in place, and return
/// the new head (to record on the run). Used when a parcel's run is taken
/// in: the record then verifies here like any other. With no key to be
/// had, the journal is left as it came (it reads as another key's, E708).
pub fn resign_in_place(run_dir: &Path) -> io::Result<Option<String>> {
    let Some(key) = journal_key() else {
        return Ok(None);
    };
    resign_in_place_under(run_dir, &key)
}

/// `resign_in_place` with the MAC subkey given explicitly. `cig tin rekey`
/// calls this with the new journal-mac subkey, because the in-process
/// cached master is still the old one (the `OnceLock` for `seal::key()`
/// was filled before the swap); working from a cached stale key would
/// write an old MAC over the new one.
pub fn resign_in_place_under(
    run_dir: &Path,
    mac_key: &super::seal::Key,
) -> io::Result<Option<String>> {
    let path = run_dir.join("journal.jsonl");
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let (new_text, head) = rechain(&text, Some(mac_key));
    let tmp = run_dir.join("journal.jsonl.resign");
    fs::write(&tmp, new_text.as_bytes())?;
    let f = fs::File::open(&tmp)?;
    full_sync_data(&f)?;
    drop(f);
    fs::rename(&tmp, &path)?;
    sync_dir(run_dir)?;
    Ok(Some(head))
}

/// Only re-sign a journal that was signed to begin with. A format-2
/// journal (no MAC key, plain-SHA chain) is left as-is: the master key
/// never signed it, so there is no signature of ours to rotate, and
/// quietly upgrading it to signed would make a run that was never
/// vouched for look like one that was (finding 3). Returns the new head
/// when the journal was signed and re-signed, `None` otherwise.
pub fn resign_signed_in_place_under(
    run_dir: &Path,
    mac_key: &super::seal::Key,
) -> io::Result<Option<String>> {
    let path = run_dir.join("journal.jsonl");
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    // Peek at the first intact line's declared format.
    let first_format: Option<u32> = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .and_then(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .and_then(|v| v.get("format").and_then(|f| f.as_u64()).map(|n| n as u32));
    match first_format {
        Some(f) if f >= 3 => resign_in_place_under(run_dir, mac_key),
        _ => Ok(None),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

/// The id a run directory was made under, for the chain's first line.
fn dir_id(run_dir: &Path) -> String {
    run_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// What the first line of a run's journal carries as `prev`: the hash of
/// the run's id, which the line names too (`run`), so a renamed directory
/// still verifies and `--verify` can say the name no longer matches.
fn chain_seed(run_id: &str) -> String {
    sha256_hex(run_id.as_bytes())
}

pub struct Journal {
    run_dir: Option<PathBuf>,
    file: Option<fs::File>,
    entries: Vec<Entry>,
    /// The format the journal on disk declares; 0 for one from before 1.2.0.
    format: u32,
    /// The last line on disk was cut short (a crash mid-append); what came
    /// before it loaded.
    truncated: bool,
    /// The SHA-256 of the last line on disk, which the next line carries as
    /// `prev`; `run.json` records it when the run ends.
    head: String,
    /// What the chain check found when the journal was loaded.
    chain: Chain,
    /// The MAC key a signed journal (format 3) links with; `None` for an
    /// unsigned one, or when the key could not be had. Wiped on drop.
    mac_key: Option<super::seal::Key>,
    /// The first line is not written yet. It is written, and the key asked
    /// for, with the first entry, so a run that burns nothing never touches
    /// the keyring (#129).
    pending: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct RollbackReport {
    pub restored: Vec<String>,
    pub irreversible: Vec<String>,
    /// Irreversible ops a compensation covers.
    pub compensated: Vec<String>,
    /// Compensations found and not run (no runner was given), newest first.
    pub compensations: Vec<Compensation>,
    pub failed: Vec<String>,
    /// Files the rollback moved into the tin instead of destroying, so a
    /// save made while a hop ran can be given back.
    pub took: Vec<String>,
    /// Everything that happened, in the order it happened: (label, text).
    /// Labels: restored, compensated, cannot undo, failed, left, gave back.
    pub actions: Vec<(String, String)>,
}

/// Runs a compensation on the journal's behalf; `Err` is the message.
pub type CompensationRunner<'a> = &'a mut dyn FnMut(&Compensation) -> Result<(), String>;

impl RollbackReport {
    pub fn clean(&self) -> bool {
        self.failed.is_empty()
    }
}

impl Journal {
    pub fn open(run_dir: Option<PathBuf>) -> Result<Self, io::Error> {
        let mut head = String::new();
        let mut chain = Chain::default();
        let mut format = JOURNAL_FORMAT;
        let mut mac_key: Option<super::seal::Key> = None;
        let mut pending = false;
        let file = match &run_dir {
            Some(dir) => {
                fs::create_dir_all(dir.join("snapshots"))?;
                let f = fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(dir.join("journal.jsonl"))?;
                if f.metadata()?.len() == 0 {
                    // The first line is written with the first entry
                    // (`write_header`), not now: a run that burns nothing
                    // never asks for the key.
                    pending = true;
                    chain.format = format;
                } else {
                    // Reopened: the chain goes on from the last intact line.
                    let loaded = Self::load(dir)?;
                    head = loaded.head;
                    chain = loaded.chain;
                    format = loaded.format;
                    mac_key = loaded.mac_key;
                }
                sync_dir(dir)?;
                Some(f)
            }
            None => None,
        };
        Ok(Self {
            run_dir,
            file,
            entries: Vec::new(),
            format,
            truncated: false,
            head,
            chain,
            mac_key,
            pending,
        })
    }

    /// The first line: it names the format and seeds the chain. It has no
    /// `op`, so a cig from before formats reads it as a marker and goes on.
    /// Signed (format 3) when the setting asks and the key can be had; else
    /// format 2. Written with the first entry, so a run that burns nothing
    /// never asks for the key (#129).
    fn write_header(&mut self) -> io::Result<()> {
        let Some(dir) = self.run_dir.clone() else {
            return Ok(());
        };
        let Some(f) = &mut self.file else {
            return Ok(());
        };
        let mac_key = if signing_wanted() {
            journal_key()
        } else {
            None
        };
        let format = if mac_key.is_some() { JOURNAL_FORMAT } else { 2 };
        let id = dir_id(&dir);
        let line = format!(
            "{{\"format\":{format},\"run\":\"{id}\",\"prev\":\"{}\"}}",
            link(mac_key.as_ref(), id.as_bytes())
        );
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
        f.flush()?;
        full_sync_data(f)?;
        sync_dir(&dir)?;
        self.head = link(mac_key.as_ref(), line.as_bytes());
        self.format = format;
        self.chain.format = format;
        self.chain.lines = 1;
        self.mac_key = mac_key;
        self.pending = false;
        Ok(())
    }

    /// Nothing was ever written: the run burned nothing, so it has no head
    /// and the ledger has nothing to keep for it (#129).
    pub fn wrote_nothing(&self) -> bool {
        self.pending
    }

    /// Load a finished run's journal for `cig unburn`.
    pub fn load(run_dir: &Path) -> Result<Self, io::Error> {
        // Take the process's shared rekey lock BEFORE reading the
        // journal text (SEC-004 N2): a `cig tin rekey` that holds the
        // exclusive lock cannot then rewrite the file out from under
        // us. Without this order, an unburn that enters mid-rekey
        // reads the pre-rekey chain, the `seal::key()` call under
        // `journal_key()` blocks on the same lock, and when the rekey
        // finishes the appended compensation markers chain to a stale
        // head. Shared lock, held once per process; a verify-only
        // call pays the cost once and later `key()` calls skip it.
        super::seal::hold_shared_rekey_lock().map_err(io::Error::other)?;
        let text = fs::read_to_string(run_dir.join("journal.jsonl"))?;
        // A run that came in a parcel reads through its path map: the
        // lines stay as written (the chain holds), the paths are ours.
        let map = super::parcel::path_map_of(run_dir);
        let mut entries: Vec<Entry> = Vec::new();
        let mut truncated = false;
        let mut format = 0;
        let mut chain = Chain::default();
        // What this line's `prev` must be: the hash of the line before it,
        // or of the run's id for the first (the id the first line names;
        // the directory's name when it does not, as 1.4 journals do not).
        let mut expect = chain_seed(&dir_id(run_dir));
        // Set when the header says format 3: the key, or none (then nothing
        // is checked and the chain reads as another key's).
        let mut mac_key: Option<super::seal::Key> = None;
        let mut unverifiable = false;
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        for (i, line) in lines.iter().enumerate() {
            let parsed: Result<serde_json::Value, _> = serde_json::from_str(line);
            let v = match parsed {
                Ok(v) => v,
                // Only the last line may be damaged: that is what a crash
                // mid-append leaves. Everything before it is intact and usable.
                Err(_) if i + 1 == lines.len() => {
                    truncated = true;
                    break;
                }
                Err(e) => return Err(io::Error::new(io::ErrorKind::InvalidData, e)),
            };
            if let Some(f) = v.get("format").and_then(|f| f.as_u64()) {
                if v.get("op").is_none() {
                    if f > u64::from(JOURNAL_FORMAT) {
                        let version = fs::read_to_string(run_dir.join("run.json"))
                            .ok()
                            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                            .and_then(|r| r["version"].as_str().map(str::to_string))
                            .unwrap_or_default();
                        return Err(super::runs::newer_format(
                            "journal.jsonl",
                            f as u32,
                            &version,
                        ));
                    }
                    format = f as u32;
                    chain.format = format;
                    if format >= 3 {
                        // During a rekey mid-transition a signed journal
                        // may be under either the current or a stash's
                        // derivation. Pick the one whose link matches
                        // this header's own `prev`; if none does, the
                        // key is simply not available and the chain is
                        // left `unverified` as before.
                        mac_key = pick_journal_mac_key(&v).or_else(journal_key);
                        if mac_key.is_none() {
                            unverifiable = true;
                            chain.unverified = true;
                        }
                    }
                    if let Some(id) = v.get("run").and_then(|r| r.as_str()) {
                        expect = link(mac_key.as_ref(), id.as_bytes());
                        chain.run = Some(id.to_string());
                    }
                }
            }
            // From format 2 on, every line names the hash of the line before
            // it. The first break is kept; what follows it is read all the
            // same, so the record can still be shown.
            if chain.format >= 2 && chain.broken_at.is_none() && !unverifiable {
                let reason = match v.get("prev").and_then(|p| p.as_str()) {
                    Some(p) if p == expect => None,
                    Some(_) if i == 0 && chain.format >= 3 => {
                        // The very first link does not verify: this journal
                        // was signed by another key (another machine, a key
                        // since replaced). Nothing after it can be judged.
                        chain.other_key = true;
                        unverifiable = true;
                        None
                    }
                    // A signed journal's links are MACs under the tin's
                    // key: one that does not match was not signed by it.
                    Some(_) if chain.format >= 3 => Some(
                        "its signature does not match: this line was not signed by your key (edited, removed or reordered, or re-chained by something without the key)",
                    ),
                    Some(_) => Some(
                        "its prev is not the hash of the line before it: a line was edited, removed or reordered",
                    ),
                    None => Some("it carries no prev, which every line of this format does"),
                };
                if let Some(reason) = reason {
                    chain.broken_at = Some(ChainBreak {
                        line: i + 1,
                        reason: reason.to_string(),
                    });
                }
            }
            expect = link(mac_key.as_ref(), line.as_bytes());
            chain.lines = i + 1;
            // Markers: `{"seq": N, "undone": true}` says an earlier entry was
            // rolled back by a retry, or never happened (`"failed": true`);
            // `{"seq": N, "compensated": true}` says its block completed and
            // the compensation recorded after it covers the op.
            if v.get("op").is_none() {
                let seq = v.get("seq").and_then(|n| n.as_u64());
                let target = seq.and_then(|seq| entries.iter_mut().find(|e| e.seq == seq));
                if let Some(e) = target {
                    if v.get("undone").and_then(|u| u.as_bool()) == Some(true) {
                        e.undone = true;
                    }
                    if v.get("restored").and_then(|u| u.as_bool()) == Some(true) {
                        e.restored = true;
                    }
                    if v.get("compensated").and_then(|u| u.as_bool()) == Some(true) {
                        e.compensated = true;
                    }
                    if let Some(t) = v.get("took") {
                        if let Ok(mut states) =
                            serde_json::from_value::<Vec<BeforeState>>(t.clone())
                        {
                            for st in &mut states {
                                remap_state(st, &map);
                            }
                            e.took = states;
                        }
                        let mut left = v
                            .get("left")
                            .and_then(|l| serde_json::from_value::<Vec<LeftState>>(l.clone()).ok())
                            .unwrap_or_default();
                        for l in &mut left {
                            l.path = super::parcel::remap(&l.path, &map);
                        }
                        e.left_after = left;
                    }
                }
                continue;
            }
            let mut e: Entry = serde_json::from_value(v)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            if !map.is_empty() {
                remap_entry(&mut e, &map);
            }
            entries.push(e);
        }
        // A journal with no line is a run that burned nothing (#129): read
        // as the current format with nothing in it, and a first append
        // writes its header then.
        let empty = text.lines().all(|l| l.trim().is_empty());
        let format = if empty { JOURNAL_FORMAT } else { format };
        if empty {
            chain.format = JOURNAL_FORMAT;
        }
        Ok(Self {
            run_dir: Some(run_dir.to_path_buf()),
            file: None,
            entries,
            format,
            truncated,
            head: expect,
            chain,
            mac_key,
            pending: empty,
        })
    }

    /// The format the journal on disk declared (0: from before 1.2.0).
    pub fn format(&self) -> u32 {
        self.format
    }

    /// The hash of the last line on disk: the chain's head.
    pub fn head(&self) -> &str {
        &self.head
    }

    /// What the chain check found when the journal was loaded.
    pub fn chain(&self) -> &Chain {
        &self.chain
    }

    /// True when the journal on disk ended mid-line (a crash while appending)
    /// and only the entries before that line were loaded.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    pub fn run_dir(&self) -> Option<&Path> {
        self.run_dir.as_deref()
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Burns still standing: not undone by a retry, and not a compensation
    /// record (which is the undo, not an effect).
    pub fn len(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| !e.undone && e.compensation.is_none())
            .count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Every entry, undone ones included: a position for `rollback_since`.
    pub fn mark(&self) -> usize {
        self.entries.len()
    }

    /// Irreversible entries recorded since `mark`.
    pub fn irreversible_since(&self, mark: usize) -> usize {
        self.entries
            .iter()
            .skip(mark)
            .filter(|e| !e.reversible && !e.undone)
            .count()
    }

    /// Roll back the entries recorded since `mark`, newest first, and mark
    /// them undone in the journal so a later `unburn` skips them. The op
    /// numbers stay; the retry's own entries follow.
    pub fn rollback_since(&mut self, mark: usize) -> RollbackReport {
        let report = self.rollback_from(mark, None, false);
        let mut markers = Vec::new();
        for e in self.entries.iter_mut().skip(mark) {
            if !e.undone {
                e.undone = true;
                markers.push(serde_json::json!({"seq": e.seq, "undone": true}));
            }
        }
        self.append_markers(markers);
        report
    }

    /// Append one line to the journal on disk, chained to the one before it
    /// and made durable. A journal loaded for `unburn` has no file open: it
    /// is opened to append. An in-memory journal writes nothing.
    ///
    /// The process-level shared rekey lock is taken the first time this
    /// process asks for the key (`seal::key()`), so a `cig tin rekey`
    /// holding the exclusive lock cannot sign anything out from under a
    /// run that lives (SEC-004 H1). This method itself is lock-free.
    fn append_line(&mut self, mut v: serde_json::Value) -> io::Result<()> {
        if self.file.is_none() {
            if let Some(dir) = &self.run_dir {
                self.file = Some(
                    fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(dir.join("journal.jsonl"))?,
                );
            }
        }
        if self.pending {
            self.write_header()?;
        }
        let Some(f) = &mut self.file else {
            return Ok(());
        };
        // A format 1 journal (one an older cig wrote) stays format 1: its
        // markers carry no `prev`, as none of its lines do.
        if self.format >= 2 {
            v["prev"] = serde_json::Value::String(self.head.clone());
        }
        let line =
            serde_json::to_string(&v).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
        f.flush()?;
        full_sync_data(f)?;
        self.head = link(self.mac_key.as_ref(), line.as_bytes());
        self.chain.lines += 1;
        Ok(())
    }

    /// Append marker lines; a marker that cannot be written is not fatal to
    /// the rollback it describes, which already happened.
    fn append_markers(&mut self, markers: Vec<serde_json::Value>) {
        for m in markers {
            let _ = self.append_line(m);
        }
    }

    /// The newest entry's effect never happened (its syscall refused before
    /// changing anything): mark it undone, in memory and on disk, so
    /// rollback skips it. Returns the entry's (reversible, compensated).
    pub fn mark_last_failed(&mut self) -> Option<(bool, bool)> {
        let e = self.entries.last_mut()?;
        if e.undone || e.compensation.is_some() {
            return None;
        }
        e.undone = true;
        let out = (e.reversible, e.compensated);
        let seq = e.seq;
        self.append_markers(vec![
            serde_json::json!({"seq": seq, "undone": true, "failed": true}),
        ]);
        Some(out)
    }

    /// The block that began at `mark` completed and its compensation is
    /// recorded: stamp its irreversible ops `compensated`, in memory and
    /// on disk. Returns how many were stamped.
    pub fn mark_compensated_since(&mut self, mark: usize) -> usize {
        let mut markers = Vec::new();
        for e in self.entries.iter_mut().skip(mark) {
            if !e.reversible && !e.undone && !e.compensated && e.compensation.is_none() {
                e.compensated = true;
                markers.push(serde_json::json!({"seq": e.seq, "compensated": true}));
            }
        }
        let count = markers.len();
        self.append_markers(markers);
        count
    }

    /// Snapshot everything `op` will change, sync it, then append and sync
    /// the entry. Only after both are durable does the effect happen.
    pub fn record(&mut self, seq: u64, op: Op) -> Result<(), io::Error> {
        self.record_with(seq, op, None, false)
    }

    /// `record`, with a compensation attached (for `Op::Compensate`) or the
    /// `compensated` label (an irreversible op inside a compensating burn).
    pub fn record_with(
        &mut self,
        seq: u64,
        op: Op,
        compensation: Option<Compensation>,
        compensated: bool,
    ) -> Result<(), io::Error> {
        let before = self.capture_before(seq, &op)?;
        self.record_with_before(seq, op, before, compensation, compensated)
    }

    /// `record_with` for an op whose before-state was captured earlier: a
    /// file the tin kept before a hop changed it.
    pub fn record_with_before(
        &mut self,
        seq: u64,
        op: Op,
        before: Vec<BeforeState>,
        compensation: Option<Compensation>,
        compensated: bool,
    ) -> Result<(), io::Error> {
        let entry = Entry {
            seq,
            at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            reversible: op.reversible(),
            op,
            before,
            undone: false,
            restored: false,
            took: Vec::new(),
            left_after: Vec::new(),
            compensated,
            compensation,
        };
        if self.file.is_some() {
            let v = serde_json::to_value(&entry)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            self.append_line(v)?;
        }
        self.entries.push(entry);
        Ok(())
    }

    /// Before-states are listed outermost-first (a created parent, then the
    /// path itself); rollback applies them innermost-first.
    fn capture_before(&self, seq: u64, op: &Op) -> Result<Vec<BeforeState>, io::Error> {
        Ok(match op {
            Op::Write { path, sensitive } | Op::Append { path, sensitive } => {
                self.destination_states(seq, path, 0, *sensitive)?
            }
            Op::Delete { path } => vec![self.snapshot(seq, path, 0, false)?],
            Op::Mkdir { path } => {
                // `mkdir -p` may create several levels; remember the highest
                // one that is about to appear so rollback removes all of them.
                let created_root = first_missing_ancestor(path);
                vec![self.snapshot(seq, &created_root, 0, false)?]
            }
            Op::Copy { to, .. } => self.destination_states(seq, to, 0, false)?,
            Op::Move { from, to } => {
                let mut states = vec![BeforeState {
                    path: from.clone(),
                    before: Before::Moved { to: to.clone() },
                }];
                // A move between two spellings of one name on a
                // case-insensitive filesystem has no destination of its
                // own: what is at `to` is the file itself, and a snapshot
                // of it restored after the move back would only put the
                // same bytes under the other spelling.
                if !(super::case_variants(from, to) && super::same_object(from, to)) {
                    states.extend(self.destination_states(seq, to, 1, false)?);
                }
                states
            }
            // A hop created this file: it did not exist before the hop.
            Op::HopCreated { path, .. } => vec![BeforeState {
                path: path.clone(),
                before: Before::Absent,
            }],
            Op::HopChanged { .. }
            | Op::Compensate { .. }
            | Op::Proc { .. }
            | Op::EnvSet { .. }
            | Op::EnvUnset { .. } => Vec::new(),
        })
    }

    /// Before-states for a path that is about to receive bytes: the highest
    /// directory that will be created, the path itself, and, when the path
    /// is a symlink, the place the bytes actually land.
    fn destination_states(
        &self,
        seq: u64,
        path: &Path,
        slot: usize,
        sensitive: bool,
    ) -> Result<Vec<BeforeState>, io::Error> {
        let mut states = Vec::new();
        let created_root = first_missing_ancestor(path);
        if created_root != *path {
            states.push(self.snapshot(seq, &created_root, slot, false)?);
        }
        states.push(self.snapshot(seq, path, slot + 1, sensitive)?);
        if is_symlink(path) {
            let landing = resolve_symlink_chain(path);
            if landing != *path {
                states.push(self.snapshot(seq, &landing, slot + 2, sensitive)?);
            }
        }
        Ok(states)
    }

    /// The before-image of one path, in the run's own directory. A secret
    /// (the op said so, or the name is a secret's and sealing is on) is kept
    /// sealed, under `<name>.cig-sealed`, with its mode in the journal.
    fn snapshot(
        &self,
        seq: u64,
        path: &Path,
        slot: usize,
        sensitive: bool,
    ) -> Result<BeforeState, io::Error> {
        let meta = match fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(BeforeState {
                    path: path.to_path_buf(),
                    before: Before::Absent,
                })
            }
            Err(e) => return Err(e),
        };
        if meta.file_type().is_symlink() {
            // A link is recorded as a link, whatever it points at, and
            // whether or not the target exists.
            return Ok(BeforeState {
                path: path.to_path_buf(),
                before: Before::Symlink {
                    target: fs::read_link(path)?,
                },
            });
        }
        if !meta.is_dir() && !meta.is_file() {
            // A pipe would block the copy forever; a socket or device has
            // no content to keep. The op is refused before anything changes.
            return Err(io::Error::other(format!(
                "{} is not a regular file or directory (a pipe, socket or device); the kernel cannot snapshot it",
                path.display()
            )));
        }
        let Some(dir) = &self.run_dir else {
            // In-memory journal (eval/repl): remember only that it existed.
            return Ok(BeforeState {
                path: path.to_path_buf(),
                before: if meta.is_dir() {
                    Before::Dir {
                        snapshot: PathBuf::new(),
                    }
                } else {
                    Before::File {
                        snapshot: PathBuf::new(),
                        sha256: String::new(),
                        bytes: meta.len(),
                        mode: None,
                        mtime_ns: None,
                        git: None,
                    }
                },
            });
        };
        let base = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "root".to_string());
        let snap = dir
            .join("snapshots")
            .join(format!("{seq:05}-{slot}-{base}"));
        if meta.is_dir() {
            copy_tree_durable(path, &snap)?;
            sync_dir(&dir.join("snapshots"))?;
            Ok(BeforeState {
                path: path.to_path_buf(),
                before: Before::Dir { snapshot: snap },
            })
        } else if super::seal::policy() != super::seal::Policy::Never
            && (sensitive || super::seal::should_seal_here(path))
        {
            let sealed = snap.with_file_name(format!(
                "{}{}",
                snap.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default(),
                super::seal::SUFFIX
            ));
            let (sha256, _) = super::seal::seal_file(path, &sealed, &super::seal::aad_of(&sealed))?;
            sync_dir(&dir.join("snapshots"))?;
            Ok(BeforeState {
                path: path.to_path_buf(),
                before: Before::File {
                    snapshot: sealed,
                    sha256,
                    bytes: meta.len(),
                    mode: super::tin::mode_of(&meta),
                    mtime_ns: mtime_ns(&meta),
                    git: None,
                },
            })
        } else {
            copy_file_with_mode(path, &snap)?;
            sync_file(&snap)?;
            sync_dir(&dir.join("snapshots"))?;
            let sha256 = sha256_file(&snap)?;
            Ok(BeforeState {
                path: path.to_path_buf(),
                before: Before::File {
                    snapshot: snap,
                    sha256,
                    bytes: meta.len(),
                    mode: None,
                    mtime_ns: mtime_ns(&meta),
                    git: None,
                },
            })
        }
    }

    /// Everything a rollback would need, checked before anything is touched.
    /// Returns one line per problem; empty means safe to proceed.
    pub fn verify(&self) -> Vec<String> {
        let mut problems = Vec::new();
        for (idx, entry) in self.entries.iter().enumerate() {
            if !entry.reversible || entry.undone {
                continue;
            }
            // Rollback runs newest-first, so a later entry's before-state
            // for a path puts it back before this entry needs it.
            let later_restores = |path: &Path| {
                self.entries[idx + 1..]
                    .iter()
                    .any(|e| e.reversible && !e.undone && e.before.iter().any(|b| b.path == *path))
            };
            for state in &entry.before {
                match &state.before {
                    Before::File {
                        snapshot,
                        sha256,
                        bytes,
                        git: Some(g),
                        ..
                    } => {
                        // The bytes live in git; a copy in the tin (a parcel
                        // brought one) stands in when git has lost them.
                        let fallback = super::tin::object_for(sha256);
                        if fallback.is_none() || snapshot.as_os_str().is_empty() {
                            for p in super::git::check_blob(&g.repo, &g.blob, *bytes) {
                                if fallback.is_some() {
                                    break;
                                }
                                problems.push(format!(
                                    "op {}: {} for {}",
                                    entry.seq,
                                    p,
                                    state.path.display()
                                ));
                            }
                        }
                    }
                    Before::File {
                        snapshot, sha256, ..
                    } => {
                        if snapshot.as_os_str().is_empty() {
                            problems.push(format!(
                                "op {}: no snapshot was taken for {} (in-memory journal)",
                                entry.seq,
                                state.path.display()
                            ));
                        } else if !snapshot.is_file() {
                            problems.push(format!(
                                "op {}: snapshot missing for {} (expected {})",
                                entry.seq,
                                state.path.display(),
                                snapshot.display()
                            ));
                        } else if super::seal::is_sealed_path(snapshot) {
                            for p in super::tin::check_sealed(snapshot, sha256) {
                                problems.push(format!(
                                    "op {}: {} for {}",
                                    entry.seq,
                                    p,
                                    state.path.display()
                                ));
                            }
                        } else if super::tin::is_recipe(snapshot) {
                            for p in super::tin::check_recipe(snapshot, sha256) {
                                problems.push(format!(
                                    "op {}: {} for {}",
                                    entry.seq,
                                    p,
                                    state.path.display()
                                ));
                            }
                        } else if !sha256.is_empty() {
                            match sha256_file(snapshot) {
                                Ok(actual) if &actual == sha256 => {}
                                Ok(actual) => problems.push(format!(
                                    "op {}: snapshot for {} is corrupt (sha256 {} recorded, {} on disk)",
                                    entry.seq,
                                    state.path.display(),
                                    &sha256[..12],
                                    &actual[..12]
                                )),
                                Err(e) => problems.push(format!(
                                    "op {}: cannot read snapshot for {}: {e}",
                                    entry.seq,
                                    state.path.display()
                                )),
                            }
                        }
                    }
                    Before::Dir { snapshot } => {
                        if snapshot.as_os_str().is_empty() || !snapshot.is_dir() {
                            problems.push(format!(
                                "op {}: directory snapshot missing for {}",
                                entry.seq,
                                state.path.display()
                            ));
                        }
                    }
                    Before::Moved { to } => {
                        if fs::symlink_metadata(to).is_err() && !later_restores(to) {
                            problems.push(format!(
                                "op {}: {} is no longer at {} to move back",
                                entry.seq,
                                state.path.display(),
                                to.display()
                            ));
                        }
                    }
                    Before::Absent | Before::Symlink { .. } | Before::Directory { .. } => {}
                }
            }
        }
        problems
    }

    /// Restore every before-state, newest entry first. Call `verify` first;
    /// this does what it can and reports what it could not.
    pub fn rollback(&mut self) -> RollbackReport {
        self.rollback_from(0, None, false)
    }

    /// Roll back with compensations run in place, newest first, by `runner`.
    pub fn rollback_with(&mut self, runner: CompensationRunner<'_>) -> RollbackReport {
        self.rollback_from(0, Some(runner), false)
    }

    /// Roll back everything again, the entries an earlier pass put back
    /// included: `cig unburn --force`.
    pub fn rollback_again_with(&mut self, runner: CompensationRunner<'_>) -> RollbackReport {
        self.rollback_from(0, Some(runner), true)
    }

    /// The run this journal belongs to, for the hints a rollback prints.
    fn run_id(&self) -> String {
        self.run_dir
            .as_ref()
            .and_then(|d| d.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "<run>".to_string())
    }

    /// Put back what a rollback of this run moved into the tin: the files
    /// it displaced go back where they were, and what is there now goes to
    /// the tin in their place, so the step is as reversible as the rollback
    /// was. `cig unburn <id> --give-back`.
    pub fn give_back(&mut self) -> RollbackReport {
        let mut report = RollbackReport::default();
        let mut took: Vec<(u64, Vec<BeforeState>, Vec<LeftState>)> = Vec::new();
        for entry in self.entries.iter().rev() {
            if entry.took.is_empty() {
                continue;
            }
            let mut now_taken = Vec::new();
            let mut left = Vec::new();
            for state in &entry.took {
                match restore(state, true) {
                    Ok((_, taken)) => {
                        now_taken.extend(taken);
                        if let Some(l) = PathState::from_before(&state.before) {
                            left.push(LeftState {
                                path: state.path.clone(),
                                state: l,
                            });
                        }
                        let text = format!(
                            "{} (the rollback of {} had moved it to the tin)",
                            state.path.display(),
                            entry.op.describe()
                        );
                        report.restored.push(text.clone());
                        report.actions.push(("gave back".to_string(), text));
                    }
                    Err(e) => {
                        let text = format!("{}: {e}", state.path.display());
                        report.failed.push(text.clone());
                        report.actions.push(("failed".to_string(), text));
                    }
                }
            }
            took.push((entry.seq, now_taken, left));
        }
        self.mark_took(took);
        report
    }

    /// What `--give-back` would do, without doing it: each file the
    /// rollback moved into the tin, what the tin holds for it, what is at
    /// the path now, and whether that is still what the rollback left.
    pub fn give_back_plan(&self) -> Vec<GiveBack> {
        let mut out = Vec::new();
        for entry in self.entries.iter().rev() {
            for state in &entry.took {
                let (sha256, bytes) = match &state.before {
                    Before::File { sha256, bytes, .. } => (sha256.clone(), *bytes),
                    _ => (String::new(), 0),
                };
                let left = rollback_left(entry, &state.path);
                let now = PathState::observe(&state.path);
                let changed = left.as_ref().is_some_and(|l| !now.same_as(l));
                out.push(GiveBack {
                    seq: entry.seq,
                    path: state.path.clone(),
                    sha256,
                    bytes,
                    now,
                    left,
                    changed,
                });
            }
        }
        out
    }

    fn rollback_from(
        &mut self,
        from: usize,
        mut runner: Option<CompensationRunner<'_>>,
        again: bool,
    ) -> RollbackReport {
        let mut report = RollbackReport::default();
        let run_id = self.run_id();
        // Entries put back whole, stamped in the journal when the pass is
        // over so that a later pass, retrying what failed, skips them.
        let mut done: Vec<u64> = Vec::new();
        // What the pass moved into the tin, by entry.
        let mut took: Vec<(u64, Vec<BeforeState>, Vec<LeftState>)> = Vec::new();
        // The compensation just run failed: the irreversible ops it covers,
        // which follow it in this newest-first order, are not undone, and
        // the report must not call them compensated.
        let mut undo_failed = false;
        // What the run last left at each path a hop touched: the `left` of
        // the newest entry for the path. A file the run's own later hop
        // changed again is the run's, not someone else's.
        let mut latest_left: std::collections::BTreeMap<PathBuf, Option<(u64, u64)>> =
            std::collections::BTreeMap::new();
        // The bytes expected at a path, where an entry's restore (this pass
        // or an earlier one) decides them rather than the run's after-state.
        let mut latest_sha: std::collections::BTreeMap<PathBuf, String> =
            std::collections::BTreeMap::new();
        // Paths a newer native op's before-state covers: a directory it
        // deleted or replaced accounts for every file under it, so a hop's
        // file inside is the run's own work too, however it reads now.
        let mut covered: Vec<PathBuf> = Vec::new();
        for e in self.entries.iter().skip(from).rev() {
            if e.undone {
                continue;
            }
            if let Some(p) = e.op.path() {
                // A hop's path is absolute; a native op's is as the script
                // wrote it. Both are read the same way before they are compared.
                let abs = super::normalize_root(&p.to_string_lossy());
                let under_newer = covered.iter().any(|d| {
                    let d = super::normalize_root(&d.to_string_lossy());
                    abs.starts_with(&d) && d != abs
                });
                latest_left.entry(p.clone()).or_insert_with(|| {
                    if under_newer {
                        None
                    } else if e.restored && !again {
                        // An earlier pass put this entry back: the path holds
                        // its before-state now, not what it left (#125).
                        e.before.first().and_then(|b| b.before.stamp())
                    } else {
                        e.op.left()
                    }
                });
                if e.restored && !again {
                    if let Some(Before::File { sha256, .. }) = e.before.first().map(|b| &b.before) {
                        latest_sha
                            .entry(p.clone())
                            .or_insert_with(|| sha256.clone());
                    }
                }
            }
            for b in &e.before {
                latest_left.entry(b.path.clone()).or_insert(None);
                if e.op.path().is_none() {
                    covered.push(b.path.clone());
                }
            }
        }
        // What the run recorded leaving at each path, by hash: a file whose
        // time or size differs from the hop's stamp but whose bytes are
        // still the run's (copied to another machine, touched) is the
        // run's own work, not someone else's.
        let after = if again { None } else { self.after_state() };
        for entry in self.entries.iter().skip(from).rev() {
            if entry.undone || (entry.restored && !again) {
                continue;
            }
            if let Some(c) = &entry.compensation {
                undo_failed = false;
                match runner.as_mut() {
                    Some(run) => match run(c) {
                        Ok(()) => {
                            let text = match &c.label {
                                Some(l) => format!("ran the compensation for {l}"),
                                None => {
                                    format!("ran the compensation for the burn at line {}", c.line)
                                }
                            };
                            report.actions.push(("compensated".to_string(), text));
                            done.push(entry.seq);
                        }
                        Err(e) => {
                            undo_failed = true;
                            // Named for the burn, once: a tool written in
                            // CigScript labels its burns with the user's words,
                            // and its own line number is noise to that user.
                            let text = match &c.label {
                                Some(l) => format!("{l} is NOT undone: its compensation failed: {e}"),
                                None => format!(
                                    "the burn at line {} was not undone: its compensation failed: {e}",
                                    c.line
                                ),
                            };
                            report.failed.push(text.clone());
                            report.actions.push(("failed".to_string(), text));
                        }
                    },
                    None => report.compensations.push(c.clone()),
                }
                continue;
            }
            if !entry.reversible {
                if entry.compensated && undo_failed {
                    let text = format!(
                        "{} (its compensation failed, so it stands; once the compensation is fixed, cig unburn {run_id} runs it again)",
                        entry.op.describe()
                    );
                    report.irreversible.push(text.clone());
                    report.actions.push(("not undone".to_string(), text));
                } else if entry.compensated {
                    report.compensated.push(entry.op.describe());
                    report
                        .actions
                        .push(("compensated".to_string(), entry.op.describe()));
                } else {
                    report.irreversible.push(entry.op.describe());
                    report
                        .actions
                        .push(("cannot undo".to_string(), entry.op.describe()));
                }
                continue;
            }
            // Before-states are captured outermost-first (created parent, then
            // the path itself) and restored innermost-first, except that a
            // move is always moved back before anything else.
            let mut order: Vec<&BeforeState> = entry.before.iter().collect();
            order.reverse();
            if let Some(pos) = order
                .iter()
                .position(|s| matches!(s.before, Before::Moved { .. }))
            {
                let moved = order.remove(pos);
                order.insert(0, moved);
            }
            let failed_before = report.failed.len();
            // A hop's write set was observed, not performed: the kernel
            // cannot tell the hop's bytes from a save made while it ran, so
            // nothing a hop entry's rollback displaces is destroyed. It goes
            // to the tin, and the record says how to get it back.
            let hop = matches!(entry.op, Op::HopCreated { .. } | Op::HopChanged { .. });
            // A file the hop left that changed again after the hop ended is
            // someone else's work now: it stays, unless `--force` asks for
            // the old bytes on purpose.
            if hop && !again {
                if let (Some(left), Some(state)) = (
                    entry
                        .before
                        .first()
                        .and_then(|st| latest_left.get(&st.path).copied().flatten()),
                    entry.before.first(),
                ) {
                    if let Some(now) = file_stamp(&state.path) {
                        let same_bytes = || {
                            let now = sha256_file(&state.path).ok();
                            if let Some(want) = latest_sha.get(&state.path) {
                                return now.as_deref() == Some(want.as_str());
                            }
                            after
                                .as_ref()
                                .and_then(|a| a.paths.get(&state.path))
                                .is_some_and(|was| match was {
                                    PathState::File { sha256, .. } => {
                                        now.as_deref() == Some(sha256.as_str())
                                    }
                                    _ => false,
                                })
                        };
                        if now != left && !same_bytes() {
                            let text = format!(
                                "{}: {} changed after the hop ran, so it was left alone (cig unburn {run_id} --force restores over it)",
                                entry.op.describe(),
                                state.path.display()
                            );
                            report.failed.push(text.clone());
                            report.actions.push(("left".to_string(), text));
                            continue;
                        }
                    }
                }
            }
            let mut kept: Option<PathBuf> = None;
            let mut taken_here: Vec<BeforeState> = Vec::new();
            for state in order {
                if kept.as_deref() == Some(state.path.as_path()) {
                    // The move back failed: the file at the destination is
                    // the only copy, so the "remove the destination" step
                    // that would follow is skipped.
                    let text = format!(
                        "{}: kept {} (the move back failed; it is the only copy)",
                        entry.op.describe(),
                        state.path.display()
                    );
                    report.failed.push(text.clone());
                    report.actions.push(("kept".to_string(), text));
                    continue;
                }
                match restore(state, hop) {
                    Ok((note, taken)) => {
                        let to_tin = match taken.len() {
                            0 => String::new(),
                            1 => format!(
                                "; its bytes went to the tin, cig unburn {run_id} --give-back puts them back"
                            ),
                            n => format!(
                                "; {n} files went to the tin, cig unburn {run_id} --give-back puts them back"
                            ),
                        };
                        for t in &taken {
                            report.took.push(t.path.display().to_string());
                        }
                        taken_here.extend(taken);
                        let text = format!(
                            "{} ({} {}{}{to_tin})",
                            entry.op.describe(),
                            match state.before {
                                Before::Absent => "removed",
                                Before::File { .. } => "file restored:",
                                Before::Dir { .. } => "directory restored:",
                                Before::Directory { .. } => "directory made again:",
                                Before::Symlink { .. } => "link restored:",
                                Before::Moved { .. } => "moved back:",
                            },
                            state.path.display(),
                            note.map(|n| format!(", {n}")).unwrap_or_default()
                        );
                        report.actions.push(("restored".to_string(), text));
                        report.restored.push(format!(
                            "{} ({})",
                            entry.op.describe(),
                            match state.before {
                                Before::Absent => "removed",
                                Before::File { .. } => "file restored",
                                Before::Dir { .. } => "directory restored",
                                Before::Directory { .. } => "directory made again",
                                Before::Symlink { .. } => "link restored",
                                Before::Moved { .. } => "moved back",
                            }
                        ))
                    }
                    Err(e) => {
                        if let Before::Moved { to } = &state.before {
                            kept = Some(to.clone());
                        }
                        let text =
                            format!("{}: {} ({e})", entry.op.describe(), state.path.display());
                        report.failed.push(text.clone());
                        report.actions.push(("failed".to_string(), text));
                    }
                }
            }
            if !taken_here.is_empty() {
                took.push((entry.seq, taken_here, Vec::new()));
            }
            if report.failed.len() == failed_before {
                done.push(entry.seq);
                // An older entry for the same path is checked against what
                // this one just put back, not against what the run left: two
                // hops that wrote one file roll back to the start (#125).
                for state in &entry.before {
                    latest_left.insert(state.path.clone(), file_stamp(&state.path));
                    match &state.before {
                        Before::File { sha256, .. } => {
                            latest_sha.insert(state.path.clone(), sha256.clone());
                        }
                        _ => {
                            latest_sha.remove(&state.path);
                        }
                    }
                }
            }
        }
        self.mark_restored(&done);
        self.mark_took(took);
        report
    }

    /// Stamp the entries a rollback put back whole, in memory and on disk.
    fn mark_restored(&mut self, seqs: &[u64]) {
        if seqs.is_empty() {
            return;
        }
        let mut markers = Vec::new();
        for e in self.entries.iter_mut() {
            if seqs.contains(&e.seq) && !e.restored {
                e.restored = true;
                markers.push(serde_json::json!({"seq": e.seq, "restored": true}));
            }
        }
        self.append_markers(markers);
    }

    /// Record what a pass moved into the tin, per entry, and what it left
    /// at the paths it gave back, in memory and on disk. The newest marker
    /// for an entry is its current list.
    fn mark_took(&mut self, took: Vec<(u64, Vec<BeforeState>, Vec<LeftState>)>) {
        if took.is_empty() {
            return;
        }
        let mut markers = Vec::new();
        for (seq, states, left) in took {
            let mut m = serde_json::json!({"seq": seq, "took": states});
            if !left.is_empty() {
                m["left"] = serde_json::to_value(&left).unwrap_or_default();
            }
            markers.push(m);
            if let Some(e) = self.entries.iter_mut().find(|e| e.seq == seq) {
                e.took = states;
                e.left_after = left;
            }
        }
        self.append_markers(markers);
    }

    /// Does any entry hold files a rollback displaced into the tin?
    pub fn has_taken(&self) -> bool {
        self.entries.iter().any(|e| !e.took.is_empty())
    }
}

/// What a pass left at one path it put back, as `cig unburn --give-back`
/// records it beside what it took.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LeftState {
    pub path: PathBuf,
    #[serde(flatten)]
    pub state: PathState,
}

/// One file `cig unburn <id> --give-back` would put back.
#[derive(Clone, Debug, Serialize)]
pub struct GiveBack {
    pub seq: u64,
    pub path: PathBuf,
    /// The bytes the tin holds for it.
    pub sha256: String,
    pub bytes: u64,
    /// What is at the path now.
    pub now: PathState,
    /// What the rollback left at the path, when the journal can tell.
    pub left: Option<PathState>,
    /// The path is no longer as the rollback left it: giving back would
    /// write over work done since.
    pub changed: bool,
}

/// What a rollback of `entry` left at `path`, as far as its before-states
/// say: the file's old bytes, nothing (the path had not existed, or lay
/// under a directory that had not), a link, a directory. None when the
/// journal cannot tell.
fn rollback_left(entry: &Entry, path: &Path) -> Option<PathState> {
    // A give-back since the rollback put the tin's bytes at the path, and
    // wrote down that it did.
    if let Some(l) = entry.left_after.iter().find(|l| l.path == *path) {
        return Some(l.state.clone());
    }
    if let Some(b) = entry.before.iter().find(|b| b.path == *path) {
        return PathState::from_before(&b.before);
    }
    if entry
        .before
        .iter()
        .any(|b| matches!(b.before, Before::Absent) && path.starts_with(&b.path) && b.path != *path)
    {
        return Some(PathState::Absent);
    }
    None
}

/// A regular file's (mtime, len) as hop watching records it, or None for
/// anything else at the path.
fn file_stamp(path: &Path) -> Option<(u64, u64)> {
    let m = fs::symlink_metadata(path).ok()?;
    if !m.is_file() {
        return None;
    }
    Some((mtime_ns(&m).unwrap_or(0), m.len()))
}

/// Clear `path` for a restore without destroying anything: a regular file
/// goes to the tin (recorded in `taken`), a directory is emptied the same
/// way and removed, a link is removed (its target is all it is).
fn displace(path: &Path, taken: &mut Vec<BeforeState>, seal: bool) -> io::Result<()> {
    let take_file = |p: &Path, taken: &mut Vec<BeforeState>| -> io::Result<()> {
        let st = super::tin::take_with(p, seal)?;
        taken.push(BeforeState {
            path: p.to_path_buf(),
            before: Before::File {
                snapshot: st.object,
                sha256: st.sha256,
                bytes: st.bytes,
                mode: st.mode,
                mtime_ns: st.mtime_ns,
                git: None,
            },
        });
        Ok(())
    };
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
        Ok(m) if m.file_type().is_symlink() => fs::remove_file(path),
        Ok(m) if m.is_dir() => {
            displace_tree(path, taken)?;
            fs::remove_dir(path)
        }
        Ok(m) if m.is_file() => take_file(path, taken),
        Ok(_) => fs::remove_file(path),
    }
}

/// Empty a directory the way `displace` empties a path: files go to the
/// tin, links and empty directories go, and a tree hop watching never looks
/// into (`.git`, `node_modules`, `target`: rebuilt, not restored) goes whole.
fn displace_tree(dir: &Path, taken: &mut Vec<BeforeState>) -> io::Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            // A directory the hop made unreadable is still the hop's to
            // lose: its mode is opened so what is inside can be taken.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
            }
            fs::read_dir(dir)?
        }
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let p = entry.path();
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            fs::remove_file(&p)?;
        } else if ft.is_dir() {
            let name = entry.file_name().to_string_lossy().to_string();
            if super::tin::SKIPPED_ALWAYS.contains(&name.as_str()) {
                super::overlay::remove_tree_force(&p)?;
            } else {
                displace_tree(&p, taken)?;
                fs::remove_dir(&p)?;
            }
        } else if ft.is_file() {
            let st = super::tin::take(&p)?;
            taken.push(BeforeState {
                path: p.clone(),
                before: Before::File {
                    snapshot: st.object,
                    sha256: st.sha256,
                    bytes: st.bytes,
                    mode: st.mode,
                    mtime_ns: st.mtime_ns,
                    git: None,
                },
            });
        } else {
            fs::remove_file(&p)?;
        }
    }
    Ok(())
}

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Follow a chain of symlinks without requiring the end to exist: where a
/// write through `path` would land. Stops at 40 hops or a loop.
pub(crate) fn resolve_symlink_chain(path: &Path) -> PathBuf {
    let mut current = path.to_path_buf();
    for _ in 0..40 {
        if !is_symlink(&current) {
            return current;
        }
        let Ok(target) = fs::read_link(&current) else {
            return current;
        };
        let next = if target.is_absolute() {
            target
        } else {
            current.parent().map(|d| d.join(&target)).unwrap_or(target)
        };
        if next == current {
            return current;
        }
        current = next;
    }
    current
}

// ----- after-state: what the run left, so unburn can tell what changed since -----

/// What a path looks like at one moment, in enough detail to notice a change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PathState {
    Absent,
    File {
        sha256: String,
        bytes: u64,
    },
    /// A directory, fingerprinted by its entries (names, kinds, sizes and
    /// mtimes) so that a file added or changed inside it since the run is
    /// seen before rollback removes the directory. Records written before
    /// 1.1.1 carry no fingerprint and compare as unknown.
    Dir {
        #[serde(default)]
        fingerprint: String,
        #[serde(default)]
        entries: u64,
    },
    Symlink {
        target: PathBuf,
    },
}

/// Directory entries a fingerprint covers before it gives up and records
/// only the count.
const FINGERPRINT_CAP: u64 = 200_000;

/// An entry's metadata read from the file itself, not from the directory
/// listing: on NTFS the listing's copy of a size or a time is refreshed only
/// when the file's last handle closes, so a fingerprint taken from it as a
/// run ends could differ from one taken a moment later with nothing changed,
/// and `cig unburn` refused the run (E704). On Unix both reads are `lstat`.
pub(crate) fn fresh_metadata(entry: &walkdir::DirEntry) -> std::io::Result<fs::Metadata> {
    if entry.path_is_symlink() {
        fs::symlink_metadata(entry.path())
    } else {
        fs::metadata(entry.path())
    }
}

fn dir_fingerprint(dir: &Path) -> (String, u64) {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let mut count = 0u64;
    for entry in walkdir::WalkDir::new(dir)
        .min_depth(1)
        .follow_links(false)
        .sort_by_file_name()
    {
        let Ok(entry) = entry else { continue };
        count += 1;
        if count > FINGERPRINT_CAP {
            return (String::new(), count);
        }
        let rel = entry.path().strip_prefix(dir).unwrap_or(entry.path());
        let kind = if entry.file_type().is_symlink() {
            "l"
        } else if entry.file_type().is_dir() {
            "d"
        } else {
            "f"
        };
        let (len, mtime) = fresh_metadata(&entry)
            .map(|m| {
                (
                    m.len(),
                    m.modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_nanos())
                        .unwrap_or(0),
                )
            })
            .unwrap_or((0, 0));
        hasher.update(rel.to_string_lossy().as_bytes());
        hasher.update([0]);
        hasher.update(kind.as_bytes());
        hasher.update(len.to_le_bytes());
        hasher.update(mtime.to_le_bytes());
        hasher.update([0]);
    }
    (hex::encode(hasher.finalize()), count)
}

impl PathState {
    /// The state a restore of this before-state leaves at its path, when
    /// that can be said without looking: the old bytes, nothing, a link, a
    /// directory (its fingerprint unknown).
    pub fn from_before(before: &Before) -> Option<Self> {
        match before {
            Before::Absent => Some(PathState::Absent),
            Before::File { sha256, bytes, .. } => Some(PathState::File {
                sha256: sha256.clone(),
                bytes: *bytes,
            }),
            Before::Symlink { target } => Some(PathState::Symlink {
                target: target.clone(),
            }),
            Before::Dir { .. } | Before::Directory { .. } => Some(PathState::Dir {
                fingerprint: String::new(),
                entries: 0,
            }),
            Before::Moved { .. } => None,
        }
    }

    pub fn observe(path: &Path) -> Self {
        match fs::symlink_metadata(path) {
            Err(_) => PathState::Absent,
            Ok(m) if m.file_type().is_symlink() => PathState::Symlink {
                target: fs::read_link(path).unwrap_or_default(),
            },
            Ok(m) if m.is_dir() => {
                let (fingerprint, entries) = dir_fingerprint(path);
                PathState::Dir {
                    fingerprint,
                    entries,
                }
            }
            Ok(m) => PathState::File {
                sha256: sha256_file(path).unwrap_or_default(),
                bytes: m.len(),
            },
        }
    }

    /// Same kind and same content, as far as each kind can be compared.
    /// Two directories compare by fingerprint; when one side has none (a
    /// record from before 1.1.1) nothing can be claimed and they count as
    /// unchanged; when both were too large to fingerprint, the counts decide.
    pub fn same_as(&self, other: &PathState) -> bool {
        match (self, other) {
            (
                PathState::Dir {
                    fingerprint: a,
                    entries: ea,
                },
                PathState::Dir {
                    fingerprint: b,
                    entries: eb,
                },
            ) => {
                if !a.is_empty() && !b.is_empty() {
                    a == b
                } else if *ea > 0 && *eb > 0 {
                    ea == eb
                } else {
                    true
                }
            }
            _ => self == other,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            PathState::Absent => "absent".to_string(),
            PathState::File { sha256, bytes } => {
                format!(
                    "a file, {bytes} bytes, sha256 {}",
                    &sha256[..sha256.len().min(12)]
                )
            }
            PathState::Dir {
                fingerprint,
                entries,
            } if fingerprint.is_empty() && *entries == 0 => "a directory".to_string(),
            PathState::Dir { entries, .. } => format!(
                "a directory with {entries} entr{}",
                if *entries == 1 { "y" } else { "ies" }
            ),
            PathState::Symlink { target } => format!("a symlink to {}", target.display()),
        }
    }
}

/// `after.json`: every path the run touched, as the run left it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AfterState {
    /// The record's format; 0 for one from before 1.2.0.
    #[serde(default)]
    pub format: u32,
    pub recorded: String,
    pub paths: std::collections::BTreeMap<PathBuf, PathState>,
}

/// A path that is no longer as the run left it.
#[derive(Clone, Debug, Serialize)]
pub struct Change {
    pub path: PathBuf,
    /// The first journal entry that touched the path.
    pub seq: u64,
    pub was: PathState,
    pub now: PathState,
}

/// What `changed_since` knows: nothing at all for a run that predates
/// after-state records, or the list of changes (possibly empty).
#[derive(Clone, Debug, Serialize)]
pub struct ChangedSince {
    pub known: bool,
    pub changes: Vec<Change>,
}

impl Journal {
    /// Every path a rollback would touch, with the entry that first names it.
    pub fn touched_paths(&self) -> Vec<(PathBuf, u64)> {
        self.touched_paths_of(false)
    }

    /// The paths a retry would touch: those of the entries an earlier pass
    /// did not put back. What it did put back may have been edited since,
    /// and that is nobody's business but the editor's.
    pub fn touched_paths_pending(&self) -> Vec<(PathBuf, u64)> {
        self.touched_paths_of(true)
    }

    fn touched_paths_of(&self, pending_only: bool) -> Vec<(PathBuf, u64)> {
        let mut seen: Vec<(PathBuf, u64)> = Vec::new();
        for e in &self.entries {
            if !e.reversible || e.undone || (pending_only && e.restored) {
                continue;
            }
            for b in &e.before {
                if !seen.iter().any(|(p, _)| p == &b.path) {
                    seen.push((b.path.clone(), e.seq));
                }
                if let Before::Moved { to } = &b.before {
                    if !seen.iter().any(|(p, _)| p == to) {
                        seen.push((to.clone(), e.seq));
                    }
                }
            }
        }
        seen
    }

    /// Record what the run left behind, once its effects are complete.
    pub fn write_after_state(&self) -> Result<(), io::Error> {
        let Some(dir) = &self.run_dir else {
            return Ok(());
        };
        let mut after = AfterState {
            format: JOURNAL_FORMAT,
            recorded: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            paths: Default::default(),
        };
        for (path, _) in self.touched_paths() {
            after.paths.insert(path.clone(), PathState::observe(&path));
        }
        let text = serde_json::to_string_pretty(&after)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        fs::write(dir.join("after.json"), text)?;
        Ok(())
    }

    pub fn after_state(&self) -> Option<AfterState> {
        let dir = self.run_dir.as_ref()?;
        let text = fs::read_to_string(dir.join("after.json")).ok()?;
        let mut after: AfterState = serde_json::from_str(&text).ok()?;
        let map = super::parcel::path_map_of(dir);
        if !map.is_empty() {
            after.paths = after
                .paths
                .into_iter()
                .map(|(p, s)| (super::parcel::remap(&p, &map), s))
                .collect();
        }
        Some(after)
    }

    /// Which touched paths are no longer as the run left them. Restoring
    /// over one of these would overwrite work done since.
    pub fn changed_since(&self) -> ChangedSince {
        self.changed_since_of(self.touched_paths())
    }

    /// `changed_since` over the paths a retry would touch alone.
    pub fn changed_since_pending(&self) -> ChangedSince {
        self.changed_since_of(self.touched_paths_pending())
    }

    fn changed_since_of(&self, paths: Vec<(PathBuf, u64)>) -> ChangedSince {
        let Some(after) = self.after_state() else {
            return ChangedSince {
                known: false,
                changes: Vec::new(),
            };
        };
        // A path an earlier pass put back holds the before-state of the
        // oldest entry it restored there (a rollback goes newest first), not
        // what the run left: a retry compares with that (#125).
        let mut restored: std::collections::BTreeMap<PathBuf, PathState> =
            std::collections::BTreeMap::new();
        for e in &self.entries {
            if !e.restored || e.undone {
                continue;
            }
            for b in &e.before {
                if let Some(st) = b.before.as_path_state() {
                    restored.entry(b.path.clone()).or_insert(st);
                }
            }
        }
        let mut changes = Vec::new();
        for (path, seq) in paths {
            let Some(was) = restored.get(&path).or_else(|| after.paths.get(&path)) else {
                continue;
            };
            let now = PathState::observe(&path);
            if !now.same_as(was) {
                changes.push(Change {
                    path,
                    seq,
                    was: was.clone(),
                    now,
                });
            }
        }
        ChangedSince {
            known: true,
            changes,
        }
    }
}

/// The topmost path component that does not exist yet (or `path` itself).
fn first_missing_ancestor(path: &Path) -> PathBuf {
    let mut candidate = path.to_path_buf();
    for ancestor in path.ancestors().skip(1) {
        if ancestor.as_os_str().is_empty() || fs::symlink_metadata(ancestor).is_ok() {
            break;
        }
        candidate = ancestor.to_path_buf();
    }
    candidate
}

/// Put one path back. `Ok((Some(note), taken))` says how, when it was not
/// the ordinary way (a file restored in place because another program held
/// it), and lists what went to the tin to make room: with `keep`, nothing
/// at the path is destroyed, a file there is moved into the tin first.
fn restore(
    state: &BeforeState,
    keep: bool,
) -> Result<(Option<&'static str>, Vec<BeforeState>), io::Error> {
    let path = &state.path;
    let mut taken = Vec::new();
    // What replaced a sealed file is kept sealed too when it is displaced.
    let sealed = match &state.before {
        Before::File { snapshot, .. } => super::seal::is_sealed_path(snapshot),
        _ => false,
    };
    match &state.before {
        Before::Absent => {
            if !keep {
                return remove_any(path).map(|()| (None, taken));
            }
            // What is at the path goes to the tin before it goes: the hop's
            // output, or a save made while the hop ran, either way recoverable.
            displace(path, &mut taken, sealed)?;
            if fs::symlink_metadata(path).is_ok() {
                // Windows: another program holds the file, so it could not be
                // removed; its bytes are kept all the same.
                return Err(io::Error::other(
                    "it is held open by another program and could not be removed (its bytes are in the tin); close that program, then `cig unburn <run>` finishes the job",
                ));
            }
            Ok((None, taken))
        }
        Before::Directory { mode, .. } => {
            match fs::symlink_metadata(path) {
                Ok(m) if m.is_dir() => {}
                Ok(_) => {
                    if keep {
                        displace(path, &mut taken, sealed)?;
                    } else {
                        remove_any(path)?;
                    }
                    fs::create_dir_all(path)?;
                }
                Err(_) => fs::create_dir_all(path)?,
            }
            #[cfg(unix)]
            if let Some(m) = mode {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(*m))?;
            }
            #[cfg(not(unix))]
            let _ = mode;
            Ok((None, taken))
        }
        Before::File {
            snapshot,
            sha256,
            mode,
            mtime_ns,
            git,
            ..
        } => {
            if git.is_none() && snapshot.as_os_str().is_empty() {
                return Err(io::Error::other(
                    "no snapshot was taken (in-memory journal)",
                ));
            }
            if git.is_none() && !snapshot.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("snapshot {} is missing", snapshot.display()),
                ));
            }
            let _ = mode;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            // The before-image comes beside the destination first, verified
            // as it is written (a tin object against its name, a recipe's
            // chunks against theirs, a git blob against our hash), so
            // nothing that fails the check ever reaches the path.
            let staged = staging_path(path);
            match git {
                Some(g) => {
                    // A copy in the tin (a parcel brought one) stands in
                    // when git no longer has the blob.
                    match super::tin::object_for(sha256) {
                        Some(object) if !g.repo.is_dir() => {
                            super::tin::materialize(&object, sha256, &staged)?
                        }
                        Some(object) => {
                            if let Err(e) =
                                super::git::materialize(&g.repo, &g.blob, sha256, &staged)
                            {
                                if e.kind() != io::ErrorKind::NotFound {
                                    return Err(e);
                                }
                                super::tin::materialize(&object, sha256, &staged)?;
                            }
                        }
                        None => super::git::materialize(&g.repo, &g.blob, sha256, &staged)?,
                    }
                }
                None => super::tin::materialize(snapshot, sha256, &staged)?,
            }
            if keep {
                // Whatever is at the path now (the hop's output, or a save
                // made while it ran) goes to the tin before the old bytes
                // come back, unless it already is the old bytes.
                let same = fs::symlink_metadata(path)
                    .map(|m| m.is_file())
                    .unwrap_or(false)
                    && !sha256.is_empty()
                    && sha256_file(path).ok().as_deref() == Some(sha256.as_str());
                if !same {
                    displace(path, &mut taken, sealed)?;
                }
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if let Ok(m) = fs::symlink_metadata(path) {
                    if m.is_file() && m.nlink() > 1 {
                        // Other names share this inode: put the bytes back
                        // in place so every link sees them, instead of
                        // swapping in a new file and leaving the other
                        // names with the run's content.
                        let copied = (|| -> io::Result<()> {
                            let mut src = fs::File::open(&staged)?;
                            let mut dst = fs::OpenOptions::new()
                                .write(true)
                                .truncate(true)
                                .open(path)?;
                            io::copy(&mut src, &mut dst)?;
                            dst.sync_all()?;
                            drop(dst);
                            fs::set_permissions(path, fs::metadata(&staged)?.permissions())
                        })();
                        let _ = fs::remove_file(&staged);
                        copied?;
                        #[cfg(unix)]
                        if let Some(m) = mode {
                            use std::os::unix::fs::PermissionsExt;
                            fs::set_permissions(path, fs::Permissions::from_mode(*m))?;
                        }
                        if let Some(ns) = mtime_ns {
                            set_mtime(path, *ns)?;
                        }
                        inject_wrong_mode(path);
                        return Ok((None, taken));
                    }
                }
            }
            // Then swap in, so a failure half-way leaves whatever is there
            // untouched.
            #[cfg(unix)]
            if let Some(m) = mode {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&staged, fs::Permissions::from_mode(*m))?;
            }
            if fs::symlink_metadata(path)
                .map(|m| m.is_dir())
                .unwrap_or(false)
            {
                remove_any(path)?;
            }
            let note = swap_in(&staged, path)?;
            if let Some(ns) = mtime_ns {
                set_mtime(path, *ns)?;
            }
            inject_wrong_mode(path);
            Ok((note, taken))
        }
        Before::Dir { snapshot } => {
            if snapshot.as_os_str().is_empty() {
                return Err(io::Error::other(
                    "no snapshot was taken (in-memory journal)",
                ));
            }
            if !snapshot.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("snapshot {} is missing", snapshot.display()),
                ));
            }
            let staged = staging_path(path);
            restore_tree(snapshot, &staged)?;
            remove_any(path)?;
            fs::rename(&staged, path).map(|()| (None, taken))
        }
        Before::Symlink { target } => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            if keep {
                displace(path, &mut taken, sealed)?;
            } else {
                remove_any(path)?;
            }
            make_symlink(target, path).map(|()| (None, taken))
        }
        Before::Moved { to } => {
            if fs::symlink_metadata(to).is_err() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("{} is no longer there to move back", to.display()),
                ));
            }
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            // A move between two spellings of one name on a case-insensitive
            // filesystem: the destination is the file, so there is nothing
            // at the source to clear away, and clearing it would delete the
            // only copy.
            if !(super::case_variants(path, to) && super::same_object(path, to)) {
                remove_any(path)?;
            }
            match fs::rename(to, path) {
                Ok(()) => Ok((None, taken)),
                // Across devices (the run moved it with a copy): copy it
                // back the same way, and only then let go of the copy.
                Err(e) if e.raw_os_error() == Some(18) => {
                    if to.is_dir() {
                        copy_tree(to, path)?;
                    } else {
                        copy_file_with_mode(to, path)?;
                    }
                    remove_any(to).map(|()| (None, taken))
                }
                Err(e) => Err(e),
            }
        }
    }
}

/// Swap a staged before-image over `path`: a rename, which is atomic and
/// leaves the old content untouched until the new is in place. Windows
/// refuses to replace a file another program holds open without delete
/// sharing (an editor, a scanner, an indexer): the swap is retried for a
/// moment, then the content is written back in place through a handle the
/// other program's sharing does allow, and the note says so. When even that
/// is refused, the error names the program's hold and what to do.
fn swap_in(staged: &Path, path: &Path) -> Result<Option<&'static str>, io::Error> {
    #[cfg(windows)]
    {
        // A read-only destination refuses the swap with "access is
        // denied", the code a held-open file gives too: the attribute is
        // lifted first, so the wait below is for a real hold. The staged
        // copy carries the mode the file gets back.
        if let Ok(m) = fs::metadata(path) {
            if m.permissions().readonly() {
                let mut p = m.permissions();
                #[allow(clippy::permissions_set_readonly_false)]
                p.set_readonly(false);
                let _ = fs::set_permissions(path, p);
            }
        }
    }
    match fs::rename(staged, path) {
        Ok(()) => Ok(None),
        #[cfg(windows)]
        Err(e) if matches!(e.raw_os_error(), Some(5) | Some(32) | Some(33)) => {
            for wait_ms in [50u64, 100, 200, 400, 800] {
                std::thread::sleep(std::time::Duration::from_millis(wait_ms));
                if fs::rename(staged, path).is_ok() {
                    return Ok(None);
                }
            }
            let in_place = (|| -> Result<(), io::Error> {
                let mut src = fs::File::open(staged)?;
                let mut dst = fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(path)?;
                io::copy(&mut src, &mut dst)?;
                dst.sync_all()?;
                drop(dst);
                let _ = fs::set_permissions(path, fs::metadata(staged)?.permissions());
                fs::remove_file(staged)
            })();
            match in_place {
                Ok(()) => Ok(Some("in place: another program held it open")),
                Err(inner) => {
                    let _ = fs::remove_file(staged);
                    Err(io::Error::new(
                        e.kind(),
                        format!(
                            "{} is held open by another program and could not be replaced or rewritten ({inner}); close that program, then `cig unburn <run>` finishes the job",
                            path.display()
                        ),
                    ))
                }
            }
        }
        Err(e) => Err(e),
    }
}

/// `fs::copy`, then the source's full mode applied again: the copy sets
/// the mode when it creates the file, and writing the bytes afterwards
/// clears a setuid or setgid bit.
fn copy_file_with_mode(from: &Path, to: &Path) -> Result<u64, io::Error> {
    let n = fs::copy(from, to)?;
    #[cfg(unix)]
    fs::set_permissions(to, fs::metadata(from)?.permissions())?;
    Ok(n)
}

/// A sibling path to stage a restore in before swapping it into place.
fn staging_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "root".to_string());
    let parent = path.parent().unwrap_or(Path::new("."));
    let mut n = 0u32;
    loop {
        let candidate = parent.join(format!(".{name}.cig-restore-{}-{n}", std::process::id()));
        if fs::symlink_metadata(&candidate).is_err() {
            return candidate;
        }
        n += 1;
    }
}

/// The test hook that proves the rollback tripwire on a real fault: with
/// `CIG_INTERNAL_FAULT=restore-mode` a restored file gets one mode bit
/// wrong, exactly the bug the tripwire exists to catch.
fn inject_wrong_mode(path: &Path) {
    #[cfg(unix)]
    {
        if crate::tripwire::injected("restore-mode") {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(m) = fs::metadata(path) {
                let wrong = (m.permissions().mode() ^ 0o100) & 0o7777;
                let _ = fs::set_permissions(path, fs::Permissions::from_mode(wrong));
            }
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// `remove_any`, for the box's apply.
pub(crate) fn remove_any_pub(path: &Path) -> Result<(), io::Error> {
    remove_any(path)
}

/// `make_symlink`, for the box's apply.
pub(crate) fn make_symlink_pub(target: &Path, at: &Path) -> Result<(), io::Error> {
    make_symlink(target, at)
}

/// Remove a file, a directory tree, or a link (never following the link).
/// A tree goes whatever the modes inside it: what a hop made unreadable
/// (overlayfs's work directory, a `chmod 000`) is still the hop's to lose.
fn remove_any(path: &Path) -> Result<(), io::Error> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => fs::remove_file(path),
        Ok(m) if m.is_dir() => super::overlay::remove_tree_force(path),
        Ok(_) => fs::remove_file(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn make_symlink(target: &Path, at: &Path) -> Result<(), io::Error> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, at)
    }
    #[cfg(windows)]
    {
        // A relative target is read from the link's own directory, not
        // from the process: `..\shared` beside `p\sub\ldir` is `p\shared`.
        let resolved = if target.is_relative() {
            at.parent()
                .map(|d| d.join(target))
                .unwrap_or_else(|| target.to_path_buf())
        } else {
            target.to_path_buf()
        };
        if resolved.is_dir() {
            std::os::windows::fs::symlink_dir(target, at)
        } else {
            std::os::windows::fs::symlink_file(target, at)
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, at);
        Err(io::Error::other(
            "symlinks are not supported on this platform",
        ))
    }
}

/// Copy a tree, preserving links as links. Directory permissions are applied
/// after the walk so a read-only directory still receives its children.
pub fn copy_tree(from: &Path, to: &Path) -> Result<(), io::Error> {
    copy_tree_inner(from, to, false, false)
}

/// `copy_tree` for a snapshot: every copied file is synced to disk and the
/// directories fsynced, so the journal entry that follows never points at
/// bytes that were still in the page cache when the power went. A secret
/// inside the tree is kept sealed, as `<name>.<mode>.cig-sealed`.
fn copy_tree_durable(from: &Path, to: &Path) -> Result<(), io::Error> {
    copy_tree_inner(from, to, true, false)
}

/// `copy_tree` from a snapshot: a sealed entry comes back open, under its
/// own name and mode, checked on the way.
fn restore_tree(snapshot: &Path, to: &Path) -> Result<(), io::Error> {
    copy_tree_inner(snapshot, to, false, true)
}

fn copy_tree_inner(
    from: &Path,
    to: &Path,
    durable: bool,
    restoring: bool,
) -> Result<(), io::Error> {
    fs::create_dir_all(to)?;
    let mut dir_modes: Vec<(PathBuf, fs::Permissions)> = Vec::new();
    for entry in walkdir::WalkDir::new(from)
        .min_depth(1)
        .follow_links(false)
        .sort_by_file_name()
    {
        let entry = entry.map_err(io::Error::other)?;
        let rel = entry
            .path()
            .strip_prefix(from)
            .map_err(|e| io::Error::other(e.to_string()))?;
        let dest = to.join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        if entry.file_type().is_symlink() {
            let target = fs::read_link(entry.path())?;
            make_symlink(&target, &dest)?;
        } else if entry.file_type().is_dir() {
            fs::create_dir_all(&dest)?;
            let meta = entry.metadata().map_err(io::Error::other)?;
            dir_modes.push((dest, meta.permissions()));
        } else if entry.file_type().is_file() {
            let name = entry.file_name().to_string_lossy().to_string();
            if restoring && name.ends_with(super::seal::SUFFIX) && starts_sealed(entry.path()) {
                // `<name>.<mode>.cig-sealed`: back under its name and mode.
                let stem = &name[..name.len() - super::seal::SUFFIX.len()];
                let (base, mode) = match stem.rsplit_once('.') {
                    Some((b, m)) => (b.to_string(), u32::from_str_radix(m, 8).ok()),
                    None => (stem.to_string(), None),
                };
                let open = dest.with_file_name(&base);
                super::seal::unseal_file(
                    entry.path(),
                    &super::seal::aad_of(entry.path()),
                    "",
                    &open,
                )?;
                #[cfg(unix)]
                if let Some(m) = mode {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&open, fs::Permissions::from_mode(m))?;
                }
                #[cfg(not(unix))]
                let _ = mode;
            } else if durable && super::seal::should_seal_here(entry.path()) {
                let mode =
                    super::tin::mode_of(&entry.metadata().map_err(io::Error::other)?).unwrap_or(0);
                let sealed = dest.with_file_name(format!("{name}.{mode:o}{}", super::seal::SUFFIX));
                super::seal::seal_file(entry.path(), &sealed, &super::seal::aad_of(&sealed))?;
            } else {
                copy_file_with_mode(entry.path(), &dest)?;
                if durable {
                    sync_file(&dest)?;
                }
            }
        } else {
            return Err(io::Error::other(format!(
                "{} is not a regular file (a pipe, socket or device); the kernel cannot snapshot it",
                entry.path().display()
            )));
        }
    }
    for (dir, perms) in dir_modes.into_iter().rev() {
        // Each directory's entries are made durable before its mode is set,
        // which may take away the right to open it for syncing.
        if durable {
            sync_dir(&dir)?;
        }
        fs::set_permissions(&dir, perms)?;
    }
    // The root of the tree keeps its own mode too (a sticky or read-only
    // directory comes back as it was).
    fs::set_permissions(to, fs::metadata(from)?.permissions())?;
    if durable {
        sync_dir(to)?;
    }
    Ok(())
}

/// Read an imported run's before-state through its path map.
fn remap_state(st: &mut BeforeState, map: &[(PathBuf, PathBuf)]) {
    st.path = super::parcel::remap(&st.path, map);
    match &mut st.before {
        Before::File { snapshot, .. } | Before::Dir { snapshot } => {
            *snapshot = super::parcel::remap(snapshot, map);
        }
        Before::Moved { to } => *to = super::parcel::remap(to, map),
        Before::Absent | Before::Directory { .. } | Before::Symlink { .. } => {}
    }
}

/// Read an imported run's entry through its path map: the op's paths and
/// every before-state's.
fn remap_entry(e: &mut Entry, map: &[(PathBuf, PathBuf)]) {
    let r = |p: &mut PathBuf| *p = super::parcel::remap(p, map);
    match &mut e.op {
        Op::Write { path, .. }
        | Op::Append { path, .. }
        | Op::Delete { path }
        | Op::Mkdir { path }
        | Op::HopCreated { path, .. }
        | Op::HopChanged { path, .. } => r(path),
        Op::Move { from, to } | Op::Copy { from, to } => {
            r(from);
            r(to);
        }
        Op::Proc { cwd: Some(c), .. } => r(c),
        Op::Proc { .. } | Op::EnvSet { .. } | Op::EnvUnset { .. } | Op::Compensate { .. } => {}
    }
    for st in &mut e.before {
        remap_state(st, map);
    }
    if let Some(c) = &mut e.compensation {
        if let Some(d) = &mut c.cwd {
            r(d);
        }
    }
}

/// Does the file begin with a sealed object's magic?
fn starts_sealed(path: &Path) -> bool {
    use std::io::Read as _;
    let mut head = [0u8; 8];
    fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .map(|()| &head == super::seal::MAGIC)
        .unwrap_or(false)
}

/// A file's modification time as the journal records it: nanoseconds since
/// the epoch, or nothing when the platform does not say.
pub(crate) fn mtime_ns(meta: &fs::Metadata) -> Option<u64> {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
}

/// Put a modification time back on a restored file. Unix sets it through a
/// read-only handle; Windows needs write access to the attributes, so a
/// read-only file has its attribute lifted around the call.
pub(crate) fn set_mtime(path: &Path, ns: u64) -> Result<(), io::Error> {
    let when = std::time::UNIX_EPOCH + std::time::Duration::from_nanos(ns);
    #[cfg(not(windows))]
    {
        fs::File::open(path)?.set_modified(when)
    }
    #[cfg(windows)]
    {
        match fs::OpenOptions::new().write(true).open(path) {
            Ok(f) => f.set_modified(when),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                let perms = fs::metadata(path)?.permissions();
                let mut writable = perms.clone();
                #[allow(clippy::permissions_set_readonly_false)]
                writable.set_readonly(false);
                fs::set_permissions(path, writable)?;
                let set = fs::OpenOptions::new()
                    .write(true)
                    .open(path)
                    .and_then(|f| f.set_modified(when));
                fs::set_permissions(path, perms)?;
                set
            }
            Err(e) => Err(e),
        }
    }
}

/// Flush one file to the disk itself. On macOS `fsync` stops at the drive's
/// cache; `F_FULLFSYNC` asks the drive to write it out, and falls back to
/// `fsync` on a filesystem that does not take it. Everywhere else
/// `sync_all` is the platform's full flush.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn full_sync(f: &fs::File) -> Result<(), io::Error> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::io::AsRawFd;
        // SAFETY: fcntl on a file descriptor this handle owns.
        let rc = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_FULLFSYNC) };
        if rc == 0 {
            return Ok(());
        }
        // `sync_all` is F_FULLFSYNC again on Apple targets; what a
        // filesystem that refuses it (SMB, NFS, FUSE) needs is fsync(2).
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::ENOTSUP) | Some(libc::ENOTTY) | Some(libc::EINVAL) => {
                // SAFETY: fsync on a file descriptor this handle owns.
                if unsafe { libc::fsync(f.as_raw_fd()) } == 0 {
                    Ok(())
                } else {
                    Err(io::Error::last_os_error())
                }
            }
            _ => Err(err),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        f.sync_all()
    }
}

/// Flush a file's data for an append: `sync_data` everywhere `fsync` reaches
/// the disk, the full flush on macOS where it does not.
pub(crate) fn full_sync_data(f: &fs::File) -> Result<(), io::Error> {
    #[cfg(target_os = "macos")]
    {
        full_sync(f)
    }
    #[cfg(not(target_os = "macos"))]
    {
        f.sync_data()
    }
}

/// fsync a snapshot file so it is durable before the op it protects runs.
/// The copy carries the source's mode, so it may be read-only (a tin
/// object, a `.git/objects` entry): Unix flushes a read-only handle, while
/// Windows refuses to ("access is denied") and needs the read-only
/// attribute lifted around the flush.
pub(crate) fn sync_file(path: &Path) -> Result<(), io::Error> {
    #[cfg(not(windows))]
    {
        full_sync(&fs::File::open(path)?)
    }
    #[cfg(windows)]
    {
        match fs::OpenOptions::new().write(true).open(path) {
            Ok(f) => f.sync_all(),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                let perms = fs::metadata(path)?.permissions();
                let mut writable = perms.clone();
                #[allow(clippy::permissions_set_readonly_false)]
                writable.set_readonly(false);
                fs::set_permissions(path, writable)?;
                let synced = fs::OpenOptions::new()
                    .write(true)
                    .open(path)
                    .and_then(|f| f.sync_all());
                fs::set_permissions(path, perms)?;
                synced
            }
            Err(e) => Err(e),
        }
    }
}

/// `sync_dir` for the tin and the run records, which live outside this file.
pub(crate) fn sync_dir_pub(dir: &Path) -> Result<(), io::Error> {
    sync_dir(dir)
}

/// fsync a directory so newly created entries in it are durable. A no-op
/// where directories cannot be opened for syncing.
fn sync_dir(dir: &Path) -> Result<(), io::Error> {
    #[cfg(unix)]
    {
        match fs::File::open(dir) {
            Ok(f) => full_sync(&f),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
    #[cfg(windows)]
    {
        // A directory handle needs FILE_FLAG_BACKUP_SEMANTICS and write
        // access for FlushFileBuffers to flush its entries on NTFS. Where
        // the open is refused (a filesystem without it), the entry is left
        // to the filesystem's own journal, as it was before.
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        match fs::OpenOptions::new()
            .write(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(dir)
        {
            Ok(f) => match f.sync_all() {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied => Ok(()),
                Err(e) if e.raw_os_error() == Some(1) => Ok(()),
                Err(e) => Err(e),
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(_) => Ok(()),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = dir;
        Ok(())
    }
}

pub fn sha256_file(path: &Path) -> Result<String, io::Error> {
    use sha2::{Digest, Sha256};
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher)?;
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn write_delete_move_roll_back() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let run_dir = root.join("run");
        let a = root.join("a.txt");
        let b = root.join("b.txt");
        let c = root.join("c.txt");
        fs::write(&a, "original").unwrap();
        fs::write(&b, "to be deleted").unwrap();

        let mut j = Journal::open(Some(run_dir.clone())).unwrap();
        j.record(
            1,
            Op::Write {
                path: a.clone(),
                sensitive: false,
            },
        )
        .unwrap();
        fs::write(&a, "changed").unwrap();
        j.record(2, Op::Delete { path: b.clone() }).unwrap();
        fs::remove_file(&b).unwrap();
        j.record(
            3,
            Op::Move {
                from: a.clone(),
                to: c.clone(),
            },
        )
        .unwrap();
        fs::rename(&a, &c).unwrap();
        j.record(
            4,
            Op::Write {
                path: root.join("new.txt"),
                sensitive: false,
            },
        )
        .unwrap();
        fs::write(root.join("new.txt"), "brand new").unwrap();
        j.record(
            5,
            Op::Proc {
                cmd: "true".into(),
                args: vec![],
                cwd: None,
                touches: Vec::new(),
                touches_sealed: Vec::new(),
                skip: Vec::new(),
                boxed: false,
                box_terms: Vec::new(),
            },
        )
        .unwrap();

        assert!(!a.exists() && c.exists() && !b.exists());

        // Reload from disk to prove the journal round-trips.
        let mut loaded = Journal::load(&run_dir).unwrap();
        assert_eq!(loaded.len(), 5);
        assert!(loaded.verify().is_empty());
        let report = loaded.rollback();
        assert!(report.clean(), "{report:?}");
        assert_eq!(report.irreversible.len(), 1);
        assert_eq!(fs::read_to_string(&a).unwrap(), "original");
        assert_eq!(fs::read_to_string(&b).unwrap(), "to be deleted");
        assert!(!c.exists());
        assert!(!root.join("new.txt").exists());
    }

    #[test]
    fn directory_delete_rolls_back() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let d = root.join("dir");
        fs::create_dir_all(d.join("nested")).unwrap();
        fs::write(d.join("nested/f.txt"), "deep").unwrap();
        let mut j = Journal::open(Some(root.join("run"))).unwrap();
        j.record(1, Op::Delete { path: d.clone() }).unwrap();
        fs::remove_dir_all(&d).unwrap();
        let report = j.rollback();
        assert!(report.clean(), "{report:?}");
        assert_eq!(fs::read_to_string(d.join("nested/f.txt")).unwrap(), "deep");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_restore_is_a_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("target.txt"), "t").unwrap();
        let live = root.join("live.lnk");
        let dangling = root.join("dangling.lnk");
        std::os::unix::fs::symlink("target.txt", &live).unwrap();
        std::os::unix::fs::symlink("nowhere/at/all", &dangling).unwrap();
        let mut j = Journal::open(Some(root.join("run"))).unwrap();
        j.record(1, Op::Delete { path: live.clone() }).unwrap();
        fs::remove_file(&live).unwrap();
        j.record(
            2,
            Op::Delete {
                path: dangling.clone(),
            },
        )
        .unwrap();
        fs::remove_file(&dangling).unwrap();
        // Overwriting a link with a file, then rolling back, gives the link back.
        j.record(
            3,
            Op::Write {
                path: root.join("target.txt"),
                sensitive: false,
            },
        )
        .unwrap();
        fs::write(root.join("target.txt"), "changed").unwrap();
        assert!(j.verify().is_empty());
        let report = j.rollback();
        assert!(report.clean(), "{report:?}");
        assert!(fs::symlink_metadata(&live)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read_link(&live).unwrap(), PathBuf::from("target.txt"));
        assert!(fs::symlink_metadata(&dangling)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read_link(&dangling).unwrap(),
            PathBuf::from("nowhere/at/all")
        );
        assert_eq!(fs::read_to_string(root.join("target.txt")).unwrap(), "t");
    }

    #[test]
    fn changed_since_sees_edits_after_the_run() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let run_dir = root.join("run");
        fs::write(root.join("f.txt"), "one").unwrap();
        let mut j = Journal::open(Some(run_dir.clone())).unwrap();
        j.record(
            1,
            Op::Write {
                path: root.join("f.txt"),
                sensitive: false,
            },
        )
        .unwrap();
        fs::write(root.join("f.txt"), "two").unwrap();
        j.record(
            2,
            Op::Write {
                path: root.join("g.txt"),
                sensitive: false,
            },
        )
        .unwrap();
        fs::write(root.join("g.txt"), "new").unwrap();
        j.write_after_state().unwrap();
        let loaded = Journal::load(&run_dir).unwrap();
        let c = loaded.changed_since();
        assert!(c.known && c.changes.is_empty(), "{c:?}");
        fs::write(root.join("f.txt"), "three").unwrap();
        fs::remove_file(root.join("g.txt")).unwrap();
        let c = loaded.changed_since();
        assert_eq!(c.changes.len(), 2, "{c:?}");
        assert!(matches!(c.changes[1].now, PathState::Absent));
        // A run without after.json knows nothing, and says so.
        fs::remove_file(run_dir.join("after.json")).unwrap();
        assert!(!loaded.changed_since().known);
    }

    #[test]
    fn verify_refuses_before_a_partial_restore() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let run_dir = root.join("run");
        fs::write(root.join("m1.txt"), "old1").unwrap();
        fs::write(root.join("m2.txt"), "old2").unwrap();
        let mut j = Journal::open(Some(run_dir.clone())).unwrap();
        j.record(
            1,
            Op::Write {
                path: root.join("m1.txt"),
                sensitive: false,
            },
        )
        .unwrap();
        fs::write(root.join("m1.txt"), "one").unwrap();
        j.record(
            2,
            Op::Write {
                path: root.join("m2.txt"),
                sensitive: false,
            },
        )
        .unwrap();
        fs::write(root.join("m2.txt"), "two").unwrap();
        // Lose the second snapshot.
        for e in fs::read_dir(run_dir.join("snapshots")).unwrap().flatten() {
            if e.file_name().to_string_lossy().starts_with("00002") {
                fs::remove_file(e.path()).unwrap();
            }
        }
        let loaded = Journal::load(&run_dir).unwrap();
        let problems = loaded.verify();
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("snapshot missing for"), "{problems:?}");
        // Nothing was touched by verifying.
        assert_eq!(fs::read_to_string(root.join("m1.txt")).unwrap(), "one");
    }

    // ----- the property test: random ops over random trees ------------------

    /// A tiny deterministic PRNG so the test needs no dependency and every
    /// failure is reproducible from its seed.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
        fn chance(&mut self, one_in: usize) -> bool {
            self.below(one_in) == 0
        }
    }

    /// Byte-for-byte fingerprint of a tree: kind, mode, content or link target.
    fn fingerprint(root: &Path) -> BTreeMap<PathBuf, String> {
        let mut out = BTreeMap::new();
        for entry in walkdir::WalkDir::new(root)
            .min_depth(1)
            .follow_links(false)
            .sort_by_file_name()
        {
            let entry = entry.unwrap();
            let rel = entry.path().strip_prefix(root).unwrap().to_path_buf();
            let meta = fs::symlink_metadata(entry.path()).unwrap();
            let desc = if meta.file_type().is_symlink() {
                format!("link -> {}", fs::read_link(entry.path()).unwrap().display())
            } else if meta.is_dir() {
                "dir".to_string()
            } else {
                #[cfg(unix)]
                let mode = {
                    use std::os::unix::fs::PermissionsExt;
                    meta.permissions().mode() & 0o777
                };
                #[cfg(not(unix))]
                let mode = 0;
                format!("file mode={mode:o} {}", sha256_file(entry.path()).unwrap())
            };
            out.insert(rel, desc);
        }
        out
    }

    fn random_tree(rng: &mut Rng, root: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let dirs = ["", "a", "a/b", "c"];
        for d in dirs {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        for i in 0..(2 + rng.below(6)) {
            let d = dirs[rng.below(dirs.len())];
            let p = root.join(d).join(format!("f{i}.txt"));
            fs::write(&p, format!("content {i} {}", rng.next())).unwrap();
            #[cfg(unix)]
            if rng.chance(3) {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
            }
            files.push(p);
        }
        #[cfg(unix)]
        for i in 0..rng.below(3) {
            let l = root.join(format!("l{i}.lnk"));
            let target = match rng.below(3) {
                0 if !files.is_empty() => files[rng.below(files.len())]
                    .file_name()
                    .unwrap()
                    .to_os_string(),
                1 => format!("ghost{i}.txt").into(), // dangling, but a write lands
                _ => "missing/target".into(),        // dangling, nowhere to land
            };
            std::os::unix::fs::symlink(target, &l).unwrap();
            files.push(l);
        }
        files
    }

    /// Would `fs::write` succeed here? A regular file, an absent path whose
    /// parent exists, or a link whose (possibly absent) target has a parent.
    fn can_receive_bytes(path: &Path) -> bool {
        match fs::metadata(path) {
            Ok(m) => m.is_file(),
            Err(_) => {
                let landing = resolve_symlink_chain(path);
                landing.parent().map(|d| d.is_dir()).unwrap_or(false)
            }
        }
    }

    fn apply(op: &Op) -> io::Result<()> {
        match op {
            Op::Write { path, .. } => {
                if let Some(p) = path.parent() {
                    fs::create_dir_all(p)?;
                }
                fs::write(path, "written")
            }
            Op::Append { path, .. } => {
                let mut f = fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)?;
                f.write_all(b"+appended")
            }
            Op::Delete { path } => remove_any(path),
            Op::Move { from, to } => {
                if let Some(p) = to.parent() {
                    fs::create_dir_all(p)?;
                }
                remove_any(to)?;
                fs::rename(from, to)
            }
            Op::Copy { from, to } => {
                if let Some(p) = to.parent() {
                    fs::create_dir_all(p)?;
                }
                if fs::metadata(from)?.is_dir() {
                    copy_tree(from, to)
                } else {
                    fs::copy(from, to).map(|_| ())
                }
            }
            Op::Mkdir { path } => fs::create_dir_all(path),
            _ => Ok(()),
        }
    }

    #[test]
    fn rollback_property_over_random_trees() {
        for seed in 1..=60u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().join("tree");
            fs::create_dir_all(&root).unwrap();
            let mut paths = random_tree(&mut rng, &root);
            let before = fingerprint(&root);
            let run_dir = tmp.path().join("run");
            let mut j = Journal::open(Some(run_dir.clone())).unwrap();
            let steps = 1 + rng.below(8);
            for seq in 1..=steps as u64 {
                let fresh = root.join(format!("n{}.txt", rng.next() % 1000));
                let pick =
                    |rng: &mut Rng, paths: &Vec<PathBuf>| paths[rng.below(paths.len())].clone();
                let op = match rng.below(7) {
                    0 => Op::Write {
                        path: pick(&mut rng, &paths),
                        sensitive: false,
                    },
                    1 => Op::Write {
                        path: fresh.clone(),
                        sensitive: false,
                    },
                    2 => Op::Append {
                        path: pick(&mut rng, &paths),
                        sensitive: false,
                    },
                    3 => Op::Delete {
                        path: pick(&mut rng, &paths),
                    },
                    4 => Op::Move {
                        from: pick(&mut rng, &paths),
                        to: if rng.chance(2) {
                            fresh.clone()
                        } else {
                            pick(&mut rng, &paths)
                        },
                    },
                    5 => Op::Copy {
                        from: pick(&mut rng, &paths),
                        to: fresh.clone(),
                    },
                    _ => Op::Mkdir {
                        path: root.join(format!("d{}/deep", rng.next() % 100)),
                    },
                };
                // Ops the interpreter itself would refuse before journaling
                // (a missing source, a write that cannot land) are skipped.
                let source_ok = match &op {
                    Op::Delete { path } => fs::symlink_metadata(path).is_ok(),
                    Op::Write { path, .. } => can_receive_bytes(path),
                    Op::Append { path, .. } => {
                        fs::metadata(path).map(|m| m.is_file()).unwrap_or(false)
                    }
                    Op::Move { from, to } => fs::symlink_metadata(from).is_ok() && from != to,
                    Op::Copy { from, .. } => {
                        fs::metadata(from).map(|m| m.is_file()).unwrap_or(false)
                    }
                    _ => true,
                };
                if !source_ok {
                    continue;
                }
                j.record(seq, op.clone())
                    .unwrap_or_else(|e| panic!("seed {seed} seq {seq} record {op:?}: {e}"));
                apply(&op).unwrap_or_else(|e| panic!("seed {seed} seq {seq} apply {op:?}: {e}"));
                if let Op::Write { path, .. }
                | Op::Copy { to: path, .. }
                | Op::Move { to: path, .. } = &op
                {
                    if !paths.contains(path) {
                        paths.push(path.clone());
                    }
                }
            }
            let mut loaded = Journal::load(&run_dir).unwrap();
            assert!(
                loaded.verify().is_empty(),
                "seed {seed}: {:?}",
                loaded.verify()
            );
            let report = loaded.rollback();
            assert!(report.clean(), "seed {seed}: {report:?}");
            let after = fingerprint(&root);
            assert_eq!(before, after, "seed {seed}: tree differs after rollback");
        }
    }
}
