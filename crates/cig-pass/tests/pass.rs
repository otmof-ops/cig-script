// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for `cig-pass`: tokens, caveats, attenuation, epochs,
//! revocation, discharges, shareable passes and every kind of recorded input.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
#[allow(unused_imports)]
use std::time::{Duration, SystemTime};

use cig_pass::{
    attenuate, verify, Caveat, Draft, EntryType, Input, IssueError, MemCtx, Mono, Now, Pass,
    PresentKind, Recorder, Why,
};

// ----- adapter: the only place that knows how contexts and keys are built ----

mod adapter {
    use super::*;

    /// The kinds, as the tests name them.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Kind {
        Check,
        Run,
        Verdict,
        Approval,
        Program,
        HumanYes,
    }

    impl Kind {
        pub fn as_str(self) -> &'static str {
            match self {
                Kind::Check => "check",
                Kind::Run => "run",
                Kind::Verdict => "verdict",
                Kind::Approval => "approval",
                Kind::Program => "program",
                Kind::HumanYes => "human-yes",
            }
        }
    }

    pub type Ctx = MemCtx;

    pub const ISSUER: [u8; 16] = [9; 16];

    fn key_for(kind: &str, epoch: u64) -> [u8; 32] {
        blake3::derive_key("cig-pass tests", format!("{kind}:{epoch}").as_bytes())
    }

    pub fn local_key(kind: Kind, epoch: u64) -> [u8; 32] {
        key_for(kind.as_str(), epoch)
    }

    /// The digest key `MemCtx::with_local` derives from the same key function.
    pub fn hash_key() -> [u8; 32] {
        *blake3::keyed_hash(&key_for("check", u64::MAX), b"cigpass1-hash-key").as_bytes()
    }

    /// A context with the test's local issuer and keys, epoch 0 for every
    /// kind, the real clock and the real environment.
    pub fn ctx() -> Ctx {
        let mut c = MemCtx::with_local(ISSUER, |k, e| Some(key_for(k, e)));
        c.env_from_process = true;
        c
    }

    pub fn ctx_without_keys() -> Ctx {
        let mut c = MemCtx::with_local(ISSUER, |_, _| None);
        c.env_from_process = true;
        c
    }

    pub trait CtxExt {
        fn set_local_issuer(&mut self, issuer: [u8; 16]);
        fn add_known_pass(&mut self, p: Pass);
    }

    impl CtxExt for MemCtx {
        fn set_local_issuer(&mut self, issuer: [u8; 16]) {
            self.issuer = Some(issuer);
        }
        fn add_known_pass(&mut self, p: Pass) {
            self.add_pass(p);
        }
    }

    pub fn set_env(c: &mut Ctx, name: &str, value: Option<&str>) {
        c.set_env(name.as_bytes(), value.map(|v| v.as_bytes()));
    }
    pub fn set_now(c: &mut Ctx, wall_ms: i64, mono: Option<Mono>) {
        c.now = Some(Now { wall_ms, mono });
    }
    pub fn set_epoch(c: &mut Ctx, kind: Kind, epoch: u64) {
        c.set_epoch(kind.as_str(), epoch);
    }
    pub fn revoke(c: &mut Ctx, id: [u8; 32]) {
        c.revoke(id);
    }
    pub fn allow_signer(c: &mut Ctx, pk: [u8; 32]) {
        c.allow_signer(pk);
    }
    pub fn add_discharge(c: &mut Ctx, p: Pass) {
        c.add_discharge(p);
    }
    pub fn set_boxed(c: &mut Ctx, boxed: bool) {
        c.action.boxed = boxed;
    }
    pub fn set_agent(c: &mut Ctx, agent: &str) {
        c.action.agent = Some(agent.to_string());
    }

    /// The keys a test issues with: local (issuer, subkey, epoch) or a
    /// shareable signing seed, optionally at a fixed time.
    #[derive(Clone)]
    pub struct IssueKeys {
        local: Option<([u8; 16], [u8; 32])>,
        seed: Option<[u8; 32]>,
        epoch: u64,
        at: Option<(i64, Option<Mono>)>,
    }

    pub fn keys_local(kind: Kind, epoch: u64) -> IssueKeys {
        IssueKeys {
            local: Some((ISSUER, local_key(kind, epoch))),
            seed: None,
            epoch,
            at: None,
        }
    }
    pub fn keys_shareable(seed: [u8; 32]) -> IssueKeys {
        IssueKeys {
            local: None,
            seed: Some(seed),
            epoch: 0,
            at: None,
        }
    }
    pub fn at(mut keys: IssueKeys, issued_ms: i64, mono: Option<Mono>) -> IssueKeys {
        keys.at = Some((issued_ms, mono));
        keys
    }
    pub fn mono(boot: [u8; 16], ns: u64) -> Mono {
        Mono { boot_id: boot, ns }
    }

    pub fn issue(
        kind: Kind,
        subject: &[u8],
        inputs: Vec<Input>,
        result: Vec<u8>,
        keys: &IssueKeys,
    ) -> Result<Pass, IssueError> {
        let (issued_ms, mono) = keys
            .at
            .unwrap_or_else(|| (cig_pass::wall_ms(), cig_pass::mono_now()));
        let draft = Draft {
            kind: kind.as_str().to_string(),
            subject: subject.to_vec(),
            inputs,
            result,
            epoch: keys.epoch,
            ledger_seq: 0,
            issued_ms,
            mono,
            flags: 0,
        };
        match (keys.local, keys.seed) {
            (Some((issuer, key)), _) => cig_pass::issue_local(&draft, issuer, &key, &hash_key()),
            (None, Some(seed)) => cig_pass::issue_shareable(&draft, &seed),
            (None, None) => unreachable!("keys are local or shareable"),
        }
    }

    pub fn encode(p: &Pass) -> Vec<u8> {
        p.encode()
    }
    pub fn decode(b: &[u8]) -> Result<Pass, cig_pass::DecodeError> {
        Pass::decode(b)
    }
    pub fn token(p: &Pass) -> String {
        p.token()
    }
    pub fn from_token(t: &str) -> Result<Pass, cig_pass::DecodeError> {
        Pass::from_token(t)
    }
    pub fn root_id(p: &Pass) -> [u8; 32] {
        p.root_id()
    }
    pub fn seal(p: &Pass) -> Pass {
        cig_pass::seal(p).expect("seal")
    }
    /// Spend: verify, then record the spend keys into the ctx.
    pub fn spend(p: &Pass, c: &mut Ctx) -> Result<(), Why> {
        c.spend_pass(p).map(|_| ())
    }
    pub fn signer_of(seed: [u8; 32]) -> [u8; 32] {
        ed25519_dalek::SigningKey::from_bytes(&seed)
            .verifying_key()
            .to_bytes()
    }
}
use adapter::*;

