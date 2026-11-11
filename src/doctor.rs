// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Doctor: automatic diagnosis on the emit hook.
//!
//! When a diagnostic is reported, doctor looks the code up in the registry,
//! walks its ranked causes and runs the read-only probe named for each one.
//! The first cause a probe confirms becomes the verdict, said the only way
//! doctor is allowed to say anything: *I think X, because I checked Y and
//! found Z.* The remaining causes become "if that is not it", each with the
//! one check that would confirm it.
//!
//! Two rules, kept by construction:
//!
//! * **Read-only, always.** Probes stat paths, read PATH, read the journal
//!   and the run records. Nothing here writes, spawns the failing command
//!   again, or touches the network. `tests/cli.rs` proves it by re-running
//!   a diagnosis against a tree it then compares byte for byte.
//! * **Quiet when it has nothing.** No probe confirmed: one line. No probe
//!   could run at all: nothing.
//!
//! Doctor never fixes. A remedy is text; the person types it.

use crate::burn::runs;
use crate::diagnostics::Diagnostic;
use crate::errors::{lookup, lookup_probe, Cause, Code};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Time budget for one automatic diagnosis. An error must never become
/// slower than the run that produced it.
pub const BUDGET_MS: u128 = 80;

/// What the diagnosis may look at.
pub struct Context {
    /// Where relative paths in the diagnostic resolve.
    pub cwd: PathBuf,
    /// The script's source, for probes that read the text.
    pub source: Option<String>,
}

impl Context {
    pub fn here(source: Option<&str>) -> Self {
        Self {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            source: source.map(str::to_string),
        }
    }
}

