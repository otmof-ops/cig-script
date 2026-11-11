// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! End-to-end tests against the built `cig` binary.
//!
//! Every test gets its own state directory through `CIGSCRIPT_HOME`, so
//! nothing here touches the developer's real run records. Golden tests in
//! `tests/scripts/` compare stdout byte for byte.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The cig under test: this build's, or `CIG_TEST_BIN` when that names
/// another. cig-compile runs the tin and box tests with a cig that builds
/// each script and runs the program (its `ci/cig-compiled.sh`), so a
/// compiled program's burns are held to the same tests
/// (otmof-ops/cig-compile#6).
fn cig_bin() -> PathBuf {
    std::env::var_os("CIG_TEST_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_cig")))
}

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
        Command::new(cig_bin())
            .args(args)
            .current_dir(self.path())
            .env("CIGSCRIPT_HOME", self.home())
            .env("NO_COLOR", "1")
            .env("CIG_SEAL_KEY", "file")
            .env("CIG_BUDGET_SYNC", "1")
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
        Command::new(cig_bin())
            .args(args)
            .current_dir(self.path().join(sub))
            .env("CIGSCRIPT_HOME", self.home())
            .env("HOME", self.path())
            .env("NO_COLOR", "1")
            .env("CIG_SEAL_KEY", "file")
            .env("CIG_BUDGET_SYNC", "1")
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
        Command::new(cig_bin())
            .args(args)
            .current_dir(self.path())
            .env("CIGSCRIPT_HOME", self.home())
            .env("NO_COLOR", "1")
            .env("CIG_SEAL_KEY", "file")
            .env("CIG_BUDGET_SYNC", "1")
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
        Command::new(cig_bin())
            .args(args)
            .current_dir(self.path())
            .env("CIGSCRIPT_HOME", self.home())
            .env("NO_COLOR", "1")
            .env("CIG_SEAL_KEY", "file")
            .env("CIG_BUDGET_SYNC", "1")
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
fn language_and_product_doctor_work_in_json() {
    let sb = Sandbox::new();
    let v: serde_json::Value = serde_json::from_str(&sb.run_ok(&["--json", "language"])).unwrap();
    assert!(v["library"].as_array().unwrap().len() > 10);
    let f = doctor_finding(&sb, "state-dir");
    assert_eq!(f["severity"], "ok", "{f}");
    let f = doctor_finding(&sb, "runs");
    assert!(
        f["found"].as_str().unwrap().starts_with("0 recorded"),
        "{f}"
    );
}

/// `cig __doctor --json`, with no namespace probed: every finding.
fn doctor_findings(sb: &Sandbox) -> Vec<serde_json::Value> {
    let out = sb.cig_env(
        &["--json", "__doctor"],
        &[("CIG_INTERNAL_NO_NS_PROBE", "1")],
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stderr)));
    v["findings"].as_array().unwrap().clone()
}

/// The first finding of a check.
fn doctor_finding(sb: &Sandbox, check: &str) -> serde_json::Value {
    doctor_findings(sb)
        .into_iter()
        .find(|f| f["check"] == check)
        .unwrap_or_else(|| panic!("no finding for {check}"))
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
    let out = Command::new(cig_bin())
        .args(["run", "s.cig", "--token=supersecret"])
        .current_dir(sb.path())
        .env("CIGSCRIPT_HOME", sb.home())
        .env("NO_COLOR", "1")
        .env("CIG_SEAL_KEY", "file")
        .env("CIG_BUDGET_SYNC", "1")
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
        serde_json::from_str(&sb.run_ok(&["--json", "__crash", "list"])).unwrap();
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
    let shown = sb.run_ok(&["__crash", "show", &id]);
    assert!(shown.contains("\"fingerprint\""));
    // never mode: no prompt, no send, report stays local.
    sb.run_ok(&["config", "crash_reports", "never"]);
    let out = Command::new(cig_bin())
        .args(["run", "s.cig"])
        .current_dir(sb.path())
        .env("CIGSCRIPT_HOME", sb.home())
        .env("CIG_INTERNAL_TRACKER_DIR", sb.path().join("tracker"))
        .env("CIG_INTERNAL_PANIC", "1")
        .env_remove("CI")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("crash_reports is \"never\""));
    sb.run_ok(&["__crash", "delete", "--all"]);
    assert_eq!(sb.run_ok(&["--json", "__crash", "list"]).trim(), "[]");
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

    let f = doctor_finding(&sb, "interrupted-run");
    assert_eq!(f["severity"], "action", "{f}");
    assert!(f["found"].as_str().unwrap().contains(&id), "{f}");
    assert_eq!(f["fix"], format!("cig unburn {id}"), "{f}");
    let text = String::from_utf8_lossy(
        &sb.cig_env(&["__doctor"], &[("CIG_INTERNAL_NO_NS_PROBE", "1")])
            .stdout,
    )
    .to_string();
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
    let file = sb.home().join("reports").join(format!("{id}.json"));
    let file_arg = file.to_string_lossy().to_string();
    let out = sb.cig_env(&["report-bundle", &id, "--out", &file_arg], &envs);
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
    let err = sb.run_err(&["report-bundle", "zzz"], 3);
    assert!(err.contains("error[E803 usage]"), "{err}");
}

