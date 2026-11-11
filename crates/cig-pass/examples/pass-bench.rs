// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The cig-pass budget harness (SPEC section 13, cig-script#186).
//!
//! ```text
//! cargo run --release -p cig-pass --example pass-bench
//! cargo run --release -p cig-pass --example pass-bench -- --gate
//! cargo run --release -p cig-pass --example pass-bench -- --json
//! ```
//!
//! What it measures, in microseconds per call:
//!
//! | measure                                   | budget  |
//! |-------------------------------------------|--------:|
//! | verify, local pass, 10 file inputs, warm  | <= 30   |
//! | attenuate one caveat                      | <= 5    |
//! | issue overhead over reading the 10 files  | <= 5 %  |
//! | store `find` hit among 100,000 passes     | <= 50   |
//! | decode of a 300-byte and a 400-byte pass  | (none)  |
//!
//! How it measures, so a power-saving CPU governor cannot decide the answer:
//!
//! - Every measure is one closure that runs `n` calls back to back. `n` is
//!   calibrated so one batch lasts about 200 us (the clock's resolution
//!   stops mattering), and a sample is the batch time divided by `n`.
//! - Rounds interleave: round r runs one batch of every measure, in an order
//!   that rotates each round. Drift in clock speed lands on all of them
//!   alike, never on "all of A, then all of B".
//! - The issue overhead is the median over rounds of
//!   `issue_i / bare_i - 1`, the two batches of one round taken together.
//! - Output is checked before it is timed (a verify that says no, a find
//!   that misses, a pass that does not round-trip is a bug, not a result),
//!   and a negative control (a changed file must fail verify) proves the
//!   verifier is not answering "yes" to everything.
//! - Files are given an mtime an hour old, so the racy rule (SPEC section 2)
//!   does not force a re-hash: this is the warm path.
//!
//! Percentiles are over the per-batch means, not single calls: a p95 here
//! is the 95th slowest batch of about 200 us, which shows scheduler and
//! governor noise but understates a single call's worst case.
//!
//! Exit: 0 all budgets met (or no `--gate`), 1 a budget missed under
//! `--gate` or a check failed, 3 usage. Everything is written under a
//! scratch folder in the temp directory and removed at the end. Nothing here
//! creates a network namespace.
//!
//! Dependency-free: std only. The crate API is touched in exactly one place,
//! `mod adapter`, so a change to the API is a change to that module.

// `--cfg stub` (the std-only self-test backend) is not a Cargo feature.

use std::fs;
use std::hint::black_box;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

// ---------------------------------------------------------------------------
// The adapter: the only code that names the cig-pass API.
// ---------------------------------------------------------------------------

/// The only code that names the cig-pass API.
mod adapter {
    use cig_pass::store::{Meta, NoGuard, Store};
    use cig_pass::{attenuate, verify, Caveat, Draft, MemCtx, Pass, Recorder};
    use std::fs::Metadata;
    use std::path::Path;

    const ISSUER: [u8; 16] = [7; 16];

    fn key(kind: &str, epoch: u64) -> [u8; 32] {
        blake3::derive_key("cig-pass bench", format!("{kind}:{epoch}").as_bytes())
    }

    fn hash_key() -> [u8; 32] {
        *blake3::keyed_hash(&key("check", u64::MAX), b"cigpass1-hash-key").as_bytes()
    }

    /// The verifying context, built once.
    pub struct World {
        pub ctx: MemCtx,
    }

    pub fn setup(dir: &Path) -> World {
        let _ = dir;
        World {
            ctx: MemCtx::with_local(ISSUER, |k, e| Some(key(k, e))),
        }
    }

    pub type P = Pass;

    /// A recording in progress.
    pub struct Rec(Recorder);

    pub fn rec_new() -> Rec {
        Rec(Recorder::new())
    }

    fn issue_over(subject: &[u8], rec: Recorder, result: Vec<u8>) -> P {
        let draft = Draft {
            kind: "check".to_string(),
            subject: subject.to_vec(),
            inputs: rec.finish().expect("closed-world recording"),
            result,
            epoch: 0,
            ledger_seq: 0,
            issued_ms: cig_pass::wall_ms(),
            mono: cig_pass::mono_now(),
            flags: 0,
        };
        cig_pass::issue_local(&draft, ISSUER, &key("check", 0), &hash_key()).expect("issue")
    }

