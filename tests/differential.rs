// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! #146: the faster evaluator prints what 1.20.0, the last release before it, printed. The corpus (the
//! error and resolution scripts under `tests/walker`, the step scripts under
//! `tests/steps` and the programs of the compute set that finish quickly)
//! runs under the 1.20.0 release and under this build, and the exit status,
//! standard output and standard error of each are compared.
//!
//! The 1.20.0 binary is `CIG_DIFF_BIN` when set, otherwise the Linux x86_64
//! release asset, fetched once into `target/` and accepted only when its
//! SHA-256 is the one pinned below. Offline, on another platform, or without
//! `curl`, the test says so and skips; `CIG_REQUIRE_DIFFERENTIAL=1` turns a
//! skip into a failure.

use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use wait_timeout::ChildExt;

const RELEASE_URL: &str =
    "https://github.com/otmof-ops/cig-script/releases/download/v1.20.0/cig-v1.20.0-linux-x86_64";
const RELEASE_SHA256: &str = "4649fda09236ada8a2ade2460c33db9cfa4b2f4dba40186099118a6a3b2dc8e3";

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

fn sha256_of(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    Some(hex::encode(Sha256::digest(bytes)))
}

/// The 1.20.0 binary, or why there is none.
fn old_binary() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("CIG_DIFF_BIN") {
        return Ok(PathBuf::from(p));
    }
    if !(cfg!(target_os = "linux") && cfg!(target_arch = "x86_64")) {
        return Err("the pinned 1.20.0 asset is the Linux x86_64 one".into());
    }
    let dir = root().join("target").join("differential");
    let bin = dir.join("cig-v1.20.0-linux-x86_64");
    if sha256_of(&bin).as_deref() == Some(RELEASE_SHA256) {
        return Ok(bin);
    }
    fs::create_dir_all(&dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
    let part = dir.join("download.part");
    let done = Command::new("curl")
        .args(["-fsSL", "--max-time", "120", "-o"])
        .arg(&part)
        .arg(RELEASE_URL)
        .status()
        .map_err(|e| format!("no curl: {e}"))?;
    if !done.success() {
        return Err("the release asset could not be fetched (offline?)".into());
    }
    if sha256_of(&part).as_deref() != Some(RELEASE_SHA256) {
        let _ = fs::remove_file(&part);
        return Err("the fetched asset does not match the pinned SHA-256".into());
    }
    fs::rename(&part, &bin).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&bin, fs::Permissions::from_mode(0o755));
    }
    Ok(bin)
}

/// Exit status, stdout and stderr of `cig run <script>` in a scratch home.
fn run(cig: &Path, script: &Path, budget: &str) -> String {
    let home = tempfile::tempdir().expect("tempdir");
    let name = script.file_name().expect("name");
    fs::copy(script, home.path().join(name)).expect("copy script");
    let mut child = Command::new(cig)
        .arg("run")
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
        .env("CIG_MAX_STEPS", budget)
        .env("CIG_INTERNAL_TRACKER_DIR", home.path().join("tracker"))
        .env("CIG_UPDATE_MANIFEST", home.path().join("no-releases.json"))
        .env_remove("CI")
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
    format!(
        "exit {}\n--- stdout\n{}--- stderr\n{}",
        status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
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

#[test]
fn differential_suite_against_1_20_0() {
    let old = match old_binary() {
        Ok(p) => p,
        Err(why) => {
            if std::env::var_os("CIG_REQUIRE_DIFFERENTIAL").is_some() {
                panic!("differential suite needs the 1.20.0 binary: {why}");
            }
            eprintln!("SKIPPED: differential suite against 1.20.0: {why}");
            return;
        }
    };
    let new = PathBuf::from(env!("CARGO_BIN_EXE_cig"));
    // The scripts written to fail run with a step budget (one of them never
    // ends); the compute programs run without one.
    let mut corpus: Vec<(PathBuf, &str)> = scripts_in("tests/walker", None)
        .into_iter()
        .chain(scripts_in("tests/steps", None))
        .map(|p| (p, "5000"))
        .collect();
    corpus.extend(
        scripts_in("bench/compute", Some(&COMPUTE))
            .into_iter()
            .map(|p| (p, "0")),
    );
    assert!(corpus.len() >= 20, "corpus is too small: {}", corpus.len());
    let mut differ = Vec::new();
    for (script, budget) in &corpus {
        if run(&old, script, budget) != run(&new, script, budget) {
            differ.push(script.display().to_string());
        }
    }
    assert!(
        differ.is_empty(),
        "output differs from 1.20.0 for: {differ:#?}"
    );
}
