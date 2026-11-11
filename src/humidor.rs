// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The humidor: every version of cig, kept fresh and ready to light.
//!
//! `~/.cigscript/humidor/<version>/cig` holds a release binary, fetched on
//! demand from the releases page and verified against its published
//! checksum before it is ever run. A pinned script is handed to the version
//! it asks for; a journal is put back by the cig that wrote it. Nothing in
//! here runs a binary whose hash was not checked.

use crate::update::{self, Version};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub fn dir() -> PathBuf {
    crate::burn::runs::home_dir().join("humidor")
}

fn index_path() -> PathBuf {
    dir().join("index.json")
}

/// What the humidor knows about one version.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Entry {
    pub version: String,
    /// Where it came from: a release tag, or `local` for one put there by hand.
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub fetched_at: String,
    #[serde(default)]
    pub last_lit: Option<String>,
    /// `verified` by checksum, `attested` by the release workflow's signature
    /// as well, or `unverified` for a binary put there by hand.
    #[serde(default)]
    pub trust: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Index {
    #[serde(default)]
    entries: Vec<Entry>,
}

fn load_index() -> Index {
    fs::read_to_string(index_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_index(index: &Index) -> io::Result<()> {
    fs::create_dir_all(dir())?;
    let text = serde_json::to_string_pretty(index)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let tmp = index_path().with_extension("json.tmp");
    fs::write(&tmp, text)?;
    fs::rename(tmp, index_path())
}

fn binary_name() -> &'static str {
    if cfg!(windows) {
        "cig.exe"
    } else {
        "cig"
    }
}

/// The binary for a version, when the humidor holds it.
pub fn binary(v: &Version) -> Option<PathBuf> {
    let p = dir().join(v.to_string()).join(binary_name());
    p.is_file().then_some(p)
}

/// Every version on the shelf, newest first: the directories that hold a
/// binary, with what the index knows about each.
pub fn list() -> Vec<(Version, Entry)> {
    list_in(&dir())
}

fn list_in(humidor: &Path) -> Vec<(Version, Entry)> {
    let index: Index = fs::read_to_string(humidor.join("index.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(humidor) else {
        return out;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(v) = Version::parse(&name) else {
            continue;
        };
        if !e.path().join(binary_name()).is_file() {
            continue;
        }
        let entry = index
            .entries
            .iter()
            .find(|x| Version::parse(&x.version) == Some(v.clone()))
            .cloned()
            .unwrap_or_else(|| Entry {
                version: v.to_string(),
                source: "local".to_string(),
                trust: "unverified".to_string(),
                ..Default::default()
            });
        out.push((v, entry));
    }
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out
}

/// The newest version on the shelf that satisfies the constraint. A range
/// never picks a yanked version; an exact pin to one is honoured, and the
/// caller warns.
pub fn best_for(c: &crate::pin::Constraint, yanked: &[Version]) -> Option<(Version, PathBuf)> {
    best_in(&dir(), c, yanked)
}

fn best_in(
    humidor: &Path,
    c: &crate::pin::Constraint,
    yanked: &[Version],
) -> Option<(Version, PathBuf)> {
    list_in(humidor)
        .into_iter()
        .filter(|(v, _)| c.satisfied_by(v))
        .find(|(v, _)| c.is_exact() || !yanked.contains(v))
        .map(|(v, _)| {
            let p = humidor.join(v.to_string()).join(binary_name());
            (v, p)
        })
}

/// Record that a version was just lit.
pub fn touch(v: &Version) {
    let mut index = load_index();
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    match index
        .entries
        .iter_mut()
        .find(|x| Version::parse(&x.version) == Some(v.clone()))
    {
        Some(e) => e.last_lit = Some(now),
        None => index.entries.push(Entry {
            version: v.to_string(),
            source: "local".to_string(),
            trust: "unverified".to_string(),
            last_lit: Some(now),
            ..Default::default()
        }),
    }
    let _ = save_index(&index);
}

/// Fetch a release into the humidor: the asset for this platform and its
/// checksum, verified before the binary is kept, and attested through `gh`
/// when it is there. Returns the entry.
pub fn add(repo: &str, version: &Version) -> Result<Entry, String> {
    let release = update::find(repo, &format!("v{version}"))?;
    let name = update::asset_name(version);
    let Some(asset) = release.asset(&name) else {
        return Err(format!(
            "release {} has no asset named {name} for this platform; build it: cargo install --git https://github.com/{repo} --tag {} --root {}",
            release.tag,
            release.tag,
            dir().join(version.to_string()).display()
        ));
    };
    let Some(sidecar) = release.asset(&format!("{name}.sha256")) else {
        return Err(format!(
            "release {} ships no checksum for {name}; refusing to keep an unverifiable binary",
            release.tag
        ));
    };
    let tmp = crate::burn::runs::home_dir().join("tmp");
    fs::create_dir_all(&tmp).map_err(|e| format!("cannot create {}: {e}", tmp.display()))?;
    let bin = tmp.join(&name);
    let sum = tmp.join(format!("{name}.sha256"));
    update::download(&asset.url, &bin)?;
    update::download(&sidecar.url, &sum)?;
    let sidecar_text = fs::read_to_string(&sum).unwrap_or_default();
    if let Err(e) = update::verify_sha256(&bin, &sidecar_text) {
        let _ = fs::remove_file(&bin);
        let _ = fs::remove_file(&sum);
        return Err(format!("{e}; nothing was kept"));
    }
    let sha256 = crate::burn::journal::sha256_file(&bin).map_err(|e| e.to_string())?;
    let trust = if attest(repo, &bin) {
        "attested"
    } else {
        "verified"
    };
    let home = dir().join(version.to_string());
    fs::create_dir_all(&home).map_err(|e| format!("cannot create {}: {e}", home.display()))?;
    let dest = home.join(binary_name());
    let staged = home.join(".cig.new");
    fs::copy(&bin, &staged).map_err(|e| format!("cannot copy into the humidor: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }
    fs::rename(&staged, &dest).map_err(|e| format!("cannot place the binary: {e}"))?;
    let _ = fs::remove_file(&bin);
    let _ = fs::remove_file(&sum);
    let entry = Entry {
        version: version.to_string(),
        source: release.tag.clone(),
        sha256,
        fetched_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        last_lit: None,
        trust: trust.to_string(),
    };
    let mut index = load_index();
    index
        .entries
        .retain(|x| Version::parse(&x.version) != Some(version.clone()));
    index.entries.push(entry.clone());
    save_index(&index).map_err(|e| e.to_string())?;
    Ok(entry)
}

/// `gh attestation verify`: proof the binary came from this repository's
/// release workflow, not only that the download was complete. Only when
/// `gh` is there; the checksum is the floor.
pub fn attest(repo: &str, bin: &Path) -> bool {
    if std::env::var_os("CIG_UPDATE_MANIFEST").is_some() || !update::has_tool("gh") {
        return false;
    }
    std::process::Command::new("gh")
        .args(["attestation", "verify"])
        .arg(bin)
        .args(["-R", repo])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Remove one version from the shelf.
pub fn remove(v: &Version) -> io::Result<()> {
    let home = dir().join(v.to_string());
    if home.is_dir() {
        fs::remove_dir_all(&home)?;
    }
    let mut index = load_index();
    index
        .entries
        .retain(|x| Version::parse(&x.version) != Some(v.clone()));
    save_index(&index)
}

/// Keep the newest `keep` versions, remove the rest. Returns what went.
pub fn prune(keep: usize) -> Vec<Version> {
    let mut gone = Vec::new();
    for (v, _) in list().into_iter().skip(keep) {
        if remove(&v).is_ok() {
            gone.push(v);
        }
    }
    gone
}

/// Versions the repository has marked as not to be picked: the `YANKED`
/// file at the repository root, one version per line with its reason after
/// a space or a `#`. Read from the network at most once a day and cached;
/// `CIG_YANKED_FILE` points at a local copy.
pub fn yanked(repo: &str) -> Vec<Version> {
    let text = if let Some(p) = std::env::var_os("CIG_YANKED_FILE") {
        fs::read_to_string(p).unwrap_or_default()
    } else {
        let cache = dir().join("yanked.txt");
        let fresh = fs::metadata(&cache)
            .and_then(|m| m.modified())
            .map(|t| t.elapsed().map(|d| d.as_secs() < 86_400).unwrap_or(false))
            .unwrap_or(false);
        if fresh {
            fs::read_to_string(&cache).unwrap_or_default()
        } else {
            let fetched = fetch_text(&format!(
                "https://raw.githubusercontent.com/{repo}/main/YANKED"
            ));
            match fetched {
                Some(t) => {
                    let _ = fs::create_dir_all(dir());
                    let _ = fs::write(&cache, &t);
                    t
                }
                None => fs::read_to_string(&cache).unwrap_or_default(),
            }
        }
    };
    parse_yanked(&text)
}

pub fn parse_yanked(text: &str) -> Vec<Version> {
    text.lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty())
        .filter_map(|l| Version::parse(l.split_whitespace().next().unwrap_or("")))
        .collect()
}

fn fetch_text(url: &str) -> Option<String> {
    if !update::has_tool("curl") {
        return None;
    }
    let out = std::process::Command::new("curl")
        .args([
            "-sS",
            "-f",
            "-L",
            "--proto",
            "=https",
            "--tlsv1.2",
            "--max-time",
            "10",
        ])
        .arg(url)
        .output()
        .ok()?;
    if !out.status.success() {
        // A repository without a YANKED file yanked nothing.
        return Some(String::new());
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Hand the whole invocation to another cig: same arguments, same state
/// directory, `CIG_DISPATCHED=1` so it never dispatches again. Returns the
/// exit status it ended with.
pub fn dispatch(bin: &Path, args: &[std::ffi::OsString]) -> Result<i32, String> {
    let status = std::process::Command::new(bin)
        .args(args)
        .env("CIG_DISPATCHED", "1")
        .status()
        .map_err(|e| format!("cannot start {}: {e}", bin.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return Ok(128 + sig);
        }
    }
    Ok(status.code().unwrap_or(70))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pin::Constraint;

    #[test]
    fn yanked_lines_parse_with_reasons() {
        let v = parse_yanked(
            "# the list\n1.1.2 journals a directory twice\n2.0.0 # bad\n\nnot-a-version\n",
        );
        assert_eq!(v, vec![Version(1, 1, 2), Version(2, 0, 0)]);
    }

    #[test]
    fn a_yanked_version_is_never_picked_for_a_range() {
        let dir = tempfile::tempdir().unwrap();
        let humidor = dir.path().join("humidor");
        for v in ["1.1.1", "1.1.2"] {
            let d = humidor.join(v);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join(binary_name()), b"not really").unwrap();
        }
        let yanked = vec![Version(1, 1, 2)];
        let (picked, _) = best_in(&humidor, &Constraint::parse("1.1").unwrap(), &yanked).unwrap();
        assert_eq!(picked, Version(1, 1, 1), "the range skips the yanked one");
        let (picked, _) = best_in(&humidor, &Constraint::parse("1.1").unwrap(), &[]).unwrap();
        assert_eq!(picked, Version(1, 1, 2), "without a yank the newest wins");
        let (picked, _) = best_in(&humidor, &Constraint::parse("1.1.2").unwrap(), &yanked).unwrap();
        assert_eq!(
            picked,
            Version(1, 1, 2),
            "an exact pin is honoured; the caller warns"
        );
        assert!(best_in(&humidor, &Constraint::parse("1.2").unwrap(), &[]).is_none());
        assert_eq!(list_in(&humidor)[0].1.trust, "unverified");
    }
}
