// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! `cig runs` and `cig unburn`.

use super::{exit, Ctx};
use crate::burn::journal::{Before, Journal};
use crate::burn::runs::{self, RunRecord};
use std::io::Write;

pub fn runs(
    ctx: &Ctx,
    id: Option<String>,
    prune: Option<usize>,
    verify: bool,
    deep: bool,
    forget: Option<String>,
    show_all: bool,
) -> i32 {
    if let Some(keep) = prune {
        return prune_runs(ctx, keep);
    }
    if let Some(gone) = forget {
        // The user says the run is gone on purpose: the ledger records it,
        // and the missing-run note stops.
        return match crate::burn::ledger::append("forgotten", &gone, "", "forgotten", 0) {
            Ok(()) => {
                eprintln!(
                    "{} {gone} will not be noted as missing again",
                    ctx.green("ok:")
                );
                exit::OK
            }
            Err(e) => {
                eprintln!("{} could not write the ledger: {e}", ctx.red("error:"));
                exit::SCRIPT_ERROR
            }
        };
    }
    // The default table (#149): the newest 50 off the end of the index,
    // no stat and no copy per run. `--all`, `--json`, an id and `--verify`
    // want every run and take the full list below, which also reconciles
    // the index when the runs folder no longer fits it.
    let (recent, total) = if id.is_none() && !verify && !show_all && !ctx.json {
        match crate::burn::runs_index::list_recent(TABLE_ROWS) {
            // A run that says it is running may have died: only the full
            // list checks, so it takes over.
            Some(r)
                if !r.is_empty()
                    && !r
                        .iter()
                        .any(|x| matches!(x.status.as_str(), "running" | "damaged")) =>
            {
                (Some(r), None)
            }
            _ => (None, None),
        }
    } else {
        (None, None)
    };
    if let Some(rows) = recent {
        print_table(ctx, &rows, total);
        return exit::OK;
    }
    let all = match runs::list() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{} cannot list runs: {e}", ctx.red("error:"));
            return exit::USAGE;
        }
    };
    match id {
        Some(id) => show(ctx, &id, &all, verify),
        None if verify => verify_all(ctx, &all, deep),
        None => {
            if ctx.json {
                let listed: Vec<serde_json::Value> = all
                    .iter()
                    .map(|r| {
                        let mut v = serde_json::to_value(r).unwrap_or_default();
                        if is_pinned(r) {
                            v["pinned"] = serde_json::json!(true);
                        }
                        v
                    })
                    .collect();
                outln!("{}", serde_json::to_string(&listed).unwrap_or_default());
                return exit::OK;
            }
            note_missing_runs(ctx, &all);
            // The layers an interrupted run never applied are no one's:
            // cig was not there to apply them, so they go (the ones kept
            // on purpose stay).
            for r in all.iter().filter(|r| r.is_interrupted()) {
                let n = crate::burn::overlay::drop_unapplied(&r.dir());
                if n > 0 {
                    eprintln!(
                        "{} {} died in a boxed hop; the layer{} it never applied {} removed (cig unburn {} puts its earlier burns back)",
                        ctx.yellow("note:"),
                        r.id,
                        if n == 1 { "" } else { "s" },
                        if n == 1 { "was" } else { "were" },
                        r.id
                    );
                }
            }
            if all.is_empty() {
                eprintln!(
                    "no runs recorded yet (state dir: {})",
                    runs::runs_dir().display()
                );
                return exit::OK;
            }
            let rows = if show_all {
                all.len()
            } else {
                all.len().min(TABLE_ROWS)
            };
            print_table(ctx, &all[..rows], Some(all.len()));
            exit::OK
        }
    }
}

/// Per run: the paths it does not write back, and the paths whose
/// changed-since check it skips (the combined restore).
type CombinePlan = std::collections::BTreeMap<String, (Vec<String>, Vec<String>)>;

const TABLE_ROWS: usize = 50;

/// The table: one buffered write, the newest 50 unless `--all` handed in
/// every run. `total` is how many runs there are, when known.
fn print_table(ctx: &Ctx, all: &[RunRecord], total: Option<usize>) {
    let rows = all.len();
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    let _ = writeln!(
        out,
        "{:<24} {:<12} {:>5} {:>4}  script",
        "run", "status", "burns", "irr"
    );
    for r in all.iter().take(rows) {
        let status = match r.status.as_str() {
            "ok" => ctx.green(&r.status),
            "rolled_back" | "unburned" => ctx.yellow(&r.status),
            "interrupted" => ctx.red(&r.status),
            "running" => ctx.dim(&r.status),
            _ => ctx.red(&r.status),
        };
        let script = if r.program.is_some() {
            format!("{} (program)", r.script)
        } else {
            r.script.clone()
        };
        let script = if is_pinned(r) {
            format!("{script}  (pinned)")
        } else {
            script
        };
        let _ = writeln!(
            out,
            "{:<24} {:<12} {:>5} {:>4}  {}",
            r.id, status, r.burns, r.irreversible, script
        );
    }
    let _ = out.flush();
    match total {
        Some(t) if t > rows => {
            eprintln!("showing the newest {rows} of {t} runs (cig runs --all lists every one)")
        }
        None if rows >= TABLE_ROWS => {
            eprintln!("showing the newest {rows} runs (cig runs --all lists every one)")
        }
        _ => {}
    }
}

/// `cig runs --verify` with no id (#149, #177): a quick check across the
/// whole history. The fast path reads each journal's **last line only**
/// (`journal::tail_head`), computes its chain head, and compares it with
/// the record's `journal_head` and the ledger's head for that run. A cut
/// journal is caught (its tail head does not fit either), a replaced one
/// is caught, a record rewritten after the fact is caught, and a stale
/// cached view is caught. No full journal is opened by the fast path.
///
/// A missing, empty or unreadable ledger, with runs that recorded heads,
/// is a problem (not a silent `ok`): the ledger is what names a journal
/// whose record was made to agree with it. The fast path then falls
/// back to a deep walk, so the chains are still checked by their own
/// `prev` links, and exits non-zero.
///
/// With `--deep` the chain of each journal is also walked (slower; the
/// path the `<id> --verify` call takes, for every run).
fn verify_all(ctx: &Ctx, all: &[RunRecord], deep: bool) -> i32 {
    let ledger_presence = crate::burn::ledger::presence();
    let ledger = crate::burn::ledger::load().ok();
    // Each run's last record and whether the ledger let it go, found in
    // one pass, so the check costs the ledger's length and not runs ×
    // ledger at ten thousand runs.
    let mut last_rec: std::collections::HashMap<&str, &crate::burn::ledger::Line> =
        std::collections::HashMap::new();
    let mut let_go: std::collections::HashSet<&str> = std::collections::HashSet::new();
    if let Some(l) = ledger.as_ref() {
        let mut decided: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for line in l.lines.iter().rev() {
            if decided.insert(line.run.as_str())
                && (line.kind == "pruned" || line.kind == "forgotten")
            {
                let_go.insert(line.run.as_str());
            }
            if (line.kind == "run" || line.kind == "unburn" || line.kind == "origin")
                && !last_rec.contains_key(line.run.as_str())
            {
                last_rec.insert(line.run.as_str(), line);
            }
        }
    }
    let mut checked = 0usize;
    let mut problems: Vec<String> = Vec::new();
    // The ledger is required when any run recorded a head: without it,
    // the one thing that can catch a journal cut short on the fast
    // path is the ledger's own record of where each chain ended.
    // Runs from before the ledger (1.17.0) have heads and no ledger
    // lines, so a history of only those has nothing to check against.
    let any_head = all
        .iter()
        .any(|r| r.journal_head.is_some() && records_the_ledger(&r.version));
    let ledger_gone = matches!(
        ledger_presence,
        crate::burn::ledger::Presence::Missing | crate::burn::ledger::Presence::Empty
    );
    // The ledger's own chain (#184): every line is signed over the line
    // before it, so an edited, cut or reordered line breaks it from there
    // on. Said first, whatever the runs say.
    let ledger_broken_at = ledger.as_ref().and_then(|l| l.broken_at);
    if let Some(at) = ledger_broken_at {
        problems.push(format!(
            "the ledger's own chain is broken at line {at} (a line was edited, removed or reordered); nothing more is appended to it [E764]"
        ));
    }
    let mut forced_deep = deep;
    if any_head && ledger_gone {
        let where_ = crate::burn::ledger::path();
        problems.push(format!(
            "the ledger is gone ({}): heads cannot be checked against it. Every run's chain was walked instead.",
            crate::crash::tilde(&where_)
        ));
        forced_deep = true;
    }
    for r in all {
        checked += 1;
        if forced_deep {
            // Walk the journal and compare it with the record, as the
            // `<id> --verify` path does. Reads the journal: slow at ten
            // thousand runs, which is why `--deep` is opt-in — a missing
            // ledger falls back here so the chains are still checked.
            let Ok(j) = Journal::load(&r.dir()) else {
                problems.push(format!("{}: the journal cannot be read", r.id));
                continue;
            };
            let v = verify_run(r, &j);
            if !v.ok {
                problems.push(format!(
                    "{}: {}",
                    r.id,
                    v.verdict.unwrap_or_else(|| "not verified".to_string())
                ));
            }
            continue;
        }
        // Heads only, with the journal tail for honesty (#177): the
        // last line's chain hash is the one the record saved when the
        // run ended, so a comparison that leaves it out will not catch
        // a journal cut short.
        let ledger_line = last_rec.get(r.id.as_str()).copied();
        let ledger_head = ledger_line.map(|line| line.head.clone());
        let tail = crate::burn::journal::tail_head(&r.dir())
            .ok()
            .unwrap_or(crate::burn::journal::TailHead::NoChain);
        let tail_h: Option<&str> = match &tail {
            crate::burn::journal::TailHead::Head(h) => Some(h.as_str()),
            // These do not give a head to compare with on the fast
            // path; the deep walk is the one that can judge them.
            _ => None,
        };
        let rec_h = r.journal_head.as_deref();
        let led_h = ledger_head.as_deref();
        match (&tail, tail_h, rec_h, led_h) {
            // Journal missing, but the record says there was one.
            (crate::burn::journal::TailHead::Missing, _, Some(_), _) => {
                problems.push(format!(
                    "{}: the journal is gone, but the record has a chain head for it",
                    r.id
                ));
            }
            // The journal is empty (zero bytes, or only the header on
            // disk) yet the record carries a head: the lines the record
            // was made against are gone. This is the #112 case: a
            // journal cut back below its body lines (#177).
            (crate::burn::journal::TailHead::Empty, _, Some(_), _) => {
                problems.push(format!(
                    "{}: the journal is cut back to its header, but the record carries a chain head for lines beyond it",
                    r.id
                ));
            }
            // The whole triple agrees, or there is nothing to judge (a
            // run that burned nothing, format 1, another key).
            (_, Some(t), Some(rh), Some(lh)) if t == rh && rh == lh => {}
            (_, _, None, None) => {}
            // All three disagree somewhere. Name every one that does.
            (_, _, _, _) => {
                let mut said = false;
                if let (Some(t), Some(lh)) = (tail_h, led_h) {
                    if t != lh {
                        problems.push(format!(
                            "{}: the journal's last line does not fit the ledger's head for this run (cut short, or replaced)",
                            r.id
                        ));
                        said = true;
                    }
                }
                if let (Some(rh), Some(lh)) = (rec_h, led_h) {
                    if rh != lh {
                        problems.push(format!(
                            "{}: the record's chain head is not the ledger's head for this run (the record was rewritten)",
                            r.id
                        ));
                        said = true;
                    }
                }
                if let (Some(t), Some(rh)) = (tail_h, rec_h) {
                    if t != rh {
                        problems.push(format!(
                            "{}: the journal's last line is not the chain head the record saved (the cached view is stale, or the journal was changed since)",
                            r.id
                        ));
                        said = true;
                    }
                }
                // A head with no ledger line: the ledger has no record
                // of this run. Exception: runs from before the ledger
                // (1.17.0), which have no line because nothing wrote one.
                if !said && rec_h.is_some() && led_h.is_none() && !ledger_gone {
                    if !records_the_ledger(&r.version) {
                        // A pre-1.17.0 run: the ledger was not written
                        // yet when this run ended, so a missing line is
                        // expected. Not a problem.
                    } else if ledger.is_some() {
                        if let_go.contains(r.id.as_str()) {
                            // Pruned or forgotten: the ledger said so
                            // on purpose; `cig runs --pruned` lists it.
                        } else {
                            problems.push(format!(
                                "{}: the ledger has no record of this run (hand-added, or its line was removed)",
                                r.id
                            ));
                        }
                    }
                }
                // A head the record should have carried but does not,
                // while the tail-read says the chain is intact: the
                // record was not saved, or it was reset to no head.
                if !said && tail_h.is_some() && rec_h.is_none() && r.finished.is_some() {
                    problems.push(format!(
                        "{}: the record carries no chain head but the journal does (the record was reset)",
                        r.id
                    ));
                }
            }
        }
    }
    let summary_tail = if forced_deep {
        if deep {
            "chains walked"
        } else {
            "chains walked, since the ledger is gone"
        }
    } else {
        "heads checked against the ledger and each journal's last line"
    };
    if ctx.json {
        outln!(
            "{}",
            serde_json::json!({
                "checked": checked,
                "deep": forced_deep,
                "problems": problems,
                "ledger": {"broken_at": ledger_broken_at},
                "ok": problems.is_empty(),
            })
        );
    } else {
        outln!(
            "{} {} run{} ({})",
            if problems.is_empty() {
                ctx.green("ok:")
            } else {
                ctx.red("problems:")
            },
            checked,
            if checked == 1 { "" } else { "s" },
            summary_tail
        );
        for p in &problems {
            outln!("  {}", ctx.red(p));
        }
        if !forced_deep {
            outln!("  --deep walks each journal's chain (slower).");
        }
    }
    if problems.is_empty() {
        exit::OK
    } else {
        exit::SCRIPT_ERROR
    }
}

