// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! `cig bench --profile` and `--repeat-until-slow` (#179 step 8): a sampling
//! profile of a case as a flamegraph SVG.
//!
//! The sampler is `perf` (`perf record -F 999 -g`), one extra untimed run of
//! the case after its samples. Where `perf` is not installed or not allowed
//! (a container, `perf_event_paranoid`) the answer is `not measured: perf is
//! not available here: <why>`; there is no second sampler. What this module
//! does itself is only the drawing: `perf script`'s text is folded into
//! stacks and drawn as an SVG, so no flamegraph tool is needed.
//!
//! The SVG goes to `.cig-bench/profiles/<case>.svg`, beside the baselines.
//! Call graphs come from frame pointers (`perf -g`): a build with
//! `-C force-frame-pointers=yes` gives deep stacks, any other build gives
//! the frames perf can walk.
//!
//! `--repeat-until-slow` reruns the case one invocation at a time, each under
//! `perf` (or, where perf is not available, with the product's phases on its
//! descriptor), until one run is over p90 x 1.5 of the reference runs, and
//! keeps that run's profile (or its phases). The reference is the first
//! `max(samples, 10)` runs, taken the same way, so the threshold and the run
//! it judges carry the same overhead. It stops after 200 reruns or 120
//! seconds and says so.

use super::debug::Breakdown;
use super::{build_command, full_env, run_befores, Prepared, Stats};
use crate::timings::Reading;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

/// Samples a second.
const HZ: u32 = 999;
/// A run is slow over this many times the reference's p90.
const SLOW_FACTOR: f64 = 1.5;
/// The fewest reference runs the threshold is taken from.
const MIN_REFERENCE: usize = 10;
/// Reruns after the reference, and the seconds the whole search may take.
const MAX_RERUNS: usize = 200;
const MAX_SECONDS: u64 = 120;
/// The words every perf problem starts with.
const NOT_AVAILABLE: &str = "perf is not available here";

/// A profile drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub svg: PathBuf,
    pub samples: u64,
    /// The frames with the most samples of their own, most first.
    pub top: Vec<(String, u64)>,
}

/// The run `--repeat-until-slow` stopped at.
#[derive(Debug, Clone)]
pub struct SlowRun {
    /// Which run it was, counting from 1 (the reference runs come first).
    pub run: usize,
    pub ms: f64,
    pub profile: Option<Result<Profile, String>>,
    pub phases: Option<Result<Breakdown, String>>,
}

#[derive(Debug, Clone)]
pub struct Slow {
    /// Runs made.
    pub runs: usize,
    pub reference_p90_ms: f64,
    pub threshold_ms: f64,
    pub found: Option<SlowRun>,
    /// Why perf was not used, when the phases stood in.
    pub note: Option<String>,
    /// Why the search ended without a slow run.
    pub gave_up: Option<String>,
}

// ----- folding and drawing ---------------------------------------------------

/// `perf script`'s text as folded stacks, `comm;outer;...;leaf` to a count.
/// perf prints a sample as a header line (the command first) and then its
/// frames, innermost first, each indented; a blank line ends the sample.
pub(super) fn fold(script: &str) -> BTreeMap<String, u64> {
    fn flush(stack: &mut Vec<String>, out: &mut BTreeMap<String, u64>) {
        if stack.len() > 1 {
            // A header alone (no frames) is not a stack.
            let key = stack.join(";");
            *out.entry(key).or_insert(0) += 1;
        }
        stack.clear();
    }
    let mut out = BTreeMap::new();
    // Innermost first, as read; the command goes under them when the
    // sample ends.
    let mut frames: Vec<String> = Vec::new();
    let mut comm = String::new();
    let mut ordered: Vec<String> = Vec::new();
    let mut end = |frames: &mut Vec<String>, comm: &mut String, out: &mut BTreeMap<String, u64>| {
        if !frames.is_empty() {
            ordered.clear();
            ordered.push(if comm.is_empty() {
                "?".to_string()
            } else {
                comm.clone()
            });
            ordered.extend(frames.iter().rev().cloned());
            flush(&mut ordered, out);
        }
        frames.clear();
        comm.clear();
    };
    for line in script.lines() {
        if line.trim().is_empty() {
            end(&mut frames, &mut comm, &mut out);
        } else if line.starts_with(char::is_whitespace) {
            if let Some(f) = frame_name(line) {
                frames.push(f);
            }
        } else {
            end(&mut frames, &mut comm, &mut out);
            comm = line.split_whitespace().next().unwrap_or("?").to_string();
        }
    }
    end(&mut frames, &mut comm, &mut out);
    out
}

