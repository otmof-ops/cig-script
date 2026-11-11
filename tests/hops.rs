// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Every resilience rule for hops (`docs/HOPS.md`, HAMMER.md section 4) has
//! a test here. The boundary is where the logic must be hardest, so this is
//! where it is checked hardest. Unix only: the children are `sh` scripts.

#![cfg(unix)]

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
    fn cig_env(&self, args: &[&str], envs: &[(&str, &str)]) -> Output {
        Command::new(cig_bin())
            .args(args)
            .current_dir(self.path())
            .env("CIGSCRIPT_HOME", self.path().join("home"))
            .env("NO_COLOR", "1")
            .env("CIG_SEAL_KEY", "file")
            .envs(envs.iter().copied())
            .output()
            .expect("run cig")
    }
    fn cig(&self, args: &[&str]) -> Output {
        self.cig_env(args, &[])
    }
    /// Run a script; return (exit code, stdout, stderr).
    fn run(&self, script: &str) -> (i32, String, String) {
        self.write("s.cig", script);
        let out = self.cig(&["run", "s.cig"]);
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }
}

fn burn(body: &str) -> String {
    format!("burn {{\n{body}\n}}\n")
}

#[test]
fn exit_codes_are_a_contract_not_a_boolean() {
    let sb = Sandbox::new();
    sb.write("hay.txt", "needle\n");
    // grep's 1 is an answer; proc.run alone passes any code through.
    let (code, out, _) = sb.run(&burn(
        "  stick r = proc.run(\"grep\", [\"nothing\", \"hay.txt\"])\n  exhale r.code",
    ));
    assert_eq!((code, out.trim()), (0, "1"));
    // A contract makes 1 an answer under the parsing verbs too.
    let (code, out, _) = sb.run(&burn(
        "  exhale proc.lines(\"grep\", [\"nothing\", \"hay.txt\"], {ok: [0, 1]}).len()",
    ));
    assert_eq!((code, out.trim()), (0, "0"));
    // Outside the contract: a named diagnostic with the command, the code and the contract.
    let (code, _, err) = sb.run(&burn("  proc.lines(\"grep\", [\"nothing\", \"hay.txt\"])"));
    assert_eq!(code, 1);
    assert!(
        err.contains(
            "error[E553 runtime]: proc.lines: `grep nothing hay.txt` exited 1 (contract: 0)"
        ),
        "{err}"
    );
    assert!(
        err.contains("{ok: [0, 1]}"),
        "the hint says how to widen the contract: {err}"
    );
    // check: true is the contract [0]; the ashtray value carries the result.
    let (code, out, _) = sb.run(&burn(
        "  try {\n    proc.run(\"sh\", [\"-c\", \"echo boom 1>&2; exit 7\"], {check: true})\n  } ashtray e {\n    exhale e.code, e.err.trim(), e.message.contains(\"contract: 0\")\n  }",
    ));
    assert_eq!((code, out.trim()), (0, "7 boom true"));
}

#[test]
fn stdout_and_stderr_are_never_merged_and_shape_is_checked_at_the_hop() {
    let sb = Sandbox::new();
    // JSON on stdout, progress on stderr: parses.
    let (code, out, _) = sb.run(&burn(
        "  stick v = proc.json(\"sh\", [\"-c\", \"echo Downloading... 1>&2; echo '[{\\\"n\\\": 1}, {\\\"n\\\": 2}]'\"])\n  exhale v.len(), v[1].n",
    ));
    assert_eq!((code, out.trim()), (0, "2 2"));
    // The same progress on stdout: a named diagnostic quoting the first bytes, no partial value.
    let (code, out, err) = sb.run(&burn(
        "  stick v = proc.json(\"sh\", [\"-c\", \"echo Downloading...; echo '[1]'\"])\n  exhale \"reached\", v",
    ));
    assert_eq!(code, 1);
    assert!(!out.contains("reached"));
    assert!(
        err.contains(
            "error[E556 runtime]: proc.json: expected JSON on stdout, got `Downloading...⏎[1]`"
        ),
        "{err}"
    );
    // csv and kv have the same rule.
    let (code, out, _) = sb.run(&burn(
        "  stick rows = proc.csv(\"printf\", [\"a,b\\\\n1,2\\\\n3,4\\\\n\"])\n  exhale rows.len(), rows[1].b\n  stick m = proc.kv(\"printf\", [\"A=1\\\\nB=\\\"two\\\"\\\\n# c\\\\n\"])\n  exhale m.A, m.B, m.len()",
    ));
    assert_eq!((code, out.trim()), (0, "2 4\n1 two 2"));
    let (code, _, err) = sb.run(&burn("  proc.kv(\"printf\", [\"no equals here\\\\n\"])"));
    assert_eq!(code, 1);
    assert!(
        err.contains("E556") && err.contains("line 1 has no `=`"),
        "{err}"
    );
    // stderr is shown on failure, last line first.
    let (_, _, err) = sb.run(&burn(
        "  proc.text(\"sh\", [\"-c\", \"echo first 1>&2; echo forbidden: no 1>&2; exit 3\"])",
    ));
    assert!(
        err.contains("exited 3 (contract: 0); stderr: `forbidden: no`"),
        "{err}"
    );
}