/// Does this version write ledger lines? The ledger was added in 1.17.0:
/// before it, a run that ended wrote no `run`/`unburn` line, so a run
/// with a recorded head and no ledger line is not missing from the
/// ledger — the ledger did not exist yet. Later runs do, and are flagged.
fn records_the_ledger(version: &str) -> bool {
    let mut parts = version
        .split(|c: char| !c.is_ascii_digit())
        .filter(|p| !p.is_empty())
        .map(|p| p.parse::<u64>().unwrap_or(0));
    let v = (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    );
    v >= (1, 17, 0)
}

fn show(ctx: &Ctx, id: &str, all: &[RunRecord], verify: bool) -> i32 {
    let rec = match runs::find(id) {
        Ok(Some(r)) => r,
        Ok(None) => {
            eprintln!(
                "{} no run matches `{id}` ({} recorded)",
                ctx.red("error[E803 usage]:"),
                all.len()
            );
            eprintln!("  = hint: `cig runs` lists them; give more of the id if it is ambiguous");
            eprintln!("  = explain: cig explain E803");
            return exit::USAGE;
        }
        Err(e) => {
            eprintln!("{} {e}", ctx.red("error[E803 usage]:"));
            eprintln!("  = explain: cig explain E803");
            return exit::USAGE;
        }
    };
    let loaded = Journal::load(&rec.dir());
    // A journal that is there but cannot be read is not a pass: --verify
    // said nothing and exited 0 for one.
    if verify {
        if let Err(e) = &loaded {
            if crate::burn::pack::journal_exists(&rec.dir()) {
                eprintln!(
                    "{} the journal of {} cannot be read: {e}",
                    ctx.red("error[E760 burn]:"),
                    rec.id
                );
                eprintln!("  = hint: it was edited or damaged after the run; cig unburn {} refuses it too", rec.id);
                eprintln!("  = explain: cig explain E760");
                return exit::SCRIPT_ERROR;
            }
        }
    }
    let journal = loaded.ok();
    let checked = if verify {
        journal.as_ref().map(|j| verify_run(&rec, j))
    } else {
        None
    };
    if ctx.json {
        let obj = serde_json::json!({
            "run": rec,
            "journal": journal.as_ref().map(|j| j.entries().to_vec()),
            "verify": checked,
        });
        outln!("{}", serde_json::to_string(&obj).unwrap_or_default());
        return match &checked {
            Some(v) if !v.ok => exit::SCRIPT_ERROR,
            _ => exit::OK,
        };
    }
    outln!("{}", ctx.bold(&rec.id));
    if let Some(p) = &rec.program {
        outln!(
            "  program   {}  (compiled from {}, sha256 {}, by {})",
            rec.script,
            p["file"].as_str().unwrap_or("?"),
            &rec.script_sha256[..rec.script_sha256.len().min(12)],
            p["compiler"].as_str().unwrap_or("?")
        );
        if let Some(b) = p["binary"].as_str().filter(|b| !b.is_empty()) {
            outln!("  binary    {b}");
        }
    } else {
        outln!(
            "  script    {}  (sha256 {})",
            rec.script,
            &rec.script_sha256[..rec.script_sha256.len().min(12)]
        );
    }
    if !rec.args.is_empty() {
        outln!("  args      {}", rec.args.join(" "));
    }
    outln!("  cwd       {}", rec.cwd);
    if let Some(parent) = &rec.parent {
        outln!(
            "  parent    {parent}  (a cig inside a hop of that run, taken in when the hop ended; that run's rollback undoes this one through the burn protocol)"
        );
    }
    let children: Vec<&str> = all
        .iter()
        .filter(|r| r.parent.as_deref() == Some(rec.id.as_str()))
        .map(|r| r.id.as_str())
        .collect();
    if !children.is_empty() {
        outln!(
            "  children  {}  (cigs inside this run's hops)",
            children.join(" ")
        );
    }
    outln!("  mode      {}", rec.mode);
    outln!("  started   {}", rec.started);
    outln!("  finished  {}", rec.finished.as_deref().unwrap_or("-"));
    outln!("  status    {}", rec.status);
    if let Some(e) = &rec.error {
        outln!("  error     {e}");
    }
    outln!(
        "  burns     {} ({} irreversible)",
        rec.burns,
        rec.irreversible
    );
    if !rec.rollback_failed.is_empty() {
        outln!(
            "  rollback  {} restore{} failed:",
            rec.rollback_failed.len(),
            if rec.rollback_failed.len() == 1 {
                ""
            } else {
                "s"
            }
        );
        for f in &rec.rollback_failed {
            outln!("    {} {f}", ctx.red("failed"));
        }
    }
    if !rec.pack.is_empty() {
        outln!(
            "  pack      {}",
            rec.pack
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if let Some(imp) = &rec.imported {
        outln!(
            "  imported  from {} at {} (paths read through its map)",
            imp["from"].as_str().unwrap_or("?"),
            imp["at"].as_str().unwrap_or("?")
        );
    }
    outln!("  dir       {}", rec.dir().display());
    if let Some(j) = journal {
        outln!(
            "  journal   {} entr{}",
            j.len(),
            if j.len() == 1 { "y" } else { "ies" }
        );
        for e in j.entries() {
            let tag = if e.reversible {
                "reversible  "
            } else {
                "irreversible"
            };
            outln!(
                "    {:>4}  {tag}  {}{}",
                e.seq,
                e.op.describe(),
                if e.undone {
                    ctx.dim("  (undone by a retry)")
                } else if e.restored {
                    ctx.dim("  (put back)")
                } else {
                    String::new()
                }
            );
        }
        let taken: Vec<&crate::burn::journal::BeforeState> =
            j.entries().iter().flat_map(|e| e.took.iter()).collect();
        if !taken.is_empty() {
            outln!(
                "  took      {} file{} the rollback moved into the tin instead of destroying; cig unburn {} --give-back puts them back:",
                taken.len(),
                if taken.len() == 1 { "" } else { "s" },
                rec.id
            );
            for t in taken {
                outln!("    {}", t.path.display());
            }
        }
    }
    if let Some(v) = checked {
        outln!(
            "  chain     {}",
            if v.chain_ok {
                v.chain_text
            } else {
                ctx.red(&v.chain_text)
            }
        );
        outln!(
            "  head      {}",
            if v.head_ok {
                v.head_text
            } else {
                ctx.red(&v.head_text)
            }
        );
        outln!(
            "  ledger    {}",
            if v.ledger_ok {
                v.ledger_text
            } else {
                ctx.red(&v.ledger_text)
            }
        );
        if v.problems.is_empty() {
            outln!(
                "  objects   every snapshot and tin object the journal needs is there and hashes as recorded"
            );
        } else {
            outln!(
                "  objects   {} problem{}:",
                v.problems.len(),
                if v.problems.len() == 1 { "" } else { "s" }
            );
            for p in &v.problems {
                outln!("    {} {p}", ctx.red("problem"));
            }
        }
        // What unburn would say, with its code: --verify is the strictest
        // view, and it agrees with unburn on every refusal.
        if let (Some(code), Some(verdict)) = (&v.code, &v.verdict) {
            eprintln!("{} {verdict}", ctx.red(&format!("error[{code} burn]:")));
            eprintln!(
                "  = hint: cig unburn {} refuses this as it stands; cig explain {code} says why",
                rec.id
            );
            eprintln!("  = explain: cig explain {code}");
        }
        return if v.ok { exit::OK } else { exit::SCRIPT_ERROR };
    }
    exit::OK
}

/// What `cig runs <id> --verify` found.
#[derive(serde::Serialize)]
struct Verified {
    chain: crate::burn::journal::Chain,
    chain_ok: bool,
    chain_text: String,
    head_recorded: Option<String>,
    head: String,
    head_ok: bool,
    head_text: String,
    ledger_ok: bool,
    ledger_text: String,
    problems: Vec<String>,
    ok: bool,
    /// What `cig unburn` would refuse with, checked in its order: another
    /// key (E708), the ledger (E709), the chain or its head (E760), the
    /// objects (E703). `None` when it would restore.
    code: Option<String>,
    /// That refusal's message, as unburn gives it.
    verdict: Option<String>,
}

/// Walk the journal's chain, compare its head with the record, and check
/// every snapshot and tin object a rollback would need.
/// Is this journal the record the run wrote? The first line whose `prev`
/// does not fit, or, when every line fits, a last line that is not the one
/// the record saved as the chain's head (edited, or lines added or removed
/// at the end after the record was written; an unburn or a give-back
/// saves the head again after the markers it appends).
/// Runs the ledger lists that are not in `runs/` and were not pruned or
/// forgotten: one calm note (E710), never an alarm. Only a ledger line that
/// fails its MAC is a tripwire, and that is reported by the verify path.
fn note_missing_runs(ctx: &Ctx, all: &[RunRecord]) {
    let Ok(ledger) = crate::burn::ledger::load() else {
        return;
    };
    // One pass each way, so the note costs the ledger's length, not its
    // square (#178): the runs here, and the runs whose last ledger line
    // lets them go (pruned or forgotten), as `is_let_go` reads it.
    let live: std::collections::HashSet<&str> = all.iter().map(|r| r.id.as_str()).collect();
    let mut last_seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut let_go: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for l in ledger.lines.iter().rev() {
        if last_seen.insert(l.run.as_str()) && (l.kind == "pruned" || l.kind == "forgotten") {
            let_go.insert(l.run.as_str());
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut missing: Vec<String> = Vec::new();
    // Only this tin's lines: another state directory beside the same key
    // keeps its own runs, and they are not missing from here.
    for l in ledger.of_this_tin().rev() {
        if l.kind != "run" && l.kind != "unburn" && l.kind != "origin" {
            continue;
        }
        if !seen.insert(l.run.clone()) {
            continue;
        }
        if live.contains(l.run.as_str()) || let_go.contains(l.run.as_str()) {
            continue;
        }
        missing.push(l.run.clone());
    }
    if missing.is_empty() {
        return;
    }
    missing.sort();
    for id in &missing {
        eprintln!(
            "{} run {id} is in the ledger but not in runs/ (removed by hand?); cig runs --forget {id} to stop this note",
            ctx.yellow("note[E710 burn]:")
        );
    }
}

/// What the ledger says this run's journal ended at, against the journal
/// on disk: `Some(reason)` when the journal was cut short or swapped for
/// an older copy (E709). `None` when it fits, when the ledger has no entry
/// for the run (a run from before the ledger), or when the ledger cannot
/// be read (the key is not here: nothing to compare with).
fn ledger_disagrees(rec: &RunRecord, journal: &Journal) -> Option<String> {
    ledger_check(rec, journal).0
}

/// The ledger's view of this run's journal: `Some(reason)` when it was cut
/// short or swapped for an older copy (E709), and what `--verify` says.
fn ledger_check(rec: &RunRecord, journal: &Journal) -> (Option<String>, String) {
    // A journal under another key cannot be judged against this ledger, and
    // a ledger whose own chain is broken judges nothing (that is E709 in
    // `cig tin verify`, not a verdict on this run).
    if journal.chain().other_key {
        return (
            None,
            "not checked: the journal is under another key".to_string(),
        );
    }
    if journal.chain().unverified {
        return (
            None,
            "not checked: the tin's key is not available here".to_string(),
        );
    }
    // The record speaks first: a journal whose head is not the one its own
    // record saved is reported by `tampered` (E760). The ledger judges what
    // the record cannot: a journal cut short or swapped whose record was
    // made to agree with it, or carries no head at all.
    if rec
        .journal_head
        .as_deref()
        .is_some_and(|h| h != journal.head())
    {
        return (
            None,
            "not consulted: the journal already disagrees with its own record (above)".to_string(),
        );
    }
    let Ok(ledger) = crate::burn::ledger::load() else {
        return (
            None,
            "not checked: the ledger cannot be read here".to_string(),
        );
    };
    if ledger.broken_at.is_some() {
        return (
            None,
            "not checked: the ledger's own chain is broken (cig tin verify says where)".to_string(),
        );
    }
    let Some(line) = crate::burn::ledger::last_record_of(&ledger, &rec.id) else {
        return (
            None,
            "no line for this run (a run from before the ledger, or another machine's)".to_string(),
        );
    };
    let lines = journal.chain().lines as u64;
    if lines < line.lines {
        return (
            Some(format!(
                "this run's journal ends at line {lines}, but the ledger says it went to {}: cut short",
                line.lines
            )),
            String::new(),
        );
    }
    if line.head != journal.head() && lines == line.lines {
        return (
            Some(
                "this is an old copy of the record: the ledger has a later one with the same length and another head".to_string(),
            ),
            String::new(),
        );
    }
    (
        None,
        "agrees: the journal ends where the ledger says it did".to_string(),
    )
}

fn tampered(rec: &RunRecord, journal: &Journal) -> Option<(usize, String)> {
    let chain = journal.chain();
    if let Some(b) = &chain.broken_at {
        return Some((b.line, b.reason.clone()));
    }
    // A finished run's record has carried its journal's head since 1.5.0;
    // since 1.15.1 one that burned nothing writes no journal line and
    // records no head (#129). A journal under a finished, headless record
    // that holds lines after all, chained or not, was written after the
    // run. (A run whose process died never recorded a head: not this.)
    if rec.journal_head.is_none()
        && rec.finished.is_some()
        && records_its_head(&rec.version)
        && (!journal.wrote_nothing() || !journal.entries().is_empty())
    {
        return Some((
            chain.lines.max(journal.entries().len()),
            "has lines, but the run's record says it wrote none (it burned nothing): they were added after the run".to_string(),
        ));
    }
    // Under another key, or with no key to check with, the head computed
    // here is not the one the record saved: that is E708's business, or a
    // note, not tampering.
    if !chain.checked() || chain.other_key || chain.unverified {
        return None;
    }
    match &rec.journal_head {
        Some(h) if *h != journal.head() => Some((
            chain.lines,
            "is not the last line the record saved as the chain's head: it was edited, or lines were added or removed at the end".to_string(),
        )),
        _ => None,
    }
}

/// Was the record made by a cig that saves its journal's head when the run
/// finishes (1.5.0 and later)?
fn records_its_head(version: &str) -> bool {
    let mut parts = version
        .split(|c: char| !c.is_ascii_digit())
        .filter(|p| !p.is_empty())
        .map(|p| p.parse::<u64>().unwrap_or(0));
    let v = (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    );
    v >= (1, 5, 0)
}

fn verify_run(rec: &RunRecord, journal: &Journal) -> Verified {
    let chain = journal.chain().clone();
    let signed = chain.format >= 3;
    let (chain_ok, chain_text) = if !chain.checked() {
        (
            true,
            format!(
                "none: the journal is format {} (from before 1.5.0), so its lines carry no prev",
                chain.format
            ),
        )
    } else if chain.other_key {
        (
            false,
            "signed by another key (another machine's, or one since replaced): not checked here; cig unburn --accept-unsigned restores from it by its hashes".to_string(),
        )
    } else if chain.unverified {
        (
            true,
            "not checked: the journal is signed and the tin's key is not available here; its files are checked by their hashes".to_string(),
        )
    } else if let Some(b) = &chain.broken_at {
        (false, format!("BROKEN at line {}: {}", b.line, b.reason))
    } else {
        let renamed = match &chain.run {
            Some(r) if *r != rec.id => {
                format!(" (written as run {r}; the directory was renamed since)")
            }
            _ => String::new(),
        };
        (
            true,
            format!(
                "intact: {} line{}, each {}{renamed}",
                chain.lines,
                if chain.lines == 1 { "" } else { "s" },
                if signed {
                    "signed by your key"
                } else {
                    "carrying the hash of the one before it"
                }
            ),
        )
    };
    let head = journal.head().to_string();
    let (head_ok, head_text) = match &rec.journal_head {
        _ if !chain.checked() => (
            true,
            "not checked: a journal without a chain has no head to compare".to_string(),
        ),
        // As unburn: a head computed under another key, or without one, is
        // not the head the record saved, and that is not tampering.
        _ if chain.other_key || chain.unverified => (
            true,
            "not checked: the chain could not be checked here (above)".to_string(),
        ),
        None if rec.finished.is_some() && records_its_head(&rec.version) => {
            let nothing = journal.wrote_nothing() && journal.entries().is_empty();
            (
                nothing,
                if nothing {
                    "none: the run burned nothing, so its journal holds no line".to_string()
                } else {
                    "none recorded, yet the journal holds lines: they were written after the run"
                        .to_string()
                },
            )
        }
        None => (
            true,
            "not recorded yet (the run has not finished, or died before it could)".to_string(),
        ),
        Some(h) if *h == head => (true, "matches the record: no line was added or removed since it was saved".to_string()),
        Some(_) => (
            false,
            "does not match the record: lines were added, removed or changed after the record was saved".to_string(),
        ),
    };
    let (ledger_says, ledger_text) = ledger_check(rec, journal);
    let ledger_ok = ledger_says.is_none();
    let ledger_text = ledger_says.clone().unwrap_or(ledger_text);
    let problems = journal.verify();
    // The verdict, in unburn's order and with its words.
    let (code, verdict) = if chain.other_key {
        (
            Some("E708"),
            Some(
                "this run was signed by another key; its files still match their hashes"
                    .to_string(),
            ),
        )
    } else if let Some(reason) = ledger_says {
        (Some("E709"), Some(reason))
    } else if let Some((line, reason)) = tampered(rec, journal) {
        (
            Some("E760"),
            Some(format!(
                "the journal of {} is not the record the run wrote: line {line} {reason}",
                rec.id
            )),
        )
    } else if !problems.is_empty() {
        (
            Some("E703"),
            Some(format!(
                "cannot roll back {}: {} problem{} found before touching anything",
                rec.id,
                problems.len(),
                if problems.len() == 1 { "" } else { "s" }
            )),
        )
    } else {
        (None, None)
    };
    let ok = code.is_none() && chain_ok && head_ok && ledger_ok;
    Verified {
        chain,
        chain_ok,
        chain_text,
        head_recorded: rec.journal_head.clone(),
        head,
        head_ok,
        head_text,
        ledger_ok,
        ledger_text,
        problems,
        ok,
        code: code.map(str::to_string),
        verdict,
    }
}

/// `cig unburn <id> --give-back`: put back what the run's rollback moved
/// into the tin. What is at those paths now goes to the tin in turn. With
/// `--dry-run` the list alone, with what is there now and whether it is
/// still what the rollback left; a path changed since is refused without
/// `--force`, as a rollback over a later edit is.
pub fn give_back(ctx: &Ctx, id: &str, dry_run: bool, force: bool) -> i32 {
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
            eprintln!("  = explain: cig explain E803");
            return exit::USAGE;
        }
    };
    let mut journal = match Journal::load(&rec.dir()) {
        Ok(j) => j,
        Err(e) => {
            eprintln!(
                "{} cannot load the journal for {}: {e}",
                ctx.red("error[E703 burn]:"),
                rec.id
            );
            eprintln!("  = explain: cig explain E703");
            return exit::SCRIPT_ERROR;
        }
    };
    if !journal.has_taken() {
        eprintln!(
            "the rollback of {} moved nothing into the tin; there is nothing to give back",
            rec.id
        );
        return exit::OK;
    }
    // The took markers are read from the journal: a journal that is not
    // the record is given back from only on purpose, as it is unburned.
    if let Some((line, reason)) = tampered(&rec, &journal) {
        if !force && !dry_run {
            eprintln!(
                "{} the journal of {} is not the record the run wrote: line {line} {reason}",
                ctx.red("error[E760 burn]:"),
                rec.id,
            );
            eprintln!(
                "  = hint: cig runs {} --verify walks the chain; pass --force to give back from the journal as it is, knowing it was changed",
                rec.id
            );
            eprintln!("  = explain: cig explain E760");
            return exit::SCRIPT_ERROR;
        }
        eprintln!(
            "{} the journal's chain is broken at line {line} ({reason}); {}",
            ctx.yellow("note:"),
            if dry_run {
                "a real give-back would refuse [E760] unless --force"
            } else {
                "--force given"
            }
        );
    }
    let plan = journal.give_back_plan();
    let changed: Vec<&crate::burn::journal::GiveBack> = plan.iter().filter(|g| g.changed).collect();
    if dry_run {
        if ctx.json {
            outln!(
                "{}",
                serde_json::json!({"run": rec.id, "would_give_back": plan})
            );
            return exit::OK;
        }
        outln!(
            "{}",
            ctx.bold(&format!(
                "would give back {} file{} the rollback of {} moved into the tin",
                plan.len(),
                if plan.len() == 1 { "" } else { "s" },
                rec.id
            ))
        );
        if !changed.is_empty() {
            outln!(
                "  {} {} path{} changed since the rollback; a real give-back would refuse [E704] unless --force",
                ctx.yellow("note:"),
                changed.len(),
                if changed.len() == 1 { "" } else { "s" }
            );
        }
        for g in &plan {
            let column = match (&g.left, g.changed) {
                (None, _) => ctx.dim("  changed since: unknown"),
                (Some(_), false) => ctx.dim("  unchanged since the rollback"),
                (Some(_), true) => ctx.yellow("  changed since the rollback"),
            };
            outln!(
                "  {:>4}  give back  {}  <- {} bytes, sha256 {} (now {}){column}",
                g.seq,
                g.path.display(),
                g.bytes,
                g.sha256.get(..12).unwrap_or(&g.sha256),
                g.now.describe()
            );
        }
        outln!("{}", ctx.dim("nothing was changed"));
        return exit::OK;
    }
    if !changed.is_empty() {
        if !force {
            eprintln!(
                "{} cannot give back for {}: {} path{} changed since the rollback; putting the tin's bytes back would overwrite that newer content",
                ctx.red("error[E704 burn]:"),
                rec.id,
                changed.len(),
                if changed.len() == 1 { "" } else { "s" }
            );
            for g in &changed {
                eprintln!(
                    "  - {}: the rollback left {}, now {}",
                    g.path.display(),
                    g.left
                        .as_ref()
                        .map(|l| l.describe())
                        .unwrap_or_else(|| "something else".to_string()),
                    g.now.describe()
                );
            }
            eprintln!(
                "  = hint: look at the files, then pass --force if the tin's bytes are what you want; cig unburn {} --give-back --dry-run shows the changed-since column",
                rec.id
            );
            eprintln!("  = explain: cig explain E704");
            return exit::SCRIPT_ERROR;
        }
        eprintln!(
            "{} {} path(s) changed since the rollback; --force given, giving back over them",
            ctx.yellow("note:"),
            changed.len()
        );
    }
    let _lock = match crate::burn::lock::acquire(&rec.pack, &rec.id) {
        Ok(l) => Some(l),
        Err(crate::burn::lock::Busy::Held { root, holder }) => {
            eprintln!(
                "{} the pack {} is in use by {holder}",
                ctx.red("error[E759 burn]:"),
                root.display()
            );
            eprintln!(
                "  = hint: wait for that run to finish (cig runs shows it); nothing was changed"
            );
            eprintln!("  = explain: cig explain E759");
            return exit::SCRIPT_ERROR;
        }
        Err(crate::burn::lock::Busy::Io(_)) => None,
    };
    let report = journal.give_back();
    if ctx.json {
        outln!("{}", serde_json::to_string(&report).unwrap_or_default());
    } else {
        for (label, text) in &report.actions {
            outln!("  {} {text}", paint_action(ctx, label));
        }
    }
    let mut rec = rec;
    if !journal.wrote_nothing() {
        rec.journal_head = Some(journal.head().to_string());
    }
    let _ = crate::burn::ledger::append(
        "unburn",
        &rec.id,
        journal.head(),
        "given_back",
        journal.chain().lines as u64,
    );
    if let Err(e) = rec.save() {
        eprintln!(
            "{} could not record the give-back in {}: {e}",
            ctx.yellow("warning:"),
            rec.dir().display()
        );
    }
    if report.clean() {
        exit::OK
    } else {
        exit::SCRIPT_ERROR
    }
}

/// A run record removed, with what it holds: the git blobs it pinned, its
/// layers, and a ledger line, so the missing-run note never fires for it.
/// `how` is written into the ledger line's `status` field: `pruned` for
/// `cig runs --prune` and the keep_runs/keep_days retention, `budget` when
/// history_budget made the choice (#149), so `cig runs --pruned` can say
/// why each went.
fn remove_run(r: &runs::RunRecord, how: &str) -> bool {
    // The tin key's lock before anything goes: the ledger line that says
    // why this run went is written under it, so a run is never removed
    // without its line (#105, SEC-004 U2).
    if crate::burn::seal::hold_shared_rekey_lock().is_err() {
        return false;
    }
    if let Ok(text) = std::fs::read_to_string(r.dir().join(crate::burn::git::PINS)) {
        for wt in text.lines().filter(|l| !l.trim().is_empty()) {
            crate::burn::git::unpin(std::path::Path::new(wt.trim()), &r.id);
        }
    }
    #[cfg(target_os = "linux")]
    {
        let _ = crate::burn::overlay::remove_tree_force(&r.dir().join("box"));
        crate::burn::overlay::sweep_scratch(&r.dir());
    }
    if crate::burn::overlay::remove_tree_force(&r.dir()).is_ok() {
        let _ = crate::burn::ledger::append("pruned", &r.id, "", how, 0);
        // The run folder is gone: drop it from the index too (#149),
        // so the missing-run note never fires against this id and the
        // next `cig runs` does not see it.
        crate::burn::runs_index::remove(&r.id);
        return true;
    }
    false
}

/// What the retention settings drop (#105, L4): past the newest
/// `keep_runs`, or started more than `keep_days` ago; 0 keeps them all. A
/// run with work left (running, interrupted, or an unburn that failed) is
/// never dropped by them, however old.
pub fn retained_out(
    all: &[runs::RunRecord],
    keep_runs: usize,
    keep_days: u64,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<usize> {
    let cutoff = (keep_days > 0).then(|| now - chrono::Duration::days(keep_days as i64));
    all.iter()
        .enumerate()
        .filter(|(i, r)| {
            let too_many = keep_runs > 0 && *i >= keep_runs;
            let too_old = cutoff.is_some_and(|c| {
                chrono::DateTime::parse_from_rfc3339(&r.started).is_ok_and(|s| s < c)
            });
            let pending = matches!(
                r.status.as_str(),
                "running" | "interrupted" | "unburn_failed"
            );
            (too_many || too_old) && !pending && !is_pinned(r)
        })
        .map(|(i, _)| i)
        .collect()
}

/// After a run: `keep_runs` and `keep_days`, applied as `cig runs --prune`
/// prunes. Nothing is read when both are 0.
///
/// (#178) When `keep_days` is off, the live count is read cheaply from
/// the index first; if it does not exceed `keep_runs`, `list()` is not
/// called at all. Pruning the retained-out set still walks the records,
/// but only when there is work to do, and the index is compacted once
/// at the end of a prune batch instead of one `rm` line per run.
pub fn keep_by_config() {
    let cfg = crate::config::Config::load();
    let keep_runs: usize = cfg.get("keep_runs").trim().parse().unwrap_or(0);
    let keep_days: u64 = cfg.get("keep_days").trim().parse().unwrap_or(0);
    if keep_runs == 0 && keep_days == 0 {
        return;
    }
    // The cheap path (#178): with keep_days off, the index already
    // knows how many runs are live. No list walk, no stat, no record
    // re-read. keep_days needs a per-run started comparison, so the
    // slow path is kept when it is set.
    if keep_days == 0 && keep_runs > 0 {
        if let Some(live) = crate::burn::runs_index::live_count() {
            if live <= keep_runs {
                return;
            }
        }
    }
    let Ok(all) = runs::list() else {
        return;
    };
    let mut pruned = 0usize;
    for i in retained_out(&all, keep_runs, keep_days, chrono::Utc::now()) {
        if remove_run(&all[i], "pruned") {
            pruned += 1;
        }
    }
    if pruned > 0 {
        let _ = crate::burn::runs_index::compact();
    }
}

/// The bytes of every regular file under `dir`.
fn dir_bytes(dir: &std::path::Path) -> u64 {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

/// Every object in the tin with its size: plain objects, recipes and sealed
/// ones, which the collector removes alike.
fn tin_objects() -> Vec<(std::path::PathBuf, u64)> {
    crate::burn::tin::objects()
        .into_iter()
        .chain(crate::burn::tin::recipes())
        .chain(crate::burn::tin::sealed())
        .collect()
}

/// The history's size on disk (#149): every run's folder and every object
/// in the tin, which is what grows by a few MB a run.
pub fn history_bytes() -> u64 {
    dir_bytes(&runs::runs_dir()) + tin_objects().iter().map(|(_, n)| n).sum::<u64>()
}

/// A run holding a box layer that waits for a person's approval
/// (`kept/<seq>.json` beside its `box/<seq>`): pruning it would take the
/// layer, so the budget never does.
fn holds_a_layer(r: &runs::RunRecord) -> bool {
    let dir = r.dir();
    std::fs::read_dir(dir.join("kept"))
        .map(|rd| {
            rd.flatten().any(|e| {
                e.path()
                    .file_stem()
                    .is_some_and(|seq| dir.join("box").join(seq).is_dir())
            })
        })
        .unwrap_or(false)
}

/// `history_budget` (#149), off by default; a run starts it in the background (`budget_in_background`): after a run, and after
/// `cig runs --prune`, the oldest runs go until the history (runs and tin)
/// fits, then the tin is collected. Never pruned: a pinned run, a run with
/// work left (running, interrupted, an unburn that failed), a run holding a
/// box layer that waits for approval, a run younger than `history_keep`,
/// and the newest run; the budget can be exceeded for those, and `cig
/// doctor` says so. An object's bytes count as freed only when the last run
/// that refers to it goes, so the pass prunes no more than it must. Each
/// prune is a ledger line with status `budget`, so `cig runs --pruned` says
/// why it went.
pub fn budget_in_background() {
    let cfg = crate::config::Config::load();
    if !matches!(
        crate::config::parse_bytes(&cfg.get("history_budget")),
        Some(Some(_))
    ) {
        return;
    }
    // The tests that read the result straight after a run ask for it here.
    if std::env::var_os("CIG_BUDGET_SYNC").is_some() {
        budget_pass();
        return;
    }
    // Marked as cig's call to itself: a program's binary is `cig` for it.
    let Ok(mut c) = crate::program::self_command() else {
        return;
    };
    c.arg("__budget")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    let _ = c.spawn();
}

/// One pass of the budget under its own lock: a second pass that finds the
/// first still running leaves it to finish.
pub fn budget_pass() {
    let Ok(Some(_held)) = crate::burn::lock::try_lock_file(&runs::home_dir().join("budget.lock"))
    else {
        return;
    };
    budget_by_config();
}

pub fn budget_by_config() {
    let cfg = crate::config::Config::load();
    let Some(Some(budget)) = crate::config::parse_bytes(&cfg.get("history_budget")) else {
        return;
    };
    let floor = crate::config::parse_history_keep(&cfg.get("history_keep")).unwrap_or_default();
    let objects = tin_objects();
    let tin_total: u64 = objects.iter().map(|(_, n)| n).sum();
    let runs_total = dir_bytes(&runs::runs_dir());
    if runs_total + tin_total <= budget {
        return;
    }
    let Ok(all) = runs::list() else {
        return;
    };
    // Who refers to each object, counted once per run; what another tool
    // keeps (`cig tin keep`) is never freed here.
    let refs: Vec<std::collections::HashSet<std::path::PathBuf>> =
        all.iter().map(crate::burn::tin::referenced_by).collect();
    let mut holders: std::collections::HashMap<&std::path::PathBuf, usize> =
        std::collections::HashMap::new();
    for set in &refs {
        for p in set {
            *holders.entry(p).or_default() += 1;
        }
    }
    let kept: std::collections::HashSet<std::path::PathBuf> = crate::burn::tin::kept_copies()
        .into_iter()
        .map(|k| k.object)
        .collect();
    let size: std::collections::HashMap<&std::path::PathBuf, u64> =
        objects.iter().map(|(p, n)| (p, *n)).collect();
    // What the collector frees already: objects nobody refers to.
    let dead: u64 = objects
        .iter()
        .filter(|(p, _)| !holders.contains_key(p) && !kept.contains(p))
        .map(|(_, n)| n)
        .sum();
    let mut total = runs_total + tin_total - dead;
    let now = chrono::Utc::now();
    let mut pruned = 0;
    // `list` is newest first: walk it oldest first, and leave the newest,
    // the run whose end fired this pass.
    for i in (1..all.len()).rev() {
        if total <= budget {
            break;
        }
        let r = &all[i];
        let young = floor.is_some_and(|keep| {
            chrono::DateTime::parse_from_rfc3339(&r.started)
                .is_ok_and(|s| now - s.with_timezone(&chrono::Utc) < keep)
        });
        if young
            || is_pinned(r)
            || holds_a_layer(r)
            || matches!(
                r.status.as_str(),
                "running" | "interrupted" | "unburn_failed"
            )
        {
            continue;
        }
        let own = dir_bytes(&r.dir());
        if !remove_run(r, "budget") {
            continue;
        }
        pruned += 1;
        let mut freed = own;
        for p in &refs[i] {
            if let Some(n) = holders.get_mut(p) {
                *n -= 1;
                if *n == 0 && !kept.contains(p) {
                    freed += size.get(p).copied().unwrap_or(0);
                }
            }
        }
        total = total.saturating_sub(freed);
    }
    if pruned > 0 || dead > 0 {
        crate::burn::tin::gc(false);
    }
    if pruned > 0 {
        let _ = crate::burn::runs_index::compact();
    }
}

fn prune_runs(ctx: &Ctx, keep: usize) -> i32 {
    if let Err(e) = crate::burn::seal::hold_shared_rekey_lock() {
        super::run::emit_lock_or_strict_error(ctx, &e);
        return exit::SCRIPT_ERROR;
    }
    let all = match runs::list() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{} cannot list runs: {e}", ctx.red("error:"));
            return exit::USAGE;
        }
    };
    let mut removed = 0;
    let mut pinned = 0;
    for r in all.iter().skip(keep) {
        if is_pinned(r) {
            pinned += 1;
            continue;
        }
        if remove_run(r, "pruned") {
            removed += 1;
        }
    }
    let kept = all.len().min(keep) + pinned;
    if ctx.json {
        outln!(
            "{}",
            serde_json::json!({ "removed": removed, "kept": kept, "pinned": pinned })
        );
    } else {
        eprintln!(
            "removed {removed} run record{}, kept {kept}{}",
            if removed == 1 { "" } else { "s" },
            if pinned > 0 {
                format!(" ({pinned} pinned)")
            } else {
                String::new()
            }
        );
    }
    // The budget runs here too (#149), in case a manual --prune left more
    // than it allows: ignored when off, which is the default.
    budget_by_config();
    // One compaction for the whole batch (#178) instead of N rm lines.
    if removed > 0 {
        let _ = crate::burn::runs_index::compact();
    }
    exit::OK
}

/// The marker a pinned run's folder holds: moved to `burn::runs` so the
/// run index (#149) can see it without pulling in the CLI.
pub use crate::burn::runs::PIN_FILE;

pub fn is_pinned(r: &runs::RunRecord) -> bool {
    r.dir().join(PIN_FILE).exists()
}

/// `cig runs --pin <id>` and `--unpin <id>`.
pub fn pin(ctx: &Ctx, id: &str, on: bool) -> i32 {
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
            eprintln!("  = explain: cig explain E803");
            return exit::USAGE;
        }
    };
    let marker = rec.dir().join(PIN_FILE);
    let done = if on {
        std::fs::write(&marker, format!("{}\n", chrono::Utc::now().to_rfc3339()))
    } else {
        match std::fs::remove_file(&marker) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    };
    if let Err(e) = done {
        eprintln!(
            "{} cannot {} {}: {e}",
            ctx.red("error[E802 usage]:"),
            if on { "pin" } else { "unpin" },
            rec.id
        );
        return exit::USAGE;
    }
    // Keep the run index's `pinned` flag in step (#149): the next
    // `cig runs` reads it without opening every run folder to check.
    crate::burn::runs_index::upsert(&rec);
    if ctx.json {
        outln!("{}", serde_json::json!({"run": rec.id, "pinned": on}));
    } else if on {
        eprintln!(
            "{} {} is pinned: keep_runs, keep_days and --prune keep it (cig runs --unpin {} lets it go)",
            ctx.green("ok:"),
            rec.id,
            rec.id
        );
    } else {
        eprintln!("{} {} is no longer pinned", ctx.green("ok:"), rec.id);
    }
    exit::OK
}

