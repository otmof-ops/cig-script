// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! The verifier: the only code in the cig family that decides "trusted".
//!
//! `verify` checks, in this order and before it touches anything the pass
//! names: the pass's kind policy, its tag or signatures, its issuer and
//! epoch, its revocation; then every caveat against the action the caller
//! says it is about to take; then every input against the world as it is
//! now. The first thing that fails is the answer, in words a person reads.

use crate::auth::{self, ChainFault};
use crate::encode::{
    discharge_subject, Caveat, Form, Input, Issuer, Mono, Network, Pass, FLAG_DECLARED_READS,
};
use crate::fsid::{self, clean, shown};
use crate::issue::keyed;
use crate::policy::policy;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt;
use std::io;

/// The deepest chain of passes that rely on passes this verifier follows.
pub const MAX_DEPTH: usize = 8;
/// The most passes one top-level verify checks, nested ones included.
pub const MAX_NESTED: usize = 1024;
/// The most inputs one top-level verify checks.
pub const MAX_INPUT_CHECKS: usize = 1_000_000;
/// The most bytes one top-level verify hashes.
pub const MAX_HASHED: u64 = 1 << 30;

/// The clocks, now.
#[derive(Clone, Copy, Debug)]
pub struct Now {
    pub wall_ms: i64,
    pub mono: Option<Mono>,
}

impl Now {
    pub fn read() -> Now {
        Now {
            wall_ms: fsid::wall_ms(),
            mono: fsid::mono_now(),
        }
    }
}

/// What the caller is about to do, as far as caveats look at it. The
/// caller is the enforcement point: it builds this from what it will
/// actually run, in the process that runs it, and uses the same value to
/// verify and to spend. A caveat about a field the caller did not supply
/// does not hold.
#[derive(Clone, Debug, Default)]
pub struct Action {
    /// The action runs in a box.
    pub boxed: bool,
    /// The action's network policy (`None`: not restricted, or not said).
    pub network: Option<Network>,
    /// The action's arguments (`None`: not said).
    pub argv: Option<Vec<String>>,
    /// The agent the action was started under, if any.
    pub agent: Option<String>,
    /// The folders the action works in (its pack's roots).
    pub roots: Vec<Vec<u8>>,
}

/// What the verifier asks of its caller.
pub trait Ctx {
    /// This machine's local subkey for a kind at an epoch (`None`: no key here).
    fn local_key(&self, kind: &str, epoch: u64) -> Option<[u8; 32]>;
    /// This machine's key for content digests in local passes.
    fn hash_key(&self) -> Option<[u8; 32]>;
    /// This machine's local issuer id (`None`: no key here).
    fn local_issuer(&self) -> Option<[u8; 16]>;
    /// The current epoch of a kind on this machine.
    fn epoch(&self, kind: &str) -> u64;
    /// How long this machine's ledger is now (`None`: not known, and not
    /// checked). A local pass made when the ledger was longer does not hold.
    fn ledger_seq(&self) -> Option<u64> {
        None
    }
    /// Is this Ed25519 key trusted to issue shareable passes of this kind?
    fn allowed_signer(&self, pk: &[u8; 32], kind: &str) -> bool;
    /// Is this Ed25519 key a human authority (for `human-yes`)? Read from
    /// a place the user's own processes cannot write.
    fn authority_signer(&self, _pk: &[u8; 32]) -> bool {
        false
    }
    /// Is this machine the issuer of this shareable key (so its epochs apply)?
    fn is_own_signer(&self, _pk: &[u8; 32]) -> bool {
        false
    }
    /// The clocks.
    fn now(&self) -> Now {
        Now::read()
    }
    /// A variable's value now (`:cwd` is the working folder).
    fn env(&self, name: &[u8]) -> Option<Vec<u8>>;
    /// A tool's version and build now.
    fn tool(&self, name: &str) -> Option<(String, String)>;
    /// A rule set's digest now.
    fn rules(&self, _name: &str) -> Option<[u8; 32]> {
        None
    }
    /// Is this build of a tool withdrawn (yanked)?
    fn yanked(&self, _tool: &str, _version: &str, _build: &str) -> bool {
        false
    }
    /// The running cig's version and build.
    fn cig(&self) -> (String, String);
    /// The action the caveats are checked against.
    fn action(&self) -> &Action;
    /// Is this id (a pass or a prefix of one) revoked?
    fn revoked(&self, _id: &[u8; 32]) -> bool {
        false
    }
    /// How many times this spend key has been spent.
    fn spends(&self, _key: &[u8; 32]) -> u32 {
        0
    }
    /// A pass by id, for a `Pass` input.
    fn find_pass(&self, _id: &[u8; 32]) -> Option<Pass> {
        None
    }
    /// The discharges held for a pass's root id, of a kind.
    fn discharges(&self, _kind: &str, _root: &[u8; 32]) -> Vec<Pass> {
        Vec::new()
    }
}

