// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! `never_more_permissive`: for random sets of files, folders and variables,
//! random work that reads a random subset of them (recorded through
//! `Recorder`), and random mutations of the world afterwards, `verify` may
//! say yes only when redoing the work gives the same observation.
//!
//! Registered in `crates/cig-pass/Cargo.toml` (cargo does not discover a
//! test in a subfolder). Runs in plain `cargo test`: 500 cases, a seeded splitmix64 written inline.
//! A failure prints the case number and its seed; replay one case with
//! `CIG_PASS_PROP_SEED=<seed> CIG_PASS_PROP_CASES=1 cargo test -p cig-pass
//! --test never_more_permissive`.
//!
//! The observation ("transcript") is the work's result: every byte read,
//! every listing, every presence answer and every variable value, in the
//! order the work saw them. Same transcript = same result and same inputs
//! in the sense that matters. (Stat identity legitimately differs after a
//! touch, so the raw `Input` list is not compared.)
//!
//! Also checked, so the property is not vacuous: an untouched world always
//! verifies; and across the run some mutated worlds are accepted (the
//! mutation did not change what the work saw) and some are refused.
//!
//! The helpers below build contexts and issue passes the way `tests/pass.rs`
//! does. Unix only: the stat-identity attacks need ctime.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use cig_pass::{verify, Draft, EntryType, Input, IssueError, MemCtx, Pass, PresentKind, Recorder};

mod adapter {
    use super::*;
    pub const ISSUER: [u8; 16] = [9; 16];

    #[derive(Clone, Copy)]
    pub enum Kind {
        Check,
    }

    pub type Ctx = MemCtx;

    fn key_for(kind: &str, epoch: u64) -> [u8; 32] {
        blake3::derive_key("cig-pass tests", format!("{kind}:{epoch}").as_bytes())
    }

    pub fn ctx() -> Ctx {
        MemCtx::with_local(ISSUER, |k, e| Some(key_for(k, e)))
    }

    pub fn set_env(c: &mut Ctx, name: &str, value: Option<&str>) {
        c.set_env(name.as_bytes(), value.map(|v| v.as_bytes()));
    }

    pub struct IssueKeys;

    pub fn keys() -> IssueKeys {
        IssueKeys
    }

    pub fn issue(
        _kind: Kind,
        subject: &[u8],
        inputs: Vec<Input>,
        result: Vec<u8>,
        _keys: &IssueKeys,
    ) -> Result<Pass, IssueError> {
        let draft = Draft {
            kind: "check".to_string(),
            subject: subject.to_vec(),
            inputs,
            result,
            epoch: 0,
            ledger_seq: 0,
            issued_ms: cig_pass::wall_ms(),
            mono: cig_pass::mono_now(),
            flags: 0,
        };
        let hash_key =
            *blake3::keyed_hash(&key_for("check", u64::MAX), b"cigpass1-hash-key").as_bytes();
        cig_pass::issue_local(&draft, ISSUER, &key_for("check", 0), &hash_key)
    }
}
use adapter::*;

// ----- the seeded generator -----------------------------------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // splitmix64
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

// ----- the world ------------------------------------------------------------------

const N_FILES: usize = 6;
const N_DIRS: usize = 3;
const N_ENTRIES: usize = 4;
const ENV_NAMES: [&str; 3] = ["CIGPASS_PROP_A", "CIGPASS_PROP_B", "CIGPASS_PROP_C"];
const ENV_VALUES: [&str; 3] = ["x", "y", "zz"];

struct World {
    root: PathBuf,
    env: BTreeMap<&'static str, Option<String>>,
    /// Every file starts an hour old so the racy rule does not hide the
    /// stat-identity path; mutations may put this mtime back.
    old: SystemTime,
}

impl World {
    fn file(&self, i: usize) -> PathBuf {
        self.root.join("files").join(format!("f{i}"))
    }
    fn dir(&self, i: usize) -> PathBuf {
        self.root.join(format!("d{i}"))
    }
    fn entry(&self, d: usize, e: usize) -> PathBuf {
        self.dir(d).join(format!("e{e}"))
    }
    fn backdate(&self, p: &Path) {
        if let Ok(f) = File::options().write(true).open(p) {
            let _ = f.set_modified(self.old);
        }
    }
    fn put(&self, p: &Path, content: &str) {
        fs::write(p, content).unwrap();
        self.backdate(p);
    }
    fn ctx(&self) -> Ctx {
        let mut c = ctx();
        for (k, v) in &self.env {
            set_env(&mut c, k, v.as_deref());
        }
        c
    }
}

fn random_content(r: &mut Rng, len: usize) -> String {
    (0..len)
        .map(|_| (b'a' + r.below(4) as u8) as char)
        .collect()
}

