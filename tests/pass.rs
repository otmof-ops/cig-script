// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! End-to-end tests for passes, against the built `cig` binary: the `pass`
//! module in scripts and `cig pass ...`. The same sandbox as `tests/cli.rs`:
//! its own `CIGSCRIPT_HOME`, `CIG_SEAL_KEY=file` (the developer's keyring
//! and keys are never touched), no network, no box. Each script runs from a
//! file of its own, since a pass records the script it was issued from.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn cig_bin() -> PathBuf {
    std::env::var_os("CIG_TEST_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_cig")))
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

    fn exists(&self, name: &str) -> bool {
        self.path().join(name).exists()
    }

    /// The sandbox's path as the scripts spell it: absolute (a pass records
    /// paths as the work used them), written as the inside of a string
    /// literal, so a Windows path's backslashes are not read as escapes.
    fn abs(&self, name: &str) -> String {
        self.path()
            .join(name)
            .to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('$', "\\$")
    }

    fn cig_env(&self, args: &[&str], envs: &[(&str, &str)]) -> Output {
        Command::new(cig_bin())
            .args(args)
            .current_dir(self.path())
            .env("CIGSCRIPT_HOME", self.home())
            .env("HOME", self.path())
            .env("NO_COLOR", "1")
            .env("CIG_SEAL_KEY", "file")
            .env("CIG_INTERNAL_TRACKER_DIR", self.path().join("tracker"))
            .env_remove("CI")
            .env("CIG_UPDATE_MANIFEST", self.path().join("no-releases.json"))
            .envs(envs.iter().copied())
            .output()
            .expect("run cig")
    }

    fn cig(&self, args: &[&str]) -> Output {
        self.cig_env(args, &[])
    }

    fn run_ok(&self, args: &[&str]) -> String {
        let out = self.cig(args);
        assert!(out.status.success(), "{}", describe(args, &out));
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// Expect exactly this exit code; return stderr.
    fn run_err(&self, args: &[&str], code: i32) -> String {
        let out = self.cig(args);
        assert_eq!(out.status.code(), Some(code), "{}", describe(args, &out));
        String::from_utf8_lossy(&out.stderr).to_string()
    }

    /// Expect a failure; return stdout and stderr together.
    fn run_fails(&self, args: &[&str]) -> String {
        let out = self.cig(args);
        assert!(!out.status.success(), "{}", describe(args, &out));
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    }

    /// Run a script and return stdout. Each script has a file of its own
    /// (named by its text): a pass records the script it was issued from,
    /// so rewriting one file would change another pass's input.
    fn script_ok(&self, source: &str) -> String {
        let name = script_name(source);
        self.write(&name, source);
        self.run_ok(&["run", &name])
    }

    /// Run a script and expect it to stop with exit code 1; return stderr.
    fn script_fails(&self, source: &str) -> String {
        let name = script_name(source);
        self.write(&name, source);
        self.run_err(&["run", &name], 1)
    }

    /// `cig pass list --json`, as a vector.
    fn passes(&self) -> Vec<serde_json::Value> {
        listed(&self.run_ok(&["pass", "list", "--json"]))
    }

    /// `cig pass verify <token>` combined output and exit code.
    fn verify(&self, token: &str, extra: &[&str]) -> (i32, String) {
        let mut args = vec!["pass", "verify", token];
        args.extend_from_slice(extra);
        let out = self.cig(&args);
        (
            out.status.code().unwrap_or(-1),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }
}

/// A file name for a script, from its text.
fn script_name(source: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut h);
    format!("s-{:016x}.cig", h.finish())
}

fn describe(args: &[&str], out: &Output) -> String {
    format!(
        "cig {args:?} exited {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

// ----- how the output is read -------------------------------------------------

/// The token on a line of its own.
fn token_in(stdout: &str) -> String {
    stdout
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("cigpass1."))
        .unwrap_or_else(|| panic!("no token in:\n{stdout}"))
        .to_string()
}

fn listed(stdout: &str) -> Vec<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_str(stdout).expect("pass list json");
    match v {
        serde_json::Value::Array(a) => a,
        serde_json::Value::Object(o) => o
            .get("passes")
            .and_then(|p| p.as_array())
            .cloned()
            .expect("a `passes` array"),
        other => panic!("unexpected list shape: {other}"),
    }
}

/// (kind, key) for every input of a `cig pass show --json` document.
fn input_pairs(show: &serde_json::Value) -> Vec<(String, String)> {
    show["inputs"]
        .as_array()
        .expect("an inputs array")
        .iter()
        .map(|i| {
            let kind = i["kind"]
                .as_str()
                .or_else(|| i["type"].as_str())
                .unwrap_or("")
                .to_lowercase();
            let key = ["path", "name", "id"]
                .iter()
                .find_map(|k| i[*k].as_str())
                .unwrap_or("")
                .to_string();
            (kind, key)
        })
        .collect()
}

fn allowed_signers_line(key_line: &str) -> String {
    format!("test-signer {}\n", key_line.trim())
}

fn has_input(pairs: &[(String, String)], kind: &str, key_suffix: &str) -> bool {
    pairs
        .iter()
        .any(|(k, key)| k == kind && key.ends_with(key_suffix))
}

