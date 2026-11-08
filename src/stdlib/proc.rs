// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! Hops: running other programs.
//!
//! A child process is the one place the kernel performed nothing, saw
//! nothing and can undo nothing, so the boundary is where the logic is
//! hardest. Every hop here keeps the rules in `docs/HOPS.md`:
//!
//! * no shell unless you ask for one by name (`proc.shell`);
//! * stdout and stderr are read concurrently and never merged;
//! * every hop has a timeout, and a timed-out or signalled child produced
//!   *nothing* as far as the next step is concerned;
//! * the child runs in its own process group and is killed with it, so
//!   grandchildren do not outlive a timeout;
//! * exit codes are a contract (`ok: [0, 1]`), not a boolean;
//! * output is decoded explicitly: invalid UTF-8 is a diagnostic unless the
//!   script asks for `lossy`, `utf-16` or `latin1`;
//! * every failure has its own code (E55x) and names the hop.
//!
//! `proc.run` is the one call shape; `proc.text`, `proc.lines`,
//! `proc.json`, `proc.csv` and `proc.kv` parse stdout into a real value and
//! fail at the hop that produced a bad shape. `proc.pipe` connects stages
//! without a shell.

use super::b;
use crate::burn::{Decision, Op};
use crate::diagnostics::{runtime, type_error, Diagnostic};
use crate::interp::Interp;
use crate::syntax::span::Span;
use crate::value::{expect_str, opt_map, Builtin, Effect, Value};
use indexmap::IndexMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use wait_timeout::ChildExt;

pub static TABLE: &[Builtin] = &[
    b("proc.run", Effect::Proc, 1, Some(3), run),
    b("proc.text", Effect::Proc, 1, Some(3), text),
    b("proc.lines", Effect::Proc, 1, Some(3), lines),
    b("proc.json", Effect::Proc, 1, Some(3), json),
    b("proc.csv", Effect::Proc, 1, Some(3), csv),
    b("proc.kv", Effect::Proc, 1, Some(3), kv),
    b("proc.pipe", Effect::Proc, 1, Some(2), pipe),
    b("proc.shell", Effect::Proc, 1, Some(2), shell),
    b("proc.which", Effect::Read, 1, Some(1), which),
    b("proc.apply_layer", Effect::Write, 1, Some(2), apply_layer),
    b(
        "proc.discard_layer",
        Effect::Write,
        1,
        Some(1),
        discard_layer,
    ),
];

/// Default wall-clock limit for a hop: ten minutes.
pub const DEFAULT_TIMEOUT_MS: u64 = 600_000;
/// After SIGTERM, how long a child gets before SIGKILL.
pub const DEFAULT_GRACE_MS: u64 = 500;
/// How much of a child's output a diagnostic quotes.
const QUOTE_BYTES: usize = 200;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Encoding {
    Utf8,
    Lossy,
    Utf16,
    Latin1,
}

struct Options {
    cwd: Option<PathBuf>,
    env: Vec<(String, String)>,
    clean_env: bool,
    timeout_ms: u64,
    grace_ms: u64,
    stdin: Option<String>,
    /// The exit-code contract; `None` means any code is an answer.
    ok: Option<Vec<i64>>,
    encoding: Encoding,
    header: bool,
    sep: char,
    /// Globs of the files the hop may change, kept in the tin first.
    touches: Vec<String>,
    /// Globs among them whose files are secrets, kept sealed.
    touches_sealed: Vec<String>,
    /// Run in the box: a copy-on-write layer over the pack, applied only
    /// when the hop succeeds.
    boxed: bool,
    /// What the box is asked for beyond the layer (`box: {network, pidns,
    /// allow_hosts, apply}`).
    box_spec: crate::burn::overlay::BoxSpec,
    /// Show the child's output as it arrives (stdout on cig's stdout, stderr
    /// on cig's stderr) as well as capturing it: a long install read live.
    stream: bool,
    /// The hop only reads (a check, a lookup): it is not journaled as a
    /// change. What it does change in the pack is still seen and journaled.
    reads: bool,
}

impl Options {
    fn parse(
        a: &[Value],
        index: usize,
        name: &str,
        s: Span,
        default_ok: Option<Vec<i64>>,
    ) -> Result<Self, Diagnostic> {
        let mut o = Options {
            cwd: None,
            env: Vec::new(),
            clean_env: false,
            timeout_ms: DEFAULT_TIMEOUT_MS,
            grace_ms: DEFAULT_GRACE_MS,
            stdin: None,
            ok: default_ok,
            encoding: Encoding::Utf8,
            header: true,
            sep: ',',
            touches: Vec::new(),
            touches_sealed: Vec::new(),
            boxed: false,
            box_spec: crate::burn::overlay::BoxSpec::default(),
            stream: false,
            reads: false,
        };
        let Some(opts) = opt_map(a, index, name, s)? else {
            return Ok(o);
        };
        // `ok` is the contract itself and wins over `check` whatever the
        // order of the keys; `check` alone sets it to [0] or clears it.
        let mut check: Option<bool> = None;
        let mut ok_given: Option<Option<Vec<i64>>> = None;
        for (k, v) in opts.borrow().iter() {
            match (k.as_str(), v) {
                ("cwd", Value::Str(p)) => o.cwd = Some(PathBuf::from(p.to_string())),
                ("touches", Value::Str(g)) => o.touches = vec![g.to_string()],
                ("touches", Value::List(l)) => {
                    for item in l.borrow().iter() {
                        match item {
                            Value::Str(g) => o.touches.push(g.to_string()),
                            other => {
                                return Err(type_error(format!(
                                    "{name}: `touches` holds a {}; want globs as strings",
                                    other.type_name()
                                ))
                                .at(s))
                            }
                        }
                    }
                }
                ("box", Value::Map(m)) => {
                    o.boxed = true;
                    o.box_spec = parse_box_spec(m, name, s)?;
                }
                ("box", v) => o.boxed = v.truthy(),
                ("touches_sealed", Value::Str(g)) => o.touches_sealed = vec![g.to_string()],
                ("touches_sealed", Value::List(l)) => {
                    for item in l.borrow().iter() {
                        match item {
                            Value::Str(g) => o.touches_sealed.push(g.to_string()),
                            other => {
                                return Err(type_error(format!(
                                    "{name}: `touches_sealed` holds a {}; want globs as strings",
                                    other.type_name()
                                ))
                                .at(s))
                            }
                        }
                    }
                }
                ("env", Value::Map(m)) => {
                    for (ek, ev) in m.borrow().iter() {
                        o.env.push((ek.clone(), ev.display()));
                    }
                }
                ("clean_env", v) => o.clean_env = v.truthy(),
                ("timeout_ms", Value::Int(t)) if *t > 0 => o.timeout_ms = *t as u64,
                ("grace_ms", Value::Int(t)) if *t >= 0 => o.grace_ms = *t as u64,
                ("stdin", Value::Str(text)) => o.stdin = Some(text.to_string()),
                ("check", v) => check = Some(v.truthy()),
                ("ok", Value::Int(n)) => ok_given = Some(Some(vec![*n])),
                ("ok", Value::List(l)) => {
                    let mut codes = Vec::new();
                    for item in l.borrow().iter() {
                        match item {
                            Value::Int(n) => codes.push(*n),
                            other => {
                                return Err(type_error(format!(
                                    "{name}: `ok` lists exit codes (ints), got {}",
                                    other.type_name()
                                ))
                                .at(s))
                            }
                        }
                    }
                    if codes.is_empty() {
                        return Err(runtime(format!(
                            "{name}: the `ok` contract is empty; no exit code could satisfy it"
                        ))
                        .at(s)
                        .with_hint("list the codes that count as success, {ok: [0, 1]}, or ok: null for no contract"));
                    }
                    ok_given = Some(Some(codes));
                }
                ("ok", Value::Null) => ok_given = Some(None),
                ("encoding", Value::Str(e)) => {
                    o.encoding = match e.to_ascii_lowercase().replace('_', "-").as_str() {
                        "utf-8" | "utf8" => Encoding::Utf8,
                        "lossy" => Encoding::Lossy,
                        "utf-16" | "utf16" => Encoding::Utf16,
                        "latin1" | "latin-1" | "iso-8859-1" => Encoding::Latin1,
                        other => {
                            return Err(runtime(format!("{name}: unknown encoding `{other}`"))
                                .at(s)
                                .with_hint("use utf-8 (strict, the default), lossy, utf-16 or latin1"))
                        }
                    }
                }
                ("header", v) => o.header = v.truthy(),
                ("stream", v) => o.stream = v.truthy(),
                ("reads", v) => o.reads = v.truthy(),
                ("sep", Value::Str(c)) => {
                    let mut chars = c.chars();
                    match (chars.next(), chars.next()) {
                        (Some(one), None) => o.sep = one,
                        _ => {
                            return Err(runtime(format!(
                                "{name}: `sep` must be one character, got `{c}`"
                            ))
                            .at(s))
                        }
                    }
                }
                ("cwd" | "env" | "timeout_ms" | "grace_ms" | "stdin" | "ok" | "encoding" | "sep" | "touches" | "touches_sealed", other) => {
                    return Err(type_error(format!(
                        "{name}: option `{k}` has the wrong type ({})",
                        other.type_name()
                    ))
                    .at(s))
                }
                (other, _) => {
                    return Err(runtime(format!("{name}: unknown option `{other}`"))
                        .at(s)
                        .with_hint("use one of cwd, env, clean_env, timeout_ms, grace_ms, stdin, check, ok, encoding, header, sep, touches, touches_sealed, box"))
                }
            }
        }
        match (ok_given, check) {
            (Some(ok), _) => o.ok = ok,
            (None, Some(true)) => o.ok = Some(vec![0]),
            (None, Some(false)) => o.ok = None,
            (None, None) => {}
        }
        // A sealed glob is a touches glob too: its files are kept, sealed.
        for g in &o.touches_sealed {
            if !o.touches.contains(g) {
                o.touches.push(g.clone());
            }
        }
        Ok(o)
    }
}

fn args_of(a: &[Value], index: usize, name: &str, s: Span) -> Result<Vec<String>, Diagnostic> {
    Ok(match a.get(index) {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::List(l)) => l.borrow().iter().map(|v| v.display()).collect(),
        Some(Value::Str(one)) => vec![one.to_string()],
        Some(other) => {
            return Err(type_error(format!(
                "{name}: arguments must be a list, got {}",
                other.type_name()
            ))
            .at(s))
        }
    })
}

/// What a hop produced, undecoded.
struct Raw {
    code: i32,
    out: Vec<u8>,
    err: Vec<u8>,
    duration_ms: i64,
    /// `<run>/<seq>` when the hop's layer was kept rather than applied.
    layer: Option<String>,
    /// What the box saw, for a hop that asked to be judged (`expect` or
    /// `contain`): the changes outside `expect`, the hosts the proxy
    /// refused, and the counts.
    judged: Option<Judgement>,
}

/// A boxed hop's judgement, as the result carries it.
struct Judgement {
    outside: Vec<crate::burn::overlay::Unexpected>,
    refused: Vec<String>,
    changed: [usize; 4],
}

impl Judgement {
    fn of(j: crate::burn::overlay::Judged, refused: Vec<String>) -> Judgement {
        Judgement {
            changed: [j.written, j.removed, j.home_written, j.home_removed],
            outside: j.outside,
            refused,
        }
    }

    /// Into a result or an error's value: `outside`, `refused`, `changed`.
    fn insert(&self, m: &mut IndexMap<String, Value>) {
        let outside: Vec<Value> = self
            .outside
            .iter()
            .map(|u| {
                let mut e = IndexMap::new();
                e.insert("path".to_string(), Value::str(&u.path));
                e.insert("kind".to_string(), Value::str(u.kind));
                Value::map(e)
            })
            .collect();
        m.insert("outside".to_string(), Value::list(outside));
        m.insert(
            "refused".to_string(),
            Value::list(self.refused.iter().map(Value::str).collect()),
        );
        let mut c = IndexMap::new();
        for (k, v) in ["written", "removed", "home_written", "home_removed"]
            .iter()
            .zip(self.changed)
        {
            c.insert(k.to_string(), Value::Int(v as i64));
        }
        m.insert("changed".to_string(), Value::map(c));
    }

    /// One note when the hop wrote outside `expect`: the first few paths,
    /// printed with control characters masked, and how many more.
    fn note(&self, err: &mut dyn std::io::Write, label: &str, held: bool) {
        if self.outside.is_empty() {
            return;
        }
        let shown: Vec<String> = self
            .outside
            .iter()
            .take(5)
            .map(|u| {
                let p = crate::burn::overlay::printable_path(&u.path);
                if u.kind == "write" {
                    p
                } else {
                    format!("{p} ({})", u.kind)
                }
            })
            .collect();
        let more = self.outside.len().saturating_sub(5);
        let _ = writeln!(
            err,
            "note[E781 burn]: `{label}` changed what it was not expected to: {}{}{}",
            shown.join(", "),
            if more > 0 {
                format!(" (and {more} more)")
            } else {
                String::new()
            },
            if held { "; its layer is held back" } else { "" }
        );
    }
}

