// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! The seam to `ciginstaller`: each stub hands over with the mapped
//! arguments and passes the exit code through, or says whose job it is and
//! exits 127; the retired install scripts redirect; `cig __doctor --json`
//! has the installer doctor's finding shape and never calls the installer.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("tempdir"),
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn home(&self) -> PathBuf {
        self.path().join("home")
    }

    /// A fake `ciginstaller` in `dir`: it records its arguments, one per
    /// line, in `args.txt` beside it and exits with `code`.
    #[cfg(unix)]
    fn fake_installer(&self, dir: &Path, code: i32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        fs::create_dir_all(dir).unwrap();
        let log = dir.join("args.txt");
        let exe = dir.join("ciginstaller");
        fs::write(
            &exe,
            format!(
                "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{}'\necho fake-installer-ran\nexit {code}\n",
                log.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        log
    }

    /// `cig` with a clean world: the sandbox as home, `path` as PATH.
    fn cig(&self, args: &[&str], path: &Path, keep_home_var: bool) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_cig"));
        c.args(args)
            .current_dir(self.path())
            .env("HOME", self.path())
            .env("PATH", path)
            .env("NO_COLOR", "1")
            .env("CIG_SEAL_KEY", "file")
            .env("CIG_INTERNAL_NO_NS_PROBE", "1")
            .env("CIG_INTERNAL_TRACKER_DIR", self.path().join("tracker"))
            .env("CIG_UPDATE_MANIFEST", self.path().join("no-releases.json"))
            .env_remove("CI");
        if keep_home_var {
            c.env("CIGSCRIPT_HOME", self.home());
        } else {
            c.env_remove("CIGSCRIPT_HOME");
        }
        c.output().expect("run cig")
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).to_string()
}

// only the unix tests drive a fake ciginstaller that logs its arguments
#[cfg(unix)]
fn args_of(log: &Path) -> Vec<String> {
    fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn assert_missing_message(out: &Output, what: &str, command: &str) {
    let err = text(&out.stderr);
    assert_eq!(out.status.code(), Some(127), "{err}");
    assert!(
        err.contains(&format!("{what} is ciginstaller's job now: {command}")),
        "{err}"
    );
    assert!(err.contains("get ciginstaller: "), "{err}");
}

// ----- update ------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn update_hands_over_to_the_installer_and_passes_its_exit_code() {
    let sb = Sandbox::new();
    let bin = sb.path().join("bin");
    let log = sb.fake_installer(&bin, 7);
    let out = sb.cig(&["update", "--check"], &bin, true);
    assert_eq!(out.status.code(), Some(7), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("fake-installer-ran"));
    assert_eq!(args_of(&log), ["update", "--check"]);
    let out = sb.cig(&["--json", "update"], &bin, true);
    assert_eq!(out.status.code(), Some(7));
    assert_eq!(args_of(&log), ["update", "--json"]);
}

#[test]
fn update_without_the_installer_says_so_and_exits_127() {
    let sb = Sandbox::new();
    let empty = sb.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let out = sb.cig(&["update", "--check"], &empty, true);
    assert_missing_message(&out, "cig update", "ciginstaller update --check");
}

#[cfg(unix)]
#[test]
fn update_to_a_version_is_explained_not_run() {
    let sb = Sandbox::new();
    let bin = sb.path().join("bin");
    let log = sb.fake_installer(&bin, 0);
    for flags in [
        &["update", "--to", "1.2.0"][..],
        &["update", "--allow-downgrade"],
    ] {
        let out = sb.cig(flags, &bin, true);
        let err = text(&out.stderr);
        assert_eq!(out.status.code(), Some(3), "{err}");
        assert!(
            err.contains("ciginstaller carton list")
                && err.contains("ciginstaller update --to <carton>"),
            "{err}"
        );
    }
    assert!(!log.exists(), "the installer must not have been run");
}

// ----- uninstall ---------------------------------------------------------------

#[cfg(unix)]
#[test]
fn uninstall_hands_over_to_the_installer_for_cig() {
    let sb = Sandbox::new();
    let bin = sb.path().join("bin");
    let log = sb.fake_installer(&bin, 2);
    let out = sb.cig(&["uninstall", "--yes"], &bin, true);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert_eq!(args_of(&log), ["uninstall", "cig", "--yes"]);
}

