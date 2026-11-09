// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! A context held in memory: every answer the verifier asks for, set by
//! hand. What tests, benchmarks and the fuzzer verify against, and a
//! ready-made context for a tool that keeps its own keys.

use crate::encode::Pass;
use crate::verify::{Action, Ctx, Now};
use std::collections::{HashMap, HashSet};

type KeyFn = Box<dyn Fn(&str, u64) -> Option<[u8; 32]>>;

/// A hash key for a context that only has a key function: derived from the
/// key it gives for a reserved (kind, epoch), so tests and tools that hold
/// one secret get a separate digest key from it.
fn derive_hash_key(key_fn: &dyn Fn(&str, u64) -> Option<[u8; 32]>) -> Option<[u8; 32]> {
    key_fn("check", u64::MAX).map(|k| *blake3::keyed_hash(&k, b"cigpass1-hash-key").as_bytes())
}

/// A context whose every answer is set by hand.
pub struct MemCtx {
    pub issuer: Option<[u8; 16]>,
    key_fn: Option<KeyFn>,
    /// The key content digests in local passes are keyed under.
    pub hash_key: Option<[u8; 32]>,
    /// Human authority keys (for `human-yes`).
    pub authorities: HashSet<[u8; 32]>,
    pub epochs: HashMap<String, u64>,
    /// Allowed signers and the kinds each may issue (`*`: any kind its
    /// policy lets come from elsewhere).
    pub signers: HashMap<[u8; 32], HashSet<String>>,
    pub own_signers: HashSet<[u8; 32]>,
    /// `None`: read the real clocks.
    pub now: Option<Now>,
    /// `None` here falls back to the process's environment.
    pub env: HashMap<Vec<u8>, Option<Vec<u8>>>,
    pub env_from_process: bool,
    pub tools: HashMap<String, (String, String)>,
    pub rules: HashMap<String, [u8; 32]>,
    pub yanked: HashSet<(String, String, String)>,
    pub cig: (String, String),
    pub action: Action,
    pub revoked: HashSet<[u8; 32]>,
    pub spends: HashMap<[u8; 32], u32>,
    pub passes: HashMap<[u8; 32], Pass>,
    pub discharges: Vec<Pass>,
}

impl Default for MemCtx {
    fn default() -> Self {
        MemCtx {
            issuer: None,
            key_fn: None,
            hash_key: None,
            authorities: HashSet::new(),
            epochs: HashMap::new(),
            signers: HashMap::new(),
            own_signers: HashSet::new(),
            now: None,
            env: HashMap::new(),
            env_from_process: false,
            tools: HashMap::new(),
            rules: HashMap::new(),
            yanked: HashSet::new(),
            cig: ("0.0.0".to_string(), "unknown".to_string()),
            action: Action::default(),
            revoked: HashSet::new(),
            spends: HashMap::new(),
            passes: HashMap::new(),
            discharges: Vec::new(),
        }
    }
}

impl MemCtx {
    pub fn new() -> Self {
        Self::default()
    }

    /// A context that knows one local key: the issuer id and a function
    /// from (kind, epoch) to the subkey.
    pub fn with_local(
        issuer: [u8; 16],
        key_fn: impl Fn(&str, u64) -> Option<[u8; 32]> + 'static,
    ) -> Self {
        let hash_key = derive_hash_key(&key_fn);
        MemCtx {
            issuer: Some(issuer),
            key_fn: Some(Box::new(key_fn)),
            hash_key,
            ..Self::default()
        }
    }

    pub fn set_local(
        &mut self,
        issuer: [u8; 16],
        key_fn: impl Fn(&str, u64) -> Option<[u8; 32]> + 'static,
    ) {
        self.hash_key = derive_hash_key(&key_fn);
        self.issuer = Some(issuer);
        self.key_fn = Some(Box::new(key_fn));
    }

    pub fn set_epoch(&mut self, kind: &str, epoch: u64) {
        self.epochs.insert(kind.to_string(), epoch);
    }

    /// Trust a key for every kind its policy lets come from elsewhere.
    pub fn allow_signer(&mut self, pk: [u8; 32]) {
        self.signers.entry(pk).or_default().insert("*".to_string());
    }