/// `cig runs --pruned` (#149): the runs whose folders went on purpose, from
/// the ledger, newest first: when, how (`pruned` by --prune or the
/// retention settings, `budget` by history_budget, `forgotten` by
/// --forget), and the status the ledger last recorded for each. A missing
/// run is explained, not mysterious.
pub fn pruned(ctx: &Ctx) -> i32 {
    let ledger = match crate::burn::ledger::load() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{} the ledger cannot be read here: {e}", ctx.red("error:"));
            return exit::SCRIPT_ERROR;
        }
    };
    let lines: Vec<&crate::burn::ledger::Line> = ledger.of_this_tin().collect();
    let gone: Vec<&crate::burn::ledger::Line> = lines
        .iter()
        .rev()
        .filter(|l| l.kind == "pruned" || l.kind == "forgotten")
        .copied()
        .collect();
    // What each run was, as the ledger last recorded it before it went.
    let was = |run: &str| {
        lines
            .iter()
            .rev()
            .find(|l| l.run == run && (l.kind == "run" || l.kind == "unburn"))
            .map(|l| l.status.clone())
            .unwrap_or_else(|| "-".to_string())
    };
    // A budget prune is a pruned line with status = `budget`, so --pruned
    // says why; the hand-forget and the ordinary prune stay as their kinds.
    fn how(l: &crate::burn::ledger::Line) -> &str {
        if l.kind == "pruned" && l.status == "budget" {
            "budget"
        } else {
            l.kind.as_str()
        }
    }
    if ctx.json {
        let v: Vec<serde_json::Value> = gone
            .iter()
            .map(
                |l| serde_json::json!({"run": l.run, "at": l.t, "how": how(l), "was": was(&l.run)}),
            )
            .collect();
        outln!("{}", serde_json::to_string(&v).unwrap_or_default());
        return exit::OK;
    }
    if gone.is_empty() {
        eprintln!("no run has been pruned or forgotten");
        return exit::OK;
    }
    outln!("{:<24} {:<26} {:<10} {}", "run", "at", "how", "was");
    for l in gone {
        outln!("{:<24} {:<26} {:<10} {}", l.run, l.t, how(l), was(&l.run));
    }
    exit::OK
}

