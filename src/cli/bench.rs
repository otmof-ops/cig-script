// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! `cig bench`: one benchmark harness for the whole family (#179, step 1).
//!
//! A repository describes its cases in `bench/bench.toml`. The harness
//! checks each case's output first: a wrong answer is `WRONG` and gets no
//! time. A right answer is sampled in batches, then summarised as median,
//! p10, p90 and CV, with `~` marking a CV over 5% as noisy. Peak resident
//! set comes from `wait4` on Unix; Windows gets wall time only, the other
//! columns `-`. The report states the machine's conditions.
//!
//! Output: a plain table by default, `--json`, or `--markdown`. Exit 0
//! when every case was timed; `SCRIPT_ERROR` (1) when any case was WRONG
//! or could not be measured; `USAGE` (3) for a malformed manifest or a
//! usage mistake.
//!
//! The kernel measures (`tin-growth`, `file:<path>`, `disk-per-byte`,
//! `fsyncs-per-op`) are in `bench/measures.rs`.
//!
//! What is here: step 1 (the manifest, the harness, `--json`,
//! `--markdown`), step 2 (`--save` and `--against`: interleaved A/B and a
//! Mann-Whitney verdict, in `bench/ab.rs`) and step 3 (`--watch`, in
//! `bench/watch.rs`), step 4 (`--timings`, `--stats`, `--syscalls`, in
//! `bench/debug.rs`), step 6 (`cig bench history`, in `bench/history.rs`) and
//! step 7 (`--family`, `--with`, `--matrix`, in `bench/family.rs`). What is
//! not: `--profile` and `--diff`, and the `gate` key. The manifest accepts
//! and ignores the fields reserved for those (`gate`, `score`, `config`), so
//! a manifest written for the whole feature still parses today.
//! The manifest's `format` key is 1; a manifest in any other format is
//! refused rather than guessed at.

use super::{exit, Ctx};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

mod ab;
mod debug;
mod family;
mod history;
mod measures;
mod watch;

use measures::{Kind, Metric};

/// The manifest and baseline format this cig reads and writes.
pub const FORMAT: i64 = 1;

/// What `cig bench` was asked to do.
#[derive(Debug, Default)]
pub struct Opts {
    /// The manifest; defaults to `./bench/bench.toml`.
    pub manifest: Option<PathBuf>,
    /// Only cases whose name contains this substring.
    pub only: Option<String>,
    /// Only cases whose group matches.
    pub group: Option<String>,
    /// Override `[defaults].samples`.
    pub samples: Option<usize>,
    /// Override `[defaults].per`.
    pub per: Option<usize>,
    /// Override `[defaults].warmup`.
    pub warmup: Option<usize>,
    /// Write a Markdown table instead of the plain one.
    pub markdown: bool,
    /// Keep this run as the baseline of that name.
    pub save: Option<String>,
    /// Compare, interleaved, against a baseline, release, git ref or binary.
    pub against: Option<String>,
    /// Rerun when a watched source changes.
    pub watch: bool,
    /// Show each case's phase breakdown (an extra, untimed run).
    pub timings: bool,
    /// Show each case's counters (an extra, untimed run).
    pub stats: bool,
    /// Count each case's syscalls (an extra, untimed run under strace).
    pub syscalls: bool,
    /// Run every product's suite (step 7).
    pub family: bool,
    /// `PRODUCT@VERSION-OR-PATH`: swap one product's binary.
    pub with: Vec<String>,
    /// `PRODUCT=V1,V2,...`: every suite once per value.
    pub matrix: Option<String>,
    /// `NAME=PATH`: where a product's checkout or manifest is.
    pub products: Vec<String>,
}

/// `cig bench history [CASE]`: one case's nightly history.
pub fn history(
    ctx: &Ctx,
    case: Option<&str>,
    manifest: Option<PathBuf>,
    file: Option<PathBuf>,
    branch: Option<String>,
) -> i32 {
    history::history(ctx, case, manifest, file, branch)
}

impl Opts {
    fn wants(&self) -> debug::Wants {
        debug::Wants {
            timings: self.timings,
            stats: self.stats,
            syscalls: self.syscalls,
        }
    }
}

/// Everything one `cig bench` invocation shares between its modes.
struct Job<'a> {
    ctx: &'a Ctx,
    opts: &'a Opts,
    manifest: &'a Manifest,
    manifest_path: &'a Path,
    manifest_dir: &'a Path,
    defaults: &'a Defaults,
    cases: Vec<&'a Case>,
}

/// Entry point for `cig bench`.
pub fn bench(ctx: &Ctx, o: Opts) -> i32 {
    if o.family {
        return family::family(ctx, &o);
    }
    if !o.with.is_empty() || o.matrix.is_some() || !o.products.is_empty() {
        eprintln!(
            "{} `--with`, `--matrix` and `--product` belong to `--family`",
            ctx.red("error[E800 usage]:")
        );
        return exit::USAGE;
    }
    let path = o
        .manifest
        .clone()
        .unwrap_or_else(|| PathBuf::from("bench").join("bench.toml"));
    let text = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "{} cannot read {}: {e}",
                ctx.red("error[E800 usage]:"),
                path.display()
            );
            return exit::USAGE;
        }
    };
    let manifest = match parse_manifest(&text) {
        Ok(m) => m,
        Err(e) => {
            eprintln!(
                "{} {}: {}",
                ctx.red("error[E800 usage]:"),
                path.display(),
                e
            );
            return exit::USAGE;
        }
    };
    let manifest_dir = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let defaults = Defaults {
        samples: o.samples.or(manifest.defaults.samples).unwrap_or(9),
        per: o.per.or(manifest.defaults.per).unwrap_or(5),
        warmup: o.warmup.or(manifest.defaults.warmup).unwrap_or(1),
        env: manifest.defaults.env.clone(),
        home: manifest.bench.home,
    };
    // No samples, or batches of nothing, would print a time that was never
    // measured.
    if defaults.samples == 0 || defaults.per == 0 {
        eprintln!(
            "{} `samples` and `per` are at least 1 (here {} and {})",
            ctx.red("error[E800 usage]:"),
            defaults.samples,
            defaults.per
        );
        return exit::USAGE;
    }
    let cases: Vec<&Case> = manifest
        .cases
        .iter()
        .filter(|c| o.only.as_ref().is_none_or(|s| c.name.contains(s)))
        .filter(|c| {
            o.group
                .as_ref()
                .is_none_or(|g| c.group.as_deref() == Some(g.as_str()))
        })
        .collect();
    if cases.is_empty() {
        eprintln!(
            "{} no case in {} matches",
            ctx.red("error[E800 usage]:"),
            path.display()
        );
        return exit::USAGE;
    }
    if let Some(name) = &o.save {
        if let Err(e) = ab::check_baseline_name(name) {
            eprintln!("{} {e}", ctx.red("error[E800 usage]:"));
            return exit::USAGE;
        }
    }
    let job = Job {
        ctx,
        opts: &o,
        manifest: &manifest,
        manifest_path: &path,
        manifest_dir: &manifest_dir,
        defaults: &defaults,
        cases,
    };
    if o.wants().any() && (o.watch || o.against.is_some()) {
        eprintln!(
            "{} `--timings`, `--stats` and `--syscalls` describe one run of one build: use them without `--against` and `--watch`",
            ctx.red("error[E800 usage]:")
        );
        return exit::USAGE;
    }
    if o.watch {
        if o.save.is_some() {
            eprintln!(
                "{} `--save` and `--watch` do not go together: press `b` in a watch to keep the build as the baseline",
                ctx.red("error[E800 usage]:")
            );
            return exit::USAGE;
        }
        return watch::watch(&job);
    }
    let scratch = match scratch_root() {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "{} cannot make a private scratch folder under {}: {e}",
                ctx.red("error[E800 usage]:"),
                std::env::temp_dir().display()
            );
            return exit::USAGE;
        }
    };
    let code = match &o.against {
        Some(spec) => ab::against(&job, spec, &scratch),
        None => {
            let side = ab::current_side(&job);
            let rows = run_rows(&job, &side, &scratch);
            let conditions = Conditions::here();
            if ctx.json {
                print_json(&manifest, &rows, &conditions);
            } else if o.markdown {
                print_markdown(&manifest, &rows, &conditions);
            } else {
                print_table(&manifest, &rows, &conditions);
            }
            report_failures(&rows);
            let mut code = exit_for(&rows);
            if let Some(name) = &o.save {
                if let Err(e) = ab::save_baseline(&job, &side, name, &rows, &conditions) {
                    eprintln!("{} {e}", ctx.red("error[E800 usage]:"));
                    code = exit::USAGE;
                }
            }
            code
        }
    };
    let _ = std::fs::remove_dir_all(&scratch);
    code
}

