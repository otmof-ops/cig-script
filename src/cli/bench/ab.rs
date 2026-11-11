// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! `cig bench --save` and `--against` (#179, step 2).
//!
//! A run is kept as a baseline, `.cig-bench/baselines/<name>.json` in the
//! current directory, and a later run is compared with something older:
//! a baseline, a git ref (built in a temporary worktree through the
//! manifest's `[bench].build`), a release (by version, its sha256 checked
//! before anything runs), or a binary.
//!
//! The comparison is interleaved: the two sides take their samples in turn,
//! and the order inside each pair alternates (A B, B A, A B, ...), so a
//! machine warming up, or a neighbour starting, lands on both sides alike
//! and a drift cancels instead of showing as a difference. The verdict is a
//! Mann-Whitney U test: `faster` or `slower` only when p < 0.01 and the
//! median moved by more than 2%, otherwise `same`.

use super::{
    exit, max_opt, prepare, stats_json, Conditions, Job, Outcome, Prepared, Row, Side, Stats,
    FORMAT,
};
use crate::update::{self, Version};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A change smaller than this is `same` however small p is.
const MIN_CHANGE: f64 = 0.02;
/// A difference is real only below this p-value.
const ALPHA: f64 = 0.01;

// ----- the statistics --------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Verdict {
    Same,
    Faster,
    Slower,
}

impl Verdict {
    pub fn word(self) -> &'static str {
        match self {
            Verdict::Same => "same",
            Verdict::Faster => "faster",
            Verdict::Slower => "SLOWER",
        }
    }

    fn json(self) -> &'static str {
        match self {
            Verdict::Same => "same",
            Verdict::Faster => "faster",
            Verdict::Slower => "slower",
        }
    }
}

/// The result of comparing side B with side A.
#[derive(Debug, Clone, Copy)]
pub struct Comparison {
    pub median_a: f64,
    /// `median_b / median_a - 1`: +0.10 is B ten percent slower.
    pub change: f64,
    /// Two-sided Mann-Whitney p-value.
    pub p: f64,
    /// Rank-biserial correlation, -1 to 1: the share of (A, B) pairs where B
    /// is the slower one, minus the share where it is the faster one.
    pub effect: f64,
    pub verdict: Verdict,
}

fn median(v: &[f64]) -> f64 {
    Stats::of(v, None).median_ms
}

/// Compare per-sample timings of B against A.
pub fn compare(a: &[f64], b: &[f64]) -> Comparison {
    let (median_a, median_b) = (median(a), median(b));
    let change = if median_a > 0.0 {
        median_b / median_a - 1.0
    } else {
        0.0
    };
    let (p, effect) = mann_whitney(a, b);
    let verdict = if p < ALPHA && change.abs() > MIN_CHANGE {
        if change > 0.0 {
            Verdict::Slower
        } else {
            Verdict::Faster
        }
    } else {
        Verdict::Same
    };
    Comparison {
        median_a,
        change,
        p,
        effect,
        verdict,
    }
}

/// Two-sided Mann-Whitney U: the exact p-value when no two timings tie and
/// the samples are small, otherwise the normal approximation with the tie
/// correction. Returns `(p, rank-biserial effect)`; the effect is positive
/// when B's timings are the larger.
pub fn mann_whitney(a: &[f64], b: &[f64]) -> (f64, f64) {
    let (n, m) = (a.len(), b.len());
    if n == 0 || m == 0 {
        return (1.0, 0.0);
    }
    // U_b: the pairs where B is larger, ties counting half.
    let mut u_b = 0.0f64;
    for x in a {
        for y in b {
            u_b += match y.total_cmp(x) {
                std::cmp::Ordering::Greater => 1.0,
                std::cmp::Ordering::Equal => 0.5,
                std::cmp::Ordering::Less => 0.0,
            };
        }
    }
    let pairs = (n * m) as f64;
    let effect = 2.0 * u_b / pairs - 1.0;
    let mut all: Vec<f64> = a.iter().chain(b.iter()).copied().collect();
    all.sort_by(|x, y| x.total_cmp(y));
    // Tie groups, for the correction.
    let mut tie_term = 0.0;
    let mut tied = false;
    let mut i = 0;
    while i < all.len() {
        let mut j = i + 1;
        while j < all.len() && all[j] == all[i] {
            j += 1;
        }
        let t = (j - i) as f64;
        if t > 1.0 {
            tied = true;
            tie_term += t * t * t - t;
        }
        i = j;
    }
    let p = if !tied && n * m <= 400 {
        exact_p(n, m, u_b as usize)
    } else {
        let nn = (n + m) as f64;
        let var = pairs / 12.0 * ((nn + 1.0) - tie_term / (nn * (nn - 1.0)));
        if var <= 0.0 {
            1.0
        } else {
            let z = ((u_b - pairs / 2.0).abs() - 0.5).max(0.0) / var.sqrt();
            erfc(z / std::f64::consts::SQRT_2)
        }
    };
    (p.clamp(0.0, 1.0), effect)
}

