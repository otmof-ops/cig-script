// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! `cig run`, `cig check`, `cig eval`.

use super::{exit, Ctx};
use crate::burn::{runs::RunRecord, Kernel, Mode, PlannedOp};
use crate::check;
use crate::config::Config;
use crate::crash::Kind as ReportKind;
use crate::diagnostics::{Diagnostic, Kind};
use crate::humidor;
use crate::interp::Interp;
use crate::pin::{self, Pin};
use crate::syntax::ast::Program;
use crate::syntax::{parse, parse_expr_src};
use crate::trial;
use crate::update::Version;
use crate::NO_CIGARETTES;
use std::io::Write;
use std::path::Path;

/// Scripts larger than this are not scripts; a device like /dev/zero would
/// otherwise be read forever.
const MAX_SCRIPT_BYTES: u64 = 64 * 1024 * 1024;

/// Print a lock-or-strict-mode error: when the message was tagged by the
/// strict-mode check (`E721`), emit under that code with a pointer to
/// `cig explain E721`; otherwise emit under `E802` as before.
pub(crate) fn emit_lock_or_strict_error(ctx: &Ctx, e: &str) {
    if let Some(rest) = e.strip_prefix("E720 burn: ") {
        eprintln!("{} {rest}", ctx.red("error[E720 burn]:"));
        eprintln!("  = explain: cig explain E720");
    } else if let Some(rest) = e.strip_prefix("E721 burn: ") {
        eprintln!("{} {rest}", ctx.red("error[E721 burn]:"));
        eprintln!("  = explain: cig explain E721");
    } else {
        eprintln!("{} {e}", ctx.red("error[E802 usage]:"));
        eprintln!("  = explain: cig explain E802");
    }
}

fn read_capped(file: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(file)?
        .take(MAX_SCRIPT_BYTES + 1)
        .read_to_end(&mut buf)?;
    if buf.len() as u64 > MAX_SCRIPT_BYTES {
        return Err(std::io::Error::other(
            "larger than 64 MiB, which no script is",
        ));
    }
    String::from_utf8(buf).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "stream did not contain valid UTF-8",
        )
    })
}

pub fn read_source(ctx: &Ctx, file: &Path) -> Result<String, i32> {
    read_capped(file).map_err(|e| {
        let hint = match e.kind() {
            std::io::ErrorKind::NotFound => {
                "check the path; scripts resolve against the current directory"
            }
            std::io::ErrorKind::PermissionDenied => "chmod +r the file, or run as its owner",
            _ => "the operating system's reason is in the message",
        };
        let d = crate::diagnostics::usage(format!("cannot read {}: {e}", file.display()))
            .code("E801")
            .with_subject(file.display().to_string())
            .with_hint(hint);
        report(ctx, &d, None, None);
        exit::USAGE
    })
}

/// Print a diagnostic the way every command does. This is the one emit
/// point: doctor's automatic diagnosis fires here, after the diagnostic,
/// unless `--no-doctor` or `CIG_DOCTOR=0` switched it off.
pub fn report(ctx: &Ctx, diag: &Diagnostic, file: Option<&str>, source: Option<&str>) {
    let diagnosis = if ctx.doctor {
        crate::doctor::diagnose(diag, &crate::doctor::Context::here(source))
    } else {
        None
    };
    if ctx.json {
        let mut obj = serde_json::to_value(diag).unwrap_or_default();
        if let Some(f) = file {
            obj["file"] = serde_json::Value::String(f.to_string());
        }
        if let Some(dx) = &diagnosis {
            obj["diagnosis"] = serde_json::to_value(dx).unwrap_or_default();
        }
        outln!("{}", serde_json::to_string(&obj).unwrap_or_default());
        return;
    }
    if !ctx.plain {
        eprintln!("{}", ctx.red(NO_CIGARETTES));
    }
    eprint!("{}", diag.render(file, source));
    if let Some(dx) = &diagnosis {
        eprint!("{}", super::doctor::render_diagnosis(ctx, dx));
    }
}

/// Several diagnostics from one check: the banner once, then each one
/// through the same hook. In JSON mode each error carries its diagnosis.
pub fn report_all(ctx: &Ctx, diags: &[Diagnostic], file: &str, source: &str) {
    if ctx.json {
        return;
    }
    if !ctx.plain {
        eprintln!("{}", ctx.red(NO_CIGARETTES));
    }
    for d in diags {
        eprint!("{}", d.render(Some(file), Some(source)));
        if ctx.doctor {
            if let Some(dx) =
                crate::doctor::diagnose(d, &crate::doctor::Context::here(Some(source)))
            {
                eprint!("{}", super::doctor::render_diagnosis(ctx, &dx));
            }
        }
    }
}

