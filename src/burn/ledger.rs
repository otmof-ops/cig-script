// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The ledger: one line per run, appended when a run ends, when it is
//! unburned, when it is pruned or forgotten, and when a parcel brings one
//! in. Each line carries a MAC under the `ledger-mac` subkey over the line
//! before it and its own text, so a line edited, removed or reordered no
//! longer verifies, and a journal cut short or swapped for an older copy
//! is caught against the head the ledger recorded for it. It lives in the
//! keys directory, apart from the tin, owner-only, append-only. Each line
//! names the tin it witnesses (a hash of the state directory's path), so
//! two state directories beside one key keep separate accounts.

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

type HmacSha256 = Hmac<Sha256>;

pub const FILE: &str = "ledger.jsonl";
/// What the first line's MAC chains from.
const SEED: &str = "cig-ledger";

/// Where the ledger lives: beside the key, outside the tin.
pub fn path() -> PathBuf {
    super::seal::keys_dir().join(FILE)
}

/// The shape of the ledger file on disk, for callers that want to tell
/// a missing file from one with no lines (#177): `cig runs --verify`
/// and `cig doctor` both report both conditions, and `cig doctor`
/// names a missing ledger as a problem when there are sealed runs.
pub enum Presence {
    /// The file exists and has bytes.
    Present,
    /// The file exists but is zero-length.
    Empty,
    /// There is no file (a fresh tin, or the file was removed).
    Missing,
}

/// Does the ledger file exist on disk, and does it carry any bytes?
/// A missing or empty ledger is what #177 catches: the whole-history
/// verify cannot judge heads without a ledger.
pub fn presence() -> Presence {
    match fs::metadata(path()) {
        Ok(m) if m.len() == 0 => Presence::Empty,
        Ok(_) => Presence::Present,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Presence::Missing,
        // An unreadable file is as good as missing for the caller's
        // purposes (and `load()` will surface the real error).
        Err(_) => Presence::Missing,
    }
}

/// One entry. `kind` is `run` (a run ended), `unburn` (a rollback ended),
/// `pruned`, `forgotten` (the user said to stop noting a missing run) or
/// `origin` (a run a parcel brought in).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Line {
    pub seq: u64,
    pub kind: String,
    pub run: String,
    pub head: String,
    pub status: String,
    pub lines: u64,
    /// The tin this line witnesses: `tin_id()` of the state directory
    /// whose run it records.
    #[serde(default)]
    pub tin: String,
    pub t: String,
    #[serde(default)]
    pub mac: String,
}

/// What names a tin in the ledger: the first sixteen hex digits of the
/// SHA-256 of its state directory's canonical path. Derived from where the
/// tin is, not from anything kept inside it, so nothing written into the
/// tin can detach it from its own account.
pub fn tin_id() -> String {
    use sha2::Digest;
    let home = super::runs::home_dir();
    let home = fs::canonicalize(&home)
        .or_else(|_| std::path::absolute(&home))
        .unwrap_or(home);
    let digest = Sha256::digest(home.to_string_lossy().as_bytes());
    hex::encode(&digest[..8])
}

/// The ledger as read: every line, and where its chain first failed.
#[derive(Clone, Debug, Default)]
pub struct Loaded {
    pub lines: Vec<Line>,
    /// 1-based line number of the first line whose MAC does not verify.
    pub broken_at: Option<usize>,
    /// The MAC of the last line (the seed when there is none).
    pub head: String,
    /// The ledger-mac subkey whose chain verified the ledger on disk.
    /// When no candidate verified cleanly, it is the first candidate
    /// tried (the primary derived from the current master), so an
    /// append can still keep going — but a broken chain would already
    /// stop a rekey (H4) and `append` is only reached when the chain
    /// was clean to begin with. Never serialised; carried so a later
    /// `append` signs under the same key the chain is written in,
    /// never one half-way through a key rotation (SEC-004 M3).
    pub verified_key: Option<super::seal::Key>,
}

impl Loaded {
    /// The lines that witness this tin. A line with no tin named is from
    /// a ledger written before lines were stamped, and counts as this
    /// tin's.
    pub fn of_this_tin(&self) -> impl DoubleEndedIterator<Item = &Line> {
        let me = tin_id();
        self.lines
            .iter()
            .filter(move |l| l.tin.is_empty() || l.tin == me)
    }
}

