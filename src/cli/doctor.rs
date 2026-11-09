// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! `cig doctor`: is the installation healthy, and what is missing?
//! `--fix` offers to repair what it can, asking before each change.

use super::{exit, Ctx};
use crate::burn::runs;
use crate::config::Config;
use crate::diagnostics::{Diagnostic, Kind};
use crate::doctor::{self, Diagnosis};
use crate::errors::lookup_no;
use crate::{crash, update};
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

struct Tool {
    name: &'static str,
    enables: &'static str,
    present: bool,
}

pub fn doctor(ctx: &Ctx, fix: bool) -> i32 {
    let home = runs::home_dir();
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("cig"));
    let exe_dir = exe.parent().map(Path::to_path_buf);
    let on_path = std::env::var_os("PATH")
        .map(|p| {
            std::env::split_paths(&p)
                .any(|d| d.join("cig").is_file() || d.join("cig.exe").is_file())
        })
        .unwrap_or(false);
    let writable = check_writable(&home);
    let all_runs = runs::list().unwrap_or_default();
    let run_count = all_runs.len();
    let interrupted: Vec<String> = all_runs
        .iter()
        .filter(|r| r.is_interrupted() && r.burns > 0)
        .map(|r| r.id.clone())
        .collect();
    let run_bytes = dir_size(&runs::runs_dir());
    let crashes = crash::list().unwrap_or_default();
    let unsent = crashes
        .iter()
        .filter(|c| c.sent.is_none() && c.issue.is_none())
        .count();
    let failing = crashes
        .iter()
        .filter(|c| c.issue.is_none() && c.queue.last_error.is_some())
        .count();
    let tools = vec![
        Tool {
            name: "curl",
            enables: "cig update, crash reports without gh",
            present: update::has_tool("curl"),
        },
        Tool {
            name: "gh",
            enables: "crash reports and updates from private repositories",
            present: update::has_tool("gh"),
        },
        Tool {
            name: "git",
            enables: "the examples that shell out to git",
            present: update::has_tool("git"),
        },
    ];
    let cfg = Config::load();
    let update_cache = update::read_cache();
    let newer = update::cached_newer();

    let mut problems: Vec<String> = Vec::new();
    // The tin key's lock lives in the keys directory (#105): where that
    // path is taken by something that is not a directory, a run refuses
    // before it burns (E802). Looked at, never made: doctor makes no key.
    let keys = crate::burn::seal::keys_dir();
    if std::fs::symlink_metadata(&keys).is_ok_and(|m| !m.is_dir()) {
        problems.push(format!(
            "the keys directory {} is not a directory, so a run cannot lock the tin's key and refuses before it burns [E802]",
            keys.display()
        ));
    }
    // StrictModes: owner and mode of the keys folder and the files in
    // it must be ours and private; a planted folder (another user, a
    // 0777 mode) is named here so the operator sees it before a run
    // refuses with E721 (#176).
    #[cfg(unix)]
    if std::fs::symlink_metadata(&keys).is_ok() {
        if let Err(e) = crate::burn::seal::check_keys_dir(&keys) {
            problems.push(e);
        }
    }
    #[cfg(unix)]
    {
        // The key file is checked only where the key is read from it: a
        // leftover beside a keyring key is not this machine's problem.
        if crate::burn::seal::peek_keeper() == Some(crate::burn::seal::Keeper::File) {
            if let Err(e) =
                crate::burn::seal::check_private_key_file(&crate::burn::seal::key_path())
            {
                problems.push(e);
            }
        }
        // The ledger and the lock hold no secret: one that is ours and too
        // open is made 0600 by the next run, so doctor says only what the
        // run would refuse (another owner, a link, not a file). Doctor
        // changes nothing.
        for name in ["ledger.jsonl", "rekey.lock"] {
            if let Some(e) = crate::burn::seal::state_file_problem(&keys.join(name)) {
                problems.push(e);
            }
        }
    }
    if let Err(e) = &writable {
        problems.push(format!(
            "state directory {} is not writable: {e}",
            home.display()
        ));
    }
    if !on_path {
        problems
            .push("`cig` is not on PATH, so `#!/usr/bin/env cig` scripts cannot run".to_string());
    }
    if !tools[0].present && !tools[1].present {
        problems.push("neither curl nor gh is installed: `cig update` and crash reporting cannot reach GitHub".to_string());
    }
    if let Some(n) = &newer {
        problems.push(format!(
            "a newer release is available: {} (you run {}); run `cig update`",
            n.latest, n.current
        ));
    }
    if unsent > 0 {
        problems.push(format!(
            "{unsent} report{} of cig's own bugs not filed yet{}; `cig crash list`",
            if unsent == 1 { "" } else { "s" },
            if failing > 0 {
                format!(" ({failing} tried and failed, waiting for a retry)")
            } else {
                String::new()
            }
        ));
    }
    if run_bytes > 1 << 30 {
        problems.push(format!(
            "run records use {} MB; `cig runs --prune 50` frees space",
            run_bytes >> 20
        ));
    }
    // The disk budget (#149): off is the default. When it is off, doctor
    // suggests a value (5% of the state directory's free space, from 1 GB
    // to 20 GB) so the history does not grow forever; nothing is changed,
    // and the suggestion is a tip, never counted as a problem.
    let budget = crate::config::parse_bytes(&cfg.get("history_budget")).flatten();
    let suggestion = if budget.is_none() {
        suggested_budget(&home)
    } else {
        None
    };
    // The history as the budget measures it: the runs and the tin.
    let history = super::runs::history_bytes();
    // Over the budget after a run means what is left may not be pruned
    // (pinned, held for approval, younger than history_keep): said, since
    // the budget is exceeded on purpose then.
    let over = budget.is_some_and(|b| history > b);

    for id in &interrupted {
        problems.push(format!(
            "run {id} was interrupted mid-burn: its process is gone and its journal is intact [E706]; `cig unburn {id}` puts things back, `cig runs {id}` shows what it burned"
        ));
    }

    // The ledger (#177): a missing or empty ledger, with runs that
    // recorded chain heads, is a problem — `cig runs --verify` cannot
    // judge heads without it. A fresh tin with no sealed runs is fine.
    let ledger_presence = crate::burn::ledger::presence();
    let any_head = all_runs.iter().any(|r| r.journal_head.is_some());
    let ledger_problem = match ledger_presence {
        crate::burn::ledger::Presence::Missing if any_head => Some("missing"),
        crate::burn::ledger::Presence::Empty if any_head => Some("empty"),
        _ => None,
    };
    if let Some(state) = ledger_problem {
        let p = crate::burn::ledger::path();
        problems.push(format!(
            "the ledger is {state} ({}): `cig runs --verify` cannot judge chain heads without it [E709]",
            crate::crash::tilde(&p)
        ));
    }

    // The run index (#178): oversized means `cig runs --reindex` is
    // worth running, and every `cig run` is paying the cost.
    let index_lines = crate::burn::runs_index::line_count();
    let index_live = crate::burn::runs_index::live_count();
    let index_oversize = match (index_lines, index_live) {
        (Some(lines), Some(live)) if live > 0 && lines > 4 * live => Some((lines, live)),
        _ => None,
    };
    if let Some((lines, live)) = index_oversize {
        problems.push(format!(
            "the run index has {lines} lines but only {live} live run{}; `cig runs --reindex` compacts it (#178)",
            if live == 1 { "" } else { "s" }
        ));
    }

    if ctx.json {
        let ledger_state = match ledger_presence {
            crate::burn::ledger::Presence::Present => "present",
            crate::burn::ledger::Presence::Empty => "empty",
            crate::burn::ledger::Presence::Missing => "missing",
        };
        let obj = serde_json::json!({
            "version": crate::VERSION,
            "executable": exe,
            "on_path": on_path,
            "state_dir": home,
            "state_dir_writable": writable.is_ok(),
            "runs": run_count,
            "runs_bytes": run_bytes,
            "history_bytes": history,
            "history_budget": cfg.get("history_budget"),
            "history_over_budget": over,
            "history_budget_suggested": suggestion,
            "interrupted_runs": interrupted,
            "ledger": ledger_state,
            "runs_index_lines": index_lines,
            "runs_index_live": index_live,
            "crash_reports": crashes.len(),
            "crash_reports_unsent": unsent,
            "tools": tools.iter().map(|t| serde_json::json!({"name": t.name, "present": t.present})).collect::<Vec<_>>(),
            "config": {"crash_reports": cfg.get("crash_reports"), "issues_repo": cfg.get("issues_repo"), "update_channel": cfg.get("update_channel")},
            "update": update_cache,
            "problems": problems,
        });
        outln!("{}", serde_json::to_string_pretty(&obj).unwrap_or_default());
        return if problems.is_empty() {
            exit::OK
        } else {
            exit::SCRIPT_ERROR
        };
    }

    outln!("cig {}", crate::VERSION);
    outln!("  executable    {}", exe.display());
    outln!("  on PATH       {}", yes_no(ctx, on_path));
    outln!(
        "  state dir     {}  {}",
        home.display(),
        ctx.dim(if std::env::var_os("CIGSCRIPT_HOME").is_some() {
            "(from CIGSCRIPT_HOME)"
        } else {
            "(default)"
        })
    );
    outln!("  writable      {}", yes_no(ctx, writable.is_ok()));
    outln!(
        "  runs          {run_count} recorded, {}{}",
        human_bytes(run_bytes),
        if interrupted.is_empty() {
            String::new()
        } else {
            ctx.red(&format!("  {} interrupted", interrupted.len()))
        }
    );
    outln!(
        "  history       {} (runs and the tin), budget {}{}",
        human_bytes(history),
        cfg.get("history_budget"),
        match (&suggestion, over) {
            (Some(bytes), _) => ctx.dim(&format!(
                "  (off: cig doctor suggests {})",
                human_bytes(*bytes)
            )),
            (None, true) => ctx.dim(
                "  (over it: what is left is pinned, held for approval, or younger than history_keep)"
            ),
            (None, false) => String::new(),
        }
    );
    // The ledger and the run index (#177, #178): one line each, named
    // so the operator sees them without `--json`.
    let ledger_label = match ledger_presence {
        crate::burn::ledger::Presence::Present => ctx.green("present"),
        crate::burn::ledger::Presence::Empty => {
            if any_head {
                ctx.red("empty")
            } else {
                ctx.dim("empty")
            }
        }
        crate::burn::ledger::Presence::Missing => {
            if any_head {
                ctx.red("missing")
            } else {
                ctx.dim("missing (no sealed runs yet)")
            }
        }
    };
    outln!(
        "  ledger        {ledger_label}  {}",
        ctx.dim(&crate::crash::tilde(&crate::burn::ledger::path()))
    );
    let index_line = match (index_lines, index_live) {
        (Some(lines), Some(live)) => format!(
            "{lines} line{} for {live} live run{}",
            if lines == 1 { "" } else { "s" },
            if live == 1 { "" } else { "s" }
        ),
        (Some(lines), None) => {
            format!("{lines} lines (torn or unparseable; the next listing rebuilds)")
        }
        _ => "missing (built on the next cig runs)".to_string(),
    };
    let index_suffix = if index_oversize.is_some() {
        ctx.yellow("  cig runs --reindex compacts it")
    } else {
        String::new()
    };
    outln!("  run index     {index_line}{index_suffix}");
    outln!(
        "  ashtray       {} report{} saved, {unsent} not filed{}",
        crashes.len(),
        if crashes.len() == 1 { "" } else { "s" },
        if failing > 0 {
            format!(", {failing} waiting for a retry")
        } else {
            String::new()
        }
    );
    outln!(
        "  config        reports={}  issues_repo={}  update_channel={}",
        match crash::consent(&cfg) {
            crash::Consent::Ask => "ask",
            crash::Consent::Always => "always",
            crash::Consent::Never => "never",
        },
        cfg.get("issues_repo"),
        cfg.get("update_channel")
    );
    match &update_cache {
        Some(c) => outln!(
            "  update        latest known {} (checked {}){}",
            c.latest,
            c.checked_at,
            if newer.is_some() {
                ctx.yellow("  newer than this build")
            } else {
                String::new()
            }
        ),
        None => outln!(
            "  update        {}",
            ctx.dim("not checked yet: cig update --check")
        ),
    }
    outln!("  tools");
    for t in &tools {
        outln!(
            "    {:<6} {}  {}",
            t.name,
            yes_no(ctx, t.present),
            ctx.dim(t.enables)
        );
    }
    // The tin's protection, in one block: who can read it, where the key
    // is, and whether a hop can read it out of cig's memory.
    outln!("  tin");
    {
        use crate::burn::{seal, tin};
        let home = crate::burn::runs::home_dir();
        outln!(
            "    private       {}  {}",
            yes_no(ctx, home_is_private(&home)),
            ctx.dim(&crate::crash::tilde(&home))
        );
        // Looked at, never made: doctor must not create a key by looking.
        let (keeper, where_) = match seal::peek_keeper() {
            Some(seal::Keeper::Keyring) => ("the OS keyring".to_string(), String::new()),
            Some(seal::Keeper::File) => (
                "a key file".to_string(),
                crate::crash::tilde(&seal::key_path()),
            ),
            Some(seal::Keeper::Descriptor) => (
                "handed over on a descriptor".to_string(),
                "--seal-key-fd: held in memory, never written".to_string(),
            ),
            None => (
                "not made yet".to_string(),
                "made on the first sealed object".to_string(),
            ),
        };
        outln!("    key           {keeper}  {}", ctx.dim(&where_));
        // Where the key folder is, who owns it and its mode (#176).
        // Looked at, never made: doctor still creates no files.
        let keys_dir = seal::keys_dir();
        let keys_line = keys_dir_line(&keys_dir);
        outln!(
            "    key folder    {}  {}",
            keys_line.state,
            ctx.dim(&keys_line.detail)
        );
        outln!(
            "    key out of tin {}  {}",
            yes_no(ctx, !seal::key_path().starts_with(tin::dir())),
            ctx.dim("a copy of the tin does not take the key")
        );
        let debug_allowed = std::env::var_os("CIG_ALLOW_DEBUG").is_some();
        outln!(
            "    key in memory {}  {}",
            yes_no(
                ctx,
                !debug_allowed && cfg!(any(target_os = "linux", target_os = "macos"))
            ),
            ctx.dim(if debug_allowed {
                "CIG_ALLOW_DEBUG=1: a debugger can read the key (E715)"
            } else {
                "a hop cannot read the key out of cig"
            })
        );
        let (guard_ok, guard_text) = guard_status();
        outln!(
            "    guard         {}  {}",
            yes_no(ctx, guard_ok),
            ctx.dim(&guard_text)
        );
        if seal::has_sealed()
            && !seal::was_exported()
            && !seal::key_was_given()
            && crate::config::Config::load().get("key_reminder") != "off"
        {
            outln!(
                "    {}",
                ctx.yellow("tip: back up the tin key so a restored backup can open your secrets: cig tin key export")
            );
        }
    }
    // The box's primitives (#106), each tried on its own, so a box that
    // cannot be made says which part is missing.
    outln!("  box");
    for (name, ok, says) in box_primitives_remembered() {
        outln!("    {:<13} {}  {}", name, yes_no(ctx, ok), ctx.dim(&says));
    }
    if problems.is_empty() {
        outln!("{}", ctx.green("no problems found"));
        return exit::OK;
    }
    for p in &problems {
        outln!("{} {p}", ctx.yellow("problem:"));
    }
    if !fix {
        outln!(
            "{}",
            ctx.dim("cig doctor --fix offers to repair what it can")
        );
        return exit::SCRIPT_ERROR;
    }

    // ----- --fix: every change is proposed and needs a y -----------------------
    if !io::stdin().is_terminal() {
        eprintln!("--fix needs a terminal to ask questions");
        return exit::USAGE;
    }
    let mut fixed = 0;
    for id in &interrupted {
        if ask(&format!(
            "run {id} was interrupted mid-burn; roll back what it burned with `cig unburn {id}`?"
        )) {
            let code = super::runs::unburn(ctx, id, false, false, false);
            if code == exit::OK {
                fixed += 1;
            }
        }
    }
    if !on_path {
        if let Some(dir) = &exe_dir {
            if let Some((rc, line)) = path_line(dir) {
                if ask(&format!("append `{line}` to {}?", rc.display())) {
                    match append_line(&rc, &line) {
                        Ok(()) => {
                            eprintln!(
                                "{} added; open a new shell to pick it up",
                                ctx.green("done:")
                            );
                            fixed += 1;
                        }
                        Err(e) => eprintln!("{} {e}", ctx.red("failed:")),
                    }
                }
            }
        }
    }
    for t in tools.iter().filter(|t| !t.present) {
        if let Some(cmd) = install_command(t.name) {
            let shown = cmd.join(" ");
            eprintln!(
                "{} {} is missing; it enables {}",
                ctx.yellow("missing:"),
                t.name,
                t.enables
            );
            if ask(&format!("run `{shown}` now?")) {
                let status = std::process::Command::new(&cmd[0]).args(&cmd[1..]).status();
                match status {
                    Ok(s) if s.success() => {
                        eprintln!("{} {} installed", ctx.green("done:"), t.name);
                        fixed += 1;
                    }
                    Ok(s) => eprintln!("{} exited with {s}", ctx.red("failed:")),
                    Err(e) => eprintln!("{} could not run it: {e}", ctx.red("failed:")),
                }
            }
        } else {
            eprintln!(
                "{} {} is missing and no package manager was recognised; install it by hand",
                ctx.yellow("missing:"),
                t.name
            );
        }
    }
    if newer.is_some() && ask("run `cig update` now?") {
        let code = super::update::update(ctx, false, false, None);
        if code == exit::OK {
            fixed += 1;
        }
    }
    eprintln!("{fixed} fix{} applied", if fixed == 1 { "" } else { "es" });
    exit::OK
}

