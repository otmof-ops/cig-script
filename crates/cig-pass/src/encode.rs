// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! The pass and its one encoding.
//!
//! A pass is encoded the same way everywhere it goes: for its tag, for its
//! id, on disk and in a token. Decoding is strict: anything this version
//! does not know (a version, a kind, an input, a caveat), anything out of
//! order, any length past the end and any byte left over is refused, never
//! read "as best it can". Decoding never panics.

use std::fmt;

/// The first seven bytes of every encoded pass.
pub const MAGIC: &[u8; 7] = b"CIGPASS";
/// The encoding's version.
pub const VERSION: u8 = 1;
/// What a pass's token starts with.
pub const TOKEN_PREFIX: &str = "cigpass1.";
/// The kinds this version knows. Anything else is not a pass.
pub const KINDS: &[&str] = &[
    "check",
    "run",
    "trial",
    "verdict",
    "approval",
    "program",
    "archive",
    "parcel",
    "rules",
    "doctor",
    "revoked",
    "human-yes",
];
/// `flags` bit 0: a hop ran with declared `reads` while the work was
/// recorded, so some inputs are the issuer's declaration, not observed.
pub const FLAG_DECLARED_READS: u8 = 1;
/// The longest glob a caveat carries.
pub const MAX_GLOB: usize = 1024;
/// The most hosts a network caveat names, and arguments an `args` caveat matches.
pub const MAX_LIST: usize = 64;
/// The most inputs a pass holds.
pub const MAX_INPUTS: usize = u16::MAX as usize;
/// The most caveats a pass holds.
pub const MAX_CAVEATS: usize = 64;
/// The largest result a pass carries.
pub const MAX_RESULT: usize = 64 * 1024;
/// The largest encoded pass.
pub const MAX_PASS: usize = 4 * 1024 * 1024;

/// Local passes carry an HMAC chain; shareable ones a chain of Ed25519
/// signatures another machine can check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    Local,
    Shareable,
}

/// Who issued a pass: a local issuer id (a digest of a subkey of the
/// machine's key), or an Ed25519 public key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Issuer {
    Local([u8; 16]),
    Ed25519([u8; 32]),
}

/// A reading of a clock that does not go back, with the boot it belongs
/// to: what keeps a wall clock set back from extending a pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mono {
    pub boot_id: [u8; 16],
    pub ns: u64,
}

/// What a file was, as `stat` saw it. On Windows only `size` and
/// `mtime_ns` are filled, and the verifier always hashes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Identity {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime_ns: i64,
    pub ctime_ns: i64,
    pub mode: u32,
}

/// What a presence check saw.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PresentKind {
    File = 1,
    Dir = 2,
    Other = 3,
}

impl PresentKind {
    fn from_u8(b: u8) -> Option<Self> {
        match b {
            1 => Some(Self::File),
            2 => Some(Self::Dir),
            3 => Some(Self::Other),
            _ => None,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Self::File => "a file",
            Self::Dir => "a folder",
            Self::Other => "something other than a file or a folder",
        }
    }
}

/// One thing the work depended on, recorded while it ran.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    /// A file the work read: what it was and what it held.
    File {
        path: Vec<u8>,
        identity: Identity,
        /// The file changed within two seconds of being read: verify always
        /// hashes it, whatever its identity says.
        racy: bool,
        blake3: [u8; 32],
    },
    /// A path the work looked for and did not find.
    Absent { path: Vec<u8> },
    /// A folder the work listed: a digest of its names and their types.
    Dir { path: Vec<u8>, listing: [u8; 32] },
    /// A variable the work read: a digest of its value, or unset.
    Env {
        name: Vec<u8>,
        value: Option<[u8; 32]>,
    },
    /// A tool the result depends on, by version and build.
    Tool {
        name: String,
        version: String,
        build: String,
    },
    /// Rule data the result depends on, by digest.
    Rules { name: String, blake3: [u8; 32] },
    /// Another pass this one relied on.
    Pass { id: [u8; 32] },
    /// A result that is true only for a while.
    Clock { not_after_ms: i64 },
    /// A path the work only checked the presence of.
    Present { path: Vec<u8>, kind: PresentKind },
}

