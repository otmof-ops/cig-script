// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The trial: before a script runs for real on a cig it has not been proved
//! on, its plan on this cig and its plan on the last version it went
//! through on are compared, op by op. Identical plans mean the new cig is,
//! for this script, the same cig. Different plans mean the proved version
//! runs it, and the difference is a finding.

use crate::burn::{Kernel, Mode, PlannedOp};
use crate::interp::Interp;
use crate::syntax::ast::Program;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// One op of a plan, as far as the comparison looks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanOp {
    pub summary: String,
    pub reversible: bool,
}

impl From<&PlannedOp> for PlanOp {
    fn from(p: &PlannedOp) -> Self {
        PlanOp {
            summary: p.summary.clone(),
            reversible: p.reversible,
        }
    }
}

/// The plan this cig makes for the script: a dry run in this process, its
/// output swallowed.
pub fn plan_here(
    program: &Program,
    name: &str,
    args: &[String],
    max_steps: Option<u64>,
) -> Result<Vec<PlanOp>, String> {
    let kernel = Kernel::ephemeral(Mode::DryRun);
    let mut interp = Interp::new(kernel);
    interp.out = Box::new(std::io::sink());
    interp.script_name = Some(name.to_string());
    interp.set_args(args);
    if let Some(n) = max_steps {
        interp.set_max_steps(n);
    }
    interp
        .run_program(program)
        .map_err(|d| format!("the dry run on this cig failed: {}", d.message))?;
    Ok(interp.kernel.dry_run_plan().map(PlanOp::from).collect())
}

/// `plan_here`, recorded: the dry run runs inside a pass's recording, so
/// what it read (files, folders, variables) is known, and a trial whose
/// inputs have not changed can be remembered (cig-script#186). The whole
/// program is the work: its globals are its own.
pub fn plan_here_recorded(
    program: &Program,
    name: &str,
    args: &[String],
    max_steps: Option<u64>,
) -> (Result<Vec<PlanOp>, String>, cig_pass::Recorder) {
    let barrier = crate::interp::env::next_serial();
    let kernel = Kernel::ephemeral(Mode::DryRun);
    let mut interp = Interp::new(kernel);
    interp.out = Box::new(std::io::sink());
    interp.script_name = Some(name.to_string());
    interp.set_args(args);
    if let Some(n) = max_steps {
        interp.set_max_steps(n);
    }
    interp
        .recs
        .push(crate::interp::record::RecFrame::simulated(barrier));
    let ran = interp.run_program(program);
    let frame = interp.recs.pop().expect("the frame pushed above");
    let plan = ran
        .map(|()| interp.kernel.dry_run_plan().map(PlanOp::from).collect())
        .map_err(|d| format!("the dry run on this cig failed: {}", d.message));
    (plan, frame.rec)
}

/// What a remembered trial says.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Remembered {
    pub same: bool,
    pub ops: usize,
    pub diff: Vec<String>,
}

fn trial_subject(source_sha: &str, args: &[String], here: &str, there: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "args": args, "here": here, "script": source_sha, "there": there,
    }))
    .unwrap_or_default()
}

/// A trial this machine remembers for this script, these arguments and
/// these two versions, still holding: nothing it read has changed.
pub fn remembered(
    source_sha: &str,
    args: &[String],
    here: &str,
    there: &str,
) -> Option<Remembered> {
    let keys = crate::pass::Keys::load().ok()?;
    let marks = crate::pass::state(&keys).ok()?;
    let store = crate::pass::store().ok()?;
    let ctx = crate::pass::CigCtx::new(&keys, &marks, Some(&store));
    let (p, _) = crate::pass::find(&ctx, "trial", &trial_subject(source_sha, args, here, there))?;
    serde_json::from_slice(&p.result).ok()
}

