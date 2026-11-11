// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! cig's side of the vault (#105, L7).
//!
//! The vault is a store that runs as its own account (`_cigtin`) and has no
//! delete, so nothing that runs as you can remove what it keeps. When it is
//! installed, every object the tin commits is also handed to it, as an open
//! file descriptor over the vault's socket (the vault hashes the bytes
//! itself). When the vault does not answer, the run goes on from the tin
//! alone, says so once (`E724`), and writes down what the vault is missing
//! in `tin/vault-pending`; the next time the vault answers, that is handed
//! over first. When a tin object is missing or no longer hashes to its name
//! at restore, it is fetched back from the vault.
//!
//! Nothing here can delete from the vault: the protocol has no such request.

#[cfg(unix)]
pub use imp::*;

#[cfg(not(unix))]
pub use stub::*;

/// The tin's three kinds of object, in the vault's naming.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VKind {
    /// A whole small file or a chunk, named by its hash.
    Object,
    /// A chunked file's recipe.
    Recipe,
    /// A sealed object, named by its random id.
    Sealed,
}

/// What a committed stash gave the tin: the kind of object, its id, and the
/// path in the tin it was committed to.
pub type Item = (VKind, String, std::path::PathBuf);

#[cfg(not(unix))]
mod stub {
    use std::path::Path;
    /// The vault runs on Unix; elsewhere the tin is all there is.
    pub fn keep_all(_items: &[super::Item]) {}
    pub fn heal(_snapshot: &Path) -> bool {
        false
    }
}

