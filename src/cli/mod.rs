// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! The `cig` command line.

/// `println!` that never panics when stdout is a closed pipe.
macro_rules! outln {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout(), $($arg)*);
    }};
}

mod bench;
mod boxcmd;
mod chains;
mod compiled;
mod config;
mod crash;
mod doctor;
mod explain;
mod forward;
mod language;
mod parcel;
mod pin;
mod repl;
mod report;
mod run;
mod runs;
mod tin;
mod update;
mod versions;

/// Diagnostics as `cig` prints them (the banner, the render, doctor's
/// diagnosis, `--json`), for tools built on the library such as cigcompile.
pub use run::{report, report_all};

use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

/// Exit codes, stable for automation.
pub mod exit {
    pub const OK: i32 = 0;
    pub const SCRIPT_ERROR: i32 = 1;
    pub const SYNTAX_ERROR: i32 = 2;
    pub const USAGE: i32 = 3;
}

#[derive(Parser)]
#[command(
    name = "cig",
    version,
    about = "CigScript: nothing real happens unless you burn",
    long_about = None,
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Machine-readable output on stdout where a command supports it.
    #[arg(long, global = true)]
    pub json: bool,
    /// Never use colour (also honoured: NO_COLOR).
    #[arg(long, global = true)]
    pub no_color: bool,
    /// Do not run doctor's automatic diagnosis on errors (also: CIG_DOCTOR=0).
    #[arg(long, global = true)]
    pub no_doctor: bool,
    /// Same codes, spans, hints and facts, without the catchphrases (also: CIG_PLAIN=1).
    #[arg(long, global = true)]
    pub plain: bool,
    /// Read the tin's key from this open descriptor, the 64 hex characters a key file holds (Unix; for CI: the key in no file and no environment variable).
    #[arg(long, global = true, value_name = "N")]
    pub seal_key_fd: Option<i32>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run a script for real: every burn journaled, rolled back if it fails (execute).
    Run(RunArgs),
    /// Measure cases from `bench/bench.toml`: samples in batches, median, p10 to p90, CV, peak RSS (benchmark).
    Bench(BenchArgs),
    /// Typos, sticks, effects: refuse before anything runs (static analysis).
    Check {
        /// Script to check.
        file: PathBuf,
        /// Exit non-zero when there are warnings (for CI).
        #[arg(long)]
        deny_warnings: bool,
    },
    /// Light a chain: each stick lit from the last, timed, stopping at the first failure (task runner).
    Light(LightArgs),
    /// What can I light? The chains a script declares, without running it.
    Chains {
        /// Script to inspect.
        file: PathBuf,
    },
    /// Evaluate a snippet and print its value.
    Eval {
        /// CigScript source, e.g. `[1,2,3].map(pack(x) => x * 2)`.
        code: String,
    },
    /// Interactive session (read, eval, print, loop).
    Repl,
    /// The lab notebook: every run, its status, its burns; or one run with its journal (run history).
    Runs {
        /// A run id (or unique prefix) to show in detail.
        id: Option<String>,
        /// Delete run records older than the newest N.
        #[arg(long, value_name = "N")]
        prune: Option<usize>,
        /// With an id: walk the journal's hash chain, compare its head with the record, and check every snapshot and tin object it needs. Without one (#149): compare each run's chain head with the ledger (the whole history).
        #[arg(long)]
        verify: bool,
        /// With --verify and no id: walk each run's full journal chain (slower; `--verify` alone just checks the heads).
        #[arg(long, requires = "verify")]
        deep: bool,
        /// Stop noting that this run (listed in the ledger) is missing from runs/: it was removed on purpose.
        #[arg(long, value_name = "ID")]
        forget: Option<String>,
        /// Keep this run whatever keep_runs, keep_days and --prune say (the run before a migration or a release).
        #[arg(long, value_name = "ID")]
        pin: Option<String>,
        /// Let a pinned run go again.
        #[arg(long, value_name = "ID")]
        unpin: Option<String>,
        /// The runs pruned or forgotten, from the ledger: when, how, and what each was.
        #[arg(long, conflicts_with_all = ["id", "prune", "pin", "unpin", "forget"])]
        pruned: bool,
        /// Rebuild the run index from the run folders (#178): fixes a torn or oversized runs-index.jsonl by hand.
        #[arg(long, conflicts_with_all = ["id", "prune", "pin", "unpin", "forget", "pruned", "verify", "deep"])]
        reindex: bool,
    },
    /// Put the world back the way it was, newest op first (rollback).
    Unburn {
        /// Run id or unique prefix.
        #[arg(required_unless_present_any = ["last", "since", "to", "resume"])]
        id: Option<String>,
        /// Show what would be restored without touching anything.
        #[arg(long)]
        dry_run: bool,
        /// Restore even if this run was rolled back already or a snapshot is missing.
        #[arg(long)]
        force: bool,
        /// Put back what this run's rollback moved into the tin: files saved while a hop ran, or the hop's own output (--dry-run previews it, --force writes over a path changed since).
        #[arg(long)]
        give_back: bool,
        /// Restore from a journal signed by another key (another machine's, or one since replaced); its objects are still checked by hash.
        #[arg(long)]
        accept_unsigned: bool,
        /// Undo the newest N runs that changed something, newest first: one plan, then run by run.
        #[arg(long, value_name = "N", conflicts_with_all = ["id", "since", "to", "resume", "give_back"])]
        last: Option<usize>,
        /// Undo every run started within this long (90s, 30m, 2h, 3d, 1w), newest first.
        #[arg(long, value_name = "AGE", conflicts_with_all = ["id", "to", "resume", "give_back"])]
        since: Option<String>,
        /// Undo every run after this one, newest first; this one stays.
        #[arg(long, value_name = "ID", conflicts_with_all = ["id", "resume", "give_back"])]
        to: Option<String>,
        /// With --last, --since or --to: only the runs whose pack is this folder, or inside it, or holds it.
        #[arg(long, value_name = "DIR")]
        pack: Option<std::path::PathBuf>,
        /// Carry on with the stretch that stopped, once what stopped it is resolved.
        #[arg(long = "continue", conflicts_with_all = ["id", "give_back"])]
        resume: bool,
        /// Do not ask before a stretch, even at a terminal.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Look the install over, or diagnose a past run again; asks before it fixes anything (diagnostics).
    Doctor {
        /// Offer to repair what can be repaired, asking before each change.
        #[arg(long)]
        fix: bool,
        /// A run id (or prefix): diagnose that run's recorded error again, read-only.
        run: Option<String>,
    },
    /// The whole language and library on one screen (reference).
    Language,
    /// The long version of an error code: which kind of no, what to type next (error documentation).
    Explain {
        /// A code such as E502.
        code: Option<String>,
        /// Print the JSON Schema for a diagnostic (generated from the registry).
        #[arg(long)]
        schema: bool,
    },
    /// Show or change the few settings there are, in ~/.cigscript/config.
    Config {
        /// Key to read or set.
        key: Option<String>,
        /// New value.
        value: Option<String>,
        /// Remove the key so the default applies.
        #[arg(long)]
        unset: bool,
    },
    /// Crash reports: list, show, send, delete; nothing leaves without a yes.
    Crash {
        #[command(subcommand)]
        command: Option<crash::CrashCommand>,
    },
    /// Bundle a run for a bug report: plan, journal, diagnostic and doctor's diagnosis, redacted (reproducible report).
    Report {
        /// The run id, or a unique prefix of it.
        id: String,
        /// Write here instead of ~/.cigscript/reports/<id>.json; `-` prints to stdout.
        #[arg(long)]
        out: Option<PathBuf>,
        /// File the bundle on the issue tracker through the ashtray, as a crash report is filed.
        #[arg(long, conflicts_with = "out")]
        send: bool,
    },
    /// The humidor: every version of cig kept for pinned scripts and old journals; list, add, remove, prune.
    Versions {
        #[command(subcommand)]
        command: Option<versions::VersionsCommand>,
    },
    /// Write the version a script was written for into its header (`# cig 1.2`), so a newer cig hands it to that version.
    Pin {
        /// The script to pin.
        file: PathBuf,
        /// The constraint to write: 1.2 (any 1.2.x), 1.2.0 (exactly), >=1.2.0; default the installed major.minor.
        #[arg(long, value_name = "CONSTRAINT")]
        to: Option<String>,
    },
    /// Pack a run's record, the tin objects it needs and its compensations into one file, for `cig unburn` on another machine.
    Parcel {
        /// The run id, or a unique prefix of it.
        id: String,
        /// Write here instead of ./<run>.cigparcel.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Carry the tin's seal key inside the parcel too, so a sealed run restores on a machine with no key of its own. The parcel is then a secret: anyone with the file can read its sealed objects.
        #[arg(long)]
        with_key: bool,
    },
    /// Import a parcel: every file checked against its manifest, the pack's roots mapped to where they are here.
    Unparcel {
        /// The .cigparcel file.
        file: PathBuf,
        /// Where a root of the pack lives here: name=path (the name is the root's last component; --show lists them).
        #[arg(long, value_name = "NAME=PATH")]
        root: Vec<String>,
        /// The run's working directory here, when it cannot be told from the roots.
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Import even the sealed objects this machine's key cannot open.
        #[arg(long)]
        allow_sealed_missing: bool,
        /// A parcel written with --with-key carries its own sealing key: accept it and re-seal under this machine's key on the way in. Without this flag, such a parcel is refused.
        #[arg(long)]
        accept_key: bool,
        /// Only list what the parcel holds and the roots to map.
        #[arg(long)]
        show: bool,
    },
    /// The layers hops kept rather than applied: list, apply (as a new run), discard.
    Box {
        #[command(subcommand)]
        command: Option<boxcmd::BoxCommand>,
    },
    /// The forwarder inside a boxed hop's network namespace (started by cig itself).
    #[command(hide = true, name = "internal-forward")]
    InternalForward { sock: std::path::PathBuf, port: u16 },
    /// The tin: the before-bytes of files hops may change; du, verify, gc.
    Tin {
        #[command(subcommand)]
        command: Option<tin::TinCommand>,
    },
    /// Get the newer binary from the repo, verified before it is installed (self-update).
    Update {
        /// Only report whether a newer release exists.
        #[arg(long)]
        check: bool,
        /// Permit installing an older release.
        #[arg(long)]
        allow_downgrade: bool,
        /// Install this release instead of the newest.
        #[arg(long, value_name = "VERSION")]
        to: Option<String>,
    },
}

