// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The issue's named tests that tests/cli.rs does not already carry under
//! those names (#105, K4). `a_loose_state_directory_warns_unattended_and_refuses_on_a_terminal`
//! in tests/cli.rs covers the terminal and the plain unattended case; the
//! CI marker, a second account and a journal from a newer cig are here.

use super::common::{guarded, text, Sandbox};
use std::fs;
use std::os::unix::fs::PermissionsExt;

// Plain journals: these tests read and edit `journal.jsonl` by hand.
const SEAL: [(&str, &str); 2] = [("CIG_SEAL_KEY", "file"), ("CIG_JOURNAL_ZST", "off")];

fn burn(sb: &Sandbox) -> String {
    sb.write("a.txt", "one\n");
    sb.write(
        "s.cig",
        "pack { \".\" }\nburn {\n  fs.write_text(\"a.txt\", \"two\")\n}\n",
    );
    let out = sb.cig_with("", &SEAL, &["run", "s.cig"]);
    assert!(out.status.success(), "{}", text(&out));
    fs::read_dir(sb.home().join("runs"))
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .file_name()
        .to_string_lossy()
        .to_string()
}

#[test]
fn a_loose_tin_is_refused_on_a_terminal_and_warned_in_ci() {
    let sb = Sandbox::new();
    fs::create_dir_all(sb.home()).unwrap();
    sb.write("s.cig", "burn {\n  fs.write_text(\"a.txt\", \"1\")\n}\n");
    let loose = || fs::set_permissions(sb.home(), fs::Permissions::from_mode(0o777)).unwrap();
    // In CI (the marker set, whatever else): a warning and the run goes on.
    loose();
    let out = sb.cig_with(
        "",
        &[("CI", "true"), ("CIG_SEAL_KEY", "file")],
        &["run", "s.cig"],
    );
    assert!(
        out.status.success(),
        "a loose tin broke a CI run: {}",
        text(&out)
    );
    assert!(text(&out).contains("E707"), "{}", text(&out));
    // On a terminal: refused, with the fix as one command.
    loose();
    let out = sb.cig_with(
        "",
        &[("CIG_INTERNAL_ATTENDED", "1"), ("CIG_SEAL_KEY", "file")],
        &["run", "s.cig"],
    );
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        text(&out).contains("E707") && text(&out).contains("chmod 700"),
        "{}",
        text(&out)
    );
    // `tin_perms = off` never looks.
    loose();
    let out = sb.cig_with(
        "",
        &[("CIG_INTERNAL_ATTENDED", "1"), ("CIG_TIN_PERMS", "off")],
        &["run", "s.cig"],
    );
    assert!(out.status.success(), "{}", text(&out));
}

#[test]
fn an_older_cig_refuses_format_3_clearly() {
    // What an older cig does with a journal written in a format it does not
    // know is what this cig does with one newer than it reads (format 4
    // here): it stops, names the format and the version that wrote it, and
    // restores nothing. (`runs::newer_format` is the one refusal for both.)
    let sb = Sandbox::new();
    let id = burn(&sb);
    let path = sb.home().join("runs").join(&id).join("journal.jsonl");
    let body = fs::read_to_string(&path).unwrap();
    let (first, rest) = body.split_once('\n').unwrap();
    let mut header: serde_json::Value = serde_json::from_str(first).unwrap();
    header["format"] = serde_json::Value::from(4);
    fs::write(&path, format!("{header}\n{rest}")).unwrap();
    let out = sb.cig_with("", &SEAL, &["unburn", &id]);
    assert!(!out.status.success(), "{}", text(&out));
    let said = text(&out).to_lowercase();
    assert!(
        said.contains("is format 4") && said.contains("reads up to format"),
        "{said}"
    );
    assert_eq!(sb.read("a.txt"), "two", "a refused unburn touched the file");
}

#[test]
fn another_user_cannot_read_the_tin() {
    if std::env::var("CIG_VAULT_VM").as_deref() != Ok("1") {
        eprintln!("skipped: needs a second account and sudo (the VM: tests/attack/run-in-vm.sh)");
        return;
    }
    let sb = Sandbox::new();
    let id = burn(&sb);
    // The sandbox's own folder is opened so that the only door left is the
    // state directory's mode.
    fs::set_permissions(sb.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let home = sb.home();
    assert_eq!(
        fs::metadata(&home).unwrap().permissions().mode() & 0o077,
        0,
        "the tin is not private"
    );
    let as_other = |args: &[&str]| {
        guarded(30, "sudo")
            .args(["-n", "-u", "nobody"])
            .args(args)
            .output()
            .unwrap()
    };
    let h = home.to_string_lossy().to_string();
    assert!(!as_other(&["ls", &h]).status.success());
    assert!(!as_other(&["cat", &format!("{h}/runs/{id}/journal.jsonl")])
        .status
        .success());
    assert!(!as_other(&["sh", "-c", &format!("echo x > {h}/planted")])
        .status
        .success());
    // The key folder is private too.
    let keys = sb.path().join(".cigscript-keys");
    let k = keys.to_string_lossy().to_string();
    assert!(!as_other(&["ls", &k]).status.success());
}