/// Is automatic diagnosis switched on? `CIG_DOCTOR=0` turns it off
/// everywhere; the `--no-doctor` flag turns it off for one command.
pub fn enabled_by_env() -> bool {
    !matches!(
        std::env::var("CIG_DOCTOR").as_deref(),
        Ok("0") | Ok("false") | Ok("off")
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The probe found the evidence this cause predicts.
    Confirmed,
    /// The probe found evidence against this cause.
    RuledOut,
    /// The probe ran and learned something, but it decides nothing.
    Inconclusive,
    /// No read-only probe exists for this cause (yet), or the budget ran out.
    Unavailable,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProbeResult {
    /// 1-based index of the cause in the registry node.
    pub cause: usize,
    pub probe: String,
    /// What the probe reads, from the registry.
    pub reads: String,
    pub outcome: Outcome,
    /// What it found, in one line; empty when it found nothing to say.
    pub evidence: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Diagnosis {
    pub code: String,
    /// True when a probe confirmed a cause.
    pub confirmed: bool,
    /// The cause doctor thinks it is (registry text), when confirmed.
    pub verdict: String,
    /// The kind-of-no id for the verdict's cause, when the cause names one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no: Option<String>,
    /// "because I checked Y and found Z", one line per probe that ran.
    pub why: Vec<String>,
    /// The remedy for the confirmed cause, then the node's fix if different.
    pub fix: Vec<String>,
    /// The other causes not ruled out, each with the check that confirms it.
    pub if_not: Vec<String>,
    pub probes: Vec<ProbeResult>,
    pub ms: u128,
}

/// Diagnose one reported diagnostic. `None` means doctor has nothing to add:
/// the code is unknown, or no cause has a probe that could run here.
pub fn diagnose(d: &Diagnostic, cx: &Context) -> Option<Diagnosis> {
    let node = lookup(d.code)?;
    let started = Instant::now();
    let mut results = Vec::new();
    for (i, cause) in node.causes.iter().enumerate() {
        let reads = lookup_probe(&cause.probe)
            .map(|p| p.reads.clone())
            .unwrap_or_default();
        if started.elapsed().as_millis() > BUDGET_MS {
            results.push(ProbeResult {
                cause: i + 1,
                probe: cause.probe.clone(),
                reads,
                outcome: Outcome::Unavailable,
                evidence: "not run: the diagnosis budget was spent".to_string(),
            });
            continue;
        }
        let (outcome, evidence) = run_probe(&cause.probe, cause, node, d, cx);
        results.push(ProbeResult {
            cause: i + 1,
            probe: cause.probe.clone(),
            reads,
            outcome,
            evidence,
        });
    }
    let ran = results.iter().any(|r| r.outcome != Outcome::Unavailable);
    if !ran {
        return None;
    }
    let confirmed_at = results.iter().position(|r| r.outcome == Outcome::Confirmed);
    // "why": the confirming evidence. Ruled-out probes stay in `probes` for
    // --json and are not narrated; nobody wants the list of what it was not.
    let mut why: Vec<String> = Vec::new();
    if let Some(i) = confirmed_at {
        why.push(format!(
            "I checked {} and found {}",
            results[i].reads, results[i].evidence
        ));
    }
    let mut fix = Vec::new();
    let mut verdict = String::new();
    let mut no = None;
    if let Some(i) = confirmed_at {
        let cause = &node.causes[i];
        verdict = cause.why.clone();
        no = cause.no.clone();
        fix.push(cause.remedy.clone());
    }
    // "if not": the other causes still standing. When something was
    // confirmed, a cause nothing can check is a guess and is left out; when
    // nothing was, every standing cause is listed with what was seen.
    let if_not: Vec<String> = node
        .causes
        .iter()
        .enumerate()
        .filter(|(i, _)| Some(*i) != confirmed_at)
        .filter(|(i, _)| results[*i].outcome != Outcome::RuledOut)
        .filter(|(i, _)| confirmed_at.is_none() || results[*i].outcome != Outcome::Unavailable)
        .filter(|(i, _)| {
            // Two causes sharing one probe confirm together; say it once.
            confirmed_at.is_none_or(|c| results[*i].evidence != results[c].evidence)
        })
        .map(|(i, c)| {
            let check = match results[i].outcome {
                Outcome::Unavailable => lookup_probe(&c.probe)
                    .filter(|p| p.name != "none")
                    .map(|p| format!("check {}", p.reads))
                    .unwrap_or_else(|| "nothing checks this; a suggestion".to_string()),
                _ => format!(
                    "I checked {} and found {}",
                    results[i].reads, results[i].evidence
                ),
            };
            format!("{} ({check})", c.why)
        })
        .collect();
    Some(Diagnosis {
        code: node.code.clone(),
        confirmed: confirmed_at.is_some(),
        verdict,
        no,
        why,
        fix,
        if_not,
        probes: results,
        ms: started.elapsed().as_millis(),
    })
}

// ----- probes ------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Reason {
    NotFound,
    PermissionDenied,
    IsDir,
    NotDir,
    Other,
}

fn reason(d: &Diagnostic) -> Reason {
    let m = d.message.to_ascii_lowercase();
    if m.contains("no such file") || m.contains("not found") {
        Reason::NotFound
    } else if m.contains("permission denied") || m.contains("not permitted") {
        Reason::PermissionDenied
    } else if m.contains("is a directory") {
        Reason::IsDir
    } else if m.contains("not a directory") {
        Reason::NotDir
    } else {
        Reason::Other
    }
}

fn subject_path(d: &Diagnostic, cx: &Context) -> Option<PathBuf> {
    let s = d.subject.as_deref()?;
    let p = Path::new(s);
    Some(if p.is_absolute() {
        p.to_path_buf()
    } else {
        cx.cwd.join(p)
    })
}

fn describe(path: &Path) -> String {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => "a symlink".to_string(),
        Ok(m) if m.is_dir() => "a directory".to_string(),
        Ok(m) => format!("a file of {} bytes", m.len()),
        Err(_) => "nothing".to_string(),
    }
}

fn run_probe(
    name: &str,
    cause: &Cause,
    node: &Code,
    d: &Diagnostic,
    cx: &Context,
) -> (Outcome, String) {
    match name {
        "path-exists" => path_exists(d, cx),
        "parent-exists" => parent_exists(d, cx),
        "path-permissions" => path_permissions(d, cx),
        "path-is-dir" => path_is_dir(d, cx),
        "state-dir-writable" => state_dir_writable(),
        "on-path" => on_path(d),
        "loop-condition" => loop_condition(d, cx),
        "plan-order" => plan_order(d),
        "env-var" => env_var(node),
        "similar-names" => similar_names(d),
        "brace-balance" => brace_balance(cx),
        "int-magnitude" => int_magnitude(d),
        "chain-report" => chain_report(cause, d),
        "network-tools" => network_tools(),
        "run-list" => run_list(d),
        "run-record" | "pid-alive" => run_record(d),
        "snapshot-integrity" => snapshot_integrity(d),
        _ => (Outcome::Unavailable, String::new()),
    }
}

fn path_exists(d: &Diagnostic, cx: &Context) -> (Outcome, String) {
    let Some(p) = subject_path(d, cx) else {
        return (Outcome::Unavailable, String::new());
    };
    let shown = d.subject.clone().unwrap_or_default();
    match std::fs::symlink_metadata(&p) {
        Ok(_) => (
            Outcome::RuledOut,
            format!("`{shown}` exists ({})", describe(&p)),
        ),
        Err(_) if reason(d) == Reason::NotFound || reason(d) == Reason::Other => {
            let parent_missing = p
                .parent()
                .map(|q| !q.as_os_str().is_empty() && std::fs::metadata(q).is_err())
                .unwrap_or(false);
            if parent_missing {
                (
                    Outcome::Inconclusive,
                    format!("nothing at `{shown}`, and its parent directory is missing too"),
                )
            } else {
                (
                    Outcome::Confirmed,
                    format!(
                        "nothing at `{shown}` (resolved against {})",
                        cx.cwd.display()
                    ),
                )
            }
        }
        Err(_) => (
            Outcome::RuledOut,
            format!("`{shown}` is absent, but the error was not about existence"),
        ),
    }
}

fn parent_exists(d: &Diagnostic, cx: &Context) -> (Outcome, String) {
    let Some(p) = subject_path(d, cx) else {
        return (Outcome::Unavailable, String::new());
    };
    let Some(parent) = p.parent().filter(|q| !q.as_os_str().is_empty()) else {
        return (
            Outcome::RuledOut,
            "the path has no parent to be missing".to_string(),
        );
    };
    let rel = Path::new(d.subject.as_deref().unwrap_or(""))
        .parent()
        .map(|q| q.display().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| parent.display().to_string());
    match std::fs::metadata(parent) {
        Ok(m) if m.is_dir() => (Outcome::RuledOut, format!("the parent `{rel}` exists")),
        Ok(_) => (
            Outcome::Confirmed,
            format!("the parent `{rel}` exists but is not a directory"),
        ),
        Err(_) => (
            Outcome::Confirmed,
            format!("the parent `{rel}` does not exist"),
        ),
    }
}

#[cfg(target_os = "linux")]
fn current_uid() -> Option<u32> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("Uid:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(not(target_os = "linux"))]
#[cfg_attr(windows, allow(dead_code))]
fn current_uid() -> Option<u32> {
    None
}

fn nearest_existing(p: &Path) -> Option<PathBuf> {
    p.ancestors()
        .find(|a| !a.as_os_str().is_empty() && std::fs::symlink_metadata(a).is_ok())
        .map(Path::to_path_buf)
}

fn ownership_line(p: &Path) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;
        if let Ok(m) = std::fs::symlink_metadata(p) {
            let mode = m.permissions().mode() & 0o777;
            let uid = m.uid();
            let you = match current_uid() {
                Some(me) if me == uid => " (that is you)".to_string(),
                Some(me) => format!(" (you are uid {me})"),
                None => String::new(),
            };
            return format!(
                "`{}` is mode {mode:04o}, owned by uid {uid}{you}",
                p.display()
            );
        }
    }
    format!("`{}` could not be inspected", p.display())
}