// ----- helpers ----------------------------------------------------------------

const BOOT_A: [u8; 16] = [1; 16];
const BOOT_B: [u8; 16] = [2; 16];

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn why(r: Result<impl std::fmt::Debug, Why>) -> String {
    match r {
        Ok(v) => panic!("expected the pass to be invalid, got Ok({v:?})"),
        Err(w) => w.to_string(),
    }
}

fn mk(kind: Kind, subject: &[u8], rec: Recorder, result: &[u8], keys: &IssueKeys) -> Pass {
    let inputs = rec.finish().expect("inputs");
    issue(kind, subject, inputs, result.to_vec(), keys).expect("issue")
}

/// Read a file the way a recording `fs.read_text` does: identity from the
/// handle that was read, bytes hashed as read.
fn read_file(rec: &mut Recorder, path: &Path) -> Vec<u8> {
    let mut f = File::open(path).unwrap();
    let meta = f.metadata().unwrap();
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes).unwrap();
    rec.file_read(path, &meta, &bytes);
    bytes
}

fn list_dir(rec: &mut Recorder, path: &Path) {
    let mut entries: Vec<(Vec<u8>, EntryType)> = fs::read_dir(path)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            let t = e.file_type().unwrap();
            let ty = if t.is_dir() {
                EntryType::Dir
            } else if t.is_file() {
                EntryType::File
            } else {
                EntryType::Other
            };
            (e.file_name().to_string_lossy().as_bytes().to_vec(), ty)
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    rec.dir_listed(path, &entries);
}

/// A check pass over one file, issued now with the local keys.
fn pass_over_file(path: &Path) -> Pass {
    let mut rec = Recorder::new();
    read_file(&mut rec, path);
    mk(
        Kind::Check,
        b"subject",
        rec,
        b"ok",
        &keys_local(Kind::Check, 0),
    )
}

fn bare(kind: Kind) -> Pass {
    mk(kind, b"bare", Recorder::new(), b"r", &keys_local(kind, 0))
}

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

// ----- token and subject --------------------------------------------------------

#[test]
fn token_roundtrip() {
    let d = tmp();
    let f = d.path().join("a.txt");
    fs::write(&f, "alpha").unwrap();
    let p = attenuate(&pass_over_file(&f), Caveat::Max(3)).unwrap();

    let t = token(&p);
    assert!(t.starts_with("cigpass1."), "{t}");
    let body = &t["cigpass1.".len()..];
    assert!(
        body.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "base64url, no padding, no standard alphabet: {t}"
    );
    assert_eq!(from_token(&t).unwrap(), p);
    assert_eq!(decode(&encode(&p)).unwrap(), p);
    assert_eq!(encode(&decode(&encode(&p)).unwrap()), encode(&p));
    // A token that went through text still verifies.
    verify(&from_token(&t).unwrap(), &ctx()).expect("still valid");
    // Wrong prefix and damaged bodies are refused, not guessed at.
    assert!(from_token(&t.replacen("cigpass1.", "cigpass2.", 1)).is_err());
    assert!(from_token(&t[..t.len() - 3]).is_err());
    assert!(from_token("").is_err());
}

#[test]
fn canonical_subject() {
    let keys = keys_local(Kind::Check, 0);
    let a = mk(Kind::Check, b"{\"a\":1}", Recorder::new(), b"r", &keys);
    let b = mk(Kind::Check, b"{\"a\":1}", Recorder::new(), b"r", &keys);
    let c = mk(Kind::Check, b"{\"a\":2}", Recorder::new(), b"r", &keys);
    assert_eq!(a.subject, b.subject);
    assert_ne!(a.subject, c.subject);
    let mut h = blake3::Hasher::new();
    h.update(b"cigpass1-subject");
    h.update(b"{\"a\":1}");
    assert_eq!(
        a.subject,
        *h.finalize().as_bytes(),
        "BLAKE3(label || bytes)"
    );
    // The same bytes under another kind are the same subject: the kind is
    // part of the key, not of the subject.
    let d = mk(
        Kind::Run,
        b"{\"a\":1}",
        Recorder::new(),
        b"r",
        &keys_local(Kind::Run, 0),
    );
    assert_eq!(a.subject, d.subject);
}

// ----- decoding is strict ------------------------------------------------------

#[test]
fn unknown_caveat_is_invalid() {
    let p = attenuate(&bare(Kind::Check), Caveat::Once).unwrap();
    let mut bytes = encode(&p);
    // Local pass tail: caveat (1 byte for `once`) then a 32-byte tag.
    let at = bytes.len() - 32 - 1;
    assert_eq!(bytes[at], 9, "once is caveat tag 9");
    bytes[at] = 99;
    assert!(decode(&bytes).is_err(), "an unknown caveat must not decode");
    bytes[at] = 0;
    assert!(decode(&bytes).is_err());
}

#[test]
fn unknown_version_form_kind_input_and_trailing_bytes_are_invalid() {
    let d = tmp();
    let f = d.path().join("a.txt");
    fs::write(&f, "x").unwrap();
    let good = encode(&pass_over_file(&f));
    assert!(decode(&good).is_ok());

    let mut v = good.clone();
    v[7] = 2; // version
    assert!(decode(&v).is_err());

    let mut v = good.clone();
    v[8] = 2; // form
    assert!(decode(&v).is_err());

    let mut v = good.clone();
    v[0] = b'X'; // magic
    assert!(decode(&v).is_err());

    // Kind string starts at 9 (u16 len, then bytes): corrupt a letter.
    let mut v = good.clone();
    v[9 + 2] = b'#';
    assert!(decode(&v).is_err(), "kinds are a fixed list");

    // First input tag: after kind (2 + len), flags (1), subject (32) and u16 count.
    let klen = u16::from_le_bytes([good[9], good[10]]) as usize;
    let tag_at = 9 + 2 + klen + 1 + 32 + 2;
    let mut v = good.clone();
    v[9 + 2 + klen] = 0x80;
    assert!(decode(&v).is_err(), "unknown flags are refused");
    assert_eq!(good[tag_at], 1, "a file read is input tag 1");
    let mut v = good.clone();
    v[tag_at] = 77;
    assert!(decode(&v).is_err());

    let mut v = good.clone();
    v.push(0);
    assert!(decode(&v).is_err(), "no trailing bytes");
}

