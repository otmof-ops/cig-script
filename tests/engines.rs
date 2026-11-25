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
/// temporary folder is often reached through a link), written with either
/// separator or as JSON writes it (a record's error quotes paths), and its
/// run id.
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
        .flat_map(|n| [n.replace('\\', "/"), n.replace('\\', "\\\\"), n])
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

/// #259: `--engine auto` runs the script on the tree-walker on its first
/// run and leaves the lowering to a `__lower` subprocess that keeps the
/// module for the run after; the third run uses the VM. With
/// `CIG_AUTO_LOWER=wait` the second run's child is waited for, so this
/// test sleeps on nothing and leaves no process behind.
#[test]
fn auto_runs_on_the_tree_walker_twice_before_the_vm() {
    let script = root().join("bench/compute/hello.cig");
    let tin = Tin::new();
    let cwd = tempfile::tempdir().expect("tempdir");
    fs::copy(&script, cwd.path().join("hello.cig")).expect("copy");
    let base = |cmd: &mut Command| {
        cmd.current_dir(cwd.path())
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
            .env("CIG_INTERNAL_TRACKER_DIR", tin.dir.path().join("tracker"))
            .env(
                "CIG_UPDATE_MANIFEST",
                tin.dir.path().join("no-releases.json"),
            )
            .env_remove("CI")
            .env_remove("CIG_ENGINE");
    };
    let run = |label: &str| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cig"));
        base(&mut cmd);
        let out = cmd
            .args(["run", "--engine", "auto", "hello.cig"])
            .env("CIG_AUTO_LOWER", "wait")
            .output()
            .unwrap_or_else(|e| panic!("{label}: {e}"));
        assert!(out.status.success(), "{label}: {out:?}");
    };
    let modules = tin.home().join("tin").join("modules");
    // First run: tree-walker, a mark is written, no module yet.
    run("first");
    let engines = tin.engines();
    assert_eq!(engines.len(), 1);
    assert_eq!(engines[0]["engine"], "tree", "first: tree");
    assert!(engines[0]["asked"].is_null(), "auto is not `asked`");
    let (modules_now, marks_now) = count_modules_and_marks(&modules);
    assert_eq!(modules_now, 0, "no module after the first sighting");
    assert_eq!(marks_now, 1, "the mark is written");
    // Second run: tree-walker again, the child runs and keeps a
    // module, and clears the mark.
    run("second");
    let engines = tin.engines();
    assert_eq!(engines.len(), 2);
    assert_eq!(engines[1]["engine"], "tree", "second: still tree");
    let (modules_now, marks_now) = count_modules_and_marks(&modules);
    assert_eq!(modules_now, 1, "the second sighting keeps a module");
    assert_eq!(marks_now, 0, "and clears the mark");
    // Third run: the VM, module from the tin.
    run("third");
    let engines = tin.engines();
    assert_eq!(engines.len(), 3);
    assert_eq!(engines[2]["engine"], "vm");
    assert_eq!(engines[2]["module"], "cached");
}

/// Count the modules and the first-sighting marks under the tin's
/// modules folder, by filename.
fn count_modules_and_marks(modules: &Path) -> (usize, usize) {
    let mut n_mods = 0;
    let mut n_marks = 0;
    if modules.exists() {
        for shard in fs::read_dir(modules).unwrap().flatten() {
            if shard.file_type().unwrap().is_dir() {
                for f in fs::read_dir(shard.path()).unwrap().flatten() {
                    let name = f.file_name().to_string_lossy().to_string();
                    if name.len() == 64 {
                        n_mods += 1;
                    } else if name.ends_with(".seen") {
                        n_marks += 1;
                    }
                }
            }
        }
    }
    (n_mods, n_marks)
}