/// A check report's diagnostics as JSON, each with its diagnosis when
/// doctor is on.
pub fn diags_json(ctx: &Ctx, diags: &[Diagnostic], source: &str) -> Vec<serde_json::Value> {
    diags
        .iter()
        .map(|d| {
            let mut v = serde_json::to_value(d).unwrap_or_default();
            if ctx.doctor {
                if let Some(dx) =
                    crate::doctor::diagnose(d, &crate::doctor::Context::here(Some(source)))
                {
                    v["diagnosis"] = serde_json::to_value(dx).unwrap_or_default();
                }
            }
            v
        })
        .collect()
}

pub fn check(ctx: &Ctx, file: &Path, deny_warnings: bool) -> i32 {
    let source = match read_source(ctx, file) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let name = file.to_string_lossy().to_string();
    let program = match parse(&source) {
        Ok(p) => p,
        Err(d) => {
            report(ctx, &d, Some(&name), Some(&source));
            return exit::SYNTAX_ERROR;
        }
    };
    let mut rep = check::check(&program, &[]);
    let pin = pin::pin_for(file, &source).ok().flatten();
    rep.warnings
        .extend(pin_warnings(&program, pin.as_ref(), &name));
    let denied = deny_warnings && !rep.warnings.is_empty();
    if ctx.json {
        let obj = serde_json::json!({
            "file": name,
            "ok": rep.ok(),
            "errors": diags_json(ctx, &rep.errors, &source),
            "warnings": rep.warnings,
            "warnings_denied": denied,
            "pin": pin.as_ref().map(|p| p.constraint.to_string()),
            "needs": crate::since::minimum(&program).to_string(),
            "uses": crate::since::uses(&program).iter().map(|u| serde_json::json!({"what": u.what, "since": u.since.to_string()})).collect::<Vec<_>>(),
            "last_good": crate::burn::runs::last_good(&sha256(&source)).map(|(v, _)| v.to_string()),
        });
        outln!("{}", serde_json::to_string(&obj).unwrap_or_default());
        return if rep.ok() && !denied {
            exit::OK
        } else {
            exit::SYNTAX_ERROR
        };
    }
    for w in &rep.warnings {
        eprint!(
            "{}",
            ctx.yellow(
                &w.render(Some(&name), Some(&source))
                    .replacen("error[", "warning[", 1)
            )
        );
    }
    if rep.ok() && denied {
        eprintln!(
            "{} {} has {} warning{} and --deny-warnings is set",
            ctx.red("error[E300 check]:"),
            name,
            rep.warnings.len(),
            if rep.warnings.len() == 1 { "" } else { "s" }
        );
        exit::SYNTAX_ERROR
    } else if rep.ok() {
        eprintln!(
            "{} {} ({} statements, {} warning{})",
            ctx.green("ok:"),
            name,
            program.body.len(),
            rep.warnings.len(),
            if rep.warnings.len() == 1 { "" } else { "s" }
        );
        exit::OK
    } else {
        report_all(ctx, &rep.errors, &name, &source);
        exit::SYNTAX_ERROR
    }
}

pub fn eval(ctx: &Ctx, code: &str) -> i32 {
    let kernel = Kernel::ephemeral(Mode::Run);
    let mut interp = Interp::new(kernel);
    // Try as an expression first so `cig eval "1 + 1"` prints 2.
    if let Ok(expr) = parse_expr_src(code, Default::default()) {
        let program = crate::syntax::ast::Program {
            body: vec![crate::syntax::ast::Stmt::Expr(expr)],
        };
        return finish_eval(ctx, &mut interp, &program, code);
    }
    match parse(code) {
        Ok(program) => finish_eval(ctx, &mut interp, &program, code),
        Err(d) => {
            report(ctx, &d, None, Some(code));
            exit::SYNTAX_ERROR
        }
    }
}

fn finish_eval(
    ctx: &Ctx,
    interp: &mut Interp,
    program: &crate::syntax::ast::Program,
    code: &str,
) -> i32 {
    match interp.eval_program(program) {
        Ok(Some(v)) => {
            if ctx.json {
                match crate::stdlib::json::from_value(&v, Default::default()) {
                    Ok(j) => outln!("{j}"),
                    Err(_) => outln!("{}", serde_json::Value::String(v.repr())),
                }
            } else {
                outln!("{}", v.repr());
            }
            interp.exit_requested.unwrap_or(exit::OK)
        }
        Ok(None) => interp.exit_requested.unwrap_or(exit::OK),
        Err(d) => {
            report(ctx, &d, None, Some(code));
            exit::SCRIPT_ERROR
        }
    }
}

pub fn run(ctx: &Ctx, args: super::RunArgs, chain: Option<String>) -> i32 {
    let source = match read_source(ctx, &args.file) {
        Ok(s) => s,
        Err(code) => return code,
    };
    run_source(ctx, args, chain, source, None)
}

