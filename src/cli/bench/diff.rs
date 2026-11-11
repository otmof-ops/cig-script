// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! `cig bench --diff <baseline>` (#179 step 8): two runs' phases, counters
//! and syscalls side by side, sorted by what changed most, so "the case got
//! 637 ms slower" becomes "the `run` phase did".
//!
//! The baseline's side is what `--save` wrote: whenever `--timings`,
//! `--stats` or `--syscalls` is on, a saved case also carries an `insight`
//! (its phases, counters and every syscall of the extra run, and the case's
//! median). `--diff NAME` switches all three on for the current run and sets
//! the two against each other. A baseline saved without them has nothing to
//! compare, and the diff says so rather than showing an empty table.
//!
//! Sorting, in each table: phases by the milliseconds that moved, syscalls
//! by the calls that moved (and, in the second table, by their time), and
//! counters by the share they moved (a counter that was not there before
//! comes first). All of it is reported, never gated.

use super::debug::Insight;

/// One side's numbers, by name.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Saved {
    /// The case's median wall time.
    pub wall_ms: Option<f64>,
    pub phases: Vec<(String, f64)>,
    pub counters: Vec<(String, f64)>,
    /// Name, calls, milliseconds.
    pub syscalls: Vec<(String, u64, f64)>,
    pub has_phases: bool,
    pub has_counters: bool,
    pub has_syscalls: bool,
}

fn label(product: &str, name: &str) -> String {
    if product == "cig" {
        name.to_string()
    } else {
        format!("{product}: {name}")
    }
}

/// What a run found, as the numbers a diff reads.
pub fn saved_of(i: &Insight, median_ms: Option<f64>) -> Saved {
    let mut s = Saved {
        wall_ms: median_ms,
        ..Saved::default()
    };
    if let Some(Ok(b)) = &i.timings {
        s.has_phases = true;
        for p in &b.phases {
            s.phases.push((label(&p.product, &p.name), p.ms));
        }
        s.phases.push(("other".into(), b.other_ms));
    }
    if let Some(Ok(cs)) = &i.stats {
        s.has_counters = true;
        for c in cs {
            if let Ok(v) = c.value {
                s.counters.push((label(&c.product, &c.name), v));
            }
        }
    }
    if let Some(Ok(r)) = &i.syscalls {
        s.has_syscalls = true;
        for c in &r.all {
            s.syscalls
                .push((c.name.clone(), c.calls, c.seconds * 1000.0));
        }
    }
    s
}

/// The `insight` a baseline keeps for a case; `None` when nothing was
/// measured.
pub fn saved_json(i: &Insight, median_ms: Option<f64>) -> Option<serde_json::Value> {
    let s = saved_of(i, median_ms);
    if !(s.has_phases || s.has_counters || s.has_syscalls) {
        return None;
    }
    Some(serde_json::json!({
        "wall_ms": s.wall_ms,
        "phases": s.has_phases.then(|| s.phases.iter().map(|(n, ms)| serde_json::json!({ "name": n, "ms": ms })).collect::<Vec<_>>()),
        "counters": s.has_counters.then(|| s.counters.iter().map(|(n, v)| serde_json::json!({ "name": n, "value": v })).collect::<Vec<_>>()),
        "syscalls": s.has_syscalls.then(|| s.syscalls.iter().map(|(n, c, ms)| serde_json::json!({ "name": n, "calls": c, "ms": ms })).collect::<Vec<_>>()),
    }))
}

/// A baseline's `insight` read back.
pub fn saved_from_json(v: &serde_json::Value) -> Saved {
    let mut s = Saved {
        wall_ms: v["wall_ms"].as_f64(),
        ..Saved::default()
    };
    if let Some(a) = v["phases"].as_array() {
        s.has_phases = true;
        for p in a {
            if let (Some(n), Some(ms)) = (p["name"].as_str(), p["ms"].as_f64()) {
                s.phases.push((n.to_string(), ms));
            }
        }
    }
    if let Some(a) = v["counters"].as_array() {
        s.has_counters = true;
        for c in a {
            if let (Some(n), Some(x)) = (c["name"].as_str(), c["value"].as_f64()) {
                s.counters.push((n.to_string(), x));
            }
        }
    }
    if let Some(a) = v["syscalls"].as_array() {
        s.has_syscalls = true;
        for c in a {
            if let (Some(n), Some(calls), Some(ms)) =
                (c["name"].as_str(), c["calls"].as_u64(), c["ms"].as_f64())
            {
                s.syscalls.push((n.to_string(), calls, ms));
            }
        }
    }
    s
}

