// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! #146: the step budget stops on the same line, step for step, whatever
//! the interpreter does underneath. Each script under `tests/steps` runs
//! with every `--max-steps` from 1 until it finishes, and where each stop
//! falls is compared with its `.steps` file, made by the interpreter
//! before the change.
//!
//! To remake the files with another cig (the release before a change to
//! the evaluator): `CIG_STEPS_BIN=<that cig> CIG_STEPS_BLESS=1 cargo test
//! --test steps`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where each budget from 1 stops: `N line:col`, then `N done` for the
/// first budget the script finishes within.
fn stops(cig: &Path, script: &Path) -> String {
    let home = tempfile::tempdir().expect("tempdir");
    let one = |n: u64| -> String {
        let out = Command::new(cig)
            .args(["--json", "run", "--max-steps", &n.to_string()])
            .arg(script)
            .current_dir(home.path())
            .env("CIGSCRIPT_HOME", home.path().join("home"))
            .env("CIG_SEAL_KEY", "file")
            .env("CIG_DOCTOR", "0")
            .env("NO_COLOR", "1")
            .env("CIG_INTERNAL_TRACKER_DIR", home.path().join("tracker"))
            .env("CIG_UPDATE_MANIFEST", home.path().join("no-releases.json"))
            .env_remove("CI")
            .env_remove("CIG_MAX_STEPS")
            .output()
            .expect("run cig");
        if out.status.success() {
            return "done".to_string();
        }
        // With --json the run's record is stdout's last line, and it
        // carries the diagnostic the run stopped on.
        let said = String::from_utf8_lossy(&out.stdout);
        let last = said.lines().last().unwrap_or_default();
        let record: serde_json::Value = serde_json::from_str(last).unwrap_or_else(|_| {
            panic!(
                "{}: budget {n}: no run record: {said}{}",
                script.display(),
                String::from_utf8_lossy(&out.stderr)
            )
        });
        let d = &record["run"]["diagnostic"];
        assert_eq!(
            d["code"],
            "E515",
            "{}: budget {n} stopped on something else: {last}",
            script.display()
        );
        format!("{}:{}", d["line"], d["col"])
    };
    // Every budget, eight at a time, until one finishes the script.
    let mut lines = Vec::new();
    let mut n = 1u64;
    'outer: loop {
        let batch: Vec<(u64, String)> = std::thread::scope(|s| {
            let handles: Vec<_> = (n..n + 8).map(|k| s.spawn(move || (k, one(k)))).collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for (k, at) in batch {
            let done = at == "done";
            lines.push(format!("{k} {at}"));
            if done {
                break 'outer;
            }
        }
        n += 8;
        assert!(n < 100_000, "{} never finished", script.display());
    }
    lines.join("\n") + "\n"
}

#[test]
fn step_counts_are_unchanged() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/steps");
    let cig = std::env::var_os("CIG_STEPS_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_cig")));
    let bless = std::env::var_os("CIG_STEPS_BLESS").is_some();
    let mut scripts: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "cig"))
        .collect();
    scripts.sort();
    assert!(scripts.len() >= 4, "the step scripts are missing");
    for script in scripts {
        let got = stops(&cig, &script);
        let golden = script.with_extension("steps");
        if bless {
            fs::write(&golden, &got).unwrap();
            continue;
        }
        let want = fs::read_to_string(&golden)
            .unwrap_or_else(|e| panic!("{}: {e}", golden.display()))
            .replace("\r\n", "\n");
        if got != want {
            let first = got
                .lines()
                .zip(want.lines())
                .find(|(g, w)| g != w)
                .map(|(g, w)| format!("now `{g}`, before `{w}`"))
                .unwrap_or_else(|| "a different number of budgets".to_string());
            panic!(
                "{}: the step budget stops somewhere else: {first}",
                script.display()
            );
        }
    }
}
