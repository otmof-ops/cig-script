// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! `cig crash`: the ashtray (every report cig made about its own bugs: a
//! crash, an internal error, a tripwire, a run bundle), and the prompt that
//! follows a crash. Nothing leaves without a yes: the one yes that sets
//! `reports = always`, or a `send` typed for one report.

use super::{exit, Ctx};
use crate::ashtray;
use crate::burn::journal::Journal;
use crate::burn::runs;
use crate::config::Config;
use crate::crash::{self, Consent, CrashReport, Kind, SendOutcome};
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;

#[derive(clap::Subcommand)]
pub enum CrashCommand {
    /// List the ashtray: every report, its kind and whether it was filed.
    List,
    /// Print a report exactly as it would be sent.
    Show {
        /// Report id or unique prefix.
        id: String,
    },
    /// File a report on the issue tracker now (asks nothing further); one that cannot go now waits in the ashtray and goes with the next run.
    Send {
        /// Report id or unique prefix.
        id: String,
    },
    /// Send every report that is waiting and due; `cig run` starts this in the background.
    Flush {
        /// Say nothing; the reports carry the outcome.
        #[arg(long)]
        quiet: bool,
    },
    /// Hand in a report another tool of the family made (cig-orchestrator): it joins the ashtray under cig's consent and routes.
    Submit {
        /// The tool that made it, e.g. cig-orchestrator.
        #[arg(long)]
        origin: String,
        /// The report, as JSON (the shape `cig crash show` prints; extra fields travel in `details`).
        #[arg(long)]
        file: PathBuf,
        /// Where it goes (owner/name); cig's issues_repo when omitted.
        #[arg(long)]
        repo: Option<String>,
        /// Send it now, in this process, instead of leaving it to the ashtray.
        #[arg(long)]
        now: bool,
    },
    /// Say which reports this machine filed were fixed in a release (run by `ciginstaller update` after it changed cig).
    #[command(hide = true, name = "announce-fixed")]
    AnnounceFixed,
    /// Delete a report, or all of them.
    Delete {
        /// Report id or unique prefix.
        id: Option<String>,
        /// Delete every saved report.
        #[arg(long)]
        all: bool,
    },
}

/// Close the loop on a bug this machine reported, fixed in a release. It
/// used to ride on `cig update`; the installer updates now and calls this
/// after an update that changed cig. A failure here is the installer's to
/// ignore, so it never reports one: no tracker, no network, nothing to say.
fn announce_fixed(ctx: &Ctx) -> i32 {
    let cfg = Config::load();
    let repo = cfg.get("issues_repo");
    let fixed = ashtray::announce_fixed(&cfg, &crate::update::releases(&repo).unwrap_or_default());
    if ctx.json {
        outln!("{}", serde_json::json!({ "fixed_reports": fixed }));
    } else {
        for line in &fixed {
            eprintln!("{} {line}", ctx.green("fixed:"));
        }
    }
    exit::OK
}

