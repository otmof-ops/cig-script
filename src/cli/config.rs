// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! `cig config`: show, set and unset the few keys in `~/.cigscript/config`.

use super::{exit, Ctx};
use crate::config::{self, Config, KEYS};

pub fn config(ctx: &Ctx, key: Option<String>, value: Option<String>, unset: bool) -> i32 {
    let mut cfg = Config::load();
    match (key, value, unset) {
        (None, _, _) => {
            if ctx.json {
                let map: serde_json::Map<String, serde_json::Value> = cfg
                    .entries()
                    .map(|(k, v, set)| {
                        (
                            k.to_string(),
                            serde_json::json!({"value": v, "explicit": set}),
                        )
                    })
                    .collect();
                outln!(
                    "{}",
                    serde_json::json!({"file": config::path(), "config": map})
                );
                return exit::OK;
            }
            outln!(
                "{}",
                ctx.dim(&format!("file: {}", config::path().display()))
            );
            for (k, default, meaning) in KEYS {
                let v = cfg.get(k);
                let origin = if cfg.is_set(k) {
                    ""
                } else if std::env::var_os(format!("CIG_{}", k.to_ascii_uppercase())).is_some() {
                    "  (from the environment)"
                } else {
                    "  (default)"
                };
                outln!("  {:<15} {}{}", ctx.bold(k), v, ctx.dim(origin));
                outln!(
                    "  {:<15} {}",
                    "",
                    ctx.dim(&format!("{meaning}; default {default}"))
                );
            }
            outln!();
            outln!(
                "{}",
                ctx.dim(
                    "set with: cig config <key> <value>    clear with: cig config <key> --unset"
                )
            );
            exit::OK
        }
        (Some(k), _, true) => {
            if cfg.unset(&k) {
                if let Err(e) = cfg.save() {
                    eprintln!(
                        "{} cannot write {}: {e}",
                        ctx.red("error[E802 usage]:"),
                        config::path().display()
                    );
                    return exit::USAGE;
                }
                eprintln!("{k} cleared; now {}", cfg.get(&k));
            } else {
                eprintln!("{k} was not set");
            }
            exit::OK
        }
        (Some(k), None, false) => {
            if KEYS.iter().any(|(name, _, _)| *name == k) {
                outln!("{}", cfg.get(&k));
                exit::OK
            } else {
                eprintln!(
                    "{} unknown key `{k}`; `cig config` lists them",
                    ctx.red("error[E800 usage]:")
                );
                exit::USAGE
            }
        }
        (Some(k), Some(v), false) => match cfg.set(&k, &v) {
            Ok(()) => {
                if let Err(e) = cfg.save() {
                    eprintln!(
                        "{} cannot write {}: {e}",
                        ctx.red("error[E802 usage]:"),
                        config::path().display()
                    );
                    return exit::USAGE;
                }
                eprintln!("{k} = {v}");
                exit::OK
            }
            Err(msg) => {
                eprintln!("{} {msg}", ctx.red("error[E800 usage]:"));
                exit::USAGE
            }
        },
    }
}