#[test]
fn uninstall_without_the_installer_says_so_and_exits_127() {
    let sb = Sandbox::new();
    let empty = sb.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let out = sb.cig(&["uninstall"], &empty, true);
    assert_missing_message(&out, "cig uninstall", "ciginstaller uninstall cig");
}

// ----- doctor ------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn bare_doctor_and_doctor_fix_hand_over_to_the_tool_check() {
    let sb = Sandbox::new();
    let bin = sb.path().join("bin");
    let log = sb.fake_installer(&bin, 2);
    for args in [&["doctor"][..], &["doctor", "--fix"]] {
        let out = sb.cig(args, &bin, true);
        assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
        assert_eq!(args_of(&log), ["doctor", "--tools"]);
        fs::remove_file(&log).unwrap();
    }
}

#[test]
fn doctor_without_the_installer_says_so_and_exits_127() {
    let sb = Sandbox::new();
    let empty = sb.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let out = sb.cig(&["doctor"], &empty, true);
    assert_missing_message(&out, "cig doctor", "ciginstaller doctor --tools");
}

#[cfg(unix)]
#[test]
fn doctor_of_a_run_stays_native() {
    let sb = Sandbox::new();
    let bin = sb.path().join("bin");
    let log = sb.fake_installer(&bin, 0);
    let out = sb.cig(&["doctor", "zzz"], &bin, true);
    assert_eq!(out.status.code(), Some(3));
    assert!(text(&out.stderr).contains("error[E803 usage]"));
    assert!(!log.exists(), "the installer must not have been run");
}

// ----- where the installer is looked for ---------------------------------------

#[cfg(unix)]
#[test]
fn the_installer_is_found_on_path_then_in_the_cigscript_homes() {
    let sb = Sandbox::new();
    let empty = sb.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    // $CIGSCRIPT_HOME/bin
    let home_log = sb.fake_installer(&sb.home().join("bin"), 11);
    let out = sb.cig(&["update"], &empty, true);
    assert_eq!(out.status.code(), Some(11), "{}", text(&out.stderr));
    assert!(home_log.exists());
    // PATH wins over it.
    let path_dir = sb.path().join("bin");
    let path_log = sb.fake_installer(&path_dir, 12);
    fs::remove_file(&home_log).unwrap();
    let out = sb.cig(&["update"], &path_dir, true);
    assert_eq!(out.status.code(), Some(12));
    assert!(path_log.exists() && !home_log.exists());
    // ~/.cigscript/bin when CIGSCRIPT_HOME is not set.
    let sb2 = Sandbox::new();
    let empty2 = sb2.path().join("empty");
    fs::create_dir_all(&empty2).unwrap();
    let dot_log = sb2.fake_installer(&sb2.path().join(".cigscript").join("bin"), 13);
    let out = sb2.cig(&["update"], &empty2, false);
    assert_eq!(out.status.code(), Some(13), "{}", text(&out.stderr));
    assert!(dot_log.exists());
}

// ----- the retired install scripts ---------------------------------------------

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[cfg(unix)]
#[test]
fn install_sh_parses_and_only_redirects() {
    let script = root().join("install.sh");
    let out = Command::new("sh").arg("-n").arg(&script).output().unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    let out = Command::new("sh").arg(&script).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = text(&out.stderr);
    assert!(
        err.contains("cig is installed by ciginstaller now: ciginstaller install stick cig"),
        "{err}"
    );
    assert!(
        err.contains(&format!(
            "get ciginstaller: {}",
            cigscript::installer::BOOTSTRAP
        )),
        "{err}"
    );
}

#[test]
fn install_scripts_carry_the_contract_message() {
    for name in ["install.sh", "install.ps1"] {
        let body = fs::read_to_string(root().join(name)).unwrap();
        assert!(
            body.contains("cig is installed by ciginstaller now: ciginstaller install stick cig"),
            "{name}"
        );
        assert!(body.contains("exit 1"), "{name}");
    }
    #[cfg(windows)]
    assert!(fs::read_to_string(root().join("install.ps1"))
        .unwrap()
        .contains(cigscript::installer::BOOTSTRAP));
}

