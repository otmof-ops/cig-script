// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! cig's side of a pass (cig-script#186): the keys, derived from the tin's
//! key; the store and the pass state in the keys folder (which the box
//! covers with an empty tmpfs, so a boxed hop sees neither); the ledger
//! lines that spend, revoke and move epochs; and the context the verifier
//! (`cig_pass::verify`, the only code that decides "trusted") asks.

pub use cig_pass as core;

use crate::burn::{ledger, seal};
use cig_pass::marks::{LedgerPos, Loaded, MarkKind, Marks};
use cig_pass::store::{DirGuard, Meta, Store};
use cig_pass::{Action, Ctx, Draft, IssueError, Pass, Recorder, Verified, Why};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

/// The build this cig is: a release's tag build, or `dev`.
pub const BUILD: &str = match option_env!("CIG_BUILD_ID") {
    Some(b) => b,
    None => "dev",
};

// ----- keys ----------------------------------------------------------------------

/// The keys a pass needs, each its own subkey of the tin's key.
pub struct Keys {
    master: seal::Key,
    issuer: [u8; 16],
    hash: seal::Key,
    sign: seal::Key,
    marks: seal::Key,
}

/// The HKDF info for a kind's subkey at an epoch: binary and versioned, so
/// no label of another use can collide with it.
fn local_info(kind: &str, epoch: u64) -> Vec<u8> {
    let mut v = b"cigpass1/local\0".to_vec();
    v.extend_from_slice(kind.as_bytes());
    v.push(0);
    v.extend_from_slice(&epoch.to_le_bytes());
    v
}

impl Keys {
    /// From the tin's key (the keyring, the key file, or the descriptor).
    pub fn load() -> Result<Keys, String> {
        let (master, _) = seal::key()?;
        let issuer_sub = seal::derive(&master, b"pass-issuer");
        let mut h = blake3::Hasher::new();
        h.update(b"cigpass1-issuer");
        h.update(issuer_sub.as_bytes());
        let mut issuer = [0u8; 16];
        issuer.copy_from_slice(&h.finalize().as_bytes()[..16]);
        Ok(Keys {
            issuer,
            hash: seal::derive(&master, b"pass-hash"),
            sign: seal::derive(&master, b"pass-sign"),
            marks: seal::derive(&master, b"pass-marks"),
            master,
        })
    }

    /// The local subkey for a kind at an epoch.
    pub fn local(&self, kind: &str, epoch: u64) -> [u8; 32] {
        *seal::derive(&self.master, &local_info(kind, epoch)).as_bytes()
    }

    pub fn issuer(&self) -> [u8; 16] {
        self.issuer
    }

    /// The seed of this machine's signing key (a shareable pass's issuer).
    pub fn signing_seed(&self) -> [u8; 32] {
        *self.sign.as_bytes()
    }

    /// The key content digests in local passes are keyed under.
    pub fn hash_key(&self) -> [u8; 32] {
        *self.hash.as_bytes()
    }

    /// This machine's public key for shareable passes.
    pub fn public(&self) -> [u8; 32] {
        cig_pass::public_key(self.sign.as_bytes())
    }

    fn marks_mac(&self, b: &[u8]) -> [u8; 32] {
        *blake3::keyed_hash(self.marks.as_bytes(), b).as_bytes()
    }
}

// ----- the store and the state ------------------------------------------------------

/// `<keys>/passes`: private, ours, 0700, like the keys folder it is in.
pub fn dir() -> PathBuf {
    seal::keys_dir().join("passes")
}

struct Guard;

impl DirGuard for Guard {
    fn ensure_dir(&self, d: &Path) -> io::Result<()> {
        seal::ensure_keys_dir().map_err(io::Error::other)?;
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut b = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700);
        }
        match b.create(d) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        seal::check_keys_dir(d).map_err(io::Error::other)
    }
}

/// The pass store.
pub fn store() -> Result<Store, String> {
    Store::open(&dir(), &Guard).map_err(|e| format!("the pass store: {e}"))
}

/// The state cache, named by its key: after `cig tin rekey` the name is
/// another, so the old cache (sealed under the old key) is left behind and
/// the state is folded again from the ledger the rekey re-signed.
fn marks_path(keys: &Keys) -> PathBuf {
    let id = blake3::hash(keys.marks.as_bytes());
    dir().join(format!("marks-{}", cig_pass::hex(&id.as_bytes()[..8])))
}

