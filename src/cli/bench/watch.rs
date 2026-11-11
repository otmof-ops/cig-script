// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! `cig bench --watch` (#179, step 3).
//!
//! Rerun the cases when a source file changes. The files are the
//! manifest's `[bench].watch` globs, relative to the directory `cig bench`
//! runs in; a case may carry its own `watch` globs to narrow which changes
//! rerun it. On a change the manifest's `[bench].build` runs first (a
//! failed build keeps the last numbers and says why), then the cases the
//! change could affect, cheapest first.
//!
//! Three shapes of output:
//! - a terminal: a table that redraws in place as each case finishes;
//! - no terminal on stdout, or `--plain`: one line per case per build, no
//!   redraws, no escape codes, for a log or another process to read;
//! - `--json`: one JSON object per line (`start`, `build`, `case`, `cycle`).
//!
//! What a build is compared with: the `--against` side when there is one
//! (interleaved, as in step 2), otherwise the session's first build.
//!
//! Keys, read only when stdin is a terminal: `q` quits, `r` reruns now,
//! space pauses and resumes the watching. With stdin from a file or a pipe
//! no key is read, and the end of that input does not end the watch.

use super::ab::{self, AbResult, Comparison};
use super::{exit, scratch_root, Case, Job, Outcome, Side, Stats};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime};

/// How often the watched files are looked at.
const POLL: Duration = Duration::from_millis(250);
/// A change is acted on once the files have held still for this long.
const SETTLE: Duration = Duration::from_millis(150);
/// Builds in a sparkline.
const SPARK: usize = 10;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Live,
    Plain,
    Json,
}

type Snapshot = BTreeMap<PathBuf, (SystemTime, u64)>;

/// The watched files: their globs and the folders worth walking for them.
struct Files {
    set: GlobSet,
    prefixes: Vec<PathBuf>,
}

fn compile(globs: &[String]) -> Result<Files, String> {
    let mut set = GlobSetBuilder::new();
    let mut prefixes = Vec::new();
    for g in globs {
        let g = g.strip_prefix("./").unwrap_or(g);
        let glob = GlobBuilder::new(g)
            .literal_separator(true)
            .build()
            .map_err(|e| format!("watch glob {g:?}: {e}"))?;
        set.add(glob);
        // Walk only from the part of the glob with no wildcard in it.
        let mut prefix = PathBuf::new();
        for part in g.split('/') {
            if part.contains(['*', '?', '[', '{']) {
                break;
            }
            prefix.push(part);
        }
        prefixes.push(prefix);
    }
    Ok(Files {
        set: set.build().map_err(|e| e.to_string())?,
        prefixes,
    })
}

impl Files {
    fn snapshot(&self) -> Snapshot {
        let mut out = Snapshot::new();
        for prefix in &self.prefixes {
            let start = if prefix.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                prefix.clone()
            };
            let walk = walkdir::WalkDir::new(&start).into_iter().filter_entry(|e| {
                let n = e.file_name().to_string_lossy();
                !(e.depth() > 0 && matches!(n.as_ref(), ".git" | "target" | ".cig-bench"))
            });
            for e in walk.flatten() {
                if !e.file_type().is_file() {
                    continue;
                }
                let p = e.path().strip_prefix(".").unwrap_or(e.path()).to_path_buf();
                if !self.set.is_match(&p) {
                    continue;
                }
                if let Ok(md) = e.metadata() {
                    let t = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    out.insert(p, (t, md.len()));
                }
            }
        }
        out
    }
}

fn changed_between(old: &Snapshot, new: &Snapshot) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = new
        .iter()
        .filter(|(k, v)| old.get(*k) != Some(v))
        .map(|(k, _)| k.clone())
        .collect();
    out.extend(old.keys().filter(|k| !new.contains_key(*k)).cloned());
    out.sort();
    out
}

// ----- what is shown ---------------------------------------------------------

/// A finished case, as the table and the lines show it.
#[derive(Clone)]
enum View {
    Pending,
    Running,
    Timed {
        now: Stats,
        base_ms: Option<f64>,
        cmp: Option<Comparison>,
    },
    Wrong(String),
    Absent(String),
}

