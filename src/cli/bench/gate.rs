// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The gates of `cig bench` (#145): what makes a run fail.
//!
//! | gate | fails on |
//! |---|---|
//! | `correctness` | an output that is not its `expect`, an `after` check that finds bytes not restored, a case that cannot be run |
//! | `regression` | a case more than 5% slower than the baseline (significantly: p < 0.01), or the set's geometric mean more than 2% slower; needs `--against` |
//! | `disk` | bytes on disk per byte burned more than 10% above the case's `reference` (what `main` had) |
//! | `start-up` | a case's median over its `budget` (hello 5 ms, a one-write burn 7 ms) |
//!
//! A case lists the gates it takes part in (`gate = ["disk"]`); every case is
//! under `correctness` and, in a comparison, under `regression`. `cig bench --gate LIST` (or `all`) evaluates all four
//! and fails the exit only for the ones named: the others are printed, so a
//! shared runner's timings are on the record without failing a pull request
//! (#153: timings are reported, never gated, where the machine is shared).
//!
//! `regression` stands in for #145's "(non-noisy)": a case whose spread hides
//! a 5% change cannot reach p < 0.01, so it cannot trip, and one that does
//! trip has shown a change the noise cannot explain. The set's geometric
//! mean counts only cases whose CV is 5% or less on both sides.

use super::ab::{AbResult, AbRow, Verdict};
use super::{Case, Outcome, Row};

/// A case's median this far above the baseline's, and significant, trips.
pub const REGRESSION_CASE: f64 = 0.05;
/// The set's geometric mean this far above, trips.
pub const REGRESSION_SET: f64 = 0.02;
/// Bytes per byte this far above the reference trips.
pub const DISK: f64 = 0.10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    Correctness,
    Regression,
    Disk,
    StartUp,
}

pub const ALL: [Gate; 4] = [
    Gate::Correctness,
    Gate::Regression,
    Gate::Disk,
    Gate::StartUp,
];

impl Gate {
    pub fn name(self) -> &'static str {
        match self {
            Gate::Correctness => "correctness",
            Gate::Regression => "regression",
            Gate::Disk => "disk",
            Gate::StartUp => "start-up",
        }
    }

    fn parse(s: &str) -> Option<Gate> {
        ALL.into_iter().find(|g| g.name() == s)
    }
}

fn names() -> String {
    ALL.iter().map(|g| g.name()).collect::<Vec<_>>().join(", ")
}

/// A case's `gate` value: one gate or a list.
pub fn parse_manifest(v: &toml::Value, where_: &str) -> Result<Vec<Gate>, String> {
    let items: Vec<&toml::Value> = match v {
        toml::Value::String(_) => vec![v],
        toml::Value::Array(a) => a.iter().collect(),
        _ => return Err(format!("{where_}: `gate` is a gate name or a list of them")),
    };
    let mut out = Vec::new();
    for it in items {
        let s = it
            .as_str()
            .ok_or_else(|| format!("{where_}: `gate` holds names, one of {}", names()))?;
        let g = Gate::parse(s)
            .ok_or_else(|| format!("{where_}: `{s}` is not a gate; the gates are {}", names()))?;
        if !out.contains(&g) {
            out.push(g);
        }
    }
    Ok(out)
}

/// `--gate correctness,disk` (or `all`).
pub fn parse_selection(args: &[String]) -> Result<Vec<Gate>, String> {
    let mut out: Vec<Gate> = Vec::new();
    for a in args.iter().flat_map(|s| s.split(',')) {
        let a = a.trim();
        if a.is_empty() {
            continue;
        }
        if a == "all" {
            return Ok(ALL.to_vec());
        }
        let g = Gate::parse(a)
            .ok_or_else(|| format!("`{a}` is not a gate; the gates are {}, or `all`", names()))?;
        if !out.contains(&g) {
            out.push(g);
        }
    }
    if out.is_empty() {
        return Err(format!("`--gate` names gates: {}, or `all`", names()));
    }
    Ok(out)
}

/// A budget such as `5 ms` or `0.2 s`, in milliseconds.
pub fn parse_budget_ms(s: &str) -> Option<f64> {
    let t = s.trim();
    let split = t
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(t.len());
    let (n, unit) = t.split_at(split);
    let n: f64 = n.parse().ok()?;
    match unit.trim() {
        "ms" | "" => Some(n),
        "s" => Some(n * 1000.0),
        _ => None,
    }
    .filter(|v| *v > 0.0 && v.is_finite())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Trip,
    NotMeasured,
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub gate: Gate,
    pub case: Option<String>,
    pub status: Status,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct Report {
    /// The gates the run fails on.
    pub enforced: Vec<Gate>,
    pub findings: Vec<Finding>,
}

impl Report {
    fn new(enforced: &[Gate]) -> Report {
        Report {
            enforced: enforced.to_vec(),
            findings: Vec::new(),
        }
    }

    fn push(&mut self, gate: Gate, case: Option<&str>, status: Status, detail: String) {
        self.findings.push(Finding {
            gate,
            case: case.map(str::to_string),
            status,
            detail,
        });
    }

    pub fn enforces(&self, g: Gate) -> bool {
        self.enforced.contains(&g)
    }

    /// A trip of a gate the run fails on.
    pub fn failed(&self) -> bool {
        self.findings
            .iter()
            .any(|f| f.status == Status::Trip && self.enforces(f.gate))
    }

    pub fn failures(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|f| f.status == Status::Trip && self.enforces(f.gate))
    }
}