/// One named number on both sides; `None` is absent from that side.
#[derive(Debug, Clone, PartialEq)]
pub struct Delta {
    pub name: String,
    pub before: Option<f64>,
    pub after: Option<f64>,
}

impl Delta {
    pub fn change(&self) -> f64 {
        self.after.unwrap_or(0.0) - self.before.unwrap_or(0.0)
    }

    /// The change as a share of what it was; `None` when it was nothing.
    pub fn percent(&self) -> Option<f64> {
        match self.before {
            Some(b) if b > 0.0 => Some(100.0 * self.change() / b),
            _ => None,
        }
    }

    /// A key that puts what moved most first: the share for a counter, with
    /// anything new at the top.
    fn weight(&self) -> f64 {
        match self.percent() {
            Some(p) => p.abs(),
            None if self.change() != 0.0 => f64::INFINITY,
            None => 0.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Diff {
    pub baseline: String,
    pub wall: Delta,
    pub phases: Vec<Delta>,
    pub counters: Vec<Delta>,
    pub syscalls: Vec<Delta>,
    pub syscall_ms: Vec<Delta>,
    /// What there is nothing to compare for, and why.
    pub notes: Vec<String>,
}

fn merge<A: Clone>(
    before: &[(String, A)],
    after: &[(String, A)],
    num: impl Fn(&A) -> f64,
) -> Vec<Delta> {
    let mut out: Vec<Delta> = Vec::new();
    for (n, v) in before {
        out.push(Delta {
            name: n.clone(),
            before: Some(num(v)),
            after: None,
        });
    }
    for (n, v) in after {
        match out.iter_mut().find(|d| &d.name == n) {
            Some(d) => d.after = Some(num(v)),
            None => out.push(Delta {
                name: n.clone(),
                before: None,
                after: Some(num(v)),
            }),
        }
    }
    out
}

fn by_change(v: &mut [Delta]) {
    v.sort_by(|a, b| {
        b.change()
            .abs()
            .total_cmp(&a.change().abs())
            .then(a.name.cmp(&b.name))
    });
}

/// The baseline's numbers against this run's.
pub fn diff(baseline: &str, before: &Saved, after: &Saved) -> Diff {
    let mut notes = Vec::new();
    let mut section = |what: &str, flag: &str, b: bool, a: bool| {
        if !b {
            notes.push(format!(
                "the baseline has no {what}: save it with `--save {baseline} {flag}`"
            ));
        }
        if !a {
            notes.push(format!("this run has no {what}"));
        }
        b && a
    };
    let ph = section("phases", "--timings", before.has_phases, after.has_phases);
    let ct = section(
        "counters",
        "--stats",
        before.has_counters,
        after.has_counters,
    );
    let sy = section(
        "syscalls",
        "--syscalls",
        before.has_syscalls,
        after.has_syscalls,
    );
    let mut phases = if ph {
        merge(&before.phases, &after.phases, |x| *x)
    } else {
        Vec::new()
    };
    by_change(&mut phases);
    let mut counters = if ct {
        merge(&before.counters, &after.counters, |x| *x)
    } else {
        Vec::new()
    };
    counters.sort_by(|a, b| {
        b.weight()
            .total_cmp(&a.weight())
            .then(b.change().abs().total_cmp(&a.change().abs()))
            .then(a.name.cmp(&b.name))
    });
    let (mut syscalls, mut syscall_ms) = (Vec::new(), Vec::new());
    if sy {
        let calls = |v: &[(String, u64, f64)]| -> Vec<(String, u64)> {
            v.iter().map(|(n, c, _)| (n.clone(), *c)).collect()
        };
        let ms = |v: &[(String, u64, f64)]| -> Vec<(String, f64)> {
            v.iter().map(|(n, _, m)| (n.clone(), *m)).collect()
        };
        syscalls = merge(&calls(&before.syscalls), &calls(&after.syscalls), |c| {
            *c as f64
        });
        syscall_ms = merge(&ms(&before.syscalls), &ms(&after.syscalls), |m| *m);
        by_change(&mut syscalls);
        by_change(&mut syscall_ms);
    }
    Diff {
        baseline: baseline.to_string(),
        wall: Delta {
            name: "wall (median)".into(),
            before: before.wall_ms,
            after: after.wall_ms,
        },
        phases,
        counters,
        syscalls,
        syscall_ms,
        notes,
    }
}

// ----- output ----------------------------------------------------------------

fn num(v: Option<f64>, decimals: usize) -> String {
    match v {
        None => "-".into(),
        Some(x) if decimals == 0 => format!("{x:.0}"),
        Some(x) => format!("{x:.decimals$}"),
    }
}

fn shown(d: &Delta, decimals: usize, unit: &str) -> String {
    let pct = match d.percent() {
        Some(p) => format!(" ({p:+.0}%)"),
        None if d.before.is_none() => " (new)".into(),
        None if d.after.is_none() => " (gone)".into(),
        None => String::new(),
    };
    format!("{:+.*}{unit}{pct}", decimals, d.change())
}

/// Rows shown in the plain table; the JSON has them all.
const ROWS: usize = 10;

fn table(title: &str, rows: &[Delta], decimals: usize, unit: &str) {
    if rows.is_empty() {
        return;
    }
    outln!("  {title} (most changed first)");
    outln!(
        "    {:<26} {:>12} {:>12}   {}",
        "",
        "before",
        "after",
        "change"
    );
    for d in rows.iter().take(ROWS) {
        outln!(
            "    {:<26} {:>12} {:>12}   {}",
            d.name,
            num(d.before, decimals),
            num(d.after, decimals),
            shown(d, decimals, unit)
        );
    }
    if rows.len() > ROWS {
        outln!("    ... and {} more (see --json)", rows.len() - ROWS);
    }
}

pub(super) fn print_text(name: &str, d: &Result<Diff, String>) {
    outln!("");
    match d {
        Err(e) => outln!("diff: {name}: {e}"),
        Ok(d) => {
            outln!("diff: {name} against baseline `{}`", d.baseline);
            outln!(
                "  {:<26} {:>12} {:>12}   {}",
                d.wall.name,
                num(d.wall.before, 2),
                num(d.wall.after, 2),
                shown(&d.wall, 2, " ms")
            );
            table("phases, ms", &d.phases, 2, " ms");
            table("counters", &d.counters, 0, "");
            table("syscalls, calls", &d.syscalls, 0, "");
            table("syscalls, ms", &d.syscall_ms, 3, " ms");
            for n in &d.notes {
                outln!("  note: {n}");
            }
        }
    }
}

pub(super) fn print_markdown(name: &str, d: &Result<Diff, String>) {
    outln!("");
    match d {
        Err(e) => outln!("diff, `{name}`: {e}"),
        Ok(d) => {
            outln!(
                "**diff, `{name}`** against `{}`: wall {} to {} ms ({})",
                d.baseline,
                num(d.wall.before, 2),
                num(d.wall.after, 2),
                shown(&d.wall, 2, " ms")
            );
            for (title, rows, dec, unit) in [
                ("phase", &d.phases, 2, " ms"),
                ("counter", &d.counters, 0, ""),
                ("syscall calls", &d.syscalls, 0, ""),
                ("syscall ms", &d.syscall_ms, 3, " ms"),
            ] {
                if rows.is_empty() {
                    continue;
                }
                outln!("");
                outln!("| {title} | before | after | change |");
                outln!("|---|---:|---:|---:|");
                for r in rows.iter().take(ROWS) {
                    outln!(
                        "| {} | {} | {} | {} |",
                        r.name,
                        num(r.before, dec),
                        num(r.after, dec),
                        shown(r, dec, unit)
                    );
                }
            }
            for n in &d.notes {
                outln!("");
                outln!("note: {n}");
            }
        }
    }
}

fn deltas_json(v: &[Delta]) -> Vec<serde_json::Value> {
    v.iter()
        .map(|d| {
            serde_json::json!({
                "name": d.name,
                "before": d.before,
                "after": d.after,
                "change": d.change(),
                "percent": d.percent(),
            })
        })
        .collect()
}

pub(super) fn to_json(d: &Result<Diff, String>) -> serde_json::Value {
    match d {
        Err(e) => serde_json::json!({ "compared": false, "note": e }),
        Ok(d) => serde_json::json!({
            "compared": true,
            "baseline": d.baseline,
            "wall": deltas_json(std::slice::from_ref(&d.wall))[0].clone(),
            "phases": deltas_json(&d.phases),
            "counters": deltas_json(&d.counters),
            "syscalls": deltas_json(&d.syscalls),
            "syscall_ms": deltas_json(&d.syscall_ms),
            "notes": d.notes,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(wall: f64, phases: &[(&str, f64)], sys: &[(&str, u64, f64)]) -> Saved {
        Saved {
            wall_ms: Some(wall),
            phases: phases.iter().map(|(n, m)| (n.to_string(), *m)).collect(),
            counters: vec![("allocs".into(), 100.0)],
            syscalls: sys
                .iter()
                .map(|(n, c, m)| (n.to_string(), *c, *m))
                .collect(),
            has_phases: true,
            has_counters: true,
            has_syscalls: true,
        }
    }

    #[test]
    fn what_moved_most_comes_first() {
        let a = saved(
            80.0,
            &[("check", 2.0), ("run", 60.0), ("burn", 10.0)],
            &[("read", 100, 1.0), ("fsync", 10, 5.0)],
        );
        let mut b = saved(
            717.0,
            &[("check", 2.1), ("run", 690.0), ("burn", 12.0)],
            &[("read", 5000, 1.5), ("fsync", 12, 5.2), ("mmap", 40, 0.1)],
        );
        b.counters = vec![("allocs".into(), 300.0), ("frees".into(), 5.0)];
        let d = diff("main", &a, &b);
        let order: Vec<&str> = d.phases.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(order, ["run", "burn", "check"], "{order:?}");
        assert!((d.phases[0].change() - 630.0).abs() < 1e-9);
        assert_eq!(d.syscalls[0].name, "read", "by calls");
        assert_eq!(d.syscall_ms[0].name, "read", "by time");
        // A counter that was not there before comes first; then by share.
        assert_eq!(d.counters[0].name, "frees");
        assert_eq!(d.counters[0].percent(), None);
        assert_eq!(d.counters[1].percent(), Some(200.0));
        assert_eq!(d.wall.change(), 637.0);
        assert!(d.notes.is_empty());
    }

    #[test]
    fn a_baseline_without_numbers_says_how_to_save_them() {
        let a = Saved {
            wall_ms: Some(10.0),
            ..Saved::default()
        };
        let b = saved(12.0, &[("run", 5.0)], &[]);
        let d = diff("main", &a, &b);
        assert!(d.phases.is_empty() && d.syscalls.is_empty());
        assert!(
            d.notes.iter().any(|n| n.contains("--save main --timings")),
            "{:?}",
            d.notes
        );
    }

    #[test]
    fn a_saved_insight_reads_back_the_same() {
        let s = saved(5.0, &[("run", 4.0)], &[("read", 9, 0.25)]);
        let v = serde_json::json!({
            "wall_ms": 5.0,
            "phases": [{"name": "run", "ms": 4.0}],
            "counters": [{"name": "allocs", "value": 100.0}],
            "syscalls": [{"name": "read", "calls": 9, "ms": 0.25}],
        });
        assert_eq!(saved_from_json(&v), s);
        assert_eq!(
            saved_from_json(&serde_json::json!({ "wall_ms": 1.0 })),
            Saved {
                wall_ms: Some(1.0),
                ..Saved::default()
            }
        );
    }
}