fn quote(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    let s = s.trim_end();
    let mut q: String = s.chars().take(QUOTE_BYTES).collect();
    if s.chars().count() > QUOTE_BYTES {
        q.push('…');
    }
    q.replace('\n', "⏎")
}

fn last_line(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    s.trim()
        .lines()
        .last()
        .unwrap_or("")
        .chars()
        .take(QUOTE_BYTES)
        .collect()
}

fn hop_label(cmd: &str, args: &[String]) -> String {
    let mut parts = vec![cmd.to_string()];
    parts.extend(args.iter().take(3).cloned());
    if args.len() > 3 {
        parts.push("…".to_string());
    }
    parts.join(" ")
}

/// The Windows `PATHEXT` list, lower-cased with the dots, or the default.
#[cfg(windows)]
fn pathext() -> Vec<String> {
    let raw = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    let mut out: Vec<String> = raw
        .split(';')
        .map(|e| e.trim().to_ascii_lowercase())
        .filter(|e| e.starts_with('.'))
        .collect();
    if out.is_empty() {
        out = vec![".com".into(), ".exe".into(), ".bat".into(), ".cmd".into()];
    }
    out
}

/// Is this directory Windows' System32 (or SysWOW64), where `bash.exe` is
/// the WSL launcher and `wsl.exe` lives? A hop asking for `bash` or `sh`
/// means a shell, never WSL, so those directories are skipped for them.
#[cfg(windows)]
fn is_system_dir(dir: &std::path::Path) -> bool {
    let root = std::env::var_os("SystemRoot")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("C:\\Windows"));
    let lower = dir.to_string_lossy().to_ascii_lowercase();
    for sub in ["system32", "syswow64"] {
        let sys = root.join(sub).to_string_lossy().to_ascii_lowercase();
        if lower.trim_end_matches('\\') == sys.trim_end_matches('\\') {
            return true;
        }
    }
    false
}

