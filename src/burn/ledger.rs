// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

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
pub fn append(kind: &str, run: &str, head: &str, status: &str, lines: u64) -> Result<(), String> {
    let loaded = load()?;
    // Sign with the subkey that verified the ledger on disk (SEC-004
    // M3): between a rekey's ledger re-sign and its keeper swap, the
    // keeper's subkey differs from the one that verified the chain,
    // and MAC-ing under the keeper's would break the next line's
    // chain. For a fresh ledger (no file yet, no chain to verify), the
    // primary subkey is the one to use.
    let key = match loaded.verified_key.clone() {
        Some(k) => k,
        None => super::seal::key()
            .map(|(m, _)| super::seal::derive(&m, super::seal::LABEL_LEDGER_MAC))?,
    };
    let seq = loaded.lines.last().map(|l| l.seq + 1).unwrap_or(1);
    let t = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    // The body, with `mac` written last so `body_of` finds it again.
    let mut v = serde_json::json!({
        "seq": seq, "kind": kind, "run": run, "head": head, "status": status, "lines": lines,
        "tin": tin_id(), "t": t,
    });
    let body = serde_json::to_string(&v).map_err(|e| e.to_string())?;
    let mac = mac_hex(&key, &loaded.head, body.as_bytes());
    v["mac"] = serde_json::Value::String(mac);
    let text = serde_json::to_string(&v).map_err(|e| e.to_string())?;
    debug_assert_eq!(body_of(&text), body, "the MAC must cover exactly the body");
    let p = path();
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
        }
    }
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