pub fn crash(ctx: &Ctx, cmd: Option<CrashCommand>) -> i32 {
    match cmd.unwrap_or(CrashCommand::List) {
        CrashCommand::AnnounceFixed => announce_fixed(ctx),
        CrashCommand::List => list(ctx),
        CrashCommand::Show { id } => match find(ctx, &id) {
            Some(r) => {
                if ctx.json {
                    outln!("{}", serde_json::to_string(&r).unwrap_or_default());
                } else {
                    outln!(
                        "{}",
                        ctx.bold(&format!("{} report {}", r.kind.word(), r.id))
                    );
                    outln!("{}", ctx.dim("this is exactly what `cig crash send` would file, inside a JSON block:"));
                    outln!();
                    outln!(
                        "{}",
                        serde_json::to_string_pretty(&crash::sent_view(&r)).unwrap_or_default()
                    );
                    if let Some(x) = &r.repro {
                        outln!();
                        outln!(
                            "{}",
                            ctx.bold(&format!(
                                "repro ({}, {} op{})",
                                if x.verified {
                                    "verified in a sandbox"
                                } else {
                                    "NOT verified: not attached"
                                },
                                x.ops,
                                if x.ops == 1 { "" } else { "s" }
                            ))
                        );
                        outln!("{}", x.script);
                    }
                }
                exit::OK
            }
            None => exit::USAGE,
        },
        CrashCommand::Send { id } => match find(ctx, &id) {
            Some(mut r) => send(ctx, &mut r, true),
            None => exit::USAGE,
        },
        CrashCommand::Flush { quiet } => flush(ctx, quiet),
        CrashCommand::Submit {
            origin,
            file,
            repo,
            now,
        } => submit(ctx, &origin, &file, repo, now),
        CrashCommand::Delete { id, all } => {
            if all {
                let n = crash::list().unwrap_or_default().len();
                let _ = std::fs::remove_dir_all(crash::crashes_dir());
                let _ = std::fs::remove_dir_all(crash::ashtray_dir());
                eprintln!("deleted {n} report{}", if n == 1 { "" } else { "s" });
                return exit::OK;
            }
            let Some(id) = id else {
                eprintln!(
                    "{} give a report id, or --all",
                    ctx.red("error[E800 usage]:")
                );
                return exit::USAGE;
            };
            match find(ctx, &id) {
                Some(r) => {
                    let _ = std::fs::remove_file(crash::path_of(&r));
                    eprintln!("deleted {}", r.id);
                    exit::OK
                }
                None => exit::USAGE,
            }
        }
    }
}

fn find(ctx: &Ctx, id: &str) -> Option<CrashReport> {
    match crash::find(id) {
        Ok(Some(r)) => Some(r),
        Ok(None) => {
            eprintln!("{} no report matches `{id}`", ctx.red("error[E800 usage]:"));
            None
        }
        Err(e) => {
            eprintln!("{} {e}", ctx.red("error[E800 usage]:"));
            None
        }
    }
}

fn list(ctx: &Ctx) -> i32 {
    let all = crash::list().unwrap_or_default();
    if ctx.json {
        outln!("{}", serde_json::to_string(&all).unwrap_or_default());
        return exit::OK;
    }
    if all.is_empty() {
        eprintln!("the ashtray is empty: no report of a bug in cig (that is good)");
        return exit::OK;
    }
    outln!(
        "{:<24} {:<9} {:<10} {:<22} {}",
        "report",
        "kind",
        "version",
        "state",
        "what"
    );
    for r in &all {
        let state = r.state();
        outln!(
            "{:<24} {:<9} {:<10} {:<22} {}",
            r.id,
            r.kind.word(),
            r.version,
            crash::truncate(&state, 22),
            crash::truncate(r.headline(), 60)
        );
        if let Some(i) = &r.issue {
            outln!("{:<24} {}", "", ctx.dim(&i.url));
        } else if let Some(e) = &r.queue.last_error {
            outln!("{:<24} {}", "", ctx.dim(&format!("last try: {e}")));
        }
    }
    outln!(
        "{}",
        ctx.dim("cig crash show <id> reads one, cig crash send <id> files it, cig crash flush sends what is waiting")
    );
    exit::OK
}

/// The setting a "never" came from, for the message: `reports`, or the
/// older `crash_reports` when that alone is set.
pub fn never_key(cfg: &Config) -> &'static str {
    if !cfg.is_set("reports")
        && std::env::var_os("CIG_REPORTS").is_none()
        && (cfg.is_set("crash_reports") || std::env::var_os("CIG_CRASH_REPORTS").is_some())
    {
        "crash_reports"
    } else {
        "reports"
    }
}