fn path_permissions(d: &Diagnostic, cx: &Context) -> (Outcome, String) {
    let Some(p) = subject_path(d, cx) else {
        return (Outcome::Unavailable, String::new());
    };
    let Some(existing) = nearest_existing(&p) else {
        return (
            Outcome::RuledOut,
            "no part of the path exists yet".to_string(),
        );
    };
    if reason(d) == Reason::PermissionDenied {
        (Outcome::Confirmed, ownership_line(&existing))
    } else {
        (
            Outcome::RuledOut,
            format!(
                "the error was not a permission denial; {}",
                ownership_line(&existing)
            ),
        )
    }
}

fn path_is_dir(d: &Diagnostic, cx: &Context) -> (Outcome, String) {
    let Some(p) = subject_path(d, cx) else {
        return (Outcome::Unavailable, String::new());
    };
    let shown = d.subject.clone().unwrap_or_default();
    match reason(d) {
        Reason::IsDir | Reason::NotDir => {
            (Outcome::Confirmed, format!("`{shown}` is {}", describe(&p)))
        }
        _ => (
            Outcome::RuledOut,
            format!(
                "`{shown}` is {}, and the error was not about that",
                describe(&p)
            ),
        ),
    }
}

fn state_dir_writable() -> (Outcome, String) {
    let home = runs::home_dir();
    match std::fs::metadata(&home) {
        Err(_) => {
            let parent = nearest_existing(&home);
            (
                Outcome::Inconclusive,
                match parent {
                    Some(p) => format!(
                        "`{}` does not exist yet; {}",
                        home.display(),
                        ownership_line(&p)
                    ),
                    None => format!("`{}` does not exist yet", home.display()),
                },
            )
        }
        Ok(m) => {
            #[allow(unused_mut)]
            let mut blocked = m.permissions().readonly();
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                use std::os::unix::fs::PermissionsExt;
                let mode = m.permissions().mode();
                if let Some(me) = current_uid() {
                    if me != 0 && me != m.uid() && mode & 0o022 == 0 {
                        blocked = true;
                    }
                }
            }
            if blocked {
                (Outcome::Confirmed, ownership_line(&home))
            } else {
                (Outcome::RuledOut, ownership_line(&home))
            }
        }
    }
}

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