fn ask(question: &str) -> bool {
    eprint!("{question} [y/N] ");
    let _ = io::stderr().flush();
    let mut line = String::new();
    let _ = io::stdin().lock().read_line(&mut line);
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// The shell rc file and the line that would put `dir` on PATH.
fn path_line(dir: &Path) -> Option<(PathBuf, String)> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let shell = std::env::var("SHELL").unwrap_or_default();
    let shown = crash::tilde(dir).replace('~', "$HOME");
    if shell.ends_with("fish") {
        return Some((
            home.join(".config/fish/config.fish"),
            format!("fish_add_path {shown}"),
        ));
    }
    let rc = if shell.ends_with("zsh") {
        ".zshrc"
    } else {
        ".bashrc"
    };
    Some((home.join(rc), format!("export PATH=\"{shown}:$PATH\"")))
}

/// Can the tin guard be made here: user and mount namespaces for this
/// user, tried rather than guessed (`true` run inside them).
/// What the box is made of here: a user namespace, a mount namespace, a
/// network namespace (`network: "off"` and `"registry"`), a PID namespace
/// (`pidns`) and overlayfs (the layer). Each namespace is tried in a child
/// that unshares it and execs `true`; overlayfs is the box's own check.
/// `box_primitives`, remembered as a doctor pass (cig-script#186): the
/// probe makes namespaces, so its answer is kept while the kernel, its
/// namespace settings, its file systems and mounts, the boot, the
/// variables the probe reads, who runs it under what confinement, and this
/// cig stay the same, for a day at most. Only on a machine that already has
/// a key: doctor never makes one.
fn box_primitives_remembered() -> Vec<(&'static str, bool, String)> {
    const NAMES: &[&str] = &[
        "user ns",
        "mount ns",
        "network ns",
        "pid ns",
        "overlayfs",
        "box",
    ];
    let files: Vec<&std::path::Path> = [
        "/proc/sys/kernel/osrelease",
        "/proc/sys/kernel/random/boot_id",
        "/proc/sys/kernel/apparmor_restrict_unprivileged_userns",
        "/proc/sys/kernel/apparmor_restrict_unprivileged_unconfined",
        "/proc/sys/kernel/unprivileged_userns_clone",
        "/proc/sys/user/max_user_namespaces",
        "/proc/sys/user/max_mnt_namespaces",
        "/proc/sys/user/max_net_namespaces",
        "/proc/sys/user/max_pid_namespaces",
        "/proc/filesystems",
        "/proc/self/mountinfo",
        "/sys/module/overlay",
    ]
    .iter()
    .map(std::path::Path::new)
    .collect();
    // What the answer depends on in this process rather than in a file: who
    // it runs as, its seccomp and no_new_privs state and its AppArmor
    // profile. A doctor run in a sandbox and one in a plain shell ask
    // different questions.
    let mut subject = b"box-primitives".to_vec();
    #[cfg(unix)]
    {
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        subject.extend_from_slice(format!("\0uid={uid}").as_bytes());
    }
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if line.starts_with("Seccomp") || line.starts_with("NoNewPrivs") {
                subject.push(0);
                subject.extend_from_slice(line.as_bytes());
            }
        }
    }
    if let Ok(profile) = std::fs::read_to_string("/proc/self/attr/current") {
        subject.extend_from_slice(b"\0apparmor=");
        subject.extend_from_slice(profile.trim().as_bytes());
    }
    let answer: Vec<(String, bool, String)> = crate::pass::remembered(
        "doctor",
        &subject,
        &files,
        &["CIG_INTERNAL_NO_BOX", "CIGSCRIPT_HOME", "HOME", "PATH"],
        24 * 3_600_000,
        || {
            box_primitives()
                .into_iter()
                .map(|(n, ok, says)| (n.to_string(), ok, says))
                .collect()
        },
    );
    answer
        .into_iter()
        .filter_map(|(n, ok, says)| NAMES.iter().find(|k| **k == n).map(|k| (*k, ok, says)))
        .collect()
}

