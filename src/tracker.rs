// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The issue tracker, behind one small interface: find the issue for a
//! fingerprint, open one, edit its body, comment on it, read its state.
//! Three ways to reach GitHub, tried in order: `gh` with the person's own
//! credentials, `curl` with `CIG_GITHUB_TOKEN` (always with
//! `Content-Type: application/json`), and none, in which case the caller
//! hands back a prefilled link. A fourth, for the tests, is a directory of
//! JSON files standing in for the tracker (`CIG_INTERNAL_TRACKER_DIR`), with
//! `CIG_INTERNAL_OFFLINE` making every call fail the way a lost network does.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// One issue, as much of it as the pipeline reads.
#[derive(Clone, Debug, Default)]
pub struct IssueRef {
    pub number: u64,
    pub url: String,
    pub title: String,
    pub body: String,
    /// `open` or `closed`.
    pub state: String,
    pub closed_at: Option<String>,
}

pub trait Tracker {
    fn name(&self) -> &'static str;
    fn find(&self, repo: &str, fingerprint: &str) -> Result<Option<IssueRef>, String>;
    fn create(
        &self,
        repo: &str,
        title: &str,
        body: &str,
        labels: &[String],
    ) -> Result<IssueRef, String>;
    fn edit_body(&self, repo: &str, number: u64, body: &str) -> Result<(), String>;
    fn comment(&self, repo: &str, number: u64, text: &str) -> Result<(), String>;
    fn get(&self, repo: &str, number: u64) -> Result<IssueRef, String>;
}

fn which(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| {
            std::env::split_paths(&p)
                .any(|d| d.join(name).is_file() || d.join(format!("{name}.exe")).is_file())
        })
        .unwrap_or(false)
}

/// The tracker this machine can reach, or `None` when nothing can file an
/// issue from here (the caller gives a link to open by hand).
pub fn pick() -> Option<Box<dyn Tracker>> {
    if let Some(dir) = std::env::var_os("CIG_INTERNAL_TRACKER_DIR").filter(|d| !d.is_empty()) {
        return Some(Box::new(FileTracker {
            dir: PathBuf::from(dir),
        }));
    }
    if which("gh") {
        return Some(Box::new(Gh));
    }
    if std::env::var("CIG_GITHUB_TOKEN").is_ok_and(|t| !t.is_empty()) && which("curl") {
        return Some(Box::new(Curl));
    }
    None
}

/// Why nothing can be filed from here, for the message beside the link.
pub fn why_none() -> &'static str {
    if std::env::var("CIG_GITHUB_TOKEN").is_ok_and(|t| !t.is_empty()) {
        "no `gh`, and CIG_GITHUB_TOKEN is set but `curl` is not on PATH"
    } else {
        "no `gh` and no CIG_GITHUB_TOKEN"
    }
}

fn offline() -> Result<(), String> {
    if std::env::var_os("CIG_INTERNAL_OFFLINE").is_some_and(|v| !v.is_empty() && v != "0") {
        return Err("the network is unreachable (CIG_INTERNAL_OFFLINE is set)".to_string());
    }
    Ok(())
}

// ----- gh --------------------------------------------------------------------------

pub struct Gh;

fn gh(args: &[&str]) -> Result<String, String> {
    let out = Command::new("gh")
        .args(args)
        .output()
        .map_err(|e| format!("gh: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if err.contains("Could not resolve") || err.contains("404") {
            Err(format!(
                "gh cannot reach the repository; if it is private you need access to file issues ({err})"
            ))
        } else {
            Err(format!("gh {}: {err}", args.first().copied().unwrap_or("")))
        }
    }
}