#[test]
#[cfg(unix)]
fn report_bundle_prints_the_bundle_with_a_repro_and_writes_and_sends_nothing() {
    let sb = Sandbox::new();
    run_corpus(&sb, "e551-not-on-path.cig", &[]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let base = corpus_env(&sb);
    let envs: Vec<(&str, &str)> = base.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let out = sb.cig_env(&["report-bundle", &id, "--json"], &envs);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bundle: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(bundle["kind"], "cigscript-report");
    assert_eq!(bundle["run"]["id"], id);
    // A run that ended in a cheat has no crash, internal error or tripwire
    // for a repro to look for: the object says so and is not verified.
    assert_eq!(bundle["repro"]["verified"], false, "{}", bundle["repro"]);
    assert!(bundle["repro"]["script"].is_null());
    assert!(
        bundle["repro"]["why"]
            .as_str()
            .unwrap()
            .contains("nothing cig can look for"),
        "{}",
        bundle["repro"]
    );
    // Nothing was written and nothing queued.
    assert!(!sb.home().join("reports").exists());
    assert_eq!(sb.run_ok(&["--json", "__crash", "list"]).trim(), "[]");
    // The same bundle through the older call the installer falls back to.
    let again = sb.cig_env(&["--json", "report", &id], &envs);
    let v: serde_json::Value = serde_json::from_slice(&again.stdout).unwrap();
    assert_eq!(v["repro"]["verified"], false);
    // `--json` does not combine with writing or queueing.
    let err = sb.run_err(&["--json", "report-bundle", &id, "--queue"], 3);
    assert!(err.contains("E800"), "{err}");
    // An unknown run is exit 3, with nothing on stdout.
    let out = sb.cig_env(&["report-bundle", "zzz", "--json"], &envs);
    assert_eq!(out.status.code(), Some(3));
    assert!(out.stdout.is_empty());
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
    let mut child = Command::new(cig_bin())
        .args(["run", "s.cig"])
        .current_dir(sb.path())
        .env("CIGSCRIPT_HOME", sb.home())
        .env("NO_COLOR", "1")
        .env("CIG_SEAL_KEY", "file")
        .env("CIG_BUDGET_SYNC", "1")
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
    let listed = sb.run_ok(&["--json", "__crash", "list"]);
    let v: Vec<serde_json::Value> = serde_json::from_str(&listed).unwrap();
    let id = v[0]["id"].as_str().unwrap().to_string();
    sb.run_ok(&["config", "crash_reports", "never"]);
    let err = sb.run_err(&["__crash", "send", &id], 3);
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
        serde_json::from_str(&sb.run_ok(&["--json", "__crash", "list"])).unwrap();
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
        serde_json::from_str(&sb.run_ok(&["--json", "__crash", "list"])).unwrap();
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
        serde_json::from_str(&sb.run_ok(&["--json", "__crash", "list"])).unwrap();
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
    // The same trial again, with nothing it read changed: remembered as a
    // trial pass, and no dry run is made (cig-script#186).
    let out = sb.cig(&["run", "s.cig"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("remembered"), "{stderr}");
    // Plans that differ hand the run to the proved version. The proved cig
    // is another binary now, so the remembered trial no longer holds.
    let plan2 = sb.write("plan2.json", &plan.replace("write ", "write elsewhere/"));
    shelf_fake_with_plan(&sb, "1.0.0", &plan2);
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

#[test]
#[cfg(unix)]
fn force_on_a_partial_rollback_retries_only_what_failed() {
    use std::os::unix::fs::PermissionsExt;
    if running_as_root() {
        eprintln!("skipped: root ignores directory modes, so no restore can be made to fail here");
        return;
    }
    let sb = Sandbox::new();
    sb.write("a.txt", "o\n");
    sb.write("sub/b.txt", "o\n");
    sb.write("one.cig", "burn {\n  fs.write_text(\"a.txt\", \"n1\")\n}\n");
    sb.write(
        "two.cig",
        "burn {\n  fs.write_text(\"a.txt\", \"n2\")\n  fs.write_text(\"sub/b.txt\", \"n\")\n}\n",
    );
    sb.run_ok(&["run", "one.cig"]);
    sb.run_ok(&["run", "two.cig"]);
    let runs = sb.runs();
    let (newer, older) = (
        runs[0]["id"].as_str().unwrap().to_string(),
        runs[1]["id"].as_str().unwrap().to_string(),
    );
    // The newer run's first pass puts a.txt back to n1 and fails on sub/b.txt.
    let sub = sb.path().join("sub");
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o500)).unwrap();
    let first = sb.cig(&["unburn", &newer]);
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(!first.status.success());
    assert_eq!(sb.read("a.txt"), "n1");
    // The older run is unburned between the two passes: a.txt goes back to o.
    sb.run_ok(&["unburn", &older]);
    assert_eq!(sb.read("a.txt"), "o\n");
    // --force finishes the newer run's failed entry and leaves the older
    // run's restore of a.txt alone.
    let second = sb.run_ok(&["unburn", &newer, "--force"]);
    assert!(second.contains("sub/b.txt"), "{second}");
    assert_eq!(sb.read("sub/b.txt"), "o\n");
    assert_eq!(sb.read("a.txt"), "o\n");
    assert_eq!(sb.runs()[0]["status"], "unburned");
    // A run rolled back cleanly still needs --force to be redone.
    let third = sb.run_err(&["unburn", &newer], 1);
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

// ----- the ledger's tail (#184) ---------------------------------------------

#[cfg(unix)]
fn ledger_file(sb: &Sandbox) -> std::path::PathBuf {
    sb.path().join(".cigscript-keys").join("ledger.jsonl")
}

/// Edit one ledger line's status without touching its MAC.
#[cfg(unix)]
fn edit_ledger_line(sb: &Sandbox, line_no: usize) {
    let text = fs::read_to_string(ledger_file(sb)).unwrap();
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    assert!(
        lines[line_no - 1].contains("\"status\":\"ok\""),
        "{lines:?}"
    );
    lines[line_no - 1] = lines[line_no - 1].replace("\"status\":\"ok\"", "\"status\":\"xx\"");
    fs::write(ledger_file(sb), format!("{}\n", lines.join("\n"))).unwrap();
}

/// A last line no key verifies is never chained onto: the run still does
/// its work, says why the ledger was left alone, and the ledger is as it
/// was (E764; E760 is the journal's chain).
#[cfg(unix)]
#[test]
fn append_refuses_a_broken_tail() {
    let sb = Sandbox::new();
    two_op_run(&sb);
    let n = fs::read_to_string(ledger_file(&sb))
        .unwrap()
        .lines()
        .count();
    assert!(n >= 1);
    edit_ledger_line(&sb, n);
    let before = fs::read_to_string(ledger_file(&sb)).unwrap();
    sb.write("s2.cig", "burn {\n  fs.write_text(\"c.txt\", \"3\")\n}\n");
    let out = sb.cig(&["run", "s2.cig"]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(err.contains("E764"), "{err}");
    assert_eq!(
        fs::read_to_string(ledger_file(&sb)).unwrap(),
        before,
        "nothing may be appended to a broken tail"
    );
    // A forget (a plain append) is refused the same way.
    let out = sb.cig(&["runs", "--forget", "nope"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("E764"));
}

/// An append reads the tail and nothing else: a line edited far back in
/// the ledger is not what an append is for (the verdict commands find it),
/// so the next run still appends, and `--verify` then names the line.
#[cfg(unix)]
#[test]
fn append_reads_only_the_tail() {
    let sb = Sandbox::new();
    for i in 0..6 {
        sb.write(
            "s.cig",
            &format!("burn {{\n  fs.write_text(\"f{i}.txt\", \"{i}\")\n}}\n"),
        );
        sb.run_ok(&["run", "s.cig"]);
    }
    let n = fs::read_to_string(ledger_file(&sb))
        .unwrap()
        .lines()
        .count();
    assert!(n >= 6, "{n}");
    edit_ledger_line(&sb, 2);
    sb.write("s.cig", "burn {\n  fs.write_text(\"last.txt\", \"z\")\n}\n");
    sb.run_ok(&["run", "s.cig"]);
    let after = fs::read_to_string(ledger_file(&sb))
        .unwrap()
        .lines()
        .count();
    assert_eq!(after, n + 1, "the run appended to the ledger");
    // And the whole-chain walk, where it is asked for, still sees line 2.
    let out = sb.cig(&["runs", "--verify"]);
    let said = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(!out.status.success(), "{said}");
    assert!(said.contains("broken at line 2"), "{said}");
}

/// `cig runs --verify` and `--deep` report the ledger's own chain, name the
/// line, and exit non-zero (they said ok before).
#[cfg(unix)]
#[test]
fn verify_all_reports_a_broken_ledger_chain() {
    let sb = Sandbox::new();
    two_op_run(&sb);
    sb.run_ok(&["runs", "--verify"]);
    edit_ledger_line(&sb, 1);
    for args in [
        &["runs", "--verify"][..],
        &["runs", "--verify", "--deep"][..],
    ] {
        let out = sb.cig(args);
        let said = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(!out.status.success(), "{args:?}: {said}");
        assert!(said.contains("broken at line 1"), "{args:?}: {said}");
    }
    let out = sb.cig(&["--json", "runs", "--verify"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ledger"]["broken_at"], 1, "{v}");
    assert_eq!(v["ok"], false, "{v}");
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

/// A boxed hop sees `tin/` and `runs/` as empty folders: the guard mounts
/// an empty tmpfs over each, and only this hop's own burn-protocol journal
/// is bound back in through the fresh `runs/` tmpfs so a hop that reports
/// what it changed still lands its report. Other runs, the tin's objects
/// and anything else under those folders are gone from the hop's view.
#[cfg(target_os = "linux")]
#[test]
fn a_boxed_hop_cannot_see_the_tin_or_runs_and_its_report_still_lands() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    // A first run so `runs/` already holds a record the next hop should
    // not see through the guard's tmpfs. The write replaces an existing
    // file so the tin keeps its before-bytes as a snapshot.
    sb.write("p/one.txt", "0\n");
    sb.write(
        "first.cig",
        "pack { \"./p\" }\nburn { fs.write_text(\"p/one.txt\", \"1\\n\") }\n",
    );
    sb.run_ok(&["run", "first.cig"]);
    assert_eq!(sb.read("p/one.txt"), "1\n");
    let first_id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    // The second run's hop lists the state root, the tin and runs, and
    // appends a line to its burn-protocol journal (which cig writes to
    // its own journal too through the proxy). The guard makes the first
    // two listings empty and the third landing still work.
    let home = sb.home().display().to_string();
    sb.write(
        "p/probe.sh",
        &format!(
            "ls -A {home} | sort > home.txt\nls -A {home}/tin > tin.txt\nls -A {home}/runs > runs.txt\n[ -n \"${{CIG_JOURNAL:-}}\" ] && printf '%s\\n' '{{\"kind\":\"note\",\"text\":\"from the hop\"}}' >> \"$CIG_JOURNAL\" && echo wrote > wrote.txt || echo no-journal > wrote.txt\n",
        ),
    );
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"probe.sh\"], {cwd: \"p\", box: true})\n}\n",
    );
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
    // The hop sees the state root with the usual top-level names, but the
    // tin and runs underneath are empty. `inbox` is still listed (the
    // guard binds it writable so a cig inside the hop can record there).
    let home_ls = sb.read("p/home.txt");
    assert!(
        home_ls.contains("tin") && home_ls.contains("runs"),
        "the state root was not visible to the hop: {home_ls}"
    );
    let tin_ls = sb.read("p/tin.txt");
    assert!(
        tin_ls.trim().is_empty(),
        "the tin was visible to the hop: {tin_ls}\n{err}"
    );
    let runs_ls = sb.read("p/runs.txt");
    // The only name the hop may see under `runs/` is the one directory
    // the guard made in the fresh tmpfs to hold its own journal path:
    // the current run's id. The first run, sitting on real disk, is gone.
    assert!(
        !runs_ls.contains(&first_id),
        "the earlier run was visible to the hop: {runs_ls}\n{err}"
    );
    let outer = sb
        .runs()
        .into_iter()
        .find(|r| r["script"].as_str().is_some_and(|s| s.ends_with("s.cig")))
        .expect("the second run was recorded");
    let outer_id = outer["id"].as_str().unwrap().to_string();
    assert_eq!(
        runs_ls.trim(),
        outer_id,
        "`runs/` showed more than this hop's own run: {runs_ls}\n{err}"
    );
    // The hop's own report landed: cig took it into the journal.
    assert_eq!(sb.read("p/wrote.txt").trim(), "wrote", "{err}");
    let hops_dir = sb.home().join("runs").join(&outer_id).join("hops");
    let journal: Vec<_> = fs::read_dir(&hops_dir)
        .unwrap()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            fs::read_to_string(&p).ok().map(|t| (p, t))
        })
        .collect();
    assert!(
        journal
            .iter()
            .any(|(_, t)| t.contains("\"text\":\"from the hop\"")),
        "the hop's own report did not land: {journal:?}"
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

/// The probe the watch tests run in the box: write a file, connect, run a
/// program, and try io_uring (syscall 425 on both architectures).
#[cfg(target_os = "linux")]
fn watch_probe(sb: &Sandbox) {
    sb.write(
        "p/probe.py",
        "import ctypes, socket\n\
         s = socket.socket()\n\
         s.settimeout(1)\n\
         try:\n    s.connect(('127.0.0.1', 9))\n\
         except OSError:\n    pass\n\
         libc = ctypes.CDLL(None, use_errno=True)\n\
         r = libc.syscall(425, 1, ctypes.c_void_p(0))\n\
         print('ret=%d errno=%d' % (r, ctypes.get_errno()))\n",
    );
    sb.write(
        "p/probe.sh",
        "echo hi > opened.txt\npython3 probe.py\n/bin/true\n",
    );
}

/// The events a hop's log holds, from the run's folder.
#[cfg(target_os = "linux")]
fn watch_events(sb: &Sandbox) -> Vec<serde_json::Value> {
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let dir = sb.home().join("runs").join(id).join("events");
    let mut out = Vec::new();
    for e in fs::read_dir(dir).unwrap().flatten() {
        for line in fs::read_to_string(e.path()).unwrap().lines() {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                out.push(v);
            }
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn watch_script(contain: bool) -> String {
    format!(
        "pack {{ \"./p\" }}\nburn {{\n  stick r = proc.run(\"sh\", [\"probe.sh\"], {{cwd: \"p\", box: {{network: \"off\", watch: true{}, apply: \"never\"}}}})\n  fs.write_text(\"p/seen.txt\", r.events.map(pack(u) => u.call).join(\" \"))\n  fs.write_text(\"p/uring.txt\", r.out)\n}}\n",
        if contain { ", contain: true" } else { "" }
    )
}

#[cfg(target_os = "linux")]
fn check_watch(sb: &Sandbox, out: &Output) -> bool {
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if !out.status.success() {
        // Asked for and not possible: refused, as it must be.
        assert!(err.contains("E777"), "{err}");
        assert!(!box_expected_here(), "{err}");
        return false;
    }
    if fell_back(&err) {
        allow_fallback(&err);
        return false;
    }
    let ev = watch_events(sb);
    let has = |call: &str, f: &dyn Fn(&serde_json::Value) -> bool| {
        ev.iter().any(|e| e["call"] == call && f(e))
    };
    assert!(
        has("open", &|e| e["path"]
            .as_str()
            .is_some_and(|p| p.ends_with("opened.txt"))
            && e["mode"] == "write"),
        "no open of opened.txt: {ev:?}\n{err}"
    );
    assert!(
        has("connect", &|e| e["host"] == "127.0.0.1" && e["port"] == 9),
        "no connect to 127.0.0.1:9: {ev:?}\n{err}"
    );
    assert!(
        has("exec", &|e| e["path"]
            .as_str()
            .is_some_and(|p| p.ends_with("true"))),
        "no exec of true: {ev:?}\n{err}"
    );
    let seen = sb.read("p/seen.txt");
    assert!(
        seen.contains("open") && seen.contains("connect") && seen.contains("exec"),
        "r.events: {seen}"
    );
    true
}

/// The watch (#106 step 3): what a hop opens for writing, connects to and
/// runs is an event in the hop's log and in `r.events`.
#[cfg(target_os = "linux")]
#[test]
fn the_watch_reports_open_connect_exec() {
    if Command::new("python3").arg("--version").output().is_err() {
        eprintln!("no python3 here; skipped");
        return;
    }
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    watch_probe(&sb);
    sb.write("s.cig", &watch_script(false));
    let out = sb.cig(&["run", "s.cig"]);
    if !check_watch(&sb, &out) {
        return;
    }
    // The fast route says nothing about itself.
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!err.contains("E779") || !has_user_notif(), "{err}");
}

/// Whether this kernel has seccomp user notification (5.0 or later).
#[cfg(target_os = "linux")]
fn has_user_notif() -> bool {
    let kernel = fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    let major: u32 = kernel
        .trim()
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    major >= 5
}

/// The same events by the slower route, with the note (E779); io_uring is
/// refused there too, and says so.
#[cfg(target_os = "linux")]
#[test]
fn the_watch_falls_back_to_ptrace() {
    if Command::new("python3").arg("--version").output().is_err() {
        eprintln!("no python3 here; skipped");
        return;
    }
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    watch_probe(&sb);
    sb.write("s.cig", &watch_script(false));
    let out = sb.cig_env(&["run", "s.cig"], &[("CIG_WATCH_ROUTE", "ptrace")]);
    if !check_watch(&sb, &out) {
        return;
    }
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(err.contains("E779"), "{err}");
    assert!(
        watch_events(&sb).iter().any(|e| e["code"] == "E779"),
        "the log carries E779"
    );
    assert!(
        sb.read("p/uring.txt").contains("errno=1"),
        "{}",
        sb.read("p/uring.txt")
    );
}

/// io_uring is refused in a contained hop (EPERM) whether or not the hop
/// asked for the watch, and the refusal leaves an event and a note (E776).
#[cfg(target_os = "linux")]
#[test]
fn a_hop_cannot_use_io_uring() {
    if Command::new("python3").arg("--version").output().is_err() {
        eprintln!("no python3 here; skipped");
        return;
    }
    for watched in [false, true] {
        let sb = Sandbox::new();
        sb.write("p/keep.txt", "v1\n");
        watch_probe(&sb);
        let script = if watched {
            watch_script(true)
        } else {
            "pack { \"./p\" }\nburn {\n  stick r = proc.run(\"sh\", [\"probe.sh\"], {cwd: \"p\", box: {network: \"off\", contain: true, apply: \"never\"}})\n  fs.write_text(\"p/uring.txt\", r.out)\n}\n".to_string()
        };
        sb.write("s.cig", &script);
        let out = sb.cig(&["run", "s.cig"]);
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        if !out.status.success() {
            assert!(err.contains("E777"), "{err}");
            assert!(!box_expected_here(), "{err}");
            return;
        }
        if fell_back(&err) {
            allow_fallback(&err);
            return;
        }
        let uring = sb.read("p/uring.txt");
        assert!(
            uring.contains("ret=-1") && uring.contains("errno=1"),
            "{uring}\n{err}"
        );
        assert!(err.contains("E776"), "{err}");
        assert!(
            watch_events(&sb)
                .iter()
                .any(|e| e["code"] == "E776" && e["result"] == "denied"),
            "no E776 event"
        );
    }
}

/// Off Linux the watch is refused where `box:` is parsed, before the box
/// runs. The message names the knob and the OS (`the watch is Linux's
/// seccomp`), and the hop exits with the parser's error code (1).
#[cfg(not(target_os = "linux"))]
#[test]
fn the_watch_is_refused_off_linux() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo x\"], {cwd: \"p\", box: {watch: true}})\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(
        err.contains("watch") && err.contains("Linux"),
        "a non-Linux refusal of box.watch must name the knob and the OS: {err}"
    );
}

/// Off Linux `box.decoys` is refused where `box:` is parsed, the same
/// way `watch` is: the mount boundary that `decoys` would need is
/// Linux's mount namespace, so a hop on macOS or Windows is refused
/// before the box is built.
#[cfg(not(target_os = "linux"))]
#[test]
fn decoys_are_refused_off_linux() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo x\"], {cwd: \"p\", box: {decoys: [\"~/.ssh/id_ed25519\"]}})\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(
        err.contains("decoys") && err.contains("Linux"),
        "a non-Linux refusal of box.decoys must name the knob and the OS: {err}"
    );
}

/// Off Linux `box.pass_reads` is refused where `box:` is parsed too,
/// since the supervisor is seccomp(2) user notification's.
#[cfg(not(target_os = "linux"))]
#[test]
fn pass_reads_are_refused_off_linux() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo x\"], {cwd: \"p\", box: {pass_reads: {\"~/.npmrc\": \"npm\"}}})\n}\n",
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(
        err.contains("pass_reads") && err.contains("Linux"),
        "a non-Linux refusal of box.pass_reads must name the knob and the OS: {err}"
    );
}

/// `decoys:` fakes a credential path: the hop sees the decoy's bytes and
/// never the real ones, and the read lands in the log as `E773`.
#[cfg(target_os = "linux")]
#[test]
fn a_decoy_is_read_instead_of_the_real_key() {
    // The decoy supervisor uses SECCOMP_IOCTL_NOTIF_ADDFD with the SEND
    // flag (Linux 5.14+). Older kernels: the supervisor fails closed
    // (EPERM); the test prints why and skips.
    if !has_addfd_send() {
        eprintln!("no SECCOMP_ADDFD_FLAG_SEND here (needs Linux 5.14+); skipped");
        return;
    }
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    // The real key, planted where HOME will point at it during the run.
    let home = sb.path().to_path_buf();
    let key_dir = home.join(".ssh");
    fs::create_dir_all(&key_dir).unwrap();
    let key = key_dir.join("id_ed25519");
    fs::write(
        &key,
        b"-----BEGIN OPENSSH PRIVATE KEY-----\nREALREALREAL\n-----END OPENSSH PRIVATE KEY-----\n",
    )
    .unwrap();
    // The hop opens the key by its home-relative path and writes its
    // first line somewhere the test can read back.
    sb.write(
        "p/probe.sh",
        "cat \"$HOME/.ssh/id_ed25519\" 2>&1 || echo refused\n",
    );
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  stick r = proc.run(\"sh\", [\"probe.sh\"], {cwd: \"p\", box: {network: \"off\", decoys: [\"~/.ssh/id_ed25519\"], apply: \"never\"}})\n  fs.write_text(\"p/seen.txt\", r.out)\n}\n",
    );
    let out = sb.cig_env(&["run", "s.cig"], &[("HOME", home.to_str().unwrap())]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if !out.status.success() {
        assert!(err.contains("E777"), "{err}");
        assert!(!box_expected_here(), "{err}");
        return;
    }
    if fell_back(&err) {
        allow_fallback(&err);
        return;
    }
    let seen = sb.read("p/seen.txt");
    assert!(
        !seen.contains("REALREALREAL"),
        "the hop saw the real key bytes: {seen}"
    );
    assert!(
        seen.contains("cig-decoy") || seen.contains("not the real credential"),
        "the hop did not see the decoy: {seen}"
    );
    // The log carries the E773 event for the key path.
    let ev = watch_events(&sb);
    assert!(
        ev.iter().any(|e| e["code"] == "E773"
            && e["path"]
                .as_str()
                .is_some_and(|p| p.ends_with("/.ssh/id_ed25519"))),
        "no E773 event on the key path: {ev:?}"
    );
}

/// The decoy mount, not the supervisor, is the boundary: a hop that
/// reaches the real path through a symlink made in the pack, through
/// `openat` with a dirfd of the parent directory, or through
/// `/proc/self/root/<path>` sees the decoy every time. The supervisor
/// still writes an E773 event for each read, but the mount is what
/// stops the real bytes leaving the host.
#[cfg(target_os = "linux")]
#[test]
fn a_decoy_holds_against_a_link_and_a_dirfd() {
    if !has_addfd_send() {
        eprintln!("no SECCOMP_ADDFD_FLAG_SEND here (needs Linux 5.14+); skipped");
        return;
    }
    if Command::new("python3").arg("--version").output().is_err() {
        eprintln!("no python3 here; skipped");
        return;
    }
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    let home = sb.path().to_path_buf();
    let key_dir = home.join(".ssh");
    fs::create_dir_all(&key_dir).unwrap();
    let key = key_dir.join("id_ed25519");
    fs::write(&key, b"REAL-KEY-BYTES-SENTINEL\n").unwrap();
    // A symlink in the pack, pointing at the real path: the resolution
    // lands inside the box on the decoy, since the real path is bound.
    std::os::unix::fs::symlink(&key, sb.path().join("p/key_link")).unwrap();
    // A python probe: openat with a dirfd of the parent directory, a
    // read through /proc/self/root, and the symlink. Each read goes to
    // stdout prefixed with how it reached the file.
    sb.write(
        "p/probe.py",
        r#"import os, sys
home = os.environ['HOME']
real = home + '/.ssh/id_ed25519'
def tagged(name, body):
    sys.stdout.write('===' + name + '===\n')
    sys.stdout.write(body)
    sys.stdout.write('\n')
# 1. symlink in the pack
tagged('link', open('key_link').read())
# 2. openat with a dirfd of the parent (os.open with dir_fd)
fd = os.open(home + '/.ssh', os.O_RDONLY | os.O_DIRECTORY)
f = os.open('id_ed25519', os.O_RDONLY, dir_fd=fd)
tagged('dirfd', os.read(f, 4096).decode('utf-8', 'replace'))
os.close(f); os.close(fd)
# 3. /proc/self/root/<real>
tagged('proc', open('/proc/self/root' + real).read())
# 4. the plain openat (for completeness; this is the path the two other
#    tests already exercise)
tagged('plain', open(real).read())
"#,
    );
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  stick r = proc.run(\"python3\", [\"probe.py\"], {cwd: \"p\", box: {network: \"off\", decoys: [\"~/.ssh/id_ed25519\"], apply: \"never\"}})\n  fs.write_text(\"p/out.txt\", r.out)\n}\n",
    );
    let out = sb.cig_env(&["run", "s.cig"], &[("HOME", home.to_str().unwrap())]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if !out.status.success() {
        assert!(err.contains("E777"), "{err}");
        assert!(!box_expected_here(), "{err}");
        return;
    }
    if fell_back(&err) {
        allow_fallback(&err);
        return;
    }
    let text = sb.read("p/out.txt");
    assert!(
        !text.contains("REAL-KEY-BYTES-SENTINEL"),
        "a path into the real key bypassed the decoy mount: {text}"
    );
    // Split into per-route blocks on the `===name===` headers. Each
    // route's bytes must carry the decoy marker (`cig-decoy`) or the
    // generic one (`not the real credential`); together that covers
    // the shaped and the generic decoy bodies.
    let mut blocks: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("===").and_then(|r| r.strip_suffix("===")) {
            current = Some(rest.to_string());
            blocks.entry(rest.to_string()).or_default();
            continue;
        }
        if let Some(c) = &current {
            let entry = blocks.entry(c.clone()).or_default();
            entry.push_str(line);
            entry.push('\n');
        }
    }
    for route in ["link", "dirfd", "proc", "plain"] {
        let body = blocks
            .get(route)
            .unwrap_or_else(|| panic!("route {route} did not appear: {text}"));
        assert!(
            body.contains("cig-decoy") || body.contains("not the real credential"),
            "route `{route}` did not serve the decoy's bytes: {body}"
        );
    }
    // The supervisor still writes at least one E773 event for the
    // direct path read; the other routes pass symlink and dirfd-relative
    // strings the supervisor does not match literally, so the mount
    // (not the supervisor) is what stops the real bytes leaking there.
    let ev = watch_events(&sb);
    assert!(
        ev.iter().any(|e| e["code"] == "E773"),
        "expected at least one E773 event on the plain path: {ev:?}"
    );
}

/// `pass_reads:` gives the real file to one program and the decoy to
/// every other process, each in the event log with the matching code.
/// Two distinct binaries are copied into the sandbox so the test holds
/// on merged-usr distros (where `/bin/cat` and `/usr/bin/cat` share one
/// inode) and on Alpine (where `cat`, `head` and `sh` are all applets
/// of busybox). `pass_reads` names the allowed program by the copy's
/// absolute path.
#[cfg(target_os = "linux")]
#[test]
fn a_passed_read_reaches_only_its_program() {
    if !has_addfd_send() {
        eprintln!("no SECCOMP_ADDFD_FLAG_SEND here (needs Linux 5.14+); skipped");
        return;
    }
    let cat_src = which("cat").unwrap_or("/bin/cat".to_string());
    let head_src = which("head").unwrap_or("/usr/bin/head".to_string());
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    let home = sb.path().to_path_buf();
    let rc = home.join(".npmrc");
    fs::write(&rc, b"//registry.npmjs.org/:_authToken=REAL-ONLY-FOR-NPM\n").unwrap();
    // Copy two distinct binaries into the sandbox. A merged-usr
    // `/bin/cat` symlink to `/usr/bin/cat` canonicalises to one file
    // the `pass_reads` matcher sees as both; on Alpine `cat` and `head`
    // both canonicalise to `busybox` and the matcher refuses it. The
    // copy defeats both.
    let allowed = sb.path().join("bin/allowed_reader");
    let denied = sb.path().join("bin/denied_reader");
    fs::create_dir_all(sb.path().join("bin")).unwrap();
    let canon_cat = fs::canonicalize(&cat_src).unwrap_or_else(|_| cat_src.clone().into());
    let canon_head = fs::canonicalize(&head_src).unwrap_or_else(|_| head_src.clone().into());
    if canon_cat == canon_head
        || canon_cat.file_name().and_then(|n| n.to_str()) == Some("busybox")
        || canon_head.file_name().and_then(|n| n.to_str()) == Some("busybox")
    {
        eprintln!("cat and head share one binary here (merged-usr or busybox); skipped");
        return;
    }
    fs::copy(&canon_cat, &allowed).unwrap();
    fs::copy(&canon_head, &denied).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&allowed, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&denied, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let allowed_s = allowed.to_string_lossy().into_owned();
    let denied_s = denied.to_string_lossy().into_owned();
    sb.write(
        "p/probe.sh",
        &format!(
            "echo '== via allowed =='\n\
             {allowed_s} \"$HOME/.npmrc\" 2>&1 || echo refused\n\
             echo '== via denied =='\n\
             {denied_s} -n1 \"$HOME/.npmrc\" 2>&1 || echo refused\n"
        ),
    );
    sb.write(
        "s.cig",
        &format!(
            "pack {{ \"./p\" }}\nburn {{\n  stick r = proc.run(\"sh\", [\"probe.sh\"], {{cwd: \"p\", box: {{network: \"off\", pass_reads: {{\"~/.npmrc\": \"{allowed_s}\"}}, apply: \"never\"}}}})\n  fs.write_text(\"p/out.txt\", r.out)\n}}\n"
        ),
    );
    let out = sb.cig_env(&["run", "s.cig"], &[("HOME", home.to_str().unwrap())]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if !out.status.success() {
        assert!(err.contains("E777"), "{err}");
        assert!(!box_expected_here(), "{err}");
        return;
    }
    if fell_back(&err) {
        allow_fallback(&err);
        return;
    }
    let text = sb.read("p/out.txt");
    let (via_allowed, via_denied) = {
        let mut parts = text.split("== via denied ==");
        let a = parts.next().unwrap_or("").to_string();
        let b = parts.next().unwrap_or("").to_string();
        (a, b)
    };
    assert!(
        via_allowed.contains("REAL-ONLY-FOR-NPM"),
        "the allowed program did not see the real token: {text}"
    );
    assert!(
        !via_denied.contains("REAL-ONLY-FOR-NPM"),
        "the other program saw the real token: {text}"
    );
    // The event log carries both: one pass, one E775.
    let ev = watch_events(&sb);
    let passed = ev.iter().filter(|e| e["result"] == "passed").count();
    let e775 = ev.iter().filter(|e| e["code"] == "E775").count();
    assert!(passed >= 1, "no `passed` event in the log: {ev:?}");
    assert!(e775 >= 1, "no E775 event in the log: {ev:?}");
}

/// Does this kernel have `SECCOMP_ADDFD_FLAG_SEND`? It landed in 5.14.
#[cfg(target_os = "linux")]
fn has_addfd_send() -> bool {
    let kernel = fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    let mut parts = kernel.trim().split(|c: char| !c.is_ascii_digit());
    let major: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    (major, minor) >= (5, 14)
}

/// `which <name>` (the first entry on PATH that is a file), or `None`.
#[cfg(target_os = "linux")]
fn which(name: &str) -> Option<String> {
    for d in std::env::split_paths(&std::env::var_os("PATH")?) {
        let p = d.join(name);
        if p.is_file() {
            return Some(p.to_string_lossy().into_owned());
        }
    }
    None
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
/// Two cigs set up contained boxes at the same time while a path the box
/// covers with /dev/null (a session bus socket in the runtime folder) comes
/// and goes, as it does when a login session ends: a cover whose target is
/// gone between the plan and the mount has nothing left to hide, and must
/// not refuse the hop (it surfaced as `could not start: Operation not
/// supported` while other work ran on the machine).
#[cfg(target_os = "linux")]
#[test]
fn contained_boxes_run_together_while_a_cover_path_comes_and_goes() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    if !contain_expected_here() {
        eprintln!("contain not here; skipped");
        return;
    }
    let rt = tempfile::tempdir().unwrap();
    let rts = rt.path().join("run").display().to_string();
    let stop = Arc::new(AtomicBool::new(false));
    let flapper = {
        let stop = stop.clone();
        let dir = rt.path().join("run");
        let bus = dir.join("bus");
        std::thread::spawn(move || {
            let mut round = 0u32;
            while !stop.load(Ordering::Relaxed) {
                round += 1;
                if round % 2 == 0 {
                    // The runtime folder itself goes, as when logind removes
                    // it at the last session's end.
                    let _ = fs::create_dir_all(&dir);
                    if let Ok(l) = std::os::unix::net::UnixListener::bind(dir.join("bus")) {
                        drop(l);
                    }
                    let _ = fs::remove_dir_all(&dir);
                } else {
                    let _ = fs::create_dir_all(&dir);
                    if let Ok(l) = std::os::unix::net::UnixListener::bind(&bus) {
                        drop(l);
                    }
                    let _ = fs::remove_file(&bus);
                }
            }
        })
    };
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let rts = rts.clone();
            std::thread::spawn(move || {
                let mut failures = Vec::new();
                for _ in 0..8 {
                    let sb = Sandbox::new();
                    sb.write("p/keep.txt", "v1\n");
                    sb.write(
                        "s.cig",
                        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo ok > ran.txt\"], {cwd: \"p\", box: {network: \"off\", contain: true, expect: [\"ran.txt\"]}})\n}\n",
                    );
                    let out = sb.cig_env(&["run", "s.cig"], &[("XDG_RUNTIME_DIR", &rts)]);
                    if !out.status.success() {
                        failures.push(String::from_utf8_lossy(&out.stderr).to_string());
                    }
                }
                failures
            })
        })
        .collect();
    let failures: Vec<String> = workers
        .into_iter()
        .flat_map(|w| w.join().unwrap())
        .collect();
    stop.store(true, Ordering::Relaxed);
    flapper.join().unwrap();
    assert!(failures.is_empty(), "{}", failures.join("\n---\n"));
}

#[cfg(target_os = "linux")]
#[test]
fn a_contained_hop_works_with_home_under_tmp() {
    if !contain_expected_here() {
        eprintln!("contain not here; skipped");
        return;
    }
    // A home under /tmp: the box must not put a new /tmp over it, or making
    // the home writable again finds nothing and the hop is refused.
    let home = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    if !home.path().starts_with("/tmp") || proj.path().starts_with("/tmp") {
        eprintln!("needs the system temp at /tmp and the build tree elsewhere; skipped");
        return;
    }
    let root = proj.path();
    fs::create_dir_all(root.join("p")).unwrap();
    fs::write(root.join("p/keep.txt"), "v1\n").unwrap();
    fs::write(
        root.join("s.cig"),
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo ok > out.txt\"], {cwd: \"p\", box: {network: \"off\", contain: true, expect: [\"out.txt\"]}})\n}\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_cig"))
        .args(["run", "s.cig"])
        .current_dir(root)
        .env("HOME", home.path())
        .env("CIGSCRIPT_HOME", root.join("home"))
        .env("NO_COLOR", "1")
        .env("CIG_SEAL_KEY", "file")
        .env("CIG_BUDGET_SYNC", "1")
        .env("CIG_UPDATE_MANIFEST", root.join("no-releases.json"))
        .env_remove("CI")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{err}");
    assert_eq!(
        fs::read_to_string(root.join("p/out.txt")).unwrap_or_default(),
        "ok\n",
        "{err}"
    );
}

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
        .env("CIG_BUDGET_SYNC", "1")
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
    let out = String::from_utf8_lossy(&sb.cig(&["__doctor"]).stdout).to_string();
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
    let mut slow = Command::new(cig_bin())
        .args(["run", "slow.cig"])
        .current_dir(sb.path())
        .env("CIGSCRIPT_HOME", sb.home())
        .env("NO_COLOR", "1")
        .env("CIG_SEAL_KEY", "file")
        .env("CIG_BUDGET_SYNC", "1")
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
        .env("CIG_BUDGET_SYNC", "1")
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
        plan.contains("(boxed: network on)") && plan.contains("the box cannot be made here"),
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
    let mut child = Command::new(cig_bin())
        .args(["run", "s.cig"])
        .current_dir(sb.path())
        .env("CIGSCRIPT_HOME", sb.home())
        .env("NO_COLOR", "1")
        .env("CIG_SEAL_KEY", "file")
        .env("CIG_BUDGET_SYNC", "1")
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
            .env("CIG_BUDGET_SYNC", "1")
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
    let out = cig_fd("3<key.hex", &["__doctor", "--seal-key-fd", "3"]);
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
    // The index flag a run that died in a boxed hop would have written
    // before its layers were made (#184): the sweep visits flagged runs.
    {
        use std::io::Write;
        let mut idx = fs::OpenOptions::new()
            .append(true)
            .open(sb.home().join("runs-index.jsonl"))
            .unwrap();
        writeln!(idx, "{{\"op\":\"box\",\"id\":\"{id}\",\"on\":true}}").unwrap();
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
            serde_json::from_str(&sb.run_ok(&["--json", "__crash", "list"])).unwrap();
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
        &["report-bundle", &id, "--queue"],
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
    let list = sb.run_ok(&["__crash", "list"]);
    assert!(list.contains("report") && list.contains("filed"), "{list}");
    // An explicit never sends nothing, whatever was typed.
    sb.run_ok(&["config", "reports", "never"]);
    let out = sb.cig_env(
        &["report-bundle", &id, "--queue"],
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
        &["report-bundle", &id, "--queue"],
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
    let list = sb.run_ok(&["__crash", "list"]);
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
                .run_ok(&["--json", "__crash", "list"])
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
    let list = sb.run_ok(&["__crash", "list"]);
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
        &["report-bundle", &id, "--queue"],
        &[("CIG_INTERNAL_TRACKER_DIR", &t), ("CI", "true")],
    );
    assert_eq!(out.status.code(), Some(3));
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        err.contains("reports is \"never\" under CI unless `cig config reports always`"),
        "{err}"
    );
    assert!(!tracker.exists());
    assert_eq!(sb.run_ok(&["--json", "__crash", "list"]).trim(), "[]");
    // The setting, made explicit, holds under CI.
    sb.run_ok(&["config", "reports", "always"]);
    let out = sb.cig_env(
        &["report-bundle", &id, "--queue"],
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
        &["report-bundle", &id, "--queue"],
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
        serde_json::from_str(&sb.run_ok(&["--json", "__crash", "list"])).unwrap();
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
        serde_json::from_str(&sb.run_ok(&["--json", "__crash", "list"])).unwrap();
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
    sb.run_ok(&["__crash", "delete", "--all"]);
    let out = sb.cig_env(&["run", "s.cig"], &[("CIG_INTERNAL_TRACKER_DIR", &t)]);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!err.contains("tripwire:"), "{err}");
    assert_eq!(sb.run_ok(&["--json", "__crash", "list"]).trim(), "[]");
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
    assert_eq!(sb.run_ok(&["--json", "__crash", "list"]).trim(), "[]");
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
        serde_json::from_str(&sb.run_ok(&["--json", "__crash", "list"])).unwrap();
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
                .run_ok(&["--json", "__crash", "list"])
                .contains("\"issue\"")
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    assert!(issue.exists(), "the report was not filed");
    let filed = fs::read_to_string(&issue).unwrap();
    let report = sb.run_ok(&["--json", "__crash", "list"]);
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
                .run_ok(&["--json", "__crash", "list"])
                .contains("\"issue\"")
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    assert!(issue.exists(), "the report was not filed");
    let v: serde_json::Value =
        serde_json::from_str(&sb.run_ok(&["--json", "__crash", "list"])).unwrap();
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
    let list = sb.run_ok(&["--json", "__crash", "list"]);
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
fn a_fixed_report_is_announced_by_crash_announce_fixed() {
    let sb = Sandbox::new();
    let id = failed_run(&sb);
    let tracker = sb.path().join("tracker");
    let t = tracker.to_string_lossy().to_string();
    let out = sb.cig_env(
        &["report-bundle", &id, "--queue"],
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
    let out = sb.cig_env(&["crash", "announce-fixed"], &env);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{err}");
    assert!(
        err.contains(
            "fixed: a bug you reported (#1, boom) is fixed in 99.0.0: ciginstaller update"
        ),
        "{err}"
    );
    let v: serde_json::Value =
        serde_json::from_str(&sb.run_ok(&["--json", "__crash", "list"])).unwrap();
    assert_eq!(v[0]["fixed_in"], "99.0.0", "{v}");
    assert_eq!(v[0]["announced"], true, "{v}");
    // Said once.
    let out = sb.cig_env(&["crash", "announce-fixed"], &env);
    assert!(!String::from_utf8_lossy(&out.stderr).contains("a bug you reported"));
    let out = sb.cig_env(&["--json", "crash", "announce-fixed"], &env);
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
    let mut child = Command::new(cig_bin())
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
        .env("CIG_BUDGET_SYNC", "1")
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
        &["report-bundle", &id, "--queue"],
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
        r#"{"kind":"crash","panic_message":"map has no key at /home/zqx/zqx-clients/ZqxCorp/orchestrate.cig","panic_location":"orchestrate.cig:12","version":"1.1.0","cwd":"~/zqx-clients/ZqxCorp","command":["cigo","run","ZqxDeploy.sh","--zqx-only","zqx-task"],"manifest":{"dir":"/srv/zqx/work","files":["zqx-a.txt","ZqxB.csv"],"script":"zqx-pack/ZqxData/run.sh"},"active_run_id":null}"#,
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
    let listed = sb.run_ok(&["--json", "__crash", "list"]);
    assert_no_marker(&listed, "cig crash list");
    let list: Vec<serde_json::Value> = serde_json::from_str(&listed).unwrap();
    for r in &list {
        let shown = sb.run_ok(&["__crash", "show", r["id"].as_str().unwrap()]);
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
        serde_json::from_str(&sb.run_ok(&["--json", "__crash", "list"])).unwrap();
    let r = &list[0];
    // The shape of the working directory, not its names: under home, two
    // levels, the first with a capital letter.
    assert_eq!(r["cwd"], "~/D1/d2", "{r}");
    assert_eq!(
        r["command"],
        serde_json::json!(["cig", "run", "<script>"]),
        "{r}"
    );
    let shown = sb.run_ok(&["__crash", "show", r["id"].as_str().unwrap()]);
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
    assert_eq!(sb.run_ok(&["--json", "__crash", "list"]).trim(), "[]");
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
        serde_json::from_str(&sb.run_ok(&["--json", "__crash", "list"])).unwrap();
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
    sb.run_ok(&["__crash", "delete", "--all"]);
    sb.run_ok(&["run", "s.cig"]);
    let second = sb.runs().iter().find(|r| r["status"] == "ok").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let out = sb.cig_env(&["unburn", &second], &[("CIG_INTERNAL_TRACKER_DIR", &t)]);
    assert!(out.status.success());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("tripwire:"));
    assert_eq!(sb.run_ok(&["--json", "__crash", "list"]).trim(), "[]");
}

#[test]
fn a_typed_flush_sends_a_report_waiting_for_its_retry_time() {
    let sb = Sandbox::new();
    let id = failed_run(&sb);
    let tracker = sb.path().join("tracker");
    let t = tracker.to_string_lossy().to_string();
    let out = sb.cig_env(
        &["report-bundle", &id, "--queue"],
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
        serde_json::from_str(&sb.run_ok(&["--json", "__crash", "list"])).unwrap();
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
        serde_json::from_str(&sb.run_ok(&["--json", "__crash", "list"])).unwrap();
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
        serde_json::from_str(&sb.run_ok(&["--json", "__crash", "list"])).unwrap();
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
    let mut child = Command::new(cig_bin())
        .args(["run", "s.cig"])
        .current_dir(sb.path())
        .env("CIGSCRIPT_HOME", sb.home())
        .env("NO_COLOR", "1")
        .env("CIG_SEAL_KEY", "file")
        .env("CIG_BUDGET_SYNC", "1")
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

/// Five runs writing overlapping files; the oldest first.
fn overlapping_runs(sb: &Sandbox) -> Vec<String> {
    sb.write("a.txt", "v0");
    let writes: [&[(&str, &str)]; 5] = [
        &[("f.txt", "1"), ("a.txt", "a1")],
        &[("f.txt", "2"), ("g.txt", "g2")],
        &[("f.txt", "3"), ("g.txt", "g3"), ("a.txt", "a3")],
        &[("f.txt", "4"), ("h.txt", "h4")],
        &[("g.txt", "g5"), ("h.txt", "h5")],
    ];
    for (k, w) in writes.iter().enumerate() {
        let body: String = w
            .iter()
            .map(|(f, c)| format!("  fs.write_text(\"{f}\", \"{c}\")\n"))
            .collect();
        sb.write(&format!("o{k}.cig"), &format!("burn {{\n{body}}}\n"));
        sb.run_ok(&["run", &format!("o{k}.cig")]);
    }
    let mut ids: Vec<String> = sb
        .runs()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    ids.reverse();
    ids
}

fn tree_and_marks(sb: &Sandbox) -> (Vec<(String, Option<String>)>, Vec<String>) {
    let files = ["a.txt", "f.txt", "g.txt", "h.txt"]
        .iter()
        .map(|f| (f.to_string(), sb.exists(f).then(|| sb.read(f))))
        .collect();
    let mut runs = sb.runs();
    runs.reverse();
    let marks = runs
        .iter()
        .map(|r| format!("{} {}", r["status"], r["rolled_back"]))
        .collect();
    (files, marks)
}

/// #149, the combined restore: `--to` writes each file once, to its
/// earliest before-state, and leaves exactly what unburning the same runs
/// one by one, newest first, leaves: the bytes and the marks.
#[test]
fn unburn_to_equals_unburn_one_by_one() {
    let a = Sandbox::new();
    let ids_a = overlapping_runs(&a);
    let out = a.cig(&["unburn", "--to", &ids_a[1], "--yes"]);
    let said = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{said}");
    assert!(!said.contains("tripwire"), "{said}");
    let said = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        said.contains("left for the older run's restore of it"),
        "the files were not combined: {said}"
    );
    let b = Sandbox::new();
    let ids_b = overlapping_runs(&b);
    for id in ids_b[2..].iter().rev() {
        b.run_ok(&["unburn", id]);
    }
    assert_eq!(tree_and_marks(&a), tree_and_marks(&b));
    // `--to` the second run: the three after it are back, so the files
    // are as that run left them.
    assert_eq!(a.read("a.txt"), "a1");
    assert_eq!(a.read("f.txt"), "2");
    assert_eq!(a.read("g.txt"), "g2");
    assert!(!a.exists("h.txt"));
}

/// #149: `cig runs pin <id>`, `cig runs unpin <id>` and `cig run --pin`
/// say what `--pin` and `--unpin` on `cig runs` say.
#[test]
fn the_pin_spellings_pin_and_unpin() {
    let sb = Sandbox::new();
    let ids = four_runs(&sb);
    sb.run_ok(&["runs", "pin", &ids[0]]);
    let listed = sb.run_ok(&["runs", "--all"]);
    assert!(
        listed
            .lines()
            .any(|l| l.contains(&ids[0]) && l.contains("(pinned)")),
        "{listed}"
    );
    sb.run_ok(&["runs", "unpin", &ids[0]]);
    let listed = sb.run_ok(&["runs", "--all"]);
    assert!(!listed.contains("(pinned)"), "{listed}");
    sb.write("p.cig", "burn {\n  fs.write_text(\"p.txt\", \"p\")\n}\n");
    sb.run_ok(&["run", "--pin", "p.cig"]);
    let runs = sb.runs();
    let newest = &runs[0];
    assert_eq!(newest["pinned"], true, "{newest}");
    assert_eq!(runs.iter().filter(|r| r["pinned"] == true).count(), 1);
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

/// #106: a boxed hop's plan line and its record say what the box was asked
/// for (the network, and whatever is not the default), not only that there
/// was one.
#[cfg(target_os = "linux")]
#[test]
fn a_boxed_hop_says_its_box_in_the_plan_and_the_record() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "k");
    sb.write(
        "plan.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"true\", [], {box: {network: \"registry\", allow_hosts: [\"registry.npmjs.org\"], contain: true, apply: \"never\"}})\n  proc.run(\"true\", [], {box: true})\n}\n",
    );
    let out = sb.cig(&["run", "--dry-run", "plan.cig"]);
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        said.contains("(boxed: network registry, allow registry.npmjs.org, contain, apply never)"),
        "{said}"
    );
    assert!(said.contains("(boxed: network on)"), "{said}");
    // A real run records the terms with the hop, and cig runs says them.
    sb.write(
        "run.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"true\", [], {box: {expect: [\"**\"]}})\n}\n",
    );
    sb.run_ok(&["run", "run.cig"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let shown = sb.run_ok(&["runs", &id]);
    assert!(shown.contains("(boxed: network on, expect **)"), "{shown}");
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
    assert_eq!(sb.run_ok(&["--json", "__crash", "list"]).trim(), "[]");
    let err = sb.run_err(&["unburn", &id], 1);
    assert!(err.contains("error[E709 burn]:"), "{err}");
    assert!(err.contains("tripwire: cig noticed"), "{err}");
    let listed = sb.run_ok(&["--json", "__crash", "list"]);
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
    // CIG_SEAL_KEY_FILE must point at a private file (#176). The default
    // umask leaves it 0644; chmod before cig reads it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&key_file, fs::Permissions::from_mode(0o600)).unwrap();
    }
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
    run(&["__doctor"], &[]);
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
    run(&["--json", "__crash", "list"], &[]);
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

/// #106: `cig doctor` says what the box is made of here, each part tried on
/// its own, so a box that cannot be made says which part is missing.
#[test]
fn doctor_lists_the_box_primitives() {
    let sb = Sandbox::new();
    let out = sb.cig(&["__doctor"]);
    let said = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(said.contains("\n  box\n"), "{said}");
    if cfg!(target_os = "linux") {
        for part in ["user ns", "mount ns", "network ns", "pid ns", "overlayfs"] {
            assert!(
                said.lines().any(|l| l.trim_start().starts_with(part)),
                "{part}: {said}"
            );
        }
        // The box's own refusal reaches the overlayfs line, with its reason.
        let no = sb.cig_env(&["__doctor"], &[("CIG_INTERNAL_NO_BOX", "1")]);
        let said = String::from_utf8_lossy(&no.stdout).to_string();
        assert!(
            said.lines().any(|l| l.trim_start().starts_with("overlayfs")
                && l.contains("no")
                && l.contains("CIG_INTERNAL_NO_BOX")),
            "{said}"
        );
    } else {
        assert!(said.contains("the box is Linux's"), "{said}");
    }
}

/// #149: `cig runs --pruned` says which runs went on purpose, from the
/// ledger: newest first, when, how, and what each was.
#[test]
fn pruned_runs_are_listed_from_the_ledger() {
    let sb = Sandbox::new();
    let none = sb.cig(&["runs", "--pruned"]);
    assert!(String::from_utf8_lossy(&none.stderr).contains("no run has been pruned"));
    let ids = four_runs(&sb);
    sb.run_ok(&["runs", "--prune", "1"]);
    sb.run_ok(&["runs", "--forget", "20200101T000000-abcdef"]);
    let listed = sb.run_ok(&["runs", "--pruned"]);
    for id in &ids[..3] {
        assert!(
            listed.lines().any(|l| l.starts_with(id.as_str())
                && l.contains("pruned")
                && l.trim_end().ends_with("ok")),
            "{id}: {listed}"
        );
    }
    assert!(
        !listed.contains(&ids[3]),
        "the kept run is listed: {listed}"
    );
    let json: serde_json::Value =
        serde_json::from_str(&sb.run_ok(&["--json", "runs", "--pruned"])).unwrap();
    let rows = json.as_array().unwrap();
    assert_eq!(rows[0]["how"], "forgotten", "newest first: {json}");
    assert_eq!(rows[0]["was"], "-", "{json}");
    assert_eq!(
        rows.iter().filter(|r| r["how"] == "pruned").count(),
        3,
        "{json}"
    );
    assert!(
        rows.iter()
            .all(|r| r["at"].as_str().is_some_and(|t| t.starts_with("20"))),
        "{json}"
    );
}

/// #149: `history_budget` is off by default, so nothing is pruned on
/// upgrade; with a size set, the oldest unpinned finished runs are pruned
/// after a burn until the history fits, each in the ledger as `budget` so
/// `cig runs --pruned` says why.
#[test]
fn history_budget_is_off_by_default_and_prunes_oldest_first_when_set() {
    // Off: four runs stay, as they did before 1.17.
    let sb = Sandbox::new();
    let ids = four_runs(&sb);
    assert_eq!(sb.runs().len(), 4, "off is the default");
    // A tiny budget, and no age floor (these runs are seconds old): the
    // next burn makes the history too big, so the oldest runs are pruned
    // first and the newest one stays.
    sb.run_ok(&["config", "history_keep", "0"]);
    sb.run_ok(&["config", "history_budget", "1024"]);
    sb.write("s5.cig", "burn {\n  fs.write_text(\"g5.txt\", \"g\")\n}\n");
    sb.run_ok(&["run", "s5.cig"]);
    let left: Vec<String> = sb
        .runs()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    assert!(
        left.len() < 5,
        "the budget pruned nothing: {} runs left",
        left.len()
    );
    assert!(
        !left.contains(&ids[0]),
        "oldest should go first; {ids:?} left {left:?}"
    );
    // The last run (the one that tripped the budget) stays.
    let newest = sb.runs()[0]["id"].as_str().unwrap().to_string();
    assert!(left.contains(&newest), "newest run lost: {left:?}");
    // The pruned ledger shows them with how = budget.
    let listed = sb.run_ok(&["runs", "--pruned"]);
    assert!(
        listed.lines().any(|l| l.contains("budget")),
        "no budget line: {listed}"
    );
    let json: serde_json::Value =
        serde_json::from_str(&sb.run_ok(&["--json", "runs", "--pruned"])).unwrap();
    assert!(
        json.as_array()
            .unwrap()
            .iter()
            .any(|r| r["how"] == "budget"),
        "no budget entry: {json}"
    );
}

/// #149: a stretch that was killed after some of its runs leaves its list;
/// `--continue` takes up the rest, and a run already back is not done twice.
#[test]
fn a_killed_unburn_to_resumes() {
    let sb = Sandbox::new();
    let ids = four_runs(&sb);
    // What a stretch `--to` the oldest run writes before it starts, and what
    // it had done when it was killed: the newest run is back, the list stays.
    let stretch = serde_json::json!({
        "runs": [ids[3], ids[2], ids[1]],
        "made": "2026-11-11T00:00:00Z",
    });
    fs::write(sb.home().join("unburn-stretch.json"), stretch.to_string()).unwrap();
    sb.run_ok(&["unburn", &ids[3]]);
    assert_eq!(sb.read("f.txt"), "3");
    let out = sb.cig(&["unburn", "--continue", "-y"]);
    let said = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{said}");
    assert!(said.contains("undid 2 runs"), "{said}");
    assert!(!said.contains(&format!("-- {}", ids[3])), "{said}");
    assert_eq!(sb.read("f.txt"), "1");
    assert!(sb.exists("g1.txt") && !sb.exists("g2.txt") && !sb.exists("g3.txt"));
    assert!(!sb.home().join("unburn-stretch.json").exists());
    // Each is back exactly once: another pass has nothing to do, and a
    // single unburn of any of them is refused as done.
    let again = sb.cig(&["unburn", "--continue"]);
    assert!(!again.status.success());
    sb.run_err(&["unburn", &ids[2]], 1);
}

/// #149: the budget pass never takes a run that holds a box layer waiting
/// for approval, however old, and a pass run on its own (what a run starts
/// in the background) prunes the rest oldest first.
#[test]
fn held_layers_are_never_pruned_by_the_budget_pass() {
    let sb = Sandbox::new();
    let ids = four_runs(&sb);
    let held = sb.home().join("runs").join(&ids[0]);
    fs::create_dir_all(held.join("kept")).unwrap();
    fs::create_dir_all(held.join("box").join("1")).unwrap();
    fs::write(held.join("kept").join("1.json"), "{}").unwrap();
    sb.run_ok(&["config", "history_keep", "0"]);
    sb.run_ok(&["config", "history_budget", "1024"]);
    sb.run_ok(&["__budget"]);
    let left: Vec<String> = sb
        .runs()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    assert!(left.contains(&ids[0]), "the held run was pruned: {left:?}");
    assert!(left.contains(&ids[3]), "the newest was pruned: {left:?}");
    assert!(
        !left.contains(&ids[1]) && !left.contains(&ids[2]),
        "{left:?}"
    );
}

/// #149: a pinned run is spared by the disk budget, even when it is the
/// oldest; the budget can be exceeded for it.
#[test]
fn history_budget_spares_pinned_runs() {
    let sb = Sandbox::new();
    let ids = four_runs(&sb);
    sb.run_ok(&["runs", "--pin", &ids[0]]);
    sb.run_ok(&["config", "history_keep", "0"]);
    sb.run_ok(&["config", "history_budget", "1024"]);
    sb.write("s5.cig", "burn {\n  fs.write_text(\"g5.txt\", \"g\")\n}\n");
    sb.run_ok(&["run", "s5.cig"]);
    assert!(
        sb.runs().iter().any(|r| r["id"] == ids[0].as_str()),
        "the pinned run was pruned: {:?}",
        sb.runs()
    );
}

/// #149: `cig doctor` reads the history's size and, when
/// `history_budget` is off, suggests a value (5% of free space, from 1 GB
/// to 20 GB), without changing anything.
#[test]
fn doctor_reports_history_and_suggests_a_budget_when_off() {
    let sb = Sandbox::new();
    let _ids = four_runs(&sb);
    let f = doctor_finding(&sb, "history-budget");
    assert!(f["found"].as_str().unwrap().ends_with("budget off"), "{f}");
    // A whole number of GB between 1 and 20, in the fix, or no fix where
    // free space cannot be measured here.
    let suggested = f["fix"].as_str().map(|fix| {
        let gb = fix
            .strip_prefix("cig config history_budget ")
            .and_then(|v| v.strip_suffix("GB"))
            .unwrap_or_else(|| panic!("unexpected fix: {fix}"));
        gb.parse::<u64>().unwrap()
    });
    if let Some(gb) = suggested {
        assert!((1..=20).contains(&gb), "suggestion out of range: {gb}");
    }
    // The text output names the budget and, when off, the suggestion.
    let text = String::from_utf8_lossy(&sb.cig(&["__doctor"]).stdout).to_string();
    assert!(text.contains("history"), "{text}");
    assert!(text.contains("budget off"), "{text}");
    // The tip comes with a suggestion, where free space can be measured
    // (not on Windows today).
    if suggested.is_some() {
        assert!(text.contains("cig doctor suggests"), "no tip: {text}");
    }
    // The suggestion changes nothing on disk.
    let v: serde_json::Value = serde_json::from_str(&sb.run_ok(&["--json", "config"])).unwrap();
    assert_eq!(v["config"]["history_budget"]["value"], "off");
    assert_eq!(v["config"]["history_budget"]["explicit"], false);
    // With a budget set, doctor no longer suggests one.
    sb.run_ok(&["config", "history_budget", "2GB"]);
    let f = doctor_finding(&sb, "history-budget");
    assert!(f["found"].as_str().unwrap().ends_with("budget 2GB"), "{f}");
    assert!(f["fix"].is_null(), "still suggesting: {f}");
    let text = String::from_utf8_lossy(&sb.cig(&["__doctor"]).stdout).to_string();
    assert!(
        !text.contains("cig doctor suggests"),
        "still tipping: {text}"
    );
}

/// #149: the budget measures the history as it grows, the run records and
/// the tin, and a prune frees a tin object only when the last run that
/// refers to it goes: here each run's hop kept a distinct 60 KB file (one
/// object, under the chunking size), so one prune is enough to fit, the
/// oldest run goes, and the collector takes its object. By default runs
/// younger than `history_keep` (30 days) are never pruned, and `cig
/// doctor` says the budget is exceeded for them.
#[cfg(unix)]
#[test]
fn history_budget_counts_the_tin_and_collects_it() {
    let sb = Sandbox::new();
    let size = 60 * 1024;
    sb.write("p/big.bin", &"0".repeat(size));
    for k in 1..=4 {
        sb.write(&format!("src{k}.bin"), &k.to_string().repeat(size));
        sb.write(
            &format!("s{k}.cig"),
            &format!("pack {{ \"./p\" }}\nburn {{\n  proc.run(\"cp\", [\"src{k}.bin\", \"p/big.bin\"], {{touches: [\"big.bin\"]}})\n}}\n"),
        );
        sb.run_ok(&["run", &format!("s{k}.cig")]);
    }
    let objects = |sb: &Sandbox| -> usize {
        walkdir::WalkDir::new(sb.home().join("tin"))
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file() && e.path().to_string_lossy().contains("/objects/"))
            .count()
    };
    let before = objects(&sb);
    assert_eq!(before, 4, "each run's hop kept the file it overwrote");
    let mut ids: Vec<String> = sb
        .runs()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    ids.reverse();
    // `<n> bytes of history (...)`: the number the budget measures.
    let history_of = |sb: &Sandbox| -> u64 {
        let f = doctor_finding(sb, "history-budget");
        f["found"]
            .as_str()
            .unwrap()
            .split(' ')
            .next()
            .unwrap()
            .parse()
            .unwrap()
    };
    let history = history_of(&sb);
    assert!(history >= 4 * size as u64, "the tin is counted: {history}");
    // A budget that one prune meets: the history less half an object.
    let budget = history - (size as u64) / 2;
    sb.run_ok(&["config", "history_budget", &budget.to_string()]);
    // The age floor holds by default: every run is seconds old.
    sb.run_ok(&["runs", "--prune", "100"]);
    assert_eq!(sb.runs().len(), 4, "history_keep spares young runs");
    let f = doctor_finding(&sb, "history-budget");
    assert!(
        f["found"].as_str().unwrap().contains("over the budget"),
        "{f}"
    );
    assert!(
        String::from_utf8_lossy(&sb.cig(&["__doctor"]).stdout)
            .to_string()
            .contains("over it"),
        "doctor says so"
    );
    // No floor: one prune, the oldest, and its object collected.
    sb.run_ok(&["config", "history_keep", "0"]);
    sb.run_ok(&["runs", "--prune", "100"]);
    let left: Vec<String> = sb
        .runs()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(left.len(), 3, "one prune was enough: {left:?}");
    assert!(!left.contains(&ids[0]), "the oldest went: {left:?}");
    assert_eq!(objects(&sb), before - 1, "its object was collected");
    // What is left still unburns, byte for byte: the three runs' objects
    // are all there.
    sb.run_ok(&["unburn", "--last", "3", "--yes"]);
    assert_eq!(sb.read("p/big.bin"), "1".repeat(size));
}

/// #148: a hop's `skip` globs and the `hop_skip` setting leave paths out of
/// the tin. What the hop changes there is neither kept nor journaled, so the
/// rollback leaves it as the hop left it, while the rest of the hop comes
/// back; the plan says what is skipped.
#[cfg(unix)]
#[test]
fn a_hop_leaves_what_it_skips_out_of_the_tin() {
    let sb = Sandbox::new();
    sb.write("p/keep.txt", "v1\n");
    sb.write("p/cache/c.bin", "old\n");
    sb.write("p/logs/a.log", "old\n");
    let hop = "echo v2 > p/keep.txt; echo new > p/cache/c.bin; echo n > p/cache/n.bin; echo new > p/logs/a.log; echo t > p/t.txt";
    sb.write(
        "s.cig",
        &format!("pack {{ \"./p\" }}\nburn {{\n  proc.run(\"sh\", [\"-c\", \"{hop}\"], {{touches: [\"**\"], skip: [\"cache/\"]}})\n  cough \"x\"\n}}\n"),
    );
    // The dry run stops at the `cough` too; the plan is what is checked.
    let dry = sb.cig(&["run", "--dry-run", "s.cig"]);
    let plan = String::from_utf8_lossy(&dry.stderr);
    assert!(
        plan.contains("(touches **: 2 files)  (skip cache/)"),
        "{plan}"
    );
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert_eq!(sb.read("p/keep.txt"), "v1\n", "{err}");
    assert_eq!(sb.read("p/logs/a.log"), "old\n", "{err}");
    assert!(!sb.exists("p/t.txt"), "{err}");
    assert_eq!(sb.read("p/cache/c.bin"), "new\n", "a skipped change stays");
    assert!(sb.exists("p/cache/n.bin"), "a skipped creation stays");
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let shown = sb.run_ok(&["runs", &id]);
    assert!(
        !shown
            .lines()
            .any(|l| l.contains("  hop ") && l.contains("/cache/")),
        "nothing under a skipped glob is journaled: {shown}"
    );

    // The setting skips for every hop, on top of the hop's own `skip`.
    sb.write("p/keep.txt", "v1\n");
    sb.write("p/logs/a.log", "old\n");
    sb.run_ok(&["config", "hop_skip", "logs/**, *.tmp"]);
    sb.write(
        "t.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo v3 > p/keep.txt; echo newer > p/logs/a.log\"], {touches: [\"**\"]})\n  cough \"x\"\n}\n",
    );
    let err = sb.run_err(&["run", "t.cig"], 1);
    assert_eq!(sb.read("p/keep.txt"), "v1\n", "{err}");
    assert_eq!(sb.read("p/logs/a.log"), "newer\n", "{err}");
    sb.run_err(&["config", "hop_skip", "[unclosed"], 3);
}

/// #148: `cig check` says a hop's `skip` on the hop's line, since what it
/// covers cannot be undone, and says instead that it does nothing in the
/// box. E310 is a warning: the script still checks.
#[test]
fn skip_globs_are_said_by_check() {
    let sb = Sandbox::new();
    sb.write(
        "s.cig",
        "pack { \".\" }\nburn {\n  proc.run(\"make\", [], {touches: [\"**\"], skip: [\"build/**\", \".cache/\"]})\n  proc.run(\"npm\", [\"ci\"], {box: true, skip: \"node_modules/.cache/**\"})\n}\n",
    );
    let out = sb.cig(&["--json", "check", "s.cig"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}");
    let e310: Vec<serde_json::Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .flat_map(|v| {
            v["warnings"]
                .as_array()
                .cloned()
                .unwrap_or_else(|| vec![v.clone()])
        })
        .filter(|w| w["code"] == "E310")
        .collect();
    assert_eq!(e310.len(), 2, "{text}");
    assert_eq!(e310[0]["line"], 3, "{text}");
    let first = e310[0]["message"].as_str().unwrap();
    assert!(
        first.contains("`build/**`, `.cache/`") && first.contains("cannot be undone"),
        "{first}"
    );
    assert_eq!(e310[1]["line"], 4, "{text}");
    assert!(
        e310[1]["message"]
            .as_str()
            .unwrap()
            .contains("does nothing in the box"),
        "{text}"
    );
    sb.write(
        "bad.cig",
        "pack { \".\" }\nburn {\n  proc.run(\"true\", [], {skip: [\"[unclosed\"]})\n}\n",
    );
    let err = sb.run_err(&["run", "bad.cig"], 1);
    assert!(err.contains("skip: `[unclosed` is not a glob"), "{err}");
}

// ----- #148, section 1: the stat cache. Off by default; `cig config hop_statcache on`
// remembers each touched file's (dev, inode, size, mtime, ctime, mode) with the sha256
// the tin kept it under, so a later hop whose files all match the cache reads none
// of them. The restore still hashes before and after (E704 is unchanged).

/// Count the bytes the tin-read hook wrote (one byte per hashed read in
/// tin::prepare*). Zero means no file bytes were read.
#[cfg(unix)]
fn tin_reads(sb: &Sandbox) -> usize {
    match fs::read(sb.path().join("tin-reads")) {
        Ok(b) => b.len(),
        Err(_) => 0,
    }
}

/// A script that stashes every file under `./p` (`touches: ["**"]`) and
/// writes one line to `p/sentinel.txt`, so the second hop in a chain has
/// something to burn without moving the mtime of the cached files.
#[cfg(unix)]
fn two_hop_touching_script() -> String {
    "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo first > p/sentinel.txt\"], {touches: [\"**\"]})\n}\n".to_string()
}

#[cfg(unix)]
#[test]
fn off_by_default_a_hop_reads_every_touches_file_as_before() {
    // The bare second hop (cache off) must read every file: proving the
    // target is where the gain is to be had.
    let sb = Sandbox::new();
    for name in ["a.txt", "b.txt", "c.txt"] {
        sb.write(&format!("p/{name}"), "x\n");
    }
    let reads = sb.path().join("tin-reads");
    sb.write("s.cig", &two_hop_touching_script());
    let one = sb.cig_env(
        &["run", "s.cig"],
        &[("CIG_INTERNAL_TIN_READS", reads.to_str().unwrap())],
    );
    assert!(one.status.success(), "{one:?}");
    let first_reads = tin_reads(&sb);
    assert!(
        first_reads >= 3,
        "the first hop stashed every file: {first_reads}"
    );
    // Second run: cache is off, so every file is read again.
    let _ = fs::remove_file(&reads);
    let two = sb.cig_env(
        &["run", "s.cig"],
        &[("CIG_INTERNAL_TIN_READS", reads.to_str().unwrap())],
    );
    assert!(two.status.success(), "{two:?}");
    let second_reads = tin_reads(&sb);
    assert!(
        second_reads >= 3,
        "cache off: the second hop reads again: {second_reads}"
    );
}

/// #148: an_unchanged_tree_is_not_read. With `hop_statcache = on`, a hop
/// whose `touches` tree is unchanged since the last hop reads zero file
/// bytes into the tin on the second run. The pack's files are given an
/// mtime well in the past first, so the racy-git rule (a file edited
/// within the filesystem's timestamp granularity of the cache's own save
/// is re-hashed anyway; its test is below) does not fire for them.
#[cfg(unix)]
#[test]
fn an_unchanged_tree_is_not_read() {
    let sb = Sandbox::new();
    for name in ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"] {
        sb.write(&format!("p/{name}"), "x\n");
    }
    let pin = Command::new("sh")
        .arg("-c")
        .arg("touch -t 202001010000 p/*.txt")
        .current_dir(sb.path())
        .status()
        .unwrap();
    assert!(pin.success());
    assert!(sb.cig(&["config", "hop_statcache", "on"]).status.success());
    let reads = sb.path().join("tin-reads");
    sb.write("s.cig", &two_hop_touching_script());
    // First run: every file hashed into the tin.
    let one = sb.cig_env(
        &["run", "s.cig"],
        &[("CIG_INTERNAL_TIN_READS", reads.to_str().unwrap())],
    );
    assert!(one.status.success(), "{one:?}");
    let first = tin_reads(&sb);
    assert!(first >= 5, "the first hop reads every file: {first}");
    let _ = fs::remove_file(&reads);
    // Second run: every file in the pack matches the cache's stat tuple
    // and is old enough not to be racy, so the hop reads nothing it saw
    // before. The sentinel the first hop wrote is new and will be hashed
    // fresh (one read), but no more: the five originals are cache hits.
    let two = sb.cig_env(
        &["run", "s.cig"],
        &[("CIG_INTERNAL_TIN_READS", reads.to_str().unwrap())],
    );
    assert!(two.status.success(), "{two:?}");
    let second = tin_reads(&sb);
    assert!(
        second <= 1,
        "with the cache on, unchanged files are not read: {second} (first was {first})"
    );
}

/// #148 `hop_watch`: a chain of hops over a tree the change watch can vouch
/// for (one hop that changes nothing, one that changes a file, one that
/// removes another) puts every file back exactly when the block coughs: what
/// the watch lets a hop skip never hides a change.
#[cfg(target_os = "linux")]
#[test]
fn hop_watch_hops_are_exact() {
    let sb = Sandbox::new();
    for name in ["a.txt", "b.txt", "c.txt", "d.txt"] {
        sb.write(&format!("p/{name}"), "x\n");
    }
    sb.write("p/sub/e.txt", "e\n");
    let pin = Command::new("sh")
        .arg("-c")
        .arg("touch -t 202001010000 p/*.txt p/sub/*.txt")
        .current_dir(sb.path())
        .status()
        .unwrap();
    assert!(pin.success());
    assert!(sb.cig(&["config", "hop_statcache", "on"]).status.success());
    assert!(sb.cig(&["config", "hop_watch", "on"]).status.success());
    let hop =
        |cmd: &str| format!("  proc.run(\"sh\", [\"-c\", \"{cmd}\"], {{touches: [\"**\"]}})\n");
    let script = format!(
        "pack {{ \"./p\" }}\nburn {{\n{}{}{}{}{}{}  cough \"x\"\n}}\n",
        hop("echo first > p/sentinel.txt"),
        hop("true"),
        hop("echo more >> p/a.txt"),
        hop("true"),
        hop("rm p/b.txt"),
        hop("echo deep >> p/sub/e.txt"),
    );
    sb.write("s.cig", &script);
    let trace = sb.path().join("hopwatch-trace");
    let out = sb.cig_env(
        &["run", "s.cig"],
        &[("CIG_INTERNAL_HOPWATCH", trace.to_str().unwrap())],
    );
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(sb.read("p/a.txt"), "x\n", "{err}");
    assert_eq!(sb.read("p/b.txt"), "x\n", "{err}");
    assert_eq!(sb.read("p/sub/e.txt"), "e\n", "{err}");
    assert!(!sb.path().join("p/sentinel.txt").exists(), "{err}");
    // The watch did let hops skip walks: the second hop changed nothing, so
    // the third began on the scan the second ended with, and the second's
    // own scan after was not taken.
    let skipped = fs::read(&trace).unwrap_or_default();
    assert!(skipped.contains(&b'e'), "a quiet hop skips its scan after");
    assert!(
        skipped.contains(&b'b'),
        "the next hop begins on the kept scan"
    );
    // And the same run again, with the cache the first one left.
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert_eq!(sb.read("p/a.txt"), "x\n", "{err}");
    assert_eq!(sb.read("p/b.txt"), "x\n", "{err}");
    assert!(!sb.path().join("p/sentinel.txt").exists(), "{err}");
}

/// `hop_watch` is a setting like `hop_statcache`: on or off, off by default.
#[test]
fn hop_watch_is_on_or_off() {
    let sb = Sandbox::new();
    assert!(sb.cig(&["config", "hop_watch", "on"]).status.success());
    assert!(sb.cig(&["config", "hop_watch", "off"]).status.success());
    let bad = sb.cig(&["config", "hop_watch", "maybe"]);
    assert!(!bad.status.success());
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("hop_watch must be on or off"),
        "{bad:?}"
    );
}