// ----- the release asset name the installer's manifest reads --------------------

#[test]
fn the_release_asset_name_is_unchanged() {
    let name = cigscript::update::asset_name(&cigscript::update::Version(1, 21, 0));
    assert_eq!(
        name,
        format!(
            "cig-v1.21.0-{}-{}{}",
            std::env::consts::OS,
            std::env::consts::ARCH,
            if cfg!(windows) { ".exe" } else { "" }
        )
    );
}

// ----- __doctor and announce-fixed ----------------------------------------------

#[test]
fn product_doctor_json_has_the_installer_doctors_finding_shape() {
    let sb = Sandbox::new();
    let empty = sb.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let out = sb.cig(&["__doctor", "--json"], &empty, true);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["tool"], "cig");
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    let findings = v["findings"].as_array().unwrap();
    assert!(findings.len() > 5, "{v}");
    for f in findings {
        assert!(f["check"].as_str().is_some_and(|s| !s.is_empty()), "{f}");
        assert!(
            matches!(f["severity"].as_str(), Some("ok" | "note" | "action")),
            "{f}"
        );
        assert!(f["found"].is_string() && f["because"].is_string(), "{f}");
        assert!(f["fix"].is_null() || f["fix"].is_string(), "{f}");
    }
    let state = findings.iter().find(|f| f["check"] == "state-dir").unwrap();
    assert_eq!(state["severity"], "ok");
    // Nothing about PATH, packages or releases: those are the installer's.
    for f in findings {
        let c = f["check"].as_str().unwrap();
        assert!(!matches!(c, "path" | "tools" | "update"), "{c}");
    }
}

#[cfg(unix)]
#[test]
fn product_doctor_never_calls_the_installer() {
    let sb = Sandbox::new();
    let bin = sb.path().join("bin");
    let log = sb.fake_installer(&bin, 0);
    let out = sb.cig(&["__doctor", "--json"], &bin, true);
    assert_eq!(out.status.code(), Some(0));
    assert!(!log.exists(), "__doctor called ciginstaller");
}

#[test]
fn product_doctor_reports_a_bad_state_dir_as_an_action() {
    let sb = Sandbox::new();
    let empty = sb.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    // The state dir is a file: it cannot be written.
    fs::write(sb.home(), "not a dir").unwrap();
    let out = sb.cig(&["__doctor", "--json"], &empty, true);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let f = v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["check"] == "state-dir")
        .unwrap();
    assert_eq!(f["severity"], "action", "{v}");
    assert!(f["fix"].is_string());
}

#[test]
fn the_internal_entries_are_hidden_from_help() {
    let sb = Sandbox::new();
    let empty = sb.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let help = text(&sb.cig(&["--help"], &empty, true).stdout);
    assert!(!help.contains("__doctor"), "{help}");
    assert!(!help.contains("__crash"), "{help}");
    assert!(!help.contains("report-bundle"), "{help}");
    let help = text(&sb.cig(&["crash", "--help"], &empty, true).stdout);
    assert!(!help.contains("announce-fixed"), "{help}");
}

#[test]
fn announce_fixed_says_nothing_when_nothing_was_filed() {
    let sb = Sandbox::new();
    let empty = sb.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let out = sb.cig(&["--json", "crash", "announce-fixed"], &empty, true);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["fixed_reports"].as_array().map(Vec::len), Some(0), "{v}");
}

// ----- crash and report (phase F3) ---------------------------------------------

/// One pair of tests per stub: a fake installer gets the mapped arguments and
/// its exit code passes through; with none there are two lines and 127.
macro_rules! stub_pair {
    ($with:ident, $without:ident, $what:expr, $command:expr, $argv:expr, $mapped:expr) => {
        #[cfg(unix)]
        #[test]
        fn $with() {
            let sb = Sandbox::new();
            let bin = sb.path().join("bin");
            let log = sb.fake_installer(&bin, 9);
            let out = sb.cig(&$argv, &bin, true);
            assert_eq!(out.status.code(), Some(9), "{}", text(&out.stderr));
            assert!(text(&out.stdout).contains("fake-installer-ran"));
            assert_eq!(args_of(&log), $mapped);
        }

        #[test]
        fn $without() {
            let sb = Sandbox::new();
            let empty = sb.path().join("empty");
            fs::create_dir_all(&empty).unwrap();
            let out = sb.cig(&$argv, &empty, true);
            assert_missing_message(&out, $what, $command);
        }
    };
}