fn has(case: &Case, g: Gate) -> bool {
    case.gate.contains(&g)
}

/// Correctness over a plain run: every case timed.
fn correctness(rep: &mut Report, rows: &[(&str, Option<&str>)]) {
    let mut bad = 0;
    for (name, why) in rows {
        if let Some(why) = why {
            bad += 1;
            rep.push(
                Gate::Correctness,
                Some(name),
                Status::Trip,
                (*why).to_string(),
            );
        }
    }
    if bad == 0 {
        rep.push(
            Gate::Correctness,
            None,
            Status::Ok,
            format!(
                "{} cases gave their expected output and left what they should",
                rows.len()
            ),
        );
    }
}

/// The start-up gate for one case.
fn start_up(rep: &mut Report, case: &Case, median_ms: f64) {
    let Some(budget) = case.budget.as_deref().and_then(parse_budget_ms) else {
        rep.push(
            Gate::StartUp,
            Some(&case.name),
            Status::NotMeasured,
            "no budget".into(),
        );
        return;
    };
    let over = median_ms > budget;
    rep.push(
        Gate::StartUp,
        Some(&case.name),
        if over { Status::Trip } else { Status::Ok },
        format!("median {median_ms:.2} ms against a budget of {budget} ms"),
    );
}

/// Gates over a plain run's rows.
pub(super) fn evaluate_run(cases: &[&Case], rows: &[Row], enforced: &[Gate]) -> Report {
    let mut rep = Report::new(enforced);
    let verdicts: Vec<(&str, Option<String>)> = rows
        .iter()
        .map(|r| {
            (
                r.name.as_str(),
                match &r.outcome {
                    Outcome::Timed(_) => None,
                    Outcome::Wrong(w) => Some(format!("WRONG: {w}")),
                    Outcome::Absent(w) => Some(format!("could not be measured: {w}")),
                },
            )
        })
        .collect();
    let view: Vec<(&str, Option<&str>)> =
        verdicts.iter().map(|(n, w)| (*n, w.as_deref())).collect();
    correctness(&mut rep, &view);
    for (case, row) in cases.iter().zip(rows) {
        let Outcome::Timed(stats) = &row.outcome else {
            continue;
        };
        if has(case, Gate::Disk) {
            disk(&mut rep, case, row);
        }
        if has(case, Gate::StartUp) {
            start_up(&mut rep, case, stats.median_ms);
        }
    }
    rep.push(
        Gate::Regression,
        None,
        Status::NotMeasured,
        "needs `--against` (a baseline, release, ref or binary to compare with)".into(),
    );
    rep
}

fn disk(rep: &mut Report, case: &Case, row: &Row) {
    let Some(reference) = case.reference_disk else {
        rep.push(
            Gate::Disk,
            Some(&case.name),
            Status::NotMeasured,
            "no reference".into(),
        );
        return;
    };
    let value = row
        .metrics
        .iter()
        .find(|m| m.name == "disk-per-byte")
        .and_then(|m| m.value);
    let Some(v) = value else {
        let why = row
            .metrics
            .iter()
            .find(|m| m.name == "disk-per-byte")
            .and_then(|m| m.note.clone())
            .unwrap_or_else(|| "not measured".into());
        rep.push(Gate::Disk, Some(&case.name), Status::NotMeasured, why);
        return;
    };
    let limit = reference * (1.0 + DISK);
    rep.push(
        Gate::Disk,
        Some(&case.name),
        if v > limit { Status::Trip } else { Status::Ok },
        format!("{v:.2} bytes per byte burned; main's {reference:.2}, the limit {limit:.2} (+10%)"),
    );
}

/// What the regression gate needs from one compared case.
#[derive(Debug, Clone)]
pub struct Compared {
    pub name: String,
    pub median_a: f64,
    pub median_b: f64,
    pub cv_a: f64,
    pub cv_b: f64,
    pub change: f64,
    pub p: f64,
    pub slower: bool,
}