/// A candidate on PATH that a hop could start: a regular file, executable
/// where the platform says so.
fn startable(p: &std::path::Path) -> bool {
    let m = match std::fs::metadata(p) {
        Ok(m) => m,
        // A Windows app execution alias (the Store's python.exe) is a
        // reparse point the metadata call cannot follow; the entry itself
        // is what CreateProcess starts.
        #[cfg(windows)]
        Err(_) => match std::fs::symlink_metadata(p) {
            Ok(m) if !m.is_dir() => m,
            _ => return false,
        },
        #[cfg(not(windows))]
        Err(_) => return false,
    };
    if m.is_dir() {
        return false;
    }
    #[cfg(not(windows))]
    if !m.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        // What execvp asks: may this process run it? A file whose execute
        // bit belongs to someone else is passed over, as the shell would.
        use std::os::unix::ffi::OsStrExt;
        let Ok(c) = std::ffi::CString::new(p.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: a NUL-terminated path, read by access(2).
        unsafe { libc::access(c.as_ptr(), libc::X_OK) == 0 }
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Where a hop's program is: the file `proc.run` would start for `name`,
/// so `proc.which` and `proc.run` can never disagree. A name with a path
/// separator is a path, made absolute against the hop's working directory
/// (the platform's own answer differs: Windows resolves it against the
/// parent's directory). A bare name is searched on `PATH`: on Unix the
/// first executable file; on Windows each directory in turn with each
/// `PATHEXT` extension (`npm` is `npm.cmd`), never the WSL launcher in
/// System32 for `bash` or `sh`, and never the current directory.
pub fn resolve_program(name: &str, cwd: Option<&std::path::Path>) -> Option<std::path::PathBuf> {
    resolve_program_on(name, cwd, std::env::var_os("PATH"))
}

/// `resolve_program` on a given PATH: the one the child will search, so a
/// hop that sets its own PATH starts the program that PATH names.
pub fn resolve_program_on(
    name: &str,
    cwd: Option<&std::path::Path>,
    paths: Option<std::ffi::OsString>,
) -> Option<std::path::PathBuf> {
    use std::path::{Path, PathBuf};
    if name.is_empty() {
        return None;
    }
    let is_path = name.contains('/') || name.contains(std::path::MAIN_SEPARATOR);
    if is_path {
        let given = Path::new(name);
        let base: PathBuf = if given.is_absolute() {
            given.to_path_buf()
        } else {
            let dir = cwd
                .map(Path::to_path_buf)
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_default();
            dir.join(given)
        };
        // Absolute, always: a relative program handed to `Command` with
        // a `cwd` is resolved against that cwd by the child, which is not
        // where it was found.
        let absolute = |p: PathBuf| std::path::absolute(&p).unwrap_or(p);
        // Windows starts `.\gradlew` the way cmd does: `gradlew.bat` and
        // the rest of PATHEXT first, the bare file last. The bare file is
        // usually the Unix shell script beside the batch file, which
        // CreateProcess cannot start (error 193).
        #[cfg(windows)]
        if base.extension().is_none() {
            for ext in pathext() {
                let mut with = base.clone().into_os_string();
                with.push(&ext);
                let with = PathBuf::from(with);
                if startable(&with) {
                    return Some(absolute(with));
                }
            }
        }
        if startable(&base) {
            return Some(absolute(base));
        }
        return None;
    }
    // No PATH at all: the platform's own default search, as execvp uses.
    let paths = paths.unwrap_or_else(default_path);
    // A relative PATH entry (`.`, `bin`) is read from where the program
    // will start: the hop's working directory, as execvp in the child
    // would.
    let here = cwd
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    let from_here = |dir: PathBuf| {
        if dir.is_relative() {
            here.join(dir)
        } else {
            dir
        }
    };
    #[cfg(windows)]
    {
        let exts = pathext();
        let lower = name.to_ascii_lowercase();
        let shell_name = matches!(lower.as_str(), "bash" | "sh" | "bash.exe" | "sh.exe");
        let has_known_ext = Path::new(&lower)
            .extension()
            .map(|e| exts.iter().any(|x| x[1..] == e.to_string_lossy()))
            .unwrap_or(false);
        // CreateProcess looks in the system directories whatever PATH
        // says, so `cmd` and `powershell` are found on a hermetic PATH too.
        let mut dirs: Vec<PathBuf> = std::env::split_paths(&paths).collect();
        if let Some(root) = std::env::var_os("SystemRoot") {
            let root = PathBuf::from(root);
            for sys in [root.join("System32"), root] {
                if !dirs
                    .iter()
                    .any(|d| d.as_os_str().eq_ignore_ascii_case(sys.as_os_str()))
                {
                    dirs.push(sys);
                }
            }
        }
        for dir in dirs {
            if dir.as_os_str().is_empty() {
                continue;
            }
            let dir = from_here(dir);
            if shell_name && is_system_dir(&dir) {
                continue;
            }
            if has_known_ext {
                let c = dir.join(name);
                if startable(&c) {
                    return Some(std::path::absolute(&c).unwrap_or(c));
                }
                continue;
            }
            for ext in &exts {
                let c = dir.join(format!("{name}{ext}"));
                if startable(&c) {
                    return Some(std::path::absolute(&c).unwrap_or(c));
                }
            }
        }
        None
    }
    #[cfg(not(windows))]
    {
        for dir in std::env::split_paths(&paths) {
            if dir.as_os_str().is_empty() {
                continue;
            }
            let c = from_here(dir).join(name);
            if startable(&c) {
                return Some(std::path::absolute(&c).unwrap_or(c));
            }
        }
        None
    }
}

/// The search path when the child has none: what execvp falls back to on
/// Unix, the system directories on Windows.
fn default_path() -> std::ffi::OsString {
    #[cfg(windows)]
    {
        let root = std::env::var_os("SystemRoot")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"));
        std::env::join_paths([root.join("System32"), root]).unwrap_or_default()
    }
    #[cfg(not(windows))]
    {
        std::ffi::OsString::from("/usr/local/bin:/usr/bin:/bin")
    }
}

/// The file is a native executable (`.exe`, `.com`), the one kind that
/// reads its command line by the C runtime's rules. A batch file (cmd.exe
/// reads the line again), a name nothing resolved and anything else take
/// the standard library's own quoting.
#[cfg(windows)]
fn native_exe(p: &std::path::Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("exe") || e.eq_ignore_ascii_case("com"))
}

/// One argument on a Windows command line. The standard library quotes an
/// argument only when the C runtime's rules need it (a space, a tab, a
/// quote), and a program built on the MSYS or Cygwin runtime (Git's `bash`,
/// `sh`, `perl`) parses the line by its own rules instead, where a bare `'`
/// opens a quoted string and `*`, `?` and `[` glob: `it's` arrived as `its`.
/// An argument with any of those characters is quoted the way the C runtime
/// reads it, which both parsers agree on; the rest go as they are.
#[cfg(windows)]
fn windows_arg(c: &mut Command, arg: &str) {
    use std::os::windows::process::CommandExt;
    if arg
        .chars()
        .any(|ch| matches!(ch, ' ' | '\t' | '"' | '\'' | '*' | '?' | '['))
    {
        c.raw_arg(quoted_for_crt(arg));
    } else {
        c.arg(arg);
    }
}

/// `"..."` with the C runtime's escapes: a backslash run before a quote is
/// doubled and the quote escaped; a run at the end is doubled; any other
/// backslash is literal.
#[cfg(windows)]
fn quoted_for_crt(arg: &str) -> String {
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
                out.push(ch);
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

/// Build a `Command` the same way for every hop: no shell, its own process
/// group, `NO_COLOR` set, the run id passed down, and either the inherited
/// environment plus the script's additions or a clean documented one. The
/// program is resolved first (`resolve_program`), so what starts is what
/// `proc.which` names: a bare name nothing resolves is `None`, and the hop
/// fails as not found (E551) rather than let the platform search again by
/// rules of its own (the current directory among them). A path nothing
/// resolves is handed over as written, so the platform's reason is told.
fn command(
    cmd: &str,
    args: &[String],
    o: &Options,
    journal: Option<&std::path::Path>,
    plan: Option<&crate::burn::overlay::Plan>,
) -> Option<Command> {
    let resolved = resolve_program_on(cmd, o.cwd.as_deref(), child_path(o));
    let is_path = cmd.contains('/') || cmd.contains(std::path::MAIN_SEPARATOR);
    if resolved.is_none() && !is_path {
        return None;
    }
    #[cfg(windows)]
    let native = resolved.as_deref().is_some_and(native_exe);
    #[cfg(windows)]
    let is_cmd = resolved
        .as_deref()
        .and_then(|p| p.file_stem())
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("cmd"));
    let program = resolved.unwrap_or_else(|| std::path::PathBuf::from(cmd));
    let mut c = Command::new(program);
    #[cfg(unix)]
    {
        // argv[0] stays the name the script typed (a usage line, a
        // multi-call binary); the file that starts is the resolved one.
        use std::os::unix::process::CommandExt;
        c.arg0(cmd);
    }
    #[cfg(windows)]
    {
        // `cmd /d /s /c <line>`: the line reaches cmd exactly as written,
        // in one pair of quotes that /s strips, instead of re-quoted by
        // the C runtime's rules, which cmd does not read.
        let cmd_line =
            is_cmd && args.len() == 4 && args[0] == "/d" && args[1] == "/s" && args[2] == "/c";
        if cmd_line {
            use std::os::windows::process::CommandExt;
            c.args(&args[..3]);
            c.raw_arg(format!("\"{}\"", args[3]));
        } else if native {
            for a in args {
                windows_arg(&mut c, a);
            }
        } else {
            // A .bat or .cmd runs through cmd.exe with the standard
            // library's own quoting for it; so does anything unresolved.
            c.args(args);
        }
    }
    #[cfg(not(windows))]
    c.args(args);
    c.stdin(if o.stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    if let Some(dir) = &o.cwd {
        c.current_dir(dir);
    }
    if o.clean_env {
        c.env_clear();
        for key in [
            "PATH",
            "HOME",
            "USER",
            "LOGNAME",
            "SHELL",
            "TMPDIR",
            "TERM",
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
            "TZ",
            "SYSTEMROOT",
            "USERPROFILE",
            "APPDATA",
            "COMSPEC",
            "PATHEXT",
            "TEMP",
            "TMP",
            "LOCALAPPDATA",
            "WINDIR",
            "SYSTEMDRIVE",
            "PROGRAMDATA",
            "PROGRAMFILES",
            "PROGRAMFILES(X86)",
            "HOMEDRIVE",
            "HOMEPATH",
            "PROCESSOR_ARCHITECTURE",
            "NUMBER_OF_PROCESSORS",
            "OS",
        ] {
            if let Some(v) = std::env::var_os(key) {
                c.env(key, v);
            }
        }
    }
    c.env("NO_COLOR", "1");
    // The run's id travels to every hop: a cig the hop starts knows which
    // run holds the pack it shares (the pack lock lets a child run in), and
    // a tool can name the run in its own records.
    if let Some(id) = crate::crash::active_run() {
        c.env("CIG_RUN_ID", id);
    } else if let Ok(id) = std::env::var("CIG_RUN_ID") {
        c.env("CIG_RUN_ID", id);
    }
    // The burn protocol: where a tool that knows what it changes may say so.
    match journal {
        Some(p) => {
            c.env(crate::burn::protocol::ENV, p);
        }
        None => {
            c.env_remove(crate::burn::protocol::ENV);
        }
    }
    // The box, or the guard alone: between fork and exec the child enters
    // its own user and mount namespaces, mounts each pack root as an
    // overlay over itself (the box), and sees the state directory
    // read-only with the key folder empty (the guard). A cig started in
    // there has the inbox as its own state root; the hop's own `env`,
    // applied after, still has the last word.
    #[cfg(target_os = "linux")]
    let setup = match plan {
        Some(p) => {
            // Absolute: the standard library has already entered the hop's
            // working directory by the time the closure runs, so a relative
            // one would be taken from there, not from here.
            let here = o
                .cwd
                .clone()
                .or_else(|| std::env::current_dir().ok())
                .map(|p| crate::burn::normalize_root(&p.to_string_lossy()))
                .unwrap_or_else(|| std::path::PathBuf::from("/"));
            match crate::burn::overlay::Setup::new(p, &here, &o.box_spec) {
                Ok(setup) => {
                    if let Some(g) = &setup.guard {
                        c.env("CIGSCRIPT_HOME", g.inbox.join("home"));
                        c.env("CIG_KEYS_DIR", g.inbox.join("keys"));
                        c.env("CIG_PARENT_RUN", p.run_id());
                    }
                    // A network of its own: the routes the outer
                    // environment names (its proxies, and their
                    // exceptions) lead nowhere from inside, and a hop has
                    // no business learning them. Only cig's, set below for
                    // the registry network, remain.
                    if setup.netns {
                        for (k, _) in std::env::vars_os() {
                            if is_proxy_var(&k.to_string_lossy()) {
                                c.env_remove(&k);
                            }
                        }
                    }
                    if setup.forward.is_some() {
                        // The one way out: every proxy-aware tool through
                        // the forwarder. A tool that ignores proxies has no
                        // network at all.
                        let url = format!("http://127.0.0.1:{}", crate::burn::overlay::PROXY_PORT);
                        for key in [
                            "HTTP_PROXY",
                            "HTTPS_PROXY",
                            "ALL_PROXY",
                            "http_proxy",
                            "https_proxy",
                            "all_proxy",
                            "npm_config_proxy",
                            "npm_config_https_proxy",
                            "PIP_PROXY",
                            "CARGO_HTTP_PROXY",
                        ] {
                            c.env(key, &url);
                        }
                        c.env("NO_PROXY", "");
                        c.env("no_proxy", "");
                    }
                    Some(setup)
                }
                Err(e) => {
                    let _ = std::fs::write(p.base().join("why"), format!("{e}\n"));
                    return None;
                }
            }
        }
        None => None,
    };
    #[cfg(not(target_os = "linux"))]
    let _ = plan;
    for (k, v) in &o.env {
        c.env(k, v);
    }
    // No way to the key by name, after the hop's own `env` so a script
    // cannot hand one back: the file `CIG_SEAL_KEY_FILE` names is covered
    // and a cig inside keeps its key in the inbox; while the key may be in
    // the OS keyring, no session bus either (its sockets are covered).
    #[cfg(target_os = "linux")]
    if let Some(g) = setup.as_ref().and_then(|x| x.guard.as_ref()) {
        c.env_remove("CIG_SEAL_KEY_FILE");
        if g.bus_cut {
            c.env_remove("DBUS_SESSION_BUS_ADDRESS");
        }
    }
    // A contained hop: its temp is the box's, a cig started in it keeps a
    // state of its own there (thrown away with the hop, never taken in), and
    // nothing names a socket it could not open anyway.
    #[cfg(target_os = "linux")]
    if let Some(ct) = setup.as_ref().and_then(|x| x.contain.as_ref()) {
        let tmp = if ct.tmp_over {
            std::path::PathBuf::from("/tmp")
        } else {
            ct.scratch.join("tmp")
        };
        for k in ["TMPDIR", "TMP", "TEMP"] {
            c.env(k, &tmp);
        }
        c.env("CIGSCRIPT_HOME", tmp.join(".cig-home"));
        c.env("CIG_KEYS_DIR", tmp.join(".cig-keys"));
        c.env("CIG_SEAL_KEY", "file");
        for k in [
            "CIG_SEAL_KEY_FILE",
            "CIG_PARENT_RUN",
            "DBUS_SESSION_BUS_ADDRESS",
            "SSH_AUTH_SOCK",
        ] {
            c.env_remove(k);
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    #[cfg(target_os = "linux")]
    if let Some(setup) = setup {
        use std::os::unix::process::CommandExt;
        // SAFETY: the closure makes only async-signal-safe calls (unshare,
        // open, write, mount, chdir) on data prepared before the fork.
        unsafe {
            c.pre_exec(move || setup.apply());
        }
    }
    Some(c)
}

/// `apply: "ask"`: at a terminal, the layer's summary and one question;
/// away from one, the layer is kept and that is said.
fn ask_to_apply(i: &mut Interp, l: &crate::burn::overlay::Layers, label: &str) -> bool {
    let (written, removed) = crate::burn::overlay::summarize(l).unwrap_or((0, 0));
    if !stdin_is_tty() {
        let _ = writeln!(
            i.err,
            "box: `{label}` asked whether to apply its layer, and there is no terminal to ask; kept"
        );
        return false;
    }
    let _ = writeln!(
        i.err,
        "box: `{label}` changed {written} path{} and removed {removed}; apply? [y/N] ",
        if written == 1 { "" } else { "s" }
    );
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    matches!(line.trim(), "y" | "Y" | "yes")
}

#[cfg(unix)]
fn stdin_is_tty() -> bool {
    // SAFETY: isatty on a descriptor number.
    unsafe { libc::isatty(0) == 1 }
}

#[cfg(not(unix))]
fn stdin_is_tty() -> bool {
    false
}

/// No isolation for untrusted code: the box was asked for a network of its
/// own and cannot be made here, so the hop is refused rather than run bare.
fn e777(name: &str, why: &str) -> Diagnostic {
    runtime(format!(
        "{name}: the box was asked to isolate this hop (its own network) and cannot be made here: {why}; running it unprotected to check it defeats the point"
    ))
    .code("E777")
    .with_hint("on Linux with user namespaces and overlayfs the box is made; `box: true` alone falls back to the tin with a warning")
}

/// `box: {network, allow_hosts, pidns, apply}`.
fn parse_box_spec(
    m: &std::rc::Rc<std::cell::RefCell<IndexMap<String, Value>>>,
    name: &str,
    s: Span,
) -> Result<crate::burn::overlay::BoxSpec, Diagnostic> {
    use crate::burn::overlay::{Apply, BoxSpec, Network};
    let mut spec = BoxSpec::default();
    for (k, v) in m.borrow().iter() {
        match (k.as_str(), v) {
            ("network", Value::Str(n)) => {
                spec.network = match n.to_string().as_str() {
                    "on" => Network::On,
                    "off" => Network::Off,
                    "registry" => Network::Registry,
                    other => {
                        return Err(type_error(format!(
                            "{name}: box.network is `{other}`; want \"on\", \"off\" or \"registry\""
                        ))
                        .at(s))
                    }
                }
            }
            ("allow_hosts", Value::List(l)) => {
                for item in l.borrow().iter() {
                    match item {
                        Value::Str(h) => spec.allow_hosts.push(h.to_string()),
                        other => {
                            return Err(type_error(format!(
                                "{name}: box.allow_hosts holds a {}; want host names as strings",
                                other.type_name()
                            ))
                            .at(s))
                        }
                    }
                }
            }
            ("pidns", v) => spec.pidns = v.truthy(),
            ("contain", v) => spec.contain = v.truthy(),
            ("quiet", v) => spec.quiet = v.truthy(),
            ("expect", Value::List(l)) => {
                for item in l.borrow().iter() {
                    match item {
                        Value::Str(g) => spec.expect.push(g.to_string()),
                        other => {
                            return Err(type_error(format!(
                                "{name}: box.expect holds a {}; want globs as strings",
                                other.type_name()
                            ))
                            .at(s))
                        }
                    }
                }
            }
            ("apply", Value::Str(a)) => {
                spec.apply = match a.to_string().as_str() {
                    "on_success" => Apply::OnSuccess,
                    "ask" => Apply::Ask,
                    "never" => Apply::Never,
                    other => {
                        return Err(type_error(format!(
                        "{name}: box.apply is `{other}`; want \"on_success\", \"ask\" or \"never\""
                    ))
                        .at(s))
                    }
                }
            }
            (other, _) => {
                return Err(type_error(format!(
                    "{name}: box has no option `{other}` (network, allow_hosts, pidns, apply, contain, expect, quiet)"
                ))
                .at(s))
            }
        }
    }
    if spec.contain {
        if spec.network == Network::On {
            return Err(type_error(format!(
                "{name}: box.contain needs network: \"off\" or \"registry\": with the machine's network the hop could reach services on this machine"
            ))
            .at(s));
        }
        if !spec.pidns {
            return Err(type_error(format!(
                "{name}: box.contain needs pidns: the hop must not see your processes"
            ))
            .at(s));
        }
    }
    Ok(spec)
}

/// An environment variable that names a proxy, or an exception to one:
/// `*_PROXY` and `*_proxy` (`HTTPS_PROXY`, `NO_PROXY`, `YARN_HTTPS_PROXY`),
/// `npm_config_*proxy`, `GIT_PROXY_COMMAND`, `GLOBAL_AGENT_*`.
#[cfg(target_os = "linux")]
fn is_proxy_var(name: &str) -> bool {
    let up = name.to_ascii_uppercase();
    up.contains("PROXY") || up.starts_with("GLOBAL_AGENT_")
}

/// The guard's refusal: `tin_guard = on`, and the guard cannot be made.
fn e712(name: &str, why: &str) -> Diagnostic {
    runtime(format!(
        "{name}: the tin guard is required (tin_guard = on) and cannot be made here: {why}"
    ))
    .code("E712")
    .with_hint("cig config tin_guard auto runs the hop without it and says so once; cig doctor explains what this system lacks")
}

/// What the guard wrote when it could not be made for this hop.
fn guard_why(plan: &crate::burn::overlay::Plan) -> Option<String> {
    std::fs::read_to_string(crate::burn::overlay::guard_why_path(&plan.base()))
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// Take in what a cig inside the hop left in the inbox: its runs, now
/// children of this one; anything else is dropped with a note.
fn take_in_children(i: &mut Interp, plan: &crate::burn::overlay::Plan) {
    let taken = crate::burn::inbox::take_in(&plan.run_dir());
    for id in &taken.children {
        let _ = writeln!(
            i.err,
            "child run {id}: a cig inside the hop recorded it; it is listed under this run and unburned with it"
        );
    }
    for note in &taken.notes {
        let _ = writeln!(i.err, "note[E713 burn]: {note}");
    }
}

/// One note per run when the tin guard could not be made under `auto`: the
/// tin is checked (the ledger, the hashes) instead of protected.
fn guard_note_once(i: &mut Interp, why: &str) {
    use std::sync::OnceLock;
    static SAID: OnceLock<()> = OnceLock::new();
    if SAID.set(()).is_ok() {
        let _ = writeln!(
            i.err,
            "note[E712 burn]: this system does not allow cig's tin guard ({why}); the tin is checked instead of protected (cig doctor)"
        );
    }
}

/// The PATH the child will search: the hop's own when its `env` sets one
/// (the last one given wins, as in the environment), else the parent's,
/// which a clean environment copies too.
fn child_path(o: &Options) -> Option<std::ffi::OsString> {
    o.env
        .iter()
        .rev()
        .find(|(k, _)| is_path_key(k))
        .map(|(_, v)| std::ffi::OsString::from(v.to_string()))
        .or_else(|| std::env::var_os("PATH"))
}

#[cfg(windows)]
fn is_path_key(k: &str) -> bool {
    k.eq_ignore_ascii_case("PATH")
}

#[cfg(not(windows))]
fn is_path_key(k: &str) -> bool {
    k == "PATH"
}

/// A missing working directory is the hop's problem, not the program's:
/// the spawn would report "not found" and blame the command.
fn check_cwd(name: &str, o: &Options, s: Span) -> Result<(), Diagnostic> {
    if let Some(dir) = &o.cwd {
        if !dir.is_dir() {
            return Err(runtime(format!(
                "{name}: the working directory `{}` {}",
                dir.display(),
                if dir.exists() {
                    "is not a directory"
                } else {
                    "does not exist"
                }
            ))
            .code("E550")
            .at(s)
            .with_subject(dir.display().to_string())
            .with_hint("create it first (fs.mkdir inside the burn), or drop the cwd option"));
        }
    }
    Ok(())
}

// ----- signals reach the child's process group ---------------------------------
//
// Every child runs in its own process group (so a timeout can kill all of it),
// which also means a Ctrl-C at the terminal reaches cig and not the child.
// The forwarder sends SIGINT, SIGTERM and SIGHUP on to every live child group,
// then lets the signal end cig the default way.

#[cfg(unix)]
mod forward {
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::sync::OnceLock;

    const SLOTS: usize = 64;
    static ACTIVE: [AtomicI32; SLOTS] = [const { AtomicI32::new(0) }; SLOTS];

    extern "C" fn on_signal(sig: libc::c_int) {
        // Async-signal-safe: atomics, kill, signal, raise.
        for slot in ACTIVE.iter() {
            let pgid = slot.load(Ordering::Relaxed);
            if pgid > 0 {
                unsafe {
                    libc::kill(-pgid, sig);
                }
            }
        }
        unsafe {
            libc::signal(sig, libc::SIG_DFL);
            libc::raise(sig);
        }
    }

    pub fn install() {
        static ONCE: OnceLock<()> = OnceLock::new();
        ONCE.get_or_init(|| unsafe {
            for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
                libc::signal(
                    sig,
                    on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
                );
            }
        });
    }

    pub fn register(pgid: u32) -> Option<usize> {
        let pgid = pgid as i32;
        (0..SLOTS).find(|&i| {
            ACTIVE[i]
                .compare_exchange(0, pgid, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        })
    }

    pub fn unregister(slot: usize) {
        ACTIVE[slot].store(0, Ordering::SeqCst);
    }
}

#[cfg(unix)]
const SIGPIPE: i32 = libc::SIGPIPE;
#[cfg(not(unix))]
const SIGPIPE: i32 = 13;

fn install_signal_forwarding() {
    #[cfg(unix)]
    forward::install();
}

/// Keeps a child's process group registered with the signal forwarder for
/// as long as the guard lives.
struct ChildGuard {
    #[cfg(unix)]
    slot: Option<usize>,
}

impl ChildGuard {
    fn register(pid: u32) -> Self {
        #[cfg(unix)]
        {
            Self {
                slot: forward::register(pid),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
            Self {}
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(slot) = self.slot {
            forward::unregister(slot);
        }
    }
}

fn spawn_error(name: &str, cmd: &str, e: std::io::Error, s: Span) -> Diagnostic {
    // execve refuses a directory with the same "permission denied" it uses
    // for a file without the execute bit; say which it was.
    if e.kind() == std::io::ErrorKind::PermissionDenied && std::path::Path::new(cmd).is_dir() {
        return runtime(format!(
            "{name}: could not start `{cmd}`: it is a directory, not a program"
        ))
        .code("E552")
        .at(s)
        .with_subject(cmd.to_string())
        .with_hint("name the program inside it, or check the path");
    }
    #[cfg(windows)]
    if e.kind() == std::io::ErrorKind::NotFound
        && matches!(
            cmd.to_ascii_lowercase().as_str(),
            "bash" | "sh" | "bash.exe" | "sh.exe"
        )
    {
        return runtime(format!(
            "{name}: could not start `{cmd}`: no shell of that name is on PATH outside System32"
        ))
        .code("E551")
        .at(s)
        .with_subject(cmd.to_string())
        .with_hint("Git for Windows or MSYS2 put bash and sh on PATH; the WSL launcher in System32 is never used for a hop");
    }
    let (code, hint) = match e.kind() {
        std::io::ErrorKind::NotFound => (
            "E551",
            "check with proc.which(name); install it, or give the full path",
        ),
        std::io::ErrorKind::PermissionDenied => {
            ("E552", "chmod +x it, or run as a user that may execute it")
        }
        _ => ("E550", "the operating system's reason is in the message"),
    };
    runtime(format!(
        "{name}: could not start `{cmd}`: {}",
        super::fs::describe_io(&e)
    ))
    .code(code)
    .at(s)
    .with_subject(cmd.to_string())
    .with_hint(hint)
}

/// A Windows Job Object around a hop: every process the child starts joins
/// it, so a timeout ends the whole tree the way the process group does on
/// Unix, and closing the last handle (cig itself dying) takes what is left
/// with it (`KILL_ON_JOB_CLOSE`). On other platforms there is no job, and
/// `new` says so.
#[cfg_attr(not(windows), allow(dead_code))]
struct Job {
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
}

impl Job {
    fn new() -> Option<Job> {
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::JobObjects::{
                CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            };
            // SAFETY: plain Win32 calls with valid arguments; the handle is
            // owned by the Job and closed in Drop.
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    return None;
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const std::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if ok == 0 {
                    windows_sys::Win32::Foundation::CloseHandle(handle);
                    return None;
                }
                Some(Job { handle })
            }
        }
        #[cfg(not(windows))]
        {
            None
        }
    }

    /// Put a freshly spawned child in the job. The child is still suspended
    /// (see `start`), so nothing it would start can slip out before it is
    /// in. False when Windows refused, which leaves the hop with no job:
    /// the caller says so once.
    fn assign(&self, child: &Child) -> bool {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
            // SAFETY: both handles are valid for the duration of the call.
            unsafe { AssignProcessToJobObject(self.handle, child.as_raw_handle() as _) != 0 }
        }
        #[cfg(not(windows))]
        {
            let _ = child;
            true
        }
    }

    /// End every process in the job.
    #[cfg_attr(not(windows), allow(dead_code))]
    fn terminate(&self) {
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::JobObjects::TerminateJobObject;
            // SAFETY: the handle is valid; the exit code is ours to pick.
            unsafe {
                TerminateJobObject(self.handle, 1);
            }
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            // SAFETY: closing a handle this struct owns, once.
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(self.handle);
            }
        }
    }
}

/// Start a hop's child so that it joins its job before it runs a single
/// instruction: on Windows the process is created suspended, assigned, then
/// resumed, so a grandchild it starts at once is in the job too (a process
/// created before the assignment never joins it). Elsewhere a plain spawn.
fn start(c: &mut Command, job: Option<&Job>) -> std::io::Result<Child> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_SUSPENDED: u32 = 0x0000_0004;
        if job.is_some() {
            c.creation_flags(CREATE_SUSPENDED);
        }
        let mut child = c.spawn()?;
        if let Some(j) = job {
            let joined = j.assign(&child);
            if let Err(e) = resume(&child) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::other(format!(
                    "the process was created but could not be resumed: {e}"
                )));
            }
            if !joined {
                warn_no_job();
            }
        }
        Ok(child)
    }
    #[cfg(not(windows))]
    {
        let child = c.spawn()?;
        if let Some(j) = job {
            j.assign(&child);
        }
        Ok(child)
    }
}

