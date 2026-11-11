// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The marks cache (cig-script#186): the pass state
//! folded from the ledger lines `pass-epoch`, `pass-spend` and
//! `pass-revoke`: each kind's epoch, how many times each spend key has been
//! spent, and which ids are revoked. The ledger is the truth; this file only
//! saves reading all of it, and it is MAC'd so it cannot be edited, rolled
//! back or deleted into a different answer.
//!
//! ```text
//! "CIGMARK2" (8) || version u32 (1) || 0 u32
//!   || ledger seq u64 || ledger byte length u64 || ledger head MAC [32]
//!   || n_spent u32 || n_revoked u32 || n_epochs u32 || 0 u32
//!   || (key [32] || count u32) * n_spent      sorted by key, count > 0
//!   || key [32] * n_revoked                   sorted
//!   || (u8 len || kind || epoch u64) * n_epochs   sorted by kind
//!   || MAC [32] over everything above
//! ```
//!
//! The MAC is computed by a closure the caller supplies (it keys it with
//! its `pass-marks` subkey; this module holds no key). The header says how
//! far into the ledger the cache has read. The caller compares it with the
//! ledger it finds: a longer ledger means replay the new tail and `record`
//! it; a shorter one, or a head that differs, is a truncated or swapped
//! ledger (E739), never a silent rebuild.
//!
//! `load` never turns a file that exists but fails its MAC into an empty
//! cache: that is `Tampered`. Only a missing file (or an older version
//! with a good MAC) is `Stale`, and the caller then folds the whole ledger.

use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::path::Path;

use crate::store::{open_private, owner_of, write_atomic, DirGuard};

const MAGIC: &[u8; 8] = b"CIGMARK2";
const VERSION: u32 = 1;
const HEAD: usize = 80;
const MAC: usize = 32;
const MAX_FILE: u64 = 256 * 1024 * 1024;

/// What a ledger line marks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkKind {
    /// `pass-spend`: one spend of this key.
    Spend,
    /// `pass-revoke`: this id is revoked.
    Revoke,
    /// `pass-epoch`: this kind's epoch goes up by one. The key is unused.
    Epoch(String),
}

/// How far into the ledger the cache has read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LedgerPos {
    /// The sequence number of the last line folded in.
    pub seq: u64,
    /// The ledger's byte length at that line.
    pub byte_len: u64,
    /// The MAC of that last line (the ledger head).
    pub head_mac: [u8; 32],
}

/// What `load` found.
#[derive(Debug, PartialEq, Eq)]
pub enum Loaded {
    /// A cache whose MAC holds.
    Ok(Marks),
    /// No cache, or a version this build does not read: fold the ledger.
    Stale,
    /// The file is there and does not check out (short, edited, rolled
    /// back, written under another key). E739: every pass is invalid until
    /// the ledger chain is verified and the cache rebuilt from it.
    Tampered(String),
}

/// Epochs, spend counts and revoked ids, sorted for binary search.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Marks {
    pos: LedgerPos,
    spent: Vec<([u8; 32], u32)>,
    revoked: Vec<[u8; 32]>,
    epochs: Vec<(String, u64)>,
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

fn le_u32(b: &[u8]) -> u32 {
    let mut a = [0u8; 4];
    a.copy_from_slice(&b[..4]);
    u32::from_le_bytes(a)
}

fn le_u64(b: &[u8]) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[..8]);
    u64::from_le_bytes(a)
}

impl Marks {
    /// Nothing marked, current to the start of an empty ledger.
    pub fn empty() -> Marks {
        Marks::default()
    }