/// Measure every selected case on one side, one after the other.
fn run_rows(job: &Job, side: &Side, scratch: &Path) -> Vec<Row> {
    job.cases
        .iter()
        .map(|case| run_case(side, scratch, case, job.defaults, job.opts.wants()))
        .collect()
}

fn report_failures(rows: &[Row]) {
    for row in rows {
        match &row.outcome {
            Outcome::Wrong(why) => eprintln!("bench: {} is WRONG: {why}", row.name),
            Outcome::Absent(why) => eprintln!("bench: {}: {why}", row.name),
            Outcome::Timed(_) => {}
        }
    }
}

fn exit_for(rows: &[Row]) -> i32 {
    if rows
        .iter()
        .any(|r| matches!(r.outcome, Outcome::Wrong(_) | Outcome::Absent(_)))
    {
        exit::SCRIPT_ERROR
    } else {
        exit::OK
    }
}

/// One side of a measurement: the files it runs against and, when the
/// product is a binary, which binary stands in for the product's name.
#[derive(Clone, Debug)]
struct Side {
    /// "A" / "B" in a comparison; the product's name otherwise.
    label: String,
    product: String,
    /// Replaces any command whose file stem is the product's name.
    binary: Option<PathBuf>,
    /// The folder the manifest's relative paths and the cases' working
    /// directory resolve against.
    dir: PathBuf,
}

impl Side {
    fn argv(&self, argv: &[String]) -> Vec<String> {
        let Some(bin) = &self.binary else {
            return argv.to_vec();
        };
        let stem = Path::new(&argv[0]).file_stem().and_then(|s| s.to_str());
        if stem == Some(self.product.as_str()) {
            let mut out = argv.to_vec();
            out[0] = bin.to_string_lossy().into_owned();
            out
        } else {
            argv.to_vec()
        }
    }
}

// ----- manifest --------------------------------------------------------------

#[derive(Debug)]
struct Manifest {
    bench: Bench,
    defaults: DefaultsOpt,
    cases: Vec<Case>,
}