/// Every candidate ledger-MAC subkey: the current one, plus one derived
/// from each stash key a rekey left on disk. A rekey that has re-signed
/// the ledger under the NEW master but not yet swapped the keeper lands
/// the ledger under NEW while `seal::key()` still gives OLD: the stash's
/// derivation covers that gap (and the one on the opposite side, where
/// the keeper swapped but the resign is still to run). The stashes are
/// honoured only when they prove the primary (SEC-004 H2).
fn candidate_keys() -> Vec<super::seal::Key> {
    let mut keys = Vec::new();
    let Ok((master, _)) = super::seal::key() else {
        return keys;
    };
    keys.push(super::seal::derive(&master, super::seal::LABEL_LEDGER_MAC));
    for sk in super::seal::secondary_keys(&master) {
        keys.push(super::seal::derive(&sk, super::seal::LABEL_LEDGER_MAC));
    }
    keys
}

fn mac_hex(key: &super::seal::Key, prev: &str, body: &[u8]) -> String {
    let mut m = HmacSha256::new_from_slice(key.as_bytes()).expect("HMAC takes any key length");
    m.update(prev.as_bytes());
    m.update(body);
    hex::encode(m.finalize().into_bytes())
}

/// The bytes a line's MAC covers: the line as it was before its own `mac`
/// field, which is always written last; the closing brace is put back, so
/// this is exactly the body that was MACed.
fn body_of(line: &str) -> String {
    match line.rfind(",\"mac\":") {
        Some(i) => format!("{}}}", &line[..i]),
        None => line.to_string(),
    }
}

/// Read and verify the ledger. A missing file is an empty ledger. The key
/// unavailable is an error: nothing can be verified without it. During a
/// rekey, the ledger may be MAC-ed under either the old or the new
/// subkey (whichever side of the keeper swap the rekey has reached): the
/// chain is checked against every candidate, and the first one with no
/// break wins. If every candidate breaks, the one that breaks latest is
/// kept, so the operator sees as much as can be read.
pub fn load() -> Result<Loaded, String> {
    // Take the process's shared rekey lock BEFORE reading the ledger
    // text (SEC-004 N2): a `cig tin rekey` that holds the exclusive
    // lock cannot then rewrite the file out from under us. Without
    // this order, an append that enters mid-rekey reads the pre-rekey
    // text, the key read behind `candidate_keys()` then blocks on the
    // same lock, and when the rekey finishes the appender signs a
    // line onto a stale head that breaks the chain for good. The
    // lock is held for the lifetime of the process (OnceLock inside
    // `seal::hold_shared_rekey_lock`), so a verify-only call pays the
    // same cost once and the `key()` later calls skip it.
    super::seal::hold_shared_rekey_lock()?;
    // StrictModes: the ledger file must be ours, a regular file, not
    // group- or world-readable or writable (#176). A missing file is
    // the fresh-ledger path below.
    super::seal::check_private_state_file(&path())?;
    let text = match fs::read_to_string(path()) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // A fresh (empty) ledger: the first append fills in its own
            // subkey from the primary. Do NOT ask for the key here —
            // `--verify` on a run that burned nothing must not create
            // one as a side effect (#129).
            return Ok(Loaded {
                head: SEED.to_string(),
                ..Default::default()
            });
        }
        Err(e) => return Err(format!("cannot read the ledger: {e}")),
    };
    let keys = candidate_keys();
    if keys.is_empty() {
        return Err(
            "the tin's key is not available; the ledger cannot be verified without it".to_string(),
        );
    }
    let mut best: Option<Loaded> = None;
    for k in &keys {
        let mut out = Loaded {
            head: SEED.to_string(),
            ..Default::default()
        };
        let mut prev = SEED.to_string();
        for (i, raw) in text.lines().filter(|l| !l.trim().is_empty()).enumerate() {
            let line: Line = match serde_json::from_str(raw) {
                Ok(l) => l,
                Err(_) => {
                    if out.broken_at.is_none() {
                        out.broken_at = Some(i + 1);
                    }
                    continue;
                }
            };
            let want = mac_hex(k, &prev, body_of(raw).as_bytes());
            if line.mac != want && out.broken_at.is_none() {
                out.broken_at = Some(i + 1);
            }
            prev = line.mac.clone();
            out.lines.push(line);
        }
        out.head = prev;
        if out.broken_at.is_none() {
            // This candidate verified: carry the key, so a later `append`
            // signs under the same subkey the chain is written in
            // (SEC-004 M3).
            out.verified_key = Some(k.clone());
            return Ok(out);
        }
        // Keep the candidate whose chain went furthest (the latest
        // `broken_at`), so an operator looking at a mid-rekey state sees
        // the ledger as read as it can be rather than the primary key's
        // break at line 1.
        match &best {
            Some(b) => {
                if out.broken_at > b.broken_at {
                    best = Some(out);
                }
            }
            None => best = Some(out),
        }
    }
    let mut best = best.expect("keys non-empty, so a Loaded was built");
    // A broken-chain ledger: `append` won't run (a rekey would have
    // refused earlier). The field is still filled so a caller that
    // reads it without panicking has a value.
    best.verified_key = keys.first().cloned();
    Ok(best)
}

