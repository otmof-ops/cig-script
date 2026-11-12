// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The vault, as its own account (#105, L7). These need the `_cigtin`
//! account and `cig-vault serve` running as it, so they run only in the
//! isolated VM (`tests/attack/run-in-vm.sh` sets `CIG_VAULT_VM=1`); anywhere
//! else they say so and pass.

use super::common::{text, Sandbox};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::{Command, Output};
use std::time::Duration;

const STORE: &str = "/var/lib/cig-vault";
const SOCKET: &str = "/run/cig-vault/vault.sock";
pub(super) const SEAL: [(&str, &str); 1] = [("CIG_SEAL_KEY", "file")];

pub(super) fn in_vm() -> bool {
    if std::env::var("CIG_VAULT_VM").as_deref() == Ok("1") {
        return true;
    }
    eprintln!("skipped: the vault tests run in the VM only (tests/attack/run-in-vm.sh)");
    false
}

pub(super) fn ctl(verb: &str) {
    let script = std::env::var("CIG_VAULT_CTL").expect("CIG_VAULT_CTL");
    let out = Command::new("timeout")
        .args(["60", &script, verb])
        .output()
        .expect("vault-ctl");
    assert!(out.status.success(), "vault-ctl {verb}: {}", text(&out));
}

fn vault_bin() -> &'static str {
    "/usr/local/bin/cig-vault"
}

pub(super) fn ask(args: &[&str]) -> Output {
    Command::new("timeout")
        .arg("30")
        .arg(vault_bin())
        .arg("ask")
        .args(args)
        .output()
        .expect("cig-vault ask")
}

pub(super) fn ids(kind: &str) -> Vec<String> {
    let out = ask(&["list", kind]);
    assert!(out.status.success(), "{}", text(&out));
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split(' ').next().map(str::to_string))
        .collect()
}

/// The size `project` writes for the large file: above `CHUNK_FILE_MIN`
/// (2 MiB; `src/burn/tin.rs`), so the tin keeps it as chunks under a
/// recipe, the same choice as `tin.rs`'s own tests.
const BIG_BYTES: usize = 3 << 20;

/// A pack with a small file, a secret and a large file (which the tin keeps
/// as chunks under a recipe): all three kinds of object.
pub(super) fn project(sb: &Sandbox) {
    sb.write("p/data.txt", "one\n");
    sb.write("p/.env", "SECRET=one\n");
    let big: Vec<u8> = (0..BIG_BYTES as u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
        .collect();
    fs::write(sb.path().join("p/big.bin"), &big).unwrap();
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo two > data.txt; echo SECRET=two > .env; printf 'small now' > big.bin\"], {cwd: \"p\", touches: [\"**\"]})\n}\n",
    );
}

fn first_run(sb: &Sandbox) -> String {
    let runs = fs::read_dir(sb.home().join("runs")).unwrap();
    let mut names: Vec<_> = runs
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names.remove(0)
}

fn run_script(sb: &Sandbox, env: &[(&str, &str)]) -> Output {
    let mut e = SEAL.to_vec();
    e.extend_from_slice(env);
    sb.cig_with("", &e, &["run", "s.cig"])
}

pub(super) fn uid() -> u32 {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() }
}

fn line_from(sock: &mut UnixStream) -> String {
    let mut line = String::new();
    let mut one = [0u8; 1];
    while sock.read(&mut one).unwrap_or(0) == 1 && one[0] != b'\n' {
        line.push(one[0] as char);
    }
    line
}