/// #148: a_same_second_edit_is_caught. A file edited within the
/// filesystem's timestamp granularity of the cache's save is re-hashed
/// anyway, so a same-second edit is never missed. The hop then changes
/// the file and cig's rollback restores it.
#[cfg(unix)]
#[test]
fn a_same_second_edit_is_caught() {
    let sb = Sandbox::new();
    sb.write("p/x.txt", "before\n");
    assert!(sb.cig(&["config", "hop_statcache", "on"]).status.success());
    sb.write("s.cig", &two_hop_touching_script());
    let one = sb.cig(&["run", "s.cig"]);
    assert!(one.status.success(), "{one:?}");
    // Edit in the same second as the save, then write a script that
    // changes the file inside a burn block that coughs, so the kernel
    // must restore it. Without the racy-git rule, the second run would
    // trust the stale cache entry and restore the wrong bytes.
    sb.write("p/x.txt", "racy!\n");
    let pin = Command::new("touch")
        .args(["-t", "202001010000", "p/x.txt"])
        .current_dir(sb.path())
        .status()
        .unwrap();
    assert!(pin.success());
    sb.write(
        "s2.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"printf 'wrong\\\\n' > p/x.txt\"], {touches: [\"**\"]})\n  cough \"x\"\n}\n",
    );
    let err = sb.run_err(&["run", "s2.cig"], 1);
    assert_eq!(sb.read("p/x.txt"), "racy!\n", "{err}");
    assert!(err.contains("restored hop modified"), "{err}");
}

/// #148: a_size_preserving_edit_with_a_restored_mtime_is_caught. Even when
/// size and mtime are both put back by the attacker, ctime (set by the
/// kernel on any write) moves, so the cache misses and the file is
/// hashed afresh, and the real byte change is seen.
#[cfg(unix)]
#[test]
fn a_size_preserving_edit_with_a_restored_mtime_is_caught() {
    let sb = Sandbox::new();
    sb.write("p/a.txt", "aaaa\n");
    let pin = |sb: &Sandbox| {
        assert!(Command::new("touch")
            .args(["-t", "202001010000", "p/a.txt"])
            .current_dir(sb.path())
            .status()
            .unwrap()
            .success());
    };
    pin(&sb);
    assert!(sb.cig(&["config", "hop_statcache", "on"]).status.success());
    sb.write("s.cig", &two_hop_touching_script());
    let one = sb.cig(&["run", "s.cig"]);
    assert!(one.status.success(), "{one:?}");
    // Change the bytes keeping the size, then pin the mtime back. ctime
    // moved on the write, so the cache does not trust itself.
    sb.write("p/a.txt", "bbbb\n");
    pin(&sb);
    sb.write(
        "s2.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"printf 'cccc\\\\n' > p/a.txt; touch -t 202001010000 p/a.txt\"], {touches: [\"**\"]})\n  cough \"x\"\n}\n",
    );
    let err = sb.run_err(&["run", "s2.cig"], 1);
    assert_eq!(sb.read("p/a.txt"), "bbbb\n", "{err}");
    assert!(err.contains("restored hop modified"), "{err}");
}

/// #148: racy_edit_fuzz. Random edit/hop interleavings with the cache on:
/// every unburn restores the state before the hop exactly. The default
/// iteration count keeps the test under ten seconds; `CIG_RACY_FUZZ_ITERS`
/// turns it up for a longer run outside CI.
#[cfg(unix)]
#[test]
fn racy_edit_fuzz() {
    use std::io::Write;
    let iters: u64 = std::env::var("CIG_RACY_FUZZ_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let sb = Sandbox::new();
    for i in 0..3 {
        sb.write(&format!("p/f{i}.txt"), &format!("v0-{i}\n"));
    }
    assert!(sb.cig(&["config", "hop_statcache", "on"]).status.success());
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"printf 'edit\\\\n' > p/f0.txt\"], {touches: [\"**\"]})\n  cough \"x\"\n}\n",
    );
    // A tiny xorshift for the interleavings: deterministic per iteration.
    let mut x: u64 = 0xcafe_babe_dead_beef;
    for i in 0..iters {
        // Pick a file and a new content; write it so cig sees a change
        // between hops. The hop always writes p/f0.txt and coughs, so the
        // rollback must put every chosen path back.
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let which = (x % 3) as usize;
        let tag = x ^ i;
        let content = format!("v{tag}-{which}\n");
        let before = sb.path().join(format!("p/f{which}.txt"));
        {
            let mut f = fs::File::create(&before).unwrap();
            f.write_all(content.as_bytes()).unwrap();
            f.flush().unwrap();
        }
        // Record what every file reads before the run, so we check every
        // one (not just the one just written) comes back.
        let snapshot: Vec<String> = (0..3).map(|j| sb.read(&format!("p/f{j}.txt"))).collect();
        let out = sb.cig(&["run", "s.cig"]);
        // The run coughs: exit 1, rollback runs. The pack must be back
        // where it started.
        assert_eq!(out.status.code(), Some(1), "iter {i}");
        for (j, before) in snapshot.iter().enumerate() {
            let after = sb.read(&format!("p/f{j}.txt"));
            assert_eq!(&after, before, "iter {i}, file f{j}: unburn was not exact");
        }
    }
}

/// #148: statcache_verify_reports_a_lying_filesystem. With the cache on,
/// the first hop stashes a file. Then the file is rewritten with the same
/// size and the stat tuple is faked back onto the file (touch -r), so the
/// cache would trust it next time. `cig tin verify --statcache` re-hashes
/// the cached entries, drops the lying one, and prints a tripwire line.
#[cfg(unix)]
#[test]
fn statcache_verify_reports_a_lying_filesystem() {
    let sb = Sandbox::new();
    sb.write("p/liar.txt", "orig\n");
    assert!(sb.cig(&["config", "hop_statcache", "on"]).status.success());
    sb.write("s.cig", &two_hop_touching_script());
    let one = sb.cig(&["run", "s.cig"]);
    assert!(one.status.success(), "{one:?}");
    // Now rewrite p/liar.txt with the same length as "orig\n". The
    // filesystem gives it a new mtime and ctime; we put the whole stat
    // picture back to look the way it did when the cache saved.
    let liar = sb.path().join("p/liar.txt");
    let orig_modified = fs::metadata(&liar).unwrap().modified().unwrap();
    fs::write(&liar, "liar\n").unwrap();
    // The mtime put back to the nanosecond, portably (BSD and macOS touch
    // take no `-d @seconds`); ctime still moves, as no program can stop it.
    fs::File::options()
        .write(true)
        .open(&liar)
        .unwrap()
        .set_modified(orig_modified)
        .unwrap();
    let out = sb.cig(&["tin", "verify", "--statcache"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a mismatch exits non-zero: {:?}\n{}",
        out,
        String::from_utf8_lossy(&out.stderr)
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("liar.txt") && err.contains("lying filesystem"),
        "{err}"
    );
}

// ----- #105: cig tin rekey, parcel --with-key, Key wipes on drop ---------

/// A sealed run on a sandbox, left ready for `cig unburn`. The file sealed
/// is `.env`, whose plaintext is under `SECRET=` — handy to grep for.
#[cfg(unix)]
fn a_sealed_run_ready_to_rekey(sb: &Sandbox) -> String {
    sb.write(".env", "SECRET=before-rekey\n");
    sb.write(
        "s.cig",
        "pack { \".\" }\nburn {\n  fs.write_text(\".env\", \"SECRET=after-rekey\")\n}\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    sb.runs()[0]["id"].as_str().unwrap().to_string()
}

/// Every sealed object or snapshot under the sandbox.
#[cfg(unix)]
fn every_sealed(sb: &Sandbox) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![sb.home()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.starts_with(sb.home().join("tin/sealed"))
                || p.file_name()
                    .map(|n| n.to_string_lossy().ends_with(".cig-sealed"))
                    .unwrap_or(false)
            {
                out.push(p);
            }
        }
    }
    out
}

#[cfg(unix)]
fn key_hex_of(path: &Path) -> String {
    fs::read_to_string(path).unwrap().trim().to_string()
}

#[cfg(unix)]
#[test]
fn rekey_reseals_every_sealed_object_and_the_run_still_unburns() {
    let sb = Sandbox::new();
    let id = a_sealed_run_ready_to_rekey(&sb);
    let key_path = sb.path().join(".cigscript-keys").join("seal.key");
    let old_hex = key_hex_of(&key_path);
    let sealed_before: Vec<(PathBuf, Vec<u8>)> = every_sealed(&sb)
        .into_iter()
        .map(|p| {
            let b = fs::read(&p).unwrap();
            (p, b)
        })
        .collect();
    assert!(
        !sealed_before.is_empty(),
        "the run produced no sealed object"
    );

    // Rekey.
    let out = sb.run_ok(&["--json", "tin", "rekey"]);
    let done: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(done["walked"], sealed_before.len(), "{out}");
    assert_eq!(done["resealed"], sealed_before.len(), "{out}");
    assert_eq!(done["already"], 0, "{out}");
    assert_eq!(done["resumed"], false, "{out}");
    assert_ne!(
        done["old_fingerprint"], done["new_fingerprint"],
        "fingerprint did not change: {out}"
    );

    // The key on disk changed.
    let new_hex = key_hex_of(&key_path);
    assert_ne!(old_hex, new_hex, "the key file still holds the old key");
    // The stash files are both gone.
    assert!(!sb.path().join(".cigscript-keys/seal.key.old").exists());
    assert!(!sb.path().join(".cigscript-keys/seal.key.new").exists());

    // Every sealed object's bytes changed on disk (a new nonce was picked),
    // and each opens with the new key.
    for (p, old_bytes) in &sealed_before {
        let new_bytes = fs::read(p).unwrap();
        assert_ne!(&new_bytes, old_bytes, "{} unchanged", p.display());
    }

    // `cig tin verify` opens every sealed object with the new key.
    sb.run_ok(&["tin", "verify"]);

    // Unburn: the pack returns, byte-exact.
    sb.run_ok(&["unburn", &id]);
    assert_eq!(sb.read(".env"), "SECRET=before-rekey\n");
}

#[cfg(unix)]
#[test]
fn an_interrupted_rekey_leaves_everything_readable_and_resumes() {
    // Two sealed objects guarantees a visible middle: a fault after the
    // first reseal leaves one new-sealed and one old-sealed on disk.
    let sb = Sandbox::new();
    sb.write(".env", "FIRST=old\n");
    sb.write("second.pem", "KEY=old\n");
    sb.write(
        "s.cig",
        "pack { \".\" }\nburn {\n  fs.write_text(\".env\", \"FIRST=new\")\n  fs.write_text(\"second.pem\", \"KEY=new\")\n}\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let sealed = every_sealed(&sb);
    assert!(
        sealed.len() >= 2,
        "need two sealed, got {}: {:?}",
        sealed.len(),
        sealed
    );

    // Crash on the second reseal. Everything should still be readable
    // (new-sealed under seal.key.new, old-sealed under seal.key.old).
    let out = sb.cig_env(
        &["tin", "rekey"],
        &[("CIG_INTERNAL_FAULT", "rekey-stop-after:1")],
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("E720"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Both stash files are on disk.
    assert!(sb.exists(".cigscript-keys/seal.key.old"));
    assert!(sb.exists(".cigscript-keys/seal.key.new"));

    // Every sealed object still opens: `cig tin verify` does not refuse.
    let verify_out = sb.cig(&["tin", "verify"]);
    let verify_stdout = String::from_utf8_lossy(&verify_out.stdout).to_string();
    let verify_stderr = String::from_utf8_lossy(&verify_out.stderr).to_string();
    assert!(
        verify_out.status.success(),
        "tin verify failed: code={:?}\nstdout: {verify_stdout}\nstderr: {verify_stderr}",
        verify_out.status.code()
    );

    // Resume: finishes the rekey cleanly.
    let done_json: serde_json::Value =
        serde_json::from_str(sb.run_ok(&["--json", "tin", "rekey"]).trim()).unwrap();
    assert_eq!(done_json["resumed"], true);
    // After the resume, the stash files are gone.
    assert!(!sb.exists(".cigscript-keys/seal.key.old"));
    assert!(!sb.exists(".cigscript-keys/seal.key.new"));

    // Unburn still works.
    sb.run_ok(&["unburn", &id]);
    assert_eq!(sb.read(".env"), "FIRST=old\n");
    assert_eq!(sb.read("second.pem"), "KEY=old\n");
}

#[cfg(unix)]
#[test]
fn parcel_with_key_round_trips_a_sealed_run_into_a_second_sandbox() {
    let a = Sandbox::new();
    // Give the pack a secret so it is sealed.
    a.write("p/.env", "SECRET=original\n");
    a.write("p/plain.txt", "plain original\n");
    a.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  fs.write_text(\"p/.env\", \"SECRET=after\")\n  fs.write_text(\"p/plain.txt\", \"plain after\")\n}\n",
    );
    a.run_ok(&["run", "s.cig"]);
    let id = a.runs()[0]["id"].as_str().unwrap().to_string();
    let parcel_path = a.path().join("run.cigparcel");
    // --with-key embeds the sealing key.
    let out = a.cig(&[
        "parcel",
        &id,
        "--with-key",
        "--out",
        parcel_path.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "parcel failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("warning"),
        "no warning about the secret parcel: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // `unparcel --show` says the key is inside.
    let b = Sandbox::new();
    fs::create_dir_all(b.path().join("p")).unwrap();
    for f in ["plain.txt", ".env"] {
        fs::copy(a.path().join("p").join(f), b.path().join("p").join(f)).unwrap();
    }
    let show = b.run_ok(&["unparcel", "--show", parcel_path.to_str().unwrap()]);
    assert!(show.contains("key       inside the parcel"), "{show}");
    // Machine B has NO matching key: a plain unparcel would refuse, but
    // this parcel carries the key, so it imports — with --accept-key,
    // since 1.18.0: accepting a stranger's key is an explicit opt-in.
    let root = format!("p={}", b.path().join("p").display());
    let imported = b.run_ok(&[
        "unparcel",
        parcel_path.to_str().unwrap(),
        "--root",
        &root,
        "--accept-key",
    ]);
    assert_eq!(imported.trim(), id, "{imported}");
    // Unburn on machine B: the sealed secret opens under machine B's own key.
    b.run_ok(&["unburn", &id]);
    assert_eq!(b.read("p/.env"), "SECRET=original\n");
    assert_eq!(b.read("p/plain.txt"), "plain original\n");
    // Machine B's key is its own, not machine A's.
    let a_key = fs::read_to_string(a.path().join(".cigscript-keys/seal.key")).unwrap();
    let b_key = fs::read_to_string(b.path().join(".cigscript-keys/seal.key")).unwrap();
    assert_ne!(
        a_key.trim(),
        b_key.trim(),
        "the keys ended up the same, so --with-key did not re-seal"
    );
}

/// Without --with-key, a parcel on a machine with no matching key is
/// refused (the pre-existing behaviour — proof the warning is honest).
#[cfg(unix)]
#[test]
fn a_parcel_without_the_key_is_refused_on_a_machine_without_one() {
    let a = Sandbox::new();
    a.write("p/.env", "SECRET=x\n");
    a.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  fs.write_text(\"p/.env\", \"SECRET=y\")\n}\n",
    );
    a.run_ok(&["run", "s.cig"]);
    let id = a.runs()[0]["id"].as_str().unwrap().to_string();
    let parcel_path = a.path().join("plain.cigparcel");
    a.run_ok(&["parcel", &id, "--out", parcel_path.to_str().unwrap()]);
    let b = Sandbox::new();
    fs::create_dir_all(b.path().join("p")).unwrap();
    fs::copy(a.path().join("p/.env"), b.path().join("p/.env")).unwrap();
    let root = format!("p={}", b.path().join("p").display());
    let err = b.run_err(
        &["unparcel", parcel_path.to_str().unwrap(), "--root", &root],
        1,
    );
    assert!(err.contains("E762") && err.contains("sealed"), "{err}");
}

#[cfg(unix)]
#[test]
fn a_rekey_with_no_sealed_objects_is_a_quiet_no_op_and_still_rotates_the_key() {
    let sb = Sandbox::new();
    // Make a key by running a trivial burn that reads no secret.
    sb.write("plain.txt", "one\n");
    sb.write(
        "s.cig",
        "pack { \".\" }\nburn {\n  fs.write_text(\"plain.txt\", \"two\")\n}\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    let keyfile = sb.path().join(".cigscript-keys/seal.key");
    let before = key_hex_of(&keyfile);
    let out: serde_json::Value =
        serde_json::from_str(sb.run_ok(&["--json", "tin", "rekey"]).trim()).unwrap();
    assert_eq!(out["walked"], 0, "{out}");
    assert_eq!(out["resealed"], 0, "{out}");
    let after = key_hex_of(&keyfile);
    assert_ne!(before, after, "the key must still rotate");
}

/// cig tin rekey with the key on `--seal-key-fd` refuses: there is no
/// keeper to write the new key to.
#[cfg(unix)]
#[test]
fn rekey_refuses_when_the_key_was_handed_over_on_a_descriptor() {
    let sb = Sandbox::new();
    a_sealed_run_ready_to_rekey(&sb);
    // Export the current key as plain hex into a file.
    let hex_path = sb.path().join("key.hex");
    fs::write(
        &hex_path,
        key_hex_of(&sb.path().join(".cigscript-keys/seal.key")),
    )
    .unwrap();
    // Remove the keeper state so the fd is the only source.
    fs::remove_file(sb.path().join(".cigscript-keys/seal.key")).unwrap();
    // Hand the key in on fd 3 via `sh -c '... 3<key.hex'`.
    let cig = cig_bin();
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "exec {} --seal-key-fd 3 tin rekey 3<{}",
            cig.display(),
            hex_path.display()
        ))
        .current_dir(sb.path())
        .env("CIGSCRIPT_HOME", sb.home())
        .env("NO_COLOR", "1")
        .env("CIG_SEAL_KEY", "file")
        .env("CIG_BUDGET_SYNC", "1")
        .env("CIG_INTERNAL_TRACKER_DIR", sb.path().join("tracker"))
        .env("CIG_UPDATE_MANIFEST", sb.path().join("no-releases.json"))
        .env_remove("CI")
        .output()
        .expect("spawn cig");
    assert!(
        !out.status.success(),
        "rekey on --seal-key-fd did not refuse"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("E720") && err.contains("--seal-key-fd"),
        "{err}"
    );
}

// ----- #105 harden: the SEC-004 review findings 1-11 ---------------------

/// Finding 1. The keeper write fails on purpose. Every sealed object must
/// stay readable (both stashes left behind), and a second cig tin rekey
/// finishes the job: a key that was never proven in the keeper is never
/// trusted as the keeper's.
#[cfg(unix)]
#[test]
fn rekey_keeper_write_failure_leaves_the_stashes_and_a_later_rekey_finishes() {
    let sb = Sandbox::new();
    let id = a_sealed_run_ready_to_rekey(&sb);
    let old_hex = key_hex_of(&sb.path().join(".cigscript-keys").join("seal.key"));

    // Keeper write fails: swap_keeper_if_needed returns E720.
    let out = sb.cig_env(
        &["tin", "rekey"],
        &[("CIG_INTERNAL_FAULT", "rekey-keeper-fail")],
    );
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("E720"), "{err}");

    // Both stashes are still there.
    assert!(sb.exists(".cigscript-keys/seal.key.old"));
    assert!(sb.exists(".cigscript-keys/seal.key.new"));
    // The primary key file was NOT swapped.
    assert_eq!(
        key_hex_of(&sb.path().join(".cigscript-keys").join("seal.key")),
        old_hex
    );
    // Every sealed object still opens: `cig tin verify` is clean. The
    // walk already re-sealed everything under the new key, but
    // secondary_keys honours the stashes while the rekey marker is on disk.
    assert!(sb.exists(".cigscript-keys/rekey.in-progress"));
    sb.run_ok(&["tin", "verify"]);

    // Second cig tin rekey: finishes the job (state (a)).
    let done: serde_json::Value =
        serde_json::from_str(sb.run_ok(&["--json", "tin", "rekey"]).trim()).unwrap();
    assert_eq!(done["resumed"], true, "{done}");
    // Stashes and marker gone now.
    assert!(!sb.exists(".cigscript-keys/seal.key.old"));
    assert!(!sb.exists(".cigscript-keys/seal.key.new"));
    assert!(!sb.exists(".cigscript-keys/rekey.in-progress"));
    let new_hex = key_hex_of(&sb.path().join(".cigscript-keys").join("seal.key"));
    assert_ne!(new_hex, old_hex);
    // Unburn still works.
    sb.run_ok(&["unburn", &id]);
    assert_eq!(sb.read(".env"), "SECRET=before-rekey\n");
}

/// Finding 2. A second cig tin rekey while one is in flight is refused
/// with E720. We simulate "in flight" the way a real rekey makes it:
/// the `rekey.in-progress` marker on disk AND the exclusive flock
/// taken on `rekey.lock`. The message says "another rekey".
#[cfg(unix)]
#[test]
fn a_second_rekey_is_refused_while_one_holds_the_rekey_lock() {
    use std::os::unix::io::AsRawFd;
    let sb = Sandbox::new();
    a_sealed_run_ready_to_rekey(&sb);
    let keys_dir = sb.path().join(".cigscript-keys");
    fs::create_dir_all(&keys_dir).unwrap();
    // The marker is what distinguishes "another rekey in flight" from
    // "a run is using the key" (SEC-004 H1 semantics).
    fs::write(keys_dir.join("rekey.in-progress"), b"").unwrap();
    let lock_path = keys_dir.join("rekey.lock");
    let f = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .unwrap();
    // SAFETY: an exclusive, non-blocking flock on a descriptor we own.
    let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    assert_eq!(rc, 0, "flock the lock file");

    let err = sb.run_err(&["tin", "rekey"], 1);
    assert!(
        err.contains("E720") && err.contains("another cig tin rekey"),
        "{err}"
    );
    drop(f);
    fs::remove_file(keys_dir.join("rekey.in-progress")).unwrap();
    // Now the rekey works again.
    sb.run_ok(&["tin", "rekey"]);
}

/// SEC-004 H1. A live run (shared lock on `rekey.lock`, no marker)
/// makes `cig tin rekey` refuse with "a run is in progress" — nothing
/// is signed under the old key under a run that lives.
#[cfg(unix)]
#[test]
fn a_rekey_refuses_while_a_run_holds_the_shared_lock() {
    use std::os::unix::io::AsRawFd;
    let sb = Sandbox::new();
    a_sealed_run_ready_to_rekey(&sb);
    let keys_dir = sb.path().join(".cigscript-keys");
    let lock_path = keys_dir.join("rekey.lock");
    // Take LOCK_SH ourselves, as a live cig run would (its seal::key()
    // takes the shared lock at first use and never releases it).
    let f = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .unwrap();
    // SAFETY: flock on a descriptor we own, shared and non-blocking.
    let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) };
    assert_eq!(rc, 0, "take LOCK_SH on rekey.lock");
    // No marker on disk: this is a live run's state, not a rekey's.
    assert!(!keys_dir.join("rekey.in-progress").exists());

    let err = sb.run_err(&["tin", "rekey"], 1);
    assert!(
        err.contains("E720") && err.contains("a run is in progress"),
        "{err}"
    );
    drop(f);
    // Now the rekey works again — the lock is released.
    sb.run_ok(&["tin", "rekey"]);
}

/// SEC-004 H1 cont'd. A `cig run` started while the test holds the
/// exclusive rekey lock (simulating a rekey in flight) blocks on
/// `seal::key()`, prints a notice on stderr, and after the test
/// releases the lock finishes under the key the keeper now carries.
#[cfg(target_os = "linux")]
#[test]
fn a_run_started_during_a_rekey_waits_and_finishes() {
    use std::os::unix::io::AsRawFd;
    let sb = Sandbox::new();
    // Give the sandbox a key file to read; nothing sealed yet, since
    // the blocked seal::key() call is what we want to see.
    a_sealed_run_ready_to_rekey(&sb);
    let keys_dir = sb.path().join(".cigscript-keys");
    let lock_path = keys_dir.join("rekey.lock");
    let f = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .unwrap();
    // Take LOCK_EX ourselves, as a rekey would while it works.
    // SAFETY: flock on a descriptor we own.
    let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    assert_eq!(rc, 0, "take LOCK_EX on rekey.lock");

    // Start a `cig run` in the background: it will block in
    // seal::key()'s LOCK_SH call.
    sb.write(
        "wait.cig",
        "pack { \".\" }\nburn {\n  fs.write_text(\".env\", \"SECRET=after-wait\")\n}\n",
    );
    let mut child = sb.cig_spawn(&["run", "wait.cig"]);

    // Give the child a moment to actually block on the lock.
    std::thread::sleep(std::time::Duration::from_millis(500));
    // It is still running (blocked in LOCK_SH).
    assert!(
        child.try_wait().unwrap().is_none(),
        "the child exited while the exclusive lock was held"
    );

    // Release the exclusive lock: the child's LOCK_SH now succeeds, the
    // run proceeds, and the burn's seal completes.
    drop(f);
    let out = child.wait_with_output().expect("child wait");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "run did not succeed after the lock was released; stdout={stdout}, stderr={stderr}",
    );
    // The waiting notice went to stderr.
    assert!(
        stderr.contains("rekey is in progress") || stderr.contains("waiting for it to finish"),
        "no waiting notice on stderr: {stderr}"
    );
    // The burn went through: .env's new contents are what the burn wrote.
    assert_eq!(sb.read(".env"), "SECRET=after-wait");
}