fn issue_from_gh_json(v: &serde_json::Value) -> IssueRef {
    IssueRef {
        number: v["number"].as_u64().unwrap_or(0),
        url: v["url"].as_str().unwrap_or("").to_string(),
        title: v["title"].as_str().unwrap_or("").to_string(),
        body: v["body"].as_str().unwrap_or("").to_string(),
        state: v["state"].as_str().unwrap_or("").to_ascii_lowercase(),
        closed_at: v["closedAt"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    }
}

impl Tracker for Gh {
    fn name(&self) -> &'static str {
        "gh"
    }
    fn find(&self, repo: &str, fingerprint: &str) -> Result<Option<IssueRef>, String> {
        offline()?;
        let text = gh(&[
            "issue",
            "list",
            "-R",
            repo,
            "--state",
            "all",
            "--search",
            &format!("[{fingerprint}] in:title"),
            "--json",
            "number,url,title,body,state,closedAt",
            "--limit",
            "5",
        ])?;
        let items: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap_or_default();
        Ok(items
            .iter()
            .map(issue_from_gh_json)
            .find(|i| i.title.contains(&format!("[{fingerprint}]"))))
    }
    fn create(
        &self,
        repo: &str,
        title: &str,
        body: &str,
        labels: &[String],
    ) -> Result<IssueRef, String> {
        offline()?;
        let tmp = std::env::temp_dir().join(format!("cig-issue-{}.md", std::process::id()));
        fs::write(&tmp, body).map_err(|e| e.to_string())?;
        let tmp_s = tmp.to_string_lossy().to_string();
        let mut args = vec![
            "issue",
            "create",
            "-R",
            repo,
            "--title",
            title,
            "--body-file",
            &tmp_s,
        ];
        let joined = labels.join(",");
        if !labels.is_empty() {
            args.push("--label");
            args.push(&joined);
        }
        // A label the repository lacks fails the whole create: file without.
        let url = match gh(&args) {
            Ok(u) => u,
            Err(e) if !labels.is_empty() && e.contains("label") => gh(&args[..8])?,
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                return Err(e);
            }
        };
        let _ = fs::remove_file(&tmp);
        let number = url
            .rsplit('/')
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        Ok(IssueRef {
            number,
            url,
            title: title.to_string(),
            body: body.to_string(),
            state: "open".to_string(),
            closed_at: None,
        })
    }
    fn edit_body(&self, repo: &str, number: u64, body: &str) -> Result<(), String> {
        offline()?;
        let tmp = std::env::temp_dir().join(format!("cig-issue-{}-edit.md", std::process::id()));
        fs::write(&tmp, body).map_err(|e| e.to_string())?;
        let r = gh(&[
            "issue",
            "edit",
            &number.to_string(),
            "-R",
            repo,
            "--body-file",
            &tmp.to_string_lossy(),
        ]);
        let _ = fs::remove_file(&tmp);
        r.map(|_| ())
    }
    fn comment(&self, repo: &str, number: u64, text: &str) -> Result<(), String> {
        offline()?;
        gh(&[
            "issue",
            "comment",
            &number.to_string(),
            "-R",
            repo,
            "--body",
            text,
        ])
        .map(|_| ())
    }
    fn get(&self, repo: &str, number: u64) -> Result<IssueRef, String> {
        offline()?;
        let text = gh(&[
            "issue",
            "view",
            &number.to_string(),
            "-R",
            repo,
            "--json",
            "number,url,title,body,state,closedAt",
        ])?;
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        Ok(issue_from_gh_json(&v))
    }
}

// ----- curl with a token ----------------------------------------------------------------

pub struct Curl;

fn curl_api(method: &str, url: &str, payload: Option<&str>) -> Result<serde_json::Value, String> {
    offline()?;
    let token = std::env::var("CIG_GITHUB_TOKEN").map_err(|_| "no token".to_string())?;
    // The token travels in a config file, never on the command line.
    let cfg = std::env::temp_dir().join(format!("cig-curl-{}.cfg", std::process::id()));
    fs::write(
        &cfg,
        format!(
            "header = \"Authorization: Bearer {token}\"\nheader = \"Accept: application/vnd.github+json\"\nheader = \"Content-Type: application/json\"\n"
        ),
    )
    .map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&cfg, fs::Permissions::from_mode(0o600));
    }
    let mut c = Command::new("curl");
    c.args([
        "-sS",
        "-f",
        "--proto",
        "=https",
        "--tlsv1.2",
        "--max-time",
        "30",
        "-K",
    ])
    .arg(&cfg)
    .args(["-X", method]);
    if let Some(p) = payload {
        c.args(["-d", p]);
    }
    c.arg(url);
    let out = c.output();
    let _ = fs::remove_file(&cfg);
    let out = out.map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "curl failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    if out.stdout.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())
}

fn issue_from_api(v: &serde_json::Value) -> IssueRef {
    IssueRef {
        number: v["number"].as_u64().unwrap_or(0),
        url: v["html_url"].as_str().unwrap_or("").to_string(),
        title: v["title"].as_str().unwrap_or("").to_string(),
        body: v["body"].as_str().unwrap_or("").to_string(),
        state: v["state"].as_str().unwrap_or("").to_ascii_lowercase(),
        closed_at: v["closed_at"].as_str().map(str::to_string),
    }
}

