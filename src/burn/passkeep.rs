// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The passphrase keeper (#105, K1): `seal_key = passphrase`.
//!
//! For a machine with no keyring and no place to leave a key file. The
//! tin's master key is not stored anywhere: it is derived, each time, from
//! a passphrase with Argon2id. What is stored is the record of how
//! (`seal.kdf`: the algorithm, its cost parameters, a random salt, and a
//! check value derived from the key, never the key), so a wrong passphrase
//! is refused by name before it can fail a sealed object. The record sits
//! in the keys folder, and a copy of it beside the tin, so a restored tin
//! opens with the passphrase alone.
//!
//! The passphrase is read from the terminal (`/dev/tty`, no echo) or from
//! `--seal-key-fd N`. It is never read from an environment variable and
//! never from argv. Unix only: Windows has no descriptor to hand over.

use super::seal::{self, Key};
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Mutex;

/// The record's first line: its format.
const MAGIC: &str = "cig-kdf 1";
/// HKDF label for the check value: one key never does two jobs.
const LABEL_CHECK: &[u8] = b"passphrase-check";
/// The most a passphrase may be (bytes): a descriptor that streams on must
/// not fill memory.
pub const MAX_PASSPHRASE: u64 = 1024;
/// Floors a record must meet to be used: a record rewritten to cheap
/// parameters is refused rather than obeyed.
const MIN_M_KIB: u32 = 19 * 1024;
const MIN_T: u32 = 2;
const MAX_M_KIB: u32 = 4 * 1024 * 1024;
const MAX_T: u32 = 64;
const MAX_P: u32 = 16;

/// Argon2id cost parameters, recorded with the data they protect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cost {
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
}

/// What a new tin is made with: 64 MiB, 3 passes, one lane.
pub const DEFAULT_COST: Cost = Cost {
    m_kib: 64 * 1024,
    t: 3,
    p: 1,
};

/// The parameters a record carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub cost: Cost,
    pub salt: [u8; 16],
    pub check: [u8; 32],
}

/// The passphrase `--seal-key-fd` handed over, read once at start-up.
static GIVEN: Mutex<Option<Vec<u8>>> = Mutex::new(None);

/// Keep the descriptor's passphrase for the key derivation (the caller has
/// read it and closed the descriptor). The newline a `printf` or an editor
/// adds is not part of it.
pub fn give(mut text: Vec<u8>) -> Result<(), String> {
    while matches!(text.last(), Some(b'\n' | b'\r')) {
        text.pop();
    }
    if text.is_empty() {
        return Err("an empty passphrase protects nothing".to_string());
    }
    *GIVEN.lock().unwrap_or_else(|p| p.into_inner()) = Some(text);
    Ok(())
}

fn take_given() -> Option<Vec<u8>> {
    GIVEN.lock().unwrap_or_else(|p| p.into_inner()).take()
}

/// Was the passphrase handed over on a descriptor?
pub fn was_given() -> bool {
    GIVEN.lock().unwrap_or_else(|p| p.into_inner()).is_some()
}

pub fn record_path() -> PathBuf {
    seal::keys_dir().join("seal.kdf")
}

fn tin_copy_path() -> PathBuf {
    super::tin::dir().join("seal.kdf")
}

/// The Argon2id key for `pass` under `salt` and `cost`.
pub fn derive_master(pass: &[u8], salt: &[u8; 16], cost: Cost) -> Result<Key, String> {
    use argon2::{Algorithm, Argon2, Params, Version};
    let params = Params::new(cost.m_kib, cost.t, cost.p, Some(32))
        .map_err(|e| format!("the key record's parameters are not usable: {e}"))?;
    let mut out = [0u8; 32];
    let r = Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(pass, salt, &mut out)
        .map_err(|e| format!("deriving the key from the passphrase failed: {e}"));
    let key = Key::new(out);
    seal::wipe_bytes(&mut out);
    r.map(|()| key)
}

fn check_of(master: &Key) -> [u8; 32] {
    *seal::derive(master, LABEL_CHECK).as_bytes()
}

/// A new record and its master key for `pass`.
pub fn make_record(pass: &[u8], cost: Cost) -> Result<(Record, Key), String> {
    let mut salt = [0u8; 16];
    getrandom::getrandom(&mut salt).map_err(|e| format!("no randomness: {e}"))?;
    let master = derive_master(pass, &salt, cost)?;
    let check = check_of(&master);
    Ok((Record { cost, salt, check }, master))
}

/// The master key for `pass` under `rec`, or the refusal.
pub fn open_record(rec: &Record, pass: &[u8]) -> Result<Key, String> {
    let master = derive_master(pass, &rec.salt, rec.cost)?;
    if Key::new(check_of(&master)) == Key::new(rec.check) {
        Ok(master)
    } else {
        Err("that is not the passphrase this tin was sealed under".to_string())
    }
}