#[derive(Debug, Default)]
struct Bench {
    product: String,
    version_cmd: Option<Vec<String>>,
    home: HomeMode,
    /// What `--against <git ref>` and `--watch` run before measuring.
    build: Option<Vec<String>>,
    /// The product's binary once built, relative to the project folder.
    binary: Option<String>,
    /// Globs (relative to the project folder) `--watch` follows.
    watch: Vec<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
enum HomeMode {
    #[default]
    Isolated,
    Shared,
}

#[derive(Debug, Default)]
struct DefaultsOpt {
    samples: Option<usize>,
    per: Option<usize>,
    warmup: Option<usize>,
    env: BTreeMap<String, String>,
}

struct Defaults {
    samples: usize,
    per: usize,
    warmup: usize,
    env: BTreeMap<String, String>,
    home: HomeMode,
}

#[derive(Debug)]
struct Case {
    name: String,
    group: Option<String>,
    run: Vec<String>,
    stdin: Option<String>,
    expect: Expect,
    setup: Option<Vec<String>>,
    /// Run once per sample, in order, not timed. One command or a list.
    before: Vec<Vec<String>>,
    /// Run once after each sample's timed batch, not timed; its stdout is
    /// checked against `expect.after_stdout`.
    after: Option<Vec<String>>,
    budget: Option<String>,
    /// This case's own batch size; wins over the defaults and `--per`.
    per: Option<usize>,
    /// Bytes one run burns: what `disk-per-byte` divides by.
    burned: Option<u64>,
    /// Operations one run performs: what `fsyncs-per-op` divides by.
    ops: Option<u64>,
    env: BTreeMap<String, String>,
    /// Which metrics to record. `wall` and `rss` are always taken; the
    /// kernel measures are `tin-growth`, `file:<path>`, `disk-per-byte` and
    /// `fsyncs-per-op`; any other entry is accepted so a manifest written
    /// for a later step still parses.
    measure: Vec<String>,
    /// Globs that narrow which changes rerun this case in `--watch`; empty
    /// means every change does.
    watch: Vec<String>,
}

#[derive(Debug)]
struct Expect {
    /// The command's exit code. `None` means no check.
    exit: Option<i32>,
    /// Path (relative to the manifest) to a file holding the expected
    /// stdout. `None` means no stdout check.
    stdout_file: Option<PathBuf>,
    /// Path to the expected stdout of the case's `after` command.
    after_stdout_file: Option<PathBuf>,
}

impl Default for Expect {
    fn default() -> Self {
        // A case with no `expect` table still demands exit 0: a crashing
        // program is a bug, not a result.
        Expect {
            exit: Some(0),
            stdout_file: None,
            after_stdout_file: None,
        }
    }
}

/// Parse the manifest, rejecting unknown keys with a line number.
fn parse_manifest(text: &str) -> Result<Manifest, String> {
    let value: toml::Value = text
        .parse()
        .map_err(|e: toml::de::Error| format!("the manifest is not TOML: {}", e.message()))?;
    let root = value
        .as_table()
        .ok_or_else(|| "the manifest must be a TOML table".to_string())?;
    // A case's reserved fields: accepted and silently ignored in step 1.
    let case_reserved: &[&str] = &["config", "gate", "score"];
    for (k, v) in root {
        match k.as_str() {
            "bench" | "defaults" | "case" => {}
            "format" => match v.as_integer() {
                Some(FORMAT) => {}
                Some(n) => {
                    return Err(format!(
                        "this manifest is format {n}; this cig reads format {FORMAT} (update cig, or the manifest)"
                    ))
                }
                None => return Err(format!("`format` is the integer {FORMAT}, not {}", v.type_str())),
            },
            _ => return Err(unknown_key(text, k, v)),
        }
    }
    let bench_tbl = root
        .get("bench")
        .ok_or_else(|| "a `[bench]` table is required".to_string())?
        .as_table()
        .ok_or_else(|| "`bench` must be a table".to_string())?;
    let mut bench = Bench::default();
    for (k, v) in bench_tbl {
        match k.as_str() {
            "product" => {
                bench.product = v
                    .as_str()
                    .ok_or_else(|| "`bench.product` must be a string".to_string())?
                    .to_string();
            }
            "version" => bench.version_cmd = Some(as_argv(v, "bench.version")?),
            "home" => {
                let s = v
                    .as_str()
                    .ok_or_else(|| "`bench.home` must be a string".to_string())?;
                bench.home = match s {
                    "isolated" => HomeMode::Isolated,
                    "shared" => HomeMode::Shared,
                    _ => {
                        return Err(format!(
                            "`bench.home` is \"isolated\" or \"shared\", not {s:?}"
                        ))
                    }
                };
            }
            "build" => bench.build = Some(as_argv(v, "bench.build")?),
            "binary" => {
                bench.binary = Some(
                    v.as_str()
                        .ok_or_else(|| "`bench.binary` must be a string".to_string())?
                        .to_string(),
                )
            }
            "watch" => bench.watch = as_strings(v, "bench.watch")?,
            _ => return Err(unknown_key(text, &format!("bench.{k}"), v)),
        }
    }
    if bench.product.is_empty() {
        return Err("`bench.product` is required".to_string());
    }
    let mut defaults = DefaultsOpt::default();
    if let Some(dv) = root.get("defaults") {
        let dt = dv
            .as_table()
            .ok_or_else(|| "`defaults` must be a table".to_string())?;
        for (k, v) in dt {
            match k.as_str() {
                "samples" => defaults.samples = Some(as_usize(v, "defaults.samples")?),
                "per" => defaults.per = Some(as_usize(v, "defaults.per")?),
                "warmup" => defaults.warmup = Some(as_usize(v, "defaults.warmup")?),
                "env" => defaults.env = as_env(v, "defaults.env")?,
                _ => return Err(unknown_key(text, &format!("defaults.{k}"), v)),
            }
        }
    }
    let mut cases = Vec::new();
    if let Some(cv) = root.get("case") {
        let arr = cv
            .as_array()
            .ok_or_else(|| "`case` must be `[[case]]` entries".to_string())?;
        for (i, item) in arr.iter().enumerate() {
            let t = item
                .as_table()
                .ok_or_else(|| format!("case #{}: not a table", i + 1))?;
            let mut case = Case {
                name: String::new(),
                group: None,
                run: Vec::new(),
                stdin: None,
                expect: Expect::default(),
                setup: None,
                before: Vec::new(),
                after: None,
                budget: None,
                per: None,
                burned: None,
                ops: None,
                env: BTreeMap::new(),
                measure: Vec::new(),
                watch: Vec::new(),
            };
            for (k, v) in t {
                match k.as_str() {
                    "name" => {
                        case.name = v
                            .as_str()
                            .ok_or_else(|| format!("case #{}: `name` is a string", i + 1))?
                            .to_string();
                    }
                    "group" => {
                        case.group = Some(
                            v.as_str()
                                .ok_or_else(|| format!("case #{}: `group` is a string", i + 1))?
                                .to_string(),
                        );
                    }
                    "run" => case.run = as_argv(v, &format!("case #{} `run`", i + 1))?,
                    "stdin" => {
                        case.stdin = Some(
                            v.as_str()
                                .ok_or_else(|| format!("case #{}: `stdin` is a string", i + 1))?
                                .to_string(),
                        );
                    }
                    "expect" => case.expect = parse_expect(v, i + 1)?,
                    "setup" => case.setup = Some(as_argv(v, &format!("case #{} `setup`", i + 1))?),
                    "before" => case.before = as_commands(v, &format!("case #{} `before`", i + 1))?,
                    "after" => case.after = Some(as_argv(v, &format!("case #{} `after`", i + 1))?),
                    "per" => case.per = Some(as_usize(v, &format!("case #{} `per`", i + 1))?),
                    "burned" => {
                        case.burned =
                            Some(as_usize(v, &format!("case #{} `burned`", i + 1))? as u64)
                    }
                    "ops" => {
                        case.ops = Some(as_usize(v, &format!("case #{} `ops`", i + 1))? as u64)
                    }
                    "budget" => {
                        case.budget = Some(
                            v.as_str()
                                .ok_or_else(|| format!("case #{}: `budget` is a string", i + 1))?
                                .to_string(),
                        );
                    }
                    "env" => case.env = as_env(v, &format!("case #{} `env`", i + 1))?,
                    "watch" => case.watch = as_strings(v, &format!("case #{} `watch`", i + 1))?,
                    "measure" => {
                        let arr = v
                            .as_array()
                            .ok_or_else(|| format!("case #{}: `measure` is an array", i + 1))?;
                        for m in arr {
                            case.measure.push(
                                m.as_str()
                                    .ok_or_else(|| {
                                        format!("case #{}: `measure` holds strings", i + 1)
                                    })?
                                    .to_string(),
                            );
                        }
                    }
                    k if case_reserved.contains(&k) => {}
                    _ => return Err(unknown_key(text, &format!("case[{i}].{k}"), v)),
                }
            }
            if case.name.is_empty() {
                return Err(format!("case #{}: a `name` is required", i + 1));
            }
            if case.run.is_empty() {
                return Err(format!("case {}: `run` is required", case.name));
            }
            if case.per == Some(0) || case.ops == Some(0) {
                return Err(format!(
                    "case {}: `per` and `ops` are at least 1",
                    case.name
                ));
            }
            let kinds = measures::kinds_of(&case.measure)
                .map_err(|e| format!("case {}: {e}", case.name))?;
            if kinds.contains(&Kind::DiskPerByte) && case.burned.is_none_or(|b| b == 0) {
                return Err(format!(
                    "case {}: `disk-per-byte` needs `burned`, the bytes one run burns",
                    case.name
                ));
            }
            if case.expect.after_stdout_file.is_some() && case.after.is_none() {
                return Err(format!(
                    "case {}: `expect.after_stdout` needs an `after` command",
                    case.name
                ));
            }
            cases.push(case);
        }
    }
    Ok(Manifest {
        bench,
        defaults,
        cases,
    })
}

fn parse_expect(v: &toml::Value, idx: usize) -> Result<Expect, String> {
    let t = v
        .as_table()
        .ok_or_else(|| format!("case #{idx}: `expect` is an inline table"))?;
    let mut out = Expect::default();
    for (k, v) in t {
        match k.as_str() {
            "exit" => {
                let n = v
                    .as_integer()
                    .ok_or_else(|| format!("case #{idx}: `expect.exit` is an integer"))?;
                out.exit = Some(n as i32);
            }
            "stdout" => {
                let s = v
                    .as_str()
                    .ok_or_else(|| format!("case #{idx}: `expect.stdout` is a string"))?;
                out.stdout_file = Some(PathBuf::from(s));
            }
            "after_stdout" => {
                let s = v
                    .as_str()
                    .ok_or_else(|| format!("case #{idx}: `expect.after_stdout` is a string"))?;
                out.after_stdout_file = Some(PathBuf::from(s));
            }
            _ => return Err(format!("case #{idx}: unknown key `expect.{k}`")),
        }
    }
    Ok(out)
}

fn as_argv(v: &toml::Value, where_: &str) -> Result<Vec<String>, String> {
    let arr = v
        .as_array()
        .ok_or_else(|| format!("`{where_}` is an array of strings"))?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        out.push(
            item.as_str()
                .ok_or_else(|| format!("`{where_}` holds strings"))?
                .to_string(),
        );
    }
    if out.is_empty() {
        return Err(format!("`{where_}` is empty"));
    }
    Ok(out)
}

