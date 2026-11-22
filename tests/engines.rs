// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! #254: `cig run` on the VM says and does what it says and does on the
//! tree-walker. Every script of the corpus runs three times: on the
//! tree-walker, on the VM with an empty tin (the run lowers the script and
//! keeps its module), and on the VM again (the run loads the module from
//! the tin). The three are compared, for a real run and for a dry run, in
//! everything a run leaves: its exit status, standard output and standard
//! error; the files in its folder; how its record says it went (status,
//! error and where, burns, whether it was rolled back); and its journal,
//! line for line. Each real run's record is read for the engine it names.
//!
//! The corpus is the scripts written to fail and to resolve names
//! (`tests/walker`), the step-budget scripts (`tests/steps`), the language
//! tour (`tests/scripts`), the examples, the compute programs that finish
//! quickly in a debug build, and the burn programs (`tests/lower/burns.rs`:
//! every way into, through and out of a burn), which are what give the
//! journals something to hold.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use wait_timeout::ChildExt;

#[path = "lower/burns.rs"]
mod burns;

/// The compute programs that finish in about a second in a debug build.
const COMPUTE: [&str; 9] = [
    "hello",
    "startup-0",
    "startup-1",
    "startup-10",
    "closures",
    "dispatch",
    "lists",
    "maps",
    "regex-ish",
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn scripts_in(dir: &str, only: Option<&[&str]>) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(root().join(dir))
        .expect("corpus folder")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "cig"))
        .filter(|p| {
            only.is_none_or(|names| {
                p.file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| names.contains(&s))
            })
        })
        .collect();
    v.sort();
    v
}

