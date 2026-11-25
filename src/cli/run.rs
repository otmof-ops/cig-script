// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! `cig run`, `cig check`, `cig eval`.

use super::{exit, Ctx};
use crate::burn::{modcache, runs::RunRecord, Kernel, Mode, PlannedOp};
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
    report_inner(ctx, diag, file, source, None);
}

/// [`report`] without the quoted line, and a note that says why: a
/// `note` field in JSON, a `= note:` line after the diagnostic otherwise.
fn report_noted(ctx: &Ctx, diag: &Diagnostic, file: Option<&str>, note: &str) {
    report_inner(ctx, diag, file, None, Some(note));
}

fn report_inner(
    ctx: &Ctx,
    diag: &Diagnostic,
    file: Option<&str>,
    source: Option<&str>,
    note: Option<&str>,
) {
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
        if let Some(n) = note {
            obj["note"] = serde_json::Value::String(n.to_string());
        }
        outln!("{}", serde_json::to_string(&obj).unwrap_or_default());
        return;
    }
    if !ctx.plain {
        eprintln!("{}", ctx.red(NO_CIGARETTES));
    }
    eprint!("{}", diag.render(file, source));
    if let Some(n) = note {
        eprintln!("  = note: {n}");
    }
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

/// Which engine a compiled program's run uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    /// The script, from the program's `source` section (format 1's
    /// record), on the tree-walker: stage `a`, or `--interp`.
    Source,
    /// The module, from the program's `bytecode` section, on the VM:
    /// stage `b` without `--interp`.
    Bytecode,
}

/// Which engine a script's run asks for: `--engine`, `CIG_ENGINE`, or the
/// config's `engine` (#254, #259). A compiled program has no say here: its
/// engine is its stage's ([`Engine`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum EngineArg {
    /// Bytecode on the VM when a module is kept for this script, the
    /// tree-walker when not; a tree-walker run leaves the lowering to a
    /// subprocess, so the next run on the VM starts with the module in
    /// place. The default.
    Auto,
    /// Bytecode on the VM: the script is lowered now and its module kept
    /// in the tin. A script the lowering does not take runs on the
    /// tree-walker, and the run's record says why.
    Vm,
    /// The tree-walker.
    Tree,
}

impl EngineArg {
    pub fn as_str(self) -> &'static str {
        match self {
            EngineArg::Auto => "auto",
            EngineArg::Vm => "vm",
            EngineArg::Tree => "tree",
        }
    }

    fn named(s: &str) -> Option<EngineArg> {
        match s.trim() {
            "auto" => Some(EngineArg::Auto),
            "vm" => Some(EngineArg::Vm),
            "tree" => Some(EngineArg::Tree),
            _ => None,
        }
    }
}

/// What the `auto_lower` config decides: `on` spawns a `__lower` child
/// after the run; `off` does not (the tree-walker runs this script every
/// time, until something else keeps a module); `wait` runs `cig __lower`
/// as a child and waits for it, so a test is deterministic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AutoLower {
    On,
    Off,
    Wait,
}

impl AutoLower {
    fn from_config(config: &Config) -> AutoLower {
        match config.get("auto_lower").trim() {
            "off" => AutoLower::Off,
            "wait" => AutoLower::Wait,
            _ => AutoLower::On,
        }
    }
}

/// The engine this run asks for: the flag, else `CIG_ENGINE`, else the
/// config's `engine`. The variable is read before the config, so one run
/// or one shell can choose whatever the default says. A value that names
/// no engine is a usage error, said before anything runs.
fn engine_asked(flag: Option<EngineArg>, config: &Config) -> Result<EngineArg, Diagnostic> {
    if let Some(e) = flag {
        return Ok(e);
    }
    if let Ok(v) = std::env::var("CIG_ENGINE") {
        if !v.trim().is_empty() {
            return EngineArg::named(&v).ok_or_else(|| {
                crate::diagnostics::usage(format!("CIG_ENGINE=`{v}` is not an engine"))
                    .code("E800")
                    .with_hint("the engines are auto, vm and tree; --engine chooses for one run")
            });
        }
    }
    Ok(EngineArg::named(&config.get("engine")).unwrap_or(EngineArg::Auto))
}

/// A pure function of (engine the run asks for, whether a module is
/// cached, whether a refusal is cached): what the run actually does, so
/// every cell of `--engine`'s table can be unit-tested without starting
/// cig at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChoiceOutcome {
    /// Run this script on the VM, module from the cache.
    VmCached,
    /// Run on the VM, lowering now (the run keeps the module).
    VmLoweringNow,
    /// Run on the tree-walker, nothing to do afterwards.
    TreeWalker,
    /// Run on the tree-walker; a `__lower` subprocess spawns after the
    /// run to keep the module for the next one.
    TreeWalkerThenLower,
    /// Run on the tree-walker, then lower in this process and wait
    /// (`auto_lower = wait`, for deterministic tests).
    TreeWalkerThenLowerWait,
}

/// The run's engine choice by the table of `--engine`'s three settings
/// against what the cache says. A compiled program never reaches this:
/// its engine is its stage's. `no_check` is read first: a script that is
/// not checked runs as written, which only the tree-walker does, and the
/// cache has nothing to add here.
pub(crate) fn choose(
    engine: EngineArg,
    no_check: bool,
    module_cached: bool,
    refusal_cached: bool,
    auto: AutoLower,
) -> ChoiceOutcome {
    if no_check {
        return ChoiceOutcome::TreeWalker;
    }
    match engine {
        EngineArg::Tree => ChoiceOutcome::TreeWalker,
        EngineArg::Vm if module_cached => ChoiceOutcome::VmCached,
        EngineArg::Vm => ChoiceOutcome::VmLoweringNow,
        EngineArg::Auto if module_cached => ChoiceOutcome::VmCached,
        EngineArg::Auto if refusal_cached => ChoiceOutcome::TreeWalker,
        EngineArg::Auto => match auto {
            AutoLower::Off => ChoiceOutcome::TreeWalker,
            AutoLower::On => ChoiceOutcome::TreeWalkerThenLower,
            AutoLower::Wait => ChoiceOutcome::TreeWalkerThenLowerWait,
        },
    }
}