/// Remove state caches left by an older key.
fn sweep_old_marks(current: &Path) {
    let Ok(rd) = std::fs::read_dir(dir()) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name();
        let name = name.to_string_lossy();
        if (name.starts_with("marks-") || name == "marks") && p != current {
            let _ = std::fs::remove_file(&p);
        }
    }
}

fn pos_of(lp: &LedgerPos) -> ledger::Pos {
    if lp.byte_len == 0 {
        ledger::Pos::start()
    } else {
        ledger::Pos {
            seq: lp.seq,
            byte_len: lp.byte_len,
            head: cig_pass::hex(&lp.head_mac),
        }
    }
}

fn lpos_of(p: &ledger::Pos) -> LedgerPos {
    LedgerPos {
        seq: p.seq,
        byte_len: p.byte_len,
        head_mac: if p.byte_len == 0 {
            [0; 32]
        } else {
            cig_pass::from_hex32(&p.head).unwrap_or([0; 32])
        },
    }
}

/// Why the pass state cannot be trusted (E739).
pub const STATE_E739: &str = "E739";

/// The pass state (epochs, spends, revocations), folded from the ledger:
/// read from its cache when the cache is current, brought up to date from
/// the lines the ledger gained since, rebuilt from the whole ledger when
/// the cache is missing. A ledger shorter than the cache expects, or one
/// whose line at that point is another, is refused: every pass is invalid
/// until `cig doctor` looks (E739).
pub fn state(keys: &Keys) -> Result<Marks, String> {
    let mac = |b: &[u8]| keys.marks_mac(b);
    let path = marks_path(keys);
    let loaded = Marks::load(&path, &Guard, &mac)
        .map_err(|e| format!("the pass state cannot be read: {e}"))?;
    let mut marks = match loaded {
        Loaded::Ok(m) => m,
        Loaded::Stale => Marks::empty(),
        Loaded::Tampered(why) => {
            return Err(format!(
                "{STATE_E739}: the pass state does not verify ({why}); every pass is refused until cig pass gc rebuilds it from the ledger"
            ))
        }
    };
    let from = pos_of(marks.ledger());
    match ledger::since(&from)? {
        ledger::Since::Lines(lines, to) => {
            if to != from {
                let lp = lpos_of(&to);
                for l in &lines {
                    if let Some((k, key)) = mark_of(l) {
                        marks.record(k, key, lp);
                    }
                }
                marks.advance(lp);
                if marks.save(&path, &Guard, &mac).is_ok() {
                    sweep_old_marks(&path);
                }
            }
            Ok(marks)
        }
        ledger::Since::Shorter => Err(format!(
            "{STATE_E739}: the ledger is shorter than the pass state expects (cut, or an older copy put back); every pass is refused (cig runs --verify says what is missing)"
        )),
        ledger::Since::Swapped => Err(format!(
            "{STATE_E739}: the ledger is not the one the pass state was read from; every pass is refused (cig runs --verify says what changed)"
        )),
    }
}

/// Fold the pass state again from the whole ledger, after verifying its
/// chain: what `cig pass gc` does for a cache that no longer verifies.
pub fn rebuild_state(keys: &Keys) -> Result<Marks, String> {
    let loaded = ledger::load()?;
    if let Some(at) = loaded.broken_at {
        return Err(format!(
            "{STATE_E739}: the ledger's chain is broken at line {at}; the pass state cannot be folded from it (cig runs --verify says more)"
        ));
    }
    let path = marks_path(keys);
    let _ = std::fs::remove_file(&path);
    sweep_old_marks(&path);
    state(keys)
}

fn mark_of(l: &ledger::Line) -> Option<(MarkKind, [u8; 32])> {
    match l.kind.as_str() {
        "pass-spend" => Some((MarkKind::Spend, cig_pass::from_hex32(&l.head)?)),
        "pass-revoke" => Some((MarkKind::Revoke, cig_pass::from_hex32(&l.head)?)),
        "pass-epoch" if cig_pass::is_kind(&l.status) => {
            Some((MarkKind::Epoch(l.status.clone()), [0; 32]))
        }
        _ => None,
    }
}

// ----- trust: allowed signers and authorities -----------------------------------------