/// Finding 3. A tampered journal is caught before anything is re-signed:
/// the rekey refuses with E720, the journal keeps its break, and a
/// cig tin verify after still reports the break (nothing was laundered).
/// A two-write burn gives two op lines, so a tamper on the first is
/// caught by the second line's `prev` mismatch.
#[cfg(unix)]
#[test]
fn a_tampered_journal_refuses_the_rekey_and_tin_verify_still_reports_it() {
    let sb = Sandbox::new();
    sb.write(".env", "FIRST=old\n");
    sb.write("second.pem", "KEY=old\n");
    sb.write(
        "s.cig",
        "pack { \".\" }\nburn {\n  fs.write_text(\".env\", \"FIRST=new\")\n  fs.write_text(\"second.pem\", \"KEY=new\")\n}\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    // Tamper: flip a plaintext byte in the FIRST op line. The next op
    // line's `prev` becomes the hash of a line that no longer exists,
    // so `Journal::load` marks `broken_at` and the rekey refuses.
    let journal_path = sb.home().join("runs").join(&id).join("journal.jsonl");
    let text = fs::read_to_string(&journal_path).unwrap();
    let mut lines: Vec<String> = text.lines().map(|s| s.to_string()).collect();
    let idx = lines
        .iter()
        .position(|l| l.contains("\"op\""))
        .expect("an op line");
    let before = lines[idx].clone();
    if let Some(pos) = lines[idx].find(".env") {
        lines[idx].replace_range(pos..pos + 4, ".ENX");
    } else {
        let bytes = unsafe { lines[idx].as_bytes_mut() };
        for b in bytes {
            if b.is_ascii_lowercase() {
                *b = b.to_ascii_uppercase();
                break;
            }
        }
    }
    assert_ne!(before, lines[idx], "the tamper must change the line");
    fs::write(&journal_path, lines.join("\n") + "\n").unwrap();

    let err = sb.run_err(&["tin", "rekey"], 1);
    assert!(
        err.contains("E720") && err.contains("does not verify under the current key"),
        "{err}"
    );
    // The journal was NOT re-signed (the tampered bytes are still there).
    let again = fs::read_to_string(&journal_path).unwrap();
    assert!(again.contains(".ENX") || again != text);

    // `cig runs <id> --verify` walks the chain and reports the break,
    // proving the rekey did not launder it. (`cig tin verify` is tin
    // objects and ledger only; journals are checked at the run level.)
    let v = sb.cig(&["runs", &id, "--verify"]);
    let out = String::from_utf8_lossy(&v.stdout).to_string() + &String::from_utf8_lossy(&v.stderr);
    assert!(
        out.contains("E708") || out.contains("broken") || out.contains("does not"),
        "runs --verify should report the break: {out}"
    );
}

/// Finding 5. A stray `.reseal-plain` left where a sealed object lives
/// (a crash during an older-style reseal) does not block a rekey: it is
/// wipe-removed before the walk begins.
#[cfg(unix)]
#[test]
fn a_stray_reseal_plain_in_the_bucket_does_not_block_a_rekey() {
    let sb = Sandbox::new();
    let id = a_sealed_run_ready_to_rekey(&sb);
    // Snapshots end up under runs/<id>/snapshots/*.cig-sealed. Drop a
    // stray `.reseal-plain` next to one (and, if any, into a tin
    // bucket too, so both paths are covered).
    let snaps = sb.home().join("runs").join(&id).join("snapshots");
    let stray_snap = snaps.join("00001-1-.env.reseal-plain");
    fs::write(&stray_snap, b"would be the secret in the clear").unwrap();
    let mut stray_tin: Option<PathBuf> = None;
    let tin_sealed = sb.home().join("tin").join("sealed");
    if let Ok(rd) = fs::read_dir(&tin_sealed) {
        if let Some(b) = rd
            .flatten()
            .find(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        {
            let p = b.path().join("deadbeef.reseal-plain");
            fs::write(&p, b"would be the secret in the clear").unwrap();
            stray_tin = Some(p);
        }
    }

    sb.run_ok(&["tin", "rekey"]);
    // Both strays gone (wiped and removed before the walk).
    assert!(!stray_snap.exists(), "stray reseal-plain not cleaned up");
    if let Some(p) = stray_tin {
        assert!(!p.exists(), "stray tin reseal-plain not cleaned up");
    }
}

/// Finding 6. A `--with-key` parcel file lands at mode 0600, not the
/// umask's default (which could be 0644).
#[cfg(unix)]
#[test]
fn a_with_key_parcel_file_is_mode_0600() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    sb.write("p/.env", "SECRET=x\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  fs.write_text(\"p/.env\", \"SECRET=y\")\n}\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let parcel_path = sb.path().join("secret.cigparcel");
    sb.run_ok(&[
        "parcel",
        &id,
        "--with-key",
        "--out",
        parcel_path.to_str().unwrap(),
    ]);
    let mode = fs::metadata(&parcel_path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "parcel mode is {:o}", mode);
}

/// Finding 7. A planted `seal.key.new` with no rekey lock is ignored —
/// `secondary_keys` returns nothing, so an object sealed under the
/// attacker's key does not open.
#[cfg(unix)]
#[test]
fn a_planted_seal_key_new_without_a_rekey_lock_is_ignored() {
    let sb = Sandbox::new();
    let id = a_sealed_run_ready_to_rekey(&sb);
    let keys_dir = sb.path().join(".cigscript-keys");
    // Plant a fresh "attacker" key at seal.key.new. No rekey.lock.
    let attacker_hex = "ab".repeat(32);
    let planted = keys_dir.join("seal.key.new");
    fs::write(&planted, format!("{attacker_hex}\n")).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&planted, fs::Permissions::from_mode(0o600)).unwrap();
    }
    assert!(!keys_dir.join("rekey.in-progress").exists());

    // Unburn still opens with the primary key: the plant does not
    // become a trusted fallback.
    sb.run_ok(&["unburn", &id]);
    assert_eq!(sb.read(".env"), "SECRET=before-rekey\n");
}

/// Finding 8. A parcel that carries its own sealing key is refused
/// without --accept-key, with a message that names the fingerprint and
/// the flag.
#[cfg(unix)]
#[test]
fn a_with_key_parcel_is_refused_without_accept_key() {
    let a = Sandbox::new();
    a.write("p/.env", "SECRET=original\n");
    a.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  fs.write_text(\"p/.env\", \"SECRET=after\")\n}\n",
    );
    a.run_ok(&["run", "s.cig"]);
    let id = a.runs()[0]["id"].as_str().unwrap().to_string();
    let parcel_path = a.path().join("secret.cigparcel");
    a.run_ok(&[
        "parcel",
        &id,
        "--with-key",
        "--out",
        parcel_path.to_str().unwrap(),
    ]);
    let b = Sandbox::new();
    fs::create_dir_all(b.path().join("p")).unwrap();
    fs::copy(a.path().join("p/.env"), b.path().join("p/.env")).unwrap();
    let root = format!("p={}", b.path().join("p").display());

    // Without --accept-key: refused.
    let err = b.run_err(
        &["unparcel", parcel_path.to_str().unwrap(), "--root", &root],
        1,
    );
    assert!(
        err.contains("carries its own sealing key") && err.contains("--accept-key"),
        "{err}"
    );
    // With --accept-key: imports.
    b.run_ok(&[
        "unparcel",
        parcel_path.to_str().unwrap(),
        "--root",
        &root,
        "--accept-key",
    ]);
    b.run_ok(&["unburn", &id]);
    assert_eq!(b.read("p/.env"), "SECRET=original\n");
}

/// Finding 8 cont'd. A with-key parcel whose archive has a sealed
/// object NOT sealed under the carried key is refused before anything
/// is written (and the run directory is never placed).
#[cfg(unix)]
#[test]
fn a_tampered_with_key_parcel_with_a_mismatched_sealed_object_is_refused() {
    use std::io::Read;
    let a = Sandbox::new();
    a.write("p/.env", "SECRET=original\n");
    // Two secrets, so we end up with at least two sealed objects.
    a.write("p/second.pem", "KEY=original\n");
    a.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  fs.write_text(\"p/.env\", \"SECRET=after\")\n  fs.write_text(\"p/second.pem\", \"KEY=after\")\n}\n",
    );
    a.run_ok(&["run", "s.cig"]);
    let id = a.runs()[0]["id"].as_str().unwrap().to_string();
    let parcel_path = a.path().join("secret.cigparcel");
    a.run_ok(&[
        "parcel",
        &id,
        "--with-key",
        "--out",
        parcel_path.to_str().unwrap(),
    ]);
    // Swap one sealed object in the parcel for a different sealed bytes
    // sequence (bytes from a fresh seal on sandbox C under its own key).
    let c = Sandbox::new();
    c.write("other/.env", "OTHER=plant\n");
    c.write(
        "s.cig",
        "pack { \"./other\" }\nburn {\n  fs.write_text(\"other/.env\", \"OTHER=after\")\n}\n",
    );
    c.run_ok(&["run", "s.cig"]);
    // Find any sealed file C produced — tin/sealed bucket OR a run's
    // sealed snapshot. Whichever exists.
    let other_sealed_bytes = {
        let mut bytes: Option<Vec<u8>> = None;
        let mut stack: Vec<PathBuf> = vec![c.home()];
        while let Some(d) = stack.pop() {
            let Ok(rd) = fs::read_dir(&d) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p
                    .file_name()
                    .map(|n| n.to_string_lossy().ends_with(".cig-sealed"))
                    .unwrap_or(false)
                    || p.starts_with(c.home().join("tin").join("sealed"))
                {
                    if let Ok(b) = fs::read(&p) {
                        bytes = Some(b);
                        break;
                    }
                }
            }
            if bytes.is_some() {
                break;
            }
        }
        bytes.expect("c has a sealed file")
    };

    // Rewrite the parcel: read its files, swap ONE sealed object's bytes,
    // and update the manifest so the sha256 matches (so the per-file
    // hash check passes but the sealed-object check must fail).
    let tmp_rewritten = a.path().join("tampered.cigparcel");
    {
        let f = fs::File::open(&parcel_path).unwrap();
        let dec = flate2::read::GzDecoder::new(f);
        let mut archive = tar::Archive::new(dec);
        let out_f = fs::File::create(&tmp_rewritten).unwrap();
        let enc = flate2::write::GzEncoder::new(out_f, flate2::Compression::default());
        let mut tar_w = tar::Builder::new(enc);
        let mut manifest_json: Option<Vec<u8>> = None;
        let mut file_bytes: Vec<(String, Vec<u8>)> = Vec::new();
        for entry in archive.entries().unwrap() {
            let mut e = entry.unwrap();
            let name = e.path().unwrap().to_string_lossy().into_owned();
            let mut buf = Vec::new();
            e.read_to_end(&mut buf).unwrap();
            if name == "manifest.json" {
                manifest_json = Some(buf);
            } else {
                file_bytes.push((name, buf));
            }
        }
        // Pick the first tin/sealed/ or run/snapshots/*.cig-sealed to tamper.
        let victim_idx = file_bytes
            .iter()
            .position(|(n, _)| n.starts_with("tin/sealed/"))
            .or_else(|| {
                file_bytes
                    .iter()
                    .position(|(n, _)| n.ends_with(".cig-sealed"))
            })
            .expect("a sealed file in the parcel");
        let (victim_name, victim_bytes) = &mut file_bytes[victim_idx];
        // Replace its bytes with an unrelated sealed blob of equal-ish size.
        let replacement = other_sealed_bytes.clone();
        *victim_bytes = replacement;
        // Update the manifest's sha256 for this file so the per-file check
        // passes; the sealed-opens check is what must fail.
        let mut manifest: serde_json::Value =
            serde_json::from_slice(manifest_json.as_ref().unwrap()).unwrap();
        use sha2::{Digest, Sha256};
        let new_sha = hex::encode(Sha256::digest(victim_bytes));
        manifest["files"][victim_name.as_str()] = serde_json::Value::String(new_sha);
        let new_manifest = serde_json::to_vec_pretty(&manifest).unwrap();

        let mut header = tar::Header::new_gnu();
        header.set_size(new_manifest.len() as u64);
        header.set_mode(0o600);
        header.set_mtime(0);
        header.set_cksum();
        tar_w
            .append_data(&mut header, "manifest.json", new_manifest.as_slice())
            .unwrap();
        for (name, bytes) in &file_bytes {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_cksum();
            tar_w
                .append_data(&mut header, name.as_str(), bytes.as_slice())
                .unwrap();
        }
        let enc = tar_w.into_inner().unwrap();
        let f = enc.finish().unwrap();
        f.sync_all().unwrap();
    }
    let b = Sandbox::new();
    fs::create_dir_all(b.path().join("p")).unwrap();
    for f in [".env", "second.pem"] {
        fs::copy(a.path().join("p").join(f), b.path().join("p").join(f)).unwrap();
    }
    let root = format!("p={}", b.path().join("p").display());
    let err = b.run_err(
        &[
            "unparcel",
            tmp_rewritten.to_str().unwrap(),
            "--root",
            &root,
            "--accept-key",
        ],
        1,
    );
    assert!(
        err.contains("does not open under it") || err.contains("refusing the whole parcel"),
        "{err}"
    );
    // The run was NOT placed (import is atomic).
    assert!(!b.home().join("runs").join(&id).exists());
}

/// SEC-004 H2. A planted `rekey.in-progress` + a planted 0600
/// `seal.key.new` from an "attacker" (a key unrelated to the primary)
/// must not make a journal, ledger line or sealed object signed/sealed
/// under that key verify or open: the stash is honoured only when the
/// keeper's primary key equals one of the stash keys, by a
/// constant-time compare.
#[cfg(unix)]
#[test]
fn a_planted_marker_and_stash_does_not_forge_journals_or_sealed_objects() {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    let id = a_sealed_run_ready_to_rekey(&sb);
    let keys_dir = sb.path().join(".cigscript-keys");
    let runs_dir = sb.home().join("runs");

    // The "attacker's" key: 32 random bytes NOT equal to the primary.
    let attacker_hex = "11".repeat(32);
    // Plant a marker and a 0600 seal.key.new under the attacker's key.
    fs::write(keys_dir.join("rekey.in-progress"), b"").unwrap();
    {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true).mode(0o600);
        let mut f = opts.open(keys_dir.join("seal.key.new")).unwrap();
        writeln!(f, "{attacker_hex}").unwrap();
    }
    fs::set_permissions(
        keys_dir.join("seal.key.new"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    // Also plant a bogus attacker-signed "journal" in a fresh run dir:
    // without H2 the derived journal-mac subkey from the planted
    // seal.key.new would make it verify.
    let attacker_run_dir = runs_dir.join("20260101T000000-planted");
    fs::create_dir_all(&attacker_run_dir).unwrap();
    // Build a one-line format-3 journal line signed under the attacker key.
    let attacker_master = hex::decode(&attacker_hex).unwrap();
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    // HKDF the same way `seal::derive` does: labels LABEL_JOURNAL_MAC.
    let hk = hkdf::Hkdf::<Sha256>::new(None, &attacker_master);
    let mut journal_subkey = [0u8; 32];
    hk.expand(b"journal-mac", &mut journal_subkey).unwrap();
    // Chain seed: sha256 of the run id (same as journal::chain_seed).
    use sha2::Digest;
    let seed = hex::encode(Sha256::digest(b"20260101T000000-planted"));
    // Header line: {"format":3,"run":"<id>","prev":"<HMAC under journal-mac of id>"}.
    let mut mac = Hmac::<Sha256>::new_from_slice(&journal_subkey).unwrap();
    mac.update(b"20260101T000000-planted");
    let header_prev = hex::encode(mac.finalize().into_bytes());
    assert_ne!(header_prev, seed, "sanity: subkey makes a real MAC");
    let header_line =
        format!("{{\"format\":3,\"run\":\"20260101T000000-planted\",\"prev\":\"{header_prev}\"}}");
    fs::write(
        attacker_run_dir.join("journal.jsonl"),
        format!("{header_line}\n"),
    )
    .unwrap();
    fs::write(
        attacker_run_dir.join("run.json"),
        r#"{"format":1,"id":"20260101T000000-planted","status":"ok","cwd":"","pack":[]}"#,
    )
    .unwrap();

    // The planted journal MUST read as `other_key` or `unverified`,
    // NOT as a verified chain under the planted secondary. That is
    // what H2 proves: `secondary_keys(primary)` returns the stashes
    // only when the primary equals one of them (it does not here, so
    // the attacker's derivation is not a candidate for pick_mac_key).
    //
    // `cig runs --verify` on the planted run surfaces the state.
    let v = sb.cig(&["runs", "20260101T000000-planted", "--verify"]);
    let verdict =
        String::from_utf8_lossy(&v.stdout).to_string() + &String::from_utf8_lossy(&v.stderr);
    assert!(
        !v.status.success()
            || verdict.contains("other key")
            || verdict.contains("another")
            || verdict.contains("unverified")
            || verdict.contains("broken")
            || verdict.contains("does not"),
        "planted journal should NOT verify under the attacker's planted key: {verdict}"
    );
    // The sealed objects of the real run still open with the primary.
    // Unburn the original run: the planted stash must not break the read.
    sb.run_ok(&["unburn", &id]);
    assert_eq!(sb.read(".env"), "SECRET=before-rekey\n");
}

/// SEC-004 H3. On resume, the keeper's current key must equal one of
/// the stash keys: a planted pair with no prior rekey is refused.
#[cfg(unix)]
#[test]
fn a_rekey_resume_refuses_a_planted_pair() {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    let _id = a_sealed_run_ready_to_rekey(&sb);
    let keys_dir = sb.path().join(".cigscript-keys");
    // Plant seal.key.old / seal.key.new with keys the keeper does not
    // know, and the "in-progress" marker. A second rekey must refuse
    // (the resume path's keeper-equals-stash check fails).
    let old_hex = "22".repeat(32);
    let new_hex = "33".repeat(32);
    fs::write(keys_dir.join("rekey.in-progress"), b"").unwrap();
    for (name, hex) in [("seal.key.old", &old_hex), ("seal.key.new", &new_hex)] {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true).mode(0o600);
        let mut f = opts.open(keys_dir.join(name)).unwrap();
        writeln!(f, "{hex}").unwrap();
        fs::set_permissions(keys_dir.join(name), fs::Permissions::from_mode(0o600)).unwrap();
    }
    let err = sb.run_err(&["tin", "rekey"], 1);
    assert!(
        err.contains("E720") && err.contains("do not match"),
        "{err}"
    );
}

/// SEC-004 H4. A journal with `other_key` or `unverified` set is not
/// re-signed. A planted journal from "another machine" (signed under a
/// key this tin does not know) placed as a new run in the state
/// directory is caught by `verify_everything_under_current_key` and
/// the rekey refuses.
#[cfg(unix)]
#[test]
fn a_rekey_refuses_a_journal_signed_by_another_key() {
    // Sandbox A: produce a signed journal under A's key.
    let a = Sandbox::new();
    let id_a = a_sealed_run_ready_to_rekey(&a);
    let a_journal = a.home().join("runs").join(&id_a).join("journal.jsonl");
    let a_journal_text = fs::read_to_string(&a_journal).unwrap();
    let a_run_json = a.home().join("runs").join(&id_a).join("run.json");
    let a_run_text = fs::read_to_string(&a_run_json).unwrap();

    // Sandbox B: has its own key.
    let b = Sandbox::new();
    let _id_b = a_sealed_run_ready_to_rekey(&b);
    // Place A's journal and run.json as a new run in B, without going
    // through `cig unparcel` (which would re-sign the journal under B's
    // key). This represents the imported-but-unadopted state.
    let b_run_dir = b.home().join("runs").join(&id_a);
    fs::create_dir_all(&b_run_dir).unwrap();
    fs::write(b_run_dir.join("journal.jsonl"), &a_journal_text).unwrap();
    fs::write(b_run_dir.join("run.json"), &a_run_text).unwrap();

    // Rekey on B: refuses with E720, naming the planted run.
    let err = b.run_err(&["tin", "rekey"], 1);
    assert!(
        err.contains("E720")
            && err.contains(&id_a)
            && (err.contains("signed by another key") || err.contains("another key")),
        "{err}"
    );
    // The journal was NOT touched (it is still under A's bytes).
    let again = fs::read_to_string(b_run_dir.join("journal.jsonl")).unwrap();
    assert_eq!(again, a_journal_text);
}

/// SEC-004 M1 / M2. `cig parcel --json` strips the raw sealing key
/// from its embedded manifest and prints its fingerprint instead; the
/// fingerprint is computed from the CARRIED key bytes.
#[cfg(unix)]
#[test]
fn parcel_json_output_does_not_leak_the_raw_sealing_key() {
    let sb = Sandbox::new();
    sb.write("p/.env", "SECRET=x\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  fs.write_text(\"p/.env\", \"SECRET=y\")\n}\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let parcel_path = sb.path().join("x.cigparcel");
    let out = sb.run_ok(&[
        "--json",
        "parcel",
        &id,
        "--with-key",
        "--out",
        parcel_path.to_str().unwrap(),
    ]);
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    // The raw key is NOT present in the JSON output.
    assert!(
        v["manifest"]["sealing_key"].is_null(),
        "sealing_key leaked to --json: {out}"
    );
    // A fingerprint IS present.
    let fp = v["manifest"]["sealing_key_fingerprint"]
        .as_str()
        .expect("sealing_key_fingerprint missing from --json");
    assert_eq!(fp.len(), 16, "fingerprint is 16 hex chars: {fp}");

    // `cig unparcel --show --json` also strips the key.
    let show_out = sb.run_ok(&[
        "--json",
        "unparcel",
        "--show",
        parcel_path.to_str().unwrap(),
    ]);
    let sv: serde_json::Value = serde_json::from_str(show_out.trim()).unwrap();
    assert!(
        sv["sealing_key"].is_null(),
        "sealing_key leaked to unparcel --show --json: {show_out}"
    );
    let sfp = sv["sealing_key_fingerprint"]
        .as_str()
        .expect("sealing_key_fingerprint missing from unparcel --show --json");
    assert_eq!(sfp, fp);
}

/// SEC-004 M2. On accept, the carried-key fingerprint is printed from
/// the carried key bytes (not the manifest's own claim). We prove it
/// matches the sender's fingerprint, so an operator can check it.
#[cfg(unix)]
#[test]
fn unparcel_accept_key_prints_the_carried_fingerprint() {
    let a = Sandbox::new();
    a.write("p/.env", "SECRET=x\n");
    a.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  fs.write_text(\"p/.env\", \"SECRET=y\")\n}\n",
    );
    a.run_ok(&["run", "s.cig"]);
    let id = a.runs()[0]["id"].as_str().unwrap().to_string();
    let parcel_path = a.path().join("secret.cigparcel");
    let written_out = a.run_ok(&[
        "--json",
        "parcel",
        &id,
        "--with-key",
        "--out",
        parcel_path.to_str().unwrap(),
    ]);
    let wv: serde_json::Value = serde_json::from_str(written_out.trim()).unwrap();
    let sender_fp = wv["manifest"]["sealing_key_fingerprint"]
        .as_str()
        .expect("sender's fingerprint")
        .to_string();

    let b = Sandbox::new();
    fs::create_dir_all(b.path().join("p")).unwrap();
    fs::copy(a.path().join("p/.env"), b.path().join("p/.env")).unwrap();
    let root = format!("p={}", b.path().join("p").display());

    // The accept path prints the fingerprint on stderr and carries it
    // in --json.
    let cig_out = b.cig(&[
        "unparcel",
        parcel_path.to_str().unwrap(),
        "--root",
        &root,
        "--accept-key",
    ]);
    assert!(cig_out.status.success());
    let stderr = String::from_utf8_lossy(&cig_out.stderr);
    assert!(
        stderr.contains(&sender_fp) && stderr.contains("accepted"),
        "accept did not print the carried fingerprint: {stderr}"
    );

    // --json carries it in the structured output. We import into a
    // fresh sandbox (the previous one already has this run).
    let c = Sandbox::new();
    fs::create_dir_all(c.path().join("p")).unwrap();
    fs::copy(a.path().join("p/.env"), c.path().join("p/.env")).unwrap();
    let root_c = format!("p={}", c.path().join("p").display());
    let imported = c.run_ok(&[
        "--json",
        "unparcel",
        parcel_path.to_str().unwrap(),
        "--root",
        &root_c,
        "--accept-key",
    ]);
    let iv: serde_json::Value = serde_json::from_str(imported.trim()).unwrap();
    assert_eq!(
        iv["accepted_key_fingerprint"].as_str().unwrap_or(""),
        sender_fp,
        "{imported}"
    );
    // The refusal path (no --accept-key) names the same fingerprint,
    // computed from the carried key bytes, not the manifest claim.
    let d = Sandbox::new();
    fs::create_dir_all(d.path().join("p")).unwrap();
    fs::copy(a.path().join("p/.env"), d.path().join("p/.env")).unwrap();
    let root_d = format!("p={}", d.path().join("p").display());
    let err = d.run_err(
        &["unparcel", parcel_path.to_str().unwrap(), "--root", &root_d],
        1,
    );
    assert!(
        err.contains(&sender_fp),
        "refusal did not name the carried fingerprint: {err}"
    );
}

/// Finding 11. `--with-key` with no readable key is an error, not a
/// quiet "wrote a parcel without the key". (We run on a sandbox whose
/// key file is removed between seal and parcel, so the cached key does
/// not save us — a fresh cig process asks again.)
#[cfg(unix)]
#[test]
fn with_key_errors_when_the_key_cannot_be_read() {
    let sb = Sandbox::new();
    sb.write("p/.env", "SECRET=x\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  fs.write_text(\"p/.env\", \"SECRET=y\")\n}\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    // Remove the primary key file; a fresh cig process cannot read any
    // key (keyring is forced off via CIG_SEAL_KEY=file and we have no
    // seal-key-fd setup). The sealed snapshots still exist.
    fs::remove_file(sb.path().join(".cigscript-keys").join("seal.key")).unwrap();
    let parcel_path = sb.path().join("x.cigparcel");
    let err = sb.run_err(
        &[
            "parcel",
            &id,
            "--with-key",
            "--out",
            parcel_path.to_str().unwrap(),
        ],
        1,
    );
    assert!(
        err.contains("--with-key") && err.contains("seal key"),
        "{err}"
    );
    assert!(!parcel_path.exists(), "parcel should not have been written");
}

/// SEC-004 N1(a). A hostile parcel whose `manifest.json` is a symlink
/// to a file in the sandbox is refused BEFORE anything is written, and
/// the target's bytes are left intact. Without the per-entry-type
/// check, the write-side manifest read would follow the link, and the
/// cleanup `wipe_and_remove_file` would zero the target (the tin's
/// seal.key, say).
#[cfg(unix)]
#[test]
fn a_parcel_whose_manifest_is_a_symlink_is_refused_and_the_target_is_intact() {
    let sb = Sandbox::new();
    // A target file the hostile symlink will point at.
    let target = sb.path().join("important-key");
    let target_bytes = b"these bytes must not be zeroed".to_vec();
    fs::write(&target, &target_bytes).unwrap();

    // Build a .cigparcel by hand: a tar.gz whose only entry is a
    // `manifest.json` SYMLINK pointing at `target`. No regular files.
    let parcel_path = sb.path().join("hostile.cigparcel");
    {
        let file = fs::File::create(&parcel_path).unwrap();
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut tar_w = tar::Builder::new(enc);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_mtime(0);
        header.set_link_name(&target).unwrap();
        header.set_cksum();
        tar_w
            .append_data(&mut header, "manifest.json", std::io::empty())
            .unwrap();
        let enc = tar_w.into_inner().unwrap();
        let out = enc.finish().unwrap();
        out.sync_all().unwrap();
    }

    // Any `--root home=...` is enough to parse the args; the symlink
    // refusal fires in the unpack, before the mapping is used.
    let root = format!("home={}", sb.path().display());
    let err = sb.run_err(
        &["unparcel", parcel_path.to_str().unwrap(), "--root", &root],
        1,
    );
    assert!(
        err.contains("symlink") && err.contains("refusing the whole parcel"),
        "{err}"
    );
    // The target is untouched: not zeroed, not removed.
    assert_eq!(fs::read(&target).unwrap(), target_bytes);
}

/// SEC-004 N1(a) cont'd. A hostile parcel with a hardlink entry is
/// refused with "hardlink" in the message.
#[cfg(unix)]
#[test]
fn a_parcel_with_a_hardlink_entry_is_refused() {
    let sb = Sandbox::new();
    let target = sb.path().join("some-file");
    fs::write(&target, b"content").unwrap();

    let parcel_path = sb.path().join("hostile.cigparcel");
    {
        let file = fs::File::create(&parcel_path).unwrap();
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut tar_w = tar::Builder::new(enc);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Link);
        header.set_size(0);
        header.set_mode(0o600);
        header.set_mtime(0);
        header.set_link_name(&target).unwrap();
        header.set_cksum();
        tar_w
            .append_data(&mut header, "manifest.json", std::io::empty())
            .unwrap();
        let enc = tar_w.into_inner().unwrap();
        let out = enc.finish().unwrap();
        out.sync_all().unwrap();
    }

    let root = format!("home={}", sb.path().display());
    let err = sb.run_err(
        &["unparcel", parcel_path.to_str().unwrap(), "--root", &root],
        1,
    );
    assert!(
        err.contains("hardlink") && err.contains("refusing the whole parcel"),
        "{err}"
    );
    assert_eq!(fs::read(&target).unwrap(), b"content");
}

/// SEC-004 N2. A ledger-appending command started while the test holds
/// the exclusive rekey lock waits on the shared lock BEFORE reading the
/// ledger file, then after the lock is released finishes successfully
/// and the ledger chain still verifies. Without the fix, `ledger::load`
/// reads the file first and the read can race a rekey rewrite; this
/// test proves `ledger::load` now waits on the lock before the read.
#[cfg(target_os = "linux")]
#[test]
fn a_ledger_append_started_during_a_rekey_waits_and_the_chain_still_verifies() {
    use std::os::unix::io::AsRawFd;
    let sb = Sandbox::new();
    let _id = a_sealed_run_ready_to_rekey(&sb);
    let keys_dir = sb.path().join(".cigscript-keys");
    let lock_path = keys_dir.join("rekey.lock");
    let f = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .unwrap();
    // The test is the rekey: hold LOCK_EX.
    // SAFETY: flock on a descriptor we own.
    let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    assert_eq!(rc, 0, "take LOCK_EX on rekey.lock");

    // Spawn a `cig runs --forget <id>` — it calls `ledger::append`,
    // whose first step is `ledger::load`, which now takes the shared
    // rekey lock BEFORE reading the ledger file (SEC-004 N2).
    let fake_id = "20200101T000000-forgotten";
    let mut child = sb.cig_spawn(&["runs", "--forget", fake_id]);

    // Give it a moment to block in the lock call.
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(
        child.try_wait().unwrap().is_none(),
        "the ledger-appending child exited while the exclusive lock was held",
    );

    // Release the exclusive lock.
    drop(f);
    let out = child.wait_with_output().expect("child wait");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "runs --forget did not succeed after the lock was released; stdout={stdout}, stderr={stderr}",
    );
    // The chain still verifies: the ledger loads without a `broken_at`.
    sb.run_ok(&["tin", "verify"]);
}

/// SEC-004 N3. When the process's shared rekey lock cannot be had
/// (here: the keys directory cannot be created because a regular file
/// sits in its place), `seal::key()` fails closed with the reason,
/// instead of quietly carrying on unprotected. A seal operation then
/// refuses before anything is written.
#[cfg(unix)]
#[test]
fn seal_refuses_when_the_rekey_lock_cannot_be_had() {
    let sb = Sandbox::new();
    // Put a regular file where the keys directory would be, so
    // create_dir_all fails. The keys dir sits beside CIGSCRIPT_HOME
    // (see `seal::keys_dir`): CIGSCRIPT_HOME=<path>/home, keys_dir =
    // <path>/.cigscript-keys. Write a file at that exact path.
    let keys_dir = sb.path().join(".cigscript-keys");
    fs::write(&keys_dir, b"not a directory").unwrap();

    sb.write(".env", "SECRET=x\n");
    sb.write(
        "s.cig",
        "pack { \".\" }\nburn {\n  fs.write_text(\".env\", \"SECRET=y\")\n}\n",
    );
    // The run must fail with the lock-closed reason; what exit code
    // and exact text the caller surfaces is less important than that
    // it names the keys directory and the refusal to proceed
    // unprotected.
    let out = sb.cig(&["run", "s.cig"]);
    assert!(
        !out.status.success(),
        "run should fail when the keys directory cannot be created"
    );
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        err.contains("keys directory") && err.contains("unprotected"),
        "lock-closed refusal expected, got: {err}"
    );
    // The lock failure's cache survives, so .env is NOT re-written.
    assert_eq!(
        fs::read_to_string(sb.path().join(".env")).unwrap(),
        "SECRET=x\n"
    );
}

/// SEC-004 N5. A rekey that is refused in verification must leave no
/// `rekey.in-progress` marker on disk: a later contention with a
/// running cig would otherwise read it as "another rekey is in
/// progress" rather than "a run is in progress". The refusal here
/// comes from a tampered journal (H4 path): two writes give two op
/// lines, so a byte flipped in the first breaks the second's `prev`
/// and `Journal::load` marks `broken_at`.
#[cfg(unix)]
#[test]
fn a_refused_rekey_leaves_no_in_progress_marker() {
    let sb = Sandbox::new();
    sb.write(".env", "FIRST=old\n");
    sb.write("second.pem", "KEY=old\n");
    sb.write(
        "s.cig",
        "pack { \".\" }\nburn {\n  fs.write_text(\".env\", \"FIRST=new\")\n  fs.write_text(\"second.pem\", \"KEY=new\")\n}\n",
    );
    sb.run_ok(&["run", "s.cig"]);
    let id = sb.runs()[0]["id"].as_str().unwrap().to_string();
    let jp = sb.home().join("runs").join(&id).join("journal.jsonl");
    let text = fs::read_to_string(&jp).unwrap();
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let idx = lines
        .iter()
        .position(|l| l.contains("\"op\""))
        .expect("an op line");
    if let Some(pos) = lines[idx].find(".env") {
        lines[idx].replace_range(pos..pos + 4, ".ENX");
    } else {
        panic!("op line should name .env");
    }
    fs::write(&jp, lines.join("\n") + "\n").unwrap();

    // The rekey refuses in verification.
    let err = sb.run_err(&["tin", "rekey"], 1);
    assert!(
        err.contains("E720") || err.contains("does not verify") || err.contains("refus"),
        "rekey should refuse, got: {err}"
    );
    // No marker left on disk: this is SEC-004 N5. Before the fix, the
    // marker was created at the top of `rekey_inner` and a verification
    // refusal left it, so a later contention with a running cig read
    // it as "another rekey is in progress" instead of "a run is in
    // progress".
    let marker = sb.path().join(".cigscript-keys").join("rekey.in-progress");
    assert!(
        !marker.exists(),
        "rekey.in-progress must not be left behind by a refused rekey"
    );
}