#[test]
fn every_truncation_is_refused_and_none_panics() {
    let d = tmp();
    let f = d.path().join("a.txt");
    fs::write(&f, "x").unwrap();
    let p = attenuate(&pass_over_file(&f), Caveat::Until(now_ms() + 60_000)).unwrap();
    let bytes = encode(&p);
    for n in 0..bytes.len() {
        assert!(decode(&bytes[..n]).is_err(), "prefix of {n} bytes decoded");
    }
}

#[test]
fn every_single_byte_flip_never_verifies_as_the_original() {
    // A flipped byte is either a decode error or a pass that does not verify
    // (the tag covers the base and every caveat).
    let d = tmp();
    let f = d.path().join("a.txt");
    fs::write(&f, "x").unwrap();
    let p = attenuate(&pass_over_file(&f), Caveat::Max(4)).unwrap();
    let bytes = encode(&p);
    let c = ctx();
    verify(&p, &c).expect("the original is valid");
    for i in 0..bytes.len() {
        let mut v = bytes.clone();
        v[i] ^= 0x01;
        if let Ok(q) = decode(&v) {
            if q == p {
                continue; // a byte with no meaning (none is expected)
            }
            assert!(verify(&q, &c).is_err(), "flip at byte {i} still verified");
        }
    }
}

// ----- attenuation ---------------------------------------------------------------

#[test]
fn attenuate_without_key() {
    // Narrowing needs the pass and nothing else: no ctx, no key.
    let p = bare(Kind::Check);
    let n = attenuate(&p, Caveat::Boxed).unwrap();
    assert_eq!(n.caveats.len(), 1);
    // The verifier that holds the key accepts it, once the caveat holds.
    let mut c = ctx();
    set_boxed(&mut c, true);
    verify(&n, &c).expect("narrowed pass is valid for the boxed action");
    // A verifier without the key cannot say yes to anything.
    assert!(matches!(verify(&n, &ctx_without_keys()), Err(Why::NoKey)));
    // The caveat narrows: an unboxed action is refused.
    set_boxed(&mut c, false);
    assert!(why(verify(&n, &c)).contains("box"), "names the caveat");
    // The original is unchanged and still broader.
    verify(&p, &c).expect("original");
}

#[test]
fn a_forged_extension_without_the_chain_is_invalid() {
    // Someone who has the bytes but not the key appends a caveat by hand
    // (here: the real chain step is skipped by reusing the old tag).
    let p = bare(Kind::Check);
    let mut forged = attenuate(&p, Caveat::Max(1)).unwrap();
    forged.tag = p.tag.clone();
    assert!(verify(&forged, &ctx()).is_err());
}

#[test]
fn cannot_remove_a_caveat() {
    let base = attenuate(
        &attenuate(
            &attenuate(&bare(Kind::Check), Caveat::Max(5)).unwrap(),
            Caveat::Agent("hivemind".into()),
        )
        .unwrap(),
        Caveat::Until(now_ms() + 3_600_000),
    )
    .unwrap();
    let mut c = ctx();
    set_agent(&mut c, "hivemind");
    verify(&base, &c).expect("the real thing is valid");

    // Drop each caveat in turn.
    for i in 0..3 {
        let mut p = base.clone();
        p.caveats.remove(i);
        assert!(verify(&p, &c).is_err(), "dropping caveat {i} verified");
        assert!(decode(&encode(&p)).map_or(true, |q| verify(&q, &c).is_err()));
    }
    // Drop the last one: "the same pass, less narrow".
    let mut p = base.clone();
    p.caveats.pop();
    assert!(verify(&p, &c).is_err());
    // Drop all.
    let mut p = base.clone();
    p.caveats.clear();
    assert!(verify(&p, &c).is_err());
    // Reorder (the chain is order-sensitive).
    let mut p = base.clone();
    p.caveats.swap(0, 2);
    assert!(verify(&p, &c).is_err(), "reordering verified");
    let mut p = base.clone();
    p.caveats.swap(0, 1);
    assert!(verify(&p, &c).is_err());
    // Alter one in place: widen the max, widen the until, change the agent.
    let mut p = base.clone();
    p.caveats[0] = Caveat::Max(500);
    assert!(verify(&p, &c).is_err(), "altering max verified");
    let mut p = base.clone();
    p.caveats[2] = Caveat::Until(now_ms() + 3_600_000_000);
    assert!(verify(&p, &c).is_err(), "altering until verified");
    let mut p = base.clone();
    p.caveats[1] = Caveat::Agent("anyone".into());
    set_agent(&mut c, "anyone");
    assert!(verify(&p, &c).is_err(), "altering agent verified");
}

#[test]
fn cannot_remove_a_caveat_from_a_shareable_pass() {
    let seed = [5u8; 32];
    let p = mk(
        Kind::Check,
        b"s",
        Recorder::new(),
        b"r",
        &keys_shareable(seed),
    );
    let n = attenuate(&attenuate(&p, Caveat::Max(2)).unwrap(), Caveat::Boxed).unwrap();
    let mut c = ctx();
    allow_signer(&mut c, signer_of(seed));
    set_boxed(&mut c, true);
    verify(&n, &c).expect("valid");
    let mut q = n.clone();
    q.caveats.remove(0);
    assert!(verify(&q, &c).is_err());
    let mut q = n.clone();
    q.caveats.swap(0, 1);
    assert!(verify(&q, &c).is_err());
    let mut q = n.clone();
    q.caveats[0] = Caveat::Max(200);
    assert!(verify(&q, &c).is_err());
}

// ----- spending -----------------------------------------------------------------

