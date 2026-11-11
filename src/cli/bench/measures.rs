// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The kernel measures of `cig bench` (#179 step 5, #145): what a case
//! leaves on disk and what it asks the disk to do, next to how long it took.
//!
//! - `tin-growth`: bytes added under the run's `$CIGSCRIPT_HOME/tin`, per run;
//! - `file:<path>`: the size of a file after the run (`$NAME` and `${NAME}`
//!   in the path are read from the sampled command's environment);
//! - `disk-per-byte`: bytes added on disk under the home per byte the case
//!   burns (the case says how many in `burned`);
//! - `fsyncs-per-op`: fsync, fdatasync, sync_file_range and syncfs calls in
//!   one run, per operation (the case's `ops`), counted from the kernel's
//!   own syscall events (`strace`, one extra untimed run); where that cannot
//!   be done the number is marked not measured and never guessed.
//!
//! They are reported, never gated: #153 decided that timings are reported,
//! and a size is the same kind of fact. Every probe reads sizes before the
//! timed batch starts or after its clock has stopped, so none of this is in
//! the time.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// One kernel measure a case asked for.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Kind {
    TinGrowth,
    File(String),
    DiskPerByte,
    FsyncsPerOp,
}

impl Kind {
    /// The name as the manifest and the reports spell it.
    pub(super) fn name(&self) -> String {
        match self {
            Kind::TinGrowth => "tin-growth".into(),
            Kind::File(p) => format!("file:{p}"),
            Kind::DiskPerByte => "disk-per-byte".into(),
            Kind::FsyncsPerOp => "fsyncs-per-op".into(),
        }
    }

    pub(super) fn unit(&self) -> &'static str {
        match self {
            Kind::TinGrowth | Kind::File(_) => "bytes",
            Kind::DiskPerByte => "bytes per byte",
            Kind::FsyncsPerOp => "fsyncs per op",
        }
    }
}

/// Which of a case's `measure` entries are kernel measures. `wall` and
/// `rss` are always taken, and the entries reserved for later steps are
/// accepted and ignored, so they are not here. `Err` is a malformed one.
pub(super) fn kinds_of(measure: &[String]) -> Result<Vec<Kind>, String> {
    let mut out: Vec<Kind> = Vec::new();
    for m in measure {
        let kind = match m.as_str() {
            "tin-growth" => Kind::TinGrowth,
            "disk-per-byte" => Kind::DiskPerByte,
            "fsyncs-per-op" => Kind::FsyncsPerOp,
            s if s.starts_with("file:") => {
                let p = &s["file:".len()..];
                if p.is_empty() {
                    return Err(
                        "`file:` needs a path (`file:$CIGSCRIPT_HOME/runs-index.jsonl`)".into(),
                    );
                }
                Kind::File(p.to_string())
            }
            _ => continue,
        };
        if !out.contains(&kind) {
            out.push(kind);
        }
    }
    Ok(out)
}

/// One reported number for a case: the median over its samples, or why
/// there is none.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Metric {
    pub name: String,
    pub unit: String,
    pub value: Option<f64>,
    /// Why `value` is empty ("not measured: ..."), or a plain remark.
    pub note: Option<String>,
}

impl Metric {
    /// The value as a cell: bytes humanized, a ratio with an `x`.
    pub fn cell(&self) -> String {
        match self.value {
            None => "not measured".into(),
            Some(v) => match self.unit.as_str() {
                "bytes" => human_bytes(v),
                "bytes per byte" => format!("{v:.2}x"),
                _ => format!("{v:.2}"),
            },
        }
    }
}

pub(super) fn human_bytes(v: f64) -> String {
    let neg = v < 0.0;
    let a = v.abs();
    let s = if a < 1024.0 {
        format!("{a:.0} B")
    } else if a < 1024.0 * 1024.0 {
        format!("{:.1} KiB", a / 1024.0)
    } else if a < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} MiB", a / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GiB", a / (1024.0 * 1024.0 * 1024.0))
    };
    if neg {
        format!("-{s}")
    } else {
        s
    }
}

/// What was on disk before a sample's timed batch.
pub(super) struct Before {
    tin: Option<u64>,
    disk: Option<u64>,
}

/// The home a sampled command writes its tin and runs to, from its own
/// environment: `home = "shared"` with no `CIGSCRIPT_HOME` set has none to
/// look at.
fn home_of(env: &BTreeMap<OsString, OsString>) -> Option<PathBuf> {
    env.get(&OsString::from("CIGSCRIPT_HOME"))
        .map(PathBuf::from)
}