fn box_primitives() -> Vec<(&'static str, bool, String)> {
    #[cfg(target_os = "linux")]
    {
        let unshares = |flags: libc::c_int| -> bool {
            use std::os::unix::process::CommandExt;
            let mut c = std::process::Command::new("true");
            c.stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            // SAFETY: unshare alone, between fork and exec.
            unsafe {
                c.pre_exec(move || {
                    if libc::unshare(flags) == 0 {
                        Ok(())
                    } else {
                        Err(std::io::Error::last_os_error())
                    }
                });
            }
            c.status().is_ok_and(|st| st.success())
        };
        let user = unshares(libc::CLONE_NEWUSER);
        let restricted =
            std::fs::read_to_string("/proc/sys/kernel/apparmor_restrict_unprivileged_userns")
                .is_ok_and(|v| v.trim() == "1");
        let mut out = vec![(
            "user ns",
            user,
            if user {
                "a hop runs as root in a namespace of its own, nobody outside".to_string()
            } else if restricted {
                "refused: AppArmor restricts unprivileged user namespaces (sysctl kernel.apparmor_restrict_unprivileged_userns=0 allows them)".to_string()
            } else {
                "refused: unprivileged user namespaces are off here (kernel.unprivileged_userns_clone, or a container's seccomp)".to_string()
            },
        )];
        let in_user = |flag: libc::c_int| user && unshares(libc::CLONE_NEWUSER | flag);
        let mnt = in_user(libc::CLONE_NEWNS);
        out.push((
            "mount ns",
            mnt,
            "the hop's own view of the files: the layer over the pack, the tin read-only"
                .to_string(),
        ));
        let net = in_user(libc::CLONE_NEWNET);
        out.push((
            "network ns",
            net,
            "network: \"off\" and \"registry\" (lo alone, the proxy the only way out)".to_string(),
        ));
        let pid = in_user(libc::CLONE_NEWPID);
        out.push((
            "pid ns",
            pid,
            "pidns: the hop sees its own processes alone".to_string(),
        ));
        match crate::burn::overlay::possible() {
            Ok(()) => out.push((
                "overlayfs",
                true,
                "the layer a hop writes into, applied after".to_string(),
            )),
            Err(e) => out.push(("overlayfs", false, e)),
        }
        out
    }
    #[cfg(not(target_os = "linux"))]
    {
        vec![(
            "box",
            false,
            "the box is Linux's (user namespaces and overlayfs): here a boxed hop is refused (E777) or falls back to the tin (E763)".to_string(),
        )]
    }
}