/// A script that issues a `check` pass over `in.txt` and prints its token.
fn issue_over_in_txt(sb: &Sandbox, kind: &str, subject: &str) -> String {
    let src = format!(
        "stick p = pass.issue(\"{kind}\", \"{subject}\", pack(s) {{\n  snuff fs.read_text(\"{}\")\n}})\nexhale p.token\n",
        sb.abs("in.txt")
    );
    token_in(&sb.script_ok(&src))
}

// ----- recording ------------------------------------------------------------------

#[test]
fn recorder_is_total_in_cig() {
    let sb = Sandbox::new();
    sb.write("in.txt", "alpha");
    sb.write("folder/one.txt", "1");
    let src = format!(
        r#"stick p = pass.issue("check", "recorder", pack(s) {{
  stick a = fs.read_text("{inp}")
  stick l = fs.list("{folder}")
  stick m = fs.exists("{missing}")
  stick e = env.get("CIGPASS_T_ENV", "none")
  snuff {{a: a, l: l, m: m, e: e}}
}})
exhale p.token
"#,
        inp = sb.abs("in.txt"),
        folder = sb.abs("folder"),
        missing = sb.abs("missing.txt"),
    );
    sb.write("s.cig", &src);
    let out = sb.cig_env(&["run", "s.cig"], &[("CIGPASS_T_ENV", "set")]);
    assert!(out.status.success(), "{}", describe(&["run"], &out));
    let token = token_in(&String::from_utf8_lossy(&out.stdout));

    let shown = sb.run_ok(&["pass", "show", &token, "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&shown).expect("show json");
    let pairs = input_pairs(&doc);
    assert!(has_input(&pairs, "file", "in.txt"), "{pairs:?}");
    assert!(has_input(&pairs, "dir", "folder"), "{pairs:?}");
    assert!(has_input(&pairs, "absent", "missing.txt"), "{pairs:?}");
    assert!(has_input(&pairs, "env", "CIGPASS_T_ENV"), "{pairs:?}");
    // The script itself and cig are inputs by default.
    assert!(has_input(&pairs, "file", "s.cig"), "{pairs:?}");
    assert!(pairs.iter().any(|(k, _)| k == "tool"), "{pairs:?}");
    // The value of the variable is never in the document.
    assert!(
        !shown.contains("\"set\""),
        "env values are digests only: {shown}"
    );

    // And each of the four is a live input: change one at a time.
    let (code, _) = sb.verify(&token, &[]);
    // (the env var is unset for this verify, so the pass is already stale)
    assert_eq!(code, 1, "env differs from the recording");
}

#[test]
fn every_recorded_input_invalidates_in_cig() {
    let sb = Sandbox::new();
    sb.write("in.txt", "alpha");
    sb.write("folder/one.txt", "1");
    let src = format!(
        r#"stick p = pass.issue("check", "each", pack(s) {{
  stick a = fs.read_text("{inp}")
  stick l = fs.list("{folder}")
  stick m = fs.exists("{missing}")
  snuff {{a: a, l: l, m: m}}
}})
exhale p.token
"#,
        inp = sb.abs("in.txt"),
        folder = sb.abs("folder"),
        missing = sb.abs("missing.txt"),
    );
    let token = token_in(&sb.script_ok(&src));
    assert_eq!(sb.verify(&token, &[]).0, 0, "valid right after issue");

    // The file changes (and changes back).
    sb.write("in.txt", "alphb");
    let (code, why) = sb.verify(&token, &[]);
    assert_eq!(code, 1, "{why}");
    assert!(why.contains("in.txt"), "names the file: {why}");
    sb.write("in.txt", "alpha");
    assert_eq!(sb.verify(&token, &[]).0, 0, "same bytes, valid again");

    // The folder gains an entry.
    sb.write("folder/two.txt", "2");
    let (code, why) = sb.verify(&token, &[]);
    assert_eq!(code, 1, "{why}");
    assert!(why.contains("folder"), "{why}");
    fs::remove_file(sb.path().join("folder/two.txt")).unwrap();
    assert_eq!(sb.verify(&token, &[]).0, 0);

    // The missing path appears.
    sb.write("missing.txt", "");
    let (code, why) = sb.verify(&token, &[]);
    assert_eq!(code, 1, "{why}");
    assert!(why.contains("appeared"), "{why}");
}

#[test]
fn canonical_subject() {
    // Key order and spacing of a map subject never matter.
    let sb = Sandbox::new();
    let out = sb.script_ok(
        r#"stick p = pass.issue("check", {a: 1, b: [1, 2], c: "x"}, pack(s) { snuff 1 })
stick q = pass.find("check", {c: "x", b: [1, 2], a: 1})
stick r = pass.find("check", {a: 2, b: [1, 2], c: "x"})
exhale q != null, r == null
"#,
    );
    assert_eq!(out.trim(), "true true", "{out}");
}