/// A key a `once` or `max n` caveat is counted under: the id of the pass
/// up to and including that caveat, so a holder who narrows the pass
/// further keeps the same key and cannot spend it again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpendKey {
    pub key: [u8; 32],
    pub limit: u32,
}

/// A pass that verified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verified {
    pub id: [u8; 32],
    pub root_id: [u8; 32],
    /// Every key a spend must count: the pass's own and its discharges'.
    pub spend: Vec<SpendKey>,
    /// An input's identity changed though its bytes did not: a fresh pass
    /// would verify faster.
    pub rehashed: bool,
}

/// Why a pass does not verify. `Display` says it in one sentence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Why {
    NoKey,
    WrongIssuer,
    Epoch { then: u64, now: u64, kind: String },
    Tag,
    UnknownSigner,
    Policy(String),
    Chain(String),
    Revoked { prefix: usize },
    Caveat { text: String, why: String },
    Input { what: String },
    Spent { text: String, used: u32 },
    Discharge(String),
    TooDeep,
    TooMuch(String),
}

impl fmt::Display for Why {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Why::NoKey => f.write_str("there is no key here to check it with"),
            Why::WrongIssuer => f.write_str(
                "issued under another key (another machine, or before cig tin rekey)",
            ),
            Why::Epoch { then, now, kind } => write!(
                f,
                "issued at epoch {then}; {kind} passes are at epoch {now} now (cig pass revoke --kind {kind})"
            ),
            Why::Tag => {
                f.write_str("its tag does not hold: it was altered, or a caveat was removed")
            }
            Why::UnknownSigner => {
                f.write_str("its signer is not allowed to issue this kind of pass here")
            }
            Why::Policy(w) => f.write_str(w),
            Why::Chain(w) => write!(f, "its signatures do not hold: {w}"),
            Why::Revoked { prefix: 0 } => f.write_str("revoked (cig pass log says when)"),
            Why::Revoked { prefix } => write!(
                f,
                "revoked as it was with {prefix} caveat(s) (cig pass log says when)"
            ),
            Why::Caveat { text, why } => write!(f, "the caveat \"{text}\" does not hold: {why}"),
            Why::Input { what } => f.write_str(what),
            Why::Spent { text, used } => write!(
                f,
                "spent: \"{text}\", used {used} time(s) already (cig pass log)"
            ),
            Why::Discharge(k) if k == "human-yes" => f.write_str(
                "it needs a person's yes (a human-yes pass from a human authority), and there is none for it here",
            ),
            Why::Discharge(k) => write!(f, "it needs a {k} pass for it, and there is none here"),
            Why::TooDeep => write!(f, "it relies on passes more than {MAX_DEPTH} deep"),
            Why::TooMuch(w) => write!(f, "checking it would take too much: {w}"),
        }
    }
}

impl std::error::Error for Why {}

/// Verify a pass now.
pub fn verify(pass: &Pass, ctx: &dyn Ctx) -> Result<Verified, Why> {
    Verifier::new(ctx).verify(pass, 0)
}

