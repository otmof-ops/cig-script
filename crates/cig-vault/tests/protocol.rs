// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The vault protocol against a vault run in a scratch folder, as the test's
//! own account. (The tests that need the vault as another account run in the
//! VM: `tests/attack/` in cig-script.)

#![cfg(unix)]

use cig_vault::client::{Client, Error};
use cig_vault::server::{bind, serve, Options};
use cig_vault::store::{Put, Store};
use cig_vault::Kind;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

struct Vault {
    _dir: tempfile::TempDir,
    root: PathBuf,
    socket: PathBuf,
    store: Arc<Store>,
}

fn vault(grace: Duration) -> Vault {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = dir.path().join("vault.sock");
    let store = Arc::new(Store::open(&root, 1 << 30).unwrap());
    let l = bind(&socket).unwrap();
    let s = store.clone();
    std::thread::spawn(move || {
        serve(
            l,
            s,
            Options {
                grace,
                idle: Duration::from_secs(20),
                ..Options::default()
            },
        )
    });
    Vault {
        _dir: dir,
        root,
        socket,
        store,
    }
}

fn file_with(dir: &std::path::Path, name: &str, bytes: &[u8]) -> fs::File {
    let p = dir.join(name);
    fs::write(&p, bytes).unwrap();
    fs::File::open(p).unwrap()
}

fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

fn me() -> u32 {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() }
}

#[test]
fn an_object_goes_in_by_descriptor_and_comes_back_whole() {
    let v = vault(Duration::from_secs(3600));
    let scratch = tempfile::tempdir().unwrap();
    let bytes = b"before-bytes of a file".to_vec();
    let id = sha(&bytes);
    let mut c = Client::connect(&v.socket, Some(me())).unwrap();
    let f = file_with(scratch.path(), "in", &bytes);
    assert_eq!(c.store(Kind::Object, &id, &f).unwrap(), Put::Stored);
    assert_eq!(c.store(Kind::Object, &id, &f).unwrap(), Put::Present);
    assert!(c.has(Kind::Object, &id).unwrap());
    assert_eq!(c.verify(Kind::Object, &id).unwrap(), bytes.len() as u64);
    assert_eq!(
        c.list(Kind::Object).unwrap(),
        vec![(id.clone(), bytes.len() as u64)]
    );
    let out = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(scratch.path().join("out"))
        .unwrap();
    assert_eq!(
        c.fetch(Kind::Object, &id, &out).unwrap(),
        bytes.len() as u64
    );
    assert_eq!(fs::read(scratch.path().join("out")).unwrap(), bytes);
    // The store holds it 0400 under a 0700 tree.
    let p = v
        .root
        .join(format!("u{}", me()))
        .join("o")
        .join(&id[..2])
        .join(&id);
    assert_eq!(
        fs::metadata(&p).unwrap().permissions().mode() & 0o777,
        0o400
    );
}

#[test]
fn bytes_that_do_not_hash_to_their_id_are_refused() {
    let v = vault(Duration::from_secs(3600));
    let scratch = tempfile::tempdir().unwrap();
    let mut c = Client::connect(&v.socket, None).unwrap();
    let f = file_with(scratch.path(), "in", b"one thing");
    let wrong = sha(b"another thing");
    match c.store(Kind::Object, &wrong, &f) {
        Err(Error::Refused { word, .. }) => assert_eq!(word, "hash-mismatch"),
        other => panic!("{other:?}"),
    }
    assert!(!c.has(Kind::Object, &wrong).unwrap());
    // Nothing is left behind in the store's temp folder.
    let tmp = v.root.join(format!("u{}", me())).join("tmp");
    assert_eq!(fs::read_dir(tmp).unwrap().count(), 0);
}