#[test]
fn narrowed_once_cannot_be_respent() {
    let once = attenuate(&bare(Kind::Check), Caveat::Once).unwrap();
    let mut c = ctx();
    spend(&once, &mut c).expect("first spend");
    assert!(why(spend(&once, &mut c)).contains("spent"));
    // Appending a caveat after `once` keeps the spend key: the narrowing is
    // spent too, not a fresh pass.
    let narrowed = attenuate(&once, Caveat::Until(now_ms() + 3_600_000)).unwrap();
    assert!(why(spend(&narrowed, &mut c)).contains("spent"));
    let narrower = attenuate(&narrowed, Caveat::Boxed).unwrap();
    set_boxed(&mut c, true);
    assert!(why(spend(&narrower, &mut c)).contains("spent"));
    // verify alone never writes: a fresh ctx has spent nothing.
    verify(&narrowed, &ctx()).expect("a fresh ledger has no spend");
}

#[test]
fn max_n_allows_exactly_n() {
    let p = attenuate(&bare(Kind::Check), Caveat::Max(3)).unwrap();
    let mut c = ctx();
    for _ in 0..3 {
        spend(&p, &mut c).unwrap();
    }
    assert!(why(spend(&p, &mut c)).contains("spent"));
}

#[test]
fn verify_alone_never_spends() {
    let p = attenuate(&bare(Kind::Check), Caveat::Once).unwrap();
    let c = ctx();
    for _ in 0..5 {
        verify(&p, &c).expect("verify is a read");
    }
}

// ----- revocation, epochs --------------------------------------------------------

#[test]
fn revoked_root_kills_narrowings() {
    let root = bare(Kind::Check);
    let n1 = attenuate(&root, Caveat::Max(9)).unwrap();
    let n2 = attenuate(&n1, Caveat::Boxed).unwrap();
    let mut c = ctx();
    set_boxed(&mut c, true);
    verify(&n2, &c).expect("valid before revocation");
    revoke(&mut c, root_id(&root));
    assert!(why(verify(&root, &c)).contains("revoked"));
    assert!(why(verify(&n1, &c)).contains("revoked"));
    assert!(why(verify(&n2, &c)).contains("revoked"));
}

#[test]
fn revoking_a_narrowing_leaves_the_root_alone() {
    let root = bare(Kind::Check);
    let n1 = attenuate(&root, Caveat::Max(9)).unwrap();
    let n2 = attenuate(&n1, Caveat::Boxed).unwrap();
    let mut c = ctx();
    set_boxed(&mut c, true);
    // The id of a narrowing is the id of its prefix chain: revoke the
    // two-caveat pass by its verified id.
    let id2 = verify(&n2, &c).unwrap().id;
    revoke(&mut c, id2);
    assert!(why(verify(&n2, &c)).contains("revoked"));
    verify(&root, &c).expect("the root is not revoked by its narrowing");
    verify(&n1, &c).expect("the shorter narrowing is not revoked");
}

#[test]
fn epoch_kills_kind() {
    let verdict = bare(Kind::Verdict);
    let approval = bare(Kind::Approval);
    let mut c = ctx();
    verify(&verdict, &c).unwrap();
    verify(&approval, &c).unwrap();
    // `cig pass revoke --kind verdict` bumps one epoch.
    set_epoch(&mut c, Kind::Verdict, 1);
    let w = why(verify(&verdict, &c));
    assert!(w.contains("epoch"), "{w}");
    verify(&approval, &c).expect("another kind is untouched");
    // Passes issued at the new epoch are good again.
    let again = mk(
        Kind::Verdict,
        b"bare",
        Recorder::new(),
        b"r",
        &keys_local(Kind::Verdict, 1),
    );
    verify(&again, &c).expect("issued at the new epoch");
    // An old pass is still refused after a second bump.
    set_epoch(&mut c, Kind::Verdict, 2);
    assert!(verify(&again, &c).is_err());
}

#[test]
fn another_issuer_is_refused() {
    let p = bare(Kind::Check);
    let mut c = ctx();
    c.set_local_issuer([8; 16]);
    let w = why(verify(&p, &c));
    assert!(w.contains("key") || w.contains("issued"), "{w}");
}

// ----- shareable -----------------------------------------------------------------

#[test]
fn shareable_verifies_elsewhere() {
    let seed = [11u8; 32];
    let p = mk(
        Kind::Program,
        b"prog",
        Recorder::new(),
        b"r",
        &keys_shareable(seed),
    );
    // Machine B: different local keys entirely, but the signer is allowed.
    let mut b = ctx_without_keys();
    allow_signer(&mut b, signer_of(seed));
    let v = verify(&p, &b).expect("verifies with the signer allowed");
    assert_eq!(v.root_id, root_id(&p));
    // Machine C: same everything, the signer is not allowed.
    let c = ctx_without_keys();
    assert!(matches!(verify(&p, &c), Err(Why::UnknownSigner)));
    // Machine D: allows somebody else.
    let mut d = ctx_without_keys();
    allow_signer(&mut d, signer_of([12u8; 32]));
    assert!(matches!(verify(&p, &d), Err(Why::UnknownSigner)));
    // It survives the token.
    let q = from_token(&token(&p)).unwrap();
    verify(&q, &b).expect("token roundtrip");
}

#[test]
fn shareable_attenuates_and_still_verifies() {
    let seed = [13u8; 32];
    let p = mk(
        Kind::Check,
        b"s",
        Recorder::new(),
        b"r",
        &keys_shareable(seed),
    );
    let n = attenuate(&p, Caveat::Boxed).unwrap();
    let mut b = ctx_without_keys();
    allow_signer(&mut b, signer_of(seed));
    set_boxed(&mut b, true);
    verify(&n, &b).unwrap();
    set_boxed(&mut b, false);
    assert!(verify(&n, &b).is_err(), "the caveat holds on machine B");
}

#[test]
fn sealed_shareable_cannot_narrow() {
    let seed = [14u8; 32];
    let p = mk(
        Kind::Check,
        b"s",
        Recorder::new(),
        b"r",
        &keys_shareable(seed),
    );
    let n = attenuate(&p, Caveat::Max(3)).unwrap();
    let sealed = seal(&n);
    let mut b = ctx_without_keys();
    allow_signer(&mut b, signer_of(seed));
    verify(&sealed, &b).expect("a sealed pass still verifies");
    assert!(
        attenuate(&sealed, Caveat::Once).is_err(),
        "sealed: no more narrowing"
    );
    // The sealed bytes do not carry the key: the token is smaller proof and
    // contains no 32-byte private key that could re-open it.
    let again = from_token(&token(&sealed)).unwrap();
    assert!(attenuate(&again, Caveat::Once).is_err());
    verify(&again, &b).unwrap();
}