/// Rewrite every ledger line's MAC under a new MAC subkey, keeping the
/// chain's order. Each line body is kept as-is except for `head`, which is
/// updated on the last `run`/`unburn`/`origin` line for each run id listed
/// in `new_heads` (that run's journal was rechained to a new head under
/// the new journal-mac subkey). Earlier lines for the same run keep their
/// own historical head — a ledger shows the heads as they stood when each
/// line was written (finding 9). `cig tin rekey` is the caller: an
/// interruption leaves a well-formed chain on disk, and a second rekey is
/// idempotent (each `mac` is recomputed from its line's `prev` whatever
/// the starting state was).
pub fn resign_under(
    new_key: &super::seal::Key,
    new_heads: &std::collections::BTreeMap<String, String>,
) -> Result<(), String> {
    let p = path();
    let text = match fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("cannot read the ledger: {e}")),
    };
    // Which ledger line is the LAST `run`/`unburn`/`origin` record for
    // each run id. `head` is only rewritten on these; earlier lines
    // keep their own historical head unchanged.
    let raw_lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut last_head_rewrite: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    for (i, raw) in raw_lines.iter().enumerate() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
            continue;
        };
        let Some(run) = v.get("run").and_then(|r| r.as_str()) else {
            continue;
        };
        if !new_heads.contains_key(run) {
            continue;
        }
        let kind = v.get("kind").and_then(|k| k.as_str()).unwrap_or("");
        if !matches!(kind, "run" | "unburn" | "origin") {
            continue;
        }
        last_head_rewrite.insert(run.to_string(), i);
    }
    let mut prev = SEED.to_string();
    let mut out = String::new();
    for (i, raw) in raw_lines.iter().enumerate() {
        let mut v: serde_json::Value = match serde_json::from_str(raw) {
            Ok(v) => v,
            // A line that does not parse is kept as-is; the next load call
            // will mark it `broken_at`, as it would have before the rekey.
            Err(_) => {
                out.push_str(raw);
                out.push('\n');
                continue;
            }
        };
        // Rewrite `head` only on the LAST record line for the run (finding 9).
        if let Some(run) = v.get("run").and_then(|r| r.as_str()).map(str::to_string) {
            if let (Some(new_head), Some(&target_i)) =
                (new_heads.get(&run), last_head_rewrite.get(&run))
            {
                if target_i == i {
                    if let Some(obj) = v.as_object_mut() {
                        obj.insert(
                            "head".to_string(),
                            serde_json::Value::String(new_head.clone()),
                        );
                    }
                }
            }
        }
        // Strip the old MAC, so the body MAC-ed below is exactly what
        // `body_of` finds again.
        if let Some(obj) = v.as_object_mut() {
            obj.shift_remove("mac");
        }
        let body = serde_json::to_string(&v).map_err(|e| e.to_string())?;
        let mac = mac_hex(new_key, &prev, body.as_bytes());
        if let Some(obj) = v.as_object_mut() {
            obj.insert("mac".to_string(), serde_json::Value::String(mac.clone()));
        }
        let written = serde_json::to_string(&v).map_err(|e| e.to_string())?;
        debug_assert_eq!(body_of(&written), body);
        out.push_str(&written);
        out.push('\n');
        prev = mac;
    }
    let tmp = p.with_extension("rekey.tmp");
    let _ = fs::remove_file(&tmp);
    {
        let mut opts = fs::OpenOptions::new();
        opts.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts
            .open(&tmp)
            .map_err(|e| format!("cannot write the new ledger: {e}"))?;
        f.write_all(out.as_bytes())
            .and_then(|()| f.sync_all())
            .map_err(|e| e.to_string())?;
    }
    fs::rename(&tmp, &p).map_err(|e| format!("cannot replace the ledger: {e}"))?;
    Ok(())
}

