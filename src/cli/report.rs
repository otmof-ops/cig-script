// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

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

/// `cig report [run] [--out FILE|-] [--send]`: the short way into
/// `ciginstaller bug` (#29). `cig report` is `ciginstaller bug cig`;
/// `cig report <run>` is `ciginstaller bug --run <run> --part cig`, with
/// `--send` forwarded; `--out FILE` is `ciginstaller report <run> --cig
/// --out FILE`, the bundle alone. Two forms stay native because an
/// installer older than this one calls them for the bundle, and a stub
/// there would hand the call straight back: `cig --json report <run>` and
/// `--out -`, both the bundle on stdout.
pub fn report(ctx: &Ctx, id: Option<&str>, out: Option<PathBuf>, send: bool) -> i32 {
    let Some(id) = id else {
        if out.is_some() {
            eprintln!(
                "{} `--out` needs a run: `cig report <run> --out FILE`; `cig runs` lists them",
                ctx.red("error[E800 usage]:")
            );
            return exit::USAGE;
        }
        let mut args = vec!["bug", "cig"];
        if send {
            args.push("--send");
        }
        return crate::installer::run("cig report", &args);
    };
    let to_stdout = ctx.json || out.as_deref() == Some(Path::new("-"));
    if to_stdout && !send {
        return print_bundle(ctx, id);
    }
    if let Some(file) = out {
        let file = file.to_string_lossy().to_string();
        return crate::installer::run(
            "cig report",
            &["report", id, "--cig", "--out", file.as_str()],
        );
    }
    let mut args = vec!["bug", "--run", id, "--part", "cig"];
    if send {
        args.push("--send");
    }
    crate::installer::run("cig report", &args)
}

fn find_run(ctx: &Ctx, id: &str) -> Result<runs::RunRecord, i32> {
    match runs::find(id) {
        Ok(Some(r)) => Ok(r),
        Ok(None) => {
            eprintln!("{} no run matches `{id}`", ctx.red("error[E803 usage]:"));
            eprintln!("  = hint: `cig runs` lists them; give more of the id if it is ambiguous");
            eprintln!("  = explain: cig explain E803");
            Err(exit::USAGE)
        }
        Err(e) => {
            eprintln!("{} {e}", ctx.red("error[E803 usage]:"));
            Err(exit::USAGE)
        }
    }
}

/// The bundle on stdout, with the `repro` object. Writes nothing, sends
/// nothing; an unknown run is exit 3.
fn print_bundle(ctx: &Ctx, id: &str) -> i32 {
    let rec = match find_run(ctx, id) {
        Ok(r) => r,
        Err(code) => return code,
    };
    let (mut bundle, _) = bundle(&rec);
    bundle["repro"] = repro_of_run(&rec);
    outln!(
        "{}",
        serde_json::to_string_pretty(&bundle).unwrap_or_default()
    );
    exit::OK
}