impl Record {
    pub fn render(&self) -> String {
        format!(
            "{MAGIC}\nalg=argon2id\nm={}\nt={}\np={}\nsalt={}\ncheck={}\n",
            self.cost.m_kib,
            self.cost.t,
            self.cost.p,
            hex::encode(self.salt),
            hex::encode(self.check)
        )
    }

    pub fn parse(text: &str) -> Result<Record, String> {
        let mut lines = text.lines();
        if lines.next().map(str::trim) != Some(MAGIC) {
            return Err("not a cig key record (or one from a newer cig)".to_string());
        }
        let (mut alg, mut m, mut t, mut p, mut salt, mut check) =
            (None, None, None, None, None, None);
        for l in lines {
            let Some((k, v)) = l.split_once('=') else {
                continue;
            };
            let v = v.trim();
            match k.trim() {
                "alg" => alg = Some(v.to_string()),
                "m" => m = v.parse::<u32>().ok(),
                "t" => t = v.parse::<u32>().ok(),
                "p" => p = v.parse::<u32>().ok(),
                "salt" => salt = hex::decode(v).ok(),
                "check" => check = hex::decode(v).ok(),
                _ => {}
            }
        }
        if alg.as_deref() != Some("argon2id") {
            return Err("the key record names a derivation this cig does not know".to_string());
        }
        let (Some(m_kib), Some(t), Some(p)) = (m, t, p) else {
            return Err("the key record is missing its cost parameters".to_string());
        };
        if !(MIN_M_KIB..=MAX_M_KIB).contains(&m_kib)
            || !(MIN_T..=MAX_T).contains(&t)
            || !(1..=MAX_P).contains(&p)
        {
            return Err(format!(
                "the key record's cost (m={m_kib} KiB, t={t}, p={p}) is outside what cig will use (at least {MIN_M_KIB} KiB and {MIN_T} passes); a record changed to cheaper parameters is refused"
            ));
        }
        let salt: [u8; 16] = salt
            .and_then(|s| s.try_into().ok())
            .ok_or("the key record's salt is not 16 bytes of hex")?;
        let check: [u8; 32] = check
            .and_then(|s| s.try_into().ok())
            .ok_or("the key record's check is not 32 bytes of hex")?;
        Ok(Record {
            cost: Cost { m_kib, t, p },
            salt,
            check,
        })
    }
}