#[test]
fn no_request_removes_an_object() {
    let v = vault(Duration::from_secs(0));
    let scratch = tempfile::tempdir().unwrap();
    let bytes = b"keep me".to_vec();
    let id = sha(&bytes);
    let mut c = Client::connect(&v.socket, None).unwrap();
    c.store(Kind::Object, &id, &file_with(scratch.path(), "in", &bytes))
        .unwrap();
    // Every way of asking for a removal is refused, on a raw connection.
    let mut raw = UnixStream::connect(&v.socket).unwrap();
    raw.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    for verb in [
        "delete", "remove", "rm", "unlink", "purge", "forget", "prune", "retire", "expire",
    ] {
        raw.write_all(format!("{verb} o {id}\n").as_bytes())
            .unwrap();
        let mut line = String::new();
        let mut one = [0u8; 1];
        use std::io::Read;
        while raw.read(&mut one).unwrap() == 1 && one[0] != b'\n' {
            line.push(one[0] as char);
        }
        assert!(line.starts_with("err bad-request"), "{verb}: {line}");
    }
    // A gc request with an empty live set, and no grace, only reports.
    let plan = c.gc_plan(&[]).unwrap();
    assert_eq!(plan, vec![(Kind::Object, id.clone())]);
    assert!(c.has(Kind::Object, &id).unwrap());
    assert_eq!(c.verify(Kind::Object, &id).unwrap(), bytes.len() as u64);
    // A live id keeps it out of the plan.
    assert!(c.gc_plan(&[(Kind::Object, id.clone())]).unwrap().is_empty());
}

#[test]
fn gc_by_the_administrator_removes_only_what_no_live_id_names() {
    let v = vault(Duration::from_secs(0));
    let scratch = tempfile::tempdir().unwrap();
    let (a, b) = (b"alpha".to_vec(), b"beta".to_vec());
    let mut c = Client::connect(&v.socket, None).unwrap();
    c.store(Kind::Object, &sha(&a), &file_with(scratch.path(), "a", &a))
        .unwrap();
    c.store(Kind::Object, &sha(&b), &file_with(scratch.path(), "b", &b))
        .unwrap();
    let live: std::collections::HashSet<_> = [(Kind::Object, sha(&a))].into_iter().collect();
    let plan = v.store.gc_plan(me(), &live, Duration::from_secs(0));
    assert_eq!(plan, vec![(Kind::Object, sha(&b))]);
    assert_eq!(v.store.gc_apply(me(), &plan).0, 1);
    assert!(c.has(Kind::Object, &sha(&a)).unwrap());
    assert!(!c.has(Kind::Object, &sha(&b)).unwrap());
    // Young objects are not collected, live or not.
    let young = v.store.gc_plan(
        me(),
        &std::collections::HashSet::new(),
        Duration::from_secs(3600),
    );
    assert!(young.is_empty());
}