/// `cig runs --reindex` (#178): rebuild the run index from the run
/// folders. `cig runs` rebuilds the index on its own when a stat
/// mismatches or the file is torn; this is the manual path for an
/// index that has grown far past its live view (`cig doctor` flags
/// that), or after a hand-edit of `runs-index.jsonl`.
pub fn reindex(ctx: &Ctx) -> i32 {
    match crate::burn::runs_index::rebuild_from_folders() {
        Ok(n) => {
            if ctx.json {
                outln!("{}", serde_json::json!({ "reindexed": true, "live": n }));
            } else {
                eprintln!(
                    "{} the run index now holds {n} live run{}",
                    ctx.green("ok:"),
                    if n == 1 { "" } else { "s" }
                );
            }
            exit::OK
        }
        Err(e) => {
            eprintln!("{} could not rebuild the run index: {e}", ctx.red("error:"));
            exit::SCRIPT_ERROR
        }
    }
}

// ----- a stretch of history (#149) -------------------------------------------------

/// Which runs `cig unburn` puts back when it is not given one.
pub enum Stretch {
    /// The newest N runs that changed something.
    Last(usize),
    /// Every run started within this long.
    Since(chrono::Duration),
    /// Every run after this one; it stays.
    To(String),
    /// The stretch that stopped, from its file.
    Continue,
}