/// The issue search that finds a report already filed. GitHub refuses a
/// search that names neither `is:issue` nor `is:pull-request` (422), so
/// the curl route never filed anything without it (#126); `gh issue list`
/// adds it itself.
fn search_query(repo: &str, fingerprint: &str) -> String {
    format!("repo:{repo} \"[{fingerprint}]\" in:title is:issue")
}

impl Tracker for Curl {
    fn name(&self) -> &'static str {
        "curl"
    }
    fn find(&self, repo: &str, fingerprint: &str) -> Result<Option<IssueRef>, String> {
        let q = crate::crash::urlencode(&search_query(repo, fingerprint));
        let v = curl_api(
            "GET",
            &format!("https://api.github.com/search/issues?q={q}&per_page=5"),
            None,
        )?;
        Ok(v["items"]
            .as_array()
            .into_iter()
            .flatten()
            .map(issue_from_api)
            .find(|i| i.title.contains(&format!("[{fingerprint}]"))))
    }
    fn create(
        &self,
        repo: &str,
        title: &str,
        body: &str,
        labels: &[String],
    ) -> Result<IssueRef, String> {
        let payload =
            serde_json::json!({"title": title, "body": body, "labels": labels}).to_string();
        let v = match curl_api(
            "POST",
            &format!("https://api.github.com/repos/{repo}/issues"),
            Some(&payload),
        ) {
            Ok(v) => v,
            Err(_) if !labels.is_empty() => {
                let bare = serde_json::json!({"title": title, "body": body}).to_string();
                curl_api(
                    "POST",
                    &format!("https://api.github.com/repos/{repo}/issues"),
                    Some(&bare),
                )?
            }
            Err(e) => return Err(e),
        };
        Ok(issue_from_api(&v))
    }
    fn edit_body(&self, repo: &str, number: u64, body: &str) -> Result<(), String> {
        let payload = serde_json::json!({"body": body}).to_string();
        curl_api(
            "PATCH",
            &format!("https://api.github.com/repos/{repo}/issues/{number}"),
            Some(&payload),
        )
        .map(|_| ())
    }
    fn comment(&self, repo: &str, number: u64, text: &str) -> Result<(), String> {
        let payload = serde_json::json!({"body": text}).to_string();
        curl_api(
            "POST",
            &format!("https://api.github.com/repos/{repo}/issues/{number}/comments"),
            Some(&payload),
        )
        .map(|_| ())
    }
    fn get(&self, repo: &str, number: u64) -> Result<IssueRef, String> {
        let v = curl_api(
            "GET",
            &format!("https://api.github.com/repos/{repo}/issues/{number}"),
            None,
        )?;
        Ok(issue_from_api(&v))
    }
}

// ----- a directory, for the tests ---------------------------------------------------------

/// Issues as JSON files under `<dir>/<owner>__<name>/<number>.json`. The
/// suite reads them back to see what was filed, and `cig` behaves exactly as
/// it would against GitHub.
pub struct FileTracker {
    pub dir: PathBuf,
}

impl FileTracker {
    fn repo_dir(&self, repo: &str) -> PathBuf {
        self.dir.join(repo.replace('/', "__"))
    }
    fn path(&self, repo: &str, number: u64) -> PathBuf {
        self.repo_dir(repo).join(format!("{number}.json"))
    }
    fn read(&self, path: &std::path::Path) -> Result<serde_json::Value, String> {
        let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
        serde_json::from_str(&text).map_err(|e| e.to_string())
    }
    fn write(&self, path: &std::path::Path, v: &serde_json::Value) -> Result<(), String> {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        fs::write(path, serde_json::to_string_pretty(v).unwrap_or_default())
            .map_err(|e| e.to_string())
    }
    fn issue_of(v: &serde_json::Value) -> IssueRef {
        IssueRef {
            number: v["number"].as_u64().unwrap_or(0),
            url: v["url"].as_str().unwrap_or("").to_string(),
            title: v["title"].as_str().unwrap_or("").to_string(),
            body: v["body"].as_str().unwrap_or("").to_string(),
            state: v["state"].as_str().unwrap_or("open").to_string(),
            closed_at: v["closed_at"].as_str().map(str::to_string),
        }
    }
    fn all(&self, repo: &str) -> Result<Vec<serde_json::Value>, String> {
        let mut out = Vec::new();
        let dir = self.repo_dir(repo);
        let Ok(entries) = fs::read_dir(&dir) else {
            return Ok(out);
        };
        for e in entries.flatten() {
            if e.path().extension().is_some_and(|x| x == "json") {
                out.push(self.read(&e.path())?);
            }
        }
        out.sort_by_key(|v| v["number"].as_u64().unwrap_or(0));
        Ok(out)
    }
}