// `cat` and `ls` are Unix programs, and the test reads the program's path.
#[cfg(unix)]
#[test]
fn proc_run_with_reads_is_recorded() {
    // `reads: [..]` declares the inputs of a program: the file, the resolved
    // program and PATH are recorded; nothing is open-world.
    let sb = Sandbox::new();
    sb.write("in.txt", "hello\n");
    let src = format!(
        r#"stick p = pass.issue("check", "cat", pack(s) {{
  roll r = null
  burn {{
    r = proc.run("cat", ["{inp}"], {{reads: ["{inp}"]}})
  }}
  snuff r.out
}})
exhale p.token
"#,
        inp = sb.abs("in.txt")
    );
    let token = token_in(&sb.script_ok(&src));
    let doc: serde_json::Value =
        serde_json::from_str(&sb.run_ok(&["pass", "show", &token, "--json"])).unwrap();
    let pairs = input_pairs(&doc);
    assert!(has_input(&pairs, "file", "in.txt"), "{pairs:?}");
    assert!(has_input(&pairs, "file", "/cat"), "the program: {pairs:?}");
    assert!(has_input(&pairs, "env", "PATH"), "{pairs:?}");
}

// ----- refusals ---------------------------------------------------------------------

#[test]
fn open_world_refused() {
    let sb = Sandbox::new();
    let err = sb.script_fails(
        r#"stick p = pass.issue("check", "open", pack(s) {
  roll r = null
  burn {
    r = proc.run("echo", ["hi"], {reads: true})
  }
  snuff r.out
})
exhale "unreachable"
"#,
    );
    assert!(err.contains("E731"), "{err}");
    assert!(err.contains("proc.run"), "names the call: {err}");
    assert!(sb.passes().is_empty(), "nothing is stored");
}

#[test]
fn other_open_world_calls_are_refused() {
    for (label, body) in [
        ("run_id", "snuff cig.run_id()"),
        ("modified", "snuff fs.modified_ms(\"in.txt\")"),
        ("time", "snuff time.now_ms()"),
    ] {
        let sb = Sandbox::new();
        sb.write("in.txt", "x");
        let err = sb.script_fails(&format!(
            "stick p = pass.issue(\"check\", \"o\", pack(s) {{\n  {body}\n}})\nexhale \"unreachable\"\n"
        ));
        assert!(err.contains("E731"), "{label}: {err}");
        assert!(sb.passes().is_empty(), "{label}: nothing is stored");
    }
}

#[test]
fn outer_local_refused() {
    let sb = Sandbox::new();
    let err = sb.script_fails(
        r#"pull make() {
  stick outer = "from the function around the work"
  snuff pass.issue("check", "o", pack(s) { snuff outer })
}
make()
"#,
    );
    assert!(err.contains("E732"), "{err}");
    assert!(sb.passes().is_empty());

    // Put it in the subject and it is fine.
    let ok = sb.script_ok(
        r#"pull make() {
  stick outer = "now in the subject"
  snuff pass.issue("check", outer, pack(s) { snuff s })
}
exhale make().result
"#,
    );
    assert_eq!(ok.trim(), "now in the subject");
    // A global is the script's: also fine, and recorded through the script
    // file as an input by default.
    let ok = sb.script_ok(
        r#"stick g = "global"
exhale pass.issue("check", "g", pack(s) { snuff g }).result
"#,
    );
    assert_eq!(ok.trim(), "global");
}

#[test]
fn burn_inside_work_refused() {
    let sb = Sandbox::new();
    let err = sb.script_fails(&format!(
        r#"stick p = pass.issue("check", "b", pack(s) {{
  burn {{
    fs.write_text("{}", "should never land")
  }}
  snuff 1
}})
exhale "unreachable"
"#,
        sb.abs("burned.txt")
    ));
    assert!(err.contains("E733"), "{err}");
    assert!(!sb.exists("burned.txt"), "the burn did not run");
    assert!(sb.passes().is_empty());
}

// ----- the store: memo, dry run ---------------------------------------------------------

#[test]
fn memo_hits_then_misses_after_the_file_changes() {
    let sb = Sandbox::new();
    sb.write("in.txt", "one");
    let src = format!(
        "stick p = pass.memo(\"check\", \"memo\", pack(s) {{\n  snuff fs.read_text(\"{}\").trim()\n}})\nexhale p.hit, p.result\n",
        sb.abs("in.txt")
    );
    sb.write("s.cig", &src);
    assert_eq!(
        sb.run_ok(&["run", "s.cig"]).trim(),
        "false one",
        "first run: a miss"
    );
    assert_eq!(
        sb.run_ok(&["run", "s.cig"]).trim(),
        "true one",
        "second run: a hit"
    );
    assert_eq!(sb.run_ok(&["run", "s.cig"]).trim(), "true one");

    sb.write("in.txt", "two");
    assert_eq!(
        sb.run_ok(&["run", "s.cig"]).trim(),
        "false two",
        "the file changed: redone"
    );
    assert_eq!(sb.run_ok(&["run", "s.cig"]).trim(), "true two");

    // The script itself is an input: edit it and the pass is not reused.
    let edited = format!("{src}# a comment\n");
    sb.write("s.cig", &edited);
    assert_eq!(sb.run_ok(&["run", "s.cig"]).trim(), "false two");
}

