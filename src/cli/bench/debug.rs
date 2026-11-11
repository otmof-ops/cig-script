// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The debugging flags of `cig bench` (#179 step 4): `--timings`, `--stats`
//! and `--syscalls`.
//!
//! Each takes one extra run of the case that is never timed (the case's
//! numbers do not change by asking), after the samples:
//!
//! - `--timings` and `--stats` hand the product a descriptor (9) named by
//!   `CIG_TIMINGS_FD` and `CIG_STATS_FD` and read back the lines it wrote
//!   (see `crate::timings` for the format). The phases are shown beside the
//!   case's own wall time with what is left over as `other`, so the
//!   breakdown always adds up to the wall time of the run it came from; a
//!   product whose phases add up to more than 5% over that wall is flagged.
//! - `--syscalls` counts every syscall in the run with `strace -c`, shows the
//!   top ten by time and flags an mmap/munmap thrash (#153): at least 100
//!   pairs and more than 1,000 a second of the case's own median wall time.
//!
//! All of it is reported, never gated (#153). Off Unix, or where `strace` is
//! absent or forbidden, a number is `not measured` with the reason; it is
//! never guessed.

use super::{build_command, clip, full_env, measures, run_befores, Prepared};
use crate::timings::{self, Reading};
use std::process::Stdio;
use std::time::Instant;

/// The descriptor a product writes its phases and counters to.
#[cfg(unix)]
pub(super) const FD: i32 = 9;
/// A breakdown whose phases pass the wall time by more than this is flagged.
#[cfg_attr(not(unix), allow(dead_code))]
const OVERLAP: f64 = 0.05;
/// mmap/munmap pairs per second above which a case is flagged, and the
/// fewest pairs worth flagging (a short run's start-up maps are not a loop).
const THRASH_PER_SECOND: f64 = 1000.0;
const THRASH_MIN_PAIRS: u64 = 100;

/// Which of the three a run was asked for.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Wants {
    pub(super) timings: bool,
    pub(super) stats: bool,
    pub(super) syscalls: bool,
    /// `--profile`: a flamegraph of one extra run (perf).
    pub(super) profile: bool,
    /// `--repeat-until-slow`: rerun until a run is over p90 x 1.5.
    pub(super) repeat: bool,
}