fn on_path(d: &Diagnostic) -> (Outcome, String) {
    let Some(name) = d.subject.as_deref() else {
        return (Outcome::Unavailable, String::new());
    };
    if reason(d) != Reason::NotFound {
        return (
            Outcome::RuledOut,
            format!("`{name}` was found; the error was about starting it"),
        );
    }
    if name.contains('/') || name.contains('\\') {
        return (
            Outcome::Inconclusive,
            format!(
                "`{name}` is a path, not a name, so PATH was not consulted; there is {} there",
                describe(Path::new(name))
            ),
        );
    }
    let dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    #[allow(unused_mut)]
    let mut candidates = vec![name.to_string()];
    #[cfg(windows)]
    candidates.push(format!("{name}.exe"));
    for dir in &dirs {
        for c in &candidates {
            if is_executable(&dir.join(c)) {
                return (
                    Outcome::RuledOut,
                    format!("`{name}` is at {}", dir.join(c).display()),
                );
            }
        }
    }
    // Places people install things that are often not on PATH for a
    // non-login shell or a service.
    let mut elsewhere = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        for rel in [
            ".local/bin",
            "bin",
            ".cargo/bin",
            "go/bin",
            ".npm-global/bin",
        ] {
            let p = home.join(rel).join(name);
            if is_executable(&p) {
                elsewhere.push(p);
            }
        }
    }
    for abs in [
        "/usr/local/bin",
        "/opt/homebrew/bin",
        "/snap/bin",
        "/opt/local/bin",
    ] {
        let p = Path::new(abs).join(name);
        if is_executable(&p) && !dirs.iter().any(|d| d == Path::new(abs)) {
            elsewhere.push(p);
        }
    }
    let mut evidence = format!(
        "no `{name}` in the {} director{} on PATH",
        dirs.len(),
        if dirs.len() == 1 { "y" } else { "ies" }
    );
    if let Some(p) = elsewhere.first() {
        evidence.push_str(&format!(
            "; a `{name}` exists at {}, and that directory is not on PATH for this run",
            p.display()
        ));
    }
    (Outcome::Confirmed, evidence)
}