/// Where this machine's allowed signers are: `cig config
/// pass_allowed_signers`, else `<cig home>/allowed_signers`.
pub fn allowed_signers_path() -> PathBuf {
    let c = crate::config::Config::load().get("pass_allowed_signers");
    if c.is_empty() {
        seal::keys_dir().join("allowed_signers")
    } else {
        PathBuf::from(c)
    }
}

/// The allowed signers in a file (none when it is not there). Whoever can
/// write this file decides which machines' passes hold here, so it is read
/// only when it is ours, a regular file (not a link) and closed to others'
/// writes; otherwise it trusts nobody, and says why on stderr.
pub fn read_signers(path: &Path) -> Vec<cig_pass::ssh::Signer> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let Ok(md) = std::fs::symlink_metadata(path) else {
            return Vec::new();
        };
        // SAFETY: geteuid has no preconditions.
        let me = unsafe { libc::geteuid() };
        if !md.file_type().is_file() || md.uid() != me || md.mode() & 0o022 != 0 {
            eprintln!(
                "note: {} is not a private file of yours (a link, another owner's, or writable by others), so it trusts no signer",
                path.display()
            );
            return Vec::new();
        }
    }
    std::fs::read_to_string(path)
        .map(|t| cig_pass::ssh::parse_allowed_signers(&t))
        .unwrap_or_default()
}