#[derive(Args)]
pub struct BenchArgs {
    /// The manifest to read; default `bench/bench.toml`.
    #[arg(long, value_name = "FILE")]
    pub manifest: Option<PathBuf>,
    /// Only cases whose name contains this substring.
    #[arg(long, value_name = "NAME")]
    pub only: Option<String>,
    /// Only cases in this group.
    #[arg(long, value_name = "GROUP")]
    pub group: Option<String>,
    /// Override `[defaults].samples`.
    #[arg(long, value_name = "N")]
    pub samples: Option<usize>,
    /// Override `[defaults].per`.
    #[arg(long, value_name = "N")]
    pub per: Option<usize>,
    /// Override `[defaults].warmup`.
    #[arg(long, value_name = "N")]
    pub warmup: Option<usize>,
    /// A Markdown table instead of the plain one.
    #[arg(long)]
    pub markdown: bool,
}

#[derive(Args)]
pub struct RunArgs {
    /// Script to run.
    pub file: PathBuf,
    /// Simulate every burn and print the plan instead of executing it.
    #[arg(long)]
    pub dry_run: bool,
    /// Leave burns in place if the script fails.
    #[arg(long)]
    pub no_rollback: bool,
    /// Skip the static check before running.
    #[arg(long)]
    pub no_check: bool,
    /// Step budget before "your loop never ends" (E515); 0 disables it (also: CIG_MAX_STEPS).
    #[arg(long, value_name = "N")]
    pub max_steps: Option<u64>,
    /// Compare this cig's plan with the script's last good version before running, fetching that version if needed.
    #[arg(long, conflicts_with = "no_trial")]
    pub trial: bool,
    /// Run here without comparing plans with the last good version.
    #[arg(long)]
    pub no_trial: bool,
    /// Arguments passed to the script as `args`.
    #[arg(last = true)]
    pub args: Vec<String>,
}

