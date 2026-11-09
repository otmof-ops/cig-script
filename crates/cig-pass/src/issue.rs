// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! Issuing a pass: the inputs the recorder saw, the result, and the
//! issuer's key, made into a pass that only that key (local) or anyone who
//! trusts that key for its kind (shareable) will accept.

use crate::auth;
use crate::encode::{
    is_absolute_bytes, subject_digest, Caveat, Form, Input, Issuer, Mono, Network, Pass, Proof,
    Tag, FLAG_DECLARED_READS, MAX_GLOB, MAX_INPUTS, MAX_LIST, MAX_PASS, MAX_RESULT,
};
use crate::fsid;
use crate::policy::policy;
use std::fmt;

/// Why a pass was not issued.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IssueError {
    /// The work did something no recording can see into: no pass.
    OpenWorld(String),
    /// The same input gave two answers while the work ran.
    Changed(String),
    /// The pass would break a limit (too many inputs, a result too big).
    TooBig(String),
    /// A kind this version does not know, a form its policy forbids, or a
    /// malformed field.
    Invalid(String),
    /// No key to issue with.
    NoKey(String),
}

impl fmt::Display for IssueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IssueError::OpenWorld(w) => write!(
                f,
                "{w}: a pass cannot vouch for what it cannot see, so none was issued"
            ),
            IssueError::Changed(w) => write!(
                f,
                "{w} changed while the work ran: two reads, two answers, so no pass was issued"
            ),
            IssueError::TooBig(w) | IssueError::Invalid(w) | IssueError::NoKey(w) => f.write_str(w),
        }
    }
}

impl std::error::Error for IssueError {}

/// What a new pass says, before it is signed.
#[derive(Clone, Debug)]
pub struct Draft {
    pub kind: String,
    /// The subject's bytes; the pass carries their digest.
    pub subject: Vec<u8>,
    /// From `Recorder::finish`: sorted, deduplicated, plain digests.
    pub inputs: Vec<Input>,
    pub result: Vec<u8>,
    pub epoch: u64,
    pub issued_ms: i64,
    pub mono: Option<Mono>,
    /// From `Recorder::flags`.
    pub flags: u8,
}

/// A content digest as a local pass carries it: keyed under the issuer's
/// hash subkey, so a token handed to someone does not let them test
/// guesses at a short secret it depended on.
pub fn keyed(hash_key: &[u8; 32], plain: &[u8; 32]) -> [u8; 32] {
    *blake3::keyed_hash(hash_key, plain).as_bytes()
}

/// The inputs with every content digest keyed.
fn key_inputs(inputs: &[Input], hash_key: &[u8; 32]) -> Vec<Input> {
    inputs
        .iter()
        .map(|i| match i {
            Input::File {
                path,
                identity,
                racy,
                blake3,
            } => Input::File {
                path: path.clone(),
                identity: *identity,
                racy: *racy,
                blake3: keyed(hash_key, blake3),
            },
            Input::Dir { path, listing } => Input::Dir {
                path: path.clone(),
                listing: keyed(hash_key, listing),
            },
            Input::Env { name, value } => Input::Env {
                name: name.clone(),
                value: value.map(|v| keyed(hash_key, &v)),
            },
            other => other.clone(),
        })
        .collect()
}

