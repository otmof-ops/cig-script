// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! A command that needs the key while a rekey holds the tin waits for it, up
//! to `rekey_wait` seconds, and then says a rekey is running (E720).

use super::common::{text, Sandbox};
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::process::Stdio;
use std::time::Instant;

const SEAL: [(&str, &str); 1] = [("CIG_SEAL_KEY", "file")];

/// A rekey's grip on the tin: the exclusive lock on the keys folder's lock
/// file, taken here, the way `cig tin rekey` takes it.
fn rekey_holds(sb: &Sandbox) -> fs::File {
    let keys = sb.path().join(".cigscript-keys");
    fs::create_dir_all(&keys).unwrap();
    fs::set_permissions(&keys, fs::Permissions::from_mode(0o700)).unwrap();
    let f = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(keys.join("rekey.lock"))
        .unwrap();
    // SAFETY: flock on a descriptor this handle owns.
    assert_eq!(
        unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    f
}

fn sealing_project(sb: &Sandbox) {
    sb.write("p/secret.txt", "one\n");
    sb.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo two > secret.txt\"], {cwd: \"p\", touches: [\"**\"]})\n}\n",
    );
}

#[test]
fn a_command_waits_a_bounded_time_for_a_rekey_and_then_says_so() {
    let sb = Sandbox::new();
    sealing_project(&sb);
    let _held = rekey_holds(&sb);
    let started = Instant::now();
    let out = sb.cig_with("", &[SEAL[0], ("CIG_REKEY_WAIT", "2")], &["run", "s.cig"]);
    let took = started.elapsed().as_secs_f32();
    assert!(!out.status.success(), "{}", text(&out));
    let said = text(&out);
    assert!(
        said.contains("E720") && said.contains("a cig tin rekey is running"),
        "{said}"
    );
    assert!(said.contains("waited 2s"), "{said}");
    assert!((1.5..30.0).contains(&took), "waited {took}s");
    assert_eq!(
        sb.read("p/secret.txt"),
        "one\n",
        "a refused run touched the file"
    );
    // No waiting at all with rekey_wait = 0.
    let out = sb.cig_with("", &[SEAL[0], ("CIG_REKEY_WAIT", "0")], &["run", "s.cig"]);
    assert!(
        !out.status.success() && text(&out).contains("E720"),
        "{}",
        text(&out)
    );
}

#[test]
fn a_command_that_waited_goes_on_when_the_rekey_finishes() {
    let sb = Sandbox::new();
    sealing_project(&sb);
    let held = rekey_holds(&sb);
    let mut cmd = sb.command("", &[SEAL[0], ("CIG_REKEY_WAIT", "30")], &["run", "s.cig"]);
    cmd.stderr(Stdio::piped()).stdout(Stdio::null());
    let mut child = cmd.spawn().unwrap();
    let mut err = BufReader::new(child.stderr.take().unwrap());
    // The note that it is waiting is the signal that the lock is being waited for.
    let mut line = String::new();
    while !line.contains("is in progress") {
        line.clear();
        assert!(
            err.read_line(&mut line).unwrap() > 0,
            "cig ended without waiting"
        );
    }
    drop(held);
    let status = wait_within(&mut child, 60);
    let mut rest = String::new();
    let _ = err.read_to_string(&mut rest);
    assert!(status.success(), "{rest}");
    assert_eq!(sb.read("p/secret.txt"), "two\n");
}

fn wait_within(child: &mut std::process::Child, secs: u64) -> std::process::ExitStatus {
    match wait_timeout::ChildExt::wait_timeout(child, std::time::Duration::from_secs(secs)).unwrap()
    {
        Some(s) => s,
        None => {
            let _ = child.kill();
            panic!("cig did not finish in {secs}s");
        }
    }
}