#[derive(Args)]
pub struct LightArgs {
    /// Script that declares the chain.
    pub file: PathBuf,
    /// Name of the chain to light.
    pub chain: String,
    /// Simulate every burn and print the plan instead of executing it.
    #[arg(long)]
    pub dry_run: bool,
    /// Leave burns in place if the chain fails.
    #[arg(long)]
    pub no_rollback: bool,
    /// Skip the static check before running.
    #[arg(long)]
    pub no_check: bool,
    /// Step budget before "your loop never ends" (E515); 0 disables it (also: CIG_MAX_STEPS).
    #[arg(long, value_name = "N")]
    pub max_steps: Option<u64>,
    /// Compare this cig's plan with the script's last good version before running, fetching that version if needed.
    #[arg(long, conflicts_with = "no_trial")]
    pub trial: bool,
    /// Run here without comparing plans with the last good version.
    #[arg(long)]
    pub no_trial: bool,
    /// Arguments passed to the script as `args`.
    #[arg(last = true)]
    pub args: Vec<String>,
}

/// Every word the parser knows (subcommands, aliases, flags, the values of
/// an enumerated option), so a report's command line can keep them and
/// reduce everything else to its shape.
fn known_words(cmd: &clap::Command, out: &mut Vec<String>) {
    out.push(cmd.get_name().to_string());
    out.extend(cmd.get_all_aliases().map(str::to_string));
    for a in cmd.get_arguments() {
        if let Some(l) = a.get_long() {
            out.push(format!("--{l}"));
        }
        if let Some(s) = a.get_short() {
            out.push(format!("-{s}"));
        }
        for pv in a.get_possible_values() {
            out.push(pv.get_name().to_string());
        }
    }
    for sub in cmd.get_subcommands() {
        known_words(sub, out);
    }
}