impl Draft {
    fn unsigned(&self, form: Form, issuer: Issuer, inputs: Vec<Input>) -> Result<Pass, IssueError> {
        let pol = policy(&self.kind).ok_or_else(|| {
            IssueError::Invalid(format!(
                "`{}` is not a kind of pass this cig knows",
                self.kind
            ))
        })?;
        match form {
            Form::Local if !pol.local => {
                return Err(IssueError::Invalid(format!(
                    "a {} pass is never a local one",
                    self.kind
                )))
            }
            Form::Shareable if !pol.shareable => {
                return Err(IssueError::Invalid(format!(
                    "a {} pass is never shared: it is only for the machine that made it",
                    self.kind
                )))
            }
            _ => {}
        }
        if self.flags & !FLAG_DECLARED_READS != 0 {
            return Err(IssueError::Invalid(
                "flags this cig does not know".to_string(),
            ));
        }
        if self.flags & FLAG_DECLARED_READS != 0 && pol.refuse_declared_reads {
            return Err(IssueError::Invalid(format!(
                "a {} pass cannot rest on a hop's declared reads: only on what was seen",
                self.kind
            )));
        }
        if inputs.len() > MAX_INPUTS {
            return Err(IssueError::TooBig(format!(
                "{} inputs; a pass holds at most {MAX_INPUTS}",
                inputs.len()
            )));
        }
        if self.result.len() > MAX_RESULT {
            return Err(IssueError::TooBig(format!(
                "a result of {} bytes; a pass carries at most 64 KiB",
                self.result.len()
            )));
        }
        for w in inputs.windows(2) {
            if w[0].sort_key() >= w[1].sort_key() {
                return Err(IssueError::Invalid(
                    "the inputs are not sorted and distinct (use Recorder::finish)".to_string(),
                ));
            }
        }
        for i in &inputs {
            check_input(i)?;
        }
        let pass = Pass {
            form,
            kind: self.kind.clone(),
            flags: self.flags,
            subject: subject_digest(&self.subject),
            inputs,
            result: self.result.clone(),
            issuer,
            epoch: self.epoch,
            issued_ms: self.issued_ms,
            mono: self.mono,
            caveats: Vec::new(),
            tag: Tag::Local([0; 32]),
        };
        if pass.encode_base().len() > MAX_PASS / 2 {
            return Err(IssueError::TooBig(
                "the pass would be larger than 2 MiB".to_string(),
            ));
        }
        Ok(pass)
    }
}

fn check_len(what: &str, n: usize) -> Result<(), IssueError> {
    if n > u16::MAX as usize {
        return Err(IssueError::TooBig(format!(
            "{what} longer than 65,535 bytes"
        )));
    }
    Ok(())
}

fn no_nul(what: &str, s: &[u8]) -> Result<(), IssueError> {
    if s.contains(&0) {
        return Err(IssueError::Invalid(format!("{what} with a NUL in it")));
    }
    Ok(())
}

fn check_input(i: &Input) -> Result<(), IssueError> {
    match i {
        Input::File { path, .. }
        | Input::Absent { path }
        | Input::Dir { path, .. }
        | Input::Present { path, .. } => {
            check_len("a path", path.len())?;
            if !is_absolute_bytes(path) {
                return Err(IssueError::Invalid(format!(
                    "{} is not an absolute path",
                    fsid::shown(path)
                )));
            }
        }
        Input::Env { name, .. } => {
            check_len("a variable's name", name.len())?;
            if name.is_empty() || name.contains(&b'=') || name.contains(&0) {
                return Err(IssueError::Invalid(
                    "a variable's name that cannot be one".to_string(),
                ));
            }
        }
        Input::Tool {
            name,
            version,
            build,
        } => {
            check_len("a tool's name", name.len())?;
            check_len("a version", version.len())?;
            check_len("a build id", build.len())?;
            no_nul("a tool", name.as_bytes())?;
            no_nul("a version", version.as_bytes())?;
            no_nul("a build id", build.as_bytes())?;
            if name.is_empty() {
                return Err(IssueError::Invalid("a tool with no name".to_string()));
            }
        }
        Input::Rules { name, .. } => {
            check_len("a rule set's name", name.len())?;
            no_nul("a rule set", name.as_bytes())?;
            if name.is_empty() {
                return Err(IssueError::Invalid("a rule set with no name".to_string()));
            }
        }
        Input::Pass { .. } | Input::Clock { .. } => {}
    }
    Ok(())
}