/// `cig report-bundle <run> [--out FILE|--queue]` (hidden). With neither, the
/// bundle with its `repro` on stdout (`--json` is accepted and the same). `--out` writes the
/// bundle to a file and `--queue` files it through the ashtray, as
/// `cig report` did before the installer took the front of it; both are
/// what the local queue needs when there is no installer.
pub fn bundle_cmd(ctx: &Ctx, id: &str, out: Option<PathBuf>, queue: bool) -> i32 {
    if out.is_none() && !queue {
        return print_bundle(ctx, id);
    }
    if ctx.json {
        eprintln!(
            "{} `--json` prints the bundle and nothing else; it does not combine with `--out` or `--queue`",
            ctx.red("error[E800 usage]:")
        );
        return exit::USAGE;
    }
    let rec = match find_run(ctx, id) {
        Ok(r) => r,
        Err(code) => return code,
    };
    let (bundle, mapping) = bundle(&rec);
    if queue {
        return send_bundle(ctx, &rec, bundle, mapping);
    }
    let text = serde_json::to_string_pretty(&bundle).unwrap_or_default();
    let out = out.unwrap_or_else(|| reports_dir().join(format!("{}.json", rec.id)));
    if out == Path::new("-") {
        outln!("{text}");
        return exit::OK;
    }
    let path = out;
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

/// The `repro` object a bundle carries. The repro is the one the run's own
/// ashtray report built (a crash, an internal error, a tripwire), or one
/// built from the journal of a run that ended in an internal error; either
/// is run again now in a scratch sandbox, and only a repro that reproduces
/// here is `verified: true`.
fn repro_of_run(rec: &runs::RunRecord) -> serde_json::Value {
    let scratch = crate::crash::ashtray_dir().join(format!("repro-bundle-{}", rec.id));
    repro_object(repro_to_run_again(rec), |x| {
        crate::repro::verify_and_shrink(x, &scratch);
        let _ = std::fs::remove_dir_all(&scratch);
    })
}

/// The repro to run again for a run, or why there is none.
fn repro_to_run_again(rec: &runs::RunRecord) -> Result<crate::repro::Repro, String> {
    use crate::crash::Kind;
    let stored = crate::crash::list()
        .unwrap_or_default()
        .into_iter()
        .find_map(|c| {
            (c.active_run_id.as_deref() == Some(rec.id.as_str()) && c.kind != Kind::Report)
                .then_some(c.repro)
                .flatten()
        });
    if let Some(x) = stored {
        return Ok(x);
    }
    let internal = rec
        .diagnostic
        .as_ref()
        .and_then(|d| d.get("code"))
        .and_then(|c| c.as_str())
        == Some("E900");
    if !internal {
        return Err("the run ended in nothing cig can look for again: only a crash, an internal error or a tripwire gives a repro something to check".to_string());
    }
    let journal =
        Journal::load(&rec.dir()).map_err(|_| "the run's journal cannot be read".to_string())?;
    crate::repro::build(rec, &journal, "internal", "an internal error (E900)")
        .map(|(x, _)| x)
        .ok_or_else(|| "the run's journal has nothing to build a repro from".to_string())
}

/// The object itself: `verify` runs the repro again and sets `verified`.
/// `Err(why)` is a run that has nothing to run again.
fn repro_object(
    found: Result<crate::repro::Repro, String>,
    verify: impl FnOnce(&mut crate::repro::Repro),
) -> serde_json::Value {
    match found {
        Err(why) => serde_json::json!({"script": null, "verified": false, "why": why}),
        Ok(mut x) => {
            x.verified = false;
            x.note.clear();
            verify(&mut x);
            let why = if x.verified {
                "it reproduced in a sandbox on this machine when this bundle was made".to_string()
            } else if x.note.is_empty() {
                "it was not verified on this machine".to_string()
            } else {
                x.note.clone()
            };
            let mut v = serde_json::to_value(&x).unwrap_or_default();
            v["why"] = serde_json::Value::String(why);
            v
        }
    }
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
            skip,
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
            if !skip.is_empty() {
                let globs: Vec<String> = skip.iter().map(|g| m.map_glob(g)).collect();
                s.push_str(&format!("  (skip {})", globs.join(", ")));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repro::Repro;

    fn repro() -> Repro {
        Repro {
            script: "fs.write_text(\"root0/a\", \"x\")\n".into(),
            seeds: Vec::new(),
            stubs: Vec::new(),
            ops: 1,
            verified: true,
            expect: "an internal error (E900)".into(),
            check: "internal".into(),
            note: "stale".into(),
        }
    }

    #[test]
    fn a_repro_is_verified_only_when_it_reproduces_when_the_bundle_is_made() {
        // a stored `verified: true` proves nothing now: it is run again, and
        // what the run again says is what the bundle says
        let yes = repro_object(Ok(repro()), |x| x.verified = true);
        assert_eq!(yes["verified"], true, "{yes}");
        assert!(yes["script"].as_str().unwrap().contains("fs.write_text"));
        assert!(yes["why"].as_str().unwrap().contains("reproduced"), "{yes}");
        let no = repro_object(Ok(repro()), |x| {
            x.verified = false;
            x.note = "It did not reproduce in the sandbox, so it is not attached.".into();
        });
        assert_eq!(no["verified"], false, "{no}");
        assert!(no["why"].as_str().unwrap().contains("did not reproduce"));
        let none = repro_object(Err("no journal".into()), |_| panic!("nothing to run"));
        assert_eq!(none["verified"], false);
        assert!(none["script"].is_null());
        assert_eq!(none["why"], "no journal");
    }
}