/// The human authorities this machine trusts for `human-yes`: Ed25519
/// keys in `/etc/cigscript/authority.pub`, read only when the file and its
/// folder belong to root and nobody else can write them. There is no
/// variable, flag or other file that names another: an authority is
/// installed by an administrator, or there is none.
pub fn authorities() -> Vec<[u8; 32]> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let dir = Path::new("/etc/cigscript");
        let file = dir.join("authority.pub");
        let root_only = |p: &Path| {
            std::fs::symlink_metadata(p)
                .map(|m| m.uid() == 0 && m.mode() & 0o022 == 0 && !m.file_type().is_symlink())
                .unwrap_or(false)
        };
        if !root_only(dir) || !root_only(&file) {
            return Vec::new();
        }
        std::fs::read_to_string(&file)
            .map(|t| {
                t.lines()
                    .filter_map(|l| {
                        let mut w = l.split_whitespace();
                        match (w.next(), w.next()) {
                            (Some("ssh-ed25519"), Some(b)) => cig_pass::ssh::from_openssh_b64(b),
                            _ => None,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    #[cfg(not(unix))]
    {
        Vec::new()
    }
}

// ----- the context ----------------------------------------------------------------------

/// What the verifier asks, answered for this machine and this run.
pub struct CigCtx<'a> {
    pub keys: &'a Keys,
    pub marks: &'a Marks,
    pub store: Option<&'a Store>,
    pub action: Action,
    pub tools: HashMap<String, (String, String)>,
    pub rules: HashMap<String, [u8; 32]>,
    pub signers: Vec<cig_pass::ssh::Signer>,
    pub authorities: Vec<[u8; 32]>,
    pub discharges: Vec<Pass>,
    /// The running script's globals, for passes that recorded one.
    pub globals: Option<crate::interp::env::Env>,
}

impl<'a> CigCtx<'a> {
    pub fn new(keys: &'a Keys, marks: &'a Marks, store: Option<&'a Store>) -> Self {
        CigCtx {
            keys,
            marks,
            store,
            action: Action::default(),
            tools: HashMap::new(),
            rules: HashMap::new(),
            signers: read_signers(&allowed_signers_path()),
            authorities: authorities(),
            discharges: Vec::new(),
            globals: None,
        }
    }
}

fn env_bytes(name: &[u8]) -> Option<Vec<u8>> {
    if name == b":cwd" {
        return std::env::current_dir()
            .ok()
            .and_then(|c| cig_pass::path_bytes(&c));
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        std::env::var_os(std::ffi::OsStr::from_bytes(name)).map(|v| v.as_bytes().to_vec())
    }
    #[cfg(not(unix))]
    {
        let n = std::str::from_utf8(name).ok()?;
        std::env::var_os(n).map(|v| v.to_string_lossy().into_owned().into_bytes())
    }
}

impl Ctx for CigCtx<'_> {
    fn local_key(&self, kind: &str, epoch: u64) -> Option<[u8; 32]> {
        Some(self.keys.local(kind, epoch))
    }

    fn hash_key(&self) -> Option<[u8; 32]> {
        Some(*self.keys.hash.as_bytes())
    }

    fn local_issuer(&self) -> Option<[u8; 16]> {
        Some(self.keys.issuer)
    }

    fn epoch(&self, kind: &str) -> u64 {
        self.marks.epoch(kind)
    }

    fn ledger_seq(&self) -> Option<u64> {
        Some(self.marks.ledger().seq)
    }

    fn allowed_signer(&self, pk: &[u8; 32], kind: &str) -> bool {
        self.signers
            .iter()
            .any(|s| s.key == *pk && s.kinds.iter().any(|k| k == kind))
    }

    fn authority_signer(&self, pk: &[u8; 32]) -> bool {
        self.authorities.contains(pk)
    }

    fn is_own_signer(&self, pk: &[u8; 32]) -> bool {
        *pk == self.keys.public()
    }

    fn env(&self, name: &[u8]) -> Option<Vec<u8>> {
        env_bytes(name)
    }

    fn tool(&self, name: &str) -> Option<(String, String)> {
        self.tools.get(name).cloned()
    }

    fn rules(&self, name: &str) -> Option<[u8; 32]> {
        if let Some(g) = name.strip_prefix("global:") {
            let v = self.globals.as_ref()?.get(g)?;
            return digest_value(&v);
        }
        self.rules.get(name).copied()
    }

    fn cig(&self) -> (String, String) {
        (crate::VERSION.to_string(), BUILD.to_string())
    }

    fn action(&self) -> &Action {
        &self.action
    }

    fn revoked(&self, id: &[u8; 32]) -> bool {
        self.marks.is_revoked(id)
    }

    fn spends(&self, key: &[u8; 32]) -> u32 {
        self.marks.spend_count(key)
    }

    fn find_pass(&self, id: &[u8; 32]) -> Option<Pass> {
        let bytes = self.store?.find_by_id(id).ok()??;
        Pass::decode(&bytes).ok()
    }

    fn discharges(&self, kind: &str, root: &[u8; 32]) -> Vec<Pass> {
        let mut out: Vec<Pass> = self
            .discharges
            .iter()
            .filter(|d| d.kind == kind)
            .cloned()
            .collect();
        if let Some(s) = self.store {
            if let Ok(c) = s.candidates(kind, &cig_pass::discharge_subject(root)) {
                out.extend(c.iter().filter_map(|b| Pass::decode(b).ok()));
            }
        }
        out
    }
}

// ----- issue, find, spend, revoke --------------------------------------------------------

/// What `pass.issue` adds to what the recorder saw.
#[derive(Clone, Debug, Default)]
pub struct IssueOpts {
    /// The pass holds until this time (unix ms): a `Clock` input.
    pub until: Option<i64>,
    /// Record cig itself as a `Tool` input (default true).
    pub cig: bool,
    /// Other tools the result depends on: name, version, build.
    pub tools: Vec<(String, String, String)>,
    /// Rule data the result depends on: name, digest.
    pub rules: Vec<(String, [u8; 32])>,
    /// Keep it in the store (false in a dry run).
    pub store: bool,
}

/// Kinds whose result is cig's own judgment: they always record cig.
const CIG_JUDGES: &[&str] = &["check", "trial", "verdict", "doctor"];

/// Add what the options declare to a recording: cig itself, other tools,
/// rule data, a time limit. Done before a nested recording is absorbed into
/// the one around it, so the outer pass carries the inner one's limits too.
pub fn prepare(rec: &mut Recorder, kind: &str, opts: &IssueOpts) {
    if opts.cig || CIG_JUDGES.contains(&kind) {
        rec.tool("cig", crate::VERSION, BUILD);
    }
    for (n, v, b) in &opts.tools {
        rec.tool(n, v, b);
    }
    for (n, d) in &opts.rules {
        rec.rules(n, *d);
    }
    if let Some(t) = opts.until {
        rec.clock(t);
    }
}

/// Issue a local pass from what the work recorded (after `prepare`), and
/// keep it.
pub fn issue(
    keys: &Keys,
    marks: &Marks,
    kind: &str,
    subject: &[u8],
    rec: Recorder,
    result: Vec<u8>,
    opts: &IssueOpts,
) -> Result<Pass, IssueError> {
    let flags = rec.flags();
    let inputs = rec.finish()?;
    let epoch = marks.epoch(kind);
    let draft = Draft {
        kind: kind.to_string(),
        subject: subject.to_vec(),
        inputs,
        result,
        epoch,
        ledger_seq: marks.ledger().seq,
        issued_ms: cig_pass::wall_ms(),
        mono: cig_pass::mono_now(),
        flags,
    };
    let mut pass = cig_pass::issue_local(
        &draft,
        keys.issuer,
        &keys.local(kind, epoch),
        keys.hash.as_bytes(),
    )?;
    if let Some(t) = opts.until {
        pass =
            cig_pass::attenuate(&pass, cig_pass::Caveat::Until(t)).map_err(IssueError::Invalid)?;
    }
    if opts.store {
        keep(&pass).map_err(|e| IssueError::Invalid(format!("{STORE_FAILED}{e}")))?;
    }
    Ok(pass)
}

/// What an issue error starts with when the pass was made and could not be
/// kept (the work's result is still good).
pub const STORE_FAILED: &str = "the pass could not be kept: ";

/// Keep a pass in the store.
pub fn keep(pass: &Pass) -> Result<(), String> {
    let s = store()?;
    let bytes = pass.encode();
    let meta = Meta {
        id: pass.id(),
        kind: pass.kind.clone(),
        subject: pass.subject,
        issued_ms: pass.issued_ms,
        size: bytes.len() as u32,
    };
    s.insert(&meta, &bytes)
        .map_err(|e| format!("the pass could not be kept: {e}"))?;
    if s.should_compact().unwrap_or(false) {
        let budget = budget_bytes();
        let _ = s.compact(budget, &|b| Pass::decode(b).is_ok());
    }
    Ok(())
}

/// `pass_budget` in bytes (default 64 MB).
pub fn budget_bytes() -> u64 {
    let v = crate::config::Config::load().get("pass_budget");
    parse_size(&v).unwrap_or(64 * 1024 * 1024)
}

pub fn parse_size(v: &str) -> Option<u64> {
    let v = v.trim();
    if v.is_empty() {
        return None;
    }
    let split = v.find(|c: char| !c.is_ascii_digit()).unwrap_or(v.len());
    let (n, unit) = v.split_at(split);
    let n: u64 = n.parse().ok()?;
    let mul = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" => 1 << 10,
        "m" | "mb" => 1 << 20,
        "g" | "gb" => 1 << 30,
        _ => return None,
    };
    n.checked_mul(mul)
}

/// The newest pass of a kind for a subject that verifies now, and what the
/// verifier said; `None` is a miss (the caller does the work and issues).
pub fn find(ctx: &CigCtx<'_>, kind: &str, subject: &[u8]) -> Option<(Pass, Verified)> {
    let s = ctx.store?;
    let digest = cig_pass::subject_digest(subject);
    let candidates = s.candidates(kind, &digest).ok()?;
    for b in candidates {
        let Ok(p) = Pass::decode(&b) else { continue };
        if let Ok(v) = cig_pass::verify(&p, ctx) {
            s.touch(&v.id);
            return Some((p, v));
        }
    }
    None
}

/// Spend a pass: verify it and count one use of each `once` or `max` it
/// carries (and of its discharges'). The full check runs first, without
/// the ledger's lock; then, under the lock, the state is read again and
/// what can have moved meanwhile (revocations, epochs, the counts) is
/// checked again before the spend lines are appended. Refused when any
/// count is used up.
pub fn spend(pass: &Pass, setup: impl FnOnce(&mut CigCtx<'_>)) -> Result<Verified, String> {
    let keys = Keys::load()?;
    let v = {
        let marks = state(&keys)?;
        let st = store().ok();
        let mut ctx = CigCtx::new(&keys, &marks, st.as_ref());
        setup(&mut ctx);
        cig_pass::verify(pass, &ctx).map_err(|w| w.to_string())?
    };
    let held = ledger::lock()?;
    let marks = state(&keys)?;
    for id in pass.prefix_ids() {
        if marks.is_revoked(&id) {
            return Err(Why::Revoked { prefix: 0 }.to_string());
        }
    }
    if pass.form == cig_pass::Form::Local && marks.epoch(&pass.kind) != pass.epoch {
        return Err(Why::Epoch {
            then: pass.epoch,
            now: marks.epoch(&pass.kind),
            kind: pass.kind.clone(),
        }
        .to_string());
    }
    for s in &v.spend {
        let used = marks.spend_count(&s.key);
        if used >= s.limit {
            return Err(Why::Spent {
                text: format!("used {used} of {}", s.limit),
                used,
            }
            .to_string());
        }
    }
    for (n, s) in v.spend.iter().enumerate() {
        if let Err(e) =
            ledger::append_locked(&held, "pass-spend", "", &cig_pass::hex(&s.key), "spent", 0)
        {
            return Err(format!(
                "{e} ({n} of its {} spend(s) were recorded before this)",
                v.spend.len()
            ));
        }
    }
    drop(held);
    let _ = state(&keys);
    Ok(v)
}

/// Revoke a pass by id (and every pass narrowed from it).
pub fn revoke_id(id: &[u8; 32]) -> Result<(), String> {
    let keys = Keys::load()?;
    let held = ledger::lock()?;
    ledger::append_locked(&held, "pass-revoke", "", &cig_pass::hex(id), "revoked", 0)?;
    drop(held);
    state(&keys).map(|_| ())
}

/// Move a kind to its next epoch: every pass of that kind is invalid.
pub fn revoke_kind(kind: &str) -> Result<u64, String> {
    if !cig_pass::is_kind(kind) {
        return Err(format!("`{kind}` is not a kind of pass"));
    }
    let keys = Keys::load()?;
    let held = ledger::lock()?;
    ledger::append_locked(&held, "pass-epoch", "", "", kind, 0)?;
    drop(held);
    state(&keys).map(|m| m.epoch(kind))
}

/// The ledger's pass lines, oldest first.
pub fn log() -> Result<Vec<ledger::Line>, String> {
    let loaded = ledger::load()?;
    Ok(loaded
        .lines
        .into_iter()
        .filter(|l| l.kind.starts_with("pass-"))
        .collect())
}

/// A pass from text: a token, a file holding one (or its bytes), `-` for
/// stdin, or a full or short hex id of a pass in the store.
pub fn read_pass(arg: &str) -> Result<Pass, String> {
    let arg = arg.trim();
    if arg.starts_with(cig_pass::TOKEN_PREFIX) {
        return Pass::from_token(arg).map_err(|e| e.to_string());
    }
    if arg == "-" {
        let mut t = String::new();
        io::Read::read_to_string(&mut io::stdin().lock(), &mut t).map_err(|e| e.to_string())?;
        return Pass::from_token(t.trim()).map_err(|e| e.to_string());
    }
    let p = Path::new(arg);
    if p.is_file() {
        let bytes = std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
        if let Ok(t) = std::str::from_utf8(&bytes) {
            if t.trim().starts_with(cig_pass::TOKEN_PREFIX) {
                return Pass::from_token(t.trim()).map_err(|e| e.to_string());
            }
        }
        return Pass::decode(&bytes).map_err(|e| e.to_string());
    }
    let hexid = arg.to_ascii_lowercase();
    if hexid.len() >= 8 && hexid.chars().all(|c| c.is_ascii_hexdigit()) {
        let s = store()?;
        let found: Vec<Pass> = s
            .list(None)
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|(m, _)| cig_pass::hex(&m.id).starts_with(&hexid))
            .filter_map(|(_, b)| Pass::decode(&b).ok())
            .collect();
        return match found.len() {
            1 => Ok(found.into_iter().next().expect("one")),
            0 => Err(format!("no pass here has an id starting {hexid}")),
            n => Err(format!(
                "{n} passes have an id starting {hexid}: give more of it"
            )),
        };
    }
    Err(format!(
        "`{arg}` is not a pass: give a token ({}...), a file, `-` for stdin, or an id",
        cig_pass::TOKEN_PREFIX
    ))
}

/// Canonical JSON for a subject or a result: keys sorted, no whitespace,
/// integers and strings only (a float could compare equal to an integer in
/// one place and not another, so it is refused).
pub fn canonical_json(v: &crate::value::Value) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    write_canonical(v, &mut out)?;
    Ok(out)
}