/// Send one report now. `typed` says the person asked for this one, which
/// is a yes for it alone; an explicit `never` still holds.
fn send(ctx: &Ctx, r: &mut CrashReport, typed: bool) -> i32 {
    let cfg = Config::load();
    let explicit_never = (cfg.is_set("reports") && cfg.get("reports") == "never")
        || std::env::var("CIG_REPORTS").as_deref() == Ok("never")
        || (!cfg.is_set("reports")
            && std::env::var_os("CIG_REPORTS").is_none()
            && cfg.is_set("crash_reports")
            && cfg.get("crash_reports") == "never");
    if (crash::consent(&cfg) == Consent::Never && !typed) || explicit_never {
        eprintln!(
            "{} {} is \"never\", so nothing leaves this machine; the report stays at {}",
            ctx.red("error[E800 usage]:"),
            never_key(&cfg),
            crash::path_of(r).display()
        );
        eprintln!("  = hint: cig config reports ask, then send it again");
        eprintln!("  = explain: cig explain E800");
        return exit::USAGE;
    }
    r.queue.requested = true;
    let repo = ashtray::repo_for(&cfg, r);
    match ashtray::try_send(r, &cfg) {
        Ok(SendOutcome::Filed(url)) => {
            eprintln!("{} filed at {url}", ctx.green("sent:"));
            exit::OK
        }
        Ok(SendOutcome::Commented(url)) => {
            eprintln!(
                "{} this one is already known; its count was updated on {url}",
                ctx.green("sent:")
            );
            exit::OK
        }
        Ok(SendOutcome::Manual(url)) => {
            eprintln!(
                "{} {}, so nothing was sent; the report waits in the ashtray and goes with a later run once a tool is there.",
                ctx.yellow("note:"),
                crate::tracker::why_none()
            );
            eprintln!("open this link to file it yourself now (the report is prefilled):");
            eprintln!("  {url}");
            eprintln!(
                "and attach {} if the form truncated it.",
                crash::path_of(r).display()
            );
            exit::OK
        }
        Err(e) => {
            eprintln!("{} could not file on {repo} now: {e}", ctx.yellow("note:"));
            eprintln!(
                "the report waits in the ashtray ({}) and goes with a later run; cig crash flush tries again now",
                crash::path_of(r).display()
            );
            exit::OK
        }
    }
}

fn flush(ctx: &Ctx, quiet: bool) -> i32 {
    let cfg = Config::load();
    // Typed, it is a request for now: the backoff is the background's.
    let done = ashtray::flush(&cfg, !quiet);
    if ctx.json {
        outln!(
            "{}",
            serde_json::json!({
                "filed": done.filed, "noted": done.noted, "failed": done.failed, "waiting": done.waiting, "locked": done.locked,
            })
        );
        return exit::OK;
    }
    if quiet {
        return exit::OK;
    }
    if done.locked {
        eprintln!("another cig is sending from the ashtray right now; nothing to do here");
        return exit::OK;
    }
    for (id, url) in &done.filed {
        eprintln!("{} {id} filed at {url}", ctx.green("sent:"));
    }
    for (id, url) in &done.noted {
        eprintln!("{} {id} counted on {url}", ctx.green("sent:"));
    }
    for (id, why) in &done.failed {
        eprintln!("{} {id} could not be sent: {why}", ctx.yellow("waiting:"));
    }
    if done.filed.is_empty() && done.noted.is_empty() && done.failed.is_empty() {
        eprintln!(
            "nothing to send{}",
            if done.waiting > 0 {
                format!(" yet ({} waiting for its retry time)", done.waiting)
            } else {
                String::new()
            }
        );
    }
    exit::OK
}