/// What `cig` does when it starts: the program appended to this binary
/// runs, when one is there (see [`crate::program`]); anything else is a
/// command. `src/main.rs` is only this.
pub fn entry() -> i32 {
    // The mark itself, not its length (a constant LTO folds): its bytes
    // stay in the binary for cigcompile to read.
    std::hint::black_box(crate::program::RUNTIME_MARK);
    crate::crash::install_hook();
    let program = crate::program::own();
    if program.is_none() {
        crate::update::cleanup_old();
    }
    // Scripts recurse; give the evaluator a generous stack.
    let handle = std::thread::Builder::new()
        .name("cig".into())
        .stack_size(256 * 1024 * 1024)
        .spawn(move || {
            // Test hook for the crash reporter: `1` panics here, `eval` inside
            // the evaluator, `after-burn` in the kernel after the first burn.
            if let Ok(v) = std::env::var("CIG_INTERNAL_PANIC") {
                if v != "eval" && v != "after-burn" {
                    panic!("deliberate test panic (CIG_INTERNAL_PANIC is set)");
                }
            }
            match program {
                None => main(),
                Some(Ok(p)) => compiled::main(p),
                Some(Err(why)) => compiled::refuse(&why),
            }
        })
        .expect("could not start the interpreter thread");
    match handle.join() {
        Ok(code) => code,
        Err(_) => {
            after_crash();
            70
        }
    }
}

