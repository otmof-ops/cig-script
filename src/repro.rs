// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! A repro: the smallest script that reproduces a bug cig found in itself,
//! built from the run's journal and carrying nothing of the person's data.
//! Every path is a placeholder, every byte generated, every hop a stub that
//! does what the journal saw. Before it is attached to a report it is run
//! in a sandbox and must show the same finding; then ops are dropped one at
//! a time while it still does, so the maintainer gets the smallest case.

use crate::burn::journal::{Before, Journal};
use crate::burn::runs::RunRecord;
use crate::burn::Op;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Repro {
    /// The script, with placeholder paths; `root0`, `root1`, ... are the
    /// pack roots, made by the harness before it runs.
    pub script: String,
    /// What the pack held before the run, in placeholders.
    #[serde(default)]
    pub seeds: Vec<Seed>,
    /// Stub hops the script calls, by file name (`stub1.sh`...), POSIX shell.
    #[serde(default)]
    pub stubs: Vec<(String, String)>,
    /// How many ops the script performs.
    pub ops: usize,
    /// It reproduced the same finding in a sandbox before it was attached.
    pub verified: bool,
    /// What a run of it is expected to show, in words.
    #[serde(default)]
    pub expect: String,
    /// The finding to look for, machine-readable: `tripwire:<check>`,
    /// `internal`, `crash`.
    #[serde(default)]
    pub check: String,
    /// What the builder has to say: dropped ops, why it could not verify.
    #[serde(default)]
    pub note: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Seed {
    /// Placeholder path, relative to the sandbox (`root0/d1/f3.txt`).
    pub path: String,
    /// `file`, `dir` or `link`.
    pub kind: String,
    #[serde(default)]
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// The bytes derive from this (the recorded hash): two files that were
    /// equal stay equal, so a deduplication bug survives the anonymising.
    #[serde(default)]
    pub seed: String,
}

// ----- placeholders ------------------------------------------------------------------------

/// The person's paths, each component given a stable placeholder that keeps
/// what a filesystem bug may depend on: depth, extension, a leading dot, a
/// capital letter, a character outside ASCII. The same mapping scrubs the
/// free text of a report: a path it has seen is written as its placeholder,
/// and anything else shaped like a path or a file name is given one too.
#[derive(Debug, Default)]
pub struct Mapping {
    /// (real root, placeholder), longest root first.
    roots: Vec<(PathBuf, String)>,
    /// The run's working directory: a journal path is relative to it when
    /// the script wrote it so.
    cwd: PathBuf,
    names: HashMap<String, String>,
    files: HashSet<String>,
    dirs: usize,
    file_count: usize,
    /// Every real path seen, for anonymising free text.
    seen: Vec<PathBuf>,
}

/// Extensions that mark a bare word as a file name in free text.
const FILE_EXTENSIONS: &[&str] = &[
    "cig", "sh", "bash", "zsh", "fish", "ps1", "bat", "cmd", "exe", "txt", "md", "rst", "csv",
    "tsv", "json", "jsonl", "yaml", "yml", "toml", "ini", "cfg", "conf", "env", "log", "xml",
    "html", "htm", "css", "js", "mjs", "cjs", "ts", "tsx", "jsx", "py", "pyc", "rs", "go", "java",
    "kt", "rb", "php", "c", "h", "cpp", "hpp", "cc", "cs", "swift", "sql", "db", "sqlite",
    "sqlite3", "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "zip", "tar",
    "gz", "tgz", "bz2", "xz", "zst", "7z", "rar", "png", "jpg", "jpeg", "gif", "svg", "webp",
    "ico", "bmp", "mp3", "mp4", "wav", "flac", "mov", "avi", "mkv", "key", "pem", "crt", "cer",
    "p12", "pfx", "pub", "bak", "tmp", "old", "orig", "lock", "parcel", "dat", "bin", "iso", "img",
    "dmg", "pkg", "deb", "rpm", "apk", "jar", "war", "wasm", "so", "dll", "dylib", "o", "a", "lib",
    "map", "snap", "ipynb", "pl", "lua", "tf", "hcl", "proto",
];

/// File names of cig's own, and the family's, that a report may say.
const OWN_NAMES: &[&str] = &[
    "journal.jsonl",
    "run.json",
    "after.json",
    "intents.jsonl",
    "config.toml",
    "cig.exe",
    "install.sh",
    "Cargo.toml",
    "Cargo.lock",
    "cheat.yml",
    "repro.cig",
    "orchestrate.cig",
    "cig-tasks.json",
    "cigo.exe",
];

/// A placeholder this mapping makes: `d3`, `F12.txt`, `.d1`, `d2é`.
fn is_placeholder_component(c: &str) -> bool {
    let body = c.strip_prefix('.').unwrap_or(c);
    let mut chars = body.chars();
    if !matches!(chars.next(), Some('d' | 'D' | 'f' | 'F')) {
        return false;
    }
    let rest: String = chars.collect();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return false;
    }
    let mut after = &rest[digits.len()..];
    if let Some(a) = after.strip_prefix('é') {
        after = a;
    }
    match after.strip_prefix('.') {
        None => after.is_empty(),
        Some(ext) => {
            !ext.is_empty() && ext.len() <= 8 && ext.chars().all(|c| c.is_ascii_alphanumeric())
        }
    }
}