/// A frame line, `   55d0c3a8b2c0 name+0x10 (/path/to/binary)`, as its name:
/// the symbol without its offset, or the file it was in, or the address.
fn frame_name(line: &str) -> Option<String> {
    let mut it = line.split_whitespace();
    let addr = it.next()?;
    if addr.is_empty() || !addr.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let rest: Vec<&str> = it.collect();
    let (sym, dso) = match rest.iter().position(|t| t.starts_with('(')) {
        Some(i) => (
            rest[..i].join(" "),
            rest[i..]
                .join(" ")
                .trim_matches(|c| c == '(' || c == ')')
                .to_string(),
        ),
        None => (rest.join(" "), String::new()),
    };
    let sym = match sym.rsplit_once("+0x") {
        Some((name, off)) if !off.is_empty() && off.chars().all(|c| c.is_ascii_hexdigit()) => {
            name.to_string()
        }
        _ => sym,
    };
    if !sym.is_empty() && sym != "[unknown]" {
        return Some(sym);
    }
    if dso.is_empty() || dso == "[unknown]" {
        return Some(addr.to_string());
    }
    Some(format!(
        "[{}]",
        Path::new(&dso)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&dso)
    ))
}

#[derive(Default)]
struct Node {
    name: String,
    total: u64,
    kids: Vec<Node>,
}

impl Node {
    fn add(&mut self, path: &[&str], n: u64) {
        self.total += n;
        let Some((head, rest)) = path.split_first() else {
            return;
        };
        let i = match self.kids.iter().position(|k| k.name == *head) {
            Some(i) => i,
            None => {
                self.kids.push(Node {
                    name: (*head).to_string(),
                    ..Node::default()
                });
                self.kids.len() - 1
            }
        };
        self.kids[i].add(rest, n);
    }

    fn depth(&self) -> usize {
        1 + self.kids.iter().map(Node::depth).max().unwrap_or(0)
    }
}

/// `&`, `<`, `>` and the quotes, so a symbol like `Vec<u8>::push` is text in
/// the drawing and never markup.
pub(super) fn xml_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            c if c.is_control() => o.push(' '),
            c => o.push(c),
        }
    }
    o
}

/// A flamegraph: the command at the bottom, callees above their callers,
/// each frame as wide as its share of the samples.
pub(super) fn svg(folded: &BTreeMap<String, u64>, title: &str) -> String {
    const W: f64 = 1200.0;
    const PAD: f64 = 10.0;
    const H: f64 = 16.0;
    const TOP: f64 = 28.0;
    let mut root = Node {
        name: "all".into(),
        ..Node::default()
    };
    for (k, n) in folded {
        let path: Vec<&str> = k.split(';').collect();
        root.add(&path, *n);
    }
    let total = root.total.max(1) as f64;
    let height = TOP + root.depth() as f64 * H + PAD;
    let mut out = String::new();
    out.push_str(&format!(
        "<?xml version=\"1.0\" standalone=\"no\"?>\n<svg version=\"1.1\" width=\"{W}\" height=\"{height}\" viewBox=\"0 0 {W} {height}\" xmlns=\"http://www.w3.org/2000/svg\">\n"
    ));
    out.push_str(&format!(
        "<rect x=\"0\" y=\"0\" width=\"{W}\" height=\"{height}\" fill=\"#f8f8f8\"/>\n<text x=\"{PAD}\" y=\"18\" font-family=\"monospace\" font-size=\"13\">{}</text>\n",
        xml_escape(title)
    ));
    fn draw(n: &Node, depth: usize, x: f64, total: f64, height: f64, out: &mut String) {
        const W: f64 = 1200.0;
        const PAD: f64 = 10.0;
        const H: f64 = 16.0;
        let w = n.total as f64 / total * (W - 2.0 * PAD);
        if w < 0.3 {
            return;
        }
        let y = height - PAD - (depth as f64 + 1.0) * H;
        let mut h: u32 = 5381;
        for b in n.name.bytes() {
            h = h.wrapping_mul(33).wrapping_add(u32::from(b));
        }
        let (r, g, b) = (205 + h % 50, 80 + (h >> 8) % 130, (h >> 16) % 55);
        out.push_str(&format!(
            "<g><title>{} ({} samples, {:.2}%)</title><rect x=\"{x:.2}\" y=\"{y:.2}\" width=\"{w:.2}\" height=\"{:.2}\" fill=\"rgb({r},{g},{b})\" rx=\"2\"/>",
            xml_escape(&n.name),
            n.total,
            100.0 * n.total as f64 / total,
            H - 1.0
        ));
        let room = ((w - 4.0) / 6.5).floor();
        if room >= 3.0 {
            let chars: Vec<char> = n.name.chars().collect();
            let label: String = if chars.len() as f64 <= room {
                n.name.clone()
            } else {
                chars[..(room as usize).saturating_sub(2)]
                    .iter()
                    .collect::<String>()
                    + ".."
            };
            out.push_str(&format!(
                "<text x=\"{:.2}\" y=\"{:.2}\" font-family=\"monospace\" font-size=\"11\">{}</text>",
                x + 2.0,
                y + 11.0,
                xml_escape(&label)
            ));
        }
        out.push_str("</g>\n");
        let mut cx = x;
        for k in &n.kids {
            draw(k, depth + 1, cx, total, height, out);
            cx += k.total as f64 / total * (W - 2.0 * PAD);
        }
    }
    draw(&root, 0, PAD, total, height, &mut out);
    out.push_str("</svg>\n");
    out
}

