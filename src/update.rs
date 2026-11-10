// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! Release versions and the release listing, through `gh` or `curl`.
//!
//! Updating cig is `ciginstaller`'s job now (`cig update` is a stub for it),
//! so nothing here replaces the running binary. What stays is what the
//! humidor, pins and the crash tracker read: version parsing and order, the
//! release list, the asset names the installer's manifest also reads, the
//! download and checksum check the humidor uses to keep an older cig.
//!
//! What this trusts: TLS to github.com and the SHA-256 sidecar published
//! with the release. The sidecar proves the download is intact, not who
//! built it; signed releases are on the roadmap and `SECURITY.md` says so.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fs;
use std::path::Path;
use std::process::Command;

pub const DEFAULT_REPO: &str = "otmof-ops/cig-script";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u64, pub u64, pub u64);

impl Version {
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim().trim_start_matches('v');
        let core = s.split(['-', '+']).next()?;
        let mut parts = core.split('.');
        let a = parts.next()?.parse().ok()?;
        let b = parts.next()?.parse().ok()?;
        let c = parts.next().unwrap_or("0").parse().ok()?;
        Some(Version(a, b, c))
    }

    pub fn current() -> Version {
        Version::parse(crate::VERSION).unwrap_or(Version(0, 0, 0))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Release {
    pub tag: String,
    pub prerelease: bool,
    pub url: String,
    pub published_at: String,
    pub assets: Vec<Asset>,
    /// The release notes, read for the issues a release says it fixed.
    #[serde(default)]
    pub body: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Asset {
    pub name: String,
    pub url: String,
}

impl Release {
    pub fn version(&self) -> Option<Version> {
        Version::parse(&self.tag)
    }

    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }
}

/// The asset name this platform expects for a version, e.g.
/// `cig-v2.1.0-linux-x86_64`.
pub fn asset_name(version: &Version) -> String {
    let os = std::env::consts::OS;
    let ext = if std::env::consts::OS == "windows" {
        ".exe"
    } else {
        ""
    };
    format!("cig-v{version}-{os}-{}{ext}", std::env::consts::ARCH)
}

/// The message every "no transport" failure starts with.
pub const NO_TOOL: &str = "neither `gh` nor `curl` is installed";

pub fn has_tool(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| {
            std::env::split_paths(&p)
                .any(|d| d.join(name).is_file() || d.join(format!("{name}.exe")).is_file())
        })
        .unwrap_or(false)
}

fn curl_base() -> Command {
    let mut c = Command::new("curl");
    c.args([
        "-sS",
        "-f",
        "-L",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--tlsv1.2",
        "--retry",
        "3",
        "--max-time",
        "120",
    ]);
    c
}

/// Every published release, newest version first, notes included.
pub fn releases(repo: &str) -> Result<Vec<Release>, String> {
    let text = releases_json(repo)?;
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("releases response is not JSON: {e}"))?;
    let items = v.as_array().ok_or("releases response is not a list")?;
    let mut out: Vec<Release> = items
        .iter()
        .filter(|i| !i["draft"].as_bool().unwrap_or(false))
        .filter_map(|i| pick(&serde_json::Value::Array(vec![i.clone()]).to_string(), true).ok())
        .collect();
    out.sort_by_key(|r| std::cmp::Reverse(r.version()));
    Ok(out)
}

/// Fetch the releases list and pick the newest matching the channel.
pub fn latest(repo: &str, prerelease_ok: bool) -> Result<Release, String> {
    let text = releases_json(repo)?;
    pick(&text, prerelease_ok)
}