fn build_world(r: &mut Rng, root: &Path) -> World {
    let w = World {
        root: root.to_path_buf(),
        env: ENV_NAMES
            .iter()
            .map(|n| {
                let v = if r.chance(70) {
                    Some(ENV_VALUES[r.below(ENV_VALUES.len())].to_string())
                } else {
                    None
                };
                (*n, v)
            })
            .collect(),
        old: SystemTime::now() - Duration::from_secs(3600),
    };
    fs::create_dir_all(root.join("files")).unwrap();
    for i in 0..N_FILES {
        if r.chance(80) {
            let len = 1 + r.below(4);
            w.put(&w.file(i), &random_content(r, len));
        }
    }
    for d in 0..N_DIRS {
        if r.chance(85) {
            fs::create_dir_all(w.dir(d)).unwrap();
            for e in 0..N_ENTRIES {
                if r.chance(50) {
                    w.put(&w.entry(d, e), &random_content(r, 2));
                }
            }
        }
    }
    w
}

// ----- the work ---------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Op {
    ReadFile(usize),
    ListDir(usize),
    Exists(usize),
    Env(usize),
}

fn random_ops(r: &mut Rng) -> Vec<Op> {
    let n = 1 + r.below(6);
    (0..n)
        .map(|_| match r.below(4) {
            0 => Op::ReadFile(r.below(N_FILES)),
            1 => Op::ListDir(r.below(N_DIRS)),
            2 => Op::Exists(r.below(N_FILES)),
            _ => Op::Env(r.below(ENV_NAMES.len())),
        })
        .collect()
}

/// Do the work against the world: record what it looks at, return what it saw.
fn work(w: &World, ops: &[Op], rec: &mut Recorder) -> Vec<u8> {
    let mut seen = String::new();
    for op in ops {
        match *op {
            Op::ReadFile(i) => {
                let p = w.file(i);
                match File::open(&p) {
                    Ok(mut f) => {
                        let meta = f.metadata().unwrap();
                        if meta.is_dir() {
                            // Opening a folder succeeds on Unix; reading it
                            // does not. The work sees an error: open-world.
                            rec.open_world(format!("read of a folder: {}", p.display()));
                            seen.push_str(&format!("F{i}:dir;"));
                            continue;
                        }
                        let mut bytes = Vec::new();
                        f.read_to_end(&mut bytes).unwrap();
                        rec.file_read(&p, &meta, &bytes);
                        seen.push_str(&format!("F{i}:{};", String::from_utf8_lossy(&bytes)));
                    }
                    Err(_) => {
                        rec.absent(&p);
                        seen.push_str(&format!("F{i}:absent;"));
                    }
                }
            }
            Op::ListDir(i) => {
                let p = w.dir(i);
                match fs::read_dir(&p) {
                    Ok(rd) => {
                        let mut entries: Vec<(Vec<u8>, EntryType)> = rd
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
                        rec.dir_listed(&p, &entries);
                        let names: Vec<String> = entries
                            .iter()
                            .map(|(n, t)| format!("{}/{:?}", String::from_utf8_lossy(n), t))
                            .collect();
                        seen.push_str(&format!("L{i}:{};", names.join(",")));
                    }
                    Err(_) => {
                        rec.absent(&p);
                        seen.push_str(&format!("L{i}:absent;"));
                    }
                }
            }
            Op::Exists(i) => {
                let p = w.file(i);
                match fs::metadata(&p) {
                    Ok(m) => {
                        let (k, tag) = if m.is_file() {
                            (PresentKind::File, "file")
                        } else if m.is_dir() {
                            (PresentKind::Dir, "dir")
                        } else {
                            (PresentKind::Other, "other")
                        };
                        rec.present(&p, k);
                        seen.push_str(&format!("P{i}:{tag};"));
                    }
                    Err(_) => {
                        rec.absent(&p);
                        seen.push_str(&format!("P{i}:absent;"));
                    }
                }
            }
            Op::Env(i) => {
                let name = ENV_NAMES[i];
                let v = w.env[name].as_deref();
                rec.env(name.as_bytes(), v.map(str::as_bytes));
                seen.push_str(&format!("V{name}:{v:?};"));
            }
        }
    }
    seen.into_bytes()
}

// ----- the mutations ------------------------------------------------------------------