#[test]
fn timeouts_always_exist_and_partial_output_is_never_mistaken_for_output() {
    let sb = Sandbox::new();
    let (code, out, err) = sb.run(&burn(
        "  stick r = proc.run(\"sh\", [\"-c\", \"echo partial; echo warn 1>&2; sleep 30\"], {timeout_ms: 300})\n  exhale \"reached\", r.out",
    ));
    assert_eq!(code, 1);
    assert!(
        !out.contains("reached"),
        "a timed-out hop produced nothing for the next step"
    );
    assert!(err.contains("error[E554 runtime]: proc.run: `sh -c echo partial; echo warn 1>&2; sleep 30` ran longer than 300 ms and was killed; it had written 8 bytes to stdout (`partial`) and 5 bytes to stderr (`warn`)"), "{err}");
    // A child killed by a signal is E555, not a code -1 result.
    let (code, out, err) = sb.run(&burn(
        "  stick r = proc.run(\"sh\", [\"-c\", \"echo half; kill -9 $$\"])\n  exhale \"reached\", r.code",
    ));
    assert_eq!(code, 1);
    assert!(!out.contains("reached"));
    assert!(err.contains("error[E555 runtime]: proc.run: `sh -c echo half; kill -9 $$` was killed by signal 9 (SIGKILL); it had written 5 bytes to stdout"), "{err}");
    // The plan and the record still list the hop as irreversible.
    let out = sb.cig(&["run", "--dry-run", "s.cig"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("irreversible  run sh -c"));
}

#[test]
fn large_output_on_either_stream_does_not_deadlock() {
    let sb = Sandbox::new();
    // 20 MB on stderr while stdout is awaited, then the reverse.
    let (code, out, _) = sb.run(&burn(
        "  stick r = proc.run(\"sh\", [\"-c\", \"yes | head -c 20000000 1>&2; echo done\"], {timeout_ms: 60000})\n  exhale r.out.trim(), r.err.len()\n  stick q = proc.run(\"sh\", [\"-c\", \"yes | head -c 20000000; echo done 1>&2\"], {timeout_ms: 60000})\n  exhale q.err.trim(), q.out.len()",
    ));
    assert_eq!(code, 0, "{out}");
    assert_eq!(out.trim(), "done 20000000\ndone 20000000");
}

#[test]
fn encoding_is_explicit_never_silently_lossy() {
    let sb = Sandbox::new();
    let (code, _, err) = sb.run(&burn("  proc.text(\"printf\", [\"ok\\\\377\\\\376\"])"));
    assert_eq!(code, 1);
    assert!(err.contains("error[E557 runtime]: proc.text: stdout is not valid UTF-8 at byte 2; the child wrote 4 bytes"), "{err}");
    assert!(err.contains("encoding: \"lossy\""), "{err}");
    let (code, out, _) = sb.run(&burn(
        "  exhale proc.text(\"printf\", [\"ok\\\\377\"], {encoding: \"lossy\"}).len()\n  exhale proc.text(\"printf\", [\"caf\\\\351\"], {encoding: \"latin1\"})\n  exhale proc.text(\"printf\", [\"\\\\377\\\\376h\\\\000i\\\\000\"], {encoding: \"utf-16\"})\n  exhale proc.text(\"printf\", [\"h\\\\000i\\\\000\"], {encoding: \"utf16\"})",
    ));
    assert_eq!((code, out.trim()), (0, "3\ncafé\nhi\nhi"));
}

fn ps_has(marker: &str) -> bool {
    // -ww: the BSDs and macOS truncate the line without it, and the marker
    // sits past the cut. On the BSDs `-e` asks for the environment, not for
    // every process, and only `-ax` lists a process without a terminal (the
    // child of a CI VM's ssh session), so the flags are not Linux's.
    #[cfg(target_os = "linux")]
    let args: &[&str] = &["-ww", "-eo", "args"];
    #[cfg(not(target_os = "linux"))]
    let args: &[&str] = &["-axww", "-o", "args"];
    let out = Command::new("ps").args(args).output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .lines()
            .any(|l| l.contains(marker) && !l.contains("-o args") && !l.contains("-eo args")),
        Err(_) => false,
    }
}

