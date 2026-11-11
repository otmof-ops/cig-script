// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The client: what cig uses to talk to the vault.

use crate::store::Put;
use crate::{fdpass, Kind, MAX_LINE, VERSION};
use std::collections::VecDeque;
use std::fs;
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

/// Why a call did not go through.
#[derive(Debug)]
pub enum Error {
    /// Nothing answered, or the answer cannot be trusted (not the vault's
    /// socket): the caller falls back and tries again later.
    Unreachable(String),
    /// The vault answered no. `word` is its reason.
    Refused { word: String, msg: String },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unreachable(m) => write!(f, "{m}"),
            Error::Refused { word, msg } => write!(f, "refused ({word}): {msg}"),
        }
    }
}

impl Error {
    /// Worth keeping for the next time the vault answers?
    pub fn transient(&self) -> bool {
        match self {
            Error::Unreachable(_) => true,
            Error::Refused { word, .. } => matches!(word.as_str(), "busy" | "io" | "quota"),
        }
    }
}

fn unreachable_io(e: io::Error) -> Error {
    Error::Unreachable(e.to_string())
}

pub struct Client {
    sock: UnixStream,
    buf: Vec<u8>,
    /// The version the vault reported.
    pub server_version: String,
}

impl Client {
    /// Connect to the vault at `socket`. With `expect_uid`, the socket file
    /// and the process behind it must belong to that account: a socket
    /// someone else made is not the vault, and nothing is sent to it.
    pub fn connect(socket: &Path, expect_uid: Option<u32>) -> Result<Client, Error> {
        let md = fs::symlink_metadata(socket)
            .map_err(|e| Error::Unreachable(format!("{}: {e}", socket.display())))?;
        if !md.file_type().is_socket() {
            return Err(Error::Unreachable(format!(
                "{} is not a socket",
                socket.display()
            )));
        }
        if let Some(uid) = expect_uid {
            if md.uid() != uid {
                return Err(Error::Unreachable(format!(
                    "{} is not owned by the vault's account",
                    socket.display()
                )));
            }
        }
        let sock = UnixStream::connect(socket)
            .map_err(|e| Error::Unreachable(format!("{}: {e}", socket.display())))?;
        if let Some(uid) = expect_uid {
            let peer = fdpass::peer_uid(&sock).map_err(unreachable_io)?;
            if peer != uid {
                return Err(Error::Unreachable(format!(
                    "the process on {} is not the vault's account",
                    socket.display()
                )));
            }
        }
        sock.set_read_timeout(Some(Duration::from_secs(300)))
            .map_err(unreachable_io)?;
        sock.set_write_timeout(Some(Duration::from_secs(300)))
            .map_err(unreachable_io)?;
        let mut c = Client {
            sock,
            buf: Vec::new(),
            server_version: String::new(),
        };
        let hello = c.ask(&format!("hello {VERSION}"), None)?;
        c.server_version = hello.trim_start_matches("cig-vault ").to_string();
        Ok(c)
    }

    fn line(&mut self) -> Result<String, Error> {
        loop {
            if let Some(i) = self.buf.iter().position(|b| *b == b'\n') {
                let l: Vec<u8> = self.buf.drain(..=i).collect();
                return Ok(String::from_utf8_lossy(&l[..i]).to_string());
            }
            if self.buf.len() > MAX_LINE * 4 {
                return Err(Error::Unreachable(
                    "the vault sent a line that is too long".into(),
                ));
            }
            let mut chunk = [0u8; 4096];
            let n = (&self.sock).read(&mut chunk).map_err(unreachable_io)?;
            if n == 0 {
                return Err(Error::Unreachable("the vault closed the connection".into()));
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }

    /// One request, one `ok` line (its text after `ok `) or an error.
    fn ask(&mut self, req: &str, fd: Option<&fs::File>) -> Result<String, Error> {
        let mut data = req.as_bytes().to_vec();
        data.push(b'\n');
        fdpass::send(&self.sock, &data, fd.map(|f| f.as_raw_fd())).map_err(unreachable_io)?;
        self.answer()
    }

    fn answer(&mut self) -> Result<String, Error> {
        let l = self.line()?;
        if let Some(rest) = l.strip_prefix("ok") {
            return Ok(rest.trim_start().to_string());
        }
        if let Some(rest) = l.strip_prefix("err ") {
            let (word, msg) = rest.split_once(' ').unwrap_or((rest, ""));
            return Err(Error::Refused {
                word: word.to_string(),
                msg: msg.to_string(),
            });
        }
        Err(Error::Unreachable(
            "the vault's answer was not understood".into(),
        ))
    }

    /// Hand `file` (open for reading) to the vault as `kind`/`id`.
    pub fn store(&mut self, kind: Kind, id: &str, file: &fs::File) -> Result<Put, Error> {
        let len = file.metadata().map_err(unreachable_io)?.len();
        let r = self.ask(&format!("store {} {id} {len}", kind.letter()), Some(file))?;
        Ok(if r == "present" {
            Put::Present
        } else {
            Put::Stored
        })
    }

    /// Have the vault write `kind`/`id` into `dest` (open for writing).
    pub fn fetch(&mut self, kind: Kind, id: &str, dest: &fs::File) -> Result<u64, Error> {
        let r = self.ask(&format!("fetch {} {id}", kind.letter()), Some(dest))?;
        r.parse()
            .map_err(|_| Error::Unreachable("bad length".into()))
    }

    pub fn has(&mut self, kind: Kind, id: &str) -> Result<bool, Error> {
        Ok(self.ask(&format!("has {} {id}", kind.letter()), None)? == "yes")
    }

    pub fn verify(&mut self, kind: Kind, id: &str) -> Result<u64, Error> {
        let r = self.ask(&format!("verify {} {id}", kind.letter()), None)?;
        r.parse()
            .map_err(|_| Error::Unreachable("bad length".into()))
    }

    pub fn list(&mut self, kind: Kind) -> Result<Vec<(String, u64)>, Error> {
        let n: usize = self
            .ask(&format!("list {}", kind.letter()), None)?
            .parse()
            .map_err(|_| Error::Unreachable("bad count".into()))?;
        let mut out = Vec::with_capacity(n.min(1 << 20));
        for _ in 0..n {
            let l = self.line()?;
            let (id, len) = l.split_once(' ').unwrap_or((&l, "0"));
            out.push((id.to_string(), len.parse().unwrap_or(0)));
        }
        Ok(out)
    }

    /// What the vault would collect given the live ids. Removes nothing.
    pub fn gc_plan(&mut self, live: &[(Kind, String)]) -> Result<Vec<(Kind, String)>, Error> {
        let mut req = format!("gc {}\n", live.len());
        for (k, id) in live {
            req.push(k.letter());
            req.push(' ');
            req.push_str(id);
            req.push('\n');
        }
        fdpass::send(&self.sock, req.as_bytes(), None).map_err(unreachable_io)?;
        let n: usize = self
            .answer()?
            .parse()
            .map_err(|_| Error::Unreachable("bad count".into()))?;
        let mut out = VecDeque::new();
        for _ in 0..n {
            let l = self.line()?;
            let mut w = l.split(' ');
            if let (Some(k), Some(id)) = (w.next().and_then(Kind::parse), w.next()) {
                out.push_back((k, id.to_string()));
            }
        }
        Ok(out.into())
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = fdpass::send(&self.sock, b"bye\n", None);
    }
}