    /// Trust a key for one kind.
    pub fn allow_signer_for(&mut self, pk: [u8; 32], kind: &str) {
        self.signers.entry(pk).or_default().insert(kind.to_string());
    }

    /// Trust a key as a human authority.
    pub fn allow_authority(&mut self, pk: [u8; 32]) {
        self.authorities.insert(pk);
    }

    pub fn set_env(&mut self, name: &[u8], value: Option<&[u8]>) {
        self.env.insert(name.to_vec(), value.map(|v| v.to_vec()));
    }

    /// A tool's version and build now; `cig` sets the running cig.
    pub fn set_tool(&mut self, name: &str, version: &str, build: &str) {
        if name == "cig" {
            self.cig = (version.to_string(), build.to_string());
        }
        self.tools
            .insert(name.to_string(), (version.to_string(), build.to_string()));
    }

    /// Verify, then count one spend of each of its spend keys: what a
    /// caller with a ledger does under its lock.
    pub fn spend_pass(&mut self, p: &Pass) -> Result<crate::verify::Verified, crate::verify::Why> {
        let v = crate::verify::verify(p, self)?;
        for s in &v.spend {
            self.spend(s.key);
        }
        Ok(v)
    }

    pub fn set_rules(&mut self, name: &str, digest: [u8; 32]) {
        self.rules.insert(name.to_string(), digest);
    }

    pub fn revoke(&mut self, id: [u8; 32]) {
        self.revoked.insert(id);
    }

    pub fn spend(&mut self, key: [u8; 32]) {
        *self.spends.entry(key).or_insert(0) += 1;
    }

    pub fn add_pass(&mut self, p: Pass) {
        self.passes.insert(p.id(), p);
    }

    pub fn add_discharge(&mut self, p: Pass) {
        self.discharges.push(p);
    }
}

impl Ctx for MemCtx {
    fn local_key(&self, kind: &str, epoch: u64) -> Option<[u8; 32]> {
        self.key_fn.as_ref().and_then(|f| f(kind, epoch))
    }

    fn local_issuer(&self) -> Option<[u8; 16]> {
        self.issuer
    }

    fn epoch(&self, kind: &str) -> u64 {
        self.epochs.get(kind).copied().unwrap_or(0)
    }

    fn hash_key(&self) -> Option<[u8; 32]> {
        self.hash_key
    }

    fn allowed_signer(&self, pk: &[u8; 32], kind: &str) -> bool {
        self.signers
            .get(pk)
            .is_some_and(|k| k.contains("*") || k.contains(kind))
    }

    fn authority_signer(&self, pk: &[u8; 32]) -> bool {
        self.authorities.contains(pk)
    }

    fn is_own_signer(&self, pk: &[u8; 32]) -> bool {
        self.own_signers.contains(pk)
    }

    fn now(&self) -> Now {
        self.now.unwrap_or_else(Now::read)
    }

    fn env(&self, name: &[u8]) -> Option<Vec<u8>> {
        if let Some(v) = self.env.get(name) {
            return v.clone();
        }
        if self.env_from_process {
            let n = String::from_utf8_lossy(name).into_owned();
            return std::env::var_os(n).map(|v| v.to_string_lossy().into_owned().into_bytes());
        }
        None
    }

    fn tool(&self, name: &str) -> Option<(String, String)> {
        self.tools.get(name).cloned()
    }

    fn rules(&self, name: &str) -> Option<[u8; 32]> {
        self.rules.get(name).copied()
    }

    fn yanked(&self, tool: &str, version: &str, build: &str) -> bool {
        self.yanked
            .contains(&(tool.to_string(), version.to_string(), build.to_string()))
    }

    fn cig(&self) -> (String, String) {
        self.cig.clone()
    }

    fn action(&self) -> &Action {
        &self.action
    }

    fn revoked(&self, id: &[u8; 32]) -> bool {
        self.revoked.contains(id)
    }

    fn spends(&self, key: &[u8; 32]) -> u32 {
        self.spends.get(key).copied().unwrap_or(0)
    }

    fn find_pass(&self, id: &[u8; 32]) -> Option<Pass> {
        self.passes.get(id).cloned()
    }

    fn discharges(&self, kind: &str, _root: &[u8; 32]) -> Vec<Pass> {
        self.discharges
            .iter()
            .filter(|d| d.kind == kind)
            .cloned()
            .collect()
    }
}