/// A path made of placeholders: `root0/d1/f2.txt`, `out/d3`, `~/d1/D2`, `/d1`.
fn is_placeholder_path(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut comps = s.split(['/', '\\']);
    let Some(first) = comps.next() else {
        return false;
    };
    let is_root = first
        .strip_prefix("root")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    let is_drive =
        first.len() == 2 && first.ends_with(':') && first.as_bytes()[0].is_ascii_alphabetic();
    if !(first.is_empty() || first == "~" || first == "out" || is_root || is_drive) {
        return false;
    }
    comps.all(|c| {
        c.is_empty()
            || c == "."
            || c == ".."
            || is_placeholder_component(c)
            || OWN_NAMES.contains(&c)
    })
}

/// A location in cig's own source, or the toolchain's: said as it is.
fn is_source_location(s: &str) -> bool {
    let s = s.replace('\\', "/");
    s.starts_with("src/")
        || s.starts_with("tests/")
        || s.starts_with("library/")
        || s.starts_with("/rustc/")
        || s.contains("/.cargo/registry/")
        || s.starts_with("~/.cigscript/")
}

/// `name.ext` with an extension from the list.
fn has_file_extension(c: &str) -> bool {
    c.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty() && FILE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
    })
}

/// A bare word shaped like a file name: `report.csv`, `Deploy.sh`; never a
/// version, a builtin (`fs.rm`), a domain, or a name of cig's own.
fn looks_like_file_name(s: &str) -> bool {
    if OWN_NAMES.contains(&s) || is_placeholder_component(s) {
        return false;
    }
    if s.starts_with("stub") && s.ends_with(".sh") {
        return false;
    }
    let Some((stem, ext)) = s.rsplit_once('.') else {
        return false;
    };
    if stem.is_empty() || stem.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return false;
    }
    FILE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
}

/// `~` and `~/...` as the home directory, when the environment names one.
fn expand_tilde(s: &str) -> String {
    let Some(home) = crate::crash::home_path() else {
        return s.to_string();
    };
    if s == "~" {
        return home.to_string_lossy().to_string();
    }
    if let Some(rest) = s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\")) {
        return home.join(rest).to_string_lossy().to_string();
    }
    s.to_string()
}

/// The placeholder path `to`, relative to the placeholder directory `from`.
fn relative_placeholder(from: &str, to: &str) -> String {
    let a: Vec<&str> = from.split('/').filter(|c| !c.is_empty()).collect();
    let b: Vec<&str> = to.split('/').filter(|c| !c.is_empty()).collect();
    let common = a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count();
    let mut out: Vec<&str> = vec![".."; a.len() - common];
    out.extend_from_slice(&b[common..]);
    if out.is_empty() {
        ".".to_string()
    } else {
        out.join("/")
    }
}

/// `needle` replaced by `with` wherever it stands as a whole: not inside a
/// longer name (`p/f` in `p/f.txt`), not inside a word.
fn replace_delimited(text: &str, needle: &str, with: &str) -> String {
    let joins = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-' | '.');
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(needle) {
        let before_ok = rest[..i].chars().next_back().is_none_or(|c| !joins(c));
        let after_ok = rest[i + needle.len()..]
            .chars()
            .next()
            .is_none_or(|c| !joins(c));
        out.push_str(&rest[..i]);
        if before_ok && after_ok {
            out.push_str(with);
        } else {
            out.push_str(needle);
        }
        rest = &rest[i + needle.len()..];
    }
    out.push_str(rest);
    out
}

impl Mapping {
    pub fn new(roots: &[PathBuf], cwd: &Path) -> Mapping {
        let mut m = Mapping {
            cwd: cwd.to_path_buf(),
            ..Mapping::default()
        };
        for (i, r) in roots.iter().enumerate() {
            m.roots.push((r.clone(), format!("root{i}")));
        }
        m.roots
            .sort_by_key(|(r, _)| std::cmp::Reverse(r.components().count()));
        m
    }