#[test]
fn a_descriptor_that_is_not_a_regular_file_is_refused() {
    use std::os::fd::AsRawFd;
    let v = vault(Duration::from_secs(3600));
    let mut c = Client::connect(&v.socket, None).unwrap();
    // A pipe would block the vault for as long as the client likes.
    let mut fds = [0i32; 2];
    // SAFETY: pipe fills two descriptors, closed below through File.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    use std::os::fd::FromRawFd;
    let rd = unsafe { fs::File::from_raw_fd(fds[0]) };
    let _wr = unsafe { fs::File::from_raw_fd(fds[1]) };
    let _ = rd.as_raw_fd();
    match c.store(Kind::Object, &sha(b""), &rd) {
        Err(Error::Refused { word, .. }) => assert_eq!(word, "bad-fd"),
        other => panic!("{other:?}"),
    }
    // A fetch into a file opened read-only is refused too.
    let scratch = tempfile::tempdir().unwrap();
    let ro = file_with(scratch.path(), "ro", b"");
    match c.fetch(Kind::Object, &sha(b"x"), &ro) {
        Err(Error::Refused { word, .. }) => assert_eq!(word, "bad-fd"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_recipe_needs_its_chunks_first_and_a_sealed_object_is_never_replaced() {
    let v = vault(Duration::from_secs(3600));
    let scratch = tempfile::tempdir().unwrap();
    let mut c = Client::connect(&v.socket, None).unwrap();
    let chunk = b"chunk one".to_vec();
    let whole = b"the whole file".to_vec();
    let recipe = format!(
        "{{\"format\":1,\"sha256\":\"{}\",\"bytes\":{},\"chunks\":[{{\"sha256\":\"{}\",\"len\":{}}}]}}",
        sha(&whole),
        whole.len(),
        sha(&chunk),
        chunk.len()
    );
    let rf = file_with(scratch.path(), "r", recipe.as_bytes());
    match c.store(Kind::Recipe, &sha(&whole), &rf) {
        Err(Error::Refused { word, .. }) => assert_eq!(word, "missing-chunk"),
        other => panic!("{other:?}"),
    }
    c.store(
        Kind::Object,
        &sha(&chunk),
        &file_with(scratch.path(), "c", &chunk),
    )
    .unwrap();
    assert_eq!(
        c.store(Kind::Recipe, &sha(&whole), &rf).unwrap(),
        Put::Stored
    );
    assert!(c.verify(Kind::Recipe, &sha(&whole)).is_ok());
    // A recipe under another file's id is refused.
    match c.store(Kind::Recipe, &sha(b"other"), &rf) {
        Err(Error::Refused { word, .. }) => assert_eq!(word, "bad-request"),
        other => panic!("{other:?}"),
    }
    // Sealed: kept under its random id, never replaced with other bytes.
    let id = "ab".repeat(16);
    let s1 = file_with(scratch.path(), "s1", b"CIGSEAL2 first sealed bytes");
    let s2 = file_with(scratch.path(), "s2", b"CIGSEAL2 other sealed bytes");
    assert_eq!(c.store(Kind::Sealed, &id, &s1).unwrap(), Put::Stored);
    assert_eq!(c.store(Kind::Sealed, &id, &s1).unwrap(), Put::Present);
    match c.store(Kind::Sealed, &id, &s2) {
        Err(Error::Refused { word, .. }) => assert_eq!(word, "exists-different"),
        other => panic!("{other:?}"),
    }
    let junk = file_with(scratch.path(), "junk", b"not sealed at all");
    assert!(c.store(Kind::Sealed, &"cd".repeat(16), &junk).is_err());
    // A live recipe keeps its chunks out of a gc plan.
    let live: std::collections::HashSet<_> = [(Kind::Recipe, sha(&whole))].into_iter().collect();
    let plan = v.store.gc_plan(me(), &live, Duration::from_secs(0));
    assert_eq!(plan, vec![(Kind::Sealed, id)]);
}

#[test]
fn a_socket_that_is_not_the_vaults_accounts_is_not_trusted() {
    let v = vault(Duration::from_secs(3600));
    match Client::connect(&v.socket, Some(me() + 1)) {
        Err(Error::Unreachable(m)) => {
            assert!(m.contains("not owned by the vault's account"), "{m}")
        }
        other => panic!("{:?}", other.err()),
    }
    // A path that is not a socket, and one that is missing.
    let scratch = tempfile::tempdir().unwrap();
    fs::write(scratch.path().join("plain"), b"x").unwrap();
    assert!(matches!(
        Client::connect(&scratch.path().join("plain"), None),
        Err(Error::Unreachable(_))
    ));
    assert!(matches!(
        Client::connect(&scratch.path().join("none"), None),
        Err(Error::Unreachable(_))
    ));
}

#[test]
fn a_store_folder_other_accounts_can_enter_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(Store::open(dir.path(), 1).is_err());
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(Store::open(dir.path(), 1).is_ok());
}

#[test]
fn an_over_long_line_and_a_bad_id_are_refused() {
    let v = vault(Duration::from_secs(3600));
    let mut c = Client::connect(&v.socket, None).unwrap();
    match c.has(Kind::Object, "../../etc/passwd") {
        Err(Error::Refused { word, .. }) => assert_eq!(word, "bad-request"),
        other => panic!("{other:?}"),
    }
    let mut raw = UnixStream::connect(&v.socket).unwrap();
    raw.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    raw.write_all(&vec![b'a'; 4000]).unwrap();
    let mut got = String::new();
    use std::io::Read;
    let _ = raw.read_to_string(&mut got);
    assert!(got.starts_with("err bad-request"), "{got}");
}