/// A specific release by tag (`v2.1.0` or `2.1.0`).
pub fn find(repo: &str, tag: &str) -> Result<Release, String> {
    let wanted = Version::parse(tag).ok_or_else(|| format!("`{tag}` is not a version"))?;
    let text = releases_json(repo)?;
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("releases response is not JSON: {e}"))?;
    let items = v.as_array().ok_or("releases response is not a list")?;
    let one: Vec<serde_json::Value> = items
        .iter()
        .filter(|i| Version::parse(i["tag_name"].as_str().unwrap_or("")) == Some(wanted.clone()))
        .cloned()
        .collect();
    if one.is_empty() {
        return Err(format!("no release tagged {tag} in {repo}"));
    }
    pick(&serde_json::Value::Array(one).to_string(), true)
}

fn releases_json(repo: &str) -> Result<String, String> {
    if let Some(path) = std::env::var_os("CIG_UPDATE_MANIFEST").filter(|p| !p.is_empty()) {
        return fs::read_to_string(&path).map_err(|e| format!("CIG_UPDATE_MANIFEST: {e}"));
    }
    let mut gh_said = None;
    if has_tool("gh") {
        let out = Command::new("gh")
            .args(["api", &format!("repos/{repo}/releases?per_page=20")])
            .output()
            .map_err(|e| format!("gh: {e}"))?;
        if out.status.success() {
            return Ok(String::from_utf8_lossy(&out.stdout).to_string());
        }
        // gh installed but not logged in (CI runners, a fresh machine): the
        // releases of a public repository are readable without it.
        let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if !has_tool("curl") {
            return Err(format!("gh could not read releases for {repo}: {said}"));
        }
        gh_said = Some(said);
    }
    if has_tool("curl") {
        let out = curl_base()
            .args(["-H", "Accept: application/vnd.github+json"])
            .arg(format!(
                "https://api.github.com/repos/{repo}/releases?per_page=20"
            ))
            .output()
            .map_err(|e| format!("curl: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "could not read releases for {repo} (is it public?): {}{}",
                String::from_utf8_lossy(&out.stderr).trim(),
                gh_said
                    .map(|g| format!("; gh said: {g}"))
                    .unwrap_or_default()
            ));
        }
        return Ok(String::from_utf8_lossy(&out.stdout).to_string());
    }
    Err(format!("{NO_TOOL}; install curl or gh"))
}

fn pick(json: &str, prerelease_ok: bool) -> Result<Release, String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("releases response is not JSON: {e}"))?;
    let items = v.as_array().ok_or("releases response is not a list")?;
    let mut best: Option<Release> = None;
    for item in items {
        let prerelease = item["prerelease"].as_bool().unwrap_or(false);
        let draft = item["draft"].as_bool().unwrap_or(false);
        if draft || (prerelease && !prerelease_ok) {
            continue;
        }
        let rel = Release {
            tag: item["tag_name"].as_str().unwrap_or("").to_string(),
            prerelease,
            url: item["html_url"].as_str().unwrap_or("").to_string(),
            published_at: item["published_at"].as_str().unwrap_or("").to_string(),
            body: item["body"].as_str().unwrap_or("").to_string(),
            assets: item["assets"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|x| Asset {
                            name: x["name"].as_str().unwrap_or("").to_string(),
                            url: x["browser_download_url"].as_str().unwrap_or("").to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        };
        let Some(ver) = rel.version() else { continue };
        if best
            .as_ref()
            .and_then(|b| b.version())
            .is_none_or(|bv| ver > bv)
        {
            best = Some(rel);
        }
    }
    best.ok_or_else(|| "no usable release found".to_string())
}

/// Every version the repository has released (drafts and prereleases
/// left out), for the humidor to pick from.
pub fn released(repo: &str) -> Result<Vec<Version>, String> {
    let text = releases_json(repo)?;
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("releases response is not JSON: {e}"))?;
    let items = v.as_array().ok_or("releases response is not a list")?;
    Ok(items
        .iter()
        .filter(|i| {
            !i["draft"].as_bool().unwrap_or(false) && !i["prerelease"].as_bool().unwrap_or(false)
        })
        .filter_map(|i| Version::parse(i["tag_name"].as_str().unwrap_or("")))
        .collect())
}

pub fn compare(latest: &Version) -> Ordering {
    latest.cmp(&Version::current())
}