fn write_canonical(v: &crate::value::Value, out: &mut Vec<u8>) -> Result<(), String> {
    use crate::value::Value;
    match v {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Value::Int(n) => out.extend_from_slice(n.to_string().as_bytes()),
        Value::Float(_) => {
            return Err("a number with a fraction cannot be part of a pass's subject or result: use an integer or a string".to_string())
        }
        Value::Str(s) => out.extend_from_slice(
            serde_json::to_string(&**s).map_err(|e| e.to_string())?.as_bytes(),
        ),
        Value::List(items) => {
            out.push(b'[');
            for (i, x) in items.borrow().iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_canonical(x, out)?;
            }
            out.push(b']');
        }
        Value::Map(m) => {
            let m = m.borrow();
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            out.push(b'{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                out.extend_from_slice(serde_json::to_string(k).map_err(|e| e.to_string())?.as_bytes());
                out.push(b':');
                write_canonical(&m[*k], out)?;
            }
            out.push(b'}');
        }
        other => {
            return Err(format!(
                "a {} cannot be part of a pass's subject or result",
                other.type_name()
            ))
        }
    }
    Ok(())
}

/// A digest of a value as a pass records a global the work read: its type
/// and contents, nested lists and maps included, keys in order. A
/// function inside is named by its text's place, not its captures (what it
/// captured is checked where its body reads it). `None` for a value no
/// digest can name.
pub fn digest_value(v: &crate::value::Value) -> Option<[u8; 32]> {
    let mut h = blake3::Hasher::new();
    h.update(b"cigpass1-global");
    feed(&mut h, v, 0)?;
    Some(*h.finalize().as_bytes())
}

