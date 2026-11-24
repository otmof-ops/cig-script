// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Stage `c` programs (src/native.rs, src/vm/native.rs, docs/PROGRAMS.md):
//! a stage `b` program with a `native` section, whose code the VM calls
//! where a call's arguments are of an entry's row.
//!
//! Each test builds its program the way cigcompile does, from the `cig`
//! this test run built, with a native section assembled here by hand: no
//! code generator is needed to hold the runtime to its side. On Linux
//! x86-64 the section's code runs; everywhere else the same programs run
//! their bytecode, which is what the tests then expect.

use cigscript::cig_bytecode as bc;
use cigscript::cig_native::{self as native, op, Arch, Entry, Exit, Site, Ty};
use cigscript::program::{self, Build, Encoding, Section, Stage, NATIVE_ALIGN, NATIVE_SECTION};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// This machine runs the tests' native code.
const NATIVE: bool = cfg!(all(target_os = "linux", target_arch = "x86_64"));

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

    /// `bytes` as an executable named `name`: always a new file.
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
            .env_remove("CIG_MAX_STEPS")
            .env_remove(program::VALIDATE_ENV)
            .env_remove(program::NATIVE_ENV)
            .env_remove(program::SELF_ENV);
        c
    }

    /// `cig run` of the script, on the tree-walker.
    fn walker(&self, script: &str, args: &[&str]) -> Output {
        self.command(Path::new(env!("CARGO_BIN_EXE_cig")))
            .arg("run")
            .arg(script)
            .args(args)
            .output()
            .expect("run cig")
    }

    fn prog(&self, exe: &Path, args: &[&str]) -> Output {
        output(self.command(exe).args(args)).expect("run the program")
    }

    /// The stage `c` program of `source`, with the native section `make`
    /// gives for its module (`None`: whatever `make` wrote as bytes).
    fn compile(&self, name: &str, source: &str, native: &[u8], module: &[u8]) -> PathBuf {
        let script = format!("{name}.cig");
        fs::write(self.path().join(&script), source).unwrap();
        let target = format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS);
        let mut b = Build::new(name, &script, source, "test-compiler 0", &target);
        b.stage = Stage::C;
        b.sections.push(Section {
            id: "bytecode".to_string(),
            version: bc::FORMAT_VERSION,
            encoding: Encoding::Stored,
            required: true,
            bytes: module.to_vec(),
        });
        b.sections.push(Section {
            id: NATIVE_SECTION.to_string(),
            version: 1,
            encoding: Encoding::Stored,
            required: false,
            bytes: native.to_vec(),
        });
        let runtime = fs::read(env!("CARGO_BIN_EXE_cig")).unwrap();
        self.place(name, &program::append2(&runtime, &b).expect("a program"))
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
        (
            a.status.code(),
            normal(&text(&a.stdout)),
            normal(&text(&a.stderr))
        ),
        (
            b.status.code(),
            normal(&text(&b.stdout)),
            normal(&text(&b.stderr))
        ),
        "{what}"
    );
}

/// A script lowered as cigcompile lowers it: the module's bytes, its
/// hash, and each named function's index in it.
struct Lowered {
    module: Vec<u8>,
    hash: [u8; 32],
    index: BTreeMap<String, u32>,
}