/// A verifier that remembers the passes it has checked (a pass relied on by
/// many is checked once) and bounds the work of one verify.
pub struct Verifier<'a> {
    ctx: &'a dyn Ctx,
    memo: RefCell<HashMap<[u8; 32], Result<Verified, Why>>>,
    passes: Cell<usize>,
    inputs: Cell<usize>,
    hashed: Cell<u64>,
}

impl<'a> Verifier<'a> {
    pub fn new(ctx: &'a dyn Ctx) -> Self {
        Verifier {
            ctx,
            memo: RefCell::new(HashMap::new()),
            passes: Cell::new(0),
            inputs: Cell::new(0),
            hashed: Cell::new(0),
        }
    }

    pub fn verify(&self, pass: &Pass, depth: usize) -> Result<Verified, Why> {
        if depth > MAX_DEPTH {
            return Err(Why::TooDeep);
        }
        let ids = pass.prefix_ids();
        let id = *ids.last().expect("at least the root id");
        if let Some(r) = self.memo.borrow().get(&id) {
            return r.clone();
        }
        let n = self.passes.get() + 1;
        self.passes.set(n);
        if n > MAX_NESTED {
            return Err(Why::TooMuch(format!("more than {MAX_NESTED} passes")));
        }
        let r = self.verify_uncached(pass, &ids, depth);
        self.memo.borrow_mut().insert(id, r.clone());
        r
    }