/// A report another tool made joins the ashtray: its fields are read as a
/// report (unknown ones are kept under `details`), anonymised again (every
/// path its shape, a command line the words cig knows), given the origin
/// and the route it names.
fn submit(ctx: &Ctx, origin: &str, file: &PathBuf, repo: Option<String>, now: bool) -> i32 {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!(
                "{} cannot read {}: {e}",
                ctx.red("error[E802 usage]:"),
                file.display()
            );
            return exit::USAGE;
        }
    };
    let v: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "{} {} is not JSON: {e}",
                ctx.red("error[E802 usage]:"),
                file.display()
            );
            return exit::USAGE;
        }
    };
    let origin = origin.trim();
    if origin.is_empty()
        || !origin
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        eprintln!(
            "{} --origin names the tool, like cig-orchestrator",
            ctx.red("error[E800 usage]:")
        );
        return exit::USAGE;
    }
    if let Some(r) = &repo {
        if r.split('/').count() != 2 || r.contains(char::is_whitespace) {
            eprintln!(
                "{} --repo must look like owner/name, not `{r}`",
                ctx.red("error[E800 usage]:")
            );
            return exit::USAGE;
        }
    }
    let kind = match v["kind"].as_str().unwrap_or("crash") {
        "internal" => Kind::Internal,
        "tripwire" => Kind::Tripwire,
        "report" => Kind::Report,
        _ => Kind::Crash,
    };
    // Nothing of the other tool's paths travels: a mapping with no roots
    // gives every path in what it said its shape, and its command line is
    // reduced to the words cig knows.
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut mapping = crate::repro::Mapping::new(&[], &cwd);
    let message = mapping.anonymise_text(
        v["panic_message"]
            .as_str()
            .or_else(|| v["title"].as_str())
            .or_else(|| v["message"].as_str())
            .unwrap_or("(no message)"),
    );
    let location_raw = v["panic_location"]
        .as_str()
        .or_else(|| v["location"].as_str())
        .unwrap_or("?");
    let location = if location_raw.contains(['/', '\\']) {
        mapping.anonymise_text(location_raw)
    } else {
        location_raw.to_string()
    };
    let mut r = CrashReport::new(
        kind,
        &location,
        &message,
        None,
        v["active_run_id"].as_str().map(str::to_string),
    );
    r.origin = origin.to_string();
    r.repo = repo;
    // The tool's own version and the rest of what it said travel as detail,
    // anonymised the same way; cig's version stays as the cig that filed it.
    if let Some(ver) = v["version"].as_str() {
        r.title = Some(format!("{} ({origin} {ver})", r.headline()));
    }
    let mut details = v.clone();
    mapping.anonymise_value(&mut details);
    r.details = Some(details);
    if let Err(e) = ashtray::queue(&mut r) {
        eprintln!(
            "{} could not save the report: {e}",
            ctx.red("error[E802 usage]:")
        );
        return exit::USAGE;
    }
    let cfg = Config::load();
    match crash::consent(&cfg) {
        Consent::Never => {
            eprintln!(
                "{} saved as {} (reports is \"never\": it stays local)",
                ctx.green("ashtray:"),
                r.id
            );
            exit::OK
        }
        Consent::Always => {
            r.queue.requested = true;
            let _ = crash::write(&r);
            if now {
                send(ctx, &mut r, true)
            } else {
                eprintln!(
                    "{} {} queued; it goes with the next run (cig crash flush sends it now)",
                    ctx.green("ashtray:"),
                    r.id
                );
                ashtray::spawn_flush(&cfg);
                exit::OK
            }
        }
        Consent::Ask => {
            if now {
                return send(ctx, &mut r, true);
            }
            eprintln!(
                "{} saved as {}; cig crash show {} reads it, cig crash send {} files it, cig config reports always does that for you",
                ctx.green("ashtray:"),
                r.id,
                r.id,
                r.id
            );
            exit::OK
        }
    }
}