#[test]
fn code_false_ignores_the_script_edit() {
    let sb = Sandbox::new();
    sb.write("in.txt", "one");
    let src = format!(
        "stick p = pass.memo(\"check\", \"nocode\", pack(s) {{\n  snuff fs.read_text(\"{}\").trim()\n}}, {{code: false}})\nexhale p.hit, p.result\n",
        sb.abs("in.txt")
    );
    sb.write("s.cig", &src);
    assert_eq!(sb.run_ok(&["run", "s.cig"]).trim(), "false one");
    sb.write("s.cig", &format!("{src}# edited\n"));
    assert_eq!(sb.run_ok(&["run", "s.cig"]).trim(), "true one");
}

#[test]
fn dry_run_never_stores() {
    let sb = Sandbox::new();
    sb.write("in.txt", "one");
    let src = format!(
        "stick p = pass.issue(\"check\", \"dry\", pack(s) {{\n  snuff fs.read_text(\"{}\")\n}})\nexhale p.result\n",
        sb.abs("in.txt")
    );
    sb.write("s.cig", &src);
    let out = sb.cig(&["run", "--dry-run", "s.cig"]);
    assert!(out.status.success(), "{}", describe(&["dry"], &out));
    // The work ran (reads go through) and returned its result...
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "one");
    // ...and nothing was stored.
    assert!(sb.passes().is_empty(), "a dry run stores no pass");
    let again = sb.cig(&["run", "--dry-run", "s.cig"]);
    assert!(again.status.success());
    assert!(sb.passes().is_empty());
    // A real run does.
    sb.run_ok(&["run", "s.cig"]);
    assert_eq!(sb.passes().len(), 1);
    // And a dry run's memo after a real one hits but still writes nothing new.
    sb.run_ok(&["run", "--dry-run", "s.cig"]);
    assert_eq!(sb.passes().len(), 1);
}

#[test]
fn dry_run_spend_records_nothing_and_says_so() {
    let sb = Sandbox::new();
    sb.write("in.txt", "x");
    let once = {
        let t = issue_over_in_txt(&sb, "check", "dry-spend");
        token_in(&sb.run_ok(&["pass", "narrow", &t, "once"]))
    };
    sb.write(
        "spend.cig",
        &format!("pass.spend(\"{once}\")\nexhale \"spent in the dry run\"\n"),
    );
    let out = sb.cig(&["run", "--dry-run", "spend.cig"]);
    assert!(out.status.success(), "{}", describe(&["dry"], &out));
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(all.contains("dry"), "says it recorded nothing: {all}");
    // The pass is still spendable for real afterwards.
    sb.run_ok(&["run", "spend.cig"]);
}

// ----- spending ----------------------------------------------------------------------------

#[test]
fn once_is_spent() {
    let sb = Sandbox::new();
    let err = sb.script_fails(
        r#"stick p = pass.issue("check", "once", pack(s) { snuff 1 })
stick o = pass.narrow(p, "once")
pass.spend(o)
exhale "first spend ok"
pass.spend(o)
exhale "unreachable"
"#,
    );
    assert!(err.contains("E735"), "{err}");
    // The first spend is in the ledger, the second never was.
    let log = sb.run_ok(&["pass", "log"]);
    assert_eq!(
        log.lines().filter(|l| l.contains("pass-spend")).count(),
        1,
        "{log}"
    );
    let json = sb.run_ok(&["pass", "log", "--json"]);
    serde_json::from_str::<serde_json::Value>(&json).expect("log --json is json");
}

#[test]
fn once_is_spent_across_processes_and_narrowings() {
    let sb = Sandbox::new();
    sb.write("in.txt", "x");
    let base = issue_over_in_txt(&sb, "check", "across");
    let once = token_in(&sb.run_ok(&["pass", "narrow", &base, "once"]));
    // Appending caveats after `once` does not make a fresh pass.
    let narrower = token_in(&sb.run_ok(&["pass", "narrow", &once, "max 5"]));

    sb.write(
        "spend1.cig",
        &format!("pass.spend(\"{once}\")\nexhale \"ok\"\n"),
    );
    assert_eq!(sb.run_ok(&["run", "spend1.cig"]).trim(), "ok");
    let err = sb.run_err(&["run", "spend1.cig"], 1);
    assert!(
        err.contains("E735"),
        "the ledger outlives the process: {err}"
    );

    sb.write(
        "spend2.cig",
        &format!("pass.spend(\"{narrower}\")\nexhale \"ok\"\n"),
    );
    let err = sb.run_err(&["run", "spend2.cig"], 1);
    assert!(
        err.contains("E735"),
        "a narrowing of a spent pass is spent: {err}"
    );
}

#[test]
fn max_n_is_spent_after_n() {
    let sb = Sandbox::new();
    let err = sb.script_fails(
        r#"stick p = pass.issue("check", "max", pack(s) { snuff 1 })
stick m = pass.narrow(p, "max 2")
pass.spend(m)
pass.spend(m)
exhale "two spends ok"
pass.spend(m)
"#,
    );
    assert!(err.contains("E735"), "{err}");
}

#[test]
fn spending_an_invalid_pass_is_e730() {
    let sb = Sandbox::new();
    sb.write("in.txt", "x");
    let t = issue_over_in_txt(&sb, "check", "stale");
    sb.write("in.txt", "changed");
    sb.write(
        "spend.cig",
        &format!("pass.spend(\"{t}\")\nexhale \"unreachable\"\n"),
    );
    let err = sb.run_err(&["run", "spend.cig"], 1);
    assert!(err.contains("E730"), "{err}");
    assert!(err.contains("in.txt"), "names the first failure: {err}");
}