/// A compiled program's run (see [`crate::program`]): the script from the
/// program's own binary, `args.file` the path it was compiled from, so an
/// error points where `cig run` would. The same run in everything else;
/// no pin (the program is its cig), and its record names the program.
pub fn run_program(
    ctx: &Ctx,
    args: super::RunArgs,
    program: &crate::program::Program,
    chain: Option<String>,
) -> i32 {
    run_source(ctx, args, chain, program.source.clone(), Some(program))
}

fn run_source(
    ctx: &Ctx,
    args: super::RunArgs,
    chain: Option<String>,
    source: String,
    compiled: Option<&crate::program::Program>,
) -> i32 {
    let dry_run = args.dry_run;
    let code = run_source_inner(ctx, args, chain, source, compiled);
    // The retention settings, once the run is recorded (#105); then the
    // disk budget (#149), which is off by default.
    if !dry_run {
        crate::cli::runs::keep_by_config();
        crate::cli::runs::budget_in_background();
    }
    code
}

fn run_source_inner(
    ctx: &Ctx,
    args: super::RunArgs,
    chain: Option<String>,
    source: String,
    compiled: Option<&crate::program::Program>,
) -> i32 {
    let name = args.file.to_string_lossy().to_string();
    let program = match parse(&source) {
        Ok(p) => p,
        Err(d) => {
            report(ctx, &d, Some(&name), Some(&source));
            return exit::SYNTAX_ERROR;
        }
    };
    let pin = match compiled {
        Some(_) => None,
        None => pin::pin_for(&args.file, &source).ok().flatten(),
    };
    if !args.no_check {
        let mut rep = check::check(&program, &[]);
        rep.warnings
            .extend(pin_warnings(&program, pin.as_ref(), &name));
        if !rep.ok() {
            if ctx.json {
                for e in &rep.errors {
                    report(ctx, e, Some(&name), None);
                }
            } else {
                report_all(ctx, &rep.errors, &name, &source);
            }
            return exit::SYNTAX_ERROR;
        }
        if !ctx.json {
            for w in &rep.warnings {
                eprint!(
                    "{}",
                    ctx.yellow(
                        &w.render(Some(&name), Some(&source))
                            .replacen("error[", "warning[", 1)
                    )
                );
            }
        }
    }

    if let Ok(v) = std::env::var("CIG_MAX_STEPS") {
        if v.trim().parse::<u64>().is_err() {
            let d = crate::diagnostics::usage(format!(
                "CIG_MAX_STEPS=`{v}` is not a step count"
            ))
            .code("E800")
            .with_hint("give a whole number of steps, or 0 to disable the budget; --max-steps does the same");
            report(ctx, &d, None, None);
            return exit::USAGE;
        }
    }
    // The trial: on a cig this script has not been proved on, compare plans
    // with its last good version first, and let that version run it when
    // they differ.
    let trial_mode = if args.no_trial {
        "never".to_string()
    } else if args.trial {
        "always".to_string()
    } else {
        Config::load().get("trial")
    };
    let mut trial_note = None;
    let last_good =
        if compiled.is_none() && !args.dry_run && !pin::dispatched() && trial_mode != "never" {
            let (handed, note, last) =
                trial_before_run(ctx, &args, &source, &program, &name, &trial_mode);
            if let Some(code) = handed {
                return code;
            }
            trial_note = note;
            last
        } else {
            None
        };

    let mode = if args.dry_run {
        Mode::DryRun
    } else {
        Mode::Run
    };
    // What a run that died in a boxed hop left behind goes before this one
    // starts: its layers were never the pack's.
    crate::burn::runs::sweep_dead_layers();
    if let Err(e) = crate::burn::ensure_private() {
        eprintln!("{} {e}", ctx.red("error[E707 burn]:"));
        eprintln!("  = explain: cig explain E707");
        return exit::SCRIPT_ERROR;
    }
    // A run holds the tin key's lock from before its first op, so a `cig
    // tin rekey` cannot change the key under it; where the lock cannot be
    // had, the run refuses before anything burns rather than run without
    // its record (#105).
    if !args.dry_run {
        if let Err(e) = crate::burn::seal::hold_shared_rekey_lock() {
            emit_lock_or_strict_error(ctx, &e);
            return exit::SCRIPT_ERROR;
        }
    }
    let mut record = RunRecord::begin(
        &args.file,
        &source,
        &args.args,
        if args.dry_run { "dry-run" } else { "run" },
    );
    if let Some(p) = compiled {
        record.script = p.name.clone();
        let mut v = p.summary();
        v["binary"] = serde_json::json!(std::env::current_exe()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default());
        record.program = Some(v);
    }
    let run_dir = if args.dry_run {
        None
    } else {
        if let Err(e) = record.save() {
            eprintln!(
                "{} cannot create the run record under {}: {e}",
                ctx.red("error[E802 usage]:"),
                crate::burn::runs::runs_dir().display()
            );
            eprintln!("  = hint: set CIGSCRIPT_HOME to a directory you can write");
            eprintln!("  = explain: cig explain E802");
            return exit::USAGE;
        }
        Some(record.dir())
    };
    let kernel = match Kernel::new(mode, run_dir) {
        Ok(k) => k,
        Err(d) => {
            report(ctx, &d, Some(&name), None);
            return exit::USAGE;
        }
    };
    crate::crash::set_active_run(if args.dry_run {
        None
    } else {
        Some(record.id.clone())
    });
    let mut interp = Interp::new(kernel);
    interp.script_name = Some(name.clone());
    interp.script_path = std::path::absolute(&args.file).ok();
    interp.set_args(&args.args);
    if let Some(n) = args.max_steps {
        interp.set_max_steps(n);
    }

    let mut result = interp.run_program(&program);
    if result.is_ok() {
        if let Some(chain_name) = &chain {
            result = light_named_chain(&mut interp, chain_name, &program);
        }
    }
    let _ = interp.out.flush();

    let mut code = match &result {
        Ok(()) => interp.exit_requested.unwrap_or(exit::OK),
        // A check-family error reached at run time (`--no-check`, or a
        // name the checker could not see) exits like the checker would.
        Err(d) if d.kind == crate::diagnostics::Kind::Check => exit::SYNTAX_ERROR,
        Err(_) => exit::SCRIPT_ERROR,
    };
    let mut rolled_back = None;
    if let Err(d) = &result {
        report(ctx, d, Some(&name), Some(&source));
        if !args.dry_run && !args.no_rollback && !interp.kernel.journal().is_empty() {
            // Compensations run in place, newest first, in this interpreter
            // with an ephemeral kernel standing in for the real one.
            let mut real = std::mem::replace(&mut interp.kernel, Kernel::ephemeral(Mode::Run));
            let rep =
                real.rollback_with(&mut |c| interp.run_compensation(c).map_err(|e| e.message));
            interp.kernel = real;
            if !ctx.json {
                eprintln!("{}", ctx.bold("unburn: rolling back this run's burns"));
                for (label, text) in &rep.actions {
                    eprintln!("  {} {text}", super::runs::paint_action(ctx, label));
                }
            }
            rolled_back = Some(rep);
        }
    }

    if args.dry_run {
        print_plan_with_pack(
            ctx,
            interp.kernel.dry_run_plan().collect(),
            "dry-run plan",
            "would burn",
            interp.kernel.pack(),
        );
    }
    let intents: Vec<&PlannedOp> = interp.kernel.intents().collect();
    if !intents.is_empty() {
        print_plan(
            ctx,
            intents.clone(),
            "unlit intents",
            "declared, never burned",
        );
    }

    if !args.dry_run {
        record.burns = interp.kernel.journal().len();
        record.no_rollback = args.no_rollback;
        record.irreversible = interp.kernel.irreversible;
        record.pack = interp
            .kernel
            .pack()
            .map(<[std::path::PathBuf]>::to_vec)
            .unwrap_or_default();
        record.rolled_back = rolled_back.as_ref().is_some_and(|r| !r.restored.is_empty());
        record.rollback_failed = rolled_back
            .as_ref()
            .map(|r| r.failed.clone())
            .unwrap_or_default();
        record.rollback_took = rolled_back.as_ref().map(|r| r.took.len()).unwrap_or(0);
        let (status, error) = match &result {
            Ok(()) => ("ok", None),
            Err(d) => (
                if !record.rollback_failed.is_empty() {
                    "rollback_incomplete"
                } else if record.rolled_back {
                    "rolled_back"
                } else {
                    "error"
                },
                Some(d.message.clone()),
            ),
        };
        if let Err(d) = &result {
            let mut v = serde_json::to_value(d).unwrap_or_default();
            v["file"] = serde_json::Value::String(name.clone());
            record.diagnostic = Some(v);
        }
        record.trial = trial_note.clone();
        // A run that burned nothing wrote no journal line: it has no head,
        // and the ledger has nothing to keep for it (#129).
        let wrote = !interp.kernel.journal().wrote_nothing();
        record.journal_head = wrote.then(|| interp.kernel.journal().head().to_string());
        record.finish(status, error);
        // The ledger: where this run's record ended, under this machine's
        // key, so a journal cut short or swapped is caught later.
        if wrote && interp.kernel.journal().format() >= 3 {
            if let Err(e) = crate::burn::ledger::append(
                "run",
                &record.id,
                interp.kernel.journal().head(),
                &record.status,
                interp.kernel.journal().chain().lines as u64,
            ) {
                eprintln!("{} could not write the ledger: {e}", ctx.yellow("warning:"));
            }
        }
        if !record.rolled_back && !interp.kernel.journal().is_empty() {
            if let Err(e) = interp.kernel.journal().write_after_state() {
                eprintln!(
                    "{} could not record the run's after-state: {e}",
                    ctx.yellow("warning:")
                );
            }
        }
        if let Err(e) = record.save() {
            eprintln!(
                "{} could not finish the run record: {e}",
                ctx.yellow("warning:")
            );
        } else if args.pin {
            // `--pin`: the run is kept whatever the budget says, as if
            // `cig runs pin <id>` had been said once it ended.
            let marker = record.dir().join(crate::burn::runs::PIN_FILE);
            match std::fs::write(&marker, format!("{}\n", chrono::Utc::now().to_rfc3339())) {
                Ok(()) => crate::burn::runs_index::upsert(&record),
                Err(e) => eprintln!(
                    "{} could not pin {}: {e}",
                    ctx.yellow("warning:"),
                    record.id
                ),
            }
        }
        // The inbox of this run's hops, if a hop died with it still there.
        let _ = crate::burn::overlay::remove_tree_force(&crate::burn::overlay::inbox_dir(
            &record.dir(),
        ));
        save_intents(&record, &interp);
        settle_own_bugs(ctx, &result, rolled_back.as_ref(), &interp, &record);
        // The run is over: its journal is kept compressed (#147).
        crate::burn::pack::settle(&record.dir());
        // A run that a hop of another cig started reports to its parent
        // (the burn protocol): the parent's rollback puts this run back
        // through this run's own journal, which is exact.
        if result.is_ok() && !interp.kernel.journal().is_empty() {
            if let Some(report) = std::env::var_os(crate::burn::protocol::ENV) {
                let me = std::env::current_exe()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| "cig".to_string());
                // A program takes `--unburn`: its first word is its own.
                let line = match compiled {
                    Some(p) => serde_json::json!({
                        "op": "compensate",
                        "label": format!("{} (run {})", p.name, record.id),
                        "undo": [me, "--unburn", record.id],
                    }),
                    None => serde_json::json!({
                        "op": "compensate",
                        "label": format!("cig run {name} (run {})", record.id),
                        "undo": [me, "unburn", record.id],
                    }),
                };
                use std::io::Write as _;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(report)
                {
                    let _ = writeln!(f, "{line}");
                }
            }
        }
        // The fallback: the run failed on this cig after burns that all came
        // back, and the script has a last good version on the shelf. Run it
        // there. Never after an irreversible op: a trial never repeats one.
        if let (Err(d), Some((last, bin))) = (&result, &last_good) {
            let clean = rolled_back.as_ref().is_some_and(|r| r.failed.is_empty())
                || interp.kernel.journal().is_empty();
            let current = Version::current();
            if interp.kernel.irreversible == 0 && clean && !args.no_rollback {
                eprintln!(
                    "{} {current}: failed ({}); rolled back ({} op{}); trying cig {last}, the last good version for this script",
                    ctx.bold("trial:"),
                    truncate_message(&d.message),
                    interp.kernel.journal().len(),
                    if interp.kernel.journal().len() == 1 { "" } else { "s" }
                );
                record.trial = Some(serde_json::json!({
                    "fallback": last.to_string(),
                    "failed_on": current.to_string(),
                    "rolled_back_ops": interp.kernel.journal().len(),
                }));
                let _ = record.save();
                humidor::touch(last);
                let argv: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
                let fallback = match humidor::dispatch(bin, &argv) {
                    Ok(code) => code,
                    Err(e) => {
                        eprintln!("{} {e}", ctx.red("error[E807 usage]:"));
                        exit::USAGE
                    }
                };
                if fallback == exit::OK {
                    eprintln!(
                        "{} this script works on cig {last} and not on {current}; cig report {} bundles the failure for an issue",
                        ctx.bold("trial:"),
                        record.id
                    );
                } else {
                    eprintln!(
                        "{} this script failed on cig {last} too (exit {fallback}); cig report {} bundles the failure on {current}",
                        ctx.bold("trial:"),
                        record.id
                    );
                }
                return fallback;
            } else if interp.kernel.irreversible > 0 {
                eprintln!(
                    "{} the failed run did {} irreversible op{}; not trying it again on cig {last} (a trial never repeats one)",
                    ctx.dim("trial:"),
                    interp.kernel.irreversible,
                    if interp.kernel.irreversible == 1 { "" } else { "s" }
                );
            }
        }
        if ctx.json {
            let obj = serde_json::json!({
                "run": record,
                "rollback": rolled_back,
                "intents": interp.kernel.intents().collect::<Vec<_>>(),
            });
            outln!("{}", serde_json::to_string(&obj).unwrap_or_default());
        } else if result.is_ok() && !interp.kernel.journal().is_empty() {
            eprintln!(
                "{}",
                ctx.dim(&format!(
                    "burned {} op{} ({} irreversible), run {}",
                    interp.kernel.journal().len(),
                    if interp.kernel.journal().len() == 1 {
                        ""
                    } else {
                        "s"
                    },
                    interp.kernel.irreversible,
                    record.id
                ))
            );
        }
    } else if ctx.json {
        let obj = serde_json::json!({
            "mode": "dry-run",
            "ok": result.is_ok(),
            "plan": interp.kernel.dry_run_plan().collect::<Vec<_>>(),
            "intents": interp.kernel.intents().collect::<Vec<_>>(),
        });
        outln!("{}", serde_json::to_string(&obj).unwrap_or_default());
    }
    if let Err(d) = &result {
        if d.kind == Kind::Burn {
            code = exit::SCRIPT_ERROR;
        }
    }
    code
}