struct State {
    view: BTreeMap<String, View>,
    /// Each case's last medians, oldest first.
    history: BTreeMap<String, VecDeque<f64>>,
    /// The session's first timings: the baseline when there is no `--against`.
    base: BTreeMap<String, Vec<f64>>,
    builds: usize,
}

fn spark(h: &VecDeque<f64>) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let lo = h.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = h.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    h.iter()
        .map(|v| {
            if hi > lo {
                BARS[(((v - lo) / (hi - lo)) * 7.0).round() as usize]
            } else {
                BARS[3]
            }
        })
        .collect()
}

fn emit(line: &str) -> bool {
    writeln!(std::io::stdout(), "{line}").is_ok() && std::io::stdout().flush().is_ok()
}

/// The text of a verdict in a line: `same`, or `SLOWER p<0.001`.
fn verdict_text(cmp: &Comparison) -> String {
    format!("{} {}", cmp.verdict.word(), ab::p_cell(cmp.p))
}

fn plain_line(n: usize, name: &str, v: &View) -> String {
    match v {
        View::Timed { now, base_ms, cmp } => {
            let vs = match (base_ms, cmp) {
                (Some(b), Some(c)) => {
                    format!(" vs {b:.1} ms {} {}", ab::change_cell(c), verdict_text(c))
                }
                _ => " first run".to_string(),
            };
            format!(
                "bench: build {n} {name}: {:.1} ms (p10 {:.1} to p90 {:.1}, CV {:.1}%){vs}",
                now.median_ms,
                now.p10_ms,
                now.p90_ms,
                now.cv * 100.0
            )
        }
        View::Wrong(why) => format!("bench: build {n} {name}: WRONG: {why}"),
        View::Absent(why) => format!("bench: build {n} {name}: not measured: {why}"),
        View::Pending | View::Running => format!("bench: build {n} {name}: not run"),
    }
}

fn json_line(n: usize, name: &str, v: &View) -> serde_json::Value {
    match v {
        View::Timed { now, base_ms, cmp } => {
            let mut o = serde_json::json!({
                "event": "case", "build": n, "case": name, "state": "timed",
                "median_ms": now.median_ms, "p10_ms": now.p10_ms, "p90_ms": now.p90_ms,
                "cv": now.cv, "noisy": now.noisy(), "samples": now.samples,
                "rss_bytes": now.rss_bytes,
            });
            if let (Some(b), Some(c)) = (base_ms, cmp) {
                o["base_median_ms"] = serde_json::json!(b);
                o["change"] = serde_json::json!(c.change);
                o["p"] = serde_json::json!(c.p);
                o["effect"] = serde_json::json!(c.effect);
                o["verdict"] = serde_json::json!(match c.verdict {
                    ab::Verdict::Same => "same",
                    ab::Verdict::Faster => "faster",
                    ab::Verdict::Slower => "slower",
                });
            }
            o
        }
        View::Wrong(why) => {
            serde_json::json!({"event": "case", "build": n, "case": name, "state": "wrong", "why": why})
        }
        View::Absent(why) => {
            serde_json::json!({"event": "case", "build": n, "case": name, "state": "absent", "why": why})
        }
        View::Pending | View::Running => {
            serde_json::json!({"event": "case", "build": n, "case": name, "state": "not-run"})
        }
    }
}