fn plan_order(d: &Diagnostic) -> (Outcome, String) {
    match d.message.find("removed earlier in this dry-run by ") {
        Some(i) => (
            Outcome::Confirmed,
            format!(
                "the plan shows {} before this read",
                d.message[i + "removed earlier in this dry-run by ".len()..]
                    .trim_end_matches(')')
                    .to_string()
                    + ")"
            ),
        ),
        None => (Outcome::Unavailable, String::new()),
    }
}

fn loop_condition(d: &Diagnostic, cx: &Context) -> (Outcome, String) {
    let (Some(src), Some(line)) = (cx.source.as_deref(), d.line) else {
        return (Outcome::Unavailable, String::new());
    };
    let Some(text) = src.lines().nth(line.saturating_sub(1) as usize) else {
        return (Outcome::Unavailable, String::new());
    };
    let trimmed = text.trim();
    let Some(rest) = trimmed.strip_prefix("while ") else {
        return (
            Outcome::Inconclusive,
            format!(
                "line {line} is `{trimmed}`; the budget ran out here, not necessarily in a loop"
            ),
        );
    };
    let cond = rest.trim_end_matches('{').trim();
    if matches!(cond, "true" | "1" | "(true)") {
        (
            Outcome::Confirmed,
            format!("the condition on line {line} is `{cond}`, so only a break can end the loop"),
        )
    } else {
        (
            Outcome::Inconclusive,
            format!(
                "the condition on line {line} is `{cond}`; check what inside the loop changes it"
            ),
        )
    }
}

fn env_var(node: &Code) -> (Outcome, String) {
    let (var, default) = match node.code.as_str() {
        "E514" => (
            "CIG_MAX_ALLOC",
            format!("{} bytes", crate::value::max_alloc()),
        ),
        "E515" => ("CIG_MAX_STEPS", "the default budget".to_string()),
        _ => return (Outcome::Unavailable, String::new()),
    };
    match std::env::var(var) {
        Ok(v) if !v.is_empty() => (Outcome::Inconclusive, format!("{var}={v} is set")),
        _ => (
            Outcome::Inconclusive,
            format!("{var} is not set, so the limit is {default}"),
        ),
    }
}

fn similar_names(d: &Diagnostic) -> (Outcome, String) {
    match d.hint.as_deref() {
        Some(h) if h.contains("did you mean") => {
            let suggested = h
                .split('`')
                .nth(1)
                .map(|s| format!("the closest name in scope is `{s}`"))
                .unwrap_or_else(|| h.to_string());
            (Outcome::Confirmed, suggested)
        }
        _ => (
            Outcome::RuledOut,
            "no name in scope is within a couple of edits of it".to_string(),
        ),
    }
}

fn brace_balance(cx: &Context) -> (Outcome, String) {
    let Some(src) = cx.source.as_deref() else {
        return (Outcome::Unavailable, String::new());
    };
    let mut open: Vec<u32> = Vec::new();
    let mut in_str: Option<char> = None;
    for (n, line) in src.lines().enumerate() {
        let mut escaped = false;
        for ch in line.chars() {
            match in_str {
                Some(q) => {
                    if escaped {
                        escaped = false;
                    } else if ch == '\\' && q == '"' {
                        escaped = true;
                    } else if ch == q {
                        in_str = None;
                    }
                }
                None => match ch {
                    '#' => break,
                    '"' | '\'' => in_str = Some(ch),
                    '{' => open.push(n as u32 + 1),
                    '}' => {
                        open.pop();
                    }
                    _ => {}
                },
            }
        }
        in_str = None; // strings do not span lines
    }
    match open.first() {
        Some(line) => (
            Outcome::Confirmed,
            format!("the `{{` on line {line} never closes"),
        ),
        None => (Outcome::RuledOut, "every `{` has a `}`".to_string()),
    }
}

fn int_magnitude(d: &Diagnostic) -> (Outcome, String) {
    let digits: String = {
        let mut best = String::new();
        let mut cur = String::new();
        for ch in d.message.chars() {
            if ch.is_ascii_digit() {
                cur.push(ch);
            } else {
                if cur.len() > best.len() {
                    best = cur.clone();
                }
                cur.clear();
            }
        }
        if cur.len() > best.len() {
            best = cur;
        }
        best
    };
    match digits.parse::<u128>() {
        Ok(n) if n > i64::MAX as u128 => (
            Outcome::Confirmed,
            format!(
                "{n} is {} more than 9223372036854775807, the largest int",
                n - i64::MAX as u128
            ),
        ),
        Ok(_) => (
            Outcome::RuledOut,
            "the number fits; the literal itself is malformed".to_string(),
        ),
        Err(_) => (Outcome::Unavailable, String::new()),
    }
}

