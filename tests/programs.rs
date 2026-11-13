// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Compiled programs (src/program.rs, docs/PROGRAMS.md): a script appended
//! to this `cig` runs as the program, through the same run as `cig run`.
//! Each test builds its programs the way cigcompile does, from the `cig`
//! this test run built, and runs them beside `cig run`. Programs are
//! format 2 unless a test says format 1.

use cigscript::cig_bytecode::{
    self as bc, BurnRegion, Constant, FuncKind, Function as BcFunction, Instr, LineEntry, Module,
    Op, RegionLabel, RegionRow, SpanBase,
};
use cigscript::program::{self, Build, Encoding, Program, Section, Stage};
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
    /// script appended, in format 2.
    fn compile(&self, name: &str, script: &str) -> PathBuf {
        self.make2(&self.build(name, script))
    }

    /// What [`Sandbox::compile`] builds from, for a test to change first.
    fn build(&self, name: &str, script: &str) -> Build {
        let source = fs::read_to_string(self.path().join(script)).unwrap();
        Build::new(name, script, &source, "test-compiler 0", &target())
    }

    /// The same program in format 1, as cigs before format 2 made it.
    fn compile1(&self, name: &str, script: &str) -> PathBuf {
        let source = fs::read_to_string(self.path().join(script)).unwrap();
        self.make(&Program::new(name, script, &source, "test-compiler 0"))
    }

    fn make(&self, p: &Program) -> PathBuf {
        let runtime = fs::read(env!("CARGO_BIN_EXE_cig")).unwrap();
        self.place(&p.name, &program::append(&runtime, p))
    }

    fn make2(&self, b: &Build) -> PathBuf {
        let runtime = fs::read(env!("CARGO_BIN_EXE_cig")).unwrap();
        self.place(&b.name, &program::append2(&runtime, b).unwrap())
    }

    /// `bytes` as an executable named `name`. Always a new file, never
    /// one written over: macOS keeps a binary's code-signing state by
    /// file, and a program rewritten in place after it has run is killed
    /// when it starts.
    fn place(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
        let out = self.path().join("bin").join(file);
        fs::create_dir_all(out.parent().unwrap()).unwrap();
        let _ = fs::remove_file(&out);
        fs::write(&out, bytes).unwrap();
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

/// The target a test program records.
fn target() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

/// A test-only section, optional or required.
fn test_section(required: bool) -> Section {
    Section {
        id: "zz-test".to_string(),
        version: 1,
        encoding: Encoding::Stored,
        required,
        bytes: b"a test-only section".to_vec(),
    }
}

/// Where section `id`'s body starts in the program at `exe`.
fn body_at(exe: &Path, id: &str) -> usize {
    let mut f = fs::File::open(exe).unwrap();
    let l = program::read(&mut f).unwrap().unwrap();
    let e = l.entry(id).unwrap();
    (l.start + e.offset) as usize
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).to_string()
}