/// Size the home before the batch.
pub(super) fn before(kinds: &[Kind], env: &BTreeMap<OsString, OsString>) -> Before {
    let home = home_of(env);
    let want_tin = kinds.contains(&Kind::TinGrowth);
    let want_disk = kinds.contains(&Kind::DiskPerByte);
    Before {
        tin: home
            .as_ref()
            .filter(|_| want_tin)
            .map(|h| allocated(&h.join("tin"))),
        disk: home.as_ref().filter(|_| want_disk).map(|h| allocated(h)),
    }
}

/// One sample's readings, one per kind, in the order of `kinds`. `None` is
/// a number that could not be read here (with the reason).
pub(super) fn after(
    kinds: &[Kind],
    b: &Before,
    env: &BTreeMap<OsString, OsString>,
    cwd: &Path,
    per: usize,
    burned: Option<u64>,
) -> Vec<Result<f64, String>> {
    let home = home_of(env);
    let per = per.max(1) as f64;
    kinds
        .iter()
        .map(|k| match k {
            Kind::TinGrowth => match (&home, b.tin) {
                (Some(h), Some(was)) => {
                    let now = allocated(&h.join("tin"));
                    Ok((now as f64 - was as f64) / per)
                }
                _ => Err("no CIGSCRIPT_HOME to look in".into()),
            },
            Kind::DiskPerByte => match (&home, b.disk, burned) {
                (Some(h), Some(was), Some(bytes)) if bytes > 0 => {
                    let now = allocated(h);
                    Ok((now as f64 - was as f64) / per / bytes as f64)
                }
                (_, _, None | Some(0)) => {
                    Err("the case does not say how many bytes it burns".into())
                }
                _ => Err("no CIGSCRIPT_HOME to look in".into()),
            },
            Kind::File(p) => {
                let path = expand(p, env);
                let abs = if Path::new(&path).is_absolute() {
                    PathBuf::from(&path)
                } else {
                    cwd.join(&path)
                };
                std::fs::metadata(&abs)
                    .map(|m| m.len() as f64)
                    .map_err(|e| format!("{}: {e}", abs.display()))
            }
            // Counted in a run of its own, after the samples.
            Kind::FsyncsPerOp => Err(String::new()),
        })
        .collect()
}

/// Median of the readings that exist, with the first reason when none do.
pub(super) fn summarize(kind: &Kind, readings: &[Result<f64, String>]) -> Metric {
    let mut ok: Vec<f64> = readings
        .iter()
        .filter_map(|r| r.as_ref().ok().copied())
        .collect();
    let value = if ok.is_empty() {
        None
    } else {
        ok.sort_by(|a, b| a.total_cmp(b));
        Some(ok[(ok.len() - 1) / 2])
    };
    let note = if value.is_none() {
        readings
            .iter()
            .find_map(|r| r.as_ref().err().filter(|e| !e.is_empty()).cloned())
            .map(|e| format!("not measured: {e}"))
    } else {
        None
    };
    Metric {
        name: kind.name(),
        unit: kind.unit().to_string(),
        value,
        note,
    }
}

