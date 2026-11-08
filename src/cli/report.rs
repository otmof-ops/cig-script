// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! `cig report <run>`: one redacted file with everything a bug report needs.
//!
//! The run record, the journal (what burned, in order, what it looked like
//! before), the unlit intents, the failing diagnostic, doctor's diagnosis
//! run again now, and the install facts doctor prints. Anonymised the way
//! a crash report is: every path is a placeholder (`root0/d1/f2.txt`, the
//! same mapping the repro uses), the command line is the words cig knows
//! and the shape of the rest, secret-shaped values are removed, and file
//! contents are never included. This is what makes the "break it"
//! invitation true for the bug that does not crash: the confident wrong
//! answer is as easy to send as a panic.

use super::{exit, Ctx};
use crate::burn::journal::{Before, Journal};
use crate::burn::runs;
use crate::burn::Op;
use crate::crash::{redact_arg, tilde};
use crate::diagnostics::Diagnostic;
use crate::repro::Mapping;
use crate::{doctor, update};
use std::path::{Path, PathBuf};

pub fn reports_dir() -> PathBuf {
    runs::home_dir().join("reports")
}

pub fn report(ctx: &Ctx, id: &str, out: Option<PathBuf>, send: bool) -> i32 {
    let rec = match runs::find(id) {
        Ok(Some(r)) => r,
        Ok(None) => {
            eprintln!("{} no run matches `{id}`", ctx.red("error[E803 usage]:"));
            eprintln!("  = hint: `cig runs` lists them; give more of the id if it is ambiguous");
            eprintln!("  = explain: cig explain E803");
            return exit::USAGE;
        }
        Err(e) => {
            eprintln!("{} {e}", ctx.red("error[E803 usage]:"));
            return exit::USAGE;
        }
    };
    let (bundle, mapping) = bundle(&rec);
    if send {
        return send_bundle(ctx, &rec, bundle, mapping);
    }
    let text = serde_json::to_string_pretty(&bundle).unwrap_or_default();
    let to_stdout = ctx.json || out.as_deref() == Some(Path::new("-"));
    if to_stdout {
        outln!("{text}");
        return exit::OK;
    }
    let path = out.unwrap_or_else(|| reports_dir().join(format!("{}.json", rec.id)));
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!(
                "{} cannot create {}: {e}",
                ctx.red("error[E802 usage]:"),
                parent.display()
            );
            return exit::USAGE;
        }
    }
    if let Err(e) = std::fs::write(&path, format!("{text}\n")) {
        eprintln!(
            "{} cannot write {}: {e}",
            ctx.red("error[E802 usage]:"),
            path.display()
        );
        return exit::USAGE;
    }
    outln!("{}", tilde(&path));
    eprintln!(
        "{} run {} bundled: {} journal entr{}, the diagnostic, doctor's diagnosis and the install facts, every path a placeholder; read it, then attach it to an issue:",
        ctx.green("report:"),
        rec.id,
        bundle["journal"].as_array().map(|a| a.len()).unwrap_or(0),
        if bundle["journal"].as_array().map(|a| a.len()).unwrap_or(0) == 1 { "y" } else { "ies" }
    );
    eprintln!(
        "  https://github.com/{}/issues/new?template=cheat.yml",
        crate::config::Config::load().get("issues_repo")
    );
    exit::OK
}

