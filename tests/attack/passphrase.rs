// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! `seal_key = passphrase` (#105, K1): a run sealed under a passphrase
//! restores with it and refuses a wrong one.

use super::common::{text, Sandbox};
use std::fs;
use std::path::Path;

const ENV: [(&str, &str); 1] = [("CIG_SEAL_KEY", "passphrase")];

fn any_sealed(dir: &Path) -> bool {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.to_string_lossy().ends_with(".cig-sealed") {
                return true;
            }
        }
    }
    false
}

#[test]
fn a_run_sealed_under_a_passphrase_restores_with_it_and_refuses_a_wrong_one() {
    let sb = Sandbox::new();
    sb.write("good.pass", "correct horse battery staple\n");
    sb.write("bad.pass", "correct horse battery stapl\n");
    sb.write(".env", "SECRET=one\n");
    sb.write(
        "s.cig",
        "pack { \".\" }\nburn {\n  fs.write_text(\".env\", \"SECRET=two\")\n}\n",
    );
    let out = sb.cig_with("3<good.pass", &ENV, &["--seal-key-fd", "3", "run", "s.cig"]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(sb.read(".env"), "SECRET=two");
    assert!(any_sealed(&sb.home()), "nothing was sealed");

    // No key is stored: no key file, and the record holds no key.
    let keys = sb.path().join(".cigscript-keys");
    assert!(!keys.join("seal.key").exists(), "a key file was made");
    let record = fs::read_to_string(keys.join("seal.kdf")).expect("the key record");
    assert!(
        record.starts_with("cig-kdf 1\nalg=argon2id\nm="),
        "{record}"
    );
    assert!(!record.contains("correct horse"), "{record}");

    let id = {
        let runs = fs::read_dir(sb.home().join("runs")).unwrap();
        runs.flatten()
            .next()
            .unwrap()
            .file_name()
            .to_string_lossy()
            .to_string()
    };

    // A wrong passphrase is refused, by name, and nothing is restored.
    let out = sb.cig_with("3<bad.pass", &ENV, &["--seal-key-fd", "3", "unburn", &id]);
    assert!(!out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("not the passphrase this tin was sealed under"),
        "{}",
        text(&out)
    );
    assert_eq!(sb.read(".env"), "SECRET=two");

    // The environment is not a way in: CIG_KEY_PASSPHRASE opens nothing,
    // and with no terminal and no descriptor there is nothing to ask on.
    let out = sb.cig_with(
        "",
        &[
            ("CIG_SEAL_KEY", "passphrase"),
            ("CIG_KEY_PASSPHRASE", "correct horse battery staple"),
        ],
        &["unburn", &id],
    );
    assert!(!out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("--seal-key-fd"), "{}", text(&out));
    assert_eq!(sb.read(".env"), "SECRET=two");

    // The right one puts the run back.
    let out = sb.cig_with("3<good.pass", &ENV, &["--seal-key-fd", "3", "unburn", &id]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(sb.read(".env"), "SECRET=one\n");
}

#[test]
fn a_lost_passphrase_record_is_never_replaced_behind_the_users_back() {
    let sb = Sandbox::new();
    sb.write("pw", "first passphrase\n");
    sb.write("other", "second passphrase\n");
    sb.write(".env", "SECRET=one\n");
    sb.write(
        "s.cig",
        "pack { \".\" }\nburn {\n  fs.write_text(\".env\", \"SECRET=two\")\n}\n",
    );
    let out = sb.cig_with("3<pw", &ENV, &["--seal-key-fd", "3", "run", "s.cig"]);
    assert!(out.status.success(), "{}", text(&out));
    let keys = sb.path().join(".cigscript-keys");
    fs::remove_file(keys.join("seal.kdf")).unwrap();
    // The copy beside the tin, if one was made, goes too: the record is gone.
    let _ = fs::remove_file(sb.home().join("tin").join("seal.kdf"));
    let out = sb.cig_with("3<other", &ENV, &["--seal-key-fd", "3", "run", "s.cig"]);
    assert!(!out.status.success(), "{}", text(&out));
    assert!(
        !keys.join("seal.kdf").exists(),
        "a new record was made over sealed objects"
    );
}
