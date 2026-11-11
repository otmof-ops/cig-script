// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Compiled programs (src/program.rs, docs/PROGRAMS.md): a script appended
//! to this `cig` runs as the program, through the same run as `cig run`.
//! Each test builds its programs the way cigcompile does, from the `cig`
//! this test run built, and runs them beside `cig run`.

use cigscript::program::{self, Program};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

    fn write(&self, name: &str, content: &str) {
        fs::write(self.path().join(name), content).unwrap();
    }

    fn exists(&self, name: &str) -> bool {
        self.path().join(name).exists()
    }

    /// The program cigcompile would make from `script`: this cig with the
    /// script appended.
    fn compile(&self, name: &str, script: &str) -> PathBuf {
        let source = fs::read_to_string(self.path().join(script)).unwrap();
        self.make(&Program::new(name, script, &source, "test-compiler 0"))
    }

    fn make(&self, p: &Program) -> PathBuf {
        let runtime = fs::read(env!("CARGO_BIN_EXE_cig")).unwrap();
        let file = format!("{}{}", p.name, std::env::consts::EXE_SUFFIX);
        let out = self.path().join("bin").join(file);
        fs::create_dir_all(out.parent().unwrap()).unwrap();
        fs::write(&out, program::append(&runtime, p)).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&out, fs::Permissions::from_mode(0o755)).unwrap();
        }
        out
    }

    fn command(&self, exe: &Path) -> Command {
        let mut c = Command::new(exe);
        c.current_dir(self.path())
            .env("CIGSCRIPT_HOME", self.path().join("home"))
            .env("NO_COLOR", "1")
            .env("CIG_SEAL_KEY", "file")
            .env("CIG_DOCTOR", "0")
            .env("CIG_REPORTS", "never")
            .env("CIG_INTERNAL_TRACKER_DIR", self.path().join("tracker"))
            .env("CIG_UPDATE_MANIFEST", self.path().join("no-releases.json"))
            .env_remove("CI")
            .env_remove(program::SELF_ENV);
        c
    }

    fn cig(&self, args: &[&str]) -> Output {
        self.command(Path::new(env!("CARGO_BIN_EXE_cig")))
            .args(args)
            .output()
            .expect("run cig")
    }

    fn prog(&self, exe: &Path, args: &[&str]) -> Output {
        output(self.command(exe).args(args)).expect("run the program")
    }
}

/// Run a program just written. A multithreaded test process can hit
/// ETXTBSY: another test's fork still holds the file open for writing
/// between its fork and its exec. That passes in a moment; try again.
fn output(c: &mut Command) -> std::io::Result<Output> {
    for _ in 0..80 {
        match c.output() {
            Err(e) if cfg!(unix) && e.raw_os_error() == Some(26) => {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            other => return other,
        }
    }
    c.output()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).to_string()
}

/// Run ids differ between two runs; nothing else may.
fn normal(s: &str) -> String {
    let re = regex::Regex::new(r"\d{8}T\d{6}-[0-9a-f]{6}").unwrap();
    re.replace_all(s, "<run>").to_string()
}

fn same(a: &Output, b: &Output, what: &str) {
    assert_eq!(
        a.status.code(),
        b.status.code(),
        "{what}: exit codes differ\ncig: {}\nprogram: {}",
        text(&a.stderr),
        text(&b.stderr)
    );
    assert_eq!(
        normal(&text(&a.stdout)),
        normal(&text(&b.stdout)),
        "{what}: stdout differs"
    );
    assert_eq!(
        normal(&text(&a.stderr)),
        normal(&text(&b.stderr)),
        "{what}: stderr differs"
    );
}

