// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The inbox: the one place under the state directory a guarded hop can
//! write. A cig started inside the hop has `inbox/<run>/home` as its state
//! root and `inbox/<run>/keys` as its key folder, so it runs as it would
//! anywhere; when the hop ends, its runs are taken in whole as children of
//! the run whose hop it was, and the inbox goes. A child's objects move
//! into this tin, checked against their names on the way; its sealed
//! objects are re-sealed under this machine's key; its journal's paths are
//! relocated and the journal re-signed. A child then unburns like any run,
//! and the parent's rollback undoes it through the burn protocol, as a
//! nested run always was.

use std::fs;
use std::path::{Path, PathBuf};

use super::overlay::dir_name;
use super::runs::RunRecord;
use super::{journal, ledger, seal, tin};

/// What the take-in found: the children now recorded, and what was
/// dropped, with why.
#[derive(Debug, Default)]
pub struct TakenIn {
    pub children: Vec<String>,
    pub notes: Vec<String>,
}

/// `YYYYMMDDTHHMMSS-xxxxxx`: the shape of a run id, and nothing else.
fn plausible_id(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 22
        && b[8] == b'T'
        && b[15] == b'-'
        && b[..8].iter().chain(&b[9..15]).all(u8::is_ascii_digit)
        && b[16..].iter().all(u8::is_ascii_hexdigit)
}

/// Take a run's inbox in: each run under it becomes a child of this run,
/// whole; whatever is not a run, or whose objects do not match their
/// names, is dropped with a note. The inbox is removed either way.
pub fn take_in(outer_run_dir: &Path) -> TakenIn {
    let mut out = TakenIn::default();
    let inbox = super::overlay::inbox_dir(outer_run_dir);
    if !inbox.is_dir() {
        return out;
    }
    let outer = dir_name(outer_run_dir);
    let from_key = seal::key_in_file(&inbox.join("keys").join("seal.key"));
    if let Ok(rd) = fs::read_dir(inbox.join("home").join("runs")) {
        let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        entries.sort();
        for dir in entries {
            let id = dir_name(&dir);
            match take_one(&dir, &id, &outer, &inbox, from_key.as_ref()) {
                Ok(()) => out.children.push(id),
                Err(why) => out.notes.push(format!(
                    "the inbox of run {outer} held `{id}`, which is not a run of a cig inside the hop ({why}); dropped"
                )),
            }
        }
    }
    let _ = super::overlay::remove_tree_force(&inbox);
    out
}