/// The frames with the most samples of their own (the innermost frame of a
/// stack is the one the sample was taken in).
pub(super) fn top_self(folded: &BTreeMap<String, u64>, n: usize) -> Vec<(String, u64)> {
    let mut by: BTreeMap<&str, u64> = BTreeMap::new();
    for (k, c) in folded {
        let leaf = k.rsplit(';').next().unwrap_or(k);
        *by.entry(leaf).or_insert(0) += c;
    }
    let mut v: Vec<(String, u64)> = by.into_iter().map(|(k, c)| (k.to_string(), c)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v.truncate(n);
    v
}

/// A case's name as a file stem.
fn stem(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() {
        "case".into()
    } else {
        s
    }
}

fn profiles_dir() -> PathBuf {
    PathBuf::from(".cig-bench").join("profiles")
}

// ----- running perf ----------------------------------------------------------

impl Prepared<'_> {
    /// One run of the case under `perf record`, its data in `data`: the
    /// run's wall time in milliseconds. `Err` begins with `perf is not
    /// available here` when perf cannot be used at all.
    pub(super) fn perf_record(&self, home_name: &str, data: &Path) -> Result<f64, String> {
        let (side, case, defaults) = (self.side, self.case, self.defaults);
        let home = self.work.join(home_name);
        let _ = std::fs::create_dir_all(&home);
        let env = full_env(defaults, &case.env, &home);
        run_befores(side, case, &env).map_err(|e| format!("`before` failed: {e}"))?;
        let mut argv: Vec<String> = vec![
            "perf".into(),
            "record".into(),
            "-F".into(),
            HZ.to_string(),
            "-g".into(),
            "-o".into(),
            data.to_string_lossy().into_owned(),
            "--".into(),
        ];
        argv.extend(side.argv(&case.run));
        let mut c = build_command(&argv, &side.dir, &env, &side.dir, case.stdin.as_deref())?;
        let started = Instant::now();
        let out = c
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    format!("{NOT_AVAILABLE}: perf is not installed")
                } else {
                    format!("{NOT_AVAILABLE}: could not start perf: {e}")
                }
            })?;
        let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
        self.tidy(&home);
        if out.status.code() != case.expect.exit {
            // The case is known to end as it expects, so this is perf
            // refusing (paranoid kernel, no PMU, a container).
            let err = String::from_utf8_lossy(&out.stderr);
            let tail: String = err
                .lines()
                .filter(|l| !l.trim().is_empty())
                .take(2)
                .collect::<Vec<_>>()
                .join(" ");
            return Err(format!(
                "{NOT_AVAILABLE}: perf record ended with {}{}",
                out.status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "a signal".into()),
                if tail.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", super::clip(&tail))
                }
            ));
        }
        Ok(wall_ms)
    }

    /// `perf script` over a recording: the text `fold` reads.
    fn perf_script(&self, data: &Path) -> Result<String, String> {
        let (side, case, defaults) = (self.side, self.case, self.defaults);
        let env = full_env(defaults, &case.env, &self.work);
        let argv: Vec<String> = vec![
            "perf".into(),
            "script".into(),
            "-i".into(),
            data.to_string_lossy().into_owned(),
        ];
        let mut c = build_command(&argv, &side.dir, &env, &side.dir, None)?;
        let out = c
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("could not start perf script: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "perf script ended with {} ({})",
                out.status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "a signal".into()),
                super::clip(String::from_utf8_lossy(&out.stderr).trim())
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// A recording drawn and written to `.cig-bench/profiles/<file>.svg`.
    fn draw_profile(&self, data: &Path, file_stem: &str, note: &str) -> Result<Profile, String> {
        let script = self
            .perf_script(data)
            .map_err(|e| format!("not measured: {e}"))?;
        let folded = fold(&script);
        let samples: u64 = folded.values().sum();
        if samples == 0 {
            return Err(
                "not measured: perf took no samples with a call chain (the run was too short, or perf could not walk its stacks)"
                    .into(),
            );
        }
        let title = format!(
            "{}{note}: {samples} samples, perf at {HZ} Hz",
            self.case.name
        );
        let doc = svg(&folded, &title);
        let dir = profiles_dir();
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("not measured: cannot make {}: {e}", dir.display()))?;
        let path = dir.join(format!("{file_stem}.svg"));
        let tmp = dir.join(format!(".{file_stem}.svg.tmp"));
        std::fs::write(&tmp, doc)
            .and_then(|()| std::fs::rename(&tmp, &path))
            .map_err(|e| format!("not measured: cannot write {}: {e}", path.display()))?;
        Ok(Profile {
            svg: path,
            samples,
            top: top_self(&folded, 5),
        })
    }

    /// `--profile`: one extra run under perf, drawn.
    pub(super) fn profile_run(&self) -> Result<Profile, String> {
        let data = self.work.join("perf.data");
        self.perf_record("profile-home", &data)
            .map_err(|e| format!("not measured: {e}"))?;
        let p = self.draw_profile(&data, &stem(&self.case.name), "");
        let _ = std::fs::remove_file(&data);
        p
    }

    /// `--repeat-until-slow`.
    pub(super) fn repeat_until_slow(&self) -> Result<Slow, String> {
        let reference_n = self.defaults.samples.max(MIN_REFERENCE);
        let started = Instant::now();
        let mut walls: Vec<f64> = Vec::new();
        let mut perf = true;
        let mut note: Option<String> = None;
        let mut threshold = f64::INFINITY;
        let mut p90 = 0.0;
        let mut i = 0usize;
        loop {
            if i >= reference_n + MAX_RERUNS || started.elapsed() > Duration::from_secs(MAX_SECONDS)
            {
                return Ok(Slow {
                    runs: i,
                    reference_p90_ms: p90,
                    threshold_ms: threshold,
                    found: None,
                    note,
                    gave_up: Some(format!(
                        "no run was over {threshold:.2} ms in {} reruns and {:.0} s",
                        i.saturating_sub(reference_n),
                        started.elapsed().as_secs_f64()
                    )),
                });
            }
            let mut recording: Option<PathBuf> = None;
            let mut phases: Option<(Vec<Reading>, Vec<String>)> = None;
            let mut wall = None;
            if perf {
                let data = self.work.join(format!("repeat-{i}.data"));
                match self.perf_record(&format!("repeat-{i}-home"), &data) {
                    Ok(w) => {
                        wall = Some(w);
                        recording = Some(data);
                    }
                    Err(e) if e.starts_with(NOT_AVAILABLE) => {
                        note = Some(format!("not measured: {e}; the phases stand in"));
                        perf = false;
                    }
                    Err(e) => return Err(format!("not measured: {e}")),
                }
            }
            if !perf {
                match self.phase_run_raw(true, false, &format!("repeat-{i}-home")) {
                    Ok((w, readings, unread)) => {
                        wall = Some(w);
                        phases = Some((readings, unread));
                    }
                    Err(e) => return Err(e),
                }
            }
            let wall = wall.expect("one of the two ran");
            i += 1;
            if walls.len() < reference_n {
                walls.push(wall);
                if walls.len() == reference_n {
                    p90 = Stats::of(&walls, None).p90_ms;
                    threshold = p90 * SLOW_FACTOR;
                }
                if let Some(d) = recording {
                    let _ = std::fs::remove_file(d);
                }
                continue;
            }
            if wall <= threshold {
                if let Some(d) = recording {
                    let _ = std::fs::remove_file(d);
                }
                continue;
            }
            // The slow one: keep what it recorded.
            let mut found = SlowRun {
                run: i,
                ms: wall,
                profile: None,
                phases: None,
            };
            if let Some(d) = recording {
                found.profile = Some(self.draw_profile(
                    &d,
                    &format!("{}-slow", stem(&self.case.name)),
                    &format!(" (run {i}, {wall:.2} ms)"),
                ));
                let _ = std::fs::remove_file(d);
            }
            if let Some((readings, unread)) = phases {
                found.phases = Some(Breakdown::of(wall, &readings, unread).ok_or_else(|| {
                    "not measured: the product wrote no phases, so only the time is known"
                        .to_string()
                }));
            }
            return Ok(Slow {
                runs: i,
                reference_p90_ms: p90,
                threshold_ms: threshold,
                found: Some(found),
                note,
                gave_up: None,
            });
        }
    }
}