    /// The mapping of a run before its journal is read: its roots (the
    /// pack, or the working directory when there is no pack) and its script.
    pub fn for_record(rec: &RunRecord) -> Mapping {
        let roots: Vec<PathBuf> = if rec.pack.is_empty() {
            vec![PathBuf::from(&rec.cwd)]
        } else {
            rec.pack.clone()
        };
        let mut m = Mapping::new(&roots, Path::new(&rec.cwd));
        if !rec.script.is_empty() {
            m.note_file(Path::new(&rec.script));
        }
        m
    }

    /// The mapping of a run: its roots and script, and every path its
    /// journal names, files told from directories.
    pub fn for_run(rec: &RunRecord, journal: &Journal) -> Mapping {
        let mut m = Mapping::for_record(rec);
        for e in journal.entries() {
            match &e.op {
                Op::Write { path, .. } | Op::Append { path, .. } => m.note_file(path),
                Op::Copy { from, to } | Op::Move { from, to } => {
                    m.note_file(from);
                    m.note_file(to);
                }
                Op::Delete { path } | Op::Mkdir { path } => m.note(path),
                Op::HopCreated { path, .. } | Op::HopChanged { path, .. } => m.note_file(path),
                Op::Proc { cwd: Some(c), .. } => m.note(c),
                _ => {}
            }
            for b in &e.before {
                match &b.before {
                    Before::File { .. } | Before::Symlink { .. } | Before::Moved { .. } => {
                        m.note_file(&b.path)
                    }
                    _ => m.note(&b.path),
                }
            }
        }
        m
    }

    pub fn root_count(&self) -> usize {
        self.roots.len()
    }