/// Is this caveat one a pass can carry (lengths, values)?
pub(crate) fn check_caveat(c: &Caveat) -> Result<(), String> {
    let text = |what: &str, s: &str, max: usize| {
        if s.len() > max {
            Err(format!("{what} longer than {max} bytes"))
        } else if s.contains('\0') {
            Err(format!("{what} with a NUL in it"))
        } else {
            Ok(())
        }
    };
    match c {
        Caveat::Within(p) => {
            if p.len() > u16::MAX as usize {
                return Err("a path longer than 65,535 bytes".to_string());
            }
            if !is_absolute_bytes(p) {
                return Err("`within` takes an absolute path".to_string());
            }
        }
        Caveat::Network(Network::Hosts(h)) => {
            if h.is_empty() || h.len() > MAX_LIST {
                return Err("`network` takes `none` or 1 to 64 hosts".to_string());
            }
            for x in h {
                text("a host", x, 253)?;
                if x.is_empty() {
                    return Err("an empty host".to_string());
                }
            }
        }
        Caveat::Args(globs) => {
            if globs.is_empty() || globs.len() > MAX_LIST {
                return Err("`args` takes 1 to 64 patterns".to_string());
            }
            for g in globs {
                text("a pattern", g, MAX_GLOB)?;
            }
        }
        Caveat::CigAtLeast(s) | Caveat::Build(s) | Caveat::Agent(s) => {
            text("a caveat's text", s, 1024)?;
            if s.is_empty() {
                return Err("a caveat with nothing after its word".to_string());
            }
        }
        Caveat::Needs(k) => {
            if policy(k).is_none() {
                return Err(format!("`needs` takes a kind of pass; `{k}` is not one"));
            }
        }
        Caveat::Max(0) => return Err("`max 0` would never hold".to_string()),
        Caveat::Until(_)
        | Caveat::Boxed
        | Caveat::Network(Network::None)
        | Caveat::Once
        | Caveat::Max(_) => {}
    }
    Ok(())
}

/// Issue a local pass under `key` (the issuer's subkey for this kind and
/// epoch), its content digests keyed under `hash_key`.
pub fn issue_local(
    draft: &Draft,
    issuer: [u8; 16],
    key: &[u8; 32],
    hash_key: &[u8; 32],
) -> Result<Pass, IssueError> {
    let inputs = key_inputs(&draft.inputs, hash_key);
    let mut p = draft.unsigned(Form::Local, Issuer::Local(issuer), inputs)?;
    p.tag = Tag::Local(auth::local_tag0(key, &p.encode_base()));
    Ok(p)
}

/// Issue a shareable pass signed with an Ed25519 key (the machine's, or an
/// authority's for `human-yes`). Its digests are plain: another machine
/// checks them.
pub fn issue_shareable(draft: &Draft, signing_seed: &[u8; 32]) -> Result<Pass, IssueError> {
    let sk = auth::signing_key(signing_seed);
    let mut p = draft.unsigned(
        Form::Shareable,
        Issuer::Ed25519(sk.verifying_key().to_bytes()),
        draft.inputs.clone(),
    )?;
    let (block, next) = auth::share_first(&sk, &p.encode_base()).map_err(IssueError::NoKey)?;
    p.tag = Tag::Shareable {
        blocks: vec![block],
        proof: Proof::Key(next.to_bytes()),
    };
    Ok(p)
}