// ----- output ----------------------------------------------------------------

fn print_profile(name: &str, p: &Result<Profile, String>) {
    match p {
        Err(e) => outln!("profile: {name}: {e}"),
        Ok(p) => {
            outln!(
                "profile: {name}: {} ({} samples at {HZ} Hz)",
                p.svg.display(),
                p.samples
            );
            let total = p.samples.max(1) as f64;
            for (f, n) in &p.top {
                outln!("  {:>6.1}%  {f}", 100.0 * *n as f64 / total);
            }
        }
    }
}

pub(super) fn print_text(name: &str, i: &super::debug::Insight) {
    if let Some(p) = &i.profile {
        outln!("");
        print_profile(name, p);
    }
    if let Some(s) = &i.slow {
        outln!("");
        match s {
            Err(e) => outln!("repeat-until-slow: {name}: {e}"),
            Ok(s) => {
                if let Some(n) = &s.note {
                    outln!("repeat-until-slow: {name}: {n}");
                }
                match &s.found {
                    None => outln!(
                        "repeat-until-slow: {name}: {}",
                        s.gave_up.clone().unwrap_or_else(|| "no slow run".into())
                    ),
                    Some(f) => {
                        outln!(
                            "repeat-until-slow: {name}: run {} took {:.2} ms, over {:.2} ms (the reference p90 {:.2} ms x {SLOW_FACTOR})",
                            f.run,
                            f.ms,
                            s.threshold_ms,
                            s.reference_p90_ms
                        );
                        if let Some(p) = &f.profile {
                            print_profile(name, p);
                        }
                        match &f.phases {
                            Some(Ok(b)) => {
                                outln!("  {:<28} {:>10}", "phase", "ms");
                                for p in &b.phases {
                                    outln!("  {:<28} {:>10.2}", p.name, p.ms);
                                }
                                outln!("  {:<28} {:>10.2}", "other", b.other_ms);
                            }
                            Some(Err(e)) => outln!("  {e}"),
                            None => {}
                        }
                    }
                }
            }
        }
    }
}