#[test]
fn the_vault_has_no_delete() {
    if !in_vm() {
        return;
    }
    ctl("stop");
    ctl("start");
    let sb = Sandbox::new();
    project(&sb);
    let out = run_script(&sb, &[]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(!text(&out).contains("E724"), "{}", text(&out));
    let (objects, recipes, sealed) = (ids("o"), ids("r"), ids("s"));
    assert!(
        !objects.is_empty() && !recipes.is_empty() && !sealed.is_empty(),
        "the vault should hold all three kinds: {objects:?} {recipes:?} {sealed:?}"
    );

    // No request removes anything: every verb that could is refused.
    let mut raw = UnixStream::connect(SOCKET).unwrap();
    raw.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    for verb in [
        "delete",
        "remove",
        "rm",
        "unlink",
        "purge",
        "prune",
        "forget",
        "retire",
        "expire",
        "truncate",
        "overwrite",
    ] {
        raw.write_all(format!("{verb} o {}\n", objects[0]).as_bytes())
            .unwrap();
        assert!(line_from(&mut raw).starts_with("err bad-request"), "{verb}");
    }
    // `gc` over the socket reports; it removes nothing, even for an empty live set.
    raw.write_all(b"gc 0\n").unwrap();
    assert!(line_from(&mut raw).starts_with("ok"));
    assert_eq!(ids("o"), objects);
    assert_eq!(ids("r"), recipes);
    assert_eq!(ids("s"), sealed);
    // The administrator's gc refuses an empty live set.
    let out = Command::new("timeout")
        .args([
            "30",
            "sudo",
            "-n",
            "-u",
            "_cigtin",
            vault_bin(),
            "gc",
            "--uid",
            &uid().to_string(),
            "--apply",
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success(), "{}", text(&out));
    assert_eq!(ids("o"), objects);

    // A direct write, or a delete, in the store as the user fails.
    let mine = format!("{STORE}/u{}/o/{}/{}", uid(), &objects[0][..2], objects[0]);
    assert!(fs::read_dir(STORE).is_err(), "the user can list the store");
    assert!(fs::read(&mine).is_err(), "the user can read the store");
    assert!(fs::write(&mine, b"overwritten").is_err());
    assert!(fs::remove_file(&mine).is_err());
    assert!(fs::write(format!("{STORE}/planted"), b"x").is_err());
    assert!(fs::create_dir(format!("{STORE}/u{}/o/planted", uid())).is_err());
    assert_eq!(ids("o"), objects);

    // The same attack on the tin (which the user owns) succeeds, and the
    // restore is still whole: the vault's copies come back.
    let id = first_run(&sb);
    for sub in ["objects", "recipes", "sealed"] {
        let _ = fs::remove_dir_all({
            let d = sb.home().join("tin").join(sub);
            // Objects are read-only: a script as the user lifts that first.
            let _ = Command::new("chmod").args(["-R", "u+rwx"]).arg(&d).status();
            d
        });
    }
    let out = sb.cig_with("", &SEAL, &["unburn", &id]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(sb.read("p/data.txt"), "one\n");
    assert_eq!(sb.read("p/.env"), "SECRET=one\n");
    assert_eq!(
        fs::metadata(sb.path().join("p/big.bin")).unwrap().len(),
        BIG_BYTES as u64
    );
    assert!(text(&out).contains("from the vault"), "{}", text(&out));

    // A socket the user made is not the vault: cig sends it nothing.
    let fake = sb.path().join("fake.sock");
    let l = UnixListener::bind(&fake).unwrap();
    l.set_nonblocking(true).unwrap();
    let sb2 = Sandbox::new();
    project(&sb2);
    let out = run_script(&sb2, &[("CIG_VAULT_SOCKET", fake.to_str().unwrap())]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("E724") && text(&out).contains("not owned by the vault's account"),
        "{}",
        text(&out)
    );
    assert!(
        l.accept().is_err(),
        "cig connected to a socket the vault does not own"
    );
}

#[test]
fn a_stopped_vault_falls_back_and_catches_up() {
    if !in_vm() {
        return;
    }
    ctl("stop");
    let sb = Sandbox::new();
    project(&sb);
    assert!(
        ids_or_none("o").is_none(),
        "the vault answered while it was stopped"
    );
    // The vault is stopped: the run goes on, says so once, and remembers.
    let out = run_script(&sb, &[]);
    assert!(out.status.success(), "{}", text(&out));
    let said = text(&out);
    assert_eq!(said.matches("E724").count(), 1, "{said}");
    assert!(
        said.contains("handed to the vault the next time it answers"),
        "{said}"
    );
    let pending = sb.home().join("tin").join("vault-pending");
    let queued = fs::read_to_string(&pending).expect("tin/vault-pending");
    assert!(
        queued.lines().any(|l| l.starts_with("o "))
            && queued.lines().any(|l| l.starts_with("r "))
            && queued.lines().any(|l| l.starts_with("s ")),
        "{queued}"
    );
    // From the tin alone, the run still unburns.
    let id = first_run(&sb);
    let out = sb.cig_with("", &SEAL, &["unburn", &id]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(sb.read("p/data.txt"), "one\n");
    assert_eq!(sb.read("p/.env"), "SECRET=one\n");
    assert!(pending.exists(), "the wait list went with the unburn");

    // The vault is back: the next run hands over what waited, first.
    ctl("start");
    let out = run_script(&sb, &[]);
    assert!(out.status.success(), "{}", text(&out));
    let said = text(&out);
    assert!(!said.contains("E724"), "{said}");
    assert!(said.contains("the vault missed to the vault"), "{said}");
    assert!(!pending.exists(), "the wait list was not emptied");
    let kept: Vec<String> = queued
        .lines()
        .filter_map(|l| l.split(' ').nth(1).map(str::to_string))
        .collect();
    let held: Vec<String> = ["o", "r", "s"].iter().flat_map(|k| ids(k)).collect();
    for id in &kept {
        assert!(held.contains(id), "{id} did not reach the vault");
    }
}

/// The vault's list when it is up, `None` when it is not answering.
fn ids_or_none(kind: &str) -> Option<Vec<String>> {
    let out = ask(&["list", kind]);
    out.status.success().then(|| {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.split(' ').next().map(str::to_string))
            .collect()
    })
}