/// The live table, redrawn whole: the cursor goes home and the screen is
/// cleared before it is written.
fn draw(job: &Job, st: &State, status: &str, against: &str) {
    let mut s = String::from("\x1b[H\x1b[2J");
    s.push_str(&format!(
        "cig bench --watch: {} · cig {} · {}\n",
        job.manifest.bench.product,
        env!("CARGO_PKG_VERSION"),
        against
    ));
    s.push_str(&format!("{status}\n\n"));
    s.push_str(&format!(
        "{:<28} {:>10} {:>10} {:>9}  {:<10}  verdict\n",
        "case", "base", "now", "change", "last 10"
    ));
    for case in &job.cases {
        let name = &case.name;
        let line = match st.view.get(name).unwrap_or(&View::Pending) {
            View::Timed { now, base_ms, cmp } => {
                let hist = st.history.get(name).map(spark).unwrap_or_default();
                let base = base_ms.map_or("-".to_string(), |b| format!("{b:.1} ms"));
                let (ch, v) = match cmp {
                    Some(c) => (ab::change_cell(c), verdict_text(c)),
                    None => ("-".into(), "first run".into()),
                };
                format!(
                    "{:<28} {:>10} {:>10} {:>9}  {:<10}  {}",
                    name,
                    base,
                    format!("{:.1} ms", now.median_ms),
                    ch,
                    hist,
                    v
                )
            }
            View::Wrong(why) => format!("{name:<28} WRONG: {why}"),
            View::Absent(why) => format!("{name:<28} not measured: {why}"),
            View::Running => format!("{name:<28} {:>10} {:>10} running…", "", ""),
            View::Pending => format!("{name:<28}"),
        };
        s.push_str(&line);
        s.push('\n');
    }
    let _ = write!(std::io::stdout(), "{s}");
    let _ = std::io::stdout().flush();
}

// ----- the terminal ----------------------------------------------------------

/// Single keys without Enter on a Unix terminal; the terminal is put back
/// when this is dropped. Elsewhere keys arrive a line at a time.
#[cfg(unix)]
struct RawMode {
    orig: libc::termios,
}

#[cfg(unix)]
impl RawMode {
    fn enter() -> Option<RawMode> {
        // SAFETY: tcgetattr fills a termios for descriptor 0, which is a
        // terminal here; a zeroed termios is a valid out-parameter.
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut t) != 0 {
                return None;
            }
            let orig = t;
            // Ctrl-C arrives as a byte so the terminal is restored on the way out.
            t.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG);
            t.c_cc[libc::VMIN] = 1;
            t.c_cc[libc::VTIME] = 0;
            if libc::tcsetattr(0, libc::TCSANOW, &t) != 0 {
                return None;
            }
            Some(RawMode { orig })
        }
    }
}

#[cfg(unix)]
impl Drop for RawMode {
    fn drop(&mut self) {
        // SAFETY: puts back the settings tcgetattr returned for descriptor 0.
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &self.orig);
        }
    }
}

// ----- the loop --------------------------------------------------------------

enum Against {
    /// Interleave with this fixed side every build.
    Side(Side),
    /// Fresh timings against a baseline's saved ones.
    Saved(Vec<(String, Vec<f64>)>),
    /// The session's first build.
    Session,
}

pub(super) fn watch(job: &Job) -> i32 {
    let ctx = job.ctx;
    let usage = |msg: &str| {
        eprintln!("{} {msg}", ctx.red("error[E800 usage]:"));
        exit::USAGE
    };
    if job.manifest.bench.watch.is_empty() {
        return usage(&format!(
            "`--watch` follows the globs in `[bench].watch`, and {} has none",
            job.manifest_path.display()
        ));
    }
    let files = match compile(&job.manifest.bench.watch) {
        Ok(f) => f,
        Err(e) => return usage(&e),
    };
    let case_files: BTreeMap<String, GlobSet> = {
        let mut m = BTreeMap::new();
        for c in &job.cases {
            if c.watch.is_empty() {
                continue;
            }
            match compile(&c.watch) {
                Ok(f) => {
                    m.insert(c.name.clone(), f.set);
                }
                Err(e) => return usage(&format!("case {:?}: {e}", c.name)),
            }
        }
        m
    };
    let scratch = match scratch_root() {
        Ok(p) => p,
        Err(e) => return usage(&format!("cannot make a private scratch folder: {e}")),
    };
    let code = run(job, &files, &case_files, &scratch);
    let _ = std::fs::remove_dir_all(&scratch);
    code
}