#[test]
fn pass_check_says_why_without_coughing() {
    let sb = Sandbox::new();
    sb.write("in.txt", "x");
    let t = issue_over_in_txt(&sb, "check", "check-why");
    sb.write("in.txt", "changed");
    sb.write(
        "c.cig",
        &format!("stick r = pass.check(\"{t}\")\nexhale r.ok, r.why\n"),
    );
    let out = sb.run_ok(&["run", "c.cig"]);
    assert!(out.starts_with("false "), "{out}");
    assert!(out.contains("in.txt"), "{out}");
}

// ----- revocation, epochs -----------------------------------------------------------------------

#[test]
fn epoch_kills_kind() {
    let sb = Sandbox::new();
    sb.write("in.txt", "x");
    let verdict = issue_over_in_txt(&sb, "verdict", "ek");
    let approval = issue_over_in_txt(&sb, "approval", "ek");
    assert_eq!(sb.verify(&verdict, &[]).0, 0);
    assert_eq!(sb.verify(&approval, &[]).0, 0);

    sb.run_ok(&["pass", "revoke", "--kind", "verdict"]);

    let (code, why) = sb.verify(&verdict, &[]);
    assert_eq!(code, 1, "{why}");
    assert!(why.contains("epoch"), "{why}");
    assert!(why.contains("verdict"), "{why}");
    let (code, why) = sb.verify(&approval, &[]);
    assert_eq!(code, 0, "an approval pass still verifies: {why}");

    // The bump is a ledger line.
    let log = sb.run_ok(&["pass", "log"]);
    assert!(log.contains("pass-epoch"), "{log}");
    // A pass issued after the bump is good.
    let fresh = issue_over_in_txt(&sb, "verdict", "ek2");
    assert_eq!(sb.verify(&fresh, &[]).0, 0);
}

#[test]
fn revoking_a_pass_by_id_kills_it_and_its_narrowings() {
    let sb = Sandbox::new();
    sb.write("in.txt", "x");
    let src = format!(
        "stick p = pass.issue(\"check\", \"rv\", pack(s) {{\n  snuff fs.read_text(\"{}\")\n}})\nexhale p.id\nexhale p.token\n",
        sb.abs("in.txt")
    );
    let out = sb.script_ok(&src);
    let id = out.lines().next().unwrap().trim().to_string();
    let token = token_in(&out);
    let narrowed = token_in(&sb.run_ok(&["pass", "narrow", &token, "max 3"]));
    assert_eq!(sb.verify(&narrowed, &[]).0, 0);

    sb.run_ok(&["pass", "revoke", &id]);
    for t in [&token, &narrowed] {
        let (code, why) = sb.verify(t, &[]);
        assert_eq!(code, 1, "{why}");
        assert!(why.contains("revoked"), "{why}");
    }
    assert!(sb.run_ok(&["pass", "log"]).contains("pass-revoke"));
}

#[test]
fn pass_state_tamper_refused() {
    let sb = Sandbox::new();
    sb.write("in.txt", "x");
    let t = issue_over_in_txt(&sb, "verdict", "tamper");
    sb.run_ok(&["pass", "revoke", "--kind", "verdict"]);
    // The pass state is a MAC'd cache of the ledger, beside the passes.
    let marks = walk(sb.path())
        .into_iter()
        .find(|p| {
            p.parent().is_some_and(|d| d.ends_with("passes"))
                && p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("marks-"))
        })
        .expect("the pass state after a revoke");
    let mut bytes = fs::read(&marks).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0x01;
    fs::write(&marks, &bytes).unwrap();
    let (code, out) = sb.verify(&t, &[]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("E739"), "a failed MAC is E739: {out}");
    // Removed, it is rebuilt from the ledger: the revocation is still there.
    fs::remove_file(&marks).unwrap();
    let (code, why) = sb.verify(&t, &[]);
    assert_eq!(code, 1, "{why}");
    assert!(
        why.contains("epoch"),
        "deleting the state never un-revokes: {why}"
    );
    let fresh = issue_over_in_txt(&sb, "verdict", "after-tamper");
    assert_eq!(sb.verify(&fresh, &[]).0, 0);
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

// ----- narrow ----------------------------------------------------------------------------------------

#[test]
fn narrow_then_verify() {
    let sb = Sandbox::new();
    sb.write("in.txt", "x");
    let t = issue_over_in_txt(&sb, "check", "narrow");
    assert_eq!(sb.verify(&t, &[]).0, 0);

    let narrowed = token_in(&sb.run_ok(&["pass", "narrow", &t, "max 3", "for 2h"]));
    assert_ne!(narrowed, t);
    assert_eq!(sb.verify(&narrowed, &[]).0, 0, "valid inside the caveats");
    let shown = sb.run_ok(&["pass", "show", &narrowed]);
    assert!(shown.contains("max 3"), "{shown}");

    // A caveat that already fails narrows to nothing.
    let past = token_in(&sb.run_ok(&["pass", "narrow", &t, "until 2001-01-01T00:00:00Z"]));
    let (code, why) = sb.verify(&past, &[]);
    assert_eq!(code, 1, "{why}");
    assert!(why.contains("passed"), "{why}");

    // `-o` writes the token to a file; a file verifies like a token.
    sb.run_ok(&["pass", "narrow", &t, "boxed", "-o", "narrowed.pass"]);
    assert!(sb.exists("narrowed.pass"));
    let (code, _) = sb.verify("narrowed.pass", &[]);
    // `boxed` holds only for a boxed action; the plain `verify` command runs
    // as none, so it is refused and says which caveat.
    assert_eq!(code, 1);
}