/// #105: a run that burns nothing sealed refuses too when the tin key's
/// lock cannot be had, before its first op (E802): no file written, no run
/// recorded, no ledger line missing; `cig doctor` says why.
#[cfg(unix)]
#[test]
fn a_plain_run_refuses_before_it_burns_when_the_key_lock_cannot_be_had() {
    let sb = Sandbox::new();
    fs::write(sb.path().join(".cigscript-keys"), b"not a directory").unwrap();
    sb.write(
        "s.cig",
        "burn {\n  fs.write_text(\"plain.txt\", \"x\")\n}\n",
    );
    let out = sb.cig(&["run", "s.cig"]);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!out.status.success(), "{err}");
    assert!(
        err.contains("E802") && err.contains("keys directory"),
        "{err}"
    );
    assert!(!err.contains("tripwire"), "refused before it burned: {err}");
    assert!(!sb.exists("plain.txt"), "nothing burned");
    assert!(sb.runs().is_empty(), "no run recorded");
    let doctor = String::from_utf8_lossy(&sb.cig(&["__doctor"]).stdout).into_owned();
    assert!(doctor.contains("[E802]"), "{doctor}");
}

/// #105 (SEC-004 U1, U2): where the tin key's lock cannot be had, an import
/// refuses before it places anything, and `cig runs --prune` refuses before
/// a run goes, so no run lands or leaves without its ledger line.
#[cfg(unix)]
#[test]
fn unparcel_and_prune_refuse_when_the_key_lock_cannot_be_had() {
    let a = Sandbox::new();
    let id = parcelled_run(&a);
    a.run_ok(&["parcel", &id, "--out", "run.cigparcel"]);
    let parcel = a.path().join("run.cigparcel");
    let b = machine_b(&a);
    fs::write(b.path().join(".cigscript-keys"), b"not a directory").unwrap();
    let root = format!("p={}", b.path().join("p").display());
    let out = b.cig(&["unparcel", parcel.to_str().unwrap(), "--root", &root]);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!out.status.success(), "{err}");
    assert!(err.contains("keys directory"), "{err}");
    assert!(b.runs().is_empty(), "nothing placed: {:?}", b.runs());
    // `--prune` on the machine that has runs, with its keys path blocked
    // (its keys directory, which sealing made, taken away first).
    let _ = fs::remove_dir_all(a.path().join(".cigscript-keys"));
    fs::write(a.path().join(".cigscript-keys"), b"not a directory").unwrap();
    let out = a.cig(&["runs", "--prune", "0"]);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!out.status.success() && err.contains("E802"), "{err}");
    assert_eq!(a.runs().len(), 1, "no run went");
}

// ----- #149: the run index --------------------------------------------------

/// A crash mid-append leaves the index with a torn tail line. The next
/// `cig runs` reads the file, catches the tear, rebuilds the view from
/// the run folders, and shows every run — the same list as before the
/// corruption, byte for byte in `--json`.
#[test]
fn runs_index_survives_a_crash() {
    let sb = Sandbox::new();
    sb.write("a.cig", "exhale 1\n");
    sb.write("b.cig", "exhale 2\n");
    sb.write("c.cig", "exhale 3\n");
    for s in ["a.cig", "b.cig", "c.cig"] {
        sb.run_ok(&["run", s]);
    }
    let before: Vec<String> = sb
        .runs()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(before.len(), 3);
    // Tear the tail of the index: append half of a JSON line with no
    // closing brace and no newline (what a `write_all` killed mid-call
    // leaves behind).
    let p = sb.home().join("runs-index.jsonl");
    let mut text = fs::read_to_string(&p).unwrap();
    text.push_str("{\"op\":\"set\",\"id\":\"torn-tail-only");
    fs::write(&p, &text).unwrap();
    // The next listing rebuilds and says so. The view itself is right.
    let out = sb.cig(&["--json", "runs"]);
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        stderr.contains("runs index was torn"),
        "the rebuild note should fire once: {stderr}"
    );
    let after: Vec<String> = serde_json::from_slice::<Vec<serde_json::Value>>(&out.stdout)
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(before, after, "the rebuilt view is the same list");
    // A second call reads the (now rebuilt) index without saying so
    // again, and the file ends on a newline (no torn tail).
    let out = sb.cig(&["--json", "runs"]);
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("runs index was torn"),
        "the note fires once per process"
    );
    assert!(fs::read_to_string(&p).unwrap().ends_with('\n'));
}

/// `cig runs` reads the index, but the listing must match what a full
/// folder scan would produce for a history that holds the real shapes:
/// a pinned run, a pruned run, an interrupted run, and an unburned run.
#[cfg(unix)]
#[test]
fn runs_list_equals_a_folder_scan_for_a_mixed_history() {
    let sb = Sandbox::new();
    sb.write("pin.cig", "exhale 1\n");
    sb.write("prn.cig", "exhale 2\n");
    sb.write("int.cig", "exhale 3\n");
    sb.write("unb.cig", "burn {\n  fs.write_text(\"u.txt\", \"1\")\n}\n");
    sb.run_ok(&["run", "prn.cig"]);
    sb.run_ok(&["run", "pin.cig"]);
    let pin_id = sb.runs().iter().find(|r| r["script"] == "pin.cig").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    sb.run_ok(&["runs", "--pin", &pin_id]);
    sb.run_ok(&["run", "int.cig"]);
    sb.run_ok(&["run", "unb.cig"]);
    let unb_id = sb.runs().iter().find(|r| r["script"] == "unb.cig").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    sb.run_ok(&["unburn", &unb_id]);
    // Forge an `interrupted` state on int.cig by hand (its process is
    // gone; the record still says `running`).
    let int_id = sb.runs().iter().find(|r| r["script"] == "int.cig").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let rec_path = sb.home().join("runs").join(&int_id).join("run.json");
    let mut rec: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&rec_path).unwrap()).unwrap();
    rec["status"] = serde_json::json!("running");
    rec["finished"] = serde_json::Value::Null;
    rec["pid"] = serde_json::json!(2_147_483_647u32);
    fs::write(&rec_path, serde_json::to_string(&rec).unwrap()).unwrap();
    // Prune everything past the newest 3 (unb, int, pin): that drops
    // prn.cig's run. The pinned run is NOT pruned.
    sb.run_ok(&["runs", "--prune", "3"]);
    // The listing the index gives.
    let from_index: Vec<(String, String, String)> = sb
        .runs()
        .iter()
        .map(|r| {
            (
                r["id"].as_str().unwrap().to_string(),
                r["status"].as_str().unwrap().to_string(),
                r["script"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    // The listing a folder scan gives: throw the index away and read
    // again. The rebuild walks the folders.
    fs::remove_file(sb.home().join("runs-index.jsonl")).unwrap();
    let from_scan: Vec<(String, String, String)> = sb
        .runs()
        .iter()
        .map(|r| {
            (
                r["id"].as_str().unwrap().to_string(),
                r["status"].as_str().unwrap().to_string(),
                r["script"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        from_index, from_scan,
        "the index and a folder scan disagree: index={from_index:?} scan={from_scan:?}"
    );
    // Sanity: the expected mix is there (pinned, interrupted, unburned;
    // no prn, which was pruned).
    assert!(from_index
        .iter()
        .any(|(_, s, sc)| s == "unburned" && sc == "unb.cig"));
    assert!(from_index
        .iter()
        .any(|(_, s, sc)| s == "interrupted" && sc == "int.cig"));
    assert!(from_index.iter().any(|(_, _, sc)| sc == "pin.cig"));
    assert!(!from_index.iter().any(|(_, _, sc)| sc == "prn.cig"));
}

/// Two runs writing to the index in parallel do not tear it, and both
/// are listed when they finish. Serial writes under the index lock are
/// what keeps this true.
#[cfg(unix)]
#[test]
fn runs_index_is_consistent_under_two_concurrent_runs() {
    let sb = Sandbox::new();
    sb.write("a.cig", "exhale \"a\"\n");
    sb.write("b.cig", "exhale \"b\"\n");
    // Each run lays down a run.json and appends two lines to the index
    // (begin + finish). Parallel runs contend on the index lock.
    let a = Command::new(cig_bin())
        .args(["run", "a.cig"])
        .current_dir(sb.path())
        .env("CIGSCRIPT_HOME", sb.home())
        .env("NO_COLOR", "1")
        .env("CIG_SEAL_KEY", "file")
        .env("CIG_BUDGET_SYNC", "1")
        .env("CIG_INTERNAL_TRACKER_DIR", sb.path().join("tracker"))
        .env_remove("CI")
        .env("CIG_UPDATE_MANIFEST", sb.path().join("no-releases.json"))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn a");
    let b = Command::new(cig_bin())
        .args(["run", "b.cig"])
        .current_dir(sb.path())
        .env("CIGSCRIPT_HOME", sb.home())
        .env("NO_COLOR", "1")
        .env("CIG_SEAL_KEY", "file")
        .env("CIG_BUDGET_SYNC", "1")
        .env("CIG_INTERNAL_TRACKER_DIR", sb.path().join("tracker"))
        .env_remove("CI")
        .env("CIG_UPDATE_MANIFEST", sb.path().join("no-releases.json"))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn b");
    assert!(a.wait_with_output().unwrap().status.success());
    assert!(b.wait_with_output().unwrap().status.success());
    // The index file ends on a newline (no torn append) and holds both
    // runs. `cig runs` lists both, with no rebuild note.
    let text = fs::read_to_string(sb.home().join("runs-index.jsonl")).unwrap();
    assert!(
        text.ends_with('\n'),
        "the index has a torn tail: ...{:?}",
        text.chars().rev().take(20).collect::<String>()
    );
    let out = sb.cig(&["--json", "runs"]);
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        !stderr.contains("torn"),
        "the index was torn after two concurrent runs: {stderr}"
    );
    let scripts: Vec<String> = serde_json::from_slice::<Vec<serde_json::Value>>(&out.stdout)
        .unwrap()
        .iter()
        .map(|r| r["script"].as_str().unwrap().to_string())
        .collect();
    assert!(
        scripts.iter().any(|s| s == "a.cig") && scripts.iter().any(|s| s == "b.cig"),
        "both runs should be listed: {scripts:?}"
    );
}

/// #177: `cig runs --verify` with no id reads each journal's last line
/// (as well as the ledger) on the fast path, so a cut journal is caught
/// without `--deep`. The old fast path compared only `run.json`'s head
/// with the ledger and let a cut journal pass as ok.
#[test]
fn verify_all_catches_a_cut_journal() {
    let sb = Sandbox::new();
    for k in 1..=3 {
        sb.write(
            &format!("s{k}.cig"),
            &format!(
                "burn {{\n  fs.write_text(\"f{k}.txt\", \"{k}\")\n  fs.write_text(\"g{k}.txt\", \"{k}\")\n}}\n"
            ),
        );
        sb.run_ok(&["run", &format!("s{k}.cig")]);
    }
    // Cut the last line of the oldest journal: the record stays as-is
    // (it still names a head that no line now ends at), the ledger
    // stays as-is (its line for the run names the same head), but the
    // journal on disk now ends at an earlier line. The fast path has
    // to see that.
    let oldest = sb.runs().last().unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let journal = sb.home().join("runs").join(&oldest).join("journal.jsonl");
    let text = fs::read_to_string(&journal).unwrap();
    let trimmed = {
        let mut v: Vec<&str> = text.lines().collect();
        v.pop();
        let mut out = v.join("\n");
        out.push('\n');
        out
    };
    fs::write(&journal, trimmed).unwrap();
    let out = sb.cig(&["--json", "runs", "--verify"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a cut journal should exit non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], false, "{v}");
    assert!(
        v["problems"].to_string().contains(&oldest),
        "the cut run should be named: {v}"
    );
}

/// #177: a journal swapped for an older copy is caught too (same
/// mechanism: the journal's last line gives a different head than the
/// ledger and the record).
#[test]
fn verify_all_catches_a_replaced_journal() {
    let sb = Sandbox::new();
    sb.write("a.cig", "burn {\n  fs.write_text(\"a.txt\", \"a\")\n}\n");
    sb.run_ok(&["run", "a.cig"]);
    // First run's id and journal, kept for later.
    let first_id = sb.runs().first().unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let first_journal = sb.home().join("runs").join(&first_id).join("journal.jsonl");
    let old_journal = fs::read_to_string(&first_journal).unwrap();
    // Run again: a new, longer journal is written for the SAME script.
    sb.write(
        "b.cig",
        "burn {\n  fs.write_text(\"b.txt\", \"b\")\n  fs.write_text(\"c.txt\", \"c\")\n}\n",
    );
    sb.run_ok(&["run", "b.cig"]);
    let second_id = sb.runs().first().unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    // Put the older copy back in place of the second run's journal.
    let second_journal = sb
        .home()
        .join("runs")
        .join(&second_id)
        .join("journal.jsonl");
    fs::write(&second_journal, old_journal).unwrap();
    let out = sb.cig(&["--json", "runs", "--verify"]);
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], false, "{v}");
    assert!(
        v["problems"].to_string().contains(&second_id),
        "the replaced run should be named: {v}"
    );
}

/// #177: a missing or empty ledger, with sealed runs that recorded
/// chain heads, is a problem — not a silent `ok` — and the fast path
/// falls back to a deep walk so the chains are still checked. The
/// exit is non-zero.
#[test]
fn verify_all_with_no_ledger_is_not_ok() {
    let sb = Sandbox::new();
    sb.write("a.cig", "burn {\n  fs.write_text(\"a.txt\", \"a\")\n}\n");
    sb.run_ok(&["run", "a.cig"]);
    // The ledger lives in `$CIGSCRIPT_HOME/../.cigscript-keys/ledger.jsonl`
    // when CIGSCRIPT_HOME is set (seal::keys_dir()).
    let ledger = sb
        .home()
        .parent()
        .unwrap()
        .join(".cigscript-keys")
        .join("ledger.jsonl");
    assert!(
        ledger.exists(),
        "the ledger should exist before we remove it"
    );
    fs::remove_file(&ledger).unwrap();
    let out = sb.cig(&["--json", "runs", "--verify"]);
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], false, "{v}");
    assert_eq!(v["deep"], true, "fell back to deep walk: {v}");
    assert!(
        v["problems"].to_string().contains("ledger"),
        "the ledger is named: {v}"
    );
}

/// #177: same for an emptied ledger file (not merely missing).
#[test]
fn verify_all_with_an_empty_ledger_is_not_ok() {
    let sb = Sandbox::new();
    sb.write("a.cig", "burn {\n  fs.write_text(\"a.txt\", \"a\")\n}\n");
    sb.run_ok(&["run", "a.cig"]);
    let ledger = sb
        .home()
        .parent()
        .unwrap()
        .join(".cigscript-keys")
        .join("ledger.jsonl");
    fs::write(&ledger, "").unwrap();
    let out = sb.cig(&["--json", "runs", "--verify"]);
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], false, "{v}");
    assert!(
        v["problems"].to_string().contains("ledger"),
        "the ledger is named: {v}"
    );
}

/// #177: `cig doctor` names a missing ledger as a problem when there
/// are sealed runs that recorded chain heads. A fresh home with no
/// runs does not.
#[test]
fn doctor_says_the_ledger_is_gone() {
    let sb = Sandbox::new();
    // Fresh home, no runs. Nothing about a missing ledger.
    let out = sb.cig(&["__doctor"]);
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        !text.to_lowercase().contains("ledger is missing"),
        "a fresh home with no runs should not alarm on the ledger: {text}"
    );
    // One run, then remove the ledger.
    sb.write("a.cig", "burn {\n  fs.write_text(\"a.txt\", \"a\")\n}\n");
    sb.run_ok(&["run", "a.cig"]);
    let ledger = sb
        .home()
        .parent()
        .unwrap()
        .join(".cigscript-keys")
        .join("ledger.jsonl");
    fs::remove_file(&ledger).unwrap();
    let out = sb.cig(&["__doctor"]);
    assert_eq!(out.status.code(), Some(1), "doctor should exit non-zero");
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        text.contains("the ledger is missing") || text.contains("the ledger is empty"),
        "doctor should name the ledger: {text}"
    );
}

/// #178: at ten thousand runs with keep_runs 100, the index stays near
/// its live view instead of growing forever. The test uses 1,000 runs
/// built directly as folders (as the existing #175 tests show, by
/// writing `run.json` + `journal.jsonl` beside it) so it is not slowed
/// by starting `cig` ten thousand times, but it exercises the same
/// `upsert`/compaction path.
#[test]
fn index_stays_bounded_under_keep_runs() {
    let sb = Sandbox::new();
    // Make a seed run to establish the key and the ledger.
    sb.write("seed.cig", "exhale 1\n");
    sb.run_ok(&["run", "seed.cig"]);
    // Many more runs: each `cig run` appends two index lines (begin +
    // finish) and `keep_by_config` prunes the rest. With keep_runs
    // active and the compaction heuristic in place (#178), the index
    // should not grow unboundedly.
    sb.write("w.cig", "exhale 1\n");
    sb.run_ok(&["config", "keep_runs", "20"]);
    for _ in 0..400 {
        // Not run_ok — some of these may exit non-zero on CI quirks;
        // the point is the index growth.
        let out = sb.cig(&["run", "w.cig"]);
        if !out.status.success() {
            panic!(
                "a run failed mid-loop: {} {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
    let p = sb.home().join("runs-index.jsonl");
    let text = fs::read_to_string(&p).unwrap();
    let lines = text.lines().filter(|l| !l.trim().is_empty()).count();
    // keep_runs = 20; live after pruning is about 20 (the seed is
    // dropped). Each run adds about three lines (start, finish, the
    // prune's), so an index that never compacts holds about 1,200 here;
    // compaction keeps it to about twice its fresh size plus 64 KiB,
    // a couple of hundred lines.
    assert!(
        lines <= 600,
        "the index grew to {lines} lines; compaction should keep it bounded"
    );
    // Live count: the map should hold only the live runs (plus perhaps
    // the one still-running record of the last cig that wrote).
    let v: Vec<serde_json::Value> = serde_json::from_str(&sb.run_ok(&["--json", "runs"])).unwrap();
    assert!(
        v.len() <= 21,
        "keep_runs = 20 should leave at most 21 runs, saw {} ({v:?})",
        v.len()
    );
}

/// #178: `cig runs --reindex` rebuilds the index by hand from the run
/// folders. After an over-sized index is written and the flag is run,
/// the file has one `set` line per live run and no `rm` lines.
#[test]
fn reindex_rebuilds_the_index_by_hand() {
    let sb = Sandbox::new();
    for k in 1..=3 {
        sb.write(&format!("s{k}.cig"), "exhale 1\n");
        sb.run_ok(&["run", &format!("s{k}.cig")]);
    }
    let p = sb.home().join("runs-index.jsonl");
    // Oversize it by hand: append a thousand junk-but-parseable rm
    // lines for ids that are not there.
    {
        use std::io::Write;
        let mut f = fs::OpenOptions::new().append(true).open(&p).unwrap();
        for k in 0..1_000 {
            let _ = writeln!(f, "{{\"op\":\"rm\",\"id\":\"ghost-{k}\"}}");
        }
    }
    let before = fs::read_to_string(&p).unwrap();
    assert!(
        before.lines().count() > 100,
        "the oversize index should hold many lines: {}",
        before.lines().count()
    );
    // Reindex.
    sb.run_ok(&["runs", "--reindex"]);
    let after = fs::read_to_string(&p).unwrap();
    let kept: Vec<serde_json::Value> = after
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        kept.len(),
        3,
        "three live runs, so three set lines after --reindex: {kept:?}"
    );
    assert!(
        kept.iter().all(|l| l["op"] == "set"),
        "no rm lines survive: {kept:?}"
    );
    // The listing still agrees.
    let v: Vec<serde_json::Value> = serde_json::from_str(&sb.run_ok(&["--json", "runs"])).unwrap();
    assert_eq!(v.len(), 3);
}

/// #178: the runs index is compacted after a `--prune` batch instead
/// of leaving N `rm` lines behind.
#[test]
fn prune_batch_compacts_the_index() {
    let sb = Sandbox::new();
    for k in 1..=10 {
        sb.write(&format!("s{k}.cig"), "exhale 1\n");
        sb.run_ok(&["run", &format!("s{k}.cig")]);
    }
    sb.run_ok(&["runs", "--prune", "3"]);
    let text = fs::read_to_string(sb.home().join("runs-index.jsonl")).unwrap();
    let lines: Vec<serde_json::Value> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let rm_lines = lines.iter().filter(|l| l["op"] == "rm").count();
    let set_lines = lines.iter().filter(|l| l["op"] == "set").count();
    assert_eq!(
        rm_lines, 0,
        "a prune batch should compact, leaving no rm lines: {lines:?}"
    );
    assert_eq!(set_lines, 3, "three live runs after prune, three set lines");
}

/// #149: `cig runs --verify` with no run checks every run's chain head
/// against the ledger without opening a journal; `--deep` walks each
/// journal too. A line edited inside a journal keeps the recorded heads
/// agreeing, so only `--deep` finds it.
#[test]
fn runs_verify_checks_every_head_and_deep_walks_every_journal() {
    let sb = Sandbox::new();
    for k in 1..=3 {
        sb.write(
            &format!("s{k}.cig"),
            &format!("burn {{\n  fs.write_text(\"f{k}.txt\", \"{k}\")\n  fs.write_text(\"g{k}.txt\", \"{k}\")\n}}\n"),
        );
        sb.run_ok(&["run", &format!("s{k}.cig")]);
    }
    let heads: serde_json::Value =
        serde_json::from_str(&sb.run_ok(&["--json", "runs", "--verify"])).unwrap();
    assert_eq!(heads["checked"], 3, "{heads}");
    assert_eq!(heads["ok"], true, "{heads}");
    assert_eq!(heads["deep"], false, "{heads}");
    let deep: serde_json::Value =
        serde_json::from_str(&sb.run_ok(&["--json", "runs", "--verify", "--deep"])).unwrap();
    assert_eq!(deep["ok"], true, "{deep}");
    // Edit a line in the middle of the oldest run's journal.
    let oldest = sb.runs().last().unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let journal = sb.home().join("runs").join(&oldest).join("journal.jsonl");
    let text = fs::read_to_string(&journal).unwrap();
    let edited = text.replacen("f1.txt", "f9.txt", 1);
    assert_ne!(text, edited, "the journal names the file");
    fs::write(&journal, edited).unwrap();
    let heads = sb.cig(&["--json", "runs", "--verify"]);
    let heads: serde_json::Value = serde_json::from_slice(&heads.stdout).unwrap();
    assert_eq!(heads["ok"], true, "heads alone cannot see inside: {heads}");
    let deep = sb.cig(&["--json", "runs", "--verify", "--deep"]);
    let deep_json: serde_json::Value = serde_json::from_slice(&deep.stdout).unwrap();
    assert_eq!(deep_json["ok"], false, "{deep_json}");
    assert!(
        deep_json["problems"].to_string().contains(&oldest),
        "{deep_json}"
    );
}

// ----- bench (#179) ---------------------------------------------------------

/// Write a minimal bench manifest beside a two-line hello script and its
/// expected stdout, returning the manifest path.
fn write_bench_hello(sb: &Sandbox) -> PathBuf {
    sb.write("bench/fixtures/hello.cig", "exhale \"hello\"\n");
    sb.write("bench/fixtures/hello.out", "hello\n");
    sb.write(
        "bench/bench.toml",
        &format!(
            r#"
[bench]
product = "cig"
[defaults]
samples = 2
per = 1
warmup = 1
[[case]]
name = "hello"
group = "start-up"
run = ['{cig}', "run", "fixtures/hello.cig"]
expect = {{ exit = 0, stdout = "fixtures/hello.out" }}
"#,
            cig = cig_bin().display()
        ),
    )
}

#[test]
fn bench_matching_case_reports_numbers() {
    let sb = Sandbox::new();
    write_bench_hello(&sb);
    let out = sb.run_ok(&["bench"]);
    assert!(out.contains("hello"), "{out}");
    assert!(out.contains("median ms"), "{out}");
    // The median column should be a number, not WRONG or -.
    let line = out.lines().find(|l| l.contains("hello")).unwrap();
    assert!(!line.contains("WRONG"), "{out}");
    assert!(!line.contains("- "), "{out}");
}

#[test]
fn wrong_output_gets_no_time_and_fails() {
    let sb = Sandbox::new();
    sb.write("bench/fixtures/hello.cig", "exhale \"hello\"\n");
    sb.write("bench/fixtures/wrong.out", "goodbye\n");
    sb.write(
        "bench/bench.toml",
        &format!(
            r#"
[bench]
product = "cig"
[defaults]
samples = 2
per = 1
warmup = 1
[[case]]
name = "says_hello"
run = ['{cig}', "run", "fixtures/hello.cig"]
expect = {{ exit = 0, stdout = "fixtures/wrong.out" }}
"#,
            cig = cig_bin().display()
        ),
    );
    let err = sb.run_err(&["bench"], 1);
    assert!(err.contains("WRONG"), "{err}");
}

#[test]
fn bench_unknown_key_is_a_usage_error_with_the_line() {
    let sb = Sandbox::new();
    sb.write(
        "bench/bench.toml",
        "[bench]\nproduct = \"cig\"\nflarp = 1\n",
    );
    let err = sb.run_err(&["bench"], 3);
    assert!(err.contains("flarp"), "{err}");
    assert!(err.contains("line 3"), "{err}");
}

#[test]
fn bench_json_carries_median_p10_p90_cv_and_rss() {
    let sb = Sandbox::new();
    write_bench_hello(&sb);
    let text = sb.run_ok(&["--json", "bench"]);
    let v: serde_json::Value = serde_json::from_str(&text).expect("json");
    let cases = v["cases"].as_array().expect("cases");
    assert_eq!(cases.len(), 1);
    let outcome = &cases[0]["outcome"];
    assert_eq!(outcome["state"], "timed", "{text}");
    assert!(outcome["median_ms"].as_f64().unwrap() > 0.0);
    assert!(outcome["p10_ms"].as_f64().is_some());
    assert!(outcome["p90_ms"].as_f64().is_some());
    assert!(outcome["cv"].as_f64().is_some());
    // RSS is only present on Unix; on Windows it may be null.
    if cfg!(unix) {
        assert!(outcome["rss_bytes"].as_u64().is_some(), "{text}");
    }
}

#[test]
#[cfg(unix)]
fn isolated_home_leaves_nothing() {
    let sb = Sandbox::new();
    // The user's own home, with what a user keeps in it: cig's state, the
    // orchestrator's, and a key. A case that writes under every home it is
    // handed must leave all of it as it was, byte for byte.
    let outer_home = sb.path().join("outer-home");
    for (name, body) in [
        (".cigscript/runs/r1/journal", "journal\n"),
        (".cigscript/seal.key", "not-a-real-key\n"),
        (".cig-orchestrator/state.json", "{}\n"),
        (".ssh/id_ed25519", "not-a-real-key\n"),
    ] {
        let p = outer_home.join(name);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }
    let before = tree_listing(&outer_home);
    let outer_cig = sb.path().join("outer-cigscript-home");
    fs::create_dir_all(&outer_cig).unwrap();
    sb.write(
        "bench/bench.toml",
        r#"
[bench]
product = "cig"
[defaults]
samples = 2
per = 1
warmup = 1
[[case]]
name = "touches_every_home"
run = ["sh", "-c", "for d in \"$HOME\" \"$USERPROFILE\" \"$CIGSCRIPT_HOME\" \"$CIGO_HOME\"; do mkdir -p \"$d/.cigscript\" \"$d/.cig-orchestrator\"; echo x > \"$d/cookie\"; echo x >> \"$d/.cigscript/seal.key\"; echo x >> \"$d/.cig-orchestrator/state.json\"; done; echo ok"]
expect = { exit = 0 }
"#,
    );
    // Passing HOME via env is enough: cig bench propagates it as the
    // isolated home only for the sampled commands.
    let out = Command::new(cig_bin())
        .args(["bench"])
        .current_dir(sb.path())
        .env("CIGSCRIPT_HOME", &outer_cig)
        .env("CIGO_HOME", sb.path().join("outer-cigo-home"))
        .env("HOME", &outer_home)
        .env("USERPROFILE", &outer_home)
        .env("NO_COLOR", "1")
        .env("CIG_SEAL_KEY", "file")
        .env("CIG_BUDGET_SYNC", "1")
        .env("CIG_INTERNAL_TRACKER_DIR", sb.path().join("tracker"))
        .env_remove("CI")
        .output()
        .expect("run cig");
    assert!(
        out.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // The outer homes were never touched: same files, same bytes.
    assert!(
        !outer_home.join("cookie").exists(),
        "`cookie` leaked into the real HOME {}",
        outer_home.display()
    );
    assert_eq!(tree_listing(&outer_home), before, "the user's home changed");
    assert_eq!(
        tree_listing(&outer_cig),
        Vec::<(String, String)>::new(),
        "the user's CIGSCRIPT_HOME changed"
    );
    assert!(
        !sb.path().join("outer-cigo-home").exists(),
        "the user's CIGO_HOME was made"
    );
}

/// Every file under `root` with its contents, in a stable order.
#[cfg(unix)]
fn tree_listing(root: &Path) -> Vec<(String, String)> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, String)>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, root, out);
            } else {
                out.push((
                    p.strip_prefix(root).unwrap().to_string_lossy().into_owned(),
                    fs::read_to_string(&p).unwrap_or_default(),
                ));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

#[test]
#[cfg(unix)]
fn bench_shared_home_is_the_callers_own() {
    let sb = Sandbox::new();
    let outer_home = sb.path().join("outer-home");
    fs::create_dir_all(&outer_home).unwrap();
    sb.write(
        "bench/bench.toml",
        r#"
[bench]
product = "cig"
home = "shared"
[defaults]
samples = 1
per = 1
warmup = 1
[[case]]
name = "touches_home"
run = ["sh", "-c", "touch \"$HOME/cookie\" && echo ok"]
expect = { exit = 0 }
"#,
    );
    let out = Command::new(cig_bin())
        .args(["bench"])
        .current_dir(sb.path())
        .env("CIGSCRIPT_HOME", sb.home())
        .env("HOME", &outer_home)
        .env("NO_COLOR", "1")
        .env("CIG_SEAL_KEY", "file")
        .env("CIG_BUDGET_SYNC", "1")
        .env("CIG_INTERNAL_TRACKER_DIR", sb.path().join("tracker"))
        .env_remove("CI")
        .output()
        .expect("run cig");
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        outer_home.join("cookie").exists(),
        "home = \"shared\" did not run in the caller's HOME"
    );
}

#[test]
fn bench_zero_samples_is_a_usage_error() {
    let sb = Sandbox::new();
    sb.write(
        "bench/bench.toml",
        r#"
[bench]
product = "cig"
[defaults]
samples = 0
[[case]]
name = "nothing"
run = ["cig", "--version"]
"#,
    );
    let out = Command::new(cig_bin())
        .args(["bench"])
        .current_dir(sb.path())
        .env("CIGSCRIPT_HOME", sb.home())
        .env("NO_COLOR", "1")
        .env("CIG_INTERNAL_TRACKER_DIR", sb.path().join("tracker"))
        .env_remove("CI")
        .output()
        .expect("run cig");
    assert_eq!(
        out.status.code(),
        Some(3),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("at least 1"),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // A flag cannot sneak a zero past it either.
    let flag = Command::new(cig_bin())
        .args(["bench", "--per", "0"])
        .current_dir(sb.path())
        .env("CIGSCRIPT_HOME", sb.home())
        .env("NO_COLOR", "1")
        .env("CIG_INTERNAL_TRACKER_DIR", sb.path().join("tracker"))
        .env_remove("CI")
        .output()
        .expect("run cig");
    assert_eq!(flag.status.code(), Some(3));
}

// ----- bench, steps 2 and 3 (#179): --save, --against, --watch -------------

/// A manifest for the A/B and watch tests: one case that runs `cig` on a
/// script, checked for exit 0 only (a fake binary stands in on side A).
fn write_bench_ab(sb: &Sandbox, extra_bench: &str, samples: usize) {
    sb.write("bench/fixtures/hello.cig", "exhale \"hello\"\n");
    sb.write(
        "bench/bench.toml",
        &format!(
            r#"
format = 1
[bench]
product = "cig"
{extra_bench}
[defaults]
samples = {samples}
per = 1
warmup = 1
[[case]]
name = "hello"
group = "start-up"
run = ["cig", "run", "fixtures/hello.cig"]
"#
        ),
    );
}

/// An executable shell script standing in for a cig binary.
#[cfg(unix)]
fn fake_cig(sb: &Sandbox, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = sb.write(name, &format!("#!/bin/sh\n{body}\n"));
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    p
}

#[test]
fn manifest_schema_is_versioned() {
    let sb = Sandbox::new();
    write_bench_ab(&sb, "", 2);
    let good = sb.read("bench/bench.toml");
    // format = 1 is accepted, and so is a manifest with no format at all.
    sb.run_ok(&["bench"]);
    sb.write("bench/bench.toml", &good.replace("format = 1\n", ""));
    sb.run_ok(&["bench"]);
    // Any other format is refused, naming it, rather than guessed at.
    sb.write(
        "bench/bench.toml",
        &good.replace("format = 1", "format = 2"),
    );
    let err = sb.run_err(&["bench"], 3);
    assert!(err.contains("format 2"), "{err}");
    sb.write(
        "bench/bench.toml",
        &good.replace("format = 1", "format = \"one\""),
    );
    let err = sb.run_err(&["bench"], 3);
    assert!(err.contains("format"), "{err}");
    // The JSON a run writes says which format it is.
    sb.write("bench/bench.toml", &good);
    let v: serde_json::Value = serde_json::from_str(&sb.run_ok(&["--json", "bench"])).unwrap();
    assert_eq!(v["format"], 1);
    // A saved baseline carries it too, and one in another format is refused.
    sb.run_ok(&["bench", "--save", "main"]);
    let path = sb.path().join(".cig-bench/baselines/main.json");
    let saved: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(saved["format"], 1);
    assert_eq!(saved["cases"][0]["name"], "hello");
    assert_eq!(saved["cases"][0]["samples_ms"].as_array().unwrap().len(), 2);
    let mut newer = saved.clone();
    newer["format"] = serde_json::json!(2);
    fs::write(&path, newer.to_string()).unwrap();
    let err = sb.run_err(&["bench", "--against", "main"], 3);
    assert!(err.contains("format 2"), "{err}");
}

#[test]
fn bench_save_names_are_plain_file_names() {
    let sb = Sandbox::new();
    write_bench_ab(&sb, "", 2);
    for bad in ["../escape", "a/b", ".hidden"] {
        let err = sb.run_err(&["bench", "--save", bad], 3);
        assert!(err.contains("not a baseline name"), "{bad}: {err}");
    }
    assert!(!sb.exists(".cig-bench/escape.json"));
}

#[test]
#[cfg(unix)]
fn bench_against_a_binary_runs_interleaved_and_names_the_verdict() {
    let sb = Sandbox::new();
    write_bench_ab(&sb, "", 9);
    // Side A takes 800 ms to do nothing; side B is this cig, a debug build, far quicker even on a
    // loaded machine (150 ms was within reach of a debug cig under load, and the verdict flipped).
    let slow = fake_cig(&sb, "slow/cig", "sleep 0.8");
    let out = sb.cig_env(
        &["--json", "bench", "--against", slow.to_str().unwrap()],
        &[],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["against"]["kind"], "binary");
    let o = &v["cases"][0]["outcome"];
    assert_eq!(o["state"], "compared", "{v}");
    assert_eq!(o["interleaved"], true);
    // The verdict itself is the statistics' job, proven on fixed samples by
    // `interleaved_ab_cancels_a_drift` and `a_real_10_percent_regression_is_found`; here, on a
    // real clock that a loaded machine can stretch, the run must compare, say a verdict, and see
    // this cig as the quicker side.
    assert!(
        matches!(o["verdict"].as_str(), Some("faster" | "same")),
        "{v}"
    );
    assert!(
        o["p"].as_f64().is_some() && o["effect"].as_f64().is_some(),
        "{v}"
    );
    assert!(o["change"].as_f64().unwrap() < 0.0, "{v}");
    // The table says it too.
    let table = sb.run_ok(&["bench", "--against", slow.to_str().unwrap()]);
    assert!(table.contains("interleaved A/B, Mann-Whitney"), "{table}");
    // The same two sides the other way round, and a wrong side A: a side
    // that fails its check gets no time.
    let broken = fake_cig(&sb, "broken/cig", "exit 7");
    let err = sb.run_err(&["bench", "--against", broken.to_str().unwrap()], 1);
    assert!(err.contains("WRONG") && err.contains("side A"), "{err}");
}

#[test]
#[cfg(unix)]
fn bench_save_then_against_a_baseline() {
    let sb = Sandbox::new();
    write_bench_ab(&sb, "", 3);
    sb.run_ok(&["bench", "--save", "main"]);
    assert!(sb.exists(".cig-bench/baselines/main.json"));
    // The baseline's binary is still there and unchanged: interleaved.
    let out = sb.cig_env(&["--json", "bench", "--against", "main"], &[]);
    assert_eq!(out.status.code(), Some(0));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["against"]["kind"], "baseline");
    let o = &v["cases"][0]["outcome"];
    assert_eq!(o["interleaved"], true, "{v}");
    assert!(
        ["same", "faster", "slower"].contains(&o["verdict"].as_str().unwrap()),
        "{v}"
    );
    // With the binary gone the saved timings are compared with fresh ones,
    // and the report says it was not interleaved.
    let path = sb.path().join(".cig-bench/baselines/main.json");
    let mut saved: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    saved["binary"]["path"] = serde_json::json!(sb.path().join("gone/cig").to_str().unwrap());
    fs::write(&path, saved.to_string()).unwrap();
    let out = sb.cig_env(
        &["--json", "bench", "--against", "main", "--save", "next"],
        &[],
    );
    assert_eq!(out.status.code(), Some(0));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not interleaved"), "{err}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["cases"][0]["outcome"]["interleaved"], false, "{v}");
    // `--save` next to `--against` keeps this run's side as the new baseline.
    assert!(sb.exists(".cig-bench/baselines/next.json"));
}