    fn absolute(&self, p: &Path) -> PathBuf {
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.cwd.join(p)
        }
    }

    /// Say that the last component of `p` names a file.
    pub fn note_file(&mut self, p: &Path) {
        if let Some(n) = p.file_name().and_then(|n| n.to_str()) {
            self.files.insert(n.to_string());
        }
        self.seen.push(p.to_path_buf());
    }

    pub fn note(&mut self, p: &Path) {
        self.seen.push(p.to_path_buf());
    }

    fn placeholder(&mut self, component: &str, last: bool) -> String {
        if let Some(p) = self.names.get(component) {
            return p.clone();
        }
        // A file of cig's own, or the family's, is said by name.
        if OWN_NAMES.contains(&component) {
            return component.to_string();
        }
        let is_file = last && (self.files.contains(component) || has_file_extension(component));
        let mut out = String::new();
        if component.starts_with('.') {
            out.push('.');
        }
        let n = if is_file {
            self.file_count += 1;
            self.file_count
        } else {
            self.dirs += 1;
            self.dirs
        };
        let letter = if component.chars().any(|c| c.is_uppercase()) {
            if is_file {
                'F'
            } else {
                'D'
            }
        } else if is_file {
            'f'
        } else {
            'd'
        };
        out.push(letter);
        out.push_str(&n.to_string());
        if !component.is_ascii() {
            out.push('é');
        }
        if is_file {
            let ext = Path::new(component)
                .extension()
                .and_then(|e| e.to_str())
                .filter(|e| e.len() <= 8 && e.chars().all(|c| c.is_ascii_alphanumeric()));
            if let Some(e) = ext {
                out.push('.');
                out.push_str(e);
            }
        }
        self.names.insert(component.to_string(), out.clone());
        out
    }

    /// The placeholder path for `p`: under its root (`root0/d1/f2.txt`) or,
    /// outside every root, under `out/`. A mapping with no roots (a report
    /// made outside a run) writes `~/d1/d2` under the home directory and
    /// `/d1/d2` elsewhere: the shape of the path, none of its names.
    pub fn map(&mut self, p: &Path) -> String {
        let abs = self.absolute(p);
        let p = abs.as_path();
        let (rel, prefix): (PathBuf, String) = match self
            .roots
            .iter()
            .find(|(r, _)| p.starts_with(r))
            .map(|(r, ph)| (p.strip_prefix(r).unwrap_or(p).to_path_buf(), ph.clone()))
        {
            Some(x) => x,
            None if !self.roots.is_empty() => (p.to_path_buf(), "out".to_string()),
            None => match crate::crash::strip_home(p) {
                Some(rest) => (rest, "~".to_string()),
                None => {
                    let drive = p
                        .components()
                        .find_map(|c| match c {
                            std::path::Component::Prefix(x) => {
                                Some(x.as_os_str().to_string_lossy().to_string())
                            }
                            _ => None,
                        })
                        .unwrap_or_default();
                    (p.to_path_buf(), drive)
                }
            },
        };
        let comps: Vec<String> = rel
            .components()
            .filter_map(|c| match c {
                std::path::Component::Normal(n) => Some(n.to_string_lossy().to_string()),
                std::path::Component::ParentDir => Some("..".to_string()),
                _ => None,
            })
            .collect();
        let mut out = prefix;
        let n = comps.len();
        for (i, c) in comps.iter().enumerate() {
            out.push('/');
            if c == ".." {
                out.push_str("..");
            } else {
                let ph = self.placeholder(c, i + 1 == n);
                out.push_str(&ph);
            }
        }
        out
    }

    /// A link's target as the repro writes it: an absolute target mapped,
    /// a relative one kept relative to the link, through the link's
    /// directory.
    pub fn map_link_target(&mut self, link: &Path, target: &Path) -> String {
        if target.is_absolute() {
            return self.map(target);
        }
        let dir = self
            .absolute(link)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let full = dir.join(target);
        self.note_file(&full);
        let from = self.map(&dir);
        let to = self.map(&full);
        relative_placeholder(&from, &to)
    }

    /// A glob, its plain components mapped and its wildcards kept: the plain
    /// prefix is a path and is mapped as one (`zqx-pack/**` under the pack
    /// root is `root0/**`), the components after the first wildcard are
    /// placeholders one by one.
    pub fn map_glob(&mut self, g: &str) -> String {
        let (optional, bare) = match g.strip_prefix('?') {
            Some(rest) => ("?", rest),
            None => ("", g),
        };
        let is_wild = |c: &str| c.contains(['*', '?', '[', '{']);
        let comps: Vec<&str> = bare.split('/').collect();
        let plain = comps.iter().take_while(|c| !is_wild(c)).count();
        let mut parts: Vec<String> = Vec::new();
        if plain > 0 && !(plain == 1 && comps[0].is_empty()) {
            parts.push(self.map(Path::new(&comps[..plain].join("/"))));
        } else if plain == 1 {
            parts.push(String::new());
        }
        let n = comps.len();
        for (i, c) in comps.iter().enumerate().skip(plain) {
            if is_wild(c) || c.is_empty() || *c == "." || *c == ".." {
                parts.push(c.to_string());
            } else {
                parts.push(self.placeholder(c, i + 1 == n));
            }
        }
        format!("{optional}{}", parts.join("/"))
    }

    /// Free text with every path it names written as a placeholder: the
    /// paths this mapping has seen, longest first, then anything else
    /// shaped like a path (`/a/b`, `~/a`, `a/b.txt`, `C:\a`) or a file name
    /// (`report.csv`); a location in cig's own source stays. Then the
    /// usual redaction.
    pub fn anonymise_text(&mut self, text: &str) -> String {
        let mut seen: Vec<PathBuf> = self.seen.clone();
        for (r, _) in &self.roots {
            seen.push(r.clone());
        }
        seen.sort_by_key(|p| std::cmp::Reverse(p.as_os_str().len()));
        seen.dedup();
        let mut out = text.to_string();
        for p in seen {
            let real = p.display().to_string();
            if real.is_empty() || !out.contains(&real) {
                continue;
            }
            let ph = self.map(&p);
            out = replace_delimited(&out, &real, &ph);
        }
        let out = self.scrub_tokens(&out);
        crate::crash::redact_text(&out)
    }

    fn scrub_tokens(&mut self, text: &str) -> String {
        let delimiter = |c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    '`' | '"'
                        | '\''
                        | '('
                        | ')'
                        | '['
                        | ']'
                        | '{'
                        | '}'
                        | '<'
                        | '>'
                        | ','
                        | ';'
                        | '='
                        | '|'
                )
        };
        let mut out = String::with_capacity(text.len());
        let mut token = String::new();
        for ch in text.chars() {
            if delimiter(ch) {
                if !token.is_empty() {
                    let t = self.scrub_token(&token);
                    out.push_str(&t);
                    token.clear();
                }
                out.push(ch);
            } else {
                token.push(ch);
            }
        }
        if !token.is_empty() {
            let t = self.scrub_token(&token);
            out.push_str(&t);
        }
        out
    }

    /// One word of free text: a path or a file name becomes its
    /// placeholder, with any `:line:col` and trailing punctuation kept.
    fn scrub_token(&mut self, tok: &str) -> String {
        let mut end = tok.len();
        while end > 0
            && matches!(
                tok.as_bytes()[end - 1],
                b'.' | b',' | b';' | b':' | b'!' | b'?'
            )
        {
            end -= 1;
        }
        let mut core = tok[..end].to_string();
        let mut tail = tok[end..].to_string();
        for _ in 0..2 {
            match core.rsplit_once(':') {
                Some((head, digits))
                    if !head.is_empty()
                        && !digits.is_empty()
                        && digits.bytes().all(|b| b.is_ascii_digit()) =>
                {
                    tail = format!(":{digits}{tail}");
                    core = head.to_string();
                }
                _ => break,
            }
        }
        if core.is_empty() || core.contains("://") {
            return tok.to_string();
        }
        if core.contains('/') || core.contains('\\') {
            if is_source_location(&core) || is_placeholder_path(&core) {
                return tok.to_string();
            }
            let expanded = expand_tilde(&core);
            return format!("{}{tail}", self.map(Path::new(&expanded)));
        }
        if looks_like_file_name(&core) {
            self.files.insert(core.clone());
            let ph = self.placeholder(&core, true);
            return format!("{ph}{tail}");
        }
        tok.to_string()
    }

    /// A JSON value with every string anonymised. A field named for a path
    /// (`cwd`, `path`, `file`, `script`, ...) is mapped whole; a command
    /// line (`command`, `args`) is reduced to the words cig knows and the
    /// shape of the rest.
    pub fn anonymise_value(&mut self, v: &mut serde_json::Value) {
        self.anonymise_under(None, v)
    }

    fn anonymise_under(&mut self, key: Option<&str>, v: &mut serde_json::Value) {
        match v {
            serde_json::Value::String(s) => {
                let t = match key {
                    Some(
                        "cwd" | "dir" | "path" | "file" | "script" | "from" | "to" | "snapshot",
                    ) if !s.is_empty() && s != "?" => self.map_text_path(s),
                    Some("cmd" | "command" | "program" | "exe") => crate::crash::program_name(s),
                    _ => self.anonymise_text(s),
                };
                *s = t;
            }
            serde_json::Value::Array(items) => {
                if matches!(
                    key,
                    Some("touches" | "touches_sealed" | "skip" | "globs" | "produces" | "inputs")
                ) {
                    for it in items.iter_mut() {
                        if let serde_json::Value::String(s) = it {
                            *s = self.map_glob(s);
                        }
                    }
                } else if matches!(key, Some("command" | "args" | "argv" | "cmd")) {
                    let is_command = matches!(key, Some("command" | "argv"));
                    for (i, it) in items.iter_mut().enumerate() {
                        if let serde_json::Value::String(s) = it {
                            *s = if i == 0 && is_command {
                                crate::crash::program_name(s)
                            } else {
                                crate::crash::redact_arg(s)
                            };
                        }
                    }
                } else {
                    for it in items.iter_mut() {
                        self.anonymise_under(None, it);
                    }
                }
            }
            serde_json::Value::Object(map) => {
                for (k, val) in map.iter_mut() {
                    self.anonymise_under(Some(k.as_str()), val);
                }
            }
            _ => {}
        }
    }

    /// A string that is a path, whole: a placeholder stays, anything else
    /// is mapped (`~` expanded first).
    fn map_text_path(&mut self, s: &str) -> String {
        if is_placeholder_path(s) {
            return s.to_string();
        }
        let expanded = expand_tilde(s);
        self.map(Path::new(&expanded))
    }
}

