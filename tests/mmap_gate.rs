// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! #146: the musl build's allocator is mimalloc, and a script that
//! allocates in a loop must not keep asking the kernel for memory.
//!
//! musl's own allocator maps and unmaps on the way (10,305 `mmap` calls for
//! the n-body benchmark, against 11 with mimalloc), which is what made the
//! static release binary 2 to 5 times slower than the glibc one. This gate
//! counts the calls with `strace` for the same script at two sizes: the
//! count must stay under a fixed ceiling, and four times the work must not
//! add more than a handful.
//!
//! It runs on a musl build (the Alpine CI job, or any musl host). To gate a
//! musl binary built on another host: `CIG_MMAP_BIN=<that cig> cargo test
//! --test mmap_gate`. `CIG_REQUIRE_MMAP_GATE=1` turns a skipped gate (no
//! musl binary, or no `strace`) into a failure.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

/// The most memory-mapping calls a whole run may make, start-up included.
/// mimalloc measures 11; musl's allocator measures 160 for `hello` alone.
const CEILING: usize = 48;
/// What four times the work may add: none in steady state, a few for a
/// larger arena.
const GROWTH: usize = 8;

const SCRIPT: &str = "\
roll total = 0
for i in 0..N {
  roll xs = [i, i + 1, i + 2]
  roll m = {a: i, b: xs}
  total += len(xs) + len(m.b)
}
exhale total
";

fn binary() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CIG_MMAP_BIN") {
        return Some(PathBuf::from(p));
    }
    if cfg!(target_env = "musl") {
        return Some(PathBuf::from(env!("CARGO_BIN_EXE_cig")));
    }
    None
}

fn have_strace() -> bool {
    Command::new("strace")
        .arg("-V")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Memory-mapping calls (`mmap`, `munmap`, `mremap`) made by one run of
/// `script`, under a hard timeout.
fn map_calls(cig: &PathBuf, n: u64) -> usize {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("loop.cig");
    std::fs::write(&script, SCRIPT.replace('N', &n.to_string())).expect("write script");
    let trace = dir.path().join("strace.txt");
    let mut child = Command::new("strace")
        .args(["-f", "-e", "trace=mmap,munmap,mremap", "-o"])
        .arg(&trace)
        .arg(cig)
        .arg("run")
        .arg(&script)
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .env("CIGSCRIPT_HOME", dir.path().join("home"))
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("XDG_CACHE_HOME", dir.path().join("cache"))
        .env("CIG_SEAL_KEY", "file")
        .env("CIG_DOCTOR", "0")
        .env("CIG_MAX_STEPS", "0")
        .env("NO_COLOR", "1")
        .env("CIG_INTERNAL_TRACKER_DIR", dir.path().join("tracker"))
        .env("CIG_UPDATE_MANIFEST", dir.path().join("no-releases.json"))
        .env_remove("CI")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start strace");
    match child.wait_timeout(Duration::from_secs(120)).expect("wait") {
        Some(status) => assert!(status.success(), "the script failed under strace"),
        None => {
            child.kill().ok();
            child.wait().ok();
            panic!("the script did not finish in 120 s");
        }
    }
    let text = std::fs::read_to_string(&trace).expect("strace output");
    text.lines()
        .filter(|l| {
            let call = l.split_whitespace().nth(1).unwrap_or("");
            call.starts_with("mmap(") || call.starts_with("munmap(") || call.starts_with("mremap(")
        })
        .count()
}

#[test]
fn a_steady_state_script_makes_no_new_mmap_calls() {
    let required = std::env::var_os("CIG_REQUIRE_MMAP_GATE").is_some();
    let Some(cig) = binary() else {
        assert!(!required, "no musl binary to gate (set CIG_MMAP_BIN)");
        eprintln!("mmap gate skipped: not a musl build and CIG_MMAP_BIN is not set");
        return;
    };
    if !have_strace() {
        assert!(!required, "strace is not installed");
        eprintln!("mmap gate skipped: strace is not installed");
        return;
    }
    let small = map_calls(&cig, 20_000);
    let large = map_calls(&cig, 80_000);
    assert!(
        small <= CEILING && large <= CEILING,
        "mmap-family calls: {small} (20k iterations) and {large} (80k), over the ceiling of {CEILING}; is the global allocator mimalloc?"
    );
    assert!(
        large <= small + GROWTH,
        "4x the work made {} more mmap-family calls ({small} -> {large}): the script is not at steady state",
        large - small
    );
}