fn feed(h: &mut blake3::Hasher, v: &crate::value::Value, depth: usize) -> Option<()> {
    use crate::value::Value;
    if depth > 256 {
        return None;
    }
    match v {
        Value::Null => {
            h.update(b"n");
        }
        Value::Bool(b) => {
            h.update(if *b { b"t" } else { b"f" });
        }
        Value::Int(n) => {
            h.update(b"i");
            h.update(&n.to_le_bytes());
        }
        Value::Float(x) => {
            h.update(b"d");
            h.update(&x.to_bits().to_le_bytes());
        }
        Value::Str(s) => {
            h.update(b"s");
            h.update(&(s.len() as u64).to_le_bytes());
            h.update(s.as_bytes());
        }
        Value::List(items) => {
            let items = items.borrow();
            h.update(b"l");
            h.update(&(items.len() as u64).to_le_bytes());
            for x in items.iter() {
                feed(h, x, depth + 1)?;
            }
        }
        Value::Map(m) => {
            let m = m.borrow();
            h.update(b"m");
            h.update(&(m.len() as u64).to_le_bytes());
            for (k, x) in m.iter() {
                h.update(&(k.len() as u64).to_le_bytes());
                h.update(k.as_bytes());
                feed(h, x, depth + 1)?;
            }
        }
        Value::Func(f) => {
            h.update(b"F");
            h.update(f.name.as_deref().unwrap_or("pack").as_bytes());
            h.update(&(f.params.len() as u64).to_le_bytes());
        }
        Value::Builtin(b) => {
            h.update(b"B");
            h.update(b.name.as_bytes());
        }
        Value::Module(m) => {
            h.update(b"M");
            h.update(m.name.as_bytes());
        }
        Value::Chain(c) => {
            h.update(b"C");
            h.update(c.name.as_bytes());
        }
    }
    Some(())
}