/// The corpus, each script with its name and its step budget: the scripts
/// written to fail run with one (one of them never ends); the rest run
/// without. The burn programs are written out under the folder returned
/// with it, which lasts as long as the caller keeps it.
fn corpus() -> (Vec<(PathBuf, String, &'static str)>, tempfile::TempDir) {
    let named = |p: PathBuf| {
        let name = p.strip_prefix(root()).unwrap_or(&p).display().to_string();
        (p, name.replace('\\', "/"))
    };
    let mut corpus: Vec<(PathBuf, String, &str)> = scripts_in("tests/walker", None)
        .into_iter()
        .chain(scripts_in("tests/steps", None))
        .map(named)
        .map(|(p, name)| (p, name, "5000"))
        .collect();
    corpus.extend(
        scripts_in("tests/scripts", None)
            .into_iter()
            .chain(scripts_in("examples", None))
            .chain(scripts_in("bench/compute", Some(&COMPUTE)))
            .map(named)
            .map(|(p, name)| (p, name, "0")),
    );
    // The burn programs, their files named beside the script.
    let written = tempfile::tempdir().expect("tempdir");
    for (name, template, files) in burns::burn_corpus() {
        let mut source = template.to_string();
        for (k, file) in files.iter().enumerate().rev() {
            source = source.replace(&format!("P{k}"), &format!("{file:?}"));
        }
        let path = written.path().join(format!("{name}.cig"));
        fs::write(&path, source).expect("write a burn program");
        corpus.push((path, format!("tests/lower/burns.rs: {name}"), "0"));
    }
    assert!(corpus.len() >= 70, "corpus is too small: {}", corpus.len());
    (corpus, written)
}

/// A state directory and a keys directory that outlive one run, so a second
/// run finds what the first kept.
struct Tin {
    dir: tempfile::TempDir,
}

impl Tin {
    fn new() -> Tin {
        Tin {
            dir: tempfile::tempdir().expect("tempdir"),
        }
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    /// The `engine` of every run recorded here, oldest first.
    fn engines(&self) -> Vec<serde_json::Value> {
        let runs: Vec<PathBuf> = fs::read_dir(self.home().join("runs"))
            .map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default();
        let mut records: Vec<serde_json::Value> = runs
            .iter()
            .filter_map(|r| fs::read_to_string(r.join("run.json")).ok())
            .filter_map(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .collect();
        // By when each started, to the millisecond: two runs in one second
        // have ids that sort by their salt, not by time.
        records.sort_by_key(|rec| rec["started"].as_str().unwrap_or_default().to_string());
        records
            .into_iter()
            .map(|rec| rec["engine"].clone())
            .collect()
    }
}

/// A run id, as a run's last line and its errors name it:
/// `YYYYMMDDTHHMMSS-xxxxxx`.
fn is_run_id(word: &str) -> bool {
    let b = word.as_bytes();
    b.len() == 22
        && b[8] == b'T'
        && b[15] == b'-'
        && b[..8].iter().all(u8::is_ascii_digit)
        && b[9..15].iter().all(u8::is_ascii_digit)
        && b[16..].iter().all(u8::is_ascii_hexdigit)
}

/// What a run printed, with what differs between any two runs taken out:
/// the folder it ran in, as it was given and as the system resolves it (a
/// temporary folder is often reached through a link), and its run id.
fn normal(text: &str, cwd: &Path) -> String {
    let mut text = text.to_string();
    let mut forms = vec![cwd.to_path_buf()];
    if let Ok(real) = cwd.canonicalize() {
        forms.push(real);
    }
    // The longest first, so one form inside another is taken whole.
    let mut names: Vec<String> = forms
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .flat_map(|n| [n.replace('\\', "/"), n])
        .collect();
    names.sort_by_key(|n| std::cmp::Reverse(n.len()));
    names.dedup();
    for name in names {
        text = text.replace(&name, "<cwd>");
    }
    text.split_inclusive(|c: char| c.is_whitespace() || c == ',' || c == ')' || c == '(')
        .map(|piece| {
            let word = piece
                .trim_end_matches(|c: char| c.is_whitespace() || c == ',' || c == ')' || c == '(');
            if is_run_id(word) {
                piece.replacen(word, "<run>", 1)
            } else {
                piece.to_string()
            }
        })
        .collect()
}

/// The files under `dir`, each with its length and the SHA-256 of its
/// bytes: what a run left in its folder.
fn files_in(dir: &Path) -> String {
    use sha2::{Digest, Sha256};
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        let mut entries: Vec<PathBuf> = fs::read_dir(dir)
            .map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default();
        entries.sort();
        for p in entries {
            let name = p
                .strip_prefix(root)
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('\\', "/");
            match fs::symlink_metadata(&p) {
                Ok(m) if m.is_dir() => {
                    out.push(format!("{name}/"));
                    walk(&p, root, out);
                }
                Ok(m) if m.is_file() => {
                    let bytes = fs::read(&p).unwrap_or_default();
                    out.push(format!(
                        "{name} {} {}",
                        bytes.len(),
                        hex::encode(Sha256::digest(&bytes))
                    ));
                }
                Ok(_) => out.push(format!("{name} (not a file)")),
                Err(_) => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.join("\n")
}

/// A journal line or a record without what each run has its own of: when
/// it happened, the hash chain over those times, the run's id, and where
/// in its own tin a before-state was put (its being there still counts).
fn without_own(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(m) => {
            for k in ["at", "prev", "run", "mtime_ns"] {
                m.remove(k);
            }
            if let Some(s) = m.get_mut("snapshot") {
                *s = serde_json::Value::Bool(true);
            }
            for (_, inner) in m.iter_mut() {
                without_own(inner);
            }
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(without_own),
        _ => {}
    }
}

/// The run folders in `tin`.
fn run_folders(tin: &Tin) -> std::collections::BTreeSet<PathBuf> {
    fs::read_dir(tin.home().join("runs"))
        .map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default()
}

/// What a run left in its state directory, `made` being the run folders
/// that were not there before it: how its record says it went, and its
/// journal. A run that never started (a script that does not parse or
/// check) left neither. `cwd` is the folder it ran in.
fn recorded(made: &[PathBuf], cwd: &Path) -> String {
    let mut out = Vec::new();
    for dir in made {
        let Some(record) = fs::read_to_string(dir.join("run.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        else {
            out.push("a run folder without a record that reads".to_string());
            continue;
        };
        for field in [
            "mode",
            "status",
            "error",
            "diagnostic",
            "burns",
            "irreversible",
            "rolled_back",
            "args",
        ] {
            out.push(format!("record {field}: {}", record[field]));
        }
        let journal = fs::read_to_string(dir.join("journal.jsonl")).unwrap_or_default();
        for line in journal.lines() {
            match serde_json::from_str::<serde_json::Value>(line) {
                Ok(mut v) => {
                    without_own(&mut v);
                    out.push(format!("journal: {v}"));
                }
                Err(_) => out.push(format!("journal (not JSON): {line}")),
            }
        }
    }
    normal(&out.join("\n"), cwd)
}

/// What `cig run [--dry-run] --engine <engine> <script>` did in a fresh
/// folder, with `tin` as its state: its exit status, stdout and stderr,
/// the files it left, and what its record and journal say.
fn run(script: &Path, budget: &str, engine: &str, dry: bool, tin: &Tin) -> String {
    let cwd = tempfile::tempdir().expect("tempdir");
    let name = script.file_name().expect("name");
    fs::copy(script, cwd.path().join(name)).expect("copy script");
    let before = run_folders(tin);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cig"));
    cmd.arg("run").arg("--engine").arg(engine);
    if dry {
        cmd.arg("--dry-run");
    }
    let mut child = cmd
        .arg(name)
        .current_dir(cwd.path())
        .env("HOME", cwd.path())
        .env("CIGSCRIPT_HOME", tin.home())
        .env("CIG_KEYS_DIR", tin.dir.path().join("keys"))
        .env("XDG_CONFIG_HOME", tin.dir.path().join("config"))
        .env("XDG_DATA_HOME", tin.dir.path().join("data"))
        .env("XDG_STATE_HOME", tin.dir.path().join("state"))
        .env("XDG_CACHE_HOME", tin.dir.path().join("cache"))
        .env("CIG_SEAL_KEY", "file")
        .env("CIG_DOCTOR", "0")
        .env("NO_COLOR", "1")
        .env("CIG_MAX_STEPS", budget)
        .env("CIG_INTERNAL_TRACKER_DIR", tin.dir.path().join("tracker"))
        .env(
            "CIG_UPDATE_MANIFEST",
            tin.dir.path().join("no-releases.json"),
        )
        .env_remove("CI")
        .env_remove("CIG_ENGINE")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("run cig");
    let status = match child.wait_timeout(Duration::from_secs(120)).expect("wait") {
        Some(s) => s,
        None => {
            child.kill().ok();
            child.wait().ok();
            panic!("{}: cig did not finish in 120 s", script.display());
        }
    };
    let out = child.wait_with_output().expect("output");
    let made: Vec<PathBuf> = run_folders(tin).difference(&before).cloned().collect();
    format!(
        "exit {}\n--- stdout\n{}--- stderr\n{}--- files\n{}\n--- recorded\n{}\n",
        status.code().unwrap_or(-1),
        normal(&String::from_utf8_lossy(&out.stdout), cwd.path()),
        normal(&String::from_utf8_lossy(&out.stderr), cwd.path()),
        files_in(cwd.path()),
        recorded(&made, cwd.path())
    )
}

/// The first lines where two outputs part, for the failure message.
fn first_difference(a: &str, b: &str) -> String {
    for (i, (x, y)) in a.lines().zip(b.lines()).enumerate() {
        if x != y {
            return format!("line {}:\n  tree: {x}\n  vm:   {y}", i + 1);
        }
    }
    format!(
        "one stops where the other goes on ({} lines against {})",
        a.lines().count(),
        b.lines().count()
    )
}

#[test]
fn a_run_on_the_vm_matches_the_tree_walker_cold_and_warm() {
    let mut differ = Vec::new();
    let mut on_tree = Vec::new();
    let mut not_cached = Vec::new();
    // How much of the corpus the comparison of records and journals is
    // about: scripts that ran at all, and scripts that burned.
    let (mut recorded_runs, mut journaled) = (0, 0);
    let (corpus, _written) = corpus();
    for (script, name, budget) in corpus {
        let tree = run(&script, budget, "tree", false, &Tin::new());
        let tin = Tin::new();
        let cold = run(&script, budget, "vm", false, &tin);
        let warm = run(&script, budget, "vm", false, &tin);
        if tree != cold {
            differ.push(format!(
                "{name} (lowered), {}",
                first_difference(&tree, &cold)
            ));
        }
        if tree != warm {
            differ.push(format!(
                "{name} (from the tin), {}",
                first_difference(&tree, &warm)
            ));
        }
        recorded_runs += usize::from(tree.contains("\nrecord status: "));
        journaled += usize::from(tree.contains("\njournal: {\"seq\":"));
        // What the two runs' records say ran them. A script that does not
        // parse or check has no run, so no record.
        let engines = tin.engines();
        match engines.as_slice() {
            [] => {}
            [first, second] => {
                if first["engine"] != "vm" {
                    on_tree.push((
                        name.clone(),
                        first["why"].as_str().unwrap_or("").to_string(),
                    ));
                } else if first["module"] != "lowered" || second["module"] != "cached" {
                    not_cached.push(format!("{name}: {first} then {second}"));
                }
            }
            other => panic!("{name}: two runs, {} records", other.len()),
        }
    }
    assert!(
        differ.is_empty(),
        "the VM and the tree-walker differ on:\n{}",
        differ.join("\n")
    );
    println!("{recorded_runs} scripts left a record, {journaled} of them a journal with burns");
    assert!(
        recorded_runs >= 70 && journaled >= 40,
        "the records and journals compared are too few to mean much: {recorded_runs} records, {journaled} journals"
    );
    assert!(
        not_cached.is_empty(),
        "a first run should lower and keep, a second load from the tin:\n{}",
        not_cached.join("\n")
    );
    // Two scripts of the corpus read a name before its declaration has
    // run: one calls a function before the chain it names is declared, one
    // before a name it reads is declared again further in. The interpreter
    // looks such a name up as the script runs; the lowering declines the
    // script, and it runs on the tree-walker with the reason recorded.
    // Any other script here is one the lowering takes: a new name in this
    // list is a lowering that lost ground.
    let names: Vec<&str> = on_tree.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        [
            "tests/walker/err-name-before-chain.cig",
            "tests/walker/resolve-shadow.cig"
        ],
        "asked for the VM, ran on the tree-walker: {on_tree:#?}"
    );
    for (name, why) in &on_tree {
        assert!(
            why.contains("is looked up as the script runs"),
            "{name}: {why}"
        );
    }
}

#[test]
fn a_dry_run_on_the_vm_plans_what_the_tree_walker_plans() {
    let mut differ = Vec::new();
    let (corpus, _written) = corpus();
    for (script, name, budget) in corpus {
        let tree = run(&script, budget, "tree", true, &Tin::new());
        let tin = Tin::new();
        let cold = run(&script, budget, "vm", true, &tin);
        let warm = run(&script, budget, "vm", true, &tin);
        if tree != cold {
            differ.push(format!(
                "{name} (lowered), {}",
                first_difference(&tree, &cold)
            ));
        }
        if tree != warm {
            differ.push(format!(
                "{name} (from the tin), {}",
                first_difference(&tree, &warm)
            ));
        }
    }
    assert!(
        differ.is_empty(),
        "the VM's dry run and the tree-walker's differ on:\n{}",
        differ.join("\n")
    );
}

#[test]
fn the_engine_is_chosen_by_flag_then_variable_then_config() {
    let script = root().join("bench/compute/hello.cig");
    let engine_of = |flag: Option<&str>, var: Option<&str>, config: Option<&str>| -> String {
        let tin = Tin::new();
        let cwd = tempfile::tempdir().expect("tempdir");
        fs::copy(&script, cwd.path().join("hello.cig")).expect("copy");
        let base = |cmd: &mut Command| {
            cmd.current_dir(cwd.path())
                .env("HOME", cwd.path())
                .env("CIGSCRIPT_HOME", tin.home())
                .env("CIG_KEYS_DIR", tin.dir.path().join("keys"))
                .env("XDG_CONFIG_HOME", tin.dir.path().join("config"))
                .env("CIG_SEAL_KEY", "file")
                .env("CIG_DOCTOR", "0")
                .env("NO_COLOR", "1")
                .env("CIG_INTERNAL_TRACKER_DIR", tin.dir.path().join("tracker"))
                .env(
                    "CIG_UPDATE_MANIFEST",
                    tin.dir.path().join("no-releases.json"),
                )
                .env_remove("CI")
                .env_remove("CIG_ENGINE");
        };
        if let Some(value) = config {
            let mut cmd = Command::new(env!("CARGO_BIN_EXE_cig"));
            base(&mut cmd);
            let out = cmd
                .args(["config", "engine", value])
                .output()
                .expect("config");
            assert!(out.status.success(), "cig config engine {value}: {out:?}");
        }
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cig"));
        base(&mut cmd);
        cmd.arg("run");
        if let Some(f) = flag {
            cmd.args(["--engine", f]);
        }
        if let Some(v) = var {
            cmd.env("CIG_ENGINE", v);
        }
        let out = cmd.arg("hello.cig").output().expect("run");
        assert!(out.status.success(), "{out:?}");
        let engines = tin.engines();
        assert_eq!(engines.len(), 1, "one run, one record");
        engines[0]["engine"].as_str().unwrap_or("?").to_string()
    };
    assert_eq!(engine_of(Some("vm"), Some("tree"), Some("tree")), "vm");
    assert_eq!(engine_of(Some("tree"), Some("vm"), Some("vm")), "tree");
    assert_eq!(engine_of(None, Some("vm"), Some("tree")), "vm");
    assert_eq!(engine_of(None, Some("tree"), Some("vm")), "tree");
    assert_eq!(engine_of(None, None, Some("vm")), "vm");
    assert_eq!(engine_of(None, None, Some("tree")), "tree");
}

#[test]
fn a_variable_that_names_no_engine_is_refused_before_anything_runs() {
    let tin = Tin::new();
    let cwd = tempfile::tempdir().expect("tempdir");
    fs::copy(
        root().join("bench/compute/hello.cig"),
        cwd.path().join("hello.cig"),
    )
    .expect("copy");
    let out = Command::new(env!("CARGO_BIN_EXE_cig"))
        .args(["run", "hello.cig"])
        .current_dir(cwd.path())
        .env("HOME", cwd.path())
        .env("CIGSCRIPT_HOME", tin.home())
        .env("CIG_KEYS_DIR", tin.dir.path().join("keys"))
        .env("XDG_CONFIG_HOME", tin.dir.path().join("config"))
        .env("CIG_SEAL_KEY", "file")
        .env("CIG_DOCTOR", "0")
        .env("NO_COLOR", "1")
        .env("CIG_ENGINE", "fast")
        .env_remove("CI")
        .output()
        .expect("run");
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "{err}");
    assert!(err.contains("CIG_ENGINE=`fast` is not an engine"), "{err}");
    assert!(err.contains("E800"), "{err}");
    assert!(out.stdout.is_empty(), "nothing ran");
    assert!(tin.engines().is_empty(), "and nothing was recorded");
}

#[test]
fn no_check_runs_on_the_tree_walker_and_the_record_says_why() {
    let tin = Tin::new();
    let cwd = tempfile::tempdir().expect("tempdir");
    fs::copy(
        root().join("bench/compute/hello.cig"),
        cwd.path().join("hello.cig"),
    )
    .expect("copy");
    let out = Command::new(env!("CARGO_BIN_EXE_cig"))
        .args(["run", "--engine", "vm", "--no-check", "hello.cig"])
        .current_dir(cwd.path())
        .env("HOME", cwd.path())
        .env("CIGSCRIPT_HOME", tin.home())
        .env("CIG_KEYS_DIR", tin.dir.path().join("keys"))
        .env("XDG_CONFIG_HOME", tin.dir.path().join("config"))
        .env("CIG_SEAL_KEY", "file")
        .env("CIG_DOCTOR", "0")
        .env("NO_COLOR", "1")
        .env_remove("CI")
        .env_remove("CIG_ENGINE")
        .output()
        .expect("run");
    assert!(out.status.success(), "{out:?}");
    let engines = tin.engines();
    assert_eq!(engines.len(), 1);
    assert_eq!(engines[0]["engine"], "tree");
    assert_eq!(engines[0]["asked"], "vm");
    assert!(
        engines[0]["why"]
            .as_str()
            .is_some_and(|w| w.contains("--no-check")),
        "{}",
        engines[0]
    );
}