impl Input {
    /// The input's tag in the encoding: also the first sort key.
    pub fn tag(&self) -> u8 {
        match self {
            Input::File { .. } => 1,
            Input::Absent { .. } => 2,
            Input::Dir { .. } => 3,
            Input::Env { .. } => 4,
            Input::Tool { .. } => 5,
            Input::Rules { .. } => 6,
            Input::Pass { .. } => 7,
            Input::Clock { .. } => 8,
            Input::Present { .. } => 9,
        }
    }

    /// The second sort key: the path, name or id the input is about.
    pub fn key(&self) -> &[u8] {
        match self {
            Input::File { path, .. }
            | Input::Absent { path }
            | Input::Dir { path, .. }
            | Input::Present { path, .. } => path,
            Input::Env { name, .. } => name,
            Input::Tool { name, .. } | Input::Rules { name, .. } => name.as_bytes(),
            Input::Pass { id } => id,
            Input::Clock { .. } => &[],
        }
    }

    /// The path the input is about, for inputs that name one.
    pub fn path(&self) -> Option<&[u8]> {
        match self {
            Input::File { path, .. }
            | Input::Absent { path }
            | Input::Dir { path, .. }
            | Input::Present { path, .. } => Some(path),
            _ => None,
        }
    }

    pub(crate) fn sort_key(&self) -> (u8, &[u8]) {
        (self.tag(), self.key())
    }

    fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.tag());
        match self {
            Input::File {
                path,
                identity,
                racy,
                blake3,
            } => {
                put_path(out, path);
                put_u64(out, identity.dev);
                put_u64(out, identity.ino);
                put_u64(out, identity.size);
                put_i64(out, identity.mtime_ns);
                put_i64(out, identity.ctime_ns);
                put_u32(out, identity.mode);
                out.push(u8::from(*racy));
                out.extend_from_slice(blake3);
            }
            Input::Absent { path } => put_path(out, path),
            Input::Dir { path, listing } => {
                put_path(out, path);
                out.extend_from_slice(listing);
            }
            Input::Env { name, value } => {
                put_path(out, name);
                match value {
                    Some(v) => {
                        out.push(1);
                        out.extend_from_slice(v);
                    }
                    None => out.push(0),
                }
            }
            Input::Tool {
                name,
                version,
                build,
            } => {
                put_str(out, name);
                put_str(out, version);
                put_str(out, build);
            }
            Input::Rules { name, blake3 } => {
                put_str(out, name);
                out.extend_from_slice(blake3);
            }
            Input::Pass { id } => out.extend_from_slice(id),
            Input::Clock { not_after_ms } => put_i64(out, *not_after_ms),
            Input::Present { path, kind } => {
                put_path(out, path);
                out.push(*kind as u8);
            }
        }
    }

    fn decode(r: &mut Reader<'_>) -> Result<Input, DecodeError> {
        let tag = r.u8()?;
        Ok(match tag {
            1 => {
                let path = r.path()?;
                let identity = Identity {
                    dev: r.u64()?,
                    ino: r.u64()?,
                    size: r.u64()?,
                    mtime_ns: r.i64()?,
                    ctime_ns: r.i64()?,
                    mode: r.u32()?,
                };
                let racy = match r.u8()? {
                    0 => false,
                    1 => true,
                    _ => return Err(DecodeError::new("a file input with an unknown racy flag")),
                };
                Input::File {
                    path,
                    identity,
                    racy,
                    blake3: r.arr32()?,
                }
            }
            2 => Input::Absent { path: r.path()? },
            3 => Input::Dir {
                path: r.path()?,
                listing: r.arr32()?,
            },
            4 => {
                let name = r.name()?;
                let value = match r.u8()? {
                    0 => None,
                    1 => Some(r.arr32()?),
                    _ => {
                        return Err(DecodeError::new(
                            "an environment input that is neither set nor unset",
                        ))
                    }
                };
                Input::Env { name, value }
            }
            5 => Input::Tool {
                name: r.str_nonempty("a tool's name")?,
                version: r.str()?,
                build: r.str()?,
            },
            6 => Input::Rules {
                name: r.str_nonempty("a rule set's name")?,
                blake3: r.arr32()?,
            },
            7 => Input::Pass { id: r.arr32()? },
            8 => Input::Clock {
                not_after_ms: r.i64()?,
            },
            9 => {
                let path = r.path()?;
                let kind = PresentKind::from_u8(r.u8()?)
                    .ok_or_else(|| DecodeError::new("a presence input of an unknown type"))?;
                Input::Present { path, kind }
            }
            t => return Err(DecodeError::new(format!("an input of unknown type {t}"))),
        })
    }
}

