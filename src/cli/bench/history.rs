// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! `cig bench history <case>` (#179 step 6): one case's history, read from
//! what the nightly workflow (`.github/workflows/bench.yml`) appended to the
//! `bench-history` branch, or from a file.
//!
//! The file is `history/<product>.jsonl` (or `history/<product>.<series>.jsonl`
//! for one filesystem of the nightly matrix: `--series btrfs`), one JSON
//! object per nightly run:
//!
//! ```text
//! {"format":1,"at":"2026-11-11T02:00:00Z","commit":"1a2b3c...","product":"cig",
//!  "cig_version":"1.20.0","machine":"CPU, 4 cores, linux 6.8",
//!  "cases":{"hello":{"group":"start-up","median_ms":3.4,"p10_ms":3.3,
//!                    "p90_ms":3.6,"cv":0.02,"rss_bytes":7340032}}}
//! ```
//!
//! Only timed cases are in a line. A line in another `format` is skipped and
//! counted, never guessed at. A change across two different machines is
//! marked `*`: a hosted runner is not the same CPU every night.

use super::{parse_manifest, watch::spark};
use crate::cli::{exit, Ctx};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Command;

struct Run {
    at: String,
    commit: String,
    machine: String,
    median: f64,
    p10: f64,
    p90: f64,
    cv: f64,
}

