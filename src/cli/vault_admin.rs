// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! `cig tin vault install|uninstall|gc|update` (#105, L7).
//!
//! Everything that needs an administrator is a list of steps, each run
//! through `sudo` (or directly, as root); `--dry-run` prints the list and
//! runs none of it. The daemon is never setuid: the one thing a user may
//! ask of the vault's account, `cig-vault gc`, is a sudo rule that names
//! that command, for that user, with a password.

use super::{exit, Ctx};

#[derive(clap::Subcommand)]
pub enum VaultCommand {
    /// Make the `_cigtin` account, the store, the service and the sudo rule for gc (needs sudo).
    Install {
        /// The cig-vault binary to install (default: the one beside this cig).
        #[arg(long)]
        binary: Option<std::path::PathBuf>,
        /// Print the steps and run none of them.
        #[arg(long)]
        dry_run: bool,
    },
    /// Stop and remove the service and the sudo rule; the store's objects stay unless --purge.
    Uninstall {
        /// Also remove the store and the account (through sudo). Needs --yes.
        #[arg(long)]
        purge: bool,
        /// Say yes to --purge.
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// Collect what no run names: the plan by default, --apply removes it (through the sudo rule).
    Gc {
        /// Remove what the plan names.
        #[arg(long)]
        apply: bool,
        /// Collect objects of any age (the default spares those under seven days old).
        #[arg(long)]
        now: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// Swap in a new daemon binary and restart the service.
    Update {
        /// The new cig-vault binary (default: the one beside this cig).
        #[arg(long)]
        binary: Option<std::path::PathBuf>,
        #[arg(long)]
        dry_run: bool,
    },
}

#[cfg(not(unix))]
pub fn vault_cmd(ctx: &Ctx, _cmd: VaultCommand) -> i32 {
    eprintln!(
        "{} the vault runs on Linux and macOS; Windows has no transport for it yet (the tin is all there is there)",
        ctx.red("error[E724 burn]:")
    );
    exit::SCRIPT_ERROR
}

#[cfg(unix)]
pub use unix::vault_cmd;

#[cfg(unix)]
mod unix {
    use super::{exit, Ctx, VaultCommand};
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::Duration;

    const ACCOUNT: &str = cig_vault::ACCOUNT;
    const BIN: &str = "/usr/local/bin/cig-vault";
    const UNIT: &str = "/etc/systemd/system/cig-vault.service";
    const SUDOERS: &str = "/etc/sudoers.d/cig-vault";
    const PLIST: &str = "/Library/LaunchDaemons/com.otmof.cig-vault.plist";

    /// How long `update` waits for the restarted vault to answer. The
    /// service is `Type=simple`, so systemd calls it active as soon as
    /// `ExecStart` has forked; the socket appears a moment later. If it
    /// does not appear in this window, `update` returns E724 so a caller
    /// never finds no socket after a successful `cig tin vault update`.
    const UPDATE_WAIT: Duration = Duration::from_secs(10);

    /// One thing an administrator must do.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Step {
        pub what: String,
        pub argv: Vec<String>,
        pub stdin: Option<String>,
        /// A failure here is not a failure of the whole (removing what may
        /// not be there).
        pub optional: bool,
    }

    fn step(what: &str, argv: &[&str]) -> Step {
        Step {
            what: what.to_string(),
            argv: argv.iter().map(|s| s.to_string()).collect(),
            stdin: None,
            optional: false,
        }
    }

    fn optional(mut s: Step) -> Step {
        s.optional = true;
        s
    }

    fn with_stdin(mut s: Step, text: String) -> Step {
        s.stdin = Some(text);
        s
    }

    fn macos() -> bool {
        cfg!(target_os = "macos")
    }

    pub fn systemd_unit() -> String {
        format!(
            "[Unit]\nDescription=cig vault: a no-delete store for the tin's objects\nAfter=local-fs.target\n\n\
[Service]\nType=simple\nUser={ACCOUNT}\nGroup={ACCOUNT}\nExecStart={BIN} serve\nRuntimeDirectory=cig-vault\nRuntimeDirectoryMode=0755\n\
Restart=on-failure\nNoNewPrivileges=yes\nProtectSystem=strict\nReadWritePaths={root}\nProtectHome=yes\nPrivateTmp=yes\n\
RestrictAddressFamilies=AF_UNIX\nCapabilityBoundingSet=\nMemoryDenyWriteExecute=yes\nLockPersonality=yes\n\n\
[Install]\nWantedBy=multi-user.target\n",
            root = cig_vault::DEFAULT_ROOT
        )
    }

    pub fn launchd_plist() -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n<dict>\n  <key>Label</key><string>com.otmof.cig-vault</string>\n  <key>UserName</key><string>{ACCOUNT}</string>\n\
  <key>ProgramArguments</key>\n  <array><string>{BIN}</string><string>serve</string></array>\n  <key>RunAtLoad</key><true/>\n  <key>KeepAlive</key><true/>\n\
</dict>\n</plist>\n"
        )
    }

    /// The sudo rule: this user may run the vault's gc as the vault's
    /// account, for their own uid only, and must give a password (no
    /// NOPASSWD): a hop running as the user cannot use it.
    pub fn sudoers_rule(user: &str, uid: u32) -> String {
        let mut lines = format!(
            "# cig-vault gc, for {user} only, as {ACCOUNT}, with a password. Written by cig tin vault install.\n"
        );
        let forms = ["", " --apply", " --grace-days 0", " --grace-days 0 --apply"];
        let rules: Vec<String> = forms
            .iter()
            .map(|f| format!("{BIN} gc --uid {uid}{f}"))
            .collect();
        lines.push_str(&format!("{user} ALL=({ACCOUNT}) {}\n", rules.join(", ")));
        lines
    }

    fn sh(what: &str, script: &str) -> Step {
        step(what, &["sh", "-c", script])
    }

    pub fn install_steps(bin: &Path, user: &str, uid: u32, have_account: bool) -> Vec<Step> {
        let bin = bin.to_string_lossy().to_string();
        let mut v = Vec::new();
        if !have_account {
            if macos() {
                v.push(sh(
                    "make the _cigtin account",
                    &format!(
                        "id=$(dscl . -list /Users UniqueID | awk '$2>=300 && $2<400 {{u[$2]=1}} END {{for(i=300;i<400;i++) if(!u[i]) {{print i; exit}}}}'); \
dscl . -create /Users/{ACCOUNT} && dscl . -create /Users/{ACCOUNT} UniqueID $id && dscl . -create /Users/{ACCOUNT} PrimaryGroupID 20 && \
dscl . -create /Users/{ACCOUNT} UserShell /usr/bin/false && dscl . -create /Users/{ACCOUNT} NFSHomeDirectory /var/empty && dscl . -create /Users/{ACCOUNT} IsHidden 1"
                    ),
                ));
            } else {
                v.push(step(
                    "make the _cigtin account",
                    &[
                        "useradd",
                        "--system",
                        "--no-create-home",
                        "--home-dir",
                        "/nonexistent",
                        "--shell",
                        "/usr/sbin/nologin",
                        ACCOUNT,
                    ],
                ));
            }
        }
        v.push(step(
            "make the store, private to the account",
            &[
                "install",
                "-d",
                "-m",
                "0700",
                "-o",
                ACCOUNT,
                cig_vault::DEFAULT_ROOT,
            ],
        ));
        v.push(step(
            "install the daemon where the account can run it",
            &["install", "-m", "0755", &bin, BIN],
        ));
        let tmp = format!("{SUDOERS}.tmp");
        v.push(with_stdin(
            step("write the sudo rule for gc", &["tee", &tmp]),
            sudoers_rule(user, uid),
        ));
        v.push(step("check the rule", &["visudo", "-cf", &tmp]));
        v.push(step(
            "put the rule in place (mode 0440)",
            &["install", "-m", "0440", "-o", "root", &tmp, SUDOERS],
        ));
        v.push(optional(step("drop the rule's draft", &["rm", "-f", &tmp])));
        if macos() {
            v.push(step(
                "make the socket's folder",
                &[
                    "install",
                    "-d",
                    "-m",
                    "0755",
                    "-o",
                    ACCOUNT,
                    "/var/run/cig-vault",
                ],
            ));
            v.push(with_stdin(
                step("write the launchd job", &["tee", PLIST]),
                launchd_plist(),
            ));
            v.push(step("lock the job's mode", &["chmod", "0644", PLIST]));
            v.push(step(
                "start the job",
                &["launchctl", "bootstrap", "system", PLIST],
            ));
        } else {
            v.push(with_stdin(
                step("write the service", &["tee", UNIT]),
                systemd_unit(),
            ));
            v.push(step("load it", &["systemctl", "daemon-reload"]));
            v.push(step(
                "enable and start it",
                &["systemctl", "enable", "--now", "cig-vault.service"],
            ));
        }
        v
    }

    pub fn uninstall_steps(purge: bool) -> Vec<Step> {
        let mut v = Vec::new();
        if macos() {
            v.push(optional(step(
                "stop the job",
                &["launchctl", "bootout", "system", PLIST],
            )));
            v.push(optional(step("remove the job", &["rm", "-f", PLIST])));
        } else {
            v.push(optional(step(
                "stop and disable the service",
                &["systemctl", "disable", "--now", "cig-vault.service"],
            )));
            v.push(optional(step("remove the service", &["rm", "-f", UNIT])));
            v.push(optional(step("load that", &["systemctl", "daemon-reload"])));
        }
        v.push(optional(step(
            "remove the sudo rule",
            &["rm", "-f", SUDOERS],
        )));
        if purge {
            v.push(step(
                "remove the store and everything in it",
                &["rm", "-rf", cig_vault::DEFAULT_ROOT],
            ));
            if macos() {
                v.push(optional(step(
                    "remove the account",
                    &["dscl", ".", "-delete", &format!("/Users/{ACCOUNT}")],
                )));
            } else {
                v.push(optional(step("remove the account", &["userdel", ACCOUNT])));
            }
        }
        v
    }

    /// Poll the vault's socket until it answers, or `wait` runs out.
    /// On timeout the error is the last reason seen. Connects
    /// through `cig_vault::client::Client` so the check is the same the
    /// caller will do next.
    pub fn wait_for_socket(wait: Duration) -> Result<(), String> {
        let socket = std::path::Path::new(cig_vault::DEFAULT_SOCKET);
        let expect = account_uid();
        let deadline = std::time::Instant::now() + wait;
        loop {
            let last = match cig_vault::client::Client::connect(socket, expect) {
                Ok(_) => return Ok(()),
                Err(e) => e.to_string(),
            };
            if std::time::Instant::now() >= deadline {
                return Err(last);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn update_steps(bin: &Path) -> Vec<Step> {
        let bin = bin.to_string_lossy().to_string();
        let new = format!("{BIN}.new");
        let restart: Step = if macos() {
            step(
                "restart the job",
                &["launchctl", "kickstart", "-k", "system/com.otmof.cig-vault"],
            )
        } else {
            step(
                "restart the service",
                &["systemctl", "restart", "cig-vault.service"],
            )
        };
        vec![
            step(
                "copy the new daemon beside the old",
                &["install", "-m", "0755", &bin, &new],
            ),
            step("swap it in", &["mv", "-f", &new, BIN]),
            restart,
        ]
    }

    fn account_uid() -> Option<u32> {
        let name = std::ffi::CString::new(ACCOUNT).ok()?;
        // SAFETY: getpwnam reads the account database; the uid is read at once.
        let pw = unsafe { libc::getpwnam(name.as_ptr()) };
        (!pw.is_null()).then(|| unsafe { (*pw).pw_uid })
    }

    fn me() -> (String, u32) {
        // SAFETY: geteuid has no preconditions; getpwuid's answer is read at once.
        unsafe {
            let uid = libc::geteuid();
            let pw = libc::getpwuid(uid);
            let name = if pw.is_null() {
                uid.to_string()
            } else {
                std::ffi::CStr::from_ptr((*pw).pw_name)
                    .to_string_lossy()
                    .to_string()
            };
            (name, uid)
        }
    }

    fn daemon_binary(given: Option<PathBuf>) -> Result<PathBuf, String> {
        let p = match given {
            Some(p) => p,
            None => std::env::current_exe()
                .map_err(|e| e.to_string())?
                .parent()
                .map(|d| d.join("cig-vault"))
                .ok_or("cannot find the daemon beside cig")?,
        };
        if p.is_file() {
            Ok(p)
        } else {
            Err(format!(
                "no cig-vault at {} (name it with --binary)",
                p.display()
            ))
        }
    }

    fn quote(a: &str) -> String {
        if a.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-=:@".contains(&b))
            && !a.is_empty()
        {
            a.to_string()
        } else {
            format!("'{}'", a.replace('\'', "'\\''"))
        }
    }

    pub fn show(s: &Step) -> String {
        let mut line = format!(
            "sudo {}",
            s.argv
                .iter()
                .map(|a| quote(a))
                .collect::<Vec<_>>()
                .join(" ")
        );
        if s.stdin.is_some() {
            line.push_str("   (with the text on its input)");
        }
        line
    }

    fn run_step(s: &Step) -> Result<(), String> {
        // SAFETY: geteuid has no preconditions.
        let root = unsafe { libc::geteuid() } == 0 && s.argv[0] != "-u";
        let mut c = if root {
            let mut c = Command::new(&s.argv[0]);
            c.args(&s.argv[1..]);
            c
        } else {
            let mut c = Command::new("sudo");
            c.args(&s.argv);
            c
        };
        c.stdin(if s.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        // `tee` echoes the file it writes; nothing else's output is hidden.
        if s.argv[0] == "tee" {
            c.stdout(Stdio::null());
        }
        let mut child = c.spawn().map_err(|e| format!("{}: {e}", s.what))?;
        if let (Some(text), Some(mut pipe)) = (&s.stdin, child.stdin.take()) {
            let _ = pipe.write_all(text.as_bytes());
        }
        let status = wait_timeout::ChildExt::wait_timeout(&mut child, Duration::from_secs(300))
            .map_err(|e| e.to_string())?;
        match status {
            Some(st) if st.success() => Ok(()),
            Some(st) => Err(format!("{} failed ({st})", s.what)),
            None => {
                let _ = child.kill();
                Err(format!("{} did not finish in five minutes", s.what))
            }
        }
    }

    fn run_all(ctx: &Ctx, steps: &[Step], dry: bool) -> i32 {
        for s in steps {
            if dry {
                eprintln!("{}: {}", s.what, show(s));
                continue;
            }
            eprintln!("{}...", s.what);
            if let Err(e) = run_step(s) {
                if s.optional {
                    continue;
                }
                eprintln!("{} {e}", ctx.red("error[E724 burn]:"));
                return exit::SCRIPT_ERROR;
            }
        }
        exit::OK
    }

    fn set_vault(value: &str) {
        let mut cfg = crate::config::Config::load();
        if cfg.set("vault", value).is_ok() {
            let _ = cfg.save();
        }
    }

    /// The live set for gc: every tin object a run (or a kept copy) names.
    fn live_lines() -> String {
        use crate::burn::tin;
        let (o, r, s) = (tin::objects_root(), tin::recipes_root(), tin::sealed_dir());
        let mut out = String::new();
        for p in tin::referenced() {
            let kind = if p.starts_with(&o) {
                cig_vault::Kind::Object
            } else if p.starts_with(&r) {
                cig_vault::Kind::Recipe
            } else if p.starts_with(&s) {
                cig_vault::Kind::Sealed
            } else {
                continue;
            };
            if let Some(id) = p.file_name().map(|n| n.to_string_lossy().to_string()) {
                if kind.id_ok(&id) {
                    out.push_str(&format!("{} {id}\n", kind.letter()));
                }
            }
        }
        out
    }

    pub fn gc_step(uid: u32, apply: bool, now: bool, live: String) -> Step {
        let uid = uid.to_string();
        let mut argv = vec!["-u", ACCOUNT, BIN, "gc", "--uid", &uid];
        if now {
            argv.extend(["--grace-days", "0"]);
        }
        if apply {
            argv.push("--apply");
        }
        let mut s = step("ask the vault what no run names", &argv);
        // `sudo -u _cigtin ...`: the step runner puts `sudo` in front.
        s.stdin = Some(live);
        s
    }

    pub fn vault_cmd(ctx: &Ctx, cmd: VaultCommand) -> i32 {
        match cmd {
            VaultCommand::Install { binary, dry_run } => {
                let bin = match daemon_binary(binary) {
                    Ok(b) => b,
                    Err(e) => {
                        eprintln!("{} {e}", ctx.red("error[E724 burn]:"));
                        return exit::USAGE;
                    }
                };
                let (user, uid) = me();
                let steps = install_steps(&bin, &user, uid, account_uid().is_some());
                let code = run_all(ctx, &steps, dry_run);
                if code == exit::OK && !dry_run {
                    set_vault("on");
                    eprintln!(
                        "{} the vault is installed and running; cig hands the tin's objects to it from the next run",
                        ctx.green("ok:")
                    );
                }
                code
            }
            VaultCommand::Uninstall {
                purge,
                yes,
                dry_run,
            } => {
                if purge && !yes && !dry_run {
                    eprintln!(
                        "{} --purge removes every object the vault keeps, for every user, and the account; say --yes to do it",
                        ctx.red("error[E801 usage]:")
                    );
                    return exit::USAGE;
                }
                let code = run_all(ctx, &uninstall_steps(purge), dry_run);
                if code == exit::OK && !dry_run {
                    set_vault("off");
                    eprintln!(
                        "{} the vault is stopped and removed; {}",
                        ctx.green("ok:"),
                        if purge {
                            "its store and account are gone"
                        } else {
                            "its store and account stay, with every object in them (--purge removes them)"
                        }
                    );
                }
                code
            }
            VaultCommand::Gc {
                apply,
                now,
                dry_run,
            } => {
                let Some(_) = account_uid() else {
                    eprintln!("{} no {ACCOUNT} account: the vault is not installed (cig tin vault install)", ctx.red("error[E724 burn]:"));
                    return exit::SCRIPT_ERROR;
                };
                let live = live_lines();
                if live.is_empty() {
                    eprintln!(
                        "{} no run names any object, so everything would be garbage; the vault refuses that. Nothing was done",
                        ctx.red("error[E724 burn]:")
                    );
                    return exit::SCRIPT_ERROR;
                }
                let (_, uid) = me();
                run_all(ctx, &[gc_step(uid, apply, now, live)], dry_run)
            }
            VaultCommand::Update { binary, dry_run } => {
                let bin = match daemon_binary(binary) {
                    Ok(b) => b,
                    Err(e) => {
                        eprintln!("{} {e}", ctx.red("error[E724 burn]:"));
                        return exit::USAGE;
                    }
                };
                let code = run_all(ctx, &update_steps(&bin), dry_run);
                if code != exit::OK || dry_run {
                    return code;
                }
                // The restart step is `systemctl restart` or `launchctl
                // kickstart`: neither waits for the `Type=simple` daemon's
                // socket. Wait here so a next call never finds no socket
                // after a successful update.
                match wait_for_socket(UPDATE_WAIT) {
                    Ok(()) => {
                        eprintln!("{} the daemon is swapped and restarted", ctx.green("ok:"));
                        exit::OK
                    }
                    Err(e) => {
                        eprintln!(
                            "{} the restarted vault did not answer within {}s: {e}",
                            ctx.red("error[E724 burn]:"),
                            UPDATE_WAIT.as_secs()
                        );
                        exit::SCRIPT_ERROR
                    }
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_sudo_rule_names_one_user_one_uid_and_asks_for_a_password() {
            let r = sudoers_rule("jay", 1000);
            assert!(r.contains("jay ALL=(_cigtin) /usr/local/bin/cig-vault gc --uid 1000,"));
            assert!(r.contains("gc --uid 1000 --grace-days 0 --apply"));
            assert!(!r.contains("NOPASSWD") && !r.contains('*'));
        }

        #[test]
        fn the_unit_runs_as_the_account_and_is_not_setuid_anything() {
            let u = systemd_unit();
            assert!(u.contains("User=_cigtin") && u.contains("NoNewPrivileges=yes"));
            assert!(u.contains("ExecStart=/usr/local/bin/cig-vault serve"));
            let p = launchd_plist();
            assert!(p.contains("<string>_cigtin</string>") && p.contains("com.otmof.cig-vault"));
        }

        #[test]
        fn install_makes_the_account_only_when_it_is_missing_and_checks_the_rule_before_it_lands() {
            let with = install_steps(Path::new("/x/cig-vault"), "u", 1, false);
            let without = install_steps(Path::new("/x/cig-vault"), "u", 1, true);
            assert_eq!(with.len(), without.len() + 1);
            let names: Vec<&str> = without.iter().map(|s| s.what.as_str()).collect();
            let check = names.iter().position(|n| *n == "check the rule").unwrap();
            let put = names
                .iter()
                .position(|n| n.starts_with("put the rule in place"))
                .unwrap();
            assert!(check < put);
            assert!(without.iter().all(|s| !s
                .argv
                .iter()
                .any(|a| a.contains("chmod u+s") || a == "4755")));
        }

        #[test]
        fn uninstall_keeps_the_store_unless_purged() {
            let keep = uninstall_steps(false);
            let purge = uninstall_steps(true);
            assert!(!keep.iter().any(|s| s.argv.contains(&"-rf".to_string())));
            assert!(purge
                .iter()
                .any(|s| s.argv.contains(&cig_vault::DEFAULT_ROOT.to_string())));
        }

        #[test]
        fn gc_goes_through_the_account_with_the_live_set_on_its_input() {
            let s = gc_step(1000, true, true, "o x\n".into());
            assert_eq!(&s.argv[..3], ["-u", "_cigtin", BIN]);
            assert!(s
                .argv
                .ends_with(&["--grace-days".into(), "0".into(), "--apply".into()]));
            assert_eq!(s.stdin.as_deref(), Some("o x\n"));
        }
    }
}