// ----- discharges ----------------------------------------------------------------

#[test]
fn discharge_required() {
    let p = attenuate(
        &bare(Kind::Approval),
        Caveat::Needs("human-yes".to_string()),
    )
    .unwrap();
    let mut c = ctx();
    let w = why(verify(&p, &c));
    assert!(w.contains("human-yes"), "names the missing discharge: {w}");

    // A person's yes comes from an authority key, shared, short-lived.
    let authority = [42u8; 32];
    let yes = |subject: &[u8], life_ms: i64| -> Pass {
        let now = now_ms();
        let d = mk(
            Kind::HumanYes,
            subject,
            Recorder::new(),
            b"y",
            &at(keys_shareable(authority), now, None),
        );
        attenuate(&d, Caveat::Until(now + life_ms)).unwrap()
    };

    // A discharge for another pass does not do.
    let other = yes(b"not-this-pass", 600_000);
    add_discharge(&mut c, other);
    c.allow_authority(signer_of(authority));
    assert!(verify(&p, &c).is_err());

    // A local human-yes cannot be made at all, and one signed by a key that
    // is not an authority does not count.
    assert!(issue(
        Kind::HumanYes,
        &root_id(&p),
        vec![],
        b"y".to_vec(),
        &keys_local(Kind::HumanYes, 0)
    )
    .is_err());
    let mut c2 = ctx();
    c2.allow_signer(signer_of(authority));
    add_discharge(&mut c2, yes(&root_id(&p), 600_000));
    assert!(
        verify(&p, &c2).is_err(),
        "an allowed signer is not an authority"
    );

    // One that lives longer than an hour is refused.
    let mut c3 = ctx();
    c3.allow_authority(signer_of(authority));
    add_discharge(&mut c3, yes(&root_id(&p), 2 * 3_600_000));
    assert!(
        verify(&p, &c3).is_err(),
        "a yes must expire within the hour"
    );

    // The right discharge: subject is this pass's root id.
    let good = yes(&root_id(&p), 600_000);
    add_discharge(&mut c, good.clone());
    verify(&p, &c).expect("discharged");

    // A narrowing of the pass keeps the same root id, so the same discharge.
    let n = attenuate(&p, Caveat::Max(4)).unwrap();
    verify(&n, &c).expect("narrowing is discharged by the same yes");

    // The discharge must itself verify: an authority no longer trusted
    // here, and the pass fails.
    c.authorities.clear();
    assert!(
        verify(&p, &c).is_err(),
        "a discharge that no longer verifies"
    );
}

// ----- inputs: files --------------------------------------------------------------

#[test]
fn file_input_invalidates_on_change() {
    let d = tmp();
    let f = d.path().join("deploy.sh");
    fs::write(&f, "echo 120 bytes").unwrap();
    let p = pass_over_file(&f);
    let c = ctx();
    verify(&p, &c).expect("unchanged");
    fs::write(&f, "echo changed, longer").unwrap();
    let w = why(verify(&p, &c));
    assert!(w.contains("deploy.sh"), "names the file: {w}");
    assert!(w.contains("changed"), "{w}");
    // Put the bytes back: the identity moved but the hash is equal.
    fs::write(&f, "echo 120 bytes").unwrap();
    verify(&p, &c).expect("same bytes again");
    // Gone.
    fs::remove_file(&f).unwrap();
    assert!(verify(&p, &c).is_err());
}

#[cfg(unix)]
#[test]
fn same_size_mtime_restored_edit() {
    let d = tmp();
    let f = d.path().join("a.txt");
    fs::write(&f, "aaaa").unwrap();
    // Make the file old, so the racy rule cannot be what catches this.
    let old = SystemTime::now() - Duration::from_secs(3600);
    File::options()
        .write(true)
        .open(&f)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let mut rec = Recorder::new();
    read_file(&mut rec, &f);
    let keys = at(keys_local(Kind::Check, 0), now_ms(), None);
    let p = mk(Kind::Check, b"s", rec, b"r", &keys);
    verify(&p, &ctx()).expect("unchanged");

    // Same size, mtime put back to the second: only ctime (and the bytes)
    // give it away.
    fs::write(&f, "bbbb").unwrap();
    File::options()
        .write(true)
        .open(&f)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let m = fs::metadata(&f).unwrap();
    assert_eq!(m.len(), 4);
    assert_eq!(m.modified().unwrap(), old, "mtime restored");
    assert!(
        verify(&p, &ctx()).is_err(),
        "an edit that restores size and mtime is still an edit"
    );
}

#[cfg(unix)]
#[test]
fn racy_mtime_hashes() {
    // The recorder is handed the bytes the work read; here they differ from
    // what is on disk, while the handle's metadata is exactly what is on disk.
    // That is the state a same-second edit leaves behind: identity matches,
    // contents do not. A racy file must be hashed at verify and refused.
    let d = tmp();
    let f = d.path().join("race.txt");
    fs::write(&f, "bbbb").unwrap();
    let mtime = fs::metadata(&f).unwrap().modified().unwrap();
    let mtime_ms = mtime
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;

    let record_stale = |issued_ms: i64| -> Pass {
        let mut rec = Recorder::new();
        let h = File::open(&f).unwrap();
        rec.file_read_at(&f, &h.metadata().unwrap(), b"aaaa", issued_ms);
        let keys = at(keys_local(Kind::Check, 0), issued_ms, None);
        mk(Kind::Check, b"s", rec, b"r", &keys)
    };

    // Racy: mtime within 2 s of issue (here 500 ms after it was written).
    let racy = record_stale(mtime_ms + 500);
    assert!(
        verify(&racy, &ctx()).is_err(),
        "a racy file is hashed even when its stat matches"
    );
    // Racy: mtime later than issue.
    let future = record_stale(mtime_ms - 5_000);
    assert!(verify(&future, &ctx()).is_err());
    // Control: not racy (issued an hour after the mtime) and the stat
    // matches, so the stat is trusted: the stat cache's contract, and the
    // reason the racy rule exists. Only where identity is trusted at all:
    // on a file system that is not a known local disk one (a container's
    // overlay) every file is hashed.
    let calm = record_stale(mtime_ms + 3_600_000);
    if !cig_pass::always_hash(&f) {
        verify(&calm, &ctx()).expect("identity equal and not racy trusts the stat");
    } else {
        assert!(
            verify(&calm, &ctx()).is_err(),
            "hashed where identity is not trusted"
        );
    }
}