/// A network policy, as narrow as a caveat asks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Network {
    /// No network at all.
    None,
    /// Only these hosts.
    Hosts(Vec<String>),
}

/// A restriction added to a pass. Caveats only narrow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Caveat {
    /// Every folder the action works in is under this path.
    Within(Vec<u8>),
    /// The pass holds until this time (unix ms).
    Until(i64),
    /// The action runs in a box.
    Boxed,
    /// The action's network is at least this narrow.
    Network(Network),
    /// The action's arguments match these globs, one per argument, in
    /// order; a last `...` matches any further arguments.
    Args(Vec<String>),
    /// The running cig is at least this version.
    CigAtLeast(String),
    /// The running cig is this build.
    Build(String),
    /// The action was started under this agent.
    Agent(String),
    /// Spendable once.
    Once,
    /// Spendable this many times.
    Max(u32),
    /// Valid only with a discharge pass of this kind.
    Needs(String),
}

impl Caveat {
    pub fn tag(&self) -> u8 {
        match self {
            Caveat::Within(_) => 1,
            Caveat::Until(_) => 2,
            Caveat::Boxed => 3,
            Caveat::Network(_) => 4,
            Caveat::Args(_) => 5,
            Caveat::CigAtLeast(_) => 6,
            Caveat::Build(_) => 7,
            Caveat::Agent(_) => 8,
            Caveat::Once => 9,
            Caveat::Max(_) => 10,
            Caveat::Needs(_) => 11,
        }
    }

    /// The caveat's own bytes, as the tag chain and the id cover them.
    pub fn encoded(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.tag());
        match self {
            Caveat::Within(p) => put_path(out, p),
            Caveat::Until(t) => put_i64(out, *t),
            Caveat::Boxed | Caveat::Once => {}
            Caveat::Network(Network::None) => out.push(0),
            Caveat::Network(Network::Hosts(hosts)) => {
                out.push(1);
                put_u16(out, hosts.len() as u16);
                for h in hosts {
                    put_str(out, h);
                }
            }
            Caveat::Args(globs) => {
                put_u16(out, globs.len() as u16);
                for g in globs {
                    put_str(out, g);
                }
            }
            Caveat::CigAtLeast(s) | Caveat::Build(s) | Caveat::Agent(s) | Caveat::Needs(s) => {
                put_str(out, s)
            }
            Caveat::Max(n) => put_u32(out, *n),
        }
    }

    fn decode(r: &mut Reader<'_>) -> Result<Caveat, DecodeError> {
        let tag = r.u8()?;
        Ok(match tag {
            1 => Caveat::Within(r.path()?),
            2 => Caveat::Until(r.i64()?),
            3 => Caveat::Boxed,
            4 => match r.u8()? {
                0 => Caveat::Network(Network::None),
                1 => {
                    let n = r.u16()? as usize;
                    if n == 0 || n > MAX_LIST {
                        return Err(DecodeError::new(
                            "a network caveat with no hosts, or more than 64",
                        ));
                    }
                    let mut hosts = Vec::with_capacity(n.min(256));
                    for _ in 0..n {
                        hosts.push(r.str_nonempty("a host")?);
                    }
                    Caveat::Network(Network::Hosts(hosts))
                }
                _ => return Err(DecodeError::new("a network caveat of an unknown form")),
            },
            5 => {
                let n = r.u16()? as usize;
                if n == 0 || n > MAX_LIST {
                    return Err(DecodeError::new(
                        "an args caveat with no patterns, or too many",
                    ));
                }
                let mut globs = Vec::with_capacity(n);
                for _ in 0..n {
                    let g = r.str()?;
                    if g.len() > MAX_GLOB {
                        return Err(DecodeError::new("a pattern longer than 1 KiB"));
                    }
                    globs.push(g);
                }
                Caveat::Args(globs)
            }
            6 => Caveat::CigAtLeast(r.str_nonempty("a version")?),
            7 => Caveat::Build(r.str_nonempty("a build id")?),
            8 => Caveat::Agent(r.str_nonempty("an agent's name")?),
            9 => Caveat::Once,
            10 => {
                let n = r.u32()?;
                if n == 0 {
                    return Err(DecodeError::new("a `max 0` caveat"));
                }
                Caveat::Max(n)
            }
            11 => {
                let k = r.str()?;
                if !is_kind(&k) {
                    return Err(DecodeError::new(format!(
                        "a discharge of unknown kind `{k}`"
                    )));
                }
                Caveat::Needs(k)
            }
            t => return Err(DecodeError::new(format!("a caveat of unknown type {t}"))),
        })
    }
}