/// Whose fault was it, and did cig keep its promises? A cough, a runtime
/// or type error in the script is the script's business. An internal error
/// (E900) is cig's, and so is anything a tripwire finds after the work:
/// each becomes a report in the ashtray, said in one line, under the
/// person's consent. Nothing here changes the run's outcome.
fn settle_own_bugs(
    ctx: &Ctx,
    result: &Result<(), Diagnostic>,
    rolled_back: Option<&crate::burn::journal::RollbackReport>,
    interp: &Interp,
    record: &RunRecord,
) {
    let mut found: Vec<Finding> = Vec::new();
    if let Err(d) = result {
        if d.kind == Kind::Internal {
            found.push(Finding {
                kind: ReportKind::Internal,
                location: format!("{} internal error", d.code),
                title: d.message.clone(),
                details: Some(serde_json::to_value(d).unwrap_or_default()),
            });
        }
    }
    let journal = interp.kernel.journal();
    let mut trips = Vec::new();
    if let Some(rep) = rolled_back {
        trips.extend(crate::tripwire::after_rollback(journal, rep));
    }
    if !journal.is_empty() {
        trips.extend(crate::tripwire::journal_reread(journal));
        let sample =
            crate::tripwire::sample_size(&crate::config::Config::load().get("verify_sample"));
        if sample > 0 {
            trips.extend(crate::tripwire::tin_sample(journal));
            trips.extend(crate::tripwire::tin_random_sample(sample));
        }
    }
    for (check, what) in &interp.kernel.trips {
        trips.push(crate::tripwire::Trip {
            check,
            what: what.clone(),
            details: serde_json::json!({}),
        });
    }
    found.extend(trips.into_iter().map(Finding::from_trip));
    settle_findings(ctx, found, record, journal);
}