pub fn main() -> i32 {
    {
        use clap::CommandFactory;
        let mut words = Vec::new();
        known_words(&Cli::command(), &mut words);
        crate::crash::know_words(words);
    }
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            // --help and --version are answers, not errors. A malformed
            // invocation is a usage problem and exits like one (3), not
            // with the parser's own 2, which here means a script that
            // failed to parse or check.
            let code = if e.use_stderr() {
                exit::USAGE
            } else {
                exit::OK
            };
            let _ = e.print();
            return code;
        }
    };
    let color = !cli.no_color && std::env::var_os("NO_COLOR").is_none() && stderr_is_tty();
    let ctx = Ctx {
        json: cli.json,
        color,
        doctor: !cli.no_doctor && crate::doctor::enabled_by_env(),
        plain: cli.plain || plain_by_env(),
    };
    // A pinned script is lit by the cig it asks for, whatever is installed.
    let pinned_file = match &cli.command {
        Command::Run(a) => Some((a.file.clone(), if a.dry_run { "dry run" } else { "run" })),
        Command::Light(a) => Some((a.file.clone(), "light")),
        Command::Check { file, .. } => Some((file.clone(), "check")),
        Command::Chains { file } => Some((file.clone(), "chains")),
        _ => None,
    };
    if let Some((file, verb)) = pinned_file {
        if let Some(code) = pin::dispatch_if_pinned(&ctx, &file, verb) {
            return code;
        }
    }
    // After a pinned script went to its own cig, which reads the
    // descriptor itself; before anything asks for the key.
    if let Some(fd) = cli.seal_key_fd {
        if let Some(code) = take_key_fd(&ctx, fd) {
            return code;
        }
    }
    // Not after the commands that just tried a send themselves: a second
    // try in the same breath would only spend a retry.
    let flush_after = !matches!(
        cli.command,
        Command::Crash { .. } | Command::Report { send: true, .. }
    );
    let code = dispatch(ctx, cli.command);
    // What the ashtray holds goes in the background, after the work: a run
    // never waits on the network.
    if flush_after {
        crate::ashtray::spawn_flush(&crate::config::Config::load());
    }
    code
}

/// `--seal-key-fd N`: the key read from the descriptor, or a usage error.
fn take_key_fd(ctx: &Ctx, fd: i32) -> Option<i32> {
    let e = crate::burn::seal::take_key_from_fd(fd).err()?;
    let d = crate::diagnostics::usage(format!("--seal-key-fd {fd}: {e}"))
        .with_hint("hand the key over as cig --seal-key-fd 3 run s.cig 3<key.hex, or name a key file with CIG_SEAL_KEY_FILE");
    run::report(ctx, &d, None, None);
    Some(exit::USAGE)
}

