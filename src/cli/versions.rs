// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! `cig versions`: what is in the humidor, and putting versions there.

use super::{exit, Ctx};
use crate::config::Config;
use crate::humidor;
use crate::update::Version;

#[derive(clap::Subcommand)]
pub enum VersionsCommand {
    /// What the humidor holds, and the installed version (the default).
    List,
    /// Fetch a release into the humidor, verified against its checksum.
    Add {
        /// A version such as 1.1.1 (with or without the v).
        version: String,
    },
    /// Remove a version from the humidor.
    Remove {
        /// A version such as 1.1.1.
        version: String,
    },
    /// Keep the newest N versions and remove the rest.
    Prune {
        #[arg(long, value_name = "N", default_value_t = 3)]
        keep: usize,
    },
}

pub fn versions(ctx: &Ctx, cmd: Option<VersionsCommand>) -> i32 {
    let cfg = Config::load();
    let repo = cfg.get("issues_repo");
    match cmd.unwrap_or(VersionsCommand::List) {
        VersionsCommand::List => list(ctx, &repo),
        VersionsCommand::Add { version } => {
            let Some(v) = Version::parse(&version) else {
                return usage(ctx, &format!("`{version}` is not a version"));
            };
            if v == Version::current() {
                eprintln!(
                    "{} {v} is the installed cig; the humidor is for the others",
                    ctx.yellow("note:")
                );
                return exit::OK;
            }
            if let Some(p) = humidor::binary(&v) {
                eprintln!(
                    "{} {v} is already in the humidor ({})",
                    ctx.green("kept:"),
                    p.display()
                );
                return exit::OK;
            }
            eprintln!("fetching cig {v} from {repo} ...");
            match humidor::add(&repo, &v) {
                Ok(entry) => {
                    eprintln!(
                        "{} {v} is in the humidor, {} (sha256 {})",
                        ctx.green("kept:"),
                        entry.trust,
                        &entry.sha256[..entry.sha256.len().min(12)]
                    );
                    if ctx.json {
                        outln!("{}", serde_json::to_string(&entry).unwrap_or_default());
                    }
                    exit::OK
                }
                Err(e) => {
                    eprintln!("{} {e}", ctx.red("error[E807 usage]:"));
                    eprintln!("  = explain: cig explain E807");
                    exit::USAGE
                }
            }
        }
        VersionsCommand::Remove { version } => {
            let Some(v) = Version::parse(&version) else {
                return usage(ctx, &format!("`{version}` is not a version"));
            };
            if humidor::binary(&v).is_none() {
                return usage(ctx, &format!("{v} is not in the humidor"));
            }
            match humidor::remove(&v) {
                Ok(()) => {
                    eprintln!("removed {v} from the humidor");
                    exit::OK
                }
                Err(e) => usage(ctx, &format!("cannot remove {v}: {e}")),
            }
        }
        VersionsCommand::Prune { keep } => {
            let gone = humidor::prune(keep);
            if ctx.json {
                outln!(
                    "{}",
                    serde_json::json!({"removed": gone.iter().map(|v| v.to_string()).collect::<Vec<_>>(), "kept": keep})
                );
            } else {
                eprintln!(
                    "removed {} version{}, kept the newest {keep}",
                    gone.len(),
                    if gone.len() == 1 { "" } else { "s" }
                );
            }
            exit::OK
        }
    }
}

fn usage(ctx: &Ctx, msg: &str) -> i32 {
    eprintln!("{} {msg}", ctx.red("error[E800 usage]:"));
    exit::USAGE
}

fn list(ctx: &Ctx, repo: &str) -> i32 {
    let shelf = humidor::list();
    let yanked = humidor::yanked(repo);
    let current = Version::current();
    if ctx.json {
        let rows: Vec<serde_json::Value> = shelf
            .iter()
            .map(|(v, e)| {
                serde_json::json!({
                    "version": v.to_string(),
                    "source": e.source,
                    "sha256": e.sha256,
                    "trust": e.trust,
                    "fetched_at": e.fetched_at,
                    "last_lit": e.last_lit,
                    "yanked": yanked.contains(v),
                })
            })
            .collect();
        outln!(
            "{}",
            serde_json::json!({
                "installed": current.to_string(),
                "humidor": rows,
                "yanked": yanked.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
                "dir": humidor::dir(),
            })
        );
        return exit::OK;
    }
    outln!(
        "{:<10} {:<10} {:<12} {}",
        "version",
        "trust",
        "last lit",
        "where"
    );
    outln!(
        "{:<10} {:<10} {:<12} {}",
        current,
        "installed",
        "-",
        std::env::current_exe()
            .map(|p| crate::crash::tilde(&p))
            .unwrap_or_default()
    );
    for (v, e) in &shelf {
        let mark = if yanked.contains(v) { " (yanked)" } else { "" };
        outln!(
            "{:<10} {:<10} {:<12} {}{mark}",
            v,
            e.trust,
            e.last_lit
                .as_deref()
                .map(|t| t.get(..10).unwrap_or(t).to_string())
                .unwrap_or_else(|| "never".to_string()),
            crate::crash::tilde(&humidor::dir().join(v.to_string()))
        );
    }
    if shelf.is_empty() {
        outln!(
            "{}",
            ctx.dim("the humidor is empty; cig versions add 1.1.2 keeps a release there, and a pinned script fetches what it needs")
        );
    }
    exit::OK
}