/// One command (an array of strings) or a list of commands (an array of
/// arrays of strings), as a list.
fn as_commands(v: &toml::Value, where_: &str) -> Result<Vec<Vec<String>>, String> {
    let arr = v
        .as_array()
        .ok_or_else(|| format!("`{where_}` is an array of strings, or an array of those"))?;
    if arr.first().is_some_and(toml::Value::is_array) {
        arr.iter().map(|c| as_argv(c, where_)).collect()
    } else {
        Ok(vec![as_argv(v, where_)?])
    }
}

/// An array of strings, empty allowed (a list of globs).
fn as_strings(v: &toml::Value, where_: &str) -> Result<Vec<String>, String> {
    let arr = v
        .as_array()
        .ok_or_else(|| format!("`{where_}` is an array of strings"))?;
    arr.iter()
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("`{where_}` holds strings"))
        })
        .collect()
}

fn as_usize(v: &toml::Value, where_: &str) -> Result<usize, String> {
    let n = v
        .as_integer()
        .ok_or_else(|| format!("`{where_}` is an integer"))?;
    if n < 0 {
        return Err(format!("`{where_}` is not negative"));
    }
    Ok(n as usize)
}

fn as_env(v: &toml::Value, where_: &str) -> Result<BTreeMap<String, String>, String> {
    let t = v
        .as_table()
        .ok_or_else(|| format!("`{where_}` is an inline table of KEY = \"value\""))?;
    let mut out = BTreeMap::new();
    for (k, v) in t {
        let s = v
            .as_str()
            .ok_or_else(|| format!("`{where_}.{k}` is a string"))?;
        out.insert(k.clone(), s.to_string());
    }
    Ok(out)
}

/// Build the "unknown key `k` at line L" message. The key path comes
/// from the caller; the line is found by searching the TOML source for
/// `<leaf> =` or `[<leaf>]`, whichever shows up first.
fn unknown_key(text: &str, path: &str, _value: &toml::Value) -> String {
    let leaf = path.rsplit('.').next().unwrap_or(path);
    let line = find_line(text, leaf);
    match line {
        Some(l) => format!("unknown key `{path}` at line {l}"),
        None => format!("unknown key `{path}`"),
    }
}

fn find_line(text: &str, leaf: &str) -> Option<usize> {
    for (i, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or(raw).trim_start();
        let eats = |rest: &str| rest.trim_start().starts_with('=');
        if let Some(rest) = line.strip_prefix(leaf) {
            let next = rest.chars().next();
            if matches!(next, None | Some(' ' | '\t' | '=')) && eats(rest) {
                return Some(i + 1);
            }
        }
        // Table headers: `[leaf]` or `[parent.leaf]` or `[[case]]`.
        if line.starts_with('[') {
            let head = line.trim_matches(['[', ']'].as_ref());
            if head == leaf || head.ends_with(&format!(".{leaf}")) {
                return Some(i + 1);
            }
        }
    }
    None
}

// ----- the sampling loop -----------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stats {
    pub median_ms: f64,
    pub p10_ms: f64,
    pub p90_ms: f64,
    pub cv: f64,
    pub samples: usize,
    pub rss_bytes: Option<u64>,
    /// The per-sample timings in the order they were taken, for a baseline
    /// and for the rank test.
    pub raw_ms: Vec<f64>,
}

impl Stats {
    /// Build a summary from a slice of per-sample millisecond timings.
    /// Empty input returns the default.
    pub fn of(ms: &[f64], rss: Option<u64>) -> Stats {
        if ms.is_empty() {
            return Stats::default();
        }
        let mut v = ms.to_vec();
        v.sort_by(|a, b| a.total_cmp(b));
        let at = |q: f64| v[((v.len() - 1) as f64 * q).round() as usize];
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        let variance = v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / v.len() as f64;
        Stats {
            median_ms: at(0.5),
            p10_ms: at(0.1),
            p90_ms: at(0.9),
            cv: if mean > 0.0 {
                variance.sqrt() / mean
            } else {
                0.0
            },
            samples: v.len(),
            rss_bytes: rss,
            raw_ms: ms.to_vec(),
        }
    }

    pub fn noisy(&self) -> bool {
        self.cv > NOISY_CV
    }
}

const NOISY_CV: f64 = 0.05;

#[derive(Debug)]
enum Outcome {
    Timed(Stats),
    Wrong(String),
    Absent(String),
}

struct Row {
    name: String,
    group: Option<String>,
    budget: Option<String>,
    outcome: Outcome,
    /// The kernel measures the case asked for (`tin-growth`, ...): reported
    /// next to the timing, never gated.
    metrics: Vec<Metric>,
    /// What `--timings`, `--stats` and `--syscalls` found.
    insight: debug::Insight,
}

fn run_case(
    side: &Side,
    scratch_root: &Path,
    case: &Case,
    defaults: &Defaults,
    wants: debug::Wants,
) -> Row {
    let (outcome, metrics, insight) =
        measure_case_full(side, scratch_root, case, defaults, true, wants);
    Row {
        name: case.name.clone(),
        group: case.group.clone(),
        budget: case.budget.clone(),
        outcome,
        metrics,
        insight,
    }
}

/// A case made ready on one side: its work folder exists, its `setup` ran
/// and its output was checked once. Dropping it removes the work folder.
struct Prepared<'a> {
    side: &'a Side,
    case: &'a Case,
    defaults: &'a Defaults,
    work: PathBuf,
    expect_stdout: Option<String>,
    expect_after: Option<String>,
    /// The kernel measures this case asked for.
    kinds: Vec<Kind>,
    /// Per kind, one reading per sample taken so far.
    readings: RefCell<Vec<Vec<Result<f64, String>>>>,
}

impl Drop for Prepared<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.work);
    }
}