/// `$NAME` and `${NAME}` read from the sampled command's environment; a
/// name it does not have stays as written.
pub(super) fn expand(s: &str, env: &BTreeMap<OsString, OsString>) -> String {
    let mut out = String::new();
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < b.len() {
        if b[i] == '$' {
            let (name, next) = if b.get(i + 1) == Some(&'{') {
                match b[i + 2..].iter().position(|c| *c == '}') {
                    Some(end) => (
                        b[i + 2..i + 2 + end].iter().collect::<String>(),
                        i + 3 + end,
                    ),
                    None => (String::new(), i + 1),
                }
            } else {
                let n: String = b[i + 1..]
                    .iter()
                    .take_while(|c| c.is_ascii_alphanumeric() || **c == '_')
                    .collect();
                let len = n.chars().count();
                (n, i + 1 + len)
            };
            if !name.is_empty() {
                if let Some(v) = env.get(&OsString::from(&name)) {
                    out.push_str(&v.to_string_lossy());
                    i = next;
                    continue;
                }
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// Bytes allocated under a path (blocks, so a sparse file counts what it
/// holds), not following links. A path that is not there is 0.
pub(super) fn allocated(p: &Path) -> u64 {
    let Ok(md) = std::fs::symlink_metadata(p) else {
        return 0;
    };
    let mut total = block_bytes(&md);
    if md.is_dir() {
        if let Ok(rd) = std::fs::read_dir(p) {
            for e in rd.flatten() {
                total += allocated(&e.path());
            }
        }
    }
    total
}

#[cfg(unix)]
fn block_bytes(md: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    md.blocks() * 512
}

#[cfg(not(unix))]
fn block_bytes(md: &std::fs::Metadata) -> u64 {
    md.len()
}

/// The syscalls that ask the disk to make data durable.
pub(super) const SYNC_CALLS: &[&str] = &["fsync", "fdatasync", "sync_file_range", "syncfs"];

/// Add up the sync calls in a `strace -c` summary.
pub(super) fn count_syncs(summary: &str) -> u64 {
    let mut total = 0;
    for line in summary.lines() {
        let t: Vec<&str> = line.split_whitespace().collect();
        // % time, seconds, usecs/call, calls, [errors], syscall
        if t.len() >= 5 && SYNC_CALLS.contains(&t[t.len() - 1]) {
            if let Ok(n) = t[3].parse::<u64>() {
                total += n;
            }
        }
    }
    total
}

/// The command that counts one run's syscalls with `strace -c`, or why it
/// cannot. `filter` is a `-e trace=` list; none traces every syscall.
pub(super) fn strace_argv(
    out: &Path,
    argv: &[String],
    filter: Option<&str>,
) -> Result<Vec<String>, String> {
    if !cfg!(target_os = "linux") {
        return Err("only counted on Linux".into());
    }
    let mut v: Vec<String> = vec!["strace".into(), "-f".into(), "-qq".into(), "-c".into()];
    if let Some(f) = filter {
        v.push("-e".into());
        v.push(format!("trace={f}"));
    }
    v.push("-o".into());
    v.push(out.to_string_lossy().into_owned());
    v.extend(argv.iter().cloned());
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<OsString, OsString> {
        pairs
            .iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
            .collect()
    }

    #[test]
    fn kinds_pick_the_kernel_measures_and_ignore_the_rest() {
        let m: Vec<String> = [
            "wall",
            "rss",
            "cpu",
            "tin-growth",
            "file:$HOME/x",
            "tin-growth",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            kinds_of(&m).unwrap(),
            vec![Kind::TinGrowth, Kind::File("$HOME/x".into())]
        );
        assert!(kinds_of(&["file:".to_string()]).is_err());
    }

    #[test]
    fn expand_reads_the_environment() {
        let e = env(&[("A", "/a"), ("B", "b")]);
        assert_eq!(expand("$A/x", &e), "/a/x");
        assert_eq!(expand("${B}c/$A", &e), "bc//a");
        assert_eq!(expand("$NOPE/x", &e), "$NOPE/x");
        assert_eq!(expand("cost $", &e), "cost $");
    }

    #[test]
    fn strace_summaries_are_added_up() {
        let s = "% time     seconds  usecs/call     calls    errors syscall\n\
                 ------ ----------- ----------- --------- --------- ----------------\n\
                 \x20 90.00    0.000900          30        30           fsync\n\
                 \x20  9.00    0.000090          10         9         1 fdatasync\n\
                 ------ ----------- ----------- --------- --------- ----------------\n\
                 100.00    0.001000                    39         1 total\n";
        assert_eq!(count_syncs(s), 39);
        assert_eq!(count_syncs(""), 0);
    }

    #[test]
    fn the_median_skips_what_could_not_be_read() {
        let k = Kind::TinGrowth;
        let m = summarize(&k, &[Ok(10.0), Err("x".into()), Ok(30.0), Ok(20.0)]);
        assert_eq!(m.value, Some(20.0));
        let m = summarize(&k, &[Err("no home".into())]);
        assert_eq!(m.value, None);
        assert_eq!(m.note.as_deref(), Some("not measured: no home"));
        assert_eq!(m.cell(), "not measured");
    }

    #[test]
    fn bytes_are_humanized() {
        assert_eq!(human_bytes(12.0), "12 B");
        assert_eq!(human_bytes(2048.0), "2.0 KiB");
        assert_eq!(human_bytes(3.0 * 1024.0 * 1024.0), "3.0 MiB");
    }
}