    /// Build from ledger lines read in order, `pos` being where the last of
    /// them sits in the ledger. (The key of an `Epoch` line is ignored.)
    pub fn rebuild(pos: LedgerPos, lines: impl IntoIterator<Item = (MarkKind, [u8; 32])>) -> Marks {
        let mut spent: Vec<[u8; 32]> = Vec::new();
        let mut revoked: Vec<[u8; 32]> = Vec::new();
        let mut epochs: Vec<(String, u64)> = Vec::new();
        for (kind, key) in lines {
            match kind {
                MarkKind::Spend => spent.push(key),
                MarkKind::Revoke => revoked.push(key),
                MarkKind::Epoch(k) => match epochs.iter_mut().find(|(n, _)| *n == k) {
                    Some((_, e)) => *e = e.saturating_add(1),
                    None => epochs.push((k, 1)),
                },
            }
        }
        spent.sort_unstable();
        let mut counted: Vec<([u8; 32], u32)> = Vec::new();
        for k in spent {
            match counted.last_mut() {
                Some((last, n)) if *last == k => *n = n.saturating_add(1),
                _ => counted.push((k, 1)),
            }
        }
        revoked.sort_unstable();
        revoked.dedup();
        epochs.sort();
        Marks {
            pos,
            spent: counted,
            revoked,
            epochs,
        }
    }

    /// Where in the ledger this cache is current to.
    pub fn ledger(&self) -> &LedgerPos {
        &self.pos
    }

    /// The ledger sequence this cache is current to.
    pub fn seq(&self) -> u64 {
        self.pos.seq
    }

    /// Is the cache current to `ledger_seq`? When not, replay the tail.
    pub fn is_current(&self, ledger_seq: u64) -> bool {
        self.pos.seq == ledger_seq
    }

    /// How many times `key` has been spent.
    pub fn spend_count(&self, key: &[u8; 32]) -> u32 {
        match self.spent.binary_search_by(|(k, _)| k.cmp(key)) {
            Ok(i) => self.spent[i].1,
            Err(_) => 0,
        }
    }

    /// Is `id` revoked?
    pub fn is_revoked(&self, id: &[u8; 32]) -> bool {
        self.revoked.binary_search(id).is_ok()
    }

    /// The epoch of `kind` (0 when no `pass-epoch` line names it).
    pub fn epoch(&self, kind: &str) -> u64 {
        match self.epochs.binary_search_by(|(k, _)| k.as_str().cmp(kind)) {
            Ok(i) => self.epochs[i].1,
            Err(_) => 0,
        }
    }

    /// Add one ledger line and move to its position (the caller holds the
    /// ledger lock, so the lines arrive in order).
    /// The ledger has been read to `pos` with nothing in it for the marks.
    pub fn advance(&mut self, pos: LedgerPos) {
        self.pos = pos;
    }

    pub fn record(&mut self, kind: MarkKind, key: [u8; 32], pos: LedgerPos) {
        match kind {
            MarkKind::Spend => match self.spent.binary_search_by(|(k, _)| k.cmp(&key)) {
                Ok(i) => self.spent[i].1 = self.spent[i].1.saturating_add(1),
                Err(i) => self.spent.insert(i, (key, 1)),
            },
            MarkKind::Revoke => {
                if let Err(i) = self.revoked.binary_search(&key) {
                    self.revoked.insert(i, key);
                }
            }
            MarkKind::Epoch(k) => match self.epochs.binary_search_by(|(n, _)| n.cmp(&k)) {
                Ok(i) => self.epochs[i].1 = self.epochs[i].1.saturating_add(1),
                Err(i) => self.epochs.insert(i, (k, 1)),
            },
        }
        self.pos = pos;
    }

    /// Distinct keys spent, ids revoked, kinds with an epoch above 0.
    pub fn counts(&self) -> (usize, usize, usize) {
        (self.spent.len(), self.revoked.len(), self.epochs.len())
    }