/// Run ids and the time a step took differ between two runs; nothing
/// else may.
fn normal(s: &str) -> String {
    let re = regex::Regex::new(r"\d{8}T\d{6}-[0-9a-f]{6}").unwrap();
    let s = re.replace_all(s, "<run>");
    let ms = regex::Regex::new(r"\(\d+ ms\)").unwrap();
    ms.replace_all(&s, "(<ms>)").to_string()
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
/// never runs as `cig` (#135). Zero padding after the trailer is fine. The
/// same for both formats.
#[test]
fn a_cut_or_extended_program_is_damaged_never_cig() {
    let s = Sandbox::new();
    s.write("x.cig", "burn {\n  fs.write_text(\"x.txt\", \"x\")\n}\n");
    for (format, exe) in [(2, s.compile("x", "x.cig")), (1, s.compile1("x1", "x.cig"))] {
        let whole = fs::read(&exe).unwrap();
        let mut cases: Vec<(String, Vec<u8>)> = [1usize, 48, 56, 200]
            .iter()
            .map(|cut| {
                (
                    format!("format {format}, {cut} bytes cut"),
                    whole[..whole.len() - cut].to_vec(),
                )
            })
            .collect();
        let mut junk = whole.clone();
        junk.extend_from_slice(b"XX");
        cases.push((format!("format {format}, XX after it"), junk));
        // Each case is a file of its own: macOS keeps a binary's code-signing
        // state by file, and one rewritten in place after it ran can be killed
        // when it is started again.
        for (n, (what, bytes)) in cases.into_iter().enumerate() {
            let exe = s.place(&format!("x{format}-{n}"), &bytes);
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
        let exe = s.place(&format!("x{format}-padded"), &padded);
        let out = s.prog(&exe, &[]);
        assert!(
            out.status.success(),
            "format {format}, padded: {}",
            text(&out.stderr)
        );
        assert!(
            s.exists("x.txt"),
            "format {format}: the padded program did not run"
        );
        fs::remove_file(s.path().join("x.txt")).unwrap();
    }
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

#[test]
fn format_1_programs_still_run() {
    let s = Sandbox::new();
    s.write(
        "hello.cig",
        "stick name = args.get(0, \"world\")\nexhale \"hello, ${name}\"\n",
    );
    let exe = s.compile1("hello", "hello.cig");
    same(
        &s.cig(&["run", "hello.cig"]),
        &s.prog(&exe, &[]),
        "format 1",
    );
    same(
        &s.cig(&["run", "hello.cig", "--", "you"]),
        &s.prog(&exe, &["you"]),
        "format 1, an argument",
    );
    let v: serde_json::Value =
        serde_json::from_slice(&s.prog(&exe, &["--cig-program", "--json"]).stdout).unwrap();
    assert_eq!(v["format"], 1);
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys.len(),
        7,
        "format 1 prints what it always did: {keys:?}"
    );
    assert!(v.get("sections").is_none() && v.get("stage").is_none());
    let info = text(&s.prog(&exe, &["--cig-program"]).stdout);
    assert!(!info.contains("sections"), "{info}");
}

#[test]
fn a_format_2_program_lists_its_record_and_sections() {
    let s = Sandbox::new();
    s.write("f.cig", "exhale \"ran\"\n");
    let mut b = s.build("f", "f.cig");
    b.sections.push(test_section(false));
    let exe = s.make2(&b);
    let v: serde_json::Value =
        serde_json::from_slice(&s.prog(&exe, &["--cig-program", "--json"]).stdout).unwrap();
    assert_eq!(v["format"], 2);
    assert_eq!(v["stage"], "a");
    assert_eq!(v["opt"], "0");
    assert_eq!(v["target"], target());
    let runtime = fs::read(env!("CARGO_BIN_EXE_cig")).unwrap();
    use sha2::Digest as _;
    assert_eq!(
        v["runtime_sha256"],
        hex::encode(sha2::Sha256::digest(&runtime))
    );
    assert!(v.get("own_flags").is_none());
    let sections = v["sections"].as_array().unwrap();
    assert_eq!(sections.len(), 2);
    assert_eq!(sections[0]["id"], "source");
    assert_eq!(sections[0]["bytes"], 13);
    assert_eq!(sections[0]["decoded"], 13);
    assert_eq!(sections[0]["encoding"], "stored");
    assert_eq!(sections[0]["required"], true);
    assert_eq!(sections[1]["id"], "zz-test");
    assert_eq!(sections[1]["required"], false);
    assert_eq!(sections[1]["sha256"].as_str().unwrap().len(), 64);
    let info = text(&s.prog(&exe, &["--cig-program"]).stdout);
    for want in [
        "format         2, stage a",
        "sections",
        "source",
        "zz-test",
        "optional",
    ] {
        assert!(info.contains(want), "{want}: {info}");
    }
}

/// A script long enough to be kept DEFLATE-compressed runs, and errs at
/// the line `cig run` names.
#[test]
fn a_deflated_program_says_what_cig_run_says() {
    let s = Sandbox::new();
    let mut script: String = (0..300).map(|n| format!("stick v{n} = {n}\n")).collect();
    script.push_str("exhale \"last ${v299}\"\nstick bad = 1 / 0\n");
    assert!(script.len() >= program::DEFLATE_FROM);
    s.write("long.cig", &script);
    let exe = s.compile("long", "long.cig");
    let mut f = fs::File::open(&exe).unwrap();
    let l = program::read(&mut f).unwrap().unwrap();
    assert_eq!(
        l.entry("source").unwrap().encoding(),
        Some(Encoding::Deflate)
    );
    let (c, p) = (s.cig(&["run", "long.cig"]), s.prog(&exe, &[]));
    same(&c, &p, "a deflated script's error");
    assert_eq!(p.status.code(), Some(1));
    assert!(text(&p.stdout).contains("last 299"), "{}", text(&p.stdout));
    assert!(
        text(&p.stderr).contains("long.cig:302:"),
        "{}",
        text(&p.stderr)
    );
}

/// A changed byte in a body is refused when the body is used, and at once
/// under CIG_VALIDATE=1; a start reads no body, so `--help` and
/// `--cig-program` still answer.
#[test]
fn archive_tamper_refused() {
    let s = Sandbox::new();
    s.write("t.cig", "burn {\n  fs.write_text(\"t.txt\", \"t\")\n}\n");
    let mut b = s.build("t", "t.cig");
    b.sections.push(test_section(false));
    let good = fs::read(s.make2(&b)).unwrap();
    let exe = s.place("t-good", &good);
    for id in ["source", "zz-test"] {
        let mut bad = good.clone();
        bad[body_at(&exe, id) + 3] ^= 0x20;
        let exe = s.place(&format!("t-{id}"), &bad);
        let want = format!("section `{id}`'s bytes do not match the sha256 in the section table");
        let out = s.prog(&exe, &[]);
        let err = text(&out.stderr);
        if id == "source" {
            assert_eq!(out.status.code(), Some(3), "{err}");
            assert!(err.contains("E1050") && err.contains(&want), "{err}");
            assert!(!s.exists("t.txt"), "a changed script ran");
            let out = s.prog(&exe, &["--chains"]);
            assert!(text(&out.stderr).contains(&want), "{}", text(&out.stderr));
        } else {
            // Nothing uses the test section: the program runs.
            assert!(out.status.success(), "{err}");
            fs::remove_file(s.path().join("t.txt")).unwrap();
        }
        let help = s.prog(&exe, &["--help"]);
        assert!(help.status.success(), "{}", text(&help.stderr));
        let validated = output(
            s.command(&exe)
                .env(program::VALIDATE_ENV, "1")
                .arg("--help"),
        )
        .unwrap();
        let err = text(&validated.stderr);
        assert_eq!(validated.status.code(), Some(3), "{id}: {err}");
        assert!(err.contains("E1050") && err.contains(&want), "{id}: {err}");
    }
    let ok = output(s.command(&exe).env(program::VALIDATE_ENV, "1")).unwrap();
    assert!(ok.status.success(), "{}", text(&ok.stderr));
}

/// A program whose `source` body is damaged still answers `--help` and
/// `--cig-program`: a start reads the trailer, the record and the table.
#[test]
fn the_head_is_all_a_start_hashes() {
    let s = Sandbox::new();
    s.write("h.cig", "exhale \"h\"\n");
    let exe = s.compile("h", "h.cig");
    let mut bytes = fs::read(&exe).unwrap();
    let at = body_at(&exe, "source");
    for b in &mut bytes[at..at + 10] {
        *b = b'#';
    }
    let exe = s.place("h-damaged", &bytes);
    let help = s.prog(&exe, &["--help"]);
    assert!(help.status.success(), "{}", text(&help.stderr));
    assert!(
        text(&help.stdout).contains("usage: h"),
        "{}",
        text(&help.stdout)
    );
    let info = s.prog(&exe, &["--cig-program", "--json"]);
    assert!(info.status.success(), "{}", text(&info.stderr));
    let v: serde_json::Value = serde_json::from_slice(&info.stdout).unwrap();
    assert_eq!(v["name"], "h");
    let run = s.prog(&exe, &[]);
    assert_eq!(run.status.code(), Some(3));
    assert!(text(&run.stderr).contains("E1050"), "{}", text(&run.stderr));
    assert!(text(&run.stdout).is_empty());
}

#[test]
fn an_unknown_required_section_is_refused() {
    let s = Sandbox::new();
    s.write("u.cig", "exhale \"ran\"\n");
    let mut b = s.build("u", "u.cig");
    b.sections.push(test_section(true));
    let exe = s.make2(&b);
    for args in [&[][..], &["--help"]] {
        let out = s.prog(&exe, args);
        let err = text(&out.stderr);
        assert_eq!(out.status.code(), Some(3), "{err}");
        assert!(err.contains("E1050"), "{err}");
        assert!(
            err.contains("the program needs section `zz-test`, which this cig does not read"),
            "{err}"
        );
        assert!(text(&out.stdout).is_empty());
    }
}

#[test]
fn an_unknown_optional_section_is_skipped() {
    let s = Sandbox::new();
    s.write("o.cig", "exhale \"ran\"\n");
    let mut b = s.build("o", "o.cig");
    b.sections.push(test_section(false));
    let exe = s.make2(&b);
    let out = s.prog(&exe, &[]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "ran\n");
}

/// Another cig's program is E1051 before its table is looked at: a section
/// this cig does not know does not change the answer.
#[test]
fn a_format_2_program_for_another_cig_is_e1051() {
    let s = Sandbox::new();
    s.write("x.cig", "exhale \"x\"\n");
    let mut b = s.build("old", "x.cig");
    b.cig = "0.0.1".to_string();
    b.sections.push(test_section(true));
    let exe = s.make2(&b);
    let out = s.prog(&exe, &[]);
    let err = text(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "{err}");
    assert!(
        err.contains("E1051") && err.contains("compiled for cig 0.0.1"),
        "{err}"
    );
}

// ----- stage `b`: a program that runs its bytecode on the VM -----------------------

/// One function of a hand-made module, as a lowering writes it: its
/// instructions, constants, line rows and burn regions.
#[derive(Default)]
struct Asm {
    code: Vec<Instr>,
    consts: Vec<Constant>,
    lines: Vec<LineEntry>,
    regions: Vec<BurnRegion>,
    children: Vec<BcFunction>,
}

impl Asm {
    fn k(&mut self, c: Constant) -> u16 {
        let i = self.consts.iter().position(|x| *x == c).unwrap_or_else(|| {
            self.consts.push(c);
            self.consts.len() - 1
        });
        i as u16
    }
    fn s(&mut self, s: &str) -> u16 {
        self.k(Constant::Str(s.to_string()))
    }
    fn i(&mut self, i: Instr) -> &mut Self {
        self.code.push(i);
        self
    }
    fn op(&mut self, op: Op, a: u8, b: u8, c: u8) -> &mut Self {
        self.i(Instr::abc(op, a, b, c))
    }
    /// The next instruction's line row: the span of the `nth` `needle` in
    /// `src`, as the parser gives it.
    fn at(&mut self, src: &str, needle: &str) -> &mut Self {
        let (start, line, col) = place_of(src, needle);
        self.lines.push(LineEntry {
            pc: self.code.len() as u32,
            span_off: start as u32,
            span_len: needle.len() as u32,
            line_off: line - 1,
            col,
        });
        self
    }
    fn loadk(&mut self, r: u8, s: &str) -> &mut Self {
        let k = self.s(s);
        self.i(Instr::abx(Op::LOADK, r, k))
    }
    fn int(&mut self, r: u8, n: i16) -> &mut Self {
        self.i(Instr::asbx(Op::LOADINT, r, n))
    }
    fn exhale(&mut self, r: u8, s: &str) -> &mut Self {
        self.loadk(r, s).op(Op::EXHALE, r, 1, 0)
    }
    /// A burn region; `burn.enter` takes its index.
    fn burn_enter(&mut self) -> &mut Self {
        self.regions.push(BurnRegion {
            class: 0,
            label: RegionLabel::None,
        });
        let r = (self.regions.len() - 1) as u16;
        self.i(Instr::abx(Op::BURN_ENTER, 0, r))
    }
    /// `R(a) = callee(R(a+1..a+1+n))`, a `burn.op` of the callee's kind.
    fn burn_op(&mut self, a: u8, callee: &str, n: u8) -> &mut Self {
        let k = self.s(callee) as u8;
        let kind = cigscript::vm::burn_op_kind(callee).expect("a burn op") as u8;
        self.op(Op::BURN_OP, a, k, (n << 4) | kind)
    }
    fn ret_null(&mut self, r: u8) -> &mut Self {
        self.op(Op::LOADNULL, r, 0, 0).op(Op::RET, r, 0, 0)
    }
    fn function(&mut self, name: &str, kind: FuncKind, regs: u32) -> BcFunction {
        let mut f = BcFunction::empty_main();
        f.name = name.to_string();
        f.kind = kind;
        f.reg_count = regs;
        f.span_base = SpanBase {
            byte_offset: 0,
            line: 1,
            col: 1,
        };
        f.instrs = std::mem::take(&mut self.code);
        f.constants = std::mem::take(&mut self.consts);
        let mut lines = std::mem::take(&mut self.lines);
        lines.sort_by_key(|r| r.pc);
        f.lines = lines;
        f.regions = std::mem::take(&mut self.regions);
        f.region_rows = burn_rows(&f.instrs);
        f.children = std::mem::take(&mut self.children);
        f
    }
    /// The module, with this as its top level.
    fn module(&mut self, src_name: &str, regs: u32) -> Module {
        Module {
            source_name: src_name.to_string(),
            main: self.function("", FuncKind::Main, regs),
        }
    }
}

/// Each burn's row, as a lowering writes it: from its `burn.enter` to the
/// pc after the `burn.leave` that closes it.
fn burn_rows(code: &[Instr]) -> Vec<RegionRow> {
    let mut open = Vec::new();
    let mut rows = Vec::new();
    for d in cigscript::cig_bytecode::instr::walk(code).filter_map(|d| d.ok()) {
        match d.op {
            Op::BURN_ENTER => open.push((d.pc, d.b_u)),
            Op::BURN_LEAVE => {
                if let Some((pc_start, region)) = open.pop() {
                    rows.push(RegionRow {
                        pc_start,
                        pc_end: d.pc + d.width,
                        region,
                    });
                }
            }
            _ => {}
        }
    }
    rows
}

/// Where `needle` starts in `src`: its byte offset, line and column.
fn place_of(src: &str, needle: &str) -> (usize, u32, u32) {
    let start = src
        .find(needle)
        .unwrap_or_else(|| panic!("no {needle:?} in the script"));
    let line = src[..start].matches('\n').count() as u32 + 1;
    let col = src[..start].rfind('\n').map_or(start, |nl| start - nl - 1) as u32 + 1;
    (start, line, col)
}

impl Sandbox {
    /// The stage `b` program cigcompile would make from `script` and the
    /// module its lowering wrote: the script's `source` section beside a
    /// `bytecode` section of `bytes`.
    fn compile_b(&self, name: &str, script: &str, bytes: Vec<u8>) -> PathBuf {
        let mut b = self.build(name, script);
        b.stage = Stage::B;
        b.sections.push(Section {
            id: "bytecode".to_string(),
            version: 1,
            encoding: Encoding::Stored,
            required: true,
            bytes,
        });
        self.make2(&b)
    }

    fn compile_module(&self, name: &str, script: &str, m: &Module) -> PathBuf {
        self.compile_b(name, script, bc::serialize_module(m))
    }

    fn read(&self, name: &str) -> Option<String> {
        fs::read_to_string(self.path().join(name)).ok()
    }

    /// Every run recorded, by id.
    fn runs(&self) -> Vec<String> {
        let out = self.cig(&["runs", "--json"]);
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "cig runs --json: {e}\n{}\n{}",
                text(&out.stdout),
                text(&out.stderr)
            )
        });
        let mut ids: Vec<String> = v
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|r| r["id"].as_str().map(str::to_string))
            .collect();
        ids.sort();
        ids
    }

    /// Run `exe` and return its output and the one run it recorded.
    fn prog_run(&self, exe: &Path, args: &[&str]) -> (Output, String) {
        let before = self.runs();
        let out = self.prog(exe, args);
        let mut new: Vec<String> = self
            .runs()
            .into_iter()
            .filter(|id| !before.contains(id))
            .collect();
        assert_eq!(
            new.len(),
            1,
            "one run: {new:?}; the program ended with {}\n{}",
            out.status,
            text(&out.stderr)
        );
        (out, new.pop().unwrap())
    }

    /// A run's record and journal, the journal without what differs
    /// between two runs of one program (run ids, times, snapshot names).
    fn run_json(&self, id: &str) -> (serde_json::Value, serde_json::Value) {
        let out = self.cig(&["runs", id, "--json"]);
        let mut v: serde_json::Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|_| panic!("runs {id}: {}", text(&out.stdout)));
        let mut journal = v["journal"].take();
        for e in journal.as_array_mut().into_iter().flatten() {
            if let Some(o) = e.as_object_mut() {
                o.remove("at");
            }
        }
        // A snapshot is kept in the tin under a name of its own each run.
        // The path's separators are the platform's; in JSON a backslash
        // is written twice.
        let tin =
            regex::Regex::new(r#""[^"]*(?:/|\\\\)tin(?:/|\\\\)packs(?:/|\\\\)[^"]*""#).unwrap();
        let journal = tin
            .replace_all(&journal.to_string(), "\"<snapshot>\"")
            .to_string();
        let journal: serde_json::Value = serde_json::from_str(&normal(&journal)).unwrap();
        (v["run"].take(), journal)
    }
}