#[test]
fn narrow_refuses_a_caveat_it_cannot_read() {
    let sb = Sandbox::new();
    sb.write("in.txt", "x");
    let t = issue_over_in_txt(&sb, "check", "badcaveat");
    let out = sb.run_fails(&["pass", "narrow", &t, "frobnicate the thing"]);
    assert!(out.contains("E809"), "{out}");
    // Nothing printed that looks like a token.
    assert!(!out.contains("cigpass1."), "{out}");
}

#[test]
fn script_narrow_matches_cli_narrow() {
    let sb = Sandbox::new();
    let out = sb.script_ok(
        r#"stick p = pass.issue("check", "sn", pack(s) { snuff 1 })
stick n = pass.narrow(p, "max 4")
exhale n.caveats
exhale pass.check(n).ok
"#,
    );
    assert!(out.contains("max 4"), "{out}");
    assert!(out.trim_end().ends_with("true"), "{out}");
}

// ----- verify exit codes ---------------------------------------------------------------------------------

#[test]
fn verify_exit_codes() {
    let sb = Sandbox::new();
    sb.write("in.txt", "x");
    let t = issue_over_in_txt(&sb, "check", "codes");

    // 0: valid, and `--json` says so.
    let out = sb.cig(&["pass", "verify", &t, "--json"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        describe(&["verify"], &out)
    );
    let j: serde_json::Value = serde_json::from_slice(&out.stdout).expect("verify --json");
    assert_eq!(j["valid"], true, "{j}");

    // 1: invalid, with the reason in words.
    sb.write("in.txt", "changed");
    let out = sb.cig(&["pass", "verify", &t, "--json"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        describe(&["verify"], &out)
    );
    let j: serde_json::Value = serde_json::from_slice(&out.stdout).expect("verify --json");
    assert_eq!(j["valid"], false, "{j}");
    assert!(j["why"].as_str().unwrap_or("").contains("in.txt"), "{j}");

    // 3: usage (no pass named).
    sb.run_err(&["pass", "verify"], 3);
    // Not a pass at all: E736, never a pass.
    let out = sb.run_fails(&["pass", "verify", "cigpass1.AAAA"]);
    assert!(out.contains("E736"), "{out}");
    let out = sb.run_fails(&["pass", "verify", "not-a-token"]);
    assert!(!out.contains("valid\n"), "{out}");
}

#[test]
fn a_tampered_token_is_never_valid() {
    let sb = Sandbox::new();
    sb.write("in.txt", "x");
    let t = issue_over_in_txt(&sb, "check", "tamper-token");
    let body_start = "cigpass1.".len();
    // Flip one character at a spread of positions in the body.
    for pos in (body_start..t.len()).step_by(7) {
        let mut b = t.clone().into_bytes();
        b[pos] = if b[pos] == b'A' { b'B' } else { b'A' };
        let bad = String::from_utf8(b).unwrap();
        let (code, out) = sb.verify(&bad, &[]);
        assert_ne!(code, 0, "flipped at {pos} still verified: {out}");
    }
}

// ----- export and import --------------------------------------------------------------------------------------

#[test]
fn pass_key_prints_the_public_key() {
    let sb = Sandbox::new();
    let key = sb.run_ok(&["pass", "key"]);
    // An allowed-signers line: principal, the kinds it may issue, the key.
    assert!(
        key.contains("namespaces=\"cigpass-check,cigpass-program\" ssh-ed25519 "),
        "{key}"
    );
    let b64 = key
        .split_whitespace()
        .skip_while(|w| *w != "ssh-ed25519")
        .nth(1)
        .expect("a key field");
    // OpenSSH wire format for ed25519 always starts like this.
    assert!(b64.starts_with("AAAAC3NzaC1lZDI1NTE5AAAAI"), "{b64}");
    assert_eq!(sb.run_ok(&["pass", "key"]), key, "stable across runs");
    // Another machine (another home) has another key.
    let other = Sandbox::new();
    assert_ne!(other.run_ok(&["pass", "key"]), key);
}

#[test]
fn export_then_import_with_allowed_signers() {
    let a = Sandbox::new();
    a.write("in.txt", "x");
    let out = a.script_ok(&format!(
        "stick p = pass.issue(\"check\", \"export\", pack(s) {{\n  snuff fs.read_text(\"{}\")\n}})\nexhale p.id\n",
        a.abs("in.txt")
    ));
    let id = out.trim().to_string();
    a.run_ok(&["pass", "export", &id, "-o", "out.pass"]);
    assert!(a.exists("out.pass"));
    let signer = a.run_ok(&["pass", "key"]);

    // Machine B: its own home, so its own keys, and its own allowed signers.
    let b = Sandbox::new();
    fs::copy(a.path().join("out.pass"), b.path().join("out.pass")).unwrap();
    b.write("allowed", &allowed_signers_line(&signer));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Whoever can write it decides whose passes hold: private, or ignored.
        fs::set_permissions(b.path().join("allowed"), fs::Permissions::from_mode(0o600)).unwrap();
    }

    // Without the signer: refused, named.
    let out = b.run_fails(&["pass", "import", "out.pass"]);
    assert!(out.contains("E738"), "{out}");
    assert!(b.passes().is_empty(), "a refused import stores nothing");
    let (code, why) = b.verify("out.pass", &[]);
    assert_eq!(code, 1, "{why}");
    assert!(why.contains("E738") || why.contains("signer"), "{why}");

    // With it: verifies, then imports.
    let (code, why) = b.verify("out.pass", &["--allowed-signers", "allowed"]);
    assert_eq!(code, 0, "{why}");
    b.run_ok(&["pass", "import", "out.pass", "--allowed-signers", "allowed"]);
    assert_eq!(b.passes().len(), 1);

    // A different signer's key does not do.
    let stranger = Sandbox::new().run_ok(&["pass", "key"]);
    b.write("stranger", &allowed_signers_line(&stranger));
    let (code, _) = b.verify("out.pass", &["--allowed-signers", "stranger"]);
    assert_eq!(code, 1);
    let out = b.run_fails(&[
        "pass",
        "import",
        "out.pass",
        "--allowed-signers",
        "stranger",
    ]);
    assert!(out.contains("E738"), "{out}");
}

#[test]
fn export_refuses_a_pass_that_is_not_valid_here() {
    let a = Sandbox::new();
    a.write("in.txt", "x");
    let out = a.script_ok(&format!(
        "stick p = pass.issue(\"check\", \"stale-export\", pack(s) {{\n  snuff fs.read_text(\"{}\")\n}})\nexhale p.id\n",
        a.abs("in.txt")
    ));
    let id = out.trim().to_string();
    a.write("in.txt", "changed");
    let msg = a.run_fails(&["pass", "export", &id, "-o", "out.pass"]);
    assert!(msg.contains("in.txt") || msg.contains("E730"), "{msg}");
    assert!(!a.exists("out.pass"), "nothing written for a stale pass");
}

// ----- the command line around the store ---------------------------------------------------------------------------

#[test]
fn list_log_stats_gc_run() {
    let sb = Sandbox::new();
    sb.write("in.txt", "x");
    issue_over_in_txt(&sb, "check", "l1");
    issue_over_in_txt(&sb, "verdict", "l2");
    let all = sb.passes();
    assert_eq!(all.len(), 2);
    let verdicts = listed(&sb.run_ok(&["pass", "list", "--kind", "verdict", "--json"]));
    assert_eq!(verdicts.len(), 1, "{verdicts:?}");
    sb.run_ok(&["pass", "list"]);
    sb.run_ok(&["pass", "stats"]);
    serde_json::from_str::<serde_json::Value>(&sb.run_ok(&["pass", "stats", "--json"]))
        .expect("stats --json is json");
    sb.run_ok(&["pass", "gc"]);
    assert_eq!(sb.passes().len(), 2, "gc keeps the passes that verify");
}

#[test]
fn an_unknown_pass_subcommand_is_a_usage_error_with_a_hint() {
    let sb = Sandbox::new();
    let out = sb.run_fails(&["pass", "frobnicate"]);
    assert!(
        out.contains("cig pass") || out.contains("cig.exe pass"),
        "a hint says what to type next: {out}"
    );
}

// ----- found in review ---------------------------------------------------------------

#[test]
fn rekey_then_pass_ops_still_work() {
    let sb = Sandbox::new();
    sb.write("in.txt", "x");
    let old = issue_over_in_txt(&sb, "check", "before-rekey");
    sb.run_ok(&["pass", "revoke", "--kind", "verdict"]);
    sb.run_ok(&["tin", "rekey"]);
    // The old local pass was made under the old key: it does not hold.
    let (code, why) = sb.verify(&old, &[]);
    assert_eq!(code, 1, "{why}");
    // Everything else goes on: the state is folded again under the new key.
    let fresh = issue_over_in_txt(&sb, "check", "after-rekey");
    assert_eq!(sb.verify(&fresh, &[]).0, 0);
    sb.run_ok(&["pass", "revoke", "--kind", "verdict"]);
    assert!(sb.run_ok(&["pass", "log"]).matches("pass-epoch").count() >= 2);
}

#[test]
fn nested_issue_with_until_bounds_the_outer_pass() {
    let sb = Sandbox::new();
    let out = sb.script_ok(
        r#"stick outer = pass.issue("check", "outer", pack(s) {
  snuff pass.issue("check", "inner", pack(t) => time.now_ms(), {until: "1h"}).result
})
stick shown = pass.show(outer)
roll clock = false
for x in shown.inputs {
  if x.kind == "clock" { clock = true }
}
exhale clock
"#,
    );
    assert_eq!(
        out.trim(),
        "true",
        "the outer pass carries the inner one's limit: {out}"
    );
}