/// What a run runs: a script's text, or a stage `b` program's loaded
/// module with the program it came from, for the text a diagnostic
/// quotes and `--light`'s list of chains.
enum Script<'a> {
    Text(String),
    Module {
        module: std::rc::Rc<crate::vm::Program>,
        program: &'a crate::program::Opened,
    },
}

impl Script<'_> {
    /// The script's text. A stage `b` program reads its `source` section
    /// the first time this is asked for, and checks it against its table
    /// and its record.
    fn text(&self) -> Result<&str, String> {
        match self {
            Script::Text(s) => Ok(s),
            Script::Module { program, .. } => program.source(),
        }
    }
}

/// A compiled program's run (see [`crate::program`]): the script from the
/// program's own binary, `args.file` the path it was compiled from, so an
/// error points where `cig run` would. The same run in everything else;
/// no pin (the program is its cig), and its record names the program.
///
/// [`Engine::Source`] reads the script through
/// [`crate::program::Opened::source`]: a format 2 program's `source`
/// section, checked against its table and its record, or a damaged program
/// (E1050). [`Engine::Bytecode`] reads the `bytecode` section and loads it
/// ([`load_bytecode`]); any failure there is E1050 and nothing runs: the
/// script is never run in its place.
pub fn run_program(
    ctx: &Ctx,
    args: super::RunArgs,
    program: &crate::program::Opened,
    chain: Option<String>,
    engine: Engine,
) -> i32 {
    let script = match engine {
        Engine::Source => match program.source() {
            Ok(s) => Script::Text(s.to_string()),
            Err(e) => return super::compiled::damaged(ctx, &e),
        },
        Engine::Bytecode => match load_bytecode(program) {
            Ok(module) => Script::Module { module, program },
            Err(e) => return super::compiled::damaged(ctx, &e),
        },
    };
    run_script(ctx, args, chain, script, Some(program.loaded()))
}

/// A stage `b` program's module: section `bytecode` read (not hashed with
/// the table's sha256: cig-compile's decision 0004, section 5), then
/// opened by [`crate::vm::open`]: its magic, version, flags and the BLAKE3
/// over every byte after its header, its head, string pool, directory and
/// tree, every function's upvalues against its parent's counts, and the
/// top level read, verified and loaded. Every other function is read,
/// verified and loaded the first time the run enters it, before its first
/// instruction runs. With `CIG_VALIDATE=1` the whole module is parsed and
/// every function verified and loaded here instead (`parse_module`,
/// `verify_module`, `vm::load`), before anything runs. The head's sha256
/// covers the table entry that says where the body lies; the module's
/// header and its BLAKE3 cover every byte of the body. The error names the
/// rule, and `fn<i>+<pc>` where the verifier or the loader gives one, as
/// `docs/BYTECODE.md` words it.
///
/// The program is never dropped: it lives until the process ends, which
/// is right after the run, so its tables are not taken apart on the way
/// out.
fn load_bytecode(
    program: &crate::program::Opened,
) -> Result<std::rc::Rc<crate::vm::Program>, String> {
    let loaded = open_module(program.bytecode()?, "the program's bytecode")?;
    // A stage `c` program's native code is loaded the first time the run
    // calls one of the script's functions; with `CIG_VALIDATE=1`, here,
    // before anything runs, and a damaged section stops the start. A
    // section this machine cannot load never does: the bytecode is the
    // whole program.
    if let Some(source) = crate::native::Source::of(program) {
        loaded.offer_native(source);
        if crate::program::validate_by_env() {
            loaded.load_native().map_err(|e| e.to_string())?;
        }
    }
    std::mem::forget(std::rc::Rc::clone(&loaded));
    Ok(loaded)
}

/// A module's bytes, opened on this cig's VM as a start opens them: the
/// header, the hash over every byte after it, and the top level, with
/// every other function verified at its first call. With `CIG_VALIDATE=1`
/// the whole module is parsed and every function verified and loaded
/// before anything runs. Either way the hash is checked first, which is
/// what ties a module kept in the tin to the MAC over its header.
fn open_module(body: Vec<u8>, what: &str) -> Result<std::rc::Rc<crate::vm::Program>, String> {
    if crate::program::validate_by_env() {
        let module =
            cig_bytecode::parse_module(&body).map_err(|e| format!("{what} does not read: {e}"))?;
        let verified =
            cig_bytecode::verify_module(&module).map_err(|e| crate::vm::refusal(&e).message)?;
        // The hash `parse_module` checked names the module, as it does
        // for one opened without the whole parse.
        match cig_bytecode::module_hash(&body) {
            Some(hash) => crate::vm::load_hashed(verified, hash),
            None => crate::vm::load(verified),
        }
        .map_err(|d| d.message)
    } else {
        crate::vm::open(body).map_err(|d| d.message)
    }
}

fn run_source(
    ctx: &Ctx,
    args: super::RunArgs,
    chain: Option<String>,
    source: String,
    compiled: Option<&crate::program::Loaded>,
) -> i32 {
    run_script(ctx, args, chain, Script::Text(source), compiled)
}