    fn verify_uncached(
        &self,
        pass: &Pass,
        ids: &[[u8; 32]],
        depth: usize,
    ) -> Result<Verified, Why> {
        let ctx = self.ctx;
        // 1: the kind's policy, before any key or path is touched.
        let pol = policy(&pass.kind)
            .ok_or_else(|| Why::Policy(format!("`{}` is not a kind of pass", clean(&pass.kind))))?;
        match pass.form {
            Form::Local if !pol.local => {
                return Err(Why::Policy(format!(
                    "a {} pass is never a local one",
                    pass.kind
                )))
            }
            Form::Shareable if !pol.shareable => {
                return Err(Why::Policy(format!(
                    "a {} pass is never a shared one",
                    pass.kind
                )))
            }
            _ => {}
        }
        if pass.flags & FLAG_DECLARED_READS != 0 && pol.refuse_declared_reads {
            return Err(Why::Policy(format!(
                "a {} pass cannot rest on a hop's declared reads",
                pass.kind
            )));
        }
        if pol.no_needs && pass.caveats.iter().any(|c| matches!(c, Caveat::Needs(_))) {
            return Err(Why::Policy(format!(
                "a {} pass cannot itself need another",
                pass.kind
            )));
        }
        if let Some(life) = pol.max_life_ms {
            let ok = pass
                .caveats
                .iter()
                .any(|c| matches!(c, Caveat::Until(t) if t.saturating_sub(pass.issued_ms) <= life));
            if !ok {
                return Err(Why::Policy(format!(
                    "a {} pass must expire within {} minutes of being made",
                    pass.kind,
                    life / 60_000
                )));
            }
        }
        // 2 and 3: the tag or the chain, the issuer, the epoch.
        match (pass.form, pass.issuer) {
            (Form::Local, Issuer::Local(issuer)) => {
                let Some(mine) = ctx.local_issuer() else {
                    return Err(Why::NoKey);
                };
                if !bool::from(subtle::ConstantTimeEq::ct_eq(&mine[..], &issuer[..])) {
                    return Err(Why::WrongIssuer);
                }
                let Some(key) = ctx.local_key(&pass.kind, pass.epoch) else {
                    return Err(Why::NoKey);
                };
                if !auth::local_ok(&key, pass) {
                    return Err(Why::Tag);
                }
                let now = ctx.epoch(&pass.kind);
                if pass.epoch != now {
                    return Err(Why::Epoch {
                        then: pass.epoch,
                        now,
                        kind: pass.kind.clone(),
                    });
                }
                if let Some(len) = ctx.ledger_seq() {
                    if len < pass.ledger_seq {
                        return Err(Why::Policy(format!(
                            "the ledger is shorter than when it was made ({len} lines now, {} then): cut, deleted or put back from an older copy",
                            pass.ledger_seq
                        )));
                    }
                }
            }
            (Form::Shareable, Issuer::Ed25519(pk)) => {
                let trusted = if pol.authority {
                    ctx.authority_signer(&pk)
                } else if ctx.is_own_signer(&pk) {
                    true
                } else {
                    pol.foreign && ctx.allowed_signer(&pk, &pass.kind)
                };
                if !trusted {
                    return Err(Why::UnknownSigner);
                }
                auth::share_ok(pass).map_err(|f| {
                    Why::Chain(match f {
                        ChainFault::IssuerKey => "the issuer's key is not a valid key".to_string(),
                        ChainFault::Block(0) => "the issuer's signature".to_string(),
                        ChainFault::Block(n) => format!("the block for caveat {n}"),
                        ChainFault::Proof => "the proof at its end".to_string(),
                        ChainFault::NotShareable => "it is not a shareable pass".to_string(),
                    })
                })?;
                if ctx.is_own_signer(&pk) {
                    let now = ctx.epoch(&pass.kind);
                    if pass.epoch != now {
                        return Err(Why::Epoch {
                            then: pass.epoch,
                            now,
                            kind: pass.kind.clone(),
                        });
                    }
                }
            }
            _ => return Err(Why::Tag),
        }
        // 4: revocation of the pass or any pass it was narrowed from.
        for (k, pid) in ids.iter().enumerate() {
            if ctx.revoked(pid) {
                return Err(Why::Revoked { prefix: k });
            }
        }
        // 5: caveats, in order.
        let root = ids[0];
        let mut spend = Vec::new();
        for (i, c) in pass.caveats.iter().enumerate() {
            match c {
                Caveat::Once | Caveat::Max(_) => {
                    let limit = if let Caveat::Max(n) = c { *n } else { 1 };
                    let key = ids[i + 1];
                    let used = ctx.spends(&key);
                    if used >= limit {
                        return Err(Why::Spent {
                            text: crate::text::caveat_text(c),
                            used,
                        });
                    }
                    spend.push(SpendKey { key, limit });
                }
                Caveat::Needs(kind) => {
                    let want = discharge_subject(&root);
                    let mut found = None;
                    for d in ctx.discharges(kind, &root) {
                        if d.kind != *kind
                            || !bool::from(subtle::ConstantTimeEq::ct_eq(&d.subject[..], &want[..]))
                        {
                            continue;
                        }
                        if let Ok(v) = self.verify(&d, depth + 1) {
                            found = Some(v);
                            break;
                        }
                    }
                    match found {
                        Some(v) => spend.extend(v.spend),
                        None => return Err(Why::Discharge(kind.clone())),
                    }
                }
                _ => {
                    if let Err(why) = caveat_holds(c, pass, ctx) {
                        return Err(Why::Caveat {
                            text: clean(&crate::text::caveat_text(c)),
                            why,
                        });
                    }
                }
            }
        }
        // 6: inputs, in encoded order.
        let mut rehashed = false;
        for input in &pass.inputs {
            let n = self.inputs.get() + 1;
            self.inputs.set(n);
            if n > MAX_INPUT_CHECKS {
                return Err(Why::TooMuch(format!("more than {MAX_INPUT_CHECKS} inputs")));
            }
            match self.input_holds(input, pass, depth) {
                Ok(r) => rehashed |= r,
                Err(what) => return Err(Why::Input { what }),
            }
        }
        Ok(Verified {
            id: *ids.last().expect("at least the root id"),
            root_id: root,
            spend,
            rehashed,
        })
    }