fn mutate(r: &mut Rng, w: &mut World) -> String {
    let i = r.below(N_FILES);
    let d = r.below(N_DIRS);
    let e = r.below(N_ENTRIES);
    let restore = r.chance(50);
    match r.below(12) {
        0 => {
            // Same size, different bytes, mtime maybe put back.
            let p = w.file(i);
            if let Ok(old) = fs::read_to_string(&p) {
                let new: String = old
                    .chars()
                    .map(|c| if c == 'a' { 'b' } else { 'a' })
                    .collect();
                fs::write(&p, &new).unwrap();
                if restore {
                    w.backdate(&p);
                }
            }
            format!("same-size edit of f{i}, mtime restored: {restore}")
        }
        1 => {
            let p = w.file(i);
            if p.is_file() {
                fs::write(&p, random_content(r, 7)).unwrap();
                if restore {
                    w.backdate(&p);
                }
            }
            format!("longer edit of f{i}, mtime restored: {restore}")
        }
        2 => {
            let _ = fs::remove_file(w.file(i));
            format!("delete f{i}")
        }
        3 => {
            let p = w.file(i);
            if !p.exists() {
                w.put(&p, &random_content(r, 2));
            }
            format!("create f{i}")
        }
        4 => {
            // Touch: mtime only, content equal.
            let p = w.file(i);
            if let Ok(f) = File::options().write(true).open(&p) {
                let _ = f.set_modified(SystemTime::now());
            }
            format!("touch f{i}")
        }
        5 => {
            if w.dir(d).is_dir() {
                w.put(&w.entry(d, e), "new");
            }
            format!("add d{d}/e{e}")
        }
        6 => {
            let _ = fs::remove_file(w.entry(d, e));
            format!("remove d{d}/e{e}")
        }
        7 => {
            let n = ENV_NAMES[r.below(ENV_NAMES.len())];
            let v = if r.chance(70) {
                Some(ENV_VALUES[r.below(ENV_VALUES.len())].to_string())
            } else {
                None
            };
            w.env.insert(n, v.clone());
            format!("env {n} = {v:?}")
        }
        8 => {
            // A file becomes a folder of the same name.
            let p = w.file(i);
            let _ = fs::remove_file(&p);
            let _ = fs::create_dir(&p);
            format!("f{i} replaced by a folder")
        }
        9 => {
            // Rewritten with the same bytes: identity moves, content does not.
            let p = w.file(i);
            if let Ok(old) = fs::read(&p) {
                fs::write(&p, old).unwrap();
                if restore {
                    w.backdate(&p);
                }
            }
            format!("rewrite f{i} with the same bytes")
        }
        10 => {
            // A whole listed folder removed or recreated empty.
            let _ = fs::remove_dir_all(w.dir(d));
            if r.chance(50) {
                fs::create_dir_all(w.dir(d)).unwrap();
            }
            format!("d{d} removed or emptied")
        }
        _ => {
            // Swap two files' contents through rename (inode moves).
            let a = w.file(i);
            let b = w.file((i + 1) % N_FILES);
            if a.is_file() && b.is_file() {
                let tmp = w.root.join("swap.tmp");
                fs::rename(&a, &tmp).unwrap();
                fs::rename(&b, &a).unwrap();
                fs::rename(&tmp, &b).unwrap();
            }
            format!("swap f{i} and f{}", (i + 1) % N_FILES)
        }
    }
}

// ----- the property ---------------------------------------------------------------------

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[test]
fn never_more_permissive() {
    let cases = env_u64("CIG_PASS_PROP_CASES", 500);
    let base = env_u64("CIG_PASS_PROP_SEED", 0);
    let mut accepted_after_mutation = 0u32;
    let mut refused = 0u32;
    let mut untouched = 0u32;

    for case in 0..cases {
        // With an explicit seed and one case, replay exactly that seed.
        let seed = if base != 0 && cases == 1 {
            base
        } else {
            0xC16_9A55_0000_0000u64 ^ (base << 8) ^ case
        };
        let mut r = Rng(seed);
        let dir = tempfile::tempdir().unwrap();
        let mut w = build_world(&mut r, dir.path());
        let ops = random_ops(&mut r);

        let mut rec = Recorder::new();
        let before = work(&w, &ops, &mut rec);
        let inputs = match rec.finish() {
            Ok(i) => i,
            Err(_) => continue, // open-world work (a folder read): no pass, nothing to check
        };
        let pass = issue(Kind::Check, b"prop", inputs, before.clone(), &keys())
            .unwrap_or_else(|e| panic!("seed {seed:#x} case {case}: issue failed: {e}"));

        // Zero to three mutations.
        let n_mut = if r.chance(15) { 0 } else { 1 + r.below(3) };
        let mut log = Vec::new();
        for _ in 0..n_mut {
            log.push(mutate(&mut r, &mut w));
        }

        let verdict = verify(&pass, &w.ctx());

        // Redo the work and see what it sees now.
        let mut rec2 = Recorder::new();
        let after = work(&w, &ops, &mut rec2);

        let context = || {
            format!(
                "seed {seed:#x}, case {case}\n ops: {ops:?}\n mutations: {log:?}\n before: {}\n after:  {}",
                String::from_utf8_lossy(&before),
                String::from_utf8_lossy(&after)
            )
        };

        match &verdict {
            Ok(_) => {
                assert_eq!(
                    before,
                    after,
                    "verify said yes but redoing the work gives another result\n{}",
                    context()
                );
                if n_mut > 0 {
                    accepted_after_mutation += 1;
                }
            }
            Err(why) => {
                assert!(
                    n_mut > 0,
                    "an untouched world was refused: {why}\n{}",
                    context()
                );
                refused += 1;
            }
        }
        if n_mut == 0 {
            untouched += 1;
        }
    }

    if cases >= 100 {
        assert!(untouched > 0, "no untouched cases ran");
        assert!(
            refused > 20,
            "the mutations never refused anything: {refused}"
        );
        assert!(
            accepted_after_mutation > 5,
            "no benign mutation was ever accepted ({accepted_after_mutation}): the generator is too violent to test anything"
        );
    }
}