/// Setup and warm-up for one case on one side. `Err` is the outcome the
/// case ends with (WRONG or absent): a wrong answer gets no time.
fn prepare<'a>(
    side: &'a Side,
    scratch_root: &Path,
    case: &'a Case,
    defaults: &'a Defaults,
) -> Result<Prepared<'a>, Outcome> {
    // The working directory is the side's folder, so a case's
    // `run = ["cig", "run", "fixtures/hello.cig"]` resolves as
    // written. Isolation is through HOME, not cwd: a case that needs a
    // scratch place puts it under `$CIGSCRIPT_HOME` or an absolute path
    // under `$TMPDIR`.
    let safe = |s: &str| {
        s.replace(
            |c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_',
            "_",
        )
    };
    let work = scratch_root.join(format!(
        "case-{}-{}-{}",
        safe(&case.name),
        safe(&side.label),
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&work);
    if let Err(e) = std::fs::create_dir_all(&work) {
        return Err(Outcome::Absent(format!(
            "cannot make the case's work folder: {e}"
        )));
    }
    let mut p = Prepared {
        side,
        case,
        defaults,
        work,
        expect_stdout: None,
        expect_after: None,
        kinds: measures::kinds_of(&case.measure).unwrap_or_default(),
        readings: RefCell::new(Vec::new()),
    };
    p.readings = RefCell::new(vec![Vec::new(); p.kinds.len()]);
    // The case's expected stdout, read once. Missing is no check.
    if let Some(f) = &case.expect.stdout_file {
        let abs = if f.is_absolute() {
            f.clone()
        } else {
            side.dir.join(f)
        };
        match std::fs::read_to_string(&abs) {
            Ok(s) => p.expect_stdout = Some(normalise(&s)),
            Err(e) => {
                return Err(Outcome::Absent(format!(
                    "cannot read expected stdout {}: {e}",
                    abs.display()
                )))
            }
        }
    }
    if let Some(f) = &case.expect.after_stdout_file {
        let abs = if f.is_absolute() {
            f.clone()
        } else {
            side.dir.join(f)
        };
        match std::fs::read_to_string(&abs) {
            Ok(s) => p.expect_after = Some(normalise(&s)),
            Err(e) => {
                return Err(Outcome::Absent(format!(
                    "cannot read expected stdout {}: {e}",
                    abs.display()
                )))
            }
        }
    }
    let expect_exit = case.expect.exit;
    // Setup, with its own isolated home.
    if let Some(argv) = &case.setup {
        let setup_home = p.work.join("setup-home");
        let _ = std::fs::create_dir_all(&setup_home);
        let env = full_env(defaults, &case.env, &setup_home);
        if let Err(e) = run_untimed(
            &side.argv(argv),
            &side.dir,
            &env,
            &side.dir,
            case.stdin.as_deref(),
        ) {
            return Err(Outcome::Absent(format!("`setup` failed: {e}")));
        }
    }
    // Warm-up: at least one; `defaults.warmup` of them. The first one is
    // the output check; later ones only prime caches.
    let mut checked = false;
    let warmup = defaults.warmup.max(1);
    for i in 0..warmup {
        let home = p.work.join(format!("warmup-{i}-home"));
        let _ = std::fs::create_dir_all(&home);
        let env = full_env(defaults, &case.env, &home);
        if let Err(e) = run_befores(side, case, &env) {
            return Err(Outcome::Absent(format!("`before` failed in warm-up: {e}")));
        }
        match measure_run(
            &side.argv(&case.run),
            &side.dir,
            &env,
            &side.dir,
            case.stdin.as_deref(),
        ) {
            Ok(m) => {
                if !checked {
                    if let Some(code) = expect_exit {
                        if m.exit != Some(code) {
                            let tail = clip(m.stderr.trim());
                            let got = m
                                .exit
                                .map(|c| c.to_string())
                                .unwrap_or_else(|| "by signal".into());
                            return Err(Outcome::Wrong(if tail.is_empty() {
                                format!("exit {got} not {code}")
                            } else {
                                format!("exit {got} not {code} (stderr: {tail})")
                            }));
                        }
                    }
                    if let Some(want) = &p.expect_stdout {
                        let got = normalise(&m.stdout);
                        if &got != want {
                            return Err(Outcome::Wrong(format!(
                                "printed {:?}, not {:?}",
                                clip(&got),
                                clip(want)
                            )));
                        }
                    }
                    check_after(side, case, &env, p.expect_after.as_deref(), "warm-up")?;
                    checked = true;
                }
            }
            Err(e) => return Err(Outcome::Absent(format!("could not run in warm-up: {e}"))),
        }
        let _ = std::fs::remove_dir_all(&home);
    }
    Ok(p)
}

impl Prepared<'_> {
    /// One sample: `before` once, then `per` runs timed as one batch, the
    /// batch's time divided by `per`. Returns milliseconds and the peak
    /// resident set over the batch. Every run is checked again.
    fn sample(&self, i: usize) -> Result<(f64, Option<u64>), Outcome> {
        let (side, case, defaults) = (self.side, self.case, self.defaults);
        let home = self.work.join(format!("sample-{i}-home"));
        let _ = std::fs::create_dir_all(&home);
        let env = full_env(defaults, &case.env, &home);
        if let Err(e) = run_befores(side, case, &env) {
            return Err(Outcome::Absent(format!(
                "`before` failed before sample {i}: {e}"
            )));
        }
        let argv = side.argv(&case.run);
        let per = case.per.unwrap_or(defaults.per).max(1);
        // Sizes are read before the clock starts and after it stops.
        let probe = measures::before(&self.kinds, &env);
        let batch_started = Instant::now();
        let mut batch_rss: Option<u64> = None;
        for p in 0..per {
            match measure_run(&argv, &side.dir, &env, &side.dir, case.stdin.as_deref()) {
                Ok(m) => {
                    if let Some(code) = case.expect.exit {
                        if m.exit != Some(code) {
                            return Err(Outcome::Wrong(format!(
                                "sample {i} invocation {p} exited {} not {}",
                                m.exit
                                    .map(|c| c.to_string())
                                    .unwrap_or_else(|| "by signal".into()),
                                code
                            )));
                        }
                    }
                    if let Some(want) = &self.expect_stdout {
                        let got = normalise(&m.stdout);
                        if &got != want {
                            return Err(Outcome::Wrong(format!(
                                "sample {i} invocation {p} printed {:?}, not {:?}",
                                clip(&got),
                                clip(want)
                            )));
                        }
                    }
                    batch_rss = max_opt(batch_rss, m.rss_bytes);
                }
                Err(e) => return Err(Outcome::Absent(format!("sample {i} invocation {p}: {e}"))),
            }
        }
        let batch_ms = batch_started.elapsed().as_secs_f64() * 1000.0;
        if !self.kinds.is_empty() {
            let got = measures::after(&self.kinds, &probe, &env, &side.dir, per, case.burned);
            let mut all = self.readings.borrow_mut();
            for (col, r) in all.iter_mut().zip(got) {
                col.push(r);
            }
        }
        check_after(
            side,
            case,
            &env,
            self.expect_after.as_deref(),
            &format!("sample {i}"),
        )?;
        let _ = std::fs::remove_dir_all(&home);
        Ok((batch_ms / per as f64, batch_rss))
    }
}

fn measure_case(side: &Side, scratch_root: &Path, case: &Case, defaults: &Defaults) -> Outcome {
    measure_case_full(
        side,
        scratch_root,
        case,
        defaults,
        false,
        debug::Wants::default(),
    )
    .0
}

/// A case measured: its timing and, when `metrics` says so, the kernel
/// measures it asked for (a comparison and a watch compare timings only, and
/// skip the extra run that counts syncs).
fn measure_case_full(
    side: &Side,
    scratch_root: &Path,
    case: &Case,
    defaults: &Defaults,
    metrics: bool,
    wants: debug::Wants,
) -> (Outcome, Vec<Metric>, debug::Insight) {
    let none = debug::Insight::default;
    let prepared = match prepare(side, scratch_root, case, defaults) {
        Ok(p) => p,
        Err(o) => return (o, Vec::new(), none()),
    };
    let mut ms: Vec<f64> = Vec::with_capacity(defaults.samples);
    let mut rss_max: Option<u64> = None;
    for i in 0..defaults.samples {
        match prepared.sample(i) {
            Ok((t, rss)) => {
                ms.push(t);
                rss_max = max_opt(rss_max, rss);
            }
            Err(o) => return (o, Vec::new(), none()),
        }
    }
    let kernel = if metrics {
        prepared.metrics()
    } else {
        Vec::new()
    };
    let stats = Stats::of(&ms, rss_max);
    // The debugging flags: extra runs, never timed, after the samples.
    let insight = if metrics && wants.any() {
        prepared.insight(wants, stats.median_ms)
    } else {
        none()
    };
    (Outcome::Timed(stats), kernel, insight)
}

