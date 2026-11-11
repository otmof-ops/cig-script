// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! #146: errors and backtraces read the same whatever the interpreter does
//! underneath (names resolved at check time, array scopes, and what comes
//! after). Each `tests/walker/err-*.cig` runs and its whole output, exit
//! status included, is compared with its `.expect`, made by the interpreter
//! before the change.
//!
//! To remake the files with another cig (the release before a change to
//! the evaluator): `CIG_WALKER_BIN=<that cig> CIG_WALKER_BLESS=1 cargo test
//! --test walker`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use wait_timeout::ChildExt;

fn run(cig: &Path, script: &Path, args: &[&str]) -> String {
    let home = tempfile::tempdir().expect("tempdir");
    let name = script.file_name().expect("name");
    fs::copy(script, home.path().join(name)).expect("copy script");
    let mut child = Command::new(cig)
        .args(args)
        .arg(name)
        .current_dir(home.path())
        .env("HOME", home.path())
        .env("CIGSCRIPT_HOME", home.path().join("home"))
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("XDG_STATE_HOME", home.path().join("state"))
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("CIG_SEAL_KEY", "file")
        .env("CIG_DOCTOR", "0")
        .env("NO_COLOR", "1")
        .env("CIG_MAX_STEPS", "5000")
        .env("CIG_INTERNAL_TRACKER_DIR", home.path().join("tracker"))
        .env("CIG_UPDATE_MANIFEST", home.path().join("no-releases.json"))
        .env_remove("CI")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("run cig");
    let status = match child.wait_timeout(Duration::from_secs(60)).expect("wait") {
        Some(s) => s,
        None => {
            child.kill().ok();
            child.wait().ok();
            panic!("{}: cig did not finish in 60 s", script.display());
        }
    };
    let out = child.wait_with_output().expect("output");
    format!(
        "$ cig {} {}\nexit {}\n--- stdout\n{}--- stderr\n{}",
        args.join(" "),
        name.to_string_lossy(),
        status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn errors_and_backtraces_are_unchanged() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/walker");
    let bless = std::env::var_os("CIG_WALKER_BLESS").is_some();
    let cig = std::env::var_os("CIG_WALKER_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_cig")));
    let mut scripts: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("tests/walker")
        .map(|e| e.expect("entry").path())
        .filter(|p| {
            p.extension().is_some_and(|x| x == "cig")
                && p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("err-"))
        })
        .collect();
    scripts.sort();
    assert!(!scripts.is_empty(), "no err-*.cig scripts");
    let mut wrong = Vec::new();
    for script in &scripts {
        let got = format!(
            "{}\n{}",
            run(&cig, script, &["run"]),
            run(&cig, script, &["check"])
        );
        let expect = script.with_extension("expect");
        if bless {
            fs::write(&expect, &got).expect("write expect");
        } else {
            let want = fs::read_to_string(&expect)
                .unwrap_or_else(|_| panic!("{} has no .expect", script.display()));
            if got != want {
                wrong.push(format!(
                    "{}:\n--- want\n{want}\n--- got\n{got}",
                    script.display()
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}