fn last_run(s: &Sandbox) -> String {
    let out = s.cig(&["runs", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    let runs = v.as_array().cloned().unwrap_or_default();
    runs.iter()
        .filter_map(|r| r["id"].as_str().map(str::to_string))
        .max()
        .unwrap_or_else(|| panic!("no run recorded: {}", text(&out.stdout)))
}

#[test]
fn the_cig_binary_says_its_version_in_its_bytes() {
    let bytes = fs::read(env!("CARGO_BIN_EXE_cig")).unwrap();
    assert_eq!(
        program::runtime_version(&bytes).as_deref(),
        Some(cigscript::VERSION),
        "the runtime mark is missing from the built cig (a release build's LTO may have dropped it)"
    );
}

#[test]
fn a_program_says_what_cig_run_says() {
    let s = Sandbox::new();
    s.write(
        "hello.cig",
        "stick name = args.get(0, \"world\")\nexhale \"hello, ${name}\"\nfor a in args {\n  exhale \"arg ${a}\"\n}\n",
    );
    let exe = s.compile("hello", "hello.cig");
    same(
        &s.cig(&["run", "hello.cig"]),
        &s.prog(&exe, &[]),
        "no arguments",
    );
    same(
        &s.cig(&["run", "hello.cig", "--", "you", "--dry-run"]),
        &s.prog(&exe, &["you", "--dry-run"]),
        "arguments, a flag-shaped one after the first",
    );
    same(
        &s.cig(&["run", "hello.cig", "--", "--plain"]),
        &s.prog(&exe, &["--", "--plain"]),
        "after --",
    );
    let out = s.prog(&exe, &["you"]);
    assert_eq!(text(&out.stdout), "hello, you\narg you\n");
}

#[test]
fn an_error_points_at_the_compiled_file_and_exits_like_cig_run() {
    let s = Sandbox::new();
    s.write(
        "bad.cig",
        "exhale \"one\"\nstick n = 1 / 0\nexhale \"never\"\n",
    );
    let exe = s.compile("bad", "bad.cig");
    let (c, p) = (s.cig(&["run", "bad.cig"]), s.prog(&exe, &[]));
    same(&c, &p, "a runtime error");
    assert_eq!(p.status.code(), Some(1));
    assert!(
        text(&p.stderr).contains("bad.cig:2:"),
        "{}",
        text(&p.stderr)
    );
    s.write("exit.cig", "exhale \"bye\"\nexit(4)\n");
    let exe = s.compile("exit", "exit.cig");
    same(&s.cig(&["run", "exit.cig"]), &s.prog(&exe, &[]), "exit(4)");
}

#[test]
fn a_failed_burn_rolls_back_the_same_way_and_a_dry_run_plans_the_same() {
    let s = Sandbox::new();
    s.write(
        "fail.cig",
        "burn {\n  fs.write_text(\"made.txt\", \"x\")\n  cough \"no\"\n}\n",
    );
    let exe = s.compile("fail", "fail.cig");
    let c = s.cig(&["run", "fail.cig"]);
    assert!(!s.exists("made.txt"), "cig run did not roll back");
    let p = s.prog(&exe, &[]);
    assert!(!s.exists("made.txt"), "the program did not roll back");
    same(&c, &p, "a failed burn");
    assert!(
        text(&p.stderr).contains("unburn: rolling back"),
        "{}",
        text(&p.stderr)
    );
    s.write(
        "plan.cig",
        "burn {\n  fs.write_text(\"p.txt\", \"x\")\n  fs.mkdir(\"d\")\n}\n",
    );
    let exe = s.compile("plan", "plan.cig");
    same(
        &s.cig(&["run", "--dry-run", "plan.cig"]),
        &s.prog(&exe, &["--dry-run"]),
        "a dry run",
    );
    assert!(!s.exists("p.txt") && !s.exists("d"), "a dry run burned");
}

#[test]
fn cig_runs_names_the_program_and_unburn_puts_it_back() {
    let s = Sandbox::new();
    s.write("w.cig", "burn {\n  fs.write_text(\"w.txt\", \"new\")\n}\n");
    s.write("w.txt", "old");
    let exe = s.compile("writer", "w.cig");
    let out = s.prog(&exe, &[]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(fs::read_to_string(s.path().join("w.txt")).unwrap(), "new");
    let id = last_run(&s);
    let list = text(&s.cig(&["runs"]).stdout);
    assert!(list.contains("writer (program)"), "{list}");
    let shown = text(&s.cig(&["runs", &id]).stdout);
    assert!(
        shown.contains("program   writer  (compiled from w.cig"),
        "{shown}"
    );
    assert!(shown.contains("by test-compiler 0"), "{shown}");
    // The program puts back its own run.
    let back = s.prog(&exe, &["--unburn", &id]);
    assert!(back.status.success(), "{}", text(&back.stderr));
    assert_eq!(fs::read_to_string(s.path().join("w.txt")).unwrap(), "old");
}

#[test]
fn a_program_unburns_its_own_runs_only() {
    let s = Sandbox::new();
    s.write(
        "mine.cig",
        "burn {\n  fs.write_text(\"mine.txt\", \"m\")\n}\n",
    );
    s.write(
        "theirs.cig",
        "burn {\n  fs.write_text(\"theirs.txt\", \"t\")\n}\n",
    );
    let exe = s.compile("mine", "mine.cig");
    assert!(s.cig(&["run", "theirs.cig"]).status.success());
    let theirs = last_run(&s);
    let out = s.prog(&exe, &["--unburn", &theirs]);
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("E1053"), "{}", text(&out.stderr));
    assert!(s.exists("theirs.txt"), "another run was put back");
}

#[test]
fn a_damaged_program_and_one_for_another_cig_refuse_to_run() {
    let s = Sandbox::new();
    s.write("x.cig", "burn {\n  fs.write_text(\"x.txt\", \"x\")\n}\n");
    let exe = s.compile("x", "x.cig");
    let mut bytes = fs::read(&exe).unwrap();
    let at = bytes.len() - program::TRAILER_LEN as usize - 10;
    bytes[at] ^= 1;
    fs::write(&exe, &bytes).unwrap();
    let out = s.prog(&exe, &[]);
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("E1050"), "{}", text(&out.stderr));
    assert!(!s.exists("x.txt"), "a damaged program ran");
    let mut p = Program::new("old", "x.cig", "exhale \"x\"\n", "test-compiler 0");
    p.cig = "0.0.1".to_string();
    let exe = s.make(&p);
    let out = s.prog(&exe, &[]);
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("E1051"), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("compiled for cig 0.0.1"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn the_self_call_marker_never_turns_a_program_into_cig() {
    let s = Sandbox::new();
    s.write("a.cig", "exhale \"args: ${args.join(\" \")}\"\n");
    let exe = s.compile("a", "a.cig");
    for words in [
        &["update"][..],
        &["run", "evil.cig"],
        &["explain", "E1050"],
        &["crash", "flush"],
    ] {
        let out = output(s.command(&exe).env(program::SELF_ENV, "1").args(words)).unwrap();
        assert_eq!(
            text(&out.stdout),
            format!("args: {}\n", words.join(" ")),
            "{words:?}: {}",
            text(&out.stderr)
        );
    }
}

#[test]
fn a_program_takes_its_run_flags_and_refuses_a_mistyped_one() {
    let s = Sandbox::new();
    s.write("f.cig", "stick n = 1\nexhale \"ran\"\n");
    let exe = s.compile("f", "f.cig");
    let info = s.prog(&exe, &["--cig-program", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&info.stdout).unwrap();
    assert_eq!(v["name"], "f");
    assert_eq!(v["file"], "f.cig");
    assert_eq!(v["cig"], cigscript::VERSION);
    assert!(v.get("source").is_none(), "the summary carries no source");
    let help = text(&s.prog(&exe, &["--help"]).stdout);
    assert!(
        help.contains("--dry-run") && help.contains("--unburn <run>"),
        "{help}"
    );
    let explain = s.prog(&exe, &["--explain", "E1050"]);
    assert!(explain.status.success(), "{}", text(&explain.stderr));
    assert!(
        text(&explain.stdout).contains("E1050"),
        "{}",
        text(&explain.stdout)
    );
    let bad = s.prog(&exe, &["--dry-rn"]);
    assert_eq!(bad.status.code(), Some(3));
    assert!(text(&bad.stderr).contains("E1052"), "{}", text(&bad.stderr));
    assert!(
        text(&bad.stdout).is_empty(),
        "a refused flag ran the program"
    );
}

#[test]
fn a_program_does_not_lint_itself_on_every_start() {
    let s = Sandbox::new();
    // An effect in a function, outside any burn: a warning, never an error.
    s.write(
        "lint.cig",
        "pull f() {\n  fs.write_text(\"x.txt\", \"y\")\n}\nexhale \"ok\"\n",
    );
    let exe = s.compile("lint", "lint.cig");
    let c = s.cig(&["run", "lint.cig"]);
    let p = s.prog(&exe, &[]);
    assert!(
        text(&c.stderr).contains("warning["),
        "cig run gave no warning to compare against: {}",
        text(&c.stderr)
    );
    assert_eq!(c.status.code(), p.status.code());
    assert_eq!(text(&c.stdout), text(&p.stdout));
    assert!(!text(&p.stderr).contains("warning["), "{}", text(&p.stderr));
}

/// A program's boxed hop with `network: "registry"`: the box starts the
/// network forwarder from the program's own binary, marked as cig's call to
/// itself, and the binary answers it as `cig`. The allowed host answers,
/// the other is refused, as for `cig run`.
#[cfg(target_os = "linux")]
#[test]
fn a_programs_registry_hop_gets_the_forwarder_from_its_own_binary() {
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
    let s = Sandbox::new();
    fs::create_dir_all(s.path().join("p")).unwrap();
    s.write("p/keep.txt", "v1\n");
    s.write(
        "p/probe.py",
        &format!(
            "import http.client\nfor port in ({allowed}, {refused}):\n    try:\n        c = http.client.HTTPConnection('127.0.0.1', 38080, timeout=5)\n        c.set_tunnel('127.0.0.1', port)\n        c.request('GET', '/')\n        r = c.getresponse()\n        print(port, r.status, r.read().decode())\n    except Exception as e:\n        print(port, 'ERR', e)\n"
        ),
    );
    s.write(
        "reg.cig",
        &format!(
            "pack {{ \"./p\" }}\nburn {{\n  proc.run(\"sh\", [\"-c\", \"python3 probe.py > reg.txt 2>&1\"], {{cwd: \"p\", box: {{network: \"registry\", allow_hosts: [\"127.0.0.1:{allowed}\"]}}}})\n}}\n"
        ),
    );
    let exe = s.compile("reg", "reg.cig");
    let out = s.prog(&exe, &[]);
    let err = text(&out.stderr);
    if !out.status.success() && err.contains("E777") {
        eprintln!("the box cannot be made here; skipped: {err}");
        return;
    }
    assert!(out.status.success(), "{err}");
    let reg = fs::read_to_string(s.path().join("p/reg.txt")).unwrap();
    assert!(
        reg.contains(&format!("{allowed} 200 hello from the registry")),
        "{reg}\n{err}"
    );
    assert!(
        reg.contains(&format!("{refused} ERR")) && reg.contains("403"),
        "{reg}"
    );
    assert!(err.contains(&format!("127.0.0.1:{refused}")), "{err}");
}

/// The cig binary's headers place its image exactly at its file's end, so
/// a payload appended to it starts there (#135).
#[test]
fn the_cig_binarys_image_ends_where_its_file_does() {
    let path = Path::new(env!("CARGO_BIN_EXE_cig"));
    let len = fs::metadata(path).unwrap().len();
    let image = program::image_of(&mut fs::File::open(path).unwrap(), len)
        .expect("the cig binary's headers read");
    assert_eq!(image.end, len);
}

/// A program cut short, or with bytes after its trailer, is damaged; it
/// never runs as `cig` (#135). Zero padding after the trailer is fine.
#[test]
fn a_cut_or_extended_program_is_damaged_never_cig() {
    let s = Sandbox::new();
    s.write("x.cig", "burn {\n  fs.write_text(\"x.txt\", \"x\")\n}\n");
    let exe = s.compile("x", "x.cig");
    let whole = fs::read(&exe).unwrap();
    let mut cases: Vec<(String, Vec<u8>)> = [1usize, 48, 200]
        .iter()
        .map(|cut| {
            (
                format!("{cut} bytes cut"),
                whole[..whole.len() - cut].to_vec(),
            )
        })
        .collect();
    let mut junk = whole.clone();
    junk.extend_from_slice(b"XX");
    cases.push(("XX after it".to_string(), junk));
    // Each case is a file of its own: macOS keeps a binary's code-signing
    // state by file, and one rewritten in place after it ran can be killed
    // when it is started again.
    let place = |n: usize, bytes: &[u8]| {
        let p = exe.with_file_name(format!("x{n}{}", std::env::consts::EXE_SUFFIX));
        fs::write(&p, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        }
        p
    };
    for (n, (what, bytes)) in cases.into_iter().enumerate() {
        let exe = place(n, &bytes);
        let out = s.prog(&exe, &["run", "x.cig"]);
        let err = text(&out.stderr);
        assert_eq!(out.status.code(), Some(3), "{what}: {err}");
        assert!(err.contains("E1050"), "{what}: {err}");
        assert!(
            !err.contains("unrecognized subcommand"),
            "{what}: ran as cig: {err}"
        );
        assert!(!s.exists("x.txt"), "{what}: the damaged program ran");
    }
    let mut padded = whole.clone();
    padded.extend_from_slice(&[0; 512]);
    let exe = place(99, &padded);
    let out = s.prog(&exe, &[]);
    assert!(out.status.success(), "padded: {}", text(&out.stderr));
    assert!(s.exists("x.txt"), "the padded program did not run");
}

#[cfg(unix)]
fn mode(p: &Path, m: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(m)).unwrap();
}

#[cfg(unix)]
fn root() -> bool {
    // SAFETY: a plain id query.
    unsafe { libc::geteuid() == 0 }
}

/// Installed execute-only and run by someone who cannot read it, a program
/// cannot find its script; it refuses rather than act as `cig` (#136). A
/// binary named `cig` is `cig` either way.
#[cfg(unix)]
#[test]
fn an_unreadable_program_refuses_and_an_unreadable_cig_is_cig() {
    if root() {
        eprintln!("skipped: root reads any file");
        return;
    }
    let s = Sandbox::new();
    s.write("x.cig", "exhale \"the program ran\"\n");
    s.write(
        "evil.cig",
        "burn {\n  fs.write_text(\"evil.txt\", \"x\")\n}\n",
    );
    let exe = s.compile("x", "x.cig");
    mode(&exe, 0o111);
    let out = match output(s.command(&exe).args(["run", "evil.cig"])) {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("skipped: this system does not run execute-only files");
            return;
        }
        Err(e) => panic!("{e}"),
    };
    let err = text(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "{err}");
    assert!(err.contains("E1055"), "{err}");
    assert!(
        !s.exists("evil.txt"),
        "an unreadable program ran a cig command"
    );
    let cig = s.path().join("cig");
    fs::copy(env!("CARGO_BIN_EXE_cig"), &cig).unwrap();
    mode(&cig, 0o111);
    let out = output(s.command(&cig).arg("--version")).unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).starts_with("cig "),
        "{}",
        text(&out.stdout)
    );
}

