// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! `cig pin`: write the version a script was written for into the script,
//! and the dispatch that honours a pin before any command runs.

use super::{exit, Ctx};
use crate::config::Config;
use crate::humidor;
use crate::pin::{self, Constraint, Pin};
use crate::update::Version;
use std::path::Path;

/// `cig pin <file> [--to 1.1]`: write a `# cig:` header. Without `--to`,
/// the installed version's major.minor, which is what the script was
/// just written against.
pub fn pin_cmd(ctx: &Ctx, file: &Path, to: Option<String>) -> i32 {
    let source = match super::run::read_source(ctx, file) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let constraint = match &to {
        Some(t) => match Constraint::parse(t) {
            Ok(c) => c.to_string(),
            Err(e) => {
                eprintln!("{} {e}", ctx.red("error[E807 usage]:"));
                eprintln!("  = explain: cig explain E807");
                return exit::USAGE;
            }
        },
        None => match crate::syntax::parse(&source) {
            Ok(program) => {
                let min = crate::since::minimum(&program);
                let uses = crate::since::uses(&program);
                if !ctx.json && !uses.is_empty() {
                    eprintln!(
                        "{}",
                        ctx.dim(&format!(
                            "the script uses {}",
                            uses.iter()
                                .map(|u| format!("{} (from {})", u.what, u.since))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ))
                    );
                }
                format!(">={min}")
            }
            Err(_) => {
                let v = Version::current();
                format!("{}.{}", v.0, v.1)
            }
        },
    };
    let had = pin::header(&source)
        .ok()
        .flatten()
        .map(|(c, _)| c.to_string());
    let out = pin::write(&source, &constraint);
    if let Err(e) = std::fs::write(file, out) {
        eprintln!(
            "{} cannot write {}: {e}",
            ctx.red("error[E801 usage]:"),
            file.display()
        );
        return exit::USAGE;
    }
    if ctx.json {
        outln!(
            "{}",
            serde_json::json!({"file": file, "pin": constraint, "was": had})
        );
    } else {
        match had {
            Some(old) => eprintln!(
                "{} {} now pins cig {constraint} (was {old})",
                ctx.green("pinned:"),
                file.display()
            ),
            None => eprintln!(
                "{} {} now pins cig {constraint}; a cig outside the pin hands it to one inside, or refuses to guess",
                ctx.green("pinned:"),
                file.display()
            ),
        }
    }
    exit::OK
}

/// The pin that governs a script, read before anything is parsed; `Err`
/// is the exit status after the diagnostic was printed.
pub fn pin_of(ctx: &Ctx, file: &Path) -> Result<Option<Pin>, i32> {
    let source = super::run::read_source(ctx, file)?;
    match pin::pin_for(file, &source) {
        Ok(p) => Ok(p),
        Err(e) => {
            let d = crate::diagnostics::usage(format!(
                "{}: the pin cannot be read: {e}",
                file.display()
            ))
            .code("E807")
            .with_hint("write `# cig 1.1` (any 1.1.x), `# cig 1.1.2` (exactly) or `# cig >=1.1.2` under the shebang");
            super::run::report(ctx, &d, None, None);
            Err(exit::USAGE)
        }
    }
}

/// When a script is pinned to a version this cig is not, hand the whole
/// invocation to that version from the humidor, fetching it first when it
/// is not there. `None` means: run here.
pub fn dispatch_if_pinned(ctx: &Ctx, file: &Path, verb: &str) -> Option<i32> {
    let pin = match pin_of(ctx, file) {
        Ok(Some(p)) => p,
        Ok(None) => return None,
        Err(code) => return Some(code),
    };
    let current = Version::current();
    if pin.constraint.satisfied_by(&current) {
        return None;
    }
    if pin::dispatched() {
        if !ctx.json {
            eprintln!(
                "{} {} asks for cig {} and this is {current}, dispatched here; running anyway",
                ctx.yellow("note:"),
                pin.describe(file),
                pin.constraint
            );
        }
        return None;
    }
    let cfg = Config::load();
    let repo = cfg.get("issues_repo");
    let yanked = humidor::yanked(&repo);
    let picked = match humidor::best_for(&pin.constraint, &yanked) {
        Some(p) => Some(p),
        None => fetch_for(ctx, &repo, &pin.constraint, &yanked),
    };
    let Some((version, bin)) = picked else {
        let d = crate::diagnostics::usage(format!(
            "{} asks for cig {} and this is {current}; no cig that satisfies it is in the humidor",
            pin.describe(file),
            pin.constraint
        ))
        .code("E807")
        .with_hint(format!(
            "cig versions add {} when online (the humidor keeps it after that), or change the pin",
            pin.constraint.floor()
        ));
        super::run::report(ctx, &d, None, None);
        return Some(exit::USAGE);
    };
    if yanked.contains(&version) && !ctx.json {
        eprintln!(
            "{} cig {version} is yanked (the repository marked it as not to be used); the pin names it exactly, so it runs",
            ctx.yellow("warning:")
        );
    }
    if !ctx.json {
        eprintln!(
            "{}",
            ctx.dim(&format!(
                "{verb} lit by cig {version} ({}); installed is {current}",
                pin.describe(file)
            ))
        );
    }
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    humidor::touch(&version);
    match humidor::dispatch(&bin, &args) {
        Ok(code) => Some(code),
        Err(e) => {
            eprintln!("{} {e}", ctx.red("error[E807 usage]:"));
            eprintln!("  = explain: cig explain E807");
            Some(exit::USAGE)
        }
    }
}

/// Fetch the newest release that satisfies a constraint into the humidor.
/// No network and nothing cached: nothing is guessed, the caller refuses.
fn fetch_for(
    ctx: &Ctx,
    repo: &str,
    c: &Constraint,
    yanked: &[Version],
) -> Option<(Version, std::path::PathBuf)> {
    let candidates = crate::update::released(repo).ok()?;
    let mut matching: Vec<Version> = candidates
        .into_iter()
        .filter(|v| c.satisfied_by(v) && (c.is_exact() || !yanked.contains(v)))
        .collect();
    matching.sort();
    let want = matching.pop()?;
    if !ctx.json {
        eprintln!(
            "{} cig {want} is not in the humidor; fetching it from {repo} ...",
            ctx.dim("note:")
        );
    }
    match humidor::add(repo, &want) {
        Ok(_) => humidor::binary(&want).map(|p| (want, p)),
        Err(e) => {
            eprintln!("{} {e}", ctx.yellow("note:"));
            None
        }
    }
}
