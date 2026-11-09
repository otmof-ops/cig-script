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

fn marks_path() -> PathBuf {
    dir().join("marks")
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
    let loaded = Marks::load(&marks_path(), &Guard, &mac)
        .map_err(|e| format!("the pass state cannot be read: {e}"))?;
    let mut marks = match loaded {
        Loaded::Ok(m) => m,
        Loaded::Stale => Marks::empty(),
        Loaded::Tampered(why) => {
            return Err(format!(
                "{STATE_E739}: the pass state does not verify ({why}); every pass is refused until cig doctor looks"
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
                let _ = marks.save(&marks_path(), &Guard, &mac);
            }
            Ok(marks)
        }
        ledger::Since::Shorter => Err(format!(
            "{STATE_E739}: the ledger is shorter than the pass state expects (cut, or an older copy put back); every pass is refused until cig doctor looks"
        )),
        ledger::Since::Swapped => Err(format!(
            "{STATE_E739}: the ledger is not the one the pass state was read from; every pass is refused until cig doctor looks"
        )),
    }
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
        crate::burn::runs::home_dir().join("allowed_signers")
    } else {
        PathBuf::from(c)
    }
}

/// The allowed signers in a file (none when it is not there).
pub fn read_signers(path: &Path) -> Vec<cig_pass::ssh::Signer> {
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

/// Issue a local pass from what the work recorded, and keep it.
pub fn issue(
    keys: &Keys,
    marks: &Marks,
    kind: &str,
    subject: &[u8],
    mut rec: Recorder,
    result: Vec<u8>,
    opts: &IssueOpts,
) -> Result<Pass, IssueError> {
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
    let flags = rec.flags();
    let inputs = rec.finish()?;
    let epoch = marks.epoch(kind);
    let draft = Draft {
        kind: kind.to_string(),
        subject: subject.to_vec(),
        inputs,
        result,
        epoch,
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
        keep(&pass).map_err(IssueError::NoKey)?;
    }
    Ok(pass)
}

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

fn parse_size(v: &str) -> Option<u64> {
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
/// carries (and of its discharges'), as one step under the ledger's lock.
/// Refused when any of them is used up.
pub fn spend(pass: &Pass, setup: impl FnOnce(&mut CigCtx<'_>)) -> Result<Verified, String> {
    let keys = Keys::load()?;
    let held = ledger::lock()?;
    let marks = state(&keys)?;
    let st = store().ok();
    let mut ctx = CigCtx::new(&keys, &marks, st.as_ref());
    setup(&mut ctx);
    let v = cig_pass::verify(pass, &ctx).map_err(|w| w.to_string())?;
    for s in &v.spend {
        if marks.spend_count(&s.key) >= s.limit {
            return Err(Why::Spent {
                text: format!("used {} of {}", marks.spend_count(&s.key), s.limit),
                used: marks.spend_count(&s.key),
            }
            .to_string());
        }
    }
    for s in &v.spend {
        ledger::append_locked(&held, "pass-spend", "", &cig_pass::hex(&s.key), "spent", 0)?;
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