/// Run each `before` command of a case, in order.
fn run_befores(side: &Side, case: &Case, env: &BTreeMap<OsString, OsString>) -> Result<(), String> {
    for argv in &case.before {
        run_untimed(
            &side.argv(argv),
            &side.dir,
            env,
            &side.dir,
            case.stdin.as_deref(),
        )?;
    }
    Ok(())
}

/// Run a case's `after` command and check its stdout against the expected
/// file: this is how a case that prints nothing deterministic itself (an
/// unburn names its home and its run) proves what it left behind.
fn check_after(
    side: &Side,
    case: &Case,
    env: &BTreeMap<OsString, OsString>,
    want: Option<&str>,
    when: &str,
) -> Result<(), Outcome> {
    let Some(argv) = &case.after else {
        return Ok(());
    };
    match measure_run(&side.argv(argv), &side.dir, env, &side.dir, None) {
        Ok(m) => {
            if m.exit != Some(0) {
                return Err(Outcome::Wrong(format!(
                    "`after` in {when} exited {} (stderr: {})",
                    m.exit
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "by signal".into()),
                    clip(m.stderr.trim())
                )));
            }
            if let Some(want) = want {
                let got = normalise(&m.stdout);
                if got != want {
                    return Err(Outcome::Wrong(format!(
                        "`after` in {when} printed {:?}, not {:?}",
                        clip(&got),
                        clip(want)
                    )));
                }
            }
            Ok(())
        }
        Err(e) => Err(Outcome::Absent(format!(
            "`after` in {when} could not run: {e}"
        ))),
    }
}

impl Prepared<'_> {
    /// The kernel measures of the samples taken so far, plus the sync
    /// count, which takes a run of its own.
    fn metrics(&self) -> Vec<Metric> {
        let cols = self.readings.borrow();
        let mut out = Vec::new();
        for (kind, col) in self.kinds.iter().zip(cols.iter()) {
            if *kind == Kind::FsyncsPerOp {
                out.push(measures::summarize(kind, &[self.fsyncs_per_op()]));
            } else {
                out.push(measures::summarize(kind, col));
            }
        }
        out
    }

    /// Sync calls in one run, per operation: counted by `strace` from the
    /// kernel's own syscall events in one extra run that is never timed.
    fn fsyncs_per_op(&self) -> Result<f64, String> {
        let (summary, _) =
            self.strace_summary("fsync-home", Some(&measures::SYNC_CALLS.join(",")))?;
        Ok(measures::count_syncs(&summary) as f64 / self.case.ops.unwrap_or(1).max(1) as f64)
    }
}

fn max_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

fn clip(s: &str) -> String {
    let mut t: String = s.chars().take(60).collect();
    if s.chars().count() > 60 {
        t.push('…');
    }
    t
}

fn normalise(s: &str) -> String {
    s.replace("\r\n", "\n")
}

/// What a process needs from the caller's environment to start at all,
/// whatever the manifest says: the temp folder everywhere, and on Windows
/// the system's own folders (a child without `SystemRoot` can fail to
/// load its libraries) and how commands are found.
const PASSED_THROUGH: &[&str] = &[
    "TMPDIR",
    "TEMP",
    "TMP",
    "SystemRoot",
    "SYSTEMROOT",
    "windir",
    "WINDIR",
    "PATHEXT",
    "COMSPEC",
];

/// The homes `home = "shared"` keeps from the caller.
const HOME_VARS: &[&str] = &["HOME", "USERPROFILE", "CIGSCRIPT_HOME", "CIGO_HOME"];

/// The whole environment a sampled process gets.
fn full_env(
    defaults: &Defaults,
    case_env: &BTreeMap<String, String>,
    home: &Path,
) -> BTreeMap<OsString, OsString> {
    let mut out: BTreeMap<OsString, OsString> = BTreeMap::new();
    for k in PASSED_THROUGH {
        if let Some(v) = std::env::var_os(k) {
            out.insert(OsString::from(k), v);
        }
    }
    // Start from a deliberately spare set: the manifest's own defaults
    // then any per-case overrides win.
    for (k, v) in &defaults.env {
        out.insert(OsString::from(k), OsString::from(v));
    }
    for (k, v) in case_env {
        out.insert(OsString::from(k), OsString::from(v));
    }
    if defaults.home == HomeMode::Shared {
        // The caller's own homes, as they are: the case measures the state
        // it will really run against.
        for k in HOME_VARS {
            if let Some(v) = std::env::var_os(k) {
                out.insert(OsString::from(k), v);
            }
        }
        return out;
    }
    // The isolated home, every time. A case's env cannot override it.
    out.insert(OsString::from("HOME"), home.as_os_str().to_os_string());
    out.insert(
        OsString::from("USERPROFILE"),
        home.as_os_str().to_os_string(),
    );
    out.insert(
        OsString::from("CIGSCRIPT_HOME"),
        home.as_os_str().to_os_string(),
    );
    out.insert(OsString::from("CIGO_HOME"), home.as_os_str().to_os_string());
    out
}

#[derive(Default)]
struct Measured {
    stdout: String,
    stderr: String,
    exit: Option<i32>,
    rss_bytes: Option<u64>,
}

/// Run a command, wait for it, and report its stdout, exit and (on Unix)
/// peak resident set.
fn measure_run(
    argv: &[String],
    cwd: &Path,
    env: &BTreeMap<OsString, OsString>,
    manifest_dir: &Path,
    stdin_file: Option<&str>,
) -> Result<Measured, String> {
    let mut c = build_command(argv, cwd, env, manifest_dir, stdin_file)?;
    c.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("could not start: {e}"))?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let err_handle = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut stderr, &mut s);
        s
    });
    let mut out = String::new();
    let _ = std::io::Read::read_to_string(&mut stdout, &mut out);
    let (exit, rss_bytes) = wait(&mut child);
    let err = err_handle.join().unwrap_or_default();
    Ok(Measured {
        stdout: out,
        stderr: err,
        exit,
        rss_bytes,
    })
}

/// Run a command (setup/before), waiting for it. Its stdout and stderr
/// are captured and reported with the error on a non-zero exit.
fn run_untimed(
    argv: &[String],
    cwd: &Path,
    env: &BTreeMap<OsString, OsString>,
    manifest_dir: &Path,
    stdin_file: Option<&str>,
) -> Result<(), String> {
    let mut c = build_command(argv, cwd, env, manifest_dir, stdin_file)?;
    let out = c
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("could not start: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "exit {}: {}",
            out.status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "by signal".into()),
            stderr.trim()
        ));
    }
    Ok(())
}