pub(super) fn history(
    ctx: &Ctx,
    case: Option<&str>,
    manifest: Option<PathBuf>,
    file: Option<PathBuf>,
    branch: Option<String>,
    series: Option<String>,
) -> i32 {
    let usage = |e: String| {
        eprintln!("{} {e}", ctx.red("error[E800 usage]:"));
        exit::USAGE
    };
    let text = match &file {
        Some(f) => match std::fs::read_to_string(f) {
            Ok(t) => t,
            Err(e) => return usage(format!("cannot read {}: {e}", f.display())),
        },
        None => {
            let path = manifest.unwrap_or_else(|| PathBuf::from("bench").join("bench.toml"));
            let product = match std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))
                .and_then(|t| parse_manifest(&t).map_err(|e| format!("{}: {e}", path.display())))
            {
                Ok(m) => m.bench.product,
                Err(e) => return usage(format!("{e} (or name the history with `--file`)")),
            };
            let branch = branch.unwrap_or_else(|| "bench-history".into());
            match read_branch(&branch, &product, series.as_deref()) {
                Ok(t) => t,
                Err(e) => return usage(e),
            }
        }
    };
    let (runs_all, skipped, product) = parse_lines(&text);
    if runs_all.is_empty() {
        return usage(format!(
            "no run in format 1 in the history ({skipped} line(s) skipped)"
        ));
    }
    // Every case name that has a time in some run.
    let mut names: Vec<String> = Vec::new();
    for (_, v) in &runs_all {
        for k in v["cases"].as_object().into_iter().flatten().map(|(k, _)| k) {
            if !names.contains(k) {
                names.push(k.clone());
            }
        }
    }
    let Some(case) = case else {
        let list: Vec<String> = names
            .iter()
            .map(|n| {
                let c = runs_all
                    .iter()
                    .filter(|(_, v)| v["cases"].get(n).is_some())
                    .count();
                format!("{n} ({c} runs)")
            })
            .collect();
        outln!(
            "cig bench history: {product}, {} runs; cases:",
            runs_all.len()
        );
        for l in list {
            outln!("  {l}");
        }
        return exit::OK;
    };
    let chosen = if names.iter().any(|n| n == case) {
        case.to_string()
    } else {
        let hits: Vec<&String> = names.iter().filter(|n| n.contains(case)).collect();
        match hits.as_slice() {
            [one] => (*one).clone(),
            [] => {
                return usage(format!(
                    "no case named like `{case}` in the history; it has: {}",
                    names.join(", ")
                ))
            }
            many => {
                return usage(format!(
                    "`{case}` matches {}; name one",
                    many.iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            }
        }
    };
    let runs: Vec<Run> = runs_all
        .iter()
        .filter_map(|(r, v)| {
            let c = v["cases"].get(&chosen)?;
            Some(Run {
                at: r.at.clone(),
                commit: r.commit.clone(),
                machine: r.machine.clone(),
                median: c["median_ms"].as_f64()?,
                p10: c["p10_ms"].as_f64().unwrap_or(f64::NAN),
                p90: c["p90_ms"].as_f64().unwrap_or(f64::NAN),
                cv: c["cv"].as_f64().unwrap_or(f64::NAN),
            })
        })
        .collect();
    if ctx.json {
        let doc = serde_json::json!({
            "format": super::FORMAT,
            "product": product,
            "case": chosen,
            "skipped_lines": skipped,
            "runs": runs.iter().map(|r| serde_json::json!({
                "at": r.at, "commit": r.commit, "machine": r.machine,
                "median_ms": r.median, "p10_ms": r.p10, "p90_ms": r.p90, "cv": r.cv,
            })).collect::<Vec<_>>(),
        });
        outln!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
        return exit::OK;
    }
    let recent: VecDeque<f64> = runs.iter().rev().take(40).rev().map(|r| r.median).collect();
    outln!(
        "cig bench history: {product}, {chosen}, {} runs",
        runs.len()
    );
    outln!(
        "{}  {:.2} ms to {:.2} ms",
        spark(&recent),
        recent.iter().copied().fold(f64::INFINITY, f64::min),
        recent.iter().copied().fold(0.0, f64::max)
    );
    outln!("");
    outln!(
        "{:<20} {:<9} {:>10} {:>16} {:>7} {:>9}",
        "at",
        "commit",
        "median ms",
        "p10 to p90 ms",
        "CV",
        "change"
    );
    let first = runs.len().saturating_sub(20);
    let mut changed_machine = false;
    for (i, r) in runs.iter().enumerate().skip(first) {
        let change = if i == 0 {
            "-".to_string()
        } else {
            let prev = &runs[i - 1];
            let pct = 100.0 * (r.median - prev.median) / prev.median;
            let star = if prev.machine != r.machine {
                changed_machine = true;
                "*"
            } else {
                ""
            };
            format!("{pct:+.1}%{star}")
        };
        outln!(
            "{:<20} {:<9} {:>10.2} {:>16} {:>6.1}% {:>9}",
            r.at.chars().take(19).collect::<String>(),
            r.commit.chars().take(8).collect::<String>(),
            r.median,
            format!("{:.2} to {:.2}", r.p10, r.p90),
            r.cv * 100.0,
            change
        );
    }
    if changed_machine {
        outln!("");
        outln!("* the machine differs from the run before: a change across machines is not a regression.");
    }
    if skipped > 0 {
        outln!("");
        outln!("{skipped} line(s) in another format were skipped.");
    }
    exit::OK
}

/// `history/<product>.jsonl` on a branch, local or `origin/`.
fn read_branch(branch: &str, product: &str, series: Option<&str>) -> Result<String, String> {
    let git = |spec: String| -> Option<String> {
        let out = Command::new("git").args(["show", &spec]).output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    // The nightly filesystem matrix writes one file per series:
    // `history/cig.btrfs.jsonl`.
    let rel = match series {
        Some(s) if !s.is_empty() => format!("history/{product}.{s}.jsonl"),
        _ => format!("history/{product}.jsonl"),
    };
    git(format!("{branch}:{rel}"))
        .or_else(|| git(format!("origin/{branch}:{rel}")))
        .ok_or_else(|| {
            format!(
                "no {rel} on the `{branch}` branch (here or on origin); the nightly workflow writes it, or read a file with `--file`"
            )
        })
}

struct Head {
    at: String,
    commit: String,
    machine: String,
}

fn parse_lines(text: &str) -> (Vec<(Head, serde_json::Value)>, usize, String) {
    let mut out = Vec::new();
    let mut skipped = 0;
    let mut product = String::from("?");
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        match serde_json::from_str::<serde_json::Value>(line) {
            Ok(v) if v["format"].as_i64() == Some(super::FORMAT) && v["cases"].is_object() => {
                if let Some(p) = v["product"].as_str() {
                    product = p.to_string();
                }
                out.push((
                    Head {
                        at: v["at"].as_str().unwrap_or("").to_string(),
                        commit: v["commit"].as_str().unwrap_or("").to_string(),
                        machine: v["machine"].as_str().unwrap_or("").to_string(),
                    },
                    v,
                ));
            }
            _ => skipped += 1,
        }
    }
    (out, skipped, product)
}