pub struct StretchOpts {
    pub dry_run: bool,
    pub force: bool,
    pub accept_unsigned: bool,
    pub yes: bool,
    /// A program's stretch takes its own runs only (PROGRAMS.md): the
    /// sha256 its runs record.
    pub only_program: Option<String>,
}

/// `90s`, `30m`, `2h`, `3d`, `1w`.
pub fn parse_age(s: &str) -> Option<chrono::Duration> {
    let s = s.trim();
    let unit = s.chars().last()?;
    let n: i64 = s[..s.len() - unit.len_utf8()].trim().parse().ok()?;
    if n <= 0 {
        return None;
    }
    match unit {
        's' => chrono::Duration::try_seconds(n),
        'm' => chrono::Duration::try_minutes(n),
        'h' => chrono::Duration::try_hours(n),
        'd' => chrono::Duration::try_days(n),
        'w' => chrono::Duration::try_weeks(n),
        _ => None,
    }
}

/// Where a stretch in progress keeps its runs, so `--continue` takes up the
/// same ones and none is undone twice.
fn stretch_file() -> std::path::PathBuf {
    runs::home_dir().join("unburn-stretch.json")
}

/// The sha256 of the program that made a run, when one did.
fn program_sha(r: &runs::RunRecord) -> Option<&str> {
    r.program.as_ref().and_then(|v| v["sha256"].as_str())
}

/// A run a stretch can put back: one of its own (a child goes back through
/// its parent), that changed something, and is not back already.
fn undoable(r: &runs::RunRecord) -> bool {
    r.parent.is_none() && r.burns > 0 && r.status != "unburned" && !r.rolled_back
}

/// Did the run work in `pack`: a pack root it declared is that folder, or
/// inside it, or holds it; or its journal touched a path under it.
fn in_pack(r: &runs::RunRecord, pack: &std::path::Path) -> bool {
    if r.pack
        .iter()
        .any(|p| p.starts_with(pack) || pack.starts_with(p))
    {
        return true;
    }
    Journal::load(&r.dir())
        .map(|j| j.touched_paths().iter().any(|(p, _)| p.starts_with(pack)))
        .unwrap_or(false)
}

