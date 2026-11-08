// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! The wall (README, "Break it") and this file are the same list: a row may
//! claim a cheat failed only when a test named after the cheat exists. Rows
//! covered by tests elsewhere name that test; the rest live here.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The cig under test: this build's, or `CIG_TEST_BIN` when that names
/// another. cig-compile runs these tests with a cig that builds each script
/// and runs the program (its `ci/cig-compiled.sh`), so a compiled program's
/// burns are held to the same tests (otmof-ops/cig-compile#6).
fn cig_bin() -> std::path::PathBuf {
    std::env::var_os("CIG_TEST_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_BIN_EXE_cig")))
}

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
    fn write(&self, name: &str, content: &str) -> PathBuf {
        let p = self.path().join(name);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, content).unwrap();
        p
    }
    fn cig(&self, args: &[&str]) -> Output {
        Command::new(cig_bin())
            .args(args)
            .current_dir(self.path())
            .env("CIGSCRIPT_HOME", self.path().join("home"))
            .env("NO_COLOR", "1")
            .env("CIG_SEAL_KEY", "file")
            .output()
            .expect("run cig")
    }
    fn stderr(&self, args: &[&str]) -> String {
        String::from_utf8_lossy(&self.cig(args).stderr).to_string()
    }
    fn run_id(&self) -> String {
        let out = self.cig(&["--json", "runs"]);
        let v: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
        v[0]["id"].as_str().unwrap().to_string()
    }
}