// ----- building -----------------------------------------------------------------------------

fn literal(n: u64) -> String {
    let n = n.clamp(1, 4096) as usize;
    "x".repeat(n)
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Build the repro and the mapping from a run's journal. `None` when the
/// journal has nothing to build from.
pub fn build(
    rec: &RunRecord,
    journal: &Journal,
    check: &str,
    expect: &str,
) -> Option<(Repro, Mapping)> {
    if journal.is_empty() {
        return None;
    }
    let mut m = Mapping::for_run(rec, journal);
    // Seeds: the first before-state of every path is what was there.
    let mut seeded: HashSet<PathBuf> = HashSet::new();
    let mut seeds = Vec::new();
    for e in journal.entries() {
        for b in &e.before {
            if !seeded.insert(b.path.clone()) {
                continue;
            }
            let path = m.map(&b.path);
            match &b.before {
                Before::Absent => {}
                Before::File {
                    bytes,
                    sha256,
                    mode,
                    ..
                } => seeds.push(Seed {
                    path,
                    kind: "file".to_string(),
                    bytes: *bytes,
                    mode: *mode,
                    target: None,
                    seed: sha256.chars().take(16).collect(),
                }),
                Before::Dir { .. } => seeds.push(Seed {
                    path,
                    kind: "dir".to_string(),
                    bytes: 0,
                    mode: None,
                    target: None,
                    seed: String::new(),
                }),
                Before::Directory { mode, .. } => seeds.push(Seed {
                    path,
                    kind: "dir".to_string(),
                    bytes: 0,
                    mode: *mode,
                    target: None,
                    seed: String::new(),
                }),
                Before::Symlink { target } => {
                    let t = m.map_link_target(&b.path, target);
                    seeds.push(Seed {
                        path,
                        kind: "link".to_string(),
                        bytes: 0,
                        mode: None,
                        target: Some(t),
                        seed: String::new(),
                    })
                }
                Before::Moved { .. } => seeds.push(Seed {
                    path,
                    kind: "file".to_string(),
                    bytes: 16,
                    mode: None,
                    target: None,
                    seed: "moved".to_string(),
                }),
            }
        }
    }
    // The ops, in order; a hop becomes a stub doing what the journal saw.
    let mut lines: Vec<String> = Vec::new();
    let mut stubs: Vec<(String, String)> = Vec::new();
    let entries = journal.entries();
    let mut i = 0;
    while i < entries.len() {
        let e = &entries[i];
        match &e.op {
            Op::Write { path, sensitive } => {
                let size = e
                    .before
                    .iter()
                    .find_map(|b| match &b.before {
                        Before::File { bytes, .. } => Some(*bytes),
                        _ => None,
                    })
                    .unwrap_or(8);
                let p = m.map(path);
                lines.push(format!(
                    "fs.write_text({}, {}{})",
                    quote(&p),
                    quote(&literal(size)),
                    if *sensitive {
                        ", {sensitive: true}"
                    } else {
                        ""
                    }
                ));
            }
            Op::Append { path, sensitive } => {
                let p = m.map(path);
                lines.push(format!(
                    "fs.append_text({}, \"x\"{})",
                    quote(&p),
                    if *sensitive {
                        ", {sensitive: true}"
                    } else {
                        ""
                    }
                ));
            }
            Op::Delete { path } => {
                let p = m.map(path);
                lines.push(format!("fs.rm({})", quote(&p)));
            }
            Op::Move { from, to } => {
                let a = m.map(from);
                let b = m.map(to);
                lines.push(format!("fs.mv({}, {})", quote(&a), quote(&b)));
            }
            Op::Copy { from, to } => {
                let a = m.map(from);
                let b = m.map(to);
                lines.push(format!("fs.cp({}, {})", quote(&a), quote(&b)));
            }
            Op::Mkdir { path } => {
                let p = m.map(path);
                lines.push(format!("fs.mkdir({})", quote(&p)));
            }
            Op::Proc {
                cwd,
                touches,
                touches_sealed,
                skip,
                boxed,
                ..
            } => {
                // The hop's effects follow it in the journal.
                let cwd_ph = cwd.as_ref().map(|c| m.map(c));
                let ups = cwd_ph
                    .as_ref()
                    .map(|c| c.split('/').filter(|s| !s.is_empty()).count())
                    .unwrap_or(0);
                let climb = "../".repeat(ups);
                let mut body = String::from("#!/bin/sh\nset -e\n");
                let mut j = i + 1;
                while j < entries.len() {
                    match &entries[j].op {
                        Op::HopCreated { path, .. } => {
                            let p = format!("{climb}{}", m.map(path));
                            body.push_str(&format!(
                                "mkdir -p \"$(dirname \"{p}\")\" && printf 'created' > \"{p}\"\n"
                            ));
                        }
                        Op::HopChanged { path, change, .. } => {
                            let p = format!("{climb}{}", m.map(path));
                            if change.to_ascii_lowercase().contains("delet") {
                                body.push_str(&format!("rm -rf \"{p}\"\n"));
                            } else {
                                body.push_str(&format!("printf 'changed' > \"{p}\"\n"));
                            }
                        }
                        _ => break,
                    }
                    j += 1;
                }
                let name = format!("stub{}.sh", stubs.len() + 1);
                let mut opts: Vec<String> = Vec::new();
                if let Some(c) = &cwd_ph {
                    opts.push(format!("cwd: {}", quote(c)));
                }
                if !touches.is_empty() {
                    let globs: Vec<String> =
                        touches.iter().map(|g| quote(&m.map_glob(g))).collect();
                    opts.push(format!("touches: [{}]", globs.join(", ")));
                }
                if !touches_sealed.is_empty() {
                    let globs: Vec<String> = touches_sealed
                        .iter()
                        .map(|g| quote(&m.map_glob(g)))
                        .collect();
                    opts.push(format!("touches_sealed: [{}]", globs.join(", ")));
                }
                if !skip.is_empty() {
                    let globs: Vec<String> = skip.iter().map(|g| quote(&m.map_glob(g))).collect();
                    opts.push(format!("skip: [{}]", globs.join(", ")));
                }
                if *boxed {
                    opts.push("box: true".to_string());
                }
                lines.push(format!(
                    "proc.run(\"sh\", [{}], {{{}}})",
                    quote(&format!("{climb}{name}")),
                    opts.join(", ")
                ));
                stubs.push((name, body));
                i = j;
                continue;
            }
            Op::EnvSet { .. } | Op::EnvUnset { .. } => {
                lines.push("# an env op ran here (keys are not copied)".to_string());
            }
            Op::Compensate { .. } => {
                lines.push("# an unburn block ended here (ops run flat in the repro)".to_string());
            }
            Op::HopCreated { .. } | Op::HopChanged { .. } => {
                lines.push("# a hop effect without its hop: omitted".to_string());
            }
        }
        i += 1;
    }
    let ops = lines.iter().filter(|l| !l.starts_with('#')).count();
    if ops == 0 {
        return None;
    }
    let script = render(&m.root_count(), &lines, rec.status != "ok");
    Some((
        Repro {
            script,
            seeds,
            stubs,
            ops,
            verified: false,
            expect: expect.to_string(),
            check: check.to_string(),
            note: String::new(),
        },
        m,
    ))
}

fn render(roots: &usize, lines: &[String], fails: bool) -> String {
    let roots_list: Vec<String> = (0..*roots).map(|i| format!("\"root{i}\"")).collect();
    let mut s = format!(
        "# a repro built by cig {} from a run's journal: placeholder paths, generated bytes, stub hops\npack {{ {} }}\nburn {{\n",
        crate::VERSION,
        roots_list.join(", ")
    );
    for l in lines {
        s.push_str("  ");
        s.push_str(l);
        s.push('\n');
    }
    if fails {
        s.push_str("  cough \"repro\"\n");
    }
    s.push_str("}\n");
    s
}

// ----- verifying and shrinking ---------------------------------------------------------------

/// Bytes for a seed: the recorded hash, repeated; equal hashes, equal bytes.
fn seed_bytes(seed: &Seed) -> Vec<u8> {
    let pattern: Vec<u8> = if seed.seed.is_empty() {
        b"seed".to_vec()
    } else {
        seed.seed.as_bytes().to_vec()
    };
    pattern
        .iter()
        .cycle()
        .take(seed.bytes as usize)
        .copied()
        .collect()
}

fn lay_out(case: &Path, repro: &Repro, script: &str) -> std::io::Result<()> {
    let roots = script
        .lines()
        .find(|l| l.starts_with("pack {"))
        .map(|l| l.matches("\"root").count())
        .unwrap_or(1);
    for i in 0..roots.max(1) {
        std::fs::create_dir_all(case.join(format!("root{i}")))?;
    }
    for s in &repro.seeds {
        let p = case.join(&s.path);
        match s.kind.as_str() {
            "dir" => std::fs::create_dir_all(&p)?,
            "link" => {
                if let Some(parent) = p.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                #[cfg(unix)]
                if let Some(t) = &s.target {
                    let _ = std::os::unix::fs::symlink(t, &p);
                }
                #[cfg(not(unix))]
                let _ = &s.target;
            }
            _ => {
                if let Some(parent) = p.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&p, seed_bytes(s))?;
            }
        }
        #[cfg(unix)]
        if let Some(mode) = s.mode {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode & 0o7777));
        }
    }
    for (name, body) in &repro.stubs {
        std::fs::write(case.join(name), body)?;
    }
    std::fs::write(case.join("repro.cig"), script)?;
    Ok(())
}