/// The regression gate over compared cases: each case, then the set.
pub fn regression(rep: &mut Report, compared: &[Compared]) {
    let mut logs: Vec<f64> = Vec::new();
    for c in compared {
        let trip = c.slower && c.change > REGRESSION_CASE;
        rep.push(
            Gate::Regression,
            Some(&c.name),
            if trip { Status::Trip } else { Status::Ok },
            format!(
                "median {:.2} ms against {:.2} ms ({:+.1}%, p {:.3})",
                c.median_b,
                c.median_a,
                100.0 * c.change,
                c.p
            ),
        );
        if c.cv_a <= 0.05 && c.cv_b <= 0.05 && c.median_a > 0.0 && c.median_b > 0.0 {
            logs.push((c.median_b / c.median_a).ln());
        }
    }
    if logs.is_empty() {
        rep.push(
            Gate::Regression,
            None,
            Status::NotMeasured,
            "no case was steady enough (CV 5% or less on both sides) for the geometric mean".into(),
        );
        return;
    }
    let geo = (logs.iter().sum::<f64>() / logs.len() as f64).exp() - 1.0;
    rep.push(
        Gate::Regression,
        None,
        if geo > REGRESSION_SET {
            Status::Trip
        } else {
            Status::Ok
        },
        format!(
            "geometric mean of {} steady cases {:+.1}% (the limit +2%)",
            logs.len(),
            100.0 * geo
        ),
    );
}

/// Gates over an A/B comparison's rows.
pub(super) fn evaluate_ab(cases: &[&Case], rows: &[AbRow], enforced: &[Gate]) -> Report {
    let mut rep = Report::new(enforced);
    let verdicts: Vec<(&str, Option<String>)> = rows
        .iter()
        .map(|r| {
            (
                r.name.as_str(),
                match &r.result {
                    AbResult::Compared { .. } => None,
                    AbResult::Wrong(w) => Some(format!("WRONG: {w}")),
                    AbResult::Absent(w) => Some(format!("could not be measured: {w}")),
                },
            )
        })
        .collect();
    let view: Vec<(&str, Option<&str>)> =
        verdicts.iter().map(|(n, w)| (*n, w.as_deref())).collect();
    correctness(&mut rep, &view);
    let mut compared = Vec::new();
    for r in rows {
        let AbResult::Compared { a, b, cmp, .. } = &r.result else {
            continue;
        };
        let Some(case) = cases.iter().find(|c| c.name == r.name) else {
            continue;
        };
        // Every compared case is under the regression gate (#145: "a
        // benchmark's median"); listing it in a case's `gate` is allowed.
        compared.push(Compared {
            name: r.name.clone(),
            median_a: a.median_ms,
            median_b: b.median_ms,
            cv_a: a.cv,
            cv_b: b.cv,
            change: cmp.change,
            p: cmp.p,
            slower: cmp.verdict == Verdict::Slower,
        });
        if has(case, Gate::StartUp) {
            start_up(&mut rep, case, b.median_ms);
        }
        if has(case, Gate::Disk) {
            rep.push(
                Gate::Disk,
                Some(&r.name),
                Status::NotMeasured,
                "a comparison measures timings only; run without `--against` for the disk gate"
                    .into(),
            );
        }
    }
    regression(&mut rep, &compared);
    rep
}

// ----- output ----------------------------------------------------------------

fn word(rep: &Report, f: &Finding) -> &'static str {
    match (f.status, rep.enforces(f.gate)) {
        (Status::Ok, _) => "ok",
        (Status::Trip, true) => "TRIP",
        (Status::Trip, false) => "over",
        (Status::NotMeasured, _) => "not measured",
    }
}

pub(super) fn print_text(rep: &Report) {
    outln!("");
    outln!(
        "gates: {} fail the run; the rest are reported",
        rep.enforced
            .iter()
            .map(|g| g.name())
            .collect::<Vec<_>>()
            .join(", ")
    );
    for f in &rep.findings {
        outln!(
            "  {:<12} {:<13} {}{}",
            f.gate.name(),
            word(rep, f),
            f.case
                .as_deref()
                .map(|c| format!("{c}: "))
                .unwrap_or_default(),
            f.detail
        );
    }
}

pub(super) fn print_markdown(rep: &Report) {
    outln!("");
    outln!(
        "**gates**: {} fail the run; the rest are reported",
        rep.enforced
            .iter()
            .map(|g| g.name())
            .collect::<Vec<_>>()
            .join(", ")
    );
    outln!("");
    outln!("| gate | result | case | detail |");
    outln!("|---|---|---|---|");
    for f in &rep.findings {
        outln!(
            "| {} | {} | {} | {} |",
            f.gate.name(),
            word(rep, f),
            f.case.as_deref().unwrap_or(""),
            f.detail
        );
    }
}