#[test]
fn file_input_boundary_is_two_seconds() {
    let d = tmp();
    let f = d.path().join("b.txt");
    fs::write(&f, "bbbb").unwrap();
    let mtime_ms = fs::metadata(&f)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let stale = |issued| {
        let mut rec = Recorder::new();
        let h = File::open(&f).unwrap();
        rec.file_read_at(&f, &h.metadata().unwrap(), b"aaaa", issued);
        mk(
            Kind::Check,
            b"s",
            rec,
            b"r",
            &at(keys_local(Kind::Check, 0), issued, None),
        )
    };
    assert!(
        verify(&stale(mtime_ms + 1_999), &ctx()).is_err(),
        "1.999 s is racy"
    );
    #[cfg(unix)]
    if !cig_pass::always_hash(&f) {
        verify(&stale(mtime_ms + 2_500), &ctx()).expect("2.5 s is not");
    }
}

#[test]
fn file_seen_hashes_the_file() {
    let d = tmp();
    let f = d.path().join("seen.txt");
    fs::write(&f, "one").unwrap();
    let mut rec = Recorder::new();
    rec.file_seen(&f).unwrap();
    let p = mk(Kind::Check, b"s", rec, b"r", &keys_local(Kind::Check, 0));
    verify(&p, &ctx()).unwrap();
    fs::write(&f, "two").unwrap();
    assert!(verify(&p, &ctx()).is_err());
    // A path that is not there is recorded absent: the pass holds until it appears.
    let nope = d.path().join("nope");
    let mut rec = Recorder::new();
    rec.file_seen(&nope).unwrap();
    let p = mk(Kind::Check, b"s", rec, b"r", &keys_local(Kind::Check, 0));
    verify(&p, &ctx()).unwrap();
    fs::write(&nope, "here").unwrap();
    assert!(why(verify(&p, &ctx())).contains("appeared"));
}

#[test]
fn absent_input_invalidates() {
    let d = tmp();
    let f = d.path().join(".npmrc");
    let mut rec = Recorder::new();
    rec.absent(&f);
    let p = mk(Kind::Check, b"s", rec, b"r", &keys_local(Kind::Check, 0));
    verify(&p, &ctx()).expect("still absent");
    fs::write(&f, "").unwrap();
    let w = why(verify(&p, &ctx()));
    assert!(w.contains(".npmrc") && w.contains("appeared"), "{w}");
    // A dangling symlink is not absent for `symlink_metadata`.
    #[cfg(unix)]
    {
        fs::remove_file(&f).unwrap();
        std::os::unix::fs::symlink(d.path().join("nowhere"), &f).unwrap();
        let mut rec = Recorder::new();
        rec.absent(&d.path().join("other"));
        let q = mk(Kind::Check, b"s", rec, b"r", &keys_local(Kind::Check, 0));
        verify(&q, &ctx()).unwrap();
    }
}

#[test]
fn dir_input_invalidates() {
    let d = tmp();
    let dir = d.path().join("src");
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("a.rs"), "a").unwrap();
    fs::write(dir.join("b.rs"), "b").unwrap();
    let mut rec = Recorder::new();
    list_dir(&mut rec, &dir);
    let p = mk(Kind::Check, b"s", rec, b"r", &keys_local(Kind::Check, 0));
    verify(&p, &ctx()).expect("unchanged");

    // Contents of a listed file are not part of a listing.
    fs::write(dir.join("a.rs"), "changed").unwrap();
    verify(&p, &ctx()).expect("a listing is names and types only");

    // A new entry.
    fs::write(dir.join("c.rs"), "c").unwrap();
    assert!(verify(&p, &ctx()).is_err(), "an entry appeared");
    fs::remove_file(dir.join("c.rs")).unwrap();
    verify(&p, &ctx()).expect("back to the same listing");
    // A removed entry.
    fs::remove_file(dir.join("b.rs")).unwrap();
    assert!(verify(&p, &ctx()).is_err());
    fs::write(dir.join("b.rs"), "b").unwrap();
    // A type change: same name, now a folder.
    fs::remove_file(dir.join("b.rs")).unwrap();
    fs::create_dir(dir.join("b.rs")).unwrap();
    assert!(verify(&p, &ctx()).is_err(), "a name that changed type");
    // The folder itself gone.
    fs::remove_dir_all(&dir).unwrap();
    assert!(verify(&p, &ctx()).is_err());
}

#[test]
fn present_inputs() {
    let d = tmp();
    let file = d.path().join("f");
    let dir = d.path().join("d");
    fs::write(&file, "x").unwrap();
    fs::create_dir(&dir).unwrap();
    let mut rec = Recorder::new();
    rec.present(&file, PresentKind::File);
    rec.present(&dir, PresentKind::Dir);
    let p = mk(Kind::Check, b"s", rec, b"r", &keys_local(Kind::Check, 0));
    verify(&p, &ctx()).unwrap();

    // Content edits do not matter: only presence and type were seen.
    fs::write(&file, "completely different").unwrap();
    verify(&p, &ctx()).expect("exists is not a read");

    // File replaced by a folder.
    fs::remove_file(&file).unwrap();
    fs::create_dir(&file).unwrap();
    assert!(verify(&p, &ctx()).is_err(), "was a file, is a folder");
    fs::remove_dir(&file).unwrap();
    fs::write(&file, "x").unwrap();
    verify(&p, &ctx()).unwrap();

    // Gone.
    fs::remove_dir(&dir).unwrap();
    assert!(verify(&p, &ctx()).is_err());
    fs::write(&dir, "now a file").unwrap();
    assert!(verify(&p, &ctx()).is_err(), "was a folder, is a file");
}