/// Remember a trial: a `trial` pass over what the dry run read, the
/// script, and the proved cig's binary. Nothing is kept when the dry run
/// could not be vouched for (a hop without `reads`, the clock).
#[allow(clippy::too_many_arguments)]
pub fn remember(
    mut rec: cig_pass::Recorder,
    script: &Path,
    proved_bin: &Path,
    source_sha: &str,
    args: &[String],
    here: &str,
    there: &str,
    verdict: &Remembered,
) {
    let (Ok(script), Ok(bin)) = (std::path::absolute(script), std::path::absolute(proved_bin))
    else {
        return;
    };
    if rec.file_seen(&script).is_err() || rec.file_seen(&bin).is_err() {
        return;
    }
    // The proved cig's own plan came from a process cig cannot see into;
    // it ran the same script on the same inputs, a declaration the pass
    // carries.
    rec.declared_reads();
    let Ok(keys) = crate::pass::Keys::load() else {
        return;
    };
    let Ok(marks) = crate::pass::state(&keys) else {
        return;
    };
    let Ok(result) = serde_json::to_vec(verdict) else {
        return;
    };
    let opts = crate::pass::IssueOpts {
        cig: true,
        store: true,
        ..Default::default()
    };
    let _ = crate::pass::issue(
        &keys,
        &marks,
        "trial",
        &trial_subject(source_sha, args, here, there),
        rec,
        result,
        &opts,
    );
}

/// The plan another cig makes: `cig --json --no-doctor run --dry-run`, the
/// last line of its stdout being the plan.
pub fn plan_with(bin: &Path, file: &Path, args: &[String]) -> Result<Vec<PlanOp>, String> {
    let mut cmd = std::process::Command::new(bin);
    cmd.args(["--json", "--no-doctor", "run", "--dry-run"])
        .arg(file)
        .env("CIG_DISPATCHED", "1");
    if !args.is_empty() {
        cmd.arg("--").args(args);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("cannot start {}: {e}", bin.display()))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let last = stdout
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .ok_or_else(|| {
            format!(
                "{} printed no plan (exit {:?}): {}",
                bin.display(),
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).trim()
            )
        })?;
    let v: serde_json::Value =
        serde_json::from_str(last).map_err(|e| format!("the plan is not JSON: {e}"))?;
    if v["ok"].as_bool() == Some(false) {
        return Err(format!(
            "the dry run on {} failed; the plan is not comparable",
            bin.display()
        ));
    }
    let plan = v["plan"]
        .as_array()
        .ok_or("the plan has no `plan` list")?
        .iter()
        .map(|op| PlanOp {
            summary: op["summary"].as_str().unwrap_or("").to_string(),
            reversible: op["reversible"].as_bool().unwrap_or(false),
        })
        .collect();
    Ok(plan)
}

/// What differs between two plans, one line each, `+` for an op only the
/// first has and `-` for one only the second has, in order.
pub fn diff(mine: &[PlanOp], theirs: &[PlanOp]) -> Vec<String> {
    if mine == theirs {
        return Vec::new();
    }
    let mut out = Vec::new();
    let common = mine
        .iter()
        .zip(theirs.iter())
        .take_while(|(a, b)| a == b)
        .count();
    for op in &theirs[common..] {
        out.push(format!("- {}{}", op.summary, tag(op)));
    }
    for op in &mine[common..] {
        out.push(format!("+ {}{}", op.summary, tag(op)));
    }
    out
}

fn tag(op: &PlanOp) -> &'static str {
    if op.reversible {
        ""
    } else {
        "  (irreversible)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_plans_have_no_diff_and_a_tail_is_listed() {
        let a = vec![
            PlanOp {
                summary: "write x".into(),
                reversible: true,
            },
            PlanOp {
                summary: "mkdir d".into(),
                reversible: true,
            },
        ];
        assert!(diff(&a, &a).is_empty());
        let mut b = a.clone();
        b.push(PlanOp {
            summary: "run rm -rf".into(),
            reversible: false,
        });
        assert_eq!(diff(&b, &a), vec!["+ run rm -rf  (irreversible)"]);
        assert_eq!(diff(&a, &b), vec!["- run rm -rf  (irreversible)"]);
    }

    #[test]
    fn the_plan_here_swallows_output_and_lists_the_burns() {
        let p =
            crate::syntax::parse("exhale \"noise\"\nburn { fs.write_text(\"a.txt\", \"1\") }\n")
                .unwrap();
        let plan = plan_here(&p, "s.cig", &[], None).unwrap();
        assert_eq!(plan.len(), 1);
        assert!(plan[0].summary.starts_with("write "));
    }
}
