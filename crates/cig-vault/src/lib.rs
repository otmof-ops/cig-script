// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The vault protocol, version 1.
//!
//! One request at a time over a stream socket, each a line of ASCII ending
//! in `\n` (at most [`MAX_LINE`] bytes), answered by one line: `ok ...` or
//! `err <word> <message>`. A request that names a file (`store`, `fetch`)
//! carries one open file descriptor with it (`SCM_RIGHTS`): the vault reads
//! the bytes of a `store` from it and hashes them itself, and writes the
//! bytes of a `fetch` into it. It never opens a path a client names.
//!
//! ```text
//! hello 1                      ok cig-vault 1 <version>
//! store <kind> <id> <len>  fd  ok stored | ok present
//! fetch <kind> <id>        fd  ok <len>
//! has <kind> <id>              ok yes | ok no
//! verify <kind> <id>           ok <len>   (rehashed from the store)
//! list <kind>                  ok <n>, then n lines "<id> <len>"
//! gc <n>, then n lines "<kind> <id>"
//!                              ok <k>, then k lines "<kind> <id>": the
//!                              objects no live id names and older than the
//!                              grace period. Nothing is removed.
//! bye                          (connection closes)
//! ```
//!
//! There is no request that removes an object. Removing is the
//! administrator's: `cig-vault gc --apply`, run as the vault's account.

/// The protocol version this crate speaks.
pub const VERSION: u32 = 1;
/// The longest request or reply line, in bytes.
pub const MAX_LINE: usize = 1024;
/// The vault's account.
pub const ACCOUNT: &str = "_cigtin";
/// The most one object may be (bytes).
pub const MAX_OBJECT: u64 = 8 * 1024 * 1024 * 1024;
/// The most a recipe may be (bytes).
pub const MAX_RECIPE: u64 = 16 * 1024 * 1024;

#[cfg(target_os = "macos")]
pub const DEFAULT_ROOT: &str = "/var/db/cig-vault";
#[cfg(not(target_os = "macos"))]
pub const DEFAULT_ROOT: &str = "/var/lib/cig-vault";
#[cfg(target_os = "macos")]
pub const DEFAULT_SOCKET: &str = "/var/run/cig-vault/vault.sock";
#[cfg(not(target_os = "macos"))]
pub const DEFAULT_SOCKET: &str = "/run/cig-vault/vault.sock";

/// What an object is, in the tin's own naming.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    /// A whole small file or one chunk: id = SHA-256 of the bytes.
    Object,
    /// A chunked file's recipe: id = SHA-256 of the whole file.
    Recipe,
    /// A sealed object: id = a random 32-hex name.
    Sealed,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Object, Kind::Recipe, Kind::Sealed];

    pub fn letter(self) -> char {
        match self {
            Kind::Object => 'o',
            Kind::Recipe => 'r',
            Kind::Sealed => 's',
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        match s {
            "o" => Some(Kind::Object),
            "r" => Some(Kind::Recipe),
            "s" => Some(Kind::Sealed),
            _ => None,
        }
    }

    /// Does `id` have the shape this kind's ids have: lowercase hex, 64
    /// characters (32 for a sealed object)? Checked on every request: an id
    /// is a file name in the store.
    pub fn id_ok(self, id: &str) -> bool {
        let want = if self == Kind::Sealed { 32 } else { 64 };
        id.len() == want && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    }
}

#[cfg(unix)]
pub mod client;
#[cfg(unix)]
pub mod fdpass;
#[cfg(unix)]
pub mod server;
#[cfg(unix)]
pub mod store;