stub_pair!(
    crash_list_hands_over_to_the_installer,
    crash_list_without_the_installer_says_so_and_exits_127,
    "cig crash list",
    "ciginstaller crash list",
    ["crash", "list"],
    ["crash", "list"]
);
stub_pair!(
    crash_list_forwards_json,
    crash_list_json_without_the_installer_exits_127,
    "cig crash list",
    "ciginstaller crash list --json",
    ["--json", "crash", "list"],
    ["crash", "list", "--json"]
);
stub_pair!(
    crash_show_hands_over_to_the_installer,
    crash_show_without_the_installer_says_so_and_exits_127,
    "cig crash show",
    "ciginstaller crash show abc",
    ["crash", "show", "abc"],
    ["crash", "show", "abc"]
);
stub_pair!(
    crash_send_hands_over_to_the_installer,
    crash_send_without_the_installer_says_so_and_exits_127,
    "cig crash send",
    "ciginstaller crash send abc",
    ["crash", "send", "abc"],
    ["crash", "send", "abc"]
);
stub_pair!(
    crash_delete_hands_over_to_the_installer,
    crash_delete_without_the_installer_says_so_and_exits_127,
    "cig crash delete",
    "ciginstaller crash delete --all",
    ["crash", "delete", "--all"],
    ["crash", "delete", "--all"]
);
stub_pair!(
    report_with_no_run_starts_the_bug_report_for_cig,
    report_with_no_run_without_the_installer_says_so_and_exits_127,
    "cig report",
    "ciginstaller bug cig",
    ["report"],
    ["bug", "cig"]
);
stub_pair!(
    report_of_a_run_names_the_run_and_the_part,
    report_of_a_run_without_the_installer_says_so_and_exits_127,
    "cig report",
    "ciginstaller bug --run r1 --part cig",
    ["report", "r1"],
    ["bug", "--run", "r1", "--part", "cig"]
);
stub_pair!(
    report_send_forwards_send,
    report_send_without_the_installer_says_so_and_exits_127,
    "cig report",
    "ciginstaller bug --run r1 --part cig --send",
    ["report", "r1", "--send"],
    ["bug", "--run", "r1", "--part", "cig", "--send"]
);
stub_pair!(
    report_out_writes_the_bundle_alone,
    report_out_without_the_installer_says_so_and_exits_127,
    "cig report",
    "ciginstaller report r1 --cig --out b.json",
    ["report", "r1", "--out", "b.json"],
    ["report", "r1", "--cig", "--out", "b.json"]
);

#[test]
fn crash_flush_submit_and_announce_fixed_never_call_the_installer() {
    // capture and the local queue work without it: with one that fails the
    // test if it runs, the native subcommands still answer
    let sb = Sandbox::new();
    let empty = sb.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let out = sb.cig(&["--json", "crash", "announce-fixed"], &empty, true);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let out = sb.cig(&["crash", "flush", "--quiet"], &empty, true);
    assert_ne!(out.status.code(), Some(127), "{}", text(&out.stderr));
}

#[test]
fn report_out_needs_a_run() {
    let sb = Sandbox::new();
    let empty = sb.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let out = sb.cig(&["report", "--out", "b.json"], &empty, true);
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));
}

#[cfg(unix)]
#[test]
fn the_json_report_of_a_run_stays_native_for_older_installers() {
    // an installer from before report-bundle calls `cig --json report <run>`;
    // handing that back to the installer would loop
    let sb = Sandbox::new();
    let bin = sb.path().join("bin");
    let log = sb.fake_installer(&bin, 0);
    let out = sb.cig(&["--json", "report", "zzz"], &bin, true);
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));
    assert!(!log.exists(), "the installer must not have been run");
}