    /// The digest a pass of this form carries for a plain digest.
    fn as_carried(&self, pass: &Pass, plain: &[u8; 32]) -> Result<[u8; 32], String> {
        match pass.form {
            Form::Local => match self.ctx.hash_key() {
                Some(k) => Ok(keyed(&k, plain)),
                None => Err("there is no key here to check its inputs with".to_string()),
            },
            Form::Shareable => Ok(*plain),
        }
    }

    /// Does this input hold now? `Ok(true)` when it held only after a hash.
    fn input_holds(&self, input: &Input, pass: &Pass, depth: usize) -> Result<bool, String> {
        let ctx = self.ctx;
        let local = pass.form == Form::Local;
        let path_of = |b: &[u8]| {
            fsid::bytes_path(b)
                .ok_or_else(|| format!("{} cannot be a path on this system", shown(b)))
        };
        match input {
            Input::File {
                path,
                identity,
                racy,
                blake3,
            } => {
                let p = path_of(path)?;
                let md = match std::fs::metadata(&p) {
                    Ok(md) => md,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        return Err(format!("{} is gone", shown(path)))
                    }
                    Err(e) => return Err(format!("{} cannot be read now: {e}", shown(path))),
                };
                if !md.is_file() {
                    return Err(format!("{} is not a file now", shown(path)));
                }
                let now = fsid::identity(&md);
                if local
                    && fsid::identity_is_enough()
                    && !*racy
                    && now == *identity
                    && !fsid::is_racy(identity.mtime_ns, pass.issued_ms)
                    && !fsid::is_racy(identity.ctime_ns, pass.issued_ms)
                    && !fsid::always_hash(&p)
                {
                    return Ok(false);
                }
                let left = MAX_HASHED.saturating_sub(self.hashed.get());
                let h = fsid::hash_regular(&p, left)
                    .map_err(|e| format!("{} cannot be checked now: {e}", shown(path)))?;
                self.hashed.set(self.hashed.get() + h.bytes);
                if self.as_carried(pass, &h.digest)? == *blake3 {
                    Ok(h.identity != *identity)
                } else if local && h.identity.size != identity.size {
                    Err(format!(
                        "{} changed (size {} then, {} now)",
                        shown(path),
                        identity.size,
                        h.identity.size
                    ))
                } else {
                    Err(format!("{} changed", shown(path)))
                }
            }
            Input::Absent { path } => {
                let p = path_of(path)?;
                match std::fs::metadata(&p) {
                    Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
                    Ok(_) => Err(format!("{} appeared", shown(path))),
                    Err(e) => Err(format!("{} cannot be checked now: {e}", shown(path))),
                }
            }
            Input::Dir { path, listing } => {
                let p = path_of(path)?;
                let entries = fsid::list_dir(&p)
                    .map_err(|e| format!("the folder {} cannot be listed now: {e}", shown(path)))?;
                if self.as_carried(pass, &fsid::listing_digest(&entries))? == *listing {
                    Ok(false)
                } else {
                    Err(format!(
                        "the folder {} changed (a name added, removed or retyped)",
                        shown(path)
                    ))
                }
            }
            Input::Present { path, kind } => {
                let p = path_of(path)?;
                match fsid::present_kind(&p) {
                    Ok(k) if k == *kind => Ok(false),
                    Ok(k) if local => Err(format!(
                        "{} is {} now, not {}",
                        shown(path),
                        k.word(),
                        kind.word()
                    )),
                    Ok(_) => Err(format!("{} changed", shown(path))),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        Err(format!("{} is gone", shown(path)))
                    }
                    Err(e) => Err(format!("{} cannot be checked now: {e}", shown(path))),
                }
            }
            Input::Env { name, value } => {
                let now = match ctx.env(name) {
                    Some(v) => Some(self.as_carried(pass, &fsid::env_digest(&v))?),
                    None => None,
                };
                if now == *value {
                    Ok(false)
                } else {
                    let n = clean(&String::from_utf8_lossy(name));
                    Err(match (value, now) {
                        (None, Some(_)) => format!("${n} is set now"),
                        (Some(_), None) => format!("${n} is unset now"),
                        _ => format!("${n} changed"),
                    })
                }
            }
            Input::Tool {
                name,
                version,
                build,
            } => {
                let now = if name == "cig" {
                    Some(ctx.cig())
                } else {
                    ctx.tool(name)
                };
                let (n, v, b) = (clean(name), clean(version), clean(build));
                match now {
                    Some((nv, nb)) if nv == *version && nb == *build => {
                        if ctx.yanked(name, version, build) {
                            Err(format!("{n} {v} ({b}) has been withdrawn (YANKED)"))
                        } else {
                            Ok(false)
                        }
                    }
                    Some((nv, nb)) if local => Err(format!(
                        "{n} is {} ({}) now; the pass was made with {v} ({b})",
                        clean(&nv),
                        clean(&nb)
                    )),
                    Some(_) => Err(format!("{n} is another version now")),
                    None => Err(format!("{n} is not known here")),
                }
            }
            Input::Rules { name, blake3 } => match ctx.rules(name) {
                Some(d) if d == *blake3 => Ok(false),
                Some(_) => Err(format!("the rules {} changed", clean(name))),
                None => Err(format!("the rules {} are not known here", clean(name))),
            },
            Input::Pass { id } => {
                let Some(inner) = ctx.find_pass(id) else {
                    return Err(format!(
                        "the pass {} it relies on is not here",
                        crate::encode::hex8(id)
                    ));
                };
                if inner.id() != *id {
                    return Err(format!(
                        "the pass {} it relies on is not that pass",
                        crate::encode::hex8(id)
                    ));
                }
                self.verify(&inner, depth + 1)
                    .map(|_| false)
                    .map_err(|w| format!("the pass {} it relies on: {w}", crate::encode::hex8(id)))
            }
            Input::Clock { not_after_ms } => {
                if time_ok(*not_after_ms, pass, ctx.now()) {
                    Ok(false)
                } else {
                    Err(
                        "what it vouched for was true only for a while, and the while is over"
                            .to_string(),
                    )
                }
            }
        }
    }
}