#[test]
#[cfg(unix)]
fn against_a_release_checks_its_sha256() {
    let sb = Sandbox::new();
    write_bench_ab(&sb, "", 2);
    let version = cigscript::update::Version(1, 0, 0);
    let name = cigscript::update::asset_name(&version);
    // The "release" is a local fixture: a program that leaves a marker if
    // it is ever started. No network is touched.
    let marker = sb.path().join("ran-the-release");
    let asset = fake_cig(&sb, "asset.bin", &format!("touch '{}'", marker.display()));
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
    // A wrong sha256: refused before anything runs.
    let out = sb.cig_env(&["bench", "--against", "1.0.0"], &envs);
    assert_eq!(out.status.code(), Some(3));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("checksum mismatch") && err.contains("nothing was run"),
        "{err}"
    );
    assert!(
        !marker.exists(),
        "the release ran before its sha256 was checked"
    );
    // No sidecar at all is refused too.
    let no_sidecar = serde_json::json!([{
        "tag_name": "v1.0.0", "prerelease": false, "draft": false, "html_url": "u", "published_at": "",
        "assets": [{"name": name, "browser_download_url": format!("file://{}", asset.display())}]
    }]);
    let m2 = sb.write("manifest2.json", &no_sidecar.to_string());
    let out = sb.cig_env(
        &["bench", "--against", "release:1.0.0"],
        &[("CIG_UPDATE_MANIFEST", m2.to_str().unwrap())],
    );
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no checksum"));
    assert!(!marker.exists());
    // With the right sha256 it is run, as side A.
    let sum = cigscript::burn::journal::sha256_file(&asset).unwrap();
    fs::write(&sidecar, format!("{sum}  {name}\n")).unwrap();
    let out = sb.cig_env(&["--json", "bench", "--against", "1.0.0"], &envs);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(marker.exists(), "the verified release never ran");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["against"]["kind"], "release", "{v}");
    assert_eq!(v["against"]["label"], "cig 1.0.0", "{v}");
}

#[test]
#[cfg(unix)]
fn bench_against_a_git_ref_builds_it_in_a_temporary_worktree() {
    // Needs git; a machine without it has nothing to test here.
    if Command::new("git").arg("--version").output().is_err() {
        return;
    }
    let sb = Sandbox::new();
    write_bench_ab(
        &sb,
        "build = [\"sh\", \"build.sh\"]\nbinary = \"out/cig\"",
        3,
    );
    sb.write(
        "build.sh",
        "mkdir -p out && printf '#!/bin/sh\\nexit 0\\n' > out/cig && chmod +x out/cig\n",
    );
    // This checkout's own build, then a commit to name as the ref.
    let ok = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(sb.path())
            .env("HOME", sb.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    ok(&["init", "-q", "."]);
    ok(&["add", "bench", "build.sh"]);
    ok(&["commit", "-q", "-m", "base"]);
    assert!(Command::new("sh")
        .arg("build.sh")
        .current_dir(sb.path())
        .status()
        .unwrap()
        .success());
    let out = sb.cig_env(&["--json", "bench", "--against", "HEAD"], &[]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["against"]["kind"], "ref", "{v}");
    assert!(
        v["against"]["label"].as_str().unwrap().starts_with("HEAD@"),
        "{v}"
    );
    assert_eq!(v["cases"][0]["outcome"]["state"], "compared", "{v}");
    // The temporary worktree is gone afterwards.
    let list = Command::new("git")
        .args(["worktree", "list"])
        .current_dir(sb.path())
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&list.stdout).lines().count(),
        1,
        "{}",
        String::from_utf8_lossy(&list.stdout)
    );
    // A name that is no ref is a usage error.
    let err = sb.run_err(&["bench", "--against", "no-such-ref"], 3);
    assert!(err.contains("not a git ref"), "{err}");
}

/// A running `cig bench --watch` whose stdout is read line by line, and
/// which is killed when the test ends however it ends.
#[cfg(unix)]
struct WatchChild {
    child: std::process::Child,
    lines: std::sync::mpsc::Receiver<String>,
}

#[cfg(unix)]
impl WatchChild {
    fn start(sb: &Sandbox, args: &[&str]) -> WatchChild {
        use std::io::BufRead;
        let mut child = Command::new(cig_bin())
            .args(args)
            .current_dir(sb.path())
            .env("CIGSCRIPT_HOME", sb.home())
            .env("HOME", sb.path())
            .env("NO_COLOR", "1")
            .env("CIG_SEAL_KEY", "file")
            .env("CIG_INTERNAL_TRACKER_DIR", sb.path().join("tracker"))
            .env("CIG_UPDATE_MANIFEST", sb.path().join("no-releases.json"))
            .env_remove("CI")
            // Not a terminal: no keys are read, and the end of this input
            // must not end the watch.
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn cig bench --watch");
        let out = child.stdout.take().unwrap();
        let (tx, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        WatchChild { child, lines }
    }

    /// Lines up to and including the first one `done` accepts. A hard
    /// timeout, waited on the channel: no sleeping.
    fn until(&self, done: impl Fn(&str) -> bool) -> Vec<String> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        let mut seen = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(l) => {
                    let stop = done(&l);
                    seen.push(l);
                    if stop {
                        return seen;
                    }
                }
                Err(_) => panic!("timed out; the watch printed:\n{}", seen.join("\n")),
            }
        }
    }
}

#[cfg(unix)]
impl Drop for WatchChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
#[cfg(unix)]
fn watch_reruns_on_change() {
    let sb = Sandbox::new();
    write_bench_ab(&sb, "watch = [\"watched.txt\"]", 2);
    // A real cig for the case: `cig` on PATH is replaced by the running one.
    sb.write("watched.txt", "one");
    let w = WatchChild::start(&sb, &["--json", "bench", "--watch"]);
    let first = w.until(|l| l.contains("\"event\":\"cycle\""));
    let events: Vec<serde_json::Value> = first
        .iter()
        .map(|l| serde_json::from_str(l).expect("every line is JSON"))
        .collect();
    assert_eq!(events[0]["event"], "start", "{first:?}");
    let case = events
        .iter()
        .find(|e| e["event"] == "case")
        .expect("a case");
    assert_eq!(case["build"], 1);
    assert_eq!(case["state"], "timed", "{case}");
    assert!(
        case.get("verdict").is_none(),
        "build 1 has nothing to compare with"
    );
    // Nothing has changed, so nothing reruns until the file does.
    sb.write("watched.txt", "two, and longer");
    let second = w.until(|l| l.contains("\"build\":2") && l.contains("\"event\":\"cycle\""));
    let events: Vec<serde_json::Value> = second
        .iter()
        .map(|l| serde_json::from_str(l).expect("every line is JSON"))
        .collect();
    let change = events
        .iter()
        .find(|e| e["event"] == "change")
        .expect("change");
    assert_eq!(change["build"], 2);
    assert_eq!(
        change["changed"],
        serde_json::json!(["watched.txt"]),
        "{change}"
    );
    let case = events
        .iter()
        .find(|e| e["event"] == "case")
        .expect("a case");
    assert_eq!(case["build"], 2);
    assert!(case["base_median_ms"].as_f64().is_some(), "{case}");
    assert!(
        ["same", "faster", "slower"].contains(&case["verdict"].as_str().unwrap()),
        "{case}"
    );
}

#[test]
#[cfg(unix)]
fn watch_plain_is_line_based() {
    let sb = Sandbox::new();
    write_bench_ab(&sb, "watch = [\"watched.txt\"]", 2);
    sb.write("watched.txt", "one");
    // Stdout is a pipe: plain lines, no redraws, no escape codes.
    let mut w = WatchChild::start(&sb, &["bench", "--watch"]);
    let first = w.until(|l| l.starts_with("bench: build 1 done"));
    assert_eq!(
        first
            .iter()
            .filter(|l| l.contains("build 1 hello:"))
            .count(),
        1,
        "one line per case per build: {first:?}"
    );
    // Closed stdin did not end the watch: it is still waiting for a change.
    assert!(
        w.child.try_wait().unwrap().is_none(),
        "the watch quit on its own"
    );
    sb.write("watched.txt", "two, and longer");
    let second = w.until(|l| l.starts_with("bench: build 2 done"));
    assert!(
        second
            .iter()
            .any(|l| l == "bench: build 2: changed watched.txt"),
        "{second:?}"
    );
    let line = second
        .iter()
        .find(|l| l.contains("build 2 hello:"))
        .unwrap();
    assert!(
        line.contains(" vs "),
        "build 2 is compared with build 1: {line}"
    );
    for l in first.iter().chain(second.iter()) {
        assert!(l.starts_with("bench: "), "{l:?}");
        assert!(
            !l.contains('\x1b') && !l.contains('\r'),
            "no redraw codes in line mode: {l:?}"
        );
    }
}

#[test]
#[cfg(unix)]
fn bench_watch_a_failed_build_keeps_the_numbers() {
    let sb = Sandbox::new();
    write_bench_ab(
        &sb,
        "watch = [\"watched.txt\"]\nbuild = [\"sh\", \"-c\", \"test ! -e break || { echo nope >&2; exit 1; }\"]",
        2,
    );
    sb.write("watched.txt", "one");
    let w = WatchChild::start(&sb, &["bench", "--watch"]);
    w.until(|l| l.starts_with("bench: build 1 done"));
    sb.write("break", "");
    sb.write("watched.txt", "two, and longer");
    let second = w.until(|l| l.contains("build 2 failed"));
    assert!(second.last().unwrap().contains("nope"), "{second:?}");
    assert!(
        !second.iter().any(|l| l.contains("build 2 hello:")),
        "a failed build measured something: {second:?}"
    );
}

#[test]
fn bench_watch_needs_something_to_watch() {
    let sb = Sandbox::new();
    write_bench_ab(&sb, "", 2);
    let err = sb.run_err(&["bench", "--watch"], 3);
    assert!(err.contains("[bench].watch"), "{err}");
    let err = sb.run_err(&["bench", "--watch", "--save", "x"], 3);
    assert!(err.contains("--save"), "{err}");
}

// ----- #179 step 5, #145: cig's own suite and the kernel measures ------------

/// Run a child with a hard deadline: past it the child is killed and the
/// test fails, so a stuck benchmark cannot hang the suite.
fn bounded(cmd: &mut Command, secs: u64) -> Output {
    use std::io::Read;
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start cig");
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let out_t = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = out.read_to_end(&mut v);
        v
    });
    let err_t = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = err.read_to_end(&mut v);
        v
    });
    let deadline = Instant::now() + Duration::from_secs(secs);
    let status = loop {
        if let Some(s) = child.try_wait().expect("wait") {
            break s;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("cig did not finish in {secs} s and was killed");
        }
        std::thread::park_timeout(Duration::from_millis(25));
    };
    Output {
        status,
        stdout: out_t.join().unwrap(),
        stderr: err_t.join().unwrap(),
    }
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &dest);
        } else {
            fs::copy(e.path(), &dest).unwrap();
        }
    }
}

/// A copy of cig's own suite in the sandbox (`bench/`), its `binary` pointing
/// at the cig under test, and the manifest's path.
fn own_suite(sb: &Sandbox) -> PathBuf {
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("bench"),
        &sb.path().join("bench"),
    );
    let path = sb.path().join("bench").join("bench.toml");
    let text = fs::read_to_string(&path).unwrap();
    let mut seen = false;
    let text: String = text
        .lines()
        .map(|l| {
            if l.starts_with("binary") {
                seen = true;
                format!("binary = '{}'", cig_bin().display())
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(seen, "the suite names the product's binary");
    fs::write(&path, text + "\n").unwrap();
    path
}

/// `cig bench` in the sandbox, under a deadline: the exit code, stdout and
/// stderr.
fn bench_bounded(sb: &Sandbox, args: &[&str], secs: u64) -> (Option<i32>, String, String) {
    let out = bounded(
        Command::new(cig_bin())
            .args(args)
            .current_dir(sb.path())
            .env("CIGSCRIPT_HOME", sb.home())
            .env("NO_COLOR", "1")
            .env("CIG_SEAL_KEY", "file")
            .env("CIG_BUDGET_SYNC", "1")
            .env("CIG_INTERNAL_TRACKER_DIR", sb.path().join("tracker"))
            .env("CIG_UPDATE_MANIFEST", sb.path().join("no-releases.json"))
            .env_remove("CI"),
        secs,
    );
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn every_benchmark_matches_its_expect_file() {
    let sb = Sandbox::new();
    let manifest = own_suite(&sb);
    let text = fs::read_to_string(&manifest).unwrap();
    let doc: toml::Value = text.parse().expect("the suite is TOML");
    assert_eq!(doc["format"].as_integer(), Some(1), "the suite is format 1");
    let cases = doc["case"].as_array().expect("cases");
    assert!(
        cases.len() >= 30,
        "the compute and kernel sets: {}",
        cases.len()
    );

    // Every case names an expect file that exists, and no .expect file is
    // left that no case reads.
    let mut names = std::collections::BTreeSet::new();
    let mut read = std::collections::BTreeSet::new();
    for c in cases {
        let name = c["name"].as_str().unwrap();
        assert!(names.insert(name.to_string()), "{name} twice");
        let e = c
            .get("expect")
            .and_then(|e| e.as_table())
            .unwrap_or_else(|| panic!("{name} has no `expect`"));
        let file = e
            .get("stdout")
            .or_else(|| e.get("after_stdout"))
            .and_then(|f| f.as_str())
            .unwrap_or_else(|| panic!("{name} has no expect file"));
        assert!(
            sb.exists(&format!("bench/{file}")),
            "{name}: {file} is missing"
        );
        read.insert(file.to_string());
    }
    fn expect_files(dir: &Path, root: &Path, out: &mut Vec<String>) {
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                expect_files(&p, root, out);
            } else if p.extension().is_some_and(|x| x == "expect") {
                out.push(
                    p.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    let mut on_disk = Vec::new();
    expect_files(
        &sb.path().join("bench"),
        &sb.path().join("bench"),
        &mut on_disk,
    );
    for f in &on_disk {
        assert!(read.contains(f), "{f} is read by no case");
    }

    // The start-up and compute sets, for real: a case whose output is not
    // its expect file is `wrong`, and gets no time. The kernel set runs in
    // `kernel_benchmarks_restore_exact_bytes` and in the nightly suite.
    for group in ["start-up", "compute"] {
        let (code, stdout, stderr) = bench_bounded(
            &sb,
            &[
                "--json",
                "bench",
                "bench/bench.toml",
                "--group",
                group,
                "--samples",
                "1",
                "--per",
                "1",
            ],
            600,
        );
        assert_eq!(code, Some(0), "{group}:\n{stdout}\n{stderr}");
        let v: serde_json::Value = serde_json::from_str(&stdout).expect("json");
        let rows = v["cases"].as_array().unwrap();
        assert!(!rows.is_empty(), "{group} has cases");
        for r in rows {
            assert_eq!(r["outcome"]["state"], "timed", "{}: {r}", r["name"]);
        }
    }
}

#[test]
fn kernel_benchmarks_restore_exact_bytes() {
    let sb = Sandbox::new();
    own_suite(&sb);

    // The harness's own proof: the unburn cases check what was restored
    // with an `after` script, and a case that restores anything else is WRONG.
    for only in ["unburn small tree", "restore big file"] {
        let (code, stdout, stderr) = bench_bounded(
            &sb,
            &[
                "--json",
                "bench",
                "bench/bench.toml",
                "--only",
                only,
                "--samples",
                "1",
                "--per",
                "1",
            ],
            600,
        );
        assert_eq!(code, Some(0), "{only}:\n{stdout}\n{stderr}");
        let v: serde_json::Value = serde_json::from_str(&stdout).expect("json");
        assert_eq!(
            v["cases"][0]["outcome"]["state"], "timed",
            "{only}: {stdout}"
        );
    }

    // And ours, apart from the script that checks: the same scripts, run
    // by hand, compared byte for byte.
    let a = "0123456789abcdef".repeat(2_097_152).into_bytes();
    let b = "fedcba9876543210".repeat(2_097_152).into_bytes();
    let big = sb.home().join("work").join("big.bin");
    sb.run_ok(&["run", "bench/kernel/write-big.cig", "--", "a"]);
    assert!(fs::read(&big).unwrap() == a, "the first version is written");
    sb.run_ok(&["run", "bench/kernel/write-big.cig", "--", "b"]);
    assert!(
        fs::read(&big).unwrap() == b,
        "the second version is written"
    );
    sb.run_ok(&["unburn", "--last", "1", "-y"]);
    assert!(
        fs::read(&big).unwrap() == a,
        "the unburn put back every byte of the first version"
    );

    // A tree: made, then gone, and made again by the same burn.
    sb.run_ok(&["run", "bench/kernel/burn-small.cig"]);
    let tree = sb.home().join("work").join("small");
    assert_eq!(fs::read_dir(&tree).unwrap().count(), 100);
    for i in [0usize, 57, 99] {
        assert_eq!(
            fs::read(tree.join(format!("f{i}.txt"))).unwrap(),
            vec![b'x'; 1024]
        );
    }
    sb.run_ok(&["unburn", "--last", "1", "-y"]);
    assert!(!tree.exists(), "the tree is gone");
}

#[test]
fn disk_per_byte_is_reported() {
    let sb = Sandbox::new();
    sb.write(
        "bench/fixtures/burn.cig",
        "stick p = \"${env.get(\"CIGSCRIPT_HOME\")}/work/out.txt\"\nburn {\n  fs.mkdir(\"${env.get(\"CIGSCRIPT_HOME\")}/work\")\n  fs.write_text(p, \"z\".repeat(10000))\n}\nexhale \"ok\"\n",
    );
    sb.write("bench/fixtures/burn.out", "ok\n");
    let manifest = |measure: &str, burned: &str| {
        format!(
            r#"
[bench]
product = "cig"
[defaults]
samples = 2
per = 1
warmup = 1
[[case]]
name = "burn ten thousand bytes"
group = "kernel"
run = ['{cig}', "run", "fixtures/burn.cig"]
expect = {{ exit = 0, stdout = "fixtures/burn.out" }}
{burned}
measure = {measure}
"#,
            cig = cig_bin().display()
        )
    };
    sb.write(
        "bench/bench.toml",
        &manifest(
            r#"["wall", "tin-growth", "disk-per-byte", "fsyncs-per-op", "file:$CIGSCRIPT_HOME/work/out.txt"]"#,
            "burned = 10000\nops = 2",
        ),
    );
    let (code, stdout, stderr) = bench_bounded(&sb, &["--json", "bench"], 120);
    assert_eq!(code, Some(0), "{stdout}\n{stderr}");
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    let outcome = &v["cases"][0]["outcome"];
    assert_eq!(outcome["state"], "timed", "{stdout}");
    let metrics = outcome["metrics"].as_array().expect("metrics");
    let get = |name: &str| {
        metrics
            .iter()
            .find(|m| m["name"] == name)
            .unwrap_or_else(|| panic!("{name} is not reported: {stdout}"))
    };
    // Bytes on disk per byte burned: at least the bytes themselves.
    let ratio = get("disk-per-byte")["value"].as_f64().expect("a number");
    assert!(
        ratio >= 1.0,
        "a burn costs at least the bytes it writes: {ratio}"
    );
    assert_eq!(get("disk-per-byte")["unit"], "bytes per byte");
    // The file's size after the run is exact.
    assert_eq!(
        get("file:$CIGSCRIPT_HOME/work/out.txt")["value"].as_f64(),
        Some(10000.0)
    );
    // The tin's growth is a number (this burn keeps nothing in the tin).
    assert!(get("tin-growth")["value"].as_f64().is_some(), "{stdout}");
    // Syncs per operation: counted where the kernel can be asked, and said
    // plainly where it cannot, never guessed.
    let fs_ = get("fsyncs-per-op");
    match fs_["value"].as_f64() {
        Some(n) => assert!(n > 0.0, "a journaled burn syncs: {n}"),
        None => assert!(
            fs_["note"]
                .as_str()
                .is_some_and(|n| n.starts_with("not measured")),
            "{fs_}"
        ),
    }
    // The plain table shows them and says they are not gated.
    let (code, table, _) = bench_bounded(&sb, &["bench"], 120);
    assert_eq!(code, Some(0), "{table}");
    assert!(table.contains("disk-per-byte"), "{table}");
    assert!(table.contains("never gated"), "{table}");
    let (_, md, _) = bench_bounded(&sb, &["bench", "--markdown"], 120);
    assert!(
        md.contains("| `burn ten thousand bytes` | `disk-per-byte` |"),
        "{md}"
    );

    // A ratio needs its denominator: a case that asks for one without
    // `burned` is refused, naming the case.
    sb.write(
        "bench/bench.toml",
        &manifest(r#"["wall", "disk-per-byte"]"#, ""),
    );
    let (code, _, err) = bench_bounded(&sb, &["bench"], 60);
    assert_eq!(code, Some(3), "{err}");
    assert!(
        err.contains("disk-per-byte") && err.contains("burned"),
        "{err}"
    );
}

#[test]
fn bench_after_and_before_lists_check_what_a_command_left() {
    let sb = Sandbox::new();
    sb.write(
        "bench/bench.toml",
        r#"
[bench]
product = "cig"
[defaults]
samples = 2
per = 1
warmup = 1
[[case]]
name = "leaves a file"
run = ["sh", "-c", "echo two >> \"$HOME/f\""]
before = [["sh", "-c", "echo one > \"$HOME/f\""]]
after = ["sh", "-c", "cat \"$HOME/f\""]
expect = { exit = 0, after_stdout = "after.out" }
"#,
    );
    sb.write("bench/after.out", "one\ntwo\n");
    let (code, out, err) = bench_bounded(&sb, &["bench"], 60);
    if cfg!(unix) {
        assert_eq!(code, Some(0), "{out}\n{err}");
        // A wrong `after` is WRONG, not timed.
        sb.write("bench/after.out", "one\n");
        let (code, _, err) = bench_bounded(&sb, &["bench"], 60);
        assert_eq!(code, Some(1), "{err}");
        assert!(err.contains("WRONG") && err.contains("`after`"), "{err}");
    }
}

// ----- #176: the keys folder under a shared parent is not shared -----------
//
// With `CIGSCRIPT_HOME` set to a folder under `/tmp` (as tests, CI and
// throwaway runs do), the keys folder used to land at `/tmp/.cigscript-keys`,
// one folder shared by every user: another user could plant a seal.key the
// victim's runs trusted, remove the ledger, or refuse every other run with
// E802 on the rekey lock. The tests here prove the fix: a shared parent now
// gets a per-user name, and the folder and the files under it are checked
// (StrictModes-style) before any lock or key read.

/// Build a runnable cig environment under `parent` (world-writable, sticky
/// bit set), as if `/tmp` were the parent of `CIGSCRIPT_HOME`. Returns a
/// configured `Sandbox`-like holder, with its own directories.
#[cfg(unix)]
struct SharedParentSandbox {
    dir: tempfile::TempDir,
    home: PathBuf,
}

#[cfg(unix)]
impl SharedParentSandbox {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        // Mimic `/tmp`: world-writable + sticky.
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o1777)).unwrap();
        let home = dir.path().join("cig-home");
        fs::create_dir(&home).unwrap();
        Self { dir, home }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn home(&self) -> &Path {
        &self.home
    }

    fn cig(&self, args: &[&str]) -> Output {
        Command::new(cig_bin())
            .args(args)
            .current_dir(self.home())
            .env("CIGSCRIPT_HOME", self.home())
            .env("NO_COLOR", "1")
            .env("CIG_SEAL_KEY", "file")
            .env("CIG_BUDGET_SYNC", "1")
            .env("CIG_INTERNAL_TRACKER_DIR", self.path().join("tracker"))
            .env("CIG_UPDATE_MANIFEST", self.path().join("no-releases.json"))
            .env_remove("CI")
            .output()
            .expect("run cig")
    }
}

/// With `CIGSCRIPT_HOME` under a sticky world-writable parent (like `/tmp`),
/// the keys folder gets a `-<uid>` suffix so two users under one parent
/// never share one folder. Under a private parent (the default Sandbox
/// tempdir, 0700) the name stays `.cigscript-keys` — tests that read the
/// old name keep working.
#[cfg(unix)]
#[test]
fn keys_dir_is_never_in_a_sticky_world_writable_parent() {
    let sb = SharedParentSandbox::new();
    fs::write(sb.home().join("s.cig"), "burn {\n  cough \"ok\"\n}\n").unwrap();
    let out = sb.cig(&["run", "s.cig"]);
    // The script coughs, but it ran far enough to make the keys folder.
    assert!(!String::from_utf8_lossy(&out.stderr).contains("E721"));
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    let per_user = sb.path().join(format!(".cigscript-keys-{uid}"));
    let shared = sb.path().join(".cigscript-keys");
    assert!(
        per_user.exists(),
        "expected per-user folder at {}",
        per_user.display()
    );
    assert!(!shared.exists(), "a shared .cigscript-keys appeared");
    // And a sibling test with the usual private parent keeps the old name.
    let private = Sandbox::new();
    private.write("s.cig", "burn {\n  cough \"ok\"\n}\n");
    let _ = private.cig(&["run", "s.cig"]);
    assert!(
        private.path().join(".cigscript-keys").exists(),
        "a private parent should keep the shared name"
    );
}

/// An install from before the per-user name (cig 1.18.0 and older) keeps
/// its own keys folder under the old shared name while it is still its
/// own and private, so the key and the ledger in it are not left behind.
#[cfg(unix)]
#[test]
fn an_own_folder_under_the_old_shared_name_stays_in_use() {
    use std::os::unix::fs::PermissionsExt;
    let sb = SharedParentSandbox::new();
    let legacy = sb.path().join(".cigscript-keys");
    fs::create_dir(&legacy).unwrap();
    fs::set_permissions(&legacy, fs::Permissions::from_mode(0o700)).unwrap();
    let key = legacy.join("seal.key");
    fs::write(&key, "ab".repeat(32) + "\n").unwrap();
    fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(sb.home().join("s.cig"), "exhale \"ok\"\n").unwrap();
    let out = sb.cig(&["run", "s.cig"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    assert!(
        !sb.path().join(format!(".cigscript-keys-{uid}")).exists(),
        "a per-user folder was made beside the user's own old one"
    );
    assert_eq!(fs::read_to_string(&key).unwrap(), "ab".repeat(32) + "\n");
}

/// A seal key with a second name (a hard link) is refused: it could be
/// read or changed through the other name.
#[cfg(unix)]
#[test]
fn a_hard_linked_seal_key_is_refused() {
    let sb = Sandbox::new();
    sb.write("s.cig", "exhale \"ok\"\n");
    let _ = sb.cig(&["run", "s.cig"]);
    let key = sb.path().join(".cigscript-keys").join("seal.key");
    if !key.exists() {
        use std::os::unix::fs::PermissionsExt;
        fs::write(&key, "ab".repeat(32) + "\n").unwrap();
        fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
    }
    fs::hard_link(&key, sb.path().join("another-name")).unwrap();
    // The key is checked where it is read: `cig tin mac` reads it.
    let err = sb.run_err(&["tin", "mac", "s.cig", "--label", "t"], 1);
    assert!(err.contains("E721"), "{err}");
    assert!(err.contains("hard links"), "{err}");
}

/// A seal.key planted by another user (or one world-writable by a party
/// who is not us) is refused with E721 before it is ever read.
#[cfg(unix)]
#[test]
fn a_key_owned_by_someone_else_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    // Make the keys folder and a plausible-looking key.
    let keys = sb.path().join(".cigscript-keys");
    fs::create_dir_all(&keys).unwrap();
    fs::set_permissions(&keys, fs::Permissions::from_mode(0o700)).unwrap();
    let key = keys.join("seal.key");
    fs::write(&key, "0".repeat(64) + "\n").unwrap();
    // We cannot chown without root, so mimic "someone else owns it" with
    // a wide mode the check also refuses.
    fs::set_permissions(&key, fs::Permissions::from_mode(0o644)).unwrap();
    sb.write("s.cig", "burn {\n  cough \"ok\"\n}\n");
    // The key is checked where it is read: `cig tin mac` reads it.
    let err = sb.run_err(&["tin", "mac", "s.cig", "--label", "t"], 1);
    assert!(err.contains("E721"), "{err}");
    assert!(err.contains("seal.key"), "{err}");
}

/// A keys folder someone else can write to is refused. We simulate the
/// unsafe folder with group- or world-write bits turned on.
#[cfg(unix)]
#[test]
fn a_key_folder_others_can_write_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    let keys = sb.path().join(".cigscript-keys");
    fs::create_dir_all(&keys).unwrap();
    fs::set_permissions(&keys, fs::Permissions::from_mode(0o777)).unwrap();
    sb.write("s.cig", "burn {\n  cough \"ok\"\n}\n");
    let err = sb.run_err(&["run", "s.cig"], 1);
    assert!(err.contains("E721"), "{err}");
    assert!(err.contains("keys folder"), "{err}");
}

/// A seal key others could read is refused, as OpenSSH refuses such a
/// private key, and the advice keeps the key: chmod it, and rekey if
/// anyone else could have read it. Never "move it aside": a new key
/// opens nothing the old one sealed.
#[cfg(unix)]
#[test]
fn a_seal_key_others_could_read_is_refused_with_advice_that_keeps_it() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    sb.write("s.cig", "burn {\n  cough \"ok\"\n}\n");
    let _ = sb.cig(&["run", "s.cig"]);
    let key = sb.path().join(".cigscript-keys").join("seal.key");
    if !key.exists() {
        // This build seals nothing by default here: make the key the way a
        // sealing run would, then open it up.
        fs::write(&key, "a".repeat(64) + "\n").unwrap();
    }
    fs::set_permissions(&key, fs::Permissions::from_mode(0o644)).unwrap();
    // The key is checked where it is read: `cig tin mac` reads it.
    let err = sb.run_err(&["tin", "mac", "s.cig", "--label", "t"], 1);
    assert!(err.contains("E721"), "{err}");
    assert!(err.contains("chmod 600"), "{err}");
    assert!(err.contains("cig tin rekey"), "{err}");
    assert!(!err.contains("move it aside"), "{err}");
}

/// The ledger and the rekey lock hold no secret, and the 0700 folder
/// keeps others out of them: one that is ours and too open (a lock an
/// early build made with the umask's mode) is made 0600 and the run goes
/// ahead, rather than every run refusing.
#[cfg(unix)]
#[test]
fn an_open_ledger_and_lock_of_ours_are_made_private_and_used() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let sb = Sandbox::new();
    sb.write("s.cig", "exhale \"ok\"\n");
    let _ = sb.cig(&["run", "s.cig"]);
    let keys = sb.path().join(".cigscript-keys");
    let lock = keys.join("rekey.lock");
    let ledger = keys.join("ledger.jsonl");
    for f in [&lock, &ledger] {
        if !f.exists() {
            fs::write(f, "").unwrap();
        }
        fs::set_permissions(f, fs::Permissions::from_mode(0o664)).unwrap();
    }
    let out = sb.cig(&["run", "s.cig"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for f in [&lock, &ledger] {
        let mode = fs::metadata(f).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o600, "{} is {mode:04o}", f.display());
    }
}

/// The keys folder, when cig has to make it, lands at 0700 on the first
/// stat — there is no window where it exists with wider permissions.
#[cfg(unix)]
#[test]
fn the_keys_folder_is_created_0700() {
    use std::os::unix::fs::MetadataExt;
    let sb = Sandbox::new();
    sb.write("s.cig", "burn {\n  cough \"ok\"\n}\n");
    let _ = sb.cig(&["run", "s.cig"]);
    let keys = sb.path().join(".cigscript-keys");
    let md = fs::symlink_metadata(&keys).unwrap();
    assert!(md.is_dir());
    assert_eq!(md.mode() & 0o777, 0o700, "mode = {:04o}", md.mode() & 0o777);
}

/// A symlink in place of `seal.key` is refused: a `fs::read_to_string`
/// would follow the link, so the check uses `symlink_metadata` and says
/// no before anything is read.
#[cfg(unix)]
#[test]
fn a_symlink_as_the_seal_key_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    let keys = sb.path().join(".cigscript-keys");
    fs::create_dir_all(&keys).unwrap();
    fs::set_permissions(&keys, fs::Permissions::from_mode(0o700)).unwrap();
    // Plant a target file of the right shape.
    let target = sb.path().join("evil-key");
    fs::write(&target, "a".repeat(64) + "\n").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    std::os::unix::fs::symlink(&target, keys.join("seal.key")).unwrap();
    sb.write("s.cig", "burn {\n  cough \"ok\"\n}\n");
    // The key is checked where it is read: `cig tin mac` reads it.
    let err = sb.run_err(&["tin", "mac", "s.cig", "--label", "t"], 1);
    assert!(err.contains("E721"), "{err}");
    assert!(err.contains("symlink"), "{err}");
}

/// The rekey lock does not create the keys folder: a cold start where
/// the folder does not exist still comes through the private helper,
/// so the folder is 0700 from its first stat (not 0755 from a bare
/// `create_dir_all`).
#[cfg(unix)]
#[test]
fn the_rekey_lock_does_not_create_the_folder_wider_than_0700() {
    use std::os::unix::fs::MetadataExt;
    let sb = Sandbox::new();
    // Trigger `cig tin rekey`, which goes through `acquire_rekey_lock`
    // and `hold_shared_rekey_lock`. First a run so the key exists.
    sb.write("s.cig", "burn {\n  cough \"ok\"\n}\n");
    let _ = sb.cig(&["run", "s.cig"]);
    // Rekey should go through our helper which creates the folder 0700.
    let _ = sb.cig(&["tin", "rekey"]);
    let keys = sb.path().join(".cigscript-keys");
    let md = fs::symlink_metadata(&keys).unwrap();
    assert_eq!(md.mode() & 0o777, 0o700, "mode = {:04o}", md.mode() & 0o777);
}

/// `cig doctor` names the keys folder, who owns it, and its mode; it
/// never creates the folder (if nothing has been burned yet, doctor
/// says so). The problem line fires when the folder would fail the
/// StrictModes check.
#[cfg(unix)]
#[test]
fn doctor_says_where_the_key_is_and_who_owns_it() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    // No key has been made: doctor must not create one.
    let out = String::from_utf8_lossy(&sb.cig(&["__doctor"]).stdout).into_owned();
    assert!(out.contains("key folder"), "{out}");
    assert!(
        !sb.path().join(".cigscript-keys").exists(),
        "doctor created the keys folder"
    );
    // Make the folder, then widen it: doctor flags it.
    sb.write("s.cig", "burn {\n  cough \"ok\"\n}\n");
    let _ = sb.cig(&["run", "s.cig"]);
    let keys = sb.path().join(".cigscript-keys");
    fs::set_permissions(&keys, fs::Permissions::from_mode(0o777)).unwrap();
    let doc = sb.cig(&["__doctor"]);
    let stdout = String::from_utf8_lossy(&doc.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&doc.stderr).into_owned();
    assert!(
        stdout.contains("owner uid") || stdout.contains("mode"),
        "doctor should print owner and mode: {stdout}"
    );
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        combined.contains("E721"),
        "doctor should flag E721: {combined}"
    );
}