/// One thing cig found wrong with itself, before it is a report: its text
/// still names the person's paths.
pub(crate) struct Finding {
    kind: ReportKind,
    location: String,
    title: String,
    details: Option<serde_json::Value>,
}

impl Finding {
    fn from_trip(t: crate::tripwire::Trip) -> Finding {
        Finding {
            kind: ReportKind::Tripwire,
            location: format!("tripwire {}", t.check),
            title: t.what,
            details: Some(serde_json::json!({"check": t.check, "found": t.details})),
        }
    }
}

/// The tripwires' findings after a rollback outside a run (`cig unburn`):
/// reports, like a run's.
pub(crate) fn settle_trips(
    ctx: &Ctx,
    trips: Vec<crate::tripwire::Trip>,
    record: &RunRecord,
    journal: &crate::burn::journal::Journal,
) {
    settle_findings(
        ctx,
        trips.into_iter().map(Finding::from_trip).collect(),
        record,
        journal,
    );
}

/// Each finding becomes a report: the repro built from the journal with
/// placeholder paths, and the same placeholders standing in for the
/// person's paths everywhere the report names one (its title, its details,
/// the working directory), before the fingerprint is taken. Then queued
/// and settled under the person's consent.
fn settle_findings(
    ctx: &Ctx,
    found: Vec<Finding>,
    record: &RunRecord,
    journal: &crate::burn::journal::Journal,
) {
    use crate::crash::CrashReport;
    for f in found {
        let (check, expect) = match f.kind {
            ReportKind::Tripwire => {
                let c = f
                    .details
                    .as_ref()
                    .and_then(|d| d["check"].as_str())
                    .unwrap_or("")
                    .to_string();
                (format!("tripwire:{c}"), format!("the `{c}` tripwire trips"))
            }
            ReportKind::Internal => (
                "internal".to_string(),
                "an internal error (E900)".to_string(),
            ),
            _ => ("crash".to_string(), "a crash".to_string()),
        };
        let (repro, mut mapping) = match crate::repro::build(record, journal, &check, &expect) {
            Some((x, m)) => (Some(x), m),
            None => (None, crate::repro::Mapping::for_run(record, journal)),
        };
        let title = mapping.anonymise_text(&f.title);
        let details = f.details.map(|mut d| {
            mapping.anonymise_value(&mut d);
            d
        });
        let mut r = CrashReport::new(
            f.kind,
            &f.location,
            &title,
            details,
            Some(record.id.clone()),
        );
        r.cwd = mapping.map(std::path::Path::new(&record.cwd));
        r.repro = repro;
        if let Err(e) = crate::ashtray::queue(&mut r) {
            eprintln!(
                "{} cig noticed something it got wrong and could not save the report: {e}",
                ctx.yellow("warning:")
            );
            continue;
        }
        super::crash::settle_new(ctx, &mut r);
    }
}