fn run_script(
    ctx: &Ctx,
    args: super::RunArgs,
    chain: Option<String>,
    script: Script<'_>,
    compiled: Option<&crate::program::Loaded>,
) -> i32 {
    let dry_run = args.dry_run;
    // Phases and counters, when asked for by flag or by CIG_TIMINGS_FD /
    // CIG_STATS_FD; otherwise nothing is read or written (see `timings`).
    let t = match crate::timings::Timings::from_run(args.timings.as_deref(), args.stats.as_deref())
    {
        Ok(t) => t,
        Err(e) => {
            let d = crate::diagnostics::usage(format!("--timings/--stats: {e}")).code("E800");
            report(ctx, &d, None, None);
            return exit::USAGE;
        }
    };
    let code = run_source_inner(ctx, args, chain, &script, compiled, &t);
    // The retention settings, once the run is recorded (#105); then the
    // disk budget (#149), which is off by default.
    let retain_t = t.start();
    if !dry_run {
        crate::cli::runs::keep_by_config();
        crate::cli::runs::budget_in_background();
    }
    t.end("retain", retain_t);
    t.finish();
    code
}

/// What a script's engine choice came to, for the run's record.
#[derive(Default)]
struct Chosen {
    /// The run asked for the VM.
    asked_vm: bool,
    /// Where the VM's module came from: `cached` (the tin) or `lowered`
    /// (by this run).
    module: Option<&'static str>,
    /// Ran on the tree-walker: why. `asked_vm` says whether the user
    /// asked for the VM or `auto` fell back from a cached refusal.
    why_tree: Option<String>,
}

impl Chosen {
    /// The record's `engine`: which engine ran, where its module came
    /// from, and why the tree-walker ran a script that asked for the VM
    /// or that `auto` had a cached refusal for. A compiled program's
    /// engine is its stage's and its module its own.
    fn note(&self, on_vm: bool, compiled: bool) -> Option<serde_json::Value> {
        let mut v = serde_json::json!({ "engine": if on_vm { "vm" } else { "tree" } });
        if !compiled {
            if let (true, Some(from)) = (on_vm, self.module) {
                v["module"] = serde_json::json!(from);
            }
            if !on_vm {
                if self.asked_vm {
                    v["asked"] = serde_json::json!("vm");
                    v["why"] = serde_json::json!(self.why_tree.as_deref().unwrap_or("no module"));
                } else if let Some(why) = &self.why_tree {
                    v["why"] = serde_json::json!(why);
                }
            }
        }
        Some(v)
    }
}

/// `cig __lower <script>` reaches the lowering through this: it reads
/// the script, parses and checks it, and keeps the module (or a refusal)
/// in `modcache` under the same key `cig run --engine vm` keyed it
/// under. Nothing is burned, no run record is written, no ledger line is
/// added. The script is read with [`read_capped`], not [`read_source`],
/// so no diagnostic is printed and no doctor is run: nothing is written
/// to a parent's stdio.
///
/// The marker is taken with `create_new` before anything else, so twenty
/// first runs that spawn one of these at once keep one module and leave
/// no leftover marker: the other nineteen find the marker there and
/// exit. A lowering refusal is kept too so a later run under `auto`
/// picks the tree-walker without starting another subprocess.
///
/// The exit code is always 0: a `__lower` whose work could not be done
/// is a run that will meet the same the next time, and nothing reads
/// this exit code anyway (the parent never waits for it under `on`, and
/// only reads its status under `wait`, where failure to lower is
/// correctly represented by an unchanged cache).
pub fn internal_lower(file: &std::path::Path) -> i32 {
    // A path that is not a regular file (a device, a pipe, a process
    // substitution): keeping a module for its one-time read would give a
    // later run the wrong module. Nothing to do. `fs::metadata` (not
    // `symlink_metadata`) follows a link, so a script reached through
    // `~/bin/x.cig` as a link still has its module made.
    if !is_regular_file(file) {
        return exit::OK;
    }
    let source = match read_capped(file) {
        Ok(s) => s,
        Err(_) => return exit::OK,
    };
    let sha = sha256(&source);
    let key = crate::burn::modcache::key_for(&sha);
    let Some(_lock) = crate::burn::modcache::claim_lowering(&key) else {
        // Someone else has the marker: one lowering at a time per script.
        return exit::OK;
    };
    // If a healthy module or a refusal for this build is already there
    // (for example, a `--engine vm` run just before the spawn), do no
    // work. A module whose body is damaged is detected by `vm::open`
    // (the MAC covers the header, not the body): treat such an entry
    // as absent so the fresh lowering below overwrites it.
    match crate::burn::modcache::lookup(&key) {
        Some(crate::burn::modcache::Hit::Refused { .. }) => return exit::OK,
        Some(crate::burn::modcache::Hit::Module { module, .. }) => {
            if crate::vm::open(module).is_ok() {
                return exit::OK;
            }
            // The body does not open: the entry heals below.
        }
        None => {}
    }
    let program = match parse(&source) {
        Ok(p) => p,
        Err(_) => return exit::OK,
    };
    let rep = check::check(&program, &[]);
    if !rep.ok() {
        // A script that does not check is a run that will say so when the
        // tree-walker tries it; nothing is cached.
        return exit::OK;
    }
    // `warned` is set the same way `cig run` sets it: the check's own
    // warnings and, when the pin lives in the script's own bytes, its
    // warnings too. One function so the two paths cannot drift.
    let warned = lowered_warned(file, &source, &program, &rep);
    match crate::lower::module_loaded(&program, "") {
        Ok((bytes, _loaded)) => {
            let _ = crate::burn::modcache::store(&key, &bytes, warned);
        }
        Err(crate::lower::Failure::Refused(what)) => {
            let _ = crate::burn::modcache::store_refusal(
                &key,
                &format!("the lowering does not take it yet: {what}"),
            );
        }
        Err(crate::lower::Failure::Internal(e)) => {
            let _ = crate::burn::modcache::store_refusal(
                &key,
                &format!("the lowering failed on it: {e}"),
            );
        }
        Err(crate::lower::Failure::Crashed(e)) => {
            // The crash hook has said what broke and where its report
            // is. A crash becomes a refusal for this build: nothing on
            // `auto` starts another child until the script or cig
            // changes, so a reproducible panic does not cost a report
            // on every run.
            let _ = crate::burn::modcache::store_refusal(
                &key,
                &format!("the lowering crashed on it: {e}"),
            );
        }
    }
    exit::OK
}