fn run(job: &Job, files: &Files, case_files: &BTreeMap<String, GlobSet>, scratch: &Path) -> i32 {
    let ctx = job.ctx;
    let stdin_tty = std::io::stdin().is_terminal();
    let mode = if ctx.json {
        Mode::Json
    } else if std::io::stdout().is_terminal() && !ctx.plain {
        Mode::Live
    } else {
        Mode::Plain
    };
    let b_side = ab::current_side(job);
    // The fixed side, resolved once; its worktree lives as long as the watch.
    let resolved = match &job.opts.against {
        Some(spec) => match ab::resolve_for_watch(job, spec, scratch) {
            Ok(r) => Some(r),
            Err(e) => {
                eprintln!("{} {e}", ctx.red("error[E800 usage]:"));
                return exit::USAGE;
            }
        },
        None => None,
    };
    let (against, against_label) = match &resolved {
        Some(r) => (
            match (&r.side, &r.saved) {
                (Some(s), _) => Against::Side(s.clone()),
                (None, Some(saved)) => Against::Saved(saved.cases.clone()),
                (None, None) => Against::Session,
            },
            format!("against {}", r.label),
        ),
        None => (
            Against::Session,
            "against this session's first build".into(),
        ),
    };

    // Keys, only from a terminal.
    let (tx, rx) = mpsc::channel::<u8>();
    #[cfg(unix)]
    let _raw = if stdin_tty { RawMode::enter() } else { None };
    if stdin_tty {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut b = [0u8; 1];
            while let Ok(1) = std::io::stdin().read(&mut b) {
                if tx.send(b[0]).is_err() {
                    break;
                }
            }
        });
    }
    // `tx` stays alive here so the wait below never sees a closed channel.
    let _keep = tx;

    let mut st = State {
        view: BTreeMap::new(),
        history: BTreeMap::new(),
        base: BTreeMap::new(),
        builds: 0,
    };
    let start = serde_json::json!({
        "event": "start", "product": job.manifest.bench.product,
        "cig_version": env!("CARGO_PKG_VERSION"), "format": super::FORMAT,
        "against": against_label, "cases": job.cases.len(),
        "watching": job.manifest.bench.watch,
    });
    let ok = match mode {
        Mode::Json => emit(&start.to_string()),
        Mode::Plain => emit(&format!(
            "bench: watching {} ({} cases, {against_label}); q or Ctrl-C quits",
            job.manifest.bench.watch.join(" "),
            job.cases.len()
        )),
        Mode::Live => true,
    };
    if !ok {
        return exit::OK;
    }

    let mut snap = files.snapshot();
    let mut paused = false;
    let mut pending: VecDeque<u8> = VecDeque::new();
    let mut changed: Vec<PathBuf> = Vec::new();
    let mut rerun = true; // the first build runs at once
    loop {
        if rerun {
            rerun = false;
            let alive = cycle(
                job,
                &mut st,
                &against,
                &b_side,
                case_files,
                scratch,
                mode,
                &against_label,
                &changed,
            );
            changed.clear();
            if !alive {
                return exit::OK; // stdout closed
            }
            if mode == Mode::Live {
                draw(job, &st, &status_line(paused, &st), &against_label);
            }
        }
        let key = match pending.pop_front() {
            Some(k) => Some(k),
            None => rx.recv_timeout(POLL).ok(),
        };
        match key {
            Some(b'q') | Some(b'Q') | Some(3) => return exit::OK,
            Some(b'r') | Some(b'R') => {
                rerun = true;
                // A manual rerun measures everything, whatever changed.
                changed.clear();
                snap = files.snapshot();
            }
            Some(b' ') => {
                paused = !paused;
                match mode {
                    Mode::Live => draw(job, &st, &status_line(paused, &st), &against_label),
                    Mode::Plain => {
                        if !emit(if paused {
                            "bench: paused (space resumes)"
                        } else {
                            "bench: watching again"
                        }) {
                            return exit::OK;
                        }
                    }
                    Mode::Json => {
                        if !emit(
                            &serde_json::json!({"event": "pause", "paused": paused}).to_string(),
                        ) {
                            return exit::OK;
                        }
                    }
                }
                if !paused {
                    snap = files.snapshot();
                }
            }
            Some(_) => {}
            None => {
                if paused {
                    continue;
                }
                let now = files.snapshot();
                if now == snap {
                    continue;
                }
                // Something moved: wait for the files to hold still.
                let mut cur = now;
                loop {
                    if let Ok(k) = rx.recv_timeout(SETTLE) {
                        pending.push_back(k);
                    }
                    let again = files.snapshot();
                    if again == cur {
                        break;
                    }
                    cur = again;
                }
                changed = changed_between(&snap, &cur);
                snap = cur;
                rerun = true;
            }
        }
    }
}