/// Resume every thread of a process created suspended (one, at that
/// point). Toolhelp lists the threads by owner; `ResumeThread` lets them go.
#[cfg(windows)]
fn resume(child: &Child) -> std::io::Result<()> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};
    let pid = child.id();
    // SAFETY: plain Win32 calls; every handle opened here is closed here.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snap == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }
        let mut entry: THREADENTRY32 = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        let mut resumed = 0u32;
        let mut ok = Thread32First(snap, &mut entry) != 0;
        while ok {
            if entry.th32OwnerProcessID == pid {
                let h = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                if !h.is_null() {
                    if ResumeThread(h) != u32::MAX {
                        resumed += 1;
                    }
                    CloseHandle(h);
                }
            }
            entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
            ok = Thread32Next(snap, &mut entry) != 0;
        }
        CloseHandle(snap);
        if resumed == 0 {
            return Err(std::io::Error::other(
                "no thread of the new process could be resumed",
            ));
        }
    }
    Ok(())
}

/// Said once per process: the hop runs without a job, so a timeout may
/// leave what it started running. A CI log then explains a hang.
#[cfg(windows)]
fn warn_no_job() {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        eprintln!(
            "warning: Windows refused to put a hop's child in a job object ({}); what that hop starts may outlive a timeout",
            std::io::Error::last_os_error()
        );
    });
}