/// Was this script's check noisy enough that a later run has something
/// to say again? The script's own check warnings, and, when the pin is
/// the script's own header line, its warnings too. Shared by `cig run
/// --engine vm` and `cig __lower` so the two paths cannot drift (an
/// in-script pin that warned had its warning said on run 1 and not on
/// run 2 before this helper was pulled out).
fn lowered_warned(
    file: &std::path::Path,
    source: &str,
    program: &Program,
    rep: &check::Report,
) -> bool {
    if !rep.warnings.is_empty() {
        return true;
    }
    let pin = pin::pin_for(file, source).ok().flatten();
    let pin_in_script = !matches!(
        pin.as_ref().map(|p| &p.source),
        Some(pin::Source::VersionFile(_))
    );
    let name = file.to_string_lossy().to_string();
    let pinned = pin_warnings(program, pin.as_ref(), &name);
    pin_in_script && !pinned.is_empty()
}

/// Configure `cmd` the way a `__lower` child is: three standard
/// streams on the null device, and (on Unix) a process group of its
/// own; on Windows the detached-process creation flags. Shared with
/// a test that spawns a small shell command under the same settings to
/// read them back.
pub(crate) fn prepare_detached(cmd: &mut std::process::Command) {
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP, as `ashtray` uses.
        cmd.creation_flags(0x0000_0008 | 0x0000_0200);
    }
}

/// Build the `cig __lower <script>` command this cig would run, in
/// the shape [`spawn_background_lower`] and [`lower_in_foreground`]
/// spawn it. Every environment variable the parent is running under
/// carries over, so `CIGSCRIPT_HOME`, `CIG_KEYS_DIR`, `CIG_SEAL_KEY`
/// and the rest reach the child.
pub(crate) fn lower_command(script: &std::path::Path) -> Option<std::process::Command> {
    // `self_command` marks this as cig's call to itself, as
    // `runs::budget_in_background` does for `__budget`.
    let mut cmd = crate::program::self_command().ok()?;
    cmd.arg("__lower").arg(script);
    prepare_detached(&mut cmd);
    Some(cmd)
}

/// Is `file` a regular file (links followed)? A device, a pipe, a
/// process substitution or a missing path is not one. The parent asks
/// before it starts a `__lower` child, and the child asks again before
/// it reads: a script on `/dev/stdin` never gets a module made for its
/// one-time read.
fn is_regular_file(file: &std::path::Path) -> bool {
    std::fs::metadata(file).is_ok_and(|m| m.file_type().is_file())
}

/// Start a `__lower` child in the background. The spawn is the last
/// thing a run does before it exits (like `__budget`'s spawn in
/// `runs::budget_in_background`), so between spawn and exit the child
/// is the parent's; the kernel reparents it when the parent exits. A
/// spawn that fails changes nothing about the run.
fn spawn_background_lower(script: &std::path::Path) {
    if !is_regular_file(script) {
        return;
    }
    if let Some(mut cmd) = lower_command(script) {
        let _ = cmd.spawn();
    }
}

/// Run `cig __lower <script>` as a child, waited for and reaped: the
/// deterministic form of the background spawn for a test under
/// `auto_lower = wait`. The child is a separate process (as
/// [`spawn_background_lower`] starts one), so it keys the script by the
/// bytes it reads itself. The parent waits for its status rather than
/// exiting, so no test leaves a child behind.
fn lower_in_foreground(script: &std::path::Path) {
    if let Some(mut cmd) = lower_command(script) {
        let _ = cmd.status();
    }
}