/// The exact two-sided p for U = `u` with sizes `n` and `m` and no ties:
/// the share of the C(n+m, n) equally likely orders that are at least as
/// far from the middle.
fn exact_p(n: usize, m: usize, u: usize) -> f64 {
    let top = n * m;
    // c[i][j][k]: orders of i and j timings with U = k.
    let w = top + 1;
    let idx = |i: usize, j: usize, k: usize| (i * (m + 1) + j) * w + k;
    let mut c = vec![0f64; (n + 1) * (m + 1) * w];
    for i in 0..=n {
        for j in 0..=m {
            for k in 0..=(i * j) {
                c[idx(i, j, k)] = if i == 0 || j == 0 {
                    if k == 0 {
                        1.0
                    } else {
                        0.0
                    }
                } else {
                    let from_i = if k >= j { c[idx(i - 1, j, k - j)] } else { 0.0 };
                    let from_j = if k <= i * (j - 1) {
                        c[idx(i, j - 1, k)]
                    } else {
                        0.0
                    };
                    from_i + from_j
                };
            }
        }
    }
    let total: f64 = (0..=top).map(|k| c[idx(n, m, k)]).sum();
    let lo = u.min(top - u);
    let tail: f64 = (0..=lo).map(|k| c[idx(n, m, k)]).sum();
    (2.0 * tail / total).min(1.0)
}

/// Complementary error function (Abramowitz and Stegun 7.1.26; absolute
/// error under 1.5e-7, far below the 0.01 the verdict cuts at).
fn erfc(x: f64) -> f64 {
    let z = x.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * z);
    let poly = t
        * (0.254_829_592
            + t * (-0.284_496_736
                + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    let r = poly * (-z * z).exp();
    if x >= 0.0 {
        r
    } else {
        2.0 - r
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Which {
    A,
    B,
}

/// Take `n` samples from each side in turn. The order alternates by pair
/// (A B, B A, A B, ...), so a linear drift (a CPU warming up) falls on both
/// sides equally; with a fixed A-then-B order, B would always be one step
/// later. `take(side, i)` returns one sample's milliseconds.
pub fn interleave<E>(
    n: usize,
    mut take: impl FnMut(Which, usize) -> Result<f64, E>,
) -> Result<(Vec<f64>, Vec<f64>), E> {
    let mut a = Vec::with_capacity(n);
    let mut b = Vec::with_capacity(n);
    for i in 0..n {
        let order = if i % 2 == 0 {
            [Which::A, Which::B]
        } else {
            [Which::B, Which::A]
        };
        for w in order {
            let t = take(w, i)?;
            match w {
                Which::A => a.push(t),
                Which::B => b.push(t),
            }
        }
    }
    Ok((a, b))
}

// ----- baselines -------------------------------------------------------------

/// A baseline name is a plain file stem: no separators, no leading dot.
pub fn check_baseline_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "`{name}` is not a baseline name: letters, digits, `-`, `_` and `.`, not starting with a dot"
        ))
    }
}

fn baseline_path(name: &str) -> PathBuf {
    PathBuf::from(".cig-bench")
        .join("baselines")
        .join(format!("{name}.json"))
}

/// The side the current checkout runs as: `[bench].binary` (relative to the
/// directory `cig bench` runs in) when the manifest names one, else the
/// running cig for the product `cig`, else no binary (the case's own
/// commands, resolved as written).
pub(super) fn current_side(job: &Job) -> Side {
    let product = job.manifest.bench.product.clone();
    let binary = match &job.manifest.bench.binary {
        Some(b) => Some(std::env::current_dir().unwrap_or_default().join(b)),
        None if product == "cig" => std::env::current_exe().ok(),
        None => None,
    };
    Side {
        label: product.clone(),
        product,
        binary,
        dir: job.manifest_dir.to_path_buf(),
    }
}

#[derive(Debug)]
pub(super) struct Baseline {
    pub(super) cases: Vec<(String, Vec<f64>)>,
    binary: Option<(PathBuf, String)>,
}

pub(super) fn save_baseline(
    job: &Job,
    side: &Side,
    name: &str,
    rows: &[Row],
    conditions: &Conditions,
) -> Result<(), String> {
    let mut cases = Vec::new();
    for r in rows {
        match &r.outcome {
            Outcome::Timed(s) => cases.push((r.name.clone(), r.group.clone(), s)),
            _ => {
                return Err(format!(
                    "baseline `{name}` not saved: {} was not timed",
                    r.name
                ))
            }
        }
    }
    write_baseline(job, side, name, &cases, conditions)
}

