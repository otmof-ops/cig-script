// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! `cig tin vault install|uninstall|gc|update` (#105, V3). The real thing
//! makes an account and a service and uses sudo, so it runs only in the VM;
//! `--dry-run` runs nothing and runs everywhere.

use super::common::{guarded, text, Sandbox};
use super::vault::{ctl, ids, in_vm, project, uid, SEAL};
use sha2::{Digest, Sha256};

fn sh(script: &str) -> std::process::Output {
    guarded(60, "sh").args(["-c", script]).output().unwrap()
}

fn vault_bin() -> String {
    std::env::var("CIG_VAULT_BIN").expect("CIG_VAULT_BIN")
}

#[test]
fn a_dry_run_prints_the_steps_and_changes_nothing() {
    let sb = Sandbox::new();
    // Any file will do as the daemon: nothing is run.
    let dummy = env!("CARGO_BIN_EXE_cig");
    for args in [
        vec!["tin", "vault", "install", "--dry-run", "--binary", dummy],
        vec!["tin", "vault", "uninstall", "--purge", "--dry-run"],
        vec!["tin", "vault", "update", "--dry-run", "--binary", dummy],
    ] {
        let out = sb.cig_with("", &SEAL, &args);
        assert!(out.status.success(), "{args:?}: {}", text(&out));
        assert!(text(&out).contains("sudo "), "{}", text(&out));
        assert!(!text(&out).contains("NOPASSWD"), "{}", text(&out));
    }
    let out = sb.cig_with("", &SEAL, &["tin", "vault", "uninstall", "--purge"]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "--purge needs --yes: {}",
        text(&out)
    );
}

#[test]
fn the_vault_installs_collects_updates_and_uninstalls() {
    if !in_vm() {
        return;
    }
    ctl("stop");
    let bin = vault_bin();
    let a = Sandbox::new();
    let out = a.cig_with("", &SEAL, &["tin", "vault", "install", "--binary", &bin]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        sh("systemctl is-active cig-vault").status.success(),
        "the service is not active"
    );
    let rule = sh("sudo cat /etc/sudoers.d/cig-vault");
    let rule = String::from_utf8_lossy(&rule.stdout).to_string();
    assert!(
        rule.contains(&format!("gc --uid {}", uid())) && !rule.contains("NOPASSWD"),
        "{rule}"
    );
    let mode = sh("sudo stat -c %a /etc/sudoers.d/cig-vault");
    assert_eq!(String::from_utf8_lossy(&mode.stdout).trim(), "440");
    // Never setuid: no file of the install carries the bit.
    let suid = sh("sudo find /usr/local/bin/cig-vault /var/lib/cig-vault -perm -4000");
    assert!(String::from_utf8_lossy(&suid.stdout).trim().is_empty());
    assert!(a.cig_with("", &SEAL, &["config", "vault"]).status.success());

    // Objects of a project in A, then a different project in B.
    project(&a);
    let out = a.cig_with("", &SEAL, &["run", "s.cig"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(!text(&out).contains("E724"), "{}", text(&out));
    let b = Sandbox::new();
    b.write("p/other.txt", "bee\n");
    b.write(
        "s.cig",
        "pack { \"./p\" }\nburn {\n  proc.run(\"sh\", [\"-c\", \"echo changed > other.txt\"], {cwd: \"p\", touches: [\"**\"]})\n}\n",
    );
    let out = b.cig_with("", &SEAL, &["run", "s.cig"]);
    assert!(out.status.success(), "{}", text(&out));
    let mine = hex::encode(Sha256::digest(b"bee\n"));
    let before = ids("o");
    assert!(before.contains(&mine) && before.len() > 1, "{before:?}");

    // The installed rule asks for a password (a hop must not be able to use
    // it); the VM has no one to type it, so the test adds its own passwordless
    // drop-in, after the real rule, and removes it at the end.
    let who = String::from_utf8_lossy(&sh("id -un").stdout)
        .trim()
        .to_string();
    let dropin = sh(&format!(
        "printf '{who} ALL=(_cigtin) NOPASSWD: /usr/local/bin/cig-vault gc *\\n' | sudo tee /etc/sudoers.d/zz-cig-vault-test >/dev/null && sudo chmod 0440 /etc/sudoers.d/zz-cig-vault-test && sudo visudo -cf /etc/sudoers.d/zz-cig-vault-test"
    ));
    assert!(dropin.status.success(), "{}", text(&dropin));
    // gc from a tin with no runs is refused (everything would be garbage).
    let empty = Sandbox::new();
    let out = empty.cig_with("", &SEAL, &["tin", "vault", "gc", "--now", "--apply"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert_eq!(ids("o"), before);
    // From B: the plan names A's objects and removes none.
    let out = b.cig_with("", &SEAL, &["tin", "vault", "gc", "--now"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("would be removed"), "{}", text(&out));
    assert_eq!(ids("o"), before);
    // --apply removes what no run of B names, and keeps what one does.
    let out = b.cig_with("", &SEAL, &["tin", "vault", "gc", "--now", "--apply"]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(ids("o"), vec![mine.clone()]);

    // update: a new daemon process, still answering.
    let pid = |_: ()| {
        String::from_utf8_lossy(&sh("systemctl show -p MainPID --value cig-vault").stdout)
            .trim()
            .to_string()
    };
    let old = pid(());
    let out = a.cig_with("", &SEAL, &["tin", "vault", "update", "--binary", &bin]);
    assert!(out.status.success(), "{}", text(&out));
    let new = pid(());
    assert!(new != old && new != "0", "{old} -> {new}");
    assert_eq!(ids("o"), vec![mine.clone()]);

    sh("sudo rm -f /etc/sudoers.d/zz-cig-vault-test");
    // uninstall keeps the store's objects; --purge removes them and the account.
    let out = a.cig_with("", &SEAL, &["tin", "vault", "uninstall"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(!sh("systemctl is-active cig-vault").status.success());
    assert!(!sh("test -e /etc/systemd/system/cig-vault.service")
        .status
        .success());
    assert!(!sh("test -e /etc/sudoers.d/cig-vault").status.success());
    let kept = sh(&format!("sudo ls /var/lib/cig-vault/u{}/o", uid()));
    assert!(
        !String::from_utf8_lossy(&kept.stdout).trim().is_empty(),
        "the objects went with the service"
    );
    assert!(sh("getent passwd _cigtin").status.success());
    let out = a.cig_with(
        "",
        &SEAL,
        &["tin", "vault", "uninstall", "--purge", "--yes"],
    );
    assert!(out.status.success(), "{}", text(&out));
    assert!(!sh("test -e /var/lib/cig-vault").status.success());
    assert!(!sh("getent passwd _cigtin").status.success());
}
