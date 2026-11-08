// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! `cig box`: the layers hops kept rather than applied (`apply: never`,
//! or `ask` declined or away from a terminal). `list` names them, `apply`
//! puts one into the pack as a run of its own (journaled, unburnable like
//! any other), `discard` drops one.

use std::path::Path;

use super::{exit, Ctx};
use crate::burn::overlay::{self, Kept};
use crate::burn::runs::RunRecord;
use crate::burn::{Kernel, Mode};

#[derive(clap::Subcommand)]
pub enum BoxCommand {
    /// The layers kept and not applied (the default).
    List,
    /// Apply a kept layer to the pack, as a new run.
    Apply {
        /// The run the hop belonged to (a prefix is enough).
        run: String,
        /// The hop's number within it (`cig box list` shows both).
        seq: u64,
    },
    /// Drop a kept layer.
    Discard { run: String, seq: u64 },
}

pub fn box_cmd(ctx: &Ctx, cmd: Option<BoxCommand>) -> i32 {
    match cmd.unwrap_or(BoxCommand::List) {
        BoxCommand::List => list(ctx),
        BoxCommand::Apply { run, seq } => match find(ctx, &run, seq) {
            Some(k) => apply(ctx, &k),
            None => exit::USAGE,
        },
        BoxCommand::Discard { run, seq } => match find(ctx, &run, seq) {
            Some(k) => {
                // The layer, its record, and a contained hop's scratch folder.
                overlay::discard(&overlay::layers_of(&k));
                outln!("discarded the layer of {}/{} ({})", k.run, k.seq, k.label);
                exit::OK
            }
            None => exit::USAGE,
        },
    }
}

fn list(ctx: &Ctx) -> i32 {
    let kept = overlay::kept_layers();
    if ctx.json {
        outln!("{}", serde_json::to_string(&kept).unwrap_or_default());
        return exit::OK;
    }
    if kept.is_empty() {
        eprintln!("no kept layers (a hop with apply: never, or ask declined, leaves one)");
        return exit::OK;
    }
    outln!(
        "{:<24} {:>4}  {:>7} {:>7}  {:<20}  {}",
        "RUN",
        "HOP",
        "WRITTEN",
        "REMOVED",
        "KEPT AT",
        "COMMAND"
    );
    for k in kept {
        outln!(
            "{:<24} {:>4}  {:>7} {:>7}  {:<20}  {}",
            k.run,
            k.seq,
            k.written,
            k.removed,
            k.at,
            k.label.lines().next().unwrap_or("")
        );
    }
    exit::OK
}

fn find(ctx: &Ctx, run: &str, seq: u64) -> Option<Kept> {
    let found: Vec<Kept> = overlay::kept_layers()
        .into_iter()
        .filter(|k| k.seq == seq && k.run.starts_with(run))
        .collect();
    match found.len() {
        1 => found.into_iter().next(),
        0 => {
            eprintln!(
                "{} no kept layer for run `{run}` hop {seq}; cig box list shows them",
                ctx.red("error[E803 usage]:")
            );
            eprintln!("  = explain: cig explain E803");
            None
        }
        _ => {
            eprintln!(
                "{} `{run}` matches more than one run; give more of the id",
                ctx.red("error[E803 usage]:")
            );
            None
        }
    }
}

/// Apply a kept layer as a run of its own: the layer's changes journaled
/// with their before-images under a new record, so `cig unburn <id>` puts
/// them back; the layer goes once it is in the pack.
fn apply(ctx: &Ctx, k: &Kept) -> i32 {
    if let Err(e) = crate::burn::ensure_private() {
        eprintln!("{} {e}", ctx.red("error[E707 burn]:"));
        return exit::SCRIPT_ERROR;
    }
    // As `cig run`: the key's lock before the first op (#105).
    if let Err(e) = crate::burn::seal::hold_shared_rekey_lock() {
        eprintln!("{} {e}", ctx.red("error[E802 usage]:"));
        return exit::SCRIPT_ERROR;
    }
    let script = format!("box apply {}/{}", k.run, k.seq);
    let mut record = RunRecord::begin(Path::new(&script), &script, &[], "run");
    if let Err(e) = record.save() {
        eprintln!(
            "{} cannot create the run record: {e}",
            ctx.red("error[E802 usage]:")
        );
        return exit::USAGE;
    }
    let run_dir = record.dir();
    let mut kernel = match Kernel::new(Mode::Run, Some(run_dir)) {
        Ok(k) => k,
        Err(d) => {
            eprintln!("{} {}", ctx.red("error:"), d.message);
            return exit::SCRIPT_ERROR;
        }
    };
    if let Err(d) = kernel.set_pack(k.roots.clone()) {
        eprintln!(
            "{} {}",
            ctx.red(&format!("error[{} burn]:", d.code)),
            d.message
        );
        record.finish("error", Some(d.message.clone()));
        let _ = record.save();
        return exit::SCRIPT_ERROR;
    }
    let layers = overlay::layers_of(k);
    let applied = kernel.box_apply(&layers, &k.touches_sealed);
    record.burns = kernel.executed;
    record.irreversible = kernel.irreversible;
    record.journal_head = Some(kernel.journal().head().to_string());
    let (status, code) = match &applied {
        Ok(_) => ("ok", exit::OK),
        Err(e) => {
            eprintln!(
                "{} applying the layer of {}/{}: {e}; what was applied before it is in run {} (cig unburn {} puts it back)",
                ctx.red("error[E763 burn]:"),
                k.run,
                k.seq,
                record.id,
                record.id
            );
            ("error", exit::SCRIPT_ERROR)
        }
    };
    record.finish(status, applied.as_ref().err().cloned());
    if kernel.journal().format() >= 3 {
        if let Err(e) = crate::burn::ledger::append(
            "run",
            &record.id,
            kernel.journal().head(),
            &record.status,
            kernel.journal().chain().lines as u64,
        ) {
            eprintln!("{} could not write the ledger: {e}", ctx.yellow("warning:"));
        }
    }
    if !kernel.journal().is_empty() {
        if let Err(e) = kernel.journal().write_after_state() {
            eprintln!(
                "{} could not record the run's after-state: {e}",
                ctx.yellow("warning:")
            );
        }
    }
    if let Err(e) = record.save() {
        eprintln!(
            "{} could not finish the run record: {e}",
            ctx.yellow("warning:")
        );
    }
    if let Ok(w) = &applied {
        overlay::discard(&layers);
        let written = w.created.len() + w.modified.len();
        outln!(
            "applied the layer of {}/{} as run {}: {} path{} written, {} removed; cig unburn {} puts them back",
            k.run,
            k.seq,
            record.id,
            written,
            if written == 1 { "" } else { "s" },
            w.deleted.len(),
            record.id
        );
    }
    code
}