fn run_source_inner(
    ctx: &Ctx,
    args: super::RunArgs,
    chain: Option<String>,
    script: &Script<'_>,
    compiled: Option<&crate::program::Loaded>,
    t: &crate::timings::Timings,
) -> i32 {
    let name = args.file.to_string_lossy().to_string();
    // A script's own text. A stage `b` program's module was read and
    // loaded before the run began, and its text is read only when
    // something needs it (a diagnostic's quoted line, `--light`'s list of
    // chains).
    let source: Option<&str> = match script {
        Script::Text(source) => Some(source.as_str()),
        Script::Module { .. } => None,
    };
    let script_sha = source.map(sha256);
    let config = Config::load();
    let pin = match (compiled, source) {
        (None, Some(source)) => pin::pin_for(&args.file, source).ok().flatten(),
        _ => None,
    };
    // A version file's pin is not in the script's bytes, so what it warns
    // about cannot be told from them: such a script is checked on every run.
    let pin_in_script = !matches!(
        pin.as_ref().map(|p| &p.source),
        Some(pin::Source::VersionFile(_))
    );
    // The engine a script asks for (#254, #259). `--no-check` runs a
    // script as written, which only the tree-walker does, and `auto`
    // picks between the two based on what the cache holds.
    let mut chosen = Chosen::default();
    let mut engine_choice = EngineArg::Tree;
    let auto_lower = AutoLower::from_config(&config);
    if let (None, Some(_)) = (compiled, source) {
        match engine_asked(args.engine, &config) {
            Ok(e) => engine_choice = e,
            Err(d) => {
                report(ctx, &d, None, None);
                return exit::USAGE;
            }
        }
        if engine_choice == EngineArg::Vm {
            if args.no_check {
                chosen.asked_vm = true;
                chosen.why_tree = Some(
                    "--no-check runs the script as written, which only the tree-walker does"
                        .to_string(),
                );
            } else {
                chosen.asked_vm = true;
            }
        }
    }
    let mut module: Option<std::rc::Rc<crate::vm::Program>> = match script {
        Script::Module { module, .. } => Some(module.clone()),
        Script::Text(_) => None,
    };
    // The script's module or refusal, when the tin holds one this machine
    // made for this cig. With nothing to say again (no warnings when it was
    // checked) the run starts from the module; otherwise the script is
    // checked first, so what it warns about is said as on every run.
    // The cache is asked only when the plan may read it: neither
    // `--no-check` (the tree-walker runs the script as written) nor
    // `--engine tree` ever uses a kept module, and a compiled program
    // (`compiled.is_some()`) sets the module already.
    let key = match (
        &script_sha,
        !args.no_check && matches!(engine_choice, EngineArg::Auto | EngineArg::Vm),
    ) {
        (Some(sha), true) => Some(modcache::key_for(sha)),
        _ => None,
    };
    let mut kept: Option<Vec<u8>> = None;
    let mut cached_refusal: Option<String> = None;
    let mut safe_to_use_cached: Option<Vec<u8>> = None;
    if let Some(key) = &key {
        let module_t = t.start();
        match modcache::lookup(key) {
            Some(modcache::Hit::Module { module: m, warned }) => {
                if warned || !pin_in_script {
                    kept = Some(m);
                } else {
                    // The kept bytes are safe to load without re-parsing
                    // or re-checking; the open itself is timed outside
                    // the `module` phase, as `compile-load`, so the
                    // lookup's phase is only the lookup and its MAC.
                    safe_to_use_cached = Some(m);
                }
            }
            Some(modcache::Hit::Refused { reason }) => {
                cached_refusal = Some(reason);
            }
            None => {}
        }
        t.end("module", module_t);
    }
    // What auto picks on this run, and what to do afterwards. `choose` is
    // the one authority: what runs and what the record says both follow
    // its result, so a cached module cannot sneak through `--no-check`
    // nor a cached refusal through `--engine vm`.
    let initial_plan = choose(
        engine_choice,
        args.no_check,
        kept.is_some() || safe_to_use_cached.is_some(),
        cached_refusal.is_some(),
        auto_lower,
    );
    // The kept bytes are opened only when the plan picked the VM from
    // the cache; a tree-walker plan leaves them where they are. An open
    // failure degrades the plan below: a damaged body gives
    // `module = None` here, the plan falls back to what a missing
    // module would have picked (lower now under `--engine vm`, tree +
    // `__lower` under `--engine auto`), and the entry is overwritten
    // (`vm` lowers inline; `auto` leaves it to the child, which skips
    // the early-return when the lookup's module cannot be opened).
    let mut open_failed_on_cached = false;
    if matches!(initial_plan, ChoiceOutcome::VmCached) {
        if let Some(bytes) = safe_to_use_cached.take() {
            let load_t = t.start();
            let opened = open_module(bytes, "the kept module");
            t.end("compile-load", load_t);
            match opened {
                Ok(opened) => {
                    module = Some(opened);
                    chosen.module = Some("cached");
                }
                Err(_) => open_failed_on_cached = true,
            }
        }
    }
    // The run's plan after any open failure. Only `VmCached` can
    // degrade: `VmLoweringNow`, `TreeWalker*` and `TreeWalker` leave no
    // room for one.
    let plan = if open_failed_on_cached && module.is_none() {
        match engine_choice {
            EngineArg::Vm => ChoiceOutcome::VmLoweringNow,
            EngineArg::Auto => match auto_lower {
                AutoLower::Off => ChoiceOutcome::TreeWalker,
                AutoLower::On => ChoiceOutcome::TreeWalkerThenLower,
                AutoLower::Wait => ChoiceOutcome::TreeWalkerThenLowerWait,
            },
            EngineArg::Tree => ChoiceOutcome::TreeWalker,
        }
    } else {
        initial_plan
    };
    let wants_module = matches!(plan, ChoiceOutcome::VmCached | ChoiceOutcome::VmLoweringNow);
    // Under `auto` a cached refusal means the tree-walker runs it and the
    // record carries the reason even though the user did not ask for the
    // VM: say it on the tree-walker side of the record so a reader of a
    // run on `auto` knows why.
    if let (EngineArg::Auto, Some(reason)) = (engine_choice, &cached_refusal) {
        chosen.asked_vm = false; // The user asked for auto, not vm.
        chosen.why_tree = Some(reason.clone());
    }
    // The script is parsed unless a module stands ready for it.
    let parsed = match (source, &module) {
        (Some(source), None) => {
            let lower_t = t.start();
            let parsed = parse(source);
            t.end("lower", lower_t);
            match parsed {
                Ok(p) => Some(p),
                Err(d) => {
                    report(ctx, &d, Some(&name), Some(source));
                    return exit::SYNTAX_ERROR;
                }
            }
        }
        _ => None,
    };
    let tree = parsed.as_ref().map(|p| (p, source.unwrap_or_default()));
    // Does a later run have something to say again? The check's own
    // warnings, and a pin's when the pin is the script's own line.
    let mut warned = false;
    // A program was checked when it was compiled (`no_check`); a module
    // has no tree to check.
    if let (false, Some((program, source))) = (args.no_check, tree) {
        let check_t = t.start();
        let mut rep = check::check(program, &[]);
        warned = lowered_warned(&args.file, source, program, &rep);
        rep.warnings
            .extend(pin_warnings(program, pin.as_ref(), &name));
        t.end("check", check_t);
        if !rep.ok() {
            if ctx.json {
                for e in &rep.errors {
                    report(ctx, e, Some(&name), None);
                }
            } else {
                report_all(ctx, &rep.errors, &name, source);
            }
            return exit::SYNTAX_ERROR;
        }
        if !ctx.json {
            for w in &rep.warnings {
                eprint!(
                    "{}",
                    ctx.yellow(
                        &w.render(Some(&name), Some(source))
                            .replacen("error[", "warning[", 1)
                    )
                );
            }
        }
    } else if let Some((program, _)) = tree {
        // Unchecked here (a compiled program, `--interp`, `--no-check`): its
        // names are still resolved, by the check's own walk without its
        // diagnostics, so the tree-walker reads them by position as it does
        // after the check.
        let resolve_t = t.start();
        check::resolve(program);
        t.end("resolve", resolve_t);
    }
    // The checked script's module: the one the tin kept, else lowered now
    // and kept for the next run. A script the lowering does not take yet
    // runs on the tree-walker, as does one whose module will not load; the
    // run's record says which engine ran and why. No script fails to run
    // because of the VM.
    if let (true, None, Some((program, _))) = (wants_module, &module, tree) {
        if let Some(bytes) = kept.take() {
            // The lookup found a module the check meant to check again
            // (warnings, or a version-file pin not in the script). The
            // check has run by now; open the kept bytes, timed as
            // `compile-load` only.
            let load_t = t.start();
            let opened = open_module(bytes, "the kept module");
            t.end("compile-load", load_t);
            if let Ok(m) = opened {
                module = Some(m);
                chosen.module = Some("cached");
            }
            // If the open failed (a damaged body): fall through to
            // lower fresh, as a missing module would. The healing path
            // is the same the vm engine took before `kept` was split
            // from `safe_to_use_cached`.
        }
        if module.is_none() {
            // The module is named by what is in it, not by where the
            // script lies: the same script anywhere is the same module.
            // Each stage of the lowering is one phase under `--timings`
            // (`compile-cir`, `compile-passes`, `compile-bytecode`,
            // `compile-verify`, `compile-load`); `compile-keep` here
            // times the store.
            let timings = if t.active() { Some(t) } else { None };
            match crate::lower::module_loaded_timed(program, "", timings) {
                Ok((bytes, m)) => {
                    if let Some(key) = &key {
                        // A module that cannot be kept is lowered again
                        // next time; the run goes on either way.
                        let keep_t = t.start();
                        let _ = modcache::store(key, &bytes, warned);
                        t.end("compile-keep", keep_t);
                    }
                    // The module as the lowering loaded it to check it:
                    // the bytes are for the next run.
                    module = Some(m);
                    chosen.module = Some("lowered");
                }
                Err(crate::lower::Failure::Refused(what)) => {
                    let reason = format!("the lowering does not take it yet: {what}");
                    if let Some(key) = &key {
                        let _ = modcache::store_refusal(key, &reason);
                    }
                    chosen.why_tree = Some(reason);
                }
                Err(crate::lower::Failure::Internal(e)) => {
                    let reason = format!("the lowering failed on it: {e}");
                    if let Some(key) = &key {
                        let _ = modcache::store_refusal(key, &reason);
                    }
                    chosen.why_tree = Some(reason);
                }
                Err(crate::lower::Failure::Crashed(e)) => {
                    // The crash hook has said what broke and where its
                    // report is; this says what happens to the run.
                    eprintln!(
                        "  = the lowering is what crashed: this run goes on, on the tree-walker"
                    );
                    chosen.why_tree = Some(format!("the lowering crashed on it: {e}"));
                }
            }
        }
    }
    // Under `--engine auto`, with no module and no refusal cached, the
    // tree-walker runs it now and the lowering is left to a `__lower`
    // subprocess (`on`), or waited for (`wait`), or skipped (`off`). The
    // spawn itself happens after the real work; this marks it.
    let after_lower = match plan {
        ChoiceOutcome::TreeWalkerThenLower => Some(AutoLower::On),
        ChoiceOutcome::TreeWalkerThenLowerWait => Some(AutoLower::Wait),
        _ => None,
    };

    // Everything between the check and the first line of the script: the
    // trial's plan comparison, the run's record and the kernel.
    let begin_t = t.start();
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
        config.get("trial")
    };
    let mut trial_note = None;
    let last_good = match source {
        Some(source)
            if compiled.is_none()
                && !args.dry_run
                && !pin::dispatched()
                && trial_mode != "never" =>
        {
            let (handed, note, last) =
                trial_before_run(ctx, &args, source, parsed.as_ref(), &name, &trial_mode);
            if let Some(code) = handed {
                return code;
            }
            trial_note = note;
            last
        }
        _ => None,
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
    let mut record = RunRecord::begin_hashed(
        &args.file,
        script_sha.unwrap_or_else(|| sha256("")),
        &args.args,
        if args.dry_run { "dry-run" } else { "run" },
    );
    record.engine = chosen.note(module.is_some(), compiled.is_some());
    if let Some(p) = compiled {
        // The script's sha256 as the program's record gives it, at either
        // stage: `cig runs` and `--unburn` see one program.
        record.script_sha256 = p.sha256().to_string();
        record.script = p.name().to_string();
        let mut v = p.summary();
        v["binary"] = serde_json::json!(std::env::current_exe()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default());
        record.program = Some(v);
    }
    // The record is written now, so `cig runs` lists the run while it
    // runs, but not flushed: a run that burns nothing makes nothing
    // durable. The first burn flushes the record, the run's folder and the
    // index before anything else it does (`Kernel::new_run`).
    let run_dir = if args.dry_run {
        None
    } else {
        if let Err(e) = record.save_unsynced() {
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
    let kernel = match run_dir {
        Some(dir) => Kernel::new_run(mode, dir),
        None => Kernel::new(mode, None),
    };
    let kernel = match kernel {
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

    t.end("begin", begin_t);
    let run_t = t.start();
    // The module on the VM when the run has one, else the tree on the
    // tree-walker. One of the two is always there: a script without a
    // module was parsed.
    let mut result = match (&module, &parsed) {
        (Some(module), _) => crate::vm::run_script(&mut interp, module),
        (None, Some(program)) => interp.run_program(program),
        (None, None) => Err(Diagnostic::new(
            Kind::Internal,
            "the run has neither a module nor a tree",
        )
        .code("E900")),
    };
    if result.is_ok() {
        if let Some(chain_name) = &chain {
            result = light_named_chain(&mut interp, chain_name, || match &parsed {
                Some(program) => Ok(super::chains::declared(program)),
                None => {
                    let text = script.text()?;
                    parse(text)
                        .map(|p| super::chains::declared(&p))
                        .map_err(|d| format!("its script does not parse: {}", d.message))
                }
            });
        }
    }
    let _ = interp.out.flush();
    t.end("run", run_t);
    // The burn phase: what the run leaves behind once the program has ended
    // (a rollback, the record, the journal's head, the ledger, the report).
    let burn_t = t.start();

    let mut code = match &result {
        Ok(()) => interp.exit_requested.unwrap_or(exit::OK),
        // A check-family error reached at run time (`--no-check`, or a
        // name the checker could not see) exits like the checker would.
        Err(d) if d.kind == crate::diagnostics::Kind::Check => exit::SYNTAX_ERROR,
        Err(_) => exit::SCRIPT_ERROR,
    };
    let mut rolled_back = None;
    if let Err(d) = &result {
        // The quoted line comes from the script's text; a stage `b`
        // program whose `source` section does not match still reports its
        // own error, without the line, and says why.
        match script.text() {
            Ok(text) => report(ctx, d, Some(&name), Some(text)),
            Err(e) => report_noted(
                ctx,
                d,
                Some(&name),
                &format!(
                    "the line is not shown: this program's source section is damaged (E1050): {e}"
                ),
            ),
        }
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
        // A run that burned is finished durably, as it always was; one that
        // burned nothing is finished as it began, unflushed.
        let durable = interp.kernel.journal().record_durable();
        let save = |r: &RunRecord| {
            if durable {
                r.save()
            } else {
                r.save_unsynced()
            }
        };
        if let Err(e) = save(&record) {
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
                        "label": format!("{} (run {})", p.name(), record.id),
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
                let _ = save(&record);
                humidor::touch(last);
                let fallback = match humidor::dispatch_run(bin, args.engine.map(EngineArg::as_str))
                {
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
    t.end("burn", burn_t);
    // Under `--engine auto` the lowering happens after the script's own
    // work, so nothing it prints or burns waits on it. A spawn that fails
    // changes nothing about the run (`on`); `wait` is the test form.
    if let Some(kind) = after_lower {
        match kind {
            AutoLower::On => spawn_background_lower(&args.file),
            AutoLower::Wait => lower_in_foreground(&args.file),
            AutoLower::Off => {}
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
/// `declared` lists the script's top-level chains for the hint when the
/// name is not one; it is asked for only then.
fn light_named_chain(
    interp: &mut Interp,
    name: &str,
    declared: impl FnOnce() -> Result<Vec<super::chains::ChainInfo>, String>,
) -> Result<(), Diagnostic> {
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
            d = match declared() {
                Ok(c) if c.is_empty() => d.with_hint("declare one with: chain name { step, step }"),
                Ok(c) => d.with_hint(format!(
                    "chains here: {}",
                    c.into_iter().map(|c| c.name).collect::<Vec<_>>().join(", ")
                )),
                Err(e) => d.with_hint(format!(
                    "its chains cannot be listed: this program's source section is damaged (E1050): {e}"
                )),
            };
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
    program: Option<&Program>,
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
    // The plan here is the tree-walker's, from the script's tree: the
    // run's own when it parsed one, else parsed now (a run that started
    // from its module in the tin has none). The script passed the check
    // when its module was made, so its names are resolved as an unchecked
    // run's are.
    let parsed_now;
    let program = match program {
        Some(p) => p,
        None => match parse(source) {
            Ok(p) => {
                check::resolve(&p);
                parsed_now = p;
                &parsed_now
            }
            Err(d) => {
                eprintln!("{} {}", ctx.yellow("trial skipped:"), d.message);
                return (
                    None,
                    Some(serde_json::json!({"last_good": last.to_string(), "skipped": d.message})),
                    Some((last, bin)),
                );
            }
        },
    };
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
    match humidor::dispatch_run(&bin, args.engine.map(EngineArg::as_str)) {
        Ok(code) => (Some(code), None, None),
        Err(e) => {
            eprintln!("{} {e}", ctx.red("error[E807 usage]:"));
            (Some(exit::USAGE), None, None)
        }
    }
}

#[cfg(test)]
mod engine_tests {
    use super::*;

    /// The choice table: for every cell of (engine asked, cache state,
    /// auto_lower), what does the run do?
    #[test]
    fn choose_covers_every_cell_of_the_engine_table() {
        // no_check: always the tree-walker, before the cache or
        // auto_lower is read, whichever engine was asked for.
        for engine in [EngineArg::Tree, EngineArg::Vm, EngineArg::Auto] {
            for (cache_mod, cache_ref) in [(false, false), (true, false), (false, true)] {
                for auto in [AutoLower::On, AutoLower::Off, AutoLower::Wait] {
                    assert_eq!(
                        choose(engine, true, cache_mod, cache_ref, auto),
                        ChoiceOutcome::TreeWalker,
                        "no_check keeps the tree-walker for {engine:?} / mod={cache_mod} ref={cache_ref} auto={auto:?}"
                    );
                }
            }
        }
        // tree: always the tree-walker, whatever the cache says.
        for (cache_mod, cache_ref) in [(false, false), (true, false), (false, true)] {
            for auto in [AutoLower::On, AutoLower::Off, AutoLower::Wait] {
                assert_eq!(
                    choose(EngineArg::Tree, false, cache_mod, cache_ref, auto),
                    ChoiceOutcome::TreeWalker
                );
            }
        }
        // vm: today's behaviour.
        assert_eq!(
            choose(EngineArg::Vm, false, false, false, AutoLower::On),
            ChoiceOutcome::VmLoweringNow
        );
        assert_eq!(
            choose(EngineArg::Vm, false, true, false, AutoLower::On),
            ChoiceOutcome::VmCached
        );
        // A refusal cached: `vm` still lowers now (and finds the same
        // refusal, which falls back to the tree-walker in the run path).
        // The choose() table says "lower now"; the lowering call that
        // follows reads the refusal path.
        assert_eq!(
            choose(EngineArg::Vm, false, false, true, AutoLower::On),
            ChoiceOutcome::VmLoweringNow
        );
        // auto: cache first, then auto_lower.
        assert_eq!(
            choose(EngineArg::Auto, false, true, false, AutoLower::On),
            ChoiceOutcome::VmCached
        );
        assert_eq!(
            choose(EngineArg::Auto, false, false, true, AutoLower::On),
            ChoiceOutcome::TreeWalker
        );
        assert_eq!(
            choose(EngineArg::Auto, false, false, false, AutoLower::On),
            ChoiceOutcome::TreeWalkerThenLower
        );
        assert_eq!(
            choose(EngineArg::Auto, false, false, false, AutoLower::Off),
            ChoiceOutcome::TreeWalker
        );
        assert_eq!(
            choose(EngineArg::Auto, false, false, false, AutoLower::Wait),
            ChoiceOutcome::TreeWalkerThenLowerWait
        );
    }

    /// `lowered_warned` returns true for an in-script pin that warns
    /// (a pin older than this cig that the script uses a feature past).
    /// The check's own warnings would be its only source without the
    /// pin walk, so a build sharing one function for both paths is the
    /// only way the `__lower` child and the run path agree.
    #[test]
    fn lowered_warned_sees_an_in_script_pin_warning() {
        use crate::check;
        use crate::syntax::parse;
        // A header pin on `cig 1.0`: a script that uses `finally` (a
        // 1.1.0 addition, see `src/since.rs`'s table) warns under E309.
        let src = "# cig 1.0\ntry { exhale 1 } finally { exhale 2 }\n";
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("p.cig");
        std::fs::write(&path, src).unwrap();
        let program = parse(src).unwrap();
        let rep = check::check(&program, &[]);
        // No check warning here (the use is valid in 1.25.0); the pin
        // is what warns, and `lowered_warned` must pick it up.
        assert!(rep.warnings.is_empty(), "the check itself says nothing");
        let warned = lowered_warned(&path, src, &program, &rep);
        assert!(
            warned,
            "a pin older than this cig that the script outgrows must set warned"
        );
    }

    /// `lower_command` runs cig with `__lower <script>`, and
    /// `prepare_detached` closes stdio and gives the child its own
    /// process group. The first is read from the configured command
    /// without starting it; the second runs a small `/bin/sh` command
    /// under the same `prepare_detached` and reads `/proc/self/fd/*`
    /// and the pgid back.
    #[test]
    fn lower_command_runs_cig_lower_with_the_script_path() {
        let script = std::path::Path::new("/tmp/script-does-not-need-to-exist.cig");
        let cmd = lower_command(script).expect("self_command is ok under tests");
        let program = cmd.get_program().to_string_lossy().to_string();
        assert!(!program.is_empty(), "program path is set");
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(args[0], "__lower");
        assert_eq!(args[1], script.to_string_lossy());
    }

    /// `is_regular_file` follows symlinks (`fs::metadata`, not
    /// `symlink_metadata`), so a script reached through `~/bin/x.cig`
    /// as a link is lowered; a directory, a missing path, or anything
    /// else that is not a regular file is refused.
    #[test]
    fn is_regular_file_follows_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real.cig");
        std::fs::write(&real, "exhale 1\n").unwrap();
        assert!(is_regular_file(&real), "a real file is a regular file");
        #[cfg(unix)]
        {
            let link = tmp.path().join("link.cig");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            assert!(
                is_regular_file(&link),
                "a symlink to a file is a regular file"
            );
        }
        assert!(
            !is_regular_file(tmp.path()),
            "a directory is not a regular file"
        );
        assert!(
            !is_regular_file(&tmp.path().join("missing")),
            "a missing path is not a regular file"
        );
    }

    /// `prepare_detached` closes fd 0, 1 and 2 against /dev/null and
    /// starts the child in its own process group. Spawn a small shell
    /// under the same settings; the shell opens a new fd 3 to the
    /// output file (so the readlink of /dev/null on fd 1 is still true
    /// when it is read) and writes what its `/proc` fds point to and
    /// its pgid there.
    #[cfg(target_os = "linux")]
    #[test]
    fn prepare_detached_closes_stdio_and_starts_a_new_process_group() {
        let tmp = tempfile::tempdir().unwrap();
        let out_path = tmp.path().join("out");
        // `readlink /proc/self/fd/N` reads readlink's own fd (opened
        // with redirection by sh): use `$$` to read sh's. The pipe
        // keeps the writer out of the way: `readlink` writes to its
        // own stdout, captured by sh's `$()`, then written to fd 3.
        let script = format!(
            "exec 3>{out}\n\
             echo $(readlink /proc/$$/fd/0) >&3\n\
             echo $(readlink /proc/$$/fd/1) >&3\n\
             echo $(readlink /proc/$$/fd/2) >&3\n\
             ps -o pgid= -p $$ >&3\n",
            out = out_path.display()
        );
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c").arg(&script);
        prepare_detached(&mut cmd);
        let status = cmd.status().expect("sh ran");
        assert!(status.success(), "sh exited {status:?}");
        let out = std::fs::read_to_string(&out_path).expect("output");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 4, "four lines: {out}");
        for (i, l) in lines.iter().take(3).enumerate() {
            assert_eq!(l.trim(), "/dev/null", "fd {i} is not /dev/null: {l:?}");
        }
        // The last line is `ps`'s pgid, with leading whitespace. The
        // parent's pgid (this test's own) must differ: `process_group(0)`
        // puts the child in a new group of its own.
        let child_pgid: i32 = lines[3].trim().parse().expect("pgid is a number");
        // SAFETY: getpgrp is a safe, no-arg syscall.
        let parent_pgid = unsafe { libc::getpgrp() };
        assert_ne!(
            child_pgid, parent_pgid,
            "the child kept the parent's process group"
        );
    }
}