/// What a burn script's module does: `fs.write_text(file, text)` in a
/// burn, then `exhale "done"`.
const BURN_SCRIPT: &str = "burn {\n  fs.write_text(\"w.txt\", \"new\")\n}\nexhale \"done\"\n";

fn burn_module() -> Module {
    let src = BURN_SCRIPT;
    let mut a = Asm::default();
    a.at(src, "burn {").burn_enter();
    a.loadk(2, "w.txt").loadk(3, "new");
    a.at(src, "fs.write_text(\"w.txt\", \"new\")")
        .burn_op(1, "fs.write_text", 2)
        .op(Op::BURN_LEAVE, 0, 0, 0);
    a.at(src, "exhale \"done\"").exhale(1, "done");
    a.ret_null(0);
    a.module("w.cig", 4)
}

/// A script that burns, then fails, so the run rolls back.
const FAIL_SCRIPT: &str = "burn {\n  fs.write_text(\"e.txt\", \"x\")\n}\nexhale \"before\"\nstick n = 1 / 0\nexhale \"never\"\n";

fn fail_module() -> Module {
    let src = FAIL_SCRIPT;
    let mut a = Asm::default();
    a.at(src, "burn {").burn_enter();
    a.loadk(2, "e.txt").loadk(3, "x");
    a.at(src, "fs.write_text(\"e.txt\", \"x\")")
        .burn_op(1, "fs.write_text", 2)
        .op(Op::BURN_LEAVE, 0, 0, 0);
    a.at(src, "exhale \"before\"").exhale(1, "before");
    a.int(1, 1).int(2, 0);
    a.at(src, "1 / 0").op(Op::DIV, 1, 1, 2);
    a.at(src, "exhale \"never\"").exhale(2, "never");
    a.ret_null(0);
    a.module("e.cig", 4)
}