fn write_baseline(
    job: &Job,
    side: &Side,
    name: &str,
    cases: &[(String, Option<String>, &Stats)],
    conditions: &Conditions,
) -> Result<(), String> {
    let binary = side.binary.as_ref().and_then(|p| {
        let sha = crate::burn::journal::sha256_file(p).ok()?;
        let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
        Some(serde_json::json!({ "path": abs.to_string_lossy(), "sha256": sha }))
    });
    let doc = serde_json::json!({
        "format": FORMAT,
        "product": job.manifest.bench.product,
        "cig_version": env!("CARGO_PKG_VERSION"),
        "saved_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "conditions": {
            "cpu": conditions.cpu,
            "cores": conditions.cores,
            "os": conditions.os,
            "kernel": conditions.kernel,
            "governor": conditions.governor,
        },
        "binary": binary,
        "cases": cases.iter().map(|(n, g, s)| serde_json::json!({
            "name": n,
            "group": g,
            "samples_ms": s.raw_ms,
            "rss_bytes": s.rss_bytes,
        })).collect::<Vec<_>>(),
    });
    let path = baseline_path(name);
    let dir = path.parent().expect("baseline folder");
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
    let tmp = dir.join(format!(".{name}.json.tmp"));
    let text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())? + "\n";
    std::fs::write(&tmp, text).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("cannot place {}: {e}", path.display()))?;
    eprintln!("bench: saved baseline `{name}` in {}", path.display());
    Ok(())
}

fn load_baseline(job: &Job, name: &str) -> Result<Baseline, String> {
    let path = baseline_path(name);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read baseline {}: {e}", path.display()))?;
    let v: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("baseline {} is not JSON: {e}", path.display()))?;
    match v["format"].as_i64() {
        Some(FORMAT) => {}
        Some(n) => {
            return Err(format!(
                "baseline {} is format {n}; this cig reads format {FORMAT}",
                path.display()
            ))
        }
        None => {
            return Err(format!(
                "baseline {} has no `format`; this cig reads format {FORMAT}",
                path.display()
            ))
        }
    }
    if v["product"].as_str() != Some(job.manifest.bench.product.as_str()) {
        return Err(format!(
            "baseline {} is for product {}, not {}",
            path.display(),
            v["product"],
            job.manifest.bench.product
        ));
    }
    let mut cases = Vec::new();
    for c in v["cases"].as_array().cloned().unwrap_or_default() {
        let name = c["name"].as_str().unwrap_or_default().to_string();
        let ms: Vec<f64> = c["samples_ms"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_f64()).collect())
            .unwrap_or_default();
        cases.push((name, ms));
    }
    let binary = match (v["binary"]["path"].as_str(), v["binary"]["sha256"].as_str()) {
        (Some(p), Some(h)) => Some((PathBuf::from(p), h.to_string())),
        _ => None,
    };
    Ok(Baseline { cases, binary })
}

// ----- what `--against` names ------------------------------------------------

#[derive(Debug, PartialEq)]
enum Kind {
    Baseline(String),
    Release(String),
    Ref(String),
    Binary(PathBuf),
}

/// `baseline:`, `release:`, `ref:` and `bin:` say which; without a prefix a
/// baseline of that name wins, then a path to a file, then a version number,
/// then a git ref.
fn classify(spec: &str) -> Kind {
    if let Some(s) = spec.strip_prefix("baseline:") {
        return Kind::Baseline(s.to_string());
    }
    if let Some(s) = spec.strip_prefix("release:") {
        return Kind::Release(s.to_string());
    }
    if let Some(s) = spec.strip_prefix("ref:") {
        return Kind::Ref(s.to_string());
    }
    if let Some(s) = spec.strip_prefix("bin:") {
        return Kind::Binary(PathBuf::from(s));
    }
    if check_baseline_name(spec).is_ok() && baseline_path(spec).is_file() {
        return Kind::Baseline(spec.to_string());
    }
    if Path::new(spec).is_file() {
        return Kind::Binary(PathBuf::from(spec));
    }
    if looks_like_version(spec) {
        return Kind::Release(spec.to_string());
    }
    Kind::Ref(spec.to_string())
}

pub(super) fn looks_like_version(s: &str) -> bool {
    let core = s.strip_prefix('v').unwrap_or(s);
    let mut n = 0;
    for part in core.split('.') {
        if part.is_empty() || !part.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
        n += 1;
    }
    (2..=3).contains(&n)
}

/// What the A side is, once resolved.
pub(super) struct Resolved {
    /// What the table calls it.
    pub(super) label: String,
    kind: &'static str,
    /// The side to interleave with; `None` compares fresh timings with a
    /// baseline's saved ones.
    pub(super) side: Option<Side>,
    pub(super) saved: Option<Baseline>,
    /// Held so a temporary worktree or download is removed at the end.
    _guard: Option<Worktree>,
}

pub(super) fn resolve_for_watch(job: &Job, spec: &str, scratch: &Path) -> Result<Resolved, String> {
    resolve(job, spec, scratch)
}