fn dispatch(ctx: Ctx, command: Command) -> i32 {
    match command {
        Command::Run(args) => run::run(&ctx, args, None),
        Command::Bench(a) => bench::bench(
            &ctx,
            bench::Opts {
                manifest: a.manifest,
                only: a.only,
                group: a.group,
                samples: a.samples,
                per: a.per,
                warmup: a.warmup,
                markdown: a.markdown,
            },
        ),
        Command::Light(a) => run::run(
            &ctx,
            RunArgs {
                file: a.file,
                dry_run: a.dry_run,
                no_rollback: a.no_rollback,
                no_check: a.no_check,
                max_steps: a.max_steps,
                trial: a.trial,
                no_trial: a.no_trial,
                args: a.args,
            },
            Some(a.chain),
        ),
        Command::Chains { file } => chains::chains(&ctx, &file),
        Command::Check {
            file,
            deny_warnings,
        } => run::check(&ctx, &file, deny_warnings),
        Command::Eval { code } => run::eval(&ctx, &code),
        Command::Repl => repl::repl(&ctx),
        Command::Runs {
            id,
            prune,
            verify,
            deep,
            forget,
            pin,
            unpin,
            pruned,
            reindex,
        } => match (pin, unpin) {
            _ if pruned => runs::pruned(&ctx),
            _ if reindex => runs::reindex(&ctx),
            (Some(id), _) => runs::pin(&ctx, &id, true),
            (None, Some(id)) => runs::pin(&ctx, &id, false),
            _ => runs::runs(&ctx, id, prune, verify, deep, forget),
        },
        Command::Unburn {
            id,
            dry_run,
            force,
            give_back,
            accept_unsigned,
            last,
            since,
            to,
            pack,
            resume,
            yes,
        } => {
            let how = match (id, last, since, to, resume) {
                (Some(id), ..) if give_back => return runs::give_back(&ctx, &id, dry_run, force),
                (Some(id), ..) => return runs::unburn(&ctx, &id, dry_run, force, accept_unsigned),
                (None, Some(n), ..) => runs::Stretch::Last(n),
                (None, None, Some(age), ..) => match runs::parse_age(&age) {
                    Some(d) => runs::Stretch::Since(d),
                    None => {
                        let d = crate::diagnostics::usage(format!("`--since {age}` is not an age"))
                            .code("E800")
                            .with_hint("a number and a unit: 90s, 30m, 2h, 3d or 1w");
                        run::report(&ctx, &d, None, None);
                        return exit::USAGE;
                    }
                },
                (None, None, None, Some(to), _) => runs::Stretch::To(to),
                _ => runs::Stretch::Continue,
            };
            runs::unburn_stretch(
                &ctx,
                how,
                pack.as_deref(),
                runs::StretchOpts {
                    dry_run,
                    force,
                    accept_unsigned,
                    yes,
                    only_program: None,
                },
            )
        }
        Command::Doctor { fix, run } => match run {
            Some(id) => doctor::rediagnose(&ctx, &id),
            None => doctor::doctor(&ctx, fix),
        },
        Command::Language => language::language(&ctx),
        Command::Explain { code, schema } => explain::explain(&ctx, code, schema),
        Command::Config { key, value, unset } => config::config(&ctx, key, value, unset),
        Command::Crash { command } => crash::crash(&ctx, command),
        Command::Report { id, out, send } => report::report(&ctx, &id, out, send),
        Command::Update {
            check,
            allow_downgrade,
            to,
        } => update::update(&ctx, check, allow_downgrade, to),
        Command::Versions { command } => versions::versions(&ctx, command),
        Command::Pin { file, to } => pin::pin_cmd(&ctx, &file, to),
        Command::Box { command } => boxcmd::box_cmd(&ctx, command),
        Command::InternalForward { sock, port } => forward::forward(&sock, port),
        Command::Tin { command } => tin::tin_cmd(&ctx, command),
        Command::Parcel { id, out, with_key } => parcel::parcel(&ctx, &id, out, with_key),
        Command::Unparcel {
            file,
            root,
            cwd,
            allow_sealed_missing,
            accept_key,
            show,
        } => parcel::unparcel(
            &ctx,
            &file,
            root,
            cwd,
            allow_sealed_missing,
            accept_key,
            show,
        ),
    }
}

/// Runs after the interpreter thread panicked.
pub fn after_crash() {
    let ctx = Ctx {
        json: false,
        color: std::env::var_os("NO_COLOR").is_none() && stderr_is_tty(),
        doctor: false,
        plain: plain_by_env(),
    };
    crash::after_crash(&ctx);
}

/// `CIG_PLAIN=1` (or anything but 0/false/off) selects plain mode.
pub fn plain_by_env() -> bool {
    match std::env::var("CIG_PLAIN") {
        Ok(v) => !matches!(v.as_str(), "" | "0" | "false" | "off"),
        Err(_) => false,
    }
}

#[derive(Clone, Copy)]
pub struct Ctx {
    pub json: bool,
    pub color: bool,
    /// Run doctor's automatic diagnosis when a diagnostic is reported.
    pub doctor: bool,
    /// Plain mode: the same information without the catchphrases.
    pub plain: bool,
}

impl Ctx {
    pub fn red(&self, s: &str) -> String {
        self.paint("31", s)
    }
    pub fn green(&self, s: &str) -> String {
        self.paint("32", s)
    }
    pub fn yellow(&self, s: &str) -> String {
        self.paint("33", s)
    }
    pub fn dim(&self, s: &str) -> String {
        self.paint("2", s)
    }
    pub fn bold(&self, s: &str) -> String {
        self.paint("1", s)
    }
    fn paint(&self, code: &str, s: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }
}

fn stderr_is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stderr().is_terminal()
}