// ----- parcels -----------------------------------------------------------------------

/// The pass a parcel carries beside it (`<parcel>.pass`): a sealed,
/// shareable `parcel` pass from this machine over the run's id and the
/// digest of the parcel's manifest. Another machine that trusts this one
/// for parcels knows who made it, and that nothing in it changed.
pub fn issue_parcel_pass(run: &str, manifest_sha256: &str) -> Result<Pass, String> {
    let keys = Keys::load()?;
    let marks = state(&keys)?;
    let result =
        serde_json::to_vec(&serde_json::json!({"manifest_sha256": manifest_sha256, "run": run}))
            .map_err(|e| e.to_string())?;
    let draft = Draft {
        kind: "parcel".to_string(),
        subject: run.as_bytes().to_vec(),
        inputs: Vec::new(),
        result,
        epoch: marks.epoch("parcel"),
        ledger_seq: 0,
        issued_ms: cig_pass::wall_ms(),
        mono: cig_pass::mono_now(),
        flags: 0,
    };
    let p = cig_pass::issue_shareable(&draft, &keys.signing_seed()).map_err(|e| e.to_string())?;
    cig_pass::seal(&p)
}

/// Check a parcel's pass: it holds here (a trusted signer for parcels),
/// and it is about this run and this manifest. The signer's principal is
/// returned for the message.
pub fn check_parcel_pass(
    pass: &Pass,
    run: Option<&str>,
    manifest_sha256: &str,
) -> Result<String, String> {
    let keys = Keys::load()?;
    let marks = state(&keys)?;
    let ctx = CigCtx::new(&keys, &marks, None);
    if pass.kind != "parcel" {
        return Err(format!("it is a {} pass, not a parcel's", pass.kind));
    }
    cig_pass::verify(pass, &ctx).map_err(|w| w.to_string())?;
    let said: serde_json::Value =
        serde_json::from_slice(&pass.result).map_err(|_| "its result does not read".to_string())?;
    if said["manifest_sha256"].as_str() != Some(manifest_sha256) {
        return Err("it is about another parcel (the manifest's digest differs)".to_string());
    }
    if let Some(r) = run {
        if said["run"].as_str() != Some(r) {
            return Err("it is about another run".to_string());
        }
    }
    let signer = match pass.issuer {
        cig_pass::Issuer::Ed25519(pk) if pk == keys.public() => "this machine".to_string(),
        cig_pass::Issuer::Ed25519(pk) => ctx
            .signers
            .iter()
            .find(|s| s.key == pk)
            .map(|s| s.principals.clone())
            .unwrap_or_else(|| "a trusted signer".to_string()),
        cig_pass::Issuer::Local(_) => "this machine".to_string(),
    };
    Ok(signer)
}