/// The runs a stretch takes, as indexes into `all` (newest first), or why
/// it cannot start. A run still running inside the stretch's reach stops
/// it: what it writes next would land on files being put back under it.
pub fn select_stretch(
    all: &[runs::RunRecord],
    how: &Stretch,
    pack: Option<&std::path::Path>,
    own: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<usize>, String> {
    let reach: Vec<usize> = match how {
        Stretch::Last(_) => (0..all.len()).collect(),
        Stretch::Since(d) => {
            let cutoff = now - *d;
            (0..all.len())
                .filter(|&i| {
                    chrono::DateTime::parse_from_rfc3339(&all[i].started).is_ok_and(|s| s >= cutoff)
                })
                .collect()
        }
        Stretch::To(id) => {
            let matching: Vec<usize> = (0..all.len())
                .filter(|&i| all[i].id.starts_with(id.as_str()))
                .collect();
            match matching.as_slice() {
                [one] => (0..*one).collect(),
                [] => return Err(format!("no run matches `{id}`")),
                _ => {
                    return Err(format!(
                        "`{id}` matches {} runs; give more of the id",
                        matching.len()
                    ))
                }
            }
        }
        Stretch::Continue => return Err("a stretch to continue is read from its file".to_string()),
    };
    let mut picked: Vec<usize> = reach
        .iter()
        .copied()
        .filter(|&i| undoable(&all[i]) && pack.is_none_or(|p| in_pack(&all[i], p)))
        .filter(|&i| own.is_none_or(|sha| program_sha(&all[i]) == Some(sha)))
        .collect();
    if let Stretch::Last(n) = how {
        picked.truncate(*n);
    }
    // Everything newer than the oldest run picked is in reach.
    if let Some(&oldest) = picked.last() {
        if let Some(live) = all[..=oldest]
            .iter()
            .find(|r| r.status == "running" && !r.is_interrupted())
        {
            return Err(format!(
                "{} is still running; put a stretch back once it has finished",
                live.id
            ));
        }
    }
    Ok(picked)
}

/// `cig unburn --last N | --since AGE | --to ID | --continue`: one plan,
/// then each run as `cig unburn <id>` would put it back, newest first. The
/// first run refused stops the stretch: the runs after it stay undone, the
/// ones before it untouched, and `--continue` takes up the rest once it is
/// resolved.
pub fn unburn_stretch(
    ctx: &Ctx,
    how: Stretch,
    pack: Option<&std::path::Path>,
    o: StretchOpts,
) -> i32 {
    let usage = |message: String, hint: &str| {
        eprintln!("{} {message}", ctx.red("error[E800 usage]:"));
        eprintln!("  = hint: {hint}");
        eprintln!("  = explain: cig explain E800");
        exit::USAGE
    };
    let all = match runs::list() {
        Ok(r) => r,
        Err(e) => {
            return usage(
                format!("cannot list runs: {e}"),
                "cig doctor looks the state directory over",
            )
        }
    };
    let mut saved_plan: Option<CombinePlan> = None;
    let ids: Vec<String> = if let Stretch::Continue = how {
        let Ok(text) = std::fs::read_to_string(stretch_file()) else {
            return usage(
                "there is no stretch to continue".to_string(),
                "a stretch that stops leaves its runs for --continue; start one with --last, --since or --to",
            );
        };
        let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        saved_plan = serde_json::from_value(v["combine"].clone()).ok();
        v["runs"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    } else {
        let pack = pack.map(|p| {
            std::fs::canonicalize(p)
                .unwrap_or_else(|_| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()))
        });
        match select_stretch(
            &all,
            &how,
            pack.as_deref(),
            o.only_program.as_deref(),
            chrono::Utc::now(),
        ) {
            Ok(picked) => picked.into_iter().map(|i| all[i].id.clone()).collect(),
            Err(e) => return usage(e, "cig runs lists the runs, newest first"),
        }
    };
    // What is still to do, in order; a run back already is done (exactly
    // once, whatever stopped the stretch before).
    let todo: Vec<&runs::RunRecord> = ids
        .iter()
        .filter_map(|id| all.iter().find(|r| &r.id == id))
        .filter(|r| r.status != "unburned" && !r.rolled_back)
        .collect();
    if todo.is_empty() {
        let _ = std::fs::remove_file(stretch_file());
        if ctx.json {
            outln!(
                "{}",
                serde_json::json!({"runs": [], "undone": [], "stopped_at": null})
            );
        } else {
            eprintln!("nothing to undo: no run in that stretch changed anything, or all of them are back already");
        }
        return exit::OK;
    }
    // The plan: how much, and where.
    let mut ops = 0usize;
    let mut files = std::collections::BTreeSet::new();
    let mut irreversible = 0usize;
    for r in &todo {
        if let Ok(j) = Journal::load(&r.dir()) {
            ops += j.len();
            files.extend(j.touched_paths().into_iter().map(|(p, _)| p));
        }
        irreversible += r.irreversible;
    }
    let newest = &todo[0].id;
    let oldest = &todo[todo.len() - 1].id;
    if ctx.json && o.dry_run {
        outln!(
            "{}",
            serde_json::json!({"runs": todo.iter().map(|r| r.id.clone()).collect::<Vec<_>>(), "ops": ops, "files": files.len(), "irreversible": irreversible, "dry_run": true})
        );
        return exit::OK;
    }
    if !ctx.json {
        eprintln!(
            "undo {} run{} ({} -> {}), newest first",
            todo.len(),
            if todo.len() == 1 { "" } else { "s" },
            oldest,
            newest
        );
        eprintln!(
            "  {ops} op{} across {} file{}; {irreversible} irreversible (a hop's own effects, which no unburn puts back)",
            if ops == 1 { "" } else { "s" },
            files.len(),
            if files.len() == 1 { "" } else { "s" }
        );
        for r in &todo {
            eprintln!(
                "  {}  {} burn{}  {}",
                r.id,
                r.burns,
                if r.burns == 1 { "" } else { "s" },
                r.script
            );
        }
    }
    if o.dry_run {
        eprintln!("dry run: nothing changed; each run is checked as it is put back, after the ones newer than it");
        return exit::OK;
    }
    use std::io::IsTerminal;
    if !o.yes && !ctx.json && std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
        eprint!("proceed? [y/N] ");
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        if !matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            eprintln!("nothing changed");
            return exit::OK;
        }
    }
    let ids: Vec<String> = todo.iter().map(|r| r.id.clone()).collect();
    // The combined restore (#149): each file written once, to the earliest
    // before-state in the stretch. Worked out here, once, and kept in the
    // stretch file, so `--continue` after a stop uses the same plan and not
    // one recomputed from the runs that are left.
    let plan: CombinePlan = if let (Stretch::Continue, Some(p)) = (&how, saved_plan.as_ref()) {
        p.clone()
    } else {
        combined_plan(&todo)
    };
    install_plan(&todo, &plan);
    let _ = std::fs::write(
        stretch_file(),
        serde_json::json!({"runs": ids, "made": chrono::Utc::now().to_rfc3339(), "combine": plan})
            .to_string(),
    );
    let mut undone = Vec::new();
    for (k, id) in ids.iter().enumerate() {
        if !ctx.json {
            eprintln!("-- {} ({} of {})", id, k + 1, ids.len());
        }
        let code = unburn(ctx, id, false, o.force, o.accept_unsigned);
        if code != exit::OK {
            if ctx.json {
                outln!(
                    "{}",
                    serde_json::json!({"runs": ids, "undone": undone, "stopped_at": id})
                );
            } else {
                eprintln!(
                    "{} the stretch stopped at {id}: {} of {} undone, {} and the runs before it untouched; once it is resolved, cig unburn --continue carries on",
                    ctx.yellow("stopped:"),
                    undone.len(),
                    ids.len(),
                    id
                );
            }
            crate::burn::journal::set_combined(Vec::new());
            // The runs done so far left files for an older run to put back;
            // that run is not coming yet, so they go back now, as if each
            // had been undone on its own.
            for done in undone.iter().rev() {
                let skipped: std::collections::BTreeSet<std::path::PathBuf> = plan
                    .get(done)
                    .map(|(skip, _)| skip.iter().map(std::path::PathBuf::from).collect())
                    .unwrap_or_default();
                if skipped.is_empty() {
                    continue;
                }
                if let Some(r) = all.iter().find(|r| &r.id == done) {
                    if let Ok(j) = Journal::load(&r.dir()) {
                        for f in j.flush_skipped(&skipped) {
                            eprintln!("{} {f}", ctx.red("error:"));
                        }
                    }
                }
            }
            return code;
        }
        undone.push(id.clone());
    }
    crate::burn::journal::set_combined(Vec::new());
    let _ = std::fs::remove_file(stretch_file());
    if ctx.json {
        outln!(
            "{}",
            serde_json::json!({"runs": ids, "undone": undone, "stopped_at": null})
        );
    } else {
        eprintln!(
            "{} undid {} run{}, newest first",
            ctx.green("ok:"),
            undone.len(),
            if undone.len() == 1 { "" } else { "s" }
        );
    }
    exit::OK
}

/// The combined-restore plan for `todo` (newest first): per run, the paths
/// it does not write back and the paths whose changed-since check it skips.
/// A path is combined only when it is a plain file write or removal in
/// every run of the stretch that touches it, and at least two do: the
/// newest run still checks it, the oldest still writes it, and the runs
/// between do neither.
fn combined_plan(todo: &[&runs::RunRecord]) -> CombinePlan {
    use std::collections::BTreeMap;
    let profiles: Vec<_> = todo
        .iter()
        .map(|r| Journal::load(&r.dir()).ok().map(|j| j.restore_profile()))
        .collect();
    let mut touching: BTreeMap<std::path::PathBuf, Vec<usize>> = BTreeMap::new();
    let mut spoiled: std::collections::BTreeSet<std::path::PathBuf> = Default::default();
    for (k, p) in profiles.iter().enumerate() {
        let Some((plain, other)) = p else {
            continue;
        };
        for path in plain {
            touching.entry(path.clone()).or_default().push(k);
        }
        spoiled.extend(other.iter().cloned());
    }
    // A run whose journal could not be read may touch anything: no combining.
    if profiles.iter().any(|p| p.is_none()) {
        return BTreeMap::new();
    }
    let mut plan: BTreeMap<String, (Vec<String>, Vec<String>)> = BTreeMap::new();
    for (path, runs_at) in touching {
        if runs_at.len() < 2 || spoiled.contains(&path) {
            continue;
        }
        let text = path.to_string_lossy().to_string();
        let last = runs_at.len() - 1;
        for (n, k) in runs_at.iter().enumerate() {
            let e = plan.entry(todo[*k].id.clone()).or_default();
            if n < last {
                e.0.push(text.clone());
            }
            if n > 0 {
                e.1.push(text.clone());
            }
        }
    }
    plan
}

fn install_plan(todo: &[&runs::RunRecord], plan: &CombinePlan) {
    let set = |v: &Vec<String>| -> std::collections::BTreeSet<std::path::PathBuf> {
        v.iter().map(std::path::PathBuf::from).collect()
    };
    crate::burn::journal::set_combined(
        todo.iter()
            .filter_map(|r| plan.get(&r.id).map(|(a, b)| (r.dir(), set(a), set(b))))
            .collect(),
    );
}

/// How many changed paths an E704 refusal names before saying how many more.
const REFUSAL_SHOWS: usize = 20;

/// For each of `paths`, the first later run (in `runs::list` order) whose
/// journal touched it: each journal is read at most once, and the reading
/// stops when every path has a name.
fn later_writers(
    id: &str,
    paths: impl Iterator<Item = std::path::PathBuf>,
) -> std::collections::BTreeMap<std::path::PathBuf, String> {
    let mut want: std::collections::BTreeSet<std::path::PathBuf> = paths.collect();
    let mut found = std::collections::BTreeMap::new();
    for r in runs::list().unwrap_or_default() {
        if want.is_empty() {
            break;
        }
        if r.id.as_str() <= id {
            continue;
        }
        let Ok(j) = Journal::load(&r.dir()) else {
            continue;
        };
        for (p, _) in j.touched_paths() {
            if want.remove(&p) {
                found.insert(p, r.id.clone());
            }
        }
    }
    found
}