#[test]
fn a_timed_out_child_is_killed_with_its_whole_process_group() {
    let sb = Sandbox::new();
    // A grandchild in the background, and a parent that ignores SIGTERM.
    let marker = format!("sleep 31.{}", std::process::id());
    let script = burn(&format!(
        "  proc.run(\"sh\", [\"-c\", \"{marker} & trap '' TERM; {marker}\"], {{timeout_ms: 300, grace_ms: 200}})"
    ));
    let (code, _, err) = sb.run(&script);
    assert_eq!(code, 1);
    assert!(err.contains("E554"), "{err}");
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(
        !ps_has(&marker),
        "a child or grandchild outlived the timeout: `{marker}` is still running"
    );
}

#[test]
fn environment_is_deliberate() {
    let sb = Sandbox::new();
    let (code, out, _) = sb.run(&burn(
        "  stick e = proc.kv(\"env\", [])\n  exhale e.NO_COLOR, e.has(\"CIG_HOP_TEST\"), e.has(\"HOME\")\n  stick c = proc.kv(\"env\", [], {clean_env: true, env: {ADDED: \"yes\"}})\n  exhale c.has(\"CIG_HOP_TEST\"), c.has(\"PATH\"), c.ADDED",
    ));
    let _ = code;
    let out2 = sb.cig_env(&["run", "s.cig"], &[("CIG_HOP_TEST", "1")]);
    let text = String::from_utf8_lossy(&out2.stdout).to_string();
    assert!(
        out2.status.success(),
        "{}",
        String::from_utf8_lossy(&out2.stderr)
    );
    assert_eq!(text.trim(), "1 true true\nfalse true yes");
    let _ = out;
}

#[test]
fn no_shell_by_default_and_the_checker_warns_when_one_is_smuggled_in() {
    let sb = Sandbox::new();
    let (code, out, _) = sb.run(&burn("  exhale proc.text(\"echo\", [\"$HOME\", \"a b\"])"));
    assert_eq!((code, out.trim()), (0, "$HOME a b"));
    // A literal command string through sh -c is allowed quietly; a built one warns.
    sb.write(
        "lit.cig",
        "burn {\n  proc.run(\"sh\", [\"-c\", \"echo hi\"])\n}\n",
    );
    let out = sb.cig(&["check", "lit.cig"]);
    assert!(out.status.success());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("E308"));
    sb.write("built.cig", "stick user_input = args[0]\nburn {\n  proc.run(\"bash\", [\"-c\", \"ls ${user_input}\"])\n}\n");
    let out = sb.cig(&["check", "built.cig"]);
    assert!(out.status.success(), "a warning, not an error");
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        err.contains("warning[E308 check]") && err.contains("proc.shell"),
        "{err}"
    );
    // proc.shell exists for the honest cases and is one hop in the plan.
    let (code, out, _) = sb.run(&burn(
        "  exhale proc.shell(\"echo a b | tr a-z A-Z\").out.trim()",
    ));
    assert_eq!((code, out.trim()), (0, "A B"));
}