/// One link of a shareable pass's chain: the next public key, signed.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Block {
    pub next_pk: [u8; 32],
    pub sig: [u8; 64],
}

impl fmt::Debug for Block {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Block(next {})", hex8(&self.next_pk))
    }
}

/// The end of a shareable pass's chain: the private key that lets its
/// holder add one more block, or a final signature that closes it.
#[derive(Clone, PartialEq, Eq)]
pub enum Proof {
    Key([u8; 32]),
    Sealed([u8; 64]),
}

impl fmt::Debug for Proof {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Proof::Key(_) => f.write_str("Proof::Key(..)"),
            Proof::Sealed(_) => f.write_str("Proof::Sealed(..)"),
        }
    }
}

/// What authenticates a pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tag {
    Local([u8; 32]),
    Shareable { blocks: Vec<Block>, proof: Proof },
}

/// A pass: what was checked, what it depended on, the result, and the
/// caveats under which it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pass {
    pub form: Form,
    pub kind: String,
    /// `FLAG_DECLARED_READS`, or 0.
    pub flags: u8,
    pub subject: [u8; 32],
    pub inputs: Vec<Input>,
    pub result: Vec<u8>,
    pub issuer: Issuer,
    pub epoch: u64,
    /// How long the issuer's ledger was when the pass was made (local
    /// passes; 0 for shareable ones): a ledger shorter than this was cut
    /// or deleted, and the pass does not hold.
    pub ledger_seq: u64,
    pub issued_ms: i64,
    pub mono: Option<Mono>,
    pub caveats: Vec<Caveat>,
    pub tag: Tag,
}

/// Why bytes are not a pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodeError(pub String);

impl DecodeError {
    pub fn new(msg: impl Into<String>) -> Self {
        DecodeError(msg.into())
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "not a pass: {}", self.0)
    }
}

impl std::error::Error for DecodeError {}

/// Is this a kind this version knows?
pub fn is_kind(k: &str) -> bool {
    KINDS.contains(&k)
}

/// The digest of a subject's bytes, as a pass carries it.
pub fn subject_digest(bytes: &[u8]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"cigpass1-subject");
    h.update(bytes);
    *h.finalize().as_bytes()
}

/// The subject a discharge for the pass with this root id carries: the
/// digest of the root id itself, so only an API that issues over raw bytes
/// (not a script's JSON subject) mints one.
pub fn discharge_subject(root: &[u8; 32]) -> [u8; 32] {
    subject_digest(root)
}

impl Pass {
    pub fn form(&self) -> Form {
        self.form
    }