/// A script the lowering does not take (a name looked up at run time):
/// the first run under `auto` keeps a refusal in the cache, and the
/// second run reads it, runs on the tree-walker and never spawns another
/// lowering. The record carries the reason.
#[test]
fn auto_remembers_a_refusal_and_does_not_retry_it() {
    let script = root().join("tests/walker/resolve-shadow.cig");
    assert!(script.exists(), "the corpus still has this script");
    let tin = Tin::new();
    let cwd = tempfile::tempdir().expect("tempdir");
    let name = script.file_name().unwrap();
    fs::copy(&script, cwd.path().join(name)).expect("copy");
    let base = |cmd: &mut Command| {
        cmd.current_dir(cwd.path())
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
            .env("CIG_INTERNAL_TRACKER_DIR", tin.dir.path().join("tracker"))
            .env(
                "CIG_UPDATE_MANIFEST",
                tin.dir.path().join("no-releases.json"),
            )
            .env_remove("CI")
            .env_remove("CIG_ENGINE")
            .env("CIG_AUTO_LOWER", "wait");
    };
    // Three runs: the first writes the mark, the second spawns the
    // `wait` child which tries to lower and stores the refusal, the
    // third reads the refusal at lookup time and the record carries
    // its reason.
    for _ in 0..3 {
        let mut c = Command::new(env!("CARGO_BIN_EXE_cig"));
        base(&mut c);
        let _out = c
            .args(["run", "--engine", "auto", name.to_str().unwrap()])
            .output()
            .expect("run");
    }
    let engines = tin.engines();
    assert_eq!(engines.len(), 3);
    assert_eq!(engines[0]["engine"], "tree", "first: tree");
    assert_eq!(engines[1]["engine"], "tree", "second: still tree");
    assert_eq!(engines[2]["engine"], "tree", "third: still tree");
    assert!(
        engines[2]["why"]
            .as_str()
            .is_some_and(|w| w.contains("looked up as the script runs")
                || w.contains("lowering does not take it yet")),
        "the refusal is carried on the third run: {}",
        engines[2]
    );
}

/// The default path: `cig run` with no `--engine`, no `CIG_ENGINE` and
/// no config picks `auto`. Three runs: tree, tree + child keeps the
/// module, vm. This proves the default is `auto`, which no other test
/// does (every other new test sets the engine).
#[test]
fn the_default_engine_is_auto() {
    let script = root().join("bench/compute/hello.cig");
    let tin = Tin::new();
    let cwd = tempfile::tempdir().expect("tempdir");
    fs::copy(&script, cwd.path().join("hello.cig")).expect("copy");
    let base = |cmd: &mut Command| {
        cmd.current_dir(cwd.path())
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
            .env("CIG_AUTO_LOWER", "wait")
            .env("CIG_INTERNAL_TRACKER_DIR", tin.dir.path().join("tracker"))
            .env(
                "CIG_UPDATE_MANIFEST",
                tin.dir.path().join("no-releases.json"),
            )
            .env_remove("CI")
            .env_remove("CIG_ENGINE");
    };
    for _ in 0..3 {
        let mut c = Command::new(env!("CARGO_BIN_EXE_cig"));
        base(&mut c);
        let out = c.args(["run", "hello.cig"]).output().expect("run");
        assert!(out.status.success(), "{out:?}");
    }
    let engines = tin.engines();
    assert_eq!(engines.len(), 3, "three runs recorded");
    assert_eq!(engines[0]["engine"], "tree", "first: tree (default auto)");
    assert_eq!(engines[1]["engine"], "tree", "second: still tree");
    assert_eq!(engines[2]["engine"], "vm", "third: vm (default auto)");
    assert_eq!(engines[2]["module"], "cached");
}

/// `--no-check` always runs the tree-walker, even when a module is
/// kept for the script: `choose` is the authority, and the cache is
/// not read under `--no-check`.
#[test]
fn no_check_runs_on_the_tree_walker_even_with_a_kept_module() {
    let script = root().join("bench/compute/hello.cig");
    let tin = Tin::new();
    let cwd = tempfile::tempdir().expect("tempdir");
    fs::copy(&script, cwd.path().join("hello.cig")).expect("copy");
    let base = |cmd: &mut Command| {
        cmd.current_dir(cwd.path())
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
            .env("CIG_AUTO_LOWER", "off")
            .env("CIG_INTERNAL_TRACKER_DIR", tin.dir.path().join("tracker"))
            .env(
                "CIG_UPDATE_MANIFEST",
                tin.dir.path().join("no-releases.json"),
            )
            .env_remove("CI")
            .env_remove("CIG_ENGINE");
    };
    // Run once with `--engine vm` to keep a module.
    let mut c = Command::new(env!("CARGO_BIN_EXE_cig"));
    base(&mut c);
    let out = c
        .args(["run", "--engine", "vm", "hello.cig"])
        .output()
        .expect("warm");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(tin.engines()[0]["engine"], "vm");
    // Now run with `--no-check`: must run on the tree-walker.
    let mut c = Command::new(env!("CARGO_BIN_EXE_cig"));
    base(&mut c);
    let out = c
        .args(["run", "--engine", "vm", "--no-check", "hello.cig"])
        .output()
        .expect("no-check");
    assert!(out.status.success(), "{out:?}");
    let engines = tin.engines();
    assert_eq!(engines.len(), 2);
    assert_eq!(engines[1]["engine"], "tree", "no-check: tree");
    assert_eq!(engines[1]["asked"], "vm");
    assert!(
        engines[1]["why"]
            .as_str()
            .is_some_and(|w| w.contains("--no-check")),
        "{}",
        engines[1]
    );
    // And under `--engine auto` with no_check, the record says tree.
    let mut c = Command::new(env!("CARGO_BIN_EXE_cig"));
    base(&mut c);
    let out = c
        .args(["run", "--engine", "auto", "--no-check", "hello.cig"])
        .output()
        .expect("auto no-check");
    assert!(out.status.success(), "{out:?}");
    let engines = tin.engines();
    assert_eq!(engines[2]["engine"], "tree", "auto + no-check: tree");
}

