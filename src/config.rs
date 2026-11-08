// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! `~/.cigscript/config`: a few `key = value` lines. No dependency, no
//! surprises, comments with `#`.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::PathBuf;

/// Every key `cig config` accepts, with its default and meaning.
pub const KEYS: &[(&str, &str, &str)] = &[
    (
        "reports",
        "ask",
        "what to do when cig finds a bug in itself (a crash, an internal error, a tripwire): ask once and remember, always (file without asking), never (keep reports local only); under CI with nothing set, never",
    ),
    (
        "crash_reports",
        "ask",
        "the older name of `reports`; read when `reports` is not set",
    ),
    (
        "crash_rollback",
        "ask",
        "after a crash mid-run: ask before putting its reversible burns back, always, or never (print the unburn command)",
    ),
    (
        "trial",
        "auto",
        "before a run on a cig this script has not been proved on: auto (compare plans with its last good version when that is in the humidor), always (fetch it if needed), never",
    ),
    (
        "issues_repo",
        "otmof-ops/cig-script",
        "GitHub repository that receives crash reports (owner/name)",
    ),
    (
        "update_channel",
        "stable",
        "which releases `cig update` considers: stable, or prerelease",
    ),
    (
        "update_notice",
        "true",
        "mention a newer release on stderr after `cig doctor` or `cig update --check` found one",
    ),
    (
        "watch_cap",
        "5000000",
        "the memory guard on hop watching: how many entries of a pack it reads before it stops (about 60 bytes each); past it, what a hop creates is irreversible and the hop says so",
    ),
    (
        "hop_skip",
        "",
        "globs every watched hop leaves out of the tin, comma-separated and relative to a pack root (`**/.cache/**, *.log`), on top of each hop's own `skip`: their changes are not kept, so a hop cannot undo them (E310)",
    ),
    (
        "hop_statcache",
        "off",
        "the stat cache (#148): off keeps every hop reading and hashing the tree anyway; on remembers each touched file's (dev, inode, size, mtime, ctime, mode) with the tin object it was stashed under, so a later hop whose files all match the cache reads none of them (restores still re-hash: E704 is unchanged). `cig tin verify --statcache` re-checks it",
    ),
    (
        "seal",
        "secrets",
        "which files the tin keeps sealed (encrypted at rest): secrets (secret-shaped names, a pack root's .cigseal, a hop's touches_sealed), all, or never",
    ),
    (
        "seal_key",
        "keyring",
        "where the tin's key lives: keyring (the OS keyring, with a key file apart from the tin as the fallback where there is none) or file (that key file, readable by you alone)",
    ),
    (
        "tin_perms",
        "strict",
        "what a state directory other users can write does: strict (refuse on a terminal, warn when unattended), warn (always warn), off (do not look)",
    ),
    (
        "key_reminder",
        "on",
        "remind you once, in cig doctor, to back up the tin's key when sealed objects exist and the key was never exported",
    ),
    (
        "tin_guard",
        "auto",
        "the tin read-only to hops, the key folder hidden from them (Linux): auto (boxed hops, wherever the kernel allows; one note where it does not), on (every hop, and refuse to run one unguarded), off",
    ),
    (
        "keep_runs",
        "0",
        "after each run, keep only this many run records, the newest, and prune the rest as cig runs --prune does (recorded in the ledger); 0 keeps every run",
    ),
    (
        "keep_days",
        "0",
        "after each run, prune run records older than this many days, as cig runs --prune does; 0 keeps them however old",
    ),
    (
        "verify_sample",
        "auto",
        "after a run that burned, how many tin objects, drawn at random from the whole tin, are checked against their names beside the run's own: auto (3), a number, or 0 for no check",
    ),
    (
        "journal_sign",
        "on",
        "sign each run's journal (format 3: a keyed chain under the tin's key, plus a ledger line per run) so a script cannot rewrite the record; off writes format 2 for anyone who needs older cigs to read new runs",
    ),
];

#[derive(Clone, Debug, Default)]
pub struct Config {
    values: BTreeMap<String, String>,
}

pub fn path() -> PathBuf {
    crate::burn::runs::home_dir().join("config")
}

impl Config {
    /// Load the config file, tolerating its absence.
    pub fn load() -> Config {
        match fs::read_to_string(path()) {
            Ok(text) => Config::parse(&text),
            Err(_) => Config::default(),
        }
    }