    impl Rec {
        pub fn file_read(&mut self, path: &Path, meta: &Metadata, bytes: &[u8]) {
            // Read an hour after the files were made: calm, not racy.
            self.0
                .file_read_at(path, meta, bytes, cig_pass::wall_ms() + 3_600_000);
        }
        /// Finish the recording and issue a `check` pass over a fixed subject.
        pub fn issue(self, _w: &World, result: Vec<u8>) -> P {
            issue_over(b"pass-bench", self.0, result)
        }
    }

    pub fn verify_ok(w: &World, p: &P) -> bool {
        verify(p, &w.ctx).is_ok()
    }

    /// One more caveat on a local pass (needs no key: that is the point).
    pub fn attenuate_one(p: &P) -> P {
        attenuate(p, Caveat::Until(4_102_444_800_000)).expect("local pass narrows")
    }

    pub fn caveats(p: &P) -> usize {
        p.caveats.len()
    }

    pub fn encode(p: &P) -> Vec<u8> {
        p.encode()
    }

    pub fn decode_ok(b: &[u8]) -> bool {
        Pass::decode(b).is_ok()
    }

    /// The store, already compacted into segments.
    pub struct Db {
        store: Store,
        subjects: Vec<Vec<u8>>,
    }

    fn subject(i: usize) -> Vec<u8> {
        format!("pass-bench-subject-{i:08}").into_bytes()
    }

    /// Issue `n` passes (distinct subjects, one `File` input each, over
    /// `file`), put them in a store under `dir`, and compact so lookups hit
    /// a segment (cig pass gc). Not timed.
    pub fn store_build(_w: &World, dir: &Path, n: usize, file: &Path) -> Db {
        let store = Store::open(dir, &NoGuard).expect("store opens");
        let bytes = std::fs::read(file).expect("input readable");
        let meta = std::fs::metadata(file).expect("input stat");
        let mut subjects = Vec::with_capacity(n);
        let mut batch: Vec<(Meta, Vec<u8>)> = Vec::new();
        for i in 0..n {
            let mut r = Recorder::new();
            r.file_read_at(file, &meta, &bytes, cig_pass::wall_ms() + 3_600_000);
            let s = subject(i);
            let p = issue_over(&s, r, Vec::new());
            let enc = p.encode();
            batch.push((
                Meta {
                    id: p.id(),
                    kind: p.kind.clone(),
                    subject: p.subject,
                    issued_ms: p.issued_ms,
                    size: enc.len() as u32,
                },
                enc,
            ));
            subjects.push(s);
            if batch.len() == 1000 {
                let items: Vec<(&Meta, &[u8])> =
                    batch.iter().map(|(m, e)| (m, e.as_slice())).collect();
                store.insert_many(&items).expect("stored");
                batch.clear();
            }
        }
        let items: Vec<(&Meta, &[u8])> = batch.iter().map(|(m, e)| (m, e.as_slice())).collect();
        store.insert_many(&items).expect("stored");
        store.compact_full(0, &|_| true).expect("compacted");
        Db { store, subjects }
    }

    /// `find` for the i-th subject: true on a verified hit.
    pub fn store_find(w: &World, db: &Db, i: usize) -> bool {
        let s = &db.subjects[i % db.subjects.len()];
        let digest = cig_pass::subject_digest(s);
        db.store
            .candidates("check", &digest)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(|b| Pass::decode(&b).ok())
            .any(|p| verify(&p, &w.ctx).is_ok())
    }

    pub fn store_len(db: &Db) -> usize {
        db.subjects.len()
    }
}

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