fn resolve(job: &Job, spec: &str, scratch: &Path) -> Result<Resolved, String> {
    let product = job.manifest.bench.product.clone();
    match classify(spec) {
        Kind::Binary(p) => {
            let abs = std::fs::canonicalize(&p)
                .map_err(|e| format!("cannot use binary {}: {e}", p.display()))?;
            Ok(Resolved {
                label: p.display().to_string(),
                kind: "binary",
                side: Some(Side {
                    label: "A".into(),
                    product,
                    binary: Some(abs),
                    dir: job.manifest_dir.to_path_buf(),
                }),
                saved: None,
                _guard: None,
            })
        }
        Kind::Baseline(name) => {
            check_baseline_name(&name)?;
            let saved = load_baseline(job, &name)?;
            let mut side = None;
            match &saved.binary {
                Some((p, sha)) => match crate::burn::journal::sha256_file(p) {
                    Ok(now) if &now == sha => {
                        side = Some(Side {
                            label: "A".into(),
                            product,
                            binary: Some(p.clone()),
                            dir: job.manifest_dir.to_path_buf(),
                        })
                    }
                    Ok(_) => eprintln!(
                        "bench: baseline `{name}`'s binary {} has changed since it was saved; its saved timings are compared with fresh ones, not interleaved",
                        p.display()
                    ),
                    Err(_) => eprintln!(
                        "bench: baseline `{name}`'s binary {} is gone; its saved timings are compared with fresh ones, not interleaved",
                        p.display()
                    ),
                },
                None => eprintln!(
                    "bench: baseline `{name}` recorded no binary; its saved timings are compared with fresh ones, not interleaved"
                ),
            }
            Ok(Resolved {
                label: name,
                kind: "baseline",
                side,
                saved: Some(saved),
                _guard: None,
            })
        }
        Kind::Release(v) => {
            if job.manifest.bench.product != "cig" {
                return Err(format!(
                    "`--against {v}` fetches a cig release; this manifest's product is {}, so name a binary or a git ref",
                    job.manifest.bench.product
                ));
            }
            let (bin, label) = fetch_release(&v, scratch)?;
            Ok(Resolved {
                label,
                kind: "release",
                side: Some(Side {
                    label: "A".into(),
                    product,
                    binary: Some(bin),
                    dir: job.manifest_dir.to_path_buf(),
                }),
                saved: None,
                _guard: None,
            })
        }
        Kind::Ref(r) => {
            let wt = Worktree::build(job, &r, scratch)?;
            Ok(Resolved {
                label: wt.label.clone(),
                kind: "ref",
                side: Some(Side {
                    label: "A".into(),
                    product,
                    binary: wt.binary.clone(),
                    dir: wt.manifest_dir.clone(),
                }),
                saved: None,
                _guard: Some(wt),
            })
        }
    }
}

