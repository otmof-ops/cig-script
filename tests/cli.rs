// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! End-to-end tests against the built `cig` binary.
//!
//! Every test gets its own state directory through `CIGSCRIPT_HOME`, so
//! nothing here touches the developer's real run records. Golden tests in
//! `tests/scripts/` compare stdout byte for byte.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Sandbox {
    dir: tempfile::TempDir,
}

/// A contained hop's scratch folder lives outside the sandbox (in /var/tmp
/// or the temporary directory, never inside the home or a pack), and a kept
/// layer keeps it: the sandbox takes its runs' folders with it.
#[cfg(target_os = "linux")]
impl Drop for Sandbox {
    fn drop(&mut self) {
        let Ok(runs) = fs::read_dir(self.home().join("runs")) else {
            return;
        };
        let uid = unsafe { libc::geteuid() };
        let ids: Vec<String> = runs
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        for base in [PathBuf::from("/var/tmp"), std::env::temp_dir()] {
            let Ok(rd) = fs::read_dir(&base) else {
                continue;
            };
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if ids
                    .iter()
                    .any(|id| name.starts_with(&format!("cig-box-{uid}-{id}-")))
                {
                    let _ = Command::new("chmod")
                        .args(["-R", "u+rwx"])
                        .arg(e.path())
                        .status();
                    let _ = fs::remove_dir_all(e.path());
                }
            }
        }
    }
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

    fn write(&self, name: &str, content: &str) -> PathBuf {
        let p = self.path().join(name);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, content).unwrap();
        p
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.path().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
    }

    fn exists(&self, name: &str) -> bool {
        self.path().join(name).exists()
    }

    fn cig(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_cig"))
            .args(args)
            .current_dir(self.path())
            .env("CIGSCRIPT_HOME", self.home())
            .env("NO_COLOR", "1")
            .env("CIG_SEAL_KEY", "file")
            // Every cig a test starts, and every background flush it spawns,
            // files into the sandbox's own tracker, never a real one.
            .env("CIG_INTERNAL_TRACKER_DIR", self.path().join("tracker"))
            // A runner's CI must not reach the sandbox: the one test that
            // wants it sets it itself.
            .env_remove("CI")
            // No test reaches the releases page: a pin that needs a fetch
            // finds no network here, as a machine offline would.
            .env("CIG_UPDATE_MANIFEST", self.path().join("no-releases.json"))
            .output()
            .expect("run cig")
    }

    fn run_ok(&self, args: &[&str]) -> String {
        let out = self.cig(args);
        assert!(
            out.status.success(),
            "cig {args:?} failed with {:?}\nstdout:\n{}\nstderr:\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn run_err(&self, args: &[&str], code: i32) -> String {
        let out = self.cig(args);
        assert_eq!(
            out.status.code(),
            Some(code),
            "cig {args:?}: expected exit {code}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stderr).to_string()
    }

    /// `cig`, started in a folder under the sandbox, the sandbox as home.
    #[cfg(unix)]
    fn cig_in(&self, sub: &str, args: &[&str], envs: &[(&str, &str)]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_cig"))
            .args(args)
            .current_dir(self.path().join(sub))
            .env("CIGSCRIPT_HOME", self.home())
            .env("HOME", self.path())
            .env("NO_COLOR", "1")
            .env("CIG_SEAL_KEY", "file")
            // Every cig a test starts, and every background flush it spawns,
            // files into the sandbox's own tracker, never a real one.
            .env("CIG_INTERNAL_TRACKER_DIR", self.path().join("tracker"))
            .env("CIG_UPDATE_MANIFEST", self.path().join("no-releases.json"))
            .env_remove("CI")
            .envs(envs.iter().copied())
            .output()
            .expect("run cig")
    }

    fn cig_env(&self, args: &[&str], envs: &[(&str, &str)]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_cig"))
            .args(args)
            .current_dir(self.path())
            .env("CIGSCRIPT_HOME", self.home())
            .env("NO_COLOR", "1")
            .env("CIG_SEAL_KEY", "file")
            // Every cig a test starts, and every background flush it spawns,
            // files into the sandbox's own tracker, never a real one.
            .env("CIG_INTERNAL_TRACKER_DIR", self.path().join("tracker"))
            .env("CIG_UPDATE_MANIFEST", self.path().join("no-releases.json"))
            .env_remove("CI")
            .envs(envs.iter().copied())
            .output()
            .expect("run cig")
    }

    fn runs(&self) -> Vec<serde_json::Value> {
        let out = self.run_ok(&["--json", "runs"]);
        serde_json::from_str(&out).expect("runs json")
    }

    /// `cig`, started and left running (for a test that kills it).
    #[cfg(target_os = "linux")]
    fn cig_spawn(&self, args: &[&str]) -> std::process::Child {
        Command::new(env!("CARGO_BIN_EXE_cig"))
            .args(args)
            .current_dir(self.path())
            .env("CIGSCRIPT_HOME", self.home())
            .env("NO_COLOR", "1")
            .env("CIG_SEAL_KEY", "file")
            // Every cig a test starts, and every background flush it spawns,
            // files into the sandbox's own tracker, never a real one.
            .env("CIG_INTERNAL_TRACKER_DIR", self.path().join("tracker"))
            .env("CIG_UPDATE_MANIFEST", self.path().join("no-releases.json"))
            .env_remove("CI")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn cig")
    }
}

// ----- golden scripts --------------------------------------------------------

#[test]
fn golden_scripts_match_expected_output() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/scripts");
    let mut count = 0;
    let mut entries: Vec<_> = fs::read_dir(&dir).unwrap().flatten().collect();
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        // The path module prints the platform's separators; its golden is
        // the Unix one.
        if cfg!(windows) {
            let n = path.to_string_lossy();
            if n.contains("05-modules") || n.contains("06-fs-read") {
                continue;
            }
        }
        if path.extension().is_none_or(|e| e != "cig") {
            continue;
        }
        let expected_path = path.with_extension("out");
        let expected = fs::read_to_string(&expected_path)
            .unwrap_or_else(|_| panic!("missing {}", expected_path.display()));
        let sb = Sandbox::new();
        let out = sb.cig(&["run", path.to_str().unwrap()]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{} failed:\n{}",
            path.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(stdout, expected, "output mismatch for {}", path.display());
        count += 1;
    }
    assert!(count >= 5, "expected golden scripts, found {count}");
}

#[test]
fn examples_pass_check() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let sb = Sandbox::new();
    for entry in fs::read_dir(&dir).unwrap().flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "cig") {
            sb.run_ok(&["check", path.to_str().unwrap()]);
        }
    }
}

// ----- burn kernel ------------------------------------------------------------

#[test]
fn effect_outside_burn_is_refused_before_running() {
    let sb = Sandbox::new();
    sb.write(
        "s.cig",
        "exhale \"start\"\nfs.write_text(\"x.txt\", \"no\")\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 2);
    assert!(err.contains("Don't see any cigarettes."), "{err}");
    assert!(err.contains("must be inside a burn block"), "{err}");
    assert!(!sb.exists("x.txt"));
}

#[test]
fn effect_outside_burn_at_runtime_is_refused() {
    let sb = Sandbox::new();
    sb.write(
        "s.cig",
        "pull write_it() {\n  fs.write_text(\"x.txt\", \"no\")\n}\nwrite_it()\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(err.contains("error[E701 burn]"), "{err}");
    assert!(!sb.exists("x.txt"));
    // The same function is fine when called from inside a burn.
    sb.write(
        "ok.cig",
        "pull write_it() {\n  fs.write_text(\"x.txt\", \"yes\")\n}\nburn {\n  write_it()\n}\n",
    );
    sb.run_ok(&["run", "ok.cig"]);
    assert_eq!(sb.read("x.txt"), "yes");
}

#[test]
fn dry_run_changes_nothing_and_prints_a_plan() {
    let sb = Sandbox::new();
    sb.write("keep.txt", "original");
    sb.write(
        "s.cig",
        "burn {\n  fs.write_text(\"keep.txt\", \"changed\")\n  fs.mkdir(\"d\")\n  fs.write_text(\"d/new.txt\", \"n\")\n  fs.mv(\"d/new.txt\", \"d/moved.txt\")\n  stick r = proc.run(\"echo\", [\"hi\"])\n  exhale r.simulated, r.out\n}\n",
    );
    let out = sb.cig(&["run", "--dry-run", "s.cig"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("dry-run plan: 5 ops"), "{stderr}");
    assert!(stderr.contains("write keep.txt"), "{stderr}");
    assert!(stderr.contains("irreversible  run echo hi"), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "true \n");
    assert_eq!(sb.read("keep.txt"), "original");
    assert!(!sb.exists("d"));
    assert!(sb.runs().is_empty(), "dry-run must not leave a run record");
}

#[test]
fn failed_run_rolls_back_every_journaled_op() {
    let sb = Sandbox::new();
    sb.write("keep.txt", "original");
    sb.write("gone.txt", "to be deleted");
    fs::create_dir_all(sb.path().join("tree/sub")).unwrap();
    sb.write("tree/sub/deep.txt", "deep");
    sb.write(
        "s.cig",
        concat!(
            "burn {\n",
            "  fs.write_text(\"keep.txt\", \"changed\")\n",
            "  fs.append_text(\"keep.txt\", \"!\")\n",
            "  fs.rm(\"gone.txt\")\n",
            "  fs.rm(\"tree\")\n",
            "  fs.mkdir(\"made/nested\")\n",
            "  fs.write_text(\"made/nested/f.txt\", \"x\")\n",
            "  fs.cp(\"keep.txt\", \"copy.txt\")\n",
            "  fs.mv(\"copy.txt\", \"moved.txt\")\n",
            "}\n",
            "cough \"abort\"\n"
        ),
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(err.contains("error[E600 cough]: abort"), "{err}");
    assert!(err.contains("unburn: rolling back"), "{err}");
    assert!(!err.contains("failed"), "{err}");
    assert_eq!(sb.read("keep.txt"), "original");
    assert_eq!(sb.read("gone.txt"), "to be deleted");
    assert_eq!(sb.read("tree/sub/deep.txt"), "deep");
    assert!(!sb.exists("made"));
    assert!(!sb.exists("copy.txt"));
    assert!(!sb.exists("moved.txt"));
    let runs = sb.runs();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0]["status"], "rolled_back");
    assert_eq!(runs[0]["burns"], 8);
}

#[test]
fn unburn_restores_a_successful_run_later() {
    let sb = Sandbox::new();
    sb.write("a.txt", "one");
    sb.write("s.cig", "burn {\n  fs.write_text(\"a.txt\", \"two\")\n  fs.write_text(\"b.txt\", \"new\")\n  fs.mv(\"a.txt\", \"c.txt\")\n}\n");
    sb.run_ok(&["run", "s.cig"]);
    assert_eq!(sb.read("c.txt"), "two");
    assert!(sb.exists("b.txt"));
    let runs = sb.runs();
    let id = runs[0]["id"].as_str().unwrap().to_string();
    assert_eq!(runs[0]["status"], "ok");
    assert_eq!(runs[0]["burns"], 3);

    // A unique prefix is enough, and --dry-run touches nothing.
    let prefix = &id[..id.len() - 2];
    sb.run_ok(&["unburn", prefix, "--dry-run"]);
    assert_eq!(sb.read("c.txt"), "two");

    sb.run_ok(&["unburn", prefix]);
    assert_eq!(sb.read("a.txt"), "one");
    assert!(!sb.exists("b.txt"));
    assert!(!sb.exists("c.txt"));
    assert_eq!(sb.runs()[0]["status"], "unburned");
}

#[test]
fn unlit_burns_never_execute_but_record_intents() {
    let sb = Sandbox::new();
    sb.write("precious.txt", "keep me");
    sb.write(
        "s.cig",
        "burn unlit {\n  fs.rm(\"precious.txt\")\n  proc.run(\"rm\", [\"-rf\", \"/\"])\n}\nexhale \"still here:\", fs.exists(\"precious.txt\")\n",
    );
    let out = sb.cig(&["run", "s.cig"]);
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "still here: true\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unlit intents: 2 ops"), "{stderr}");
    let runs = sb.runs();
    let dir = sb.home().join("runs").join(runs[0]["id"].as_str().unwrap());
    let intents = fs::read_to_string(dir.join("intents.jsonl")).unwrap();
    assert_eq!(intents.lines().count(), 2);
    assert!(intents.contains("\"class\":\"unlit\""));
}

#[test]
fn no_rollback_flag_leaves_burns_in_place() {
    let sb = Sandbox::new();
    sb.write(
        "s.cig",
        "burn {\n  fs.write_text(\"x.txt\", \"kept\")\n}\ncough \"boom\"\n",
    );
    sb.run_err(&["run", "--no-rollback", "s.cig"], 1);
    assert_eq!(sb.read("x.txt"), "kept");
    assert_eq!(sb.runs()[0]["status"], "error");
}

#[test]
fn proc_run_captures_output_and_honours_check_and_timeout() {
    let sb = Sandbox::new();
    sb.write(
        "s.cig",
        concat!(
            "burn {\n",
            "  stick r = proc.run(\"sh\", [\"-c\", \"echo out; echo err 1>&2; exit 3\"])\n",
            "  exhale r.code, r.out.trim(), r.err.trim()\n",
            "  try {\n",
            "    proc.run(\"sh\", [\"-c\", \"exit 7\"], {check: true})\n",
            "  } ashtray e {\n",
            "    exhale \"caught\", e.code\n",
            "  }\n",
            "  try {\n",
            "    proc.run(\"sleep\", [\"5\"], {timeout_ms: 200})\n",
            "  } ashtray e {\n",
            "    exhale e.message.contains(\"killed\")\n",
            "  }\n",
            "}\n"
        ),
    );
    let out = sb.run_ok(&["run", "s.cig"]);
    assert_eq!(out, "3 out err\ncaught 7\ntrue\n");
}

// ----- diagnostics and modes -----------------------------------------------------

#[test]
fn check_reports_errors_and_warnings_with_json() {
    let sb = Sandbox::new();
    sb.write(
        "s.cig",
        "pull f() {\n  fs.rm(\"x\")\n}\nroll a = 1\nexhale b\nstick c = 1\nc = 2\n",
    );
    let out = sb.cig(&["--json", "check", "s.cig"]);
    assert_eq!(out.status.code(), Some(2));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["errors"].as_array().unwrap().len(), 2);
    assert_eq!(v["warnings"].as_array().unwrap().len(), 1);
    assert_eq!(v["errors"][0]["message"], "unknown name `b`");
    assert_eq!(v["errors"][0]["hint"], "did you mean `a`?");
}

#[test]
fn syntax_errors_exit_2_and_point_at_the_problem() {
    let sb = Sandbox::new();
    sb.write("s.cig", "roll x = 1\nif x {\n  exhale x\n");
    let err = sb.run_err(&["run", "s.cig"], 2);
    assert!(err.contains("never closed"), "{err}");
    assert!(err.contains("s.cig:2:6"), "{err}");
    sb.write("t.cig", "exhael 1\n");
    let err = sb.run_err(&["check", "t.cig"], 2);
    assert!(err.contains("did you mean `exhale`?"), "{err}");
}

#[test]
fn runtime_errors_exit_1_and_are_catchable() {
    let sb = Sandbox::new();
    sb.write("s.cig", "roll m = {a: 1}\nexhale m.b\n");
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(err.contains("map has no key `b`"), "{err}");
    sb.write("t.cig", "try {\n  roll m = {a: 1}\n  exhale m.b\n} ashtray e {\n  exhale e.kind, e.line, e.message\n}\nexit(4)\n");
    let out = sb.cig(&["run", "t.cig"]);
    assert_eq!(out.status.code(), Some(4));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "runtime 3 map has no key `b`\n"
    );
}

#[test]
fn eval_prints_values_and_json() {
    let sb = Sandbox::new();
    assert_eq!(
        sb.run_ok(&["eval", "[1, 2, 3].map(pack(x) => x * x)"]),
        "[1, 4, 9]\n"
    );
    assert_eq!(sb.run_ok(&["eval", "roll a = 2; a * 21"]), "42\n");
    assert_eq!(
        sb.run_ok(&["--json", "eval", "{a: 1.5, b: \"x\"}"]),
        "{\"a\":1.5,\"b\":\"x\"}\n"
    );
    let err = sb.run_err(&["eval", "fs.rm(\"x\")"], 1);
    assert!(err.contains("error[E701 burn]"), "{err}");
}

#[test]
fn args_reach_the_script() {
    let sb = Sandbox::new();
    sb.write("s.cig", "exhale args.len(), args.join(\"|\")\n");
    assert_eq!(sb.run_ok(&["run", "s.cig", "--", "a", "b c"]), "2 a|b c\n");
}

#[test]
fn language_and_doctor_work_in_json() {
    let sb = Sandbox::new();
    let v: serde_json::Value = serde_json::from_str(&sb.run_ok(&["--json", "language"])).unwrap();
    assert!(v["library"].as_array().unwrap().len() > 10);
    let out = sb.cig(&["--json", "doctor"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["state_dir_writable"], true);
    assert_eq!(v["runs"], 0);
}

#[test]
fn runs_prune_keeps_the_newest() {
    let sb = Sandbox::new();
    sb.write("s.cig", "exhale 1\n");
    for _ in 0..4 {
        sb.run_ok(&["run", "s.cig"]);
    }
    assert_eq!(sb.runs().len(), 4);
    sb.run_ok(&["runs", "--prune", "2"]);
    assert_eq!(sb.runs().len(), 2);
}

#[test]
fn deep_recursion_is_an_error_not_a_crash() {
    let sb = Sandbox::new();
    sb.write("s.cig", "pull down(n) {\n  snuff down(n + 1)\n}\ndown(0)\n");
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(err.contains("call depth exceeded"), "{err}");
}

// ----- 2.1: chains, error codes, config, crash reports, updates -----------------------

#[test]
fn chains_list_and_light_from_the_cli() {
    let sb = Sandbox::new();
    sb.write(
        "tasks.cig",
        concat!(
            "roll log = []\n",
            "stick prepare = pack() {\n  burn { fs.write_text(\"out.txt\", \"prepared\") }\n  log.push(\"prepare\")\n}\n",
            "stick verify = pack() {\n  log.push(\"verify\")\n  snuff fs.read_text(\"out.txt\")\n}\n",
            "stick explode = pack() { cough \"kaboom\" }\n",
            "chain deploy { prepare, verify }\n",
            "chain broken { prepare, explode, verify }\n",
            "exhale \"top level ran\"\n"
        ),
    );
    let out = sb.run_ok(&["chains", "tasks.cig"]);
    assert!(
        out.contains("deploy") && out.contains("1. prepare") && out.contains("broken"),
        "{out}"
    );
    let v: serde_json::Value =
        serde_json::from_str(&sb.run_ok(&["--json", "chains", "tasks.cig"])).unwrap();
    assert_eq!(
        v["chains"][0]["steps"],
        serde_json::json!(["prepare", "verify"])
    );

    let out = sb.cig(&["light", "tasks.cig", "deploy"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("chain deploy: step 2/2 verify ok"),
        "{stderr}"
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "top level ran\n");
    assert_eq!(sb.read("out.txt"), "prepared");

    // A failing chain fails the run and rolls back what it burned.
    fs::remove_file(sb.path().join("out.txt")).unwrap();
    let err = sb.run_err(&["light", "tasks.cig", "broken"], 1);
    assert!(err.contains("error[E602 cough]"), "{err}");
    assert!(err.contains("failed at step 2 (explode)"), "{err}");
    assert!(
        !sb.exists("out.txt"),
        "the burn from step 1 must be rolled back"
    );

    let err = sb.run_err(&["light", "tasks.cig", "nope"], 1);
    assert!(
        err.contains("E804") && err.contains("chains here: deploy, broken"),
        "{err}"
    );

    // Dry-run lights the chain without burning.
    let out = sb.cig(&["light", "--dry-run", "tasks.cig", "deploy"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("dry-run plan: 1 op"), "{stderr}");
    assert!(!sb.exists("out.txt"));
}

#[test]
fn error_codes_render_and_explain() {
    let sb = Sandbox::new();
    let err = sb.run_err(&["eval", "[1][9]"], 1);
    assert!(
        err.contains("error[E504 runtime]: index 9 is out of range"),
        "{err}"
    );
    assert!(err.contains("= explain: cig explain E504"), "{err}");
    let out = sb.run_ok(&["explain", "E504"]);
    assert!(out.contains("index out of range"), "{out}");
    let all: serde_json::Value = serde_json::from_str(&sb.run_ok(&["--json", "explain"])).unwrap();
    assert!(all.as_array().unwrap().len() > 40);
    sb.run_err(&["explain", "E9999"], 3);
    let v: serde_json::Value =
        serde_json::from_slice(&sb.cig(&["--json", "check", "nope.cig"]).stdout)
            .unwrap_or_default();
    let _ = v;
    sb.write("s.cig", "roll a = 1\nexhale b\n");
    let v: serde_json::Value =
        serde_json::from_slice(&sb.cig(&["--json", "check", "s.cig"]).stdout).unwrap();
    assert_eq!(v["errors"][0]["code"], "E301");
}

#[test]
fn config_reads_validates_and_persists() {
    let sb = Sandbox::new();
    assert_eq!(sb.run_ok(&["config", "crash_reports"]), "ask\n");
    sb.run_ok(&["config", "crash_reports", "never"]);
    assert_eq!(sb.run_ok(&["config", "crash_reports"]), "never\n");
    assert!(sb.read("home/config").contains("crash_reports = never"));
    sb.run_err(&["config", "crash_reports", "sometimes"], 3);
    sb.run_err(&["config", "nonsense", "1"], 3);
    sb.run_ok(&["config", "crash_reports", "--unset"]);
    assert_eq!(sb.run_ok(&["config", "crash_reports"]), "ask\n");
    let v: serde_json::Value = serde_json::from_str(&sb.run_ok(&["--json", "config"])).unwrap();
    assert_eq!(v["config"]["issues_repo"]["value"], "otmof-ops/cig-script");
}

#[test]
fn a_panic_writes_a_redacted_crash_report_and_never_sends_without_consent() {
    let sb = Sandbox::new();
    sb.write("s.cig", "exhale 1\n");
    let out = Command::new(env!("CARGO_BIN_EXE_cig"))
        .args(["run", "s.cig", "--token=supersecret"])
        .current_dir(sb.path())
        .env("CIGSCRIPT_HOME", sb.home())
        .env("NO_COLOR", "1")
        .env("CIG_SEAL_KEY", "file")
        .env("CIG_INTERNAL_TRACKER_DIR", sb.path().join("tracker"))
        .env("CIG_INTERNAL_PANIC", "1")
        .env_remove("CI")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(70));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("error[E901 internal]: cig crashed"),
        "{stderr}"
    );
    assert!(stderr.contains("cig crash send"), "{stderr}");
    let reports: serde_json::Value =
        serde_json::from_str(&sb.run_ok(&["--json", "crash", "list"])).unwrap();
    assert_eq!(reports.as_array().unwrap().len(), 1);
    let r = &reports[0];
    assert_eq!(r["sent"], serde_json::Value::Null);
    let cmd = r["command"].as_array().unwrap();
    assert!(cmd.iter().any(|a| a == "--token=<redacted>"), "{cmd:?}");
    assert!(!serde_json::to_string(r).unwrap().contains("supersecret"));
    assert!(r["panic_location"]
        .as_str()
        .unwrap()
        .starts_with("src/cli/mod.rs"));
    let id = r["id"].as_str().unwrap().to_string();
    let shown = sb.run_ok(&["crash", "show", &id]);
    assert!(shown.contains("\"fingerprint\""));
    // never mode: no prompt, no send, report stays local.
    sb.run_ok(&["config", "crash_reports", "never"]);
    let out = Command::new(env!("CARGO_BIN_EXE_cig"))
        .args(["run", "s.cig"])
        .current_dir(sb.path())
        .env("CIGSCRIPT_HOME", sb.home())
        .env("CIG_INTERNAL_TRACKER_DIR", sb.path().join("tracker"))
        .env("CIG_INTERNAL_PANIC", "1")
        .env_remove("CI")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("crash_reports is \"never\""));
    sb.run_ok(&["crash", "delete", "--all"]);
    assert_eq!(sb.run_ok(&["--json", "crash", "list"]).trim(), "[]");
}

#[test]
fn update_checks_verifies_and_installs_from_a_local_manifest() {
    let sb = Sandbox::new();
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_cig"));
    // A private, user-owned install dir (0755) holding a copy of the binary.
    let inst = sb.path().join("inst");
    fs::create_dir_all(&inst).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&inst, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::copy(&bin, inst.join("cig")).unwrap();
    // The "release": the same binary under a huge version, with its checksum.
    let asset = sb.path().join("cig-v99.0.0-asset");
    fs::copy(&bin, &asset).unwrap();
    let sum = cigscript::burn::journal::sha256_file(&asset).unwrap();
    sb.write("asset.sha256", &format!("{sum}  cig\n"));
    let name = cigscript::update::asset_name(&cigscript::update::Version(99, 0, 0));
    let manifest = serde_json::json!([{
        "tag_name": "v99.0.0", "prerelease": false, "draft": false, "html_url": "https://example.invalid/r", "published_at": "",
        "assets": [
            {"name": name, "browser_download_url": format!("file://{}", asset.display())},
            {"name": format!("{name}.sha256"), "browser_download_url": format!("file://{}", sb.path().join("asset.sha256").display())}
        ]
    }]);
    sb.write("manifest.json", &manifest.to_string());
    let run = |args: &[&str]| {
        Command::new(inst.join("cig"))
            .args(args)
            .current_dir(sb.path())
            .env("CIGSCRIPT_HOME", sb.home())
            .env("NO_COLOR", "1")
            .env("CIG_SEAL_KEY", "file")
            .env("CIG_UPDATE_MANIFEST", sb.path().join("manifest.json"))
            .output()
            .unwrap()
    };
    let out = run(&["--json", "update", "--check"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["latest"], "99.0.0");
    assert_eq!(v["newer"], true);
    assert!(sb.exists("home/update-check.json"));
    // doctor reports the cached newer version
    let d = String::from_utf8_lossy(&run(&["doctor"]).stdout).to_string();
    assert!(d.contains("newer than this build"), "{d}");

    let out = run(&["update"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(
        stderr.contains("verified: sha256 matches") && stderr.contains("updated:"),
        "{stderr}"
    );
    assert!(!inst.join(".cig.new").exists());

    // A corrupted checksum is refused and nothing is installed.
    sb.write("asset.sha256", "deadbeef  cig\n");
    let out = run(&["update"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("checksum mismatch"));
}

#[test]
fn installer_script_parses() {
    let out = Command::new("sh")
        .args(["-n", concat!(env!("CARGO_MANIFEST_DIR"), "/install.sh")])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn lab_examples_run_end_to_end() {
    let ex = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let sb = Sandbox::new();

    // measurements: a citable report with the input's hash
    sb.write("readings.csv", "series,value\ntemperature,21.4\ntemperature,21.9\ntemperature,22.1\npressure,101.2\npressure,101.5\n");
    let out = sb.run_ok(&[
        "run",
        ex.join("lab-measurements.cig").to_str().unwrap(),
        "--",
        "readings.csv",
        "report.json",
    ]);
    assert!(out.contains("temperature") && out.contains("n=3"), "{out}");
    let report: serde_json::Value = serde_json::from_str(&sb.read("report.json")).unwrap();
    assert_eq!(report["series"]["temperature"]["n"], 3);
    assert_eq!(report["series"]["pressure"]["n"], 2);
    assert_eq!(report["input_sha256"].as_str().unwrap().len(), 64);

    // manifest: write, verify clean, detect drift
    fs::create_dir_all(sb.path().join("data/sub")).unwrap();
    sb.write("data/a.txt", "alpha");
    sb.write("data/sub/b.txt", "beta");
    let manifest = ex.join("lab-manifest.cig");
    sb.run_ok(&["run", manifest.to_str().unwrap(), "--", "data"]);
    assert!(sb.exists("data/MANIFEST.json"));
    let out = sb.run_ok(&["run", manifest.to_str().unwrap(), "--", "data", "verify"]);
    assert!(out.contains("0 changed, 0 missing, 0 new"), "{out}");
    sb.write("data/a.txt", "ALPHA");
    fs::remove_file(sb.path().join("data/sub/b.txt")).unwrap();
    sb.write("data/c.txt", "new");
    let err_out = sb.cig(&["run", manifest.to_str().unwrap(), "--", "data", "verify"]);
    assert_eq!(err_out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&err_out.stdout);
    assert!(text.contains("1 changed, 1 missing, 1 new"), "{text}");

    // pipeline: light it for real, then dry-run it
    let pipeline = ex.join("lab-pipeline.cig");
    let out = sb.run_ok(&["light", pipeline.to_str().unwrap(), "analysis"]);
    assert!(out.contains("| temperature | 3 |"), "{out}");
    assert!(out.contains("1 out-of-range row(s) dropped"), "{out}");
    let report: serde_json::Value = serde_json::from_str(&sb.read("work/report.json")).unwrap();
    assert_eq!(report["result"]["stats"]["temperature"]["n"], 3);
    assert_eq!(report["result"]["dropped"], 1);
    fs::remove_dir_all(sb.path().join("work")).unwrap();
    let out = sb.cig(&["light", "--dry-run", pipeline.to_str().unwrap(), "analysis"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("dry-run plan: 5 ops"), "{stderr}");
    assert!(!sb.exists("work"));
}

// ----- the Hammer update: verified bugs and the rollback contract -----------

#[test]
fn interrupted_run_is_recognised_counted_from_the_journal_and_unburnable() {
    let sb = Sandbox::new();
    sb.write("a.txt", "one");
    sb.write(
        "s.cig",
        "burn {\n  fs.write_text(\"a.txt\", \"two\")\n  fs.write_text(\"b.txt\", \"new\")\n}\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    let runs = sb.runs();
    let id = runs[0]["id"].as_str().unwrap().to_string();
    // Forge what a process killed mid-burn leaves behind: status still
    // `running`, totals never written, a pid that is gone.
    let rec_path = sb.home().join("runs").join(&id).join("run.json");
    let mut rec: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&rec_path).unwrap()).unwrap();
    rec["status"] = serde_json::json!("running");
    rec["burns"] = serde_json::json!(0);
    rec["finished"] = serde_json::Value::Null;
    rec["pid"] = serde_json::json!(2_147_483_647u32);
    fs::write(&rec_path, serde_json::to_string(&rec).unwrap()).unwrap();

    let runs = sb.runs();
    assert_eq!(runs[0]["status"], "interrupted", "{runs:?}");
    assert_eq!(runs[0]["burns"], 2, "counted from the journal: {runs:?}");

    let doctor: serde_json::Value =
        serde_json::from_slice(&sb.cig(&["--json", "doctor"]).stdout).unwrap();
    assert_eq!(doctor["interrupted_runs"][0], id, "{doctor}");
    let text = String::from_utf8_lossy(&sb.cig(&["doctor"]).stdout).to_string();
    assert!(
        text.contains("interrupted mid-burn") && text.contains("E706"),
        "{text}"
    );

    let out = sb.cig(&["doctor", &id]);
    let text = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        text.contains("error[E706 burn]")
            && text.contains("= doctor: I think the process was killed"),
        "{text}"
    );

    sb.run_ok(&["unburn", &id]);
    assert_eq!(sb.read("a.txt"), "one");
    assert!(!sb.exists("b.txt"));
    assert_eq!(sb.runs()[0]["status"], "unburned");
}

#[test]
fn unburn_refuses_a_second_time_without_force() {
    let sb = Sandbox::new();
    sb.write("a.txt", "one");
    sb.write("s.cig", "burn {\n  fs.write_text(\"a.txt\", \"two\")\n}\n");
    sb.run_ok(&["run", "s.cig"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    sb.run_ok(&["unburn", &id]);
    assert_eq!(sb.read("a.txt"), "one");
    sb.write("a.txt", "three");
    let err = sb.run_err(&["unburn", &id], 1);
    assert!(
        err.contains("error[E705 burn]:") && err.contains("already rolled back"),
        "{err}"
    );
    assert_eq!(
        sb.read("a.txt"),
        "three",
        "a refused unburn touches nothing"
    );
    // --dry-run is always allowed.
    sb.run_ok(&["unburn", &id, "--dry-run"]);
    assert_eq!(sb.read("a.txt"), "three");
    sb.run_ok(&["unburn", &id, "--force"]);
    assert_eq!(sb.read("a.txt"), "one");
}

#[test]
fn unburn_checks_every_snapshot_before_touching_anything() {
    let sb = Sandbox::new();
    sb.write("m1.txt", "old1");
    sb.write("m2.txt", "old2");
    sb.write(
        "s.cig",
        "burn {\n  fs.write_text(\"m1.txt\", \"new1\")\n  fs.write_text(\"m2.txt\", \"new2\")\n}\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let snaps = sb.home().join("runs").join(&id).join("snapshots");
    let mut removed = 0;
    for e in fs::read_dir(&snaps).unwrap().flatten() {
        if e.file_name().to_string_lossy().contains("m2.txt") {
            fs::remove_file(e.path()).unwrap();
            removed += 1;
        }
    }
    assert_eq!(removed, 1, "one snapshot for m2.txt");
    let err = sb.run_err(&["unburn", &id], 1);
    assert!(
        err.contains("error[E703 burn]:") && err.contains("snapshot missing for"),
        "{err}"
    );
    assert_eq!(
        sb.read("m1.txt"),
        "new1",
        "nothing restored before the check passes"
    );
    assert_eq!(sb.read("m2.txt"), "new2");
    let plan = String::from_utf8_lossy(&sb.cig(&["unburn", &id, "--dry-run"]).stdout).to_string();
    assert!(plan.contains("problem:") && plan.contains("E703"), "{plan}");
    // --force restores what it can and reports the rest.
    let out = sb.cig(&["unburn", &id, "--force"]);
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.contains("restored") && text.contains("failed"),
        "{text}"
    );
    assert_eq!(sb.read("m1.txt"), "old1");
    assert_eq!(sb.read("m2.txt"), "new2");
}

#[test]
fn i64_min_is_a_literal_and_the_overflow_next_to_it_is_a_diagnostic() {
    let sb = Sandbox::new();
    let out = sb.run_ok(&["eval", "--", "-9223372036854775808"]);
    assert_eq!(out.trim(), "-9223372036854775808");
    let out = sb.run_ok(&[
        "eval",
        "--",
        "-9223372036854775808 == -9223372036854775807 - 1",
    ]);
    assert_eq!(out.trim(), "true");
    let err = sb.run_err(&["eval", "--", "9223372036854775808"], 2);
    assert!(
        err.contains("E104") && err.contains("does not fit"),
        "{err}"
    );
    let err = sb.run_err(&["eval", "--", "9223372036854775809"], 2);
    assert!(err.contains("E104"), "{err}");
    let err = sb.run_err(&["eval", "--", "-9223372036854775808 - 1"], 1);
    assert!(err.contains("overflow"), "{err}");
    let err = sb.run_err(&["eval", "--", "0 - -9223372036854775808"], 1);
    assert!(err.contains("overflow"), "{err}");
}

#[test]
fn chain_failures_point_at_the_step_not_the_light_call() {
    let sb = Sandbox::new();
    sb.write(
        "s.cig",
        concat!(
            "roll boom = pack(x) {\n",
            "  cough \"kaboom\"\n",
            "}\n",
            "chain deploy {\n",
            "  pack() => 1\n",
            "  boom\n",
            "}\n",
            "\n",
            "light(deploy)\n"
        ),
    );
    // From inside the script: the step's line, plus where the cough was raised.
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(
        err.contains(
            "error[E602 cough]: chain `deploy` failed at step 2 (boom): kaboom (raised at line 2)"
        ),
        "{err}"
    );
    assert!(
        err.contains("s.cig:6:"),
        "points at the step, not line 9: {err}"
    );
    assert!(!err.contains(":9:"), "{err}");
    // From the CLI: there is no light() call at all, and still no 0:0.
    let err = sb.run_err(&["light", "s.cig", "deploy"], 1);
    assert!(err.contains("failed at step 2 (boom)"), "{err}");
    assert!(err.contains("s.cig:6:"), "{err}");
    assert!(!err.contains(":0:0"), "{err}");
}

#[test]
fn allocation_ceiling_is_a_diagnostic_not_an_abort() {
    let sb = Sandbox::new();
    let err = sb.run_err(&["eval", "\"x\".repeat(9223372036854775807)"], 1);
    assert!(
        err.contains("error[E514 runtime]:") && err.contains("over the ceiling"),
        "{err}"
    );
    let err = sb.run_err(&["eval", "\"ab\" * 9223372036854775807"], 1);
    assert!(err.contains("E514"), "{err}");
    let err = sb.run_err(&["eval", "\"x\".pad_left(9223372036854775807)"], 1);
    assert!(err.contains("E514"), "{err}");
    // The ceiling is configurable, and a value under it is built normally.
    let out = sb.cig_env(
        &["eval", "\"x\".repeat(2000).len()"],
        &[("CIG_MAX_ALLOC", "1024")],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("E514"));
    let out = sb.cig_env(
        &["eval", "\"x\".repeat(10).len()"],
        &[("CIG_MAX_ALLOC", "1024")],
    );
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "10");
    // Reading a file over the ceiling is refused before it is read.
    sb.write("big.txt", &"y".repeat(4096));
    let out = sb.cig_env(
        &["eval", "fs.read_text(\"big.txt\").len()"],
        &[("CIG_MAX_ALLOC", "1024")],
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("E514"));
    sb.run_ok(&["explain", "E514"]);
}

#[cfg(unix)]
#[test]
fn symlinks_roll_back_as_symlinks() {
    use std::os::unix::fs::symlink;
    let sb = Sandbox::new();
    sb.write("target.txt", "t");
    symlink("target.txt", sb.path().join("live.lnk")).unwrap();
    symlink("nowhere/at/all", sb.path().join("dangling.lnk")).unwrap();
    fs::create_dir_all(sb.path().join("tree")).unwrap();
    symlink("../target.txt", sb.path().join("tree/inner.lnk")).unwrap();
    sb.write(
        "s.cig",
        concat!(
            "burn {\n",
            "  fs.rm(\"live.lnk\")\n",
            "  fs.rm(\"dangling.lnk\")\n",
            "  fs.rm(\"tree\")\n",
            "  fs.write_text(\"target.txt\", \"changed\")\n",
            "}\n",
            "cough \"abort\"\n"
        ),
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(
        err.contains("unburn: rolling back") && !err.contains("failed"),
        "{err}"
    );
    let is_link = |p: &str| {
        fs::symlink_metadata(sb.path().join(p))
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
    };
    assert!(
        is_link("live.lnk"),
        "live.lnk came back as a real file or not at all"
    );
    assert_eq!(
        fs::read_link(sb.path().join("live.lnk")).unwrap(),
        Path::new("target.txt")
    );
    assert!(is_link("dangling.lnk"));
    assert_eq!(
        fs::read_link(sb.path().join("dangling.lnk")).unwrap(),
        Path::new("nowhere/at/all")
    );
    assert!(
        is_link("tree/inner.lnk"),
        "links inside a restored directory stay links"
    );
    assert_eq!(sb.read("target.txt"), "t");

    // Writing through a link changes the target; rollback restores the
    // target's bytes and leaves the link a link.
    sb.write(
        "s2.cig",
        "burn {\n  fs.write_text(\"live.lnk\", \"through\")\n}\ncough \"abort\"\n",
    );
    sb.run_err(&["run", "s2.cig"], 1);
    assert!(is_link("live.lnk"));
    assert_eq!(sb.read("target.txt"), "t");
    // ... and a write through a dangling link that would create its target
    // removes that target again.
    symlink("ghost.txt", sb.path().join("ghost.lnk")).unwrap();
    sb.write(
        "s3.cig",
        "burn {\n  fs.write_text(\"ghost.lnk\", \"boo\")\n}\ncough \"abort\"\n",
    );
    sb.run_err(&["run", "s3.cig"], 1);
    assert!(is_link("ghost.lnk"));
    assert!(!sb.exists("ghost.txt"));
    // fs.exists sees a dangling link, so `if fs.exists(p) { fs.rm(p) }` works.
    assert_eq!(
        sb.run_ok(&["eval", "fs.exists(\"dangling.lnk\")"]).trim(),
        "true"
    );
}

// ----- the Hammer update: the error registry -------------------------------

#[test]
fn explain_pages_match_golden_text() {
    let sb = Sandbox::new();
    let nodes: Vec<serde_json::Value> =
        serde_json::from_str(&sb.run_ok(&["--json", "explain"])).unwrap();
    assert!(nodes.len() > 50);
    let mut text = sb.run_ok(&["explain"]);
    for n in &nodes {
        let code = n["code"].as_str().unwrap();
        text.push_str(&format!("\n======== {code} ========\n"));
        text.push_str(&sb.run_ok(&["explain", code]));
    }
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/explain.txt");
    if std::env::var_os("CIG_UPDATE_GOLDEN").is_some() {
        fs::create_dir_all(golden.parent().unwrap()).unwrap();
        fs::write(&golden, &text).unwrap();
    }
    let expected = fs::read_to_string(&golden)
        .unwrap_or_else(|_| panic!("missing {}; run with CIG_UPDATE_GOLDEN=1", golden.display()));
    assert_eq!(
        text, expected,
        "explain wording changed; if that is intended, rerun with CIG_UPDATE_GOLDEN=1"
    );
}

#[test]
fn registry_nodes_and_schema_agree_with_json_diagnostics() {
    let sb = Sandbox::new();
    let schema: serde_json::Value =
        serde_json::from_str(&sb.run_ok(&["explain", "--schema"])).unwrap();
    let codes: Vec<&str> = schema["properties"]["code"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let kinds: Vec<&str> = schema["properties"]["kind"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let props: Vec<&str> = schema["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let nodes: Vec<serde_json::Value> =
        serde_json::from_str(&sb.run_ok(&["--json", "explain"])).unwrap();
    assert_eq!(codes.len(), nodes.len());
    for n in &nodes {
        assert!(codes.contains(&n["code"].as_str().unwrap()));
        assert!(kinds.contains(&n["kind"].as_str().unwrap()), "{n}");
        assert!(
            n["family"].is_string() && n["no"]["hear"].is_string() && n["no"]["manual"].is_string(),
            "{n}"
        );
        assert!(!n["causes"].as_array().unwrap().is_empty(), "{n}");
    }
    let one: serde_json::Value =
        serde_json::from_str(&sb.run_ok(&["--json", "explain", "E508"])).unwrap();
    assert_eq!(one["no"]["id"], "depends");
    assert_eq!(one["causes"][0]["probe"], "path-exists");
    // A real diagnostic validates against the schema, structurally.
    sb.write("s.cig", "roll a = 1\nexhale b\nstick c = 1\nc = 2\n");
    let rep: serde_json::Value =
        serde_json::from_slice(&sb.cig(&["--json", "check", "s.cig"]).stdout).unwrap();
    let diags = rep["errors"].as_array().unwrap();
    assert_eq!(diags.len(), 2);
    for d in diags {
        for k in d.as_object().unwrap().keys() {
            assert!(
                props.contains(&k.as_str()),
                "field `{k}` is not in the schema: {d}"
            );
        }
        assert!(codes.contains(&d["code"].as_str().unwrap()), "{d}");
        assert!(kinds.contains(&d["kind"].as_str().unwrap()), "{d}");
        assert!(d["message"].is_string());
        assert!(d["line"].as_u64().unwrap() >= 1 && d["col"].as_u64().unwrap() >= 1);
    }
}

#[test]
fn check_can_deny_warnings_for_ci() {
    let sb = Sandbox::new();
    sb.write("w.cig", "pull f() {\n  fs.rm(\"x\")\n}\n");
    sb.run_ok(&["check", "w.cig"]);
    let err = sb.run_err(&["check", "w.cig", "--deny-warnings"], 2);
    assert!(
        err.contains("warning[E303 check]") && err.contains("--deny-warnings"),
        "{err}"
    );
    let out = sb.cig(&["--json", "check", "w.cig", "--deny-warnings"]);
    assert_eq!(out.status.code(), Some(2));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["warnings_denied"], true);
    assert_eq!(v["warnings"][0]["code"], "E303");
}

#[test]
fn usage_layer_errors_carry_their_codes() {
    let sb = Sandbox::new();
    let err = sb.run_err(&["run", "nope.cig"], 3);
    assert!(
        err.contains("error[E801 usage]: cannot read nope.cig") && err.contains("cig explain E801"),
        "{err}"
    );
    let err = sb.run_err(&["runs", "zzz"], 3);
    assert!(
        err.contains("error[E803 usage]: no run matches `zzz`"),
        "{err}"
    );
    let err = sb.run_err(&["unburn", "zzz"], 3);
    assert!(err.contains("error[E803 usage]"), "{err}");
    // No transport at all is E805, distinct from a transport that failed (E806).
    let out = sb.cig_env(
        &["update", "--check"],
        &[("PATH", ""), ("CIG_UPDATE_MANIFEST", "")],
    );
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        err.contains("error[E805 usage]") && err.contains("neither `gh` nor `curl`"),
        "{err}"
    );
}

// ----- the Hammer update: doctor on the emit hook -----------------------------

/// Normalise the parts of doctor's output that legitimately vary.
fn normalise(text: &str, sandbox: &Path) -> String {
    let ms = regex::Regex::new(r"\(\d+ ms\)").unwrap();
    let ids = regex::Regex::new(r"\b\d{8}T\d{6}-[0-9a-f]{6}\b").unwrap();
    // macOS hands back the canonical path (/private/var/...) for a tempdir
    // under /var; both spellings are the sandbox.
    let canonical = fs::canonicalize(sandbox).unwrap_or_else(|_| sandbox.to_path_buf());
    let t = text
        .replace(canonical.to_str().unwrap(), "<sb>")
        .replace(sandbox.to_str().unwrap(), "<sb>");
    let t = ms.replace_all(&t, "(N ms)").to_string();
    ids.replace_all(&t, "<run-id>").to_string()
}

/// (hallway phrase, manual name) for every kind of no, from the binary.
fn kinds_of_no() -> Vec<(String, String)> {
    let sb = Sandbox::new();
    let nodes: Vec<serde_json::Value> =
        serde_json::from_str(&sb.run_ok(&["--json", "explain"])).unwrap();
    let mut out: Vec<(String, String)> = Vec::new();
    let mut push = |v: &serde_json::Value| {
        let pair = (
            v["hear"].as_str().unwrap().to_string(),
            v["manual"].as_str().unwrap().to_string(),
        );
        if !out.contains(&pair) {
            out.push(pair);
        }
    };
    for n in &nodes {
        push(&n["no"]);
        for c in n["causes"].as_array().unwrap() {
            if let Some(id) = c["no"].as_str() {
                if let Some(m) = nodes.iter().find(|x| x["no"]["id"] == id) {
                    push(&m["no"]);
                }
            }
        }
    }
    out
}

/// What plain mode must equal: the themed text without the banner line and
/// with every hallway phrase replaced by the manual's name.
fn subtract_theme(themed: &str, nos: &[(String, String)]) -> String {
    let mut out = String::new();
    for line in themed.lines() {
        if line == "Don't see any cigarettes." {
            continue;
        }
        let mut l = line.to_string();
        for (hear, manual) in nos {
            l = l.replace(&format!(" ({hear})"), &format!(" ({manual})"));
        }
        out.push_str(&l);
        out.push('\n');
    }
    out
}

fn corpus_env(sb: &Sandbox) -> Vec<(String, String)> {
    vec![
        ("PATH".to_string(), "/usr/bin:/bin".to_string()),
        ("HOME".to_string(), sb.path().to_str().unwrap().to_string()),
        // A small budget so the runaway-loop case ends in milliseconds.
        ("CIG_MAX_STEPS".to_string(), "100000".to_string()),
    ]
}

fn run_corpus(sb: &Sandbox, name: &str, extra: &[(&str, &str)]) -> Output {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/corpus")
        .join(name);
    fs::copy(&src, sb.path().join(name)).unwrap();
    let base = corpus_env(sb);
    let mut envs: Vec<(&str, &str)> = base.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    envs.extend_from_slice(extra);
    let mut args = corpus_args(&src);
    args.push(name.to_string());
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    sb.cig_env(&argv, &envs)
}

/// A first line `# cig: run --dry-run` chooses the command; `run` otherwise.
fn corpus_args(script: &Path) -> Vec<String> {
    fs::read_to_string(script)
        .unwrap()
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("# cig:"))
        .map(|rest| rest.split_whitespace().map(str::to_string).collect())
        .unwrap_or_else(|| vec!["run".to_string()])
}

#[test]
fn corpus_diagnoses_match_golden_output() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let mut scripts: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "cig"))
        .collect();
    scripts.sort();
    assert!(scripts.len() >= 8, "corpus has {} scripts", scripts.len());
    let update = std::env::var_os("CIG_UPDATE_GOLDEN").is_some();
    let nos = kinds_of_no();
    let mut failures = Vec::new();
    for path in scripts {
        let name = path.file_name().unwrap().to_str().unwrap().to_string();
        // The operating system words a missing program its own way; this
        // golden carries the Unix wording.
        if cfg!(windows) && name.starts_with("e551") {
            continue;
        }
        let sb = Sandbox::new();
        let out = run_corpus(&sb, &name, &[]);
        let text = normalise(&String::from_utf8_lossy(&out.stderr), sb.path());
        let expect = path.with_extension("expect");
        if update {
            fs::write(&expect, &text).unwrap();
        }
        let want = fs::read_to_string(&expect).unwrap_or_else(|_| {
            panic!("missing {}; run with CIG_UPDATE_GOLDEN=1", expect.display())
        });
        if text != want {
            failures.push(format!("{name}:\n--- expected\n{want}\n--- got\n{text}"));
        }
        // Plain mode: golden too, and a strict subtraction of the themed text.
        let base = corpus_env(&sb);
        let mut envs: Vec<(&str, &str)> =
            base.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        envs.push(("CIG_PLAIN", "1"));
        let mut plain_args = corpus_args(&path);
        plain_args.push(name.clone());
        let plain_argv: Vec<&str> = plain_args.iter().map(String::as_str).collect();
        let plain_out = sb.cig_env(&plain_argv, &envs);
        let plain = normalise(&String::from_utf8_lossy(&plain_out.stderr), sb.path());
        let plain_expect = path.with_extension("plain.expect");
        if update {
            fs::write(&plain_expect, &plain).unwrap();
        }
        let plain_want = fs::read_to_string(&plain_expect).unwrap_or_else(|_| {
            panic!(
                "missing {}; run with CIG_UPDATE_GOLDEN=1",
                plain_expect.display()
            )
        });
        if plain != plain_want {
            failures.push(format!(
                "{name} (plain):\n--- expected\n{plain_want}\n--- got\n{plain}"
            ));
        }
        let subtracted = subtract_theme(&text, &nos);
        if plain != subtracted {
            failures.push(format!("{name}: plain mode is not a strict subtraction of themed mode\n--- themed minus catchphrases\n{subtracted}\n--- plain\n{plain}"));
        }
    }
    assert!(
        failures.is_empty(),
        "doctor output changed; if intended, rerun with CIG_UPDATE_GOLDEN=1\n{}",
        failures.join("\n")
    );
}

#[test]
fn no_doctor_is_byte_identical_to_the_bare_diagnostic() {
    let sb = Sandbox::new();
    let with = normalise(
        &String::from_utf8_lossy(&run_corpus(&sb, "e508-missing-file.cig", &[]).stderr),
        sb.path(),
    );
    let base = corpus_env(&sb);
    let mut envs: Vec<(&str, &str)> = base.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let flag = normalise(
        &String::from_utf8_lossy(
            &sb.cig_env(&["--no-doctor", "run", "e508-missing-file.cig"], &envs)
                .stderr,
        ),
        sb.path(),
    );
    envs.push(("CIG_DOCTOR", "0"));
    let env_off = normalise(
        &String::from_utf8_lossy(&sb.cig_env(&["run", "e508-missing-file.cig"], &envs).stderr),
        sb.path(),
    );
    assert!(with.contains("= doctor: I think"), "{with}");
    assert!(!flag.contains("doctor"), "{flag}");
    assert_eq!(flag, env_off);
    let stripped: String = with
        .lines()
        .filter(|l| {
            !(l.starts_with("  = doctor:")
                || l.starts_with("  = fix:")
                || l.starts_with("  = if not:")
                || l.starts_with("  = maybe:")
                || l.starts_with("  =  "))
        })
        .map(|l| format!("{l}\n"))
        .collect();
    assert_eq!(
        flag, stripped,
        "--no-doctor must only remove doctor's lines"
    );
}

#[test]
fn json_diagnostics_carry_the_diagnosis_within_budget() {
    let sb = Sandbox::new();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/e551-not-on-path.cig");
    fs::copy(&src, sb.path().join("p.cig")).unwrap();
    let out = sb.cig_env(&["--json", "run", "p.cig"], &[("PATH", "/usr/bin:/bin")]);
    let first = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap()
        .to_string();
    let v: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(v["code"], "E551");
    assert_eq!(v["subject"], "definitely-not-a-program-xyz");
    let dx = &v["diagnosis"];
    assert_eq!(dx["confirmed"], true, "{dx}");
    assert_eq!(dx["no"], "cant-see-any");
    assert_eq!(dx["probes"][0]["probe"], "on-path");
    assert_eq!(dx["probes"][0]["outcome"], "confirmed");
    for key in [
        "code",
        "confirmed",
        "verdict",
        "why",
        "fix",
        "if_not",
        "probes",
        "ms",
    ] {
        assert!(dx.get(key).is_some(), "diagnosis lacks `{key}`: {dx}");
    }
    assert!(dx["ms"].as_u64().unwrap() < 500, "{dx}");
    // The check command's JSON carries it per error too.
    sb.write("t.cig", "roll count = 1\nexhale cuont\n");
    let rep: serde_json::Value =
        serde_json::from_slice(&sb.cig(&["--json", "check", "t.cig"]).stdout).unwrap();
    assert_eq!(rep["errors"][0]["diagnosis"]["confirmed"], true, "{rep}");
    assert!(rep["errors"][0]["diagnosis"]["why"][0]
        .as_str()
        .unwrap()
        .contains("`count`"));
}

/// Every file under `root` with its size and modified time. Directories are
/// left out: their modified time moves when the OS finishes a pending
/// delete or rename under them (seen on Windows), which says nothing about
/// what a command wrote.
fn tree_fingerprint(root: &Path) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
    let mut out = Vec::new();
    for e in walkdir::WalkDir::new(root).sort_by_file_name() {
        let e = e.unwrap();
        if e.file_type().is_dir() {
            continue;
        }
        let m = e.metadata().unwrap();
        out.push((e.path().to_path_buf(), m.len(), m.modified().unwrap()));
    }
    out
}

/// The entries that differ between two fingerprints, for the message.
fn fingerprint_diff(
    before: &[(PathBuf, u64, std::time::SystemTime)],
    after: &[(PathBuf, u64, std::time::SystemTime)],
) -> Vec<String> {
    let mut out = Vec::new();
    for b in before {
        match after.iter().find(|a| a.0 == b.0) {
            None => out.push(format!("removed {}", b.0.display())),
            Some(a) if a.1 != b.1 || a.2 != b.2 => out.push(format!("changed {}", b.0.display())),
            Some(_) => {}
        }
    }
    for a in after {
        if !before.iter().any(|b| b.0 == a.0) {
            out.push(format!("added {}", a.0.display()));
        }
    }
    out
}

#[test]
fn rediagnosis_is_read_only_and_reproduces_the_verdict() {
    let sb = Sandbox::new();
    run_corpus(&sb, "e508-missing-file.cig", &[]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let before = tree_fingerprint(sb.path());
    let out = sb.cig(&["doctor", &id]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        err.contains("error[E508 runtime]")
            && err.contains("= doctor: I think the path does not exist"),
        "{err}"
    );
    let after = tree_fingerprint(sb.path());
    assert!(
        before == after,
        "cig doctor <run> wrote something: {:?}",
        fingerprint_diff(&before, &after)
    );
    // With the whole state directory read-only it still answers.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let home = sb.home();
        if fs::metadata("/proc/self/status").is_ok()
            && fs::read_to_string("/proc/self/status")
                .unwrap()
                .lines()
                .any(|l| l.starts_with("Uid:\t0\t"))
        {
            return; // root ignores mode bits
        }
        for e in walkdir::WalkDir::new(&home).contents_first(false) {
            let e = e.unwrap();
            if e.file_type().is_dir() {
                fs::set_permissions(e.path(), fs::Permissions::from_mode(0o500)).unwrap();
            }
        }
        let out = sb.cig(&["doctor", &id]);
        for e in walkdir::WalkDir::new(&home) {
            let e = e.unwrap();
            if e.file_type().is_dir() {
                fs::set_permissions(e.path(), fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(String::from_utf8_lossy(&out.stderr).contains("= doctor: I think"));
    }
    // JSON form.
    let v: serde_json::Value =
        serde_json::from_slice(&sb.cig(&["--json", "doctor", &id]).stdout).unwrap();
    assert_eq!(v["diagnosis"]["confirmed"], true, "{v}");
    assert_eq!(v["diagnostic"]["code"], "E508");
}

#[test]
fn plain_mode_drops_the_catchphrases_and_keeps_the_facts() {
    let sb = Sandbox::new();
    let themed = normalise(
        &String::from_utf8_lossy(&run_corpus(&sb, "e508-missing-file.cig", &[]).stderr),
        sb.path(),
    );
    let base = corpus_env(&sb);
    let envs: Vec<(&str, &str)> = base.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let plain = normalise(
        &String::from_utf8_lossy(
            &sb.cig_env(&["--plain", "run", "e508-missing-file.cig"], &envs)
                .stderr,
        ),
        sb.path(),
    );
    let mut env_plain = envs.clone();
    env_plain.push(("CIG_PLAIN", "1"));
    let via_env = normalise(
        &String::from_utf8_lossy(
            &sb.cig_env(&["run", "e508-missing-file.cig"], &env_plain)
                .stderr,
        ),
        sb.path(),
    );
    assert_eq!(plain, via_env);
    assert!(
        themed.starts_with("Don't see any cigarettes.\n"),
        "{themed}"
    );
    assert!(
        !plain.contains("cigarettes") && !plain.contains("ciggies"),
        "{plain}"
    );
    assert!(
        plain.contains("(wrong address"),
        "the manual's name replaces the hallway phrase: {plain}"
    );
    // Everything else is identical: plain is a subtraction.
    let themed_facts: Vec<&str> = themed
        .lines()
        .skip(1)
        .map(|l| l.split(" (can't see any").next().unwrap())
        .collect();
    let plain_facts: Vec<&str> = plain
        .lines()
        .map(|l| l.split(" (wrong address").next().unwrap())
        .collect();
    assert_eq!(themed_facts, plain_facts);
}

#[test]
#[cfg(unix)]
fn report_bundles_a_run_redacted() {
    let sb = Sandbox::new();
    run_corpus(&sb, "e551-not-on-path.cig", &[]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let base = corpus_env(&sb);
    let envs: Vec<(&str, &str)> = base.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let out = sb.cig_env(&["report", &id], &envs);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let printed = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(
        printed.starts_with("~/home/reports/"),
        "the path is printed with ~ for HOME: {printed}"
    );
    let file = sb.home().join("reports").join(format!("{id}.json"));
    let bundle: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(bundle["kind"], "cigscript-report");
    assert_eq!(bundle["run"]["id"], id);
    // Every path is a placeholder of the run's mapping: the working
    // directory is the one root when there is no pack, the script a file
    // under it; the state directory is said to be custom, not where.
    assert_eq!(bundle["run"]["cwd"], "root0", "{}", bundle["run"]);
    assert_eq!(bundle["run"]["script"], "root0/f1.cig", "{}", bundle["run"]);
    assert_eq!(
        bundle["install"]["state_dir"], "<custom>",
        "{}",
        bundle["install"]
    );
    assert_eq!(bundle["diagnostic"]["code"], "E551");
    assert_eq!(
        bundle["diagnosis"]["confirmed"], true,
        "{}",
        bundle["diagnosis"]
    );
    assert_eq!(bundle["journal"].as_array().unwrap().len(), 1);
    assert_eq!(bundle["journal"][0]["reversible"], false);
    assert!(bundle["note"].as_str().unwrap().contains("Anonymised"));
    let text = fs::read_to_string(&file).unwrap();
    assert!(
        !text.contains(sb.path().to_str().unwrap()),
        "an absolute sandbox path leaked into the bundle"
    );
    let out = sb.cig_env(&["--json", "report", &id], &envs);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["run"]["id"], id);
    let err = sb.run_err(&["report", "zzz"], 3);
    assert!(err.contains("error[E803 usage]"), "{err}");
}

// ----- the Hammer update: the step budget and finally -------------------------

#[test]
fn while_true_ends_with_your_loop_never_ends_and_a_line() {
    let sb = Sandbox::new();
    sb.write("loop.cig", "roll i = 0\nwhile true {\n  i += 1\n}\n");
    let err = sb.run_err(&["run", "--max-steps", "5000", "loop.cig"], 1);
    assert!(
        err.contains("error[E515 runtime]: your loop never ends: 5000 steps and still going"),
        "{err}"
    );
    assert!(err.contains("loop.cig:2:"), "the while line: {err}");
    assert!(
        err.contains("= doctor: I think a while loop whose condition never becomes false"),
        "{err}"
    );
    assert!(err.contains("the condition on line 2 is `true`"), "{err}");
    // The environment sets it too; 0 disables it.
    let out = sb.cig_env(&["run", "loop.cig"], &[("CIG_MAX_STEPS", "3000")]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("3000 steps"));
    sb.write(
        "bounded.cig",
        "roll n = 0\nfor i in 1..20001 {\n  n += 1\n}\nexhale n\n",
    );
    let err = sb.run_err(&["run", "--max-steps", "100", "bounded.cig"], 1);
    assert!(err.contains("E515"), "{err}");
    assert_eq!(
        sb.run_ok(&["run", "--max-steps", "0", "bounded.cig"])
            .trim(),
        "20000"
    );
    assert_eq!(
        sb.run_ok(&["run", "bounded.cig"]).trim(),
        "20000",
        "the default budget is generous"
    );
    // Outside a loop the message does not blame a loop.
    sb.write(
        "deep.cig",
        "pull f(n) {\n  if n == 0 { snuff 0 }\n  snuff f(n - 1)\n}\nexhale f(3000)\n",
    );
    let err = sb.run_err(&["run", "--max-steps", "1000", "deep.cig"], 1);
    assert!(
        err.contains("the script needs more than 1000 steps"),
        "{err}"
    );
    // A run that fails on the budget still rolls back what it burned.
    sb.write("k.txt", "keep");
    sb.write(
        "burnloop.cig",
        "burn {\n  fs.write_text(\"k.txt\", \"changed\")\n  while true {}\n}\n",
    );
    let err = sb.run_err(&["run", "--max-steps", "1000", "burnloop.cig"], 1);
    assert!(err.contains("E515") && err.contains("restored"), "{err}");
    assert_eq!(sb.read("k.txt"), "keep");
}

#[test]
fn finally_runs_on_exit_and_a_missing_ashtray_is_a_syntax_error() {
    let sb = Sandbox::new();
    sb.write(
        "x.cig",
        "try {\n  exit(4)\n} finally {\n  exhale \"cleanup\"\n}\nexhale \"not reached\"\n",
    );
    let out = sb.cig(&["run", "x.cig"]);
    assert_eq!(out.status.code(), Some(4));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "cleanup\n");
    sb.write("bad.cig", "try {\n  exhale 1\n}\nexhale 2\n");
    let err = sb.run_err(&["run", "bad.cig"], 2);
    assert!(
        err.contains("expected `ashtray` or `finally` after the `try` block"),
        "{err}"
    );
    sb.write("stray.cig", "finally {\n}\n");
    let err = sb.run_err(&["run", "stray.cig"], 2);
    assert!(err.contains("`finally` without a preceding `try`"), "{err}");
    // finally inside a burn keeps the burn accounting straight.
    sb.write("b.cig", "burn {\n  try {\n    fs.write_text(\"a.txt\", \"1\")\n  } finally {\n    fs.write_text(\"b.txt\", \"2\")\n  }\n}\nfs.write_text(\"c.txt\", \"3\")\n");
    let err = sb.run_err(&["run", "b.cig"], 2);
    assert!(
        err.contains("E303"),
        "the write after the burn is still refused: {err}"
    );
}

// ----- the Hammer update: the cross-run check on unburn ------------------------

#[test]
fn unburn_refuses_when_a_file_changed_since_the_run_unless_forced() {
    let sb = Sandbox::new();
    sb.write("f.txt", "one");
    sb.write(
        "s.cig",
        "burn {\n  fs.write_text(\"f.txt\", \"two\")\n  fs.write_text(\"g.txt\", \"new\")\n}\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    assert!(sb
        .home()
        .join("runs")
        .join(&id)
        .join("after.json")
        .is_file());
    // Untouched since: the plan says so, and the rollback is allowed.
    let plan = sb.run_ok(&["unburn", &id, "--dry-run"]);
    assert!(
        plan.contains("unchanged since") && !plan.contains("changed since:"),
        "{plan}"
    );
    // Someone edits f.txt by hand.
    sb.write("f.txt", "three");
    let plan = sb.run_ok(&["unburn", &id, "--dry-run"]);
    assert!(
        plan.contains("changed since: ") && plan.contains("f.txt") && plan.contains("E704"),
        "{plan}"
    );
    let err = sb.run_err(&["unburn", &id], 1);
    assert!(
        err.contains("error[E704 burn]:") && err.contains("f.txt") && err.contains("now a file"),
        "{err}"
    );
    assert_eq!(
        sb.read("f.txt"),
        "three",
        "a refused unburn touches nothing"
    );
    assert!(sb.exists("g.txt"));
    sb.run_ok(&["unburn", &id, "--force"]);
    assert_eq!(sb.read("f.txt"), "one");
    assert!(!sb.exists("g.txt"));
}

/// #129: a run that burns nothing never asks for the seal key. Its journal
/// writes no line, its record keeps no head and the ledger no line, and
/// `cig unburn` says there is nothing to undo, as before. The first burn
/// asks for the key, once.
#[test]
fn a_run_that_burns_nothing_never_asks_for_the_key() {
    let sb = Sandbox::new();
    let key = sb.path().join("keys").join("seal.key");
    let key_s = key.to_string_lossy().to_string();
    let env = [("CIG_SEAL_KEY_FILE", key_s.as_str())];
    sb.write("quiet.cig", "exhale \"nothing burns\"\n");
    let out = sb.cig_env(&["run", "quiet.cig"], &env);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!key.exists(), "a run that burned nothing asked for the key");
    let rec = sb.runs()[0].clone();
    assert!(rec["journal_head"].is_null(), "{rec}");
    let id = rec["id"].as_str().unwrap().to_string();
    let undo = sb.cig_env(&["unburn", &id], &env);
    assert!(undo.status.success());
    assert!(
        String::from_utf8_lossy(&undo.stderr).contains("burned nothing"),
        "{}",
        String::from_utf8_lossy(&undo.stderr)
    );
    let verified = sb.cig_env(&["runs", &id, "--verify"], &env);
    assert!(
        verified.status.success(),
        "{}{}",
        String::from_utf8_lossy(&verified.stdout),
        String::from_utf8_lossy(&verified.stderr)
    );
    assert!(
        !key.exists(),
        "verifying a run that burned nothing asked for the key"
    );
    // The first burn asks for it, and the run keeps its head.
    sb.write("burn.cig", "burn {\n  fs.write_text(\"f.txt\", \"x\")\n}\n");
    let out = sb.cig_env(&["run", "burn.cig"], &env);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(key.exists(), "the first burn did not ask for the key");
    let rec = sb.runs()[0].clone();
    assert!(
        rec["journal_head"].as_str().is_some_and(|h| !h.is_empty()),
        "{rec}"
    );
}

/// #129: a finished run that burned nothing has no head. Lines found under
/// it later were written after the run, chained or not: E760.
#[test]
fn lines_planted_under_a_headless_run_are_tampering() {
    let sb = Sandbox::new();
    // A real entry, from a run that burned, planted under one that did not.
    sb.write("burn.cig", "burn {\n  fs.write_text(\"f.txt\", \"x\")\n}\n");
    sb.run_ok(&["run", "burn.cig"]);
    let donor = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let entry = fs::read_to_string(sb.home().join("runs").join(&donor).join("journal.jsonl"))
        .unwrap()
        .lines()
        .nth(1)
        .unwrap()
        .to_string();
    std::thread::sleep(std::time::Duration::from_millis(1100)); // ids are second-resolution
    sb.write("quiet.cig", "exhale \"nothing burns\"\n");
    sb.run_ok(&["run", "quiet.cig"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    assert_ne!(id, donor);
    let journal = sb.home().join("runs").join(&id).join("journal.jsonl");
    fs::write(&journal, format!("{entry}\n")).unwrap();
    let out = sb.cig(&["runs", &id, "--verify"]);
    assert!(!out.status.success());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("E760"), "{text}");
    // A journal that cannot be read at all fails verification too (a bad
    // last line alone reads as a write cut short by a crash; one with good
    // lines after it cannot be).
    fs::write(
        &journal,
        format!("{{\"seq\":\"x\",\"op\":{{}}}}\n{entry}\n"),
    )
    .unwrap();
    let out = sb.cig(&["runs", &id, "--verify"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot be read"));
}

/// #128: a refusal over many changed paths names the first 20, with the
/// later run that changed them (each later journal is read once, not once
/// per path), and says how many more; `--dry-run` still lists them all.
#[test]
fn a_long_e704_refusal_names_twenty_and_counts_the_rest() {
    let sb = Sandbox::new();
    let mut a = String::from("burn {\n");
    let mut b = String::from("burn {\n");
    for k in 0..25 {
        sb.write(&format!("f{k:02}.txt"), "one");
        a.push_str(&format!("  fs.write_text(\"f{k:02}.txt\", \"two\")\n"));
        b.push_str(&format!("  fs.write_text(\"f{k:02}.txt\", \"three\")\n"));
    }
    a.push_str("}\n");
    b.push_str("}\n");
    sb.write("a.cig", &a);
    sb.write("b.cig", &b);
    sb.run_ok(&["run", "a.cig"]);
    let first = sb.runs()[0]["id"].as_str().unwrap().to_string();
    std::thread::sleep(std::time::Duration::from_millis(1100)); // ids are second-resolution
    sb.run_ok(&["run", "b.cig"]);
    let later = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let err = sb.run_err(&["unburn", &first], 1);
    assert!(
        err.contains("E704") && err.contains("25 paths changed"),
        "{err}"
    );
    assert_eq!(
        err.matches(&format!("changed by run {later}")).count(),
        20,
        "{err}"
    );
    assert!(err.contains("and 5 more"), "{err}");
    let plan = sb.run_ok(&["unburn", &first, "--dry-run"]);
    assert_eq!(
        plan.matches(&format!("by run {later}")).count(),
        25,
        "{plan}"
    );
}

#[test]
fn unburn_names_the_later_run_that_changed_the_file() {
    let sb = Sandbox::new();
    sb.write("f.txt", "one");
    sb.write("a.cig", "burn {\n  fs.write_text(\"f.txt\", \"two\")\n}\n");
    sb.write(
        "b.cig",
        "burn {\n  fs.write_text(\"f.txt\", \"three\")\n}\n",
    );
    sb.run_ok(&["run", "a.cig"]);
    let a = sb.runs()[0]["id"].as_str().unwrap().to_string();
    std::thread::sleep(std::time::Duration::from_millis(1100)); // ids are second-resolution
    sb.run_ok(&["run", "b.cig"]);
    let b = sb.runs()[0]["id"].as_str().unwrap().to_string();
    assert_ne!(a, b);
    let err = sb.run_err(&["unburn", &a], 1);
    assert!(
        err.contains("E704") && err.contains(&format!("changed by run {b}")),
        "{err}"
    );
    let plan = sb.run_ok(&["unburn", &a, "--dry-run"]);
    assert!(plan.contains(&format!("by run {b}")), "{plan}");
    // Newest first across runs: unburn b, then a.
    sb.run_ok(&["unburn", &b]);
    assert_eq!(sb.read("f.txt"), "two");
    sb.run_ok(&["unburn", &a]);
    assert_eq!(sb.read("f.txt"), "one");
}

#[test]
fn runs_without_after_state_are_still_unburnable_and_say_so() {
    let sb = Sandbox::new();
    sb.write("f.txt", "one");
    sb.write("s.cig", "burn {\n  fs.write_text(\"f.txt\", \"two\")\n}\n");
    sb.run_ok(&["run", "s.cig"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    fs::remove_file(sb.home().join("runs").join(&id).join("after.json")).unwrap();
    sb.write("f.txt", "three");
    let plan = sb.run_ok(&["unburn", &id, "--dry-run"]);
    assert!(
        plan.contains("changed since: unknown") && plan.contains("predates"),
        "{plan}"
    );
    sb.run_ok(&["unburn", &id]);
    assert_eq!(sb.read("f.txt"), "one");
}

// ----- the Hammer update: the ghost filesystem --------------------------------

#[test]
fn chain_dry_run_reads_ghost_write() {
    let sb = Sandbox::new();
    sb.write(
        "tasks.cig",
        concat!(
            "stick fetch = pack() { burn { fs.write_text(\"build/src.txt\", \"hello\") } }\n",
            "stick build = pack() { burn { fs.write_text(\"build/out.txt\", fs.read_text(\"build/src.txt\").upper()) } }\n",
            "stick report = pack() { exhale fs.read_text(\"build/out.txt\"), fs.list(\"build\").len() }\n",
            "chain release { fetch, build, report }\n"
        ),
    );
    let out = sb.cig(&["light", "--dry-run", "tasks.cig", "release"]);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{stderr}");
    assert!(
        stdout.contains("HELLO 2"),
        "step 3 read what step 2 pretended to write: {stdout}"
    );
    assert!(stderr.contains("dry-run plan: 2 ops"), "{stderr}");
    assert!(!sb.exists("build"), "the disk was touched by a dry-run");
    let out = sb.cig(&["light", "tasks.cig", "release"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(sb.read("build/out.txt"), "HELLO");
}

#[test]
#[cfg(unix)]
fn dry_run_reads_see_the_ghost_filesystem() {
    let sb = Sandbox::new();
    sb.write("real.txt", "disk");
    sb.write("log.txt", "one\n");
    fs::create_dir_all(sb.path().join("dir")).unwrap();
    sb.write("dir/a.txt", "a");
    sb.write(
        "s.cig",
        concat!(
            "burn {\n",
            "  fs.write_text(\"new/deep/file.txt\", \"made\")\n",
            "  exhale fs.read_text(\"new/deep/file.txt\"), fs.exists(\"new/deep\"), fs.is_dir(\"new\"), fs.size(\"new/deep/file.txt\")\n",
            "  fs.append_text(\"log.txt\", \"two\\n\")\n",
            "  exhale fs.read_lines(\"log.txt\").len()\n",
            "  fs.rm(\"real.txt\")\n",
            "  exhale fs.exists(\"real.txt\"), fs.is_file(\"real.txt\")\n",
            "  fs.mv(\"dir\", \"moved\")\n",
            "  exhale fs.exists(\"dir\"), fs.read_text(\"moved/a.txt\"), fs.list(\"moved\")\n",
            "  fs.cp(\"moved/a.txt\", \"copy.txt\")\n",
            "  json.save(\"data.json\", {n: 1})\n",
            "  exhale json.load(\"data.json\").n, hash.sha256_file(\"copy.txt\") == hash.sha256(\"a\")\n",
            "  exhale fs.glob(\"*.txt\")\n",
            "  exhale fs.mkdir(\"new\")\n",
            "}\n"
        ),
    );
    let out = sb.cig(&["run", "--dry-run", "s.cig"]);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines[0], "made true true 4");
    assert_eq!(lines[1], "2");
    assert_eq!(lines[2], "false false");
    assert_eq!(lines[3], "false a [\"moved/a.txt\"]");
    assert_eq!(lines[4], "1 true");
    assert_eq!(
        lines[5], "[\"copy.txt\", \"log.txt\"]",
        "removed real.txt hidden, ghost copy.txt shown"
    );
    assert_eq!(lines[6], "false", "the ghost knows `new` exists");
    assert_eq!(sb.read("real.txt"), "disk");
    assert_eq!(sb.read("log.txt"), "one\n");
    assert!(
        sb.exists("dir/a.txt")
            && !sb.exists("moved")
            && !sb.exists("new")
            && !sb.exists("copy.txt")
    );
}

#[test]
fn dry_run_reports_reads_of_ghost_removed_paths_as_e520_and_missing_sources_as_e508() {
    let sb = Sandbox::new();
    sb.write("a.txt", "a");
    sb.write(
        "s.cig",
        "burn {\n  fs.rm(\"a.txt\")\n  exhale fs.read_text(\"a.txt\")\n}\n",
    );
    let err = sb.run_err(&["run", "--dry-run", "s.cig"], 1);
    assert!(err.contains("error[E520 runtime]: fs.read_text: a.txt: removed earlier in this dry-run by op 1 (delete a.txt)"), "{err}");
    assert!(
        err.contains(
            "= doctor: I think an earlier step's simulated delete or move took the path away"
        ),
        "{err}"
    );
    assert_eq!(sb.read("a.txt"), "a");
    sb.write("t.cig", "burn {\n  fs.cp(\"nope.txt\", \"x.txt\")\n}\n");
    let err = sb.run_err(&["run", "--dry-run", "t.cig"], 1);
    assert!(
        err.contains("error[E508 runtime]: fs.cp: nope.txt: no such file or directory"),
        "{err}"
    );
    sb.write(
        "u.cig",
        "burn {\n  fs.rm(\"a.txt\")\n  fs.rm(\"a.txt\")\n}\n",
    );
    let err = sb.run_err(&["run", "--dry-run", "u.cig"], 1);
    assert!(
        err.contains("E520") && err.contains("fs.rm: a.txt"),
        "{err}"
    );
}

#[test]
fn unlit_burns_stay_out_of_the_ghost_and_off_the_disk() {
    let sb = Sandbox::new();
    sb.write("s.cig", "burn unlit {\n  fs.write_text(\"u.txt\", \"x\")\n}\nexhale fs.exists(\"u.txt\")\nburn {\n  fs.write_text(\"w.txt\", \"y\")\n}\nexhale fs.exists(\"w.txt\")\n");
    assert_eq!(sb.run_ok(&["run", "s.cig"]).trim(), "false\ntrue");
    assert!(!sb.exists("u.txt") && sb.exists("w.txt"));
    fs::remove_file(sb.path().join("w.txt")).unwrap();
    assert_eq!(
        sb.run_ok(&["run", "--dry-run", "s.cig"]).trim(),
        "false\ntrue",
        "in a dry-run the unlit write stays out, the burn's write is a ghost"
    );
    assert!(!sb.exists("u.txt") && !sb.exists("w.txt"));
}

#[test]
fn a_file_added_to_a_run_created_directory_is_not_lost_by_unburn() {
    let sb = Sandbox::new();
    sb.write(
        "s.cig",
        "burn {\n  fs.mkdir(\"made\")\n  fs.write_text(\"made/a.txt\", \"a\")\n}\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    // Untouched since: the directory's fingerprint matches.
    let plan = sb.run_ok(&["unburn", &id, "--dry-run"]);
    assert!(
        plan.contains("unchanged since") && !plan.contains("changed since:"),
        "{plan}"
    );
    // Someone keeps their own file in the directory the run created.
    sb.write("made/keep.txt", "mine");
    let plan = sb.run_ok(&["unburn", &id, "--dry-run"]);
    assert!(
        plan.contains("changed since: ") && plan.contains("made") && plan.contains("E704"),
        "{plan}"
    );
    let err = sb.run_err(&["unburn", &id], 1);
    assert!(
        err.contains("error[E704 burn]:") && err.contains("now a directory with 2 entries"),
        "{err}"
    );
    assert_eq!(
        sb.read("made/keep.txt"),
        "mine",
        "a refused unburn touches nothing"
    );
    // --force is the knowing choice.
    sb.run_ok(&["unburn", &id, "--force"]);
    assert!(!sb.exists("made"));
}

#[test]
fn a_journal_cut_short_by_a_crash_still_unburns_what_came_before_it() {
    let sb = Sandbox::new();
    sb.write(
        "s.cig",
        "burn {\n  fs.write_text(\"one.txt\", \"1\")\n  fs.write_text(\"two.txt\", \"2\")\n}\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let journal = sb.home().join("runs").join(&id).join("journal.jsonl");
    // A crash while appending leaves half a line.
    let mut text = fs::read_to_string(&journal).unwrap();
    text.push_str("{\"seq\": 3, \"op\": {\"kind\": \"wri");
    fs::write(&journal, text).unwrap();
    let out = sb.cig(&["unburn", &id]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(
        err.contains("the journal's last line was cut short")
            && err.contains("2 entries before it loaded"),
        "{err}"
    );
    assert!(!sb.exists("one.txt") && !sb.exists("two.txt"));
}

#[test]
fn fifty_thousand_nested_brackets_are_a_diagnostic_not_a_stack_overflow() {
    let sb = Sandbox::new();
    let deep = format!("stick x = {}{}\n", "[".repeat(50_000), "]".repeat(50_000));
    sb.write("deep.cig", &deep);
    let err = sb.run_err(&["run", "deep.cig"], 2);
    assert!(
        err.contains("error[E208 syntax]: nesting deeper than 5000 levels"),
        "{err}"
    );
    // The 100 kB line is windowed around the caret, not printed whole.
    assert!(
        err.len() < 2_000,
        "the source line was printed whole: {} bytes",
        err.len()
    );
    assert!(err.contains("…[[[["), "{err}");
    let deep = format!("stick y = {}1{}\n", "(".repeat(50_000), ")".repeat(50_000));
    sb.write("parens.cig", &deep);
    let err = sb.run_err(&["run", "parens.cig"], 2);
    assert!(err.contains("E208"), "{err}");
    let deep = format!("{}{}", "if true {\n".repeat(20_000), "}\n".repeat(20_000));
    sb.write("blocks.cig", &deep);
    let err = sb.run_err(&["run", "blocks.cig"], 2);
    assert!(err.contains("E208"), "{err}");
}

#[test]
fn a_utf8_byte_order_mark_is_skipped() {
    let sb = Sandbox::new();
    sb.write("bom.cig", "\u{feff}exhale \"bom ok\"\n");
    let out = sb.run_ok(&["run", "bom.cig"]);
    assert_eq!(out.trim(), "bom ok");
}

#[test]
fn a_list_that_contains_itself_prints_without_overflowing() {
    let sb = Sandbox::new();
    sb.write(
        "cyc.cig",
        "roll l = [1]\nl.push(l)\nexhale l\nroll m = {}\nm.me = m\nexhale m\n",
    );
    let out = sb.run_ok(&["run", "cyc.cig"]);
    assert!(out.contains("…"), "{out}");
    assert!(out.len() < 20_000, "{} bytes", out.len());
}

// ----- hardening, second round --------------------------------------------

#[test]
fn a_copy_or_move_onto_itself_or_into_itself_is_refused() {
    let sb = Sandbox::new();
    sb.write("a.txt", "hello");
    sb.write("s.cig", "burn {\n  fs.cp(\"a.txt\", \"a.txt\")\n}\n");
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(
        err.contains("error[E508 runtime]: fs.cp: a.txt and a.txt are the same file"),
        "{err}"
    );
    assert_eq!(sb.read("a.txt"), "hello", "the copy truncated the file");
    sb.write("s.cig", "burn {\n  fs.mv(\"a.txt\", \"./a.txt\")\n}\n");
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(err.contains("are the same file"), "{err}");
    assert_eq!(sb.read("a.txt"), "hello");
    sb.write("d/f.txt", "keep");
    sb.write("s.cig", "burn {\n  fs.cp(\"d\", \"d/sub\")\n}\n");
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(
        err.contains("fs.cp: d/sub is inside d, the directory being copied"),
        "{err}"
    );
    assert!(!sb.exists("d/sub"));
}

#[test]
fn a_move_the_os_refused_is_marked_undone_and_never_moved_back() {
    let sb = Sandbox::new();
    // Caught inside the run: the journal must not say the move happened.
    sb.write("d/f.txt", "keep");
    sb.write(
        "s.cig",
        "burn {\n  fs.write_text(\"d/g.txt\", \"g\")\n  try {\n    fs.mv(\"d\", \"d/sub\")\n  } ashtray e {\n    exhale \"caught\"\n  }\n}\n",
    );
    let out = sb.run_ok(&["run", "s.cig"]);
    assert_eq!(out.trim(), "caught");
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    assert_eq!(sb.runs()[0]["burns"], 1, "the refused move is not a burn");
    sb.run_ok(&["unburn", &id]);
    assert_eq!(sb.read("d/f.txt"), "keep");
    assert!(!sb.exists("d/g.txt"));
    // Uncaught, onto a non-empty directory: rollback leaves the source alone.
    sb.write("important.txt", "important");
    sb.write("blocker/other.txt", "other");
    sb.write(
        "s.cig",
        "burn {\n  fs.mv(\"important.txt\", \"blocker\")\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(
        err.contains("error[E508 runtime]: fs.mv: important.txt"),
        "{err}"
    );
    assert_eq!(sb.read("important.txt"), "important");
    assert_eq!(sb.read("blocker/other.txt"), "other");
}

#[test]
#[cfg(unix)]
fn a_cross_device_move_rolls_back_by_copying() {
    use std::os::unix::fs::MetadataExt;
    let sb = Sandbox::new();
    let shm = Path::new("/dev/shm");
    let Ok(other) = tempfile::tempdir_in(shm) else {
        return; // no second filesystem to move across on this machine
    };
    if fs::metadata(other.path()).map(|m| m.dev()).ok()
        == fs::metadata(sb.path()).map(|m| m.dev()).ok()
    {
        return;
    }
    let src = other.path().join("src.txt");
    fs::write(&src, "across").unwrap();
    sb.write(
        "s.cig",
        &format!(
            "burn {{\n  fs.mv(\"{}\", \"landed.txt\")\n  cough \"x\"\n}}\n",
            src.display()
        ),
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(err.contains("moved back"), "{err}");
    assert_eq!(fs::read_to_string(&src).unwrap(), "across");
    assert!(!sb.exists("landed.txt"));
}

#[test]
#[cfg(unix)]
fn a_hardlinked_file_is_restored_in_place() {
    let sb = Sandbox::new();
    sb.write("orig", "before");
    fs::hard_link(sb.path().join("orig"), sb.path().join("other")).unwrap();
    sb.write(
        "s.cig",
        "burn {\n  fs.write_text(\"orig\", \"after\")\n  cough \"x\"\n}\n",
    );
    sb.run_err(&["run", "s.cig"], 1);
    assert_eq!(sb.read("orig"), "before");
    assert_eq!(
        sb.read("other"),
        "before",
        "the other name kept the run's content"
    );
}

#[test]
#[cfg(unix)]
fn setuid_and_sticky_bits_survive_rollback() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    sb.write("suid", "s");
    fs::set_permissions(sb.path().join("suid"), fs::Permissions::from_mode(0o4755)).unwrap();
    fs::create_dir(sb.path().join("sticky")).unwrap();
    fs::set_permissions(sb.path().join("sticky"), fs::Permissions::from_mode(0o1777)).unwrap();
    sb.write(
        "s.cig",
        "burn {\n  fs.rm(\"suid\")\n  fs.rm(\"sticky\")\n  cough \"x\"\n}\n",
    );
    sb.run_err(&["run", "s.cig"], 1);
    let mode = |name: &str| {
        fs::metadata(sb.path().join(name))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777
    };
    assert_eq!(mode("suid"), 0o4755);
    assert_eq!(mode("sticky"), 0o1777);
}

#[test]
#[cfg(unix)]
fn a_named_pipe_is_refused_not_hung() {
    use wait_timeout::ChildExt;
    let sb = Sandbox::new();
    assert!(Command::new("mkfifo")
        .arg(sb.path().join("fifo"))
        .status()
        .expect("mkfifo")
        .success());
    sb.write("s.cig", "burn {\n  fs.rm(\"fifo\")\n}\n");
    let mut child = Command::new(env!("CARGO_BIN_EXE_cig"))
        .args(["run", "s.cig"])
        .current_dir(sb.path())
        .env("CIGSCRIPT_HOME", sb.home())
        .env("NO_COLOR", "1")
        .env("CIG_SEAL_KEY", "file")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn cig");
    let status = child
        .wait_timeout(std::time::Duration::from_secs(20))
        .expect("wait");
    let Some(status) = status else {
        let _ = child.kill();
        panic!("cig hung on the named pipe");
    };
    assert_eq!(status.code(), Some(1));
    let mut err = String::new();
    std::io::Read::read_to_string(child.stderr.as_mut().unwrap(), &mut err).unwrap();
    assert!(
        err.contains("error[E702 burn]:") && err.contains("cannot snapshot it"),
        "{err}"
    );
    assert!(sb.path().join("fifo").exists());
}

/// Root ignores file modes, so a test that locks a directory with
/// `chmod 000` proves nothing as root (a container, a FreeBSD VM): it says
/// so and steps aside.
#[cfg(unix)]
fn running_as_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false)
}

#[test]
#[cfg(unix)]
fn rollback_failures_are_recorded_on_the_run() {
    use std::os::unix::fs::PermissionsExt;
    if running_as_root() {
        eprintln!("skipped: root ignores chmod 000, so a locked directory cannot be made here");
        return;
    }
    let sb = Sandbox::new();
    // A directory the run did not make, locked after the run wrote into it:
    // the restore cannot write there, and the rollback opens no mode it did
    // not set (a tree the hop made is another matter, since 1.6.2).
    sb.write("locked/secret.txt", "hidden\n");
    sb.write(
        "s.cig",
        "pack { \".\" }\nburn {\n  fs.write_text(\"free.txt\", \"1\")\n  fs.write_text(\"locked/secret.txt\", \"changed\")\n  proc.run(\"chmod\", [\"500\", \"locked\"])\n  cough \"boom\"\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(
        err.contains("failed write locked/secret.txt") && err.contains("restored write free.txt"),
        "{err}"
    );
    let rec = &sb.runs()[0];
    assert_eq!(rec["status"], "rollback_incomplete");
    assert_eq!(rec["rolled_back"], true);
    assert_eq!(rec["rollback_failed"].as_array().map(Vec::len), Some(1));
    let id = rec["id"].as_str().unwrap().to_string();
    let shown = sb.run_ok(&["runs", &id]);
    assert!(
        shown.contains("status    rollback_incomplete") && shown.contains("1 restore failed"),
        "{shown}"
    );
    // Still locked: the retry fails the same way, and is not refused.
    let err = sb.run_err(&["unburn", &id], 1);
    assert!(
        err.contains("1 restore failed last time") && !err.contains("E705"),
        "{err}"
    );
    fs::set_permissions(sb.path().join("locked"), fs::Permissions::from_mode(0o755)).unwrap();
    sb.run_ok(&["unburn", &id]);
    assert_eq!(sb.read("locked/secret.txt"), "hidden\n");
    assert!(!sb.exists("free.txt"));
    assert_eq!(sb.runs()[0]["status"], "unburned");
}

#[test]
#[cfg(unix)]
fn a_read_only_run_directory_refuses_unburn_until_forced() {
    use std::os::unix::fs::PermissionsExt;
    if running_as_root() {
        eprintln!(
            "skipped: root writes into a read-only directory, so the refusal cannot be shown here"
        );
        return;
    }
    let sb = Sandbox::new();
    sb.write("s.cig", "burn {\n  fs.write_text(\"ro.txt\", \"1\")\n}\n");
    sb.run_ok(&["run", "s.cig"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let dir = sb.home().join("runs").join(&id);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
    let err = sb.run_err(&["unburn", &id], 3);
    assert!(
        err.contains("error[E802 usage]: cannot record the rollback"),
        "{err}"
    );
    assert!(sb.exists("ro.txt"), "a refused unburn touches nothing");
    let out = sb.cig(&["unburn", &id, "--force"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(
        err.contains("--force given") && err.contains("could not record"),
        "{err}"
    );
    assert!(!sb.exists("ro.txt"));
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn a_damaged_or_renamed_run_record_still_lists_and_unburns() {
    let sb = Sandbox::new();
    sb.write("s.cig", "burn {\n  fs.write_text(\"dm.txt\", \"1\")\n}\n");
    sb.run_ok(&["run", "s.cig"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    fs::write(sb.home().join("runs").join(&id).join("run.json"), "garbage").unwrap();
    let rec = sb
        .runs()
        .into_iter()
        .find(|r| r["id"] == id.as_str())
        .expect("listed");
    assert_eq!(rec["status"], "damaged");
    assert_eq!(rec["burns"], 1, "counted from the journal");
    let shown = sb.run_ok(&["runs", &id]);
    assert!(shown.contains("damaged"), "{shown}");
    sb.run_ok(&["unburn", &id]);
    assert!(!sb.exists("dm.txt"));
    // A renamed directory: the record remembers where it was read from.
    sb.run_ok(&["run", "s.cig"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let renamed = sb.home().join("runs").join("RENAMED-1");
    fs::rename(sb.home().join("runs").join(&id), &renamed).unwrap();
    let shown = sb.run_ok(&["runs", &id]);
    assert!(shown.contains("RENAMED-1"), "{shown}");
    sb.run_ok(&["unburn", &id]);
    assert!(!sb.exists("dm.txt"));
}

#[test]
fn a_dead_run_whose_pid_was_reused_is_interrupted_not_running() {
    let sb = Sandbox::new();
    sb.write("s.cig", "burn {\n  fs.write_text(\"p.txt\", \"1\")\n}\n");
    sb.run_ok(&["run", "s.cig"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let path = sb.home().join("runs").join(&id).join("run.json");
    let mut rec: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    rec["status"] = serde_json::Value::String("running".to_string());
    rec["pid"] = serde_json::Value::from(1);
    fs::write(&path, rec.to_string()).unwrap();
    assert_eq!(sb.runs()[0]["status"], "interrupted");
    // A live process that is not a cig (this test runner) is a reused pid
    // too, on every platform; and a cig whose start time differs from the
    // one the record kept is another cig, not the run that died.
    rec["pid"] = serde_json::Value::from(std::process::id());
    fs::write(&path, rec.to_string()).unwrap();
    assert_eq!(sb.runs()[0]["status"], "interrupted");
    rec["pid_started"] = serde_json::Value::String("ticks:1".to_string());
    fs::write(&path, rec.to_string()).unwrap();
    assert_eq!(sb.runs()[0]["status"], "interrupted");
    // A finished run records when its process started, for the next check.
    let fresh: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(sb.home().join("runs").join(&id).join("run.json")).unwrap(),
    )
    .unwrap();
    let _ = fresh;
}

#[test]
fn a_run_record_names_when_its_process_started() {
    let sb = Sandbox::new();
    sb.write("s.cig", "burn {\n  fs.write_text(\"p.txt\", \"1\")\n}\n");
    sb.run_ok(&["run", "s.cig"]);
    let rec = &sb.runs()[0];
    assert!(rec["pid"].as_u64().unwrap_or(0) > 0, "{rec}");
    let started = rec["pid_started"].as_str().unwrap_or("");
    assert!(
        started.starts_with("ticks:")
            || started.starts_with("lstart:")
            || started.starts_with("filetime:"),
        "pid_started = {started:?} in {rec}"
    );
}

#[test]
fn a_burn_block_that_fails_half_way_leaves_its_hops_honestly_irreversible() {
    let sb = Sandbox::new();
    sb.write(
        "s.cig",
        "burn (s) {\n  proc.run(\"sh\", [\"-c\", \"echo before > before.txt\"])\n  cough \"abort\"\n} unburn {\n  exhale \"should never run\"\n}\n",
    );
    let out = sb.cig(&["run", "s.cig"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1));
    assert!(err.contains("cannot undo run sh -c"), "{err}");
    assert!(!err.contains("compensated"), "{err}");
    assert!(!String::from_utf8_lossy(&out.stdout).contains("should never run"));
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let shown = sb.run_ok(&["runs", &id]);
    assert!(shown.contains("irreversible  run sh -c"), "{shown}");
    // A block that completes stamps its hop, and a later cough runs the block.
    sb.write(
        "s.cig",
        "burn (s) {\n  proc.run(\"sh\", [\"-c\", \"echo done > done.txt\"])\n  s.n = 1\n} unburn {\n  exhale \"undoing\", s.n\n}\nburn {\n  cough \"later\"\n}\n",
    );
    let out = sb.cig(&["run", "s.cig"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("compensated run sh -c"), "{err}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("undoing 1"),
        "{err}"
    );
}

#[test]
fn a_compensation_that_reads_args_runs_on_a_deferred_unburn() {
    let sb = Sandbox::new();
    sb.write(
        "s.cig",
        "burn (s) {\n  fs.write_text(\"marker.txt\", \"x\")\n} unburn {\n  exhale \"args:\", args.len(), args[0]\n}\n",
    );
    sb.run_ok(&["run", "s.cig", "--", "a", "b", "c"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let out = sb.cig(&["unburn", &id]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("args: 3 a"),
        "{err}"
    );
    assert!(!sb.exists("marker.txt"));
}

#[test]
#[cfg(unix)]
fn the_ghost_resolves_symlinked_directories_and_lists_created_ancestors() {
    use std::os::unix::fs::symlink;
    let sb = Sandbox::new();
    fs::create_dir(sb.path().join("real")).unwrap();
    symlink("real", sb.path().join("link")).unwrap();
    sb.write(
        "s.cig",
        "burn {\n  fs.write_text(\"link/x.txt\", \"via-link\")\n  exhale fs.read_text(\"real/x.txt\")\n  fs.write_text(\"a/b/c/file.txt\", \"deep\")\n  exhale fs.list(\"a\"), fs.list(\"a/b\")\n}\n",
    );
    let out = sb.run_ok(&["run", "--dry-run", "s.cig"]);
    assert_eq!(out.trim(), "via-link\n[\"a/b\"] [\"a/b/c\"]", "{out}");
    assert!(!sb.exists("real/x.txt") && !sb.exists("a"));
}

#[test]
fn an_empty_pack_root_is_refused() {
    let sb = Sandbox::new();
    sb.write(
        "s.cig",
        "pack { \"\" }\nburn {\n  fs.write_text(\"z.txt\", \"z\")\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 2);
    assert!(
        err.contains("error[E755 check]: a pack root is empty"),
        "{err}"
    );
    sb.write(
        "s.cig",
        "pack { \"\" + \"\" }\nburn {\n  fs.write_text(\"z.txt\", \"z\")\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(
        err.contains("error[E755 burn]: a pack root is empty"),
        "{err}"
    );
    assert!(!sb.exists("z.txt"));
}

#[test]
fn i64_min_divided_by_minus_one_is_a_diagnostic() {
    let sb = Sandbox::new();
    let err = sb.run_err(&["eval", "--", "-9223372036854775808 / -1"], 1);
    assert!(
        err.contains("error[E503 runtime]: integer overflow in `/`"),
        "{err}"
    );
    assert!(!err.contains("E901"), "{err}");
    let out = sb.run_ok(&["eval", "--", "-9223372036854775808 % -1"]);
    assert_eq!(out.trim(), "0");
}

#[test]
fn a_value_that_contains_itself_is_refused_by_json_and_compared_without_looping() {
    let sb = Sandbox::new();
    sb.write(
        "s.cig",
        "roll l = [1]\nl.push(l)\nroll m = [1]\nm.push(m)\nexhale l == m, l == [1, l], l == [2, l]\nroll a = {}\na.me = a\nroll b = {}\nb.me = b\nexhale a == b\nexhale json.stringify(l)\n",
    );
    let out = sb.cig(&["run", "s.cig"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "true true false\ntrue"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("error[E500 runtime]: json: the value contains itself"),
        "{err}"
    );
}

#[test]
fn floats_stay_floats_past_sixteen_digits_and_int_refuses_what_does_not_fit() {
    let sb = Sandbox::new();
    sb.write(
        "s.cig",
        "exhale 1e16, 1.5e20, 9999999999999998.0, 0.1, 2.0, type_of(1e16)\nexhale int(3.9), int(\"42\"), int(\"1e3\")\n",
    );
    let out = sb.run_ok(&["run", "s.cig"]);
    assert_eq!(
        out.trim(),
        "1e16 1.5e20 9999999999999998.0 0.1 2.0 float\n3 42 1000"
    );
    let err = sb.run_err(&["eval", "--", "int(1e300)"], 1);
    assert!(
        err.contains("error[E503 runtime]: int: 1e300 does not fit in an int"),
        "{err}"
    );
    let err = sb.run_err(&["eval", "--", "int(\"9223372036854775808\")"], 1);
    assert!(err.contains("does not fit in an int"), "{err}");
    let err = sb.run_err(&["eval", "--", "int(-1e300)"], 1);
    assert!(err.contains("E503"), "{err}");
}

#[test]
fn a_redeclaration_at_run_time_is_a_check_error_and_args_is_shadowable() {
    let sb = Sandbox::new();
    sb.write("s.cig", "roll y = 1\nroll y = 2\n");
    let err = sb.run_err(&["run", "--no-check", "s.cig"], 2);
    assert!(err.contains("error[E306 check]:"), "{err}");
    sb.write("s.cig", "roll args = [\"mine\"]\nexhale args\n");
    let out = sb.run_ok(&["check", "s.cig"]);
    assert!(out.contains("ok") || out.is_empty(), "{out}");
    let out = sb.run_ok(&["run", "s.cig"]);
    assert_eq!(out.trim(), "[\"mine\"]");
    sb.write("s.cig", "exhale args\n");
    let out = sb.run_ok(&["run", "s.cig", "--", "x"]);
    assert_eq!(out.trim(), "[\"x\"]");
}

#[test]
fn a_string_that_runs_past_its_line_is_e101() {
    let sb = Sandbox::new();
    sb.write("s.cig", "roll s = \"a\nb\"\nexhale s\n");
    let err = sb.run_err(&["run", "s.cig"], 2);
    assert!(
        err.contains(
            "error[E101 lex]: unterminated string literal: the line ends before the closing quote"
        ) && err.contains("--> s.cig:1:10"),
        "{err}"
    );
    sb.write("s.cig", "roll s = \"a\\nb\"\nexhale s.len()\n");
    assert_eq!(sb.run_ok(&["run", "s.cig"]).trim(), "3");
}

#[test]
fn chained_operators_are_capped_and_a_bad_step_budget_is_a_usage_error() {
    let sb = Sandbox::new();
    let chain = format!("exhale 1{}\n", " + 1".repeat(12_000));
    sb.write("chain.cig", &chain);
    let err = sb.run_err(&["run", "chain.cig"], 2);
    assert!(
        err.contains("error[E208 syntax]: more than 10000 operators chained in one expression"),
        "{err}"
    );
    let chain = format!("exhale 1{}\n", " + 1".repeat(8_000));
    sb.write("chain.cig", &chain);
    assert_eq!(sb.run_ok(&["run", "chain.cig"]).trim(), "8001");
    sb.write("s.cig", "exhale 1\n");
    let out = sb.cig_env(&["run", "s.cig"], &[("CIG_MAX_STEPS", "abc")]);
    assert_eq!(out.status.code(), Some(3));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("error[E800 usage]: CIG_MAX_STEPS=`abc` is not a step count"),
        "{err}"
    );
    let out = sb.cig_env(&["run", "s.cig"], &[("CIG_MAX_STEPS", "5")]);
    assert!(out.status.success());
}

#[test]
fn a_device_as_the_script_is_refused_and_a_bad_invocation_exits_3() {
    let sb = Sandbox::new();
    if Path::new("/dev/zero").exists() {
        let err = sb.run_err(&["run", "/dev/zero"], 3);
        assert!(
            err.contains("error[E801 usage]: cannot read /dev/zero: larger than 64 MiB"),
            "{err}"
        );
    }
    let err = sb.run_err(&["run", "does-not-exist.cig"], 3);
    assert!(err.contains("error[E801 usage]:"), "{err}");
    assert_eq!(sb.cig(&["frobnicate"]).status.code(), Some(3));
    assert_eq!(sb.cig(&["run"]).status.code(), Some(3));
    assert_eq!(
        sb.cig(&["run", "--max-steps", "abc", "x.cig"])
            .status
            .code(),
        Some(3)
    );
    assert_eq!(sb.cig(&["--version"]).status.code(), Some(0));
    assert_eq!(sb.cig(&["--help"]).status.code(), Some(0));
}

#[test]
fn crash_send_honours_never() {
    let sb = Sandbox::new();
    sb.write("s.cig", "exhale 1\n");
    let out = sb.cig_env(&["run", "s.cig"], &[("CIG_INTERNAL_PANIC", "1")]);
    assert_eq!(out.status.code(), Some(70));
    let listed = sb.run_ok(&["--json", "crash", "list"]);
    let v: Vec<serde_json::Value> = serde_json::from_str(&listed).unwrap();
    let id = v[0]["id"].as_str().unwrap().to_string();
    sb.run_ok(&["config", "crash_reports", "never"]);
    let err = sb.run_err(&["crash", "send", &id], 3);
    assert!(
        err.contains("error[E800 usage]: crash_reports is \"never\""),
        "{err}"
    );
}

#[test]
fn env_changes_are_labelled_irreversible_in_the_plan() {
    let sb = Sandbox::new();
    sb.write(
        "s.cig",
        "burn {\n  env.set(\"CIG_PROBE\", \"1\")\n  env.unset(\"CIG_PROBE\")\n}\n",
    );
    let out = sb.cig(&["run", "--dry-run", "s.cig"]);
    let plan = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{plan}");
    assert!(
        plan.contains("irreversible  set env CIG_PROBE")
            && plan.contains("irreversible  unset env CIG_PROBE"),
        "{plan}"
    );
    assert!(!plan.contains("reversible    set env"), "{plan}");
    let out = sb.cig(&["run", "s.cig"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(err.contains("(2 irreversible)"), "{err}");
}

#[test]
fn a_compensating_block_left_by_snuff_still_records_its_compensation() {
    let sb = Sandbox::new();
    sb.write(
        "s.cig",
        "pull deploy() {\n  burn (s) {\n    fs.write_text(\"deployed.txt\", \"1\")\n    s.v = \"1\"\n    snuff \"early\"\n  } unburn {\n    exhale \"compensation ran for v\" + s.v\n  }\n  snuff \"late\"\n}\nexhale deploy()\nburn { cough \"later failure\" }\n",
    );
    let out = sb.cig(&["run", "s.cig"]);
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("early") && stdout.contains("compensation ran for v1"),
        "{stdout}"
    );
    // Left by break, and undone weeks later by cig unburn.
    sb.write(
        "s.cig",
        "pull deploy() {\n  burn (s) {\n    fs.write_text(\"deployed.txt\", \"1\")\n    s.v = \"2\"\n    snuff \"early\"\n  } unburn {\n    exhale \"compensation ran for v\" + s.v\n  }\n}\nfor i in 0..3 {\n  burn (s) {\n    s.i = i\n    fs.write_text(\"loop.txt\", \"x\")\n    break\n  } unburn {\n    exhale \"loop compensation\", s.i\n  }\n}\ndeploy()\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let out = sb.run_ok(&["unburn", &id]);
    assert!(
        out.contains("compensation ran for v2") && out.contains("loop compensation 0"),
        "{out}"
    );
    assert!(!sb.exists("deployed.txt") && !sb.exists("loop.txt"));
}

#[test]
fn append_text_creates_the_parent_directory_like_write_text_and_the_dry_run_agrees() {
    let sb = Sandbox::new();
    sb.write(
        "s.cig",
        "burn {\n  fs.append_text(\"logs/run.log\", \"one\\n\")\n  fs.append_text(\"logs/run.log\", \"two\\n\")\n  exhale fs.read_text(\"logs/run.log\").lines().len()\n}\n",
    );
    let plan = sb.run_ok(&["run", "--dry-run", "s.cig"]);
    assert_eq!(plan.trim(), "2", "{plan}");
    assert!(!sb.exists("logs"));
    let out = sb.run_ok(&["run", "s.cig"]);
    assert_eq!(out.trim(), "2");
    assert_eq!(sb.read("logs/run.log"), "one\ntwo\n");
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    sb.run_ok(&["unburn", &id]);
    assert!(
        !sb.exists("logs"),
        "the created parent is removed with the file"
    );
}

#[test]
fn an_ad_hoc_chain_can_name_its_steps() {
    let sb = Sandbox::new();
    sb.write(
        "s.cig",
        "stick steps = [\"fetch\", \"build\"].map(pack(n) => pack() => n.upper())\nstick r = light(steps, {names: [\"fetch\", \"build\"], quiet: true})\nexhale r.steps[0].name, r.steps[1].name, r.result\nstick bad = [pack() => 1, pack() { cough \"boom\" }]\ntry {\n  light(bad, {names: [\"one\", \"two\"], quiet: true})\n} ashtray e {\n  exhale e.step, e.index\n}\nlight(bad, {names: [\"one\", \"two\"]})\n",
    );
    let out = sb.cig(&["run", "s.cig"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "fetch build BUILD\ntwo 1"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("failed at step 2 (two): boom") && err.contains("step 2/2 two"),
        "{err}"
    );
    let err = sb.run_err(&["eval", "--", "light([pack() => 1], {names: \"x\"})"], 1);
    assert!(err.contains("`names` is a list of strings"), "{err}");
}

#[test]
fn a_semicolon_may_follow_a_block_statement() {
    let sb = Sandbox::new();
    sb.write(
        "s.cig",
        "if true { exhale 1 }; exhale 2\nburn { exhale 3 }; exhale 4\npull f() { snuff 5 }; exhale f()\nfor i in [6] { exhale i };\n;; exhale 7\n",
    );
    let out = sb.run_ok(&["run", "s.cig"]);
    assert_eq!(out.trim(), "1\n2\n3\n4\n5\n6\n7");
}

#[test]
#[cfg(unix)]
fn glob_star_lists_no_empty_entry_and_dot_slash_patterns_match() {
    let sb = Sandbox::new();
    sb.write("scripts/a.sh", "");
    sb.write("scripts/deep/b.sh", "");
    sb.write("top.txt", "");
    sb.write(
        "s.cig",
        "exhale fs.glob(\"*\")\nexhale fs.glob(\"./scripts/*\")\nexhale fs.glob(\"scripts/*\")\nexhale fs.glob(\"scripts/**/*.sh\")\nexhale fs.glob(\"./top.txt\")\n",
    );
    let out = sb.run_ok(&["run", "s.cig"]);
    let lines: Vec<&str> = out.lines().collect();
    assert!(!lines[0].contains("\"\""), "an empty entry: {}", lines[0]);
    assert!(
        lines[0].contains("\"scripts\"") && lines[0].contains("\"top.txt\""),
        "{}",
        lines[0]
    );
    assert!(
        !lines[0].contains("scripts/a.sh"),
        "`*` walked below the top: {}",
        lines[0]
    );
    assert_eq!(lines[1], "[\"./scripts/a.sh\", \"./scripts/deep\"]");
    assert_eq!(lines[2], "[\"scripts/a.sh\", \"scripts/deep\"]");
    assert_eq!(lines[3], "[\"scripts/a.sh\", \"scripts/deep/b.sh\"]");
    assert_eq!(lines[4], "[\"./top.txt\"]");
}

// ----- crash round: backtraces that name the function (#23), burns put back after a crash (#24)

#[test]
fn a_release_crash_report_names_the_function_that_panicked() {
    let sb = Sandbox::new();
    sb.write("x.cig", "exhale \"never\"\n");
    let out = sb.cig_env(&["run", "x.cig"], &[("CIG_INTERNAL_PANIC", "eval")]);
    assert_eq!(out.status.code(), Some(70));
    let list: Vec<serde_json::Value> =
        serde_json::from_str(&sb.run_ok(&["--json", "crash", "list"])).unwrap();
    let report = &list[0];
    assert!(
        report["panic_location"]
            .as_str()
            .unwrap()
            .starts_with("src/interp/"),
        "{report}"
    );
    let bt: Vec<&str> = report["backtrace"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap())
        .collect();
    assert!(
        bt.iter().any(|f| f.contains("cigscript::interp")),
        "no evaluator frame in {bt:?}"
    );
    assert!(
        bt.iter().all(|f| !f.contains("install_hook")),
        "the hook's own frame is in {bt:?}"
    );
}

fn mid_run_script(sb: &Sandbox) {
    sb.write("old.txt", "old\n");
    sb.write(
        "k.cig",
        "burn {\n  fs.write_text(\"a.txt\", \"1\")\n  fs.write_text(\"old.txt\", \"changed\")\n  fs.write_text(\"b.txt\", \"2\")\n}\n",
    );
}

#[test]
fn a_panic_mid_run_offers_to_put_its_burns_back() {
    let sb = Sandbox::new();
    mid_run_script(&sb);
    sb.run_ok(&["config", "crash_rollback", "always"]);
    let out = sb.cig_env(&["run", "k.cig"], &[("CIG_INTERNAL_PANIC", "after-burn")]);
    assert_eq!(out.status.code(), Some(70));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("crash_rollback is \"always\""), "{stderr}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("restored") || stdout.contains("removed"),
        "the rollback report was not printed:\n{stdout}\n{stderr}"
    );
    assert!(!sb.exists("a.txt"), "a.txt was not put back:\n{stderr}");
    assert!(!sb.exists("b.txt"), "b.txt was written after the crash");
    assert_eq!(sb.read("old.txt"), "old\n");
    let list: Vec<serde_json::Value> =
        serde_json::from_str(&sb.run_ok(&["--json", "crash", "list"])).unwrap();
    assert_eq!(list[0]["rolled_back"], serde_json::Value::Bool(true));
    assert!(list[0]["active_run_id"].is_string(), "{}", list[0]);
}

#[test]
fn without_a_terminal_the_crash_names_the_unburn_command() {
    let sb = Sandbox::new();
    mid_run_script(&sb);
    let out = sb.cig_env(&["run", "k.cig"], &[("CIG_INTERNAL_PANIC", "after-burn")]);
    assert_eq!(out.status.code(), Some(70));
    let stderr = String::from_utf8_lossy(&out.stderr);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    assert!(
        stderr.contains(&format!("cig unburn {id} puts them back")),
        "{stderr}"
    );
    assert!(sb.exists("a.txt"), "nothing was asked, so nothing moves");
    let list: Vec<serde_json::Value> =
        serde_json::from_str(&sb.run_ok(&["--json", "crash", "list"])).unwrap();
    assert_eq!(list[0]["rolled_back"], serde_json::Value::Bool(false));
    // The command it named works, and puts the files back.
    sb.run_ok(&["unburn", &id]);
    assert!(!sb.exists("a.txt"));
    assert_eq!(sb.read("old.txt"), "old\n");
}

#[test]
fn crash_rollback_never_leaves_the_run_interrupted() {
    let sb = Sandbox::new();
    mid_run_script(&sb);
    sb.run_ok(&["config", "crash_rollback", "always"]);
    let out = sb.cig_env(&["run", "k.cig"], &[("CIG_INTERNAL_PANIC", "after-burn")]);
    assert_eq!(out.status.code(), Some(70));
    let run = &sb.runs()[0];
    assert_eq!(run["status"], "unburned", "{run}");
    assert_eq!(run["rolled_back"], serde_json::Value::Bool(true));
    // A second unburn is refused like any other repeat: the record was written.
    let id = run["id"].as_str().unwrap().to_string();
    let err = sb.run_err(&["unburn", &id], 1);
    assert!(err.contains("error[E705 burn]"), "{err}");
}

#[test]
fn the_crash_rollback_reads_the_journal_not_the_panicked_state() {
    // The second op was journaled (its snapshot of old.txt taken) and never
    // executed; the restore comes from that snapshot on disk, so old.txt is
    // byte for byte what it was, and the created file goes.
    let sb = Sandbox::new();
    mid_run_script(&sb);
    sb.run_ok(&["config", "crash_rollback", "always"]);
    let out = sb.cig_env(&["run", "k.cig"], &[("CIG_INTERNAL_PANIC", "after-burn")]);
    assert_eq!(out.status.code(), Some(70));
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let shown = sb.run_ok(&["--json", "runs", &id]);
    let v: serde_json::Value = serde_json::from_str(&shown).unwrap();
    let journal = v["journal"].as_array().unwrap();
    assert_eq!(journal.len(), 2, "{journal:?}");
    assert_eq!(sb.read("old.txt"), "old\n");
    assert!(!sb.exists("a.txt"));
}

// ----- pins and the humidor (#22): a script runs on the cig it was written for

fn write_run(sb: &Sandbox, id: &str, run: serde_json::Value, journal: &str) -> PathBuf {
    let dir = sb.home().join("runs").join(id);
    fs::create_dir_all(dir.join("snapshots")).unwrap();
    fs::write(dir.join("run.json"), run.to_string()).unwrap();
    fs::write(dir.join("journal.jsonl"), journal).unwrap();
    dir
}

#[test]
fn a_run_from_1_1_1_reads_on_the_current_version() {
    let sb = Sandbox::new();
    // A record and a journal as 1.1.x wrote them: no format anywhere.
    let target = sb.path().join("made.txt");
    fs::write(&target, "by the old run").unwrap();
    let id = "20260101T000000-abc123";
    write_run(
        &sb,
        id,
        serde_json::json!({"id": id, "version": "1.1.1", "script": "old.cig", "script_sha256": "", "args": [],
            "cwd": sb.path(), "mode": "run", "started": "2026-01-01T00:00:00.000Z",
            "finished": "2026-01-01T00:00:01.000Z", "status": "ok", "burns": 1, "irreversible": 0}),
        &format!(
            "{}\n",
            serde_json::json!({"seq": 1, "at": "2026-01-01T00:00:00.500Z", "reversible": true,
                "op": {"kind": "write", "path": target}, "before": [{"path": target, "state": "absent"}]})
        ),
    );
    let runs = sb.runs();
    assert_eq!(runs[0]["format"], 0, "{}", runs[0]);
    assert_eq!(runs[0]["status"], "ok");
    let shown = sb.run_ok(&["unburn", id, "--dry-run"]);
    assert!(shown.contains("would restore 1 entry"), "{shown}");
    sb.run_ok(&["unburn", id]);
    assert!(!target.exists(), "the old journal was replayed");

    // A record this cig writes says its format, and a newer one is refused
    // by its number, naming the cig that wrote it.
    sb.write("s.cig", "burn { fs.write_text(\"n.txt\", \"1\") }\n");
    sb.run_ok(&["run", "s.cig"]);
    let runs = sb.runs();
    assert_eq!(runs[0]["format"], 1);
    let journal = sb.read(&format!(
        "home/runs/{}/journal.jsonl",
        runs[0]["id"].as_str().unwrap()
    ));
    assert!(journal.starts_with("{\"format\":3,\"run\":\""), "{journal}");
    let id2 = "20260102T000000-def456";
    write_run(
        &sb,
        id2,
        serde_json::json!({"id": id2, "format": 1, "version": "9.9.9", "script": "new.cig", "script_sha256": "",
            "args": [], "cwd": "", "mode": "run", "started": "2026-01-02T00:00:00.000Z", "status": "ok",
            "burns": 1, "irreversible": 0}),
        "{\"format\":99}\n{\"seq\":1,\"at\":\"x\",\"reversible\":true,\"op\":{\"kind\":\"mkdir\",\"path\":\"nowhere\"},\"before\":[]}\n",
    );
    let err = sb.run_err(&["unburn", id2], 3);
    assert!(
        err.contains("error[E808 usage]")
            && err.contains("9.9.9")
            && err.contains("cig versions add 9.9.9"),
        "{err}"
    );
    let id3 = "20260103T000000-aaa999";
    write_run(
        &sb,
        id3,
        serde_json::json!({"id": id3, "format": 7, "version": "9.9.9", "script": "newer.cig"}),
        "{\"format\":7}\n",
    );
    let runs = sb.runs();
    let rec = runs.iter().find(|r| r["id"] == id3).unwrap();
    assert_eq!(rec["status"], "newer", "{rec}");
}

#[test]
fn a_pinned_script_names_the_version_it_needs() {
    let sb = Sandbox::new();
    // Satisfied: the pin names this cig's own major.minor.
    let mm = {
        let mut parts = env!("CARGO_PKG_VERSION").split('.');
        format!("{}.{}", parts.next().unwrap(), parts.next().unwrap())
    };
    sb.write(
        "ok.cig",
        &format!("#!/usr/bin/env cig\n# cig {mm}\nexhale \"hi\"\n"),
    );
    assert_eq!(sb.run_ok(&["run", "ok.cig"]), "hi\n");
    // A usage line in a comment is not a pin.
    sb.write("usage.cig", "# cig run usage.cig -- x\nexhale \"hi\"\n");
    assert_eq!(sb.run_ok(&["run", "usage.cig"]), "hi\n");
    // Not satisfied, nothing on the shelf, no network: refused, never run.
    sb.write("old.cig", "# cig 1.0.0\nexhale \"never\"\n");
    let err = sb.run_err(&["run", "old.cig"], 3);
    assert!(
        err.contains("error[E807 usage]")
            && err.contains("1.0.0")
            && err.contains("cig versions add 1.0.0"),
        "{err}"
    );
    let err = sb.run_err(&["check", "old.cig"], 3);
    assert!(
        err.contains("error[E807 usage]"),
        "check dispatches too: {err}"
    );
    // A project file pins every script; a header wins over it.
    fs::write(sb.path().join(".cig-version"), "1.0.0\n").unwrap();
    sb.write("plain.cig", "exhale \"file-pinned\"\n");
    let err = sb.run_err(&["run", "plain.cig"], 3);
    assert!(
        err.contains("pinned by") && err.contains(".cig-version"),
        "{err}"
    );
    assert_eq!(sb.run_ok(&["run", "ok.cig"]), "hi\n", "the header wins");
    fs::remove_file(sb.path().join(".cig-version")).unwrap();
    // A pin that cannot be read is an error, not a guess.
    sb.write("bad.cig", "# cig 1.x\nexhale \"never\"\n");
    let err = sb.run_err(&["run", "bad.cig"], 3);
    assert!(
        err.contains("error[E807 usage]") && err.contains("cannot be read"),
        "{err}"
    );
    // A dispatched cig never dispatches again: it runs, and says so.
    let out = sb.cig_env(&["run", "old.cig"], &[("CIG_DISPATCHED", "1")]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "never\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("running anyway"));
}

#[test]
fn a_pin_is_written_under_the_shebang_by_cig_pin() {
    let sb = Sandbox::new();
    sb.write("s.cig", "#!/usr/bin/env cig\nexhale 1\n");
    sb.run_ok(&["pin", "s.cig"]);
    assert_eq!(
        sb.read("s.cig"),
        "#!/usr/bin/env cig\n# cig >=1.0.0\nexhale 1\n"
    );
    sb.run_ok(&["pin", "s.cig", "--to", ">=1.1.2"]);
    assert_eq!(
        sb.read("s.cig"),
        "#!/usr/bin/env cig\n# cig >=1.1.2\nexhale 1\n"
    );
    assert_eq!(sb.run_ok(&["run", "s.cig"]), "1\n");
    let err = sb.run_err(&["pin", "s.cig", "--to", "banana"], 3);
    assert!(err.contains("error[E807 usage]"), "{err}");
}

#[test]
fn a_humidor_binary_with_the_wrong_hash_is_never_run() {
    let sb = Sandbox::new();
    let version = cigscript::update::Version(1, 0, 0);
    let name = cigscript::update::asset_name(&version);
    let asset = sb.write("asset.bin", "not a real cig, but bytes");
    let sidecar = sb.write("asset.sha256", &format!("{}  {name}\n", "0".repeat(64)));
    let manifest = serde_json::json!([{
        "tag_name": "v1.0.0", "prerelease": false, "draft": false, "html_url": "u", "published_at": "",
        "assets": [
            {"name": name, "browser_download_url": format!("file://{}", asset.display())},
            {"name": format!("{name}.sha256"), "browser_download_url": format!("file://{}", sidecar.display())}
        ]
    }]);
    let manifest_path = sb.write("manifest.json", &manifest.to_string());
    let envs = [("CIG_UPDATE_MANIFEST", manifest_path.to_str().unwrap())];
    let out = sb.cig_env(&["versions", "add", "1.0.0"], &envs);
    assert_eq!(out.status.code(), Some(3));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("checksum mismatch") && err.contains("nothing was kept"),
        "{err}"
    );
    assert!(
        !sb.exists("home/humidor/1.0.0"),
        "a failed check left a binary on the shelf"
    );
    // With the right checksum it is kept, listed, and a pin can use it.
    let sum = cigscript::burn::journal::sha256_file(&asset).unwrap();
    fs::write(&sidecar, format!("{sum}  {name}\n")).unwrap();
    let out = sb.cig_env(&["versions", "add", "1.0.0"], &envs);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(sb.exists("home/humidor/1.0.0"));
    let v: serde_json::Value = serde_json::from_str(&sb.run_ok(&["--json", "versions"])).unwrap();
    assert_eq!(v["humidor"][0]["version"], "1.0.0");
    assert_eq!(v["humidor"][0]["trust"], "verified");
    assert_eq!(v["humidor"][0]["sha256"], sum);
    // A yanked version is marked, and a range pin steps over it.
    let yanked = sb.write("YANKED", "1.0.0 the test says so\n");
    let out = sb.cig_env(
        &["--json", "versions"],
        &[("CIG_YANKED_FILE", yanked.to_str().unwrap())],
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["humidor"][0]["yanked"], true);
    sb.run_ok(&["versions", "remove", "1.0.0"]);
    assert!(!sb.exists("home/humidor/1.0.0"));
}

#[cfg(unix)]
fn shelf_fake(sb: &Sandbox, version: &str, exit: i32) {
    use std::os::unix::fs::PermissionsExt;
    let home = sb.home().join("humidor").join(version);
    fs::create_dir_all(&home).unwrap();
    let fake = home.join("cig");
    fs::write(
        &fake,
        format!("#!/bin/sh\necho \"fake cig {version}: $* (dispatched=${{CIG_DISPATCHED:-no}})\"\nexit {exit}\n"),
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
#[test]
fn a_pinned_script_is_lit_by_its_version_and_says_so() {
    let sb = Sandbox::new();
    shelf_fake(&sb, "1.0.0", 7);
    sb.write("s.cig", "# cig 1.0.0\nexhale \"never here\"\n");
    let out = sb.cig(&["run", "s.cig", "--", "a", "b"]);
    assert_eq!(
        out.status.code(),
        Some(7),
        "the dispatched cig's exit status is the exit status"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(stdout, "fake cig 1.0.0: run s.cig -- a b (dispatched=1)\n");
    assert!(
        stderr.contains("run lit by cig 1.0.0 (pinned in s.cig (line 1)); installed is"),
        "{stderr}"
    );
    let out = sb.cig(&["--json", "check", "s.cig"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "fake cig 1.0.0: --json check s.cig (dispatched=1)\n"
    );
    let v: serde_json::Value = serde_json::from_str(&sb.run_ok(&["--json", "versions"])).unwrap();
    assert_eq!(v["humidor"][0]["version"], "1.0.0");
    assert!(v["humidor"][0]["last_lit"].is_string(), "{v}");
}

#[cfg(unix)]
#[test]
fn unburn_hands_an_old_journal_to_the_cig_that_wrote_it() {
    let sb = Sandbox::new();
    shelf_fake(&sb, "9.9.9", 0);
    let id = "20260102T000000-def456";
    write_run(
        &sb,
        id,
        serde_json::json!({"id": id, "format": 1, "version": "9.9.9", "script": "new.cig", "script_sha256": "",
            "args": [], "cwd": "", "mode": "run", "started": "2026-01-02T00:00:00.000Z", "status": "ok",
            "burns": 1, "irreversible": 0}),
        "{\"format\":99}\n",
    );
    let out = sb.cig(&["unburn", id]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("fake cig 9.9.9: unburn {id} (dispatched=1)\n")
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unburn handed to cig 9.9.9"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ----- #22, the automatic part: the since table, last good, the trial and the fallback

#[test]
fn a_script_pinned_below_a_builtin_it_calls_is_caught_before_it_runs() {
    let sb = Sandbox::new();
    // proc.text arrived in 1.1.0; a pin that allows 1.0.0 contradicts it.
    sb.write(
        "p.cig",
        "# cig >=1.0.0\nburn { stick r = proc.text(\"true\", []) }\n",
    );
    let err = sb.run_err(&["check", "--deny-warnings", "p.cig"], 2);
    assert!(
        err.contains("warning[E309 check]") && err.contains("proc.text") && err.contains("1.1.0"),
        "{err}"
    );
    sb.write(
        "q.cig",
        "# cig >=1.1.0\nburn { stick r = proc.text(\"true\", []) }\n",
    );
    sb.run_ok(&["check", "--deny-warnings", "q.cig"]);
    // cig pin writes what the script needs, and check --json says the same.
    sb.write("r.cig", "burn { stick r = proc.text(\"true\", []) }\n");
    sb.run_ok(&["pin", "r.cig"]);
    assert!(
        sb.read("r.cig").starts_with("# cig >=1.1.0\n"),
        "{}",
        sb.read("r.cig")
    );
    let v: serde_json::Value =
        serde_json::from_str(&sb.run_ok(&["--json", "check", "r.cig"])).unwrap();
    assert_eq!(v["pin"], ">=1.1.0");
    assert_eq!(v["needs"], "1.1.0");
    assert_eq!(v["uses"][0]["what"], "proc.text");
    assert!(v["last_good"].is_null(), "{v}");
    sb.write("plain.cig", "exhale 1\n");
    sb.run_ok(&["pin", "plain.cig"]);
    assert_eq!(sb.read("plain.cig"), "# cig >=1.0.0\nexhale 1\n");
}

#[test]
fn the_newest_ok_run_names_the_last_good_version() {
    let sb = Sandbox::new();
    sb.write("s.cig", "exhale 1\n");
    sb.run_ok(&["run", "s.cig"]);
    let sha = sb.runs()[0]["script_sha256"].as_str().unwrap().to_string();
    let v: serde_json::Value =
        serde_json::from_str(&sb.run_ok(&["--json", "check", "s.cig"])).unwrap();
    assert_eq!(v["last_good"], env!("CARGO_PKG_VERSION"), "{v}");
    // A later ok run by an older cig is the last good one; a later failed run is not.
    for (id, started, version, status) in [
        (
            "20991231T000000-aaaaaa",
            "2099-12-31T00:00:00.000Z",
            "1.0.0",
            "ok",
        ),
        (
            "20991231T000001-bbbbbb",
            "2099-12-31T00:00:01.000Z",
            "1.0.5",
            "error",
        ),
    ] {
        write_run(
            &sb,
            id,
            serde_json::json!({"id": id, "format": 1, "version": version, "script": "s.cig", "script_sha256": sha,
                "args": [], "cwd": "", "mode": "run", "started": started, "finished": started, "status": status,
                "burns": 0, "irreversible": 0}),
            "{\"format\":1}\n",
        );
    }
    let v: serde_json::Value =
        serde_json::from_str(&sb.run_ok(&["--json", "check", "s.cig"])).unwrap();
    assert_eq!(v["last_good"], "1.0.0", "{v}");
}

/// A fake cig on the shelf that hands back a recorded plan for a dry run
/// and reports itself for a real one.
#[cfg(unix)]
fn shelf_fake_with_plan(sb: &Sandbox, version: &str, plan_path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let home = sb.home().join("humidor").join(version);
    fs::create_dir_all(&home).unwrap();
    let fake = home.join("cig");
    fs::write(
        &fake,
        format!(
            "#!/bin/sh\ncase \"$*\" in\n  *--dry-run*) cat \"{}\" ;;\n  *) echo \"fake cig {version} ran it: $*\" ;;\nesac\n",
            plan_path.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
fn prove_on(sb: &Sandbox, script: &str, version: &str) {
    let sha = cigscript::burn::journal::sha256_file(&sb.path().join(script)).unwrap();
    let id = format!("20990101T000000-{}", &sha[..6]);
    write_run(
        sb,
        &id,
        serde_json::json!({"id": id, "format": 1, "version": version, "script": script, "script_sha256": sha,
            "args": [], "cwd": "", "mode": "run", "started": "2099-01-01T00:00:00.000Z",
            "finished": "2099-01-01T00:00:01.000Z", "status": "ok", "burns": 1, "irreversible": 0}),
        "{\"format\":1}\n",
    );
}

#[cfg(unix)]
#[test]
fn identical_plans_run_on_the_new_version() {
    let sb = Sandbox::new();
    sb.write(
        "s.cig",
        "burn { fs.write_text(\"out.txt\", \"made\") }\nexhale \"done\"\n",
    );
    let plan = sb.run_ok(&["--json", "run", "--dry-run", "s.cig"]);
    let plan_path = sb.write("plan.json", &plan);
    shelf_fake_with_plan(&sb, "1.0.0", &plan_path);
    prove_on(&sb, "s.cig", "1.0.0");
    let out = sb.cig(&["run", "s.cig"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "done\n");
    assert!(
        stderr.contains("identical plans") && stderr.contains("running on"),
        "{stderr}"
    );
    assert_eq!(sb.read("out.txt"), "made");
    let runs = sb.runs();
    let run = runs
        .iter()
        .find(|r| r["version"] == env!("CARGO_PKG_VERSION"))
        .unwrap();
    assert_eq!(run["trial"]["same"], true, "{run}");
    assert_eq!(run["trial"]["compared"][0], "1.0.0");
    // Plans that differ hand the run to the proved version.
    sb.write("plan.json", &plan.replace("write ", "write elsewhere/"));
    let out = sb.cig(&["run", "s.cig"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "fake cig 1.0.0 ran it: run s.cig\n",
        "{stderr}"
    );
    assert!(
        stderr.contains("plans differ") || stderr.contains("plans 1 op"),
        "{stderr}"
    );
    assert!(stderr.contains("running on cig 1.0.0"), "{stderr}");
    // And --no-trial runs here regardless.
    let out = sb.cig(&["run", "--no-trial", "s.cig"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "done\n");
}

#[cfg(unix)]
#[test]
fn a_failed_reversible_run_falls_back_and_an_irreversible_one_stops() {
    // Reversible: the write came back, so the last good version gets a go.
    let sb = Sandbox::new();
    sb.write(
        "f.cig",
        "burn { fs.write_text(\"a.txt\", \"1\") }\ncough \"boom\"\n",
    );
    let plan = String::from_utf8_lossy(&sb.cig(&["--json", "run", "--dry-run", "f.cig"]).stdout)
        .to_string();
    let plan_path = sb.write("plan.json", &plan);
    shelf_fake_with_plan(&sb, "1.0.0", &plan_path);
    prove_on(&sb, "f.cig", "1.0.0");
    let out = sb.cig(&["run", "f.cig"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "fake cig 1.0.0 ran it: run f.cig\n"
    );
    assert!(
        stderr.contains("boom")
            && stderr.contains("trying cig 1.0.0")
            && stderr.contains("works on cig 1.0.0 and not on"),
        "{stderr}"
    );
    assert!(
        !sb.exists("a.txt"),
        "the failed attempt was rolled back before the fallback"
    );
    let failed = sb
        .runs()
        .into_iter()
        .find(|r| r["status"] == "rolled_back")
        .unwrap();
    assert_eq!(failed["trial"]["fallback"], "1.0.0", "{failed}");

    // Irreversible: a hop ran, so nothing is tried twice.
    let sb = Sandbox::new();
    sb.write("g.cig", "burn { proc.run(\"true\", []) }\ncough \"boom\"\n");
    let plan = String::from_utf8_lossy(&sb.cig(&["--json", "run", "--dry-run", "g.cig"]).stdout)
        .to_string();
    let plan_path = sb.write("plan.json", &plan);
    shelf_fake_with_plan(&sb, "1.0.0", &plan_path);
    prove_on(&sb, "g.cig", "1.0.0");
    let out = sb.cig(&["run", "g.cig"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(!String::from_utf8_lossy(&out.stdout).contains("fake cig"));
    assert!(
        stderr.contains("irreversible") && stderr.contains("not trying it again"),
        "{stderr}"
    );
}

// ----- cig.*: a script can read the cig it runs under (for the tools written in CigScript)

#[test]
fn a_script_reads_its_cig_version_platform_and_run_id() {
    let sb = Sandbox::new();
    sb.write(
        "v.cig",
        "exhale cig.version()\nexhale cig.os(), cig.arch()\nexhale cig.run_id()\nburn { fs.write_text(\"a.txt\", \"x\") }\nexhale cig.run_id()\n",
    );
    let out = sb.run_ok(&["run", "v.cig"]);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        lines[1],
        format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)
    );
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    assert_eq!(
        lines[2], id,
        "the record exists before the first statement: {out}"
    );
    assert_eq!(lines[3], id);
    // A dry run has no record to name.
    let dry = sb.run_ok(&["run", "--dry-run", "v.cig"]);
    assert!(dry.lines().nth(2) == Some("null"), "{dry}");
    // And `check` sees the namespace.
    sb.run_ok(&["check", "v.cig"]);
}

// ----- the tin (cig-orchestrator #18, the kernel side): a hop's modified and deleted files come back

#[cfg(unix)]
#[test]
fn a_modified_file_comes_back_byte_for_byte() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "original\n");
    sb.write("p/config.ini", "cfg=1\n");
    sb.write("p/gone.txt", "doomed\n");
    sb.write("p/sub/d.txt", "deep\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo HACKED > p/keep.txt; printf 'cfg=999\\\\n' > p/config.ini; rm p/gone.txt; rm -r p/sub\"], {touches: [\"**\"]})\n  cough \"a later task failed\"\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert_eq!(sb.read("p/keep.txt"), "original\n", "{err}");
    assert_eq!(sb.read("p/config.ini"), "cfg=1\n");
    assert_eq!(sb.read("p/gone.txt"), "doomed\n");
    assert_eq!(sb.read("p/sub/d.txt"), "deep\n");
    assert!(
        err.contains("restored hop modified") && err.contains("restored hop deleted"),
        "{err}"
    );
    assert!(!err.contains("cannot undo hop"), "{err}");
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let shown = sb.run_ok(&["runs", &id]);
    assert!(shown.contains("(tin)"), "{shown}");
    let du: serde_json::Value = serde_json::from_str(&sb.run_ok(&["--json", "tin", "du"])).unwrap();
    // Four before-images, plus the two files the hop rewrote: the rollback
    // moved the hop's bytes into the tin before restoring over them.
    assert_eq!(du["objects"], 6, "{du}");
    assert_eq!(du["referenced"], 6);
    sb.run_ok(&["tin", "verify"]);
    // The same content stashed again costs nothing: one object per distinct bytes.
    sb.run_err(&["run", "s.cig"], 1);
    let du: serde_json::Value = serde_json::from_str(&sb.run_ok(&["--json", "tin", "du"])).unwrap();
    assert_eq!(du["objects"], 6, "{du}");
}

#[cfg(unix)]
#[test]
fn a_same_size_edit_inside_touches_is_seen() {
    let sb = Sandbox::new();
    sb.write("p/a.txt", "aaaa\n");
    let pin_time = |sb: &Sandbox| {
        assert!(Command::new("touch")
            .args(["-t", "202001010000", "p/a.txt"])
            .current_dir(sb.path())
            .status()
            .unwrap()
            .success());
    };
    pin_time(&sb);
    // The edit keeps the size and puts the mtime back: invisible to a watch
    // that compares those, visible to the hash of the tinned file.
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"printf 'bbbb\\\\n' > p/a.txt; touch -t 202001010000 p/a.txt\"], {touches: [\"*\"]})\n  cough \"x\"\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert_eq!(sb.read("p/a.txt"), "aaaa\n", "{err}");
    assert!(err.contains("restored hop modified"), "{err}");
}

#[cfg(unix)]
#[test]
fn a_venv_inside_the_pack_is_not_journaled() {
    let sb = Sandbox::new();
    sb.write("p/.cigignore", "# rebuilt, not restored\ncache/\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"mkdir -p p/cache p/__pycache__; echo x > p/cache/x; echo y > p/__pycache__/m.pyc; echo z > p/keep.txt\"])\n  cough \"x\"\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(!sb.exists("p/keep.txt"), "{err}");
    assert!(sb.exists("p/cache/x"), "an ignored tree is left alone");
    assert!(
        sb.exists("p/__pycache__/m.pyc"),
        "a built-in skip is left alone"
    );
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let shown = sb.run_ok(&["runs", &id]);
    assert_eq!(shown.matches("hop created").count(), 1, "{shown}");
}

/// #125: two hops in one run that wrote the same file roll back to the
/// start. The older hop's entry is checked against what the newer one's
/// restore put back, not against what the run left, so no `--force` is
/// needed; a second unburn is refused as already done.
#[cfg(unix)]
#[test]
fn two_hops_one_file_roll_back_to_the_start() {
    let sb = Sandbox::new();
    sb.write("p/a.txt", "v0\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn { proc.run(\"sh\", [\"-c\", \"echo v1 > a.txt\"], {cwd: \"p\", touches: [\"a.txt\"]}) }\nburn { proc.run(\"sh\", [\"-c\", \"echo v2 > a.txt\"], {cwd: \"p\", touches: [\"a.txt\"]}) }\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    assert_eq!(sb.read("p/a.txt"), "v2\n");
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    sb.run_ok(&["unburn", &id]);
    assert_eq!(
        sb.read("p/a.txt"),
        "v0\n",
        "the run did not roll back to the start"
    );
    assert_eq!(sb.runs()[0]["status"], "unburned", "{}", sb.runs()[0]);
}

#[cfg(unix)]
#[test]
fn gc_keeps_every_chunk_a_live_run_needs() {
    let sb = Sandbox::new();
    sb.write("p/f.txt", "one\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo two > p/f.txt\"], {touches: [\"f.txt\"]})\n}\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    assert_eq!(sb.read("p/f.txt"), "two\n");
    let gc: serde_json::Value = serde_json::from_str(&sb.run_ok(&["--json", "tin", "gc"])).unwrap();
    assert_eq!(gc["removed"], 0, "a run still refers to it: {gc}");
    assert_eq!(gc["kept"], 1);
    // Corruption is seen before anything is restored from it.
    let objects = sb.home().join("tin").join("objects");
    let mut found = None;
    for bucket in fs::read_dir(&objects).unwrap().flatten() {
        for obj in fs::read_dir(bucket.path()).unwrap().flatten() {
            found = Some(obj.path());
        }
    }
    let object = found.expect("an object");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&object, fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(&object, "garbage").unwrap();
    let err = sb.run_err(&["tin", "verify"], 1);
    assert!(err.contains("does not hash to its name"), "{err}");
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let err = sb.run_err(&["unburn", &id], 1);
    assert!(
        err.contains("error[E703") && err.contains("corrupt"),
        "{err}"
    );
    assert_eq!(sb.read("p/f.txt"), "two\n", "nothing was restored");
    // Once no run needs it, gc lets it go.
    sb.run_ok(&["runs", "--prune", "0"]);
    let gc: serde_json::Value = serde_json::from_str(&sb.run_ok(&["--json", "tin", "gc"])).unwrap();
    assert_eq!(gc["removed"], 1, "{gc}");
    assert_eq!(gc["kept"], 0);
}

#[cfg(unix)]
#[test]
fn touches_without_a_pack_is_refused() {
    let sb = Sandbox::new();
    sb.write(
        "s.cig",
        "burn { proc.run(\"true\", [], {touches: [\"**\"]}) }\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(err.contains("error[E757 burn]"), "{err}");
}

// ----- 1.2.1: a read-only file is snapshotted like any other (the tin's objects, .git/objects)

#[test]
fn a_read_only_file_in_a_tree_is_snapshotted_and_comes_back_read_only() {
    let sb = Sandbox::new();
    sb.write("t/keep.txt", "keep\n");
    sb.write("t/ro.txt", "frozen\n");
    let ro = sb.path().join("t").join("ro.txt");
    let mut perms = fs::metadata(&ro).unwrap().permissions();
    perms.set_readonly(true);
    fs::set_permissions(&ro, perms).unwrap();
    sb.write("s.cig", "burn { fs.rm(\"t\") }\n");
    sb.run_ok(&["run", "s.cig"]);
    assert!(!sb.exists("t"));
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    sb.run_ok(&["unburn", &id]);
    assert_eq!(sb.read("t/ro.txt"), "frozen\n");
    assert!(fs::metadata(&ro).unwrap().permissions().readonly());
    // A single read-only file, snapshotted on its own (and the same as
    // root, who may write it: the snapshot is what is under test).
    sb.write("s2.cig", "burn { fs.rm(\"t/ro.txt\") }\n");
    sb.run_ok(&["run", "s2.cig"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    sb.run_ok(&["unburn", &id]);
    assert_eq!(sb.read("t/ro.txt"), "frozen\n");
}

// ----- the tin keeps what a file is, not only its bytes (#41): links, modes, times

#[cfg(unix)]
#[test]
fn the_tin_restores_a_symlink_as_a_symlink() {
    use std::os::unix::fs::symlink;
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "keep\n");
    sb.write("p/other.txt", "other\n");
    symlink("keep.txt", sb.path().join("p/link.txt")).unwrap();
    symlink("keep.txt", sb.path().join("p/retarget.txt")).unwrap();
    symlink("keep.txt", sb.path().join("p/gone.txt")).unwrap();
    // The hop replaces one link with a plain file, points another somewhere
    // else, and deletes the third; only the first is under `touches`, and
    // none of them needs the tin: a link is its target, and the journal
    // keeps that itself.
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"rm p/link.txt; echo plain > p/link.txt; ln -sfn other.txt p/retarget.txt; rm p/gone.txt\"], {touches: [\"link.txt\"]})\n  cough \"x\"\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    for name in ["link.txt", "retarget.txt", "gone.txt"] {
        let p = sb.path().join("p").join(name);
        let meta = fs::symlink_metadata(&p).unwrap_or_else(|_| panic!("{name} is gone: {err}"));
        assert!(meta.file_type().is_symlink(), "{name} is not a link: {err}");
        assert_eq!(
            fs::read_link(&p).unwrap(),
            PathBuf::from("keep.txt"),
            "{name}"
        );
    }
    assert_eq!(sb.read("p/keep.txt"), "keep\n");
    assert!(err.contains("(link kept)"), "{err}");
    assert!(!err.contains("cannot undo hop"), "{err}");
}

#[cfg(unix)]
#[test]
fn the_tin_restores_a_mode_changed_by_a_hop() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    sb.write("p/run.sh", "#!/bin/sh\necho hi\n");
    sb.write("p/other.sh", "#!/bin/sh\necho there\n");
    fs::set_permissions(
        sb.path().join("p/run.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    fs::set_permissions(
        sb.path().join("p/other.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    // A chmod changes neither size nor mtime; it is seen all the same, and
    // put back where the file was kept in the tin.
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"chmod 644 p/run.sh p/other.sh\"], {touches: [\"run.sh\"]})\n  cough \"x\"\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    let mode = |name: &str| {
        fs::metadata(sb.path().join("p").join(name))
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode("run.sh"), 0o755, "{err}");
    assert!(
        err.contains("restored hop modified") && err.contains("run.sh"),
        "{err}"
    );
    // The one outside `touches` is seen too, and said to be irreversible.
    assert_eq!(mode("other.sh"), 0o644);
    assert!(
        err.contains("cannot undo hop modified") && err.contains("other.sh"),
        "{err}"
    );
}

#[cfg(unix)]
#[test]
fn the_tin_restores_mtime() {
    let sb = Sandbox::new();
    sb.write("p/config.ini", "[a]\nb = 1\n");
    sb.write("p/native.ini", "[c]\nd = 2\n");
    let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_577_836_800); // 2020-01-01
    for name in ["p/config.ini", "p/native.ini"] {
        fs::File::open(sb.path().join(name))
            .unwrap()
            .set_modified(old)
            .unwrap();
    }
    let mtime = |name: &str| {
        fs::metadata(sb.path().join(name))
            .unwrap()
            .modified()
            .unwrap()
    };
    // A hop's edit under `touches`, and a native write: both come back with
    // the time they had, so make and its kin see no change.
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo e = 3 >> p/config.ini\"], {touches: [\"*.ini\"]})\n  fs.write_text(\"p/native.ini\", \"changed\")\n  cough \"x\"\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert_eq!(sb.read("p/config.ini"), "[a]\nb = 1\n", "{err}");
    assert_eq!(sb.read("p/native.ini"), "[c]\nd = 2\n", "{err}");
    assert_eq!(
        mtime("p/config.ini"),
        old,
        "the hop's edit came back dated now"
    );
    assert_eq!(
        mtime("p/native.ini"),
        old,
        "the native write came back dated now"
    );
}

#[cfg(unix)]
#[test]
fn a_read_only_file_under_touches_is_kept_in_the_tin() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    sb.write("p/ro.txt", "frozen\n");
    fs::set_permissions(
        sb.path().join("p/ro.txt"),
        fs::Permissions::from_mode(0o444),
    )
    .unwrap();
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"chmod 644 p/ro.txt; echo thawed > p/ro.txt\"], {touches: [\"ro.txt\"]})\n  cough \"x\"\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(!err.contains("could not be kept in the tin"), "{err}");
    assert_eq!(sb.read("p/ro.txt"), "frozen\n", "{err}");
    assert_eq!(
        fs::metadata(sb.path().join("p/ro.txt"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o444
    );
}

// ----- finding programs (#42, #43): one resolver for run, which and shell, on every OS

#[test]
fn a_relative_program_resolves_against_the_hops_cwd() {
    let sb = Sandbox::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        sb.write("sub/tool.sh", "#!/bin/sh\necho from-sub\n");
        fs::set_permissions(
            sb.path().join("sub/tool.sh"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        sb.write(
            "s.cig",
            "burn { exhale proc.text(\"./tool.sh\", [], {cwd: \"sub\"}) }\n",
        );
    }
    #[cfg(windows)]
    {
        sb.write("sub/tool.cmd", "@echo off\r\necho from-sub\r\n");
        // No extension given: PATHEXT finds tool.cmd beside the hop's cwd.
        sb.write(
            "s.cig",
            "burn { exhale proc.text(\"./tool\", [], {cwd: \"sub\"}) }\n",
        );
    }
    let out = sb.run_ok(&["run", "s.cig"]);
    assert_eq!(out.trim(), "from-sub");
}

#[cfg(windows)]
#[test]
fn npm_resolves_to_npm_cmd_on_windows() {
    let sb = Sandbox::new();
    sb.write(
        "s.cig",
        "exhale proc.which(\"npm\")\nexhale proc.which(\"node\")\nburn { exhale proc.text(\"npm\", [\"--version\"]) }\n",
    );
    let out = sb.run_ok(&["run", "s.cig"]);
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].to_ascii_lowercase().ends_with("npm.cmd"), "{out}");
    assert!(lines[1].to_ascii_lowercase().ends_with("node.exe"), "{out}");
    assert!(
        lines[2]
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false),
        "{out}"
    );
}

#[cfg(windows)]
#[test]
fn which_and_run_agree_on_windows() {
    let sb = Sandbox::new();
    // A program with its extension spelled, one without, and a path.
    sb.write("tools/hello.bat", "@echo off\r\necho hello\r\n");
    sb.write(
        "s.cig",
        "exhale proc.which(\"node.exe\") == proc.which(\"node\")\nexhale proc.which(\"tools/hello\")\nburn { exhale proc.text(\"tools/hello\", []).trim() }\nexhale proc.which(\"no-such-program-anywhere\")\n",
    );
    let out = sb.run_ok(&["run", "s.cig"]);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "true", "{out}");
    assert!(
        lines[1].to_ascii_lowercase().ends_with("hello.bat"),
        "{out}"
    );
    assert_eq!(lines[2].trim(), "hello", "{out}");
    assert_eq!(lines[3], "null", "{out}");
}

#[cfg(windows)]
#[test]
fn bash_on_windows_is_not_the_wsl_launcher() {
    let sb = Sandbox::new();
    sb.write(
        "s.cig",
        "exhale proc.which(\"bash\")\nexhale proc.which(\"sh\")\nburn { exhale proc.text(\"bash\", [\"-c\", \"echo $((20+22))\"]) }\n",
    );
    let out = sb.run_ok(&["run", "s.cig"]);
    let lines: Vec<&str> = out.lines().collect();
    for which in &lines[..2] {
        let lower = which.to_ascii_lowercase();
        assert!(
            !lower.contains("system32") && !lower.contains("syswow64"),
            "{out}"
        );
        assert!(lower.ends_with(".exe"), "{out}");
    }
    assert_eq!(lines[2].trim(), "42", "{out}");
}

#[cfg(windows)]
#[test]
fn proc_shell_on_windows_runs_sh_when_present() {
    let sb = Sandbox::new();
    // Git for Windows puts sh on PATH on the runners: a POSIX line works,
    // and the plan names the shell that ran it.
    sb.write(
        "s.cig",
        "burn { exhale proc.shell(\"x=$((6*7)); echo \\\"answer $x\\\"\").out.trim() }\n",
    );
    let out = sb.run_ok(&["run", "s.cig"]);
    assert_eq!(out.trim(), "answer 42", "{out}");
    let dry = sb.cig(&["run", "--dry-run", "s.cig"]);
    let plan = format!(
        "{}{}",
        String::from_utf8_lossy(&dry.stdout),
        String::from_utf8_lossy(&dry.stderr)
    );
    assert!(plan.contains("run sh -c"), "{plan}");
}

// ----- Windows: a held-open file comes back (#44); a timeout ends the whole tree (#45)

#[cfg(windows)]
#[test]
fn a_file_held_open_without_share_delete_is_restored_in_place_on_windows() {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    let sb = Sandbox::new();
    sb.write("p/a.txt", "before\n");
    // An editor, a scanner or an indexer: read and write may be shared,
    // delete is not, so the file cannot be renamed over while it is open.
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(sb.path().join("p/a.txt"))
        .unwrap();
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"cmd\", [\"/c\", \"echo changed> p\\\\a.txt\"], {touches: [\"a.txt\"]})\n  cough \"x\"\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    drop(held);
    assert_eq!(sb.read("p/a.txt"), "before\n", "{err}");
    assert!(
        err.contains("in place: another program held it open"),
        "{err}"
    );
    assert!(
        !err.contains("rollback_incomplete") && !err.contains("failed write"),
        "{err}"
    );
}

#[cfg(windows)]
#[test]
fn an_apostrophe_reaches_an_msys_program_on_windows() {
    let sb = Sandbox::new();
    // Git's sh parses its own command line: bare, `it's` arrived as `its`
    // and `a*b` could glob.
    sb.write(
        "s.cig",
        "burn {\n  stick r = proc.run(\"sh\", [\"-c\", \"printf '<%s>' \\\"$@\\\"\", \"_\", \"it's\", \"a*b\", \"plain\"])\n  exhale r.out\n}\n",
    );
    let out = sb.run_ok(&["run", "s.cig"]);
    assert!(out.contains("<it's><a*b><plain>"), "{out}");
}

/// Some case-insensitive filesystems rename across case (APFS, NTFS); a
/// casefold ext4 directory reads the two spellings as one entry and leaves
/// the old one. Only the first kind can prove a case-only rename happened.
fn renames_across_case(dir: &Path) -> bool {
    std::fs::write(dir.join("probe"), "x").unwrap();
    let renamed = std::fs::rename(dir.join("probe"), dir.join("PROBE")).is_ok()
        && std::fs::read_dir(dir)
            .unwrap()
            .any(|e| e.unwrap().file_name() == "PROBE");
    let _ = std::fs::remove_file(dir.join("probe"));
    let _ = std::fs::remove_file(dir.join("PROBE"));
    renamed
}

#[test]
fn a_case_only_rename_is_a_rename_on_a_case_insensitive_filesystem() {
    let sb = Sandbox::new();
    sb.write("dockerfile", "FROM scratch\n");
    if !sb.path().join("DOCKERFILE").exists() {
        eprintln!("skipped: this filesystem is case-sensitive");
        return;
    }
    if !renames_across_case(sb.path()) {
        eprintln!("skipped: this filesystem keeps the old spelling on a case-only rename");
        return;
    }
    sb.write(
        "s.cig",
        "burn {\n  fs.mv(\"dockerfile\", \"Dockerfile\")\n}\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    let names: Vec<String> = std::fs::read_dir(sb.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        names.iter().any(|n| n == "Dockerfile") && !names.iter().any(|n| n == "dockerfile"),
        "{names:?}"
    );
}

#[test]
fn a_case_only_rename_of_a_directory_is_a_rename_on_a_case_insensitive_filesystem() {
    let sb = Sandbox::new();
    std::fs::create_dir(sb.path().join("docs")).unwrap();
    sb.write("docs/a.md", "a\n");
    if !sb.path().join("DOCS").exists() {
        eprintln!("skipped: this filesystem is case-sensitive");
        return;
    }
    if !renames_across_case(sb.path()) {
        eprintln!("skipped: this filesystem keeps the old spelling on a case-only rename");
        return;
    }
    sb.write("s.cig", "burn {\n  fs.mv(\"docs\", \"Docs\")\n}\n");
    sb.run_ok(&["run", "s.cig"]);
    let names: Vec<String> = std::fs::read_dir(sb.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        names.iter().any(|n| n == "Docs") && !names.iter().any(|n| n == "docs"),
        "{names:?}"
    );
    assert!(sb.path().join("Docs").join("a.md").exists());
}

#[test]
fn a_copy_onto_its_own_name_in_another_case_is_refused() {
    let sb = Sandbox::new();
    sb.write("readme.md", "FROM scratch\n");
    if !sb.path().join("README.md").exists() {
        eprintln!("skipped: this filesystem is case-sensitive");
        return;
    }
    sb.write(
        "s.cig",
        "burn {\n  fs.cp(\"readme.md\", \"README.md\")\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(err.contains("error[E508 runtime]"), "{err}");
    assert!(err.contains("are the same file"), "{err}");
    assert_eq!(
        std::fs::read_to_string(sb.path().join("readme.md")).unwrap(),
        "FROM scratch\n"
    );
}

#[cfg(unix)]
#[test]
fn a_symlink_is_not_renamed_over_its_target_by_a_case_only_name() {
    let sb = Sandbox::new();
    sb.write("A", "important\n");
    if sb.path().join("a").exists() {
        eprintln!("skipped: this filesystem is case-insensitive");
        return;
    }
    std::os::unix::fs::symlink("A", sb.path().join("a")).unwrap();
    sb.write("s.cig", "burn {\n  fs.mv(\"a\", \"A\")\n}\n");
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(err.contains("error[E508 runtime]"), "{err}");
    assert_eq!(
        std::fs::read_to_string(sb.path().join("A")).unwrap(),
        "important\n"
    );
    assert!(std::fs::symlink_metadata(sb.path().join("a"))
        .unwrap()
        .file_type()
        .is_symlink());
}

#[test]
fn a_hops_own_path_chooses_the_program() {
    let sb = Sandbox::new();
    let (a, b) = (sb.path().join("binA"), sb.path().join("binB"));
    for (dir, word) in [(&a, "A-from-binA"), (&b, "B-from-binB")] {
        std::fs::create_dir_all(dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let tool = dir.join("tool");
            std::fs::write(&tool, format!("#!/bin/sh\necho {word}\n")).unwrap();
            std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        #[cfg(windows)]
        std::fs::write(
            dir.join("tool.cmd"),
            format!("@echo off\r\necho {word}\r\n"),
        )
        .unwrap();
    }
    let sep = if cfg!(windows) { ";" } else { ":" };
    let inherited = std::env::var("PATH").unwrap_or_default();
    let parent = format!("{}{sep}{inherited}", a.display());
    let child = format!("{}{sep}{inherited}", b.display());
    sb.write(
        "s.cig",
        "burn {\n  stick r = proc.run(\"tool\", [], {env: {PATH: env.get(\"CHILD_PATH\")}})\n  exhale r.out.trim()\n}\n",
    );
    let out = sb.cig_env(
        &["run", "s.cig"],
        &[("PATH", &parent), ("CHILD_PATH", &child)],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("B-from-binB"), "{stdout}");
}

#[test]
#[cfg(unix)]
fn a_partial_rollback_is_finished_by_a_plain_unburn() {
    use std::os::unix::fs::PermissionsExt;
    if running_as_root() {
        eprintln!("skipped: root ignores directory modes, so no restore can be made to fail here");
        return;
    }
    let sb = Sandbox::new();
    sb.write("a.txt", "o\n");
    sb.write("sub/b.txt", "o\n");
    sb.write(
        "s.cig",
        "burn {\n  fs.write_text(\"a.txt\", \"n\")\n  fs.write_text(\"sub/b.txt\", \"n\")\n}\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let sub = sb.path().join("sub");
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o500)).unwrap();
    let first = sb.cig(&["unburn", &id]);
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(!first.status.success());
    assert_eq!(sb.read("a.txt"), "o\n");
    assert_eq!(sb.read("sub/b.txt"), "n");
    let rec = &sb.runs()[0];
    assert_eq!(rec["status"], "unburn_failed");
    assert_eq!(rec["rollback_failed"].as_array().map(Vec::len), Some(1));
    // An edit made after the first pass stays: the retry touches only what failed.
    sb.write("a.txt", "edited\n");
    let second = sb.run_ok(&["unburn", &id]);
    assert!(second.contains("sub/b.txt"), "{second}");
    assert_eq!(sb.read("sub/b.txt"), "o\n");
    assert_eq!(sb.read("a.txt"), "edited\n");
    assert_eq!(sb.runs()[0]["status"], "unburned");
    let third = sb.run_err(&["unburn", &id], 1);
    assert!(third.contains("error[E705 burn]:"), "{third}");
}

#[cfg(unix)]
#[test]
fn the_tin_key_is_kept_out_of_the_tin_and_a_sealed_run_unburns() {
    let sb = Sandbox::new();
    sb.write(".env", "SECRET=hunter2\n");
    sb.write(
        "s.cig",
        "pack { \".\" }\nburn {\n  fs.write_text(\".env\", \"SECRET=changed\")\n}\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    // The key is in the keys directory beside the state root, never in the tin.
    let keys = sb.path().join(".cigscript-keys").join("seal.key");
    assert!(keys.exists(), "key not at {}", keys.display());
    assert!(
        !sb.home().join("tin").join("seal.key").exists(),
        "the key is still in the tin"
    );
    // A sealed before-copy was made (a `.cig-sealed` file under the state
    // root), and it opens again on unburn (the CIGSEAL2 round-trip).
    fn has_sealed(dir: &std::path::Path) -> bool {
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            if let Ok(rd) = std::fs::read_dir(&d) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        stack.push(p);
                    } else if p
                        .file_name()
                        .map(|n| n.to_string_lossy().ends_with(".cig-sealed"))
                        .unwrap_or(false)
                    {
                        return true;
                    }
                }
            }
        }
        false
    }
    assert!(
        has_sealed(&sb.home()),
        "no sealed object under {}",
        sb.home().display()
    );
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    sb.run_ok(&["unburn", &id]);
    assert_eq!(sb.read(".env"), "SECRET=hunter2\n");
}

#[cfg(unix)]
#[test]
fn a_pre_1_8_key_in_the_tin_is_migrated_out() {
    let sb = Sandbox::new();
    std::fs::create_dir_all(sb.home().join("tin")).unwrap();
    let legacy = sb.home().join("tin").join("seal.key");
    let hexkey = "11".repeat(32);
    std::fs::write(&legacy, format!("{hexkey}\n")).unwrap();
    sb.write(".env", "SECRET=one\n");
    sb.write(
        "s.cig",
        "pack { \".\" }\nburn {\n  fs.write_text(\".env\", \"SECRET=two\")\n}\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    assert!(!legacy.exists(), "the legacy key was left in the tin");
    let moved = sb.path().join(".cigscript-keys").join("seal.key");
    assert_eq!(
        std::fs::read_to_string(&moved).unwrap().trim(),
        hexkey,
        "the migrated key changed"
    );
}

#[cfg(unix)]
#[test]
fn the_tin_key_exports_and_imports() {
    let sb = Sandbox::new();
    sb.write(".env", "SECRET=one\n");
    sb.write(
        "s.cig",
        "pack { \".\" }\nburn {\n  fs.write_text(\".env\", \"SECRET=two\")\n}\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    let keyfile = sb.path().join(".cigscript-keys").join("seal.key");
    let before = std::fs::read_to_string(&keyfile).unwrap();
    let export = sb.path().join("key.export");
    let out = sb.cig_in(
        ".",
        &["tin", "key", "export", "--out", export.to_str().unwrap()],
        &[("CIG_KEY_PASSPHRASE", "correct horse")],
    );
    assert!(out.status.success(), "export failed");
    assert!(export.exists());
    // Lose the key, then put it back from the export.
    std::fs::remove_file(&keyfile).unwrap();
    let bad = sb.cig_in(
        ".",
        &["tin", "key", "import", export.to_str().unwrap()],
        &[("CIG_KEY_PASSPHRASE", "wrong")],
    );
    assert_eq!(bad.status.code(), Some(1), "a wrong passphrase imported");
    let ok = sb.cig_in(
        ".",
        &["tin", "key", "import", export.to_str().unwrap()],
        &[("CIG_KEY_PASSPHRASE", "correct horse")],
    );
    assert!(ok.status.success(), "import failed");
    assert_eq!(
        std::fs::read_to_string(&keyfile).unwrap().trim(),
        before.trim(),
        "the imported key differs"
    );
}

#[cfg(unix)]
#[test]
fn a_loose_state_directory_warns_unattended_and_refuses_on_a_terminal() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    std::fs::create_dir_all(sb.home()).unwrap();
    std::fs::set_permissions(sb.home(), std::fs::Permissions::from_mode(0o777)).unwrap();
    sb.write("s.cig", "burn {\n  fs.write_text(\"a.txt\", \"1\")\n}\n");
    // Unattended (no tty in the harness): a warning, and the run carries on.
    let warned = sb.cig(&["run", "s.cig"]);
    assert!(
        warned.status.success(),
        "a loose home broke an unattended run"
    );
    assert!(
        String::from_utf8_lossy(&warned.stderr).contains("E707"),
        "no E707 warning"
    );
    // On a terminal (forced): refused with the fix.
    std::fs::set_permissions(sb.home(), std::fs::Permissions::from_mode(0o777)).unwrap();
    let refused = sb.cig_in("", &["run", "s.cig"], &[("CIG_INTERNAL_ATTENDED", "1")]);
    assert_eq!(
        refused.status.code(),
        Some(1),
        "a loose home was not refused"
    );
    let err = String::from_utf8_lossy(&refused.stderr);
    assert!(err.contains("E707") && err.contains("chmod 700"), "{err}");
}

/// Seam 2 of #105: the signed journal and the ledger.
#[cfg(unix)]
fn journal_path(sb: &Sandbox, id: &str) -> PathBuf {
    sb.home().join("runs").join(id).join("journal.jsonl")
}

/// What an attacker who cut or swapped a journal also does: take the head
/// out of the record, so only the ledger can tell.
#[cfg(unix)]
fn forget_head(sb: &Sandbox, id: &str) {
    let p = sb.home().join("runs").join(id).join("run.json");
    let mut v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
    v["journal_head"] = serde_json::Value::Null;
    fs::write(&p, serde_json::to_string_pretty(&v).unwrap()).unwrap();
}

#[cfg(unix)]
fn two_op_run(sb: &Sandbox) -> String {
    sb.write("a.txt", "one\n");
    sb.write(
        "s.cig",
        "burn {\n  fs.write_text(\"a.txt\", \"two\")\n  fs.write_text(\"b.txt\", \"new\")\n}\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    sb.runs()[0]["id"].as_str().unwrap().to_string()
}

#[cfg(unix)]
#[test]
fn a_journal_is_signed_and_a_recomputed_chain_does_not_verify() {
    use sha2::Digest;
    let sb = Sandbox::new();
    let id = two_op_run(&sb);
    let path = journal_path(&sb, &id);
    let text = fs::read_to_string(&path).unwrap();
    assert!(
        text.lines().next().unwrap().contains("\"format\":3"),
        "{text}"
    );
    // The attack format 2 could not see: rewrite a line, then recompute every
    // prev as the plain hash of the line before it.
    let mut lines: Vec<serde_json::Value> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    lines[1]["at"] = serde_json::Value::String("2001-01-01T00:00:00Z".to_string());
    let mut prev = lines[0]["prev"].as_str().unwrap().to_string();
    let mut out = String::new();
    for (i, v) in lines.iter_mut().enumerate() {
        if i > 0 {
            v["prev"] = serde_json::Value::String(prev.clone());
        }
        let line = serde_json::to_string(v).unwrap();
        prev = hex::encode(sha2::Sha256::digest(line.as_bytes()));
        out.push_str(&line);
        out.push('\n');
    }
    fs::write(&path, out).unwrap();
    let err = sb.run_err(&["unburn", &id], 1);
    assert!(err.contains("error[E760 burn]:"), "{err}");
    assert_eq!(sb.read("a.txt"), "two");
}

#[cfg(unix)]
#[test]
fn a_journal_cut_short_is_caught_by_the_ledger() {
    let sb = Sandbox::new();
    let id = two_op_run(&sb);
    let path = journal_path(&sb, &id);
    let text = fs::read_to_string(&path).unwrap();
    let kept: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    fs::write(&path, format!("{}\n", kept[..kept.len() - 1].join("\n"))).unwrap();
    forget_head(&sb, &id);
    let err = sb.run_err(&["unburn", &id], 1);
    assert!(
        err.contains("error[E709 burn]:") && err.contains("cut short"),
        "{err}"
    );
    // --force restores from it as it is.
    sb.run_ok(&["unburn", &id, "--force"]);
}

#[cfg(unix)]
#[test]
fn an_older_copy_of_the_journal_is_caught_by_the_ledger() {
    let sb = Sandbox::new();
    let id = two_op_run(&sb);
    let path = journal_path(&sb, &id);
    let before = fs::read_to_string(&path).unwrap();
    // The unburn grows the record and the ledger follows it; the earlier
    // copy put back is an older record than the ledger knows.
    sb.run_ok(&["unburn", &id]);
    fs::write(&path, before).unwrap();
    forget_head(&sb, &id);
    // The run is rolled back already (E705 would answer a real unburn
    // first); the dry run shows the ledger's verdict on the old copy.
    let plan = sb.cig(&["unburn", &id, "--dry-run"]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&plan.stdout),
        String::from_utf8_lossy(&plan.stderr)
    );
    assert!(
        text.contains("E709") && text.contains("cut short"),
        "{text}"
    );
}

/// `keep_runs` prunes after each run, as `cig runs --prune` does: the
/// ledger knows, so nothing is noted missing, and the runs kept still
/// unburn (#105).
#[test]
fn keep_runs_prunes_after_each_run_and_the_ledger_stays_quiet() {
    let sb = Sandbox::new();
    sb.run_ok(&["config", "keep_runs", "2"]);
    for k in 0..4 {
        sb.write(
            "t.cig",
            &format!("burn {{\n  fs.write_text(\"f{k}.txt\", \"{k}\")\n}}\n"),
        );
        sb.run_ok(&["run", "t.cig"]);
        std::thread::sleep(std::time::Duration::from_millis(1100));
    }
    let left = sb.runs();
    assert_eq!(left.len(), 2, "{left:?}");
    let quiet = sb.cig(&["runs"]);
    assert!(
        !String::from_utf8_lossy(&quiet.stderr).contains("E710"),
        "a pruned run was noted"
    );
    sb.run_ok(&["unburn", left[0]["id"].as_str().unwrap()]);
    assert!(!sb.path().join("f3.txt").exists());
}

/// Once the tin's key is loaded, cig's memory is closed to the programs it
/// starts (#105, L5): cig is not dumpable, which a hop sees as its `/proc`
/// entries owned by root, so nothing running as you can attach to it or
/// open its memory, whatever the kernel's ptrace policy.
/// `CIG_ALLOW_DEBUG=1` opens it again, and the hop sees that too.
#[cfg(target_os = "linux")]
#[test]
fn a_hop_cannot_read_cigs_memory() {
    // SAFETY: a plain id query.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipped: root owns every /proc entry");
        return;
    }
    let sb = Sandbox::new();
    sb.write(
        "m.cig",
        "burn {\n  stick r = proc.run(\"sh\", [\"-c\", \"stat -c %u /proc/$PPID/mem\"])\n  fs.write_text(\"m.txt\", r.out)\n}\n",
    );
    sb.run_ok(&["run", "m.cig"]);
    let owner = fs::read_to_string(sb.path().join("m.txt")).unwrap();
    assert_eq!(owner.trim(), "0", "cig was dumpable while it held the key");
    let o = sb.cig_env(&["run", "m.cig"], &[("CIG_ALLOW_DEBUG", "1")]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let me = unsafe { libc::geteuid() }.to_string();
    let owner = fs::read_to_string(sb.path().join("m.txt")).unwrap();
    assert_eq!(
        owner.trim(),
        me,
        "CIG_ALLOW_DEBUG did not open cig to a debugger"
    );
}

#[cfg(unix)]
#[test]
fn a_pruned_run_is_quiet_and_a_hand_deleted_run_is_a_note() {
    let sb = Sandbox::new();
    let first = two_op_run(&sb);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    sb.write("t.cig", "burn {\n  fs.write_text(\"c.txt\", \"c\")\n}\n");
    sb.run_ok(&["run", "t.cig"]);
    let second = sb.runs()[0]["id"].as_str().unwrap().to_string();
    assert_ne!(first, second);
    sb.run_ok(&["runs", "--prune", "1"]);
    let quiet = sb.cig(&["runs"]);
    assert!(
        !String::from_utf8_lossy(&quiet.stderr).contains("E710"),
        "a pruned run was noted"
    );
    fs::remove_dir_all(sb.home().join("runs").join(&second)).unwrap();
    let noted = sb.cig(&["runs"]);
    let err = String::from_utf8_lossy(&noted.stderr).to_string();
    assert!(err.contains("E710") && err.contains(&second), "{err}");
    sb.run_ok(&["runs", "--forget", &second]);
    let silent = sb.cig(&["runs"]);
    assert!(
        !String::from_utf8_lossy(&silent.stderr).contains("E710"),
        "forgotten run still noted"
    );
}

#[cfg(unix)]
#[test]
fn a_run_signed_by_another_key_restores_with_accept_unsigned() {
    let sb = Sandbox::new();
    let id = two_op_run(&sb);
    // The key is replaced: this run's journal is under another key now.
    let key = sb.path().join(".cigscript-keys").join("seal.key");
    fs::write(&key, format!("{}\n", "22".repeat(32))).unwrap();
    let err = sb.run_err(&["unburn", &id], 1);
    assert!(err.contains("error[E708 burn]:"), "{err}");
    assert_eq!(sb.read("a.txt"), "two");
    sb.run_ok(&["unburn", &id, "--accept-unsigned"]);
    assert_eq!(sb.read("a.txt"), "one\n");
    assert!(!sb.exists("b.txt"));
}

#[cfg(unix)]
#[test]
fn journal_sign_off_writes_format_2_and_no_ledger_line() {
    let sb = Sandbox::new();
    sb.run_ok(&["config", "journal_sign", "off"]);
    let id = two_op_run(&sb);
    let text = fs::read_to_string(journal_path(&sb, &id)).unwrap();
    assert!(
        text.lines().next().unwrap().contains("\"format\":2"),
        "{text}"
    );
    assert!(!sb
        .path()
        .join(".cigscript-keys")
        .join("ledger.jsonl")
        .exists());
    sb.run_ok(&["unburn", &id]);
    assert_eq!(sb.read("a.txt"), "one\n");
}

#[cfg(unix)]
#[test]
fn tin_head_matches_the_ledger_and_an_edited_ledger_is_caught() {
    let sb = Sandbox::new();
    two_op_run(&sb);
    let head = sb.run_ok(&["tin", "head"]);
    assert!(head.trim().starts_with("1 "), "{head}");
    sb.run_ok(&["tin", "verify", "--head", head.trim()]);
    let err = sb.run_err(&["tin", "verify", "--head", "1 deadbeef"], 1);
    assert!(err.contains("cut short or swapped"), "{err}");
    // A line edited in the ledger no longer carries cig's signature.
    let ledger = sb.path().join(".cigscript-keys").join("ledger.jsonl");
    let text = fs::read_to_string(&ledger).unwrap();
    fs::write(
        &ledger,
        text.replace("\"kind\":\"run\"", "\"kind\":\"ran\""),
    )
    .unwrap();
    let err = sb.run_err(&["tin", "verify"], 1);
    assert!(err.contains("ledger") && err.contains("E709"), "{err}");
}

// ----- the tin guard (#105 L1) ---------------------------------------------

/// A boxed hop sees the state directory read-only and the key folder
/// empty, and its own layer still applies. The pack is a subfolder so the
/// state directory sits outside the overlay, as it does in use.
#[cfg(target_os = "linux")]
#[test]
fn a_boxed_hop_sees_the_tin_read_only_and_no_key() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    let home = sb.home().display().to_string();
    let keys = sb.path().join(".cigscript-keys").display().to_string();
    let script = format!(
        "pack {{ \"./p\" }}\nburn {{\n  proc.run(\"sh\", [\"-c\", \"{{ echo probe > {home}/tin-probe; }} 2>err.txt; echo rc=$? > rc.txt; cat err.txt >> rc.txt; ls -A {keys} | wc -l > keys.txt; echo $CIGSCRIPT_HOME > inner-home.txt; echo made > made.txt\"], {{cwd: \"p\", box: true}})\n}}\n"
    );
    sb.write("s.cig", &script);
    let out = sb.cig(&["run", "s.cig"]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{err}");
    if fell_back(&err) {
        allow_fallback(&err);
        return;
    }
    assert!(
        !err.contains("E712"),
        "the guard fell back where the box did not: {err}"
    );
    // The layer applied with the state directory read-only.
    assert_eq!(sb.read("p/made.txt").trim(), "made", "{err}");
    let rc = sb.read("p/rc.txt");
    assert!(
        !rc.contains("rc=0") && rc.contains("Read-only file system"),
        "the hop wrote into the tin: {rc}\n{err}"
    );
    assert!(!sb.home().join("tin-probe").exists());
    assert_eq!(
        sb.read("p/keys.txt").trim(),
        "0",
        "the key folder was not empty"
    );
    assert!(
        sb.read("p/inner-home.txt").contains("/inbox/"),
        "a cig inside the hop would not have the inbox as its state root: {}",
        sb.read("p/inner-home.txt")
    );
    // The inbox went with the hop.
    assert!(
        !sb.home().join("inbox").exists()
            || fs::read_dir(sb.home().join("inbox"))
                .unwrap()
                .next()
                .is_none(),
        "the inbox was left behind"
    );
}

/// A key file named outright (`CIG_SEAL_KEY_FILE`, the CI setup) lies
/// outside the key folder the guard hides: the hop is not told where it is,
/// and reading it there gives nothing.
#[cfg(target_os = "linux")]
#[test]
fn a_hop_cannot_read_a_key_file_named_outside_the_key_folder() {
    let sb = Sandbox::new();
    let key = sb.path().join("elsewhere").join("tin.key");
    fs::create_dir_all(key.parent().unwrap()).unwrap();
    sb.write("p/keep.txt", "v1\n");
    sb.write(
        "p/probe.sh",
        &format!(
            "printf 'bytes=%s\\n' \"$(cat {} 2>/dev/null | wc -c)\" > key.txt\nprintf 'var=%s\\n' \"${{CIG_SEAL_KEY_FILE:-unset}}\" >> key.txt\n",
            key.display()
        ),
    );
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"probe.sh\"], {cwd: \"p\", box: true})\n}\n",
    );
    let key_s = key.display().to_string();
    let out = sb.cig_env(&["run", "s.cig"], &[("CIG_SEAL_KEY_FILE", key_s.as_str())]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{err}");
    if fell_back(&err) {
        allow_fallback(&err);
        return;
    }
    // The run signed its journal with that key, so it is there, and real.
    assert!(
        fs::metadata(&key).map(|m| m.len() > 0).unwrap_or(false),
        "the key file was not made: {err}"
    );
    let seen = sb.read("p/key.txt");
    assert!(
        seen.contains("bytes=0") && seen.contains("var=unset"),
        "the hop reached the key file: {seen}\n{err}"
    );
}

/// While the key may be in the OS keyring (the Secret Service, on the
/// session bus), a guarded hop gets no session bus: the address is not
/// passed on and the bus and systemd's private socket are covered. With
/// `seal_key = file` the bus is left alone.
#[cfg(target_os = "linux")]
#[test]
fn a_hop_gets_no_session_bus_while_the_key_may_be_in_the_keyring() {
    let sb = Sandbox::new();
    // Stand-ins for the bus and systemd's private socket. cig's own keyring
    // lookup meets the same stand-in and falls back to a key file, so the
    // test never reaches a real keyring.
    sb.write("rt/bus", "");
    sb.write("rt/systemd/private", "");
    let rt = sb.path().join("rt").display().to_string();
    let addr = format!("unix:path={rt}/bus");
    sb.write("p/keep.txt", "v1\n");
    sb.write(
        "p/probe.sh",
        "b=open; [ -c \"$XDG_RUNTIME_DIR/bus\" ] && b=covered\np=open; [ -c \"$XDG_RUNTIME_DIR/systemd/private\" ] && p=covered\nprintf 'bus=%s private=%s addr=%s\\n' \"$b\" \"$p\" \"${DBUS_SESSION_BUS_ADDRESS:-unset}\" > bus.txt\n",
    );
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"probe.sh\"], {cwd: \"p\", box: true})\n}\n",
    );
    let envs = [
        ("XDG_RUNTIME_DIR", rt.as_str()),
        ("DBUS_SESSION_BUS_ADDRESS", addr.as_str()),
        ("CIG_SEAL_KEY", "keyring"),
    ];
    let out = sb.cig_env(&["run", "s.cig"], &envs);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{err}");
    if fell_back(&err) {
        allow_fallback(&err);
        return;
    }
    let seen = sb.read("p/bus.txt");
    assert!(
        seen.contains("bus=covered")
            && seen.contains("private=covered")
            && seen.contains("addr=unset"),
        "the hop kept a way to the keyring: {seen}\n{err}"
    );
    // The stand-ins themselves are untouched outside the hop.
    assert!(sb.path().join("rt/bus").is_file());
    // With the key in a file the guard hides, the bus is left to the hop.
    let envs = [
        ("XDG_RUNTIME_DIR", rt.as_str()),
        ("DBUS_SESSION_BUS_ADDRESS", addr.as_str()),
        ("CIG_SEAL_KEY", "file"),
    ];
    let out = sb.cig_env(&["run", "s.cig"], &envs);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{err}");
    let seen = sb.read("p/bus.txt");
    assert!(
        seen.contains("bus=open") && seen.contains(&format!("addr={addr}")),
        "the bus was cut although the key is a file: {seen}\n{err}"
    );
}

/// A cig started inside a boxed hop records its run in the inbox; the run
/// is taken in as a child, listed under its parent, and unburned with it.
#[cfg(target_os = "linux")]
#[test]
fn a_cig_inside_a_boxed_hop_is_a_child_run_unburned_with_its_parent() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    sb.write(
        "p/inner.cig",
        "burn {\n  fs.write_text(\"inner.txt\", \"from inside\")\n}\n",
    );
    let script = format!(
        "pack {{ \"./p\" }}\nburn {{\n  proc.run(\"{}\", [\"run\", \"inner.cig\"], {{cwd: \"p\", box: true}})\n}}\n",
        env!("CARGO_BIN_EXE_cig")
    );
    sb.write("outer.cig", &script);
    let out = sb.cig(&["run", "outer.cig"]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{err}");
    if fell_back(&err) {
        allow_fallback(&err);
        return;
    }
    assert_eq!(sb.read("p/inner.txt"), "from inside", "{err}");
    assert!(err.contains("child run "), "{err}");
    let runs = sb.runs();
    assert_eq!(runs.len(), 2, "{runs:?}");
    let child = runs
        .iter()
        .find(|r| r["parent"].is_string())
        .unwrap_or_else(|| panic!("no child run: {runs:?}"));
    let outer = runs.iter().find(|r| r["parent"].is_null()).unwrap();
    let child_id = child["id"].as_str().unwrap().to_string();
    let outer_id = outer["id"].as_str().unwrap().to_string();
    assert_eq!(child["parent"].as_str().unwrap(), outer_id);
    assert_eq!(child["status"], "ok");
    let shown = sb.run_ok(&["runs", &child_id]);
    assert!(
        shown.contains("parent") && shown.contains(&outer_id),
        "{shown}"
    );
    let shown = sb.run_ok(&["runs", &outer_id]);
    assert!(
        shown.contains("children") && shown.contains(&child_id),
        "{shown}"
    );
    // The child came in whole: its journal reads under this machine's key
    // and its objects are here.
    let shown = sb.run_ok(&["runs", &child_id, "--verify"]);
    assert!(shown.contains("intact"), "{shown}");
    // The parent's rollback undoes the child through the burn protocol.
    sb.run_ok(&["unburn", &outer_id]);
    assert!(
        !sb.exists("p/inner.txt"),
        "the child's change stayed after the parent's unburn"
    );
    assert_eq!(sb.read("p/keep.txt"), "v1\n");
    let runs = sb.runs();
    let child = runs.iter().find(|r| r["id"] == child_id.as_str()).unwrap();
    assert_eq!(child["status"], "unburned", "{child:?}");
}

/// A cig inside a boxed hop that writes outside the parent's pack: the
/// box's layer does not see it, and the child's own journal does; the
/// parent's rollback undoes it through the burn protocol, as before the
/// guard.
#[cfg(target_os = "linux")]
#[test]
fn a_cig_inside_a_hop_writing_outside_the_pack_is_undone_by_the_parents_rollback() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    fs::create_dir_all(sb.path().join("outside")).unwrap();
    sb.write(
        "p/inner.cig",
        "burn {\n  fs.write_text(\"../outside/flag.txt\", \"set\")\n}\n",
    );
    let script = format!(
        "pack {{ \"./p\" }}\nburn {{\n  proc.run(\"{}\", [\"run\", \"inner.cig\"], {{cwd: \"p\", box: true}})\n  cough \"boom\"\n}}\n",
        env!("CARGO_BIN_EXE_cig")
    );
    sb.write("outer.cig", &script);
    let out = sb.cig(&["run", "outer.cig"]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(1), "{err}");
    if fell_back(&err) {
        allow_fallback(&err);
        return;
    }
    assert!(err.contains("child run "), "{err}");
    assert!(
        !sb.exists("outside/flag.txt"),
        "the child's write outside the pack survived the parent's rollback: {err}"
    );
}

/// A cig inside a boxed hop that rewrites a secret: its sealed before-image
/// is re-sealed under this machine's key on the way in, so the child
/// unburns on its own.
#[cfg(target_os = "linux")]
#[test]
fn a_cig_inside_a_hop_that_seals_a_secret_unburns_on_its_own() {
    let sb = Sandbox::new();
    sb.write("p/.env", "SECRET=1\n");
    sb.write(
        "p/inner.cig",
        "burn {\n  fs.write_text(\".env\", \"SECRET=2\\n\")\n}\n",
    );
    let script = format!(
        "pack {{ \"./p\" }}\nburn {{\n  proc.run(\"{}\", [\"run\", \"inner.cig\"], {{cwd: \"p\", box: true}})\n}}\n",
        env!("CARGO_BIN_EXE_cig")
    );
    sb.write("outer.cig", &script);
    let out = sb.cig(&["run", "outer.cig"]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{err}");
    if fell_back(&err) {
        allow_fallback(&err);
        return;
    }
    assert_eq!(sb.read("p/.env"), "SECRET=2\n");
    let runs = sb.runs();
    let child = runs
        .iter()
        .find(|r| r["parent"].is_string())
        .unwrap_or_else(|| panic!("no child run: {runs:?}"));
    let child_id = child["id"].as_str().unwrap().to_string();
    let shown = sb.run_ok(&["runs", &child_id, "--verify"]);
    assert!(
        shown.contains("intact") && !shown.contains("BROKEN"),
        "{shown}"
    );
    // From the child's own directory, as any run with relative paths.
    let out = sb.cig_in("p", &["unburn", &child_id], &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(sb.read("p/.env"), "SECRET=1\n", "{err}");
}

/// Whatever a hop leaves in the inbox that is not a run is dropped, and
/// said so once per entry.
#[cfg(target_os = "linux")]
#[test]
fn an_inbox_entry_that_is_not_a_run_is_dropped_with_a_note() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"mkdir -p $CIGSCRIPT_HOME/runs/20990101T000000-abcdef $CIGSCRIPT_HOME/runs/not-a-run && echo junk > $CIGSCRIPT_HOME/runs/20990101T000000-abcdef/run.json\"], {cwd: \"p\", box: true})\n}\n",
    );
    let out = sb.cig(&["run", "s.cig"]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{err}");
    if fell_back(&err) {
        allow_fallback(&err);
        return;
    }
    assert_eq!(err.matches("note[E713 burn]").count(), 2, "{err}");
    assert!(
        err.contains("not a run id") && err.contains("run.json:"),
        "{err}"
    );
    assert_eq!(sb.runs().len(), 1, "a dropped entry was listed as a run");
    assert!(!sb.home().join("runs").join("not-a-run").exists());
}

/// `CIG_TIN_GUARD=0` is the guard off for one run: the proof that the
/// guard is what keeps a hop out of the tin.
#[cfg(target_os = "linux")]
#[test]
fn tin_guard_off_leaves_the_tin_writable_to_a_boxed_hop() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    let home = sb.home().display().to_string();
    let script = format!(
        "pack {{ \"./p\" }}\nburn {{\n  proc.run(\"sh\", [\"-c\", \"echo probe > {home}/tin-probe; echo $CIGSCRIPT_HOME > inner-home.txt\"], {{cwd: \"p\", box: true}})\n}}\n"
    );
    sb.write("s.cig", &script);
    let out = sb.cig_env(&["run", "s.cig"], &[("CIG_TIN_GUARD", "0")]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{err}");
    if fell_back(&err) {
        allow_fallback(&err);
        return;
    }
    assert!(sb.home().join("tin-probe").exists(), "{err}");
    assert_eq!(sb.read("p/inner-home.txt").trim(), home);
}

/// Where the guard cannot be made, `auto` says so once for the run and the
/// hops run boxed without it; `on` refuses the hop.
#[cfg(target_os = "linux")]
#[test]
fn the_guard_unavailable_is_one_note_under_auto_and_a_refusal_under_on() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo a > a.txt\"], {cwd: \"p\", box: true})\n  proc.run(\"sh\", [\"-c\", \"echo b > b.txt\"], {cwd: \"p\", box: true})\n}\n",
    );
    let out = sb.cig_env(&["run", "s.cig"], &[("CIG_INTERNAL_NO_GUARD", "1")]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{err}");
    if fell_back(&err) {
        allow_fallback(&err);
        return;
    }
    assert_eq!(err.matches("note[E712 burn]").count(), 1, "{err}");
    assert!(err.contains("CIG_INTERNAL_NO_GUARD"), "{err}");
    assert!(sb.exists("p/a.txt") && sb.exists("p/b.txt"), "{err}");
    sb.run_ok(&["config", "tin_guard", "on"]);
    let out = sb.cig_env(&["run", "s.cig"], &[("CIG_INTERNAL_NO_GUARD", "1")]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(
        err.contains("E712") && err.contains("tin_guard = on"),
        "{err}"
    );
    assert!(sb.cig(&["explain", "E712"]).status.success());
    assert!(sb.cig(&["explain", "E713"]).status.success());
    assert!(sb.cig(&["explain", "E714"]).status.success());
}

/// `tin_guard = on` guards a hop outside the box too: the same namespaces
/// without the overlay, the tin read-only, the key folder empty.
#[cfg(target_os = "linux")]
#[test]
fn tin_guard_on_guards_a_hop_outside_the_box() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    sb.run_ok(&["config", "tin_guard", "on"]);
    let home = sb.home().display().to_string();
    let script = format!(
        "pack {{ \"./p\" }}\nburn {{\n  proc.run(\"sh\", [\"-c\", \"{{ echo probe > {home}/tin-probe; }} 2>err.txt; echo rc=$? > rc.txt; cat err.txt >> rc.txt; echo $CIGSCRIPT_HOME > inner-home.txt\"], {{cwd: \"p\", touches: [\"**\"]}})\n}}\n"
    );
    sb.write("s.cig", &script);
    let out = sb.cig(&["run", "s.cig"]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if !out.status.success() {
        // Refused, as `on` must where the namespaces cannot be had.
        assert!(err.contains("E712"), "{err}");
        assert!(
            !box_expected_here(),
            "this machine makes user namespaces, yet the guard was refused: {err}"
        );
        return;
    }
    let rc = sb.read("p/rc.txt");
    assert!(
        !rc.contains("rc=0") && rc.contains("Read-only file system"),
        "an unboxed hop wrote into the tin under tin_guard = on: {rc}\n{err}"
    );
    assert!(!sb.home().join("tin-probe").exists());
    assert!(sb.read("p/inner-home.txt").contains("/inbox/"));
    assert!(!err.contains("E712"), "{err}");
}

/// Two state directories under one parent sit beside one key and so share
/// one ledger file. They are two tins: each line names the tin it
/// witnesses, so a run in one is not a missing run in the other. The
/// portability probe keeps every case's home under one parent and found
/// the note firing across them.
#[cfg(unix)]
#[test]
fn two_tins_beside_one_key_keep_separate_accounts() {
    let sb = Sandbox::new();
    let id = two_op_run(&sb);
    let other = sb.path().join("home2");
    let other = other.to_string_lossy().to_string();
    let out = sb.cig_env(&["run", "s.cig"], &[("CIGSCRIPT_HOME", &other)]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let ledger = sb.read(".cigscript-keys/ledger.jsonl");
    assert_eq!(ledger.lines().count(), 2, "{ledger}");
    assert!(ledger.contains("\"tin\":\""), "{ledger}");
    // Neither tin sees the other's run as missing.
    let err = String::from_utf8_lossy(&sb.cig(&["runs"]).stderr).to_string();
    assert!(!err.contains("E710"), "{err}");
    let out = sb.cig_env(&["runs"], &[("CIGSCRIPT_HOME", &other)]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!err.contains("E710"), "{err}");
    // A run really removed by hand is still noted, in its own tin only.
    fs::remove_dir_all(sb.home().join("runs").join(&id)).unwrap();
    let err = String::from_utf8_lossy(&sb.cig(&["runs"]).stderr).to_string();
    assert!(err.contains("E710") && err.contains(&id), "{err}");
    let out = sb.cig_env(&["runs"], &[("CIGSCRIPT_HOME", &other)]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!err.contains("E710"), "{err}");
}

// ----- the box for untrusted code (#106: the PID namespace, the network, kept layers) -----

/// A boxed hop's timeout fires: the box's relay and init hold no pipe of
/// the parent's, so `spawn` returns when the hop starts, not when it ends.
#[cfg(target_os = "linux")]
#[test]
fn a_boxed_hops_timeout_fires() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sleep\", [\"5\"], {cwd: \"p\", box: true, timeout_ms: 300})\n}\n",
    );
    let started = std::time::Instant::now();
    let out = sb.cig(&["run", "s.cig"]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.contains("E554") && err.contains("300 ms"), "{err}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(4),
        "the timeout did not fire: {:?}\n{err}",
        started.elapsed()
    );
}

/// Inside the box a hop is pid 2 under an init of cig's, with the
/// namespace's processes alone in `/proc`; `pidns: false` keeps the
/// machine's view.
#[cfg(target_os = "linux")]
#[test]
fn a_boxed_hop_cannot_see_cig() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    let view = "echo pid=$$ init=$(cat /proc/1/comm) procs=$(ls /proc | grep -c '^[0-9]') ppid=$PPID > VIEW";
    sb.write(
        "s.cig",
        &format!(
            "pack {{ \"./p\" }}\nburn {{\n  proc.run(\"sh\", [\"-c\", \"{}\"], {{cwd: \"p\", box: true}})\n  proc.run(\"sh\", [\"-c\", \"{}\"], {{cwd: \"p\", box: {{pidns: false}}}})\n}}\n",
            view.replace("VIEW", "view.txt"),
            view.replace("VIEW", "view2.txt")
        ),
    );
    let out = sb.cig(&["run", "s.cig"]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{err}");
    if fell_back(&err) {
        allow_fallback(&err);
        return;
    }
    let view = sb.read("p/view.txt");
    assert!(
        view.contains("pid=2 ") && view.contains("init=cig") && view.contains("ppid=1"),
        "{view}\n{err}"
    );
    let procs: usize = view
        .split("procs=")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(999);
    assert!(procs < 10, "{view}");
    let view2 = sb.read("p/view2.txt");
    assert!(!view2.contains("init=cig"), "{view2}");
}

/// A cig killed mid-hop takes the hop with it (the init dies with its
/// parent) and applies nothing; the next command drops the layer it never
/// applied, and the run's earlier burns are still unburnable.
#[cfg(target_os = "linux")]
#[test]
fn cig_dying_applies_nothing() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  fs.write_text(\"p/before.txt\", \"early\")\n  proc.run(\"sh\", [\"-c\", \"echo x > made.txt; exec sleep 31.7\"], {cwd: \"p\", box: true})\n}\n",
    );
    let sleeping = || {
        Command::new("pgrep")
            .args(["-f", "sleep 31.7"])
            .stdout(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    let mut cig = sb.cig_spawn(&["run", "s.cig"]);
    let mut up = false;
    for _ in 0..100 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if sleeping() {
            up = true;
            break;
        }
    }
    let _ = cig.kill();
    let out = cig.wait_with_output().expect("wait");
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(up, "the hop never started: {err}");
    let boxed = !fell_back(&err);
    if boxed {
        let mut gone = false;
        for _ in 0..30 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if !sleeping() {
                gone = true;
                break;
            }
        }
        assert!(gone, "the hop outlived cig");
    } else {
        allow_fallback(&err);
        let _ = Command::new("pkill").args(["-f", "sleep 31.7"]).status();
    }
    assert!(
        !boxed || !sb.exists("p/made.txt"),
        "the dead cig's layer reached the pack: {err}"
    );
    let out = sb.cig(&["runs"]);
    let listed = String::from_utf8_lossy(&out.stdout).to_string();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(listed.contains("interrupted"), "{listed}\n{err}");
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    if boxed {
        assert!(err.contains("died in a boxed hop"), "{err}");
        assert!(!sb.home().join("runs").join(&id).join("box").exists());
    }
    sb.run_ok(&["unburn", &id]);
    assert!(!sb.exists("p/before.txt"));
}

/// `network: "off"`: a namespace with `lo` alone and no resolver.
#[cfg(target_os = "linux")]
#[test]
fn a_hop_has_no_network_under_off() {
    if Command::new("python3").arg("--version").output().is_err() {
        eprintln!("no python3 here; skipped");
        return;
    }
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    sb.write(
        "p/probe.py",
        &format!(
            "import socket\ns = socket.socket()\ns.settimeout(3)\ns.connect(('127.0.0.1', {port}))\nprint('CONNECTED')\n"
        ),
    );
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"python3 probe.py > net.txt 2>&1; echo rc=$? >> net.txt; echo resolv=$(wc -c < /etc/resolv.conf) >> net.txt\"], {cwd: \"p\", box: {network: \"off\"}})\n}\n",
    );
    let out = sb.cig(&["run", "s.cig"]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if !out.status.success() {
        // Isolation asked for and no box here: refused, as it must be.
        assert!(err.contains("E777"), "{err}");
        assert!(!box_expected_here(), "{err}");
        return;
    }
    let net = sb.read("p/net.txt");
    assert!(
        !net.contains("CONNECTED") && net.contains("rc=1") && net.contains("resolv=0"),
        "{net}\n{err}"
    );
    drop(listener);
}

/// `network: "registry"`: the proxy lets `allow_hosts` through and
/// refuses the rest, each refusal recorded and said.
#[cfg(target_os = "linux")]
#[test]
fn a_registry_only_hop_reaches_the_registry_and_nothing_else() {
    if Command::new("python3").arg("--version").output().is_err() {
        eprintln!("no python3 here; skipped");
        return;
    }
    fn serve(name: &'static str) -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            for conn in listener.incoming() {
                let Ok(mut c) = conn else { break };
                let mut buf = [0u8; 4096];
                let _ = c.read(&mut buf);
                let body = format!("hello from {name}");
                let _ = c.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            }
        });
        port
    }
    let allowed = serve("the registry");
    let refused = serve("elsewhere");
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    sb.write(
        "p/probe.py",
        &format!(
            "import http.client\nfor port in ({allowed}, {refused}):\n    try:\n        c = http.client.HTTPConnection('127.0.0.1', 38080, timeout=5)\n        c.set_tunnel('127.0.0.1', port)\n        c.request('GET', '/')\n        r = c.getresponse()\n        print(port, r.status, r.read().decode())\n    except Exception as e:\n        print(port, 'ERR', e)\n"
        ),
    );
    sb.write(
        "s.cig",
        &format!(
            "pack {{ \"./p\" }}\nburn {{\n  proc.run(\"sh\", [\"-c\", \"python3 probe.py > reg.txt 2>&1\"], {{cwd: \"p\", box: {{network: \"registry\", allow_hosts: [\"127.0.0.1:{allowed}\"]}}}})\n}}\n"
        ),
    );
    let out = sb.cig(&["run", "s.cig"]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if !out.status.success() {
        assert!(err.contains("E777"), "{err}");
        assert!(!box_expected_here(), "{err}");
        return;
    }
    let reg = sb.read("p/reg.txt");
    assert!(
        reg.contains(&format!("{allowed} 200 hello from the registry")),
        "{reg}\n{err}"
    );
    assert!(
        reg.contains(&format!("{refused} ERR")) && reg.contains("403"),
        "{reg}"
    );
    assert!(
        err.contains("note[E771 burn]") && err.contains(&format!("127.0.0.1:{refused}")),
        "{err}"
    );
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let events = sb.read(&format!("home/runs/{id}/events/1.jsonl"));
    assert!(
        events.contains("\"result\":\"refused\"") && events.contains("\"result\":\"allowed\""),
        "{events}"
    );
}

/// Isolation asked for where the box cannot be made is a refusal, not a
/// fallback.
#[cfg(target_os = "linux")]
#[test]
fn no_box_means_no_untrusted_run() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo x > made.txt\"], {cwd: \"p\", box: {network: \"off\"}})\n}\n",
    );
    let out = sb.cig_env(&["run", "s.cig"], &[("CIG_INTERNAL_NO_BOX", "1")]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.contains("E777"), "{err}");
    assert!(!sb.exists("p/made.txt"));
    assert!(sb.cig(&["explain", "E777"]).status.success());
    assert!(sb.cig(&["explain", "E771"]).status.success());
    assert!(sb.cig(&["explain", "E780"]).status.success());
}

/// `apply: "never"` keeps the layer: the pack is untouched, the hop's
/// result names the layer, `cig box apply` puts it in as a run of its own,
/// and that run unburns.
#[cfg(target_os = "linux")]
#[test]
fn a_kept_layer_applies_later() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  stick r = proc.run(\"sh\", [\"-c\", \"echo new > made.txt; rm keep.txt\"], {cwd: \"p\", box: {apply: \"never\"}})\n  fs.write_text(\"p/layer.txt\", r.layer)\n}\n",
    );
    let out = sb.cig(&["run", "s.cig"]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if fell_back(&err) {
        allow_fallback(&err);
        return;
    }
    assert!(out.status.success(), "{err}");
    assert!(!sb.exists("p/made.txt") && sb.exists("p/keep.txt"), "{err}");
    assert!(err.contains("note[E780 burn]"), "{err}");
    let layer = sb.read("p/layer.txt");
    let (run, seq) = layer.trim().split_once('/').expect("run/seq");
    assert_eq!(run, sb.runs()[0]["id"].as_str().unwrap(), "{layer}");
    // The record is kept apart from the layer, where no hop may write.
    let run_dir = sb.home().join("runs").join(run);
    assert!(
        run_dir.join("kept").join(format!("{seq}.json")).is_file(),
        "no record under kept/"
    );
    assert!(!run_dir.join("box").join(seq).join("kept.json").exists());
    // Every folder under the run can be read: the overlay's work folder,
    // which overlayfs leaves unreadable, went with the hop.
    fn walk(d: &Path) {
        for e in fs::read_dir(d).unwrap_or_else(|e| panic!("{}: {e}", d.display())) {
            let p = e.unwrap().path();
            if fs::symlink_metadata(&p).unwrap().is_dir() {
                walk(&p);
            }
        }
    }
    walk(&run_dir);
    assert!(!run_dir
        .join("box")
        .join(seq)
        .join("0")
        .join("work")
        .exists());
    let list = sb.run_ok(&["box", "list"]);
    assert!(list.contains(run) && list.contains("rm keep.txt"), "{list}");
    let out = sb.run_ok(&["box", "apply", &run[..12], seq]);
    assert!(out.contains("applied the layer"), "{out}");
    assert_eq!(sb.read("p/made.txt"), "new\n");
    assert!(!sb.exists("p/keep.txt"));
    let runs = sb.runs();
    assert_eq!(runs.len(), 2, "{runs:?}");
    let applied = runs
        .iter()
        .find(|r| r["script"].as_str().unwrap_or("").starts_with("box apply"))
        .expect("the apply run");
    assert_eq!(applied["status"], "ok");
    let err = String::from_utf8_lossy(&sb.cig(&["box", "list"]).stderr).to_string();
    assert!(err.contains("no kept layers"), "{err}");
    sb.run_ok(&["unburn", applied["id"].as_str().unwrap()]);
    assert!(!sb.exists("p/made.txt") && sb.read("p/keep.txt") == "v1\n");
}

/// A layer record planted in the layers directory by the hop keeps
/// nothing: only cig's record, kept where no hop may write, does. The case
/// that matters is a cig that dies mid-hop: its unapplied layers are
/// dropped on sight unless they were kept on purpose.
#[cfg(target_os = "linux")]
#[test]
fn a_record_planted_beside_a_layer_keeps_nothing() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    let home = sb.home().display().to_string();
    sb.write(
        "p/plant.sh",
        &format!(
            "for d in {home}/runs/*/box/*; do printf '%s' '{{\"run\":\"x\",\"seq\":1,\"label\":\"planted\",\"roots\":[\"/\"],\"written\":0,\"removed\":0,\"at\":\"\"}}' > \"$d/kept.json\"; done\nfor d in {home}/runs/*; do mkdir -p \"$d/kept\"; done\necho x > made.txt\nexec sleep 31.9\n"
        ),
    );
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"plant.sh\"], {cwd: \"p\", box: true})\n}\n",
    );
    let sleeping = || {
        Command::new("pgrep")
            .args(["-f", "sleep 31.9"])
            .stdout(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    let mut cig = sb.cig_spawn(&["run", "s.cig"]);
    let mut up = false;
    for _ in 0..100 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if sleeping() {
            up = true;
            break;
        }
    }
    let _ = cig.kill();
    let out = cig.wait_with_output().expect("wait");
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(up, "the hop never started: {err}");
    if fell_back(&err) {
        allow_fallback(&err);
        let _ = Command::new("pkill").args(["-f", "sleep 31.9"]).status();
        return;
    }
    for _ in 0..30 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if !sleeping() {
            break;
        }
    }
    // The next look at the runs drops the dead cig's layers; a planted
    // record does not keep one.
    sb.cig(&["runs"]);
    let list = String::from_utf8_lossy(&sb.cig(&["box", "list"]).stderr).to_string();
    assert!(
        list.contains("no kept layers"),
        "a planted record kept a layer: {list}"
    );
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    assert!(!sb.home().join("runs").join(&id).join("box").exists());
}

/// A hop on the registry network cannot put a link in the place of the
/// proxy's socket, which the forwarder would follow to another socket:
/// the folder it sits in is read-only inside the box.
#[cfg(target_os = "linux")]
#[test]
fn the_proxy_socket_cannot_be_swapped_by_the_hop() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    let home = sb.home().display().to_string();
    sb.write(
        "p/swap.sh",
        &format!(
            "n=0\nfor s in {home}/runs/*/proxy/*.sock; do n=$((n+1)); {{ rm -f \"$s\" && ln -s /dev/null \"$s\"; }} 2>>err.txt; done\necho \"sockets=$n\" > swap.txt\n"
        ),
    );
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"swap.sh\"], {cwd: \"p\", box: {network: \"registry\", allow_hosts: [\"registry.npmjs.org\"]}})\n}\n",
    );
    let out = sb.cig(&["run", "s.cig"]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if err.contains("E777") && !box_expected_here() {
        return;
    }
    assert!(out.status.success(), "{err}");
    assert!(
        sb.read("p/swap.txt").contains("sockets=1"),
        "the hop did not find the socket: {}\n{err}",
        sb.read("p/swap.txt")
    );
    let tried = sb.read("p/err.txt");
    assert!(
        tried.contains("Read-only file system"),
        "the hop could replace the proxy's socket: {tried}\n{err}"
    );
    // With the guard off the state directory is the hop's to write, and the
    // socket's folder is still read-only to it.
    fs::remove_file(sb.path().join("p/err.txt")).unwrap();
    let out = sb.cig_env(&["run", "s.cig"], &[("CIG_TIN_GUARD", "0")]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{err}");
    let tried = sb.read("p/err.txt");
    assert!(
        tried.contains("Read-only file system"),
        "without the guard the hop could replace the proxy's socket: {tried}\n{err}"
    );
}

/// A hop asked to be isolated (a network of its own) gets no burn
/// protocol: an undo it declared would be a command cig runs later,
/// outside the box. The unburn runs nothing of the hop's.
#[cfg(target_os = "linux")]
#[test]
fn an_isolated_hop_has_no_burn_protocol() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    let marker = sb.path().join("compensation-ran");
    sb.write(
        "p/claim.sh",
        &format!(
            "printf 'journal=%s\\n' \"${{CIG_JOURNAL:-unset}}\" > claim.txt\nprintf '%s\\n' '{{\"op\":\"compensate\",\"label\":\"x\",\"undo\":[\"touch\",\"{}\"]}}' >> \"${{CIG_JOURNAL:-/dev/null}}\"\n",
            marker.display()
        ),
    );
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"claim.sh\"], {cwd: \"p\", box: {network: \"off\"}})\n}\n",
    );
    let out = sb.cig(&["run", "s.cig"]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if err.contains("E777") && !box_expected_here() {
        return;
    }
    assert!(out.status.success(), "{err}");
    assert!(
        sb.read("p/claim.txt").contains("journal=unset"),
        "{}",
        sb.read("p/claim.txt")
    );
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    sb.run_ok(&["unburn", &id]);
    assert!(
        !marker.exists(),
        "the unburn ran an undo the isolated hop declared"
    );
}

/// `contain`: everything the hop can write is a layer cig reads afterwards.
/// A home file, a git hook and a temp file all land in layers; a write
/// anywhere else is refused; what the hop was not expected to write is
/// reported; and the real home, project and /var/tmp are untouched.
#[cfg(target_os = "linux")]
#[test]
fn a_contained_hop_writes_only_its_layers() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    fs::create_dir_all(sb.path().join("p/.git/hooks")).unwrap();
    let tag = format!("cig-contain-test-{}", std::process::id());
    sb.write(
        "p/probe.sh",
        &format!(
            "echo evil >> \"$HOME/.{tag}-rc\"\necho hook > .git/hooks/pre-commit\nmkdir -p node_modules/x && echo m > node_modules/x/index.js\necho t > \"$TMPDIR/{tag}\" && echo temp=ok >> probe.txt\n{{ echo v > /var/tmp/{tag}; }} 2>/dev/null && echo var=written >> probe.txt || echo var=refused >> probe.txt\n"
        ),
    );
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  stick r = proc.run(\"sh\", [\"probe.sh\"], {cwd: \"p\", box: {network: \"off\", contain: true, apply: \"never\", expect: [\"node_modules/**\", \"probe.txt\"]}})\n  stick seen = r.outside.map(pack(u) => u.path + \" \" + u.kind).join(\"\\n\")\n  fs.write_text(\"p/outside.txt\", seen)\n  fs.write_text(\"p/layer.txt\", r.layer)\n}\n",
    );
    let Some((out, err)) = run_contained(&sb, "s.cig") else {
        return;
    };
    assert!(out.status.success(), "{err}");
    assert!(err.contains("note[E781 burn]"), "{err}");
    let outside = sb.read("p/outside.txt");
    assert!(outside.contains(".git/hooks/pre-commit write"), "{outside}");
    assert!(outside.contains(&format!("~/.{tag}-rc write")), "{outside}");
    assert!(!outside.contains("node_modules"), "{outside}");
    // Nothing reached the real world.
    let home = PathBuf::from(std::env::var("HOME").unwrap());
    assert!(!home.join(format!(".{tag}-rc")).exists());
    assert!(!sb.exists("p/.git/hooks/pre-commit"));
    assert!(!sb.exists("p/node_modules"));
    assert!(!Path::new(&format!("/var/tmp/{tag}")).exists());
    // The layer is kept, the hop's own temp worked, /var/tmp did not.
    let list = sb.run_ok(&["box", "list"]);
    assert!(list.contains("sh probe.sh"), "{list}");
    let layer = sb.read("p/layer.txt");
    let (run, seq) = layer.trim().split_once('/').unwrap();
    let shown = sb.run_ok(&["box", "discard", run, seq]);
    assert!(shown.contains("discarded"), "{shown}");
    assert!(
        !fs::read_dir("/var/tmp")
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains(run)),
        "the box's scratch folder outlived its layer"
    );
}

/// A contained hop opens no Unix socket (a session bus, systemd's manager,
/// docker.sock, an agent), while socketpair, which child pipes use, works;
/// its /dev holds the plain devices alone; the key folder is empty and the
/// tin read-only.
#[cfg(target_os = "linux")]
#[test]
fn a_contained_hop_reaches_no_socket_device_key_or_tin() {
    if Command::new("python3").arg("--version").output().is_err() {
        eprintln!("no python3 here; skipped");
        return;
    }
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    let home = sb.home().display().to_string();
    let keys = sb.path().join(".cigscript-keys").display().to_string();
    sb.write(
        "p/probe.py",
        "import socket\ntry:\n    socket.socket(socket.AF_UNIX)\n    print('unix=open')\nexcept OSError as e:\n    print('unix=refused', e.errno)\na, b = socket.socketpair()\nprint('pair=ok')\n",
    );
    sb.write(
        "p/probe.sh",
        &format!(
            "python3 probe.py > probe.txt 2>&1\necho \"dev=$(ls /dev | tr '\\n' ' ')\" >> probe.txt\necho \"keys=$(ls -A {keys} 2>/dev/null | wc -l)\" >> probe.txt\n{{ echo x > {home}/probe; }} 2>/dev/null && echo tin=written >> probe.txt || echo tin=refused >> probe.txt\n"
        ),
    );
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"probe.sh\"], {cwd: \"p\", box: {network: \"off\", contain: true, expect: [\"probe.txt\"]}})\n}\n",
    );
    let Some((out, err)) = run_contained(&sb, "s.cig") else {
        return;
    };
    assert!(out.status.success(), "{err}");
    let seen = sb.read("p/probe.txt");
    assert!(seen.contains("unix=refused 13"), "{seen}");
    assert!(seen.contains("pair=ok"), "{seen}");
    assert!(seen.contains("keys=0"), "{seen}");
    assert!(seen.contains("tin=refused"), "{seen}");
    let dev = seen.lines().find(|l| l.starts_with("dev=")).unwrap();
    for name in dev.trim_start_matches("dev=").split_whitespace() {
        assert!(
            [
                "fd", "full", "null", "ptmx", "pts", "random", "shm", "stderr", "stdin", "stdout",
                "tty", "urandom", "zero"
            ]
            .contains(&name),
            "the hop's /dev holds {name}: {dev}"
        );
    }
}

/// A contained box whose setup is refused says why (E777), never "not
/// found" (E551): with HOME=/ (a service account, a container run as any
/// user) the box has no scratch folder outside the home, and as root
/// `contain` is refused outright. Either way the hop does not run.
#[cfg(target_os = "linux")]
#[test]
fn a_box_that_cannot_be_set_up_says_why() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo ran > ran.txt\"], {cwd: \"p\", box: {network: \"off\", contain: true, apply: \"never\"}})\n}\n",
    );
    let out = sb.cig_env(&["run", "s.cig"], &[("HOME", "/")]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success(), "{err}");
    assert!(err.contains("E777"), "{err}");
    assert!(!err.contains("E551"), "{err}");
    assert!(!err.contains("no such file"), "{err}");
    assert!(!sb.exists("p/ran.txt"), "{err}");
    if box_expected_here() {
        // The box is made here, so its setup refused, and the reason is told.
        assert!(
            err.contains("scratch folder") || err.contains("not as root"),
            "{err}"
        );
    }
}

/// With `contain` and the default `apply`, a layer that stays inside its
/// footprint is applied; one that leaves it is held back, the pack untouched.
#[cfg(target_os = "linux")]
#[test]
fn a_contained_hop_off_its_footprint_is_held_back() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo a > made.txt\"], {cwd: \"p\", box: {network: \"off\", contain: true, expect: [\"made.txt\"]}})\n  proc.run(\"sh\", [\"-c\", \"echo b > stray.txt\"], {cwd: \"p\", box: {network: \"off\", contain: true, expect: [\"made.txt\"]}})\n}\n",
    );
    let Some((out, err)) = run_contained(&sb, "s.cig") else {
        return;
    };
    assert!(out.status.success(), "{err}");
    assert_eq!(sb.read("p/made.txt"), "a\n", "{err}");
    assert!(!sb.exists("p/stray.txt"), "{err}");
    assert!(
        err.contains("note[E781 burn]") && err.contains("stray.txt"),
        "{err}"
    );
    assert!(err.contains("held back"), "{err}");
}

/// A box with no pack to lay over: an isolated hop is refused before it
/// runs (never run unprotected), and a plain box says it runs on the tin.
#[test]
fn a_box_without_a_pack_is_refused_when_isolated_and_said_when_plain() {
    let sb = Sandbox::new();
    sb.write(
        "iso.cig",
        "burn {\n  proc.run(\"sh\", [\"-c\", \"echo x > iso.txt\"], {box: {network: \"off\"}})\n}\n",
    );
    let out = sb.cig(&["run", "iso.cig"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    assert!(
        err.contains("E777") && err.contains("declares no pack"),
        "{err}"
    );
    assert!(!sb.exists("iso.txt"), "an isolated hop ran without its box");
    assert!(!err.contains("cannot undo run"), "{err}");
    sb.write(
        "plain.cig",
        "burn {\n  proc.run(\"sh\", [\"-c\", \"echo x > plain.txt\"], {box: true})\n}\n",
    );
    let out = sb.cig(&["run", "plain.cig"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(
        err.contains("warning[E763 burn]") && err.contains("declares no pack"),
        "{err}"
    );
    assert_eq!(sb.read("plain.txt"), "x\n");
}

/// A contained hop's home writes are never applied with its layer, unless
/// the script names them: `proc.apply_layer(layer, {home: [...]})` places
/// what the globs name (outside the pack, so not journaled) and leaves the
/// rest, and a home layer changed after it was judged applies nothing.
#[cfg(target_os = "linux")]
#[test]
fn a_contained_hops_home_writes_apply_only_by_name_and_only_as_judged() {
    let sb = Sandbox::new();
    let home = sb.path().join("h");
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join(".bashrc"), "orig\n").unwrap();
    fs::create_dir_all(sb.path().join("p")).unwrap();
    let hop = "mkdir -p \\\"$HOME/.cache/tool\\\" && echo c > \\\"$HOME/.cache/tool/a.txt\\\"; echo x >> \\\"$HOME/.bashrc\\\"; echo made > made.txt";
    sb.write(
        "s.cig",
        &format!("pack {{ \"./p\" }}\nburn {{\n  stick r = proc.run(\"sh\", [\"-c\", \"{hop}\"], {{cwd: \"p\", box: {{network: \"off\", contain: true, apply: \"never\", quiet: true, expect: [\"made.txt\"]}}}})\n  stick done = proc.apply_layer(r.layer, {{home: [\"~/.cache/**\"]}})\n  fs.write_text(\"p/home.txt\", done.home.join(\",\"))\n}}\n"),
    );
    let out = sb.cig_env(&["run", "s.cig"], &[("HOME", home.to_str().unwrap())]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if !contain_expected_here() {
        assert!(err.contains("E777") || out.status.success(), "{err}");
        return;
    }
    assert!(out.status.success(), "{err}");
    assert_eq!(sb.read("p/made.txt"), "made\n", "{err}");
    assert_eq!(
        fs::read_to_string(home.join(".cache/tool/a.txt")).unwrap(),
        "c\n",
        "{err}"
    );
    assert_eq!(
        fs::read_to_string(home.join(".bashrc")).unwrap(),
        "orig\n",
        "{err}"
    );
    assert!(sb.read("p/home.txt").contains(".cache/tool/a.txt"), "{err}");
    // Changed after it was judged: nothing of the home is applied.
    fs::remove_dir_all(home.join(".cache")).unwrap();
    sb.write(
        "t.cig",
        &format!("pack {{ \"./p\" }}\nburn {{\n  stick r = proc.run(\"sh\", [\"-c\", \"{hop}\"], {{cwd: \"p\", box: {{network: \"off\", contain: true, apply: \"never\", quiet: true, expect: [\"made.txt\"]}}}})\n  stick run = cig.run_id()\n  proc.run(\"sh\", [\"-c\", \"for f in /var/tmp/cig-box-*-\" + run + \"-*/home-upper/.cache/tool/a.txt /tmp/cig-box-*-\" + run + \"-*/home-upper/.cache/tool/a.txt; do [ -f \\\"$f\\\" ] && echo tampered > \\\"$f\\\"; done; true\"])\n  try {{\n    proc.apply_layer(r.layer, {{home: [\"~/.cache/**\"]}})\n  }} ashtray e {{\n    fs.write_text(\"p/refused.txt\", e.message)\n  }}\n}}\n"),
    );
    let out = sb.cig_env(&["run", "t.cig"], &[("HOME", home.to_str().unwrap())]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        sb.read("p/refused.txt")
            .contains("changed since they were judged"),
        "{err}"
    );
    assert!(!home.join(".cache/tool/a.txt").exists(), "{err}");
}

/// `quiet: true`: a script that reads the layer and says what it means
/// itself gets the same result, without cig's own lines about the layer.
#[cfg(target_os = "linux")]
#[test]
fn a_quiet_box_leaves_its_layer_to_the_script() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  stick a = proc.run(\"sh\", [\"-c\", \"echo a > made.txt\"], {cwd: \"p\", box: {network: \"off\", contain: true, quiet: true, expect: [\"made.txt\"]}})\n  stick b = proc.run(\"sh\", [\"-c\", \"echo b > stray.txt\"], {cwd: \"p\", box: {network: \"off\", contain: true, quiet: true, apply: \"never\", expect: [\"made.txt\"]}})\n  fs.write_text(\"p/said.txt\", str(b.layer != null) + \" \" + b.outside.map(pack(u) => u.path).join(\",\"))\n  proc.discard_layer(b.layer)\n}\n",
    );
    let Some((out, err)) = run_contained(&sb, "s.cig") else {
        return;
    };
    assert!(out.status.success(), "{err}");
    assert_eq!(sb.read("p/made.txt"), "a\n", "{err}");
    assert!(!sb.exists("p/stray.txt"), "{err}");
    assert!(sb.read("p/said.txt").starts_with("true "), "{err}");
    assert!(sb.read("p/said.txt").contains("stray.txt"), "{err}");
    assert!(!err.contains("E780") && !err.contains("E781"), "{err}");
    assert!(!err.contains("box: `"), "{err}");
}

/// A rehearsal (`apply: "never"`) applies in the same run with
/// `proc.apply_layer`, journaled, so the run's unburn takes it back out; a
/// layer changed after it was judged is not applied (E782).
#[cfg(target_os = "linux")]
#[test]
fn a_rehearsed_layer_applies_in_the_same_run_and_only_as_judged() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  stick r = proc.run(\"sh\", [\"-c\", \"echo new > made.txt; echo v2 > keep.txt\"], {cwd: \"p\", box: {network: \"off\", contain: true, apply: \"never\", expect: [\"made.txt\", \"keep.txt\"]}})\n  stick done = proc.apply_layer(r.layer)\n  fs.write_text(\"p/applied.txt\", str(done.written))\n}\n",
    );
    let Some((out, err)) = run_contained(&sb, "s.cig") else {
        return;
    };
    assert!(out.status.success(), "{err}");
    assert_eq!(sb.read("p/made.txt"), "new\n", "{err}");
    assert_eq!(sb.read("p/keep.txt"), "v2\n");
    assert_eq!(sb.read("p/applied.txt"), "2");
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    sb.run_ok(&["unburn", &id]);
    assert!(!sb.exists("p/made.txt") && sb.read("p/keep.txt") == "v1\n");

    // A layer someone changed between the judgement and the apply.
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    let home = sb.home().display().to_string();
    sb.write(
        "s.cig",
        &format!(
            "pack {{ \"./p\" }}\nburn {{\n  stick r = proc.run(\"sh\", [\"-c\", \"echo new > made.txt\"], {{cwd: \"p\", box: {{network: \"off\", contain: true, apply: \"never\", expect: [\"made.txt\"]}}}})\n  stick up = \"{home}/runs/\" + r.layer.replace(\"/\", \"/box/\") + \"/0/upper/made.txt\"\n  proc.run(\"sh\", [\"-c\", \"echo tampered >> \" + up])\n  try {{\n    proc.apply_layer(r.layer)\n  }} ashtray e {{\n    fs.write_text(\"p/refused.txt\", e.message)\n  }}\n}}\n"
        ),
    );
    let Some((out, err)) = run_contained(&sb, "s.cig") else {
        return;
    };
    assert!(out.status.success(), "{err}");
    assert!(
        sb.read("p/refused.txt")
            .contains("changed since it was judged"),
        "{}",
        sb.read("p/refused.txt")
    );
    assert!(!sb.exists("p/made.txt"));
}

/// A contained hop that fails keeps its layer, so what it did can be read:
/// the error's value carries the layer and what it changed off its footprint.
#[cfg(target_os = "linux")]
#[test]
fn a_contained_hop_that_fails_keeps_its_layer_as_evidence() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  try {\n    proc.run(\"sh\", [\"-c\", \"echo x > stray.txt; exit 3\"], {cwd: \"p\", ok: [0], box: {network: \"off\", contain: true, expect: []}})\n  } ashtray e {\n    fs.write_text(\"p/evidence.txt\", str(e.get(\"layer\", \"none\")) + \" \" + e.get(\"outside\", []).map(pack(u) => u.path).join(\",\"))\n  }\n}\n",
    );
    let Some((out, err)) = run_contained(&sb, "s.cig") else {
        return;
    };
    assert!(out.status.success(), "{err}");
    let ev = sb.read("p/evidence.txt");
    assert!(ev.contains("/") && ev.contains("stray.txt"), "{ev}\n{err}");
    assert!(!sb.exists("p/stray.txt"));
    let list = sb.run_ok(&["box", "list"]);
    assert!(list.contains("exit 3"), "{list}");
}

/// `proc.shell` gives back what `proc.run` does for a boxed hop: the layer
/// it kept and its judgement.
#[cfg(target_os = "linux")]
#[test]
fn a_boxed_shell_hop_returns_its_layer_and_judgement() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  stick r = proc.shell(\"echo x > stray.txt\", {cwd: \"p\", box: {apply: \"never\", expect: [\"made.txt\"]}})\n  fs.write_text(\"p/shell.txt\", str(r.layer != null) + \" \" + r.outside.map(pack(u) => u.path).join(\",\"))\n}\n",
    );
    let out = sb.cig(&["run", "s.cig"]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if fell_back(&err) {
        allow_fallback(&err);
        return;
    }
    assert!(out.status.success(), "{err}");
    assert_eq!(sb.read("p/shell.txt"), "true stray.txt", "{err}");
}

/// A contained hop's way out works when cig itself lives under /tmp, which
/// the box's own temp covers: the forwarder is started through a handle
/// held before the mounts, not by its path (found rehearsing npm with a cig
/// built under /tmp: the hop had no network and npm fell back to a stale
/// cache after a minute of retries).
#[cfg(target_os = "linux")]
#[test]
fn a_contained_hops_network_works_with_cig_under_tmp() {
    if !contain_expected_here() || Command::new("python3").arg("--version").output().is_err() {
        eprintln!("contain or python3 not here; skipped");
        return;
    }
    let bin = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    if !bin.path().starts_with("/tmp") || proj.path().starts_with("/tmp") {
        eprintln!("needs the system temp at /tmp and the build tree elsewhere; skipped");
        return;
    }
    let cig = bin.path().join("cig");
    fs::copy(env!("CARGO_BIN_EXE_cig"), &cig).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for conn in listener.incoming() {
            let Ok(mut c) = conn else { break };
            let mut buf = [0u8; 4096];
            let _ = c.read(&mut buf);
            let body = "hello from the registry";
            let _ = c.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
        }
    });
    let root = proj.path();
    fs::create_dir_all(root.join("p")).unwrap();
    fs::write(root.join("p/keep.txt"), "v1\n").unwrap();
    fs::write(
        root.join("p/probe.py"),
        format!("import http.client\nc = http.client.HTTPConnection('127.0.0.1', 38080, timeout=5)\nc.set_tunnel('127.0.0.1', {port})\nc.request('GET', '/')\nr = c.getresponse()\nopen('reg.txt', 'w').write(str(r.status) + ' ' + r.read().decode())\n"),
    )
    .unwrap();
    fs::write(
        root.join("s.cig"),
        format!("pack {{ \"./p\" }}\nburn {{\n  proc.run(\"python3\", [\"probe.py\"], {{cwd: \"p\", box: {{network: \"registry\", allow_hosts: [\"127.0.0.1:{port}\"], contain: true, expect: [\"reg.txt\"]}}}})\n}}\n"),
    )
    .unwrap();
    let out = Command::new(&cig)
        .args(["run", "s.cig"])
        .current_dir(root)
        .env("CIGSCRIPT_HOME", root.join("home"))
        .env("NO_COLOR", "1")
        .env("CIG_SEAL_KEY", "file")
        .env("CIG_UPDATE_MANIFEST", root.join("no-releases.json"))
        .env_remove("CI")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{err}");
    let got = fs::read_to_string(root.join("p/reg.txt")).unwrap_or_default();
    assert_eq!(got, "200 hello from the registry", "{err}");
}

/// A hop that connects to the registry network at once is queued for the
/// forwarder, never refused, however slowly the forwarder starts: the box
/// makes the forwarder's listener before the hop exists. (A compiled
/// program's forwarder reads its own payload first; cig-orchestrator's
/// agent box lost that race now and then.)
#[cfg(target_os = "linux")]
#[test]
fn a_boxed_hop_that_connects_at_once_reaches_a_slow_forwarder() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"bash\", [\"-c\", \"if exec 3<>/dev/tcp/127.0.0.1/38080; then echo connected; else echo refused; fi > got.txt\"], {cwd: \"p\", box: {network: \"registry\", allow_hosts: [\"registry.npmjs.org\"]}})\n}\n",
    );
    let out = sb.cig_env(
        &["run", "s.cig"],
        &[("CIG_INTERNAL_FORWARD_DELAY_MS", "1500")],
    );
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if err.contains("E777") && !box_expected_here() {
        return;
    }
    assert!(out.status.success(), "{err}");
    assert_eq!(sb.read("p/got.txt").trim(), "connected", "{err}");
}

/// A hop in a box with a network of its own sees cig's proxy alone: the
/// outer environment's proxies and exceptions lead nowhere from inside (#112).
#[cfg(target_os = "linux")]
#[test]
fn a_boxed_hop_sees_only_cigs_proxy() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"env | grep -i -E '^[^=]*(proxy|global_agent)[^=]*=' | sort > reg.txt\"], {cwd: \"p\", box: {network: \"registry\", allow_hosts: [\"registry.npmjs.org\"]}})\n  proc.run(\"sh\", [\"-c\", \"env | grep -i -E '^[^=]*(proxy|global_agent)[^=]*=' | sort > off.txt; true\"], {cwd: \"p\", box: {network: \"off\"}})\n}\n",
    );
    let out = sb.cig_env(
        &["run", "s.cig"],
        &[
            ("GLOBAL_AGENT_HTTPS_PROXY", "http://127.0.0.1:41005"),
            ("HTTPS_PROXY", "http://outer.example:3128"),
            ("no_proxy", "intranet.example"),
            ("YARN_HTTPS_PROXY", "http://outer.example:3128"),
            ("GIT_PROXY_COMMAND", "outer-proxy"),
        ],
    );
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if err.contains("E777") && !box_expected_here() {
        return;
    }
    assert!(out.status.success(), "{err}");
    let reg = sb.read("p/reg.txt");
    for line in reg.lines() {
        let (k, v) = line.split_once('=').unwrap();
        assert!(
            v == "http://127.0.0.1:38080" || (v.is_empty() && k.eq_ignore_ascii_case("no_proxy")),
            "the registry box kept an outer proxy variable: {line}\n{reg}"
        );
    }
    assert!(reg.contains("HTTPS_PROXY=http://127.0.0.1:38080"), "{reg}");
    assert!(
        !reg.contains("GLOBAL_AGENT") && !reg.contains("outer"),
        "{reg}"
    );
    let off = sb.read("p/off.txt");
    assert!(
        off.trim().is_empty(),
        "the network-off box kept proxy variables: {off}"
    );
}

/// `contain` asks for a network of the hop's own and its own PID namespace:
/// the machine's network would reach services on this machine.
#[cfg(target_os = "linux")]
#[test]
fn contain_needs_its_own_network_and_pid_namespace() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    sb.write(
        "a.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"true\", [], {cwd: \"p\", box: {contain: true}})\n}\n",
    );
    let err = String::from_utf8_lossy(&sb.cig(&["run", "a.cig"]).stderr).to_string();
    assert!(err.contains("box.contain needs network"), "{err}");
    sb.write(
        "b.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"true\", [], {cwd: \"p\", box: {contain: true, network: \"off\", pidns: false}})\n}\n",
    );
    let err = String::from_utf8_lossy(&sb.cig(&["run", "b.cig"]).stderr).to_string();
    assert!(err.contains("box.contain needs pidns"), "{err}");
}

/// A cig inside a contained hop keeps a throwaway state in the box's temp:
/// nothing of it is taken in as a child run.
#[cfg(target_os = "linux")]
#[test]
fn a_cig_inside_a_contained_hop_is_not_taken_in() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    sb.write(
        "p/inner.cig",
        "burn {\n  fs.write_text(\"inner.txt\", \"from inside\")\n}\n",
    );
    let cig = env!("CARGO_BIN_EXE_cig");
    sb.write(
        "s.cig",
        &format!(
            "pack {{ \"./p\" }}\nburn {{\n  proc.run(\"{cig}\", [\"run\", \"inner.cig\"], {{cwd: \"p\", box: {{network: \"off\", contain: true, expect: [\"inner.txt\"]}}}})\n}}\n"
        ),
    );
    let Some((out, err)) = run_contained(&sb, "s.cig") else {
        return;
    };
    assert!(out.status.success(), "{err}");
    assert_eq!(sb.read("p/inner.txt"), "from inside", "{err}");
    let runs = sb.runs();
    assert_eq!(runs.len(), 1, "a child run was taken in: {runs:?}");
}

/// A kept layer waits for `cig box apply` across other runs: the sweep a
/// `cig run` makes of dead runs' layers takes only those never applied.
#[cfg(target_os = "linux")]
#[test]
fn a_kept_layer_survives_another_run() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    sb.write(
        "keep.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo new > made.txt\"], {cwd: \"p\", box: {apply: \"never\"}})\n}\n",
    );
    sb.write("other.cig", "burn {\n  exhale \"another run\"\n}\n");
    let out = sb.cig(&["run", "keep.cig"]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if fell_back(&err) {
        allow_fallback(&err);
        return;
    }
    assert!(out.status.success(), "{err}");
    sb.run_ok(&["run", "other.cig"]);
    let list = sb.run_ok(&["box", "list"]);
    assert!(
        list.contains("echo new > made.txt"),
        "another run swept the kept layer: {list}"
    );
}

/// A kept layer goes with `cig box discard`, and with an unburn of the run
/// it belongs to (the base it was made against is gone).
#[cfg(target_os = "linux")]
#[test]
fn a_kept_layer_is_discarded_by_hand_or_with_its_run() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  fs.write_text(\"p/early.txt\", \"early\")\n  stick r = proc.run(\"sh\", [\"-c\", \"echo new > made.txt\"], {cwd: \"p\", box: {apply: \"never\"}})\n  fs.write_text(\"p/layer.txt\", r.layer)\n}\n",
    );
    let out = sb.cig(&["run", "s.cig"]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if fell_back(&err) {
        allow_fallback(&err);
        return;
    }
    assert!(out.status.success(), "{err}");
    let layer = sb.read("p/layer.txt");
    let (run, seq) = layer.trim().split_once('/').expect("run/seq");
    sb.run_ok(&["box", "discard", run, seq]);
    let err = String::from_utf8_lossy(&sb.cig(&["box", "list"]).stderr).to_string();
    assert!(err.contains("no kept layers"), "{err}");
    // Again, and this time the unburn takes it.
    let out = sb.cig(&["run", "s.cig"]);
    assert!(out.status.success());
    let layer = sb.read("p/layer.txt");
    let run = layer.trim().split_once('/').unwrap().0.to_string();
    assert!(sb.run_ok(&["box", "list"]).contains(&run));
    sb.run_ok(&["unburn", &run]);
    let err = String::from_utf8_lossy(&sb.cig(&["box", "list"]).stderr).to_string();
    assert!(err.contains("no kept layers"), "{err}");
    assert!(!sb.exists("p/made.txt"));
}

/// `cig tin mac`: a keyed MAC under a label of the caller's, the same for
/// the same bytes and label, different under another label, checkable; and
/// not makeable under the tin's key from inside a boxed hop, whose key
/// folder is empty (it gets a key of its own in the inbox, which verifies
/// nowhere else).
#[cfg(unix)]
#[test]
fn tin_mac_signs_under_the_tins_key_and_a_hop_cannot() {
    let sb = Sandbox::new();
    sb.write("p/rec.json", "{\"task\":\"setup\"}\n");
    let a = sb.run_ok(&["tin", "mac", "p/rec.json", "--label", "cigo-trust"]);
    assert_eq!(a.trim().len(), 64, "{a}");
    let b = sb.run_ok(&["tin", "mac", "p/rec.json", "--label", "cigo-trust"]);
    assert_eq!(a, b);
    let c = sb.run_ok(&["tin", "mac", "p/rec.json", "--label", "other"]);
    assert_ne!(a, c);
    let ok = sb.run_ok(&[
        "tin",
        "mac",
        "p/rec.json",
        "--label",
        "cigo-trust",
        "--check",
        a.trim(),
    ]);
    assert!(ok.contains("matches"), "{ok}");
    sb.write("p/rec.json", "{\"task\":\"setup\",\"approved\":true}\n");
    let err = sb.run_err(
        &[
            "tin",
            "mac",
            "p/rec.json",
            "--label",
            "cigo-trust",
            "--check",
            a.trim(),
        ],
        1,
    );
    assert!(err.contains("does not match"), "{err}");
    let err = sb.run_err(&["tin", "mac", "p/rec.json", "--label", "Bad Label"], 3);
    assert!(err.contains("E803"), "{err}");
    #[cfg(target_os = "linux")]
    {
        let script = format!(
            "pack {{ \"./p\" }}\nburn {{\n  proc.run(\"sh\", [\"-c\", \"{} tin mac rec.json --label cigo-trust > inside.txt 2>/dev/null; echo rc=$? >> inside.txt\"], {{cwd: \"p\", box: true}})\n}}\n",
            env!("CARGO_BIN_EXE_cig")
        );
        sb.write("s.cig", &script);
        let out = sb.cig(&["run", "s.cig"]);
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(out.status.success(), "{err}");
        if !fell_back(&err) {
            let inside = sb.read("p/inside.txt");
            let mac = inside.lines().next().unwrap_or("").trim().to_string();
            let outside = sb.run_ok(&["tin", "mac", "p/rec.json", "--label", "cigo-trust"]);
            assert_ne!(
                mac,
                outside.trim(),
                "a hop made the tin's own MAC: {inside}"
            );
            if mac.len() == 64 {
                sb.run_err(
                    &[
                        "tin",
                        "mac",
                        "p/rec.json",
                        "--label",
                        "cigo-trust",
                        "--check",
                        &mac,
                    ],
                    1,
                );
            }
        }
    }
}

/// Doctor looks at the tin's protection; looking must not make a key.
#[cfg(unix)]
#[test]
fn doctor_reports_the_tin_without_making_a_key() {
    let sb = Sandbox::new();
    // Doctor's exit code reflects the machine (a tool missing, cig not on
    // PATH); the tin block is what this test is about.
    let out = String::from_utf8_lossy(&sb.cig(&["doctor"]).stdout).to_string();
    assert!(out.contains("  tin"), "{out}");
    assert!(out.contains("not made yet"), "{out}");
    assert!(
        !sb.path().join(".cigscript-keys").exists(),
        "cig doctor created a key"
    );
}

/// Loading a key makes cig non-dumpable; a box forked afterwards still
/// writes its own uid_map and binds through /proc/self/fd. A control run
/// without a seal says whether this machine has the box at all, so a box
/// lost to the hardening cannot hide behind the ordinary fallback.
#[cfg(target_os = "linux")]
#[test]
fn a_run_that_seals_then_boxes_a_hop_still_makes_the_box() {
    let sb = Sandbox::new();
    sb.write("p/data.txt", "old\n");
    sb.write(
        "control.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo ctl > data.txt\"], {cwd: \"p\", box: true})\n}\n",
    );
    let control = sb.cig(&["run", "control.cig"]);
    let cerr = String::from_utf8_lossy(&control.stderr).to_string();
    assert!(control.status.success(), "{cerr}");
    if fell_back(&cerr) {
        allow_fallback(&cerr);
        return;
    }
    // This machine makes the box. Now the same hop after a seal.
    sb.write("p/.env", "SECRET=one\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  fs.write_text(\"p/.env\", \"SECRET=two\")\n  proc.run(\"sh\", [\"-c\", \"echo new > data.txt\"], {cwd: \"p\", box: true})\n}\n",
    );
    let out = sb.cig(&["run", "s.cig"]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{err}");
    assert!(
        !fell_back(&err),
        "the box fell back after a key was loaded (non-dumpable parent): {err}"
    );
    assert_eq!(sb.read("p/data.txt"), "new\n");
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    sb.run_ok(&["unburn", &id]);
    assert_eq!(sb.read("p/.env"), "SECRET=one\n");
    assert_eq!(sb.read("p/data.txt"), "ctl\n");
}

#[cfg(windows)]
#[test]
fn a_timeout_kills_the_grandchild_on_windows() {
    let sb = Sandbox::new();
    // cmd starts powershell, which would sleep well past the timeout while
    // holding the hop's stdout; the job ends it with cmd, so the hop
    // returns at its timeout, not forty seconds later.
    sb.write(
        "s.cig",
        "burn { proc.run(\"cmd\", [\"/c\", \"powershell -NoProfile -Command Start-Sleep 40\"], {timeout_ms: 1500}) }\n",
    );
    let started = std::time::Instant::now();
    let err = sb.run_err(&["run", "s.cig"], 1);
    let took = started.elapsed();
    assert!(err.contains("error[E554 runtime]"), "{err}");
    assert!(
        took < std::time::Duration::from_secs(20),
        "the hop took {took:?}: the grandchild outlived the timeout\n{err}"
    );
}

// ----- Windows device names (#48): refused before anything runs

#[test]
fn writing_to_nul_is_refused_before_anything_runs() {
    let sb = Sandbox::new();
    sb.write(
        "s.cig",
        "pack { \".\" }\nburn {\n  fs.write_text(\"nul\", \"x\")\n  fs.write_text(\"COM1.txt\", \"y\")\n  fs.write_text(\"console.txt\", \"fine\")\n}\n",
    );
    if cfg!(windows) {
        // A device here: the checker refuses the script outright (check
        // errors exit 2), and so does run, before anything is written.
        let err = sb.run_err(&["check", "s.cig"], 2);
        assert!(
            err.contains("error[E758 check]") && err.contains("nul"),
            "{err}"
        );
        assert!(err.contains("COM1.txt"), "{err}");
        assert!(!err.contains("console.txt"), "{err}");
        let refused = sb.cig(&["run", "s.cig"]);
        let err = String::from_utf8_lossy(&refused.stderr);
        assert_ne!(refused.status.code(), Some(0), "{err}");
        assert!(err.contains("E758"), "{err}");
        assert!(!sb.exists("console.txt"));
    } else {
        // An ordinary file here, but the script will not run on Windows:
        // a warning, which --deny-warnings turns into a stop.
        let out = sb.cig(&["check", "s.cig"]);
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(0), "{err}");
        assert!(
            err.contains("warning[E758 check]") && err.contains("nul"),
            "{err}"
        );
        assert!(
            err.contains("COM1.txt") && !err.contains("console.txt"),
            "{err}"
        );
        let denied = sb.cig(&["check", "--deny-warnings", "s.cig"]);
        let err = String::from_utf8_lossy(&denied.stderr);
        assert_ne!(denied.status.code(), Some(0), "{err}");
        assert!(err.contains("E758"), "{err}");
        // And the kernel lets it through at run time, as the file it is here.
        sb.run_ok(&["run", "s.cig"]);
        assert_eq!(sb.read("nul"), "x");
    }
}

#[cfg(windows)]
#[test]
fn a_computed_device_name_is_refused_by_the_kernel_on_windows() {
    let sb = Sandbox::new();
    sb.write(
        "s.cig",
        "pack { \".\" }\nstick name = \"nu\" + \"l\"\nburn {\n  fs.write_text(\"first.txt\", \"1\")\n  fs.write_text(name, \"x\")\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(
        err.contains("error[E758 burn]") && err.contains("nul device"),
        "{err}"
    );
    // The refusal came before the write was journaled, and the run rolled back.
    assert!(!sb.exists("first.txt"), "{err}");
}

// ----- Windows ports of Unix-only guarantees (#50)

#[cfg(windows)]
#[test]
fn symlinks_roll_back_as_symlinks_on_windows() {
    use std::os::windows::fs::symlink_file;
    let sb = Sandbox::new();
    sb.write("target.txt", "t");
    // Symbolic links need a privilege or developer mode on Windows; where
    // this runner has neither, the guarantee cannot be shown and the test
    // says so instead of failing.
    if symlink_file("target.txt", sb.path().join("live.lnk")).is_err() {
        eprintln!("skipped: this runner cannot create symbolic links");
        return;
    }
    symlink_file("nowhere\\at\\all", sb.path().join("dangling.lnk")).unwrap();
    fs::create_dir_all(sb.path().join("tree")).unwrap();
    symlink_file("..\\target.txt", sb.path().join("tree\\inner.lnk")).unwrap();
    sb.write(
        "s.cig",
        concat!(
            "burn {\n",
            "  fs.rm(\"live.lnk\")\n",
            "  fs.rm(\"dangling.lnk\")\n",
            "  fs.rm(\"tree\")\n",
            "  fs.write_text(\"target.txt\", \"changed\")\n",
            "}\n",
            "cough \"abort\"\n"
        ),
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(
        err.contains("unburn: rolling back") && !err.contains("failed"),
        "{err}"
    );
    let is_link = |p: &str| {
        fs::symlink_metadata(sb.path().join(p))
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
    };
    assert!(
        is_link("live.lnk"),
        "live.lnk came back as a real file or not at all: {err}"
    );
    assert_eq!(
        fs::read_link(sb.path().join("live.lnk")).unwrap(),
        Path::new("target.txt")
    );
    assert!(is_link("dangling.lnk"), "{err}");
    assert!(is_link("tree\\inner.lnk"), "{err}");
    assert_eq!(sb.read("target.txt"), "t");
}

// ----- 1.4.0: what a rollback must never do, found by a pass over 1.3.1 ----------

/// Hop watching stops at the cap; past it, a file first seen after the hop
/// cannot be told from one the scan never reached, so nothing is journaled
/// as created and nothing that existed before the run is removed.
#[cfg(unix)]
#[test]
fn a_rollback_on_a_pack_past_the_scan_cap_removes_nothing_that_existed() {
    let sb = Sandbox::new();
    sb.write("home/config", "watch_cap = 20\n");
    for d in 0..5 {
        for i in 0..10 {
            sb.write(&format!("p/d{d:02}/f{i:03}.txt"), "x");
        }
    }
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"rm\", [\"-rf\", \"p/d00\"])\n  cough \"boom\"\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(
        err.contains("hop watching saw only part of it"),
        "no warning about the cap: {err}"
    );
    assert!(!err.contains("restored hop created"), "{err}");
    for d in 1..5 {
        for i in 0..10 {
            assert!(
                sb.exists(&format!("p/d{d:02}/f{i:03}.txt")),
                "p/d{d:02}/f{i:03}.txt existed before the run and the rollback removed it\n{err}"
            );
        }
    }
}

/// A link inside the pack that leads outside it is not walked: what lies
/// behind it is not the pack's, so a hop deleting through it is reported
/// (E752) and the rollback never touches what remains there.
#[cfg(unix)]
#[test]
fn a_link_to_a_folder_outside_the_pack_is_not_walked() {
    use std::os::unix::fs::symlink;
    let sb = Sandbox::new();
    for i in 0..6 {
        sb.write(&format!("outside/lib/m{i}.py"), "x");
        sb.write(&format!("outside/other/n{i}.py"), "x");
    }
    sb.write("p/own.txt", "x");
    symlink(sb.path().join("outside"), sb.path().join("p/vendor")).unwrap();
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"rm\", [\"-rf\", \"p/vendor/lib\"])\n  cough \"boom\"\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(
        err.contains("E752") && err.contains("wrote outside the pack"),
        "{err}"
    );
    assert!(!err.contains("hop created"), "{err}");
    for i in 0..6 {
        assert!(
            sb.exists(&format!("outside/other/n{i}.py")),
            "a file outside the pack was removed through the link\n{err}"
        );
    }
    assert!(sb.exists("p/own.txt"));
}

/// Hop watching cannot tell the hop's bytes from a save made while it ran,
/// so a rollback moves what it displaces into the tin instead of destroying
/// it, and `cig unburn <id> --give-back` puts it back.
#[cfg(unix)]
#[test]
fn a_file_saved_during_a_hop_survives_its_rollback() {
    let sb = Sandbox::new();
    sb.write("p/src/app.txt", "v1\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo 'my new notes' > src/notes.txt; echo 'v2 my edit' > src/app.txt\"], {cwd: \"p\", touches: [\"**\"]})\n  cough \"boom\"\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    // The rollback did its job...
    assert!(!sb.exists("p/src/notes.txt"), "{err}");
    assert_eq!(sb.read("p/src/app.txt"), "v1\n");
    // ...and destroyed nothing: both files went to the tin, and it says so.
    assert!(
        err.contains("went to the tin") && err.contains("--give-back"),
        "{err}"
    );
    let rec = &sb.runs()[0];
    let id = rec["id"].as_str().unwrap().to_string();
    assert_eq!(rec["rollback_took"], 2, "{rec}");
    let shown = sb.run_ok(&["runs", &id]);
    assert!(
        shown.contains("took      2 files") && shown.contains("src/notes.txt"),
        "{shown}"
    );
    let gave = sb.run_ok(&["unburn", &id, "--give-back"]);
    assert!(gave.contains("gave back"), "{gave}");
    assert_eq!(sb.read("p/src/notes.txt"), "my new notes\n");
    assert_eq!(sb.read("p/src/app.txt"), "v2 my edit\n");
    // A second give-back swaps back what the first displaced: nothing is
    // ever the last copy.
    sb.run_ok(&["unburn", &id, "--give-back"]);
    assert_eq!(sb.read("p/src/app.txt"), "v1\n");
}

/// A file the hop left that changed again after the hop ended, and that no
/// later entry of the run accounts for, is someone else's work: the
/// automatic rollback leaves it, says so, and `--force` restores over it on
/// purpose. (A process the hop left behind plays the editor; its output is
/// redirected, so the hop ends when its child does.)
#[cfg(unix)]
#[test]
fn a_file_changed_after_the_hop_is_left_alone_by_the_rollback() {
    let sb = Sandbox::new();
    sb.write("p/a.txt", "v1\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo v2 > p/a.txt; ( sleep 0.3; echo v3-user-edit > p/a.txt ) >/dev/null 2>&1 &\"], {touches: [\"**\"]})\n  time.sleep_ms(900)\n  cough \"boom\"\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(err.contains("left alone"), "{err}");
    assert_eq!(sb.read("p/a.txt"), "v3-user-edit\n", "{err}");
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    sb.run_ok(&["unburn", &id, "--force"]);
    assert_eq!(sb.read("p/a.txt"), "v1\n");
}

/// Two runs on one pack would roll back each other's work: the second is
/// refused while the first holds the pack, and runs once it is done.
#[cfg(unix)]
#[test]
fn a_second_run_on_the_same_pack_is_refused_while_the_first_runs() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "x\n");
    sb.write(
        "slow.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"sleep 2\"])\n}\n",
    );
    sb.write(
        "other.cig",
        "pack { \"./p/sub\" }\nburn {\n  fs.write_text(\"p/sub/x.txt\", \"1\")\n}\n",
    );
    let mut slow = Command::new(env!("CARGO_BIN_EXE_cig"))
        .args(["run", "slow.cig"])
        .current_dir(sb.path())
        .env("CIGSCRIPT_HOME", sb.home())
        .env("NO_COLOR", "1")
        .env("CIG_SEAL_KEY", "file")
        .env("CIG_UPDATE_MANIFEST", sb.path().join("no-releases.json"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn the slow run");
    // Until the slow run holds the lock, which it takes as its pack is declared.
    let locks = sb.home().join("locks");
    for _ in 0..100 {
        if fs::read_dir(&locks)
            .map(|d| {
                d.flatten()
                    .any(|e| e.path().extension().is_some_and(|x| x == "who"))
            })
            .unwrap_or(false)
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let err = sb.run_err(&["run", "other.cig"], 1);
    assert!(
        err.contains("error[E759 burn]") && err.contains("is in use by run"),
        "{err}"
    );
    assert!(!sb.exists("p/sub/x.txt"), "the refused run wrote anyway");
    assert!(slow.wait().unwrap().success());
    sb.run_ok(&["run", "other.cig"]);
    assert_eq!(sb.read("p/sub/x.txt"), "1");
}

/// The objects a hop stashed are listed in its run directory before they
/// exist, so a `cig tin gc` during the hop keeps them.
#[cfg(unix)]
#[test]
fn tin_gc_during_a_hop_keeps_what_the_hop_stashed() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "original\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"\\\"$CIG_BIN\\\" tin gc; echo CHANGED > p/keep.txt\"], {touches: [\"**\"]})\n  cough \"boom\"\n}\n",
    );
    let out = sb.cig_env(&["run", "s.cig"], &[("CIG_BIN", env!("CARGO_BIN_EXE_cig"))]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert_eq!(sb.read("p/keep.txt"), "original\n", "{err}");
    assert!(err.contains("restored hop modified"), "{err}");
    assert!(!sb.exists(
        &sb.runs()[0]["id"]
            .as_str()
            .map(|id| format!("home/runs/{id}/tin.pending"))
            .unwrap()
    ));
}

/// A folder a hop turned into a file comes back as the folder, with what
/// was in it; the file goes to the tin.
#[cfg(unix)]
#[test]
fn a_folder_replaced_by_a_file_is_restored_as_the_folder() {
    let sb = Sandbox::new();
    sb.write("p/cfg/a.conf", "a\n");
    sb.write("p/cfg/b.conf", "b\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"rm -rf cfg; echo flat > cfg\"], {cwd: \"p\", touches: [\"**\"]})\n  cough \"boom\"\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(sb.path().join("p/cfg").is_dir(), "{err}");
    assert_eq!(sb.read("p/cfg/a.conf"), "a\n", "{err}");
    assert_eq!(sb.read("p/cfg/b.conf"), "b\n", "{err}");
    assert!(
        err.contains("directory made again") && !err.contains("File exists"),
        "{err}"
    );
    assert_eq!(
        sb.runs()[0]["rollback_took"],
        1,
        "the flat file went to the tin: {err}"
    );
}

/// An empty folder a hop deletes comes back, with its mode.
#[cfg(unix)]
#[test]
fn an_empty_folder_a_hop_deletes_comes_back() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    fs::create_dir_all(sb.path().join("p/keepme/empty")).unwrap();
    fs::set_permissions(
        sb.path().join("p/keepme/empty"),
        fs::Permissions::from_mode(0o750),
    )
    .unwrap();
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"rmdir\", [\"p/keepme/empty\"])\n  cough \"boom\"\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    let meta = fs::metadata(sb.path().join("p/keepme/empty"))
        .unwrap_or_else(|_| panic!("the folder did not come back\n{err}"));
    assert!(meta.is_dir());
    assert_eq!(meta.permissions().mode() & 0o777, 0o750, "{err}");
    assert!(
        err.contains("hop deleted") && err.contains("directory made again"),
        "{err}"
    );
}

/// A `touches` glob that matches nothing is said, before the run and
/// after it, with the glob that would have covered what changed.
#[cfg(unix)]
#[test]
fn a_touches_glob_that_matches_nothing_is_a_warning() {
    let sb = Sandbox::new();
    sb.write("p/src/a.txt", "v1\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo v2 > p/src/a.txt\"], {touches: [\"scr/**\"]})\n}\n",
    );
    let dry = sb.cig(&["run", "--dry-run", "s.cig"]);
    let plan = String::from_utf8_lossy(&dry.stderr);
    assert!(dry.status.success(), "{plan}");
    assert!(plan.contains("(touches scr/**: 0 files)"), "{plan}");
    let out = sb.cig(&["run", "s.cig"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(
        err.contains("touches `scr/**` matches no file in the pack (did you mean `src/**`?)"),
        "{err}"
    );
    // One glob among several matching nothing is ordinary (a lockfile the
    // project may have); only a list that covers nothing at all is said.
    sb.write(
        "t.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"true\"], {touches: [\"src/**\", \"yarn.lock\"]})\n}\n",
    );
    let quiet = sb.cig(&["run", "t.cig"]);
    let quiet_err = String::from_utf8_lossy(&quiet.stderr);
    assert!(quiet.status.success(), "{quiet_err}");
    assert!(!quiet_err.contains("matches no file"), "{quiet_err}");
    assert!(
        err.contains("p/src/a.txt is not under touches")
            && err.contains("touches: [\"src/**\"] would keep it"),
        "{err}"
    );
}

/// A dangling link is a link all the same: a hop that deletes or retargets
/// one is undone like any other.
#[cfg(unix)]
#[test]
fn a_dangling_link_a_hop_retargets_or_deletes_comes_back() {
    use std::os::unix::fs::symlink;
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "keep\n");
    symlink("nowhere.txt", sb.path().join("p/dangle.txt")).unwrap();
    symlink("nowhere.txt", sb.path().join("p/dangle2.txt")).unwrap();
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"rm p/dangle.txt; ln -sfn keep.txt p/dangle2.txt\"], {touches: [\"**\"]})\n  cough \"boom\"\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    for name in ["p/dangle.txt", "p/dangle2.txt"] {
        let p = sb.path().join(name);
        let meta = fs::symlink_metadata(&p).unwrap_or_else(|_| panic!("{name} is gone\n{err}"));
        assert!(meta.file_type().is_symlink(), "{name}: {err}");
        assert_eq!(
            fs::read_link(&p).unwrap(),
            Path::new("nowhere.txt"),
            "{name}: {err}"
        );
    }
    assert_eq!(sb.read("p/keep.txt"), "keep\n");
}

/// A write through a file link that leads out of the pack is a write
/// outside the pack: reported, not undone, and the file it reached is left
/// as the hop left it.
#[cfg(unix)]
#[test]
fn a_write_through_a_link_to_outside_the_pack_is_reported() {
    use std::os::unix::fs::symlink;
    let sb = Sandbox::new();
    sb.write("outside/target.txt", "o\n");
    fs::create_dir_all(sb.path().join("p")).unwrap();
    symlink("../outside/target.txt", sb.path().join("p/viaLink.txt")).unwrap();
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo CHANGED > viaLink.txt\"], {cwd: \"p\", touches: [\"**\"]})\n  cough \"boom\"\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(
        err.contains("E752") && err.contains("outside/target.txt"),
        "{err}"
    );
    assert_eq!(sb.read("outside/target.txt"), "CHANGED\n");
    assert!(fs::symlink_metadata(sb.path().join("p/viaLink.txt"))
        .unwrap()
        .file_type()
        .is_symlink());
}

/// An empty PATH entry is not the current directory, and a bare name the
/// resolver cannot find is not found: `proc.which` and `proc.run` agree.
#[cfg(unix)]
#[test]
fn an_empty_path_entry_is_not_the_current_directory() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    let tool = sb.write("mytool", "#!/bin/sh\necho from-cwd\n");
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
    sb.write(
        "s.cig",
        "exhale str(proc.which(\"mytool\"))\nburn {\n  proc.run(\"mytool\", [])\n}\n",
    );
    let out = sb.cig_env(&["run", "s.cig"], &[("PATH", ":/usr/bin:/bin")]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(stdout.trim(), "null", "{err}");
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.contains("error[E551 runtime]"), "{err}");
}

/// A compensation that fails leaves the ops it covered standing, and the
/// report says so instead of calling them compensated.
#[cfg(unix)]
#[test]
fn a_failed_compensation_leaves_its_ops_honestly_not_undone() {
    let sb = Sandbox::new();
    sb.write(
        "s.cig",
        concat!(
            "burn (s) {\n",
            "  proc.run(\"sh\", [\"-c\", \"echo live > remote.txt\"])\n",
            "} unburn {\n",
            "  proc.run(\"sh\", [\"-c\", \"echo undo-broke >&2; exit 7\"], {check: true})\n",
            "}\n",
            "burn { cough \"boom\" }\n"
        ),
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(
        err.contains("the burn at line 3 was not undone: its compensation failed:"),
        "{err}"
    );
    assert!(
        err.contains("not undone run sh -c 'echo live > remote.txt'")
            && err.contains("its compensation failed, so it stands"),
        "{err}"
    );
    assert!(!err.contains("compensated run sh"), "{err}");
    assert_eq!(sb.read("remote.txt"), "live\n");
    assert_eq!(sb.runs()[0]["status"], "rollback_incomplete");
}

/// A cig a hop starts (`cig run inner.cig` as a task) works inside the
/// parent's pack: the parent waits for it, so nothing interleaves.
#[cfg(unix)]
#[test]
fn a_cig_run_started_by_a_hop_shares_its_parents_pack() {
    let sb = Sandbox::new();
    fs::create_dir_all(sb.path().join("out")).unwrap();
    sb.write(
        "inner.cig",
        "pack { \"./out\" }\nburn {\n  fs.write_text(\"out/x.txt\", \"1\")\n}\n",
    );
    sb.write(
        "outer.cig",
        "pack { \".\" }\nburn {\n  stick r = proc.run(env.get(\"CIG_BIN\"), [\"run\", \"inner.cig\"], {check: true})\n  exhale r.err.trim()\n}\n",
    );
    let out = sb.cig_env(
        &["run", "outer.cig"],
        &[("CIG_BIN", env!("CARGO_BIN_EXE_cig"))],
    );
    let err = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{err}\n{stdout}");
    assert!(
        !stdout.contains("E759"),
        "the child run was refused the pack:\n{stdout}"
    );
    assert_eq!(sb.read("out/x.txt"), "1");
}

/// A link a hop makes inside the pack is the pack's entry, wherever it
/// points: rollback removes it, and nothing outside the pack is touched.
#[cfg(unix)]
#[test]
fn a_link_a_hop_creates_to_outside_the_pack_is_removed_on_rollback() {
    let sb = Sandbox::new();
    sb.write("outside/target.txt", "o\n");
    fs::create_dir_all(sb.path().join("p")).unwrap();
    let target = sb.path().join("outside/target.txt");
    sb.write(
        "s.cig",
        &format!(
            "pack {{ \"./p\" }}\nburn {{\n  proc.run(\"ln\", [\"-s\", \"{}\", \"p/link.txt\"])\n  cough \"boom\"\n}}\n",
            target.display()
        ),
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(err.contains("restored hop created"), "{err}");
    assert!(
        !err.contains("E752"),
        "the link was taken for a write outside the pack:\n{err}"
    );
    assert!(
        fs::symlink_metadata(sb.path().join("p/link.txt")).is_err(),
        "the link the hop made is still there"
    );
    assert_eq!(sb.read("outside/target.txt"), "o\n");
}

/// A burn with a label is named by it wherever the plan, the record or a
/// rollback would have named its line: a tool written in CigScript passes
/// the user's own words.
#[cfg(unix)]
#[test]
fn a_labelled_burn_names_itself_when_its_compensation_fails() {
    let sb = Sandbox::new();
    sb.write(
        "s.cig",
        concat!(
            "stick task = \"deploy\"\n",
            "burn (s, task) {\n",
            "  s.what = \"live\"\n",
            "  proc.run(\"sh\", [\"-c\", \"echo live > remote.txt\"])\n",
            "} unburn {\n",
            "  proc.run(\"sh\", [\"-c\", \"echo undo-broke >&2; exit 7\"], {check: true})\n",
            "}\n",
            "burn (\"notify\") {\n",
            "  proc.run(\"true\", [])\n",
            "} unburn {\n",
            "  proc.run(\"true\", [])\n",
            "}\n",
            "burn { cough \"boom\" }\n"
        ),
    );
    let dry = sb.cig(&["run", "--dry-run", "s.cig"]);
    let plan = String::from_utf8_lossy(&dry.stderr);
    assert!(
        plan.contains("compensation for deploy") && plan.contains("compensation for notify"),
        "{plan}"
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(
        err.contains("deploy is NOT undone: its compensation failed: proc.run:"),
        "{err}"
    );
    assert!(err.contains("ran the compensation for notify"), "{err}");
    assert!(
        !err.contains("at line"),
        "a labelled burn is never named by its line:\n{err}"
    );
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let shown = sb.run_ok(&["runs", &id]);
    assert!(shown.contains("compensation for deploy"), "{shown}");
    // The label is a string, read before the block runs.
    sb.write("bad.cig", "burn (s, 42) {\n  exhale 1\n}\n");
    let bad = sb.run_err(&["run", "bad.cig"], 1);
    assert!(bad.contains("a burn's label is a string, got int"), "{bad}");
}

/// Without a label the line stands in, said once.
#[cfg(unix)]
#[test]
fn an_unlabelled_burn_says_its_line_once() {
    let sb = Sandbox::new();
    sb.write(
        "s.cig",
        concat!(
            "burn (s) {\n",
            "  proc.run(\"true\", [])\n",
            "} unburn {\n",
            "  proc.run(\"sh\", [\"-c\", \"exit 7\"], {check: true})\n",
            "}\n",
            "burn { cough \"boom\" }\n"
        ),
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    let line = err
        .lines()
        .find(|l| l.contains("was not undone"))
        .unwrap_or_else(|| panic!("no 'was not undone' line:\n{err}"));
    assert!(
        line.contains("the burn at line 3 was not undone: its compensation failed: proc.run:"),
        "{line}"
    );
    assert_eq!(line.matches("at line").count(), 1, "{line}");
    assert!(
        !err.contains("compensation for the burn at line 3 failed"),
        "{err}"
    );
}

/// A `touches` glob written with a leading `?` may match nothing without a
/// word: a lockfile the project does not have yet.
#[cfg(unix)]
#[test]
fn an_optional_touches_glob_that_matches_nothing_is_quiet() {
    let sb = Sandbox::new();
    sb.write("p/src/a.txt", "v1\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo v2 > p/src/a.txt\"], {touches: [\"?yarn.lock\", \"?package-lock.json\", \"src/**\"]})\n  cough \"boom\"\n}\n",
    );
    let dry = sb.cig(&["run", "--dry-run", "s.cig"]);
    let plan = String::from_utf8_lossy(&dry.stderr);
    assert!(plan.contains("?yarn.lock: 0 files"), "{plan}");
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(
        !err.contains("match"),
        "an optional glob was counted as a typo:\n{err}"
    );
    assert_eq!(sb.read("p/src/a.txt"), "v1\n", "{err}");
    // A list that is optional through and through says nothing either.
    sb.write(
        "t.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"true\", [], {touches: [\"?yarn.lock\"]})\n}\n",
    );
    let quiet = sb.cig(&["run", "t.cig"]);
    let quiet_err = String::from_utf8_lossy(&quiet.stderr);
    assert!(
        quiet.status.success() && !quiet_err.contains("match"),
        "{quiet_err}"
    );
}

/// Each warning about a hop's watch is printed once.
#[cfg(unix)]
#[test]
fn e757_is_printed_once() {
    let sb = Sandbox::new();
    sb.write("home/config", "watch_cap = 20\n");
    for i in 0..30 {
        sb.write(&format!("p/f{i:03}.txt"), "x");
    }
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"true\", [], {touches: [\"scr/**\"]})\n}\n",
    );
    let out = sb.cig(&["run", "s.cig"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert_eq!(err.matches("saw only part of it").count(), 1, "{err}");
    sb.write("home/config", "watch_cap = 50000\n");
    let out = sb.cig(&["run", "s.cig"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(err.matches("matches no file").count(), 1, "{err}");
}

/// A deleted folder outside `touches` is one warning, for the folder, not
/// one per file; a lone file is still named itself (#90).
#[cfg(unix)]
#[test]
fn changes_outside_touches_are_reported_once_per_folder() {
    let sb = Sandbox::new();
    sb.write("p/config.ini", "original\n");
    sb.write("p/other.txt", "o\n");
    for i in 0..1200 {
        sb.write(&format!("p/d01/sub{}/f{i:04}.txt", i % 3), "x");
    }
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo CHANGED > config.ini; echo new > made.txt; rm -rf d01; echo o2 > other.txt\"], {cwd: \"p\", touches: [\"config.ini\"]})\n  cough \"boom\"\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert_eq!(sb.read("p/config.ini"), "original\n", "{err}");
    assert!(!sb.exists("p/made.txt"), "{err}");
    let lines: Vec<&str> = err
        .lines()
        .filter(|l| l.contains("not under touches"))
        .collect();
    assert_eq!(lines.len(), 2, "{err}");
    let folder = lines
        .iter()
        .find(|l| l.contains("1,200 files under d01/ are not under touches"))
        .unwrap_or_else(|| panic!("{err}"));
    assert!(
        folder.contains("touches: [\"d01/**\"] would keep them")
            && folder.contains("d01/sub0/f0000.txt")
            && folder.contains("lists them all"),
        "{folder}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("p/other.txt is not under touches")
                && l.contains("touches: [\"*\"] would keep it")),
        "{err}"
    );
    // The full list is on the run.
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let shown = sb.run_ok(&["runs", &id]);
    assert_eq!(shown.matches("d01/sub1/f0001.txt").count(), 1, "{shown}");
    assert_eq!(shown.matches("hop deleted").count(), 1204, "{shown}");
}

/// `--give-back --dry-run` says what it would put back and changes
/// nothing; a path changed since the rollback is refused without `--force`
/// (#91).
#[cfg(unix)]
#[test]
fn give_back_dry_run_lists_what_it_would_restore_and_changes_nothing() {
    let sb = Sandbox::new();
    sb.write("p/src/app.txt", "v1\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo 'my new notes' > src/notes.txt; echo 'v2 my edit' > src/app.txt\"], {cwd: \"p\", touches: [\"**\"]})\n  cough \"boom\"\n}\n",
    );
    sb.run_err(&["run", "s.cig"], 1);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let plan = sb.run_ok(&["unburn", &id, "--give-back", "--dry-run"]);
    assert!(plan.contains("would give back 2 files"), "{plan}");
    assert!(
        plan.contains("give back")
            && plan.contains("src/notes.txt")
            && plan.contains("src/app.txt"),
        "{plan}"
    );
    assert!(
        plan.contains("11 bytes") && plan.contains("13 bytes"),
        "the tin's bytes for each: {plan}"
    );
    assert!(
        plan.contains("(now absent)"),
        "notes.txt is gone since the rollback: {plan}"
    );
    assert!(
        plan.contains("(now a file, 3 bytes"),
        "app.txt holds v1 again: {plan}"
    );
    assert_eq!(
        plan.matches("unchanged since the rollback").count(),
        2,
        "{plan}"
    );
    assert!(plan.contains("nothing was changed"), "{plan}");
    assert_eq!(sb.read("p/src/app.txt"), "v1\n");
    assert!(!sb.exists("p/src/notes.txt"));
    let json = sb.run_ok(&["--json", "unburn", &id, "--give-back", "--dry-run"]);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(
        v["would_give_back"].as_array().map(Vec::len),
        Some(2),
        "{json}"
    );
    assert_eq!(v["would_give_back"][0]["changed"], false, "{json}");
    // An edit since the rollback: the preview says so, the real give-back
    // refuses before touching anything, and --force goes ahead.
    sb.write("p/src/app.txt", "v1 edited since\n");
    let plan = sb.run_ok(&["unburn", &id, "--give-back", "--dry-run"]);
    assert!(
        plan.contains("1 path changed since the rollback") && plan.contains("E704"),
        "{plan}"
    );
    assert_eq!(
        plan.matches("changed since the rollback").count(),
        3,
        "{plan}"
    );
    let refused = sb.run_err(&["unburn", &id, "--give-back"], 1);
    assert!(
        refused.contains("error[E704 burn]") && refused.contains("src/app.txt"),
        "{refused}"
    );
    assert_eq!(sb.read("p/src/app.txt"), "v1 edited since\n");
    assert!(
        !sb.exists("p/src/notes.txt"),
        "the refusal gave nothing back"
    );
    let forced = sb.run_ok(&["unburn", &id, "--give-back", "--force"]);
    assert!(forced.contains("gave back"), "{forced}");
    assert_eq!(sb.read("p/src/app.txt"), "v2 my edit\n");
    assert_eq!(sb.read("p/src/notes.txt"), "my new notes\n");
}

// ----- the hash chain (#88): a journal edited after the fact is not the record

/// Runs a script that burns twice and returns the run id and the journal path.
fn chained_run(sb: &Sandbox) -> (String, PathBuf) {
    sb.write("a.txt", "one\n");
    sb.write(
        "s.cig",
        "burn {\n  fs.write_text(\"a.txt\", \"two\")\n  fs.write_text(\"b.txt\", \"new\")\n}\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let jp = sb.home().join("runs").join(&id).join("journal.jsonl");
    (id, jp)
}

#[test]
fn an_edited_journal_line_breaks_the_chain() {
    let sb = Sandbox::new();
    let (id, jp) = chained_run(&sb);
    let shown = sb.run_ok(&["runs", &id, "--verify"]);
    assert!(
        shown.contains("chain     intact: 3 lines")
            && shown.contains("head      matches the record")
            && shown.contains("objects   every snapshot"),
        "{shown}"
    );
    // Every line but the first carries the hash of the line before it.
    let text = fs::read_to_string(&jp).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text}");
    assert!(lines[0].starts_with("{\"format\":3,\"run\":\""), "{text}");
    for l in &lines[1..] {
        assert!(l.contains("\"prev\":\""), "{l}");
    }
    // Edit the first entry: the second no longer fits it.
    let edited = text.replacen("a.txt", "z.txt", 1);
    assert_ne!(edited, text);
    fs::write(&jp, &edited).unwrap();
    let out = sb.cig(&["runs", &id, "--verify"]);
    assert_eq!(out.status.code(), Some(1));
    let shown = String::from_utf8_lossy(&out.stdout);
    assert!(
        shown
            .contains("chain     BROKEN at line 3: its signature does not match: this line was not signed by your key"),
        "{shown}"
    );
    // Edit the last line instead: the chain fits, the recorded head does not.
    let last_edited = text.replace("b.txt", "c.txt");
    assert_ne!(last_edited, text);
    fs::write(&jp, last_edited).unwrap();
    let out = sb.cig(&["runs", &id, "--verify"]);
    assert_eq!(out.status.code(), Some(1));
    let shown = String::from_utf8_lossy(&out.stdout);
    assert!(
        shown.contains("chain     intact") && shown.contains("head      does not match the record"),
        "{shown}"
    );
    // The JSON form carries the same verdict.
    let json = sb.cig(&["--json", "runs", &id, "--verify"]);
    let v: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(v["verify"]["ok"], false, "{v}");
    assert_eq!(v["verify"]["head_ok"], false, "{v}");
}

/// `cig runs <id> --verify` and `cig unburn <id>` agree: for every way the
/// record can be tampered with, the one fails exactly when the other
/// refuses, with the same code (#112).
#[test]
fn verify_and_unburn_agree() {
    fn code_of(stderr: &str) -> Option<String> {
        stderr
            .find("error[E")
            .map(|i| stderr[i + 6..i + 10].to_string())
    }
    type Tamper = fn(&Sandbox, &str, &Path);
    let cases: Vec<(&str, Option<&str>, Tamper)> = vec![
        ("untouched", None, |_, _, _| {}),
        ("an edited line", Some("E760"), |_, _, jp| {
            let text = fs::read_to_string(jp).unwrap();
            fs::write(jp, text.replacen("a.txt", "z.txt", 1)).unwrap();
        }),
        ("cut short", Some("E760"), |_, _, jp| {
            let text = fs::read_to_string(jp).unwrap();
            let kept: Vec<&str> = text.lines().collect();
            fs::write(jp, kept[..kept.len() - 1].join("\n") + "\n").unwrap();
        }),
        (
            "cut short, the record made to agree",
            Some("E709"),
            |sb, id, jp| {
                let text = fs::read_to_string(jp).unwrap();
                let kept: Vec<&str> = text.lines().collect();
                fs::write(jp, kept[..kept.len() - 1].join("\n") + "\n").unwrap();
                let out = sb.cig(&["--json", "runs", id, "--verify"]);
                let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
                let head = v["verify"]["head"].as_str().unwrap().to_string();
                let rp = sb.home().join("runs").join(id).join("run.json");
                let mut rec: serde_json::Value =
                    serde_json::from_str(&fs::read_to_string(&rp).unwrap()).unwrap();
                rec["journal_head"] = serde_json::Value::String(head);
                fs::write(&rp, serde_json::to_string_pretty(&rec).unwrap()).unwrap();
            },
        ),
        ("a snapshot gone", Some("E703"), |sb, id, _| {
            let snaps = sb.home().join("runs").join(id).join("snapshots");
            for e in fs::read_dir(&snaps).unwrap().flatten() {
                fs::remove_file(e.path()).unwrap();
            }
        }),
    ];
    for (what, want, tamper) in cases {
        let sb = Sandbox::new();
        let (id, jp) = chained_run(&sb);
        tamper(&sb, &id, &jp);
        let verify = sb.cig(&["runs", &id, "--verify"]);
        let verr = String::from_utf8_lossy(&verify.stderr).to_string();
        let unburn = sb.cig(&["unburn", &id]);
        let uerr = String::from_utf8_lossy(&unburn.stderr).to_string();
        assert_eq!(
            verify.status.success(),
            unburn.status.success(),
            "{what}: --verify and unburn disagree\nverify: {verr}\n{}\nunburn: {uerr}",
            String::from_utf8_lossy(&verify.stdout)
        );
        assert_eq!(
            code_of(&verr),
            code_of(&uerr),
            "{what}\nverify: {verr}\nunburn: {uerr}"
        );
        assert_eq!(
            code_of(&verr).as_deref(),
            want,
            "{what}\nverify: {verr}\n{}",
            String::from_utf8_lossy(&verify.stdout)
        );
    }
}

/// A format 2 journal (unsigned) keeps the hash wording; the signed format
/// says "signature" (above).
#[test]
fn e760_says_hash_on_format_2() {
    let sb = Sandbox::new();
    sb.run_ok(&["config", "journal_sign", "off"]);
    let (id, jp) = chained_run(&sb);
    let text = fs::read_to_string(&jp).unwrap();
    assert!(text.starts_with("{\"format\":2,"), "{text}");
    fs::write(&jp, text.replacen("a.txt", "z.txt", 1)).unwrap();
    let out = sb.cig(&["runs", &id, "--verify"]);
    assert_eq!(out.status.code(), Some(1));
    let shown = String::from_utf8_lossy(&out.stdout);
    assert!(
        shown.contains("its prev is not the hash of the line before it"),
        "{shown}"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("error[E760 burn]"), "{err}");
}

#[test]
fn a_removed_journal_line_breaks_the_chain() {
    let sb = Sandbox::new();
    let (id, jp) = chained_run(&sb);
    let text = fs::read_to_string(&jp).unwrap();
    let kept: Vec<&str> = text
        .lines()
        .enumerate()
        .filter(|(i, _)| *i != 1)
        .map(|(_, l)| l)
        .collect();
    fs::write(&jp, format!("{}\n", kept.join("\n"))).unwrap();
    let out = sb.cig(&["runs", &id, "--verify"]);
    assert_eq!(out.status.code(), Some(1));
    let shown = String::from_utf8_lossy(&out.stdout);
    assert!(shown.contains("chain     BROKEN at line 2"), "{shown}");
    // Reordering is a removal and an insertion: seen the same way.
    let mut lines: Vec<&str> = text.lines().collect();
    lines.swap(1, 2);
    fs::write(&jp, format!("{}\n", lines.join("\n"))).unwrap();
    let out = sb.cig(&["runs", &id, "--verify"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn unburn_refuses_a_broken_chain() {
    let sb = Sandbox::new();
    let (id, jp) = chained_run(&sb);
    let text = fs::read_to_string(&jp).unwrap();
    fs::write(&jp, text.replacen("a.txt", "z.txt", 1)).unwrap();
    let plan = sb.run_ok(&["unburn", &id, "--dry-run"]);
    assert!(
        plan.contains("the journal's chain is broken at line 3") && plan.contains("E760"),
        "{plan}"
    );
    let err = sb.run_err(&["unburn", &id], 1);
    assert!(
        err.contains("error[E760 burn]: the journal of") && err.contains("line 3"),
        "{err}"
    );
    assert_eq!(sb.read("a.txt"), "two", "the refusal restored nothing");
    assert!(sb.exists("b.txt"));
    // --force restores from the journal as it is, and says so.
    let out = sb.cig(&["unburn", &id, "--force"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("chain is broken at line 3") && err.contains("--force given"),
        "{err}"
    );
    assert!(!sb.exists("b.txt"), "{err}");
    assert!(sb.cig(&["explain", "E760"]).status.success());
}

/// A journal an older cig wrote carries no chain, and is read as it always was.
#[test]
fn a_format_1_journal_still_unburns() {
    let sb = Sandbox::new();
    let (id, jp) = chained_run(&sb);
    // Make the journal format 1 by hand: no prev anywhere.
    let text = fs::read_to_string(&jp).unwrap();
    let mut out = String::from("{\"format\":1}\n");
    for l in text.lines().skip(1) {
        let v: serde_json::Value = serde_json::from_str(l).unwrap();
        let mut m = v.as_object().unwrap().clone();
        m.remove("prev");
        out.push_str(&serde_json::to_string(&m).unwrap());
        out.push('\n');
    }
    fs::write(&jp, out).unwrap();
    let shown = sb.run_ok(&["runs", &id, "--verify"]);
    assert!(
        shown.contains("chain     none: the journal is format 1"),
        "{shown}"
    );
    sb.run_ok(&["unburn", &id]);
    assert_eq!(sb.read("a.txt"), "one\n");
    assert!(!sb.exists("b.txt"));
    // The markers the unburn appended carry no prev either: the file stays format 1.
    let after = fs::read_to_string(&jp).unwrap();
    assert!(!after.contains("prev"), "{after}");
    assert!(after.contains("\"restored\":true"), "{after}");
}

/// A hop's one-byte edit to a large file costs the tin one chunk, and the
/// file comes back byte for byte from its recipe (#82).
#[cfg(unix)]
#[test]
fn a_hop_edit_to_a_large_file_stores_the_chunk_that_changed() {
    let sb = Sandbox::new();
    let mut x: u64 = 99;
    let data: Vec<u8> = (0..(1usize << 20))
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect();
    fs::create_dir_all(sb.path().join("p")).unwrap();
    fs::write(sb.path().join("p/big.bin"), &data).unwrap();
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", 'printf X | dd of=big.bin bs=1 seek=$1 conv=notrunc 2>/dev/null', \"sh\", args[0]], {cwd: \"p\", touches: [\"**\"]})\n  cough \"boom\"\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig", "--", "500000"], 1);
    assert_eq!(
        fs::read(sb.path().join("p/big.bin")).unwrap(),
        data,
        "the file came back byte for byte: {err}"
    );
    let du: serde_json::Value = serde_json::from_str(&sb.run_ok(&["--json", "tin", "du"])).unwrap();
    // The original and the hop's output (taken by the rollback) share every
    // chunk but the one the edit landed in.
    assert_eq!(du["recipes"], 2, "{du}");
    assert_eq!(du["logical"], 2u64 << 20, "{du}");
    let stored1 = du["stored"].as_u64().unwrap();
    assert!(
        stored1 < (1 << 20) + 2 * 256 * 1024,
        "two versions of 1 MiB stored in {stored1} bytes: {du}"
    );
    // Another edit, another run: one more chunk.
    sb.run_err(&["run", "s.cig", "--", "900000"], 1);
    assert_eq!(fs::read(sb.path().join("p/big.bin")).unwrap(), data);
    let du: serde_json::Value = serde_json::from_str(&sb.run_ok(&["--json", "tin", "du"])).unwrap();
    assert_eq!(du["recipes"], 3, "{du}");
    assert_eq!(du["logical"], 3u64 << 20, "{du}");
    let stored2 = du["stored"].as_u64().unwrap();
    assert!(
        stored2 - stored1 <= 2 * 256 * 1024,
        "the second edit added {} bytes: {du}",
        stored2 - stored1
    );
    // Every chunk a live run names survives gc, and the tin verifies.
    let gc: serde_json::Value = serde_json::from_str(&sb.run_ok(&["--json", "tin", "gc"])).unwrap();
    assert_eq!(gc["removed"], 0, "{gc}");
    sb.run_ok(&["tin", "verify"]);
    let text = sb.run_ok(&["tin"]);
    assert!(text.contains("chunked under a recipe"), "{text}");
}

// ----- git blob reuse (#87): a clean tracked file is not copied into the tin

/// A git repository at `p` in the sandbox, with one commit.
#[cfg(unix)]
fn git_repo(sb: &Sandbox) {
    fs::create_dir_all(sb.path().join("p/src")).unwrap();
    sb.write("p/src/app.txt", "tracked v1\n");
    sb.write("p/src/other.txt", "other file\n");
    for args in [
        &["init", "-q", "."][..],
        &["add", "."][..],
        &["-c", "commit.gpgsign=false", "commit", "-q", "-m", "init"][..],
    ] {
        let out = git_in(sb, args);
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[cfg(unix)]
fn git_in(sb: &Sandbox, args: &[&str]) -> Output {
    Command::new("git")
        .args(args)
        .current_dir(sb.path().join("p"))
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git")
}

#[cfg(unix)]
fn git_blob_of(sb: &Sandbox, id: &str) -> (serde_json::Value, String) {
    let shown: serde_json::Value =
        serde_json::from_str(&sb.run_ok(&["--json", "runs", id])).unwrap();
    let entry = shown["journal"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["op"]["kind"] == "hop_changed")
        .unwrap_or_else(|| panic!("{shown}"))
        .clone();
    let blob = entry["before"][0]["git"]["blob"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    (entry, blob)
}

#[cfg(unix)]
#[test]
fn a_clean_tracked_file_costs_no_tin_space() {
    let sb = Sandbox::new();
    git_repo(&sb);
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo v2 > src/app.txt\"], {cwd: \"p\", touches: [\"src/**\"]})\n  cough \"boom\"\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert_eq!(sb.read("p/src/app.txt"), "tracked v1\n", "{err}");
    assert!(err.contains("(git)"), "git is named as the keeper: {err}");
    // The tin holds only what the rollback set aside: the hop's output.
    let du: serde_json::Value = serde_json::from_str(&sb.run_ok(&["--json", "tin", "du"])).unwrap();
    assert_eq!(du["objects"], 1, "{du}");
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let (entry, blob) = git_blob_of(&sb, &id);
    assert_eq!(blob.len(), 40, "{entry}");
    assert_eq!(entry["before"][0]["snapshot"], "", "{entry}");
    // The run pinned the blob under its ref, and prune lets it go.
    let refs =
        String::from_utf8_lossy(&git_in(&sb, &["for-each-ref", "refs/cig/"]).stdout).to_string();
    assert!(refs.contains(&format!("refs/cig/runs/{id}")), "{refs}");
    let tree = refs.split_whitespace().next().unwrap().to_string();
    let listed =
        String::from_utf8_lossy(&git_in(&sb, &["cat-file", "-p", &tree]).stdout).to_string();
    assert!(listed.contains(&blob), "{listed}");
    assert!(sb.exists(&format!("home/runs/{id}/git-pins.txt")));
    sb.run_ok(&["runs", "--prune", "0"]);
    let refs =
        String::from_utf8_lossy(&git_in(&sb, &["for-each-ref", "refs/cig/"]).stdout).to_string();
    assert!(refs.trim().is_empty(), "{refs}");
}

#[cfg(unix)]
#[test]
fn a_restore_from_a_git_blob_is_hash_checked() {
    let sb = Sandbox::new();
    git_repo(&sb);
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo v2 > src/app.txt\"], {cwd: \"p\", touches: [\"src/**\"]})\n}\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let (_, blob) = git_blob_of(&sb, &id);
    // Make git answer with other bytes of the same size for that blob.
    let other = Command::new("git")
        .args(["hash-object", "-w", "--stdin"])
        .current_dir(sb.path().join("p"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            use std::io::Write as _;
            c.stdin.take().unwrap().write_all(b"tracked v9\n")?;
            c.wait_with_output()
        })
        .unwrap();
    let other = String::from_utf8_lossy(&other.stdout).trim().to_string();
    assert!(git_in(&sb, &["replace", "-f", &blob, &other])
        .status
        .success());
    let out = sb.cig(&["unburn", &id]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(
        text.contains("holds other bytes than the journal recorded"),
        "{text}"
    );
    assert_eq!(sb.read("p/src/app.txt"), "v2\n", "nothing was written");
    // With git honest again, the retry puts the file back.
    assert!(git_in(&sb, &["replace", "-d", &blob]).status.success());
    sb.run_ok(&["unburn", &id]);
    assert_eq!(sb.read("p/src/app.txt"), "tracked v1\n");
}

#[cfg(unix)]
#[test]
fn a_dirty_tracked_file_goes_to_the_tin() {
    let sb = Sandbox::new();
    git_repo(&sb);
    sb.write("p/src/app.txt", "tracked v1, edited\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo v2 > src/app.txt\"], {cwd: \"p\", touches: [\"src/app.txt\"]})\n  cough \"boom\"\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert_eq!(sb.read("p/src/app.txt"), "tracked v1, edited\n", "{err}");
    assert!(err.contains("(tin)") && !err.contains("(git)"), "{err}");
    let du: serde_json::Value = serde_json::from_str(&sb.run_ok(&["--json", "tin", "du"])).unwrap();
    assert_eq!(
        du["objects"], 2,
        "the dirty file and the hop's output: {du}"
    );
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let (entry, blob) = git_blob_of(&sb, &id);
    assert!(blob.is_empty(), "{entry}");
    let refs =
        String::from_utf8_lossy(&git_in(&sb, &["for-each-ref", "refs/cig/"]).stdout).to_string();
    assert!(refs.trim().is_empty(), "nothing to pin: {refs}");
}

#[cfg(unix)]
#[test]
fn a_blob_git_has_lost_refuses_the_restore_with_the_reason() {
    let sb = Sandbox::new();
    git_repo(&sb);
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo v2 > src/app.txt\"], {cwd: \"p\", touches: [\"src/**\"]})\n}\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let (_, blob) = git_blob_of(&sb, &id);
    // Someone dropped the pin and ran a gc: the loose object is gone.
    assert!(
        git_in(&sb, &["update-ref", "-d", &format!("refs/cig/runs/{id}")])
            .status
            .success()
    );
    let loose = sb
        .path()
        .join("p/.git/objects")
        .join(&blob[..2])
        .join(&blob[2..]);
    assert!(loose.is_file(), "{}", loose.display());
    fs::remove_file(&loose).unwrap();
    let shown = sb.cig(&["runs", &id, "--verify"]);
    assert_eq!(shown.status.code(), Some(1));
    let err = sb.run_err(&["unburn", &id], 1);
    assert!(
        err.contains("error[E703 burn]") && err.contains("git no longer has blob"),
        "{err}"
    );
    assert_eq!(sb.read("p/src/app.txt"), "v2\n", "nothing was restored");
}

// ----- sealed (#83): a secret the tin keeps is never in the clear

/// Do these bytes appear anywhere under the sandbox's state directory (tin
/// objects, recipes, sealed objects, run snapshots)?
#[cfg(unix)]
fn state_holds_bytes(sb: &Sandbox, needle: &[u8]) -> bool {
    fn walk(dir: &Path, needle: &[u8]) -> bool {
        for e in fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                if walk(&p, needle) {
                    return true;
                }
            } else if let Ok(bytes) = fs::read(&p) {
                if bytes.windows(needle.len()).any(|w| w == needle) {
                    return true;
                }
            }
        }
        false
    }
    walk(&sb.home(), needle)
}

#[cfg(unix)]
fn sealed_objects(sb: &Sandbox) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for bucket in fs::read_dir(sb.home().join("tin/sealed"))
        .into_iter()
        .flatten()
        .flatten()
    {
        for e in fs::read_dir(bucket.path()).into_iter().flatten().flatten() {
            out.push(e.path());
        }
    }
    out.sort();
    out
}

#[cfg(unix)]
#[test]
fn a_private_key_under_touches_is_never_stored_in_the_clear() {
    let sb = Sandbox::new();
    let secret = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZWQyNTUx\n-----END OPENSSH PRIVATE KEY-----\n";
    sb.write("p/.ssh/id_rsa", secret);
    sb.write("p/.env", "DB_PASSWORD=hunter2-very-secret\n");
    sb.write("p/README.md", "plain readme\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo rotated > .ssh/id_rsa; rm .env; echo changed > README.md\"], {cwd: \"p\", touches: [\"**\"]})\n  cough \"boom\"\n}\n",
    );
    let dry = sb.cig(&["run", "--dry-run", "s.cig"]);
    let plan = String::from_utf8_lossy(&dry.stderr);
    assert!(plan.contains("2 sealed"), "{plan}");
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert_eq!(sb.read("p/.ssh/id_rsa"), secret, "{err}");
    assert_eq!(sb.read("p/.env"), "DB_PASSWORD=hunter2-very-secret\n");
    assert_eq!(sb.read("p/README.md"), "plain readme\n");
    assert!(err.contains("(sealed)"), "{err}");
    assert!(
        !state_holds_bytes(&sb, b"BEGIN OPENSSH PRIVATE KEY"),
        "the private key is in the clear under the state directory"
    );
    assert!(!state_holds_bytes(&sb, b"hunter2-very-secret"));
    assert!(
        state_holds_bytes(&sb, b"plain readme\n"),
        "an ordinary file is kept as it is"
    );
    let du: serde_json::Value = serde_json::from_str(&sb.run_ok(&["--json", "tin", "du"])).unwrap();
    // The two secrets, and the hop's rotated key the rollback set aside.
    assert_eq!(du["sealed"], 3, "{du}");
    assert_eq!(du["objects"], 2, "the readme and its changed version: {du}");
    assert!(sb.exists(".cigscript-keys/seal.key"));
    sb.run_ok(&["tin", "verify"]);
    sb.run_ok(&["tin", "verify", "--quick"]);
    let text = sb.run_ok(&["tin"]);
    assert!(text.contains("3 sealed"), "{text}");
}

#[cfg(unix)]
#[test]
fn a_sealed_file_restores_byte_for_byte() {
    let sb = Sandbox::new();
    // Three megabytes: several segments, and a tail.
    let blob: Vec<u8> = (0..(3usize << 20) + 777)
        .map(|i| (i * 7 % 251) as u8)
        .collect();
    fs::create_dir_all(sb.path().join("p/data")).unwrap();
    fs::write(sb.path().join("p/data/customers.db"), &blob).unwrap();
    sb.write("p/config/token.txt", "tok-123-not-a-secret-name\n");
    sb.write("p/notes.txt", "old notes, sensitive by the script's word\n");
    sb.write("p/.cigseal", "config/**\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo x > data/customers.db; echo y > config/token.txt\"], {cwd: \"p\", touches: [\"config/**\"], touches_sealed: [\"data/*.db\"]})\n  fs.write_text(\"p/notes.txt\", \"new\", {sensitive: true})\n  fs.rm(\"p/config\")\n  cough \"boom\"\n}\n",
    );
    let dry = sb.cig(&["run", "--dry-run", "s.cig"]);
    let plan = String::from_utf8_lossy(&dry.stderr);
    assert!(plan.contains("2 sealed"), "{plan}");
    assert!(plan.contains("write p/notes.txt  (sealed)"), "{plan}");
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert_eq!(
        fs::read(sb.path().join("p/data/customers.db")).unwrap(),
        blob,
        "{err}"
    );
    assert_eq!(sb.read("p/config/token.txt"), "tok-123-not-a-secret-name\n");
    assert_eq!(
        sb.read("p/notes.txt"),
        "old notes, sensitive by the script's word\n"
    );
    assert!(!state_holds_bytes(&sb, b"tok-123-not-a-secret-name"));
    assert!(!state_holds_bytes(&sb, b"old notes, sensitive"));
    assert!(!state_holds_bytes(&sb, &blob[1000..1200]));
    let du: serde_json::Value = serde_json::from_str(&sb.run_ok(&["--json", "tin", "du"])).unwrap();
    // The database and the token, and the hop's two outputs the rollback
    // set aside: what replaced a sealed file is sealed too.
    assert_eq!(du["sealed"], 4, "{du}");
    assert_eq!(du["objects"], 0, "{du}");
    sb.run_ok(&["tin", "verify"]);
}

#[cfg(unix)]
#[test]
fn two_equal_secrets_are_two_objects() {
    let sb = Sandbox::new();
    sb.write("p/.env", "SAME=secret\n");
    sb.write("p/sub/.env", "SAME=secret\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"rm .env sub/.env\"], {cwd: \"p\", touches: [\"**\"]})\n  cough \"boom\"\n}\n",
    );
    sb.run_err(&["run", "s.cig"], 1);
    assert_eq!(sb.read("p/.env"), "SAME=secret\n");
    assert_eq!(sb.read("p/sub/.env"), "SAME=secret\n");
    let objects = sealed_objects(&sb);
    assert_eq!(objects.len(), 2, "{objects:?}");
    assert_ne!(
        fs::read(&objects[0]).unwrap(),
        fs::read(&objects[1]).unwrap(),
        "two equal secrets must not share bytes"
    );
    assert!(!state_holds_bytes(&sb, b"SAME=secret"));
}

#[cfg(unix)]
#[test]
fn a_tampered_sealed_object_refuses_the_restore() {
    let sb = Sandbox::new();
    sb.write("p/.env", "TOKEN=v1\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo TOKEN=v2 > .env\"], {cwd: \"p\", touches: [\"**\"]})\n}\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let objects = sealed_objects(&sb);
    assert_eq!(objects.len(), 1, "{objects:?}");
    let mut bytes = fs::read(&objects[0]).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&objects[0], fs::Permissions::from_mode(0o644)).unwrap();
    }
    fs::write(&objects[0], &bytes).unwrap();
    let shown = sb.cig(&["runs", &id, "--verify"]);
    assert_eq!(shown.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&shown.stdout).contains("does not open with the tin's key"),
        "{}",
        String::from_utf8_lossy(&shown.stdout)
    );
    let err = sb.run_err(&["unburn", &id], 1);
    assert!(
        err.contains("error[E703 burn]") && err.contains("does not open with the tin's key"),
        "{err}"
    );
    assert_eq!(sb.read("p/.env"), "TOKEN=v2\n", "nothing was written");
    let problems = sb.cig(&["tin", "verify"]);
    assert_eq!(problems.status.code(), Some(1));
}

#[cfg(unix)]
#[test]
fn without_the_key_a_restore_refuses_and_names_the_file() {
    let sb = Sandbox::new();
    sb.write("p/.env", "TOKEN=v1\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo TOKEN=v2 > .env\"], {cwd: \"p\", touches: [\"**\"]})\n}\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    // The key is gone: nothing takes its place while sealed objects depend
    // on it, and the restore names the key, where it was, and the file.
    let key = sb.path().join(".cigscript-keys").join("seal.key");
    let aside = sb.home().join("key-aside");
    fs::rename(&key, &aside).unwrap();
    let err = sb.run_err(&["unburn", &id], 1);
    assert!(
        err.contains("error[E703 burn]")
            && err.contains("the tin's key is gone")
            && err.contains("seal.key")
            && err.contains("p/.env"),
        "{err}"
    );
    assert!(!key.exists(), "a new key was made behind the user's back");
    assert_eq!(sb.read("p/.env"), "TOKEN=v2\n");
    // Without the key, "what nothing opens" would be everything: refused.
    let gc: serde_json::Value = serde_json::from_str(&sb.run_ok(&["--json", "tin", "gc"])).unwrap();
    assert_eq!(gc["removed"], 0, "{gc}");
    let err = sb.run_err(&["tin", "gc", "--sealed-orphans"], 1);
    assert!(
        err.contains("E703") && err.contains("needs the tin's key"),
        "{err}"
    );
    assert_eq!(sealed_objects(&sb).len(), 1);
    // The key back: the object opens, is referred to, and stays.
    fs::rename(&aside, &key).unwrap();
    let gc: serde_json::Value =
        serde_json::from_str(&sb.run_ok(&["--json", "tin", "gc", "--sealed-orphans"])).unwrap();
    assert_eq!(gc["removed"], 0, "{gc}");
    sb.run_ok(&["unburn", &id]);
    assert_eq!(sb.read("p/.env"), "TOKEN=v1\n");
}

// ----- the burn protocol (#89): a hop that knows what it changed says so

/// A tool that rewrites a file, keeps the bytes before, and reports what it
/// did through CIG_JOURNAL, honestly or not, as told.
#[cfg(unix)]
const REPORTING_TOOL: &str = r#"import hashlib, json, os, shutil, sys
def report(**line):
    p = os.environ.get("CIG_JOURNAL")
    if p:
        with open(p, "a") as f:
            f.write(json.dumps(line) + "\n")
path, mode = sys.argv[1], sys.argv[2]
before = hashlib.sha256(open(path, "rb").read()).hexdigest()
shutil.copy(path, path + ".orig")
open(path, "w").write("rewritten by the tool\n")
if mode == "honest":
    report(op="write", path=path, before="sha256:" + before, saved=path + ".orig")
elif mode == "liar":
    report(op="write", path=path, before="sha256:" + "0" * 64, saved=path + ".orig")
elif mode == "nocopy":
    os.remove(path + ".orig")
    report(op="write", path=path, before="sha256:" + before)
elif mode == "outside":
    open(sys.argv[3], "w").write("stray\n")
    report(op="write", path=sys.argv[3], before="absent")
elif mode == "undo":
    report(op="compensate", label="migration 0042", undo=["sh", "-c", "echo undone > undone.txt"], cwd=".")
"#;

#[cfg(unix)]
fn reporting_script(mode: &str, extra: &str) -> String {
    format!(
        "pack {{ \"./p\" }}\nburn {{\n  proc.run(\"python3\", [\"../tool.py\", \"db/schema.sql\", \"{mode}\"{extra}], {{cwd: \"p\"}})\n  cough \"boom\"\n}}\n"
    )
}

#[cfg(unix)]
#[test]
fn a_tool_that_reports_its_write_is_reversible_without_touches() {
    let sb = Sandbox::new();
    sb.write("tool.py", REPORTING_TOOL);
    sb.write("p/db/schema.sql", "create table a;\n");
    sb.write("s.cig", &reporting_script("honest", ""));
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert_eq!(sb.read("p/db/schema.sql"), "create table a;\n", "{err}");
    assert!(err.contains("(reported by the hop)"), "{err}");
    assert!(!err.contains("E761"), "{err}");
    assert!(
        !sb.exists("p/db/schema.sql.orig"),
        "the tool's copy was a hop-created file: {err}"
    );
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let shown = sb.run_ok(&["runs", &id]);
    assert!(
        shown.contains("hop modified") && shown.contains("(reported by the hop)"),
        "{shown}"
    );
    assert!(sb.exists(&format!("home/runs/{id}/hops")));
}

#[cfg(unix)]
#[test]
fn a_reported_before_state_that_does_not_match_is_irreversible() {
    let sb = Sandbox::new();
    sb.write("tool.py", REPORTING_TOOL);
    sb.write("p/db/schema.sql", "create table a;\n");
    sb.write("s.cig", &reporting_script("liar", ""));
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert_eq!(
        sb.read("p/db/schema.sql"),
        "rewritten by the tool\n",
        "{err}"
    );
    assert!(
        err.contains("warning[E761 burn]") && err.contains("does not hash to the before it claims"),
        "{err}"
    );
    assert!(err.contains("cannot undo hop modified"), "{err}");
    // A report with no copy of the bytes is honest about being irreversible.
    sb.write("p/db/schema.sql", "create table a;\n");
    sb.write("s.cig", &reporting_script("nocopy", ""));
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert_eq!(sb.read("p/db/schema.sql"), "rewritten by the tool\n");
    assert!(err.contains("kept no copy of the bytes before it"), "{err}");
    assert!(sb.cig(&["explain", "E761"]).status.success());
}

#[cfg(unix)]
#[test]
fn a_reported_compensation_runs_on_rollback() {
    let sb = Sandbox::new();
    sb.write("tool.py", REPORTING_TOOL);
    sb.write("p/db/schema.sql", "create table a;\n");
    sb.write("s.cig", &reporting_script("undo", ""));
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert_eq!(sb.read("p/undone.txt"), "undone\n", "{err}");
    assert!(
        err.contains("compensated ran the compensation for migration 0042"),
        "{err}"
    );
    assert!(
        err.contains("compensated run python3"),
        "the hop is covered: {err}"
    );
    // Deferred: a run that went through is undone later by the same command.
    fs::remove_file(sb.path().join("p/undone.txt")).unwrap();
    sb.write("p/db/schema.sql", "create table a;\n");
    sb.write(
        "t.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"python3\", [\"../tool.py\", \"db/schema.sql\", \"undo\"], {cwd: \"p\"})\n}\n",
    );
    let out = sb.cig(&["run", "t.cig"]);
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains("(0 irreversible)"),
        "the hop is compensated: {text}"
    );
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let plan = sb.run_ok(&["unburn", &id, "--dry-run"]);
    assert!(
        plan.contains("compensate  compensation for migration 0042"),
        "{plan}"
    );
    let done = sb.run_ok(&["unburn", &id]);
    assert!(
        done.contains("ran the compensation for migration 0042"),
        "{done}"
    );
    assert_eq!(sb.read("p/undone.txt"), "undone\n");
}

#[cfg(unix)]
#[test]
fn a_reported_path_outside_the_pack_is_refused() {
    let sb = Sandbox::new();
    sb.write("tool.py", REPORTING_TOOL);
    sb.write("p/db/schema.sql", "create table a;\n");
    let outside = sb.path().join("elsewhere.txt");
    sb.write(
        "s.cig",
        &reporting_script("outside", &format!(", \"{}\"", outside.display())),
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(
        err.contains("warning[E761 burn]") && err.contains("outside the pack"),
        "{err}"
    );
    assert!(err.contains("E752"), "{err}");
    assert_eq!(
        fs::read_to_string(&outside).unwrap(),
        "stray\n",
        "never rolled back"
    );
}

/// cig is the protocol's first speaker: a run a hop started reports its own
/// undo, and the parent's rollback puts it back through its own journal.
#[cfg(unix)]
#[test]
fn a_nested_cig_run_is_undone_by_the_parents_rollback() {
    let sb = Sandbox::new();
    fs::create_dir_all(sb.path().join("p")).unwrap();
    sb.write(
        "p/child.cig",
        "burn {\n  fs.write_text(\"from-child.txt\", \"child\")\n}\n",
    );
    sb.write(
        "s.cig",
        &format!(
            "pack {{ \"./p\" }}\nburn {{\n  proc.run(\"{}\", [\"run\", \"child.cig\"], {{cwd: \"p\"}})\n  cough \"boom\"\n}}\n",
            env!("CARGO_BIN_EXE_cig")
        ),
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(!sb.exists("p/from-child.txt"), "{err}");
    assert!(
        err.contains("ran the compensation for cig run child.cig"),
        "{err}"
    );
    let runs = sb.runs();
    assert_eq!(runs.len(), 2, "{runs:?}");
    let child = runs.iter().find(|r| r["script"] == "child.cig").unwrap();
    assert_eq!(child["status"], "unburned", "{child}");
}

// ----- the parcel (#86): a run's undo on another machine

/// A run on "machine A": a pack at `p`, a hop that edits one file and makes
/// another, and a native write.
#[cfg(unix)]
fn parcelled_run(sb: &Sandbox) -> String {
    sb.write("p/a.txt", "alpha v1\n");
    sb.write("p/c.txt", "gamma v1\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo 'alpha v2' > a.txt; echo beta > b.txt\"], {cwd: \"p\", touches: [\"**\"]})\n  fs.write_text(\"p/c.txt\", \"gamma v2\")\n}\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    sb.runs()[0]["id"].as_str().unwrap().to_string()
}

/// Machine B: another state directory, the pack as the run left it.
#[cfg(unix)]
fn machine_b(a: &Sandbox) -> Sandbox {
    let b = Sandbox::new();
    fs::create_dir_all(b.path().join("p")).unwrap();
    for f in ["a.txt", "b.txt", "c.txt"] {
        fs::copy(a.path().join("p").join(f), b.path().join("p").join(f)).unwrap();
    }
    b
}

#[cfg(unix)]
#[test]
fn a_parcel_unburns_on_another_machine() {
    let a = Sandbox::new();
    let id = parcelled_run(&a);
    let out = a.run_ok(&["parcel", &id, "--out", "run.cigparcel"]);
    assert!(out.trim().ends_with("run.cigparcel"), "{out}");
    let parcel = a.path().join("run.cigparcel");
    let b = machine_b(&a);
    let show = b.run_ok(&["unparcel", "--show", parcel.to_str().unwrap()]);
    assert!(
        show.contains(&format!("run       {id}")) && show.contains("root      p ="),
        "{show}"
    );
    let root = format!("p={}", b.path().join("p").display());
    let imported = b.run_ok(&["unparcel", parcel.to_str().unwrap(), "--root", &root]);
    assert_eq!(imported.trim(), id, "{imported}");
    let shown = b.run_ok(&["runs", &id, "--verify"]);
    assert!(
        shown.contains("imported  from")
            && shown.contains("chain     intact")
            && shown.contains("objects   every snapshot"),
        "{shown}"
    );
    // Signed again under this machine's key on the way in, and its origin
    // recorded: the ledger here has an `origin` line for it.
    let ledger = fs::read_to_string(b.path().join(".cigscript-keys").join("ledger.jsonl"))
        .unwrap_or_default();
    assert!(
        ledger.contains("\"kind\":\"origin\"") && ledger.contains(&id),
        "no origin line for {id} in the ledger: {ledger}"
    );
    let plan = b.run_ok(&["unburn", &id, "--dry-run"]);
    assert!(
        plan.contains("unchanged since") && !plan.contains("changed since:"),
        "{plan}"
    );
    b.run_ok(&["unburn", &id]);
    assert_eq!(b.read("p/a.txt"), "alpha v1\n");
    assert!(!b.exists("p/b.txt"));
    assert_eq!(b.read("p/c.txt"), "gamma v1\n");
    assert_eq!(b.runs()[0]["status"], "unburned");
    // Machine A is untouched, and its own unburn still works.
    assert_eq!(a.read("p/a.txt"), "alpha v2\n");
    a.run_ok(&["unburn", &id]);
    assert_eq!(a.read("p/a.txt"), "alpha v1\n");
    // The same parcel a second time: the run is here already.
    let err = b.run_err(&["unparcel", parcel.to_str().unwrap(), "--root", &root], 1);
    assert!(
        err.contains("E762") && err.contains("already here"),
        "{err}"
    );
}

#[cfg(unix)]
#[test]
fn a_parcel_with_a_wrong_hash_is_refused_whole() {
    let a = Sandbox::new();
    let id = parcelled_run(&a);
    a.run_ok(&["parcel", &id, "--out", "run.cigparcel"]);
    // Change one byte of one object inside the parcel, then pack it again.
    let work = a.path().join("work");
    fs::create_dir_all(&work).unwrap();
    let tar = |args: &[&str]| {
        let out = Command::new("tar")
            .args(args)
            .current_dir(a.path())
            .output()
            .expect("tar");
        assert!(
            out.status.success(),
            "tar {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    tar(&["xzf", "run.cigparcel", "-C", "work"]);
    let mut objects = Vec::new();
    for bucket in fs::read_dir(work.join("tin/objects")).unwrap().flatten() {
        for e in fs::read_dir(bucket.path()).unwrap().flatten() {
            objects.push(e.path());
        }
    }
    assert!(!objects.is_empty());
    let mut bytes = fs::read(&objects[0]).unwrap();
    bytes.push(b'!');
    fs::write(&objects[0], bytes).unwrap();
    tar(&["czf", "bad.cigparcel", "-C", "work", "."]);
    let b = machine_b(&a);
    let root = format!("p={}", b.path().join("p").display());
    let err = b.run_err(
        &[
            "unparcel",
            a.path().join("bad.cigparcel").to_str().unwrap(),
            "--root",
            &root,
        ],
        1,
    );
    assert!(
        err.contains("error[E762 burn]") && err.contains("does not hash as the manifest says"),
        "{err}"
    );
    assert!(b.runs().is_empty(), "nothing was imported");
    assert!(!b.exists("home/tin/objects"), "no object was placed");
}

#[cfg(unix)]
#[test]
fn an_unmapped_root_refuses_with_its_name() {
    let a = Sandbox::new();
    let id = parcelled_run(&a);
    a.run_ok(&["parcel", &id, "--out", "run.cigparcel"]);
    let b = machine_b(&a);
    let err = b.run_err(
        &["unparcel", a.path().join("run.cigparcel").to_str().unwrap()],
        1,
    );
    assert!(
        err.contains("root `p` (") && err.contains("--root p=/path/to/it"),
        "{err}"
    );
    assert!(b.runs().is_empty());
    // A root mapped to a place that is not there is refused too.
    let err = b.run_err(
        &[
            "unparcel",
            a.path().join("run.cigparcel").to_str().unwrap(),
            "--root",
            "p=/nowhere/at/all",
        ],
        1,
    );
    assert!(err.contains("not a directory here"), "{err}");
}

#[cfg(unix)]
#[test]
fn a_parcel_carries_only_the_objects_its_run_needs() {
    let a = Sandbox::new();
    let first = parcelled_run(&a);
    a.write("p/d.txt", "delta v1\n");
    a.write(
        "t.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo 'delta v2' > d.txt\"], {cwd: \"p\", touches: [\"d.txt\"]})\n}\n",
    );
    a.run_ok(&["run", "t.cig"]);
    let second = a.runs()[0]["id"].as_str().unwrap().to_string();
    assert_ne!(first, second);
    let object_of = |id: &str| -> String {
        let shown: serde_json::Value =
            serde_json::from_str(&a.run_ok(&["--json", "runs", id])).unwrap();
        shown["journal"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|e| {
                e["before"]
                    .as_array()?
                    .iter()
                    .find_map(|b| {
                        b["snapshot"]
                            .as_str()
                            .filter(|s| s.contains("/tin/objects/"))
                    })
                    .map(|s| s.rsplit('/').next().unwrap().to_string())
            })
            .unwrap_or_else(|| panic!("{shown}"))
    };
    let (mine, theirs) = (object_of(&first), object_of(&second));
    a.run_ok(&["parcel", &first, "--out", "first.cigparcel"]);
    let listed = Command::new("tar")
        .args(["tzf", "first.cigparcel"])
        .current_dir(a.path())
        .output()
        .expect("tar");
    let names = String::from_utf8_lossy(&listed.stdout).to_string();
    assert!(names.contains(&mine), "{names}");
    assert!(!names.contains(&theirs), "{names}");
    assert!(
        names.contains("manifest.json") && names.contains("run/journal.jsonl"),
        "{names}"
    );
    assert!(!names.contains("run/hops/"), "{names}");
}

// ----- hop watching that scales (#84): no cap past which changes go irreversible

/// Makes `n` small files under `p`, a thousand to a directory.
#[cfg(unix)]
fn big_pack(sb: &Sandbox, n: usize) {
    for d in 0..n.div_ceil(1000) {
        let dir = sb.path().join(format!("p/d{d:04}"));
        fs::create_dir_all(&dir).unwrap();
        let count = (n - d * 1000).min(1000);
        for i in 0..count {
            fs::write(dir.join(format!("f{i:04}.txt")), "x").unwrap();
        }
    }
}

/// 1.4 stopped reading a pack at 50,000 entries and left what a hop created
/// past that point in place. The scan has no such point now.
#[cfg(unix)]
#[test]
fn a_created_file_past_the_old_cap_is_removed_on_rollback() {
    let sb = Sandbox::new();
    big_pack(&sb, 60_000);
    sb.write("p/config.ini", "original\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo CHANGED > config.ini; echo new > d0059/made.txt; rm d0030/f0007.txt\"], {cwd: \"p\", touches: [\"config.ini\"]})\n  cough \"boom\"\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(!err.contains("saw only part of it"), "{err}");
    assert_eq!(sb.read("p/config.ini"), "original\n", "{err}");
    assert!(
        !sb.exists("p/d0059/made.txt"),
        "the created file survived: {err}"
    );
    assert!(err.contains("restored hop created"), "{err}");
    assert!(
        err.contains("d0030/f0007.txt is not under touches"),
        "{err}"
    );
    assert!(
        sb.exists("p/d0030/f0006.txt"),
        "a file that existed was removed"
    );
}

/// Half a million entries, a hop that creates, edits and deletes among
/// them, everything undone. Heavy: `cargo test --release -- --ignored`.
#[cfg(unix)]
#[test]
#[ignore = "heavy: makes 500,000 files; run with --ignored, in release mode"]
fn a_hop_in_a_500k_entry_pack_is_fully_reversible() {
    let sb = Sandbox::new();
    big_pack(&sb, 500_000);
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo new > d0400/made.txt; echo changed > d0200/f0500.txt; rm d0100/f0001.txt\"], {cwd: \"p\", touches: [\"d0200/**\", \"d0100/**\"]})\n  cough \"boom\"\n}\n",
    );
    let started = std::time::Instant::now();
    let err = sb.run_err(&["run", "s.cig"], 1);
    let took = started.elapsed();
    assert!(!err.contains("saw only part of it"), "{err}");
    assert!(!sb.exists("p/d0400/made.txt"), "{err}");
    assert_eq!(sb.read("p/d0200/f0500.txt"), "x", "{err}");
    assert_eq!(sb.read("p/d0100/f0001.txt"), "x", "{err}");
    assert!(
        took < std::time::Duration::from_secs(300),
        "a run over 500k entries took {took:?}"
    );
}

/// The scan of a million entries, before and after a hop, stays under the
/// memory budget: the whole run under 400 MB of resident memory on Linux.
/// Heavy: `cargo test --release -- --ignored`.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "heavy: makes 1,000,000 files; run with --ignored, in release mode"]
fn scan_memory_stays_under_the_budget_for_a_million_entries() {
    let sb = Sandbox::new();
    big_pack(&sb, 1_000_000);
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo new > d0999/made.txt\"], {cwd: \"p\"})\n  cough \"boom\"\n}\n",
    );
    let out = Command::new("/usr/bin/time")
        .args(["-f", "MAXRSS=%M", env!("CARGO_BIN_EXE_cig"), "run", "s.cig"])
        .current_dir(sb.path())
        .env("CIGSCRIPT_HOME", sb.home())
        .env("NO_COLOR", "1")
        .env("CIG_SEAL_KEY", "file")
        .output()
        .expect("time");
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    let rss_kb: u64 = err
        .lines()
        .find_map(|l| l.strip_prefix("MAXRSS="))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or_else(|| panic!("{err}"));
    assert!(!err.contains("saw only part of it"), "{err}");
    assert!(!sb.exists("p/d0999/made.txt"), "{err}");
    assert!(
        rss_kb < 400 * 1024,
        "a million entries took {} MB of resident memory",
        rss_kb / 1024
    );
}

// ----- the box (#85): a failing hop's changes never land

/// The box needs unprivileged user namespaces and overlayfs; where the
/// machine has neither, the hop falls back to the tin and the run says so.
/// These tests exercise the box where it can be made, and the fallback's
/// wording where it cannot.
#[cfg(target_os = "linux")]
fn fell_back(err: &str) -> bool {
    err.contains("the box cannot be made here") || err.contains("the box could not be made here")
}

/// Should the box work on this machine? A kernel of 5.11 or later with
/// overlayfs, where this user can make user and mount namespaces (`unshare
/// -Urm true` succeeds): there a fallback is a defect, not a shrug, and the
/// box tests say so instead of passing through the fallback.
#[cfg(target_os = "linux")]
fn box_expected_here() -> bool {
    let kernel = fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    let mut parts = kernel.trim().split(|c: char| !c.is_ascii_digit());
    let major: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    if (major, minor) < (5, 11) {
        return false;
    }
    let overlay = fs::read_to_string("/proc/filesystems")
        .map(|t| t.lines().any(|l| l.trim_end().ends_with("overlay")))
        .unwrap_or(false);
    if !overlay {
        return false;
    }
    Command::new("unshare")
        .args(["-Urm", "true"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Can `contain` be had here? The box, a user that is not root (a root hop
/// would keep its capabilities), and an architecture the hop's syscall
/// filter is written for.
#[cfg(target_os = "linux")]
fn contain_expected_here() -> bool {
    box_expected_here()
        && unsafe { libc::geteuid() } != 0
        && (cfg!(target_arch = "x86_64") || cfg!(target_arch = "aarch64"))
}

/// Run a script whose hop asks for `contain`; `None` where contain cannot be
/// had here and the run said so (E777), which is the only right answer
/// there.
#[cfg(target_os = "linux")]
fn run_contained(sb: &Sandbox, script: &str) -> Option<(Output, String)> {
    let out = sb.cig(&["run", script]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if !contain_expected_here() {
        assert!(
            err.contains("E777") || out.status.success(),
            "contain where it cannot be had must be refused with E777: {err}"
        );
        return None;
    }
    Some((out, err))
}

/// The fallback ran: allowed only where the box could not have worked.
#[cfg(target_os = "linux")]
fn allow_fallback(err: &str) {
    assert!(
        !box_expected_here(),
        "this machine can make a box (unshare -Urm works, overlayfs is here), yet the hop fell back:\n{err}"
    );
    eprintln!("the box could not be made on this machine; the fallback ran: {err}");
}

#[cfg(target_os = "linux")]
#[test]
fn a_failing_hop_in_a_box_changes_nothing() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "original\n");
    sb.write("p/gone.txt", "doomed\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo changed > keep.txt; rm gone.txt; echo new > made.txt; mkdir sub; echo deep > sub/d.txt; exit 3\"], {cwd: \"p\", box: true, ok: 0})\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    if fell_back(&err) {
        allow_fallback(&err);
        assert!(err.contains("warning[E763 burn]"), "{err}");
        return;
    }
    assert!(err.contains("error[E553 runtime]"), "{err}");
    assert!(
        err.contains("layer was dropped") && err.contains("nothing of it reached the pack"),
        "{err}"
    );
    assert_eq!(sb.read("p/keep.txt"), "original\n");
    assert_eq!(sb.read("p/gone.txt"), "doomed\n");
    assert!(!sb.exists("p/made.txt") && !sb.exists("p/sub"));
    // Nothing of the pack was journaled: the hop alone, and no rollback.
    let rec = &sb.runs()[0];
    assert_eq!(rec["burns"], 1, "{rec}");
    assert!(!err.contains("restored"), "{err}");
    let id = rec["id"].as_str().unwrap();
    assert!(
        !sb.exists(&format!("home/runs/{id}/box/1")),
        "the layer was not dropped"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_succeeding_hop_in_a_box_is_applied_and_unburnable() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "original\n");
    sb.write("p/gone.txt", "doomed\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo changed > keep.txt; rm gone.txt; echo new > made.txt; mkdir sub; echo deep > sub/d.txt; ln -s keep.txt link.txt\"], {cwd: \"p\", box: true})\n}\n",
    );
    let out = sb.cig(&["run", "s.cig"]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{err}");
    if fell_back(&err) {
        allow_fallback(&err);
        return;
    }
    assert!(
        err.contains("box: `sh -c") && err.contains("applied: 5 paths written, 1 removed"),
        "{err}"
    );
    assert_eq!(sb.read("p/keep.txt"), "changed\n");
    assert!(!sb.exists("p/gone.txt"));
    assert_eq!(sb.read("p/made.txt"), "new\n");
    assert_eq!(sb.read("p/sub/d.txt"), "deep\n");
    assert_eq!(
        fs::read_link(sb.path().join("p/link.txt")).unwrap(),
        Path::new("keep.txt")
    );
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let shown = sb.run_ok(&["runs", &id, "--verify"]);
    assert!(
        shown.contains("write") && shown.contains("delete") && shown.contains("mkdir"),
        "{shown}"
    );
    assert!(shown.contains("chain     intact"), "{shown}");
    sb.run_ok(&["unburn", &id]);
    assert_eq!(sb.read("p/keep.txt"), "original\n");
    assert_eq!(sb.read("p/gone.txt"), "doomed\n");
    assert!(!sb.exists("p/made.txt") && !sb.exists("p/sub") && !sb.exists("p/link.txt"));
}

#[cfg(target_os = "linux")]
#[test]
fn a_boxed_hop_that_deletes_and_replaces_folders_applies_correctly() {
    let sb = Sandbox::new();
    sb.write("p/dir/a.txt", "a\n");
    sb.write("p/dir/b.txt", "b\n");
    sb.write("p/dir/sub/c.txt", "c\n");
    sb.write("p/other/x.txt", "x\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"rm -rf other; rm -rf dir; mkdir dir; echo fresh > dir/new.txt\"], {cwd: \"p\", box: true})\n}\n",
    );
    let out = sb.cig(&["run", "s.cig"]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{err}");
    if fell_back(&err) {
        allow_fallback(&err);
        return;
    }
    assert!(!sb.exists("p/other"), "{err}");
    assert_eq!(sb.read("p/dir/new.txt"), "fresh\n");
    assert!(
        !sb.exists("p/dir/a.txt") && !sb.exists("p/dir/b.txt") && !sb.exists("p/dir/sub"),
        "the replaced folder kept old entries: {err}"
    );
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    sb.run_ok(&["unburn", &id]);
    assert_eq!(sb.read("p/dir/a.txt"), "a\n");
    assert_eq!(sb.read("p/dir/b.txt"), "b\n");
    assert_eq!(sb.read("p/dir/sub/c.txt"), "c\n");
    assert_eq!(sb.read("p/other/x.txt"), "x\n");
    assert!(!sb.exists("p/dir/new.txt"));
}

#[cfg(unix)]
#[test]
fn when_the_box_cannot_be_made_the_hop_falls_back_to_the_tin_and_says_so() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "original\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo changed > keep.txt; echo new > made.txt\"], {cwd: \"p\", box: true, touches: [\"**\"]})\n  cough \"boom\"\n}\n",
    );
    let dry = sb.cig_env(
        &["run", "--dry-run", "s.cig"],
        &[("CIG_INTERNAL_NO_BOX", "1")],
    );
    let plan = String::from_utf8_lossy(&dry.stderr);
    assert!(
        plan.contains("(boxed)") && plan.contains("the box cannot be made here"),
        "{plan}"
    );
    let out = sb.cig_env(&["run", "s.cig"], &[("CIG_INTERNAL_NO_BOX", "1")]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(
        err.contains("warning[E763 burn]: the box cannot be made here")
            && err.contains("runs on the tin instead"),
        "{err}"
    );
    // The tin did its usual work.
    assert_eq!(sb.read("p/keep.txt"), "original\n", "{err}");
    assert!(!sb.exists("p/made.txt"), "{err}");
    assert!(sb.cig(&["explain", "E763"]).status.success());
}

// ----- 1.6.1: the debugging pass over the tin, end to end -------------------------

#[cfg(unix)]
#[test]
fn a_hops_compensation_counts_without_a_pack() {
    let sb = Sandbox::new();
    sb.write(
        "tool.sh",
        "#!/bin/sh\nif [ \"$1\" = undo ]; then echo undone > undone.txt; exit 0; fi\nprintf '%s\\n' '{\"op\":\"compensate\",\"label\":\"undo tool\",\"undo\":[\"sh\",\"tool.sh\",\"undo\"]}' >> \"$CIG_JOURNAL\"\nprintf '%s\\n' '{\"op\":\"write\",\"path\":\"x.txt\",\"before\":\"absent\"}' >> \"$CIG_JOURNAL\"\necho did > did.txt\necho hi > x.txt\n",
    );
    sb.write(
        "s.cig",
        "burn {\n  proc.run(\"sh\", [\"tool.sh\"], {})\n  cough \"boom\"\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    // The hop's own undo needs no pack: it ran on the rollback.
    assert!(err.contains("ran the compensation for undo tool"), "{err}");
    assert!(sb.exists("undone.txt"), "{err}");
    // A file claim has nothing to be judged against: said, and dropped.
    assert!(
        err.contains("warning[E761 burn]") && err.contains("declares no pack"),
        "{err}"
    );
    assert!(sb.exists("x.txt"));
}

#[cfg(unix)]
#[test]
fn a_chatty_undo_neither_hangs_the_rollback_nor_floods_the_report() {
    let sb = Sandbox::new();
    fs::create_dir_all(sb.path().join("p")).unwrap();
    // 300 KB on stdout: more than any pipe buffers.
    sb.write(
        "chatty.sh",
        "#!/bin/sh\nif [ \"$1\" = undo ]; then head -c 300000 /dev/zero | tr '\\0' x; echo undone > undone.txt; exit 0; fi\nprintf '%s\\n' '{\"op\":\"compensate\",\"undo\":[\"sh\",\"chatty.sh\",\"undo\"]}' >> \"$CIG_JOURNAL\"\n",
    );
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"chatty.sh\"], {touches: [\"**\"]})\n  cough \"boom\"\n}\n",
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_cig"))
        .args(["run", "s.cig"])
        .current_dir(sb.path())
        .env("CIGSCRIPT_HOME", sb.home())
        .env("NO_COLOR", "1")
        .env("CIG_SEAL_KEY", "file")
        .env_remove("CI")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use wait_timeout::ChildExt;
    let Some(status) = child
        .wait_timeout(std::time::Duration::from_secs(120))
        .unwrap()
    else {
        let _ = child.kill();
        panic!("the rollback hung on an undo that wrote 300 KB to stdout");
    };
    let mut err = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut err).unwrap();
    assert_eq!(status.code(), Some(1), "{err}");
    assert!(err.contains("ran the compensation for"), "{err}");
    assert!(sb.exists("undone.txt"), "{err}");
    // A failing undo with a 3000-character last line: quoted, not pasted.
    sb.write(
        "loud.sh",
        "#!/bin/sh\nif [ \"$1\" = undo ]; then head -c 3000 /dev/zero | tr '\\0' y >&2; echo >&2; exit 3; fi\nprintf '%s\\n' '{\"op\":\"compensate\",\"undo\":[\"sh\",\"loud.sh\",\"undo\"]}' >> \"$CIG_JOURNAL\"\n",
    );
    sb.write(
        "t.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"loud.sh\"], {touches: [\"**\"]})\n  cough \"boom\"\n}\n",
    );
    let out = sb.cig(&["run", "t.cig"]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    let line = err
        .lines()
        .find(|l| l.contains("NOT undone"))
        .unwrap_or_else(|| panic!("{err}"));
    assert!(
        line.contains("exited 3") && line.contains("more characters)"),
        "{line}"
    );
    assert!(
        line.len() < 700,
        "the report pasted the whole line: {} characters",
        line.len()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_boxed_hop_that_empties_and_remakes_a_folder_leaves_it_in_place() {
    let sb = Sandbox::new();
    sb.write("p/d/old.txt", "old\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"rm -rf d; mkdir d\"], {cwd: \"p\", box: true})\n}\n",
    );
    let out = sb.cig(&["run", "s.cig"]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{err}");
    if fell_back(&err) {
        allow_fallback(&err);
        return;
    }
    assert!(err.contains("applied: 1 path written, 1 removed"), "{err}");
    let d = sb.path().join("p/d");
    assert!(d.is_dir(), "the emptied folder is gone: {err}");
    assert_eq!(fs::read_dir(&d).unwrap().count(), 0);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    // The layers went with the hop, so the record can be pruned.
    assert!(
        !sb.home().join("runs").join(&id).join("box").exists(),
        "the box's layers were left under the run"
    );
    sb.run_ok(&["unburn", &id]);
    assert_eq!(sb.read("p/d/old.txt"), "old\n");
    sb.run_ok(&["runs", "--prune", "0"]);
    assert_eq!(
        fs::read_dir(sb.home().join("runs"))
            .map(|r| r.count())
            .unwrap_or(0),
        0
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_boxed_hops_touches_sealed_seals_what_it_rewrote() {
    let sb = Sandbox::new();
    sb.write("p/data/customers.db", "old-rows-not-a-secret-name\n");
    sb.write("p/plain.txt", "plain words\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo new-rows > data/customers.db; echo y > plain.txt\"], {cwd: \"p\", box: true, touches_sealed: [\"data/*.db\"]})\n  cough \"boom\"\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    if fell_back(&err) {
        allow_fallback(&err);
        return;
    }
    assert!(err.contains("customers.db  (sealed)"), "{err}");
    assert_eq!(
        sb.read("p/data/customers.db"),
        "old-rows-not-a-secret-name\n"
    );
    assert_eq!(sb.read("p/plain.txt"), "plain words\n");
    assert!(!state_holds_bytes(&sb, b"old-rows-not-a-secret-name"));
    // The plain file's before-bytes are kept as a native write's are.
    assert!(state_holds_bytes(&sb, b"plain words\n"));
}

#[cfg(unix)]
#[test]
fn a_parcel_written_from_an_imported_run_still_unburns_on_a_third_machine() {
    let a = Sandbox::new();
    let id = parcelled_run(&a);
    a.run_ok(&["parcel", &id, "--out", "run.cigparcel"]);
    let first = a.path().join("run.cigparcel");
    let b = machine_b(&a);
    let root_b = format!("p={}", b.path().join("p").display());
    b.run_ok(&["unparcel", first.to_str().unwrap(), "--root", &root_b]);
    // B passes the run on as it stands; C undoes it.
    b.run_ok(&["parcel", &id, "--out", "again.cigparcel"]);
    let again = b.path().join("again.cigparcel");
    let c = machine_b(&a);
    let root_c = format!("p={}", c.path().join("p").display());
    c.run_ok(&["unparcel", again.to_str().unwrap(), "--root", &root_c]);
    let shown = c.run_ok(&["runs", &id, "--verify"]);
    assert!(
        shown.contains("chain     intact") && shown.contains("objects   every snapshot"),
        "{shown}"
    );
    let plan = c.run_ok(&["unburn", &id, "--dry-run"]);
    assert!(
        plan.contains("unchanged since") && !plan.contains("changed since:"),
        "{plan}"
    );
    c.run_ok(&["unburn", &id]);
    assert_eq!(c.read("p/a.txt"), "alpha v1\n");
    assert!(!c.exists("p/b.txt"));
    assert_eq!(c.read("p/c.txt"), "gamma v1\n");
    // B undoes it, gives the hop's output back, and passes on a journal
    // that names two machines' paths; D reads both through its map.
    b.run_ok(&["unburn", &id]);
    assert_eq!(b.read("p/a.txt"), "alpha v1\n");
    b.run_ok(&["unburn", &id, "--give-back"]);
    assert_eq!(b.read("p/a.txt"), "alpha v2\n");
    b.run_ok(&["parcel", &id, "--out", "third.cigparcel"]);
    let third = b.path().join("third.cigparcel");
    let d = machine_b(&a);
    let root_d = format!("p={}", d.path().join("p").display());
    d.run_ok(&["unparcel", third.to_str().unwrap(), "--root", &root_d]);
    let shown = d.run_ok(&["runs", &id, "--verify"]);
    assert!(
        shown.contains("status    unburned")
            && shown.contains("chain     intact")
            && shown.contains("objects   every snapshot"),
        "{shown}"
    );
}

#[cfg(unix)]
#[test]
fn a_pipeline_with_touches_is_reversible_like_a_hop() {
    let sb = Sandbox::new();
    sb.write("q/a.txt", "orig\n");
    sb.write(
        "q/tool.sh",
        "#!/bin/sh\nprintf '%s\\n' '{\"op\":\"compensate\",\"label\":\"pipe undo\",\"undo\":[\"sh\",\"-c\",\"echo undone > undone.txt\"]}' >> \"$CIG_JOURNAL\"\necho new > a.txt; echo made > made.txt; echo piped\n",
    );
    sb.write(
        "s.cig",
        "pack { \"./q\" }\nburn {\n  stick r = proc.pipe([[\"sh\", \"tool.sh\"], [\"cat\"]], {cwd: \"q\", touches: [\"a.txt\"]})\n  if r.out.trim() != \"piped\" { cough \"the pipeline lost its output\" }\n  cough \"boom\"\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(
        err.contains("restored hop modified") && err.contains("restored hop created"),
        "{err}"
    );
    assert_eq!(sb.read("q/a.txt"), "orig\n");
    assert!(!sb.exists("q/made.txt"));
    // Every stage reports through the burn protocol, as one hop does.
    assert!(err.contains("ran the compensation for pipe undo"), "{err}");
    assert!(sb.exists("q/undone.txt"), "{err}");
    // A pipeline is several processes: it cannot be boxed, and says so.
    sb.write(
        "b.cig",
        "pack { \"./q\" }\nburn {\n  proc.pipe([[\"true\"]], {box: true})\n}\n",
    );
    let out = sb.cig(&["run", "b.cig"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && err.contains("E400") && err.contains("cannot be boxed"),
        "{err}"
    );
    // And `touches` without a pack is the same refusal a hop gets.
    sb.write(
        "t.cig",
        "burn {\n  proc.pipe([[\"true\"]], {touches: [\"**\"]})\n}\n",
    );
    let out = sb.cig(&["run", "t.cig"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success() && err.contains("E757"), "{err}");
}

#[cfg(unix)]
#[test]
fn a_missing_seal_key_is_refused_rather_than_replaced() {
    let sb = Sandbox::new();
    sb.write("p/cfg/.env", "K=v\n");
    sb.write("p/other/.env", "X=0\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo K=w > cfg/.env\"], {cwd: \"p\", touches: [\"cfg/**\"]})\n}\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let key = sb.path().join(".cigscript-keys").join("seal.key");
    let aside = sb.path().join(".cigscript-keys").join("key-aside");
    fs::rename(&key, &aside).unwrap();
    // The restore names the missing key; it makes no new one.
    let out = sb.cig(&["unburn", &id]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        !out.status.success()
            && err.contains("E703")
            && err.contains("a new key would lock them out for good"),
        "{err}"
    );
    assert!(!key.exists(), "a new key was made behind the user's back");
    assert_eq!(sb.read("p/cfg/.env"), "K=w\n");
    // A run that would seal says so and keeps the change irreversible.
    sb.write(
        "t.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo X=1 > other/.env\"], {cwd: \"p\", touches: [\"other/**\"]})\n}\n",
    );
    let out = sb.cig(&["run", "t.cig"]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        out.status.success()
            && err.contains("warning[E757 burn]")
            && err.contains("could not be sealed")
            && err.contains("2 irreversible"),
        "{err}"
    );
    assert!(!key.exists(), "a new key was made behind the user's back");
    // The key back: the first run unburns.
    fs::rename(&aside, &key).unwrap();
    sb.run_ok(&["unburn", &id]);
    assert_eq!(sb.read("p/cfg/.env"), "K=v\n");
}

/// #105: `--seal-key-fd N` hands the tin's key over on a descriptor. A run
/// that seals burns and unburns with it, no key file is ever made, a hop
/// cannot read the descriptor (cig closed it), the key on stdin works too,
/// and a descriptor with nothing open, or no key on it, is a usage error.
#[cfg(unix)]
#[test]
fn a_key_handed_over_on_a_descriptor_seals_and_is_never_written() {
    let sb = Sandbox::new();
    let hexkey = "5a".repeat(32);
    sb.write("key.hex", &format!("{hexkey}\n"));
    sb.write("not-a-key", "hello\n");
    sb.write(".env", "SECRET=one\n");
    sb.write(
        "s.cig",
        "pack { \".\" }\nburn {\n  fs.write_text(\".env\", \"SECRET=two\")\n  stick r = proc.run(\"sh\", [\"-c\", \"cat <&3 && echo LEAKED || echo closed\"])\n  exhale r.out\n}\n",
    );
    // cig with a redirection the shell makes before exec, as CI would.
    let cig_fd = |redirect: &str, args: &[&str]| -> Output {
        Command::new("sh")
            .arg("-c")
            .arg(format!("exec \"$0\" \"$@\" {redirect}"))
            .arg(env!("CARGO_BIN_EXE_cig"))
            .args(args)
            .current_dir(sb.path())
            .env("CIGSCRIPT_HOME", sb.home())
            .env("NO_COLOR", "1")
            .env("CIG_SEAL_KEY", "file")
            .env("CIG_INTERNAL_TRACKER_DIR", sb.path().join("tracker"))
            .env("CIG_UPDATE_MANIFEST", sb.path().join("no-releases.json"))
            .env_remove("CI")
            .output()
            .expect("run cig")
    };
    let text = |o: &Output| {
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    };
    let key_file = sb.path().join(".cigscript-keys").join("seal.key");
    let out = cig_fd("3<key.hex", &["--seal-key-fd", "3", "run", "s.cig"]);
    assert!(out.status.success(), "{}", text(&out));
    let said = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        said.contains("closed") && !said.contains("LEAKED") && !said.contains(&hexkey),
        "a hop read the key's descriptor: {said}"
    );
    assert_eq!(sb.read(".env"), "SECRET=two");
    assert!(
        !key_file.exists(),
        "a key file was made: {}",
        key_file.display()
    );
    fn sealed_under(dir: &Path) -> bool {
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.to_string_lossy().ends_with(".cig-sealed") {
                    return true;
                }
            }
        }
        false
    }
    assert!(sealed_under(&sb.home()), "nothing was sealed");
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    // Without the key nothing opens, and no key is made in its place.
    let out = sb.cig(&["unburn", &id]);
    assert!(!out.status.success(), "{}", text(&out));
    assert!(
        !key_file.exists(),
        "a key file was made: {}",
        key_file.display()
    );
    assert_eq!(sb.read(".env"), "SECRET=two");
    // Doctor names the keeper.
    let out = cig_fd("3<key.hex", &["doctor", "--seal-key-fd", "3"]);
    assert!(
        text(&out).contains("handed over on a descriptor"),
        "{}",
        text(&out)
    );
    // The key on stdin puts the run back.
    let out = cig_fd("0<key.hex", &["--seal-key-fd", "0", "unburn", &id]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(sb.read(".env"), "SECRET=one\n");
    assert!(
        !key_file.exists(),
        "a key file was made: {}",
        key_file.display()
    );
    // Nothing open, no key on it, or cig's own stream: usage errors.
    for (redirect, fd, says) in [
        ("", "9", "nothing is open on descriptor 9"),
        ("3<not-a-key", "3", "did not hold a tin key"),
        ("", "1", "cig's own output"),
        ("", "x", "invalid value"),
    ] {
        let out = cig_fd(redirect, &["--seal-key-fd", fd, "run", "s.cig"]);
        assert_eq!(out.status.code(), Some(3), "{}", text(&out));
        assert!(text(&out).contains(says), "{says}: {}", text(&out));
    }
    assert_eq!(sb.read(".env"), "SECRET=one\n");
}

#[test]
fn a_script_pinned_below_the_box_or_sealed_touches_is_caught() {
    let sb = Sandbox::new();
    fs::create_dir_all(sb.path().join("p")).unwrap();
    sb.write(
        "b.cig",
        "# cig >=1.5.0\npack { \"./p\" }\nburn { proc.run(\"true\", [], {box: true}) }\n",
    );
    let err = sb.run_err(&["check", "--deny-warnings", "b.cig"], 2);
    assert!(
        err.contains("warning[E309 check]") && err.contains("box: true") && err.contains("1.6.0"),
        "{err}"
    );
    sb.write(
        "t.cig",
        "# cig >=1.4.0\npack { \"./p\" }\nburn { proc.run(\"true\", [], {touches_sealed: [\"*.db\"]}) }\n",
    );
    let err = sb.run_err(&["check", "--deny-warnings", "t.cig"], 2);
    assert!(
        err.contains("touches_sealed") && err.contains("1.5.0"),
        "{err}"
    );
    sb.write(
        "w.cig",
        "burn { fs.write_text(\"a.txt\", \"b\", {sensitive: true}) }\n",
    );
    sb.run_ok(&["pin", "w.cig"]);
    assert!(
        sb.read("w.cig").starts_with("# cig >=1.5.0\n"),
        "{}",
        sb.read("w.cig")
    );
}

#[test]
fn a_changed_last_journal_line_refuses_the_unburn() {
    let sb = Sandbox::new();
    sb.write("p/t.txt", "t1\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  fs.write_text(\"p/t.txt\", \"t2\")\n}\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    // The last line has no line after it to carry its hash; the record's
    // head is what catches a change to it.
    let journal = sb.home().join("runs").join(&id).join("journal.jsonl");
    let text = fs::read_to_string(&journal).unwrap();
    assert!(text.contains("\"seq\":1,"), "{text}");
    fs::write(&journal, text.replacen("\"seq\":1,", "\"seq\":1 ,", 1)).unwrap();
    let out = sb.cig(&["runs", &id, "--verify"]);
    let shown = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{shown}");
    assert!(
        shown.contains("chain     intact") && shown.contains("head      does not match the record"),
        "{shown}"
    );
    let err = sb.run_err(&["unburn", &id], 1);
    assert!(
        err.contains("error[E760 burn]") && err.contains("chain's head"),
        "{err}"
    );
    assert_eq!(sb.read("p/t.txt"), "t2", "the refusal restored anyway");
    let plan = sb.run_ok(&["unburn", &id, "--dry-run"]);
    assert!(plan.contains("would refuse [E760]"), "{plan}");
    sb.run_ok(&["unburn", &id, "--force"]);
    assert_eq!(sb.read("p/t.txt"), "t1\n");
    // The give-back reads the took markers from the journal, and refuses a
    // journal that is not the record the same way. A hop's output the
    // rollback set aside is what a give-back returns.
    sb.write("q/h.txt", "h1\n");
    sb.write(
        "h.cig",
        "pack { \"./q\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo h2 > h.txt\"], {cwd: \"q\", touches: [\"**\"]})\n  cough \"boom\"\n}\n",
    );
    sb.run_err(&["run", "h.cig"], 1);
    assert_eq!(sb.read("q/h.txt"), "h1\n");
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    // The automatic rollback saved the head after its own markers.
    sb.run_ok(&["runs", &id, "--verify"]);
    let journal = sb.home().join("runs").join(&id).join("journal.jsonl");
    let text = fs::read_to_string(&journal).unwrap();
    let last = text.trim_end().lines().last().unwrap().to_string();
    fs::write(&journal, text.replacen(&last, &format!("{last} "), 1)).unwrap();
    let err = sb.run_err(&["unburn", &id, "--give-back"], 1);
    assert!(
        err.contains("error[E760 burn]") && err.contains("chain's head"),
        "{err}"
    );
    assert_eq!(sb.read("q/h.txt"), "h1\n", "the refusal gave back anyway");
    sb.run_ok(&["unburn", &id, "--give-back", "--force"]);
    assert_eq!(sb.read("q/h.txt"), "h2\n");
}

#[cfg(target_os = "linux")]
#[test]
fn a_boxed_hops_writes_under_a_build_tree_pass_through() {
    let sb = Sandbox::new();
    sb.write("p/target/old.o", "old\n");
    sb.write("p/node_modules/dep/index.js", "1\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"for i in 1 2 3; do echo $i > target/$i.o; done; echo changed > target/old.o; rm -r node_modules/dep; echo built > out.txt\"], {cwd: \"p\", box: true})\n  cough \"boom\"\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    if fell_back(&err) {
        allow_fallback(&err);
        return;
    }
    // One path came through the box; the trees hop watching never looks
    // into are not the box's business either, and keep what the hop did.
    assert!(err.contains("applied: 1 path written, 0 removed"), "{err}");
    assert!(!sb.exists("p/out.txt"));
    assert_eq!(sb.read("p/target/old.o"), "changed\n");
    assert!(sb.exists("p/target/3.o"));
    assert!(!sb.exists("p/node_modules/dep"));
}

// ----- 1.6.2: what cig-orchestrator's suite found running on 1.6.1 -------------------

#[cfg(unix)]
#[test]
fn a_rollback_removes_a_hop_created_tree_with_unreadable_directories() {
    let sb = Sandbox::new();
    fs::create_dir_all(sb.path().join("p")).unwrap();
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"mkdir -p made/inner/deep; echo x > made/inner/deep/f; chmod 000 made/inner/deep; chmod 000 made/inner\"], {cwd: \"p\", box: false})\n  cough \"boom\"\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(err.contains("restored hop created"), "{err}");
    assert!(!err.contains("Permission denied"), "{err}");
    assert!(
        !sb.exists("p/made"),
        "the unreadable tree survived the rollback: {err}"
    );
}

#[cfg(unix)]
#[test]
fn the_layers_an_interrupted_boxed_hop_left_go_with_the_unburn() {
    let sb = Sandbox::new();
    sb.write("p/a.txt", "v1\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  fs.write_text(\"p/a.txt\", \"v2\")\n}\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    // What overlayfs leaves when the hop dies: a work directory nobody can
    // read, under the run's box layers.
    let work = sb.home().join("runs").join(&id).join("box/3/0/work/work");
    fs::create_dir_all(&work).unwrap();
    fs::create_dir_all(sb.home().join("runs").join(&id).join("box/3/0/upper")).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&work, fs::Permissions::from_mode(0o000)).unwrap();
    }
    sb.run_ok(&["unburn", &id]);
    assert_eq!(sb.read("p/a.txt"), "v1\n");
    assert!(
        !sb.home().join("runs").join(&id).join("box").exists(),
        "the layers stayed under the run"
    );
    // The record prunes afterwards as any other.
    sb.run_ok(&["runs", "--prune", "0"]);
    assert_eq!(
        fs::read_dir(sb.home().join("runs"))
            .map(|r| r.count())
            .unwrap_or(0),
        0
    );
}

#[cfg(unix)]
#[test]
fn the_layers_a_dead_run_left_are_swept_when_the_next_run_starts() {
    let sb = Sandbox::new();
    sb.write("p/a.txt", "v1\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  fs.write_text(\"p/a.txt\", \"v2\")\n}\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let layers = sb.home().join("runs").join(&id).join("box");
    let work = layers.join("2/0/work/work");
    fs::create_dir_all(&work).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&work, fs::Permissions::from_mode(0o000)).unwrap();
    }
    // Another run, of anything: the dead run's layers are gone with it.
    sb.write("t.cig", "exhale \"hello\"\n");
    sb.run_ok(&["run", "t.cig"]);
    assert!(!layers.exists(), "the layers stayed under the dead run");
    assert_eq!(sb.read("p/a.txt"), "v2");
}

// ----- 1.6.3: the box does not nest, and says so before the run -----------------------

#[cfg(target_os = "linux")]
#[test]
fn a_box_inside_a_box_is_refused_in_the_plan_with_the_reason() {
    let sb = Sandbox::new();
    sb.write("p/q/f.txt", "v1\n");
    sb.write(
        "p/inner.cig",
        "pack { \"./q\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo v2 > f.txt\"], {cwd: \"q\", box: true})\n}\n",
    );
    // The inner cig's home sits inside the outer box's layer: its own layers
    // would be an upper directory on an overlay.
    let home2 = sb.path().join("p/home2");
    let path = format!(
        "{}:{}",
        Path::new(env!("CARGO_BIN_EXE_cig"))
            .parent()
            .unwrap()
            .display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let outer = String::from("pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"cig run --dry-run inner.cig > plan.txt 2>&1; cig run inner.cig > run.txt 2>&1\"], {cwd: \"p\", box: true, env: {CIGSCRIPT_HOME: \"HOME2\", CIG_SEAL_KEY: \"file\", NO_COLOR: \"1\", PATH: \"PATH2\"}})\n}\n")
        .replace("HOME2", &home2.display().to_string())
        .replace("PATH2", &path);
    sb.write("outer.cig", &outer);
    let out = sb.cig(&["run", "outer.cig"]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{err}");
    if fell_back(&err) {
        allow_fallback(&err);
        return;
    }
    let plan = sb.read("p/plan.txt");
    assert!(
        plan.contains("the box cannot be made here")
            && plan.contains("on an overlay filesystem")
            && plan.contains("another box"),
        "{plan}"
    );
    let run = sb.read("p/run.txt");
    assert!(
        run.contains("warning[E763 burn]") && run.contains("on an overlay filesystem"),
        "{run}"
    );
    assert_eq!(sb.read("p/q/f.txt"), "v2\n");
}

// ----- 1.7.0: the tool reports its own bugs (#101) ----------------------------------

/// A failed run to report: one burn, then a cough.
fn failed_run(sb: &Sandbox) -> String {
    sb.write(
        "s.cig",
        "burn {\n  fs.write_text(\"a.txt\", \"1\")\n  cough \"boom\"\n}\n",
    );
    sb.run_err(&["run", "s.cig"], 1);
    sb.runs()[0]["id"].as_str().unwrap().to_string()
}

/// Wait for the background flush to have filed `n` reports.
#[cfg(unix)]
fn wait_filed(sb: &Sandbox, n: usize) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        let v: serde_json::Value =
            serde_json::from_str(&sb.run_ok(&["--json", "crash", "list"])).unwrap();
        let filed = v
            .as_array()
            .map(|a| a.iter().filter(|r| r["issue"].is_object()).count())
            .unwrap_or(0);
        if filed >= n {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "only {filed} of {n} reports were filed in time: {v}"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn issue_file(tracker: &Path, number: u64) -> serde_json::Value {
    let text = fs::read_to_string(tracker.join(format!("otmof-ops__cig-script/{number}.json")))
        .unwrap_or_else(|e| {
            let mut listing = Vec::new();
            fn walk(d: &Path, out: &mut Vec<String>) {
                for e in fs::read_dir(d).into_iter().flatten().flatten() {
                    out.push(e.path().display().to_string());
                    if e.path().is_dir() {
                        walk(&e.path(), out);
                    }
                }
            }
            walk(tracker, &mut listing);
            panic!("issue {number}: {e}; the tracker holds: {listing:?}")
        });
    serde_json::from_str(&text).unwrap()
}

#[test]
fn report_send_files_the_bundle() {
    let sb = Sandbox::new();
    let id = failed_run(&sb);
    let tracker = sb.path().join("tracker");
    let t = tracker.to_string_lossy().to_string();
    let out = sb.cig_env(
        &["report", &id, "--send"],
        &[("CIG_INTERNAL_TRACKER_DIR", &t)],
    );
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{err}");
    assert!(err.contains("filed at file://"), "{err}");
    let issue = issue_file(&tracker, 1);
    assert!(
        issue["title"].as_str().unwrap().starts_with("report ["),
        "{issue}"
    );
    let body = issue["body"].as_str().unwrap();
    assert!(
        body.contains("Run bundle filed with `cig report --send`")
            && body.contains("\"kind\": \"cigscript-report\"")
            && body.contains("<!-- cig-stats"),
        "{body}"
    );
    assert!(issue["labels"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l == "report"));
    let list = sb.run_ok(&["crash", "list"]);
    assert!(list.contains("report") && list.contains("filed"), "{list}");
    // An explicit never sends nothing, whatever was typed.
    sb.run_ok(&["config", "reports", "never"]);
    let out = sb.cig_env(
        &["report", &id, "--send"],
        &[("CIG_INTERNAL_TRACKER_DIR", &t)],
    );
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("reports is \"never\""));
    assert!(!tracker.join("otmof-ops__cig-script/2.json").exists());
}

#[test]
fn a_report_made_offline_is_sent_on_the_next_run() {
    let sb = Sandbox::new();
    let id = failed_run(&sb);
    let tracker = sb.path().join("tracker");
    let t = tracker.to_string_lossy().to_string();
    // Offline: the send fails, the report waits with the reason written on it.
    let out = sb.cig_env(
        &["report", &id, "--send"],
        &[
            ("CIG_INTERNAL_TRACKER_DIR", &t),
            ("CIG_INTERNAL_OFFLINE", "1"),
        ],
    );
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        out.status.success() && err.contains("waits in the ashtray"),
        "{err}"
    );
    assert!(!tracker.exists());
    let list = sb.run_ok(&["crash", "list"]);
    assert!(
        list.contains("failed: the network")
            && list.contains("last try: the network is unreachable"),
        "{list}"
    );
    // Back online: a run of anything sends it in the background, and the
    // run itself does not wait.
    sb.write("t.cig", "exhale \"hello\"\n");
    let out = sb.cig_env(&["run", "t.cig"], &[("CIG_INTERNAL_TRACKER_DIR", &t)]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let issue = tracker.join("otmof-ops__cig-script/1.json");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        if issue.exists()
            && sb
                .run_ok(&["--json", "crash", "list"])
                .contains("\"issue\"")
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(
        issue.exists(),
        "the background flush did not file the report"
    );
    let list = sb.run_ok(&["crash", "list"]);
    assert!(list.contains("filed"), "{list}");
    // Nothing left to send.
    let out = sb.cig_env(
        &["--json", "crash", "flush"],
        &[("CIG_INTERNAL_TRACKER_DIR", &t)],
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["filed"].as_array().map(Vec::len), Some(0), "{v}");
    assert_eq!(v["failed"].as_array().map(Vec::len), Some(0), "{v}");
}

#[test]
fn ci_never_files_without_the_setting() {
    let sb = Sandbox::new();
    let id = failed_run(&sb);
    let tracker = sb.path().join("tracker");
    let t = tracker.to_string_lossy().to_string();
    // Under CI with nothing set: never, and the message says what would change it.
    let out = sb.cig_env(
        &["report", &id, "--send"],
        &[("CIG_INTERNAL_TRACKER_DIR", &t), ("CI", "true")],
    );
    assert_eq!(out.status.code(), Some(3));
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        err.contains("reports is \"never\" under CI unless `cig config reports always`"),
        "{err}"
    );
    assert!(!tracker.exists());
    assert_eq!(sb.run_ok(&["--json", "crash", "list"]).trim(), "[]");
    // The setting, made explicit, holds under CI.
    sb.run_ok(&["config", "reports", "always"]);
    let out = sb.cig_env(
        &["report", &id, "--send"],
        &[("CIG_INTERNAL_TRACKER_DIR", &t), ("CI", "true")],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(tracker.join("otmof-ops__cig-script/1.json").exists());
    // And `reports` is the one setting: the older name still reads.
    sb.run_ok(&["config", "reports", "--unset"]);
    sb.run_ok(&["config", "crash_reports", "never"]);
    let out = sb.cig_env(
        &["report", &id, "--send"],
        &[("CIG_INTERNAL_TRACKER_DIR", &t)],
    );
    assert_eq!(out.status.code(), Some(3));
}

/// After a burn, objects drawn at random from the whole tin are checked
/// (#105, L6, `verify_sample`): an object an earlier run left, damaged
/// since, trips the next run that burns anything, and `verify_sample 0`
/// checks nothing.
#[cfg(unix)]
#[test]
fn a_damaged_object_from_an_earlier_run_trips_the_next_burn() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    sb.write("p/f.txt", "the before-copy\n");
    // A hop's `touches` keep the pack's files in the tin.
    sb.write(
        "a.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo after > p/f.txt\"], {touches: [\"*\"]})\n}\n",
    );
    sb.run_ok(&["run", "a.cig"]);
    let objects = sb.home().join("tin").join("objects");
    let object = fs::read_dir(&objects)
        .unwrap()
        .flatten()
        .flat_map(|b| fs::read_dir(b.path()).unwrap().flatten().map(|e| e.path()))
        .find(|p| fs::read_to_string(p).is_ok_and(|s| s == "the before-copy\n"))
        .expect("the before-copy in the tin");
    fs::set_permissions(&object, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&object, "rotted\n").unwrap();
    sb.write(
        "b.cig",
        "pack { \"./p\" }\nburn {\n  fs.write_text(\"p/g.txt\", \"new\")\n}\n",
    );
    sb.run_ok(&["config", "reports", "always"]);
    let t = sb.path().join("tracker").to_string_lossy().to_string();
    sb.run_ok(&["config", "verify_sample", "0"]);
    let quiet = sb.cig_env(&["run", "b.cig"], &[("CIG_INTERNAL_TRACKER_DIR", &t)]);
    assert!(quiet.status.success());
    assert!(!String::from_utf8_lossy(&quiet.stderr).contains("tripwire"));
    sb.run_ok(&["config", "verify_sample", "50"]);
    let out = sb.cig_env(&["run", "b.cig"], &[("CIG_INTERNAL_TRACKER_DIR", &t)]);
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        err.contains("tripwire: cig noticed something it got wrong"),
        "{err}"
    );
    let reports: serde_json::Value =
        serde_json::from_str(&sb.run_ok(&["--json", "crash", "list"])).unwrap();
    assert!(
        reports[0]["title"]
            .as_str()
            .unwrap()
            .contains("drawn at random"),
        "{reports}"
    );
}

#[cfg(unix)]
#[test]
fn a_rollback_that_restores_the_wrong_mode_trips() {
    let sb = Sandbox::new();
    sb.write("p/f.txt", "before\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  fs.write_text(\"p/f.txt\", \"after\")\n  cough \"boom\"\n}\n",
    );
    sb.run_ok(&["config", "reports", "always"]);
    let tracker = sb.path().join("tracker");
    let t = tracker.to_string_lossy().to_string();
    // The fault: the restore puts one mode bit back wrong. The run's own
    // outcome is what it was (the cough, exit 1); the tripwire says so.
    let out = sb.cig_env(
        &["run", "s.cig"],
        &[
            ("CIG_INTERNAL_FAULT", "restore-mode"),
            ("CIG_INTERNAL_TRACKER_DIR", &t),
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        err.contains("tripwire: cig noticed something it got wrong")
            && err.contains("a report is queued"),
        "{err}"
    );
    assert_eq!(sb.read("p/f.txt"), "before\n");
    let reports: serde_json::Value =
        serde_json::from_str(&sb.run_ok(&["--json", "crash", "list"])).unwrap();
    let r = &reports[0];
    assert_eq!(r["kind"], "tripwire", "{r}");
    assert!(
        r["title"].as_str().unwrap().contains("should have mode"),
        "{r}"
    );
    // The path travels as its placeholder, never as the person's name.
    assert_eq!(
        r["details"]["found"]["mismatches"][0]["path"], "root0/f1.txt",
        "{r}"
    );
    // Filed in the background, labelled for what it is.
    let issue = tracker.join("otmof-ops__cig-script/1.json");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !issue.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(issue.exists(), "the tripwire report was not filed");
    let filed = issue_file(&tracker, 1);
    assert!(
        filed["title"].as_str().unwrap().starts_with("tripwire ["),
        "{filed}"
    );
    assert!(filed["labels"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l == "tripwire"));
    // The same run without the fault trips nothing.
    sb.run_ok(&["crash", "delete", "--all"]);
    let out = sb.cig_env(&["run", "s.cig"], &[("CIG_INTERNAL_TRACKER_DIR", &t)]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!err.contains("tripwire:"), "{err}");
    assert_eq!(sb.run_ok(&["--json", "crash", "list"]).trim(), "[]");
}

#[test]
fn a_cough_in_the_users_script_is_never_reported() {
    let sb = Sandbox::new();
    sb.run_ok(&["config", "reports", "always"]);
    let tracker = sb.path().join("tracker");
    let t = tracker.to_string_lossy().to_string();
    // The script's own failures: a cough, a runtime error, a type error.
    sb.write(
        "cough.cig",
        "burn {\n  fs.write_text(\"a.txt\", \"1\")\n  cough \"boom\"\n}\n",
    );
    sb.write("runtime.cig", "burn {\n  stick x = 1 / 0\n}\n");
    sb.write("typed.cig", "stick s = \"a\" + 1\n");
    for script in ["cough.cig", "runtime.cig", "typed.cig"] {
        let out = sb.cig_env(&["run", script], &[("CIG_INTERNAL_TRACKER_DIR", &t)]);
        assert!(!out.status.success(), "{script}");
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(
            !err.contains("noticed something it got wrong"),
            "{script}: {err}"
        );
    }
    assert_eq!(sb.run_ok(&["--json", "crash", "list"]).trim(), "[]");
    assert!(!tracker.exists());
    // An internal error is cig's own: reported, filed, labelled.
    let out = sb.cig_env(
        &["run", "cough.cig"],
        &[
            ("CIG_INTERNAL_FAULT", "internal"),
            ("CIG_INTERNAL_TRACKER_DIR", &t),
        ],
    );
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        err.contains("error[E900 internal]")
            && err.contains("internal: cig noticed something it got wrong"),
        "{err}"
    );
    let reports: serde_json::Value =
        serde_json::from_str(&sb.run_ok(&["--json", "crash", "list"])).unwrap();
    assert_eq!(reports.as_array().unwrap().len(), 1);
    assert_eq!(reports[0]["kind"], "internal");
    let issue = tracker.join("otmof-ops__cig-script/1.json");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !issue.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(issue.exists(), "the internal error was not filed");
    assert!(issue_file(&tracker, 1)["title"]
        .as_str()
        .unwrap()
        .starts_with("internal ["));
}

#[cfg(unix)]
#[test]
fn a_repro_contains_no_user_path_or_content() {
    let sb = Sandbox::new();
    // Names and bytes a report must never carry.
    sb.write(
        "books/Clients-Private/Ledger-2026.txt",
        "ACCOUNT-7781 owes 40\n",
    );
    sb.write(
        "books/Clients-Private/notes.md",
        "call Maria about the Zürich invoice\n",
    );
    sb.write(
        "s.cig",
        "pack { \"./books\" }\nburn {\n  fs.write_text(\"books/Clients-Private/Ledger-2026.txt\", \"ACCOUNT-7781 owes 0\")\n  fs.write_text(\"books/Clients-Private/notes.md\", \"paid\")\n  proc.run(\"sh\", [\"-c\", \"echo receipt > books/Clients-Private/receipt.txt\"], {touches: [\"books/Clients-Private/**\"]})\n  cough \"boom\"\n}\n",
    );
    sb.run_ok(&["config", "reports", "always"]);
    let tracker = sb.path().join("tracker");
    let t = tracker.to_string_lossy().to_string();
    let out = sb.cig_env(
        &["run", "s.cig"],
        &[
            ("CIG_INTERNAL_FAULT", "restore-mode"),
            ("CIG_INTERNAL_TRACKER_DIR", &t),
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    // The flush verifies the repro in a sandbox and files it.
    let issue = tracker.join("otmof-ops__cig-script/1.json");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while std::time::Instant::now() < deadline {
        if issue.exists()
            && sb
                .run_ok(&["--json", "crash", "list"])
                .contains("\"issue\"")
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    assert!(issue.exists(), "the report was not filed");
    let filed = fs::read_to_string(&issue).unwrap();
    let report = sb.run_ok(&["--json", "crash", "list"]);
    let v: serde_json::Value = serde_json::from_str(&report).unwrap();
    let r = &v[0];
    assert_eq!(r["repro"]["verified"], true, "{r}");
    let script = r["repro"]["script"].as_str().unwrap();
    assert!(
        script.contains("pack { \"root0\" }") && script.contains("fs.write_text(\"root0/"),
        "{script}"
    );
    // Three ops went in; the shrinker kept what still trips.
    assert!(
        r["repro"]["note"]
            .as_str()
            .unwrap()
            .contains("shrunk from 3 to"),
        "{r}"
    );
    for needle in [
        "books", "Clients", "Private", "Ledger", "2026.txt", "notes.md", "ACCOUNT", "7781",
        "Maria", "Zürich", "receipt",
    ] {
        assert!(
            !filed.contains(needle),
            "the issue carries `{needle}`:\n{filed}"
        );
        assert!(
            !report.contains(needle),
            "the report carries `{needle}`:\n{report}"
        );
    }
    assert!(filed.contains("Repro (verified in a sandbox"), "{filed}");
    // Placeholders keep what a filesystem bug may depend on: the capital
    // letter, the extension, the depth.
    assert!(
        r["repro"]["seeds"].as_array().unwrap().iter().any(|s| {
            let p = s["path"].as_str().unwrap();
            p.starts_with("root0/D") && p.contains("/F") && p.ends_with(".txt")
        }),
        "{r}"
    );
}

#[cfg(unix)]
#[test]
fn an_unverified_repro_is_not_attached() {
    let sb = Sandbox::new();
    sb.write("p/f.txt", "before\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  fs.write_text(\"p/f.txt\", \"after\")\n  cough \"boom\"\n}\n",
    );
    sb.run_ok(&["config", "reports", "always"]);
    let tracker = sb.path().join("tracker");
    let t = tracker.to_string_lossy().to_string();
    // The fault fires once: the run trips, the sandbox run does not.
    let marker = sb.path().join("fired-once");
    let fault = format!("restore-mode-once:{}", marker.display());
    let out = sb.cig_env(
        &["run", "s.cig"],
        &[
            ("CIG_INTERNAL_FAULT", &fault),
            ("CIG_INTERNAL_TRACKER_DIR", &t),
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("tripwire: cig noticed"));
    assert!(marker.exists());
    let issue = tracker.join("otmof-ops__cig-script/1.json");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while std::time::Instant::now() < deadline {
        if issue.exists()
            && sb
                .run_ok(&["--json", "crash", "list"])
                .contains("\"issue\"")
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    assert!(issue.exists(), "the report was not filed");
    let v: serde_json::Value =
        serde_json::from_str(&sb.run_ok(&["--json", "crash", "list"])).unwrap();
    let r = &v[0];
    assert_eq!(r["repro"]["verified"], false, "{r}");
    assert!(
        r["repro"]["note"]
            .as_str()
            .unwrap()
            .contains("did not reproduce"),
        "{r}"
    );
    let filed = fs::read_to_string(&issue).unwrap();
    assert!(
        filed.contains("Repro: none attached") && !filed.contains("Repro (verified"),
        "{filed}"
    );
    // The report still says what was found, with the path as a placeholder.
    assert!(
        r["title"].as_str().unwrap().contains("should have mode"),
        "{r}"
    );
    let details = r["details"].to_string();
    assert!(
        details.contains("root0/f1.txt") && !details.contains("p/f.txt"),
        "{details}"
    );
}

#[cfg(unix)]
#[test]
fn a_second_report_updates_the_stats_not_a_comment() {
    let sb = Sandbox::new();
    sb.write("p/f.txt", "before\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  fs.write_text(\"p/f.txt\", \"after\")\n  cough \"boom\"\n}\n",
    );
    sb.run_ok(&["config", "reports", "always"]);
    let tracker = sb.path().join("tracker");
    let t = tracker.to_string_lossy().to_string();
    let env = [
        ("CIG_INTERNAL_FAULT", "restore-mode"),
        ("CIG_INTERNAL_TRACKER_DIR", &t),
    ];
    sb.cig_env(&["run", "s.cig"], &env);
    wait_filed(&sb, 1);
    let first = issue_file(&tracker, 1);
    let stats = first["body"].as_str().unwrap();
    assert!(stats.contains("\"count\":1"), "{stats}");
    // The same bug again: the issue's count moves, nothing is commented,
    // no second issue opens.
    sb.cig_env(&["run", "s.cig"], &env);
    wait_filed(&sb, 2);
    let again = issue_file(&tracker, 1);
    let body = again["body"].as_str().unwrap();
    assert!(body.contains("\"count\":2"), "{body}");
    assert!(body.contains("| 2 |"), "{body}");
    assert_eq!(
        again["comments"].as_array().map(Vec::len),
        Some(0),
        "{again}"
    );
    assert!(!tracker.join("otmof-ops__cig-script/2.json").exists());
    let list = sb.run_ok(&["--json", "crash", "list"]);
    let v: serde_json::Value = serde_json::from_str(&list).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 2);
    assert!(
        v.as_array()
            .unwrap()
            .iter()
            .all(|r| r["issue"]["number"] == 1),
        "{v}"
    );
    // What the first report said stays as it was: the body keeps its section.
    assert!(
        body.contains("<!-- cig-report -->") && body.contains("### What cig found"),
        "{body}"
    );
}

#[test]
fn a_fixed_report_is_announced_on_update_check() {
    let sb = Sandbox::new();
    let id = failed_run(&sb);
    let tracker = sb.path().join("tracker");
    let t = tracker.to_string_lossy().to_string();
    let out = sb.cig_env(
        &["report", &id, "--send"],
        &[("CIG_INTERNAL_TRACKER_DIR", &t)],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let issue_path = tracker.join("otmof-ops__cig-script/1.json");
    // The maintainer closes the issue; a release's notes say it fixed #1.
    let mut issue: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&issue_path).unwrap()).unwrap();
    issue["state"] = serde_json::Value::String("closed".to_string());
    issue["closed_at"] = serde_json::Value::String("2026-10-30T00:00:00Z".to_string());
    fs::write(&issue_path, issue.to_string()).unwrap();
    let manifest = serde_json::json!([
        {"tag_name": "v99.0.0", "prerelease": false, "draft": false, "html_url": "https://example.invalid/99", "published_at": "",
         "body": "## 99.0.0\n\n- the rollback kept its promise (#1).\n", "assets": []},
        {"tag_name": "v98.0.0", "prerelease": false, "draft": false, "html_url": "https://example.invalid/98", "published_at": "",
         "body": "nothing about it (#10 is another issue)", "assets": []}
    ]);
    sb.write("manifest.json", &manifest.to_string());
    let m = sb
        .path()
        .join("manifest.json")
        .to_string_lossy()
        .to_string();
    let env = [
        ("CIG_INTERNAL_TRACKER_DIR", &t[..]),
        ("CIG_UPDATE_MANIFEST", &m[..]),
    ];
    let out = sb.cig_env(&["update", "--check"], &env);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{err}");
    assert!(
        err.contains("fixed: a bug you reported (#1, boom) is fixed in 99.0.0: cig update"),
        "{err}"
    );
    let v: serde_json::Value =
        serde_json::from_str(&sb.run_ok(&["--json", "crash", "list"])).unwrap();
    assert_eq!(v[0]["fixed_in"], "99.0.0", "{v}");
    assert_eq!(v[0]["announced"], true, "{v}");
    // Said once.
    let out = sb.cig_env(&["update", "--check"], &env);
    assert!(!String::from_utf8_lossy(&out.stderr).contains("a bug you reported"));
    let out = sb.cig_env(&["--json", "update", "--check"], &env);
    let j: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(j["fixed_reports"].as_array().map(Vec::len), Some(0), "{j}");
}

// ----- 1.7.1: nothing of the person's in a report ------------------------------

/// Every file under `dir`, recursively.
#[cfg(unix)]
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        if e.path().is_dir() {
            out.extend(files_under(&e.path()));
        } else {
            out.push(e.path());
        }
    }
    out
}

/// `zqx` marks every folder, file name and byte of the person's in these
/// tests; nothing that travels, and nothing cig shows as travelling, may
/// carry it.
#[cfg(unix)]
fn assert_no_marker(text: &str, what: &str) {
    assert!(
        !text.to_ascii_lowercase().contains("zqx"),
        "{what} carries a name of the person's:\n{text}"
    );
}

/// One sender at a time, really: a send waits while another cig holds the
/// ashtray, and files only once it is let go. (Before 1.12.0 a send gave
/// up waiting after five seconds and filed alongside the holder, so two
/// senders could file at once: on the test tracker two issues took one
/// number, against GitHub the same report could be filed twice.)
#[cfg(unix)]
#[test]
fn a_send_waits_for_the_sender_that_holds_the_ashtray() {
    let sb = Sandbox::new();
    sb.run_ok(&["config", "reports", "always"]);
    let tracker = sb.path().join("tracker");
    sb.write(
        "tool.json",
        r#"{"kind":"crash","panic_message":"map has no key","panic_location":"orchestrate.cig:12","version":"1.1.0","cwd":"~/w","command":["cigo","run","t"],"active_run_id":null}"#,
    );
    // Another sender holds the ashtray: a live lock, by another process
    // (this test's: alive, and not the cig below). A lock whose holder is
    // gone is taken over since #127, so a made-up pid would not hold it.
    fs::create_dir_all(sb.home().join("ashtray")).unwrap();
    let held = sb.home().join("ashtray").join(".flush.lock");
    fs::write(&held, format!("{}\n", std::process::id())).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_cig"))
        .args([
            "crash",
            "submit",
            "--origin",
            "cig-orchestrator",
            "--repo",
            "otmof-ops/cig-orchestrator",
            "--file",
            &sb.path().join("tool.json").to_string_lossy(),
            "--now",
        ])
        .current_dir(sb.path())
        .env("CIGSCRIPT_HOME", sb.home())
        .env("NO_COLOR", "1")
        .env("CIG_SEAL_KEY", "file")
        .env("CIG_UPDATE_MANIFEST", sb.path().join("no-releases.json"))
        .env("CIG_INTERNAL_TRACKER_DIR", &tracker)
        .env_remove("CI")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Past the five seconds the old send waited before going ahead anyway.
    std::thread::sleep(std::time::Duration::from_millis(6500));
    let early = files_under(&tracker);
    let still = child.try_wait().unwrap().is_none();
    fs::remove_file(&held).unwrap();
    let out = child.wait_with_output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        early.is_empty(),
        "filed while another sender held the ashtray: {early:?}\n{err}"
    );
    assert!(still, "the send did not wait for the holder: {err}");
    assert!(out.status.success(), "{err}");
    assert_eq!(files_under(&tracker).len(), 1, "{err}");
}

#[cfg(unix)]
#[test]
fn a_report_never_contains_a_path_component_of_the_users_folder() {
    let sb = Sandbox::new();
    let project = "zqx-clients/ZqxCorp/zqx-project";
    fs::create_dir_all(sb.path().join(project).join("zqx-pack/ZqxData")).unwrap();
    sb.write(
        &format!("{project}/zqx-pack/ZqxData/zqx-ledger.csv"),
        "zqx-secret-7781\n",
    );
    sb.write(
        &format!("{project}/ZqxDeploy.cig"),
        "pack { \"./zqx-pack\" }\nburn {\n  fs.write_text(\"zqx-pack/ZqxData/zqx-ledger.csv\", \"changed\")\n  proc.run(\"sh\", [\"-c\", \"echo zqx-secret > zqx-pack/ZqxData/zqx-note.txt\"], {touches: [\"zqx-pack/**\"]})\n  cough \"boom in zqx-pack/ZqxData\"\n}\n",
    );
    sb.run_ok(&["config", "reports", "always"]);
    let tracker = sb.path().join("tracker");
    let t = tracker.to_string_lossy().to_string();
    // 1. A tripwire's report, from a run started in the marked folder.
    let out = sb.cig_in(
        project,
        &["run", "ZqxDeploy.cig"],
        &[
            ("CIG_INTERNAL_FAULT", "restore-mode"),
            ("CIG_INTERNAL_TRACKER_DIR", &t),
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // 2. The run's bundle, sent.
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let out = sb.cig_in(
        project,
        &["report", &id, "--send"],
        &[("CIG_INTERNAL_TRACKER_DIR", &t)],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // 3. Another tool's report, handed in with the marker in every field a
    // path or a command line can reach.
    sb.write(
        "zqx-tool.json",
        r#"{"kind":"crash","panic_message":"map has no key at /home/zqx/zqx-clients/ZqxCorp/orchestrate.cig","panic_location":"orchestrate.cig:12","version":"1.1.0","cwd":"~/zqx-clients/ZqxCorp","command":["cigo","run","ZqxDeploy.sh","--only","zqx-task"],"manifest":{"dir":"/srv/zqx/work","files":["zqx-a.txt","ZqxB.csv"],"script":"zqx-pack/ZqxData/run.sh"},"active_run_id":null}"#,
    );
    let file = sb
        .path()
        .join("zqx-tool.json")
        .to_string_lossy()
        .to_string();
    let out = sb.cig_in(
        project,
        &[
            "crash",
            "submit",
            "--origin",
            "cig-orchestrator",
            "--repo",
            "otmof-ops/cig-orchestrator",
            "--file",
            &file,
            "--now",
        ],
        &[("CIG_INTERNAL_TRACKER_DIR", &t)],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    wait_filed(&sb, 3);
    // Nothing of the marker in what was filed, in the ashtray, or in what
    // `cig crash show` says would be filed.
    let filed = files_under(&tracker);
    assert_eq!(filed.len(), 3, "{filed:?}");
    for f in &filed {
        assert_no_marker(&fs::read_to_string(f).unwrap(), &f.display().to_string());
    }
    let listed = sb.run_ok(&["--json", "crash", "list"]);
    assert_no_marker(&listed, "cig crash list");
    let list: Vec<serde_json::Value> = serde_json::from_str(&listed).unwrap();
    for r in &list {
        let shown = sb.run_ok(&["crash", "show", r["id"].as_str().unwrap()]);
        assert_no_marker(&shown, "cig crash show");
    }
    // The shape stays for the maintainer: the pack root, the depth, the
    // capital letter, the extension, the words of the command line.
    let tripwire = list.iter().find(|r| r["kind"] == "tripwire").unwrap();
    assert_eq!(
        tripwire["details"]["found"]["mismatches"][0]["path"], "root0/D1/f1.csv",
        "{tripwire}"
    );
    assert_eq!(
        tripwire["command"],
        serde_json::json!(["cig", "run", "<script>"]),
        "{tripwire}"
    );
    assert!(
        tripwire["cwd"].as_str().unwrap().starts_with("out/"),
        "{tripwire}"
    );
    let bundle = list.iter().find(|r| r["kind"] == "report").unwrap();
    let run = &bundle["details"]["run"];
    assert!(run["cwd"].as_str().unwrap().starts_with("out/"), "{run}");
    assert!(
        run["script"].as_str().unwrap().ends_with(".cig")
            && run["script"].as_str().unwrap().starts_with("out/"),
        "{run}"
    );
    assert_eq!(run["error"], "boom in root0/D1", "{run}");
    assert_eq!(
        bundle["details"]["journal"][0]["before"][0]["path"], "root0/D1/f1.csv",
        "{}",
        bundle["details"]["journal"]
    );
    // A hop is its program and the count of its arguments; the command
    // line it ran is never copied.
    let hop = bundle["details"]["journal"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["op"].as_str().unwrap().starts_with("run "))
        .unwrap();
    assert_eq!(
        hop["op"], "run sh (2 arguments)  (touches root0/**)",
        "{hop}"
    );
    let handed = list
        .iter()
        .find(|r| r["origin"] == "cig-orchestrator")
        .unwrap();
    assert_eq!(
        handed["details"]["command"],
        serde_json::json!(["cigo", "run", "<arg>", "<flag>", "<arg>"]),
        "{handed}"
    );
    // Under home, two levels, the second with a capital letter.
    let cwd = handed["details"]["cwd"].as_str().unwrap();
    let comps: Vec<&str> = cwd.split('/').collect();
    assert!(
        comps.len() == 3
            && comps[0] == "~"
            && comps[1].starts_with('d')
            && comps[2].starts_with('D'),
        "{handed}"
    );
    assert_eq!(handed["panic_location"], "orchestrate.cig:12", "{handed}");
    assert!(
        handed["panic_message"]
            .as_str()
            .unwrap()
            .ends_with("/orchestrate.cig"),
        "{handed}"
    );
}

#[cfg(unix)]
#[test]
fn a_crash_report_cwd_is_a_placeholder() {
    let sb = Sandbox::new();
    fs::create_dir_all(sb.path().join("Zqx-Work/sub")).unwrap();
    sb.write("Zqx-Work/sub/x.cig", "exhale \"never\"\n");
    let out = sb.cig_in(
        "Zqx-Work/sub",
        &["run", "x.cig"],
        &[("CIG_INTERNAL_PANIC", "eval")],
    );
    assert_eq!(out.status.code(), Some(70));
    let list: Vec<serde_json::Value> =
        serde_json::from_str(&sb.run_ok(&["--json", "crash", "list"])).unwrap();
    let r = &list[0];
    // The shape of the working directory, not its names: under home, two
    // levels, the first with a capital letter.
    assert_eq!(r["cwd"], "~/D1/d2", "{r}");
    assert_eq!(
        r["command"],
        serde_json::json!(["cig", "run", "<script>"]),
        "{r}"
    );
    let shown = sb.run_ok(&["crash", "show", r["id"].as_str().unwrap()]);
    assert_no_marker(&shown, "cig crash show");
    assert!(
        !shown.contains(sb.path().to_str().unwrap()),
        "the sandbox path is in the report:\n{shown}"
    );
    // What travels is the allowlist: the ashtray's bookkeeping stays home.
    assert!(
        !shown.contains("\"queue\"") && !shown.contains("\"announced\""),
        "{shown}"
    );
}

#[cfg(unix)]
#[test]
fn a_path_touched_twice_does_not_trip_the_rollback_tripwire() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  fs.mkdir(\"p/d\")\n  fs.write_text(\"p/d/x\", \"x\")\n  fs.rm(\"p/d\")\n  fs.write_text(\"p/keep.txt\", \"v2\")\n  fs.rm(\"p/keep.txt\")\n  proc.run(\"sh\", [\"-c\", \"mkdir -p h; echo x > h/y\"], {cwd: \"p\"})\n  fs.rm(\"p/h\")\n  cough \"boom\"\n}\n",
    );
    sb.run_ok(&["config", "reports", "always"]);
    let tracker = sb.path().join("tracker");
    let t = tracker.to_string_lossy().to_string();
    let out = sb.cig_env(&["run", "s.cig"], &[("CIG_INTERNAL_TRACKER_DIR", &t)]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    // The pack is as it was: each path judged by its state before the
    // first op that touched it, not by the run's own intermediate states.
    assert_eq!(sb.read("p/keep.txt"), "v1\n");
    assert!(!sb.exists("p/d") && !sb.exists("p/h"));
    assert!(!err.contains("tripwire:"), "{err}");
    assert_eq!(sb.run_ok(&["--json", "crash", "list"]).trim(), "[]");
    assert!(!tracker.exists());
}

#[cfg(unix)]
#[test]
fn cig_unburn_runs_the_rollback_tripwire() {
    let sb = Sandbox::new();
    sb.write("p/f.txt", "before\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  fs.write_text(\"p/f.txt\", \"after\")\n}\n",
    );
    sb.run_ok(&["config", "reports", "always"]);
    let tracker = sb.path().join("tracker");
    let t = tracker.to_string_lossy().to_string();
    sb.run_ok(&["run", "s.cig"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    // The fault: the unburn's restore puts one mode bit back wrong. The
    // unburn's own outcome is what it was; the tripwire says so.
    let out = sb.cig_env(
        &["unburn", &id],
        &[
            ("CIG_INTERNAL_FAULT", "restore-mode"),
            ("CIG_INTERNAL_TRACKER_DIR", &t),
        ],
    );
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{err}");
    assert!(
        err.contains("tripwire: cig noticed something it got wrong"),
        "{err}"
    );
    assert_eq!(sb.read("p/f.txt"), "before\n");
    let reports: Vec<serde_json::Value> =
        serde_json::from_str(&sb.run_ok(&["--json", "crash", "list"])).unwrap();
    let r = &reports[0];
    assert_eq!(r["kind"], "tripwire", "{r}");
    assert_eq!(r["active_run_id"], id, "{r}");
    assert!(
        r["title"].as_str().unwrap().contains("should have mode"),
        "{r}"
    );
    assert_eq!(
        r["details"]["found"]["mismatches"][0]["path"], "root0/f1.txt",
        "{r}"
    );
    wait_filed(&sb, 1);
    // The same unburn without the fault trips nothing.
    sb.run_ok(&["crash", "delete", "--all"]);
    sb.run_ok(&["run", "s.cig"]);
    let second = sb.runs().iter().find(|r| r["status"] == "ok").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let out = sb.cig_env(&["unburn", &second], &[("CIG_INTERNAL_TRACKER_DIR", &t)]);
    assert!(out.status.success());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("tripwire:"));
    assert_eq!(sb.run_ok(&["--json", "crash", "list"]).trim(), "[]");
}

#[test]
fn a_typed_flush_sends_a_report_waiting_for_its_retry_time() {
    let sb = Sandbox::new();
    let id = failed_run(&sb);
    let tracker = sb.path().join("tracker");
    let t = tracker.to_string_lossy().to_string();
    let out = sb.cig_env(
        &["report", &id, "--send"],
        &[
            ("CIG_INTERNAL_TRACKER_DIR", &t),
            ("CIG_INTERNAL_OFFLINE", "1"),
        ],
    );
    assert!(out.status.success());
    // A second failure puts the report in its backoff.
    let out = sb.cig_env(
        &["--json", "crash", "flush"],
        &[
            ("CIG_INTERNAL_TRACKER_DIR", &t),
            ("CIG_INTERNAL_OFFLINE", "1"),
        ],
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["failed"].as_array().map(Vec::len), Some(1), "{v}");
    let list: Vec<serde_json::Value> =
        serde_json::from_str(&sb.run_ok(&["--json", "crash", "list"])).unwrap();
    assert_eq!(list[0]["queue"]["attempts"], 2, "{}", list[0]);
    assert!(list[0]["queue"]["next_try"].is_string(), "{}", list[0]);
    // The background's pass honours the backoff; the typed one is a
    // request for now.
    let out = sb.cig_env(
        &["--json", "crash", "flush", "--quiet"],
        &[("CIG_INTERNAL_TRACKER_DIR", &t)],
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["waiting"], 1, "{v}");
    assert_eq!(v["filed"].as_array().map(Vec::len), Some(0), "{v}");
    let out = sb.cig_env(
        &["--json", "crash", "flush"],
        &[("CIG_INTERNAL_TRACKER_DIR", &t)],
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["filed"].as_array().map(Vec::len), Some(1), "{v}");
    assert!(issue_file(&tracker, 1)["title"]
        .as_str()
        .unwrap()
        .starts_with("report ["));
}

#[cfg(unix)]
#[test]
fn a_report_with_many_mismatches_lists_twenty_five_and_counts_the_rest() {
    let sb = Sandbox::new();
    let mut script = String::from("pack { \"./p\" }\nburn {\n");
    for i in 0..60 {
        sb.write(&format!("p/f{i}.txt"), "v\n");
        script.push_str(&format!("  fs.write_text(\"p/f{i}.txt\", \"w\")\n"));
    }
    script.push_str("  cough \"boom\"\n}\n");
    sb.write("s.cig", &script);
    sb.run_ok(&["config", "reports", "never"]);
    let out = sb.cig_env(&["run", "s.cig"], &[("CIG_INTERNAL_FAULT", "restore-mode")]);
    assert_eq!(out.status.code(), Some(1));
    let reports: Vec<serde_json::Value> =
        serde_json::from_str(&sb.run_ok(&["--json", "crash", "list"])).unwrap();
    let r = &reports[0];
    let found = &r["details"]["found"];
    assert_eq!(found["checked"], 60, "{found}");
    assert_eq!(
        found["mismatches"].as_array().map(Vec::len),
        Some(25),
        "{found}"
    );
    assert_eq!(found["more"], 35, "{found}");
    assert!(
        r["title"]
            .as_str()
            .unwrap()
            .contains("restored 60 paths, but 60 are not as recorded"),
        "{r}"
    );
}

#[cfg(unix)]
#[test]
fn a_relative_link_target_stays_relative_in_the_repro() {
    let sb = Sandbox::new();
    sb.write("p/one.txt", "1\n");
    sb.write("p/two.txt", "2\n");
    std::os::unix::fs::symlink("one.txt", sb.path().join("p/link")).unwrap();
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  fs.write_text(\"p/two.txt\", \"x\")\n  fs.rm(\"p/link\")\n  cough \"boom\"\n}\n",
    );
    sb.run_ok(&["config", "reports", "never"]);
    let out = sb.cig_env(&["run", "s.cig"], &[("CIG_INTERNAL_FAULT", "restore-mode")]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        fs::read_link(sb.path().join("p/link")).unwrap(),
        PathBuf::from("one.txt")
    );
    let reports: Vec<serde_json::Value> =
        serde_json::from_str(&sb.run_ok(&["--json", "crash", "list"])).unwrap();
    let seeds = reports[0]["repro"]["seeds"].as_array().unwrap().clone();
    let link = seeds.iter().find(|s| s["kind"] == "link").unwrap();
    let target = link["target"].as_str().unwrap();
    // A sibling, as it was: not a path from the working directory, not
    // under `out/`.
    assert!(!target.contains('/') && target.ends_with(".txt"), "{link}");
}

#[cfg(unix)]
#[test]
fn a_hop_under_nested_roots_is_journaled_once() {
    let sb = Sandbox::new();
    fs::create_dir_all(sb.path().join("q/a")).unwrap();
    sb.write(
        "s.cig",
        "pack { \"./q\", \"./q/a\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo h > h.txt\"], {cwd: \"q/a\"})\n}\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    // The hop, and the one file it made: seen through both roots, one effect.
    assert_eq!(sb.runs()[0]["burns"], 2, "{}", sb.runs()[0]);
}

/// Every start asks for the script's last good version, and that must not
/// read the journal of every run in the history: a history of a thousand
/// runs made each start two seconds slower (1.13.2). A journal that would
/// block a reader forever (a FIFO) proves no journal is opened.
#[cfg(unix)]
#[test]
fn a_run_starts_without_reading_other_runs_journals() {
    let sb = Sandbox::new();
    sb.write("s.cig", "exhale \"started\"\n");
    let other = sb.home().join("runs").join("20200101T000000-aaaaaa");
    fs::create_dir_all(&other).unwrap();
    fs::write(
        other.join("run.json"),
        r#"{"id":"20200101T000000-aaaaaa","script":"other.cig","script_sha256":"0000","started":"2020-01-01T00:00:00Z","status":"ok","mode":"run","version":"1.0.0","format":1}"#,
    )
    .unwrap();
    let fifo = other.join("journal.jsonl");
    let made = Command::new("mkfifo").arg(&fifo).status().expect("mkfifo");
    assert!(made.success());
    let mut child = Command::new(env!("CARGO_BIN_EXE_cig"))
        .args(["run", "s.cig"])
        .current_dir(sb.path())
        .env("CIGSCRIPT_HOME", sb.home())
        .env("NO_COLOR", "1")
        .env("CIG_SEAL_KEY", "file")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("run cig");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let status = loop {
        if let Some(s) = child.try_wait().expect("wait") {
            break Some(s);
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    // A cig that opened the FIFO is blocked in it, and was killed above.
    let status = status.expect("cig run blocked: it opened another run's journal at start");
    assert!(status.success());
}

/// A door out of the pack that leads to the pack's own ancestors (a Wine
/// prefix's `z:` leads to `/`) is not watched: the whole machine changes by
/// itself, and a shallow watch reported the system's writes as the hop's.
/// A door to a tree too big for the shallow watch reports only what both
/// walks saw (1.13.2).
#[cfg(unix)]
#[test]
fn doors_to_the_whole_machine_or_past_the_cap_report_no_writes_the_hop_did_not_make() {
    let sb = Sandbox::new();
    fs::create_dir_all(sb.path().join("p")).unwrap();
    std::os::unix::fs::symlink("/", sb.path().join("p/z")).unwrap();
    let big = sb.path().join("big");
    fs::create_dir_all(big.join("a")).unwrap();
    fs::create_dir_all(big.join("b")).unwrap();
    for i in 0..6000 {
        fs::write(big.join("b").join(format!("f{i:05}")), "x").unwrap();
    }
    std::os::unix::fs::symlink(&big, sb.path().join("p/big")).unwrap();
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo new > big/a/new.txt; echo kept > here.txt\"], {cwd: \"p\"})\n}\n",
    );
    let out = sb.cig(&["run", "s.cig"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(!err.contains("/etc/"), "{err}");
    assert!(!err.contains("/proc/"), "{err}");
    assert!(!err.contains("/b/f"), "{err}");
    assert_eq!(sb.read("p/here.txt"), "kept\n");
}

/// `cig tin keep` and `cig tin give`: a copy another tool keeps in the tin
/// comes back as it was kept, survives `cig tin gc`, and once the ledger
/// has a keep, a copy deleted from the tin is missed (exit 4), never taken
/// for nothing kept (exit 1).
#[test]
fn a_kept_copy_comes_back_survives_gc_and_is_missed_when_deleted() {
    let sb = Sandbox::new();
    sb.write("mine.list", "npm:evil\n");
    let keep = sb.cig(&["tin", "keep", "mine.list", "--label", "cigo-blacklist"]);
    assert!(
        keep.status.success(),
        "{}",
        String::from_utf8_lossy(&keep.stderr)
    );
    let gc = sb.cig(&["tin", "gc"]);
    assert!(
        gc.status.success(),
        "{}",
        String::from_utf8_lossy(&gc.stderr)
    );
    let give = sb.cig(&[
        "tin",
        "give",
        "--label",
        "cigo-blacklist",
        "--name",
        "mine.list",
    ]);
    assert!(
        give.status.success(),
        "{}",
        String::from_utf8_lossy(&give.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&give.stdout), "npm:evil\n");
    let none = sb.cig(&[
        "tin",
        "give",
        "--label",
        "cigo-blacklist",
        "--name",
        "other",
    ]);
    assert_eq!(none.status.code(), Some(1));
    let record = sb
        .home()
        .join("tin")
        .join("keeps")
        .join("cigo-blacklist")
        .join("mine.list.json");
    let record = if record.is_file() {
        record
    } else {
        // wherever this cig keeps the tin, the record is the one json under keeps/
        let mut found = None;
        for e in walk(&sb.home()) {
            if e.ends_with("cigo-blacklist/mine.list.json") {
                found = Some(e);
            }
        }
        found.expect("the keep's record")
    };
    fs::remove_file(&record).unwrap();
    let gone = sb.cig(&[
        "tin",
        "give",
        "--label",
        "cigo-blacklist",
        "--name",
        "mine.list",
    ]);
    assert_eq!(
        gone.status.code(),
        Some(4),
        "{}",
        String::from_utf8_lossy(&gone.stderr)
    );
    assert!(String::from_utf8_lossy(&gone.stderr).contains("the ledger says mine.list was kept"));
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

/// #149: four runs, each rewriting f.txt and leaving a g<k>.txt.
fn four_runs(sb: &Sandbox) -> Vec<String> {
    for k in 1..=4 {
        sb.write(
            &format!("s{k}.cig"),
            &format!("burn {{\n  fs.write_text(\"f.txt\", \"{k}\")\n  fs.write_text(\"g{k}.txt\", \"g\")\n}}\n"),
        );
        sb.run_ok(&["run", &format!("s{k}.cig")]);
    }
    // Oldest first.
    let mut ids: Vec<String> = sb
        .runs()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    ids.reverse();
    ids
}

/// #149: `--last`, `--since` and `--to` put back what unburning the same
/// runs one by one, newest first, would: the plan first, and a dry run
/// changes nothing.
#[test]
fn a_stretch_puts_back_what_one_by_one_would() {
    let sb = Sandbox::new();
    let ids = four_runs(&sb);
    let plan = sb.cig(&["unburn", "--to", &ids[0], "--dry-run"]);
    let said = String::from_utf8_lossy(&plan.stderr).to_string();
    assert!(
        plan.status.success() && said.contains("undo 3 runs"),
        "{said}"
    );
    assert_eq!(sb.read("f.txt"), "4", "a dry run changed something");
    let out = sb.cig(&["unburn", "--last", "2"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(sb.read("f.txt"), "2");
    assert!(!sb.exists("g4.txt") && !sb.exists("g3.txt") && sb.exists("g2.txt"));
    // The same as one at a time, in another sandbox.
    let other = Sandbox::new();
    let theirs = four_runs(&other);
    other.run_ok(&["unburn", &theirs[3]]);
    other.run_ok(&["unburn", &theirs[2]]);
    for f in ["f.txt", "g1.txt", "g2.txt"] {
        assert_eq!(sb.read(f), other.read(f), "{f}");
    }
    // --to keeps the run it names; the ones back already are not done twice.
    let out = sb.cig(&["unburn", "--to", &ids[0]]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(sb.read("f.txt"), "1");
    assert!(sb.exists("g1.txt") && !sb.exists("g2.txt"));
    let runs = sb.runs();
    let status = |id: &str| {
        runs.iter()
            .find(|r| r["id"] == id)
            .map(|r| r["status"].as_str().unwrap().to_string())
            .unwrap()
    };
    assert_eq!(status(&ids[0]), "ok");
    for id in &ids[1..] {
        assert_eq!(status(id), "unburned", "{id}");
    }
    // --since takes what is left.
    sb.run_ok(&["unburn", "--since", "1h", "-y"]);
    assert!(!sb.exists("f.txt") && !sb.exists("g1.txt"));
    let none = sb.cig(&["unburn", "--last", "5"]);
    assert!(none.status.success());
    assert!(String::from_utf8_lossy(&none.stderr).contains("nothing to undo"));
}

/// #149: a run refused in the middle of a stretch stops it there: the runs
/// after it stay undone, it and the ones before it untouched, and
/// `--continue` finishes the same stretch once the refusal is resolved.
#[test]
fn a_refused_run_stops_the_stretch_cleanly() {
    let sb = Sandbox::new();
    let ids = four_runs(&sb);
    // Run 3's file is changed by hand after it.
    sb.write("g3.txt", "edited");
    let out = sb.cig(&["unburn", "--to", &ids[0]]);
    let said = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success(), "{said}");
    assert!(
        said.contains("E704") && said.contains(&format!("the stretch stopped at {}", ids[2])),
        "{said}"
    );
    assert!(!sb.exists("g4.txt"), "the run after it was not undone");
    assert_eq!(sb.read("f.txt"), "3");
    assert_eq!(sb.read("g3.txt"), "edited");
    assert!(sb.exists("g2.txt") && sb.exists("g1.txt"));
    // Resolved, the same stretch carries on, and nothing is undone twice.
    sb.write("g3.txt", "g");
    let out = sb.cig(&["unburn", "--continue"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let said = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(said.contains("undo 2 runs"), "{said}");
    assert_eq!(sb.read("f.txt"), "1");
    assert!(!sb.exists("g3.txt") && !sb.exists("g2.txt") && sb.exists("g1.txt"));
    let again = sb.cig(&["unburn", "--continue"]);
    assert_eq!(again.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&again.stderr).contains("no stretch to continue"));
}

/// #149: a pinned run is kept by --prune and by keep_runs, listed as
/// pinned, and goes like any other once it is unpinned.
#[test]
fn pinned_runs_survive_prune_and_keep_runs() {
    let sb = Sandbox::new();
    let ids = four_runs(&sb);
    sb.run_ok(&["runs", "--pin", &ids[0]]);
    let listed = sb.run_ok(&["runs"]);
    assert!(
        listed
            .lines()
            .any(|l| l.contains(&ids[0]) && l.contains("(pinned)")),
        "{listed}"
    );
    let json = sb.runs();
    assert_eq!(
        json.iter().find(|r| r["id"] == ids[0].as_str()).unwrap()["pinned"],
        true
    );
    let out = sb.cig(&["runs", "--prune", "1"]);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("removed 2 run records, kept 2 (1 pinned)"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let left: Vec<String> = sb
        .runs()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(left, vec![ids[3].clone(), ids[0].clone()]);
    // keep_runs keeps it too.
    sb.run_ok(&["config", "keep_runs", "1"]);
    sb.run_ok(&["run", "s1.cig"]);
    assert!(sb.runs().iter().any(|r| r["id"] == ids[0].as_str()));
    // Unpinned, it goes.
    sb.run_ok(&["runs", "--unpin", &ids[0]]);
    sb.run_ok(&["runs", "--prune", "1"]);
    assert!(!sb.runs().iter().any(|r| r["id"] == ids[0].as_str()));
}

/// #105, L6: a journal the ledger says was cut short is a tripwire as well
/// as an E709 refusal: reported like any other, under `reports`, and with
/// no path of the person's in it. A dry run reports nothing.
#[cfg(unix)]
#[test]
fn a_journal_cut_short_is_reported_as_a_ledger_tripwire() {
    let sb = Sandbox::new();
    let id = two_op_run(&sb);
    sb.run_ok(&["config", "reports", "always"]);
    let path = journal_path(&sb, &id);
    let text = fs::read_to_string(&path).unwrap();
    let kept: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    fs::write(&path, format!("{}\n", kept[..kept.len() - 1].join("\n"))).unwrap();
    forget_head(&sb, &id);
    let dry = sb.cig(&["unburn", &id, "--dry-run"]);
    assert!(!String::from_utf8_lossy(&dry.stderr).contains("tripwire:"));
    assert_eq!(sb.run_ok(&["--json", "crash", "list"]).trim(), "[]");
    let err = sb.run_err(&["unburn", &id], 1);
    assert!(err.contains("error[E709 burn]:"), "{err}");
    assert!(err.contains("tripwire: cig noticed"), "{err}");
    let listed = sb.run_ok(&["--json", "crash", "list"]);
    let reports: serde_json::Value = serde_json::from_str(&listed).unwrap();
    let r = &reports[0];
    assert_eq!(r["kind"], "tripwire", "{r}");
    assert!(
        r["title"]
            .as_str()
            .unwrap()
            .contains("the ledger disagrees"),
        "{r}"
    );
    assert_eq!(r["details"]["check"], "ledger", "{r}");
    assert!(
        !listed.contains(&*sb.path().to_string_lossy()),
        "a path of the person's is in the report: {listed}"
    );
}

/// #105, L5: the tin's key is in no record, report or log cig writes, and
/// in nothing it prints: after a sealed burn, a failing run, an unburn, a
/// verify, doctor, a crash report and a tripwire report, every file under
/// the sandbox but the key file itself is read for the key's hex (either
/// case) and its raw bytes.
#[cfg(unix)]
#[test]
fn the_key_is_in_no_record_report_or_log() {
    let sb = Sandbox::new();
    let hexkey = "c19a5e7d0b3f4e21a7c6d8e9f0a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3";
    let key_file = sb.path().join("key.hex");
    fs::write(&key_file, format!("{hexkey}\n")).unwrap();
    let k = key_file.to_string_lossy().to_string();
    let env = [("CIG_SEAL_KEY_FILE", k.as_str())];
    sb.run_ok(&["config", "reports", "always"]);
    let mut said = String::new();
    let mut run = |args: &[&str], extra: &[(&str, &str)]| {
        let mut all: Vec<(&str, &str)> = env.to_vec();
        all.extend_from_slice(extra);
        let out = sb.cig_env(args, &all);
        said.push_str(&String::from_utf8_lossy(&out.stdout));
        said.push_str(&String::from_utf8_lossy(&out.stderr));
        out
    };
    sb.write(".env", "SECRET=one\n");
    sb.write(
        "s.cig",
        "pack { \".\" }\nburn {\n  fs.write_text(\".env\", \"SECRET=two\")\n  fs.write_text(\"other.txt\", \"x\")\n}\n",
    );
    sb.write(
        "bad.cig",
        "burn {\n  fs.write_text(\".env\", \"x\")\n  cough \"boom\"\n}\n",
    );
    run(&["run", "s.cig"], &[]);
    run(&["run", "bad.cig"], &[]);
    let ids: Vec<String> = sb
        .runs()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    run(&["runs", &ids[1], "--verify"], &[]);
    run(&["doctor"], &[]);
    run(&["tin", "verify"], &[]);
    run(&["run", "s.cig"], &[("CIG_INTERNAL_PANIC", "1")]);
    // A tripwire report: the ledger catches a journal cut short.
    let path = journal_path(&sb, &ids[1]);
    let text = fs::read_to_string(&path).unwrap();
    let kept: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    fs::write(&path, format!("{}\n", kept[..kept.len() - 1].join("\n"))).unwrap();
    forget_head(&sb, &ids[1]);
    run(&["unburn", &ids[1]], &[]);
    run(&["unburn", &ids[0]], &[]);
    run(&["--json", "crash", "list"], &[]);
    assert!(
        said.contains("tripwire: cig noticed") && said.contains("crash report saved"),
        "the reports were not made: {said}"
    );
    let raw = hex::decode(hexkey).unwrap();
    let needles = [
        hexkey.as_bytes().to_vec(),
        hexkey.to_uppercase().into_bytes(),
        raw,
    ];
    let found_in = |hay: &[u8]| {
        needles
            .iter()
            .any(|n| hay.windows(n.len()).any(|w| w == n.as_slice()))
    };
    assert!(!found_in(said.as_bytes()), "cig printed the key");
    let mut stack = vec![sb.path().to_path_buf()];
    let mut read = 0;
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p != key_file {
                read += 1;
                let bytes = fs::read(&p).unwrap_or_default();
                assert!(!found_in(&bytes), "the key is in {}", p.display());
            }
        }
    }
    assert!(
        read >= 12,
        "only {read} files read: is the walk reading the sandbox?"
    );
}

/// #105, L2: on macOS, Spotlight leaves the state directory out.
#[cfg(target_os = "macos")]
#[test]
fn the_state_directory_is_kept_out_of_spotlight() {
    let sb = Sandbox::new();
    sb.write("s.cig", "burn {\n  fs.write_text(\"f.txt\", \"x\")\n}\n");
    sb.run_ok(&["run", "s.cig"]);
    assert!(sb.home().join(".metadata_never_index").exists());
}
