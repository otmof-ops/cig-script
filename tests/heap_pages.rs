// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The musl build's heap starts in small pages (cig-compile#30; carton 0.2.0
//! rows 9 and 10).
//!
//! mimalloc 3, left alone, reserves a 1 GiB arena and asks the kernel to back
//! all of it with 2 MiB pages; a short program's few hundred KB were spread
//! over four of them, which took a compiled hello from 5.2 MB to 13.9 MB at
//! its peak and added about 0.7 ms to its start. `src/main.rs` maps the arena
//! itself before `main`, its first 8 MiB in 4 KiB pages and the rest in 2 MiB
//! pages, and turns transparent huge pages off only while mimalloc maps its
//! page map, never for what the program starts.
//!
//! Like `mmap_gate`, these run on a musl build (the Alpine CI job, or any
//! musl host), or on the binary `CIG_MMAP_BIN` names. `CIG_REQUIRE_MMAP_GATE=1`
//! turns a skip into a failure.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

/// A hello's peak resident set, dry run, on a musl build: 5.2 MB on cig
/// 1.20.0 (musl's own allocator), 13.9 MB on 1.21.0 as released, 6.3 MB
/// with the heap in small pages (Linux 6.17, THP `madvise`). The budget
/// leaves room for a kernel or a machine that touches a little more.
const HELLO_PEAK_BUDGET: u64 = 9 << 20;

/// The head of the arena in 4 KiB pages, as `src/main.rs` sets it.
const SMALL_HEAD: u64 = 8 << 20;

const HELLO: (&str, &str) = ("hello.cig", "exhale \"hello\"\n");