impl Wants {
    pub(super) fn any(&self) -> bool {
        self.timings || self.stats || self.syscalls || self.profile || self.repeat
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Phase {
    pub product: String,
    pub name: String,
    pub ms: f64,
}

/// Where one run's time went.
#[derive(Debug, Clone, PartialEq)]
pub struct Breakdown {
    /// The extra run's own wall time, in milliseconds.
    pub wall_ms: f64,
    pub phases: Vec<Phase>,
    /// Wall time minus the phases; negative when they overlap.
    pub other_ms: f64,
    /// The phases add up to more than the wall time by over 5%.
    pub overlap: bool,
    /// Lines the reader could not use.
    pub unread: Vec<String>,
}

impl Breakdown {
    /// Phases of the same product and name are added (a product that times a
    /// phase twice has one phase); order is the order they first ended.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub fn of(wall_ms: f64, readings: &[Reading], unread: Vec<String>) -> Option<Breakdown> {
        let mut phases: Vec<Phase> = Vec::new();
        for r in readings {
            if let Reading::Phase { product, phase, ms } = r {
                match phases
                    .iter_mut()
                    .find(|p| &p.product == product && &p.name == phase)
                {
                    Some(p) => p.ms += ms,
                    None => phases.push(Phase {
                        product: product.clone(),
                        name: phase.clone(),
                        ms: *ms,
                    }),
                }
            }
        }
        if phases.is_empty() {
            return None;
        }
        let sum: f64 = phases.iter().map(|p| p.ms).sum();
        Some(Breakdown {
            wall_ms,
            other_ms: wall_ms - sum,
            overlap: sum > wall_ms * (1.0 + OVERLAP),
            phases,
            unread,
        })
    }

    pub fn sum_ms(&self) -> f64 {
        self.phases.iter().map(|p| p.ms).sum()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Counter {
    pub product: String,
    pub name: String,
    /// `Err` is why it is not measured.
    pub value: Result<f64, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Syscall {
    pub name: String,
    pub calls: u64,
    pub seconds: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SyscallReport {
    pub total: u64,
    /// Every syscall seen, by time, then by calls.
    pub all: Vec<Syscall>,
    /// The thrash flag's sentence, when it applies.
    pub thrash: Option<String>,
}

impl SyscallReport {
    pub fn top(&self) -> &[Syscall] {
        &self.all[..self.all.len().min(10)]
    }
}

/// What the three flags found for one case. `None` is not asked for.
#[derive(Debug, Default, Clone)]
pub struct Insight {
    pub timings: Option<Result<Breakdown, String>>,
    pub stats: Option<Result<Vec<Counter>, String>>,
    pub syscalls: Option<Result<SyscallReport, String>>,
    pub profile: Option<Result<super::profile::Profile, String>>,
    pub slow: Option<Result<super::profile::Slow, String>>,
}

impl Insight {
    pub fn is_empty(&self) -> bool {
        self.timings.is_none()
            && self.stats.is_none()
            && self.syscalls.is_none()
            && self.profile.is_none()
            && self.slow.is_none()
    }
}

type PhaseResult = (
    Option<Result<Breakdown, String>>,
    Option<Result<Vec<Counter>, String>>,
);

impl Prepared<'_> {
    /// The extra runs the flags ask for, after the samples have been taken.
    /// `median_ms` is the case's own median wall time.
    pub(super) fn insight(&self, wants: Wants, median_ms: f64) -> Insight {
        let mut out = Insight::default();
        if wants.timings || wants.stats {
            let (t, s) = self.phase_run(wants);
            out.timings = t;
            out.stats = s;
        }
        if wants.syscalls {
            out.syscalls = Some(self.syscall_run(median_ms));
        }
        if wants.profile {
            out.profile = Some(self.profile_run());
        }
        if wants.repeat {
            out.slow = Some(self.repeat_until_slow());
        }
        out
    }

    /// One run with the descriptor handed over; its lines read back.
    fn phase_run(&self, wants: Wants) -> PhaseResult {
        let raw = self.phase_run_raw(wants.timings, wants.stats, "timings-home");
        let (wall_ms, readings, unread) = match raw {
            Ok(r) => r,
            Err(e) => {
                return (
                    wants.timings.then(|| Err(e.clone())),
                    wants.stats.then_some(Err(e)),
                )
            }
        };
        let t = wants.timings.then(|| {
            Breakdown::of(wall_ms, &readings, unread.clone()).ok_or_else(|| {
                format!(
                    "not measured: the product wrote no phases to {} (it does not speak the timings format{})",
                    timings::TIMINGS_ENV,
                    unread
                        .first()
                        .map(|u| format!("; first line it wrote: {}", clip(u)))
                        .unwrap_or_default()
                )
            })
        });
        let s = wants.stats.then(|| {
            let counters: Vec<Counter> = readings
                .iter()
                .filter_map(|r| match r {
                    Reading::Counter {
                        product,
                        counter,
                        value,
                    } => Some(Counter {
                        product: product.clone(),
                        name: counter.clone(),
                        value: Ok(*value),
                    }),
                    Reading::Unavailable {
                        product,
                        counter,
                        why,
                    } => Some(Counter {
                        product: product.clone(),
                        name: counter.clone(),
                        value: Err(format!("not measured: {why}")),
                    }),
                    Reading::Phase { .. } => None,
                })
                .collect();
            if counters.is_empty() {
                Err(format!(
                    "not measured: the product wrote no counters to {}",
                    timings::STATS_ENV
                ))
            } else {
                Ok(counters)
            }
        });
        (t, s)
    }

    /// One run with the descriptor handed over: its wall time in
    /// milliseconds, what it wrote and the lines nobody could read. `Err` is
    /// why the run could not be made.
    #[cfg(unix)]
    pub(super) fn phase_run_raw(
        &self,
        timings_on: bool,
        stats_on: bool,
        home_name: &str,
    ) -> Result<(f64, Vec<Reading>, Vec<String>), String> {
        use std::os::unix::io::AsRawFd;
        use std::os::unix::process::CommandExt;
        let (side, case, defaults) = (self.side, self.case, self.defaults);
        let home = self.work.join(home_name);
        let _ = std::fs::create_dir_all(&home);
        let mut env = full_env(defaults, &case.env, &home);
        if timings_on {
            env.insert(timings::TIMINGS_ENV.into(), FD.to_string().into());
        }
        if stats_on {
            env.insert(timings::STATS_ENV.into(), FD.to_string().into());
        }
        if let Err(e) = run_befores(side, case, &env) {
            return Err(format!("not measured: `before` failed: {e}"));
        }
        let sink = self.work.join(format!("{home_name}.jsonl"));
        let file = match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&sink)
        {
            Ok(f) => f,
            Err(e) => return Err(format!("not measured: cannot make {}: {e}", sink.display())),
        };
        let argv = side.argv(&case.run);
        let mut c = match build_command(&argv, &side.dir, &env, &side.dir, case.stdin.as_deref()) {
            Ok(c) => c,
            Err(e) => return Err(format!("not measured: {e}")),
        };
        let raw = file.as_raw_fd();
        // SAFETY: between fork and exec only dup2 and fcntl run, both
        // async-signal-safe; `raw` stays open in the parent until the child
        // has ended.
        unsafe {
            c.pre_exec(move || {
                if raw == FD {
                    let flags = libc::fcntl(FD, libc::F_GETFD);
                    libc::fcntl(FD, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
                } else if libc::dup2(raw, FD) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let started = Instant::now();
        let status = c.stdout(Stdio::null()).stderr(Stdio::null()).status();
        let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
        drop(file);
        match status {
            Ok(s) if s.code() == case.expect.exit => {}
            Ok(s) => {
                return Err(format!(
                    "not measured: the run ended with {} not {:?}",
                    s.code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "a signal".into()),
                    case.expect.exit
                ))
            }
            Err(e) => return Err(format!("not measured: could not start: {e}")),
        }
        let text = std::fs::read_to_string(&sink).unwrap_or_default();
        self.tidy(&home);
        let (readings, unread) = timings::parse(&text);
        Ok((wall_ms, readings, unread))
    }

    #[cfg(not(unix))]
    pub(super) fn phase_run_raw(
        &self,
        _timings_on: bool,
        _stats_on: bool,
        _home_name: &str,
    ) -> Result<(f64, Vec<Reading>, Vec<String>), String> {
        Err("not measured: handing a product a descriptor is Unix only".to_string())
    }

    fn syscall_run(&self, median_ms: f64) -> Result<SyscallReport, String> {
        let (summary, wall_ms) = self.strace_summary("syscalls-home", None)?;
        let all = parse_strace(&summary);
        if all.is_empty() {
            return Err("strace printed no syscall table".into());
        }
        let wall = if median_ms > 0.0 { median_ms } else { wall_ms };
        let total = all.iter().map(|s| s.calls).sum();
        let thrash = thrash_note(&all, wall);
        Ok(SyscallReport { total, all, thrash })
    }

    /// One run of the case under `strace -f -c`, optionally only tracing
    /// `filter` (a `-e trace=` list): the summary table and the run's wall
    /// time under strace. Shared by `fsyncs-per-op` and `--syscalls`.
    pub(super) fn strace_summary(
        &self,
        home_name: &str,
        filter: Option<&str>,
    ) -> Result<(String, f64), String> {
        let (side, case, defaults) = (self.side, self.case, self.defaults);
        let home = self.work.join(home_name);
        let _ = std::fs::create_dir_all(&home);
        let env = full_env(defaults, &case.env, &home);
        run_befores(side, case, &env)?;
        let out = self.work.join(format!("{home_name}-strace.txt"));
        let argv = measures::strace_argv(&out, &side.argv(&case.run), filter)?;
        let mut c = build_command(&argv, &side.dir, &env, &side.dir, case.stdin.as_deref())?;
        let started = Instant::now();
        let res = c
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    "strace is not installed".to_string()
                } else {
                    format!("could not start strace: {e}")
                }
            })?;
        let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
        let code = res.status.code();
        if code != case.expect.exit {
            return Err(format!(
                "strace ended with {} (stderr: {})",
                code.map(|c| c.to_string())
                    .unwrap_or_else(|| "a signal".into()),
                clip(String::from_utf8_lossy(&res.stderr).trim())
            ));
        }
        let summary = std::fs::read_to_string(&out).unwrap_or_default();
        self.tidy(&home);
        Ok((summary, wall_ms))
    }
}

/// The rows of a `strace -c` table, most time first. The header, the rule
/// lines and the `total` row are not syscalls.
pub(super) fn parse_strace(summary: &str) -> Vec<Syscall> {
    let mut out = Vec::new();
    for line in summary.lines() {
        let t: Vec<&str> = line.split_whitespace().collect();
        // % time, seconds, usecs/call, calls, [errors], syscall
        if t.len() < 5 || t[0].parse::<f64>().is_err() {
            continue;
        }
        let name = t[t.len() - 1];
        if name == "total" {
            continue;
        }
        if let (Ok(seconds), Ok(calls)) = (t[1].parse::<f64>(), t[3].parse::<u64>()) {
            out.push(Syscall {
                name: name.to_string(),
                calls,
                seconds,
            });
        }
    }
    out.sort_by(|a, b| {
        b.seconds
            .total_cmp(&a.seconds)
            .then(b.calls.cmp(&a.calls))
            .then(a.name.cmp(&b.name))
    });
    out
}

/// The thrash flag: mmap and munmap in pairs, above 1,000 a second of the
/// run's wall time and at least 100 of them (#153).
pub(super) fn thrash_note(all: &[Syscall], wall_ms: f64) -> Option<String> {
    let n = |names: &[&str]| -> u64 {
        all.iter()
            .filter(|s| names.contains(&s.name.as_str()))
            .map(|s| s.calls)
            .sum()
    };
    let pairs = n(&["mmap", "mmap2"]).min(n(&["munmap"]));
    if wall_ms <= 0.0 {
        return None;
    }
    let rate = pairs as f64 / (wall_ms / 1000.0);
    if pairs >= THRASH_MIN_PAIRS && rate > THRASH_PER_SECOND {
        Some(format!(
            "mmap/munmap thrash: {pairs} pairs in one run of {wall_ms:.1} ms ({rate:.0} a second, over {THRASH_PER_SECOND:.0}); the allocator is mapping and unmapping instead of reusing (#153)"
        ))
    } else {
        None
    }
}

// ----- output ----------------------------------------------------------------

fn share(ms: f64, wall: f64) -> f64 {
    100.0 * ms / wall.max(f64::MIN_POSITIVE)
}

/// The plain table's section for one case.
pub(super) fn print_text(name: &str, median_ms: Option<f64>, i: &Insight) {
    if let Some(t) = &i.timings {
        outln!("");
        match t {
            Err(e) => outln!("timings: {name}: {e}"),
            Ok(b) => {
                outln!(
                    "timings: {name} (one extra run, {:.1} ms{})",
                    b.wall_ms,
                    median_ms
                        .map(|m| format!("; the case's median is {m:.1} ms"))
                        .unwrap_or_default()
                );
                outln!("  {:<28} {:>10} {:>7}", "phase", "ms", "wall");
                for p in &b.phases {
                    let label = if p.product == "cig" {
                        p.name.clone()
                    } else {
                        format!("{}: {}", p.product, p.name)
                    };
                    outln!(
                        "  {:<28} {:>10.2} {:>6.1}%",
                        label,
                        p.ms,
                        share(p.ms, b.wall_ms)
                    );
                }
                outln!(
                    "  {:<28} {:>10.2} {:>6.1}%",
                    "other",
                    b.other_ms,
                    share(b.other_ms, b.wall_ms)
                );
                outln!("  {:<28} {:>10.2} {:>6.1}%", "wall", b.wall_ms, 100.0);
                if b.overlap {
                    outln!(
                        "  the phases add up to {:.2} ms, more than 5% over the wall time: they overlap or repeat",
                        b.sum_ms()
                    );
                }
                for u in &b.unread {
                    outln!("  unread: {}", clip(u));
                }
            }
        }
    }
    if let Some(s) = &i.stats {
        outln!("");
        match s {
            Err(e) => outln!("stats: {name}: {e}"),
            Ok(cs) => {
                outln!("stats: {name} (one extra run)");
                for c in cs {
                    let label = format!("{}: {}", c.product, c.name);
                    match &c.value {
                        Ok(v) => outln!("  {:<28} {:>14}", label, group(*v)),
                        Err(e) => outln!("  {:<28} {}", label, e),
                    }
                }
            }
        }
    }
    if let Some(s) = &i.syscalls {
        outln!("");
        match s {
            Err(e) => outln!("syscalls: {name}: not measured: {e}"),
            Ok(r) => {
                outln!(
                    "syscalls: {name}: {} in one run under strace, top {} by time",
                    group(r.total as f64),
                    r.top().len()
                );
                outln!("  {:<24} {:>10} {:>10}", "syscall", "calls", "ms");
                for c in r.top() {
                    outln!(
                        "  {:<24} {:>10} {:>10.3}",
                        c.name,
                        c.calls,
                        c.seconds * 1000.0
                    );
                }
                if let Some(t) = &r.thrash {
                    outln!("  FLAG: {t}");
                }
            }
        }
    }
    super::profile::print_text(name, i);
}

fn group(v: f64) -> String {
    let s = format!("{:.0}", v.abs());
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if v < 0.0 {
        format!("-{out}")
    } else {
        out
    }
}

pub(super) fn print_markdown(name: &str, i: &Insight) {
    match &i.timings {
        Some(Ok(b)) => {
            outln!("");
            outln!("**timings, `{name}`** ({:.1} ms)", b.wall_ms);
            outln!("");
            outln!("| phase | ms | wall |");
            outln!("|---|---:|---:|");
            for p in &b.phases {
                outln!(
                    "| {} | {:.2} | {:.1}% |",
                    p.name,
                    p.ms,
                    share(p.ms, b.wall_ms)
                );
            }
            outln!(
                "| other | {:.2} | {:.1}% |",
                b.other_ms,
                share(b.other_ms, b.wall_ms)
            );
        }
        Some(Err(e)) => {
            outln!("");
            outln!("timings, `{name}`: {e}");
        }
        None => {}
    }
    match &i.stats {
        Some(Ok(cs)) => {
            outln!("");
            outln!("| `{name}` counter | value |");
            outln!("|---|---:|");
            for c in cs {
                match &c.value {
                    Ok(v) => outln!("| {} | {} |", c.name, group(*v)),
                    Err(e) => outln!("| {} | {} |", c.name, e),
                }
            }
        }
        Some(Err(e)) => {
            outln!("");
            outln!("stats, `{name}`: {e}");
        }
        None => {}
    }
    match &i.syscalls {
        Some(Ok(r)) => {
            outln!("");
            outln!("| `{name}` syscall | calls | ms |");
            outln!("|---|---:|---:|");
            for c in r.top() {
                outln!("| {} | {} | {:.3} |", c.name, c.calls, c.seconds * 1000.0);
            }
            if let Some(t) = &r.thrash {
                outln!("");
                outln!("FLAG: {t}");
            }
        }
        Some(Err(e)) => {
            outln!("");
            outln!("syscalls, `{name}`: not measured: {e}");
        }
        None => {}
    }
    super::profile::print_markdown(name, i);
}

/// The insight as the JSON a case carries under `debug`.
pub(super) fn to_json(i: &Insight) -> serde_json::Value {
    let timings = i.timings.as_ref().map(|t| match t {
        Err(e) => serde_json::json!({ "measured": false, "note": e }),
        Ok(b) => serde_json::json!({
            "measured": true,
            "format": timings::VERSION,
            "wall_ms": b.wall_ms,
            "phases": b.phases.iter().map(|p| serde_json::json!({
                "product": p.product, "phase": p.name, "ms": p.ms })).collect::<Vec<_>>(),
            "other_ms": b.other_ms,
            "overlap": b.overlap,
            "unread": b.unread,
        }),
    });
    let stats = i.stats.as_ref().map(|s| match s {
        Err(e) => serde_json::json!({ "measured": false, "note": e }),
        Ok(cs) => serde_json::json!({
            "measured": true,
            "counters": cs.iter().map(|c| match &c.value {
                Ok(v) => serde_json::json!({ "product": c.product, "counter": c.name, "value": v }),
                Err(e) => serde_json::json!({ "product": c.product, "counter": c.name, "value": null, "note": e }),
            }).collect::<Vec<_>>(),
        }),
    });
    let syscalls = i.syscalls.as_ref().map(|s| match s {
        Err(e) => serde_json::json!({ "measured": false, "note": format!("not measured: {e}") }),
        Ok(r) => serde_json::json!({
            "measured": true,
            "total": r.total,
            "top": r.top().iter().map(|c| serde_json::json!({
                "syscall": c.name, "calls": c.calls, "ms": c.seconds * 1000.0 })).collect::<Vec<_>>(),
            "thrash": r.thrash,
        }),
    });
    serde_json::json!({
        "timings": timings,
        "stats": stats,
        "syscalls": syscalls,
        "profile": i.profile.as_ref().map(super::profile::profile_json),
        "repeat_until_slow": i.slow.as_ref().map(super::profile::slow_json),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ph(p: &str, n: &str, ms: f64) -> Reading {
        Reading::Phase {
            product: p.into(),
            phase: n.into(),
            ms,
        }
    }

    #[test]
    fn the_breakdown_adds_up_to_the_wall_with_other() {
        let r = [
            ph("cig", "check", 2.0),
            ph("cig", "run", 6.0),
            ph("cig", "check", 1.0),
        ];
        let b = Breakdown::of(10.0, &r, vec![]).unwrap();
        assert_eq!(b.phases.len(), 2, "the same phase twice is one");
        assert_eq!(b.phases[0].ms, 3.0);
        assert!((b.sum_ms() + b.other_ms - 10.0).abs() < 1e-9);
        assert!((b.other_ms - 1.0).abs() < 1e-9);
        assert!(!b.overlap);
        // Phases 20% over the wall time are flagged; 4% over are not.
        assert!(
            Breakdown::of(10.0, &[ph("x", "a", 12.0)], vec![])
                .unwrap()
                .overlap
        );
        assert!(
            !Breakdown::of(10.0, &[ph("x", "a", 10.4)], vec![])
                .unwrap()
                .overlap
        );
        assert!(Breakdown::of(10.0, &[], vec![]).is_none());
    }

    const THRASH: &str = "% time     seconds  usecs/call     calls    errors syscall\n\
        ------ ----------- ----------- --------- --------- ----------------\n\
         60.00    0.600000          12     50000           munmap\n\
         30.00    0.300000           6     50000           mmap\n\
         10.00    0.100000          50      2000       100 openat\n\
        ------ ----------- ----------- --------- --------- ----------------\n\
        100.00    1.000000                102000       100 total\n";

    #[test]
    fn strace_tables_are_read_by_time() {
        let all = parse_strace(THRASH);
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].name, "munmap");
        assert_eq!(all[2].calls, 2000);
    }

    #[test]
    fn a_thrash_is_flagged_and_a_start_up_is_not() {
        let all = parse_strace(THRASH);
        let note = thrash_note(&all, 400.0).expect("50,000 pairs in 400 ms");
        assert!(
            note.contains("50000 pairs") && note.contains("#153"),
            "{note}"
        );
        // The same counts over a long run are under 1,000 a second.
        assert!(thrash_note(&all, 100_000.0).is_none());
        // A start-up's few dozen maps are not a loop, however short the run.
        let quiet = vec![
            Syscall {
                name: "mmap".into(),
                calls: 40,
                seconds: 0.0,
            },
            Syscall {
                name: "munmap".into(),
                calls: 12,
                seconds: 0.0,
            },
        ];
        assert!(thrash_note(&quiet, 5.0).is_none());
    }

    #[test]
    fn numbers_get_commas() {
        assert_eq!(group(1234567.0), "1,234,567");
        assert_eq!(group(12.0), "12");
    }
}