/// Three runs under `auto` print, record and leave behind the same
/// thing as the tree-walker: a first sighting (tree + mark), a second
/// sighting (tree + the `wait` child keeps the module) and a third
/// (VM). This reuses the comparison the main
/// `a_run_on_the_vm_matches_the_tree_walker_cold_and_warm` test does,
/// against the `auto` engine.
#[test]
fn auto_matches_the_tree_walker_through_three_runs() {
    let script = root().join("bench/compute/hello.cig");
    let name = "bench/compute/hello.cig".to_string();
    let tree = run(&script, "0", "tree", false, &Tin::new());
    let tin = Tin::new();
    let first = run_auto(&script, "0", false, &tin);
    let second = run_auto(&script, "0", false, &tin);
    let third = run_auto(&script, "0", false, &tin);
    assert_eq!(tree, first, "first: {}", first_difference(&tree, &first));
    assert_eq!(tree, second, "second: {}", first_difference(&tree, &second));
    assert_eq!(tree, third, "third: {}", first_difference(&tree, &third));
    let engines = tin.engines();
    assert_eq!(engines[0]["engine"], "tree", "{name}: first = tree");
    assert_eq!(engines[1]["engine"], "tree", "{name}: second = still tree");
    assert_eq!(engines[2]["engine"], "vm", "{name}: third = vm");
    assert_eq!(engines[2]["module"], "cached");
}