#[test]
fn a_global_the_work_reads_is_part_of_the_pass() {
    let sb = Sandbox::new();
    // A list global changed through an alias before the work runs: the
    // pass records what it held, and a later find with another value misses.
    let src = r#"stick base = [1, 2, 3]
stick alias = base
pull memo() {
  snuff pass.memo("check", "g", pack(s) => base.len())
}
stick a = memo()
alias.push(4)
stick b = memo()
exhale a.result, b.result, b.hit
"#;
    let out = sb.script_ok(src);
    assert_eq!(out.trim(), "3 4 false", "{out}");
}

#[test]
fn work_cannot_mutate_its_subject() {
    let sb = Sandbox::new();
    let out = sb.script_ok(
        r#"stick subject = {list: [1, 2]}
stick p = pass.issue("check", subject, pack(s) {
  s.list.push(3)
  snuff s.list.len()
})
exhale p.result, subject.list.len()
"#,
    );
    assert_eq!(out.trim(), "3 2", "the work got a copy: {out}");
}

#[test]
fn dry_run_issues_no_token() {
    let sb = Sandbox::new();
    sb.write("in.txt", "x");
    sb.write(
        "d.cig",
        &format!(
            "stick p = pass.issue(\"check\", \"dry\", pack(s) {{ snuff fs.read_text(\"{}\") }})\nexhale p.token == null, p.result\n",
            sb.abs("in.txt")
        ),
    );
    let out = sb.run_ok(&["run", "--dry-run", "d.cig"]);
    assert!(out.contains("true x"), "{out}");
    assert!(sb.passes().is_empty());
}