impl Tracker for FileTracker {
    fn name(&self) -> &'static str {
        "file"
    }
    fn find(&self, repo: &str, fingerprint: &str) -> Result<Option<IssueRef>, String> {
        offline()?;
        Ok(self
            .all(repo)?
            .iter()
            .map(Self::issue_of)
            .find(|i| i.title.contains(&format!("[{fingerprint}]"))))
    }
    fn create(
        &self,
        repo: &str,
        title: &str,
        body: &str,
        labels: &[String],
    ) -> Result<IssueRef, String> {
        offline()?;
        // A number is never handed out twice, as the real tracker's are not:
        // each issue is written aside and linked into its name, which fails
        // when another sender took that number first, and the next is tried.
        let mut number = self
            .all(repo)?
            .iter()
            .map(|v| v["number"].as_u64().unwrap_or(0))
            .max()
            .unwrap_or(0)
            + 1;
        let dir = self.repo_dir(repo);
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let aside = dir.join(format!(
            ".new-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        loop {
            let url = format!("file://{}", self.path(repo, number).display());
            let v = serde_json::json!({
                "number": number, "url": url, "title": title, "body": body,
                "labels": labels, "state": "open", "closed_at": null, "comments": [],
            });
            self.write(&aside, &v)?;
            match fs::hard_link(&aside, self.path(repo, number)) {
                Ok(()) => {
                    let _ = fs::remove_file(&aside);
                    return Ok(Self::issue_of(&v));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => number += 1,
                Err(e) => {
                    let _ = fs::remove_file(&aside);
                    return Err(e.to_string());
                }
            }
        }
    }
    fn edit_body(&self, repo: &str, number: u64, body: &str) -> Result<(), String> {
        offline()?;
        let p = self.path(repo, number);
        let mut v = self.read(&p)?;
        v["body"] = serde_json::Value::String(body.to_string());
        self.write(&p, &v)
    }
    fn comment(&self, repo: &str, number: u64, text: &str) -> Result<(), String> {
        offline()?;
        let p = self.path(repo, number);
        let mut v = self.read(&p)?;
        if let Some(c) = v["comments"].as_array_mut() {
            c.push(serde_json::Value::String(text.to_string()));
        } else {
            v["comments"] = serde_json::json!([text]);
        }
        self.write(&p, &v)
    }
    fn get(&self, repo: &str, number: u64) -> Result<IssueRef, String> {
        offline()?;
        Ok(Self::issue_of(&self.read(&self.path(repo, number))?))
    }
}

#[cfg(test)]
mod file_tracker_tests {
    use super::*;

    /// #126: without `is:issue`, GitHub answers the search 422 and the
    /// curl route never reaches the create.
    #[test]
    fn the_curl_search_names_issues() {
        let q = search_query("otmof-ops/cig-script", "b08bb6f3");
        assert!(q.contains(" is:issue"), "{q}");
        assert!(
            q.starts_with("repo:otmof-ops/cig-script \"[b08bb6f3]\" in:title"),
            "{q}"
        );
    }

    #[test]
    fn senders_at_once_never_share_an_issue_number() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().to_path_buf();
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    let tr = FileTracker { dir };
                    tr.create("o/r", &format!("[fp{i}] report {i}"), "body", &[])
                        .unwrap()
                        .number
                })
            })
            .collect();
        let mut numbers: Vec<u64> = threads.into_iter().map(|h| h.join().unwrap()).collect();
        numbers.sort();
        assert_eq!(numbers, (1..=8).collect::<Vec<u64>>());
        let files = fs::read_dir(dir.join("o__r")).unwrap().flatten().count();
        assert_eq!(files, 8, "an issue was overwritten or a temporary was left");
    }
}
