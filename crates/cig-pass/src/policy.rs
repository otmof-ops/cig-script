// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! What each kind of pass may be: local or shareable, issued here or by
//! another machine, by whom, and under what extra rules. One table, read
//! when a pass is issued and again when it is verified.

/// The rules for one kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KindPolicy {
    pub kind: &'static str,
    /// May be a local (HMAC) pass.
    pub local: bool,
    /// May be a shareable (Ed25519) pass.
    pub shareable: bool,
    /// A shareable pass of this kind may come from another machine's key
    /// (an allowed signer scoped to this kind).
    pub foreign: bool,
    /// Only an authority signer (a key behind the platform's own
    /// authentication, its public key installed by an administrator) may
    /// issue it.
    pub authority: bool,
    /// A pass whose inputs include a hop's declared reads is refused.
    pub refuse_declared_reads: bool,
    /// The longest it may live: it must carry an `until` caveat no later
    /// than this many ms after it was issued.
    pub max_life_ms: Option<i64>,
    /// It may not itself carry `needs`.
    pub no_needs: bool,
}

const fn kind(k: &'static str) -> KindPolicy {
    KindPolicy {
        kind: k,
        local: true,
        shareable: false,
        foreign: false,
        authority: false,
        refuse_declared_reads: false,
        max_life_ms: None,
        no_needs: false,
    }
}

const fn shareable(mut p: KindPolicy) -> KindPolicy {
    p.shareable = true;
    p.foreign = true;
    p
}

const fn strict(mut p: KindPolicy) -> KindPolicy {
    p.refuse_declared_reads = true;
    p
}

/// The table. A kind not in it is not a pass.
pub const POLICY: &[KindPolicy] = &[
    shareable(kind("check")),
    shareable(kind("program")),
    shareable(kind("parcel")),
    shareable(kind("archive")),
    KindPolicy {
        kind: "revoked",
        local: false,
        shareable: true,
        foreign: true,
        authority: false,
        refuse_declared_reads: true,
        max_life_ms: None,
        no_needs: true,
    },
    strict(kind("approval")),
    strict(kind("verdict")),
    strict(kind("rules")),
    kind("doctor"),
    kind("trial"),
    kind("run"),
    KindPolicy {
        kind: "human-yes",
        local: false,
        shareable: true,
        foreign: false,
        authority: true,
        refuse_declared_reads: true,
        max_life_ms: Some(3_600_000),
        no_needs: true,
    },
];

/// The rules for a kind (`None`: not a kind of pass).
pub fn policy(kind: &str) -> Option<&'static KindPolicy> {
    POLICY.iter().find(|p| p.kind == kind)
}