fn status_line(paused: bool, st: &State) -> String {
    let keys = "[r]erun [space] pause/resume [q]uit";
    if paused {
        format!("build {} · PAUSED · {keys}", st.builds)
    } else {
        format!("build {} · watching · {keys}", st.builds)
    }
}

/// One build and its measurements. Returns false when stdout has closed.
#[allow(clippy::too_many_arguments)]
fn cycle(
    job: &Job,
    st: &mut State,
    against: &Against,
    b_side: &Side,
    case_files: &BTreeMap<String, GlobSet>,
    scratch: &Path,
    mode: Mode,
    against_label: &str,
    changed: &[PathBuf],
) -> bool {
    st.builds += 1;
    let n = st.builds;
    let shown: Vec<String> = changed.iter().map(|p| p.display().to_string()).collect();
    match mode {
        Mode::Json => {
            if !emit(
                &serde_json::json!({"event": "change", "build": n, "changed": shown}).to_string(),
            ) {
                return false;
            }
        }
        Mode::Plain
            if !shown.is_empty()
                && !emit(&format!("bench: build {n}: changed {}", shown.join(", "))) =>
        {
            return false;
        }
        _ => {}
    }
    // The build, when the manifest has one.
    if let Some(argv) = &job.manifest.bench.build {
        let out = Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::null())
            .output();
        let failure = match out {
            Ok(o) if o.status.success() => None,
            Ok(o) => Some(format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            )),
            Err(e) => Some(format!("cannot run `{}`: {e}", argv[0])),
        };
        if let Some(text) = failure {
            let text = text.trim().to_string();
            return match mode {
                Mode::Json => emit(
                    &serde_json::json!({"event": "build", "build": n, "ok": false, "error": text})
                        .to_string(),
                ),
                Mode::Plain => {
                    let first = text.lines().next().unwrap_or("");
                    emit(&format!(
                        "bench: build {n} failed: {first} (the last numbers stand)"
                    ))
                }
                Mode::Live => {
                    // The compiler's error in place of the table.
                    let _ = write!(
                        std::io::stdout(),
                        "\x1b[H\x1b[2Jcig bench --watch: build {n} FAILED (`{}`); the last numbers stand\n\n{text}\n",
                        argv.join(" ")
                    );
                    let _ = std::io::stdout().flush();
                    true
                }
            };
        }
        if mode == Mode::Json
            && !emit(&serde_json::json!({"event": "build", "build": n, "ok": true}).to_string())
        {
            return false;
        }
    }
    // Which cases: all on the first build or a manual rerun, otherwise those
    // with no `watch` of their own or one the change matches.
    let mut todo: Vec<&Case> = job
        .cases
        .iter()
        .copied()
        .filter(|c| {
            changed.is_empty()
                || case_files
                    .get(&c.name)
                    .is_none_or(|set| changed.iter().any(|p| set.is_match(p)))
        })
        .collect();
    // Cheapest first: what ran fastest last time, the unmeasured in front.
    let last = |c: &Case| -> f64 {
        match st.view.get(&c.name) {
            Some(View::Timed { now, .. }) => now.median_ms,
            _ => -1.0,
        }
    };
    todo.sort_by(|x, y| last(x).total_cmp(&last(y)));
    let names: BTreeSet<&str> = todo.iter().map(|c| c.name.as_str()).collect();
    for c in &job.cases {
        if names.contains(c.name.as_str()) {
            st.view.insert(c.name.clone(), View::Pending);
        }
    }
    let total = todo.len();
    for (i, case) in todo.iter().enumerate() {
        if mode == Mode::Live {
            st.view.insert(case.name.clone(), View::Running);
            draw(
                job,
                st,
                &format!("build {n} · running {} of {total}: {}", i + 1, case.name),
                against_label,
            );
        }
        let view = measure(job, st, against, b_side, scratch, case);
        if let View::Timed { now, .. } = &view {
            let h = st.history.entry(case.name.clone()).or_default();
            h.push_back(now.median_ms);
            while h.len() > SPARK {
                h.pop_front();
            }
            st.base
                .entry(case.name.clone())
                .or_insert_with(|| now.raw_ms.clone());
        }
        let line_ok = match mode {
            Mode::Json => emit(&json_line(n, &case.name, &view).to_string()),
            Mode::Plain => emit(&plain_line(n, &case.name, &view)),
            Mode::Live => true,
        };
        st.view.insert(case.name.clone(), view);
        if !line_ok {
            return false;
        }
    }
    match mode {
        Mode::Json => {
            emit(&serde_json::json!({"event": "cycle", "build": n, "cases": total}).to_string())
        }
        Mode::Plain => emit(&format!("bench: build {n} done, {total} cases")),
        Mode::Live => true,
    }
}