/// Kill a child and everything in its process group: SIGTERM, a grace
/// period, then SIGKILL. Returns once the group is gone. On Windows the
/// job the child was put in is terminated, which reaches the grandchildren
/// a plain kill would leave running and holding the pipes.
fn kill_group(child: &mut Child, grace_ms: u64, job: Option<&Job>) {
    #[cfg(unix)]
    {
        let pid = child.id() as libc::pid_t;
        // The child is its own group leader (process_group(0)).
        unsafe {
            libc::kill(-pid, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_millis(grace_ms);
        loop {
            if let Ok(Some(_)) = child.try_wait() {
                break;
            }
            if Instant::now() >= deadline {
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                }
                let _ = child.wait();
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // Grandchildren that ignored SIGTERM get the SIGKILL too.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = grace_ms;
        if let Some(j) = job {
            j.terminate();
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    #[cfg(unix)]
    let _ = job;
}

/// Run one child to completion under the rules. `Err` for anything that
/// means the next step must not see the output.
fn run_child(
    name: &str,
    cmd: &str,
    args: &[String],
    o: &Options,
    s: Span,
    journal: Option<&std::path::Path>,
    plan: Option<&crate::burn::overlay::Plan>,
) -> Result<Raw, Diagnostic> {
    let started = Instant::now();
    let deadline = started + Duration::from_millis(o.timeout_ms);
    check_cwd(name, o, s)?;
    install_signal_forwarding();
    let mut c = command(cmd, args, o, journal, plan).ok_or_else(|| {
        // The box or the guard could not be set up for this hop: its setup
        // wrote why, and that is the answer, not "not found". The callers
        // read the same reason back (E777, E712, or the tin with E763).
        if let Some(why) = plan.and_then(|p| crate::burn::overlay::why_at(&p.base())) {
            return runtime(format!(
                "{name}: could not start `{cmd}`: the box could not be made: {why}"
            ))
            .code("E550")
            .at(s)
            .with_subject(cmd.to_string());
        }
        spawn_error(
            name,
            cmd,
            std::io::Error::from(std::io::ErrorKind::NotFound),
            s,
        )
    })?;
    let job = Job::new();
    let mut child = start(&mut c, job.as_ref()).map_err(|e| spawn_error(name, cmd, e, s))?;
    let _guard = ChildGuard::register(child.id());
    // Readers first, then a writer thread for stdin: writing before reading
    // deadlocks as soon as the child fills a pipe while waiting for its input.
    let (out_cap, err_cap) = readers(&mut child, o.stream);
    let stdin_thread = child
        .stdin
        .take()
        .zip(o.stdin.clone())
        .map(|(mut pipe, text)| {
            std::thread::spawn(move || {
                // A closed pipe is not an error: the child may not read stdin.
                let _ = pipe.write_all(text.as_bytes());
            })
        });
    let label = hop_label(cmd, args);
    let waited = child
        .wait_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|e| runtime(format!("{name}: {e}")).code("E550").at(s))?;
    match waited {
        None => {
            kill_group(&mut child, o.grace_ms, job.as_ref());
            // A reader still blocked after the kill is held by a process
            // that left the group; it is given a moment, then left behind.
            let _ = wait_closed(
                &[&out_cap, &err_cap],
                Instant::now() + Duration::from_secs(1),
            );
            let out = out_cap.bytes();
            let err = err_cap.bytes();
            if let Some(t) = stdin_thread {
                let _ = t.join();
            }
            Err(runtime(format!(
                "{name}: `{label}` ran longer than {} ms and was killed; it had written {} bytes to stdout{} and {} bytes to stderr{}",
                o.timeout_ms,
                out.len(),
                if out.is_empty() { String::new() } else { format!(" (`{}`)", quote(&out)) },
                err.len(),
                if err.is_empty() { String::new() } else { format!(" (`{}`)", quote(&err)) },
            ))
            .code("E554")
            .at(s)
            .with_subject(cmd.to_string())
            .with_hint("raise {timeout_ms: N} on this hop, or split the work; nothing it wrote was passed on"))
        }
        Some(status) => {
            // The hop is over when its output closes, not when its child
            // exits: a process the child left behind may still hold the
            // pipes. It has until the deadline; past it the rest of the
            // group (the job on Windows) is ended and the hop ran too long.
            if !wait_closed(&[&out_cap, &err_cap], deadline) {
                let held = match (out_cap.is_open(), err_cap.is_open()) {
                    (true, true) => "stdout and stderr",
                    (true, false) => "stdout",
                    _ => "stderr",
                };
                kill_group(&mut child, o.grace_ms, job.as_ref());
                let _ = wait_closed(
                    &[&out_cap, &err_cap],
                    Instant::now() + Duration::from_secs(1),
                );
                let out = out_cap.bytes();
                let err = err_cap.bytes();
                if let Some(t) = stdin_thread {
                    let _ = t.join();
                }
                return Err(runtime(format!(
                    "{name}: `{label}` exited {} but a process it left behind kept its {held} open past the {} ms timeout, so the rest of its process group was killed; it had written {} bytes to stdout and {} bytes to stderr{}",
                    status.code().map(|c| c.to_string()).unwrap_or_else(|| "by signal".to_string()),
                    o.timeout_ms,
                    out.len(),
                    err.len(),
                    if err.is_empty() { String::new() } else { format!(" (`{}`)", quote(&err)) },
                ))
                .code("E554")
                .at(s)
                .with_subject(cmd.to_string())
                .with_hint("a program the hop starts in the background must close its stdout and stderr (redirect them) or end before the hop does; nothing the hop wrote was passed on"));
            }
            let out = out_cap.bytes();
            let err = err_cap.bytes();
            if let Some(t) = stdin_thread {
                let _ = t.join();
            }
            let duration_ms = started.elapsed().as_millis() as i64;
            match status.code() {
                Some(code) => Ok(Raw {
                    code,
                    out,
                    err,
                    duration_ms,
                    layer: None,
                    judged: None,
                }),
                None => {
                    #[cfg(unix)]
                    let signal = {
                        use std::os::unix::process::ExitStatusExt;
                        status.signal().unwrap_or(0)
                    };
                    #[cfg(not(unix))]
                    let signal = 0;
                    Err(runtime(format!(
                        "{name}: `{label}` was killed by signal {signal}{}; it had written {} bytes to stdout and {} bytes to stderr{}",
                        signal_name(signal),
                        out.len(),
                        err.len(),
                        if err.is_empty() { String::new() } else { format!(" (`{}`)", quote(&err)) },
                    ))
                    .code("E555")
                    .at(s)
                    .with_subject(cmd.to_string())
                    .with_hint("the program died, it did not finish; its partial output was not passed on"))
                }
            }
        }
    }
}

fn signal_name(signal: i32) -> &'static str {
    match signal {
        1 => " (SIGHUP)",
        2 => " (SIGINT)",
        6 => " (SIGABRT)",
        9 => " (SIGKILL)",
        11 => " (SIGSEGV)",
        13 => " (SIGPIPE)",
        15 => " (SIGTERM)",
        _ => "",
    }
}

/// One stream of a child, read on its own thread into a buffer the hop can
/// look at whenever it likes. `closed` fires when the stream reaches its
/// end; a stream that never closes (a leftover process holds it) leaves
/// the thread behind, with what was read so far still readable.
struct Capture {
    buf: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    closed: std::sync::mpsc::Receiver<()>,
    open: std::cell::Cell<bool>,
}

/// Where a streamed hop's bytes are shown as they arrive (`stream: true`):
/// the child's stdout on cig's, its stderr on cig's.
#[derive(Clone, Copy)]
enum Tee {
    Stdout,
    Stderr,
}

impl Tee {
    fn show(self, bytes: &[u8]) {
        // A terminal that went away is not the hop's failure: what the child
        // wrote is still captured, and the hop's result is the same.
        let _ = match self {
            Tee::Stdout => {
                let mut o = std::io::stdout().lock();
                o.write_all(bytes).and_then(|()| o.flush())
            }
            Tee::Stderr => {
                let mut e = std::io::stderr().lock();
                e.write_all(bytes).and_then(|()| e.flush())
            }
        };
    }
}

impl Capture {
    fn new<R: Read + Send + 'static>(pipe: Option<R>) -> Capture {
        Capture::shown(pipe, None)
    }

    /// A capture that also shows each chunk on `tee` as it is read.
    fn shown<R: Read + Send + 'static>(pipe: Option<R>, tee: Option<Tee>) -> Capture {
        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (tx, rx) = std::sync::mpsc::channel();
        let into = buf.clone();
        std::thread::spawn(move || {
            if let Some(mut p) = pipe {
                let mut chunk = [0u8; 8192];
                loop {
                    match p.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            into.lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .extend_from_slice(&chunk[..n]);
                            if let Some(t) = tee {
                                t.show(&chunk[..n]);
                            }
                        }
                    }
                }
            }
            let _ = tx.send(());
        });
        Capture {
            buf,
            closed: rx,
            open: std::cell::Cell::new(true),
        }
    }

    /// Wait until the stream closes or `deadline` passes.
    fn wait_closed(&self, deadline: Instant) -> bool {
        if !self.open.get() {
            return true;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        match self.closed.recv_timeout(left) {
            Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                self.open.set(false);
                true
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => false,
        }
    }

    fn is_open(&self) -> bool {
        self.open.get()
    }

    fn bytes(&self) -> Vec<u8> {
        self.buf.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// Wait for every capture to close, or for the deadline. True when all closed.
fn wait_closed(caps: &[&Capture], deadline: Instant) -> bool {
    let mut all = true;
    for c in caps {
        all &= c.wait_closed(deadline);
    }
    all
}

/// Read both streams concurrently: a child filling stderr while stdout is
/// awaited is the classic deadlock.
fn readers(child: &mut Child, stream: bool) -> (Capture, Capture) {
    (
        Capture::shown(child.stdout.take(), stream.then_some(Tee::Stdout)),
        Capture::shown(child.stderr.take(), stream.then_some(Tee::Stderr)),
    )
}

fn decode(
    name: &str,
    stream: &str,
    bytes: &[u8],
    enc: Encoding,
    s: Span,
) -> Result<String, Diagnostic> {
    match enc {
        Encoding::Utf8 => String::from_utf8(bytes.to_vec()).map_err(|e| {
            let at = e.utf8_error().valid_up_to();
            runtime(format!(
                "{name}: {stream} is not valid UTF-8 at byte {at}; the child wrote {} bytes",
                bytes.len()
            ))
            .code("E557")
            .at(s)
            .with_hint("add {encoding: \"lossy\"} to replace bad bytes, \"latin1\" for single-byte text, or \"utf-16\" for a Windows tool")
        }),
        Encoding::Lossy => Ok(String::from_utf8_lossy(bytes).to_string()),
        Encoding::Latin1 => Ok(bytes.iter().map(|&b| b as char).collect()),
        Encoding::Utf16 => {
            let (body, big_endian) = match bytes {
                [0xFF, 0xFE, rest @ ..] => (rest, false),
                [0xFE, 0xFF, rest @ ..] => (rest, true),
                _ => (bytes, false),
            };
            if body.len() % 2 != 0 {
                return Err(runtime(format!(
                    "{name}: {stream} is not valid UTF-16 (odd length {})",
                    body.len()
                ))
                .code("E557")
                .at(s));
            }
            let units: Vec<u16> = body
                .chunks(2)
                .map(|c| {
                    if big_endian {
                        u16::from_be_bytes([c[0], c[1]])
                    } else {
                        u16::from_le_bytes([c[0], c[1]])
                    }
                })
                .collect();
            String::from_utf16(&units).map_err(|_| {
                runtime(format!("{name}: {stream} is not valid UTF-16"))
                    .code("E557")
                    .at(s)
            })
        }
    }
}

/// Enforce the exit-code contract: outside it, the hop failed, with the
/// stderr's last line in the message and the whole result as the payload.
fn enforce_contract(
    i: &mut Interp,
    name: &str,
    label: &str,
    raw: &Raw,
    o: &Options,
    result_value: Value,
    s: Span,
) -> Result<(), Diagnostic> {
    let Some(ok) = &o.ok else {
        return Ok(());
    };
    if ok.contains(&(raw.code as i64)) {
        return Ok(());
    }
    let contract = ok
        .iter()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let tail = last_line(&raw.err);
    i.cough_payload = Some(result_value);
    Err(runtime(format!(
        "{name}: `{label}` exited {} (contract: {contract}){}",
        raw.code,
        if tail.is_empty() {
            String::new()
        } else {
            format!("; stderr: `{tail}`")
        }
    ))
    .code("E553")
    .at(s)
    .with_hint("if that code is an answer, not a failure, say so: {ok: [0, 1]}; read err in the ashtray value otherwise"))
}

fn result_map(code: i32, out: &str, err: &str, duration_ms: i64, simulated: bool) -> Value {
    let mut m = IndexMap::new();
    m.insert("code".to_string(), Value::Int(code as i64));
    m.insert("out".to_string(), Value::str(out));
    m.insert("err".to_string(), Value::str(err));
    m.insert("duration_ms".to_string(), Value::Int(duration_ms));
    m.insert("timed_out".to_string(), Value::Bool(false));
    m.insert("simulated".to_string(), Value::Bool(simulated));
    Value::map(m)
}

/// The end of a hop's watch: its report read (the burn protocol), the
/// claims judged against the pack, the pack compared with the scan before,
/// each warning said once. Without a pack, a compensation still counts and
/// a file claim is said and dropped.
fn finish_watch(
    i: &mut Interp,
    watch: Option<crate::burn::PackWatch>,
    report: Option<&std::path::Path>,
    cwd: Option<&std::path::Path>,
    mark: usize,
    label: &str,
) {
    let mut claims = report.map(crate::burn::protocol::read).unwrap_or_default();
    for warning in &claims.warnings {
        let _ = writeln!(i.err, "warning[E761 burn]: {warning}");
    }
    if watch.is_none() && !claims.claims.is_empty() {
        // No pack: nothing to judge a file claim against, so those are
        // said and dropped; the hop's own undo stands on its own.
        let (compensations, files): (Vec<_>, Vec<_>) = std::mem::take(&mut claims.claims)
            .into_iter()
            .partition(|c| matches!(c, crate::burn::protocol::Claim::Compensate { .. }));
        if !files.is_empty() {
            let _ = writeln!(
                i.err,
                "warning[E761 burn]: the hop reported {} change{} to files, but the script declares no pack {{ }} to judge them against; they stay as the hop's irreversible work",
                files.len(),
                if files.len() == 1 { "" } else { "s" }
            );
        }
        if !compensations.is_empty() {
            i.kernel.hop_compensations(compensations, cwd, mark);
        }
    }
    if let Some(mut w) = watch {
        // Printed here and taken out, so the watch's end does not say them again.
        for warning in std::mem::take(&mut w.warnings) {
            let _ = writeln!(i.err, "warning[E757 burn]: {warning}");
        }
        let writes = i.kernel.pack_watch_end(w, claims.claims, cwd, mark);
        for warning in i.kernel.take_late_warnings() {
            let _ = writeln!(i.err, "warning[E757 burn]: {warning}");
        }
        for warning in i.kernel.take_claim_warnings() {
            let _ = writeln!(i.err, "warning[E761 burn]: {warning}");
        }
        if !writes.outside.is_empty() {
            let shown: Vec<String> = writes
                .outside
                .iter()
                .take(5)
                .map(|p| p.display().to_string())
                .collect();
            let _ = writeln!(
                i.err,
                "warning[E752 burn]: `{label}` wrote outside the pack: {} path{} ({}{}); the kernel cannot undo them",
                writes.outside.len(),
                if writes.outside.len() == 1 { "" } else { "s" },
                shown.join(", "),
                if writes.outside.len() > 5 { ", ..." } else { "" }
            );
        }
    }
}

/// The shared path of every verb: gate the effect, run, decode, enforce.
/// `None` when the hop was simulated.
fn hop(
    i: &mut Interp,
    name: &str,
    cmd: &str,
    args: &[String],
    o: &Options,
    s: Span,
) -> Result<Option<(Raw, String, String)>, Diagnostic> {
    let op = Op::Proc {
        cmd: cmd.to_string(),
        args: args.to_vec(),
        cwd: o.cwd.clone(),
        touches: o.touches.clone(),
        touches_sealed: o.touches_sealed.clone(),
        boxed: o.boxed,
        box_terms: if o.boxed {
            o.box_spec.terms()
        } else {
            Vec::new()
        },
    };
    if !o.touches.is_empty() && i.kernel.pack().is_none() {
        return Err(cigscript_burn(format!(
            "{name}: `touches` was given, but the script declares no pack {{ }}; the kernel has nothing to watch"
        ))
        .code("E757")
        .at(s)
        .with_hint("declare pack { \".\" } (or the roots the hop writes under) as the first statement"));
    }
    // Isolation without a pack: there is nothing to put the box's layer
    // over, and an isolated hop never runs without its box. Refused before
    // it is journaled, since it never runs.
    if o.boxed
        && o.box_spec.network != crate::burn::overlay::Network::On
        && i.kernel.pack().is_none()
    {
        return Err(runtime(format!(
            "{name}: the box was asked to isolate this hop and cannot be made: the script declares no pack {{ }}, and the box is a layer over the pack; running it unprotected to check it defeats the point"
        ))
        .code("E777")
        .at(s)
        .with_hint("declare pack { \".\" } (or the roots the hop writes under) as the first statement"));
    }
    if o.reads && (o.boxed || !o.touches.is_empty()) {
        return Err(type_error(format!(
            "{name}: `reads: true` is for a hop that only reads; it takes no `box` or `touches`"
        ))
        .at(s));
    }
    let mark = i.kernel.mark();
    if o.reads {
        // Not journaled as a change (nothing of it to undo); the pack is
        // still watched around it, so anything it does change there is.
        if !i.burning(name, s)? {
            return Ok(None);
        }
    } else if i.effect(name, op, s)? == Decision::Simulate {
        return Ok(None);
    }
    // The box: the hop writes into a layer over the pack. Where the box
    // cannot be made, the hop runs on the tin and says so.
    // Isolation asked for (a network other than the machine's) is not a
    // thing to fall back from: without the box it is refused (E777).
    let isolation = o.boxed && o.box_spec.network != crate::burn::overlay::Network::On;
    // A hop asked to be isolated is code its caller does not trust, so it
    // gets no burn protocol: an undo it declared there is a command cig
    // would run later, outside every namespace, as you.
    let report = if isolation {
        None
    } else {
        i.kernel.hop_journal()
    };
    let layers: Option<crate::burn::overlay::Layers> = if o.boxed {
        match i.kernel.box_prepare() {
            Some(Ok(l)) => Some(l),
            Some(Err(why)) => {
                if isolation {
                    return Err(e777(name, &why).at(s));
                }
                let _ = writeln!(
                    i.err,
                    "warning[E763 burn]: the box cannot be made here ({why}); `{}` runs on the tin instead",
                    hop_label(cmd, args)
                );
                None
            }
            None => {
                // Nothing to put the layer over: no pack, or a run that keeps
                // no directory for it. Isolation is never run without its
                // box; a plain box falls back to the tin, and says so.
                let why = if i.kernel.pack().is_none() {
                    "the script declares no pack { }, and the box is a layer over the pack"
                } else {
                    "this run keeps no directory for the box's layer"
                };
                if isolation {
                    return Err(runtime(format!(
                        "{name}: the box was asked to isolate this hop and cannot be made: {why}; running it unprotected to check it defeats the point"
                    ))
                    .code("E777")
                    .at(s)
                    .with_hint("declare pack { \".\" } (or the roots the hop writes under) as the first statement"));
                }
                let _ = writeln!(
                    i.err,
                    "warning[E763 burn]: the box cannot be made here ({why}); `{}` runs on the tin instead",
                    hop_label(cmd, args)
                );
                None
            }
        }
    } else {
        None
    };
    if let Some(l) = layers.as_ref() {
        let plan = crate::burn::overlay::Plan::Boxed(l);
        // The registry network: the parent's proxy, up before the hop.
        let run_dir = plan.run_dir();
        let seq = crate::burn::overlay::seq_of(l);
        let events = crate::burn::overlay::events_path(&run_dir, seq);
        let proxy = if o.box_spec.network == crate::burn::overlay::Network::Registry {
            match crate::burn::proxy::start(
                &crate::burn::overlay::proxy_sock_path(&run_dir, seq),
                o.box_spec.allow_hosts.clone(),
                &events,
            ) {
                Ok(p) => Some(p),
                Err(e) => {
                    crate::burn::overlay::discard(l);
                    return Err(e777(name, &format!("the proxy could not be started: {e}")).at(s));
                }
            }
        } else {
            None
        };
        let ran = run_child(name, cmd, args, o, s, report.as_deref(), Some(&plan));
        let mut refused = Vec::new();
        if let Some(p) = proxy {
            p.stop();
            for what in crate::burn::proxy::refused(&events) {
                let _ = writeln!(
                    i.err,
                    "note[E771 burn]: `{}` tried {what}, which is not in allow_hosts; refused",
                    hop_label(cmd, args)
                );
                refused.push(what);
            }
        }
        // `contain`: a cig inside kept a throwaway state in the box's temp;
        // nothing of it is taken in (its records could name any path with
        // any before-image, and it could not write outside the layers).
        let contained = o.box_spec.contain;
        if !contained {
            take_in_children(i, &plan);
        }
        // The judgement, for a hop that asked for one: what its layers hold
        // outside `expect` (the home's too, under `contain`).
        let wants_judgement = contained || !o.box_spec.expect.is_empty();
        let judge_now = |l: &crate::burn::overlay::Layers, refused: &[String]| {
            let home = crate::burn::overlay::home_layer_of(&l.dir);
            let j = crate::burn::overlay::judge(
                l,
                home.as_ref().map(|(h, u)| (h.as_path(), u.as_path())),
                &o.box_spec.expect,
            );
            Judgement::of(j, refused.to_vec())
        };
        // Evidence: a rehearsal (`apply: "never"`) or a contained hop keeps
        // its layer whatever became of the hop, so what it did can be read.
        let keep_evidence = contained || o.box_spec.apply == crate::burn::overlay::Apply::Never;
        // The guard: asked for and not available is a refusal (E712); not
        // available under `auto` is one note, and the hop ran boxed without it.
        if let Some(why) = guard_why(&plan) {
            if crate::burn::overlay::guard_setting() == "on" {
                crate::burn::overlay::discard(l);
                return Err(e712(name, &why).at(s));
            }
            guard_note_once(i, &why);
        }
        // The box could not be made after all (the kernel refused the
        // namespaces or the mount): the hop runs again, on the tin; under
        // `tin_guard = on` it does not, since the guard went with the box.
        let fell_back = match &ran {
            Err(d) if d.code == "E550" || d.code == "E552" => crate::burn::overlay::why(l),
            _ => None,
        };
        if let Some(why) = fell_back {
            if isolation {
                crate::burn::overlay::discard(l);
                return Err(e777(name, &why).at(s));
            }
            if crate::burn::overlay::guard_setting() == "on" {
                crate::burn::overlay::discard(l);
                return Err(e712(name, &why).at(s));
            }
            let _ = writeln!(
                i.err,
                "warning[E763 burn]: the box could not be made here ({why}); `{}` runs on the tin instead",
                hop_label(cmd, args)
            );
            crate::burn::overlay::discard(l);
        } else {
            let claims = report
                .as_deref()
                .map(crate::burn::protocol::read)
                .unwrap_or_default();
            for warning in &claims.warnings {
                let _ = writeln!(i.err, "warning[E761 burn]: {warning}");
            }
            // The hop's own undo, if it gave one, is journaled whatever
            // happens to the layer; its file claims are not needed, the
            // layer is exact.
            let compensations: Vec<crate::burn::protocol::Claim> = claims
                .claims
                .into_iter()
                .filter(|c| matches!(c, crate::burn::protocol::Claim::Compensate { .. }))
                .collect();
            let raw = match ran {
                Ok(raw) => raw,
                Err(d) => {
                    if keep_evidence {
                        if let Some(payload) = keep_as_evidence(
                            i,
                            l,
                            cmd,
                            args,
                            o,
                            wants_judgement.then(|| judge_now(l, &refused)),
                            IndexMap::new(),
                        ) {
                            i.cough_payload = Some(payload);
                            return Err(d);
                        }
                    }
                    crate::burn::overlay::discard(l);
                    if !o.box_spec.quiet {
                        let _ = writeln!(
                            i.err,
                            "{}",
                            format_args!(
                                "box: `{}` failed, so its layer was dropped; nothing of it reached the pack",
                                hop_label(cmd, args)
                            )
                        );
                    }
                    return Err(d);
                }
            };
            let out = decode(name, "stdout", &raw.out, o.encoding, s)?;
            let err = decode(name, "stderr", &raw.err, Encoding::Lossy, s)?;
            let value = result_map(raw.code, &out, &err, raw.duration_ms, false);
            let base = match &value {
                Value::Map(m) => m.borrow().clone(),
                _ => IndexMap::new(),
            };
            if let Err(d) = enforce_contract(i, name, &hop_label(cmd, args), &raw, o, value, s) {
                if keep_evidence {
                    if let Some(payload) = keep_as_evidence(
                        i,
                        l,
                        cmd,
                        args,
                        o,
                        wants_judgement.then(|| judge_now(l, &refused)),
                        base,
                    ) {
                        i.cough_payload = Some(payload);
                        return Err(d);
                    }
                }
                crate::burn::overlay::discard(l);
                if !o.box_spec.quiet {
                    let _ = writeln!(
                        i.err,
                        "box: `{}` exited outside its contract, so its layer was dropped; nothing of it reached the pack",
                        hop_label(cmd, args)
                    );
                }
                return Err(d);
            }
            // The judgement, then the layer's fate: applied (the default),
            // kept (`apply: never`, `ask` away from a terminal, or a
            // contained hop that changed what it was not expected to), or
            // asked about.
            let judgement = wants_judgement.then(|| judge_now(l, &refused));
            let off_footprint = judgement.as_ref().is_some_and(|j| !j.outside.is_empty());
            let keep = match o.box_spec.apply {
                crate::burn::overlay::Apply::OnSuccess => contained && off_footprint,
                crate::burn::overlay::Apply::Never => true,
                crate::burn::overlay::Apply::Ask => !ask_to_apply(i, l, &hop_label(cmd, args)),
            };
            if let Some(j) = judgement.as_ref().filter(|_| !o.box_spec.quiet) {
                j.note(&mut i.err, &hop_label(cmd, args), keep);
            }
            if keep {
                let scratch = crate::burn::overlay::scratch_of(&l.dir);
                let kept = crate::burn::overlay::keep(
                    l,
                    &hop_label(cmd, args),
                    o.cwd.as_deref(),
                    &o.touches_sealed,
                    scratch.as_deref(),
                )
                .map_err(|e| {
                    cigscript_burn(format!("{name}: the layer could not be kept: {e}"))
                        .code("E763")
                        .at(s)
                })?;
                if !o.box_spec.quiet {
                    let _ = writeln!(
                        i.err,
                        "note[E780 burn]: held back: `{}` changed {} path{} and removed {} in the box, not in the pack; cig box apply {} {} keeps them, cig box discard {} {} drops them",
                        hop_label(cmd, args),
                        kept.written,
                        if kept.written == 1 { "" } else { "s" },
                        kept.removed,
                        kept.run,
                        kept.seq,
                        kept.run,
                        kept.seq
                    );
                }
                let mut raw = raw;
                raw.layer = Some(format!("{}/{}", kept.run, kept.seq));
                raw.judged = judgement;
                return Ok(Some((raw, out, err)));
            }
            let applied = i.kernel.box_apply(l, &o.touches_sealed).map_err(|e| {
                cigscript_burn(format!("{name}: {e}"))
                    .code("E763")
                    .at(s)
                    .with_hint("what was applied before the failure is in the journal; the run's rollback puts it back")
            })?;
            if !compensations.is_empty() {
                i.kernel
                    .hop_compensations(compensations, o.cwd.as_deref(), mark);
            }
            if !o.box_spec.quiet {
                let _ = writeln!(
                    i.err,
                    "{}",
                    format_args!(
                        "box: `{}` applied: {} path{} written, {} removed",
                        hop_label(cmd, args),
                        applied.created.len(),
                        if applied.created.len() == 1 { "" } else { "s" },
                        applied.deleted.len()
                    )
                );
            }
            let mut raw = raw;
            raw.judged = judgement;
            return Ok(Some((raw, out, err)));
        }
    }
    // `tin_guard = on` guards a hop outside the box too: the same
    // namespaces without the overlay, and a refusal where they cannot be
    // had.
    let guard = if crate::burn::overlay::guard_setting() == "on" {
        if !cfg!(target_os = "linux") {
            return Err(e712(
                name,
                "the guard is made with user and mount namespaces, which only Linux has",
            )
            .at(s));
        }
        match i.kernel.guard_prepare() {
            Some(p) => Some(p),
            None => {
                return Err(e712(name, "this run keeps no directory to guard from").at(s));
            }
        }
    } else {
        None
    };
    let watch = i
        .kernel
        .pack_watch_begin(o.cwd.as_deref(), &o.touches, &o.touches_sealed);
    let ran = run_child(name, cmd, args, o, s, report.as_deref(), guard.as_ref());
    finish_watch(
        i,
        watch,
        report.as_deref(),
        o.cwd.as_deref(),
        mark,
        &hop_label(cmd, args),
    );
    if let Some(p) = &guard {
        take_in_children(i, p);
        if let Some(why) = crate::burn::overlay::why_at(&p.base()).or_else(|| guard_why(p)) {
            return Err(e712(name, &why).at(s));
        }
    }
    let raw = ran?;
    let out = decode(name, "stdout", &raw.out, o.encoding, s)?;
    let err = decode(name, "stderr", &raw.err, Encoding::Lossy, s)?;
    let value = result_map(raw.code, &out, &err, raw.duration_ms, false);
    enforce_contract(i, name, &hop_label(cmd, args), &raw, o, value, s)?;
    Ok(Some((raw, out, err)))
}

/// `proc.run(cmd, args = [], opts = {})` → `{code, out, err, duration_ms,
/// timed_out, simulated}`. Any exit code is an answer unless `check` or
/// `ok` says otherwise.
/// A layer this run kept, named by the `layer` a boxed hop's result (or
/// error) carried: `<run>/<hop>`.
fn this_runs_layer(
    a: &[Value],
    name: &str,
    s: Span,
) -> Result<crate::burn::overlay::Kept, Diagnostic> {
    let layer = expect_str(a, 0, name, s)?.to_string();
    let Some((run, seq)) = layer.split_once('/') else {
        return Err(e782(name, &format!("`{layer}` is not a layer (want the `layer` a boxed hop's result carried, \"<run>/<hop>\")")).at(s));
    };
    if crate::crash::active_run().as_deref() != Some(run) {
        return Err(e782(name, &format!("the layer {layer} is not this run's; cig box apply {run} {seq} applies another run's")).at(s));
    }
    crate::burn::overlay::kept_layers()
        .into_iter()
        .find(|k| k.run == run && k.seq.to_string() == seq)
        .ok_or_else(|| {
            e782(
                name,
                &format!("this run keeps no layer {layer} (applied or dropped already?)"),
            )
            .at(s)
        })
}

fn e782(name: &str, why: &str) -> Diagnostic {
    cigscript_burn(format!("{name}: {why}"))
        .code("E782")
        .with_hint("proc.apply_layer and proc.discard_layer take the `layer` a boxed hop of this run kept (apply: \"never\", or a contained hop held back)")
}

/// `proc.apply_layer(layer)`: apply a layer this run kept, as native burns
/// journaled with their before-states, so the run unburns it like any other;
/// only while the layer still holds exactly what it held when it was judged.
/// A contained hop's home layer is never applied: it is dropped with the rest.
/// The `home` globs of `proc.apply_layer(layer, {home: [...]})`: the
/// contained hop's home writes to apply, `~/`-relative.
fn home_globs_of(a: &[Value], name: &str, s: Span) -> Result<Vec<String>, Diagnostic> {
    let Some(opts) = opt_map(a, 1, name, s)? else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (k, v) in opts.borrow().iter() {
        match (k.as_str(), v) {
            ("home", Value::List(l)) => {
                for item in l.borrow().iter() {
                    match item {
                        Value::Str(g) => {
                            let g = g.to_string();
                            let rel = g.strip_prefix("~/").unwrap_or(&g).to_string();
                            if rel.is_empty()
                                || rel.starts_with('/')
                                || rel.split('/').any(|c| c == "..")
                            {
                                return Err(type_error(format!(
                                    "{name}: home glob `{g}` must name paths under the home (`~/.cache/**`)"
                                ))
                                .at(s));
                            }
                            out.push(rel);
                        }
                        other => {
                            return Err(type_error(format!(
                                "{name}: `home` holds a {}; want globs as strings",
                                other.type_name()
                            ))
                            .at(s))
                        }
                    }
                }
            }
            (other, _) => {
                return Err(type_error(format!("{name}: unknown option `{other}` (home)")).at(s))
            }
        }
    }
    Ok(out)
}

/// The changes a contained hop's home layer holds that `globs` name, after
/// checking the layer still holds what was judged.
fn home_changes(
    kept: &crate::burn::overlay::Kept,
    globs: &[String],
    name: &str,
    s: Span,
) -> Result<Vec<crate::burn::overlay::Change>, Diagnostic> {
    if globs.is_empty() {
        return Ok(Vec::new());
    }
    let Some((home, upper)) = crate::burn::overlay::home_layer_of(&kept.dir) else {
        return Ok(Vec::new());
    };
    let Some(judged) = &kept.home_digest else {
        return Err(e782(
            name,
            &format!(
                "the layer {}/{} kept no record of its home writes as judged, so none of them is applied",
                kept.run, kept.seq
            ),
        )
        .at(s));
    };
    match crate::burn::overlay::home_digest(&kept.dir) {
        Ok(Some(now)) if &now == judged => {}
        _ => {
            return Err(e782(
                name,
                &format!(
                    "the home writes of the layer {}/{} changed since they were judged, so none of them is applied",
                    kept.run, kept.seq
                ),
            )
            .at(s))
        }
    }
    let mut b = globset::GlobSetBuilder::new();
    for g in globs {
        let glob = globset::GlobBuilder::new(g)
            .literal_separator(true)
            .build()
            .map_err(|e| type_error(format!("{name}: home glob `{g}`: {e}")).at(s))?;
        b.add(glob);
    }
    let set = b
        .build()
        .map_err(|e| type_error(format!("{name}: {e}")).at(s))?;
    let all = crate::burn::overlay::changes(&home, &upper).map_err(|e| {
        e782(
            name,
            &format!(
                "the home layer of {}/{} cannot be read: {e}",
                kept.run, kept.seq
            ),
        )
        .at(s)
    })?;
    Ok(all
        .into_iter()
        .filter(|c| {
            c.real()
                .strip_prefix(&home)
                .map(|rel| set.is_match(rel))
                .unwrap_or(false)
        })
        .collect())
}

fn apply_layer(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let name = "proc.apply_layer";
    let kept = this_runs_layer(a, name, s)?;
    let home_globs = home_globs_of(a, name, s)?;
    let real = i.burning(name, s)?;
    let layers = crate::burn::overlay::layers_of(&kept);
    // The home writes asked for, checked as judged before anything lands.
    let home = home_changes(&kept, &home_globs, name, s)?;
    if let Some(d) = &kept.digest {
        match crate::burn::overlay::layer_digest(&layers) {
            Ok(now) if &now == d => {}
            Ok(_) => {
                return Err(e782(
                    name,
                    &format!(
                        "the layer {}/{} changed since it was judged, so it is not applied",
                        kept.run, kept.seq
                    ),
                )
                .at(s))
            }
            Err(e) => {
                return Err(e782(
                    name,
                    &format!("the layer {}/{} cannot be judged: {e}", kept.run, kept.seq),
                )
                .at(s))
            }
        }
    }
    let mut m = IndexMap::new();
    let home_paths = |cs: &[crate::burn::overlay::Change]| -> Value {
        Value::list(
            cs.iter()
                .map(|c| Value::str(crate::crash::tilde(c.real())))
                .collect(),
        )
    };
    if !real {
        m.insert("written".to_string(), Value::Int(kept.written as i64));
        m.insert("removed".to_string(), Value::Int(kept.removed as i64));
        m.insert("home".to_string(), home_paths(&home));
        m.insert("simulated".to_string(), Value::Bool(true));
        return Ok(Value::map(m));
    }
    // The home writes asked for go first (the pack's apply drops the layer,
    // its home with it): outside the pack, so placed as they are and named
    // in the result, never journaled (nothing could put them back).
    let mut placed = Vec::new();
    for c in home {
        crate::burn::overlay::place(&c).map_err(|e| {
            cigscript_burn(format!(
                "{name}: the home write {} could not be applied: {e}",
                crate::crash::tilde(c.real())
            ))
            .code("E763")
            .at(s)
        })?;
        placed.push(c);
    }
    let applied = i.kernel.box_apply(&layers, &kept.touches_sealed).map_err(|e| {
        cigscript_burn(format!("{name}: {e}"))
            .code("E763")
            .at(s)
            .with_hint("what was applied before the failure is in the journal; the run's rollback puts it back")
    })?;
    m.insert(
        "written".to_string(),
        Value::Int(applied.created.len() as i64),
    );
    m.insert(
        "removed".to_string(),
        Value::Int(applied.deleted.len() as i64),
    );
    m.insert("home".to_string(), home_paths(&placed));
    Ok(Value::map(m))
}

/// `proc.discard_layer(layer)`: drop a layer this run kept, with its record
/// and, for a contained hop, its scratch folder.
fn discard_layer(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let name = "proc.discard_layer";
    let kept = this_runs_layer(a, name, s)?;
    if i.burning(name, s)? {
        crate::burn::overlay::discard(&crate::burn::overlay::layers_of(&kept));
    }
    Ok(Value::Null)
}

/// Keep a failed hop's layer as evidence (a rehearsal, or a contained hop),
/// and give back the error's value: what the result would have carried, the
/// layer, and the judgement. `None` when the layer could not be kept.
fn keep_as_evidence(
    i: &mut Interp,
    l: &crate::burn::overlay::Layers,
    cmd: &str,
    args: &[String],
    o: &Options,
    judgement: Option<Judgement>,
    mut base: IndexMap<String, Value>,
) -> Option<Value> {
    let scratch = crate::burn::overlay::scratch_of(&l.dir);
    let kept = crate::burn::overlay::keep(
        l,
        &hop_label(cmd, args),
        o.cwd.as_deref(),
        &o.touches_sealed,
        scratch.as_deref(),
    )
    .ok()?;
    if let Some(j) = &judgement {
        if !o.box_spec.quiet {
            j.note(&mut i.err, &hop_label(cmd, args), true);
        }
        j.insert(&mut base);
    }
    if !o.box_spec.quiet {
        let _ = writeln!(
            i.err,
            "note[E780 burn]: `{}` failed in the box; its layer is kept to be read: cig box list shows it, cig box discard {} {} drops it",
            hop_label(cmd, args),
            kept.run,
            kept.seq
        );
    }
    base.insert(
        "layer".to_string(),
        Value::str(format!("{}/{}", kept.run, kept.seq)),
    );
    Some(Value::map(base))
}

fn run(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let cmd = expect_str(a, 0, "proc.run", s)?.to_string();
    if cmd.trim().is_empty() {
        return Err(runtime("proc.run: command is empty").at(s));
    }
    let args = args_of(a, 1, "proc.run", s)?;
    let o = Options::parse(a, 2, "proc.run", s, None)?;
    match hop(i, "proc.run", &cmd, &args, &o, s)? {
        None => Ok(result_map(0, "", "", 0, true)),
        Some((raw, out, err)) => Ok(hop_value(&raw, &out, &err)),
    }
}

/// The sugar verbs share one shape: contract `[0]` unless told otherwise,
/// stdout parsed, stderr shown only on failure.
fn sugar(
    i: &mut Interp,
    name: &str,
    a: &[Value],
    s: Span,
) -> Result<Option<(Raw, String)>, Diagnostic> {
    let cmd = expect_str(a, 0, name, s)?.to_string();
    if cmd.trim().is_empty() {
        return Err(runtime(format!("{name}: command is empty")).at(s));
    }
    let args = args_of(a, 1, name, s)?;
    let o = Options::parse(a, 2, name, s, Some(vec![0]))?;
    Ok(hop(i, name, &cmd, &args, &o, s)?.map(|(raw, out, _)| (raw, out)))
}

fn text(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    Ok(match sugar(i, "proc.text", a, s)? {
        None => Value::str(""),
        Some((_, out)) => Value::str(out),
    })
}

fn lines(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    Ok(match sugar(i, "proc.lines", a, s)? {
        None => Value::list(vec![]),
        Some((_, out)) => Value::list(out.lines().map(Value::str).collect()),
    })
}

fn bad_shape(name: &str, expected: &str, out: &str, detail: &str, s: Span) -> Diagnostic {
    runtime(format!(
        "{name}: expected {expected} on stdout, got `{}`{}",
        quote(out.as_bytes()),
        if detail.is_empty() {
            String::new()
        } else {
            format!(" ({detail})")
        }
    ))
    .code("E556")
    .at(s)
    .with_hint("structured data comes from stdout only; send progress to stderr, or use proc.run and parse what you can")
}

fn json(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    match sugar(i, "proc.json", a, s)? {
        None => Ok(Value::Null),
        Some((_, out)) => serde_json::from_str::<serde_json::Value>(&out)
            .map(super::json::to_value)
            .map_err(|e| bad_shape("proc.json", "JSON", &out, &e.to_string(), s)),
    }
}

fn csv(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let o = Options::parse(a, 2, "proc.csv", s, Some(vec![0]))?;
    match sugar(i, "proc.csv", a, s)? {
        None => Ok(Value::list(vec![])),
        Some((_, out)) => {
            if out.trim().is_empty() {
                return Ok(Value::list(vec![]));
            }
            Ok(super::csv::rows_to_value(
                super::csv::parse_rows(&out, o.sep),
                o.header,
            ))
        }
    }
}

/// `KEY=value` lines (an `.env` file, `env`, `git config -l`) as a map.
fn kv(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    match sugar(i, "proc.kv", a, s)? {
        None => Ok(Value::map(IndexMap::new())),
        Some((_, out)) => {
            let mut m = IndexMap::new();
            for (n, line) in out.lines().enumerate() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                // `export FOO=bar`, as `export -p` and .env files write it.
                let line = line
                    .strip_prefix("export ")
                    .map(str::trim_start)
                    .unwrap_or(line);
                let Some((k, v)) = line.split_once('=') else {
                    return Err(bad_shape(
                        "proc.kv",
                        "KEY=value lines",
                        &out,
                        &format!("line {} has no `=`", n + 1),
                        s,
                    ));
                };
                let v = v.trim();
                let v = v
                    .strip_prefix('"')
                    .and_then(|x| x.strip_suffix('"'))
                    .unwrap_or(v);
                m.insert(k.trim().to_string(), Value::str(v));
            }
            Ok(Value::map(m))
        }
    }
}

/// `proc.shell(command, opts)`: the shell, by name, for the honest cases.
/// Everything you pass is interpreted by `sh -c`; that is where injection
/// lives, so this verb exists so that `proc.run` does not have to.
/// The shell `proc.shell` runs a line with, as a program and the arguments
/// before the line. Unix: `sh -c`. Windows: `sh -c` too when an `sh` is on
/// PATH (Git for Windows and MSYS2 ship one; the WSL launcher does not
/// count), else `cmd /d /s /c` with the line reaching cmd exactly as
/// written. The plan names which, so a dry run is honest about it.
pub fn shell_command(cwd: Option<&std::path::Path>) -> (String, Vec<String>) {
    #[cfg(windows)]
    {
        if resolve_program("sh", cwd).is_some() {
            ("sh".to_string(), vec!["-c".to_string()])
        } else {
            (
                "cmd".to_string(),
                vec!["/d".to_string(), "/s".to_string(), "/c".to_string()],
            )
        }
    }
    #[cfg(not(windows))]
    {
        let _ = cwd;
        ("sh".to_string(), vec!["-c".to_string()])
    }
}

fn shell(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let script = expect_str(a, 0, "proc.shell", s)?.to_string();
    let o = Options::parse(a, 1, "proc.shell", s, None)?;
    let (cmd, mut args) = shell_command(o.cwd.as_deref());
    args.push(script.clone());
    match hop(i, "proc.shell", &cmd, &args, &o, s)? {
        None => Ok(result_map(0, "", "", 0, true)),
        Some((raw, out, err)) => Ok(hop_value(&raw, &out, &err)),
    }
}

/// A hop's result as `proc.run` and `proc.shell` give it: the exit code and
/// the output, and for a boxed hop the layer it kept and its judgement.
fn hop_value(raw: &Raw, out: &str, err: &str) -> Value {
    let value = result_map(raw.code, out, err, raw.duration_ms, false);
    if let Value::Map(m) = &value {
        if let Some(layer) = &raw.layer {
            m.borrow_mut()
                .insert("layer".to_string(), Value::str(layer));
        }
        if let Some(j) = &raw.judged {
            j.insert(&mut m.borrow_mut());
        }
    }
    value
}

/// `proc.pipe([[cmd, args...], ...], opts)`: stages connected stdout to
/// stdin, no shell, one process group, one timeout. Every stage must exit
/// 0 (the last one honours `ok`); a failure names the stage.
fn pipe(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let stages: Vec<(String, Vec<String>)> = match a.first() {
        Some(Value::List(l)) => {
            let mut out = Vec::new();
            for (n, stage) in l.borrow().iter().enumerate() {
                let words: Vec<String> = match stage {
                    Value::List(w) => w.borrow().iter().map(|v| v.display()).collect(),
                    Value::Str(one) => vec![one.to_string()],
                    other => {
                        return Err(type_error(format!(
                            "proc.pipe: stage {} must be a list of words, got {}",
                            n + 1,
                            other.type_name()
                        ))
                        .at(s))
                    }
                };
                let Some((cmd, rest)) = words.split_first() else {
                    return Err(runtime(format!("proc.pipe: stage {} is empty", n + 1)).at(s));
                };
                out.push((cmd.clone(), rest.to_vec()));
            }
            out
        }
        _ => {
            return Err(
                type_error("proc.pipe: expected a list of stages, each a list of words")
                    .at(s)
                    .with_hint(
                        "proc.pipe([[\"cat\", \"big.log\"], [\"grep\", \"ERROR\"], [\"sort\"]])",
                    ),
            )
        }
    };
    if stages.is_empty() {
        return Err(runtime("proc.pipe: no stages").at(s));
    }
    let o = Options::parse(a, 1, "proc.pipe", s, None)?;
    if o.boxed {
        return Err(type_error(
            "proc.pipe: `box` is for one hop; a pipeline is several processes and cannot be boxed",
        )
        .at(s)
        .with_hint(
            "run the stages as one hop, proc.shell(\"a | b\", {box: true}), or drop `box`",
        ));
    }
    if !o.touches.is_empty() && i.kernel.pack().is_none() {
        return Err(cigscript_burn(
            "proc.pipe: `touches` was given, but the script declares no pack { }; the kernel has nothing to watch".to_string(),
        )
        .code("E757")
        .at(s)
        .with_hint("declare pack { \".\" } (or the roots the pipeline writes under) as the first statement"));
    }
    // One effect for the pipeline; the plan shows it as one hop.
    let summary_args: Vec<String> = stages
        .iter()
        .map(|(c, args)| hop_label(c, args))
        .collect::<Vec<_>>()
        .join(" | ")
        .split(' ')
        .map(str::to_string)
        .collect();
    let op = Op::Proc {
        cmd: "pipe:".to_string(),
        args: summary_args,
        cwd: o.cwd.clone(),
        touches: o.touches.clone(),
        touches_sealed: o.touches_sealed.clone(),
        boxed: o.boxed,
        box_terms: if o.boxed {
            o.box_spec.terms()
        } else {
            Vec::new()
        },
    };
    let mark = i.kernel.mark();
    if i.effect("proc.pipe", op, s)? == Decision::Simulate {
        return Ok(result_map(0, "", "", 0, true));
    }
    check_cwd("proc.pipe", &o, s)?;
    // The pack is watched around the whole pipeline, and every stage may
    // report through the burn protocol, exactly as one hop does.
    let report = i.kernel.hop_journal();
    let watch = i
        .kernel
        .pack_watch_begin(o.cwd.as_deref(), &o.touches, &o.touches_sealed);
    let pipeline_label = stages
        .iter()
        .map(|(c, a)| hop_label(c, a))
        .collect::<Vec<_>>()
        .join(" | ");
    install_signal_forwarding();
    let started = Instant::now();
    // The clock starts before the first stage does: everything below,
    // feeding stdin included, is inside the timeout.
    let deadline = started + Duration::from_millis(o.timeout_ms);
    let mut children: Vec<Child> = Vec::new();
    // Every stage joins one job on Windows, so one timeout ends them all.
    let job = Job::new();
    // Registered with the signal forwarder for as long as the pipeline runs.
    let mut _group_guard: Option<ChildGuard> = None;
    let mut err_readers: Vec<Capture> = Vec::new();
    let mut stdin_thread: Option<std::thread::JoinHandle<()>> = None;
    let mut previous_out: Option<std::process::ChildStdout> = None;
    for (n, (cmd, args)) in stages.iter().enumerate() {
        let Some(mut c) = command(cmd, args, &o, report.as_deref(), None) else {
            for mut earlier in children.drain(..) {
                kill_group(&mut earlier, 0, job.as_ref());
            }
            return Err(spawn_error(
                &format!("proc.pipe (stage {})", n + 1),
                cmd,
                std::io::Error::from(std::io::ErrorKind::NotFound),
                s,
            ));
        };
        match previous_out.take() {
            Some(prev) => {
                c.stdin(Stdio::from(prev));
            }
            None if n == 0 && o.stdin.is_some() => {
                c.stdin(Stdio::piped());
            }
            None => {
                c.stdin(Stdio::null());
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Every stage joins the first stage's group so one kill ends all.
            if let Some(first) = children.first() {
                c.process_group(first.id() as i32);
            }
        }
        let mut child = start(&mut c, job.as_ref()).map_err(|e| {
            for mut earlier in children.drain(..) {
                kill_group(&mut earlier, 0, job.as_ref());
            }
            spawn_error(&format!("proc.pipe (stage {})", n + 1), cmd, e, s)
        })?;
        if n == 0 {
            // Fed from a thread of its own: a first stage that never reads
            // its stdin would otherwise hold the pipeline, and its timeout,
            // on a full pipe.
            if let (Some(text), Some(mut pipe_in)) = (o.stdin.clone(), child.stdin.take()) {
                stdin_thread = Some(std::thread::spawn(move || {
                    let _ = pipe_in.write_all(text.as_bytes());
                }));
            }
        }
        err_readers.push(Capture::new(child.stderr.take()));
        if n + 1 < stages.len() {
            previous_out = child.stdout.take();
        }
        if n == 0 {
            _group_guard = Some(ChildGuard::register(child.id()));
        }
        children.push(child);
    }
    let last = children.len() - 1;
    let out_reader = Capture::new(children[last].stdout.take());
    let mut codes: Vec<Option<i32>> = vec![None; children.len()];
    // Only Unix fills this in; Windows reports exit codes alone.
    #[cfg_attr(windows, allow(unused_mut))]
    let mut signals: Vec<i32> = vec![0; children.len()];
    let mut timed_out = false;
    'wait: loop {
        for (n, child) in children.iter_mut().enumerate() {
            if codes[n].is_none() {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        codes[n] = Some(status.code().unwrap_or(-1));
                        #[cfg(unix)]
                        {
                            use std::os::unix::process::ExitStatusExt;
                            signals[n] = status.signal().unwrap_or(0);
                        }
                    }
                    Ok(None) => {}
                    Err(_) => codes[n] = Some(-1),
                }
            }
        }
        if codes.iter().all(Option::is_some) {
            break 'wait;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            break 'wait;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    // Every stage has exited; its output has until the deadline to close,
    // in case a stage left a process behind that holds a pipe.
    let mut all_caps: Vec<&Capture> = err_readers.iter().collect();
    all_caps.push(&out_reader);
    let closed = !timed_out && wait_closed(&all_caps, deadline);
    if timed_out || !closed {
        for child in children.iter_mut() {
            kill_group(child, o.grace_ms, job.as_ref());
        }
        let _ = wait_closed(&all_caps, Instant::now() + Duration::from_secs(1));
        if let Some(t) = stdin_thread.take() {
            let _ = t.join();
        }
        finish_watch(
            i,
            watch,
            report.as_deref(),
            o.cwd.as_deref(),
            mark,
            &pipeline_label,
        );
        return Err(runtime(if timed_out {
            format!(
                "proc.pipe: the pipeline ran longer than {} ms and was killed",
                o.timeout_ms
            )
        } else {
            format!(
                "proc.pipe: every stage exited, but a process one left behind kept the pipeline's output open past the {} ms timeout, so the rest of its process group was killed",
                o.timeout_ms
            )
        })
        .code("E554")
        .at(s)
        .with_hint(
            "raise {timeout_ms: N}, or split the pipeline; nothing it wrote was passed on",
        ));
    }
    if let Some(t) = stdin_thread.take() {
        let _ = t.join();
    }
    finish_watch(
        i,
        watch,
        report.as_deref(),
        o.cwd.as_deref(),
        mark,
        &pipeline_label,
    );
    let out_bytes = out_reader.bytes();
    let errs: Vec<Vec<u8>> = err_readers.iter().map(Capture::bytes).collect();
    let duration_ms = started.elapsed().as_millis() as i64;
    // Every stage but the last must exit 0; the last honours the contract.
    // A stage ended by SIGPIPE was cut off by its consumer closing early
    // (`yes | head -1`); whatever the consumer did next is the story, never
    // the producer's death.
    let closed_early = |n: usize| -> bool { n < last && signals[n] == SIGPIPE };
    for (n, code) in codes.iter().enumerate() {
        let code = code.unwrap_or(-1);
        let is_last = n == last;
        let allowed: Vec<i64> = if is_last {
            o.ok.clone().unwrap_or_else(|| vec![0])
        } else {
            vec![0]
        };
        if closed_early(n) {
            continue;
        }
        if !allowed.contains(&(code as i64)) {
            let (cmd, args) = &stages[n];
            let tail = last_line(&errs[n]);
            let out_text = String::from_utf8_lossy(&out_bytes).to_string();
            let err_text = String::from_utf8_lossy(&errs[n]).to_string();
            i.cough_payload = Some(result_map(code, &out_text, &err_text, duration_ms, false));
            return Err(runtime(format!(
                "proc.pipe: stage {} (`{}`) exited {code} (contract: {}){}",
                n + 1,
                hop_label(cmd, args),
                allowed
                    .iter()
                    .map(|c| c.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
                if tail.is_empty() {
                    String::new()
                } else {
                    format!("; stderr: `{tail}`")
                }
            ))
            .code("E553")
            .at(s)
            .with_hint("if that code is an answer for the last stage, say so with {ok: [0, 1]}"));
        }
    }
    let out = decode("proc.pipe", "stdout", &out_bytes, o.encoding, s)?;
    let err: String = errs
        .iter()
        .map(|e| String::from_utf8_lossy(e).to_string())
        .collect::<Vec<_>>()
        .join("");
    let mut m = match result_map(codes[last].unwrap_or(-1), &out, &err, duration_ms, false) {
        Value::Map(m) => m.borrow().clone(),
        _ => unreachable!(),
    };
    m.insert(
        "stages".to_string(),
        Value::list(
            stages
                .iter()
                .zip(codes.iter())
                .enumerate()
                .map(|(n, ((cmd, args), code))| {
                    let mut sm = IndexMap::new();
                    sm.insert("cmd".to_string(), Value::str(hop_label(cmd, args)));
                    sm.insert("code".to_string(), Value::Int(code.unwrap_or(-1) as i64));
                    if signals[n] != 0 {
                        sm.insert("signal".to_string(), Value::Int(signals[n] as i64));
                    }
                    if closed_early(n) {
                        sm.insert("closed_early".to_string(), Value::Bool(true));
                    }
                    Value::map(sm)
                })
                .collect(),
        ),
    );
    Ok(Value::map(m))
}

fn which(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let name = expect_str(a, 0, "proc.which", s)?;
    // Exactly what proc.run would start: a path against the current
    // directory, a bare name on PATH (with PATHEXT on Windows). A path is
    // answered as it was spelled, unless PATHEXT completed its name.
    let is_path = name.contains('/') || name.contains(std::path::MAIN_SEPARATOR);
    Ok(match resolve_program(name, None) {
        Some(p) => {
            let same_name = p.file_name() == std::path::Path::new(name).file_name();
            if is_path && same_name {
                Value::str(name)
            } else {
                Value::str(p.to_string_lossy())
            }
        }
        None => Value::Null,
    })
}

fn cigscript_burn(msg: String) -> Diagnostic {
    crate::diagnostics::burn(msg)
}