/// Append one line, MACed against the current head, and make it durable.
///
/// The process-level shared rekey lock is taken the first time this
/// process asks for the key (`seal::key()`), so a `cig tin rekey`
/// holding the exclusive lock cannot sign anything out from under a
/// run that lives (SEC-004 H1).
/// What an append needs from the ledger, read from its last two lines
/// alone: the last line's MAC (the head the next line chains on), its
/// `seq`, and the subkey the last line verifies under. The whole chain is
/// verified where the ledger is read for a verdict (`load`: `cig runs
/// --verify`, `cig tin verify`, a rekey); an append does not pay for it,
/// so a `cig run` costs the same at ten thousand runs as at ten (#178).
struct Tail {
    head: String,
    seq: u64,
    key: Option<super::seal::Key>,
}

/// What an append says when the ledger's last line does not verify (E764).
fn broken_tail_message(at: u64) -> String {
    format!(
        "error[E764 burn]: the ledger's chain is broken at line {at}, so nothing is appended to it (cig runs --verify says more; cig explain E764)"
    )
}

/// The tail an append chains on, or `None` when the tail is not one an
/// append can read on its own (a torn last line, a line that does not
/// parse, a last line longer than the window): the caller loads the whole
/// ledger then, as before.
fn tail_for_append() -> Result<Option<Tail>, String> {
    use std::io::{Read, Seek, SeekFrom};
    // The same order as `load` (SEC-004 N2): the shared rekey lock first,
    // then the file's own check (#176): ours, a regular file, no link.
    super::seal::hold_shared_rekey_lock()?;
    super::seal::check_private_state_file(&path())?;
    let empty = || Tail {
        head: SEED.to_string(),
        seq: 0,
        key: None,
    };
    let mut f = match fs::File::open(path()) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Some(empty())),
        Err(e) => return Err(format!("cannot read the ledger: {e}")),
    };
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    // Two ledger lines are a few hundred bytes; 64 KiB is plenty.
    let start = len.saturating_sub(64 * 1024);
    f.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    if start > 0 {
        // The window began mid-line: start at the first whole one.
        match buf.iter().position(|&b| b == b'\n') {
            Some(i) => {
                buf.drain(..=i);
            }
            None => return Ok(None),
        }
    }
    let Ok(text) = std::str::from_utf8(&buf) else {
        return Ok(None);
    };
    if !text.is_empty() && !text.ends_with('\n') {
        // A torn last line (a crashed append): the whole load decides.
        return Ok(None);
    }
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let Some(last_raw) = lines.last().copied() else {
        return Ok(if start == 0 { Some(empty()) } else { None });
    };
    let prev = if lines.len() >= 2 {
        match serde_json::from_str::<Line>(lines[lines.len() - 2]) {
            Ok(l) => l.mac,
            Err(_) => return Ok(None),
        }
    } else if start == 0 {
        SEED.to_string()
    } else {
        return Ok(None);
    };
    let Ok(last) = serde_json::from_str::<Line>(last_raw) else {
        return Ok(None);
    };
    let body = body_of(last_raw);
    let key = candidate_keys()
        .into_iter()
        .find(|k| mac_hex(k, &prev, body.as_bytes()) == last.mac);
    if key.is_none() {
        // No key verifies the last line: an append would chain a good
        // line onto a bad one and make the break look as if it had been
        // there all along. Refuse; `cig runs --verify` names the line.
        return Err(broken_tail_message(last.seq));
    }
    Ok(Some(Tail {
        head: last.mac,
        seq: last.seq,
        key,
    }))
}