    /// The bytes everything else covers: all of the pass but its caveats
    /// and its tag.
    pub fn encode_base(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(256 + self.result.len());
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.push(match self.form {
            Form::Local => 0,
            Form::Shareable => 1,
        });
        put_str(&mut out, &self.kind);
        out.push(self.flags);
        out.extend_from_slice(&self.subject);
        put_u16(&mut out, self.inputs.len() as u16);
        for i in &self.inputs {
            i.encode(&mut out);
        }
        put_bytes(&mut out, &self.result);
        match self.issuer {
            Issuer::Local(id) => {
                out.push(16);
                out.extend_from_slice(&id);
            }
            Issuer::Ed25519(pk) => {
                out.push(32);
                out.extend_from_slice(&pk);
            }
        }
        put_u64(&mut out, self.epoch);
        put_u64(&mut out, self.ledger_seq);
        put_i64(&mut out, self.issued_ms);
        match self.mono {
            Some(m) => {
                out.push(1);
                out.extend_from_slice(&m.boot_id);
                put_u64(&mut out, m.ns);
            }
            None => out.push(0),
        }
        out
    }

    /// The whole pass, as it is stored and sent.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = self.encode_base();
        put_u16(&mut out, self.caveats.len() as u16);
        for c in &self.caveats {
            c.encode(&mut out);
        }
        match &self.tag {
            Tag::Local(t) => out.extend_from_slice(t),
            Tag::Shareable { blocks, proof } => {
                out.push(blocks.len() as u8);
                for b in blocks {
                    out.extend_from_slice(&b.next_pk);
                    out.extend_from_slice(&b.sig);
                }
                match proof {
                    Proof::Key(k) => {
                        out.push(0);
                        out.extend_from_slice(k);
                    }
                    Proof::Sealed(s) => {
                        out.push(1);
                        out.extend_from_slice(s);
                    }
                }
            }
        }
        out
    }

    /// Read a pass, strictly.
    pub fn decode(bytes: &[u8]) -> Result<Pass, DecodeError> {
        if bytes.len() > MAX_PASS {
            return Err(DecodeError::new("larger than any pass"));
        }
        let mut r = Reader::new(bytes);
        if r.take(MAGIC.len())? != MAGIC {
            return Err(DecodeError::new("it does not start with CIGPASS"));
        }
        let version = r.u8()?;
        if version != VERSION {
            return Err(DecodeError::new(format!(
                "version {version}; this cig reads version {VERSION}"
            )));
        }
        let form = match r.u8()? {
            0 => Form::Local,
            1 => Form::Shareable,
            f => return Err(DecodeError::new(format!("a form of unknown type {f}"))),
        };
        let kind = r.str()?;
        if !is_kind(&kind) {
            return Err(DecodeError::new(format!(
                "a kind this cig does not know, `{kind}`"
            )));
        }
        let flags = r.u8()?;
        if flags & !FLAG_DECLARED_READS != 0 {
            return Err(DecodeError::new("flags this cig does not know"));
        }
        let subject = r.arr32()?;
        let n_inputs = r.u16()? as usize;
        let mut inputs: Vec<Input> = Vec::with_capacity(n_inputs.min(1024));
        for _ in 0..n_inputs {
            let input = Input::decode(&mut r)?;
            if let Some(prev) = inputs.last() {
                if prev.sort_key() >= input.sort_key() {
                    return Err(DecodeError::new("its inputs are out of order or repeated"));
                }
            }
            inputs.push(input);
        }
        let result = r.bytes()?;
        if result.len() > MAX_RESULT {
            return Err(DecodeError::new("a result larger than 64 KiB"));
        }
        let issuer = match r.u8()? {
            16 => Issuer::Local(r.arr16()?),
            32 => {
                let pk = r.arr32()?;
                if ed25519_dalek::VerifyingKey::from_bytes(&pk).is_err() {
                    return Err(DecodeError::new("an issuer key that is not an Ed25519 key"));
                }
                Issuer::Ed25519(pk)
            }
            n => return Err(DecodeError::new(format!("an issuer {n} bytes long"))),
        };
        match (form, issuer) {
            (Form::Local, Issuer::Local(_)) | (Form::Shareable, Issuer::Ed25519(_)) => {}
            _ => return Err(DecodeError::new("its issuer does not match its form")),
        }
        let epoch = r.u64()?;
        let ledger_seq = r.u64()?;
        let issued_ms = r.i64()?;
        let mono = match r.u8()? {
            0 => None,
            1 => Some(Mono {
                boot_id: r.arr16()?,
                ns: r.u64()?,
            }),
            _ => return Err(DecodeError::new("a clock reading of unknown form")),
        };
        let n_caveats = r.u16()? as usize;
        if n_caveats > MAX_CAVEATS {
            return Err(DecodeError::new(format!(
                "{n_caveats} caveats; at most {MAX_CAVEATS}"
            )));
        }
        let mut caveats = Vec::with_capacity(n_caveats);
        for _ in 0..n_caveats {
            caveats.push(Caveat::decode(&mut r)?);
        }
        let tag = match form {
            Form::Local => Tag::Local(r.arr32()?),
            Form::Shareable => {
                let n_blocks = r.u8()? as usize;
                if n_blocks != n_caveats + 1 {
                    return Err(DecodeError::new(
                        "its signature blocks do not match its caveats",
                    ));
                }
                let mut blocks = Vec::with_capacity(n_blocks);
                for _ in 0..n_blocks {
                    blocks.push(Block {
                        next_pk: r.arr32()?,
                        sig: r.arr64()?,
                    });
                }
                let proof = match r.u8()? {
                    0 => Proof::Key(r.arr32()?),
                    1 => Proof::Sealed(r.arr64()?),
                    _ => return Err(DecodeError::new("a proof of unknown form")),
                };
                Tag::Shareable { blocks, proof }
            }
        };
        if !r.is_empty() {
            return Err(DecodeError::new("bytes left over after the pass"));
        }
        Ok(Pass {
            form,
            kind,
            flags,
            subject,
            inputs,
            result,
            issuer,
            epoch,
            ledger_seq,
            issued_ms,
            mono,
            caveats,
            tag,
        })
    }

    /// The pass's id: a digest of everything but its tag.
    pub fn id(&self) -> [u8; 32] {
        self.prefix_id(self.caveats.len())
    }

    /// The id of the pass as it was with only its first `k` caveats.
    pub fn prefix_id(&self, k: usize) -> [u8; 32] {
        let mut h = blake3::Hasher::new();
        h.update(b"cigpass1-id");
        h.update(&self.encode_base());
        for c in self.caveats.iter().take(k) {
            h.update(&c.encoded());
        }
        *h.finalize().as_bytes()
    }

    /// Every prefix id, from the root (no caveats) to the whole pass.
    pub fn prefix_ids(&self) -> Vec<[u8; 32]> {
        let mut h = blake3::Hasher::new();
        h.update(b"cigpass1-id");
        h.update(&self.encode_base());
        let mut out = Vec::with_capacity(self.caveats.len() + 1);
        out.push(*h.clone().finalize().as_bytes());
        for c in &self.caveats {
            h.update(&c.encoded());
            out.push(*h.clone().finalize().as_bytes());
        }
        out
    }

    /// The id of the pass before any caveat was added.
    pub fn root_id(&self) -> [u8; 32] {
        self.prefix_id(0)
    }

    /// The pass as text: `cigpass1.` and the encoding in base64url.
    pub fn token(&self) -> String {
        use base64::Engine;
        let mut s = String::from(TOKEN_PREFIX);
        s.push_str(&base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(self.encode()));
        s
    }

    /// Read a pass from its token.
    pub fn from_token(token: &str) -> Result<Pass, DecodeError> {
        use base64::Engine;
        let body = token
            .trim()
            .strip_prefix(TOKEN_PREFIX)
            .ok_or_else(|| DecodeError::new(format!("a token starts with {TOKEN_PREFIX}")))?;
        if body.len() > MAX_PASS * 4 / 3 + 4 {
            return Err(DecodeError::new("larger than any pass"));
        }
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(body.as_bytes())
            .map_err(|e| DecodeError::new(format!("the token is not base64url: {e}")))?;
        Pass::decode(&bytes)
    }
}