fn read_record_at(path: &std::path::Path) -> Result<Option<Record>, String> {
    match fs::read_to_string(path) {
        Ok(t) => Record::parse(&t)
            .map(Some)
            .map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// The record: the keys folder's, else the copy beside the tin.
pub fn load_record() -> Result<Option<Record>, String> {
    if let Some(r) = read_record_at(&record_path())? {
        return Ok(Some(r));
    }
    read_record_at(&tin_copy_path())
}

fn write_new(path: &std::path::Path, text: &str) -> io::Result<()> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()
}

/// Write the record to the keys folder, and beside the tin when that
/// exists. Never replaces a record that is there.
fn store_record(rec: &Record) -> Result<(), String> {
    let text = rec.render();
    let keys = seal::ensure_keys_dir()?;
    let primary = keys.join("seal.kdf");
    if fs::symlink_metadata(&primary).is_err() {
        write_new(&primary, &text)
            .map_err(|e| format!("cannot write the key record {}: {e}", primary.display()))?;
    }
    keep_tin_copy(&text);
    Ok(())
}

/// The copy beside the tin, written once the tin is there, best effort.
fn keep_tin_copy(text: &str) {
    let copy = tin_copy_path();
    if copy.parent().is_some_and(|d| d.is_dir()) && fs::symlink_metadata(&copy).is_err() {
        let _ = write_new(&copy, text);
    }
}

/// The master key under `seal_key = passphrase`. `sealed_in_use` says the
/// machine holds sealed objects: then a missing record is an error, never
/// a new passphrase, so nothing is locked out behind the user's back.
pub fn key(sealed_in_use: bool) -> Result<Key, String> {
    match load_record()? {
        Some(rec) => {
            let pass = match take_given() {
                Some(p) => p,
                None => ask("passphrase for the tin: ", false)?,
            };
            let r = open_record(&rec, &pass);
            let mut pass = pass;
            seal::wipe_bytes(&mut pass);
            let master = r?;
            if let Ok(t) = fs::read_to_string(record_path()) {
                keep_tin_copy(&t);
            }
            Ok(master)
        }
        None => {
            if sealed_in_use {
                return Err(format!(
                    "the tin's passphrase record is gone ({} is not there, nor a copy beside the tin), and this machine holds sealed objects only that passphrase and its salt open; a new passphrase would lock them out for good. Put seal.kdf back from a backup, or move the sealed objects away to start over",
                    crate::crash::tilde(&record_path())
                ));
            }
            let (pass, confirm_needed) = match take_given() {
                Some(p) => (p, false),
                None => (ask("new passphrase for the tin: ", true)?, false),
            };
            let _ = confirm_needed;
            let mut pass = pass;
            let made = make_record(&pass, DEFAULT_COST);
            seal::wipe_bytes(&mut pass);
            let (rec, master) = made?;
            store_record(&rec)?;
            Ok(master)
        }
    }
}

/// Ask on the terminal, no echo; with `twice` the passphrase is typed
/// again and the two must agree.
#[cfg(unix)]
fn ask(prompt: &str, twice: bool) -> Result<Vec<u8>, String> {
    let first = read_tty(prompt)?;
    if twice {
        let mut again = read_tty("again: ")?;
        let same = first == again;
        seal::wipe_bytes(&mut again);
        if !same {
            let mut f = first;
            seal::wipe_bytes(&mut f);
            return Err("the two passphrases differ; nothing was made".to_string());
        }
    }
    Ok(first)
}

#[cfg(not(unix))]
fn ask(_prompt: &str, _twice: bool) -> Result<Vec<u8>, String> {
    Err("seal_key = passphrase needs Linux, macOS or the BSDs; on Windows use the keyring or a key file".to_string())
}

#[cfg(unix)]
fn read_tty(prompt: &str) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    use std::os::fd::AsRawFd as _;
    let mut tty = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .map_err(|_| {
            "seal_key = passphrase needs a terminal to ask on; hand the passphrase over with --seal-key-fd N (for example 3<pass.txt)".to_string()
        })?;
    let fd = tty.as_raw_fd();
    // SAFETY: tcgetattr fills a zeroed termios for an open terminal descriptor.
    let mut old: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut old) } != 0 {
        return Err("cannot read the terminal's settings, so the passphrase would echo; hand it over with --seal-key-fd N".to_string());
    }
    let mut quiet = old;
    quiet.c_lflag &= !libc::ECHO;
    // SAFETY: tcsetattr applies a termios struct the kernel just gave us.
    if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &quiet) } != 0 {
        return Err(
            "cannot turn the terminal's echo off; hand the passphrase over with --seal-key-fd N"
                .to_string(),
        );
    }
    let _ = tty.write_all(prompt.as_bytes());
    let mut bytes = Vec::with_capacity(64);
    let mut one = [0u8; 1];
    let outcome = loop {
        match (&mut tty).take(1).read(&mut one) {
            Ok(0) => break Ok(()),
            Ok(_) if one[0] == b'\n' || one[0] == b'\r' => break Ok(()),
            Ok(_) => {
                if bytes.len() as u64 >= MAX_PASSPHRASE {
                    break Err("that passphrase is longer than 1024 bytes".to_string());
                }
                bytes.push(one[0]);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => break Err(e.to_string()),
        }
    };
    // SAFETY: puts back the settings read above.
    unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &old) };
    let _ = tty.write_all(b"\n");
    if let Err(e) = outcome {
        seal::wipe_bytes(&mut bytes);
        return Err(e);
    }
    if bytes.is_empty() {
        return Err("an empty passphrase protects nothing".to_string());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHEAP: Cost = Cost {
        m_kib: MIN_M_KIB,
        t: MIN_T,
        p: 1,
    };

    #[test]
    fn a_record_opens_with_its_passphrase_and_refuses_another() {
        let (rec, master) = make_record(b"correct horse", CHEAP).unwrap();
        assert!(open_record(&rec, b"correct horse").unwrap() == master);
        let wrong = open_record(&rec, b"correct hors");
        assert!(wrong.is_err(), "a wrong passphrase must be refused");
        // The record never holds the key.
        assert!(!rec.render().contains(&hex::encode(master.as_bytes())));
    }

    #[test]
    fn a_record_round_trips_through_its_text() {
        let (rec, _) = make_record(b"pw", CHEAP).unwrap();
        assert_eq!(Record::parse(&rec.render()).unwrap(), rec);
    }

    #[test]
    fn a_record_with_cheaper_parameters_is_refused() {
        let (rec, _) = make_record(b"pw", CHEAP).unwrap();
        let cheap = rec.render().replace(&format!("m={}", MIN_M_KIB), "m=8");
        assert!(Record::parse(&cheap).unwrap_err().contains("cheaper"));
        let one_pass = rec.render().replace(&format!("t={}", MIN_T), "t=1");
        assert!(Record::parse(&one_pass).is_err());
        assert!(Record::parse("cig-kdf 9\n").is_err());
    }

    #[test]
    fn the_same_passphrase_under_another_salt_is_another_key() {
        let (_, a) = make_record(b"pw", CHEAP).unwrap();
        let (_, b) = make_record(b"pw", CHEAP).unwrap();
        assert!(a != b);
    }

    #[test]
    fn a_given_passphrase_loses_its_newline_and_an_empty_one_is_refused() {
        let _g = super::super::test_env_lock();
        assert!(give(b"\n".to_vec()).is_err());
        give(b"abc\r\n".to_vec()).unwrap();
        assert_eq!(take_given().unwrap(), b"abc");
    }
}
