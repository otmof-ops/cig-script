// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The burn protocol: a hop that knows what it changed says so, in JSON
//! lines appended to the file `CIG_JOURNAL` names, and says how to undo it.
//! `docs/PROTOCOL.md` is the contract. This module reads the lines; the
//! kernel decides what each one is worth, and never trusts a hop's word
//! about bytes it can check.

use super::journal::Compensation;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// The environment variable that names a hop's report file.
pub const ENV: &str = "CIG_JOURNAL";

/// What a tool said was there before it wrote.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum Claimed {
    /// `"before":"sha256:<hex>"`.
    Hash(String),
    /// `"before":"absent"`.
    Absent,
    /// Nothing said.
    Unknown,
}

/// One line of a hop's report, with the line it came from.
#[derive(Clone, Debug, Serialize)]
pub enum Claim {
    Write {
        path: PathBuf,
        before: Claimed,
        saved: Option<PathBuf>,
        line: usize,
    },
    Delete {
        path: PathBuf,
        before: Claimed,
        saved: Option<PathBuf>,
        line: usize,
    },
    Create {
        path: PathBuf,
        line: usize,
    },
    Compensate {
        label: String,
        undo: Vec<String>,
        cwd: Option<PathBuf>,
        line: usize,
    },
}

impl Claim {
    pub fn line(&self) -> usize {
        match self {
            Claim::Write { line, .. }
            | Claim::Delete { line, .. }
            | Claim::Create { line, .. }
            | Claim::Compensate { line, .. } => *line,
        }
    }

    /// The path a file claim is about, as the tool wrote it.
    pub fn path(&self) -> Option<&Path> {
        match self {
            Claim::Write { path, .. } | Claim::Delete { path, .. } | Claim::Create { path, .. } => {
                Some(path)
            }
            Claim::Compensate { .. } => None,
        }
    }
}

/// A hop's report, read: the claims that parsed, and one word per line
/// that did not.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Report {
    pub claims: Vec<Claim>,
    pub warnings: Vec<String>,
}

/// Read the report a hop left at `path`, if any.
pub fn read(path: &Path) -> Report {
    match fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(_) => Report::default(),
    }
}

fn claimed(v: &serde_json::Value) -> Claimed {
    match v.get("before").and_then(|b| b.as_str()) {
        Some("absent") => Claimed::Absent,
        Some(s) => match s.strip_prefix("sha256:") {
            Some(hex) if hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()) => {
                Claimed::Hash(hex.to_ascii_lowercase())
            }
            _ => Claimed::Unknown,
        },
        None => Claimed::Unknown,
    }
}

fn path_of(v: &serde_json::Value, key: &str) -> Option<PathBuf> {
    v.get(key)
        .and_then(|p| p.as_str())
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
}

/// Parse the lines of a report. A line that is not JSON, names no `op`, or
/// lacks what its `op` needs is skipped with a word naming the line.
pub fn parse(text: &str) -> Report {
    let mut report = Report::default();
    for (i, raw) in text.lines().enumerate() {
        let n = i + 1;
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(_) => {
                report
                    .warnings
                    .push(format!("line {n} of the hop's report is not JSON; skipped"));
                continue;
            }
        };
        let op = v.get("op").and_then(|o| o.as_str()).unwrap_or("");
        match op {
            "write" | "delete" => {
                let Some(path) = path_of(&v, "path") else {
                    report.warnings.push(format!(
                        "line {n} of the hop's report (`{op}`) names no path; skipped"
                    ));
                    continue;
                };
                let before = claimed(&v);
                if matches!(before, Claimed::Unknown) && v.get("before").is_some() {
                    report.warnings.push(format!(
                        "line {n} of the hop's report: `before` is neither `sha256:<hex>` nor `absent`; read as unknown"
                    ));
                }
                let saved = path_of(&v, "saved");
                report.claims.push(if op == "write" {
                    Claim::Write {
                        path,
                        before,
                        saved,
                        line: n,
                    }
                } else {
                    Claim::Delete {
                        path,
                        before,
                        saved,
                        line: n,
                    }
                });
            }
            "create" => match path_of(&v, "path") {
                Some(path) => report.claims.push(Claim::Create { path, line: n }),
                None => report.warnings.push(format!(
                    "line {n} of the hop's report (`create`) names no path; skipped"
                )),
            },
            "compensate" => {
                let undo: Vec<String> = v
                    .get("undo")
                    .and_then(|u| u.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                if undo.is_empty() {
                    report.warnings.push(format!(
                        "line {n} of the hop's report (`compensate`) has no `undo` command; skipped"
                    ));
                    continue;
                }
                let label = v
                    .get("label")
                    .and_then(|l| l.as_str())
                    .filter(|l| !l.trim().is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| undo.join(" "));
                report.claims.push(Claim::Compensate {
                    label,
                    undo,
                    cwd: path_of(&v, "cwd"),
                    line: n,
                });
            }
            "" => report.warnings.push(format!(
                "line {n} of the hop's report names no `op`; skipped"
            )),
            other => report.warnings.push(format!(
                "line {n} of the hop's report: unknown op `{other}` (write, delete, create, compensate); skipped"
            )),
        }
    }
    report
}