/// The first eight hex digits of a digest, for messages.
pub fn hex8(b: &[u8]) -> String {
    b.iter().take(4).map(|x| format!("{x:02x}")).collect()
}

/// A digest in full hex.
pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Read a 32-byte digest from hex.
pub fn from_hex32(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

// ----- writing ---------------------------------------------------------------

pub(crate) fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

pub(crate) fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

pub(crate) fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

pub(crate) fn put_i64(out: &mut Vec<u8>, v: i64) {
    out.extend_from_slice(&v.to_le_bytes());
}

pub(crate) fn put_str(out: &mut Vec<u8>, s: &str) {
    put_u16(out, s.len() as u16);
    out.extend_from_slice(s.as_bytes());
}

pub(crate) fn put_path(out: &mut Vec<u8>, p: &[u8]) {
    put_u16(out, p.len() as u16);
    out.extend_from_slice(p);
}

pub(crate) fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    put_u32(out, b.len() as u32);
    out.extend_from_slice(b);
}

// ----- reading ---------------------------------------------------------------

pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Reader { buf, at: 0 }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.at == self.buf.len()
    }

    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let end = self
            .at
            .checked_add(n)
            .filter(|&e| e <= self.buf.len())
            .ok_or_else(|| DecodeError::new("it ends early"))?;
        let s = &self.buf[self.at..end];
        self.at = end;
        Ok(s)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().expect("2 bytes"),
        ))
    }

    pub(crate) fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }

    pub(crate) fn u64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("8 bytes"),
        ))
    }

    pub(crate) fn i64(&mut self) -> Result<i64, DecodeError> {
        Ok(i64::from_le_bytes(
            self.take(8)?.try_into().expect("8 bytes"),
        ))
    }

    pub(crate) fn arr16(&mut self) -> Result<[u8; 16], DecodeError> {
        Ok(self.take(16)?.try_into().expect("16 bytes"))
    }

    pub(crate) fn arr32(&mut self) -> Result<[u8; 32], DecodeError> {
        Ok(self.take(32)?.try_into().expect("32 bytes"))
    }

    pub(crate) fn arr64(&mut self) -> Result<[u8; 64], DecodeError> {
        Ok(self.take(64)?.try_into().expect("64 bytes"))
    }

    pub(crate) fn bytes(&mut self) -> Result<Vec<u8>, DecodeError> {
        let n = self.u32()? as usize;
        if n > MAX_PASS {
            return Err(DecodeError::new("a length larger than any pass"));
        }
        Ok(self.take(n)?.to_vec())
    }

    pub(crate) fn str(&mut self) -> Result<String, DecodeError> {
        let n = self.u16()? as usize;
        let b = self.take(n)?;
        if b.contains(&0) {
            return Err(DecodeError::new("text with a NUL in it"));
        }
        String::from_utf8(b.to_vec()).map_err(|_| DecodeError::new("text that is not UTF-8"))
    }

    fn str_nonempty(&mut self, what: &str) -> Result<String, DecodeError> {
        let s = self.str()?;
        if s.is_empty() {
            return Err(DecodeError::new(format!("{what} that is empty")));
        }
        Ok(s)
    }

    /// An absolute path, as the work used it.
    fn path(&mut self) -> Result<Vec<u8>, DecodeError> {
        let n = self.u16()? as usize;
        let p = self.take(n)?.to_vec();
        if !is_absolute_bytes(&p) {
            return Err(DecodeError::new("a path that is not absolute"));
        }
        Ok(p)
    }

    /// A variable's name: not empty, no `=` and no NUL.
    fn name(&mut self) -> Result<Vec<u8>, DecodeError> {
        let n = self.u16()? as usize;
        let b = self.take(n)?.to_vec();
        if b.is_empty() || b.contains(&b'=') || b.contains(&0) {
            return Err(DecodeError::new("a variable's name that cannot be one"));
        }
        Ok(b)
    }
}

/// Is this path absolute on Unix (`/...`) or Windows (`C:\...`, `\\...`)?
/// A pass made on one system can be read on another, so both are accepted
/// here; the verifier on each system says whether it can check it.
pub fn is_absolute_bytes(p: &[u8]) -> bool {
    if p.contains(&0) {
        return false;
    }
    match p {
        [b'/', ..] => true,
        [b'\\', b'\\', ..] => true,
        [d, b':', b'\\' | b'/', ..] if d.is_ascii_alphabetic() => true,
        _ => false,
    }
}