fn guard_status() -> (bool, String) {
    let setting = crate::burn::overlay::guard_setting();
    if setting == "off" {
        return (
            false,
            "tin_guard = off: hops run on the tin, unguarded".to_string(),
        );
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        let mut c = std::process::Command::new("true");
        c.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // SAFETY: unshare alone, between fork and exec.
        unsafe {
            c.pre_exec(|| {
                if libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNS) == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            });
        }
        match c.status() {
            Ok(st) if st.success() => (
                true,
                format!(
                    "tin_guard = {setting}: {} see the tin read-only and no key{}",
                    if setting == "on" { "all hops" } else { "boxed hops" },
                    if crate::burn::overlay::bus_cut() {
                        ", and no session bus while the key may be in the OS keyring (cig config seal_key file gives them the bus)"
                    } else {
                        ""
                    }
                ),
            ),
            _ => (
                false,
                format!(
                    "tin_guard = {setting}: user namespaces are not allowed here; the tin is checked, not protected (E712)"
                ),
            ),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        (
            false,
            format!(
                "tin_guard = {setting}: the guard is Linux-only; the tin is checked, not protected"
            ),
        )
    }
}

fn append_line(rc: &Path, line: &str) -> io::Result<()> {
    if let Ok(existing) = std::fs::read_to_string(rc) {
        if existing.lines().any(|l| l.trim() == line) {
            return Ok(());
        }
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(rc)?;
    writeln!(f, "\n# added by cig doctor --fix\n{line}")
}

/// The exact package-manager command for a tool, or none if unrecognised.
/// Shown in full and run only after a `y`; sudo is part of the command the
/// user sees, never something added silently.
fn install_command(tool: &str) -> Option<Vec<String>> {
    let has = |m: &str| update::has_tool(m);
    let pkg = match tool {
        "gh" => "gh",
        "curl" => "curl",
        "git" => "git",
        _ => return None,
    };
    let cmd: Vec<&str> = if has("apt-get") {
        vec!["sudo", "apt-get", "install", "-y", pkg]
    } else if has("dnf") {
        vec!["sudo", "dnf", "install", "-y", pkg]
    } else if has("pacman") {
        vec![
            "sudo",
            "pacman",
            "-S",
            "--noconfirm",
            if pkg == "gh" { "github-cli" } else { pkg },
        ]
    } else if has("zypper") {
        vec!["sudo", "zypper", "install", "-y", pkg]
    } else if has("apk") {
        vec![
            "sudo",
            "apk",
            "add",
            if pkg == "gh" { "github-cli" } else { pkg },
        ]
    } else if has("brew") {
        vec!["brew", "install", pkg]
    } else if has("winget") {
        vec![
            "winget",
            "install",
            if pkg == "gh" {
                "GitHub.cli"
            } else if pkg == "git" {
                "Git.Git"
            } else {
                "cURL.cURL"
            },
        ]
    } else {
        return None;
    };
    Some(cmd.into_iter().map(String::from).collect())
}

fn yes_no(ctx: &Ctx, v: bool) -> String {
    if v {
        ctx.green("yes")
    } else {
        ctx.yellow("no")
    }
}

/// One line of doctor output for the keys folder (#176): does it exist,
/// who owns it, what mode is it in, and if a strict-mode check would
/// refuse it, say which. Looked at only; doctor never creates the folder.
struct KeysDirLine {
    state: String,
    detail: String,
}

fn keys_dir_line(dir: &Path) -> KeysDirLine {
    let shown = crate::crash::tilde(dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match std::fs::symlink_metadata(dir) {
            Ok(m) => {
                let euid = unsafe { libc::geteuid() };
                let mode = m.mode() & 0o7777;
                let owned = m.uid() == euid;
                let private = (mode & 0o022) == 0;
                let state = if owned && private && m.file_type().is_dir() {
                    "private".to_string()
                } else if !m.file_type().is_dir() {
                    "not-a-dir".to_string()
                } else if !owned {
                    "not yours".to_string()
                } else {
                    "open".to_string()
                };
                KeysDirLine {
                    state,
                    detail: format!("{shown}  owner uid {}, mode {:04o}", m.uid(), mode,),
                }
            }
            Err(_) => KeysDirLine {
                state: "not made yet".to_string(),
                detail: format!("{shown}  made on the first sealed object"),
            },
        }
    }
    #[cfg(not(unix))]
    {
        let detail = shown;
        let state = if std::fs::symlink_metadata(dir).is_ok() {
            "present".to_string()
        } else {
            "not made yet".to_string()
        };
        KeysDirLine { state, detail }
    }
}

/// Is the state directory owner-only (0700, owned by this user) on Unix?
/// Elsewhere the user's profile already restricts it.
fn home_is_private(home: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;
        match std::fs::metadata(home) {
            Ok(m) => m.uid() == unsafe { libc::geteuid() } && m.permissions().mode() & 0o077 == 0,
            Err(_) => true,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = home;
        true
    }
}

fn check_writable(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let probe = dir.join(".doctor-probe");
    std::fs::write(&probe, b"ok")?;
    std::fs::remove_file(&probe)
}

fn dir_size(dir: &Path) -> u64 {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

/// A short size in the units `history_budget` accepts, rounded for the eye.
fn human_bytes(n: u64) -> String {
    const K: u64 = 1 << 10;
    const M: u64 = 1 << 20;
    const G: u64 = 1 << 30;
    const T: u64 = 1 << 40;
    if n >= T {
        format!("{}TB", n / T)
    } else if n >= G {
        format!("{}GB", n / G)
    } else if n >= M {
        format!("{}MB", n / M)
    } else if n >= K {
        format!("{}KB", n / K)
    } else {
        format!("{n}B")
    }
}

/// A value `history_budget` could reasonably hold: about 5% of the state
/// directory's free space, rounded to a GB where there is room, from 1 GB
/// to 20 GB. `None` where the free space cannot be measured here (a
/// platform without a statvfs, say).
#[cfg(unix)]
fn suggested_budget(home: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    // The home may not exist yet: step up to the first parent that does,
    // whose filesystem is the one the state will land on.
    let mut probe: PathBuf = home.to_path_buf();
    while !probe.exists() {
        probe = probe.parent()?.to_path_buf();
        if probe.as_os_str().is_empty() {
            return None;
        }
    }
    let c = std::ffi::CString::new(probe.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let free = (st.f_bavail as u64).checked_mul(st.f_frsize as u64)?;
    if free == 0 {
        return None;
    }
    let five_pct = free / 20;
    let min: u64 = 1 << 30; // 1 GB
    let max: u64 = 20 * (1 << 30); // 20 GB
    let picked = five_pct.clamp(min, max);
    // Round down to the nearest GB when it will fit.
    let rounded = if picked >= 1 << 30 {
        (picked >> 30) << 30
    } else {
        picked
    };
    Some(rounded)
}

#[cfg(not(unix))]
fn suggested_budget(_home: &Path) -> Option<u64> {
    None
}

// ----- automatic diagnosis: rendering and re-diagnosis -----------------------

/// The layered output under a diagnostic. Same shape for every code:
/// verdict, why, fix, if not. Themed and plain differ only in the kind-of-no
/// phrase: the hallway version, or the manual's name.
pub fn render_diagnosis(ctx: &Ctx, dx: &Diagnosis) -> String {
    let mut out = String::new();
    let no = dx.no.as_deref().and_then(lookup_no).map(|n| {
        if ctx.plain {
            format!(" ({})", n.manual)
        } else {
            format!(" ({})", n.hear)
        }
    });
    if dx.confirmed {
        let because = dx
            .why
            .first()
            .map(|w| format!(", because {w}"))
            .unwrap_or_default();
        out.push_str(&format!(
            "  = {} I think {}{}{because}.\n",
            ctx.bold("doctor:"),
            dx.verdict,
            no.unwrap_or_default()
        ));
        for f in &dx.fix {
            out.push_str(&format!("  = {} {f}\n", ctx.green("fix:")));
        }
        for (i, alt) in dx.if_not.iter().enumerate() {
            let label = if i == 0 {
                ctx.yellow("if not:")
            } else {
                "       ".to_string()
            };
            out.push_str(&format!("  = {label} {alt}\n"));
        }
    } else {
        out.push_str(&format!(
            "  = {} no known cause matched; cig explain {} for the general case\n",
            ctx.bold("doctor:"),
            dx.code
        ));
        for (i, alt) in dx.if_not.iter().enumerate() {
            let label = if i == 0 {
                ctx.yellow("maybe:")
            } else {
                "      ".to_string()
            };
            out.push_str(&format!("  = {label} {alt}\n"));
        }
    }
    out
}

/// `cig doctor <run>`: diagnose a stored run's error again, from its record,
/// without touching anything. What CI saw can be looked at on a laptop.
pub fn rediagnose(ctx: &Ctx, id: &str) -> i32 {
    let rec = match runs::find(id) {
        Ok(Some(r)) => r,
        Ok(None) => {
            eprintln!("{} no run matches `{id}`", ctx.red("error[E803 usage]:"));
            eprintln!("  = explain: cig explain E803");
            return exit::USAGE;
        }
        Err(e) => {
            eprintln!("{} {e}", ctx.red("error[E803 usage]:"));
            return exit::USAGE;
        }
    };
    let cx = doctor::Context {
        cwd: PathBuf::from(&rec.cwd),
        source: None,
    };
    let mut diag: Option<Diagnostic> = rec.diagnostic.as_ref().and_then(Diagnostic::from_json);
    let file = rec
        .diagnostic
        .as_ref()
        .and_then(|v| v.get("file"))
        .and_then(|f| f.as_str())
        .map(str::to_string);
    if rec.is_interrupted() {
        diag = Some(
            Diagnostic::new(
                Kind::Burn,
                format!(
                    "run {} was interrupted mid-burn: its process is gone and its journal is intact",
                    rec.id
                ),
            )
            .code("E706")
            .with_hint(format!(
                "cig unburn {} puts things back; cig runs {} shows what it burned",
                rec.id, rec.id
            ))
            .with_subject(rec.id.clone()),
        );
    }
    let diagnosis = diag.as_ref().and_then(|d| doctor::diagnose(d, &cx));
    if ctx.json {
        outln!(
            "{}",
            serde_json::json!({
                "run": rec,
                "diagnostic": diag,
                "diagnosis": diagnosis,
            })
        );
        return exit::OK;
    }
    outln!(
        "{} {}  {}  {} burn{} ({} irreversible)  cwd {}",
        ctx.bold(&rec.id),
        rec.status,
        rec.script,
        rec.burns,
        if rec.burns == 1 { "" } else { "s" },
        rec.irreversible,
        rec.cwd
    );
    let Some(d) = diag else {
        outln!(
            "{}",
            ctx.dim("finished without a diagnostic; nothing to diagnose")
        );
        return exit::OK;
    };
    outln!();
    eprint!("{}", d.render(file.as_deref(), None));
    match diagnosis {
        Some(dx) => eprint!("{}", render_diagnosis(ctx, &dx)),
        None => eprintln!(
            "  = {} nothing to add beyond the hint; cig explain {} for the page",
            ctx.bold("doctor:"),
            d.code
        ),
    }
    exit::OK
}