fn save_intents(record: &RunRecord, interp: &Interp) -> Option<()> {
    let intents: Vec<&PlannedOp> = interp.kernel.intents().collect();
    if intents.is_empty() {
        return None;
    }
    let mut text = String::new();
    for i in intents {
        text.push_str(&serde_json::to_string(i).ok()?);
        text.push('\n');
    }
    std::fs::write(record.dir().join("intents.jsonl"), text).ok()
}

fn print_plan(ctx: &Ctx, ops: Vec<&PlannedOp>, title: &str, verb: &str) {
    print_plan_with_pack(ctx, ops, title, verb, None)
}

fn print_plan_with_pack(
    ctx: &Ctx,
    ops: Vec<&PlannedOp>,
    title: &str,
    verb: &str,
    pack: Option<&[std::path::PathBuf]>,
) {
    if ctx.json {
        return;
    }
    eprintln!(
        "{}",
        ctx.bold(&format!(
            "{title}: {} op{}",
            ops.len(),
            if ops.len() == 1 { "" } else { "s" }
        ))
    );
    if let Some(roots) = pack {
        eprintln!(
            "  {} {}",
            ctx.dim("pack:"),
            roots
                .iter()
                .map(|r| crate::crash::tilde(r))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    for op in ops {
        let tag = if op.reversible {
            ctx.dim("reversible  ")
        } else if op.compensated {
            ctx.green("compensated ")
        } else {
            ctx.yellow("irreversible")
        };
        let owner = op
            .owner
            .as_ref()
            .map(|o| ctx.dim(&format!("  [{o}]")))
            .unwrap_or_default();
        eprintln!("  {:>4}  {tag}  {}{owner}", op.seq, op.summary);
    }
    if verb == "would burn" {
        eprintln!("{}", ctx.dim("nothing was changed"));
    }
}

/// After the script has run, find the named chain in its globals and light it.
fn light_named_chain(
    interp: &mut Interp,
    name: &str,
    program: &crate::syntax::ast::Program,
) -> Result<(), Diagnostic> {
    let declared: Vec<String> = super::chains::declared(program)
        .into_iter()
        .map(|c| c.name)
        .collect();
    let value = match interp.globals.get(name) {
        Some(v @ crate::value::Value::Chain(_)) => v,
        Some(other) => {
            return Err(Diagnostic::new(
                Kind::Runtime,
                format!("`{name}` is a {}, not a chain", other.type_name()),
            ))
        }
        None => {
            let mut d = Diagnostic::new(
                Kind::Usage,
                format!("no chain named `{name}` in this script"),
            )
            .code("E804");
            if declared.is_empty() {
                d = d.with_hint("declare one with: chain name { step, step }");
            } else {
                d = d.with_hint(format!("chains here: {}", declared.join(", ")));
            }
            return Err(d);
        }
    };
    let light = interp
        .globals
        .get("light")
        .expect("light is in the prelude");
    interp
        .call(&light, vec![value], Default::default())
        .map(|_| ())
}

fn sha256(source: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(source.as_bytes()))
}

fn truncate_message(m: &str) -> String {
    crate::crash::truncate(m, 120)
}

/// E309: the script uses something newer than its pin allows.
fn pin_warnings(program: &Program, pin: Option<&Pin>, name: &str) -> Vec<Diagnostic> {
    let Some(pin) = pin else {
        return Vec::new();
    };
    let floor = pin.constraint.floor();
    crate::since::uses(program)
        .into_iter()
        .filter(|u| u.since > floor)
        .map(|u| {
            Diagnostic::new(
                Kind::Check,
                format!(
                    "`{}` arrived in cig {}, and the script is pinned to {} (which allows {})",
                    u.what, u.since, pin.constraint, floor
                ),
            )
            .code("E309")
            .at(u.span)
            .with_hint(format!(
                "cig pin {name} --to >={} raises the pin; the pinned version would refuse this",
                u.since
            ))
        })
        .collect()
}

/// Before a real run: find the script's last good version and, when this
/// cig is newer and that version is on the shelf, compare the two plans.
/// Returns the exit status when the run was handed to the last good
/// version, the note for the run record, and the last good version with
/// its binary for the fallback.
fn trial_before_run(
    ctx: &Ctx,
    args: &super::RunArgs,
    source: &str,
    program: &Program,
    name: &str,
    mode: &str,
) -> (
    Option<i32>,
    Option<serde_json::Value>,
    Option<(Version, std::path::PathBuf)>,
) {
    let Some((last, run_id)) = crate::burn::runs::last_good(&sha256(source)) else {
        return (None, None, None);
    };
    let current = Version::current();
    if last >= current {
        return (None, None, None);
    }
    let bin = match humidor::binary(&last) {
        Some(b) => b,
        None if mode == "always" => {
            let repo = Config::load().get("issues_repo");
            eprintln!(
                "{}",
                ctx.dim(&format!(
                    "trial: cig {last}, the last good version for this script, is not in the humidor; fetching it from {repo}"
                ))
            );
            match humidor::add(&repo, &last) {
                Ok(_) => match humidor::binary(&last) {
                    Some(b) => b,
                    None => return (None, None, None),
                },
                Err(e) => {
                    eprintln!("{} {e}", ctx.yellow("trial skipped:"));
                    return (
                        None,
                        Some(serde_json::json!({"last_good": last.to_string(), "skipped": e})),
                        None,
                    );
                }
            }
        }
        None => {
            eprintln!(
                "{}",
                ctx.dim(&format!(
                    "trial skipped: this script last went through on cig {last} (run {run_id}), which is not in the humidor; cig versions add {last} enables the trial"
                ))
            );
            return (
                None,
                Some(
                    serde_json::json!({"last_good": last.to_string(), "skipped": "not in the humidor"}),
                ),
                None,
            );
        }
    };
    let source_sha = sha256(source);
    let (here_v, there_v) = (current.to_string(), last.to_string());
    if let Some(r) = trial::remembered(&source_sha, &args.args, &here_v, &there_v) {
        if r.same {
            eprintln!(
                "{}",
                ctx.dim(&format!(
                    "trial: identical plans ({} op{}) on {last} and {current}, remembered: nothing the script read has changed; running on {current}",
                    r.ops,
                    if r.ops == 1 { "" } else { "s" }
                ))
            );
            let note = serde_json::json!({
                "compared": [last.to_string(), current.to_string()],
                "same": true,
                "ran_on": current.to_string(),
                "ops": r.ops,
                "remembered": true,
            });
            return (None, Some(note), Some((last, bin)));
        }
    }
    eprintln!(
        "{}",
        ctx.dim(&format!(
            "trial: this script last went through on cig {last} (run {run_id}); comparing its plan with {current}"
        ))
    );
    let (mine, recorded) = trial::plan_here_recorded(program, name, &args.args, args.max_steps);
    let mine = match mine {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{} {e}", ctx.yellow("trial skipped:"));
            return (
                None,
                Some(serde_json::json!({"last_good": last.to_string(), "skipped": e})),
                Some((last, bin)),
            );
        }
    };
    let theirs = match trial::plan_with(&bin, &args.file, &args.args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{} {e}", ctx.yellow("trial skipped:"));
            return (
                None,
                Some(serde_json::json!({"last_good": last.to_string(), "skipped": e})),
                Some((last, bin)),
            );
        }
    };
    let diff = trial::diff(&mine, &theirs);
    trial::remember(
        recorded,
        &args.file,
        &bin,
        &source_sha,
        &args.args,
        &here_v,
        &there_v,
        &trial::Remembered {
            same: diff.is_empty(),
            ops: mine.len(),
            diff: diff.clone(),
        },
    );
    if diff.is_empty() {
        eprintln!(
            "{}",
            ctx.dim(&format!(
                "trial: identical plans ({} op{}) on {last} and {current}; running on {current}",
                mine.len(),
                if mine.len() == 1 { "" } else { "s" }
            ))
        );
        let note = serde_json::json!({
            "compared": [last.to_string(), current.to_string()],
            "same": true,
            "ran_on": current.to_string(),
            "ops": mine.len(),
        });
        return (None, Some(note), Some((last, bin)));
    }
    eprintln!(
        "{} cig {last} plans {} op{}; {current} plans {}:",
        ctx.bold("trial:"),
        theirs.len(),
        if theirs.len() == 1 { "" } else { "s" },
        mine.len()
    );
    for line in &diff {
        eprintln!("  {line}");
    }
    eprintln!(
        "{} running on cig {last}, the version this script was proved on; cig run --no-trial {name} runs it on {current}",
        ctx.bold("trial:")
    );
    humidor::touch(&last);
    let argv: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    match humidor::dispatch(&bin, &argv) {
        Ok(code) => (Some(code), None, None),
        Err(e) => {
            eprintln!("{} {e}", ctx.red("error[E807 usage]:"));
            (Some(exit::USAGE), None, None)
        }
    }
}