/// Root-owned and setuid, setgid or with file capabilities, a program run by
/// someone else refuses before it reads their environment (#134). These
/// need sudo, so they run in CI, where the runners have it.
#[cfg(unix)]
#[test]
fn a_program_with_raised_rights_refuses() {
    let sudo = |args: &[&str]| {
        Command::new("sudo")
            .arg("-n")
            .args(args)
            .status()
            .is_ok_and(|s| s.success())
    };
    if std::env::var_os("CI").is_none() || root() || !sudo(&["true"]) {
        eprintln!("skipped: needs CI, a user that is not root, and sudo");
        return;
    }
    let s = Sandbox::new();
    // A path cig would find on PATH: a program run setuid must never run it.
    let fake = s.path().join("fakebin");
    fs::create_dir_all(&fake).unwrap();
    fs::write(
        fake.join("date"),
        "#!/bin/sh\necho ran > \"$PWD/fake-ran.txt\"\n",
    )
    .unwrap();
    mode(&fake.join("date"), 0o755);
    s.write(
        "stamp.cig",
        "burn {\n  stick d = proc.run(\"date\", [], {})\n  fs.write_text(\"stamp.txt\", d.out)\n}\n",
    );
    let mut raised = vec![("setuid", "4755"), ("setgid", "2755")];
    let caps = cfg!(target_os = "linux")
        && Command::new("sh")
            .args(["-c", "command -v setcap"])
            .output()
            .is_ok_and(|o| o.status.success());
    if caps {
        raised.push(("capabilities", ""));
    }
    for (what, bits) in raised {
        let exe = s.compile(what, "stamp.cig");
        let p = exe.to_str().unwrap();
        assert!(sudo(&["chown", "0:0", p]), "{what}: chown");
        if bits.is_empty() {
            assert!(
                sudo(&["setcap", "cap_dac_read_search+ep", p]),
                "{what}: setcap"
            );
        } else {
            assert!(sudo(&["chmod", bits, p]), "{what}: chmod");
        }
        let path = format!(
            "{}:{}",
            fake.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let out = output(
            s.command(&exe)
                .env("PATH", path)
                .env("CIGSCRIPT_HOME", s.path().join("callers-home")),
        )
        .unwrap();
        let err = text(&out.stderr);
        assert_eq!(out.status.code(), Some(3), "{what}: {err}");
        assert!(err.contains("E1054"), "{what}: {err}");
        assert!(!s.exists("fake-ran.txt"), "{what}: the caller's date ran");
        assert!(!s.exists("stamp.txt"), "{what}: the program ran");
        assert!(
            !s.exists("callers-home"),
            "{what}: the caller's home was used"
        );
    }
}

/// #149: a program puts back a stretch of its own runs, newest first, and
/// leaves every other run alone: another program's, and `cig run`'s.
#[test]
fn a_program_puts_back_a_stretch_of_its_own_runs() {
    let s = Sandbox::new();
    s.write(
        "w.cig",
        "burn {\n  fs.append_text(\"log.txt\", args.get(0, \"-\") + \"\\n\")\n}\n",
    );
    s.write(
        "other.cig",
        "burn {\n  fs.write_text(\"other.txt\", \"theirs\")\n}\n",
    );
    let exe = s.compile("w", "w.cig");
    for k in ["1", "2", "3"] {
        let out = s.prog(&exe, &[k]);
        assert!(out.status.success(), "{}", text(&out.stderr));
    }
    // Someone else's run in between, newer than all of ours.
    let other = s.cig(&["run", "other.cig"]);
    assert!(other.status.success(), "{}", text(&other.stderr));
    let plan = s.prog(&exe, &["--unburn-last", "2", "--dry-run"]);
    assert!(
        text(&plan.stderr).contains("undo 2 runs"),
        "{}",
        text(&plan.stderr)
    );
    let out = s.prog(&exe, &["--unburn-last", "2"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(fs::read_to_string(s.path().join("log.txt")).unwrap(), "1\n");
    assert_eq!(
        fs::read_to_string(s.path().join("other.txt")).unwrap(),
        "theirs",
        "another run was put back"
    );
    // --unburn-since takes what is left of its own; an age that is not one is refused.
    let out = s.prog(&exe, &["--unburn-since=1h"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(!s.exists("log.txt"));
    assert!(s.exists("other.txt"));
    let bad = s.prog(&exe, &["--unburn-since", "soon"]);
    assert_eq!(bad.status.code(), Some(3));
    assert!(text(&bad.stderr).contains("E1052"), "{}", text(&bad.stderr));
}