/// `--send`: the bundle joins the ashtray as a report of kind `report` and
/// goes to the tracker the way a crash does, under the same consent; one
/// that cannot go now waits and goes with a later run.
fn send_bundle(
    ctx: &Ctx,
    rec: &runs::RunRecord,
    bundle: serde_json::Value,
    mut mapping: Mapping,
) -> i32 {
    use crate::crash::{Consent, CrashReport, Kind};
    let cfg = crate::config::Config::load();
    if crate::crash::consent(&cfg) == Consent::Never {
        eprintln!(
            "{} reports is \"never\"{}, so nothing leaves this machine; `cig report {}` writes the bundle to a file instead",
            ctx.red("error[E800 usage]:"),
            if crate::crash::under_ci() && !cfg.is_set("reports") && !cfg.is_set("crash_reports") {
                " under CI unless `cig config reports always` says otherwise"
            } else {
                ""
            },
            rec.id
        );
        eprintln!("  = explain: cig explain E800");
        return exit::USAGE;
    }
    let what = rec
        .error
        .as_deref()
        .map(|e| mapping.anonymise_text(e))
        .unwrap_or_else(|| format!("run {} finished {}", rec.id, rec.status));
    let code = rec
        .diagnostic
        .as_ref()
        .and_then(|d| d.get("code"))
        .and_then(|c| c.as_str())
        .unwrap_or("none");
    let mut report = CrashReport::new(
        Kind::Report,
        &format!("run bundle, {code}"),
        &what,
        Some(bundle),
        Some(rec.id.clone()),
    );
    report.cwd = mapping.map(Path::new(&rec.cwd));
    report.queue.requested = true;
    if let Err(e) = crate::ashtray::queue(&mut report) {
        eprintln!(
            "{} could not save the report: {e}",
            ctx.red("error[E802 usage]:")
        );
        return exit::USAGE;
    }
    let repo = crate::ashtray::repo_for(&cfg, &report);
    match crate::ashtray::try_send(&mut report, &cfg) {
        Ok(crate::crash::SendOutcome::Filed(url)) => {
            eprintln!("{} run {} filed at {url}", ctx.green("sent:"), rec.id);
            exit::OK
        }
        Ok(crate::crash::SendOutcome::Commented(url)) => {
            eprintln!(
                "{} this failure is already known; its count was updated on {url}",
                ctx.green("sent:")
            );
            exit::OK
        }
        Ok(crate::crash::SendOutcome::Manual(url)) => {
            eprintln!(
                "{} {}, so nothing was sent; the bundle waits in the ashtray as {} and goes with a later run once a tool is there.",
                ctx.yellow("note:"),
                crate::tracker::why_none(),
                report.id
            );
            eprintln!("open this link to file it yourself now (the report is prefilled):");
            eprintln!("  {url}");
            exit::OK
        }
        Err(e) => {
            eprintln!("{} could not file on {repo} now: {e}", ctx.yellow("note:"));
            eprintln!(
                "the bundle waits in the ashtray as {} and goes with a later run; cig crash flush tries again now",
                report.id
            );
            exit::OK
        }
    }
}

/// An op as a bundle says it: a hop is the program's name and the count of
/// its arguments (the command line a hop ran is never copied), with its
/// working directory and globs mapped; any other op is its description
/// with every path a placeholder.
fn op_shape(op: &Op, m: &mut Mapping) -> String {
    match op {
        Op::Proc {
            cmd,
            args,
            cwd,
            touches,
            touches_sealed,
            boxed,
            // The box's hosts and globs could name someone: a report says
            // only that the hop was boxed.
            box_terms: _,
        } => {
            let mut s = format!(
                "run {} ({} argument{})",
                crate::crash::program_name(cmd),
                args.len(),
                if args.len() == 1 { "" } else { "s" }
            );
            if let Some(c) = cwd {
                s.push_str(&format!("  (in {})", m.map(c)));
            }
            if *boxed {
                s.push_str("  (boxed)");
            }
            if !touches.is_empty() {
                let globs: Vec<String> = touches.iter().map(|g| m.map_glob(g)).collect();
                s.push_str(&format!("  (touches {})", globs.join(", ")));
            }
            if !touches_sealed.is_empty() {
                let globs: Vec<String> = touches_sealed.iter().map(|g| m.map_glob(g)).collect();
                s.push_str(&format!("  (touches_sealed {})", globs.join(", ")));
            }
            s
        }
        other => m.anonymise_text(&other.describe()),
    }
}

/// The state directory as the install facts name it: the default, or
/// `<custom>` (`CIGSCRIPT_HOME` points elsewhere; where is nobody's business).
fn state_dir_shape() -> String {
    let default = crate::crash::home_path().map(|h| h.join(".cigscript"));
    if default.as_deref() == Some(runs::home_dir().as_path()) {
        "~/.cigscript".to_string()
    } else {
        "<custom>".to_string()
    }
}