/// Two `__lower` of one script spawned together keep one module and
/// leave no marker. Whichever gets the claim first keeps the module;
/// the other sees it under `lookup` after its own `claim_lowering` (or
/// is refused the claim outright) and exits.
#[test]
fn two_lowers_at_once_keep_one_module_and_leave_no_marker() {
    let script = root().join("bench/compute/hello.cig");
    let tin = Tin::new();
    let cwd = tempfile::tempdir().expect("tempdir");
    fs::copy(&script, cwd.path().join("hello.cig")).expect("copy");
    let base = |cmd: &mut Command| {
        cmd.current_dir(cwd.path())
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
            .env("CIG_INTERNAL_TRACKER_DIR", tin.dir.path().join("tracker"))
            .env(
                "CIG_UPDATE_MANIFEST",
                tin.dir.path().join("no-releases.json"),
            )
            .env_remove("CI")
            .env_remove("CIG_ENGINE");
    };
    let mut a = Command::new(env!("CARGO_BIN_EXE_cig"));
    base(&mut a);
    let child_a = a
        .args(["__lower", "hello.cig"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("a");
    let mut b = Command::new(env!("CARGO_BIN_EXE_cig"));
    base(&mut b);
    let child_b = b
        .args(["__lower", "hello.cig"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("b");
    for mut c in [child_a, child_b] {
        c.wait().expect("wait");
    }
    let modules = tin.home().join("tin").join("modules");
    let mut n_mods = 0;
    let mut n_markers = 0;
    if modules.exists() {
        for shard in fs::read_dir(&modules).unwrap().flatten() {
            if shard.file_type().unwrap().is_dir() {
                for f in fs::read_dir(shard.path()).unwrap().flatten() {
                    let name = f.file_name().to_string_lossy().to_string();
                    if name.len() == 64 {
                        n_mods += 1;
                    } else if name.ends_with(".lowering") {
                        n_markers += 1;
                    }
                }
            }
        }
    }
    assert_eq!(n_mods, 1, "one module kept");
    assert_eq!(n_markers, 0, "no marker left behind");
}

/// Mirrors [`run`] for `--engine auto` with `CIG_AUTO_LOWER=wait` so
/// the lowering finishes before the parent exits. No background work,
/// no sleeps.
fn run_auto(script: &Path, budget: &str, dry: bool, tin: &Tin) -> String {
    let cwd = tempfile::tempdir().expect("tempdir");
    let name = script.file_name().expect("name");
    fs::copy(script, cwd.path().join(name)).expect("copy script");
    let before: std::collections::BTreeSet<PathBuf> = fs::read_dir(tin.home().join("runs"))
        .map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cig"));
    cmd.arg("run").arg("--engine").arg("auto");
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
        .env("CIG_AUTO_LOWER", "wait")
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
    let status = child
        .wait_timeout(Duration::from_secs(120))
        .expect("wait")
        .expect("done");
    let out = child.wait_with_output().expect("output");
    let after: std::collections::BTreeSet<PathBuf> = fs::read_dir(tin.home().join("runs"))
        .map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    let made: Vec<PathBuf> = after.difference(&before).cloned().collect();
    format!(
        "exit {}\n--- stdout\n{}--- stderr\n{}--- files\n{}\n--- recorded\n{}\n",
        status.code().unwrap_or(-1),
        normal(&String::from_utf8_lossy(&out.stdout), cwd.path()),
        normal(&String::from_utf8_lossy(&out.stderr), cwd.path()),
        files_in(cwd.path()),
        recorded(&made, cwd.path())
    )
}

/// `CIG_AUTO_LOWER=off` under `--engine auto`: no `__lower` is started,
/// no module appears, and auto stays the tree-walker.
#[test]
fn auto_lower_off_starts_no_subprocess_and_keeps_no_module() {
    let script = root().join("bench/compute/hello.cig");
    let tin = Tin::new();
    let cwd = tempfile::tempdir().expect("tempdir");
    fs::copy(&script, cwd.path().join("hello.cig")).expect("copy");
    let out = Command::new(env!("CARGO_BIN_EXE_cig"))
        .args(["run", "--engine", "auto", "hello.cig"])
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
        .env("CIG_AUTO_LOWER", "off")
        .env("CIG_INTERNAL_TRACKER_DIR", tin.dir.path().join("tracker"))
        .env(
            "CIG_UPDATE_MANIFEST",
            tin.dir.path().join("no-releases.json"),
        )
        .env_remove("CI")
        .env_remove("CIG_ENGINE")
        .output()
        .expect("run");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(tin.engines()[0]["engine"], "tree");
    let modules = tin.home().join("tin").join("modules");
    assert!(
        !modules.exists() || fs::read_dir(&modules).map(|d| d.count()).unwrap_or(0) == 0,
        "no module was kept under {}",
        modules.display()
    );
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

/// A script reached through a symlink is lowered: the first two runs
/// on `auto` record `tree` (the first writes the mark, the second
/// keeps the module), and the third uses the VM. The real script is in
/// one folder, a link to it in another, and `cig run` is given the
/// link.
#[cfg(unix)]
#[test]
fn a_symlinked_script_is_lowered_for_the_next_run() {
    let real = tempfile::tempdir().expect("real");
    let link = tempfile::tempdir().expect("link");
    let script = real.path().join("hello.cig");
    std::fs::write(&script, "exhale \"hi\"\n").expect("write");
    let via = link.path().join("via.cig");
    std::os::unix::fs::symlink(&script, &via).expect("symlink");
    let tin = Tin::new();
    let base = |cmd: &mut Command| {
        cmd.current_dir(link.path())
            .env("HOME", link.path())
            .env("CIGSCRIPT_HOME", tin.home())
            .env("CIG_KEYS_DIR", tin.dir.path().join("keys"))
            .env("XDG_CONFIG_HOME", tin.dir.path().join("config"))
            .env("XDG_DATA_HOME", tin.dir.path().join("data"))
            .env("XDG_STATE_HOME", tin.dir.path().join("state"))
            .env("XDG_CACHE_HOME", tin.dir.path().join("cache"))
            .env("CIG_SEAL_KEY", "file")
            .env("CIG_DOCTOR", "0")
            .env("NO_COLOR", "1")
            .env("CIG_AUTO_LOWER", "wait")
            .env("CIG_INTERNAL_TRACKER_DIR", tin.dir.path().join("tracker"))
            .env(
                "CIG_UPDATE_MANIFEST",
                tin.dir.path().join("no-releases.json"),
            )
            .env_remove("CI")
            .env_remove("CIG_ENGINE");
    };
    for _ in 0..3 {
        let mut c = Command::new(env!("CARGO_BIN_EXE_cig"));
        base(&mut c);
        let out = c
            .args(["run", "--engine", "auto", "via.cig"])
            .output()
            .expect("run");
        assert!(out.status.success(), "{out:?}");
    }
    let engines = tin.engines();
    assert_eq!(engines.len(), 3);
    assert_eq!(engines[0]["engine"], "tree");
    assert_eq!(engines[1]["engine"], "tree");
    assert_eq!(engines[2]["engine"], "vm", "symlinked script was lowered");
}

/// A kept module whose body is damaged (bytes past its header) opens
/// no longer. Under `--engine vm` the run heals: it lowers fresh and
/// writes a new entry. Under `--engine auto` the parent runs on the
/// tree-walker and the `__lower` child (seeing the lookup's damaged
/// module) heals the entry for the next run.
#[test]
fn a_damaged_kept_module_heals_on_the_next_run() {
    let script = root().join("bench/compute/hello.cig");
    let tin = Tin::new();
    let cwd = tempfile::tempdir().expect("tempdir");
    fs::copy(&script, cwd.path().join("hello.cig")).expect("copy");
    let base = |cmd: &mut Command| {
        cmd.current_dir(cwd.path())
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
            .env("CIG_AUTO_LOWER", "wait")
            .env("CIG_INTERNAL_TRACKER_DIR", tin.dir.path().join("tracker"))
            .env(
                "CIG_UPDATE_MANIFEST",
                tin.dir.path().join("no-releases.json"),
            )
            .env_remove("CI")
            .env_remove("CIG_ENGINE");
    };
    // Warm: keep a module.
    let mut warm = Command::new(env!("CARGO_BIN_EXE_cig"));
    base(&mut warm);
    let out = warm
        .args(["run", "--engine", "vm", "hello.cig"])
        .output()
        .expect("warm");
    assert!(out.status.success());
    // Find and damage the body of the stored entry.
    let modules = tin.home().join("tin").join("modules");
    let mut entry = None;
    for shard in fs::read_dir(&modules).unwrap().flatten() {
        if shard.file_type().unwrap().is_dir() {
            for f in fs::read_dir(shard.path()).unwrap().flatten() {
                let name = f.file_name().to_string_lossy().to_string();
                if name.len() == 64 {
                    entry = Some(f.path());
                }
            }
        }
    }
    let entry = entry.expect("one module was kept");
    let mut bytes = fs::read(&entry).unwrap();
    // The entry's header is 128 bytes, the module's own header follows
    // the entry's for another 64 bytes; the body starts after that.
    // Flip a byte deep in the body.
    let at = bytes.len() - 1;
    bytes[at] ^= 0x01;
    fs::write(&entry, &bytes).expect("damage");
    // Under `--engine vm` the run heals: it lowers fresh and keeps a
    // new entry. The run itself succeeds.
    let mut vm = Command::new(env!("CARGO_BIN_EXE_cig"));
    base(&mut vm);
    let out = vm
        .args(["run", "--engine", "vm", "hello.cig"])
        .output()
        .expect("vm");
    assert!(out.status.success(), "vm healed: {out:?}");
    let engines = tin.engines();
    // Three records: the warm, the vm-heal, and nothing else yet.
    assert_eq!(engines.len(), 2);
    assert_eq!(engines[1]["engine"], "vm");
    assert_eq!(engines[1]["module"], "lowered", "vm healed and relowered");
    // Damage again, then run on `--engine auto` with the child in
    // `wait`: the parent uses the tree-walker (no VM open on a damaged
    // module), and the child heals the entry.
    let mut bytes = fs::read(&entry).unwrap();
    let at = bytes.len() - 1;
    bytes[at] ^= 0x01;
    fs::write(&entry, &bytes).expect("damage again");
    let mut au = Command::new(env!("CARGO_BIN_EXE_cig"));
    base(&mut au);
    let out = au
        .args(["run", "--engine", "auto", "hello.cig"])
        .output()
        .expect("auto heal");
    assert!(out.status.success());
    // The next run on `auto` finds the healed module.
    let mut next = Command::new(env!("CARGO_BIN_EXE_cig"));
    base(&mut next);
    let out = next
        .args(["run", "--engine", "auto", "hello.cig"])
        .output()
        .expect("auto next");
    assert!(out.status.success());
    let engines = tin.engines();
    assert_eq!(engines.len(), 4);
    assert_eq!(engines[3]["engine"], "vm", "auto healed between runs");
    assert_eq!(engines[3]["module"], "cached");
}

/// Build the sandbox env a short engine test needs. Returns a closure
/// that configures a `Command` the same way across tests.
fn with_env<'a>(
    cwd: &'a tempfile::TempDir,
    tin: &'a Tin,
    auto_lower: &'a str,
) -> impl Fn(&mut Command) + 'a {
    move |cmd: &mut Command| {
        cmd.current_dir(cwd.path())
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
            .env("CIG_AUTO_LOWER", auto_lower)
            .env("CIG_INTERNAL_TRACKER_DIR", tin.dir.path().join("tracker"))
            .env(
                "CIG_UPDATE_MANIFEST",
                tin.dir.path().join("no-releases.json"),
            )
            .env_remove("CI")
            .env_remove("CIG_ENGINE");
    }
}

/// `--engine vm`, `--engine tree` and `--no-check` on a new home leave
/// no mark: the mark belongs to `auto` alone. `--engine vm` after a
/// first `auto` run leaves one module and no mark (the run clears
/// what the first sighting wrote).
#[test]
fn other_engines_leave_no_mark_and_vm_clears_a_mark_auto_left() {
    let script = root().join("bench/compute/hello.cig");
    let modules_of = |tin: &Tin| tin.home().join("tin").join("modules");
    for engine in ["vm", "tree"] {
        let tin = Tin::new();
        let cwd = tempfile::tempdir().unwrap();
        fs::copy(&script, cwd.path().join("hello.cig")).unwrap();
        let base = with_env(&cwd, &tin, "off");
        let mut c = Command::new(env!("CARGO_BIN_EXE_cig"));
        base(&mut c);
        let out = c
            .args(["run", "--engine", engine, "hello.cig"])
            .output()
            .expect("run");
        assert!(out.status.success(), "{engine}: {out:?}");
        let (_, marks) = count_modules_and_marks(&modules_of(&tin));
        assert_eq!(marks, 0, "{engine} left a mark");
    }
    // --no-check (under vm, to hit the no_check branch) leaves no mark.
    {
        let tin = Tin::new();
        let cwd = tempfile::tempdir().unwrap();
        fs::copy(&script, cwd.path().join("hello.cig")).unwrap();
        let base = with_env(&cwd, &tin, "off");
        let mut c = Command::new(env!("CARGO_BIN_EXE_cig"));
        base(&mut c);
        let out = c
            .args(["run", "--engine", "vm", "--no-check", "hello.cig"])
            .output()
            .expect("run");
        assert!(out.status.success(), "{out:?}");
        let (_, marks) = count_modules_and_marks(&modules_of(&tin));
        assert_eq!(marks, 0, "--no-check left a mark");
    }
    // After a first `auto` run (a mark is written), `--engine vm`
    // keeps a module and clears the mark.
    let tin = Tin::new();
    let cwd = tempfile::tempdir().unwrap();
    fs::copy(&script, cwd.path().join("hello.cig")).unwrap();
    let base = with_env(&cwd, &tin, "wait");
    let mut a = Command::new(env!("CARGO_BIN_EXE_cig"));
    base(&mut a);
    assert!(a
        .args(["run", "--engine", "auto", "hello.cig"])
        .output()
        .unwrap()
        .status
        .success());
    let (mods, marks) = count_modules_and_marks(&modules_of(&tin));
    assert_eq!((mods, marks), (0, 1), "after auto: one mark, no module");
    let mut b = Command::new(env!("CARGO_BIN_EXE_cig"));
    base(&mut b);
    assert!(b
        .args(["run", "--engine", "vm", "hello.cig"])
        .output()
        .unwrap()
        .status
        .success());
    let (mods, marks) = count_modules_and_marks(&modules_of(&tin));
    assert_eq!(
        (mods, marks),
        (1, 0),
        "--engine vm kept a module and cleared the mark"
    );
}

/// A script the lowering refuses leaves no mark once the refusal is
/// kept: the child clears it as it stores the refusal.
#[test]
fn a_refusal_leaves_no_mark() {
    let script = root().join("tests/walker/resolve-shadow.cig");
    assert!(script.exists());
    let tin = Tin::new();
    let cwd = tempfile::tempdir().unwrap();
    let name = script.file_name().unwrap();
    fs::copy(&script, cwd.path().join(name)).unwrap();
    let base = with_env(&cwd, &tin, "wait");
    // Three runs: the second spawns the child that stores the refusal
    // and clears the mark.
    for _ in 0..3 {
        let mut c = Command::new(env!("CARGO_BIN_EXE_cig"));
        base(&mut c);
        c.args(["run", "--engine", "auto", name.to_str().unwrap()])
            .output()
            .expect("run");
    }
    let (_, marks) = count_modules_and_marks(&tin.home().join("tin").join("modules"));
    assert_eq!(marks, 0, "the refusal cleared the mark");
}

/// An edited script is seen for the first time again: the key is over
/// the bytes, so a run, an edit and a run keeps the first run's
/// behavior for the second (tree-walker, no module, a mark under the
/// new key).
#[test]
fn an_edited_script_is_seen_for_the_first_time_again() {
    let tin = Tin::new();
    let cwd = tempfile::tempdir().unwrap();
    let script = cwd.path().join("s.cig");
    fs::write(&script, "exhale \"hi\"\n").unwrap();
    let base = with_env(&cwd, &tin, "wait");
    let mut a = Command::new(env!("CARGO_BIN_EXE_cig"));
    base(&mut a);
    assert!(a
        .args(["run", "--engine", "auto", "s.cig"])
        .output()
        .unwrap()
        .status
        .success());
    let modules = tin.home().join("tin").join("modules");
    let (mods, marks) = count_modules_and_marks(&modules);
    assert_eq!((mods, marks), (0, 1), "first run: a mark, no module");
    fs::write(&script, "exhale \"hi2\"\n").unwrap();
    let mut b = Command::new(env!("CARGO_BIN_EXE_cig"));
    base(&mut b);
    assert!(b
        .args(["run", "--engine", "auto", "s.cig"])
        .output()
        .unwrap()
        .status
        .success());
    let (mods, marks) = count_modules_and_marks(&modules);
    assert_eq!(
        (mods, marks),
        (0, 2),
        "the edit keyed a new mark; the old one stays (another script's)"
    );
    let engines = tin.engines();
    assert_eq!(engines[1]["engine"], "tree");
}

/// A mark that cannot be made (the modules folder is read-only) leaves
/// the run's output, exit code and stderr as the tree-walker's.
/// Skipped when running as root, which is not refused by `chmod`.
#[cfg(unix)]
#[test]
fn a_run_whose_mark_cannot_be_made_still_runs_cleanly() {
    use std::os::unix::fs::PermissionsExt;
    // SAFETY: `geteuid` takes nothing and cannot fail.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let script = root().join("bench/compute/hello.cig");
    let tin = Tin::new();
    let cwd = tempfile::tempdir().unwrap();
    fs::copy(&script, cwd.path().join("hello.cig")).unwrap();
    // Make the modules folder and strip write permission.
    let modules = tin.home().join("tin").join("modules");
    fs::create_dir_all(&modules).unwrap();
    fs::set_permissions(&modules, fs::Permissions::from_mode(0o500)).unwrap();
    let base = with_env(&cwd, &tin, "wait");
    let auto_out = {
        let mut c = Command::new(env!("CARGO_BIN_EXE_cig"));
        base(&mut c);
        c.args(["run", "--engine", "auto", "hello.cig"])
            .output()
            .expect("auto")
    };
    let tree_tin = Tin::new();
    let tree_out = {
        let base2 = with_env(&cwd, &tree_tin, "off");
        let mut c = Command::new(env!("CARGO_BIN_EXE_cig"));
        base2(&mut c);
        c.args(["run", "--engine", "tree", "hello.cig"])
            .output()
            .expect("tree")
    };
    // Restore permissions so TempDir can clean up.
    fs::set_permissions(&modules, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(auto_out.status.success(), "the run ran cleanly");
    assert_eq!(auto_out.status.code(), tree_out.status.code());
    assert_eq!(auto_out.stdout, tree_out.stdout);
    // Nothing is said about the mark: what the run wrote to stderr is
    // what the tree-walker's wrote, the run id and the folder apart.
    assert_eq!(
        normal(&String::from_utf8_lossy(&auto_out.stderr), cwd.path()),
        normal(&String::from_utf8_lossy(&tree_out.stderr), cwd.path()),
    );
}

/// A damaged kept module under `auto` is lowered again at once (the
/// heal path), then runs on the VM. Unlike the existing
/// `a_damaged_kept_module_heals_on_the_next_run`, this one asks the
/// heal to come from `--engine auto`'s very first run after the
/// damage, with no first-sighting mark involved (the module was there
/// before).
#[test]
fn a_damaged_kept_module_under_auto_heals_at_once() {
    let script = root().join("bench/compute/hello.cig");
    let tin = Tin::new();
    let cwd = tempfile::tempdir().unwrap();
    fs::copy(&script, cwd.path().join("hello.cig")).unwrap();
    let base = with_env(&cwd, &tin, "wait");
    // Warm under `--engine vm` to keep a module (no mark).
    let mut w = Command::new(env!("CARGO_BIN_EXE_cig"));
    base(&mut w);
    assert!(w
        .args(["run", "--engine", "vm", "hello.cig"])
        .output()
        .unwrap()
        .status
        .success());
    // Damage the body.
    let modules = tin.home().join("tin").join("modules");
    let mut entry = None;
    for shard in fs::read_dir(&modules).unwrap().flatten() {
        if shard.file_type().unwrap().is_dir() {
            for f in fs::read_dir(shard.path()).unwrap().flatten() {
                let n = f.file_name().to_string_lossy().to_string();
                if n.len() == 64 {
                    entry = Some(f.path());
                }
            }
        }
    }
    let path = entry.expect("one module");
    let mut bytes = fs::read(&path).unwrap();
    let at = bytes.len() - 1;
    bytes[at] ^= 0x01;
    fs::write(&path, &bytes).unwrap();
    // First run under `auto`: the heal path takes over, the `wait`
    // child relowers, and the parent records tree.
    let mut a = Command::new(env!("CARGO_BIN_EXE_cig"));
    base(&mut a);
    assert!(a
        .args(["run", "--engine", "auto", "hello.cig"])
        .output()
        .unwrap()
        .status
        .success());
    // Second run under `auto`: the VM, module from the tin.
    let mut b = Command::new(env!("CARGO_BIN_EXE_cig"));
    base(&mut b);
    assert!(b
        .args(["run", "--engine", "auto", "hello.cig"])
        .output()
        .unwrap()
        .status
        .success());
    let engines = tin.engines();
    assert_eq!(engines.len(), 3);
    assert_eq!(engines[1]["engine"], "tree", "heal ran on tree");
    assert_eq!(engines[2]["engine"], "vm", "then on the vm");
    assert_eq!(engines[2]["module"], "cached");
}

/// A script given as `/dev/stdin` under `auto` runs on the tree-walker
/// each time, leaves no mark, and keeps no module.
#[cfg(unix)]
#[test]
fn a_dev_stdin_script_leaves_no_mark_and_no_module() {
    let tin = Tin::new();
    let cwd = tempfile::tempdir().unwrap();
    let base = with_env(&cwd, &tin, "wait");
    for _ in 0..2 {
        let mut c = Command::new(env!("CARGO_BIN_EXE_cig"));
        base(&mut c);
        // Feed the script on stdin.
        let mut child = c
            .args(["run", "--engine", "auto", "/dev/stdin"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn");
        {
            use std::io::Write;
            let mut stdin = child.stdin.take().expect("stdin");
            stdin.write_all(b"exhale \"hi\"\n").unwrap();
        }
        let out = child.wait_with_output().expect("wait");
        assert!(out.status.success(), "{out:?}");
    }
    let modules = tin.home().join("tin").join("modules");
    let (mods, marks) = count_modules_and_marks(&modules);
    assert_eq!((mods, marks), (0, 0), "no mark, no module for /dev/stdin");
    let engines = tin.engines();
    assert_eq!(engines.len(), 2);
    assert_eq!(engines[0]["engine"], "tree");
    assert_eq!(engines[1]["engine"], "tree");
}