fn binary() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CIG_MMAP_BIN") {
        return Some(PathBuf::from(p));
    }
    if cfg!(all(target_env = "musl", target_pointer_width = "64")) {
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

/// `cig run --dry-run <name>` of `source` in a scratch home, as `program`
/// then `args` (strace and its flags, or nothing).
fn dry_run(dir: &Path, program: &Path, args: &[&std::ffi::OsStr], script: (&str, &str)) -> Command {
    run(dir, program, args, script, true)
}

/// `cig run [--dry-run] <name>` of `source` in a scratch home.
fn run(
    dir: &Path,
    program: &Path,
    args: &[&std::ffi::OsStr],
    (name, source): (&str, &str),
    dry: bool,
) -> Command {
    let script = dir.join(name);
    std::fs::write(&script, source).expect("write script");
    let mut c = Command::new(program);
    c.args(args).arg("run");
    if dry {
        c.arg("--dry-run");
    }
    c.arg(&script)
        .current_dir(dir)
        .env("HOME", dir)
        .env("CIGSCRIPT_HOME", dir.join("home"))
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_DATA_HOME", dir.join("data"))
        .env("XDG_STATE_HOME", dir.join("state"))
        .env("XDG_CACHE_HOME", dir.join("cache"))
        .env("CIG_SEAL_KEY", "file")
        .env("CIG_DOCTOR", "0")
        .env("CIG_REPORTS", "never")
        .env("NO_COLOR", "1")
        .env("CIG_INTERNAL_TRACKER_DIR", dir.join("tracker"))
        .env("CIG_UPDATE_MANIFEST", dir.join("no-releases.json"))
        .env_remove("CI")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    c
}

fn finish(child: &mut std::process::Child) {
    match child.wait_timeout(Duration::from_secs(60)).expect("wait") {
        Some(status) => assert!(status.success(), "hello failed"),
        None => {
            child.kill().ok();
            child.wait().ok();
            panic!("hello did not finish in 60 s");
        }
    }
}

#[test]
fn the_heaps_head_is_in_small_pages() {
    let required = std::env::var_os("CIG_REQUIRE_MMAP_GATE").is_some();
    let Some(cig) = binary() else {
        assert!(!required, "no musl binary to check (set CIG_MMAP_BIN)");
        eprintln!("heap pages check skipped: not a musl build and CIG_MMAP_BIN is not set");
        return;
    };
    if !have_strace() {
        assert!(!required, "strace is not installed");
        eprintln!("heap pages check skipped: strace is not installed");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let trace = dir.path().join("strace.txt");
    let args = [
        "-f".as_ref(),
        "-e".as_ref(),
        "trace=mmap,madvise,prctl".as_ref(),
        "-o".as_ref(),
        trace.as_os_str(),
        cig.as_os_str(),
    ];
    let mut child = dry_run(dir.path(), Path::new("strace"), &args, HELLO)
        .spawn()
        .expect("start strace");
    finish(&mut child);
    let text = std::fs::read_to_string(&trace).expect("strace output");
    let at = |needle: &str| text.lines().position(|l| l.contains(needle));
    let head = at(&format!(", {SMALL_HEAD}, MADV_NOHUGEPAGE)"));
    let tail = at(", MADV_HUGEPAGE)");
    assert!(
        head.is_some(),
        "the arena's first 8 MiB were not marked MADV_NOHUGEPAGE:\n{text}"
    );
    assert!(
        tail.is_none_or(|t| Some(t) > head),
        "a huge-page range was asked for before the head was marked:\n{text}"
    );
    assert!(
        !text.lines().any(|l| l.contains(", 1073741824, ")),
        "mimalloc reserved its own 1 GiB arena at start:\n{text}"
    );
    let off = at("PR_SET_THP_DISABLE, 1");
    let on = at("PR_SET_THP_DISABLE, 0");
    assert!(
        off.is_some() && on > off,
        "transparent huge pages were turned off and not back on before main:\n{text}"
    );
}

/// The switch that keeps mimalloc's page map out of huge pages is put back
/// before `main`: a hop's process has transparent huge pages as it would
/// under any other parent.
#[cfg(target_os = "linux")]
#[test]
fn a_hop_keeps_transparent_huge_pages() {
    let required = std::env::var_os("CIG_REQUIRE_MMAP_GATE").is_some();
    let Some(cig) = binary() else {
        assert!(!required, "no musl binary to check (set CIG_MMAP_BIN)");
        eprintln!("hop THP check skipped: not a musl build and CIG_MMAP_BIN is not set");
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let script = "burn {\n  exhale proc.text(\"grep\", [\"THP_enabled\", \"/proc/self/status\"], {reads: true})\n}\n";
    let out = run(dir.path(), &cig, &[], ("hop.cig", script), false)
        .stdout(Stdio::piped())
        .output()
        .expect("run cig");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "the hop script failed: {text}");
    assert!(
        text.contains("THP_enabled:\t1"),
        "a hop's process lost transparent huge pages: {text}"
    );
}

#[cfg(target_os = "linux")]
fn peak_of(cig: &Path, script: (&str, &str)) -> u64 {
    use std::os::unix::process::CommandExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let mut c = dry_run(dir.path(), cig, &[], script);
    // SAFETY: runs in the forked child before exec and makes only
    // async-signal-safe calls (open, write, close) on a constant path.
    unsafe {
        c.pre_exec(|| {
            let fd = libc::open(c"/proc/self/clear_refs".as_ptr(), libc::O_WRONLY);
            if fd >= 0 {
                libc::write(fd, b"5".as_ptr().cast(), 1);
                libc::close(fd);
            }
            Ok(())
        });
    }
    let mut child = c.spawn().expect("start cig");
    reap_with_peak(&mut child)
}

#[cfg(target_os = "linux")]
#[test]
fn a_hello_peaks_within_its_memory_budget() {
    let required = std::env::var_os("CIG_REQUIRE_MMAP_GATE").is_some();
    let Some(cig) = binary() else {
        assert!(!required, "no musl binary to check (set CIG_MMAP_BIN)");
        eprintln!("hello memory budget skipped: not a musl build and CIG_MMAP_BIN is not set");
        return;
    };
    let peak = peak_of(&cig, HELLO);
    assert!(
        peak <= HELLO_PEAK_BUDGET,
        "hello peaked at {:.1} MB, over its budget of {:.1} MB",
        peak as f64 / 1e6,
        HELLO_PEAK_BUDGET as f64 / 1e6
    );
}

/// Reap `child` with `wait4` and return its peak resident set in bytes;
/// fails the test if it did not exit 0.
#[cfg(target_os = "linux")]
fn reap_with_peak(child: &mut std::process::Child) -> u64 {
    let pid = child.id() as libc::pid_t;
    let mut status = 0;
    // SAFETY: a zeroed rusage is valid; wait4 writes only through the two
    // out-pointers, and the child is ours and not yet reaped.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let got = unsafe { libc::wait4(pid, &mut status, 0, &mut usage) };
    assert_eq!(got, pid, "wait4");
    assert!(
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
        "hello failed"
    );
    u64::try_from(usage.ru_maxrss).unwrap_or(0) * 1024
}