fn chain_report(cause: &Cause, d: &Diagnostic) -> (Outcome, String) {
    let m = &d.message;
    let step = m
        .find("failed at step ")
        .map(|i| &m[i + "failed at step ".len()..])
        .and_then(|rest| rest.split(':').next())
        .map(str::trim)
        .unwrap_or("");
    let raised = m
        .find("(raised at line ")
        .map(|i| m[i + 1..].trim_end_matches(')').to_string());
    if cause.why.contains("null") {
        let inner = m.rsplit("): ").next().unwrap_or("").to_ascii_lowercase();
        return if inner.contains("null") {
            (
                Outcome::Confirmed,
                format!("step {step} failed on a null value"),
            )
        } else {
            (
                Outcome::RuledOut,
                "the inner error does not mention null".to_string(),
            )
        };
    }
    let mut evidence = if step.is_empty() {
        "the chain itself raised the error, not one of its steps".to_string()
    } else {
        format!("step {step} raised the error")
    };
    if let Some(r) = raised {
        evidence.push_str(&format!(", {r}"));
    }
    (Outcome::Confirmed, evidence)
}

fn network_tools() -> (Outcome, String) {
    let gh = crate::update::has_tool("gh");
    let curl = crate::update::has_tool("curl");
    match (gh, curl) {
        (false, false) => (
            Outcome::Confirmed,
            "neither `gh` nor `curl` is on PATH".to_string(),
        ),
        _ => (
            Outcome::RuledOut,
            format!(
                "{}{}",
                if gh { "`gh` is on PATH" } else { "" },
                if curl {
                    if gh {
                        " and `curl` is on PATH"
                    } else {
                        "`curl` is on PATH"
                    }
                } else {
                    ""
                }
            ),
        ),
    }
}

fn run_list(d: &Diagnostic) -> (Outcome, String) {
    let all = runs::list().unwrap_or_default();
    let wanted = d.subject.as_deref().unwrap_or("");
    let matches = all
        .iter()
        .filter(|r| !wanted.is_empty() && r.id.starts_with(wanted))
        .count();
    let newest: Vec<&str> = all.iter().take(3).map(|r| r.id.as_str()).collect();
    if all.is_empty() {
        return (Outcome::Confirmed, "no run records exist".to_string());
    }
    match matches {
        0 => (
            Outcome::Confirmed,
            format!(
                "{} run{} recorded, none starting with `{wanted}`; the newest: {}",
                all.len(),
                if all.len() == 1 { "" } else { "s" },
                newest.join(", ")
            ),
        ),
        1 => (
            Outcome::RuledOut,
            format!("exactly one run starts with `{wanted}`"),
        ),
        n => (
            Outcome::Confirmed,
            format!("{n} runs start with `{wanted}`; more of the id is needed"),
        ),
    }
}

fn run_record(d: &Diagnostic) -> (Outcome, String) {
    let Some(id) = d.subject.as_deref() else {
        return (Outcome::Unavailable, String::new());
    };
    match runs::find(id) {
        Ok(Some(r)) => {
            let alive = r
                .pid
                .map(|p| runs::pid_alive(p, r.pid_started.as_deref()))
                .unwrap_or(false);
            let evidence = format!(
                "run {} is `{}`, pid {}{}, {} burn{} in the journal",
                r.id,
                r.status,
                r.pid
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "unknown".into()),
                if alive { " (alive)" } else { " (gone)" },
                r.burns,
                if r.burns == 1 { "" } else { "s" }
            );
            if r.is_interrupted() || r.status == "unburned" {
                (Outcome::Confirmed, evidence)
            } else {
                (Outcome::Inconclusive, evidence)
            }
        }
        _ => (Outcome::Unavailable, String::new()),
    }
}