/// Held while lines are appended: one writer at a time, so two processes
/// never chain a line onto the same head. Before this lock two runs that
/// appended at once forked the chain, and `cig runs --verify` called the
/// ledger broken from there (cig-pass review, C2).
pub struct Guard {
    _file: fs::File,
}

/// Take the ledger's lock (blocking): `<keys>/ledger.lock`, ours and 0600.
pub fn lock() -> Result<Guard, String> {
    super::seal::hold_shared_rekey_lock()?;
    let dir = super::seal::ensure_keys_dir()?;
    let p = dir.join("ledger.lock");
    // On Unix the lock file is made private, and never through a link,
    // before it is locked. Not on Windows: there the lock is an exclusive
    // open, so a plain open while another process holds it fails with a
    // sharing violation instead of waiting; `exclusive_lock` makes the file
    // itself and waits its turn.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&p)
            .map_err(|e| format!("cannot make the ledger's lock {}: {e}", p.display()))?;
        super::seal::check_private_state_file(&p)?;
    }
    let f = super::runs_index::exclusive_lock(&p)
        .map_err(|e| format!("cannot lock the ledger: {e}"))?;
    Ok(Guard { _file: f })
}

pub fn append(kind: &str, run: &str, head: &str, status: &str, lines: u64) -> Result<(), String> {
    let g = lock()?;
    append_locked(&g, kind, run, head, status, lines)
}

/// `append`, for a caller that holds the lock (a spend: check, then
/// append, as one step).
pub fn append_locked(
    _held: &Guard,
    kind: &str,
    run: &str,
    head: &str,
    status: &str,
    lines: u64,
) -> Result<(), String> {
    let tail = match tail_for_append()? {
        Some(t) => t,
        None => {
            let loaded = load()?;
            if let Some(at) = loaded.broken_at {
                return Err(broken_tail_message(at as u64));
            }
            Tail {
                head: loaded.head.clone(),
                seq: loaded.lines.last().map(|l| l.seq).unwrap_or(0),
                key: loaded.verified_key.clone(),
            }
        }
    };
    // Sign with the subkey the chain on disk is written in (SEC-004 M3):
    // between a rekey's ledger re-sign and its keeper swap, the keeper's
    // subkey differs from the one the last line verifies under, and
    // MAC-ing under the keeper's would break the next line's chain. For a
    // fresh ledger (no file yet), or a last line no candidate verifies,
    // the primary subkey is the one to use, as before.
    let key = match tail.key {
        Some(k) => k,
        None => super::seal::key()
            .map(|(m, _)| super::seal::derive(&m, super::seal::LABEL_LEDGER_MAC))?,
    };
    let seq = tail.seq + 1;
    let t = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    // The body, with `mac` written last so `body_of` finds it again.
    let mut v = serde_json::json!({
        "seq": seq, "kind": kind, "run": run, "head": head, "status": status, "lines": lines,
        "tin": tin_id(), "t": t,
    });
    let body = serde_json::to_string(&v).map_err(|e| e.to_string())?;
    let mac = mac_hex(&key, &tail.head, body.as_bytes());
    v["mac"] = serde_json::Value::String(mac);
    let text = serde_json::to_string(&v).map_err(|e| e.to_string())?;
    debug_assert_eq!(body_of(&text), body, "the MAC must cover exactly the body");
    let p = path();
    // Make or verify the keys folder private (#176). The folder exists
    // already once `load()` has run above, but keep this guard here too
    // so a later refactor cannot sneak in a wider mode.
    let _ = super::seal::ensure_keys_dir()?;
    super::seal::check_private_state_file(&p)?;
    let mut opts = fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(&p)
        .map_err(|e| format!("cannot open the ledger: {e}"))?;
    f.write_all(text.as_bytes())
        .and_then(|()| f.write_all(b"\n"))
        .and_then(|()| f.sync_all())
        .map_err(|e| e.to_string())
}