/// Run the repro in `scratch` and say whether it showed the finding.
fn reproduces(repro: &Repro, script: &str, scratch: &Path) -> Result<bool, String> {
    // A compiled program is not a general cig: it runs nothing but itself.
    if crate::program::running() {
        return Err("a compiled program does not run repros".to_string());
    }
    let _ = std::fs::remove_dir_all(scratch);
    let case = scratch.join("case");
    let home = scratch.join("home");
    std::fs::create_dir_all(&case).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    lay_out(&case, repro, script).map_err(|e| e.to_string())?;
    // Marked as cig's call to itself: a program's binary is `cig` for it.
    let mut cmd = crate::program::self_command().map_err(|e| e.to_string())?;
    cmd.args(["run", "repro.cig"])
        .current_dir(&case)
        .env("CIGSCRIPT_HOME", &home)
        .env("NO_COLOR", "1")
        .env("CIG_SEAL_KEY", "file")
        .env("CIG_DOCTOR", "0")
        .env("CIG_REPORTS", "never")
        .env_remove("CIG_INTERNAL_TRACKER_DIR")
        .env_remove("CIG_JOURNAL")
        .env_remove("CIG_RUN_ID")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not start cig: {e}"))?;
    let out_reader = child.stdout.take();
    let err_reader = child.stderr.take();
    let drain = |p: Option<std::process::ChildStderr>| {
        p.map(|mut r| {
            std::thread::spawn(move || {
                use std::io::Read;
                let mut buf = Vec::new();
                let _ = r.read_to_end(&mut buf);
                buf
            })
        })
    };
    let out_thread = out_reader.map(|mut r| {
        std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = Vec::new();
            let _ = r.read_to_end(&mut buf);
            buf
        })
    });
    let err_thread = drain(err_reader);
    use wait_timeout::ChildExt;
    let status = match child
        .wait_timeout(std::time::Duration::from_secs(60))
        .map_err(|e| e.to_string())?
    {
        Some(s) => s,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err("the repro ran longer than a minute and was killed".to_string());
        }
    };
    if let Some(t) = out_thread {
        let _ = t.join();
    }
    let stderr = err_thread
        .and_then(|t| t.join().ok())
        .map(|b| String::from_utf8_lossy(&b).to_string())
        .unwrap_or_default();
    let hit = match repro.check.as_str() {
        "internal" => stderr.contains("error[E900"),
        "crash" => status.code() == Some(70),
        c => {
            let check = c.strip_prefix("tripwire:").unwrap_or(c);
            // The inner run queued its own tripwire report: read it back.
            let ashtray = home.join("ashtray");
            let mut found = false;
            if let Ok(entries) = std::fs::read_dir(&ashtray) {
                for e in entries.flatten() {
                    if let Ok(text) = std::fs::read_to_string(e.path()) {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                            if v["kind"] == "tripwire" && v["details"]["check"] == check {
                                found = true;
                            }
                        }
                    }
                }
            }
            found
        }
    };
    Ok(hit)
}