fn snapshot_integrity(d: &Diagnostic) -> (Outcome, String) {
    let Some(id) = d.subject.as_deref() else {
        return (Outcome::Unavailable, String::new());
    };
    let Ok(Some(r)) = runs::find(id) else {
        return (Outcome::Unavailable, String::new());
    };
    match crate::burn::journal::Journal::load(&r.dir()) {
        Ok(j) => {
            let problems = j.verify();
            if problems.is_empty() {
                (
                    Outcome::RuledOut,
                    "every snapshot is present and matches its hash".to_string(),
                )
            } else {
                (
                    Outcome::Confirmed,
                    format!(
                        "{} problem{}; the first: {}",
                        problems.len(),
                        if problems.len() == 1 { "" } else { "s" },
                        problems[0]
                    ),
                )
            }
        }
        Err(e) => (
            Outcome::Confirmed,
            format!("the journal cannot be read: {e}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::runtime;

    fn cx(dir: &Path, source: Option<&str>) -> Context {
        Context {
            cwd: dir.to_path_buf(),
            source: source.map(str::to_string),
        }
    }

    #[test]
    fn missing_file_is_confirmed_by_stat() {
        let tmp = tempfile::tempdir().unwrap();
        let d = runtime("fs.read_text: in.csv: no such file or directory")
            .code("E508")
            .with_subject("in.csv");
        let dx = diagnose(&d, &cx(tmp.path(), None)).expect("a diagnosis");
        assert!(dx.confirmed);
        assert_eq!(dx.probes[0].outcome, Outcome::Confirmed);
        assert!(dx.verdict.contains("does not exist"), "{dx:?}");
        assert!(dx.why[0].contains("nothing at `in.csv`"), "{dx:?}");
        assert_eq!(dx.no.as_deref(), Some("cant-see-any"));
        // The remaining causes are offered, ruled-out ones are not.
        assert!(
            dx.if_not.iter().all(|s| !s.contains("permission denied")),
            "{dx:?}"
        );
    }

    #[test]
    fn missing_parent_beats_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let d = runtime("fs.read_text: out/in.csv: no such file or directory")
            .code("E508")
            .with_subject("out/in.csv");
        let dx = diagnose(&d, &cx(tmp.path(), None)).unwrap();
        assert!(dx.confirmed);
        assert!(dx.verdict.contains("directory itself"), "{dx:?}");
    }

    #[test]
    fn program_not_on_path_is_confirmed() {
        let d = runtime(
            "proc.run: could not start `definitely-not-a-program-xyz`: no such file or directory",
        )
        .code("E509")
        .with_subject("definitely-not-a-program-xyz");
        let dx = diagnose(&d, &Context::here(None)).unwrap();
        assert!(dx.confirmed);
        assert!(dx.why[0].contains("on PATH"), "{dx:?}");
    }

    #[test]
    fn nothing_to_add_means_none() {
        // E303 has only source-line and none probes: doctor stays quiet.
        let d = crate::diagnostics::Diagnostic::new(
            crate::diagnostics::Kind::Check,
            "effect outside burn",
        )
        .code("E303");
        assert!(diagnose(&d, &Context::here(None)).is_none());
    }

    #[test]
    fn unclosed_brace_is_located() {
        let src = "roll a = 1\nif a > 0 {\n  exhale \"}\"\n";
        let d = crate::diagnostics::syntax("unclosed block", Default::default()).code("E202");
        let dx = diagnose(&d, &cx(Path::new("."), Some(src))).unwrap();
        assert!(dx.confirmed);
        assert!(dx.why[0].contains("line 2"), "{dx:?}");
    }

    #[test]
    fn unconfirmed_is_reported_as_such() {
        let d = runtime("repeat would need 9223372036854775807 bytes").code("E514");
        let dx = diagnose(&d, &Context::here(None)).unwrap();
        assert!(!dx.confirmed);
        assert!(dx.why.is_empty());
        assert!(
            dx.if_not.iter().any(|w| w.contains("CIG_MAX_ALLOC")),
            "{dx:?}"
        );
    }
}