#[test]
fn effect_hidden_in_a_function_a_lambda_in_map_and_proc_run_outside_a_burn_are_refused() {
    let sb = Sandbox::new();
    // Statically: the top-level call is refused before anything runs.
    sb.write("top.cig", "proc.run(\"true\", [])\n");
    let err = sb.stderr(&["run", "top.cig"]);
    assert!(err.contains("error[E303 check]"), "{err}");
    // At run time: hidden in a function called outside any burn.
    sb.write(
        "fn.cig",
        "pull hidden() {\n  fs.write_text(\"x.txt\", \"boo\")\n}\nhidden()\n",
    );
    let out = sb.cig(&["run", "fn.cig"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("error[E701 burn]"));
    assert!(
        !sb.path().join("x.txt").exists(),
        "the effect happened anyway"
    );
    // At run time: hidden in a lambda handed to .map.
    sb.write(
        "map.cig",
        "stick names = [\"a\", \"b\"]\nnames.map(pack(n) => fs.write_text(\"${n}.txt\", n))\n",
    );
    let out = sb.cig(&["run", "map.cig"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("error[E701 burn]"));
    assert!(!sb.path().join("a.txt").exists());
}

#[test]
fn cough_mid_burn_after_a_write_and_a_delete_rolls_back_newest_first() {
    let sb = Sandbox::new();
    sb.write("keep.txt", "original");
    sb.write("gone.txt", "to be deleted");
    sb.write(
        "s.cig",
        "burn {\n  fs.write_text(\"keep.txt\", \"changed\")\n  fs.rm(\"gone.txt\")\n  cough \"mid-burn\"\n}\n",
    );
    let err = sb.stderr(&["run", "s.cig"]);
    assert!(err.contains("error[E600 cough]: mid-burn"), "{err}");
    let restored: Vec<&str> = err.lines().filter(|l| l.contains("restored")).collect();
    assert_eq!(restored.len(), 2, "{err}");
    assert!(
        restored[0].contains("delete"),
        "newest first: the delete is undone before the write: {err}"
    );
    assert!(restored[1].contains("write"), "{err}");
    assert_eq!(
        fs::read_to_string(sb.path().join("keep.txt")).unwrap(),
        "original"
    );
    assert_eq!(
        fs::read_to_string(sb.path().join("gone.txt")).unwrap(),
        "to be deleted"
    );
}

#[test]
fn mv_over_an_existing_file_then_unburn_brings_both_back() {
    let sb = Sandbox::new();
    sb.write("a.txt", "A");
    sb.write("b.txt", "B");
    sb.write("s.cig", "burn {\n  fs.mv(\"a.txt\", \"b.txt\")\n}\n");
    assert!(sb.cig(&["run", "s.cig"]).status.success());
    assert!(!sb.path().join("a.txt").exists());
    assert_eq!(fs::read_to_string(sb.path().join("b.txt")).unwrap(), "A");
    let id = sb.run_id();
    assert!(sb.cig(&["unburn", &id]).status.success());
    assert_eq!(fs::read_to_string(sb.path().join("a.txt")).unwrap(), "A");
    assert_eq!(fs::read_to_string(sb.path().join("b.txt")).unwrap(), "B");
}

#[test]
fn rm_on_a_non_empty_dir_then_rollback_restores_tree_and_modes() {
    let sb = Sandbox::new();
    sb.write("tree/sub/deep.txt", "deep");
    sb.write("tree/top.txt", "top");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            sb.path().join("tree/sub/deep.txt"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        fs::set_permissions(
            sb.path().join("tree/top.txt"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    sb.write("s.cig", "burn {\n  fs.rm(\"tree\")\n}\ncough \"abort\"\n");
    let err = sb.stderr(&["run", "s.cig"]);
    assert!(err.contains("restored") && !err.contains("failed"), "{err}");
    assert_eq!(
        fs::read_to_string(sb.path().join("tree/sub/deep.txt")).unwrap(),
        "deep"
    );
    assert_eq!(
        fs::read_to_string(sb.path().join("tree/top.txt")).unwrap(),
        "top"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let m = |p: &str| {
            fs::metadata(sb.path().join(p))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(m("tree/sub/deep.txt"), 0o600);
        assert_eq!(m("tree/top.txt"), 0o755);
    }
}

#[test]
fn killed_mid_burn_leaves_the_journal_intact_and_unburn_restores_everything() {
    // A process killed with SIGKILL cannot clean up. Simulate exactly what it
    // leaves: journaled burns, no totals, a dead pid.
    let sb = Sandbox::new();
    sb.write("a.txt", "one");
    sb.write("s.cig", "burn {\n  fs.write_text(\"a.txt\", \"two\")\n  fs.write_text(\"b.txt\", \"new\")\n  fs.mkdir(\"made/deep\")\n}\n");
    assert!(sb.cig(&["run", "s.cig"]).status.success());
    let id = sb.run_id();
    let rec_path = sb.path().join("home/runs").join(&id).join("run.json");
    let mut rec: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&rec_path).unwrap()).unwrap();
    rec["status"] = serde_json::json!("running");
    rec["burns"] = serde_json::json!(0);
    rec["finished"] = serde_json::Value::Null;
    rec["pid"] = serde_json::json!(2_147_483_647u32);
    fs::write(&rec_path, serde_json::to_string(&rec).unwrap()).unwrap();
    let out = sb.cig(&["--json", "runs"]);
    let v: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v[0]["status"], "interrupted");
    assert_eq!(v[0]["burns"], 3);
    assert!(sb.cig(&["unburn", &id]).status.success());
    assert_eq!(fs::read_to_string(sb.path().join("a.txt")).unwrap(), "one");
    assert!(!sb.path().join("b.txt").exists());
    assert!(!sb.path().join("made").exists());
}

#[test]
fn unbounded_recursion_is_a_clean_e506() {
    let sb = Sandbox::new();
    sb.write("s.cig", "pull down(n) {\n  snuff down(n + 1)\n}\ndown(0)\n");
    let out = sb.cig(&["run", "s.cig"]);
    assert_eq!(out.status.code(), Some(1), "not a crash: {:?}", out.status);
    assert!(String::from_utf8_lossy(&out.stderr).contains("error[E506 runtime]"));
}

#[test]
fn arithmetic_edges_are_diagnostics_never_silent() {
    let sb = Sandbox::new();
    for (expr, code) in [
        ("9223372036854775807 + 1", "E503"),
        ("(9223372036854775807 + 9223372036854775807) / 2", "E503"),
        ("-9223372036854775808 - 1", "E503"),
        ("1.0 / 0.0", "E502"),
        ("0.0 / 0.0", "E502"),
        ("7 % 0", "E502"),
    ] {
        let out = sb.cig(&["eval", "--", expr]);
        assert_eq!(out.status.code(), Some(1), "{expr}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(code), "{expr}: expected {code}\n{err}");
    }
}