/// Is `now` still at or before `t`, by the wall clock and, on the boot the
/// pass was issued in, by the clock that never goes back?
fn time_ok(t: i64, pass: &Pass, now: Now) -> bool {
    if now.wall_ms > t {
        return false;
    }
    if let (Some(then), Some(nowm)) = (pass.mono, now.mono) {
        if then.boot_id == nowm.boot_id {
            let Some(elapsed_ns) = nowm.ns.checked_sub(then.ns) else {
                return false;
            };
            let elapsed_ms = i64::try_from(elapsed_ns / 1_000_000).unwrap_or(i64::MAX);
            if pass.issued_ms.saturating_add(elapsed_ms) > t {
                return false;
            }
        }
    }
    true
}

/// A path made canonical where it exists; `None` for one that cannot be
/// resolved or that climbs out with `..`.
fn canonical(b: &[u8]) -> Option<std::path::PathBuf> {
    let p = fsid::bytes_path(b)?;
    if p.components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return None;
    }
    std::fs::canonicalize(&p).ok().or(Some(p))
}

fn caveat_holds(c: &Caveat, pass: &Pass, ctx: &dyn Ctx) -> Result<(), String> {
    let action = ctx.action();
    match c {
        Caveat::Within(p) => {
            if action.roots.is_empty() {
                return Err("the action names no folder".to_string());
            }
            let Some(outer) = canonical(p) else {
                return Err("its folder cannot be resolved".to_string());
            };
            for r in &action.roots {
                let Some(inner) = fsid::bytes_path(r).and_then(|p| std::fs::canonicalize(p).ok())
                else {
                    return Err(format!("{} cannot be resolved", shown(r)));
                };
                if !inner.starts_with(&outer) {
                    return Err(format!(
                        "it works in {}",
                        clean(&inner.display().to_string())
                    ));
                }
            }
            Ok(())
        }
        Caveat::Until(t) => {
            if time_ok(*t, pass, ctx.now()) {
                Ok(())
            } else {
                Err("that time has passed".to_string())
            }
        }
        Caveat::Boxed => {
            if action.boxed {
                Ok(())
            } else {
                Err("the action is not boxed".to_string())
            }
        }
        Caveat::Network(want) => match (&action.network, want) {
            (Some(Network::None), _) => Ok(()),
            (Some(Network::Hosts(have)), Network::Hosts(allowed)) => {
                let norm = |h: &str| h.trim_end_matches('.').to_ascii_lowercase();
                match have
                    .iter()
                    .find(|h| !allowed.iter().any(|a| norm(a) == norm(h)))
                {
                    None => Ok(()),
                    Some(h) => Err(format!("the action may reach {}", clean(h))),
                }
            }
            (Some(Network::Hosts(_)), Network::None) => {
                Err("the action may reach the network".to_string())
            }
            (None, _) => Err("the action's network is not restricted".to_string()),
        },
        Caveat::Args(globs) => {
            let Some(argv) = &action.argv else {
                return Err("the action's arguments were not given".to_string());
            };
            args_match(globs, argv)
        }
        Caveat::CigAtLeast(v) => {
            let (have, _) = ctx.cig();
            match (version_parts(&have), version_parts(v)) {
                (Some(h), Some(w)) if h >= w => Ok(()),
                (Some(_), Some(_)) => Err(format!("this is cig {}", clean(&have))),
                _ => Err(format!(
                    "cig {} cannot be compared with {}",
                    clean(&have),
                    clean(v)
                )),
            }
        }
        Caveat::Build(id) => {
            let (_, have) = ctx.cig();
            if have == *id {
                Ok(())
            } else {
                Err(format!("this cig is build {}", clean(&have)))
            }
        }
        Caveat::Agent(name) => match &action.agent {
            Some(a) if a == name => Ok(()),
            Some(a) => Err(format!("the action was started under {}", clean(a))),
            None => Err("the action was not started under an agent".to_string()),
        },
        Caveat::Once | Caveat::Max(_) | Caveat::Needs(_) => Ok(()),
    }
}