#[cfg(unix)]
mod imp {
    use cig_vault::client::{Client, Error};
    use cig_vault::{Kind, ACCOUNT, DEFAULT_SOCKET};
    use sha2::{Digest, Sha256};
    use std::ffi::CString;
    use std::fs;
    use std::io::{Read, Write};
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, OnceLock};

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mode {
        Auto,
        On,
        Off,
    }

    enum State {
        /// Not asked yet this run.
        Fresh,
        Up(Box<Client>),
        /// Asked and not answering: no more tries this run.
        Down,
        /// Not installed, or switched off.
        None,
    }

    static SESSION: Mutex<State> = Mutex::new(State::Fresh);
    static NOTED: OnceLock<()> = OnceLock::new();

    fn mode() -> Mode {
        match crate::config::Config::load().get("vault").as_str() {
            "off" => Mode::Off,
            "on" => Mode::On,
            _ => Mode::Auto,
        }
    }

    fn socket_path() -> PathBuf {
        let s = crate::config::Config::load().get("vault_socket");
        if s.is_empty() {
            PathBuf::from(DEFAULT_SOCKET)
        } else {
            PathBuf::from(s)
        }
    }

    /// The vault account's uid: its existence is what "installed" means.
    fn account_uid() -> Option<u32> {
        static UID: OnceLock<Option<u32>> = OnceLock::new();
        *UID.get_or_init(|| {
            let name = CString::new(ACCOUNT).ok()?;
            // SAFETY: getpwnam reads the account database; the result is
            // read at once, before any other call could reuse its buffer.
            let pw = unsafe { libc::getpwnam(name.as_ptr()) };
            if pw.is_null() {
                None
            } else {
                Some(unsafe { (*pw).pw_uid })
            }
        })
    }

    fn note_once(text: &str) {
        if NOTED.set(()).is_ok() {
            eprintln!("note[E724 burn]: {text}");
        }
    }

    fn pending_path() -> PathBuf {
        super::super::tin::dir().join("vault-pending")
    }

    fn path_of(kind: Kind, id: &str) -> PathBuf {
        match kind {
            Kind::Object => super::super::tin::object_path(id),
            Kind::Recipe => super::super::tin::recipe_path(id),
            Kind::Sealed => super::super::tin::sealed_dir().join(&id[..2]).join(id),
        }
    }

    /// Run `f` against the vault's connection, connecting (and handing over
    /// what waited) the first time. `None` when there is no vault to ask:
    /// not installed, off, or not answering (said once).
    fn with_client<T>(f: impl FnOnce(&mut Client) -> Result<T, Error>) -> Option<Result<T, Error>> {
        let mut g = SESSION.lock().unwrap_or_else(|p| p.into_inner());
        if matches!(*g, State::Fresh) {
            *g = connect();
        }
        let State::Up(c) = &mut *g else {
            return None;
        };
        let r = f(c);
        if let Err(Error::Unreachable(why)) = &r {
            note_once(&format!(
                "the vault stopped answering ({why}); the rest of this run is kept in the tin and handed to the vault the next time it answers"
            ));
            *g = State::Down;
        }
        Some(r)
    }

    fn connect() -> State {
        let m = mode();
        if m == Mode::Off {
            return State::None;
        }
        let Some(uid) = account_uid() else {
            if m == Mode::On {
                note_once(&format!(
                    "vault is on but no {ACCOUNT} account exists on this machine; the tin is all there is (cig tin vault install)"
                ));
            }
            return State::None;
        };
        match Client::connect(&socket_path(), Some(uid)) {
            Ok(mut c) => {
                catch_up(&mut c);
                State::Up(Box::new(c))
            }
            Err(e) => {
                note_once(&format!(
                    "the vault did not answer ({e}); this run is kept in the tin and handed to the vault the next time it answers"
                ));
                State::Down
            }
        }
    }

    /// Write down what the vault is missing.
    fn queue(kind: Kind, id: &str) {
        let p = pending_path();
        let mut opts = fs::OpenOptions::new();
        opts.append(true).create(true).mode(0o600);
        if let Ok(mut f) = opts.open(p) {
            let _ = writeln!(f, "{} {id}", kind.letter());
        }
    }

    /// Hand over what waited in `tin/vault-pending`, objects and sealed
    /// ones before recipes (a recipe is refused until its chunks are there).
    /// Stops at the first failure and keeps what is left.
    fn catch_up(c: &mut Client) {
        let p = pending_path();
        let Ok(text) = fs::read_to_string(&p) else {
            return;
        };
        let mut want: Vec<(Kind, String)> = text
            .lines()
            .filter_map(|l| {
                let (k, id) = l.split_once(' ')?;
                let k = Kind::parse(k)?;
                k.id_ok(id).then(|| (k, id.to_string()))
            })
            .collect();
        want.sort();
        want.dedup();
        want.sort_by_key(|(k, _)| *k == Kind::Recipe);
        let (mut handed, mut left) = (0usize, Vec::new());
        let mut stopped = false;
        for (k, id) in want {
            if stopped {
                left.push((k, id));
                continue;
            }
            let Ok(f) = fs::File::open(path_of(k, &id)) else {
                // Gone from the tin (a gc): nothing to hand over.
                continue;
            };
            match c.store(k, &id, &f) {
                Ok(_) => handed += 1,
                Err(e) if e.transient() => {
                    stopped = true;
                    left.push((k, id));
                }
                Err(_) => {}
            }
        }
        if left.is_empty() {
            let _ = fs::remove_file(&p);
        } else {
            let body: String = left
                .iter()
                .map(|(k, id)| format!("{} {id}\n", k.letter()))
                .collect();
            let _ = fs::write(&p, body);
        }
        if handed > 0 {
            eprintln!(
                "note: handed {handed} object{} the vault missed to the vault",
                if handed == 1 { "" } else { "s" }
            );
        }
    }

    /// Give the vault what was just committed to the tin, in order (chunks
    /// before the recipe). A vault that cannot take it leaves it queued.
    pub fn keep_all(items: &[super::Item]) {
        for (vkind, id, path) in items {
            let kind = &match vkind {
                super::VKind::Object => Kind::Object,
                super::VKind::Recipe => Kind::Recipe,
                super::VKind::Sealed => Kind::Sealed,
            };
            let r = with_client(|c| {
                let f = fs::File::open(path).map_err(|e| Error::Refused {
                    word: "io".to_string(),
                    msg: format!("{}: {e}", path.display()),
                })?;
                c.store(*kind, id, &f)
            });
            match r {
                // Not installed or off.
                None if matches!(
                    *SESSION.lock().unwrap_or_else(|p| p.into_inner()),
                    State::None
                ) => {}
                None => queue(*kind, id),
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    if e.transient() {
                        queue(*kind, id);
                    }
                    note_once(&format!(
                        "the vault did not keep {} {} ({e}); it is in the tin only",
                        kind.letter(),
                        &id[..12]
                    ));
                }
            }
        }
    }

    fn tmp_beside(p: &Path) -> PathBuf {
        let mut s = p.as_os_str().to_os_string();
        s.push(".vault.tmp");
        PathBuf::from(s)
    }

    /// Put the vault's copy of `snapshot` (a tin object, recipe or sealed
    /// object) back in the tin when the tin's own is missing or differs,
    /// for a restore that just failed on it. A recipe's chunks are checked
    /// too. `true` when something was put back.
    pub fn heal(snapshot: &Path) -> bool {
        let Some((kind, id)) = identify(snapshot) else {
            return false;
        };
        let mut healed = fetch_into_place(kind, &id);
        if kind == Kind::Recipe {
            if let Ok(r) = super::super::tin::read_recipe(snapshot) {
                for c in r.chunks {
                    let o = super::super::tin::object_path(&c.sha256);
                    let ok = fs::metadata(&o).map(|m| m.len()).ok() == Some(c.len)
                        && super::super::journal::sha256_file(&o).ok().as_deref()
                            == Some(&c.sha256);
                    if !ok {
                        healed |= fetch_into_place(Kind::Object, &c.sha256);
                    }
                }
            }
        }
        healed
    }

    fn identify(p: &Path) -> Option<(Kind, String)> {
        let id = p.file_name()?.to_string_lossy().to_string();
        let kind = if p.starts_with(super::super::tin::sealed_dir()) {
            Kind::Sealed
        } else if super::super::tin::is_recipe(p) {
            Kind::Recipe
        } else if p.starts_with(super::super::tin::objects_root()) {
            Kind::Object
        } else {
            return None;
        };
        kind.id_ok(&id).then_some((kind, id))
    }

    /// Fetch `kind`/`id` beside its place in the tin; when it differs from
    /// what is there (or nothing is), put it in place. `true` if placed.
    fn fetch_into_place(kind: Kind, id: &str) -> bool {
        let dest = path_of(kind, id);
        let tmp = tmp_beside(&dest);
        let Some(parent) = dest.parent() else {
            return false;
        };
        let _ = fs::create_dir_all(parent);
        let _ = fs::remove_file(&tmp);
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true).mode(0o600);
        let Ok(out) = opts.open(&tmp) else {
            return false;
        };
        let got = with_client(|c| c.fetch(kind, id, &out));
        drop(out);
        let placed = (|| {
            if !matches!(got, Some(Ok(_))) {
                return false;
            }
            let Ok(bytes) = fs::read(&tmp) else {
                return false;
            };
            // The vault checked it; it is checked again here, because what
            // goes back in the tin is what a restore will trust.
            if kind == Kind::Object && hex::encode(Sha256::digest(&bytes)) != id {
                return false;
            }
            if let Ok(mut have) = fs::File::open(&dest) {
                let mut cur = Vec::new();
                if have.read_to_end(&mut cur).is_ok() && cur == bytes {
                    return false;
                }
            }
            if super::super::tin::make_read_only(&tmp).is_err() {
                return false;
            }
            if dest.exists() {
                super::super::tin::lift_read_only(&dest);
                let _ = fs::remove_file(&dest);
            }
            fs::rename(&tmp, &dest).is_ok()
        })();
        let _ = fs::remove_file(&tmp);
        if placed {
            eprintln!(
                "note: {} {} was put back in the tin from the vault",
                kind.letter(),
                &id[..12]
            );
        }
        placed
    }
}