#[test]
fn deleting_the_ledger_and_the_state_refuses_older_passes() {
    let sb = Sandbox::new();
    sb.write("in.txt", "x");
    sb.run_ok(&["pass", "revoke", "--kind", "verdict"]);
    let t = issue_over_in_txt(&sb, "check", "anchored");
    assert_eq!(sb.verify(&t, &[]).0, 0);
    for p in walk(sb.path()) {
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        if name == "ledger.jsonl" || name.starts_with("marks-") {
            fs::remove_file(&p).unwrap();
        }
    }
    let (code, why) = sb.verify(&t, &[]);
    assert_eq!(code, 1, "{why}");
    assert!(why.contains("ledger is shorter"), "{why}");
}

#[cfg(unix)]
#[test]
fn a_folder_in_reads_covers_the_files_in_it() {
    let sb = Sandbox::new();
    sb.write("lib/a.txt", "one");
    sb.write("lib/deep/b.txt", "two");
    let src = format!(
        r#"stick p = pass.issue("check", "dir", pack(s) {{
  roll r = null
  burn {{
    r = proc.run("ls", ["{dir}"], {{reads: ["{dir}"]}})
  }}
  snuff r.code
}})
exhale p.token
"#,
        dir = sb.abs("lib")
    );
    let t = token_in(&sb.script_ok(&src));
    assert_eq!(sb.verify(&t, &[]).0, 0);
    sb.write("lib/deep/b.txt", "TWO");
    let (code, why) = sb.verify(&t, &[]);
    assert_eq!(code, 1, "an edit deep inside a declared folder: {why}");
}

#[test]
fn two_memos_with_one_subject_do_not_collide() {
    let sb = Sandbox::new();
    let out = sb.script_ok(
        r#"stick a = pass.memo("check", "same", pack(s) => "first")
stick b = pass.memo("check", "same", pack(s) => "second")
exhale a.result, b.result
"#,
    );
    assert_eq!(out.trim(), "first second", "{out}");
}

#[test]
fn pass_verify_takes_the_action() {
    let sb = Sandbox::new();
    let t = token_in(&sb.script_ok(
        "stick p = pass.issue(\"check\", \"act\", pack(s) => 1)\nexhale pass.narrow(pass.narrow(p, \"boxed\"), \"agent hivemind\").token\n",
    ));
    assert_eq!(
        sb.verify(&t, &[]).0,
        1,
        "no action given: the caveats do not hold"
    );
    let (code, why) = sb.verify(&t, &["--boxed", "--agent", "hivemind"]);
    assert_eq!(code, 0, "{why}");
    assert_eq!(sb.verify(&t, &["--boxed", "--agent", "someone"]).0, 1);
}

#[test]
fn concurrent_ledger_appends_keep_its_chain() {
    let sb = Sandbox::new();
    sb.run_ok(&["pass", "revoke", "--kind", "check"]);
    let children: Vec<_> = (0..8)
        .map(|_| {
            Command::new(cig_bin())
                .args(["pass", "revoke", "--kind", "verdict"])
                .current_dir(sb.path())
                .env("CIGSCRIPT_HOME", sb.home())
                .env("HOME", sb.path())
                .env("NO_COLOR", "1")
                .env("CIG_SEAL_KEY", "file")
                .env("CIG_INTERNAL_TRACKER_DIR", sb.path().join("tracker"))
                .spawn()
                .expect("spawn cig")
        })
        .collect();
    for mut c in children {
        assert!(c.wait().unwrap().success());
    }
    // Every append chained on the one before: the state folds, and the
    // count is exact.
    let out = sb.run_ok(&["pass", "revoke", "--kind", "verdict"]);
    assert!(out.contains("epoch 9"), "{out}");
}