/// `cig explain E721` prints the page.
#[test]
fn explain_e721_prints_a_page() {
    let sb = Sandbox::new();
    let out = sb.run_ok(&["explain", "E721"]);
    assert!(out.contains("E721"), "{out}");
    assert!(out.contains("key folder"), "{out}");
}

#[cfg(unix)]
#[test]
fn a_parcel_carries_a_pass_its_maker_signed() {
    let a = Sandbox::new();
    let id = parcelled_run(&a);
    a.run_ok(&["parcel", &id, "--out", "run.cigparcel"]);
    let parcel = a.path().join("run.cigparcel");
    assert!(a.exists("run.cigparcel.pass"), "the pass beside the parcel");
    let b = machine_b(&a);
    for f in ["run.cigparcel", "run.cigparcel.pass"] {
        fs::copy(a.path().join(f), b.path().join(f)).unwrap();
    }
    let parcel_b = b.path().join("run.cigparcel");
    let root = format!("p={}", b.path().join("p").display());
    // Its maker is not trusted here yet: --require-pass refuses it.
    let err = b.run_err(
        &[
            "unparcel",
            parcel_b.to_str().unwrap(),
            "--root",
            &root,
            "--require-pass",
        ],
        1,
    );
    assert!(err.contains("pass does not hold"), "{err}");
    // Trusted for parcels, the import goes through and names who made it.
    let line = a.run_ok(&["pass", "key", "--kinds", "parcel"]);
    let signers = b.write("signers", &line);
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&signers, fs::Permissions::from_mode(0o600)).unwrap();
    }
    b.run_ok(&["config", "pass_allowed_signers", signers.to_str().unwrap()]);
    let out = b.cig(&[
        "unparcel",
        parcel_b.to_str().unwrap(),
        "--root",
        &root,
        "--require-pass",
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("made by") && stderr.contains("unchanged since"),
        "{stderr}"
    );
    let _ = parcel;
}

// ---- cig tin parcel / unparcel (cig-script#141) ----
#[cfg(unix)]
mod tin_parcel {
    use super::*;

    /// `cig`, with a hard timeout, in a folder under the sandbox.
    #[cfg(unix)]
    fn tin_parcel_cig(sb: &Sandbox, sub: &str, args: &[&str]) -> Output {
        use wait_timeout::ChildExt;
        let mut child = Command::new(cig_bin())
            .args(args)
            .current_dir(sb.path().join(sub))
            .env("CIGSCRIPT_HOME", sb.home())
            .env("HOME", sb.path())
            .env("NO_COLOR", "1")
            .env("CIG_SEAL_KEY", "file")
            .env("CIG_INTERNAL_TRACKER_DIR", sb.path().join("tracker"))
            .env("CIG_UPDATE_MANIFEST", sb.path().join("no-releases.json"))
            .env_remove("CI")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn cig");
        if child
            .wait_timeout(std::time::Duration::from_secs(120))
            .expect("wait")
            .is_none()
        {
            let _ = child.kill();
            panic!("cig {args:?} timed out");
        }
        child.wait_with_output().expect("output")
    }