struct Opts {
    samples: usize,
    json: bool,
    gate: bool,
    gate_on: Stat,
    only: Option<String>,
    store_size: usize,
    file_size: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum Stat {
    Median,
    P90,
    P95,
}

const USAGE: &str =
    "usage: pass-bench [--samples N] [--json] [--gate] [--gate-on median|p90|p95]\n\
                     \x20                 [--only NAME] [--store-size N] [--file-size BYTES]\n\
                     defaults: 401 samples, gate on the median, 100000 passes, 1024-byte files";

fn parse_args() -> Result<Opts, String> {
    let mut o = Opts {
        samples: 401,
        json: false,
        gate: false,
        gate_on: Stat::Median,
        only: None,
        store_size: 100_000,
        file_size: 1024,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut val = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "--json" => o.json = true,
            "--gate" => o.gate = true,
            "--samples" => {
                o.samples = val("--samples")?
                    .parse()
                    .map_err(|_| "--samples is a number")?
            }
            "--only" => o.only = Some(val("--only")?),
            "--store-size" => {
                o.store_size = val("--store-size")?
                    .parse()
                    .map_err(|_| "--store-size is a number")?
            }
            "--file-size" => {
                o.file_size = val("--file-size")?
                    .parse()
                    .map_err(|_| "--file-size is a number")?
            }
            "--gate-on" => {
                o.gate_on = match val("--gate-on")?.as_str() {
                    "median" => Stat::Median,
                    "p90" => Stat::P90,
                    "p95" => Stat::P95,
                    _ => return Err("--gate-on is median, p90 or p95".into()),
                }
            }
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if o.samples < 5 {
        return Err("--samples must be at least 5".into());
    }
    if o.store_size < 1 {
        return Err("--store-size must be at least 1".into());
    }
    Ok(o)
}

// ---------------------------------------------------------------------------
// Statistics
// ---------------------------------------------------------------------------

/// Linear-interpolated percentile of an already sorted slice.
fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let pos = p * (sorted.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo as f64)
}

struct Summary {
    median: f64,
    p10: f64,
    p90: f64,
    p95: f64,
    cv: f64,
}

fn summarize(samples: &[f64]) -> Summary {
    let mut s = samples.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = s.iter().sum::<f64>() / s.len() as f64;
    let var = s.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / s.len() as f64;
    Summary {
        median: pct(&s, 0.5),
        p10: pct(&s, 0.10),
        p90: pct(&s, 0.90),
        p95: pct(&s, 0.95),
        cv: if mean > 0.0 { var.sqrt() / mean } else { 0.0 },
    }
}

// ---------------------------------------------------------------------------
// The harness
// ---------------------------------------------------------------------------

struct Measure<'a> {
    name: &'static str,
    /// Budget in microseconds per call; None = reported only.
    budget_us: Option<f64>,
    /// Runs `n` calls back to back.
    run: Box<dyn FnMut(usize) + 'a>,
    n: usize,
    /// Nanoseconds per call, one entry per round.
    samples: Vec<f64>,
}

const BATCH_TARGET_NS: f64 = 200_000.0;

fn time_batch(m: &mut Measure) -> f64 {
    let t = Instant::now();
    (m.run)(m.n);
    t.elapsed().as_nanos() as f64 / m.n as f64
}

fn calibrate(m: &mut Measure) {
    // One call to estimate, then size the batch; a second pass corrects the
    // estimate (the first call is cold).
    for _ in 0..2 {
        m.n = 1;
        let per = time_batch(m).max(1.0);
        m.n = ((BATCH_TARGET_NS / per).ceil() as usize).clamp(1, 1_000_000);
        let per = time_batch(m).max(1.0);
        m.n = ((BATCH_TARGET_NS / per).ceil() as usize).clamp(1, 1_000_000);
    }
}

/// Read a file the way a script's `fs.read_text` does: open, stat the
/// handle, read to the end. The bare work and the issued work both come
/// through here, so the only difference between them is the recording.
fn read_handle(path: &Path, buf: &mut Vec<u8>) -> std::io::Result<fs::Metadata> {
    let mut f = fs::File::open(path)?;
    let meta = f.metadata()?;
    buf.clear();
    f.read_to_end(buf)?;
    Ok(meta)
}

fn make_files(dir: &Path, count: usize, size: usize) -> Result<Vec<PathBuf>, String> {
    let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    let mut out = Vec::new();
    for i in 0..count {
        let p = dir.join(format!("input-{i}.txt"));
        let mut body = Vec::with_capacity(size);
        let mut x = 0x2545f4914f6cdd1du64 ^ i as u64;
        while body.len() < size {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            body.push(b'a' + (x % 26) as u8);
        }
        fs::write(&p, &body).map_err(|e| format!("cannot write {}: {e}", p.display()))?;
        let f = fs::OpenOptions::new()
            .write(true)
            .open(&p)
            .map_err(|e| e.to_string())?;
        f.set_modified(hour_ago)
            .map_err(|e| format!("cannot age {}: {e}", p.display()))?;
        out.push(p);
    }
    Ok(out)
}

fn fail(msg: &str) -> ExitCode {
    eprintln!("pass-bench: {msg}");
    ExitCode::from(1)
}

fn main() -> ExitCode {
    let o = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            if e.is_empty() {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            eprintln!("pass-bench: {e}\n{USAGE}");
            return ExitCode::from(3);
        }
    };
    let scratch = std::env::temp_dir().join(format!("pass-bench-{}", std::process::id()));
    let _ = fs::remove_dir_all(&scratch);
    if let Err(e) = fs::create_dir_all(&scratch) {
        return fail(&format!("cannot make {}: {e}", scratch.display()));
    }
    let code = run(&o, &scratch);
    let _ = fs::remove_dir_all(&scratch);
    code
}