pub fn unburn(ctx: &Ctx, id: &str, dry_run: bool, force: bool, accept_unsigned: bool) -> i32 {
    // A rollback writes run state: the state directory must be ours alone.
    if !dry_run {
        if let Err(e) = crate::burn::ensure_private() {
            eprintln!("{} {e}", ctx.red("error[E707 burn]:"));
            eprintln!("  = explain: cig explain E707");
            return exit::SCRIPT_ERROR;
        }
    }
    let mut rec = match runs::find(id) {
        Ok(Some(r)) => r,
        Ok(None) => {
            eprintln!("{} no run matches `{id}`", ctx.red("error[E803 usage]:"));
            eprintln!("  = hint: `cig runs` lists them; give more of the id if it is ambiguous");
            eprintln!("  = explain: cig explain E803");
            return exit::USAGE;
        }
        Err(e) => {
            eprintln!("{} {e}", ctx.red("error[E803 usage]:"));
            eprintln!("  = explain: cig explain E803");
            return exit::USAGE;
        }
    };
    let mut journal = match Journal::load(&rec.dir()) {
        Ok(j) => j,
        Err(e) if e.kind() == std::io::ErrorKind::Unsupported || rec.status == "newer" => {
            return hand_back(ctx, &rec, &e);
        }
        Err(e) => {
            eprintln!(
                "{} cannot load the journal for {}: {e}",
                ctx.red("error[E703 burn]:"),
                rec.id
            );
            eprintln!(
                "  = hint: a line before the last is damaged; cig report {} bundles the journal for a bug report",
                rec.id
            );
            eprintln!("  = explain: cig explain E703");
            return exit::SCRIPT_ERROR;
        }
    };
    if journal.truncated() {
        eprintln!(
            "{} the journal's last line was cut short (a crash while appending); the {} entr{} before it loaded and can be restored",
            ctx.yellow("note:"),
            journal.len(),
            if journal.len() == 1 { "y" } else { "ies" }
        );
    }
    if journal.is_empty() {
        eprintln!("{} burned nothing; there is nothing to unburn", rec.id);
        return exit::OK;
    }
    if journal.chain().unverified {
        eprintln!(
            "{} this run's journal is signed and the tin's key is not available to check it; restoring by the hashes it carries",
            ctx.yellow("note:")
        );
    }
    // A signed journal under another key (another machine's, or one since
    // replaced) is not tampering: its objects still verify by hash, and
    // `--accept-unsigned` restores from it after you have read that.
    if journal.chain().other_key {
        if !accept_unsigned && !dry_run {
            eprintln!(
                "{} this run was signed by another key; its files still match their hashes",
                ctx.red("error[E708 burn]:")
            );
            eprintln!(
                "  = hint: cig unburn {} --accept-unsigned restores it anyway (sealed objects still need the key that sealed them: cig tin key import)",
                rec.id
            );
            eprintln!("  = explain: cig explain E708");
            return exit::SCRIPT_ERROR;
        }
        eprintln!(
            "{} this run was signed by another key; restoring from it as its hashes allow",
            ctx.yellow("note:")
        );
    }
    // The ledger recorded where this run's journal ended: a journal cut
    // short, or an older copy swapped in, is caught here.
    if let Some(reason) = ledger_disagrees(&rec, &journal) {
        // A journal the ledger says was cut short or swapped is a tripwire
        // too (#105, L6): reported like any other, under the person's
        // `reports`, with no path in it. A dry run reports nothing.
        if !dry_run {
            super::run::settle_trips(
                ctx,
                vec![crate::tripwire::Trip {
                    check: "ledger",
                    what: format!("the ledger disagrees with a run's journal: {reason}"),
                    details: serde_json::json!({
                        "journal_lines": journal.chain().lines,
                        "said": reason,
                    }),
                }],
                &rec,
                &journal,
            );
        }
        if !force && !dry_run {
            eprintln!("{} {reason}", ctx.red("error[E709 burn]:"));
            eprintln!("  = hint: the record was changed since it was written; --force restores from it as it is");
            eprintln!("  = explain: cig explain E709");
            return exit::SCRIPT_ERROR;
        }
        eprintln!(
            "{} {reason}; {}",
            ctx.yellow("note:"),
            if dry_run {
                "a real unburn would refuse [E709] unless --force"
            } else {
                "--force given"
            }
        );
    }
    // Every line of a format 2 journal carries the hash of the one before
    // it: a journal that no longer fits itself is not the record, and is
    // restored from only on purpose.
    if let Some((line, reason)) = tampered(&rec, &journal) {
        if !force && !dry_run {
            eprintln!(
                "{} the journal of {} is not the record the run wrote: line {line} {reason}",
                ctx.red("error[E760 burn]:"),
                rec.id,
            );
            eprintln!(
                "  = hint: cig runs {} --verify walks the chain; pass --force to restore from the journal as it is, knowing it was changed",
                rec.id
            );
            eprintln!("  = explain: cig explain E760");
            return exit::SCRIPT_ERROR;
        }
        if dry_run {
            outln!(
                "  {} the journal's chain is broken at line {line} ({reason}); a real unburn would refuse [E760] unless --force",
                ctx.yellow("note:"),
            );
        } else {
            eprintln!(
                "{} the journal's chain is broken at line {line} ({reason}); --force given",
                ctx.yellow("note:"),
            );
        }
    }
    // A rollback that left restores failed is finished by a retry: the
    // entries restored the first time are skipped, the failed ones tried
    // again. `--force` on such a run is the same retry with the checks
    // overridden: it never redoes what a pass already put back, which an
    // older run's own unburn may have rewound since (#132). `--force` on a
    // run that was rolled back cleanly still restores everything again.
    let retry = rec.rolled_back && !rec.rollback_failed.is_empty();
    if (rec.status == "unburned" || rec.rolled_back) && !dry_run && !retry {
        if !force {
            eprintln!(
                "{} {} was already rolled back; doing it again would overwrite whatever happened since",
                ctx.red("error[E705 burn]:"),
                rec.id
            );
            eprintln!("  = hint: pass --force only if you mean to restore the old snapshots again");
            eprintln!("  = explain: cig explain E705");
            return exit::SCRIPT_ERROR;
        }
        eprintln!(
            "{} {} was already rolled back; --force given",
            ctx.yellow("note:"),
            rec.id
        );
    }
    if retry && !dry_run {
        eprintln!(
            "{} {} restore{} failed last time; trying {} again and leaving the rest as it is",
            ctx.yellow("note:"),
            rec.rollback_failed.len(),
            if rec.rollback_failed.len() == 1 {
                ""
            } else {
                "s"
            },
            if rec.rollback_failed.len() == 1 {
                "it"
            } else {
                "them"
            }
        );
    }
    if !dry_run {
        // The record must be writable before anything is restored: without
        // it a second unburn could not be refused.
        if let Err(e) = rec.save() {
            if !force {
                eprintln!(
                    "{} cannot record the rollback in {}: {e}",
                    ctx.red("error[E802 usage]:"),
                    rec.dir().display()
                );
                eprintln!("  = hint: fix the permissions on that directory first, or pass --force to restore without a record; nothing was restored");
                eprintln!("  = explain: cig explain E802");
                return exit::USAGE;
            }
            eprintln!(
                "{} cannot record the rollback in {}: {e}; --force given",
                ctx.yellow("note:"),
                rec.dir().display()
            );
        }
    }
    // A run using this pack now would have the restore read as its hop's
    // writes; the run holds the lock until it is over.
    let _lock = if dry_run {
        None
    } else {
        match crate::burn::lock::acquire(&rec.pack, &rec.id) {
            Ok(l) => Some(l),
            Err(crate::burn::lock::Busy::Held { root, holder }) => {
                eprintln!(
                    "{} the pack {} is in use by {holder}",
                    ctx.red("error[E759 burn]:"),
                    root.display()
                );
                eprintln!("  = hint: wait for that run to finish (cig runs shows it), then unburn; nothing was changed");
                eprintln!("  = explain: cig explain E759");
                return exit::SCRIPT_ERROR;
            }
            Err(crate::burn::lock::Busy::Io(_)) => None,
        }
    };
    let problems = journal.verify();
    // A retry asks only about the paths it will touch: what the first pass
    // put back may have been edited since, on purpose.
    let changed = if retry {
        journal.changed_since_pending()
    } else {
        journal.changed_since()
    };
    // Who changed each path since: a later run that touched it. Worked out
    // once, when first asked, reading each later run's journal at most once
    // and stopping when every changed path is named: it was read again for
    // every path, minutes on a long history (#128).
    let writers: std::cell::OnceCell<std::collections::BTreeMap<std::path::PathBuf, String>> =
        std::cell::OnceCell::new();
    let by_run = |path: &std::path::Path| -> Option<String> {
        writers
            .get_or_init(|| later_writers(&rec.id, changed.changes.iter().map(|c| c.path.clone())))
            .get(path)
            .cloned()
    };
    if changed.known && !changed.changes.is_empty() && !dry_run {
        if !force {
            eprintln!(
                "{} cannot roll back {}: {} path{} changed since the run finished; restoring would overwrite that newer content",
                ctx.red("error[E704 burn]:"),
                rec.id,
                changed.changes.len(),
                if changed.changes.len() == 1 { "" } else { "s" }
            );
            // One changed path is enough to refuse; a long list says how
            // many, and --dry-run shows them all.
            for c in changed.changes.iter().take(REFUSAL_SHOWS) {
                let who = by_run(&c.path)
                    .map(|id| format!("; changed by run {id}, unburn that one first"))
                    .unwrap_or_default();
                eprintln!(
                    "  - {}: the run left {}, now {}{who}",
                    c.path.display(),
                    c.was.describe(),
                    c.now.describe()
                );
            }
            if changed.changes.len() > REFUSAL_SHOWS {
                eprintln!(
                    "  - and {} more; cig unburn {} --dry-run lists them all",
                    changed.changes.len() - REFUSAL_SHOWS,
                    rec.id
                );
            }
            eprintln!("  = hint: look at the files, then pass --force if the old content is what you want; cig unburn {} --dry-run shows the changed-since column", rec.id);
            eprintln!("  = explain: cig explain E704");
            return exit::SCRIPT_ERROR;
        }
        eprintln!(
            "{} {} path(s) changed since the run; --force given, restoring over them",
            ctx.yellow("note:"),
            changed.changes.len()
        );
    }
    if !problems.is_empty() && !dry_run {
        if !force {
            eprintln!(
                "{} cannot roll back {}: {} problem{} found before touching anything",
                ctx.red("error[E703 burn]:"),
                rec.id,
                problems.len(),
                if problems.len() == 1 { "" } else { "s" }
            );
            for p in &problems {
                eprintln!("  - {p}");
            }
            eprintln!("  = hint: nothing was restored; pass --force to restore what can be and list what cannot");
            eprintln!("  = explain: cig explain E703");
            return exit::SCRIPT_ERROR;
        }
        eprintln!(
            "{} {} problem(s) with the journal; --force given, restoring what can be",
            ctx.yellow("note:"),
            problems.len()
        );
    }
    if dry_run {
        for p in &problems {
            outln!("  {} {p}", ctx.yellow("problem:"));
        }
        if !problems.is_empty() {
            outln!(
                "  {} a real unburn would refuse [E703]; --force restores what it can",
                ctx.yellow("note:")
            );
        }
        outln!(
            "{}",
            ctx.bold(&format!(
                "would restore {} entr{} from {}",
                journal.len(),
                if journal.len() == 1 { "y" } else { "ies" },
                rec.id
            ))
        );
        if !changed.known {
            outln!(
                "  {} this run predates after-state records; changed-since is unknown",
                ctx.dim("note:")
            );
        } else if !changed.changes.is_empty() {
            outln!(
                "  {} {} path{} changed since the run finished; a real unburn would refuse [E704] unless --force",
                ctx.yellow("note:"),
                changed.changes.len(),
                if changed.changes.len() == 1 { "" } else { "s" }
            );
        }
        for e in journal.entries().iter().rev() {
            let tag = if e.compensation.is_some() {
                "compensate"
            } else if e.reversible {
                "restore "
            } else if e.compensated {
                "compensated"
            } else {
                "cannot undo"
            };
            let touched: Vec<&std::path::PathBuf> = e
                .before
                .iter()
                .flat_map(|b| {
                    let mut v = vec![&b.path];
                    if let Before::Moved { to } = &b.before {
                        v.push(to);
                    }
                    v
                })
                .collect();
            let flagged: Vec<String> = changed
                .changes
                .iter()
                .filter(|c| touched.contains(&&c.path))
                .map(|c| {
                    let who = by_run(&c.path)
                        .map(|id| format!(" by run {id}"))
                        .unwrap_or_default();
                    format!("{}{who}", c.path.display())
                })
                .collect();
            let column = if !changed.known {
                ctx.dim("  changed since: unknown")
            } else if flagged.is_empty() {
                ctx.dim("  unchanged since")
            } else {
                ctx.yellow(&format!("  changed since: {}", flagged.join(", ")))
            };
            outln!("  {:>4}  {tag}  {}{column}", e.seq, e.op.describe());
        }
        return if problems.is_empty() {
            exit::OK
        } else {
            exit::SCRIPT_ERROR
        };
    }
    // Compensations run in a fresh interpreter, effects allowed, nothing
    // journaled: they are the undo. They run from the run's directory, and a
    // command among them (a hop's own undo) sees this run's id, so a cig it
    // starts may enter the pack this unburn holds.
    std::env::set_var("CIG_RUN_ID", &rec.id);
    let kernel = crate::burn::Kernel::ephemeral(crate::burn::Mode::Run);
    let mut interp = crate::interp::Interp::new(kernel);
    // The compensation sees the run's own arguments, as it did in the run.
    interp.set_args(&rec.args);
    let mut entered = true;
    if rec.cwd.is_empty() {
        eprintln!(
            "{} the run's working directory is not recorded; relative paths are taken from here",
            ctx.yellow("note:")
        );
    } else if let Ok(dir) = std::env::current_dir() {
        if rec.cwd != dir.to_string_lossy() && std::env::set_current_dir(&rec.cwd).is_err() {
            entered = false;
        }
    }
    let mut runner = |c: &crate::burn::journal::Compensation| -> Result<(), String> {
        if !entered {
            return Err(format!("cannot enter {} to run it", rec.cwd));
        }
        interp.run_compensation(c).map_err(|e| e.message)
    };
    let mut report = if force && !retry {
        journal.rollback_again_with(&mut runner)
    } else {
        journal.rollback_with(&mut runner)
    };
    if !entered && !report.actions.is_empty() {
        report.failed.push(format!("could not enter {}", rec.cwd));
    }
    if ctx.json {
        outln!("{}", serde_json::to_string(&report).unwrap_or_default());
    } else {
        for (label, text) in &report.actions {
            outln!("  {} {text}", paint_action(ctx, label));
        }
    }
    rec.rolled_back = true;
    rec.rollback_failed = report.failed.clone();
    if let Err(e) = crate::burn::ledger::append(
        "unburn",
        &rec.id,
        journal.head(),
        if report.clean() {
            "unburned"
        } else {
            "unburn_failed"
        },
        journal.chain().lines as u64,
    ) {
        eprintln!("{} could not write the ledger: {e}", ctx.yellow("warning:"));
    }
    // The layers of a boxed hop the run never got to apply (it died in the
    // hop) were never the pack's: they go with the rollback.
    let _ = crate::burn::overlay::remove_tree_force(&rec.dir().join("box"));
    #[cfg(target_os = "linux")]
    crate::burn::overlay::sweep_scratch(&rec.dir());
    let _ = crate::burn::overlay::remove_tree_force(&crate::burn::overlay::inbox_dir(&rec.dir()));
    rec.rollback_took = report.took.len();
    if !journal.wrote_nothing() {
        rec.journal_head = Some(journal.head().to_string());
    }
    rec.status = if report.clean() {
        "unburned".to_string()
    } else {
        "unburn_failed".to_string()
    };
    if let Err(e) = rec.save() {
        eprintln!(
            "{} could not record the rollback in {}: {e}",
            ctx.yellow("warning:"),
            rec.dir().display()
        );
    }
    // The tripwires, as after a run's own rollback: every restored path
    // read back against the journal, a sample of the tin re-hashed. A
    // broken promise is a report; the unburn's outcome is what it was.
    let mut trips = Vec::new();
    trips.extend(crate::tripwire::after_rollback(&journal, &report));
    trips.extend(crate::tripwire::tin_sample(&journal));
    super::run::settle_trips(ctx, trips, &rec, &journal);
    drop(journal);
    crate::burn::pack::settle(&rec.dir());
    if report.clean() {
        exit::OK
    } else {
        exit::SCRIPT_ERROR
    }
}