/// A release's binary for this platform: downloaded, its sha256 checked
/// against the release's sidecar, and only then made runnable. A binary that
/// does not verify is deleted and nothing runs.
pub(super) fn fetch_release(spec: &str, scratch: &Path) -> Result<(PathBuf, String), String> {
    let version = Version::parse(spec).ok_or_else(|| format!("`{spec}` is not a version"))?;
    let release = update::find(update::DEFAULT_REPO, &format!("v{version}"))?;
    let name = update::asset_name(&version);
    let Some(asset) = release.asset(&name) else {
        return Err(format!(
            "release {} has no asset named {name} for this platform",
            release.tag
        ));
    };
    let Some(sidecar) = release.asset(&format!("{name}.sha256")) else {
        return Err(format!(
            "release {} ships no checksum for {name}; refusing to run an unverifiable binary",
            release.tag
        ));
    };
    let dir = scratch.join("release");
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
    let bin = dir.join(&name);
    let sum = dir.join(format!("{name}.sha256"));
    update::download(&asset.url, &bin)?;
    update::download(&sidecar.url, &sum)?;
    let sidecar_text = std::fs::read_to_string(&sum).unwrap_or_default();
    if let Err(e) = update::verify_sha256(&bin, &sidecar_text) {
        let _ = std::fs::remove_file(&bin);
        return Err(format!("{e}; nothing was run"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }
    Ok((bin, format!("cig {version}")))
}

/// A git ref checked out in a temporary worktree and built through the
/// manifest's `[bench].build`. Dropping it removes the worktree.
struct Worktree {
    top: PathBuf,
    tree: PathBuf,
    label: String,
    binary: Option<PathBuf>,
    manifest_dir: PathBuf,
}

fn git(top: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(top)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

impl Worktree {
    fn build(job: &Job, spec: &str, scratch: &Path) -> Result<Worktree, String> {
        if spec.starts_with('-') || spec.is_empty() {
            return Err(format!("`{spec}` is not a git ref"));
        }
        let Some(build) = &job.manifest.bench.build else {
            return Err(format!(
                "`--against {spec}` builds the ref with `[bench].build`, and {} has none",
                job.manifest_path.display()
            ));
        };
        let cwd = std::env::current_dir()
            .and_then(std::fs::canonicalize)
            .map_err(|e| format!("cannot read the current directory: {e}"))?;
        let top = PathBuf::from(git(&cwd, &["rev-parse", "--show-toplevel"]).map_err(|e| {
            format!("`--against {spec}` needs a git checkout (`cig bench` is not in one): {e}")
        })?);
        let top = std::fs::canonicalize(&top).unwrap_or(top);
        let commit = git(
            &top,
            &[
                "rev-parse",
                "--verify",
                "--end-of-options",
                &format!("{spec}^{{commit}}"),
            ],
        )
        .map_err(|_| format!("`{spec}` is not a git ref in this checkout"))?;
        let short = commit.chars().take(7).collect::<String>();
        let rel = cwd
            .strip_prefix(&top)
            .map_err(|_| "the current directory is outside its checkout".to_string())?
            .to_path_buf();
        let manifest_abs = std::fs::canonicalize(job.manifest_dir)
            .map_err(|e| format!("cannot read the manifest's folder: {e}"))?;
        let manifest_rel = manifest_abs.strip_prefix(&cwd).map_err(|_| {
            "`--against <ref>` needs the manifest inside the current directory".to_string()
        })?;
        let tree = scratch.join("ref-tree");
        git(
            &top,
            &[
                "worktree",
                "add",
                "--detach",
                &tree.to_string_lossy(),
                &commit,
            ],
        )?;
        let mut wt = Worktree {
            top,
            tree: tree.clone(),
            label: format!("{spec}@{short}"),
            binary: None,
            manifest_dir: tree.join(&rel).join(manifest_rel),
        };
        let root = tree.join(&rel);
        // The build is the product's own: it keeps the caller's environment
        // (a compiler needs its toolchain's).
        let out = Command::new(&build[0])
            .args(&build[1..])
            .current_dir(&root)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("cannot run `{}` in the {spec} worktree: {e}", build[0]))?;
        if !out.status.success() {
            return Err(format!(
                "building {spec} failed (`{}`):\n{}{}",
                build.join(" "),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        if let Some(b) = &job.manifest.bench.binary {
            let bin = root.join(b);
            if !bin.is_file() {
                return Err(format!(
                    "building {spec} left no binary at {} (`[bench].binary` = {b:?})",
                    bin.display()
                ));
            }
            wt.binary = Some(bin);
        }
        if !wt.manifest_dir.is_dir() {
            return Err(format!(
                "{spec} has no {} to measure against",
                manifest_rel.display()
            ));
        }
        Ok(wt)
    }
}

impl Drop for Worktree {
    fn drop(&mut self) {
        let _ = git(
            &self.top,
            &[
                "worktree",
                "remove",
                "--force",
                &self.tree.to_string_lossy(),
            ],
        );
        let _ = std::fs::remove_dir_all(&self.tree);
        let _ = git(&self.top, &["worktree", "prune"]);
    }
}

// ----- the comparison itself -------------------------------------------------

/// One case, compared.
pub(super) enum AbResult {
    Compared {
        a: Stats,
        b: Stats,
        cmp: Comparison,
        interleaved: bool,
    },
    Wrong(String),
    Absent(String),
}

pub(super) struct AbRow {
    pub name: String,
    pub group: Option<String>,
    pub budget: Option<String>,
    pub result: AbResult,
}

/// Measure one case on both sides, interleaved. A side that is wrong or
/// cannot run ends the case: there are no timings to compare.
pub(super) fn compare_case(
    a_side: &Side,
    b_side: &Side,
    scratch: &Path,
    case: &super::Case,
    defaults: &super::Defaults,
) -> AbResult {
    let pa = match prepare(a_side, scratch, case, defaults) {
        Ok(p) => p,
        Err(o) => return failed("A", o),
    };
    let pb = match prepare(b_side, scratch, case, defaults) {
        Ok(p) => p,
        Err(o) => return failed("B", o),
    };
    let mut rss_a = None;
    let mut rss_b = None;
    let mut failure: Option<(Which, Outcome)> = None;
    let taken = interleave(defaults.samples, |w, i| {
        let (p, rss): (&Prepared, &mut Option<u64>) = match w {
            Which::A => (&pa, &mut rss_a),
            Which::B => (&pb, &mut rss_b),
        };
        match p.sample(i) {
            Ok((t, r)) => {
                *rss = max_opt(*rss, r);
                Ok(t)
            }
            Err(o) => {
                failure = Some((w, o));
                Err(())
            }
        }
    });
    match taken {
        Ok((a, b)) => {
            let cmp = compare(&a, &b);
            AbResult::Compared {
                a: Stats::of(&a, rss_a),
                b: Stats::of(&b, rss_b),
                cmp,
                interleaved: true,
            }
        }
        Err(()) => {
            let (w, o) = failure.expect("a failed sample records why");
            failed(if w == Which::A { "A" } else { "B" }, o)
        }
    }
}

fn failed(side: &str, o: Outcome) -> AbResult {
    match o {
        Outcome::Wrong(w) => AbResult::Wrong(format!("side {side}: {w}")),
        Outcome::Absent(w) => AbResult::Absent(format!("side {side}: {w}")),
        Outcome::Timed(_) => AbResult::Absent(format!("side {side}: not run")),
    }
}

/// Run everything `--against` asks for and print the result.
pub(super) fn against(job: &Job, spec: &str, scratch: &Path) -> i32 {
    let ctx = job.ctx;
    let resolved = match resolve(job, spec, scratch) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{} {e}", ctx.red("error[E800 usage]:"));
            return exit::USAGE;
        }
    };
    let mut b_side = current_side(job);
    b_side.label = "B".into();
    let rows: Vec<AbRow> = job
        .cases
        .iter()
        .map(|case| {
            let result = match (&resolved.side, &resolved.saved) {
                (Some(a), _) => compare_case(a, &b_side, scratch, case, job.defaults),
                (None, Some(saved)) => compare_saved(saved, &b_side, scratch, case, job.defaults),
                (None, None) => AbResult::Absent("nothing to compare with".into()),
            };
            AbRow {
                name: case.name.clone(),
                group: case.group.clone(),
                budget: case.budget.clone(),
                result,
            }
        })
        .collect();
    let conditions = Conditions::here();
    if ctx.json {
        print_json(job, spec, &resolved, &rows, &conditions);
    } else if job.opts.markdown {
        print_markdown(job, &resolved, &rows, &conditions);
    } else {
        print_table(job, &resolved, &rows, &conditions);
    }
    let mut code = exit::OK;
    for r in &rows {
        match &r.result {
            AbResult::Wrong(w) => {
                eprintln!("bench: {} is WRONG: {w}", r.name);
                code = exit::SCRIPT_ERROR;
            }
            AbResult::Absent(w) => {
                eprintln!("bench: {}: {w}", r.name);
                code = exit::SCRIPT_ERROR;
            }
            AbResult::Compared { .. } => {}
        }
    }
    if let Some(name) = &job.opts.save {
        let cases: Vec<(String, Option<String>, &Stats)> = rows
            .iter()
            .filter_map(|r| match &r.result {
                AbResult::Compared { b, .. } => Some((r.name.clone(), r.group.clone(), b)),
                _ => None,
            })
            .collect();
        if cases.len() != rows.len() {
            eprintln!("bench: baseline `{name}` not saved: a case was not timed");
        } else if let Err(e) = write_baseline(job, &b_side, name, &cases, &conditions) {
            eprintln!("{} {e}", ctx.red("error[E800 usage]:"));
            code = exit::USAGE;
        }
    }
    code
}

/// B's fresh samples against a baseline's saved ones: one side after the
/// other, so not interleaved, and reported as such.
fn compare_saved(
    saved: &Baseline,
    b_side: &Side,
    scratch: &Path,
    case: &super::Case,
    defaults: &super::Defaults,
) -> AbResult {
    let Some((_, ms)) = saved.cases.iter().find(|(n, _)| n == &case.name) else {
        return AbResult::Absent("the baseline has no such case".into());
    };
    match super::measure_case(b_side, scratch, case, defaults) {
        Outcome::Timed(b) => {
            let cmp = compare(ms, &b.raw_ms);
            AbResult::Compared {
                a: Stats::of(ms, None),
                b,
                cmp,
                interleaved: false,
            }
        }
        o => failed("B", o),
    }
}

// ----- rendering -------------------------------------------------------------

pub(super) fn change_cell(c: &Comparison) -> String {
    let ratio = 1.0 + c.change;
    if ratio >= 2.0 {
        format!("x{ratio:.2}")
    } else {
        format!("{:+.1}%", c.change * 100.0)
    }
}

pub(super) fn p_cell(p: f64) -> String {
    if p < 0.001 {
        "p<0.001".into()
    } else {
        format!("p={p:.3}")
    }
}

fn mark(s: &Stats) -> String {
    if s.noisy() {
        format!("{:.1}~", s.median_ms)
    } else {
        format!("{:.1}", s.median_ms)
    }
}

fn row_cells(r: &AbRow) -> [String; 6] {
    match &r.result {
        AbResult::Compared { a, b, cmp, .. } => [
            mark(a),
            mark(b),
            change_cell(cmp),
            p_cell(cmp.p),
            format!("{:+.2}", cmp.effect),
            cmp.verdict.word().to_string(),
        ],
        AbResult::Wrong(_) => [
            "-".into(),
            "-".into(),
            "-".into(),
            "-".into(),
            "-".into(),
            "WRONG".into(),
        ],
        AbResult::Absent(_) => [
            "-".into(),
            "-".into(),
            "-".into(),
            "-".into(),
            "-".into(),
            "-".into(),
        ],
    }
}

fn mode_note(rows: &[AbRow]) -> &'static str {
    if rows.iter().any(|r| {
        matches!(
            r.result,
            AbResult::Compared {
                interleaved: false,
                ..
            }
        )
    }) {
        "saved timings against fresh ones (not interleaved)"
    } else {
        "interleaved A/B, Mann-Whitney"
    }
}

fn print_table(job: &Job, res: &Resolved, rows: &[AbRow], c: &Conditions) {
    outln!(
        "cig bench: {}, A = {} against B = this checkout (cig {})",
        job.manifest.bench.product,
        res.label,
        env!("CARGO_PKG_VERSION")
    );
    outln!("{}", c.line());
    outln!("{}", mode_note(rows));
    outln!("");
    outln!(
        "{:<30} {:<10} {:>10} {:>10} {:>9} {:>8} {:>7}  verdict",
        "case",
        "group",
        "A ms",
        "B ms",
        "change",
        "p",
        "effect"
    );
    for r in rows {
        let [a, b, ch, p, e, v] = row_cells(r);
        outln!(
            "{:<30} {:<10} {:>10} {:>10} {:>9} {:>8} {:>7}  {}",
            r.name,
            r.group.as_deref().unwrap_or("-"),
            a,
            b,
            ch,
            p,
            e,
            v
        );
    }
}

fn print_markdown(job: &Job, res: &Resolved, rows: &[AbRow], c: &Conditions) {
    outln!(
        "**cig bench**: {}, A = `{}` against B = this checkout (cig {}) — {}; {}",
        job.manifest.bench.product,
        res.label,
        env!("CARGO_PKG_VERSION"),
        c.line(),
        mode_note(rows)
    );
    outln!("");
    outln!("| case | group | A ms | B ms | change | p | effect | verdict |");
    outln!("|---|---|---:|---:|---:|---:|---:|---|");
    for r in rows {
        let [a, b, ch, p, e, v] = row_cells(r);
        outln!(
            "| `{}` | {} | {} | {} | {} | {} | {} | {} |",
            r.name,
            r.group.as_deref().unwrap_or("-"),
            a,
            b,
            ch,
            p,
            e,
            v
        );
    }
}

pub(super) fn ab_json(r: &AbRow) -> serde_json::Value {
    let outcome = match &r.result {
        AbResult::Compared {
            a,
            b,
            cmp,
            interleaved,
        } => serde_json::json!({
            "state": "compared",
            "a": stats_json(a),
            "b": stats_json(b),
            "change": cmp.change,
            "p": cmp.p,
            "effect": cmp.effect,
            "verdict": cmp.verdict.json(),
            "interleaved": interleaved,
        }),
        AbResult::Wrong(why) => serde_json::json!({ "state": "wrong", "why": why }),
        AbResult::Absent(why) => serde_json::json!({ "state": "absent", "why": why }),
    };
    serde_json::json!({
        "name": r.name,
        "group": r.group,
        "budget": r.budget,
        "outcome": outcome,
    })
}

fn print_json(job: &Job, spec: &str, res: &Resolved, rows: &[AbRow], c: &Conditions) {
    let doc = serde_json::json!({
        "format": FORMAT,
        "product": job.manifest.bench.product,
        "cig_version": env!("CARGO_PKG_VERSION"),
        "against": { "spec": spec, "kind": res.kind, "label": res.label },
        "conditions": {
            "cpu": c.cpu,
            "cores": c.cores,
            "os": c.os,
            "kernel": c.kernel,
            "governor": c.governor,
        },
        "cases": rows.iter().map(ab_json).collect::<Vec<_>>(),
    });
    outln!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
}

// ----- tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic noise in [-1, 1): a small linear congruential stream.
    struct Noise(u64);
    impl Noise {
        fn next(&mut self) -> f64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 33) as f64 / (1u64 << 31) as f64) * 2.0 - 1.0
        }
    }

    #[test]
    fn interleaved_ab_cancels_a_drift() {
        // The machine warms up: every sample, whichever side takes it, is
        // 1.5 ms slower than the one before. Both sides are the same program.
        let mut tick = 0.0f64;
        let mut noise = Noise(7);
        let (a, b) = interleave::<()>(12, |_, _| {
            tick += 1.5;
            Ok(100.0 + tick + noise.next())
        })
        .unwrap();
        let cmp = compare(&a, &b);
        assert_eq!(
            cmp.verdict,
            Verdict::Same,
            "a drift showed as a difference: {cmp:?}"
        );
        // The test has teeth: the same drift, taken one side after the
        // other, is reported as a regression.
        let mut tick = 0.0f64;
        let mut noise = Noise(7);
        let mut seq = |n: usize| -> Vec<f64> {
            (0..n)
                .map(|_| {
                    tick += 1.5;
                    100.0 + tick + noise.next()
                })
                .collect()
        };
        let (a2, b2) = (seq(12), seq(12));
        assert_eq!(compare(&a2, &b2).verdict, Verdict::Slower);
    }

    #[test]
    fn interleaving_alternates_the_order_in_each_pair() {
        let mut seen = Vec::new();
        interleave::<()>(4, |w, i| {
            seen.push((w, i));
            Ok(1.0)
        })
        .unwrap();
        use Which::{A, B};
        assert_eq!(
            seen,
            vec![
                (A, 0),
                (B, 0),
                (B, 1),
                (A, 1),
                (A, 2),
                (B, 2),
                (B, 3),
                (A, 3)
            ]
        );
    }

    #[test]
    fn a_real_10_percent_regression_is_found() {
        // B is 10% slower; both sides are noisy (+-3%) and drift 0.2 ms a sample.
        let mut tick = 0.0f64;
        let mut noise = Noise(42);
        let (a, b) = interleave::<()>(9, |w, _| {
            tick += 0.2;
            let base = if w == Which::A { 100.0 } else { 110.0 };
            Ok(base + tick + 3.0 * noise.next())
        })
        .unwrap();
        let cmp = compare(&a, &b);
        assert_eq!(cmp.verdict, Verdict::Slower, "{cmp:?}");
        assert!(cmp.p < ALPHA, "{cmp:?}");
        assert!((cmp.change - 0.10).abs() < 0.03, "{cmp:?}");
        assert!(
            cmp.effect > 0.8,
            "B is the slower one in most pairs: {cmp:?}"
        );
        // And the same noise with no regression is not reported.
        let mut tick = 0.0f64;
        let mut noise = Noise(42);
        let (a, b) = interleave::<()>(9, |_, _| {
            tick += 0.2;
            Ok(100.0 + tick + 3.0 * noise.next())
        })
        .unwrap();
        assert_eq!(compare(&a, &b).verdict, Verdict::Same);
    }

    #[test]
    fn a_small_but_certain_change_stays_same() {
        // 1% slower, perfectly separated: p is tiny but the change is under 2%.
        let a: Vec<f64> = (0..9).map(|i| 100.0 + i as f64 * 0.01).collect();
        let b: Vec<f64> = (0..9).map(|i| 101.0 + i as f64 * 0.01).collect();
        let cmp = compare(&a, &b);
        assert!(cmp.p < ALPHA);
        assert_eq!(cmp.verdict, Verdict::Same);
    }

    #[test]
    fn mann_whitney_matches_the_textbook() {
        // Complete separation of 5 against 5: p = 2 / C(10, 5) = 2/252.
        let a = [1.0, 2.0, 3.0, 4.0, 5.0];
        let b = [6.0, 7.0, 8.0, 9.0, 10.0];
        let (p, r) = mann_whitney(&a, &b);
        assert!((p - 2.0 / 252.0).abs() < 1e-12, "{p}");
        assert_eq!(r, 1.0);
        let (p, r) = mann_whitney(&b, &a);
        assert!((p - 2.0 / 252.0).abs() < 1e-12, "{p}");
        assert_eq!(r, -1.0);
        // Identical samples: p is 1, effect 0 (ties take the normal branch).
        let (p, r) = mann_whitney(&a, &a);
        assert!((p - 1.0).abs() < 1e-6 && r == 0.0, "{p} {r}");
        // Three against three, one overlap: U = 1, p = 0.2 (exact).
        let (p, _) = mann_whitney(&[1.0, 2.0, 4.0], &[3.0, 5.0, 6.0]);
        assert!((p - 0.2).abs() < 1e-12, "{p}");
    }

    #[test]
    fn the_normal_branch_agrees_with_the_exact_one() {
        // 25 against 25 is past the exact limit: well separated, so tiny p;
        // heavily overlapping, so large p.
        let a: Vec<f64> = (0..25).map(|i| i as f64 + 0.5).collect();
        let far: Vec<f64> = (0..25).map(|i| 100.0 + i as f64).collect();
        assert!(mann_whitney(&a, &far).0 < 1e-6);
        let near: Vec<f64> = (0..25).map(|i| i as f64 + 0.25).collect();
        assert!(mann_whitney(&a, &near).0 > 0.5);
    }

    #[test]
    fn baseline_names_are_plain() {
        assert!(check_baseline_name("main").is_ok());
        assert!(check_baseline_name("v1.16.0-rc_1").is_ok());
        for bad in ["", ".hidden", "a/b", "..", "a\\b", "x y"] {
            assert!(check_baseline_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn against_names_say_what_they_are() {
        assert_eq!(classify("release:1.2.3"), Kind::Release("1.2.3".into()));
        assert_eq!(classify("ref:main"), Kind::Ref("main".into()));
        assert_eq!(classify("baseline:old"), Kind::Baseline("old".into()));
        assert_eq!(classify("bin:./x"), Kind::Binary(PathBuf::from("./x")));
        assert_eq!(classify("v1.16.0"), Kind::Release("v1.16.0".into()));
        assert_eq!(classify("1.16"), Kind::Release("1.16".into()));
        assert_eq!(classify("origin/main"), Kind::Ref("origin/main".into()));
        assert_eq!(classify("feature-1"), Kind::Ref("feature-1".into()));
    }
}