fn build_command(
    argv: &[String],
    cwd: &Path,
    env: &BTreeMap<OsString, OsString>,
    manifest_dir: &Path,
    stdin_file: Option<&str>,
) -> Result<Command, String> {
    let exe = &argv[0];
    let mut c = Command::new(exe);
    c.args(&argv[1..]);
    c.current_dir(cwd);
    c.env_clear();
    for (k, v) in env {
        c.env(k, v);
    }
    // Pass through PATH so the binary under test is findable, unless the
    // environment names one (the family runner puts a swapped product's
    // folder first).
    if !env.contains_key(&OsString::from("PATH")) {
        if let Some(path) = std::env::var_os("PATH") {
            c.env("PATH", path);
        }
    }
    match stdin_file {
        Some(p) => {
            let abs = if Path::new(p).is_absolute() {
                PathBuf::from(p)
            } else {
                manifest_dir.join(p)
            };
            let f = std::fs::File::open(&abs)
                .map_err(|e| format!("cannot open stdin {}: {e}", abs.display()))?;
            c.stdin(Stdio::from(f));
        }
        None => {
            c.stdin(Stdio::null());
        }
    }
    Ok(c)
}

/// Wait for a child, pulling peak resident set from `wait4` on Unix.
#[cfg(unix)]
fn wait(child: &mut std::process::Child) -> (Option<i32>, Option<u64>) {
    let pid = child.id() as libc::pid_t;
    let mut status = 0;
    // SAFETY: a zeroed rusage is valid, wait4 writes only into the two
    // out-pointers, the child is ours and not yet reaped.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let got = unsafe { libc::wait4(pid, &mut status, 0, &mut usage) };
    if got != pid {
        return match child.wait() {
            Ok(s) => (s.code(), None),
            Err(_) => (None, None),
        };
    }
    // ru_maxrss is bytes on macOS and kibibytes on every other Unix.
    let unit: u64 = if cfg!(target_os = "macos") { 1 } else { 1024 };
    let rss = u64::try_from(usage.ru_maxrss).ok().map(|k| k * unit);
    let code = if libc::WIFEXITED(status) {
        Some(libc::WEXITSTATUS(status))
    } else {
        None
    };
    (code, rss)
}

#[cfg(not(unix))]
fn wait(child: &mut std::process::Child) -> (Option<i32>, Option<u64>) {
    match child.wait() {
        Ok(s) => (s.code(), None),
        Err(_) => (None, None),
    }
}

// ----- machine conditions ----------------------------------------------------

struct Conditions {
    cpu: String,
    cores: usize,
    os: String,
    kernel: String,
    governor: Option<String>,
}

impl Conditions {
    fn here() -> Conditions {
        let read = |p: &str| {
            std::fs::read_to_string(p)
                .ok()
                .map(|s| s.trim().to_string())
        };
        let cpu = read("/proc/cpuinfo")
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("model name") || l.starts_with("Model"))
                    .and_then(|l| l.split_once(':').map(|(_, v)| v.trim().to_string()))
            })
            .or_else(|| {
                Command::new("sysctl")
                    .args(["-n", "machdep.cpu.brand_string"])
                    .output()
                    .ok()
                    .filter(|o| o.status.success())
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            })
            .unwrap_or_else(|| std::env::consts::ARCH.to_string());
        let kernel = read("/proc/sys/kernel/osrelease")
            .or_else(|| {
                Command::new("uname")
                    .arg("-r")
                    .output()
                    .ok()
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            })
            .unwrap_or_default();
        Conditions {
            cpu,
            cores: std::thread::available_parallelism().map_or(1, |n| n.get()),
            os: std::env::consts::OS.to_string(),
            kernel,
            governor: read("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor"),
        }
    }

    fn line(&self) -> String {
        let governor = self
            .governor
            .as_ref()
            .map(|g| format!(", governor {g}"))
            .unwrap_or_default();
        let kernel = if self.kernel.is_empty() {
            String::new()
        } else {
            format!(" {}", self.kernel)
        };
        format!(
            "{}, {} cores, {}{kernel}{governor}",
            self.cpu, self.cores, self.os
        )
    }
}

// ----- scratch root ----------------------------------------------------------

/// A fresh folder under the temp directory for this run's homes: a name
/// nobody can guess, made by this call (never one that was already
/// there), and private to this user. The homes under it hold each
/// sample's tin and keys.
fn scratch_root() -> std::io::Result<PathBuf> {
    let base = std::env::temp_dir();
    for _ in 0..16 {
        let mut bytes = [0u8; 8];
        getrandom::getrandom(&mut bytes).map_err(|e| std::io::Error::other(e.to_string()))?;
        let tag: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let dir = base.join(format!("cig-bench-{}-{tag}", std::process::id()));
        let mut b = std::fs::DirBuilder::new();
        // Exactly this folder: never one made on the way to it.
        b.recursive(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700);
        }
        match b.create(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("sixteen names in a row were taken"))
}

// ----- rendering -------------------------------------------------------------

fn cell(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Timed(s) if s.noisy() => format!("{:.1}~", s.median_ms),
        Outcome::Timed(s) => format!("{:.1}", s.median_ms),
        Outcome::Wrong(_) => "WRONG".into(),
        Outcome::Absent(_) => "-".into(),
    }
}

fn spread(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Timed(s) => format!("{:.1} to {:.1}", s.p10_ms, s.p90_ms),
        _ => "-".into(),
    }
}

fn cv_cell(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Timed(s) => format!("{:.1}%", s.cv * 100.0),
        _ => "-".into(),
    }
}

fn rss_cell(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Timed(Stats {
            rss_bytes: Some(b), ..
        }) => format!("{:.1}", *b as f64 / 1e6),
        _ => "-".into(),
    }
}

fn print_table(m: &Manifest, rows: &[Row], c: &Conditions) {
    outln!(
        "cig bench: {}, cig {}",
        m.bench.product,
        env!("CARGO_PKG_VERSION")
    );
    outln!("{}", c.line());
    outln!("");
    outln!(
        "{:<30} {:<10} {:>10} {:>14} {:>7} {:>8}",
        "case",
        "group",
        "median ms",
        "p10 to p90 ms",
        "CV",
        "RSS MB"
    );
    for r in rows {
        outln!(
            "{:<30} {:<10} {:>10} {:>14} {:>7} {:>8}",
            r.name,
            r.group.as_deref().unwrap_or("-"),
            cell(&r.outcome),
            spread(&r.outcome),
            cv_cell(&r.outcome),
            rss_cell(&r.outcome)
        );
    }
    let with: Vec<&Row> = rows
        .iter()
        .filter(|r| matches!(r.outcome, Outcome::Timed(_)) && !r.metrics.is_empty())
        .collect();
    if !with.is_empty() {
        outln!("");
        outln!("{:<30} {:<24} {:>14}", "case", "measure", "value");
        for r in with {
            for m in &r.metrics {
                outln!("{:<30} {:<24} {:>14}", r.name, m.name, m.cell());
                if let Some(n) = &m.note {
                    outln!("{:<30}   {n}", "");
                }
            }
        }
        outln!("");
        outln!("Measures are reported, never gated.");
    }
    for r in rows {
        let median = match &r.outcome {
            Outcome::Timed(s) => Some(s.median_ms),
            _ => None,
        };
        debug::print_text(&r.name, median, &r.insight);
    }
}