/// Match arguments one pattern to one argument; a last `...` takes any
/// further ones. `*` never crosses from one argument into the next.
pub fn args_match(globs: &[String], argv: &[String]) -> Result<(), String> {
    let (globs, rest) = match globs.split_last() {
        Some((last, head)) if last == "..." => (head, true),
        _ => (globs, false),
    };
    if argv.len() < globs.len() || (!rest && argv.len() != globs.len()) {
        return Err(format!(
            "the action has {} argument(s); the caveat allows {}{}",
            argv.len(),
            globs.len(),
            if rest { " or more" } else { "" }
        ));
    }
    for (i, (g, a)) in globs.iter().zip(argv).enumerate() {
        let m = globset::GlobBuilder::new(g)
            .literal_separator(false)
            .backslash_escape(true)
            .build()
            .map_err(|e| format!("its pattern {} does not read: {e}", i + 1))?
            .compile_matcher();
        if !m.is_match(a) {
            return Err(format!("argument {} is `{}`", i + 1, clean(a)));
        }
    }
    Ok(())
}

/// `1.20.0` → [1, 20, 0, 1]; `1.2` → [1, 2, 0, 1]. A pre-release
/// (`1.20.0-rc1`) orders below its release, so it is not "at least" it.
pub fn version_parts(v: &str) -> Option<[u64; 4]> {
    let v = v.trim().trim_start_matches('v');
    let v = v.split('+').next()?;
    let (core, pre) = match v.split_once('-') {
        Some((c, _)) => (c, true),
        None => (v, false),
    };
    let mut out = [0u64; 4];
    let parts: Vec<&str> = core.split('.').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    for (i, p) in parts.iter().enumerate() {
        out[i] = p.parse().ok()?;
    }
    out[3] = u64::from(!pre);
    Some(out)
}