/// A compensation the tool gave as a command: run it, no shell, from the
/// directory it named, with the run's id in its environment. Ten minutes
/// at most; a non-zero exit is the compensation failing, with stderr's
/// last line.
pub fn run_command(c: &Compensation) -> Result<(), String> {
    let argv = c
        .command
        .as_ref()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| "the compensation names no command".to_string())?;
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    if let Some(d) = &c.cwd {
        cmd.current_dir(d);
    }
    cmd.env("NO_COLOR", "1");
    if let Some(id) = crate::crash::active_run().or_else(|| std::env::var("CIG_RUN_ID").ok()) {
        cmd.env("CIG_RUN_ID", id);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let label = c.label.clone().unwrap_or_else(|| argv.join(" "));
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("the undo `{}` could not start: {e}", argv.join(" ")))?;
    // Both pipes are drained while the undo runs: one that says a lot must
    // not fill a pipe and then wait on it for ever.
    let out_thread = child.stdout.take().map(|mut p| {
        std::thread::spawn(move || {
            use std::io::Read as _;
            let mut buf = Vec::new();
            let _ = p.read_to_end(&mut buf);
        })
    });
    let err_thread = child.stderr.take().map(|mut p| {
        std::thread::spawn(move || {
            use std::io::Read as _;
            let mut buf = Vec::new();
            let _ = p.read_to_end(&mut buf);
            buf
        })
    });
    use wait_timeout::ChildExt;
    let status = match child
        .wait_timeout(Duration::from_secs(600))
        .map_err(|e| e.to_string())?
    {
        Some(s) => s,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "the undo for {label} ran longer than ten minutes and was killed"
            ));
        }
    };
    if let Some(t) = out_thread {
        let _ = t.join();
    }
    let err = err_thread
        .and_then(|t| t.join().ok())
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    if status.success() {
        return Ok(());
    }
    let tail = cut(err.trim().lines().last().unwrap_or(""), 200);
    Err(format!(
        "the undo `{}` exited {}{}",
        argv.join(" "),
        status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "by signal".to_string()),
        if tail.is_empty() {
            String::new()
        } else {
            format!("; stderr: `{tail}`")
        }
    ))
}

/// The first `max` characters of a line, with a word for the rest.
fn cut(line: &str, max: usize) -> String {
    let total = line.chars().count();
    if total <= max {
        return line.to_string();
    }
    let head: String = line.chars().take(max).collect();
    format!("{head}... ({} more characters)", total - max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_parse_and_bad_ones_are_named() {
        let r = parse(concat!(
            "{\"op\":\"write\",\"path\":\"db/schema.sql\",\"before\":\"sha256:",
            "0000000000000000000000000000000000000000000000000000000000000000\",\"saved\":\"/tmp/s\"}\n",
            "not json\n",
            "{\"op\":\"create\",\"path\":\"out.js\"}\n",
            "{\"op\":\"delete\",\"path\":\"x\",\"before\":\"absent\"}\n",
            "{\"op\":\"compensate\",\"label\":\"m 42\",\"undo\":[\"migrate\",\"down\"]}\n",
            "{\"op\":\"compensate\"}\n",
            "{\"op\":\"dance\"}\n",
            "{\"path\":\"no-op\"}\n",
        ));
        assert_eq!(r.claims.len(), 4, "{r:?}");
        assert_eq!(r.warnings.len(), 4, "{r:?}");
        assert!(r.warnings[0].contains("line 2") && r.warnings[0].contains("not JSON"));
        assert!(r.warnings[1].contains("line 6"));
        assert!(r.warnings[2].contains("unknown op `dance`"));
        assert!(
            matches!(&r.claims[0], Claim::Write { before: Claimed::Hash(h), saved: Some(_), .. } if h.len() == 64)
        );
        assert!(matches!(
            &r.claims[2],
            Claim::Delete {
                before: Claimed::Absent,
                ..
            }
        ));
        assert!(
            matches!(&r.claims[3], Claim::Compensate { label, undo, .. } if label == "m 42" && undo.len() == 2)
        );
    }
}