// ----- inputs: env, tool, rules ------------------------------------------------------

#[test]
fn env_inputs() {
    let mut rec = Recorder::new();
    rec.env(b"CIGPASS_T_SET", Some(b"one"));
    rec.env(b"CIGPASS_T_UNSET", None);
    let p = mk(Kind::Check, b"s", rec, b"r", &keys_local(Kind::Check, 0));

    let mut c = ctx();
    set_env(&mut c, "CIGPASS_T_SET", Some("one"));
    set_env(&mut c, "CIGPASS_T_UNSET", None);
    verify(&p, &c).expect("same environment");

    set_env(&mut c, "CIGPASS_T_SET", Some("two"));
    let w = why(verify(&p, &c));
    assert!(w.contains("CIGPASS_T_SET"), "{w}");
    // The value is never printed in the reason.
    assert!(!w.contains("two") && !w.contains("one"), "{w}");

    set_env(&mut c, "CIGPASS_T_SET", Some("one"));
    set_env(&mut c, "CIGPASS_T_UNSET", Some("now set"));
    assert!(verify(&p, &c).is_err(), "unset became set");

    set_env(&mut c, "CIGPASS_T_UNSET", None);
    set_env(&mut c, "CIGPASS_T_SET", None);
    assert!(verify(&p, &c).is_err(), "set became unset");

    // Set to the empty string is not unset.
    set_env(&mut c, "CIGPASS_T_SET", Some(""));
    assert!(verify(&p, &c).is_err());
}

#[test]
fn tool_and_rules_inputs() {
    let mut rec = Recorder::new();
    rec.tool("cig", "1.20.0", "abc1234");
    rec.rules("blacklist", [7; 32]);
    let p = mk(Kind::Check, b"s", rec, b"r", &keys_local(Kind::Check, 0));

    let mut c = ctx();
    c.set_tool("cig", "1.20.0", "abc1234");
    c.set_rules("blacklist", [7; 32]);
    verify(&p, &c).expect("same tool, same rules");

    c.set_tool("cig", "1.20.1", "abc1234");
    assert!(verify(&p, &c).is_err(), "version moved");
    c.set_tool("cig", "1.20.0", "fffffff");
    assert!(verify(&p, &c).is_err(), "build moved");
    c.set_tool("cig", "1.20.0", "abc1234");
    c.set_rules("blacklist", [8; 32]);
    assert!(verify(&p, &c).is_err(), "rules moved");
    c.set_rules("blacklist", [7; 32]);
    verify(&p, &c).unwrap();
    // A tool the ctx does not know is a failure, never a pass.
    let mut rec = Recorder::new();
    rec.tool("unknown-tool", "1", "x");
    let q = mk(Kind::Check, b"s", rec, b"r", &keys_local(Kind::Check, 0));
    assert!(verify(&q, &c).is_err());
}

// ----- inputs: nested passes ----------------------------------------------------------

#[test]
fn nested_pass_input_is_found_and_verified() {
    let d = tmp();
    let f = d.path().join("dep.txt");
    fs::write(&f, "1").unwrap();
    let inner = pass_over_file(&f);
    let inner_id = verify(&inner, &ctx()).unwrap().id;

    let mut rec = Recorder::new();
    rec.pass(inner_id);
    let outer = mk(Kind::Run, b"outer", rec, b"r", &keys_local(Kind::Run, 0));

    let mut c = ctx();
    c.add_known_pass(inner.clone());
    verify(&outer, &c).expect("the nested pass is there and good");

    fs::write(&f, "22").unwrap();
    assert!(verify(&outer, &c).is_err(), "the nested pass went stale");

    // Not found at all.
    assert!(verify(&outer, &ctx()).is_err());
}

// ----- clocks -------------------------------------------------------------------------

#[test]
fn until_caveat_follows_the_wall_clock() {
    let t0 = 1_800_000_000_000i64;
    let keys = at(keys_local(Kind::Check, 0), t0, None);
    let p = attenuate(
        &mk(Kind::Check, b"s", Recorder::new(), b"r", &keys),
        Caveat::Until(t0 + 1_000),
    )
    .unwrap();
    let mut c = ctx();
    set_now(&mut c, t0 + 1_000, None);
    verify(&p, &c).expect("now <= t holds on the boundary");
    set_now(&mut c, t0 + 1_001, None);
    let w = why(verify(&p, &c));
    assert!(w.contains("passed"), "{w}");
}

#[test]
fn clock_set_back_does_not_extend_an_until() {
    let t0 = 1_800_000_000_000i64;
    let n0 = 5_000_000_000u64;
    let keys = at(keys_local(Kind::Check, 0), t0, Some(mono(BOOT_A, n0)));
    let p = attenuate(
        &mk(Kind::Check, b"s", Recorder::new(), b"r", &keys),
        Caveat::Until(t0 + 1_000),
    )
    .unwrap();

    // Honest: half a second passed on both clocks.
    let mut c = ctx();
    set_now(&mut c, t0 + 500, Some(mono(BOOT_A, n0 + 500_000_000)));
    verify(&p, &c).expect("inside the window");

    // Wall clock set back to before issue, monotonic says 10 s passed.
    set_now(
        &mut c,
        t0 - 86_400_000,
        Some(mono(BOOT_A, n0 + 10_000_000_000)),
    );
    assert!(
        verify(&p, &c).is_err(),
        "a clock set back must not extend the caveat"
    );

    // After a reboot the boot id differs: the mono value says nothing, the
    // wall clock decides.
    set_now(&mut c, t0 + 500, Some(mono(BOOT_B, 1)));
    verify(&p, &c).expect("another boot falls back to the wall clock");
    set_now(&mut c, t0 + 5_000, Some(mono(BOOT_B, 1)));
    assert!(verify(&p, &c).is_err());

    // No mono at all (not Linux, or an older pass): wall clock only.
    set_now(&mut c, t0 + 500, None);
    verify(&p, &c).unwrap();
}