pub(super) fn print_markdown(name: &str, i: &super::debug::Insight) {
    if let Some(Ok(p)) = &i.profile {
        outln!("");
        outln!(
            "**profile, `{name}`**: `{}` ({} samples)",
            p.svg.display(),
            p.samples
        );
    } else if let Some(Err(e)) = &i.profile {
        outln!("");
        outln!("profile, `{name}`: {e}");
    }
    if let Some(s) = &i.slow {
        outln!("");
        match s {
            Err(e) => outln!("repeat-until-slow, `{name}`: {e}"),
            Ok(s) => match &s.found {
                None => outln!(
                    "repeat-until-slow, `{name}`: {}",
                    s.gave_up.clone().unwrap_or_default()
                ),
                Some(f) => outln!(
                    "repeat-until-slow, `{name}`: run {} took {:.2} ms, over {:.2} ms",
                    f.run,
                    f.ms,
                    s.threshold_ms
                ),
            },
        }
    }
}

pub(super) fn profile_json(p: &Result<Profile, String>) -> serde_json::Value {
    match p {
        Err(e) => serde_json::json!({ "measured": false, "note": e }),
        Ok(p) => serde_json::json!({
            "measured": true,
            "svg": p.svg.to_string_lossy(),
            "samples": p.samples,
            "hz": HZ,
            "top": p.top.iter().map(|(f, n)| serde_json::json!({ "frame": f, "samples": n })).collect::<Vec<_>>(),
        }),
    }
}