/// A report cig just made about itself, at the end of a run: queued, and
/// either sent (`always`), kept (`never`), or asked about once, the answer
/// remembered. Prints one line in every case; never changes the run.
pub fn settle_new(ctx: &Ctx, report: &mut CrashReport) {
    let cfg = Config::load();
    let repo = ashtray::repo_for(&cfg, report);
    let word = ctx.yellow(&format!("{}:", report.kind.word()));
    match crash::consent(&cfg) {
        Consent::Never => {
            eprintln!(
                "{} cig noticed something it got wrong; the report stays local ({} is \"never\"): cig crash show {}",
                word,
                never_key(&cfg),
                report.id
            );
        }
        Consent::Always => {
            report.queue.requested = true;
            let _ = crash::write(report);
            eprintln!(
                "{} cig noticed something it got wrong; a report is queued for github.com/{repo} (cig crash show {}; cig explain E902)",
                word,
                report.id
            );
        }
        Consent::Ask => {
            if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
                eprintln!(
                    "{} cig noticed something it got wrong; a report is saved: cig crash show {}   cig crash send {} files it; cig config reports always does that for you (cig explain E902)",
                    word,
                    report.id,
                    report.id
                );
                return;
            }
            eprintln!();
            eprintln!(
                "{} cig noticed something it got wrong ({}): {}",
                word,
                report.kind.word(),
                report.headline()
            );
            eprintln!(
                "A report was saved locally: {}",
                crash::path_of(report).display()
            );
            eprintln!("This is what it holds (nothing of your files, script source or environment values):");
            eprintln!(
                "{}",
                serde_json::to_string_pretty(report).unwrap_or_default()
            );
            eprintln!();
            eprint!(
                "Send reports of cig's own bugs to github.com/{repo} as public issues, this one and the ones to come? [y/N] "
            );
            let _ = io::stderr().flush();
            let mut line = String::new();
            let _ = io::stdin().lock().read_line(&mut line);
            let mut cfg = Config::load();
            if matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                let _ = cfg.set("reports", "always");
                let _ = cfg.save();
                report.queue.requested = true;
                let _ = crash::write(report);
                eprintln!("remembered: reports = always (cig config reports ask|never changes it). Sending.");
                send(ctx, report, true);
            } else {
                let _ = cfg.set("reports", "never");
                let _ = cfg.save();
                eprintln!(
                    "remembered: reports = never; the report stays at {} (cig config reports ask asks again, cig crash send {} files this one)",
                    crash::path_of(report).display(),
                    report.id
                );
            }
        }
    }
}

/// Runs in `main` after the interpreter thread panicked.
pub fn after_crash(ctx: &Ctx) {
    let Some(path) = crash::last_report() else {
        return;
    };
    let Ok(mut report) = crash::load(&path) else {
        return;
    };
    let cfg = Config::load();
    // The files are the person's first worry; the report is cig's.
    offer_rollback(ctx, &mut report, &cfg);
    let repo = ashtray::repo_for(&cfg, &report);
    match crash::consent(&cfg) {
        Consent::Never => {
            eprintln!(
                "{} is \"never\": the report stays local at {}. `cig config reports ask` changes this.",
                never_key(&cfg),
                path.display()
            );
        }
        Consent::Always => {
            eprintln!("reports is \"always\": sending the report below to github.com/{repo}.");
            eprintln!(
                "{}",
                serde_json::to_string_pretty(&report).unwrap_or_default()
            );
            report.queue.requested = true;
            let _ = crash::write(&report);
            send(ctx, &mut report, true);
        }
        Consent::Ask => {
            if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
                eprintln!(
                    "To file it: cig crash show {}   then   cig crash send {}",
                    report.id, report.id
                );
                return;
            }
            eprintln!();
            eprintln!("cig hit a bug and had to stop. A crash report was saved locally:");
            eprintln!("  {}", path.display());
            eprintln!(
                "View the full contents first:  cig crash show {}",
                report.id
            );
            eprintln!();
            eprintln!("It contains: cig version, OS/arch, the command line (token- and path-looking values");
            eprintln!(
                "redacted), the working directory relative to your home, a timestamp, the panic"
            );
            eprintln!(
                "message and where it happened in cig's own code, a backtrace of cig's own frames,"
            );
            eprintln!(
                "and the id of the run in progress, if any. It does NOT contain your script's"
            );
            eprintln!("source, environment variable values, or file contents.");
            eprintln!();
            eprint!("Send this report to github.com/{repo} as a public, permanent issue, and the ones to come without asking? [y/N/once] ");
            let _ = io::stderr().flush();
            let mut line = String::new();
            let _ = io::stdin().lock().read_line(&mut line);
            let answer = line.trim().to_ascii_lowercase();
            if matches!(answer.as_str(), "y" | "yes") {
                let mut cfg = Config::load();
                let _ = cfg.set("reports", "always");
                let _ = cfg.save();
                eprintln!(
                    "remembered: reports = always (cig config reports ask|never changes it)."
                );
                report.queue.requested = true;
                let _ = crash::write(&report);
                send(ctx, &mut report, true);
            } else if answer == "once" {
                report.queue.requested = true;
                let _ = crash::write(&report);
                send(ctx, &mut report, true);
            } else {
                eprintln!("not sent. Later: cig crash send {}", report.id);
            }
        }
    }
}