    /// The file's bytes, MAC'd by `mac`.
    pub fn encode(&self, mac: &dyn Fn(&[u8]) -> [u8; 32]) -> Vec<u8> {
        let mut b = Vec::with_capacity(
            HEAD + self.spent.len() * 36 + self.revoked.len() * 32 + self.epochs.len() * 20 + MAC,
        );
        b.extend_from_slice(MAGIC);
        b.extend_from_slice(&VERSION.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&self.pos.seq.to_le_bytes());
        b.extend_from_slice(&self.pos.byte_len.to_le_bytes());
        b.extend_from_slice(&self.pos.head_mac);
        b.extend_from_slice(&(self.spent.len() as u32).to_le_bytes());
        b.extend_from_slice(&(self.revoked.len() as u32).to_le_bytes());
        b.extend_from_slice(&(self.epochs.len() as u32).to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        for (k, n) in &self.spent {
            b.extend_from_slice(k);
            b.extend_from_slice(&n.to_le_bytes());
        }
        for k in &self.revoked {
            b.extend_from_slice(k);
        }
        for (k, e) in &self.epochs {
            b.push(k.len() as u8);
            b.extend_from_slice(k.as_bytes());
            b.extend_from_slice(&e.to_le_bytes());
        }
        let m = mac(&b);
        b.extend_from_slice(&m);
        b
    }

    /// Read a file's bytes. The MAC is checked first, in constant time,
    /// before anything in the body is believed.
    pub fn decode(b: &[u8], mac: &dyn Fn(&[u8]) -> [u8; 32]) -> Loaded {
        let t = |why: &str| Loaded::Tampered(why.to_string());
        if b.len() < HEAD + MAC || &b[..8] != MAGIC {
            return t("the marks file is cut short or is not a marks file");
        }
        let (body, tag) = b.split_at(b.len() - MAC);
        if !ct_eq(&mac(body), tag) {
            return t("the marks file does not match its MAC");
        }
        if le_u32(&b[8..]) != VERSION {
            return Loaded::Stale;
        }
        let seq = le_u64(&b[16..]);
        let byte_len = le_u64(&b[24..]);
        let mut head_mac = [0u8; 32];
        head_mac.copy_from_slice(&b[32..64]);
        let ns = le_u32(&b[64..]) as usize;
        let nr = le_u32(&b[68..]) as usize;
        let ne = le_u32(&b[72..]) as usize;
        // The counts must fit in the bytes that are there, before any
        // vector is sized from them.
        let fixed = ns
            .checked_mul(36)
            .and_then(|x| x.checked_add(nr.checked_mul(32)?))
            .and_then(|x| x.checked_add(HEAD));
        let Some(fixed) = fixed.filter(|f| *f <= body.len()) else {
            return t("the marks counts do not fit the file");
        };
        let mut p = HEAD;
        let mut spent: Vec<([u8; 32], u32)> = Vec::with_capacity(ns);
        for _ in 0..ns {
            let mut k = [0u8; 32];
            k.copy_from_slice(&b[p..p + 32]);
            let n = le_u32(&b[p + 32..]);
            if n == 0 || spent.last().is_some_and(|(l, _)| *l >= k) {
                return t("the spend keys are not sorted");
            }
            spent.push((k, n));
            p += 36;
        }
        let mut revoked: Vec<[u8; 32]> = Vec::with_capacity(nr);
        for _ in 0..nr {
            let mut k = [0u8; 32];
            k.copy_from_slice(&b[p..p + 32]);
            if revoked.last().is_some_and(|l| *l >= k) {
                return t("the revoked ids are not sorted");
            }
            revoked.push(k);
            p += 32;
        }
        debug_assert_eq!(p, fixed);
        let mut epochs: Vec<(String, u64)> = Vec::new();
        for _ in 0..ne {
            if p >= body.len() {
                return t("the epochs are cut short");
            }
            let kl = b[p] as usize;
            p += 1;
            if kl == 0 || p + kl + 8 > body.len() {
                return t("the epochs are cut short");
            }
            let Ok(k) = std::str::from_utf8(&b[p..p + kl]) else {
                return t("an epoch kind is not text");
            };
            let e = le_u64(&b[p + kl..]);
            if epochs.last().is_some_and(|(l, _)| l.as_str() >= k) || e == 0 {
                return t("the epochs are not sorted");
            }
            epochs.push((k.to_string(), e));
            p += kl + 8;
        }
        if p != body.len() {
            return t("the marks file has bytes after its last record");
        }
        Loaded::Ok(Marks {
            pos: LedgerPos {
                seq,
                byte_len,
                head_mac,
            },
            spent,
            revoked,
            epochs,
        })
    }

    /// Load `path`. Missing is `Stale`; present and wrong is `Tampered`;
    /// only a refusal by the guard or an I/O failure is an error.
    pub fn load(
        path: &Path,
        guard: &dyn DirGuard,
        mac: &dyn Fn(&[u8]) -> [u8; 32],
    ) -> io::Result<Loaded> {
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        guard.ensure_dir(dir)?;
        let owner = owner_of(dir)?;
        let mut o = OpenOptions::new();
        o.read(true);
        let f = match open_private(path, o, owner) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Loaded::Stale),
            Err(e) => return Err(e),
        };
        if f.metadata()?.len() > MAX_FILE {
            return Ok(Loaded::Tampered("the marks file is too large".to_string()));
        }
        let mut buf = Vec::new();
        f.take(MAX_FILE).read_to_end(&mut buf)?;
        Ok(Marks::decode(&buf, mac))
    }

    /// Save to `path` by temp and rename (0600, fsync of file and folder).
    pub fn save(
        &self,
        path: &Path,
        guard: &dyn DirGuard,
        mac: &dyn Fn(&[u8]) -> [u8; 32],
    ) -> io::Result<()> {
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        guard.ensure_dir(dir)?;
        let owner = owner_of(dir)?;
        write_atomic(path, &self.encode(mac), owner)
    }

    /// Remove the cache file. Do this only to rebuild from a ledger whose
    /// chain has been verified.
    pub fn forget(path: &Path) -> io::Result<()> {
        match fs::remove_file(path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::NoGuard;

    fn k(n: u8) -> [u8; 32] {
        [n; 32]
    }

    fn mac_under(key: u8) -> impl Fn(&[u8]) -> [u8; 32] {
        move |b: &[u8]| *blake3::keyed_hash(&[key; 32], b).as_bytes()
    }

    fn pos(seq: u64) -> LedgerPos {
        LedgerPos {
            seq,
            byte_len: seq * 100,
            head_mac: [seq as u8; 32],
        }
    }

    fn sample() -> Marks {
        Marks::rebuild(
            pos(9),
            [
                (MarkKind::Spend, k(3)),
                (MarkKind::Spend, k(3)),
                (MarkKind::Revoke, k(8)),
                (MarkKind::Epoch("verdict".into()), [0; 32]),
                (MarkKind::Epoch("verdict".into()), [0; 32]),
                (MarkKind::Epoch("approval".into()), [0; 32]),
            ],
        )
    }

    #[test]
    fn rebuild_counts_revokes_and_epochs() {
        let m = sample();
        assert_eq!(m.seq(), 9);
        assert_eq!(m.ledger(), &pos(9));
        assert_eq!(m.spend_count(&k(3)), 2);
        assert_eq!(m.spend_count(&k(1)), 0);
        assert!(m.is_revoked(&k(8)) && !m.is_revoked(&k(3)));
        assert_eq!(m.epoch("verdict"), 2);
        assert_eq!(m.epoch("approval"), 1);
        assert_eq!(m.epoch("run"), 0);
        assert_eq!(m.counts(), (1, 1, 2));
        assert!(m.is_current(9) && !m.is_current(10));
    }

    #[test]
    fn record_keeps_order_and_moves_the_position() {
        let mut m = Marks::empty();
        m.record(MarkKind::Spend, k(5), pos(1));
        m.record(MarkKind::Spend, k(2), pos(2));
        m.record(MarkKind::Spend, k(5), pos(3));
        m.record(MarkKind::Revoke, k(4), pos(4));
        m.record(MarkKind::Epoch("run".into()), [0; 32], pos(5));
        m.record(MarkKind::Epoch("check".into()), [0; 32], pos(6));
        m.record(MarkKind::Epoch("run".into()), [0; 32], pos(7));
        assert_eq!(m.spend_count(&k(5)), 2);
        assert!(m.is_revoked(&k(4)));
        assert_eq!((m.epoch("run"), m.epoch("check")), (2, 1));
        assert_eq!(m.ledger(), &pos(7));
        let mac = mac_under(1);
        assert_eq!(Marks::decode(&m.encode(&mac), &mac), Loaded::Ok(m));
    }

    #[test]
    fn save_and_load_round_trip() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("marks");
        let mac = mac_under(1);
        let m = sample();
        m.save(&p, &NoGuard, &mac).unwrap();
        assert_eq!(Marks::load(&p, &NoGuard, &mac).unwrap(), Loaded::Ok(m));
    }

    #[test]
    fn missing_is_stale_not_empty() {
        let d = tempfile::tempdir().unwrap();
        let r = Marks::load(&d.path().join("marks"), &NoGuard, &mac_under(1)).unwrap();
        assert_eq!(r, Loaded::Stale);
    }

    #[test]
    fn another_key_or_any_edit_is_tampered_never_ok_never_empty() {
        let good = sample().encode(&mac_under(1));
        assert!(matches!(
            Marks::decode(&good, &mac_under(2)),
            Loaded::Tampered(_)
        ));
        for n in 0..good.len() {
            assert!(
                matches!(
                    Marks::decode(&good[..n], &mac_under(1)),
                    Loaded::Tampered(_)
                ),
                "cut at {n}"
            );
        }
        for i in 0..good.len() {
            let mut b = good.clone();
            b[i] ^= 0x55;
            assert!(
                matches!(Marks::decode(&b, &mac_under(1)), Loaded::Tampered(_)),
                "flip at {i}"
            );
        }
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("marks");
        fs::write(
            &p,
            b"not a marks file at all, but long enough to try, really, yes",
        )
        .unwrap();
        assert!(matches!(
            Marks::load(&p, &NoGuard, &mac_under(1)).unwrap(),
            Loaded::Tampered(_)
        ));
        // An older authentic copy is a good file; the ledger position in it
        // is what tells the caller it is behind.
        let old = Marks::rebuild(pos(2), [(MarkKind::Spend, k(1))]);
        fs::write(&p, old.encode(&mac_under(1))).unwrap();
        match Marks::load(&p, &NoGuard, &mac_under(1)).unwrap() {
            Loaded::Ok(m) => assert_eq!(m.ledger(), &pos(2)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_good_mac_over_a_bad_body_is_still_refused() {
        let mac = mac_under(1);
        // Counts that do not fit.
        let mut b = sample().encode(&mac)[..HEAD].to_vec();
        b[64..68].copy_from_slice(&u32::MAX.to_le_bytes());
        let m = mac(&b);
        b.extend_from_slice(&m);
        assert!(matches!(Marks::decode(&b, &mac), Loaded::Tampered(_)));
        // Unsorted spend keys.
        let mut m = sample();
        m.spent = vec![(k(5), 1), (k(2), 1)];
        assert!(matches!(
            Marks::decode(&m.encode(&mac), &mac),
            Loaded::Tampered(_)
        ));
        // Unsorted epochs.
        let mut m = sample();
        m.epochs = vec![("run".into(), 1), ("check".into(), 1)];
        assert!(matches!(
            Marks::decode(&m.encode(&mac), &mac),
            Loaded::Tampered(_)
        ));
        // An epoch of 0 (it would hide as "no line").
        let mut m = sample();
        m.epochs = vec![("run".into(), 0)];
        assert!(matches!(
            Marks::decode(&m.encode(&mac), &mac),
            Loaded::Tampered(_)
        ));
    }

    #[test]
    fn an_unknown_version_with_a_good_mac_is_stale() {
        let mac = mac_under(1);
        let mut b = sample().encode(&mac);
        let n = b.len() - MAC;
        b.truncate(n);
        b[8..12].copy_from_slice(&7u32.to_le_bytes());
        let m = mac(&b);
        b.extend_from_slice(&m);
        assert_eq!(Marks::decode(&b, &mac), Loaded::Stale);
    }

    #[test]
    fn lookup_among_many() {
        let lines = (0..50_000u32).map(|i| {
            let mut key = [0u8; 32];
            key[..4].copy_from_slice(&i.to_be_bytes());
            (MarkKind::Spend, key)
        });
        let m = Marks::rebuild(pos(50_000), lines);
        let mut key = [0u8; 32];
        key[..4].copy_from_slice(&777u32.to_be_bytes());
        assert_eq!(m.spend_count(&key), 1);
        assert_eq!(m.counts().0, 50_000);
        let mac = mac_under(1);
        let t = std::time::Instant::now();
        assert!(matches!(
            Marks::decode(&m.encode(&mac), &mac),
            Loaded::Ok(_)
        ));
        println!("marks encode+decode+MAC, 50000 spends: {:?}", t.elapsed());
    }
}