fn lower(source: &str) -> Lowered {
    let ast = cigscript::syntax::parse(source).expect("the script parses");
    let errors = cigscript::check::check(&ast, &[]).errors;
    assert!(
        errors.is_empty(),
        "the script checks: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
    let l = cigscript::lower::lowered_with_cir(&ast, "t.cig", cigscript::lower::DEFAULT_LEVEL)
        .expect("the script lowers");
    let index = l
        .funcs
        .iter()
        .zip(0u32..)
        .filter_map(|(f, i)| Some((l.cir.funcs.get(f.0 as usize)?.name.clone()?, i)))
        .collect();
    Lowered {
        hash: bc::module_hash(&l.bytes).expect("a module has a header"),
        module: l.bytes,
        index,
    }
}

// ----- the native code -------------------------------------------------
//
// x86-64, System V: the context in rdi, the arguments in rsi, the result
// slot in rdx, the status in eax. `Context`: steps_left at 0, site at 16,
// flags at 20, left at 24, right at 32.

/// `add(a, b)` on two ints, as the code generator compiles it: its one
/// statement's step (an exit at site 9 when that spends the budget),
/// then the sum, or an exit at site 0 with both operands when it
/// overflows.
#[rustfmt::skip]
const ADD: &[u8] = &[
    0x48, 0x8B, 0x07,                         // mov rax, [rdi]
    0x48, 0x83, 0xE8, 0x01,                   // sub rax, 1
    0x48, 0x89, 0x07,                         // mov [rdi], rax
    0x78, 0x2A,                               // js  +42 (the budget)
    0x48, 0x8B, 0x06,                         // mov rax, [rsi]
    0x48, 0x8B, 0x4E, 0x08,                   // mov rcx, [rsi+8]
    0x49, 0x89, 0xC0,                         // mov r8, rax
    0x49, 0x01, 0xC8,                         // add r8, rcx
    0x70, 0x06,                               // jo  +6 (the overflow)
    0x4C, 0x89, 0x02,                         // mov [rdx], r8
    0x31, 0xC0,                               // xor eax, eax
    0xC3,                                     // ret
    0xC7, 0x47, 0x10, 0x00, 0x00, 0x00, 0x00, // mov dword [rdi+16], 0
    0x48, 0x89, 0x47, 0x18,                   // mov [rdi+24], rax
    0x48, 0x89, 0x4F, 0x20,                   // mov [rdi+32], rcx
    0xB8, 0x01, 0x00, 0x00, 0x00,             // mov eax, 1
    0xC3,                                     // ret
    0xC7, 0x47, 0x10, 0x09, 0x00, 0x00, 0x00, // mov dword [rdi+16], 9
    0xB8, 0x04, 0x00, 0x00, 0x00,             // mov eax, 4
    0xC3,                                     // ret
];

/// `stub(site, left, right, flags, steps, status)`: code that does what
/// its arguments say, to drive every way native code can come back. It
/// writes the site, the operands and the flags to the context, takes
/// `steps` from the steps left, puts `left` in the result slot, and
/// returns `status`.
#[rustfmt::skip]
const STUB: &[u8] = &[
    0x48, 0x8B, 0x06,                         // mov rax, [rsi]
    0x89, 0x47, 0x10,                         // mov [rdi+16], eax
    0x48, 0x8B, 0x46, 0x08,                   // mov rax, [rsi+8]
    0x48, 0x89, 0x47, 0x18,                   // mov [rdi+24], rax
    0x48, 0x89, 0x02,                         // mov [rdx], rax
    0x48, 0x8B, 0x46, 0x10,                   // mov rax, [rsi+16]
    0x48, 0x89, 0x47, 0x20,                   // mov [rdi+32], rax
    0x48, 0x8B, 0x46, 0x18,                   // mov rax, [rsi+24]
    0x89, 0x47, 0x14,                         // mov [rdi+20], eax
    0x48, 0x8B, 0x46, 0x20,                   // mov rax, [rsi+32]
    0x48, 0x29, 0x07,                         // sub [rdi], rax
    0x48, 0x8B, 0x46, 0x28,                   // mov rax, [rsi+40]
    0xC3,                                     // ret
];

/// Where `needle` is in `source`: a site's position.
fn at(source: &str, needle: &str) -> (u32, u32, u32, u32) {
    let start = source
        .find(needle)
        .unwrap_or_else(|| panic!("no `{needle}`"));
    let line = source[..start].matches('\n').count() + 1;
    let col = start - source[..start].rfind('\n').map_or(0, |n| n + 1) + 1;
    (
        start as u32,
        (start + needle.len()) as u32,
        line as u32,
        col as u32,
    )
}

fn site(source: &str, needle: &str, kind: Exit, op: u8, left: Ty, right: Ty) -> Site {
    let (start, end, line, col) = at(source, needle);
    Site {
        start,
        end,
        line,
        col,
        kind,
        op,
        left,
        right,
        in_loop: false,
    }
}

/// The tests' script: `add`, compiled for real, and the stubs, whose
/// bytecode returns -1 so a result says which of the two ran. The marks
/// in the comments are where the sites point.
const SCRIPT: &str = "\
pull add(a, b) { snuff a + b }
pull stub(site, left, right, flags, steps, status) { snuff -1 }
pull stubf(site, left, right, flags, steps, status) { snuff -1 }
pull stubb(site, left, right, flags, steps, status) { snuff -1 }
pull stubn(site, left, right, flags, steps, status) { snuff -1 }
# mark-budget
# mark-loop
# mark-depth
# mark-negate
# mark-range
# mark-divide
# mark-order
# mark-modulo
";

/// The sites the stub can leave from, by index.
fn sites(source: &str) -> Vec<Site> {
    use Ty::{Float, Int, Null};
    vec![
        site(source, "a + b", Exit::Operator, op::ADD, Int, Int),
        site(source, "mark-budget", Exit::Budget, 0, Null, Null),
        Site {
            in_loop: true,
            ..site(source, "mark-loop", Exit::Budget, 0, Null, Null)
        },
        site(source, "mark-depth", Exit::Depth, 0, Null, Null),
        site(source, "mark-negate", Exit::Negate, 0, Int, Null),
        site(source, "mark-range", Exit::Range, 0, Int, Int),
        site(source, "mark-divide", Exit::Operator, op::DIV, Float, Float),
        site(source, "mark-order", Exit::Operator, op::LT, Float, Float),
        site(source, "mark-modulo", Exit::Operator, op::REM, Int, Int),
        // `add`'s one statement: its step mark.
        site(source, "snuff a + b", Exit::Budget, 0, Null, Null),
    ]
}

/// The section for `source` (which starts with [`SCRIPT`]): `add` for
/// two ints; `stub` for six ints, and for a float in `left` (its result
/// then a float); `stubf`, `stubb` and `stubn` with a float, a bool and
/// null in `left` and as their result.
fn section(source: &str, l: &Lowered) -> native::Section {
    use Ty::{Bool, Float, Int, Null};
    let stub = ADD.len() as u32;
    let row = |left: Ty| vec![Int, left, Int, Int, Int, Int];
    let entry = |name: &str, offset: u32, args: Vec<Ty>, ret: Ty| Entry {
        func: l.index[name],
        offset,
        args,
        ret,
    };
    let mut entries = vec![
        entry("add", 0, vec![Int, Int], Int),
        entry("stub", stub, row(Int), Int),
        entry("stub", stub, row(Float), Float),
        entry("stubf", stub, row(Float), Float),
        entry("stubb", stub, row(Bool), Bool),
        entry("stubn", stub, row(Null), Null),
    ];
    entries.sort_by(|a, b| (a.func, &a.args).cmp(&(b.func, &b.args)));
    native::Section {
        arch: Arch::X86_64,
        module: l.hash,
        code: [ADD, STUB].concat(),
        entries,
        sites: sites(source),
        bodies: Vec::new(),
        rule: native::Rule::None,
    }
}

/// The program of [`SCRIPT`] and `rest`, with the tests' native code.
fn program(s: &Sandbox, name: &str, rest: &str) -> PathBuf {
    let source = format!("{SCRIPT}{rest}");
    let l = lower(&source);
    let body = section(&source, &l).to_bytes().expect("a section");
    s.compile(name, &source, &body, &l.module)
}

/// What the program of `rest` prints, and that it ended well.
fn prints(rest: &str) -> String {
    let s = Sandbox::new();
    let exe = program(&s, "p", rest);
    let out = s.prog(&exe, &[]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    text(&out.stdout)
}

// ----- the section in the program --------------------------------------

/// The `cig` these programs are built on says, in its bytes, that it
/// runs stage `c` programs: what a compiler reads before it builds one.
#[test]
fn the_cig_binary_says_it_runs_stage_c_programs() {
    let bytes = fs::read(env!("CARGO_BIN_EXE_cig")).unwrap();
    assert_eq!(program::runtime_stages(&bytes), "abc");
    assert!(program::runtime_runs(&bytes, Stage::C));
}

#[test]
fn a_programs_native_section_lies_on_a_64_kib_boundary_behind_zeros() {
    let s = Sandbox::new();
    let exe = program(&s, "p", "exhale add(40, 2)\n");
    let bytes = fs::read(&exe).unwrap();
    let mut f = fs::File::open(&exe).unwrap();
    let l = program::read(&mut f).unwrap().unwrap();
    assert_eq!(l.stage(), Stage::C);
    let e = l.entry(NATIVE_SECTION).expect("a native section");
    let start = (l.start + e.offset) as usize;
    assert_eq!(start as u64 % NATIVE_ALIGN, 0);
    assert!(!e.required());
    assert_eq!(e.encoding(), Some(Encoding::Stored));
    // The section before it ends less than 64 KiB earlier, and what lies
    // between is zeros.
    let b = l.entry("bytecode").unwrap();
    let before = (l.start + b.offset + b.length) as usize;
    assert!(start - before < NATIVE_ALIGN as usize);
    assert!(bytes[before..start].iter().all(|b| *b == 0));
    // Every other body keeps its own alignment.
    for id in ["source", "bytecode"] {
        assert_eq!((l.start + l.entry(id).unwrap().offset) % program::ALIGN, 0);
    }
    // The body is the section, and reads back.
    let body = &bytes[start..start + e.length as usize];
    let t = native::Tables::parse(body).expect("the section reads");
    assert_eq!(
        (t.arch, t.entries.len(), t.sites.len()),
        (Arch::X86_64, 6, 10)
    );
}

#[test]
fn the_program_says_what_its_native_section_holds() {
    let s = Sandbox::new();
    let exe = program(&s, "p", "exhale add(40, 2)\n");
    let out = s.prog(&exe, &["--cig-program"]);
    let said = text(&out.stdout);
    assert!(said.contains("format         2, stage c"), "{said}");
    let line = said
        .lines()
        .find(|l| l.contains("native code"))
        .unwrap_or_else(|| panic!("no native line:\n{said}"));
    let want = if NATIVE {
        "6 entries for x86-64, this machine's"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "6 entries for x86-64, not this machine's: the bytecode runs"
    } else {
        "not loaded on this system: the bytecode runs"
    };
    assert_eq!(line.trim(), format!("native code    {want}"));
}

// ----- bodies ----------------------------------------------------------

/// A body that stops at once: `xor eax, eax; ret` is `At(0)`, "the
/// bytecode goes on at pc 0".
const AT_ZERO: &[u8] = &[0x31, 0xC0, 0xC3];

/// A body that reports a defect at pc 3: `movabs rax, (2 << 32) | 3; ret`.
const DEFECT_AT_3: &[u8] = &[0x48, 0xB8, 0x03, 0, 0, 0, 0x02, 0, 0, 0, 0xC3];

/// The program of `rest` whose native section also holds a body for
/// function `func`, made of `code`, with the resume points `resumes`.
fn program_with_body(s: &Sandbox, rest: &str, func: u32, code: &[u8], resumes: &[u32]) -> PathBuf {
    let source = format!("{SCRIPT}{rest}");
    let l = lower(&source);
    let mut section = section(&source, &l);
    let offset = section.code.len() as u32;
    section.code.extend_from_slice(code);
    section.bodies.push(native::Body {
        func,
        offset,
        resumes: resumes.to_vec(),
    });
    section.rule = native::Rule::Every;
    s.compile(
        "p",
        &source,
        &section.to_bytes().expect("a section"),
        &l.module,
    )
}

#[test]
fn a_body_is_entered_and_the_bytecode_goes_on_where_it_stops() {
    if !NATIVE {
        return;
    }
    let s = Sandbox::new();
    let rest = "exhale add(40, 2)\n";
    // The top level's body says "go on at pc 0": the bytecode runs it.
    let exe = program_with_body(&s, rest, 0, AT_ZERO, &[0]);
    for validate in [false, true] {
        let mut c = s.command(&exe);
        if validate {
            c.env(program::VALIDATE_ENV, "1");
        }
        let out = output(&mut c).unwrap();
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        assert_eq!(text(&out.stdout), "42\n");
    }
    let said = text(&s.prog(&exe, &["--cig-program"]).stdout);
    assert!(
        said.contains("native code    1 body and 6 entries for x86-64, this machine's"),
        "{said}"
    );
    // That the body is entered, not passed over: one that reports a
    // defect stops the run before anything is printed.
    let exe = program_with_body(&s, rest, 0, DEFECT_AT_3, &[0]);
    let out = s.prog(&exe, &[]);
    assert_ne!(out.status.code(), Some(0));
    assert_eq!(text(&out.stdout), "");
    let err = text(&out.stderr);
    assert!(
        err.contains("the native body of fn0") && err.contains("at pc 3"),
        "{err}"
    );
    // Without its native code the same program runs its bytecode.
    let out = output(s.command(&exe).env("CIG_NATIVE", "0")).unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "42\n");
}

#[test]
fn a_body_that_cannot_be_entered_is_not_used_and_is_damage_when_checked() {
    if !NATIVE {
        return;
    }
    let s = Sandbox::new();
    let rest = "exhale add(40, 2)\n";
    for (what, func, resumes, said) in [
        (
            "a resume point past the function",
            0u32,
            vec![0u32, 9_999],
            "the body of fn0 has a resume point that starts no instruction of it",
        ),
        (
            "a function the module does not have",
            900,
            vec![0],
            "body 0 is for function 900, which the module does not have",
        ),
    ] {
        // The body would report a defect if it were entered.
        let exe = program_with_body(&s, rest, func, DEFECT_AT_3, &resumes);
        let plain = s.prog(&exe, &[]);
        assert_eq!(
            plain.status.code(),
            Some(0),
            "{what}: {}",
            text(&plain.stderr)
        );
        assert_eq!(text(&plain.stdout), "42\n", "{what}");
        let checked = output(s.command(&exe).env(program::VALIDATE_ENV, "1")).unwrap();
        assert_eq!(checked.status.code(), Some(3), "{what}");
        let err = text(&checked.stderr);
        assert!(err.contains("E1050") && err.contains(said), "{what}: {err}");
    }
}

// ----- which code runs -------------------------------------------------

#[test]
fn native_code_runs_where_the_arguments_are_of_a_row_and_the_bytecode_elsewhere() {
    let said = prints(
        "exhale stub(0, 41, 0, 0, 0, 0)
exhale stub(0, \"text\", 0, 0, 0, 0)
exhale stub(0, 41, 0, 0, 0, null)
exhale stub(0, [41], 0, 0, 0, 0)
",
    );
    // The first call is of a row; no other is.
    let native = if NATIVE { "41" } else { "-1" };
    assert_eq!(said, format!("{native}\n-1\n-1\n-1\n"));
}

#[test]
fn a_function_through_another_name_and_from_a_closure_is_still_native() {
    let said = prints(
        "stick other = stub
exhale other(0, 7, 0, 0, 0, 0)
pull outer() {
  roll x = 40
  pull bump() { x = x + 1 }
  bump()
  snuff stub(0, x, 0, 0, 0, 0)
}
exhale outer()
",
    );
    assert_eq!(said, if NATIVE { "7\n41\n" } else { "-1\n-1\n" });
}

#[test]
fn a_result_is_of_its_entrys_type() {
    let said = prints(
        "exhale stub(0, 7, 0, 0, 0, 0), stub(0, 2.5, 0, 0, 0, 0)
exhale stubf(0, -0.25, 0, 0, 0, 0), stubb(0, true, 0, 0, 0, 0), stubb(0, false, 0, 0, 0, 0)
exhale stubn(0, null, 0, 0, 0, 0)
exhale type_of(stub(0, 7, 0, 0, 0, 0)), type_of(stubf(0, 1.0, 0, 0, 0, 0)), type_of(stubb(0, true, 0, 0, 0, 0)), type_of(stubn(0, null, 0, 0, 0, 0))
",
    );
    let want = if NATIVE {
        "7 2.5\n-0.25 true false\nnull\nint float bool null\n"
    } else {
        "-1 -1\n-1 -1 -1\n-1\nint int int int\n"
    };
    assert_eq!(said, want);
}

/// What the tree-walker does is what a stage `c` program does: the same
/// script on both, output, errors and exit code. `add` is real code.
#[test]
fn add_says_what_the_tree_walker_says_caught_uncaught_and_as_json() {
    let s = Sandbox::new();
    let rest = "exhale add(40, 2), add(-5, 5)
exhale add(9223372036854775806, 1), add(-9223372036854775807, -1)
exhale add(1, 2.5), add(\"a\", \"b\")
try {
  exhale add(9223372036854775807, 1)
} ashtray e {
  exhale e.kind, e.message, e.line
}
pull deep(n) {
  if n <= 0 { snuff add(n, 1) }
  snuff deep(n - 1) + 1
}
exhale deep(50)
exhale add(-9223372036854775807, -2)
exhale \"not reached\"
";
    let exe = program(&s, "sum", rest);
    for args in [&[][..], &["--plain"][..]] {
        same(
            &s.walker("sum.cig", args),
            &s.prog(&exe, args),
            &format!("{args:?}"),
        );
    }
    // As JSON: everything but the run's own record, which names what ran
    // (a script there, a program here).
    let lines = |o: &Output| -> Vec<String> {
        text(&o.stdout)
            .lines()
            .filter(|l| !l.starts_with("{\"run\":"))
            .map(str::to_string)
            .collect()
    };
    let (walker, prog) = (s.walker("sum.cig", &["--json"]), s.prog(&exe, &["--json"]));
    assert_eq!(
        (walker.status.code(), lines(&walker)),
        (prog.status.code(), lines(&prog))
    );
    assert!(lines(&prog).iter().any(|l| l.contains("\"code\":\"E503\"")));
    let out = s.prog(&exe, &[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("integer overflow in `+`"));
}

/// A native call where a call can stand: in a `try` with a `finally`,
/// in a burn that its error rolls back, as a callback and in tail
/// position (which reach the function as bytecode), with its arguments
/// and its result in variables a closure shares, and in a loop under a
/// step budget.
#[test]
fn a_native_call_is_a_call_wherever_one_stands() {
    let s = Sandbox::new();
    let rest = "pull guarded(a, b) {
  try {
    snuff add(a, b)
  } ashtray e {
    snuff -1
  } finally {
    exhale \"finally\", a
  }
}
exhale guarded(1, 2), guarded(9223372036854775807, 1)
pull passes(a, b) {
  try {
    snuff add(a, b)
  } finally {
    exhale \"leaving\", a
  }
}
try {
  exhale passes(9223372036854775807, 1)
} ashtray e {
  exhale \"caught outside\", e.message, e.line
}
exhale [1, 2, 3].map(pack(x) => add(x, 10))
pull tail(a) { snuff add(a, 1) }
exhale tail(41)
pull shared() {
  roll total = 0
  pull bump(n) { total = add(total, n) }
  bump(5)
  bump(6)
  snuff total
}
exhale shared()
pull owner() {
  roll t = 1
  pull peek() { snuff t }
  t = add(t, 5)
  snuff add(peek(), t)
}
exhale owner()
roll i = 0
roll sum = 0
while i < 50 {
  sum = add(sum, i)
  i += 1
}
exhale sum
try {
  burn {
    fs.write_text(\"kept.txt\", str(add(40, 2)))
  }
  burn {
    fs.write_text(\"gone.txt\", \"written\")
    exhale add(9223372036854775807, 1)
  }
} ashtray e {
  exhale \"the burn failed\", e.message
}
exhale fs.exists(\"kept.txt\"), fs.exists(\"gone.txt\")
";
    let exe = program(&s, "calls", rest);
    let walker = s.walker("calls.cig", &[]);
    assert_eq!(walker.status.code(), Some(0), "{}", text(&walker.stderr));
    let _ = fs::remove_file(s.path().join("kept.txt"));
    same(&walker, &s.prog(&exe, &[]), "a native call in each place");
    // The step budget stops both on the same statement, wherever it
    // falls: before, inside and after the loop of native calls.
    for budget in (1..=40).chain([60, 100, 150, 190, 200, 230, 260, 400]) {
        let b = budget.to_string();
        for f in ["kept.txt", "gone.txt"] {
            let _ = fs::remove_file(s.path().join(f));
        }
        let walker = s.walker("calls.cig", &["--max-steps", &b]);
        for f in ["kept.txt", "gone.txt"] {
            let _ = fs::remove_file(s.path().join(f));
        }
        let prog = s.prog(&exe, &["--max-steps", &b]);
        same(&walker, &prog, &format!("a budget of {budget}"));
    }
}

#[test]
fn the_call_depth_is_the_tree_walkers_at_a_native_call() {
    let s = Sandbox::new();
    // `down(3998)` is 3,999 calls deep where it calls `add`, the 4,000th:
    // the deepest that fits. One more is E506, at the call of `add`.
    let rest = "pull down(n) {
  if n <= 0 { snuff add(1, 2) }
  snuff down(n - 1) + 1
}
exhale down(3998)
try {
  exhale down(3999)
} ashtray e {
  exhale e.kind, e.message, e.line
}
exhale down(3999)
";
    let exe = program(&s, "deep", rest);
    let (walker, prog) = (s.walker("deep.cig", &[]), s.prog(&exe, &[]));
    same(&walker, &prog, "the depth limit");
    assert!(
        text(&prog.stdout).starts_with("4001\n"),
        "{}",
        text(&prog.stdout)
    );
    assert!(
        text(&prog.stderr).contains("E506"),
        "{}",
        text(&prog.stderr)
    );
}

// ----- every way native code comes back --------------------------------

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod exits {
    use super::*;

    /// The program of `rest` run with `args`: its output and its errors.
    fn run(rest: &str, args: &[&str]) -> (String, String, Option<i32>) {
        let s = Sandbox::new();
        let exe = program(&s, "p", rest);
        let out = s.prog(&exe, args);
        (text(&out.stdout), text(&out.stderr), out.status.code())
    }

    /// What a `try` around `call` catches: its kind, message and line.
    fn caught(call: &str) -> String {
        let (out, err, code) = run(
            &format!("try {{\n  exhale {call}\n}} ashtray e {{\n  exhale e.kind, e.message, e.line\n}}\n"),
            &[],
        );
        assert_eq!(code, Some(0), "{err}");
        out.trim_end().to_string()
    }

    fn line_of(mark: &str) -> u32 {
        at(SCRIPT, mark).2
    }

    #[test]
    fn an_operator_that_cannot_give_a_result_is_the_interpreters_error() {
        // An exit names its site; the operands are the stub's `left` and
        // `right`. Floats go as their bits: 1.0, 0.0 and a NaN.
        let one = 1.0f64.to_bits();
        let nan = f64::NAN.to_bits();
        for (call, message, mark) in [
            (
                "stub(0, 9223372036854775807, 1, 0, 0, 1)".to_string(),
                "integer overflow in `+`",
                "a + b",
            ),
            (
                format!("stub(6, {one}, 0, 0, 0, 1)"),
                "division by zero",
                "mark-divide",
            ),
            (
                format!("stub(7, {nan}, 0, 0, 0, 1)"),
                "cannot order a NaN",
                "mark-order",
            ),
            (
                "stub(8, 5, 0, 0, 0, 1)".to_string(),
                "modulo by zero",
                "mark-modulo",
            ),
        ] {
            assert_eq!(
                caught(&call),
                format!("runtime {message} {}", line_of(mark)),
                "{call}"
            );
        }
    }

    #[test]
    fn a_negation_a_range_and_the_depth_are_the_interpreters_errors() {
        assert_eq!(
            caught("stub(4, -9223372036854775807 - 1, 0, 0, 0, 2)"),
            format!(
                "runtime integer overflow in negation {}",
                line_of("mark-negate")
            )
        );
        assert_eq!(
            caught("stub(5, 0, 10000001, 0, 0, 3)"),
            format!(
                "runtime range is too large (limit 10,000,000) {}",
                line_of("mark-range")
            )
        );
        assert_eq!(
            caught("stub(5, -9223372036854775807, 9223372036854775807, 0, 0, 3)"),
            format!(
                "runtime range is too large (limit 10,000,000) {}",
                line_of("mark-range")
            )
        );
        assert_eq!(
            caught("stub(3, 0, 0, 0, 0, 5)"),
            format!(
                "runtime call depth exceeded 4000; is there unbounded recursion? {}",
                line_of("mark-depth")
            )
        );
    }

    #[test]
    fn an_uncaught_exit_points_at_its_site_with_its_code() {
        let (out, err, code) = run(
            "exhale \"before\"\nexhale stub(8, 5, 0, 0, 0, 1)\nexhale \"after\"\n",
            &[],
        );
        assert_eq!((out.as_str(), code), ("before\n", Some(1)));
        let (_, _, line, col) = at(SCRIPT, "mark-modulo");
        assert!(err.contains("error[E502 runtime]: modulo by zero"), "{err}");
        assert!(err.contains(&format!("--> p.cig:{line}:{col}")), "{err}");
    }

    #[test]
    fn the_steps_native_code_takes_are_the_runs() {
        // The call is the first statement: one step. The stub takes
        // five. Each `exhale` after it is one more.
        let rest = "stick a = stub(0, 1, 0, 0, 5, 0)
exhale \"seventh\"
exhale \"eighth\"
exhale \"ninth\"
";
        for (budget, printed) in [
            (6, ""),
            (7, "seventh\n"),
            (8, "seventh\neighth\n"),
            (9, "seventh\neighth\nninth\n"),
        ] {
            let (out, err, code) = run(rest, &["--max-steps", &budget.to_string()]);
            assert_eq!(out, printed, "a budget of {budget}");
            if budget == 9 {
                assert_eq!(code, Some(0), "{err}");
                continue;
            }
            assert_eq!(code, Some(1));
            let want = format!("error[E515 runtime]: the script needs more than {budget} steps");
            assert!(err.contains(&want), "{err}");
        }
    }

    #[test]
    fn a_budget_spent_in_native_code_stops_at_the_mark_worded_by_the_loops_around_it() {
        let script = "the script needs more than 10 steps";
        let looped = "your loop never ends: 10 steps and still going";
        // With a budget of ten and the call in the first statement, nine
        // steps are left: the stub takes ten, as code does that passes
        // its tenth mark, and leaves at a `Budget` site.
        for (rest, message, mark) in [
            // Nothing is looping.
            ("stick a = stub(1, 0, 0, 0, 10, 4)\n", script, "mark-budget"),
            // A native caller on the way out was in a loop: the flag.
            ("stick a = stub(1, 0, 0, 1, 10, 4)\n", looped, "mark-budget"),
            // The mark is in a loop of its own function.
            ("stick a = stub(2, 0, 0, 0, 10, 4)\n", looped, "mark-loop"),
            // The VM's frame that called is in a loop: the `while`, its
            // turn and the statement are three steps, so seven are left.
            (
                "while true {\n  stick a = stub(1, 0, 0, 0, 8, 4)\n}\n",
                looped,
                "mark-budget",
            ),
        ] {
            let (out, err, code) = run(rest, &["--max-steps", "10"]);
            assert_eq!((out.as_str(), code), ("", Some(1)), "{rest}{err}");
            let (_, _, line, col) = at(SCRIPT, mark);
            assert!(
                err.contains(&format!("error[E515 runtime]: {message}")),
                "{rest}{err}"
            );
            assert!(
                err.contains(&format!("--> p.cig:{line}:{col}")),
                "{rest}{err}"
            );
        }
        // One step fewer and the budget is not spent: the run goes on,
        // with exactly one step left.
        let (out, _, code) = run(
            "stick a = stub(1, 0, 0, 0, 8, 0)\nexhale \"tenth\"\nexhale \"eleventh\"\n",
            &["--max-steps", "10"],
        );
        assert_eq!((out.as_str(), code), ("tenth\n", Some(1)));
    }

    #[test]
    fn code_that_leaves_wrongly_is_an_internal_error_not_a_wrong_answer() {
        for (call, said) in [
            // An operator that does give a result.
            (
                "stub(0, 1, 2, 0, 0, 1)",
                "native code left at site 0 with status 1 where the interpreter raises nothing",
            ),
            // A budget exit that did not overspend the budget by the
            // one step the mark took.
            (
                "stub(1, 0, 0, 0, 0, 4)",
                "native code left at site 1 with status 4 and not with the budget one step overspent",
            ),
            // A status that is not the site's kind, and one that is none.
            (
                "stub(0, 0, 0, 0, 0, 4)",
                "native code left at site 0 with status 4 that is not the site's kind",
            ),
            (
                "stub(0, 0, 0, 0, 0, 77)",
                "native code left at site 0 with status 77 that is not the site's kind",
            ),
            // A site the program does not have.
            (
                "stub(99, 0, 0, 0, 0, 1)",
                "native code left with status 1 at site 99, which the program does not have",
            ),
            // A result, with steps given back, and with more taken than
            // there were.
            ("stub(0, 1, 0, 0, -3, 0)", "steps left of"),
            (
                "stub(0, 1, 0, 0, 9223372036854775807, 0)",
                "steps left of",
            ),
        ] {
            let (out, err, code) = run(&format!("exhale {call}\nexhale \"after\"\n"), &[]);
            assert_eq!(out, "", "{call}");
            assert_ne!(code, Some(0), "{call}");
            assert!(err.contains(said), "{call}: {err}");
        }
    }

    // ----- a section that is not this program's ------------------------

    /// The program of `rest` with the section `change` made of the
    /// tests', run plainly and with `CIG_VALIDATE=1`.
    fn with_section(
        rest: &str,
        change: impl FnOnce(&mut native::Section),
    ) -> ((String, Option<i32>), String) {
        let s = Sandbox::new();
        let source = format!("{SCRIPT}{rest}");
        let l = lower(&source);
        let mut section = section(&source, &l);
        change(&mut section);
        let exe = s.compile(
            "p",
            &source,
            &section.to_bytes().expect("a section"),
            &l.module,
        );
        let plain = s.prog(&exe, &[]);
        let checked = output(s.command(&exe).env(program::VALIDATE_ENV, "1")).unwrap();
        assert_eq!(checked.status.code(), Some(3), "{}", text(&checked.stderr));
        (
            (text(&plain.stdout), plain.status.code()),
            text(&checked.stderr),
        )
    }

    #[test]
    fn a_section_for_another_module_or_machine_is_not_run_and_is_damage_when_checked() {
        let rest = "exhale stub(0, 41, 0, 0, 0, 0)\n";
        let bytecode = ("-1\n".to_string(), Some(0));
        for (what, said) in [
            ("module", "it was compiled from another bytecode module"),
            ("machine", "its code is for another machine"),
            (
                "function",
                "is for function 900, which the module does not have",
            ),
            ("top level", "entry 0 is for the top level"),
            ("arguments", "takes 5 arguments, and function"),
        ] {
            let (plain, checked) = with_section(rest, |s| match what {
                "module" => s.module[0] ^= 1,
                "machine" => {
                    s.arch = Arch::Aarch64;
                    s.code.resize(s.code.len().next_multiple_of(4), 0x90);
                    for e in &mut s.entries {
                        e.offset = 0;
                    }
                }
                "function" => s.entries.last_mut().unwrap().func = 900,
                "top level" => s.entries[0].func = 0,
                _ => {
                    for e in s.entries.iter_mut().filter(|e| e.args.len() == 6) {
                        e.args.pop();
                    }
                }
            });
            // Unchecked, the program runs its bytecode and says nothing.
            assert_eq!(plain, bytecode, "{what}");
            assert!(checked.contains("E1050"), "{what}: {checked}");
            assert!(checked.contains(said), "{what}: {checked}");
        }
    }

    #[test]
    fn a_changed_byte_of_the_section_is_not_run_and_is_damage_when_checked() {
        let s = Sandbox::new();
        let exe = program(&s, "p", "exhale stub(0, 41, 0, 0, 0, 0)\n");
        let good = fs::read(&exe).unwrap();
        let mut f = fs::File::open(&exe).unwrap();
        let l = program::read(&mut f).unwrap().unwrap();
        let e = l.entry(NATIVE_SECTION).unwrap();
        let (start, len) = ((l.start + e.offset) as usize, e.length as usize);
        // A byte of the code, of the tables and of the footer.
        for (n, at) in [0, ADD.len() + 3, len - 100, len - 1]
            .into_iter()
            .enumerate()
        {
            let mut bytes = good.clone();
            bytes[start + at] ^= 0x20;
            let exe = s.place(&format!("changed{n}"), &bytes);
            let plain = s.prog(&exe, &[]);
            assert_eq!(
                (text(&plain.stdout).as_str(), plain.status.code()),
                ("-1\n", Some(0)),
                "byte {at}: {}",
                text(&plain.stderr)
            );
            let checked = output(s.command(&exe).env(program::VALIDATE_ENV, "1")).unwrap();
            let said = text(&checked.stderr);
            assert_eq!(checked.status.code(), Some(3), "byte {at}: {said}");
            assert!(
                said.contains("section `native`'s bytes do not match the sha256"),
                "byte {at}: {said}"
            );
        }
        // And the program as built is not damaged.
        let checked = output(s.command(&exe).env(program::VALIDATE_ENV, "1")).unwrap();
        assert_eq!(
            (text(&checked.stdout).as_str(), checked.status.code()),
            ("41\n", Some(0))
        );
    }

    #[test]
    fn the_tree_walker_runs_the_script_when_asked_and_the_bytecode_alone_when_told() {
        let s = Sandbox::new();
        let exe = program(&s, "p", "exhale stub(0, 41, 0, 0, 0, 0)\n");
        assert_eq!(text(&s.prog(&exe, &[]).stdout), "41\n");
        assert_eq!(text(&s.prog(&exe, &["--interp"]).stdout), "-1\n");
        // `CIG_NATIVE=0`: the VM, without the native code.
        let off = |args: &[&str]| {
            let out = output(s.command(&exe).env(program::NATIVE_ENV, "0").args(args)).unwrap();
            assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
            text(&out.stdout)
        };
        assert_eq!(off(&[]), "-1\n");
        assert!(off(&["--cig-program"])
            .contains("native code    not loaded, CIG_NATIVE=0 says: the bytecode runs"));
        // Any other value leaves it on.
        let on = output(s.command(&exe).env(program::NATIVE_ENV, "1")).unwrap();
        assert_eq!(text(&on.stdout), "41\n");
    }
}