#[test]
fn pipe_connects_stages_without_a_shell_and_names_the_failing_stage() {
    let sb = Sandbox::new();
    sb.write("big.log", "INFO one\nERROR two\nINFO three\nERROR four\n");
    let (code, out, _) = sb.run(&burn(
        "  stick r = proc.pipe([[\"cat\", \"big.log\"], [\"grep\", \"ERROR\"], [\"sort\", \"-r\"]])\n  exhale r.out.trim(), r.stages.len(), r.stages[1].code",
    ));
    assert_eq!((code, out.trim()), (0, "ERROR two\nERROR four 3 0"));
    // grep with no match ends the pipeline with a named stage, unless the last stage's contract allows it.
    let (code, _, err) = sb.run(&burn(
        "  proc.pipe([[\"cat\", \"big.log\"], [\"grep\", \"NOPE\"], [\"sort\"]])",
    ));
    assert_eq!(code, 1);
    assert!(
        err.contains(
            "error[E553 runtime]: proc.pipe: stage 2 (`grep NOPE`) exited 1 (contract: 0)"
        ),
        "{err}"
    );
    let (code, out, _) = sb.run(&burn(
        "  exhale proc.pipe([[\"cat\", \"big.log\"], [\"grep\", \"NOPE\"]], {ok: [0, 1]}).code",
    ));
    assert_eq!((code, out.trim()), (0, "1"));
    // stdin feeds the first stage; a missing program names its stage.
    let (code, out, _) = sb.run(&burn(
        "  exhale proc.pipe([[\"tr\", \"a-z\", \"A-Z\"]], {stdin: \"hi\"}).out",
    ));
    assert_eq!((code, out.trim()), (0, "HI"));
    let (code, _, err) = sb.run(&burn(
        "  proc.pipe([[\"cat\", \"big.log\"], [\"definitely-not-a-program-xyz\"]])",
    ));
    assert_eq!(code, 1);
    assert!(err.contains("error[E551 runtime]: proc.pipe (stage 2): could not start `definitely-not-a-program-xyz`"), "{err}");
    // The plan shows one irreversible hop for the pipeline.
    let out = sb.cig(&["run", "--dry-run", "s.cig"]);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("irreversible  run pipe:"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn failure_taxonomy_has_distinct_codes() {
    let sb = Sandbox::new();
    let (_, _, err) = sb.run(&burn("  proc.run(\"definitely-not-a-program-xyz\", [])"));
    assert!(err.contains("error[E551 runtime]: proc.run: could not start `definitely-not-a-program-xyz`: no such file or directory"), "{err}");
    assert!(
        err.contains("= doctor: I think the program is not on PATH for this run"),
        "{err}"
    );
    let script = sb.write("noexec.sh", "#!/bin/sh\necho hi\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o644)).unwrap();
    }
    let (_, _, err) = sb.run(&burn("  proc.run(\"./noexec.sh\", [])"));
    assert!(
        err.contains(
            "error[E552 runtime]: proc.run: could not start `./noexec.sh`: permission denied"
        ),
        "{err}"
    );
    assert!(err.contains("chmod +x"), "{err}");
    for code in [
        "E550", "E551", "E552", "E553", "E554", "E555", "E556", "E557",
    ] {
        let out = sb.cig(&["explain", code]);
        assert!(out.status.success(), "{code} has no page");
    }
    // E603 is retired, and says so.
    let out = sb.cig(&["explain", "E603"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("retired"));
}

#[test]
fn retries_are_per_hop_and_idempotence_aware() {
    let sb = Sandbox::new();
    // A step whose failed attempt only burned files is retried against the
    // rolled-back state: the second attempt sees the original file. Whether
    // an attempt fails is decided by script state, which rollback does not
    // touch; the disk is put back between attempts.
    sb.write("counter.txt", "0");
    sb.write(
        "retry.cig",
        concat!(
            "roll tries = 0\n",
            "stick flaky = pack() {\n",
            "  tries += 1\n",
            "  burn {\n",
            "    stick n = int(fs.read_text(\"counter.txt\"))\n",
            "    fs.write_text(\"counter.txt\", str(n + 1))\n",
            "    if tries == 1 {\n",
            "      cough \"first attempt fails\"\n",
            "    }\n",
            "    snuff fs.read_text(\"counter.txt\")\n",
            "  }\n",
            "}\n",
            "chain go { flaky }\n",
            "stick report = light(go, {retries: 2})\n",
            "exhale report.ok, report.steps[0].retries, report.result\n"
        ),
    );
    let out = sb.cig(&["run", "retry.cig"]);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{stderr}");
    assert_eq!(
        stdout.trim(),
        "true 1 1",
        "the retry saw counter.txt as 0 again: {stderr}"
    );
    assert!(
        stderr.contains("rolled back 1 op before the retry"),
        "{stderr}"
    );
    assert_eq!(
        fs::read_to_string(sb.path().join("counter.txt")).unwrap(),
        "1"
    );
    // The run's journal shows the first attempt as undone, and unburn skips it.
    let runs = sb.cig(&["--json", "runs"]);
    let v: Vec<serde_json::Value> = serde_json::from_slice(&runs.stdout).unwrap();
    let id = v[0]["id"].as_str().unwrap().to_string();
    assert_eq!(v[0]["burns"], 1, "undone ops are not counted: {}", v[0]);
    let shown = String::from_utf8_lossy(&sb.cig(&["runs", &id]).stdout).to_string();
    assert!(shown.contains("(undone by a retry)"), "{shown}");
    assert!(sb.cig(&["unburn", &id]).status.success());
    assert_eq!(
        fs::read_to_string(sb.path().join("counter.txt")).unwrap(),
        "0"
    );

    // A step that ran a hop is not retried on its own.
    sb.write(
        "hop.cig",
        concat!(
            "stick pushes = pack() {\n",
            "  burn {\n",
            "    proc.run(\"true\", [])\n",
            "    cough \"after the hop\"\n",
            "  }\n",
            "}\n",
            "chain go { pushes }\n",
            "try {\n",
            "  light(go, {retries: 3})\n",
            "} ashtray e {\n",
            "  exhale e.steps[0].retries, e.cause.message\n",
            "}\n"
        ),
    );
    let out = sb.cig(&["run", "hop.cig"]);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{stderr}");
    assert_eq!(stdout.trim(), "0 after the hop");
    assert!(
        stderr.contains("not retried: it ran a hop the kernel cannot undo"),
        "{stderr}"
    );
    // ... unless told to.
    sb.write(
        "force.cig",
        &fs::read_to_string(sb.path().join("hop.cig"))
            .unwrap()
            .replace("{retries: 3}", "{retries: 2, retry_irreversible: true}"),
    );
    let out = sb.cig(&["run", "force.cig"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "2 after the hop"
    );
}

#[test]
fn large_stdin_does_not_deadlock() {
    let sb = Sandbox::new();
    // 8 MB in through stdin while the child echoes it straight back: writing
    // all of stdin before reading stdout is the classic hang.
    let (code, out, err) = sb.run(&burn(
        "  stick big = \"x\".repeat(8000000)\n  stick r = proc.text(\"cat\", [], {stdin: big, timeout_ms: 60000})\n  exhale r.len()",
    ));
    assert_eq!((code, out.trim()), (0, "8000000"), "{err}");
}

#[test]
fn a_missing_cwd_is_e550_naming_the_directory_not_the_command() {
    let sb = Sandbox::new();
    let (code, _, err) = sb.run(&burn("  proc.run(\"true\", [], {cwd: \"nope/here\"})"));
    assert_eq!(code, 1);
    assert!(
        err.contains(
            "error[E550 runtime]: proc.run: the working directory `nope/here` does not exist"
        ),
        "{err}"
    );
    assert!(!err.contains("E551"), "blamed the command: {err}");
    sb.write("flat", "");
    let (code, _, err) = sb.run(&burn("  proc.text(\"true\", [], {cwd: \"flat\"})"));
    assert_eq!(code, 1);
    assert!(err.contains("`flat` is not a directory"), "{err}");
}

#[test]
fn an_upstream_stage_closed_early_by_its_consumer_is_not_a_failure() {
    let sb = Sandbox::new();
    // `yes | head -1`: yes dies of SIGPIPE because head stopped reading.
    let (code, out, err) = sb.run(&burn(
        "  stick r = proc.pipe([[\"yes\"], [\"head\", \"-1\"]], {timeout_ms: 60000})\n  exhale r.out.trim(), r.stages[0].closed_early, r.stages[0].signal, r.code",
    ));
    assert_eq!((code, out.trim()), (0, "y true 13 0"), "{err}");
    // The consumer's own failure is still the failure, named at its stage.
    let (code, _, err) = sb.run(&burn(
        "  proc.pipe([[\"yes\"], [\"sh\", \"-c\", \"head -1 >/dev/null; exit 3\"]])",
    ));
    assert_eq!(code, 1);
    assert!(
        err.contains("error[E553 runtime]: proc.pipe: stage 2") && err.contains("exited 3"),
        "{err}"
    );
}

#[test]
fn a_signal_to_cig_reaches_the_child_process_group() {
    let sb = Sandbox::new();
    // The child sits in its own process group, where a terminal's Ctrl-C
    // would never reach it.
    let marker = format!("sleep 32.{}", std::process::id());
    sb.write(
        "s.cig",
        &burn(&format!("  proc.run(\"sh\", [\"-c\", \"{marker}\"])")),
    );
    let mut child = Command::new(cig_bin())
        .args(["run", "s.cig"])
        .current_dir(sb.path())
        .env("CIGSCRIPT_HOME", sb.path().join("home"))
        .env("NO_COLOR", "1")
        .env("CIG_SEAL_KEY", "file")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn cig");
    let mut running = false;
    for _ in 0..100 {
        if ps_has(&marker) {
            running = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(running, "the child never started");
    let pid = child.id().to_string();
    assert!(Command::new("kill")
        .args(["-INT", &pid])
        .status()
        .expect("kill")
        .success());
    let status = child.wait().expect("wait");
    assert!(!status.success(), "cig should have died of the signal");
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(
        !ps_has(&marker),
        "the child outlived cig's SIGINT: `{marker}` is still running"
    );
}

#[test]
fn ok_and_check_resolve_the_same_whatever_their_order() {
    let sb = Sandbox::new();
    for opts in [
        "{ok: [0], check: false}",
        "{check: false, ok: [0]}",
        "{ok: [0], check: true}",
        "{check: true, ok: [0]}",
    ] {
        let (code, _, err) = sb.run(&burn(&format!(
            "  proc.run(\"sh\", [\"-c\", \"exit 5\"], {opts})"
        )));
        assert_eq!(code, 1, "{opts}: {err}");
        assert!(err.contains("exited 5 (contract: 0)"), "{opts}: {err}");
    }
    for opts in ["{check: true, ok: [1]}", "{ok: [1], check: true}"] {
        let (code, out, err) = sb.run(&burn(&format!(
            "  exhale proc.run(\"sh\", [\"-c\", \"exit 1\"], {opts}).code"
        )));
        assert_eq!((code, out.trim()), (0, "1"), "{opts}: {err}");
    }
    let (code, _, err) = sb.run(&burn("  proc.run(\"true\", [], {ok: []})"));
    assert_eq!(code, 1);
    assert!(err.contains("the `ok` contract is empty"), "{err}");
    let (code, _, err) = sb.run(&burn(
        "  proc.csv(\"printf\", [\"a,b\\\\n1,2\\\\n\"], {sep: \"\"})",
    ));
    assert_eq!(code, 1);
    assert!(err.contains("`sep` must be one character"), "{err}");
}

#[test]
fn a_directory_as_the_command_export_lines_and_a_path_to_which() {
    let sb = Sandbox::new();
    fs::create_dir(sb.path().join("adir")).unwrap();
    let (code, _, err) = sb.run(&burn("  proc.run(\"./adir\", [])"));
    assert_eq!(code, 1);
    assert!(
        err.contains("error[E552 runtime]: proc.run: could not start `./adir`: it is a directory, not a program"),
        "{err}"
    );
    let bin = sb.write("bin/ls", "#!/bin/sh\necho local\n");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    let (code, out, err) = sb.run(&burn(
        "  stick m = proc.kv(\"printf\", [\"export FOO=bar\\\\nBAZ=1\\\\n\"])\n  exhale m.has(\"FOO\"), m.FOO, m.BAZ, proc.which(\"bin/ls\"), proc.which(\"nope/x\")",
    ));
    assert_eq!((code, out.trim()), (0, "true bar 1 bin/ls null"), "{err}");
}

// ----- 1.4.0: the timeout covers the hop's output, not only its child -----------

/// A process the child leaves behind holding the pipe does not hold the hop
/// past its timeout: at the deadline the group is ended and the hop has run
/// too long.
#[test]
fn a_background_child_holding_the_pipe_does_not_outlive_the_timeout() {
    let sb = Sandbox::new();
    let marker = format!("sleep 4.{}", std::process::id());
    let started = std::time::Instant::now();
    let (code, _, err) = sb.run(&burn(&format!(
        "  proc.run(\"sh\", [\"-c\", \"{marker} & echo parent-done\"], {{timeout_ms: 700, grace_ms: 100}})"
    )));
    let wall = started.elapsed();
    assert_eq!(code, 1, "{err}");
    // The background sleep inherited both streams.
    assert!(
        err.contains("E554") && err.contains("kept its stdout and stderr open"),
        "{err}"
    );
    assert!(
        wall < std::time::Duration::from_secs(3),
        "the hop waited for the pipe: {wall:?}"
    );
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(!ps_has(&marker), "`{marker}` outlived the hop");
}

/// A grandchild that left the process group cannot be killed with it; the
/// hop still ends at its deadline, with what was read so far reported.
#[test]
fn a_grandchild_outside_the_group_does_not_hang_the_hop() {
    let sb = Sandbox::new();
    let marker = format!("sleep 59.{}", std::process::id());
    let started = std::time::Instant::now();
    let (code, _, err) = sb.run(&burn(&format!(
        "  proc.run(\"sh\", [\"-c\", \"setsid {marker} & sleep 60\"], {{timeout_ms: 500, grace_ms: 100}})"
    )));
    let wall = started.elapsed();
    let _ = Command::new("pkill").args(["-f", &marker]).output();
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("E554"), "{err}");
    assert!(
        wall < std::time::Duration::from_secs(6),
        "the hop hung on the pipe: {wall:?}"
    );
}

/// Feeding `stdin` to a first stage that never reads it does not stop the
/// pipeline's clock.
#[test]
fn pipe_stdin_write_does_not_disable_the_timeout() {
    let sb = Sandbox::new();
    let started = std::time::Instant::now();
    let (code, _, err) = sb.run(
        "stick big = \"x\" * 300000\nburn {\n  proc.pipe([[\"sleep\", \"6\"], [\"cat\"]], {stdin: big, timeout_ms: 1000})\n}\n",
    );
    let wall = started.elapsed();
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("E554"), "{err}");
    assert!(
        wall < std::time::Duration::from_secs(4),
        "the write blocked the timeout: {wall:?}"
    );
}

#[test]
fn stream_shows_a_hops_output_as_it_arrives_and_still_captures_it_whole() {
    use std::io::{BufRead, BufReader};
    let sb = Sandbox::new();
    // Shown on the matching stream, then the value is the same as without it.
    let (code, out, err) = sb.run(&burn(
        "  stick r = proc.run(\"sh\", [\"-c\", \"echo early; echo warned 1>&2; echo late\"], {stream: true})\n  exhale \"got:\", r.out.trim().replace(\"\\n\", \"|\"), r.err.trim()",
    ));
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("early\nlate\n"), "{out}");
    assert!(out.contains("got: early|late warned"), "{out}");
    assert!(err.contains("warned\n"), "{err}");
    // proc.shell takes it too; without it nothing is shown.
    let (code, out, _) = sb.run(&burn(
        "  stick a = proc.shell(\"echo shown\", {stream: true})\n  stick b = proc.shell(\"echo kept\")\n  exhale a.out.trim(), b.out.trim()",
    ));
    assert_eq!((code, out.as_str()), (0, "shown\nshown kept\n"));
    // Live: the first line is on cig's stdout while the child still runs.
    sb.write(
        "live.cig",
        &burn("  proc.run(\"sh\", [\"-c\", \"echo early; sleep 3; echo late\"], {stream: true})"),
    );
    let mut child = Command::new(cig_bin())
        .args(["run", "live.cig"])
        .current_dir(sb.path())
        .env("CIGSCRIPT_HOME", sb.path().join("home"))
        .env("NO_COLOR", "1")
        .env("CIG_SEAL_KEY", "file")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("run cig");
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let first = lines.next().expect("a line").expect("text");
    assert_eq!(first, "early");
    assert!(
        child.try_wait().expect("wait").is_none(),
        "the first line came only after the hop ended"
    );
    assert_eq!(lines.next().expect("a line").expect("text"), "late");
    assert!(child.wait().expect("wait").success());
}

#[test]
fn a_hop_that_only_reads_is_not_journaled_but_what_it_writes_is() {
    let sb = Sandbox::new();
    // Only reads: nothing journaled, so no burn line.
    let (code, _, err) = sb.run(&burn("  proc.run(\"true\", [], {reads: true})"));
    assert_eq!(code, 0, "{err}");
    assert!(!err.contains("burned"), "{err}");
    // Said it only reads, and wrote in the pack: the watch still journals it.
    let (code, _, err) = sb.run(&format!(
        "pack {{ \".\" }}\n{}",
        burn("  proc.run(\"sh\", [\"-c\", \"echo x > made.txt\"], {reads: true})")
    ));
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("burned 1 op"), "{err}");
    // It takes no box or touches.
    let (code, _, err) = sb.run(&burn("  proc.run(\"true\", [], {reads: true, box: true})"));
    assert_eq!(code, 1);
    assert!(err.contains("only reads"), "{err}");
}