fn run(o: &Opts, scratch: &Path) -> ExitCode {
    let world = adapter::setup(scratch);
    let files = match make_files(scratch, 10, o.file_size) {
        Ok(f) => f,
        Err(e) => return fail(&e),
    };

    // ---- the passes the measures use -------------------------------------
    let issue_ten = |w: &adapter::World| -> Result<adapter::P, String> {
        let mut rec = adapter::rec_new();
        let mut buf = Vec::new();
        for f in &files {
            let meta = read_handle(f, &mut buf).map_err(|e| e.to_string())?;
            rec.file_read(f, &meta, &buf);
        }
        Ok(rec.issue(w, b"{\"ok\":true}".to_vec()))
    };
    let pass10 = match issue_ten(&world) {
        Ok(p) => p,
        Err(e) => return fail(&format!("cannot issue the 10-file pass: {e}")),
    };

    // ---- checks before timing --------------------------------------------
    if !adapter::verify_ok(&world, &pass10) {
        return fail("check failed: a fresh pass over 10 unchanged files does not verify");
    }
    // Negative control: a changed input must fail. Uses its own file so the
    // ten warm files are left alone.
    {
        let neg = scratch.join("negative.txt");
        if fs::write(&neg, b"before").is_err() {
            return fail("cannot write the negative-control file");
        }
        let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        if let Ok(f) = fs::OpenOptions::new().write(true).open(&neg) {
            let _ = f.set_modified(hour_ago);
        }
        let mut rec = adapter::rec_new();
        let mut buf = Vec::new();
        let meta = match read_handle(&neg, &mut buf) {
            Ok(m) => m,
            Err(e) => return fail(&format!("negative control: {e}")),
        };
        rec.file_read(&neg, &meta, &buf);
        let p = rec.issue(&world, Vec::new());
        if !adapter::verify_ok(&world, &p) {
            return fail("check failed: the negative-control pass does not verify when unchanged");
        }
        if fs::write(&neg, b"after, and longer").is_err() {
            return fail("cannot change the negative-control file");
        }
        if adapter::verify_ok(&world, &p) {
            return fail("check failed: a changed input still verifies (the verifier says yes to everything)");
        }
    }
    let narrowed = adapter::attenuate_one(&pass10);
    if adapter::caveats(&narrowed) != adapter::caveats(&pass10) + 1 {
        return fail("check failed: attenuate did not add exactly one caveat");
    }
    if !adapter::verify_ok(&world, &narrowed) {
        return fail("check failed: a pass with a far-future until caveat does not verify");
    }

    // Decode: padded to exactly 300 and 400 bytes through the result field.
    let sized = |target: usize| -> Result<Vec<u8>, String> {
        let base = adapter::encode(&adapter::rec_new().issue(&world, Vec::new()));
        if base.len() > target {
            return Err(format!("an empty pass is already {} bytes", base.len()));
        }
        let padded =
            adapter::encode(&adapter::rec_new().issue(&world, vec![b'x'; target - base.len()]));
        if padded.len() != target {
            return Err(format!("wanted {target} bytes, built {}", padded.len()));
        }
        Ok(padded)
    };
    let (b300, b400) = match (sized(300), sized(400)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => return fail(&format!("cannot size a pass: {e}")),
    };
    if !adapter::decode_ok(&b300) || !adapter::decode_ok(&b400) {
        return fail("check failed: a built pass does not decode");
    }

    // The store: built once, not timed.
    let want = |name: &str| o.only.as_deref().map(|s| name.contains(s)).unwrap_or(true);
    let store_dir = scratch.join("store");
    let db = if want("find") {
        if fs::create_dir_all(&store_dir).is_err() {
            return fail("cannot make the store folder");
        }
        if !o.json {
            eprintln!("building a store of {} passes (not timed)...", o.store_size);
        }
        let t = Instant::now();
        let db = adapter::store_build(&world, &store_dir, o.store_size, &files[0]);
        if !o.json {
            eprintln!("built in {:.1} s", t.elapsed().as_secs_f64());
        }
        for probe in [0, o.store_size / 2, o.store_size - 1] {
            if !adapter::store_find(&world, &db, probe) {
                return fail(&format!(
                    "check failed: find missed pass {probe}, which is in the store"
                ));
            }
        }
        Some(db)
    } else {
        None
    };

    // ---- the measures -----------------------------------------------------
    let mut ms: Vec<Measure> = Vec::new();
    // The issue/bare pair always runs (the overhead row needs both); the
    // rest honour --only.
    fn add<'a>(
        ms: &mut Vec<Measure<'a>>,
        keep: bool,
        name: &'static str,
        budget_us: Option<f64>,
        run: Box<dyn FnMut(usize) + 'a>,
    ) {
        if keep {
            ms.push(Measure {
                name,
                budget_us,
                run,
                n: 1,
                samples: Vec::new(),
            });
        }
    }
    let keep =
        |name: &str| want(name) || name.starts_with("bare work") || name.starts_with("issue:");
    add(
        &mut ms,
        keep("verify, 10 file inputs, warm"),
        "verify, 10 file inputs, warm",
        Some(30.0),
        Box::new(|n| {
            for _ in 0..n {
                black_box(adapter::verify_ok(&world, black_box(&pass10)));
            }
        }),
    );
    add(
        &mut ms,
        keep("attenuate one caveat"),
        "attenuate one caveat",
        Some(5.0),
        Box::new(|n| {
            for _ in 0..n {
                black_box(adapter::attenuate_one(black_box(&pass10)));
            }
        }),
    );
    {
        let files = &files;
        add(
            &mut ms,
            keep("bare work: read 10 files"),
            "bare work: read 10 files",
            None,
            Box::new(move |n| {
                let mut buf = Vec::new();
                for _ in 0..n {
                    for f in files {
                        black_box(read_handle(f, &mut buf).ok());
                        black_box(&buf);
                    }
                }
            }),
        );
    }
    add(
        &mut ms,
        keep("issue: read 10 files and record"),
        "issue: read 10 files and record",
        None,
        Box::new(|n| {
            for _ in 0..n {
                black_box(issue_ten(&world).ok());
            }
        }),
    );
    if let Some(db) = &db {
        let len = adapter::store_len(db);
        let mut cursor = 0usize;
        let world = &world;
        add(
            &mut ms,
            keep("find hit in the store"),
            "find hit in the store",
            Some(50.0),
            Box::new(move |n| {
                for _ in 0..n {
                    // A stride coprime with the store size walks every subject,
                    // so no one index slot stays hot.
                    cursor = (cursor + 7919) % len;
                    black_box(adapter::store_find(world, db, cursor));
                }
            }),
        );
    }
    add(
        &mut ms,
        keep("decode 300-byte pass"),
        "decode 300-byte pass",
        None,
        Box::new(|n| {
            for _ in 0..n {
                black_box(adapter::decode_ok(black_box(&b300)));
            }
        }),
    );
    add(
        &mut ms,
        keep("decode 400-byte pass"),
        "decode 400-byte pass",
        None,
        Box::new(|n| {
            for _ in 0..n {
                black_box(adapter::decode_ok(black_box(&b400)));
            }
        }),
    );

    // Calibrate, then warm up (untimed rounds).
    for m in ms.iter_mut() {
        calibrate(m);
    }
    // The issue and bare measures run the same n, so a round compares like with like.
    let bi = ms.iter().position(|m| m.name == "bare work: read 10 files");
    let ii = ms
        .iter()
        .position(|m| m.name == "issue: read 10 files and record");
    if let (Some(b), Some(i)) = (bi, ii) {
        let n = ms[b].n.min(ms[i].n);
        ms[b].n = n;
        ms[i].n = n;
    }
    for _ in 0..5 {
        for m in ms.iter_mut() {
            time_batch(m);
        }
    }

    // Interleaved rounds; the starting measure rotates each round.
    let count = ms.len();
    for r in 0..o.samples {
        for k in 0..count {
            let m = &mut ms[(r + k) % count];
            let t = time_batch(m);
            m.samples.push(t);
        }
    }

    // ---- results ------------------------------------------------------------
    struct Row {
        name: String,
        unit: &'static str,
        n: usize,
        s: Summary,
        budget: Option<f64>,
        value: f64,
    }
    let mut rows: Vec<Row> = Vec::new();
    for m in &ms {
        let s = summarize(&m.samples.iter().map(|x| x / 1000.0).collect::<Vec<_>>());
        rows.push(Row {
            name: m.name.to_string(),
            unit: "us",
            n: m.n,
            budget: m.budget_us,
            value: 0.0,
            s,
        });
    }
    if let (Some(b), Some(i)) = (bi, ii) {
        // Paired ratios, one per round.
        let ratios: Vec<f64> = ms[i]
            .samples
            .iter()
            .zip(&ms[b].samples)
            .map(|(a, c)| (a / c - 1.0) * 100.0)
            .collect();
        let s = summarize(&ratios);
        rows.push(Row {
            name: "issue overhead on the work".into(),
            unit: "%",
            n: ms[b].n,
            budget: Some(5.0),
            value: 0.0,
            s,
        });
    }
    for r in rows.iter_mut() {
        r.value = match o.gate_on {
            Stat::Median => r.s.median,
            Stat::P90 => r.s.p90,
            Stat::P95 => r.s.p95,
        };
    }
    let missed: Vec<&Row> = rows
        .iter()
        .filter(|r| r.budget.map(|b| r.value > b).unwrap_or(false))
        .collect();

    let cpu = read_first_line("/proc/cpuinfo", "model name");
    let gov = fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown".into());
    let gate_name = match o.gate_on {
        Stat::Median => "median",
        Stat::P90 => "p90",
        Stat::P95 => "p95",
    };

    if o.json {
        let mut s = String::new();
        s.push_str("{\n");
        s.push_str(&format!(
            "  \"cpu\": {},\n  \"governor\": {},\n",
            jstr(&cpu),
            jstr(&gov)
        ));
        s.push_str(&format!(
            "  \"rounds\": {},\n  \"store_size\": {},\n  \"file_size\": {},\n  \"gate_on\": \"{gate_name}\",\n",
            o.samples, o.store_size, o.file_size
        ));
        s.push_str("  \"measures\": [\n");
        for (k, r) in rows.iter().enumerate() {
            let budget = r
                .budget
                .map(|b| format!("{b}"))
                .unwrap_or_else(|| "null".into());
            let met = r
                .budget
                .map(|b| (r.value <= b).to_string())
                .unwrap_or_else(|| "null".into());
            s.push_str(&format!(
                "    {{\"name\": {}, \"unit\": \"{}\", \"calls_per_batch\": {}, \"median\": {:.4}, \"p10\": {:.4}, \"p90\": {:.4}, \"p95\": {:.4}, \"cv\": {:.4}, \"budget\": {budget}, \"met\": {met}}}{}\n",
                jstr(&r.name), r.unit, r.n, r.s.median, r.s.p10, r.s.p90, r.s.p95, r.s.cv,
                if k + 1 < rows.len() { "," } else { "" }
            ));
        }
        s.push_str("  ],\n");
        s.push_str(&format!("  \"all_met\": {}\n}}\n", missed.is_empty()));
        print!("{s}");
    } else {
        println!("pass-bench: {cpu}, governor {gov}");
        println!(
            "{} interleaved rounds, batches of about 200 us, gate on the {gate_name}",
            o.samples
        );
        println!();
        println!(
            "{:<36} {:>9} {:>9} {:>9} {:>9} {:>6} {:>9}",
            "measure", "median", "p10", "p90", "p95", "CV", "budget"
        );
        for r in &rows {
            let budget = r
                .budget
                .map(|b| format!("<= {b}"))
                .unwrap_or_else(|| "-".into());
            let verdict = match r.budget {
                None => "",
                Some(b) if r.value <= b => "ok",
                Some(_) => "MISSED",
            };
            let noisy = if r.s.cv > 0.05 { "~" } else { " " };
            println!(
                "{:<36} {:>9.3} {:>9.3} {:>9.3} {:>9.3} {:>5.1}%{} {:>9}  {}",
                format!("{} ({})", r.name, r.unit),
                r.s.median,
                r.s.p10,
                r.s.p90,
                r.s.p95,
                r.s.cv * 100.0,
                noisy,
                budget,
                verdict
            );
        }
        println!();
        println!("~ marks a CV over 5%: noisy, repeat before believing it.");
    }

    if o.gate && !missed.is_empty() {
        for r in &missed {
            eprintln!(
                "pass-bench: budget missed: {} is {:.3} {} ({}), budget {}",
                r.name,
                r.value,
                r.unit,
                gate_name,
                r.budget.unwrap()
            );
        }
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

fn read_first_line(path: &str, key: &str) -> String {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| {
            s.lines().find(|l| l.starts_with(key)).map(|l| {
                l.split_once(':')
                    .map(|x| x.1)
                    .unwrap_or("")
                    .trim()
                    .to_string()
            })
        })
        .unwrap_or_else(|| "unknown cpu".into())
}

fn jstr(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}