/// The same pass, issued again as a shareable one with the same caveats:
/// what `cig pass export` sends. The caller has verified it. Its inputs are
/// read again for their plain digests (a local pass carries keyed ones),
/// and each must still match the keyed one recorded. Refused for a kind
/// that never leaves this machine, a pass with `once`, `max` or `needs`
/// (spent and discharged here), and a pass that read a variable (its value
/// would travel as a digest anyone could test guesses against).
pub fn reissue_shareable(
    pass: &Pass,
    signing_seed: &[u8; 32],
    hash_key: &[u8; 32],
) -> Result<Pass, IssueError> {
    let pol = policy(&pass.kind)
        .ok_or_else(|| IssueError::Invalid(format!("`{}` is not a kind of pass", pass.kind)))?;
    if !pol.shareable || !pol.foreign {
        return Err(IssueError::Invalid(format!(
            "a {} pass never leaves the machine that made it",
            pass.kind
        )));
    }
    if pass
        .caveats
        .iter()
        .any(|c| matches!(c, Caveat::Once | Caveat::Max(_) | Caveat::Needs(_)))
    {
        return Err(IssueError::Invalid(
            "a pass with `once`, `max` or `needs` is spent and discharged on this machine and cannot be exported"
                .to_string(),
        ));
    }
    let mut inputs = Vec::with_capacity(pass.inputs.len());
    for i in &pass.inputs {
        inputs.push(match i {
            Input::Env { name, value } if name.as_slice() == b":cwd" => {
                // The working folder is a path, not a secret: it travels as
                // the plain digest of the folder it was, checked against
                // the keyed one first.
                let now = std::env::current_dir()
                    .ok()
                    .and_then(|c| fsid::path_bytes(&c))
                    .map(|b| fsid::env_digest(&b));
                match (now, value) {
                    (Some(plain), Some(k)) if keyed(hash_key, &plain) == *k => Input::Env {
                        name: name.clone(),
                        value: Some(plain),
                    },
                    _ => return Err(IssueError::Changed("the working folder".to_string())),
                }
            }
            Input::Env { name, .. } => {
                return Err(IssueError::Invalid(format!(
                "it depends on ${}, whose value would travel as a digest: it cannot be exported",
                fsid::clean(&String::from_utf8_lossy(name))
            )))
            }
            Input::File {
                path,
                identity,
                racy,
                blake3,
            } => {
                let p = fsid::bytes_path(path).ok_or_else(|| {
                    IssueError::Invalid(format!("{} cannot be a path here", fsid::shown(path)))
                })?;
                let h = fsid::hash_regular(&p, u64::MAX)
                    .map_err(|e| IssueError::Changed(format!("{} ({e})", fsid::shown(path))))?;
                if keyed(hash_key, &h.digest) != *blake3 {
                    return Err(IssueError::Changed(fsid::shown(path)));
                }
                Input::File {
                    path: path.clone(),
                    identity: *identity,
                    racy: *racy,
                    blake3: h.digest,
                }
            }
            Input::Dir { path, listing } => {
                let p = fsid::bytes_path(path).ok_or_else(|| {
                    IssueError::Invalid(format!("{} cannot be a path here", fsid::shown(path)))
                })?;
                let entries = fsid::list_dir(&p)
                    .map_err(|e| IssueError::Changed(format!("{} ({e})", fsid::shown(path))))?;
                let plain = fsid::listing_digest(&entries);
                if keyed(hash_key, &plain) != *listing {
                    return Err(IssueError::Changed(fsid::shown(path)));
                }
                Input::Dir {
                    path: path.clone(),
                    listing: plain,
                }
            }
            other => other.clone(),
        });
    }
    let sk = auth::signing_key(signing_seed);
    let mut p = pass.clone();
    p.form = Form::Shareable;
    p.issuer = Issuer::Ed25519(sk.verifying_key().to_bytes());
    p.inputs = inputs;
    p.caveats = Vec::new();
    let (block, next) = auth::share_first(&sk, &p.encode_base()).map_err(IssueError::NoKey)?;
    p.tag = Tag::Shareable {
        blocks: vec![block],
        proof: Proof::Key(next.to_bytes()),
    };
    for c in &pass.caveats {
        p = auth::attenuate(&p, c.clone()).map_err(IssueError::Invalid)?;
    }
    if p.encode().len() > MAX_PASS {
        return Err(IssueError::TooBig(
            "the shareable pass would be larger than 4 MiB".to_string(),
        ));
    }
    Ok(p)
}
