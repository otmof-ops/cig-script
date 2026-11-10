// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! The commands that moved to `ciginstaller`: `cig update`, `cig uninstall`
//! and `cig doctor` with no run. Each is a stub that hands over (see
//! [`crate::installer`]) and passes the installer's exit code through.

use super::{exit, Ctx};
use crate::installer;

/// `cig update [--check]` is `ciginstaller update [--check]`. A product
/// version is not something the installer takes: updates come by carton, so
/// `--to` and `--allow-downgrade` are explained, not run.
pub fn update(ctx: &Ctx, check_only: bool, allow_downgrade: bool, to: Option<String>) -> i32 {
    if to.is_some() || allow_downgrade {
        eprintln!(
            "{} updates come by carton now, not by cig version",
            ctx.red("error[E800 usage]:")
        );
        eprintln!("  = hint: `ciginstaller carton list` shows the cartons, then `ciginstaller update --to <carton>` moves to one (an older one is a downgrade)");
        eprintln!("  = hint: `ciginstaller update --check` says whether a newer carton exists");
        return exit::USAGE;
    }
    let mut args = vec!["update"];
    if check_only {
        args.push("--check");
    }
    if ctx.json {
        args.push("--json");
    }
    installer::run("cig update", &args)
}

/// `cig uninstall [args]` is `ciginstaller uninstall cig [args]`.
pub fn uninstall_stub(ctx: &Ctx, extra: &[String]) -> i32 {
    let mut args: Vec<&str> = vec!["uninstall", "cig"];
    args.extend(extra.iter().map(String::as_str));
    if ctx.json {
        args.push("--json");
    }
    installer::run("cig uninstall", &args)
}

/// `cig doctor` and `cig doctor --fix` are `ciginstaller doctor --tools`.
/// It calls back into `cig __doctor --json` for what only cig can check.
pub fn doctor_stub(ctx: &Ctx, _fix: bool) -> i32 {
    let mut args = vec!["doctor", "--tools"];
    if ctx.json {
        args.push("--json");
    }
    installer::run("cig doctor", &args)
}