fn take_one(
    dir: &Path,
    id: &str,
    outer: &str,
    inbox: &Path,
    from_key: Option<&seal::Key>,
) -> Result<(), String> {
    if !plausible_id(id) {
        return Err("not a run id".to_string());
    }
    let mut rec = RunRecord::load(dir).map_err(|e| format!("run.json: {e}"))?;
    if rec.id != id {
        return Err("run.json names another run".to_string());
    }
    let dest = super::runs::runs_dir().join(id);
    if dest.exists() {
        return Err("a run of that id already exists".to_string());
    }
    // Every path under the inbox's home becomes the same path under this
    // one: the journal's snapshots, recipes and sealed objects.
    let inbox_home = inbox.join("home");
    let real_home = super::runs::home_dir();
    let real_home = std::path::absolute(&real_home).unwrap_or(real_home);
    let journal_path = dir.join("journal.jsonl");
    if journal_path.is_file() {
        let text = fs::read_to_string(&journal_path).map_err(|e| e.to_string())?;
        let mut out = String::new();
        for (n, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let mut v: serde_json::Value = serde_json::from_str(line)
                .map_err(|_| format!("journal line {} is not JSON", n + 1))?;
            relocate(
                &mut v,
                &inbox_home.to_string_lossy(),
                &real_home.to_string_lossy(),
            );
            out.push_str(&serde_json::to_string(&v).map_err(|e| e.to_string())?);
            out.push('\n');
        }
        fs::write(&journal_path, out).map_err(|e| e.to_string())?;
    }
    // The child's tin into this one: objects by their names, recipes as
    // they are (a restore checks every hash), sealed objects re-sealed.
    let child_tin = inbox_home.join("tin");
    move_objects(&child_tin.join("objects"), &tin::objects_root(), true)?;
    move_objects(&child_tin.join("recipes"), &tin::recipes_root(), false)?;
    move_sealed(&child_tin.join("sealed"), &tin::sealed_dir(), from_key)?;
    // The run directory itself, snapshots included.
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::rename(dir, &dest).map_err(|e| format!("could not move the run in: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dest, fs::Permissions::from_mode(0o700));
    }
    reseal_snapshots(&dest.join("snapshots"), from_key)?;
    // Under this machine's key now, like any run recorded here.
    let head = if dest.join("journal.jsonl").is_file() {
        journal::resign_in_place(&dest).map_err(|e| e.to_string())?
    } else {
        None
    };
    // A child the cig inside took in itself keeps its own parent.
    if rec.parent.is_none() {
        rec.parent = Some(outer.to_string());
    }
    if head.is_some() {
        rec.journal_head = head.clone();
    }
    rec.loaded_from = Some(dest.clone());
    rec.save().map_err(|e| e.to_string())?;
    if let Some(h) = &head {
        let lines = journal::Journal::load(&dest)
            .map(|j| j.chain().lines as u64)
            .unwrap_or(0);
        let _ = ledger::append("run", id, h, &rec.status, lines);
    }
    Ok(())
}

/// Every string under `v` that names a path under `from` now names the
/// same path under `to`.
fn relocate(v: &mut serde_json::Value, from: &str, to: &str) {
    match v {
        serde_json::Value::String(s) => {
            if s == from {
                *s = to.to_string();
            } else if let Some(rest) = s.strip_prefix(from) {
                if rest.starts_with('/') || rest.starts_with('\\') {
                    *s = format!("{to}{rest}");
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                relocate(item, from, to);
            }
        }
        serde_json::Value::Object(map) => {
            for (_, item) in map.iter_mut() {
                relocate(item, from, to);
            }
        }
        _ => {}
    }
}

/// `<xx>/<name>` files under `src` into the same places under `dst`. An
/// object whose bytes do not hash to its name is a poisoned inbox: the
/// whole run is refused. One already here stays as it is.
fn move_objects(src: &Path, dst: &Path, verify: bool) -> Result<(), String> {
    let Ok(buckets) = fs::read_dir(src) else {
        return Ok(());
    };
    for bucket in buckets.flatten() {
        let Ok(files) = fs::read_dir(bucket.path()) else {
            continue;
        };
        for f in files.flatten() {
            let from = f.path();
            let name = dir_name(&from);
            let to = dst.join(dir_name(&bucket.path())).join(&name);
            if to.exists() {
                let _ = fs::remove_file(&from);
                continue;
            }
            if verify {
                let bytes = fs::read(&from).map_err(|e| e.to_string())?;
                let actual = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&bytes));
                if actual != name {
                    return Err(format!(
                        "object {name} does not hash to its name (sha256 {})",
                        &actual[..12]
                    ));
                }
            }
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            fs::rename(&from, &to).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Sealed objects under `src` into `dst`, each re-sealed under this
/// machine's key (they were sealed under the inbox's own).
fn move_sealed(src: &Path, dst: &Path, from_key: Option<&seal::Key>) -> Result<(), String> {
    let Ok(buckets) = fs::read_dir(src) else {
        return Ok(());
    };
    for bucket in buckets.flatten() {
        let Ok(files) = fs::read_dir(bucket.path()) else {
            continue;
        };
        for f in files.flatten() {
            let from = f.path();
            let to = dst.join(dir_name(&bucket.path())).join(dir_name(&from));
            if to.exists() {
                let _ = fs::remove_file(&from);
                continue;
            }
            let Some(key) = from_key else {
                return Err("a sealed object with no key to open it".to_string());
            };
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            seal::reseal(&from, &to, &seal::aad_of(&from), key)
                .map_err(|e| format!("a sealed object could not be re-sealed: {e}"))?;
            let _ = fs::remove_file(&from);
        }
    }
    Ok(())
}

/// The sealed snapshots in a run's own folder, re-sealed in place.
fn reseal_snapshots(dir: &Path, from_key: Option<&seal::Key>) -> Result<(), String> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Ok(());
    };
    for f in rd.flatten() {
        let p = f.path();
        if !p.to_string_lossy().ends_with(seal::SUFFIX) {
            continue;
        }
        let Some(key) = from_key else {
            return Err("a sealed snapshot with no key to open it".to_string());
        };
        seal::reseal(&p, &p, &seal::aad_of(&p), key)
            .map_err(|e| format!("a sealed snapshot could not be re-sealed: {e}"))?;
    }
    Ok(())
}
