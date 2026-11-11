// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! `cig runs adopt`: a committed installer transaction becomes a run that
//! `cig unburn` puts back, byte for byte.
//!
//! The installer's transaction is built here from its documented journal
//! format (files, then `journal.jsonl` and `saved/`), in a scratch folder
//! that stands for an installer home. No box, no namespace, no network.
#![cfg(unix)]

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("tempdir"),
        }
    }
    fn path(&self) -> &Path {
        self.dir.path()
    }
    fn home(&self) -> PathBuf {
        self.path().join("home")
    }
    /// The scratch folder that stands for an installer home.
    fn ihome(&self) -> PathBuf {
        self.path().join("ihome")
    }
    fn cig(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_cig"))
            .args(args)
            .current_dir(self.path())
            .env("CIGSCRIPT_HOME", self.home())
            .env("HOME", self.path())
            .env("XDG_CONFIG_HOME", self.path().join("xdg-config"))
            .env("XDG_DATA_HOME", self.path().join("xdg-data"))
            .env("XDG_STATE_HOME", self.path().join("xdg-state"))
            .env("XDG_CACHE_HOME", self.path().join("xdg-cache"))
            .env("NO_COLOR", "1")
            .env("CIG_SEAL_KEY", "file")
            .env("CIG_BUDGET_SYNC", "1")
            .env("CIG_INTERNAL_TRACKER_DIR", self.path().join("tracker"))
            .env_remove("CI")
            .env("CIG_UPDATE_MANIFEST", self.path().join("no-releases.json"))
            .output()
            .expect("run cig")
    }
    fn ok(&self, args: &[&str]) -> (String, String) {
        let out = self.cig(args);
        let so = String::from_utf8_lossy(&out.stdout).to_string();
        let se = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(
            out.status.success(),
            "cig {args:?} failed\nstdout:\n{so}\nstderr:\n{se}"
        );
        (so, se)
    }
    /// A refusal: exits non-zero and says `code`.
    fn refused(&self, args: &[&str], code: &str) -> String {
        let out = self.cig(args);
        let se = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(
            !out.status.success(),
            "cig {args:?} should have been refused\nstderr:\n{se}"
        );
        assert!(
            se.contains(&format!("error[{code} burn]")),
            "{code} not in:\n{se}"
        );
        se
    }
    fn runs(&self) -> Vec<Value> {
        let (so, _) = self.ok(&["--json", "runs"]);
        serde_json::from_str(&so).unwrap_or_default()
    }
}

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Every file, folder and link under `root` (the installer's own transaction
/// folders and the store left out): kind and content.
fn tree(root: &Path, skip: &[PathBuf]) -> BTreeMap<String, String> {
    fn walk(dir: &Path, root: &Path, skip: &[PathBuf], out: &mut BTreeMap<String, String>) {
        let mut names: Vec<_> = fs::read_dir(dir).unwrap().flatten().collect();
        names.sort_by_key(|e| e.file_name());
        for e in names {
            let p = e.path();
            if skip.iter().any(|s| p.starts_with(s)) {
                continue;
            }
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().to_string();
            let md = fs::symlink_metadata(&p).unwrap();
            if md.file_type().is_symlink() {
                out.insert(
                    rel,
                    format!("link {}", fs::read_link(&p).unwrap().display()),
                );
            } else if md.is_dir() {
                out.insert(rel, "dir".into());
                walk(&p, root, skip, out);
            } else {
                out.insert(rel, format!("file {}", sha(&fs::read(&p).unwrap())));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, skip, &mut out);
    out
}

/// An installer transaction being written, line by line, as the installer's
/// own journal writes it (its SPEC 5.5: `begin`, ops, `commit`).
struct Txn {
    dir: PathBuf,
    lines: Vec<Value>,
    saved_n: u32,
}

impl Txn {
    fn begin(ihome: &Path, label: &str) -> Txn {
        let dir = ihome.join("installer/txn/20261101T120000-a1b2c3");
        fs::create_dir_all(dir.join("saved")).unwrap();
        Txn {
            dir,
            lines: vec![json!({"seq": 0, "t": "begin", "label": label, "pid": 1, "format": 1})],
            saved_n: 0,
        }
    }
    fn add(&mut self, mut line: Value) {
        line["seq"] = json!(self.lines.len());
        self.lines.push(line);
    }
    /// Copy `path` into saved/ (what the installer's snapshot does) and say so.
    fn save_file(&mut self, path: &Path) -> Value {
        self.saved_n += 1;
        let rel = format!("saved/{:04}", self.saved_n);
        fs::copy(path, self.dir.join(&rel)).unwrap();
        json!({"state": "file", "saved": rel, "sha256": sha(&fs::read(path).unwrap())})
    }
    /// Move a folder into saved/ (an installer `delete` of a tree).
    fn save_dir(&mut self, path: &Path) -> Value {
        self.saved_n += 1;
        let rel = format!("saved/{:04}", self.saved_n);
        fs::rename(path, self.dir.join(&rel)).unwrap();
        json!({"state": "dir", "saved": rel})
    }
    fn finish(&mut self, commit: bool) -> PathBuf {
        if commit {
            self.add(json!({"t": "commit"}));
        }
        let mut text = String::new();
        for l in &self.lines {
            text.push_str(&serde_json::to_string(l).unwrap());
            text.push('\n');
        }
        fs::write(self.dir.join("journal.jsonl"), text).unwrap();
        self.dir.clone()
    }
}

fn put(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

struct Built {
    txn: PathBuf,
    before: BTreeMap<String, String>,
    skip: Vec<PathBuf>,
    store_object: PathBuf,
}

/// An `update`: every op the installer has, over an installer home that exists.
fn build_update(sb: &Sandbox) -> Built {
    let h = sb.ihome();
    put(&h.join("toolchains/cig/1.0/bin/cig"), "old-binary\n");
    put(&h.join("state.json"), "{\"old\":true}\n");
    put(&h.join("installer/shim-index"), "cig 1.0\n");
    put(&h.join("shared-file"), "old-shared\n");
    put(&h.join("old-dir/inner/a.txt"), "alpha\n");
    put(&h.join("old-dir/b.txt"), "beta\n");
    put(&h.join("gone.txt"), "gone\n");
    put(&h.join("a-dir/x"), "x\n");
    fs::create_dir_all(h.join("shims")).unwrap();
    std::os::unix::fs::symlink("../toolchains/cig/1.0/bin/cig", h.join("shims/cig")).unwrap();
    let skip = vec![h.join("installer/txn"), h.join("store")];
    let before = tree(&h, &skip);
    // the toolchain the update builds beside the old one, as it will move it into place
    put(
        &h.join("toolchains/cig/.3.0.partial-a1b2c3/bin/cig"),
        "new-binary\n",
    );

    let mut t = Txn::begin(&h, "update cig 1.0 -> 3.0");
    // mkdir
    t.add(
        json!({"t": "mkdir", "path": h.join("toolchains/cig/2.0"), "before": {"state": "absent"}}),
    );
    fs::create_dir_all(h.join("toolchains/cig/2.0")).unwrap();
    // store: a content addressed object, left in place by the adoption
    let obj = h.join("store/objects/ab/new-object");
    put(&obj, "new-object-bytes\n");
    t.add(json!({"t": "store", "object": "ab", "exec": false, "before": {"state": "absent"}}));
    // link: absent target
    t.add(json!({"t": "link", "from": obj, "to": h.join("toolchains/cig/2.0/cig"), "before": {"state": "absent"}}));
    fs::hard_link(&obj, h.join("toolchains/cig/2.0/cig")).unwrap();
    // link over an existing file: the new name shares its inode with the store object
    let was = t.save_file(&h.join("shared-file"));
    t.add(json!({"t": "link", "from": obj, "to": h.join("shared-file"), "before": was}));
    fs::remove_file(h.join("shared-file")).unwrap();
    fs::hard_link(&obj, h.join("shared-file")).unwrap();
    // move: a toolchain's directory flip, and a plain move
    t.add(json!({"t": "move", "from": h.join("toolchains/cig/.3.0.partial-a1b2c3"), "to": h.join("toolchains/cig/3.0"), "before": {"state": "absent"}}));
    fs::rename(
        h.join("toolchains/cig/.3.0.partial-a1b2c3"),
        h.join("toolchains/cig/3.0"),
    )
    .unwrap();
    t.add(json!({"t": "move", "from": h.join("a-dir"), "to": h.join("b-dir"), "before": {"state": "absent"}}));
    fs::rename(h.join("a-dir"), h.join("b-dir")).unwrap();
    // write
    let was = t.save_file(&h.join("state.json"));
    t.add(json!({"t": "write", "path": h.join("state.json"), "before": was, "after_sha256": sha(b"{\"new\":true}\n")}));
    put(&h.join("state.json"), "{\"new\":true}\n");
    // delete: a tree, a link, a file
    let was = t.save_dir(&h.join("old-dir"));
    t.add(json!({"t": "delete", "path": h.join("old-dir"), "before": was}));
    t.add(json!({"t": "delete", "path": h.join("shims/cig"), "before": {"state": "link", "target": "../toolchains/cig/1.0/bin/cig"}}));
    fs::remove_file(h.join("shims/cig")).unwrap();
    let was = t.save_file(&h.join("gone.txt"));
    t.add(json!({"t": "delete", "path": h.join("gone.txt"), "before": was}));
    fs::remove_file(h.join("gone.txt")).unwrap();
    // flip: the rename of installer/shim-index
    let idx = h.join("installer/shim-index");
    t.saved_n += 1;
    let rel = format!("saved/{:04}", t.saved_n);
    fs::copy(&idx, t.dir.join(&rel)).unwrap();
    t.add(json!({"t": "flip", "index_sha256": sha(b"cig 3.0\n"), "previous_sha256": sha(b"cig 1.0\n"), "saved": rel}));
    put(&idx, "cig 3.0\n");
    let txn = t.finish(true);
    Built {
        txn,
        before,
        skip,
        store_object: obj,
    }
}

fn adopted_id(stdout: &str) -> String {
    stdout.lines().last().unwrap_or("").trim().to_string()
}

#[test]
fn an_adopted_update_unburns_every_file_back_byte_for_byte() {
    let sb = Sandbox::new();
    let b = build_update(&sb);
    let h = sb.ihome();
    let after = tree(&h, &b.skip);
    assert_ne!(after, b.before, "the fixture changed something");

    let (so, se) = sb.ok(&["runs", "adopt", b.txn.to_str().unwrap()]);
    let id = adopted_id(&so);
    assert!(id.starts_with("20261101T120000-"), "{so}{se}");
    assert!(se.contains("1 store object"), "{se}");

    // listed by `cig runs` with its origin
    let (table, _) = sb.ok(&["runs"]);
    assert!(
        table.contains(&id) && table.contains("ciginstaller"),
        "{table}"
    );
    let runs = sb.runs();
    assert_eq!(runs.len(), 1);
    assert!(runs[0]["imported"]["from"]
        .as_str()
        .unwrap()
        .contains("ciginstaller"));
    assert_eq!(
        runs[0]["imported"]["foreign"]["txn"],
        "20261101T120000-a1b2c3"
    );
    // the chain, the record and the ledger agree
    let (_, se) = sb.ok(&["runs", &id, "--verify"]);
    assert!(!se.contains("error"), "{se}");
    sb.ok(&["runs", "--verify"]);

    // adopting nothing changed in the installer home
    assert_eq!(tree(&h, &b.skip), after);

    sb.ok(&["unburn", &id, "--dry-run"]);
    assert_eq!(tree(&h, &b.skip), after, "a dry run changes nothing");
    sb.ok(&["unburn", &id]);
    assert_eq!(
        tree(&h, &b.skip),
        b.before,
        "every file back, byte for byte"
    );
    // the store object the update made is left (it may be in use), and its bytes are whole:
    // restoring `shared-file` did not write through the hard link
    assert_eq!(
        fs::read_to_string(&b.store_object).unwrap(),
        "new-object-bytes\n"
    );
    assert_eq!(runs_status(&sb, &id), "unburned");

    // the same transaction is the same run: adopting again makes no second one
    let (so, se) = sb.ok(&["runs", "adopt", b.txn.to_str().unwrap()]);
    assert_eq!(adopted_id(&so), id);
    assert!(se.contains("already adopted"), "{se}");
    assert_eq!(sb.runs().len(), 1);
    // and a run that is back already is not put back twice
    assert_eq!(tree(&h, &b.skip), b.before);
}

fn runs_status(sb: &Sandbox, id: &str) -> String {
    sb.runs()
        .iter()
        .find(|r| r["id"] == id)
        .map(|r| r["status"].as_str().unwrap_or("").to_string())
        .unwrap_or_default()
}

#[test]
fn a_flip_of_the_shim_index_is_restored() {
    let sb = Sandbox::new();
    let h = sb.ihome();
    let idx = h.join("installer/shim-index");
    put(&idx, "cig 1.0\n");
    let mut t = Txn::begin(&h, "update");
    t.saved_n += 1;
    let rel = format!("saved/{:04}", t.saved_n);
    fs::copy(&idx, t.dir.join(&rel)).unwrap();
    t.add(json!({"t": "flip", "index_sha256": sha(b"cig 2.0\n"), "previous_sha256": sha(b"cig 1.0\n"), "saved": rel}));
    put(&idx, "cig 2.0\n");
    let txn = t.finish(true);
    let (so, _) = sb.ok(&["runs", "adopt", txn.to_str().unwrap()]);
    let id = adopted_id(&so);
    sb.ok(&["unburn", &id]);
    assert_eq!(fs::read_to_string(&idx).unwrap(), "cig 1.0\n");

    // a first install: there was no index before, so the flip's undo removes it
    let sb = Sandbox::new();
    let h = sb.ihome();
    let mut t = Txn::begin(&h, "install");
    t.add(json!({"t": "flip", "index_sha256": sha(b"cig 1.0\n"), "previous_sha256": "absent"}));
    let idx = h.join("installer/shim-index");
    put(&idx, "cig 1.0\n");
    let txn = t.finish(true);
    let (so, _) = sb.ok(&["runs", "adopt", txn.to_str().unwrap()]);
    sb.ok(&["unburn", &adopted_id(&so)]);
    assert!(!idx.exists());
}

#[test]
fn a_tampered_saved_file_is_refused_and_nothing_is_imported() {
    let sb = Sandbox::new();
    let b = build_update(&sb);
    fs::write(b.txn.join("saved/0002"), "not what was saved\n").unwrap();
    let h = sb.ihome();
    let after = tree(&h, &b.skip);
    let se = sb.refused(&["runs", "adopt", b.txn.to_str().unwrap()], "E717");
    assert!(
        se.contains("saved/0002") || se.contains("saved copy"),
        "{se}"
    );
    assert!(sb.runs().is_empty(), "no run was made");
    assert_eq!(tree(&h, &b.skip), after);

    // a saved copy that is gone
    let sb = Sandbox::new();
    let b = build_update(&sb);
    fs::remove_file(b.txn.join("saved/0001")).unwrap();
    sb.refused(&["runs", "adopt", b.txn.to_str().unwrap()], "E717");

    // a journal that names a saved copy outside its transaction
    let sb = Sandbox::new();
    let h = sb.ihome();
    put(&h.join("f"), "f\n");
    let mut t = Txn::begin(&h, "x");
    t.add(json!({"t": "write", "path": h.join("f"), "before": {"state": "file", "saved": "../../../../f", "sha256": sha(b"f\n")}, "after_sha256": sha(b"g")}));
    let txn = t.finish(true);
    sb.refused(&["runs", "adopt", txn.to_str().unwrap()], "E717");
}

#[test]
fn a_path_outside_the_installer_home_is_refused() {
    let sb = Sandbox::new();
    let h = sb.ihome();
    let outside = sb.path().join("outside/profile");
    put(&outside, "mine\n");
    put(&h.join("inside"), "in\n");
    let mut t = Txn::begin(&h, "update");
    let was = t.save_file(&h.join("inside"));
    t.add(
        json!({"t": "write", "path": h.join("inside"), "before": was, "after_sha256": sha(b"x")}),
    );
    let was = t.save_file(&outside);
    t.add(json!({"t": "block", "path": outside, "before": was, "marker": "cig"}));
    let txn = t.finish(true);
    let se = sb.refused(&["runs", "adopt", txn.to_str().unwrap()], "E718");
    assert!(se.contains("outside"), "{se}");
    assert!(sb.runs().is_empty());

    // a `..` that leads out
    let sb = Sandbox::new();
    let h = sb.ihome();
    let mut t = Txn::begin(&h, "update");
    t.add(json!({"t": "mkdir", "path": format!("{}/../elsewhere", h.display()), "before": {"state": "absent"}}));
    let txn = t.finish(true);
    sb.refused(&["runs", "adopt", txn.to_str().unwrap()], "E718");

    // a link on the way down: a restore would write through it
    let sb = Sandbox::new();
    let h = sb.ihome();
    fs::create_dir_all(sb.path().join("elsewhere")).unwrap();
    fs::create_dir_all(&h).unwrap();
    std::os::unix::fs::symlink(sb.path().join("elsewhere"), h.join("trap")).unwrap();
    let mut t = Txn::begin(&h, "update");
    t.add(json!({"t": "mkdir", "path": h.join("trap/sub"), "before": {"state": "absent"}}));
    let txn = t.finish(true);
    sb.refused(&["runs", "adopt", txn.to_str().unwrap()], "E718");
}

#[test]
fn an_uncommitted_or_rolled_back_transaction_is_refused() {
    // open: no commit line
    let sb = Sandbox::new();
    let h = sb.ihome();
    let mut t = Txn::begin(&h, "update");
    t.add(json!({"t": "mkdir", "path": h.join("d"), "before": {"state": "absent"}}));
    fs::create_dir_all(h.join("d")).unwrap();
    let txn = t.finish(false);
    sb.refused(&["runs", "adopt", txn.to_str().unwrap()], "E716");
    assert!(sb.runs().is_empty());

    // rolled back
    let sb = Sandbox::new();
    let h = sb.ihome();
    let mut t = Txn::begin(&h, "update");
    t.add(json!({"t": "mkdir", "path": h.join("d"), "before": {"state": "absent"}}));
    t.add(json!({"t": "rollback-done"}));
    let txn = t.finish(false);
    sb.refused(&["runs", "adopt", txn.to_str().unwrap()], "E716");

    // cut short mid-line
    let sb = Sandbox::new();
    let h = sb.ihome();
    let mut t = Txn::begin(&h, "update");
    t.add(json!({"t": "mkdir", "path": h.join("d"), "before": {"state": "absent"}}));
    let txn = t.finish(true);
    let jp = txn.join("journal.jsonl");
    let mut text = fs::read_to_string(&jp).unwrap();
    text.push_str("{\"seq\":3,\"t\":\"wri");
    fs::write(&jp, text).unwrap();
    // a commit that is not the last line is not a commit either
    sb.refused(&["runs", "adopt", txn.to_str().unwrap()], "E716");
}

#[test]
fn a_folder_that_is_not_a_transaction_is_refused() {
    let sb = Sandbox::new();
    fs::create_dir_all(sb.path().join("plain")).unwrap();
    sb.refused(
        &["runs", "adopt", sb.path().join("plain").to_str().unwrap()],
        "E719",
    );
    let h = sb.ihome();
    let mut t = Txn::begin(&h, "update");
    let txn = t.finish(true);
    fs::write(txn.join("journal.jsonl"), "not json\n").unwrap();
    sb.refused(&["runs", "adopt", txn.to_str().unwrap()], "E719");
    // no folder at all
    let out = sb.cig(&["runs", "adopt"]);
    assert!(!out.status.success());
}