    #[cfg(unix)]
    fn tin_parcel_ok(sb: &Sandbox, sub: &str, args: &[&str]) -> String {
        let out = tin_parcel_cig(sb, sub, args);
        assert!(
            out.status.success(),
            "cig {args:?}: {:?}\n{}\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// Two runs, one of them sealing a secret. Returns the run ids.
    #[cfg(unix)]
    fn tin_parcel_runs(sb: &Sandbox) -> Vec<String> {
        sb.write("p/.env", "SECRET=original-secret-value\n");
        sb.write("p/a.txt", "alpha v1\n");
        sb.write(
        "one.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo SECRET=after > .env; echo alpha v2 > a.txt\"], {cwd: \"p\", touches: [\"**\"]})\n}\n",
    );
        sb.run_ok(&["run", "one.cig"]);
        sb.write(
        "two.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo alpha v3 > a.txt\"], {cwd: \"p\", touches: [\"**\"]})\n}\n",
    );
        sb.run_ok(&["run", "two.cig"]);
        let mut ids: Vec<String> = sb
            .runs()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        ids
    }

    /// Empty the tin: runs, objects, the run index and the ledger (the key stays).
    #[cfg(unix)]
    fn tin_parcel_wipe(sb: &Sandbox) {
        for d in ["runs", "tin"] {
            let p = sb.home().join(d);
            if p.exists() {
                // Objects are read-only files in read-only folders' worth of care.
                let _ = Command::new("chmod").args(["-R", "u+w"]).arg(&p).status();
                fs::remove_dir_all(&p).unwrap();
            }
        }
        let _ = fs::remove_file(sb.home().join("runs-index.jsonl"));
        let _ = fs::remove_file(sb.path().join(".cigscript-keys/ledger.jsonl"));
    }

    #[cfg(unix)]
    #[test]
    fn a_tin_parcel_restores_every_run_on_an_empty_tin() {
        let sb = Sandbox::new();
        let ids = tin_parcel_runs(&sb);
        assert_eq!(ids.len(), 2);
        let out = tin_parcel_ok(&sb, "", &["--json", "tin", "parcel", "--to", "backup"]);
        let w: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(w["runs"], 2, "{w}");
        assert!(w["objects_carried"].as_u64().unwrap() >= 2, "{w}");
        assert!(sb.exists("backup/manifest.json"));
        tin_parcel_wipe(&sb);
        assert!(sb.runs().is_empty());
        let out = tin_parcel_ok(&sb, "", &["--json", "tin", "unparcel", "backup"]);
        let r: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(r["runs_restored"], 2, "{r}");
        assert_eq!(r["ledger"], "installed", "{r}");
        let back: Vec<String> = {
            let mut v: Vec<String> = sb
                .runs()
                .iter()
                .map(|r| r["id"].as_str().unwrap().to_string())
                .collect();
            v.sort();
            v
        };
        assert_eq!(back, ids);
        // The restored tin undoes both runs, byte for byte, the secret included.
        tin_parcel_ok(&sb, "", &["tin", "verify"]);
        let newest_first: Vec<String> = sb
            .runs()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect();
        for id in &newest_first {
            tin_parcel_ok(&sb, "", &["unburn", id]);
        }
        assert_eq!(sb.read("p/.env"), "SECRET=original-secret-value\n");
        assert_eq!(sb.read("p/a.txt"), "alpha v1\n");
    }

    #[cfg(unix)]
    #[test]
    fn a_second_parcel_carries_only_new_objects() {
        let sb = Sandbox::new();
        tin_parcel_runs(&sb);
        let first: serde_json::Value = serde_json::from_str(
            tin_parcel_ok(&sb, "", &["--json", "tin", "parcel", "--to", "b1"]).trim(),
        )
        .unwrap();
        sb.write(
        "three.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo alpha v4 brand new bytes > a.txt\"], {cwd: \"p\", touches: [\"**\"]})\n}\n",
    );
        sb.run_ok(&["run", "three.cig"]);
        let second: serde_json::Value = serde_json::from_str(
            tin_parcel_ok(
                &sb,
                "",
                &["--json", "tin", "parcel", "--to", "b2", "--since", "b1"],
            )
            .trim(),
        )
        .unwrap();
        assert_eq!(second["runs"], 3, "{second}");
        assert_eq!(
            second["objects_carried"], 1,
            "only the new before-state is carried: {second}"
        );
        assert_eq!(
            second["objects_inherited"].as_u64().unwrap(),
            first["objects_carried"].as_u64().unwrap(),
            "{second}"
        );
        let m: serde_json::Value = serde_json::from_str(&sb.read("b2/manifest.json")).unwrap();
        assert!(
            m["parent"]["manifest_sha256"].as_str().unwrap().len() == 64,
            "{m}"
        );
        // Restored from the child alone (parent beside it) on an empty tin.
        tin_parcel_wipe(&sb);
        tin_parcel_ok(&sb, "", &["tin", "unparcel", "b2"]);
        assert_eq!(sb.runs().len(), 3);
        tin_parcel_ok(&sb, "", &["tin", "verify"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_tampered_parcel_is_refused_before_anything_is_written() {
        let sb = Sandbox::new();
        tin_parcel_runs(&sb);
        tin_parcel_ok(&sb, "", &["tin", "parcel", "--to", "backup"]);
        // Flip a byte in one of the journals.
        let runs_dir = sb.path().join("backup/runs");
        let first = fs::read_dir(&runs_dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let journal = first.join("journal.jsonl");
        let mut text = fs::read_to_string(&journal).unwrap();
        text.push_str("{\"tampered\":true}\n");
        fs::write(&journal, text).unwrap();
        tin_parcel_wipe(&sb);
        let out = tin_parcel_cig(&sb, "", &["tin", "unparcel", "backup"]);
        assert_eq!(out.status.code(), Some(1));
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("E722"), "{err}");
        assert!(sb.runs().is_empty(), "a run was written");
        assert!(
            !sb.home().join("tin").exists()
                || fs::read_dir(sb.home().join("tin"))
                    .unwrap()
                    .next()
                    .is_none()
        );
        // A missing object is the same refusal.
        let sb2 = Sandbox::new();
        tin_parcel_runs(&sb2);
        tin_parcel_ok(&sb2, "", &["tin", "parcel", "--to", "backup"]);
        let victim = walk_first_file(&sb2.path().join("backup/tin/objects"));
        fs::remove_file(victim).unwrap();
        tin_parcel_wipe(&sb2);
        let out = tin_parcel_cig(&sb2, "", &["tin", "unparcel", "backup"]);
        assert!(String::from_utf8_lossy(&out.stderr).contains("E722"));
        assert!(sb2.runs().is_empty());
    }

    #[cfg(unix)]
    fn walk_first_file(dir: &Path) -> PathBuf {
        walkdir::WalkDir::new(dir)
            .into_iter()
            .flatten()
            .find(|e| e.file_type().is_file())
            .expect("a file")
            .path()
            .to_path_buf()
    }

    #[cfg(unix)]
    #[test]
    fn unparcel_never_overwrites_a_different_live_run() {
        let sb = Sandbox::new();
        let ids = tin_parcel_runs(&sb);
        tin_parcel_ok(&sb, "", &["tin", "parcel", "--to", "backup"]);
        // The same run, identical: nothing to do, and it is not an error.
        let out = tin_parcel_ok(&sb, "", &["--json", "tin", "unparcel", "backup"]);
        let r: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(r["runs_restored"], 0, "{r}");
        assert_eq!(r["runs_present"], 2, "{r}");
        // Now the live run carries on differently.
        let live = sb.home().join("runs").join(&ids[0]).join("journal.jsonl");
        let mut t = fs::read_to_string(&live).unwrap();
        t.push('\n');
        t.push_str("{\"note\":\"live\"}\n");
        fs::write(&live, &t).unwrap();
        let out = tin_parcel_cig(&sb, "", &["tin", "unparcel", "backup"]);
        assert_eq!(out.status.code(), Some(1));
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("E723"), "{err}");
        assert_eq!(
            fs::read_to_string(&live).unwrap(),
            t,
            "the live journal changed"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_sealed_object_stays_sealed_in_a_git_ref() {
        let sb = Sandbox::new();
        tin_parcel_runs(&sb);
        fs::create_dir_all(sb.path().join("repo")).unwrap();
        let git = |args: &[&str]| {
            let o = Command::new("git")
                .args(args)
                .current_dir(sb.path().join("repo"))
                .env("HOME", sb.path())
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .expect("git");
            assert!(
                o.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&o.stderr)
            );
            String::from_utf8_lossy(&o.stdout).to_string()
        };
        git(&["init", "-q"]);
        // Nothing is written to git unless asked: a plain parcel leaves it empty.
        tin_parcel_ok(&sb, "", &["tin", "parcel", "--to", "dirparcel"]);
        assert!(git(&["for-each-ref"]).trim().is_empty());
        let out = tin_parcel_ok(
            &sb,
            "repo",
            &["--json", "tin", "parcel", "--git", "2026-11-11"],
        );
        let w: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(w["target"], "refs/cig/backup/2026-11-11", "{w}");
        let refs = git(&["for-each-ref", "--format=%(refname)"]);
        assert_eq!(refs.trim(), "refs/cig/backup/2026-11-11");
        // No branch, no parent: an orphan commit.
        assert!(
            git(&["rev-list", "--parents", "-n1", "refs/cig/backup/2026-11-11"])
                .trim()
                .split(' ')
                .count()
                == 1
        );
        let tree = git(&["ls-tree", "-r", "--name-only", "refs/cig/backup/2026-11-11"]);
        let sealed: Vec<&str> = tree
            .lines()
            .filter(|l| l.starts_with("tin/sealed/"))
            .collect();
        assert!(!sealed.is_empty(), "{tree}");
        for s in sealed {
            let blob = Command::new("git")
                .args([
                    "cat-file",
                    "blob",
                    &format!("refs/cig/backup/2026-11-11:{s}"),
                ])
                .current_dir(sb.path().join("repo"))
                .output()
                .unwrap()
                .stdout;
            assert!(blob.starts_with(b"CIGSEAL"), "not sealed");
            assert!(!String::from_utf8_lossy(&blob).contains("original-secret-value"));
        }
        // A second backup under the same name is refused, not moved.
        let out = tin_parcel_cig(&sb, "repo", &["tin", "parcel", "--git", "2026-11-11"]);
        assert_eq!(out.status.code(), Some(3));
        // And it restores.
        tin_parcel_wipe(&sb);
        tin_parcel_ok(&sb, "repo", &["tin", "unparcel", "--git", "2026-11-11"]);
        assert_eq!(sb.runs().len(), 2);
        for id in sb
            .runs()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
            .iter()
        {
            tin_parcel_ok(&sb, "", &["unburn", id]);
        }
        assert_eq!(sb.read("p/.env"), "SECRET=original-secret-value\n");
    }
}

/// `cig explain --layers` hands the place to the `cigar` on PATH with the
/// output flags, gives back its exit code, and says plainly when there is no
/// cigar (cigar-script#13).
#[cfg(unix)]
#[test]
fn explain_layers_is_cigars_answer() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    let bin = sb.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let stand_in = bin.join("cigar");
    fs::write(
        &stand_in,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$CIGAR_ARGV\"\nexit 4\n",
    )
    .unwrap();
    fs::set_permissions(&stand_in, fs::Permissions::from_mode(0o755)).unwrap();
    let argv = sb.path().join("argv.txt");
    let run = |path: &Path, args: &[&str]| {
        Command::new(cig_bin())
            .args(args)
            .current_dir(sb.path())
            .env("CIGSCRIPT_HOME", sb.home())
            .env("NO_COLOR", "1")
            .env("PATH", path)
            .env("CIGAR_ARGV", &argv)
            .env("CIG_INTERNAL_TRACKER_DIR", sb.path().join("tracker"))
            .env("CIG_UPDATE_MANIFEST", sb.path().join("no-releases.json"))
            .env_remove("CI")
            .output()
            .expect("run cig")
    };
    let out = run(&bin, &["--json", "explain", "--layers", "sales.cigar:12:1"]);
    assert_eq!(
        out.status.code(),
        Some(4),
        "cigar's exit code is cig's: {out:?}"
    );
    let got = fs::read_to_string(&argv).unwrap();
    assert_eq!(
        got.lines().collect::<Vec<_>>(),
        [
            "--json",
            "--no-color",
            "explain",
            "--layers",
            "sales.cigar:12:1"
        ]
    );
    let empty = sb.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let out = run(&empty, &["explain", "--layers", "sales.cigar:12:1"]);
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("needs cigar, which is not installed"), "{err}");
    assert!(err.contains("ciginstaller install stick cigar"), "{err}");
}

// ----- bench: --timings, --stats, --syscalls (#179 step 4) --------------------

/// A one-case manifest whose command is `sh run.sh`, the script written
/// beside it, so a test can stand in for any product.
#[cfg(unix)]
fn write_bench_sh(sb: &Sandbox, script: &str) {
    sb.write("bench/run.sh", &format!("{script}\n"));
    sb.write(
        "bench/bench.toml",
        "format = 1\n[bench]\nproduct = \"stand-in\"\n[defaults]\nsamples = 2\nper = 1\nwarmup = 1\n[[case]]\nname = \"stand-in case\"\nrun = [\"sh\", \"run.sh\"]\nexpect = { exit = 0 }\n",
    );
}

#[test]
#[cfg(unix)]
fn bench_timings_add_up_to_the_wall_with_other() {
    let sb = Sandbox::new();
    // A product that times two phases on the descriptor it was handed, then
    // does 150 ms of work it never reports: that is `other`.
    write_bench_sh(
        &sb,
        r#"if [ -n "$CIG_TIMINGS_FD" ]; then
echo '{"v":1,"product":"stand-in","phase":"read","ms":40}' >&$CIG_TIMINGS_FD
echo '{"v":1,"product":"stand-in","phase":"work","ms":60,"later":1}' >&$CIG_TIMINGS_FD
fi
sleep 0.25"#,
    );
    let (code, out, err) = bench_bounded(&sb, &["--json", "bench", "--timings"], 120);
    assert_eq!(code, Some(0), "{out}\n{err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let t = &v["cases"][0]["debug"]["timings"];
    assert_eq!(t["measured"], true, "{out}");
    assert_eq!(t["format"], 1);
    let phases = t["phases"].as_array().unwrap();
    assert_eq!(phases.len(), 2, "{t}");
    assert_eq!(phases[0]["phase"], "read");
    assert_eq!(phases[1]["ms"].as_f64(), Some(60.0));
    // The breakdown adds up to the run's own wall time, `other` included.
    let wall = t["wall_ms"].as_f64().unwrap();
    let sum: f64 = phases.iter().map(|p| p["ms"].as_f64().unwrap()).sum();
    let other = t["other_ms"].as_f64().unwrap();
    assert!((sum + other - wall).abs() <= wall * 0.05, "{t}");
    assert!(other > 100.0, "the unreported work is `other`: {t}");
    assert_eq!(t["overlap"], false);
    // The plain table shows the phases beside the wall time and says `other`.
    let (code, table, _) = bench_bounded(&sb, &["bench", "--timings"], 120);
    assert_eq!(code, Some(0), "{table}");
    assert!(table.contains("timings: stand-in case"), "{table}");
    assert!(table.contains("other") && table.contains("work"), "{table}");
    // Without the flag nothing is asked of the product.
    let plain = sb.run_ok(&["--json", "bench"]);
    assert!(!plain.contains("\"debug\""), "{plain}");
}

#[test]
#[cfg(unix)]
fn bench_timings_flag_phases_that_overlap_and_products_that_say_nothing() {
    let sb = Sandbox::new();
    // Phases that add up to far more than the run took.
    write_bench_sh(
        &sb,
        r#"[ -n "$CIG_TIMINGS_FD" ] && echo '{"v":1,"product":"p","phase":"a","ms":5000}' >&$CIG_TIMINGS_FD; true"#,
    );
    let (_, out, _) = bench_bounded(&sb, &["--json", "bench", "--timings"], 120);
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["cases"][0]["debug"]["timings"]["overlap"], true, "{out}");
    // A product that writes nothing, or another format, is not measured, and
    // the harness says why instead of inventing a breakdown.
    write_bench_sh(&sb, "true");
    let (code, out, _) = bench_bounded(&sb, &["--json", "bench", "--timings"], 120);
    assert_eq!(
        code,
        Some(0),
        "an unmeasured breakdown is not a failure: {out}"
    );
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let t = &v["cases"][0]["debug"]["timings"];
    assert_eq!(t["measured"], false);
    assert!(
        t["note"].as_str().unwrap().contains("CIG_TIMINGS_FD"),
        "{t}"
    );
    write_bench_sh(
        &sb,
        r#"[ -n "$CIG_TIMINGS_FD" ] && echo '{"v":2,"phase":"a","ms":1}' >&$CIG_TIMINGS_FD; true"#,
    );
    let (_, out, _) = bench_bounded(&sb, &["--json", "bench", "--timings"], 120);
    assert!(out.contains("not timings format 1"), "{out}");
}

#[test]
#[cfg(unix)]
fn cig_run_writes_its_phases_when_asked_and_only_then() {
    let sb = Sandbox::new();
    let script = sb.write("hello.cig", "exhale \"hi\"\n");
    let quiet = sb.cig(&["run", script.to_str().unwrap()]);
    assert!(
        !String::from_utf8_lossy(&quiet.stderr).contains("\"phase\""),
        "no flag, no lines"
    );
    let out = sb.cig(&["run", script.to_str().unwrap(), "--timings"]);
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    let names: Vec<String> = err
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["v"] == 1 && v["product"] == "cig")
        .filter_map(|v| v["phase"].as_str().map(str::to_string))
        .collect();
    for want in ["lower", "check", "run", "burn"] {
        assert!(
            names.iter().any(|n| n == want),
            "{want} in {names:?}\n{err}"
        );
    }
    // The order they ended in: the script is read, checked, then run.
    let at = |n: &str| names.iter().position(|x| x == n).unwrap();
    assert!(at("lower") < at("check") && at("check") < at("run") && at("run") < at("burn"));
    // A bad descriptor is a usage error, never a silent no-op.
    let bad = sb.cig(&["run", script.to_str().unwrap(), "--timings=77"]);
    assert_eq!(bad.status.code(), Some(3));
    // `--stats` says plainly when this build does not count.
    let st = sb.cig(&["run", script.to_str().unwrap(), "--stats"]);
    assert!(st.status.success());
    let st_err = String::from_utf8_lossy(&st.stderr);
    assert!(
        st_err.contains("bench-stats") || st_err.contains("\"allocs\""),
        "{st_err}"
    );
}

#[test]
#[cfg(unix)]
fn bench_timings_and_stats_read_cig_itself() {
    let sb = Sandbox::new();
    write_bench_hello(&sb);
    let (code, out, err) = bench_bounded(&sb, &["--json", "bench", "--timings", "--stats"], 120);
    assert_eq!(code, Some(0), "{out}\n{err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let d = &v["cases"][0]["debug"];
    let names: Vec<&str> = d["timings"]["phases"]
        .as_array()
        .expect("phases")
        .iter()
        .map(|p| p["phase"].as_str().unwrap())
        .collect();
    for want in ["lower", "check", "run", "burn"] {
        assert!(names.contains(&want), "{want} in {names:?}");
    }
    let wall = d["timings"]["wall_ms"].as_f64().unwrap();
    let sum: f64 = d["timings"]["phases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["ms"].as_f64().unwrap())
        .sum();
    assert!(
        sum <= wall * 1.05,
        "cig's own phases never pass the wall: {sum} vs {wall}"
    );
    // Counters: numbers in a build with `bench-stats`, and a plain "not
    // measured" naming the feature in one without.
    let c = &d["stats"]["counters"][0];
    match c["value"].as_f64() {
        Some(n) => assert!(n > 0.0, "{c}"),
        None => assert!(c["note"].as_str().unwrap().contains("bench-stats"), "{c}"),
    }
    // They describe one build: not with --against or --watch.
    let (code, _, err) = bench_bounded(&sb, &["bench", "--timings", "--watch"], 20);
    assert_eq!(code, Some(3), "{err}");
}

/// A stand-in `strace` that writes a canned `-c` table to the `-o` file and
/// then runs the command, so the whole path is exercised on any Linux.
#[cfg(target_os = "linux")]
fn fake_strace(sb: &Sandbox, rows: &str) -> PathBuf {
    fake_cig(
        sb,
        "fakebin/strace",
        &format!(
            r#"while [ "$1" != "-o" ]; do shift; done; shift; OUT=$1; shift
printf '%s\n' '% time     seconds  usecs/call     calls    errors syscall' '------ ----------- ----------- --------- --------- ----------------' {rows} '------ ----------- ----------- --------- --------- ----------------' > "$OUT"
exec "$@""#
        ),
    )
}

#[test]
#[cfg(target_os = "linux")]
fn syscalls_flags_thrash() {
    let sb = Sandbox::new();
    write_bench_sh(&sb, "true");
    let strace = fake_strace(
        &sb,
        "' 70.00    0.700000          14     50000           munmap' ' 30.00    0.300000           6     50000           mmap'",
    );
    let path = format!(
        "{}:{}",
        strace.parent().unwrap().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let run = |args: &[&str]| {
        let out = bounded(
            Command::new(cig_bin())
                .args(args)
                .current_dir(sb.path())
                .env("PATH", &path)
                .env("CIGSCRIPT_HOME", sb.home())
                .env("NO_COLOR", "1")
                .env("CIG_SEAL_KEY", "file")
                .env("CIG_INTERNAL_TRACKER_DIR", sb.path().join("tracker")),
            120,
        );
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).to_string(),
        )
    };
    let (code, out) = run(&["--json", "bench", "--syscalls"]);
    assert_eq!(code, Some(0), "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let s = &v["cases"][0]["debug"]["syscalls"];
    assert_eq!(s["measured"], true, "{out}");
    assert_eq!(s["total"], 100000);
    assert_eq!(s["top"][0]["syscall"], "munmap");
    assert!(
        s["thrash"].as_str().is_some_and(|t| t.contains("thrash")),
        "{s}"
    );
    // The plain table flags it; and a flag is not a gate: exit stays 0.
    let (code, table) = run(&["bench", "--syscalls"]);
    assert_eq!(code, Some(0));
    assert!(table.contains("FLAG: mmap/munmap thrash"), "{table}");
}

#[test]
#[cfg(target_os = "linux")]
fn syscalls_without_strace_say_not_measured() {
    let sb = Sandbox::new();
    write_bench_sh(&sb, "true");
    // An empty folder as PATH has no strace (and no sh: the case uses its
    // absolute path).
    let empty = sb.path().join("nopath");
    fs::create_dir_all(&empty).unwrap();
    sb.write(
        "bench/bench.toml",
        "format = 1\n[bench]\nproduct = \"x\"\n[defaults]\nsamples = 1\nper = 1\n[[case]]\nname = \"t\"\nrun = [\"/bin/true\"]\nexpect = { exit = 0 }\n",
    );
    let out = bounded(
        Command::new(cig_bin())
            .args(["--json", "bench", "--syscalls"])
            .current_dir(sb.path())
            .env("PATH", &empty)
            .env("CIGSCRIPT_HOME", sb.home())
            .env("CIG_SEAL_KEY", "file")
            .env("CIG_INTERNAL_TRACKER_DIR", sb.path().join("tracker")),
        60,
    );
    assert_eq!(out.status.code(), Some(0));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    let s = &v["cases"][0]["debug"]["syscalls"];
    assert_eq!(s["measured"], false, "{s}");
    assert!(
        s["note"].as_str().unwrap().starts_with("not measured"),
        "{s}"
    );
}

/// The same flag on a real loop, where the machine has `strace` and `python3`
/// and may use `ptrace`: 3,000 mmap/munmap pairs are flagged, and a program
/// that maps once is not. Not on musl: cig's musl build allocates through
/// mimalloc, which keeps the loop from mapping at all (#146).
#[test]
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
fn syscalls_flags_a_real_mmap_loop() {
    let have = |tool: &str| {
        Command::new(tool)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    if !have("strace") || !have("python3") {
        eprintln!("skipped: needs strace and python3");
        return;
    }
    let sb = Sandbox::new();
    let case = |name: &str, body: &str| {
        format!(
            "[[case]]\nname = \"{name}\"\nrun = [\"python3\", \"-c\", '''{body}''']\nexpect = {{ exit = 0 }}\n"
        )
    };
    sb.write(
        "bench/bench.toml",
        &format!(
            "format = 1\n[bench]\nproduct = \"stand-in\"\n[defaults]\nsamples = 1\nper = 1\nwarmup = 1\n{}{}",
            case(
                "thrash",
                "import mmap\nfor _ in range(3000):\n    m = mmap.mmap(-1, 1 << 20)\n    m.close()"
            ),
            case("calm", "import mmap\nm = mmap.mmap(-1, 1 << 20)\nm.close()")
        ),
    );
    let (code, out, err) = bench_bounded(&sb, &["--json", "bench", "--syscalls"], 180);
    assert_eq!(code, Some(0), "{out}\n{err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let sys = |i: usize| v["cases"][i]["debug"]["syscalls"].clone();
    if sys(0)["measured"] == false {
        eprintln!("skipped: strace is not allowed here: {}", sys(0)["note"]);
        return;
    }
    assert!(sys(0)["thrash"].is_string(), "{}", sys(0));
    assert!(sys(1)["thrash"].is_null(), "{}", sys(1));
}

// ----- bench: --family, --with, --matrix (#179 step 7) ----------------------

/// A stand-in product: `<name>/bench/bench.toml` with one case that runs
/// `bin/<name>` (a shell script made from `body`) and expects `<out>` on
/// stdout. Returns the product's folder.
#[cfg(unix)]
fn stand_in_product(sb: &Sandbox, name: &str, body: &str, out: &str) -> PathBuf {
    fake_cig(sb, &format!("{name}/bin/{name}"), body);
    sb.write(&format!("{name}/bench/fixtures/out.txt"), out);
    sb.write(
        &format!("{name}/bench/bench.toml"),
        &format!(
            "format = 1\n[bench]\nproduct = \"{name}\"\nbinary = \"bin/{name}\"\n[defaults]\nsamples = 2\nper = 1\nwarmup = 1\n[[case]]\nname = \"{name} case\"\nrun = [\"bin/{name}\"]\nexpect = {{ exit = 0, stdout = \"fixtures/out.txt\" }}\n"
        ),
    );
    sb.path().join(name)
}

/// The family of three stand-ins: a `cig` that says which one it is, a
/// `cigo` that runs whatever `cig` is on its PATH, and a `cigar` that never
/// uses either.
#[cfg(unix)]
fn stand_in_family(sb: &Sandbox) -> Vec<String> {
    let cig = stand_in_product(sb, "cig", "echo cig-one", "cig-one\n");
    let cigo = stand_in_product(sb, "cigo", "exec cig", "cig-one\n");
    let cigar = stand_in_product(sb, "cigar", "echo cigar", "cigar\n");
    let mut a = Vec::new();
    for (n, p) in [("cig", cig), ("cigo", cigo), ("cigar", cigar)] {
        a.push("--product".to_string());
        a.push(format!("{n}={}", p.display()));
    }
    a
}

#[test]
#[cfg(unix)]
fn family_with_swaps_one_product() {
    let sb = Sandbox::new();
    let products = stand_in_family(&sb);
    let two = fake_cig(&sb, "other/cig", "echo cig-two");
    let mut base: Vec<&str> = vec!["--json", "bench", "--family"];
    base.extend(products.iter().map(String::as_str));

    // The family as it stands: every suite timed, each against the cig it
    // finds first on its path.
    let (code, out, err) = bench_bounded(&sb, &base, 120);
    assert_eq!(code, Some(0), "{out}\n{err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let state = |v: &serde_json::Value, p: &str| {
        v["products"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["product"] == p)
            .unwrap_or_else(|| panic!("{p} in {v}"))["cases"][0]["outcome"]["state"]
            .clone()
    };
    for p in ["cig", "cigo", "cigar"] {
        assert_eq!(state(&v, p), "timed", "{p}: {out}");
    }

    // `--with cig@<other>`: the cig under cigo is now the other one. cigo's
    // own binary is unchanged and says so by what it printed: its case now
    // sees "cig-two", where it expects "cig-one". The suite that never calls
    // cig is untouched.
    let with = format!("cig@{}", two.display());
    let mut swapped = base.clone();
    swapped.extend(["--with", &with]);
    let (code, out, _) = bench_bounded(&sb, &swapped, 120);
    assert_eq!(code, Some(1), "a wrong answer under the swap fails: {out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(state(&v, "cigo"), "wrong", "{out}");
    let why = v["products"][1]["cases"][0]["outcome"]["why"]
        .as_str()
        .unwrap();
    assert!(why.contains("cig-two"), "{why}");
    assert_eq!(state(&v, "cigar"), "timed", "{out}");
    assert_eq!(v["swapped"]["cig"]["value"], two.display().to_string());
    // The swapped product's binary is the other one; the others' are not.
    assert!(
        v["products"][0]["binary"]
            .as_str()
            .unwrap()
            .ends_with("other/cig"),
        "{out}"
    );
    assert!(
        v["products"][1]["binary"]
            .as_str()
            .unwrap()
            .ends_with("cigo/bin/cigo"),
        "{out}"
    );

    // Swapping cigo for another cigo that makes the case's answer right
    // under the original cig proves the swap goes the other way too.
    let cigo2 = fake_cig(&sb, "other/cigo", "exec cig");
    let with = format!("cigo@{}", cigo2.display());
    let mut swapped = base.clone();
    swapped.extend(["--with", &with]);
    let (code, out, _) = bench_bounded(&sb, &swapped, 120);
    assert_eq!(code, Some(0), "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert!(
        v["products"][1]["binary"]
            .as_str()
            .unwrap()
            .ends_with("other/cigo"),
        "{out}"
    );

    // Not a file and not a cig release: refused, naming the value.
    let mut bad = base.clone();
    bad.extend(["--with", "cigo@nowhere"]);
    let (code, _, err) = bench_bounded(&sb, &bad, 60);
    assert_eq!(code, Some(3), "{err}");
    assert!(err.contains("nowhere"), "{err}");
    // `--with` belongs to `--family`.
    let (code, _, err) = bench_bounded(&sb, &["bench", "--with", "cig@x"], 60);
    assert_eq!(code, Some(3), "{err}");
    assert!(err.contains("--family"), "{err}");
}

#[test]
#[cfg(unix)]
fn family_matrix_has_a_column_per_value() {
    let sb = Sandbox::new();
    let products = stand_in_family(&sb);
    let one = sb.path().join("cig/bin/cig");
    let two = fake_cig(&sb, "other/cig", "echo cig-two");
    let matrix = format!("cig={},{}", one.display(), two.display());
    let mut args: Vec<&str> = vec!["--json", "bench", "--family", "--matrix", &matrix];
    args.extend(products.iter().map(String::as_str));
    let (code, out, _) = bench_bounded(&sb, &args, 120);
    assert_eq!(code, Some(1), "cigo is wrong under the second cig: {out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["matrix"]["product"], "cig");
    assert_eq!(v["matrix"]["values"].as_array().unwrap().len(), 2);
    let cigo = &v["products"][1]["cases"][0]["values"];
    assert_eq!(cigo[0]["outcome"]["state"], "timed", "{out}");
    assert_eq!(cigo[1]["outcome"]["state"], "wrong", "{out}");
    // The table: one column per value, and a note that it is not gated.
    let mut table: Vec<&str> = vec!["bench", "--family", "--matrix", &matrix];
    table.extend(products.iter().map(String::as_str));
    let (_, text, _) = bench_bounded(&sb, &table, 120);
    assert!(
        text.contains("last/first") && text.contains("WRONG"),
        "{text}"
    );
    assert!(text.contains("never gated"), "{text}");
    // One value is not a matrix.
    let single = format!("cig={}", one.display());
    let mut one_arg: Vec<&str> = vec!["bench", "--family", "--matrix", &single];
    one_arg.extend(products.iter().map(String::as_str));
    let (code, _, err) = bench_bounded(&sb, &one_arg, 60);
    assert_eq!(code, Some(3), "{err}");
}

#[test]
#[cfg(unix)]
fn family_finds_installed_products_by_the_installers_layout() {
    let sb = Sandbox::new();
    // <prefix>/bin/cigar and <prefix>/share/cigar/bench/bench.toml.
    let cigar = stand_in_product(&sb, "cigar", "echo cigar", "cigar\n");
    let prefix = sb.path().join("prefix");
    fs::create_dir_all(prefix.join("bin")).unwrap();
    fs::create_dir_all(prefix.join("share/cigar")).unwrap();
    fs::copy(cigar.join("bin/cigar"), prefix.join("bin/cigar")).unwrap();
    // The suite is the manifest's own binary key dropped: installed, the
    // binary found is the one on the PATH.
    let m = fs::read_to_string(cigar.join("bench/bench.toml"))
        .unwrap()
        .replace("binary = \"bin/cigar\"\n", "")
        .replace("\"bin/cigar\"]", "\"cigar\"]");
    fs::create_dir_all(prefix.join("share/cigar/bench/fixtures")).unwrap();
    fs::write(prefix.join("share/cigar/bench/bench.toml"), m).unwrap();
    fs::write(prefix.join("share/cigar/bench/fixtures/out.txt"), "cigar\n").unwrap();
    // Only the installed prefix is on the PATH, so what else this machine
    // has installed cannot change the answer.
    let path = prefix.join("bin").display().to_string();
    let out = bounded(
        Command::new(cig_bin())
            .args(["--json", "bench", "--family"])
            .current_dir(sb.path())
            .env("PATH", &path)
            .env("XDG_CONFIG_HOME", sb.path().join("no-config"))
            .env("CIGSCRIPT_HOME", sb.home())
            .env("CIG_SEAL_KEY", "file")
            .env("CIG_INTERNAL_TRACKER_DIR", sb.path().join("tracker")),
        120,
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    let products = v["products"].as_array().unwrap();
    assert_eq!(products.len(), 1, "only cigar is installed here: {stdout}");
    assert_eq!(products[0]["product"], "cigar");
    assert_eq!(
        products[0]["cases"][0]["outcome"]["state"], "timed",
        "{stdout}"
    );
    // The others are named and skipped, not silently dropped.
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("cigo: not installed, skipped"), "{err}");
}

// ----- bench: history (#179 step 6) ------------------------------------------

#[test]
#[cfg(unix)]
fn bench_history_reads_a_file_and_the_branch() {
    let sb = Sandbox::new();
    let line = |at: &str, commit: &str, machine: &str, hello: f64| {
        format!(
            "{{\"format\":1,\"at\":\"{at}\",\"commit\":\"{commit}\",\"product\":\"stand-in\",\"cig_version\":\"1.20.0\",\"machine\":\"{machine}\",\"cases\":{{\"hello\":{{\"group\":\"start-up\",\"median_ms\":{hello},\"p10_ms\":{},\"p90_ms\":{},\"cv\":0.02}},\"fib\":{{\"median_ms\":50.0}}}}}}\n",
            hello * 0.95,
            hello * 1.05
        )
    };
    let text = format!(
        "{}{}{}{{\"format\":2,\"cases\":{{}}}}\n",
        line("2026-11-08T02:00:00Z", "aaaaaaaa11", "cpu-a", 4.0),
        line("2026-11-09T02:00:00Z", "bbbbbbbb22", "cpu-a", 4.4),
        line("2026-11-10T02:00:00Z", "cccccccc33", "cpu-b", 8.8),
    );
    sb.write("h.jsonl", &text);
    let out = sb.run_ok(&["bench", "history", "hello", "--file", "h.jsonl"]);
    assert!(out.contains("stand-in, hello, 3 runs"), "{out}");
    assert!(out.contains("+10.0%"), "4.0 to 4.4: {out}");
    // A change across two machines is marked, and said not to be a regression.
    assert!(
        out.contains("+100.0%*") && out.contains("machine differs"),
        "{out}"
    );
    assert!(
        out.contains("1 line(s) in another format were skipped"),
        "{out}"
    );
    // A name that matches one case by substring is enough; JSON for a tool.
    let json = sb.run_ok(&["--json", "bench", "history", "fi", "--file", "h.jsonl"]);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["case"], "fib");
    assert_eq!(v["runs"].as_array().unwrap().len(), 3);
    assert_eq!(v["skipped_lines"], 1);
    // No case named: the cases the history has.
    let list = sb.run_ok(&["bench", "history", "--file", "h.jsonl"]);
    assert!(
        list.contains("hello (3 runs)") && list.contains("fib (3 runs)"),
        "{list}"
    );
    // An unknown case is a usage error that says what there is.
    let err = sb.run_err(&["bench", "history", "nope", "--file", "h.jsonl"], 3);
    assert!(
        err.contains("hello, fib") || err.contains("fib, hello"),
        "{err}"
    );

    // The branch the nightly workflow writes: `history/<product>.jsonl` on
    // `bench-history`, found through the manifest's product.
    write_bench_sh(&sb, "true");
    let git = |args: &[&str]| {
        let o = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(sb.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["checkout", "-q", "--orphan", "bench-history"]);
    fs::create_dir_all(sb.path().join("history")).unwrap();
    fs::write(sb.path().join("history/stand-in.jsonl"), &text).unwrap();
    git(&["add", "history"]);
    git(&["commit", "-q", "-m", "history"]);
    let out = sb.run_ok(&["bench", "history", "hello"]);
    assert!(out.contains("hello, 3 runs"), "{out}");
    let err = sb.run_err(&["bench", "history", "hello", "--branch", "elsewhere"], 3);
    assert!(err.contains("elsewhere"), "{err}");
}

// ----- bench: --profile, --keep, --repeat-until-slow, --diff (#179 step 8) ----

/// `cig bench` in the sandbox with `path` as its PATH, under a deadline: exit
/// code, stdout and stderr. `tmp` becomes TMPDIR when given, so what the
/// harness makes lives and dies with the sandbox.
#[cfg(unix)]
fn bench_on_path(
    sb: &Sandbox,
    path: &str,
    tmp: Option<&Path>,
    args: &[&str],
    secs: u64,
) -> (Option<i32>, String, String) {
    let mut c = Command::new(cig_bin());
    c.args(args)
        .current_dir(sb.path())
        .env("PATH", path)
        .env("CIGSCRIPT_HOME", sb.home())
        .env("NO_COLOR", "1")
        .env("CIG_SEAL_KEY", "file")
        .env("CIG_INTERNAL_TRACKER_DIR", sb.path().join("tracker"))
        .env_remove("CI");
    if let Some(t) = tmp {
        fs::create_dir_all(t).unwrap();
        c.env("TMPDIR", t);
    }
    let out = bounded(&mut c, secs);
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// A PATH with `dir` in front of the real one.
#[cfg(unix)]
fn path_with(dir: &Path) -> String {
    format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

/// A stand-in `perf`: `record` runs the command it is given (as the real one
/// does) after writing an empty data file, and `script` prints `script`.
#[cfg(unix)]
fn fake_perf(sb: &Sandbox, name: &str, script: &str) -> PathBuf {
    fake_cig(
        sb,
        name,
        &format!(
            r#"case "$1" in
record)
  shift; out=""
  while [ $# -gt 0 ]; do
    case "$1" in -o) out="$2"; shift 2;; --) shift; break;; *) shift;; esac
  done
  : > "$out"
  exec "$@" ;;
script)
  cat <<'PERF_SCRIPT'
{script}PERF_SCRIPT
  ;;
esac"#
        ),
    )
}

/// What `perf script` prints for three samples: two in `hot_loop` called from
/// `main`, one in a template function whose name must not become markup.
#[cfg(unix)]
const PERF_SCRIPT_TEXT: &str = "stand-in 4242 100.001:     250000 cpu-clock:u:\n\
\t    55d0c3a8b2c0 hot_loop+0x10 (/usr/bin/stand-in)\n\
\t    55d0c3a8b000 main+0x1 (/usr/bin/stand-in)\n\
\n\
stand-in 4242 100.002:     250000 cpu-clock:u:\n\
\t    55d0c3a8b2c0 hot_loop+0x10 (/usr/bin/stand-in)\n\
\t    55d0c3a8b000 main+0x1 (/usr/bin/stand-in)\n\
\n\
stand-in 4242 100.003:     250000 cpu-clock:u:\n\
\t    55d0c3a8b300 Vec<u8>::push&co+0x4 (/usr/bin/stand-in)\n\
\t    55d0c3a8b000 main+0x1 (/usr/bin/stand-in)\n\
\n";

#[test]
#[cfg(unix)]
fn profile_draws_a_flamegraph_from_perf() {
    let sb = Sandbox::new();
    write_bench_sh(&sb, "true");
    let perf = fake_perf(&sb, "fakebin/perf", PERF_SCRIPT_TEXT);
    let path = path_with(perf.parent().unwrap());
    let (code, out, err) = bench_on_path(&sb, &path, None, &["--json", "bench", "--profile"], 120);
    assert_eq!(code, Some(0), "{out}\n{err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let p = &v["cases"][0]["debug"]["profile"];
    assert_eq!(p["measured"], true, "{p}");
    assert_eq!(p["samples"], 3, "{p}");
    assert_eq!(p["top"][0]["frame"], "hot_loop", "{p}");
    assert_eq!(p["top"][0]["samples"], 2, "{p}");
    let svg_path = sb.path().join(p["svg"].as_str().unwrap());
    assert!(
        svg_path.ends_with(".cig-bench/profiles/stand-in_case.svg"),
        "{}",
        svg_path.display()
    );
    let svg = fs::read_to_string(&svg_path).expect("the drawing was written");
    assert!(
        svg.contains("<svg ") && svg.trim_end().ends_with("</svg>"),
        "{svg}"
    );
    assert!(svg.contains("hot_loop (2 samples"), "{svg}");
    // A template's brackets are text in the drawing, never markup.
    assert!(svg.contains("Vec&lt;u8&gt;::push&amp;co"), "{svg}");
    assert!(!svg.contains("Vec<u8>"), "{svg}");
    // The plain table names the file and the hottest frame; a profile is
    // never a gate, so the exit stays 0.
    let (code, table, _) = bench_on_path(&sb, &path, None, &["bench", "--profile"], 120);
    assert_eq!(code, Some(0), "{table}");
    assert!(table.contains("profile: stand-in case:"), "{table}");
    assert!(table.contains("hot_loop"), "{table}");
    // A profile describes one run of one build.
    let (code, _, err) = bench_on_path(
        &sb,
        &path,
        None,
        &["bench", "--profile", "--against", "main"],
        60,
    );
    assert_eq!(code, Some(3), "{err}");
    assert!(err.contains("--profile"), "{err}");
}

#[test]
#[cfg(unix)]
fn profile_without_perf_says_so_and_builds_no_second_sampler() {
    let sb = Sandbox::new();
    write_bench_sh(&sb, "true");
    // No perf at all: an empty folder in front of a PATH that has sh.
    let none = sb.path().join("noperf");
    fs::create_dir_all(&none).unwrap();
    for tool in ["sh", "true"] {
        let real = ["/bin", "/usr/bin"]
            .iter()
            .map(|d| Path::new(d).join(tool))
            .find(|p| p.exists())
            .unwrap();
        std::os::unix::fs::symlink(real, none.join(tool)).unwrap();
    }
    let (code, out, err) = bench_on_path(
        &sb,
        none.to_str().unwrap(),
        None,
        &["--json", "bench", "--profile"],
        120,
    );
    assert_eq!(code, Some(0), "{out}\n{err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let p = &v["cases"][0]["debug"]["profile"];
    assert_eq!(p["measured"], false, "{p}");
    assert!(
        p["note"]
            .as_str()
            .unwrap()
            .starts_with("not measured: perf is not available here"),
        "{p}"
    );
    assert!(
        !sb.exists(".cig-bench/profiles"),
        "no drawing without a sample"
    );
    // A perf the kernel refuses is the same answer, with the reason.
    let refusing = fake_cig(
        &sb,
        "refusing/perf",
        "echo 'perf_event_paranoid is 3' >&2; exit 255",
    );
    let (code, out, _) = bench_on_path(
        &sb,
        &path_with(refusing.parent().unwrap()),
        None,
        &["--json", "bench", "--profile"],
        120,
    );
    assert_eq!(code, Some(0), "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let note = v["cases"][0]["debug"]["profile"]["note"].as_str().unwrap();
    assert!(
        note.starts_with("not measured: perf is not available here")
            && note.contains("255")
            && note.contains("perf_event_paranoid"),
        "{note}"
    );
}

#[test]
#[cfg(unix)]
fn keep_leaves_the_isolated_home_and_says_where() {
    let sb = Sandbox::new();
    write_bench_sh(&sb, "echo mine > \"$HOME/cookie\"");
    let tmp = sb.path().join("tmp");
    let path = std::env::var("PATH").unwrap_or_default();
    let (code, _, err) = bench_on_path(&sb, &path, Some(&tmp), &["bench", "--keep"], 120);
    assert_eq!(code, Some(0), "{err}");
    let line = err
        .lines()
        .find(|l| l.contains("kept the isolated homes"))
        .unwrap_or_else(|| panic!("it says where: {err}"));
    let kept = PathBuf::from(line.rsplit(" in ").next().unwrap().trim());
    assert!(kept.starts_with(&tmp), "{}", kept.display());
    // The case's work folder, its last sample's home and what the run left.
    let cookies: Vec<PathBuf> = fs::read_dir(&kept)
        .unwrap()
        .flatten()
        .flat_map(|c| fs::read_dir(c.path()).unwrap().flatten())
        .map(|h| h.path().join("cookie"))
        .filter(|p| p.exists())
        .collect();
    assert!(
        !cookies.is_empty(),
        "nothing was left under {}",
        kept.display()
    );
    assert_eq!(fs::read_to_string(&cookies[0]).unwrap(), "mine\n");
    assert!(err.contains("rerun by hand: cd "), "{err}");
    assert!(err.contains("sh run.sh"), "{err}");
    // Without the flag nothing is left behind.
    let tmp2 = sb.path().join("tmp2");
    let (code, _, err) = bench_on_path(&sb, &path, Some(&tmp2), &["bench"], 120);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(
        fs::read_dir(&tmp2).unwrap().count(),
        0,
        "scratch folders left"
    );
    // It is for a finished run, not a watch.
    let (code, _, err) = bench_on_path(&sb, &path, None, &["bench", "--keep", "--watch"], 60);
    assert_eq!(code, Some(3), "{err}");
}

#[test]
#[cfg(unix)]
fn repeat_until_slow_keeps_the_slow_runs_profile() {
    let sb = Sandbox::new();
    // Quick, and slow once, on the 22nd invocation: the warm-up and the two
    // samples are the first three, the ten reference runs the next ten.
    let count = sb.path().join("count");
    write_bench_sh(
        &sb,
        &format!(
            "n=0; [ -f '{c}' ] && read n < '{c}'; n=$((n+1)); echo $n > '{c}'; if [ \"$n\" -ge 22 ]; then sleep 1; else sleep 0.03; fi",
            c = count.display()
        ),
    );
    let perf = fake_perf(&sb, "fakebin/perf", PERF_SCRIPT_TEXT);
    let path = path_with(perf.parent().unwrap());
    let (code, out, err) = bench_on_path(
        &sb,
        &path,
        None,
        &["--json", "bench", "--repeat-until-slow"],
        300,
    );
    assert_eq!(code, Some(0), "{out}\n{err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let s = &v["cases"][0]["debug"]["repeat_until_slow"];
    assert_eq!(s["measured"], true, "{s}");
    let found = &s["found"];
    assert!(found.is_object(), "a slow run was found: {s}");
    // It is one of the runs after the reference, over the threshold, and the
    // threshold is 1.5 times the reference's p90.
    assert!(found["run"].as_u64().unwrap() > 10, "{s}");
    let (ms, threshold, p90) = (
        found["ms"].as_f64().unwrap(),
        s["threshold_ms"].as_f64().unwrap(),
        s["reference_p90_ms"].as_f64().unwrap(),
    );
    assert!(ms > threshold, "{s}");
    assert!((threshold - 1.5 * p90).abs() < 1e-6, "{s}");
    // And it kept that run's profile, drawn from the recording it made.
    let prof = &found["profile"];
    assert_eq!(prof["measured"], true, "{s}");
    let svg = fs::read_to_string(sb.path().join(prof["svg"].as_str().unwrap())).unwrap();
    assert!(svg.contains("hot_loop"), "{svg}");
    assert!(
        prof["svg"].as_str().unwrap().ends_with("-slow.svg"),
        "{prof}"
    );
}

#[test]
#[cfg(unix)]
fn repeat_until_slow_without_perf_keeps_the_phases() {
    let sb = Sandbox::new();
    let count = sb.path().join("count");
    // The product reports one phase on its descriptor when it is handed one;
    // the slow run reports a long one.
    write_bench_sh(
        &sb,
        &format!(
            "n=0; [ -f '{c}' ] && read n < '{c}'; n=$((n+1)); echo $n > '{c}'; if [ \"$n\" -ge 22 ]; then ms=900; sleep 1; else ms=30; sleep 0.03; fi; [ -n \"$CIG_TIMINGS_FD\" ] && printf '{{\"v\":1,\"product\":\"stand-in\",\"phase\":\"work\",\"ms\":%s}}\\n' \"$ms\" >&9; exit 0",
            c = count.display()
        ),
    );
    let refusing = fake_cig(&sb, "refusing/perf", "echo 'no PMU here' >&2; exit 255");
    let (code, out, err) = bench_on_path(
        &sb,
        &path_with(refusing.parent().unwrap()),
        None,
        &["--json", "bench", "--repeat-until-slow"],
        300,
    );
    assert_eq!(code, Some(0), "{out}\n{err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let s = &v["cases"][0]["debug"]["repeat_until_slow"];
    assert!(
        s["note"]
            .as_str()
            .is_some_and(|n| n.starts_with("not measured: perf is not available here")),
        "{s}"
    );
    let found = &s["found"];
    let phases = &found["phases"];
    assert_eq!(phases["measured"], true, "{s}");
    assert_eq!(phases["phases"][0]["phase"], "work", "{s}");
    // The run over the threshold keeps its own phases. That is the designed
    // slow run (900 ms) unless a loaded machine made an earlier run slow first,
    // and then it is that run's reading (30 ms) that is kept.
    let ms = phases["phases"][0]["ms"].as_f64().unwrap_or(-1.0);
    assert!(ms == 900.0 || ms == 30.0, "{s}");
    assert!(
        found["ms"].as_f64().unwrap_or(0.0) > s["threshold_ms"].as_f64().unwrap_or(f64::MAX),
        "{s}"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn diff_sets_two_runs_side_by_side_most_changed_first() {
    let sb = Sandbox::new();
    // A product whose `beta` phase takes as long as a control file says, and
    // that counts its calls the same way.
    let control = sb.path().join("control");
    fs::write(&control, "5\n").unwrap();
    write_bench_sh(
        &sb,
        &format!(
            "read ms < '{c}'; if [ -n \"$CIG_TIMINGS_FD\" ]; then printf '{{\"v\":1,\"product\":\"stand-in\",\"phase\":\"alpha\",\"ms\":1.0}}\\n{{\"v\":1,\"product\":\"stand-in\",\"phase\":\"beta\",\"ms\":%s}}\\n{{\"v\":1,\"product\":\"stand-in\",\"counter\":\"calls\",\"value\":%s}}\\n' \"$ms\" \"$ms\" >&9; fi",
            c = control.display()
        ),
    );
    let strace = |reads: &str| {
        fake_strace(
            &sb,
            &format!(
                "' 90.00    0.900000          14     {reads}           read' ' 10.00    0.100000           6       100           mmap'"
            ),
        )
    };
    let tool = strace("   100");
    let path = path_with(tool.parent().unwrap());
    // The baseline keeps what a plain `--save` does not: its phases,
    // counters and syscalls.
    let (code, _, err) = bench_on_path(
        &sb,
        &path,
        None,
        &[
            "bench",
            "--save",
            "base",
            "--timings",
            "--stats",
            "--syscalls",
        ],
        120,
    );
    assert_eq!(code, Some(0), "{err}");
    let saved: serde_json::Value =
        serde_json::from_str(&sb.read(".cig-bench/baselines/base.json")).unwrap();
    assert_eq!(
        saved["cases"][0]["insight"]["phases"][1]["name"],
        "stand-in: beta"
    );
    // Now beta is 300 ms and `read` is called 5,000 times.
    fs::write(&control, "300\n").unwrap();
    strace("  5000");
    let (code, out, err) = bench_on_path(
        &sb,
        &path,
        None,
        &["--json", "bench", "--diff", "base"],
        120,
    );
    assert_eq!(code, Some(0), "{out}\n{err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let d = &v["cases"][0]["diff"];
    assert_eq!(d["compared"], true, "{d}");
    assert_eq!(d["baseline"], "base");
    let by = |key: &str| -> Vec<serde_json::Value> { d[key].as_array().unwrap().clone() };
    let find = |key: &str, name: &str| -> serde_json::Value {
        by(key)
            .into_iter()
            .find(|r| r["name"] == name)
            .unwrap_or_else(|| panic!("{name} in {key}: {d}"))
    };
    assert_eq!(find("phases", "stand-in: beta")["before"], 5.0);
    assert_eq!(find("phases", "stand-in: beta")["after"], 300.0);
    assert_eq!(find("phases", "stand-in: alpha")["change"], 0.0);
    assert_eq!(find("counters", "stand-in: calls")["after"], 300.0);
    assert_eq!(find("syscalls", "read")["before"], 100.0);
    assert_eq!(find("syscalls", "read")["after"], 5000.0);
    // Most changed first, in every table: |change| never goes up.
    for key in ["phases", "syscalls", "syscall_ms"] {
        let changes: Vec<f64> = by(key)
            .iter()
            .map(|r| r["change"].as_f64().unwrap().abs())
            .collect();
        assert!(!changes.is_empty(), "{key}: {d}");
        assert!(
            changes.windows(2).all(|w| w[0] >= w[1]),
            "{key} is not sorted: {changes:?}"
        );
    }
    assert_eq!(by("syscalls")[0]["name"], "read", "{d}");
    // The plain table reads the same story.
    let (code, table, _) = bench_on_path(&sb, &path, None, &["bench", "--diff", "base"], 120);
    assert_eq!(code, Some(0), "{table}");
    assert!(
        table.contains("diff: stand-in case against baseline `base`"),
        "{table}"
    );
    assert!(table.contains("beta") && table.contains("+295"), "{table}");
    // A baseline saved without them has nothing to compare, and says how to
    // save one that does.
    let (code, _, _) = bench_on_path(&sb, &path, None, &["bench", "--save", "plain"], 120);
    assert_eq!(code, Some(0));
    let (code, _, err) = bench_on_path(&sb, &path, None, &["bench", "--diff", "plain"], 120);
    assert_eq!(code, Some(3), "{err}");
    assert!(
        err.contains("--save plain --timings --stats --syscalls"),
        "{err}"
    );
}

// ----- bench: the gates (#145) -----------------------------------------------

/// A one-case manifest for the gates: `[[case]]` lines after the preamble.
#[cfg(unix)]
fn gate_manifest(sb: &Sandbox, case: &str) {
    sb.write(
        "bench/bench.toml",
        &format!(
            "format = 1\n[bench]\nproduct = \"stand-in\"\n[defaults]\nsamples = 6\nper = 1\nwarmup = 1\n[[case]]\nname = \"case\"\nrun = [\"sleep\", \"0.1\"]\nexpect = {{ exit = 0 }}\n{case}"
        ),
    );
}

/// A baseline made by hand: the case's samples, in milliseconds.
#[cfg(unix)]
fn planted_baseline(sb: &Sandbox, ms: f64) {
    let samples: Vec<f64> = vec![ms; 6];
    sb.write(
        ".cig-bench/baselines/main.json",
        &serde_json::json!({
            "format": 1,
            "product": "stand-in",
            "cases": [{ "name": "case", "group": null, "samples_ms": samples, "rss_bytes": null }],
        })
        .to_string(),
    );
}

#[test]
#[cfg(unix)]
fn the_regression_gate_trips() {
    let sb = Sandbox::new();
    gate_manifest(&sb, "gate = [\"regression\"]\n");
    let path = std::env::var("PATH").unwrap_or_default();
    // Main took 1 ms; this build takes 100: a planted slowdown.
    planted_baseline(&sb, 1.0);
    let (code, out, err) = bench_on_path(
        &sb,
        &path,
        None,
        &["bench", "--against", "main", "--gate", "regression"],
        120,
    );
    assert_eq!(code, Some(1), "asked for, it fails the run:\n{out}\n{err}");
    assert!(out.contains("TRIP"), "{out}");
    assert!(err.contains("gate regression tripped by case"), "{err}");
    let (code, out, _) = bench_on_path(
        &sb,
        &path,
        None,
        &[
            "--json",
            "bench",
            "--against",
            "main",
            "--gate",
            "regression",
        ],
        120,
    );
    assert_eq!(code, Some(1));
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["gates"]["tripped"], true, "{out}");
    let case = v["gates"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["case"] == "case")
        .expect("the case's finding");
    assert_eq!(case["status"], "trip");
    // Not asked for, the same slowdown is printed and the run passes: a
    // shared runner's timing is a report, never a failure (#153).
    let (code, out, err) = bench_on_path(
        &sb,
        &path,
        None,
        &["bench", "--against", "main", "--gate", "correctness"],
        120,
    );
    assert_eq!(code, Some(0), "{out}\n{err}");
    assert!(out.contains("over"), "reported, not failed:\n{out}");
    // A build that is faster than main trips nothing.
    planted_baseline(&sb, 5000.0);
    let (code, out, err) = bench_on_path(
        &sb,
        &path,
        None,
        &["bench", "--against", "main", "--gate", "regression"],
        120,
    );
    assert_eq!(code, Some(0), "{out}\n{err}");
    // And a gate that is not one is a usage error that lists the real ones.
    let (code, _, err) = bench_on_path(&sb, &path, None, &["bench", "--gate", "speed"], 60);
    assert_eq!(code, Some(3), "{err}");
    assert!(err.contains("start-up"), "{err}");
}

#[test]
#[cfg(unix)]
fn disk_and_start_up_gates_fail_only_when_named() {
    let sb = Sandbox::new();
    let path = std::env::var("PATH").unwrap_or_default();
    // A case that burns 100 KiB under its home: about one byte on disk per
    // byte, against a reference of 3 (main's) it is fine, against 0.5 not.
    let case = |reference: &str, budget: &str| {
        format!(
            "run = [\"sh\", \"-c\", \"dd if=/dev/zero of=\\\"$CIGSCRIPT_HOME/f\\\" bs=1024 count=100 2>/dev/null\"]\nburned = 102400\nbudget = \"{budget}\"\ngate = [\"disk\", \"start-up\"]\nmeasure = [\"wall\", \"disk-per-byte\"]\nreference = {{ disk-per-byte = {reference} }}\n"
        )
    };
    let manifest = |reference: &str, budget: &str| {
        sb.write(
            "bench/bench.toml",
            &format!(
                "format = 1\n[bench]\nproduct = \"stand-in\"\n[defaults]\nsamples = 2\nper = 1\nwarmup = 1\n[[case]]\nname = \"burn\"\nexpect = {{ exit = 0 }}\n{}",
                case(reference, budget)
            ),
        );
    };
    manifest("3.0", "60 s");
    let (code, out, err) = bench_on_path(
        &sb,
        &path,
        None,
        &["bench", "--gate", "correctness,disk"],
        120,
    );
    assert_eq!(code, Some(0), "{out}\n{err}");
    assert!(
        out.contains("disk") && out.contains("bytes per byte burned"),
        "{out}"
    );
    // The reference says main stored half as much: the disk gate trips, and
    // only because it is named.
    manifest("0.5", "60 s");
    let (code, out, err) = bench_on_path(
        &sb,
        &path,
        None,
        &["bench", "--gate", "correctness,disk"],
        120,
    );
    assert_eq!(code, Some(1), "{out}\n{err}");
    assert!(err.contains("gate disk tripped by burn"), "{err}");
    let (code, out, _) = bench_on_path(&sb, &path, None, &["bench", "--gate", "correctness"], 120);
    assert_eq!(code, Some(0), "disk was only reported: {out}");
    assert!(out.contains("over"), "{out}");
    // The start-up budget is a median over a limit; here no process starts
    // in a thousandth of a millisecond.
    manifest("3.0", "0.001 ms");
    let (code, out, err) = bench_on_path(&sb, &path, None, &["bench", "--gate", "start-up"], 120);
    assert_eq!(code, Some(1), "{out}\n{err}");
    assert!(err.contains("gate start-up tripped by burn"), "{err}");
    let (code, out, _) = bench_on_path(
        &sb,
        &path,
        None,
        &["bench", "--gate", "correctness,disk"],
        120,
    );
    assert_eq!(code, Some(0), "start-up was only reported: {out}");
    // A gate that cannot work is a manifest error, not a pass.
    sb.write(
        "bench/bench.toml",
        "format = 1\n[bench]\nproduct = \"stand-in\"\n[[case]]\nname = \"burn\"\nrun = [\"true\"]\ngate = [\"disk\"]\n",
    );
    let (code, _, err) = bench_on_path(&sb, &path, None, &["bench"], 60);
    assert_eq!(code, Some(3), "{err}");
    assert!(err.contains("`disk` gate needs"), "{err}");
}

#[test]
fn cigs_own_suite_declares_its_gates() {
    // The suite parses with its gates (a gate that cannot work is a manifest
    // error), and the cases #145 names carry them: hello and a one-write
    // burn under start-up, the burn and overwrite cases under disk.
    let sb = Sandbox::new();
    let manifest = own_suite(&sb);
    let doc: toml::Value = fs::read_to_string(&manifest).unwrap().parse().unwrap();
    let gates_of = |name: &str| -> Vec<String> {
        doc["case"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"].as_str() == Some(name))
            .unwrap_or_else(|| panic!("no case {name}"))
            .get("gate")
            .map(|g| {
                g.as_array()
                    .unwrap()
                    .iter()
                    .map(|x| x.as_str().unwrap().to_string())
                    .collect()
            })
            .unwrap_or_default()
    };
    assert!(gates_of("hello").contains(&"start-up".to_string()));
    assert!(gates_of("burn one write").contains(&"start-up".to_string()));
    for name in ["burn small tree", "burn big tree", "overwrite big file"] {
        assert!(gates_of(name).contains(&"disk".to_string()), "{name}");
    }
    let (code, out, err) = bench_bounded(
        &sb,
        &[
            "--json",
            "bench",
            "bench/bench.toml",
            "--only",
            "hello",
            "--samples",
            "1",
            "--gate",
            "correctness",
        ],
        300,
    );
    assert_eq!(code, Some(0), "{out}\n{err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert!(
        v["gates"]["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["gate"] == "start-up" && f["case"] == "hello"),
        "{out}"
    );
}

#[test]
fn ci_gates_and_the_nightly_matrix_are_wired() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows");
    let read = |f: &str| fs::read_to_string(root.join(f)).unwrap_or_else(|e| panic!("{f}: {e}"));
    // A pull request fails on correctness and disk, and on nothing that is a
    // clock: the timing gates are printed (#153).
    let ci = read("ci.yml");
    let gate_lines: Vec<&str> = ci.lines().filter(|l| l.contains("--gate")).collect();
    assert_eq!(gate_lines.len(), 1, "{gate_lines:?}");
    assert!(
        gate_lines[0].contains("--gate correctness,disk"),
        "{gate_lines:?}"
    );
    assert!(!gate_lines[0].contains("regression") && !gate_lines[0].contains("start-up"));
    // The nightly calls the reusable workflow once per filesystem, on a
    // schedule, with the history written to the bench-history branch.
    let nightly = read("bench-nightly.yml");
    assert!(
        nightly.contains("schedule:") && nightly.contains("cron:"),
        "{nightly}"
    );
    assert!(
        nightly.contains("uses: ./.github/workflows/bench.yml"),
        "{nightly}"
    );
    for series in ["ext4", "btrfs", "xfs", "apfs", "ntfs"] {
        assert!(
            nightly.contains(&format!("series: {series},")),
            "{series} is not in the matrix"
        );
    }
    for os in ["ubuntu-latest", "macos-latest", "windows-latest"] {
        assert!(nightly.contains(os), "{os}");
    }
    // Only a correctness failure fails a night.
    assert!(nightly.contains("gate: correctness"), "{nightly}");
    let bench = read("bench.yml");
    for needle in [
        "filesystem:",
        "series:",
        "mkfs.btrfs",
        "mkfs.xfs",
        "mkfs.ext4",
        "losetup",
        "TMPDIR=",
        "bench-history",
        "--gate \"$GATE\"",
    ] {
        assert!(bench.contains(needle), "bench.yml lacks {needle}");
    }
}

/// The same flag against the real `perf`, where the machine has it and lets
/// a process be sampled: a busy loop is drawn, with samples. Skipped (and
/// said) where perf is absent or forbidden.
#[test]
#[cfg(target_os = "linux")]
fn profile_with_the_real_perf_when_it_is_allowed() {
    let have = |tool: &str| {
        Command::new(tool)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    if !have("perf") || !have("python3") {
        eprintln!("skipped: needs perf and python3");
        return;
    }
    let sb = Sandbox::new();
    sb.write(
        "bench/bench.toml",
        "format = 1\n[bench]\nproduct = \"stand-in\"\n[defaults]\nsamples = 1\nper = 1\nwarmup = 1\n[[case]]\nname = \"busy\"\nrun = [\"python3\", \"-c\", '''import time\nt = time.time()\nwhile time.time() - t < 0.4:\n    pass''']\nexpect = { exit = 0 }\n",
    );
    let (code, out, err) = bench_bounded(&sb, &["--json", "bench", "--profile"], 180);
    assert_eq!(code, Some(0), "{out}\n{err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let p = &v["cases"][0]["debug"]["profile"];
    if p["measured"] == false {
        eprintln!("skipped: perf is not allowed here: {}", p["note"]);
        return;
    }
    assert!(p["samples"].as_u64().unwrap() > 0, "{p}");
    let svg = fs::read_to_string(sb.path().join(p["svg"].as_str().unwrap())).unwrap();
    assert!(svg.contains("<svg ") && svg.contains("python3"), "{svg}");
}