/// A run journaled by a newer cig is put back by that cig: the whole
/// `unburn` is handed to its version from the humidor, or refused with
/// the command that fetches it. Nothing is parsed by guess.
fn hand_back(ctx: &Ctx, rec: &RunRecord, e: &std::io::Error) -> i32 {
    let wrote = rec.written_by();
    // A program's command line is not cig's: never hand it to another cig.
    if crate::program::running() {
        eprintln!("{} {e}", ctx.red("error[E808 usage]:"));
        eprintln!(
            "  = hint: cig unburn {} with the cig that journaled it puts it back",
            rec.id
        );
        eprintln!("  = explain: cig explain E808");
        return exit::USAGE;
    }
    if crate::pin::dispatched() {
        eprintln!("{} {e}", ctx.red("error[E808 usage]:"));
        eprintln!("  = explain: cig explain E808");
        return exit::USAGE;
    }
    if let Some(v) = &wrote {
        if let Some(bin) = crate::humidor::binary(v) {
            if !ctx.json {
                eprintln!(
                    "{}",
                    ctx.dim(&format!(
                        "unburn handed to cig {v}, which wrote this journal; installed is {}",
                        crate::VERSION
                    ))
                );
            }
            let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
            crate::humidor::touch(v);
            return match crate::humidor::dispatch(&bin, &args) {
                Ok(code) => code,
                Err(e) => {
                    eprintln!("{} {e}", ctx.red("error[E808 usage]:"));
                    exit::USAGE
                }
            };
        }
    }
    eprintln!("{} {e}", ctx.red("error[E808 usage]:"));
    match wrote {
        Some(v) => eprintln!(
            "  = hint: cig versions add {v} puts that cig in the humidor; cig unburn {} then hands the run to it",
            rec.id
        ),
        None => eprintln!("  = hint: the record does not say which cig wrote it; ciginstaller update to the newest release and try again"),
    }
    eprintln!("  = explain: cig explain E808");
    exit::USAGE
}

/// The colour for a rollback action's label.
pub fn paint_action(ctx: &Ctx, label: &str) -> String {
    match label {
        "restored" | "compensated" | "gave back" => ctx.green(label),
        "cannot undo" | "kept" | "left" | "not undone" => ctx.yellow(label),
        "failed" => ctx.red(label),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, started: &str, status: &str) -> runs::RunRecord {
        serde_json::from_value(serde_json::json!({
            "id": id, "version": "1.15.1", "script": "t.cig", "script_sha256": "", "args": [],
            "cwd": "/", "mode": "run", "started": started, "status": status,
        }))
        .expect("a record")
    }

    #[test]
    fn retention_drops_the_oldest_and_the_old_but_never_a_run_with_work_left() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-07T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        // Newest first, as runs::list gives them.
        let all = vec![
            record("d", "2026-11-07T11:00:00Z", "ok"),
            record("c", "2026-11-06T11:00:00Z", "ok"),
            record("b", "2026-11-01T11:00:00Z", "interrupted"),
            record("a", "2026-10-01T11:00:00Z", "rolled_back"),
        ];
        assert_eq!(retained_out(&all, 0, 0, now), Vec::<usize>::new());
        assert_eq!(retained_out(&all, 2, 0, now), vec![3], "b has work left");
        assert_eq!(retained_out(&all, 0, 2, now), vec![3]);
        assert_eq!(retained_out(&all, 1, 0, now), vec![1, 3]);
        assert_eq!(retained_out(&all, 0, 60, now), Vec::<usize>::new());
    }

    #[test]
    fn ages_read_as_people_write_them() {
        assert_eq!(parse_age("90s"), chrono::Duration::try_seconds(90));
        assert_eq!(parse_age("30m"), chrono::Duration::try_minutes(30));
        assert_eq!(parse_age(" 2h "), chrono::Duration::try_hours(2));
        assert_eq!(parse_age("3d"), chrono::Duration::try_days(3));
        assert_eq!(parse_age("1w"), chrono::Duration::try_weeks(1));
        for bad in ["", "h", "2", "2y", "-2h", "0h", "2 hours", "1é"] {
            assert_eq!(parse_age(bad), None, "{bad}");
        }
    }

    /// #149: a stretch takes a run's own work only (not a child, not one
    /// that burned nothing, not one back already), newest first, and will
    /// not start under a run that is still running.
    fn select_stretch_t(
        all: &[runs::RunRecord],
        how: &Stretch,
        pack: Option<&std::path::Path>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<usize>, String> {
        select_stretch(all, how, pack, None, now)
    }

    #[test]
    fn a_stretch_takes_the_runs_it_should_and_waits_for_a_live_one() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-08T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let mut all = vec![
            record("20261108T115900-e", "2026-11-08T11:59:00Z", "ok"),
            record("20261108T115800-d", "2026-11-08T11:58:00Z", "ok"),
            record("20261108T115700-c", "2026-11-08T11:57:00Z", "unburned"),
            record("20261108T100000-b", "2026-11-08T10:00:00Z", "error"),
            record("20261107T100000-a", "2026-11-07T10:00:00Z", "ok"),
        ];
        for r in all.iter_mut() {
            r.burns = 2;
        }
        // d is a child (put back through its parent).
        all[1].parent = Some("20261108T115900-e".to_string());
        assert_eq!(
            select_stretch_t(&all, &Stretch::Last(2), None, now).unwrap(),
            vec![0, 3]
        );
        assert_eq!(
            select_stretch_t(&all, &Stretch::Last(9), None, now).unwrap(),
            vec![0, 3, 4]
        );
        assert_eq!(
            select_stretch_t(
                &all,
                &Stretch::Since(chrono::Duration::try_hours(1).unwrap()),
                None,
                now
            )
            .unwrap(),
            vec![0]
        );
        assert_eq!(
            select_stretch_t(&all, &Stretch::To("20261107T100000".into()), None, now).unwrap(),
            vec![0, 3],
            "--to keeps the run it names"
        );
        assert!(
            select_stretch_t(&all, &Stretch::To("2026110".into()), None, now)
                .unwrap_err()
                .contains("matches 5 runs")
        );
        assert!(select_stretch_t(&all, &Stretch::To("nope".into()), None, now).is_err());
        // A program's stretch keeps to its own runs.
        all[3].program = Some(serde_json::json!({"sha256": "abc"}));
        assert_eq!(
            select_stretch(&all, &Stretch::Last(9), None, Some("abc"), now).unwrap(),
            vec![3]
        );
        all[3].program = None;
        // A run that burned nothing has nothing to put back.
        all[0].burns = 0;
        assert_eq!(
            select_stretch_t(&all, &Stretch::Last(1), None, now).unwrap(),
            vec![3]
        );
        // A run still running stops a stretch that reaches past it.
        all[0].status = "running".to_string();
        assert!(select_stretch_t(&all, &Stretch::Last(1), None, now)
            .unwrap_err()
            .contains("still running"));
    }
}