fn measure(
    job: &Job,
    st: &State,
    against: &Against,
    b_side: &Side,
    scratch: &Path,
    case: &Case,
) -> View {
    match against {
        Against::Side(a) => from_ab(ab::compare_case(a, b_side, scratch, case, job.defaults)),
        Against::Saved(saved) => match saved.iter().find(|(n, _)| n == &case.name) {
            None => View::Absent("the baseline has no such case".into()),
            Some((_, ms)) => match super::measure_case(b_side, scratch, case, job.defaults) {
                Outcome::Timed(now) => {
                    let cmp = ab::compare(ms, &now.raw_ms);
                    View::Timed {
                        base_ms: Some(cmp.median_a),
                        now,
                        cmp: Some(cmp),
                    }
                }
                Outcome::Wrong(w) => View::Wrong(w),
                Outcome::Absent(w) => View::Absent(w),
            },
        },
        Against::Session => match super::measure_case(b_side, scratch, case, job.defaults) {
            Outcome::Timed(now) => match st.base.get(&case.name) {
                Some(base) => {
                    let cmp = ab::compare(base, &now.raw_ms);
                    View::Timed {
                        base_ms: Some(cmp.median_a),
                        now,
                        cmp: Some(cmp),
                    }
                }
                None => View::Timed {
                    now,
                    base_ms: None,
                    cmp: None,
                },
            },
            Outcome::Wrong(w) => View::Wrong(w),
            Outcome::Absent(w) => View::Absent(w),
        },
    }
}

fn from_ab(r: AbResult) -> View {
    match r {
        AbResult::Compared { b, cmp, .. } => View::Timed {
            base_ms: Some(cmp.median_a),
            now: b,
            cmp: Some(cmp),
        },
        AbResult::Wrong(w) => View::Wrong(w),
        AbResult::Absent(w) => View::Absent(w),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_walk_from_their_static_prefix() {
        let f = compile(&["lib/**".into(), "orchestrate.cig".into(), "src/*.rs".into()]).unwrap();
        assert_eq!(
            f.prefixes,
            vec![
                PathBuf::from("lib"),
                PathBuf::from("orchestrate.cig"),
                PathBuf::from("src")
            ]
        );
        assert!(f.set.is_match("lib/a/b.cig"));
        assert!(f.set.is_match("src/x.rs"));
        assert!(!f.set.is_match("src/deep/x.rs"));
        assert!(compile(&["[".into()]).is_err());
    }

    #[test]
    fn a_sparkline_scales_to_its_own_range() {
        let h: VecDeque<f64> = [1.0, 2.0, 8.0].into_iter().collect();
        assert_eq!(spark(&h), "▁▂█");
        let flat: VecDeque<f64> = [5.0, 5.0].into_iter().collect();
        assert_eq!(spark(&flat).chars().count(), 2);
    }

    #[test]
    fn changes_are_found_in_both_directions() {
        let t = SystemTime::UNIX_EPOCH;
        let mut old = Snapshot::new();
        old.insert("a".into(), (t, 1));
        old.insert("gone".into(), (t, 1));
        let mut new = Snapshot::new();
        new.insert("a".into(), (t, 2));
        new.insert("fresh".into(), (t, 1));
        assert_eq!(
            changed_between(&old, &new),
            vec![
                PathBuf::from("a"),
                PathBuf::from("fresh"),
                PathBuf::from("gone")
            ]
        );
    }
}