#[test]
fn a_program_with_damaged_bytecode_is_e1050() {
    let s = Sandbox::new();
    s.write("w.cig", BURN_SCRIPT);
    let good = bc::serialize_module(&burn_module());
    let refused = |exe: &Path, rule: &str| {
        let out = s.prog(exe, &[]);
        let err = text(&out.stderr);
        assert_eq!(out.status.code(), Some(3), "{err}");
        assert!(err.contains("E1050") && err.contains(rule), "{rule}: {err}");
        assert_eq!(text(&out.stdout), "", "{rule}: it ran");
        assert!(!s.exists("w.txt"), "{rule}: it burned");
        assert!(s.runs().is_empty(), "{rule}: it began a run");
    };

    // A changed byte in the body: the module's own BLAKE3 (a start does not
    // hash the body with the table's sha256).
    let exe = s.compile_b("changed", "w.cig", good.clone());
    let mut bytes = fs::read(&exe).unwrap();
    bytes[body_at(&exe, "bytecode") + 50] ^= 0x01;
    let exe = s.place("changed-body", &bytes);
    refused(
        &exe,
        "the program's bytecode does not read: bytecode integrity hash does not match",
    );

    // A module that does not parse: its own BLAKE3, and its magic.
    let mut bad = good.clone();
    let last = bad.len() - 1;
    bad[last] ^= 0x01;
    refused(
        &s.compile_b("unparsed", "w.cig", bad),
        "the program's bytecode does not read: bytecode integrity hash does not match",
    );
    let mut bad = good.clone();
    bad[0] = b'X';
    refused(
        &s.compile_b("magic", "w.cig", bad),
        "bytecode magic is not `CIGB`",
    );

    // One the verifier refuses: a register past the frame.
    let mut a = Asm::default();
    a.op(Op::MOV, 9, 0, 0).ret_null(0);
    refused(
        &s.compile_module("unverified", "w.cig", &a.module("w.cig", 2)),
        "the program's bytecode does not verify: register-out-of-range at fn0+0",
    );

    // One the loader refuses: a prelude name this cig does not have.
    let mut a = Asm::default();
    let k = a.s("no_such_prelude_name");
    a.i(Instr::abx(Op::LOADG, 0, k)).ret_null(0);
    let exe = s.compile_module("unloaded", "w.cig", &a.module("w.cig", 2));
    refused(
        &exe,
        "the program's bytecode does not verify: unknown-prelude-name at fn0+0",
    );

    // Its script is there, and runs only when asked.
    let out = s.prog(&exe, &["--interp"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "done\n");
    assert_eq!(s.read("w.txt").as_deref(), Some("new"));
    // `--help` names the flag.
    let help = text(&s.prog(&exe, &["--help"]).stdout);
    assert!(help.contains("--interp"), "{help}");
}

/// Where table entry `id` starts in the program `bytes`, and where the
/// table starts: the table sits directly before the 56-byte trailer.
fn entry_at(bytes: &[u8], id: &str) -> (usize, usize) {
    let t = bytes.len() - 56;
    let count = u16::from_le_bytes(bytes[t + 20..t + 22].try_into().unwrap()) as usize;
    let table = t - 64 * count;
    let entry = (table..t)
        .step_by(64)
        .find(|e| bytes[*e..][..8].starts_with(id.as_bytes()))
        .unwrap();
    (entry, table)
}

/// Table entry `id`'s sha256 in the program `bytes` (whose payload starts
/// at `start`) changed, and the head's sha256 recomputed over the record and
/// the table, so only the table's own sha256 of the body is wrong.
fn wrong_table_sha256(bytes: &mut [u8], start: usize, id: &str) {
    use sha2::Digest as _;
    let t = bytes.len() - 56;
    let record_len = u32::from_le_bytes(bytes[t + 16..t + 20].try_into().unwrap()) as usize;
    let (entry, table) = entry_at(bytes, id);
    bytes[entry + 40] ^= 0x01;
    let mut h = sha2::Sha256::new();
    h.update(&bytes[start..start + record_len]);
    h.update(&bytes[table..t]);
    let head: [u8; 32] = h.finalize().into();
    bytes[t + 24..t + 56].copy_from_slice(&head);
}

/// A stage `b` start checks its `bytecode` body by the module's own header
/// and BLAKE3, not by the table's sha256 (decision 0004, section 5): a
/// changed byte anywhere in the body or its table entry is still `E1050`
/// before the first instruction, and `CIG_VALIDATE=1` still checks the
/// table's sha256.
#[test]
fn a_stage_b_start_checks_its_bytecode_by_the_modules_own_hash() {
    let s = Sandbox::new();
    s.write("w.cig", BURN_SCRIPT);
    let good_module = bc::serialize_module(&burn_module());
    let exe = s.compile_b("good", "w.cig", good_module.clone());
    let good = fs::read(&exe).unwrap();
    let at = body_at(&exe, "bytecode");
    let start = {
        let mut f = fs::File::open(&exe).unwrap();
        program::read(&mut f).unwrap().unwrap().start as usize
    };
    let refused = |exe: &Path, what: &str, rule: &str| {
        let out = s.prog(exe, &[]);
        let err = text(&out.stderr);
        assert_eq!(out.status.code(), Some(3), "{what}: {err}");
        assert!(err.contains("E1050") && err.contains(rule), "{what}: {err}");
        assert_eq!(text(&out.stdout), "", "{what}: it ran");
        assert!(!s.exists("w.txt"), "{what}: it burned");
        assert!(s.runs().is_empty(), "{what}: it began a run");
    };
    let read = "the program's bytecode does not read: ";
    let last = good_module.len() - 1;
    for (n, (what, offset, rule)) in [
        ("magic", 0, "bytecode magic is not `CIGB`"),
        ("version", 4, "bytecode format version"),
        ("flags", 6, "bytecode header flags 0x0001 are not zero"),
        ("stored hash", 8, "bytecode integrity hash does not match"),
        (
            "first payload byte",
            40,
            "bytecode integrity hash does not match",
        ),
        ("last byte", last, "bytecode integrity hash does not match"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut bytes = good.clone();
        bytes[at + offset] ^= 0x01;
        let exe = s.place(&format!("damaged-{n}"), &bytes);
        refused(&exe, what, &format!("{read}{rule}"));
    }

    // A table entry's sha256: the head's sha256 covers the table.
    let mut bytes = good.clone();
    let entry = entry_at(&bytes, "bytecode").0;
    bytes[entry + 40] ^= 0x01;
    let exe = s.place("damaged-entry", &bytes);
    refused(
        &exe,
        "table entry",
        "the record and the section table do not match the sha256 in the trailer",
    );

    // The table's sha256 wrong and the head resealed over it: a start runs
    // (the body is whole, and its own hash says so) ...
    let mut bytes = good.clone();
    wrong_table_sha256(&mut bytes, start, "bytecode");
    let exe = s.place("wrong-table-sha256", &bytes);
    let out = s.prog(&exe, &[]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "done\n");
    fs::remove_file(s.path().join("w.txt")).unwrap();
    // ... and CIG_VALIDATE=1 still refuses it by the table's sha256.
    let before = s.runs();
    let out = output(s.command(&exe).env(program::VALIDATE_ENV, "1")).unwrap();
    let err = text(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "{err}");
    assert!(
        err.contains("E1050")
            && err.contains(
                "section `bytecode`'s bytes do not match the sha256 in the section table"
            ),
        "{err}"
    );
    assert_eq!(text(&out.stdout), "");
    assert!(!s.exists("w.txt"));
    assert_eq!(s.runs(), before, "CIG_VALIDATE=1: it began a run");
}

#[test]
fn interp_fallback_gives_the_same_bytes() {
    let s = Sandbox::new();
    s.write("e.cig", FAIL_SCRIPT);
    let b = s.compile_module("e", "e.cig", &fail_module());
    let a = s.compile("e", "e.cig");
    let mut seen = Vec::new();
    for (exe, args, what) in [
        (&b, &[][..], "stage b"),
        (&b, &["--interp"][..], "stage b, --interp"),
        (&a, &[][..], "stage a"),
        (&a, &["--interp"][..], "stage a, --interp"),
    ] {
        let (out, id) = s.prog_run(exe, args);
        assert_eq!(out.status.code(), Some(1), "{what}: {}", text(&out.stderr));
        assert_eq!(text(&out.stdout), "before\n", "{what}");
        assert!(!s.exists("e.txt"), "{what}: not rolled back");
        let (run, journal) = s.run_json(&id);
        assert_eq!(run["status"], "rolled_back", "{what}");
        seen.push((what, out, run, journal));
    }
    let (_, first, run0, journal0) = &seen[0];
    let err = text(&first.stderr);
    assert!(err.contains("e.cig:5:"), "{err}");
    assert!(
        err.contains("5 | stick n = 1 / 0"),
        "the quoted line: {err}"
    );
    for (what, out, run, journal) in &seen[1..] {
        same(first, out, what);
        assert_eq!(journal, journal0, "{what}: the journal");
        for key in [
            "script_sha256",
            "burns",
            "rolled_back",
            "diagnostic",
            "error",
        ] {
            assert_eq!(run[key], run0[key], "{what}: the record's {key}");
        }
    }
}

#[test]
fn a_stage_b_burn_journals_plans_and_unburns_as_stage_a() {
    let s = Sandbox::new();
    s.write("w.cig", BURN_SCRIPT);
    s.write("w.txt", "old");
    let a = s.compile("writer", "w.cig");
    let b = s.compile_module("writer", "w.cig", &burn_module());

    // The plan.
    let (pa, pb) = (s.prog(&a, &["--dry-run"]), s.prog(&b, &["--dry-run"]));
    same(&pa, &pb, "--dry-run");
    assert!(text(&pb.stderr).contains("w.txt"), "{}", text(&pb.stderr));
    assert_eq!(s.read("w.txt").as_deref(), Some("old"));

    // The run, its record and its journal.
    let (ra, id_a) = s.prog_run(&a, &[]);
    assert!(ra.status.success(), "{}", text(&ra.stderr));
    assert_eq!(s.read("w.txt").as_deref(), Some("new"));
    assert!(s.prog(&a, &["--unburn", &id_a]).status.success());
    assert_eq!(s.read("w.txt").as_deref(), Some("old"));
    let (rb, id_b) = s.prog_run(&b, &[]);
    same(&ra, &rb, "the run");
    assert_eq!(s.read("w.txt").as_deref(), Some("new"));
    let ((run_a, journal_a), (run_b, journal_b)) = (s.run_json(&id_a), s.run_json(&id_b));
    assert_eq!(journal_a, journal_b, "the journal");
    assert_eq!(run_a["script_sha256"], run_b["script_sha256"]);
    assert_eq!(run_a["program"]["sha256"], run_b["program"]["sha256"]);
    assert_eq!(run_b["program"]["stage"], "b");
    assert_eq!(run_a["burns"], run_b["burns"]);

    // `cig runs` sees one program, and the stage `b` program puts its run back.
    let shown = text(&s.cig(&["runs", &id_b]).stdout);
    assert!(
        shown.contains("program   writer  (compiled from w.cig"),
        "{shown}"
    );
    let back = s.prog(&b, &["--unburn", &id_b]);
    assert!(back.status.success(), "{}", text(&back.stderr));
    assert_eq!(s.read("w.txt").as_deref(), Some("old"));
}

#[test]
fn a_stage_b_pack_runs_where_the_tree_walker_runs_it() {
    // The VM refuses a `pack` once the kernel's journal or plan holds
    // anything (E753); a run puts nothing there before the first statement.
    let s = Sandbox::new();
    let src = "pack { \"out\" }\nburn {\n  fs.write_text(\"out/a.txt\", \"a\")\n}\n";
    s.write("p.cig", src);
    fs::create_dir(s.path().join("out")).unwrap();
    let mut m = Asm::default();
    m.loadk(1, "out")
        .at(src, "pack { \"out\" }")
        .op(Op::PACK, 1, 1, 0)
        .at(src, "\"out\"")
        .op(Op::NOP, 0, 0, 0);
    m.at(src, "burn {").burn_enter();
    m.loadk(3, "out/a.txt").loadk(4, "a");
    m.at(src, "fs.write_text(\"out/a.txt\", \"a\")")
        .burn_op(2, "fs.write_text", 2)
        .op(Op::BURN_LEAVE, 0, 0, 0);
    m.ret_null(0);
    let b = s.compile_module("p", "p.cig", &m.module("p.cig", 5));
    let a = s.compile("p", "p.cig");
    same(
        &s.prog(&a, &["--dry-run"]),
        &s.prog(&b, &["--dry-run"]),
        "--dry-run",
    );
    let (ra, id_a) = s.prog_run(&a, &[]);
    assert!(ra.status.success(), "{}", text(&ra.stderr));
    fs::remove_file(s.path().join("out/a.txt")).unwrap();
    let (rb, id_b) = s.prog_run(&b, &[]);
    same(&ra, &rb, "the run");
    assert_eq!(s.read("out/a.txt").as_deref(), Some("a"));
    let (run_a, _) = s.run_json(&id_a);
    let (run_b, _) = s.run_json(&id_b);
    assert_eq!(run_a["pack"], run_b["pack"]);
    assert!(
        run_b["pack"].as_array().is_some_and(|p| p.len() == 1),
        "{run_b}"
    );
}

#[test]
fn max_steps_stops_a_stage_b_program_where_it_stops_the_tree_walker() {
    let s = Sandbox::new();
    let src = "roll i = 0\nwhile i < 3 {\n  i = i + 1\n}\nexhale i\n";
    s.write("l.cig", src);
    let steps = |nest: u8, n: u16| Instr::abx(Op::STEPS, nest, n);
    let mut m = Asm::default();
    let one = m.k(Constant::Int(1)) as u8;
    m.at(src, "roll i = 0").i(steps(0, 1)).int(0, 0);
    m.at(src, "while i < 3 {").i(steps(0, 1)).i(steps(1, 0));
    m.int(1, 3).op(Op::LT, 2, 0, 1);
    m.i(Instr::asbx(Op::TESTN, 2, 4)); // to the loop's end
    m.at(src, "while i < 3 {").i(steps(1, 1));
    m.at(src, "i = i + 1").i(steps(1, 1));
    m.op(Op::ADDK, 0, 0, one);
    m.i(Instr::asbx(Op::LOOP, 0, -7)); // to the test
    m.i(steps(0, 0));
    m.at(src, "exhale i").i(steps(0, 1)).op(Op::EXHALE, 0, 1, 0);
    m.ret_null(1);
    let b = s.compile_module("l", "l.cig", &m.module("l.cig", 3));
    let a = s.compile("l", "l.cig");
    let mut stopped = 0;
    for n in 1..=10 {
        let budget = n.to_string();
        let (pa, pb) = (
            s.prog(&a, &["--max-steps", &budget]),
            s.prog(&b, &["--max-steps", &budget]),
        );
        same(&pa, &pb, &format!("--max-steps {n}"));
        if text(&pb.stderr).contains("E515") {
            stopped = n;
        }
    }
    assert!(stopped >= 8, "the budget stopped it {stopped} times");
    let done = s.prog(&b, &["--max-steps", "0"]);
    assert_eq!(text(&done.stdout), "3\n", "{}", text(&done.stderr));
}

#[test]
fn a_stage_b_error_quotes_its_line_from_the_source_section() {
    let s = Sandbox::new();
    s.write("e.cig", FAIL_SCRIPT);
    let whole = s.prog(&s.compile_module("e", "e.cig", &fail_module()), &[]);
    let exe = s.compile_module("e", "e.cig", &fail_module());
    let mut bytes = fs::read(&exe).unwrap();
    bytes[body_at(&exe, "source") + 3] ^= 0x01;
    let exe = s.place("e", &bytes);
    let out = s.prog(&exe, &[]);
    let err = text(&out.stderr);
    // The program's own error, its exit code, its rollback; no quoted line.
    assert_eq!(out.status.code(), whole.status.code(), "{err}");
    assert_eq!(text(&out.stdout), "before\n");
    assert!(!s.exists("e.txt"), "not rolled back");
    let first = |e: &str| {
        e.lines()
            .find(|l| l.starts_with("error["))
            .map(str::to_string)
    };
    assert_eq!(first(&err), first(&text(&whole.stderr)), "{err}");
    assert!(err.contains("--> e.cig:5:"), "{err}");
    assert!(!err.contains("stick n = 1 / 0"), "a quoted line: {err}");
    assert!(
        err.contains("= note: the line is not shown: this program's source section is damaged (E1050): section `source`'s bytes do not match"),
        "{err}"
    );
    // JSON carries the note beside the diagnostic.
    let out = s.prog(&exe, &["--json"]);
    let noted = text(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["code"].is_string());
    let v = noted.unwrap_or_else(|| panic!("no diagnostic: {}", text(&out.stdout)));
    assert!(
        v["note"].as_str().is_some_and(|n| n.contains("E1050")),
        "{v}"
    );

    // A run that needs no diagnostic never reads the section; `--interp`
    // and `--chains` read it and refuse.
    s.write("w.cig", BURN_SCRIPT);
    let exe = s.compile_module("w", "w.cig", &burn_module());
    let mut bytes = fs::read(&exe).unwrap();
    bytes[body_at(&exe, "source") + 3] ^= 0x01;
    let exe = s.place("w", &bytes);
    let out = s.prog(&exe, &[]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "done\n");
    for flag in ["--interp", "--chains"] {
        let out = s.prog(&exe, &[flag]);
        assert_eq!(out.status.code(), Some(3), "{flag}");
        assert!(
            text(&out.stderr).contains("E1050"),
            "{flag}: {}",
            text(&out.stderr)
        );
    }
}

/// A script with a pull, a top-level chain, a top-level `stick` and a
/// chain declared in a block; `exit` puts `exit(2)` after the first chain.
fn chains_script(exit: bool) -> String {
    format!(
        "pull hi() {{\n  exhale \"hi\"\n}}\nchain go {{ hi }}\n{}stick n = 5\nif true {{\n  chain inner {{ hi }}\n}}\n",
        if exit { "exit(2)\n" } else { "" }
    )
}

/// Its module: `chain.make` with C = 1 for the top-level chain, C = 0 for
/// the one in a block, and the top level's names returned as a map.
fn chains_module(src: &str, exit: bool) -> Module {
    let mut hi = Asm::default();
    hi.at(src, "exhale \"hi\"").exhale(0, "hi").ret_null(0);
    let hi = hi.function("hi", FuncKind::Pull, 1);
    let mut a = Asm::default();
    a.children.push(hi);
    a.i(Instr::abx(Op::CLOSURE, 0, 0));
    a.loadk(2, "go").loadk(3, "hi").op(Op::MOV, 4, 0, 0);
    a.at(src, "chain go").op(Op::CHAIN_MAKE, 1, 3, 1);
    if exit {
        let k = a.s("exit");
        a.i(Instr::abx(Op::LOADG, 5, k)).int(6, 2);
        a.at(src, "exit(2)").op(Op::CALL, 5, 1, 0);
    }
    a.int(5, 5);
    a.op(Op::LOADBOOL, 6, 1, 0);
    a.i(Instr::asbx(Op::TESTN, 6, 4)); // over the block
    a.loadk(8, "inner").loadk(9, "hi").op(Op::MOV, 10, 0, 0);
    a.at(src, "chain inner").op(Op::CHAIN_MAKE, 7, 3, 0);
    a.op(Op::NEWMAP, 11, 3, 0);
    for (name, r) in [("hi", 0), ("go", 1), ("n", 5)] {
        a.loadk(12, name).op(Op::INSERTMAP, 11, 12, r);
    }
    a.op(Op::RET, 11, 0, 0);
    a.module("c.cig", 13)
}

#[test]
fn light_and_chains_answer_the_same_at_stage_b() {
    let s = Sandbox::new();
    let src = chains_script(false);
    s.write("c.cig", &src);
    let a = s.compile("c", "c.cig");
    let b = s.compile_module("c", "c.cig", &chains_module(&src, false));
    for args in [
        &[][..],
        &["--chains"][..],
        &["--light", "go"][..],
        &["--light", "n"][..],
        &["--light", "hi"][..],
        &["--light", "len"][..],
        &["--light", "inner"][..],
        &["--light", "nope"][..],
    ] {
        same(&s.prog(&a, args), &s.prog(&b, args), &format!("{args:?}"));
    }
    let lit = s.prog(&b, &["--light", "go"]);
    assert_eq!(text(&lit.stdout), "hi\n", "{}", text(&lit.stderr));
    let out = s.prog(&b, &["--light", "inner"]);
    let err = text(&out.stderr);
    assert!(
        err.contains("E804") && err.contains("chains here: go"),
        "{err}"
    );
    let out = s.prog(&b, &["--light", "n"]);
    assert!(
        text(&out.stderr).contains("`n` is a int, not a chain"),
        "{}",
        text(&out.stderr)
    );

    // After `exit()`, the tree-walker lights what the script had declared.
    let src = chains_script(true);
    s.write("x.cig", &src);
    let a = s.compile("x", "x.cig");
    let b = s.compile_module("x", "x.cig", &chains_module(&src, true));
    for args in [&["--light", "go"][..], &["--light", "n"][..]] {
        same(
            &s.prog(&a, args),
            &s.prog(&b, args),
            &format!("exit, {args:?}"),
        );
    }
    let lit = s.prog(&b, &["--light", "go"]);
    assert_eq!(lit.status.code(), Some(2));
    assert_eq!(text(&lit.stdout), "hi\n");
}

// ----- own flags: a program that takes every argument (cig-compile#8) ------------

impl Sandbox {
    /// The stage `a` program [`Sandbox::compile`] makes, with `own_flags`.
    fn compile_own(&self, name: &str, script: &str) -> PathBuf {
        let mut b = self.build(name, script);
        b.own_flags = true;
        self.make2(&b)
    }

    /// A stage `b` program with `own_flags`, from `m`.
    fn compile_own_b(&self, name: &str, script: &str, m: &Module) -> PathBuf {
        let mut b = self.build(name, script);
        b.own_flags = true;
        b.stage = Stage::B;
        b.sections.push(Section {
            id: "bytecode".to_string(),
            version: 1,
            encoding: Encoding::Stored,
            required: true,
            bytes: bc::serialize_module(m),
        });
        self.make2(&b)
    }
}

/// `exhale args`, as a lowering writes it.
const ARGS_SCRIPT: &str = "exhale args\n";

fn args_module() -> Module {
    let src = ARGS_SCRIPT;
    let mut a = Asm::default();
    let k = a.s("args");
    a.at(src, "args").i(Instr::abx(Op::LOADG, 1, k));
    a.at(src, "exhale args").op(Op::EXHALE, 1, 1, 0);
    a.ret_null(0);
    a.module("a.cig", 2)
}

#[test]
fn a_program_with_own_flags_gets_every_argument() {
    let s = Sandbox::new();
    s.write("a.cig", ARGS_SCRIPT);
    let a = s.compile_own("a", "a.cig");
    let b = s.compile_own_b("a", "a.cig", &args_module());
    let every = [
        "--version",
        "--help",
        "--",
        "x",
        "--dry-run",
        "-h",
        "--max-steps=1",
    ];
    for (args, said) in [
        (
            &every[..],
            "[\"--version\", \"--help\", \"--\", \"x\", \"--dry-run\", \"-h\", \"--max-steps=1\"]",
        ),
        (&["--version"][..], "[\"--version\"]"),
        (&["--help"][..], "[\"--help\"]"),
        (&["--", "x"][..], "[\"--\", \"x\"]"),
        (&["--cig-program"][..], "[\"--cig-program\"]"),
        (&[][..], "[]"),
    ] {
        let at_a = s.prog(&a, args);
        assert!(at_a.status.success(), "{args:?}: {}", text(&at_a.stderr));
        assert_eq!(text(&at_a.stdout).trim(), said, "{args:?}");
        // Stage `b` gets the same arguments, the same way.
        same(&at_a, &s.prog(&b, args), &format!("stage b, {args:?}"));
    }
    for exe in [&a, &b] {
        let read = program::read(&mut fs::File::open(exe).unwrap())
            .unwrap()
            .unwrap();
        assert!(read.own_flags(), "the record carries the key");
    }
    // It takes no run flags from anywhere: a variable in its environment
    // that names some changes nothing.
    let with_env = output(
        s.command(&a)
            .env("CIG_RUN_FLAGS", "--dry-run --help")
            .args(["x"]),
    )
    .expect("run the program");
    assert!(with_env.status.success(), "{}", text(&with_env.stderr));
    assert_eq!(text(&with_env.stdout).trim(), "[\"x\"]");
    // A program without the key still takes the run flags first.
    let plain = s.compile("p", "a.cig");
    let refused = s.prog(&plain, &["--version"]);
    assert_eq!(refused.status.code(), Some(3));
    assert!(text(&refused.stderr).contains("E1052"));
    assert_eq!(
        text(&s.prog(&plain, &["--", "--version"]).stdout).trim(),
        "[\"--version\"]"
    );
}

#[test]
fn an_old_program_keeps_its_bytes() {
    let s = Sandbox::new();
    let source = "stick name = args.get(0, \"world\")\nexhale \"hello, ${name}\"\n";
    s.write("hello.cig", source);
    let runtime = fs::read(env!("CARGO_BIN_EXE_cig")).unwrap();
    // Format 1, as `append` has always written it: the runtime, the record
    // as JSON in its field order, then `CIGPROG1`, the payload's length and
    // its sha256.
    use sha2::Digest;
    let payload = format!(
        "{{\"format\":1,\"name\":\"hello\",\"file\":\"hello.cig\",\"source\":{},\"sha256\":\"{}\",\"cig\":\"{}\",\"compiler\":\"test-compiler 0\"}}",
        serde_json::to_string(source).unwrap(),
        hex::encode(sha2::Sha256::digest(source.as_bytes())),
        cigscript::VERSION,
    );
    let mut expected = runtime.clone();
    expected.extend_from_slice(payload.as_bytes());
    expected.extend_from_slice(b"CIGPROG1");
    expected.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    expected.extend_from_slice(&sha2::Sha256::digest(payload.as_bytes()));
    let p = Program::new("hello", "hello.cig", source, "test-compiler 0");
    assert!(
        program::append(&runtime, &p) == expected,
        "format 1's bytes changed"
    );
    let exe = s.place("hello", &expected);
    // It still runs, and its flags are still the run's.
    same(
        &s.cig(&["run", "hello.cig"]),
        &s.prog(&exe, &[]),
        "format 1",
    );
    same(
        &s.cig(&["run", "hello.cig", "--", "--version"]),
        &s.prog(&exe, &["--", "--version"]),
        "format 1, an argument after --",
    );
    let refused = s.prog(&exe, &["--version"]);
    assert_eq!(refused.status.code(), Some(3));
    assert!(
        text(&refused.stderr).contains("E1052"),
        "{}",
        text(&refused.stderr)
    );
    // A format 2 program without the key: no key in its record, and it
    // starts as before.
    let b = s.build("hello", "hello.cig");
    let two = program::append2(&runtime, &b).unwrap();
    let rec = text(&two[runtime.len()..]);
    assert!(
        !rec.contains("own_flags"),
        "the key is written only when set"
    );
    let exe = s.place("hello2", &two);
    same(
        &s.cig(&["run", "hello.cig", "--", "you"]),
        &s.prog(&exe, &["you"]),
        "format 2",
    );
    assert_eq!(s.prog(&exe, &["--version"]).status.code(), Some(3));
    let v: serde_json::Value =
        serde_json::from_slice(&s.prog(&exe, &["--cig-program", "--json"]).stdout).unwrap();
    assert!(v.get("own_flags").is_none(), "{v}");
}

#[test]
fn a_rootless_pack_in_a_program_writes_where_no_pack_writes() {
    let s = Sandbox::new();
    // The same lines with and without the pack, so the plans agree.
    let rootless = "pack { own_flags: true }\nburn {\n  fs.write_text(\"free.txt\", \"x\")\n}\n";
    let none = "\nburn {\n  fs.write_text(\"free.txt\", \"x\")\n}\n";
    s.write("r.cig", rootless);
    s.write("n.cig", none);
    let r = s.compile_own("r", "r.cig");
    let n = s.compile("n", "n.cig");
    // The plan, from the same script compiled without own flags (a program
    // with them takes no `--dry-run`).
    let planned = s.compile("rp", "r.cig");
    let pr = s.prog(&planned, &["--dry-run"]);
    let pn = s.prog(&n, &["--dry-run"]);
    assert!(pr.status.success(), "{}", text(&pr.stderr));
    assert!(
        text(&pr.stderr).contains("dry-run plan"),
        "{}",
        text(&pr.stderr)
    );
    assert_eq!(text(&pr.stdout), text(&pn.stdout), "the outputs differ");
    assert_eq!(text(&pr.stderr), text(&pn.stderr), "the plans differ");
    assert!(!text(&pr.stderr).contains("pack"), "{}", text(&pr.stderr));
    let (out, id_r) = s.prog_run(&r, &[]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(s.read("free.txt").as_deref(), Some("x"));
    fs::remove_file(s.path().join("free.txt")).unwrap();
    let (_, id_n) = s.prog_run(&n, &[]);
    assert_eq!(s.read("free.txt").as_deref(), Some("x"));
    let (run_r, _) = s.run_json(&id_r);
    let (run_n, _) = s.run_json(&id_n);
    assert!(
        run_r.get("pack").is_none() && run_n.get("pack").is_none(),
        "{run_r}"
    );
    // At stage `b`, a `pack` instruction with no root declares no scope
    // either (a lowering writes none for a rootless pack).
    fs::remove_file(s.path().join("free.txt")).unwrap();
    let mut m = Asm::default();
    m.at(rootless, "pack { own_flags: true }")
        .op(Op::PACK, 1, 0, 0);
    m.at(rootless, "burn {").burn_enter();
    m.loadk(3, "free.txt").loadk(4, "x");
    m.at(rootless, "fs.write_text(\"free.txt\", \"x\")")
        .burn_op(2, "fs.write_text", 2)
        .op(Op::BURN_LEAVE, 0, 0, 0);
    m.ret_null(0);
    let b = s.compile_own_b("rb", "r.cig", &m.module("r.cig", 5));
    let (out, id_b) = s.prog_run(&b, &[]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(s.read("free.txt").as_deref(), Some("x"));
    let (run_b, _) = s.run_json(&id_b);
    assert!(run_b.get("pack").is_none(), "{run_b}");
}