/// The last `run` or `unburn` line for a run: what the ledger says its
/// journal ended at.
pub fn last_record_of<'a>(loaded: &'a Loaded, run: &str) -> Option<&'a Line> {
    loaded
        .lines
        .iter()
        .rev()
        .find(|l| l.run == run && (l.kind == "run" || l.kind == "unburn" || l.kind == "origin"))
}

/// Has the ledger been told this run is gone on purpose (pruned or
/// forgotten)?
pub fn is_let_go(loaded: &Loaded, run: &str) -> bool {
    loaded
        .lines
        .iter()
        .rev()
        .find(|l| l.run == run)
        .map(|l| l.kind == "pruned" || l.kind == "forgotten")
        .unwrap_or(false)
}

/// Where the ledger ended when something read it: the last line's `seq`,
/// the bytes up to the end of that line, and its MAC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pos {
    pub seq: u64,
    pub byte_len: u64,
    pub head: String,
}

impl Pos {
    /// An empty ledger.
    pub fn start() -> Pos {
        Pos {
            seq: 0,
            byte_len: 0,
            head: SEED.to_string(),
        }
    }
}

/// What the ledger holds after a position read earlier.
#[derive(Debug)]
pub enum Since {
    /// The lines after it (verified, chained from its head) and where the
    /// ledger ends now.
    Lines(Vec<Line>, Pos),
    /// The ledger is shorter than it was: cut, or an older copy.
    Shorter,
    /// The line at that position is not the one read there: replaced.
    Swapped,
}

/// The lines appended after `pos`, each verified to chain from the one
/// before it, starting from `pos.head`. Reads only what is new, plus the
/// line `pos` ends on. A torn last line (an append that crashed) is left
/// for the next read.
pub fn since(pos: &Pos) -> Result<Since, String> {
    use std::io::{Read, Seek, SeekFrom};
    super::seal::hold_shared_rekey_lock()?;
    let p = path();
    super::seal::check_private_state_file(&p)?;
    let mut f = match fs::File::open(&p) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(if pos.byte_len == 0 {
                Since::Lines(Vec::new(), pos.clone())
            } else {
                Since::Shorter
            })
        }
        Err(e) => return Err(format!("cannot read the ledger: {e}")),
    };
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    if len < pos.byte_len {
        return Ok(Since::Shorter);
    }
    if pos.byte_len > 0 {
        let start = pos.byte_len.saturating_sub(64 * 1024);
        f.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; (pos.byte_len - start) as usize];
        f.read_exact(&mut buf).map_err(|e| e.to_string())?;
        if buf.last() != Some(&b'\n') {
            return Ok(Since::Swapped);
        }
        let body = &buf[..buf.len() - 1];
        let line_start = body.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        if start > 0 && line_start == 0 {
            return Ok(Since::Swapped);
        }
        let last = std::str::from_utf8(&body[line_start..]).map_err(|e| e.to_string())?;
        match serde_json::from_str::<Line>(last) {
            Ok(l) if l.mac == pos.head && l.seq == pos.seq => {}
            _ => return Ok(Since::Swapped),
        }
    }
    f.seek(SeekFrom::Start(pos.byte_len))
        .map_err(|e| e.to_string())?;
    let mut rest = Vec::new();
    f.read_to_end(&mut rest).map_err(|e| e.to_string())?;
    let complete = rest.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let text = std::str::from_utf8(&rest[..complete]).map_err(|e| e.to_string())?;
    let keys = candidate_keys();
    let mut prev = pos.head.clone();
    let mut out = Vec::new();
    let mut seq = pos.seq;
    for raw in text.lines().filter(|l| !l.trim().is_empty()) {
        let line: Line = serde_json::from_str(raw)
            .map_err(|_| "the ledger has a line that does not read".to_string())?;
        let body = body_of(raw);
        if !keys
            .iter()
            .any(|k| mac_hex(k, &prev, body.as_bytes()) == line.mac)
        {
            return Err(format!(
                "the ledger's chain is broken at line {} (cig runs --verify says more)",
                line.seq
            ));
        }
        prev = line.mac.clone();
        seq = line.seq;
        out.push(line);
    }
    Ok(Since::Lines(
        out,
        Pos {
            seq,
            byte_len: pos.byte_len + complete as u64,
            head: prev,
        },
    ))
}