// ----- remembered answers ------------------------------------------------------------

/// An answer worth remembering while the files it depends on stay as they
/// are, for at most `life_ms`: the newest pass of `kind` for `subject` that
/// holds, else `compute` runs and its answer is kept as a pass over those
/// files (and this cig). Without a key, or with a state that cannot be
/// trusted, `compute` simply runs.
pub fn remembered<T, F>(kind: &str, subject: &[u8], files: &[&Path], life_ms: i64, compute: F) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned,
    F: FnOnce() -> T,
{
    let opened = Keys::load()
        .ok()
        .and_then(|k| state(&k).ok().map(|m| (k, m)));
    if let Some((keys, marks)) = &opened {
        let st = store().ok();
        let ctx = CigCtx::new(keys, marks, st.as_ref());
        if let Some((p, _)) = find(&ctx, kind, subject) {
            if let Ok(v) = serde_json::from_slice::<T>(&p.result) {
                return v;
            }
        }
    }
    let answer = compute();
    if let Some((keys, marks)) = &opened {
        let mut rec = Recorder::new();
        for f in files {
            if rec.file_seen(f).is_err() {
                return answer;
            }
        }
        if let Ok(result) = serde_json::to_vec(&answer) {
            let opts = IssueOpts {
                until: Some(cig_pass::wall_ms().saturating_add(life_ms)),
                cig: true,
                store: true,
                ..Default::default()
            };
            prepare(&mut rec, kind, &opts);
            let _ = issue(keys, marks, kind, subject, rec, result, &opts);
        }
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remembered_answer_is_reused_until_a_file_it_read_changes() {
        let _g = crate::burn::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let old_home = std::env::var_os("CIGSCRIPT_HOME");
        let old_key = std::env::var_os("CIG_SEAL_KEY");
        std::env::set_var("CIGSCRIPT_HOME", tmp.path().join("home"));
        std::env::set_var("CIG_SEAL_KEY", "file");
        let f = tmp.path().join("setting");
        std::fs::write(&f, "1").unwrap();
        let calls = std::cell::Cell::new(0);
        let ask = || {
            remembered("doctor", b"test-probe", &[f.as_path()], 60_000, || {
                calls.set(calls.get() + 1);
                vec![("probe".to_string(), true)]
            })
        };
        let a = ask();
        let b = ask();
        assert_eq!(a, b);
        assert_eq!(calls.get(), 1, "the second ask is a lookup");
        std::fs::write(&f, "22").unwrap();
        let _ = ask();
        assert_eq!(calls.get(), 2, "a changed file asks again");
        match old_home {
            Some(v) => std::env::set_var("CIGSCRIPT_HOME", v),
            None => std::env::remove_var("CIGSCRIPT_HOME"),
        }
        match old_key {
            Some(v) => std::env::set_var("CIG_SEAL_KEY", v),
            None => std::env::remove_var("CIG_SEAL_KEY"),
        }
    }
}