/// Verify the repro in a sandbox under `scratch`, then drop ops one at a
/// time while it still reproduces. Sets `verified`, `note`, `ops` and the
/// shrunk `script`; a repro that did not reproduce is kept but not attached.
pub fn verify_and_shrink(repro: &mut Repro, scratch: &Path) {
    let started = std::time::Instant::now();
    match reproduces(repro, &repro.script.clone(), scratch) {
        Ok(true) => {}
        Ok(false) => {
            repro.verified = false;
            repro.note = "It did not reproduce in the sandbox, so it is not attached.".to_string();
            let _ = std::fs::remove_dir_all(scratch);
            return;
        }
        Err(e) => {
            repro.verified = false;
            repro.note = format!("It could not be verified ({e}), so it is not attached.");
            let _ = std::fs::remove_dir_all(scratch);
            return;
        }
    }
    repro.verified = true;
    let before = repro.ops;
    // Shrink: every op line, newest first, dropped while it still reproduces.
    let mut lines: Vec<String> = repro.script.lines().map(str::to_string).collect();
    let mut attempts = 0;
    let mut idx = lines.len();
    while idx > 0 {
        idx -= 1;
        let l = lines[idx].trim_start();
        if !(l.starts_with("fs.") || l.starts_with("proc.")) {
            continue;
        }
        if attempts >= 16 || started.elapsed().as_secs() > 45 {
            break;
        }
        attempts += 1;
        let mut candidate = lines.clone();
        candidate.remove(idx);
        let script = candidate.join("\n") + "\n";
        if let Ok(true) = reproduces(repro, &script, scratch) {
            lines = candidate;
        }
    }
    repro.script = lines.join("\n") + "\n";
    repro.ops = lines
        .iter()
        .map(|l| l.trim_start())
        .filter(|l| l.starts_with("fs.") || l.starts_with("proc."))
        .count();
    repro.note = if repro.ops < before {
        format!(
            "Reproduced in a sandbox; shrunk from {before} to {} op{}.",
            repro.ops,
            if repro.ops == 1 { "" } else { "s" }
        )
    } else {
        "Reproduced in a sandbox.".to_string()
    };
    let _ = std::fs::remove_dir_all(scratch);
}