pub(super) fn slow_json(s: &Result<Slow, String>) -> serde_json::Value {
    match s {
        Err(e) => serde_json::json!({ "measured": false, "note": e }),
        Ok(s) => serde_json::json!({
            "measured": true,
            "runs": s.runs,
            "reference_p90_ms": s.reference_p90_ms,
            "threshold_ms": if s.threshold_ms.is_finite() { serde_json::json!(s.threshold_ms) } else { serde_json::Value::Null },
            "note": s.note,
            "gave_up": s.gave_up,
            "found": s.found.as_ref().map(|f| serde_json::json!({
                "run": f.run,
                "ms": f.ms,
                "profile": f.profile.as_ref().map(profile_json),
                "phases": f.phases.as_ref().map(|p| match p {
                    Err(e) => serde_json::json!({ "measured": false, "note": e }),
                    Ok(b) => serde_json::json!({
                        "measured": true,
                        "phases": b.phases.iter().map(|p| serde_json::json!({
                            "product": p.product, "phase": p.name, "ms": p.ms })).collect::<Vec<_>>(),
                        "other_ms": b.other_ms,
                    }),
                }),
            })),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCRIPT: &str = "prog 1234 100.1:     250000 cpu-clock:u:\n\
        \t    55d0c3a8b2c0 leaf_fn+0x10 (/usr/bin/prog)\n\
        \t    55d0c3a8b100 mid_fn+0x4 (/usr/bin/prog)\n\
        \t    55d0c3a8b000 main+0x1 (/usr/bin/prog)\n\
        \n\
        prog 1234 100.2:     250000 cpu-clock:u:\n\
        \t    55d0c3a8b2c0 leaf_fn+0x10 (/usr/bin/prog)\n\
        \t    55d0c3a8b100 mid_fn+0x4 (/usr/bin/prog)\n\
        \t    55d0c3a8b000 main+0x1 (/usr/bin/prog)\n\
        \n\
        prog 1234 100.3:     250000 cpu-clock:u:\n\
        \t    7f0000001000 [unknown] (/lib/libc.so.6)\n\
        \t    55d0c3a8b000 main+0x1 (/usr/bin/prog)\n\
        \n";

    #[test]
    fn perf_text_folds_into_stacks_outermost_first() {
        let f = fold(SCRIPT);
        assert_eq!(f.get("prog;main;mid_fn;leaf_fn"), Some(&2), "{f:?}");
        assert_eq!(f.get("prog;main;[libc.so.6]"), Some(&1), "{f:?}");
        assert_eq!(f.values().sum::<u64>(), 3);
    }

    #[test]
    fn a_header_with_no_frames_is_not_a_stack() {
        assert!(fold("prog 1 2: x\n\nprog 1 3: y\n").is_empty());
    }

    #[test]
    fn the_hottest_frames_are_the_ones_with_self_samples() {
        let f = fold(SCRIPT);
        let top = top_self(&f, 2);
        assert_eq!(top[0], ("leaf_fn".to_string(), 2));
        assert_eq!(top[1], ("[libc.so.6]".to_string(), 1));
    }

    #[test]
    fn the_drawing_is_an_svg_and_names_are_text_not_markup() {
        let mut f = BTreeMap::new();
        f.insert("prog;main;Vec<u8>::push&\"x\"".to_string(), 3);
        f.insert("prog;main;other".to_string(), 1);
        let s = svg(&f, "case <one>");
        assert!(s.starts_with("<?xml"), "{s}");
        assert!(s.contains("<svg ") && s.trim_end().ends_with("</svg>"));
        assert!(s.contains("Vec&lt;u8&gt;::push&amp;&quot;x&quot;"), "{s}");
        assert!(!s.contains("Vec<u8>"), "{s}");
        assert!(s.contains("case &lt;one&gt;"));
        // The widest frame is the root: all four samples.
        assert!(s.contains("all (4 samples, 100.00%)"), "{s}");
    }

    #[test]
    fn case_names_become_file_stems() {
        assert_eq!(stem("burn small tree"), "burn_small_tree");
        assert_eq!(stem("../x"), "___x");
        assert_eq!(stem(""), "case");
    }
}