fn print_markdown(m: &Manifest, rows: &[Row], c: &Conditions) {
    outln!(
        "**cig bench**: {}, cig {} — {}",
        m.bench.product,
        env!("CARGO_PKG_VERSION"),
        c.line()
    );
    outln!("");
    outln!("| case | group | median ms | p10 to p90 ms | CV | RSS MB |");
    outln!("|---|---|---:|---|---:|---:|");
    for r in rows {
        outln!(
            "| `{}` | {} | {} | {} | {} | {} |",
            r.name,
            r.group.as_deref().unwrap_or("-"),
            cell(&r.outcome),
            spread(&r.outcome),
            cv_cell(&r.outcome),
            rss_cell(&r.outcome)
        );
    }
    let mut first = true;
    for r in rows {
        if !matches!(r.outcome, Outcome::Timed(_)) {
            continue;
        }
        for m in &r.metrics {
            if first {
                outln!("");
                outln!("| case | measure | value |");
                outln!("|---|---|---:|");
                first = false;
            }
            outln!("| `{}` | `{}` | {} |", r.name, m.name, m.cell());
        }
    }
    for r in rows {
        debug::print_markdown(&r.name, &r.insight);
    }
}

/// One kernel measure as JSON.
fn metric_json(m: &Metric) -> serde_json::Value {
    serde_json::json!({
        "name": m.name,
        "unit": m.unit,
        "value": m.value,
        "note": m.note,
    })
}

/// One side's timings as JSON.
fn stats_json(s: &Stats) -> serde_json::Value {
    serde_json::json!({
        "median_ms": s.median_ms,
        "p10_ms": s.p10_ms,
        "p90_ms": s.p90_ms,
        "cv": s.cv,
        "noisy": s.noisy(),
        "samples": s.samples,
        "rss_bytes": s.rss_bytes,
    })
}

fn cases_json(rows: &[Row]) -> Vec<serde_json::Value> {
    rows.iter()
        .map(|r| {
            let outcome = match &r.outcome {
                Outcome::Timed(s) => serde_json::json!({
                    "state": "timed",
                    "median_ms": s.median_ms,
                    "p10_ms": s.p10_ms,
                    "p90_ms": s.p90_ms,
                    "cv": s.cv,
                    "noisy": s.noisy(),
                    "samples": s.samples,
                    "rss_bytes": s.rss_bytes,
                    "metrics": r.metrics.iter().map(metric_json).collect::<Vec<_>>(),
                }),
                Outcome::Wrong(why) => serde_json::json!({ "state": "wrong", "why": why }),
                Outcome::Absent(why) => serde_json::json!({ "state": "absent", "why": why }),
            };
            let mut case = serde_json::json!({
                "name": r.name,
                "group": r.group,
                "budget": r.budget,
                "outcome": outcome,
            });
            if !r.insight.is_empty() {
                case["debug"] = debug::to_json(&r.insight);
            }
            case
        })
        .collect()
}

fn conditions_json(c: &Conditions) -> serde_json::Value {
    serde_json::json!({
        "cpu": c.cpu,
        "cores": c.cores,
        "os": c.os,
        "kernel": c.kernel,
        "governor": c.governor,
    })
}

fn print_json(m: &Manifest, rows: &[Row], c: &Conditions) {
    let doc = serde_json::json!({
        "format": FORMAT,
        "product": m.bench.product,
        "cig_version": env!("CARGO_PKG_VERSION"),
        "conditions": conditions_json(c),
        "cases": cases_json(rows),
    });
    outln!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
}

// ----- tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_median_spread_and_cv() {
        let flat = Stats::of(&[10.0, 10.0, 10.1, 9.9, 10.0], None);
        assert_eq!(flat.median_ms, 10.0);
        assert!(!flat.noisy(), "{flat:?}");
        let noisy = Stats::of(&[10.0, 14.0, 7.0, 12.0, 9.0], Some(5));
        assert!(noisy.noisy(), "a CV over 5% is noisy: {noisy:?}");
        assert_eq!(
            (noisy.p10_ms, noisy.median_ms, noisy.p90_ms),
            (7.0, 10.0, 14.0)
        );
        assert_eq!(noisy.rss_bytes, Some(5));
        assert_eq!(Stats::of(&[], None), Stats::default());
    }

    #[test]
    fn manifest_minimum_parses() {
        let text = r#"
            [bench]
            product = "cig"
            [[case]]
            name = "a"
            run = ["echo", "hi"]
        "#;
        let m = parse_manifest(text).unwrap();
        assert_eq!(m.bench.product, "cig");
        assert_eq!(m.cases.len(), 1);
        assert_eq!(m.cases[0].name, "a");
        assert_eq!(m.cases[0].run, vec!["echo", "hi"]);
        // Default expect: exit 0, no stdout check.
        assert_eq!(m.cases[0].expect.exit, Some(0));
        assert!(m.cases[0].expect.stdout_file.is_none());
    }

    #[test]
    fn manifest_unknown_key_names_it_and_the_line() {
        let text = "[bench]\nproduct = \"cig\"\nflarp = 1\n";
        let err = parse_manifest(text).unwrap_err();
        assert!(err.contains("flarp"), "{err}");
        assert!(err.contains("line 3"), "{err}");
    }

    #[test]
    fn manifest_unknown_top_level_names_it() {
        let text = "[bench]\nproduct = \"cig\"\n[wibble]\nfoo = 1\n";
        let err = parse_manifest(text).unwrap_err();
        assert!(err.contains("wibble"), "{err}");
    }

    #[test]
    fn manifest_reserved_keys_are_ignored() {
        let text = r#"
            [bench]
            product = "cig"
            build = ["cig", "check"]
            watch = ["src/**"]
            [[case]]
            name = "a"
            run = ["true"]
            gate = "+10%"
            score = "fixtures/score.cig"
            config = { hop_statcache = "on" }
        "#;
        parse_manifest(text).expect("reserved keys are accepted");
    }

    #[test]
    fn manifest_expect_parses_both_shapes() {
        let text = r#"
            [bench]
            product = "cig"
            [[case]]
            name = "a"
            run = ["true"]
            expect = { exit = 0, stdout = "out.txt" }
        "#;
        let m = parse_manifest(text).unwrap();
        assert_eq!(m.cases[0].expect.exit, Some(0));
        assert_eq!(
            m.cases[0].expect.stdout_file,
            Some(PathBuf::from("out.txt"))
        );
    }

    #[test]
    fn manifest_schema_is_versioned() {
        let head = "[bench]\nproduct = \"cig\"\n";
        // No `format` is format 1; a 1 is accepted.
        parse_manifest(head).unwrap();
        parse_manifest(&format!("format = 1\n{head}")).unwrap();
        // Another format, or a format that is not a number, is refused.
        let err = parse_manifest(&format!("format = 2\n{head}")).unwrap_err();
        assert!(
            err.contains("format 2") && err.contains("reads format 1"),
            "{err}"
        );
        let err = parse_manifest(&format!("format = \"1\"\n{head}")).unwrap_err();
        assert!(err.contains("format"), "{err}");
    }

    #[test]
    fn build_binary_and_watch_keys_parse() {
        let text = r#"
            [bench]
            product = "cig"
            build = ["cargo", "build"]
            binary = "target/debug/cig"
            watch = ["src/**"]
            [[case]]
            name = "a"
            run = ["true"]
            watch = ["src/lib.rs"]
        "#;
        let m = parse_manifest(text).unwrap();
        assert_eq!(m.bench.build.as_deref().unwrap()[0], "cargo");
        assert_eq!(m.bench.binary.as_deref(), Some("target/debug/cig"));
        assert_eq!(m.bench.watch, vec!["src/**"]);
        assert_eq!(m.cases[0].watch, vec!["src/lib.rs"]);
    }
}