    pub fn parse(text: &str) -> Config {
        let mut values = BTreeMap::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                let k = k.trim();
                let v = v.trim().trim_matches('"').trim_matches('\'');
                if !k.is_empty() {
                    values.insert(k.to_string(), v.to_string());
                }
            }
        }
        Config { values }
    }

    /// The effective value: the file, else the environment variable
    /// `CIG_<KEY>`, else the default.
    pub fn get(&self, key: &str) -> String {
        if let Some(v) = self.values.get(key) {
            return v.clone();
        }
        let env_key = format!("CIG_{}", key.to_ascii_uppercase());
        if let Ok(v) = std::env::var(&env_key) {
            return v;
        }
        KEYS.iter()
            .find(|(k, _, _)| *k == key)
            .map(|(_, d, _)| d.to_string())
            .unwrap_or_default()
    }

    pub fn is_set(&self, key: &str) -> bool {
        self.values.contains_key(key)
    }

    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        let Some((_, _, _)) = KEYS.iter().find(|(k, _, _)| *k == key) else {
            let known: Vec<&str> = KEYS.iter().map(|(k, _, _)| *k).collect();
            return Err(format!(
                "unknown key `{key}`; known keys: {}",
                known.join(", ")
            ));
        };
        match (key, value) {
            ("reports", "ask" | "always" | "never") => {}
            ("reports", other) => {
                return Err(format!(
                    "reports must be ask, always or never, not `{other}`"
                ))
            }
            ("crash_reports", "ask" | "always" | "never") => {}
            ("crash_reports", other) => {
                return Err(format!(
                    "crash_reports must be ask, always or never, not `{other}`"
                ))
            }
            ("crash_rollback", "ask" | "always" | "never") => {}
            ("crash_rollback", other) => {
                return Err(format!(
                    "crash_rollback must be ask, always or never, not `{other}`"
                ))
            }
            ("trial", "auto" | "always" | "never") => {}
            ("trial", other) => {
                return Err(format!(
                    "trial must be auto, always or never, not `{other}`"
                ))
            }
            ("update_channel", "stable" | "prerelease") => {}
            ("update_channel", other) => {
                return Err(format!(
                    "update_channel must be stable or prerelease, not `{other}`"
                ))
            }
            ("update_notice", "true" | "false") => {}
            ("update_notice", other) => {
                return Err(format!(
                    "update_notice must be true or false, not `{other}`"
                ))
            }
            ("seal", "secrets" | "all" | "never") => {}
            ("seal", other) => {
                return Err(format!("seal must be secrets, all or never, not `{other}`"))
            }
            ("seal_key", "keyring" | "file") => {}
            ("seal_key", other) => {
                return Err(format!("seal_key must be keyring or file, not `{other}`"))
            }
            ("hop_skip", v) => {
                for g in crate::burn::tin::skip_setting_globs(v) {
                    if let Err(e) = globset::Glob::new(g.trim_start_matches("./")) {
                        return Err(format!("hop_skip: `{g}` is not a glob: {e}"));
                    }
                }
            }
            ("hop_statcache", "on" | "off") => {}
            ("hop_statcache", other) => {
                return Err(format!("hop_statcache must be on or off, not `{other}`"))
            }
            ("watch_cap", v) if v.parse::<usize>().is_ok_and(|n| n > 0) => {}
            ("watch_cap", other) => {
                return Err(format!(
                    "watch_cap must be a whole number of entries above zero, not `{other}`"
                ))
            }
            ("issues_repo", v) if v.split('/').count() == 2 && !v.contains(char::is_whitespace) => {
            }
            ("issues_repo", other) => {
                return Err(format!(
                    "issues_repo must look like owner/name, not `{other}`"
                ))
            }
            _ => {}
        }
        self.values.insert(key.to_string(), value.to_string());
        Ok(())
    }

    pub fn unset(&mut self, key: &str) -> bool {
        self.values.remove(key).is_some()
    }

    pub fn save(&self) -> io::Result<()> {
        let p = path();
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut text =
            String::from("# CigScript configuration. `cig config` explains every key.\n");
        for (k, v) in &self.values {
            text.push_str(&format!("{k} = {v}\n"));
        }
        let tmp = p.with_extension("tmp");
        fs::write(&tmp, text)?;
        fs::rename(tmp, p)
    }

    pub fn entries(&self) -> impl Iterator<Item = (&str, String, bool)> {
        KEYS.iter()
            .map(|(k, _, _)| (*k, self.get(k), self.is_set(k)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_validates() {
        let c =
            Config::parse("# comment\ncrash_reports = never\n\nissues_repo=\"a/b\"\nbad line\n");
        assert_eq!(c.get("crash_reports"), "never");
        assert_eq!(c.get("issues_repo"), "a/b");
        assert_eq!(c.get("update_channel"), "stable");
        let mut c = Config::default();
        assert!(c.set("crash_reports", "sometimes").is_err());
        assert!(c.set("nope", "x").is_err());
        assert!(c.set("issues_repo", "just-a-name").is_err());
        assert!(c.set("crash_reports", "always").is_ok());
        assert!(c.unset("crash_reports"));
        assert!(!c.unset("crash_reports"));
    }
}