/// A crash mid-run leaves its burns in place; the journal (write-ahead, so
/// it survived) can put the reversible ones back. The journal on disk is
/// the authority here, never anything from the panicked thread: this is
/// the same path as `cig unburn` on an interrupted run.
fn offer_rollback(ctx: &Ctx, report: &mut CrashReport, cfg: &Config) {
    let Some(id) = report.active_run_id.clone() else {
        return;
    };
    let Ok(Some(rec)) = runs::find(&id) else {
        return;
    };
    let Ok(journal) = Journal::load(&rec.dir()) else {
        return;
    };
    if journal.is_empty() {
        return;
    }
    let total = journal.len();
    let irreversible: Vec<String> = journal
        .entries()
        .iter()
        .filter(|e| !e.reversible && !e.compensated && e.compensation.is_none() && !e.undone)
        .map(|e| e.op.describe())
        .collect();
    let reversible = total - irreversible.len();
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    let interactive = io::stdin().is_terminal() && io::stderr().is_terminal();
    let put_back = match cfg.get("crash_rollback").as_str() {
        "always" => {
            eprintln!(
                "crash_rollback is \"always\": putting back the {reversible} reversible change{} the run in progress ({id}) made.",
                plural(reversible)
            );
            true
        }
        "never" => {
            eprintln!(
                "The run in progress ({id}) made {total} change{}: cig unburn {id} puts them back (crash_rollback is \"never\").",
                plural(total)
            );
            false
        }
        _ if !interactive => {
            eprintln!(
                "The run in progress ({id}) made {total} change{}: cig unburn {id} puts them back.",
                plural(total)
            );
            false
        }
        _ => {
            eprintln!();
            eprintln!(
                "The run in progress ({id}) made {total} change{} before cig stopped:",
                plural(total)
            );
            if irreversible.is_empty() {
                eprintln!("  {reversible} reversible");
            } else {
                eprintln!(
                    "  {reversible} reversible, {} irreversible ({})",
                    irreversible.len(),
                    irreversible.join("; ")
                );
            }
            eprint!("Put the reversible ones back now? [Y/n] ");
            let _ = io::stderr().flush();
            let mut line = String::new();
            let _ = io::stdin().lock().read_line(&mut line);
            !matches!(line.trim().to_ascii_lowercase().as_str(), "n" | "no")
        }
    };
    if put_back {
        let code = super::runs::unburn(ctx, &id, false, false, false);
        report.rolled_back = Some(code == exit::OK);
    } else {
        if cfg.get("crash_rollback") == "ask" && interactive {
            eprintln!("not put back. Later: cig unburn {id}");
        }
        report.rolled_back = Some(false);
    }
    let _ = crash::write(report);
}