/// The bundle, every path in it a placeholder of the run's mapping, which
/// comes back with it so the report that carries the bundle uses the same.
fn bundle(rec: &runs::RunRecord) -> (serde_json::Value, Mapping) {
    let loaded = Journal::load(&rec.dir()).ok();
    let mut m = match &loaded {
        Some(j) => Mapping::for_run(rec, j),
        None => Mapping::for_record(rec),
    };
    let journal: Vec<serde_json::Value> = match &loaded {
        Some(j) => j
            .entries()
            .iter()
            .map(|e| {
                let before: Vec<serde_json::Value> = e
                    .before
                    .iter()
                    .map(|b| {
                        let state = match &b.before {
                            Before::Absent => "absent".to_string(),
                            Before::File {
                                bytes, sha256, git, ..
                            } => format!(
                                "file, {bytes} bytes, sha256 {}{}",
                                &sha256[..sha256.len().min(12)],
                                if git.is_some() { " (in git)" } else { "" }
                            ),
                            Before::Dir { .. } => "directory".to_string(),
                            Before::Directory { .. } => "directory (made again)".to_string(),
                            Before::Symlink { target } => {
                                format!("symlink -> {}", m.map_link_target(&b.path, target))
                            }
                            Before::Moved { to } => format!("moved to {}", m.map(to)),
                        };
                        serde_json::json!({"path": m.map(&b.path), "state": state})
                    })
                    .collect();
                serde_json::json!({
                    "seq": e.seq,
                    "at": e.at,
                    "reversible": e.reversible,
                    "op": op_shape(&e.op, &mut m),
                    "before": before,
                })
            })
            .collect(),
        None => Vec::new(),
    };
    let intents: Vec<serde_json::Value> = std::fs::read_to_string(rec.dir().join("intents.jsonl"))
        .map(|t| {
            t.lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .map(|mut v| {
                    // The summary is the op's description with the hop's
                    // command line in it: said as the op's shape instead.
                    if let Ok(op) = serde_json::from_value::<Op>(v["op"].clone()) {
                        v["summary"] = serde_json::Value::String(op_shape(&op, &mut m));
                    }
                    m.anonymise_value(&mut v);
                    v
                })
                .collect()
        })
        .unwrap_or_default();
    let mut diagnostic = rec.diagnostic.clone();
    if let Some(v) = diagnostic.as_mut() {
        m.anonymise_value(v);
    }
    let diagnosis = rec
        .diagnostic
        .as_ref()
        .and_then(Diagnostic::from_json)
        .and_then(|d| {
            doctor::diagnose(
                &d,
                &doctor::Context {
                    cwd: PathBuf::from(&rec.cwd),
                    source: None,
                },
            )
        })
        .map(|dx| {
            let mut v = serde_json::to_value(&dx).unwrap_or_default();
            m.anonymise_value(&mut v);
            v
        });
    let crashes = crate::crash::list().unwrap_or_default();
    let error = rec.error.as_deref().map(|e| m.anonymise_text(e));
    let bundle = serde_json::json!({
        "kind": "cigscript-report",
        "cig": crate::VERSION,
        "generated": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "platform": {"os": std::env::consts::OS, "arch": std::env::consts::ARCH},
        "run": {
            "id": rec.id,
            "version": rec.version,
            "script": m.map(Path::new(&rec.script)),
            "script_sha256": rec.script_sha256,
            "args": rec.args.iter().map(|a| redact_arg(a)).collect::<Vec<_>>(),
            "cwd": m.map(Path::new(&rec.cwd)),
            "mode": rec.mode,
            "started": rec.started,
            "finished": rec.finished,
            "status": rec.status,
            "error": error,
            "burns": rec.burns,
            "irreversible": rec.irreversible,
            "rolled_back": rec.rolled_back,
        },
        "diagnostic": diagnostic,
        "diagnosis": diagnosis,
        "journal": journal,
        "intents": intents,
        "install": {
            "state_dir": state_dir_shape(),
            "tools": {"gh": update::has_tool("gh"), "curl": update::has_tool("curl")},
            "crash_reports_unsent": crashes.iter().filter(|c| c.sent.is_none()).count(),
        },
        "note": "Anonymised like a crash report: every path is a placeholder (root0/d1/f2.txt; out/ for one outside the pack), the command line keeps only the words cig knows, secret-shaped values are removed, file contents are never included. Read it before you send it.",
    });
    (bundle, m)
}