/// Download `url` to `dest` with curl or gh.
pub fn download(url: &str, dest: &Path) -> Result<(), String> {
    if let Some(local) = url.strip_prefix("file://") {
        return fs::copy(local, dest)
            .map(|_| ())
            .map_err(|e| format!("copy {local}: {e}"));
    }
    if has_tool("curl") {
        let out = curl_base()
            .arg("-o")
            .arg(dest)
            .arg(url)
            .output()
            .map_err(|e| format!("curl: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "download failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        return Ok(());
    }
    if has_tool("gh") {
        // gh can fetch release assets from private repos the user can access.
        let (repo, tag, name) = parse_asset_url(url).ok_or("unrecognised asset url")?;
        let dir = dest.parent().ok_or("bad destination")?;
        let out = Command::new("gh")
            .args([
                "release",
                "download",
                &tag,
                "-R",
                &repo,
                "--pattern",
                &name,
                "--clobber",
                "--dir",
            ])
            .arg(dir)
            .output()
            .map_err(|e| format!("gh: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "gh release download failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        fs::rename(dir.join(&name), dest).map_err(|e| e.to_string())?;
        return Ok(());
    }
    Err(NO_TOOL.to_string())
}

fn parse_asset_url(url: &str) -> Option<(String, String, String)> {
    // https://github.com/<owner>/<repo>/releases/download/<tag>/<name>
    let rest = url.strip_prefix("https://github.com/")?;
    let parts: Vec<&str> = rest.split('/').collect();
    if parts.len() < 6 || parts[2] != "releases" || parts[3] != "download" {
        return None;
    }
    Some((
        format!("{}/{}", parts[0], parts[1]),
        parts[4].to_string(),
        parts[5..].join("/"),
    ))
}

pub fn verify_sha256(file: &Path, sidecar_text: &str) -> Result<(), String> {
    let expected = sidecar_text
        .split_whitespace()
        .next()
        .ok_or("empty checksum file")?
        .to_ascii_lowercase();
    let actual = crate::burn::journal::sha256_file(file).map_err(|e| e.to_string())?;
    if actual != expected {
        return Err(format!(
            "checksum mismatch: expected {expected}, got {actual}"
        ));
    }
    Ok(())
}

/// Remove a `cig.exe.old` that an update before the installer left behind.
pub fn cleanup_old() {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let _ = fs::remove_file(dir.join("cig.exe.old"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_parse_and_order() {
        assert_eq!(Version::parse("v2.1.0"), Some(Version(2, 1, 0)));
        assert_eq!(Version::parse("2.10"), Some(Version(2, 10, 0)));
        assert_eq!(Version::parse("3.0.0-rc.1"), Some(Version(3, 0, 0)));
        assert!(Version::parse("nope").is_none());
        assert!(Version(2, 1, 1) > Version(2, 1, 0));
        assert!(Version(2, 10, 0) > Version(2, 9, 9));
    }

    #[test]
    fn picks_newest_stable_release() {
        let json = r#"[
          {"tag_name":"v2.2.0-rc1","prerelease":true,"draft":false,"html_url":"u1","published_at":"","assets":[]},
          {"tag_name":"v2.1.0","prerelease":false,"draft":false,"html_url":"u2","published_at":"","assets":[{"name":"a","browser_download_url":"b"}]},
          {"tag_name":"v2.0.1","prerelease":false,"draft":false,"html_url":"u3","published_at":"","assets":[]}
        ]"#;
        assert_eq!(pick(json, false).unwrap().tag, "v2.1.0");
        assert_eq!(pick(json, true).unwrap().tag, "v2.2.0-rc1");
        assert_eq!(
            parse_asset_url(
                "https://github.com/o/r/releases/download/v1.0.0/cig-v1.0.0-linux-x86_64"
            )
            .unwrap()
            .2,
            "cig-v1.0.0-linux-x86_64"
        );
    }
}
