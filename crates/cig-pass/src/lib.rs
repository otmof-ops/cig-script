// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! # cig-pass
//!
//! One primitive for every "this was already checked" in the cig family
//! (otmof-ops/cig-script#186).
//!
//! A **pass** is a small authenticated record of what was checked, every
//! input the check depended on (recorded while it ran, never declared by
//! hand), the result, and the caveats under which it holds. Verifying
//! something is then asking "do I hold a pass for these exact inputs?",
//! which costs one MAC or signature check and a `stat` per input, whatever
//! the work it stands for.
//!
//! - **Local** passes carry an HMAC-SHA256 chain under a subkey of the
//!   machine's key: fast and private. Anyone holding one can add a caveat
//!   ([`attenuate`]); nobody can remove one (the macaroons construction).
//! - **Shareable** passes carry a chain of Ed25519 signatures that another
//!   machine checks against its allowed signers (the Biscuit design), and
//!   narrow the same way.
//! - Passes die by epoch (a kind's subkey changes), by revocation of any
//!   pass they were narrowed from, by spending (`once`, `max n`), and the
//!   moment any recorded input changes.
//!
//! [`verify`] is the only code in the family that decides "trusted". It is
//! small, strict (an unknown version, kind, input or caveat is invalid),
//! fuzzed, and compares tags in constant time. Its threat model is
//! `THREAT-MODEL.md` beside this crate.
//!
//! The crate does no key management: the caller gives it keys through
//! [`Ctx`] (cig derives them from the tin's key).

mod auth;
mod encode;
mod fsid;
mod issue;
pub mod marks;
mod memctx;
pub mod policy;
mod record;
pub mod ssh;
pub mod store;

mod text;
mod verify;

pub use auth::{attenuate, public_key, seal};
pub use encode::{
    discharge_subject, from_hex32, hex, hex8, is_absolute_bytes, is_kind, subject_digest, Block,
    Caveat, DecodeError, Form, Identity, Input, Issuer, Mono, Network, Pass, PresentKind, Proof,
    Tag, FLAG_DECLARED_READS, KINDS, MAX_CAVEATS, MAX_GLOB, MAX_INPUTS, MAX_LIST, MAX_PASS,
    MAX_RESULT, TOKEN_PREFIX,
};
pub use fsid::{
    always_hash, bytes_path, clean, env_digest, hash_bytes, hash_file, hash_regular, identity,
    is_racy, list_dir, listing_digest, mono_now, path_bytes, racy_at, shown, wall_ms, EntryType,
    Hashed,
};
pub use issue::{issue_local, issue_shareable, keyed, reissue_shareable, Draft, IssueError};
pub use memctx::MemCtx;
pub use record::Recorder;
pub use text::{caveat_text, iso, parse_caveat, parse_span, parse_time};
pub use verify::{
    args_match, verify, version_parts, Action, Ctx, Now, SpendKey, Verified, Verifier, Why,
    MAX_DEPTH, MAX_HASHED, MAX_INPUT_CHECKS, MAX_NESTED,
};