#[test]
fn clock_input_follows_the_clock() {
    let t0 = 1_800_000_000_000i64;
    let n0 = 9_000_000_000u64;
    let mut rec = Recorder::new();
    rec.clock(t0 + 2_000);
    let keys = at(keys_local(Kind::Check, 0), t0, Some(mono(BOOT_A, n0)));
    let p = mk(Kind::Check, b"s", rec, b"r", &keys);

    let mut c = ctx();
    set_now(&mut c, t0 + 1_000, Some(mono(BOOT_A, n0 + 1_000_000_000)));
    verify(&p, &c).expect("before not_after");
    set_now(&mut c, t0 + 2_001, Some(mono(BOOT_A, n0 + 2_001_000_000)));
    assert!(verify(&p, &c).is_err(), "after not_after");
    // Wall clock set back, mono honest: still expired.
    set_now(
        &mut c,
        t0 - 1_000_000,
        Some(mono(BOOT_A, n0 + 3_000_000_000)),
    );
    assert!(verify(&p, &c).is_err(), "mono check on the Clock input");
}

// ----- the reasons are words, in encoded order -----------------------------------------

#[test]
fn the_first_failure_is_reported_in_encoded_order() {
    let d = tmp();
    let a = d.path().join("a.txt");
    let b = d.path().join("b.txt");
    fs::write(&a, "a").unwrap();
    fs::write(&b, "b").unwrap();
    let mut rec = Recorder::new();
    read_file(&mut rec, &a);
    read_file(&mut rec, &b);
    let p = mk(Kind::Check, b"s", rec, b"r", &keys_local(Kind::Check, 0));
    fs::write(&a, "AA").unwrap();
    fs::write(&b, "BB").unwrap();
    // Inputs sort by (tag, path bytes): a.txt comes first.
    let w = why(verify(&p, &ctx()));
    assert!(w.contains("a.txt") && !w.contains("b.txt"), "{w}");
}

// ----- recorder rules ---------------------------------------------------------------------

#[test]
fn recorder_sorts_and_collapses_duplicates() {
    let d = tmp();
    let a = d.path().join("a.txt");
    let b = d.path().join("b.txt");
    fs::write(&a, "a").unwrap();
    fs::write(&b, "b").unwrap();
    let mut r1 = Recorder::new();
    read_file(&mut r1, &b);
    read_file(&mut r1, &a);
    read_file(&mut r1, &a);
    let mut r2 = Recorder::new();
    read_file(&mut r2, &a);
    read_file(&mut r2, &b);
    let i1 = r1.finish().unwrap();
    let i2 = r2.finish().unwrap();
    assert_eq!(i1.len(), 2);
    assert_eq!(i1, i2, "order and repeats do not matter");
}

#[test]
fn recorder_refuses_two_answers_for_one_key() {
    let d = tmp();
    let a = d.path().join("a.txt");
    fs::write(&a, "one").unwrap();
    let mut rec = Recorder::new();
    read_file(&mut rec, &a);
    fs::write(&a, "two").unwrap();
    read_file(&mut rec, &a);
    assert!(rec.finish().is_err(), "an input changed while the work ran");

    let mut rec = Recorder::new();
    rec.env(b"X", Some(b"1"));
    rec.env(b"X", Some(b"2"));
    assert!(rec.finish().is_err());

    // Same key, same value: collapses without complaint.
    let mut rec = Recorder::new();
    rec.env(b"X", Some(b"1"));
    rec.env(b"X", Some(b"1"));
    assert_eq!(rec.finish().unwrap().len(), 1);
}

#[test]
fn open_world_keeps_the_first_reason_and_issues_nothing() {
    let mut rec = Recorder::new();
    rec.open_world("proc.run without reads".to_string());
    rec.open_world("stdin()".to_string());
    let e = rec.finish().unwrap_err().to_string();
    assert!(e.contains("proc.run without reads"), "{e}");
    assert!(!e.contains("stdin"), "first reason kept: {e}");
}

#[test]
fn a_pass_is_never_bigger_than_the_limits() {
    // Result over 64 KiB is refused at issue.
    let too_big = vec![0u8; 64 * 1024 + 1];
    assert!(issue(
        Kind::Check,
        b"s",
        vec![],
        too_big,
        &keys_local(Kind::Check, 0)
    )
    .is_err());
    let just = vec![0u8; 64 * 1024];
    assert!(issue(Kind::Check, b"s", vec![], just, &keys_local(Kind::Check, 0)).is_ok());
    // More than 64 caveats cannot be appended.
    let mut p = bare(Kind::Check);
    for i in 0..64 {
        p = attenuate(&p, Caveat::Max(1000 + i)).unwrap();
    }
    assert!(attenuate(&p, Caveat::Max(1)).is_err(), "at most 64 caveats");
}

/// A file on a pseudo file system is hashed at every check: writing a
/// sysctl leaves its file's times as they were, so its identity says
/// nothing. A file on a known local disk file system is not.
#[cfg(target_os = "linux")]
#[test]
fn files_under_proc_and_sys_are_always_hashed() {
    assert!(cig_pass::always_hash(Path::new("/proc/self/status")));
    assert!(cig_pass::always_hash(Path::new(
        "/proc/sys/kernel/osrelease"
    )));
    if Path::new("/sys/kernel").exists() {
        assert!(cig_pass::always_hash(Path::new("/sys/kernel")));
    }
    let d = tmp();
    let f = d.path().join("plain");
    fs::write(&f, "x").unwrap();
    // The file system the temporary folder is on: the longest mount point
    // above it. Identity is trusted on a local disk one, never elsewhere
    // (a container's overlay, a network share).
    let mounts = fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    let mut best: (usize, String) = (0, String::new());
    for l in mounts.lines() {
        let mut w = l.split_whitespace();
        let (_, at, ty) = (w.next(), w.next().unwrap_or(""), w.next().unwrap_or(""));
        if d.path().starts_with(at) && at.len() >= best.0 {
            best = (at.len(), ty.to_string());
        }
    }
    let local = [
        "ext4", "ext3", "ext2", "xfs", "btrfs", "f2fs", "tmpfs", "zfs",
    ];
    if local.contains(&best.1.as_str()) {
        assert!(
            !cig_pass::always_hash(&f),
            "{} is on {}",
            f.display(),
            best.1
        );
    } else if !best.1.is_empty() {
        assert!(
            cig_pass::always_hash(&f),
            "{} is on {}",
            f.display(),
            best.1
        );
    }
}

// Some helpers' imports are used only on one system.
#[allow(dead_code)]
fn _unused(_: PathBuf, _: Input) {}