pub(super) fn to_json(rep: &Report) -> serde_json::Value {
    serde_json::json!({
        "enforced": rep.enforced.iter().map(|g| g.name()).collect::<Vec<_>>(),
        "tripped": rep.failed(),
        "findings": rep.findings.iter().map(|f| serde_json::json!({
            "gate": f.gate.name(),
            "case": f.case,
            "status": match f.status {
                Status::Ok => "ok",
                Status::Trip => "trip",
                Status::NotMeasured => "not-measured",
            },
            "enforced": rep.enforces(f.gate),
            "detail": f.detail,
        })).collect::<Vec<_>>(),
    })
}

/// Say on stderr what failed the run.
pub(super) fn report_failures(rep: &Report) {
    for f in rep.failures() {
        eprintln!(
            "bench: gate {} tripped{}: {}",
            f.gate.name(),
            f.case
                .as_deref()
                .map(|c| format!(" by {c}"))
                .unwrap_or_default(),
            f.detail
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(name: &str, a: f64, b: f64, cv: f64, slower: bool) -> Compared {
        Compared {
            name: name.into(),
            median_a: a,
            median_b: b,
            cv_a: cv,
            cv_b: cv,
            change: b / a - 1.0,
            p: if slower { 0.002 } else { 0.4 },
            slower,
        }
    }

    #[test]
    fn the_regression_gate_trips_on_a_planted_slowdown_and_not_on_noise() {
        let mut rep = Report::new(&[Gate::Regression]);
        regression(
            &mut rep,
            &[
                c("planted", 10.0, 13.0, 0.01, true),
                c("steady", 10.0, 10.0, 0.01, false),
            ],
        );
        let trips: Vec<_> = rep
            .findings
            .iter()
            .filter(|f| f.status == Status::Trip)
            .collect();
        assert_eq!(trips[0].case.as_deref(), Some("planted"));
        assert!(rep.failed());

        // Six percent slower but not significant: noise, not a trip.
        let mut quiet = Report::new(&[Gate::Regression]);
        regression(&mut quiet, &[c("noisy", 10.0, 10.6, 0.2, false)]);
        assert!(!quiet.failed(), "{:?}", quiet.findings);

        // Significant but only 3%: under the 5% case threshold.
        let mut small = Report::new(&[Gate::Regression]);
        regression(&mut small, &[c("small", 100.0, 103.0, 0.001, true)]);
        assert!(
            small
                .findings
                .iter()
                .all(|f| f.case.is_none() || f.status == Status::Ok),
            "{:?}",
            small.findings
        );
    }

    #[test]
    fn the_set_trips_on_a_geometric_mean_over_two_percent() {
        // Each case 3% slower: no case trips, the set does.
        let many: Vec<Compared> = (0..5)
            .map(|i| c(&format!("c{i}"), 100.0, 103.0, 0.001, true))
            .collect();
        let mut rep = Report::new(&[Gate::Regression]);
        regression(&mut rep, &many);
        let set = rep.findings.iter().find(|f| f.case.is_none()).unwrap();
        assert_eq!(set.status, Status::Trip, "{}", set.detail);
        // Noisy cases do not count toward the mean.
        let mut rep = Report::new(&[Gate::Regression]);
        regression(&mut rep, &[c("noisy", 100.0, 150.0, 0.3, false)]);
        let set = rep.findings.iter().find(|f| f.case.is_none()).unwrap();
        assert_eq!(set.status, Status::NotMeasured);
    }

    #[test]
    fn only_the_named_gates_fail_the_run() {
        let mut rep = Report::new(&[Gate::Correctness, Gate::Disk]);
        rep.push(Gate::StartUp, Some("hello"), Status::Trip, "slow".into());
        assert!(!rep.failed(), "a timing gate is reported");
        rep.push(Gate::Disk, Some("burn"), Status::Trip, "big".into());
        assert!(rep.failed());
        assert_eq!(rep.failures().count(), 1);
    }

    #[test]
    fn gate_names_and_selections_parse() {
        assert_eq!(
            parse_selection(&["correctness,disk".into()]).unwrap(),
            vec![Gate::Correctness, Gate::Disk]
        );
        assert_eq!(parse_selection(&["all".into()]).unwrap().len(), 4);
        let e = parse_selection(&["speed".into()]).unwrap_err();
        assert!(e.contains("start-up"), "{e}");
        let v: toml::Value = toml::Value::String("disk".into());
        assert_eq!(parse_manifest(&v, "x").unwrap(), vec![Gate::Disk]);
        let v: toml::Value = toml::Value::Integer(3);
        assert!(parse_manifest(&v, "x").is_err());
    }

    #[test]
    fn budgets_are_milliseconds() {
        assert_eq!(parse_budget_ms("5 ms"), Some(5.0));
        assert_eq!(parse_budget_ms("7ms"), Some(7.0));
        assert_eq!(parse_budget_ms("0.5 s"), Some(500.0));
        assert_eq!(parse_budget_ms("fast"), None);
        assert_eq!(parse_budget_ms("0 ms"), None);
    }
}
