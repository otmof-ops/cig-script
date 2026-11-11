// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The vault's side of the socket: one thread a connection, one request at
//! a time, the peer's uid read from the kernel and never from the request.

use crate::fdpass;
use crate::store::{Put, Refusal, Store};
use crate::{Kind, MAX_LINE, VERSION};
use std::collections::HashSet;
use std::io::Write;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub struct Options {
    /// The most connections at once.
    pub max_conns: usize,
    /// How long a connection may sit silent.
    pub idle: Duration,
    /// Objects younger than this are never in a gc plan.
    pub grace: Duration,
    /// The most live ids one gc request may carry.
    pub max_live: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_conns: 64,
            idle: Duration::from_secs(120),
            grace: Duration::from_secs(7 * 24 * 3600),
            max_live: 2_000_000,
        }
    }
}

/// Bind the socket (mode 0666: any local account may ask; the peer's uid
/// says whose tin it is). A stale socket file of ours is replaced; one that
/// answers is left alone.
pub fn bind(path: &Path) -> Result<UnixListener, String> {
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            return Err(format!("a vault already answers on {}", path.display()));
        }
        std::fs::remove_file(path)
            .map_err(|e| format!("cannot replace {}: {e}", path.display()))?;
    }
    let l = UnixListener::bind(path)
        .map_err(|e| format!("cannot listen on {}: {e}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))
        .map_err(|e| format!("cannot open {} to local accounts: {e}", path.display()))?;
    Ok(l)
}

/// Serve until the listener fails. Each connection runs on its own thread.
pub fn serve(listener: UnixListener, store: Arc<Store>, opts: Options) {
    let live = Arc::new(AtomicUsize::new(0));
    let opts = Arc::new(opts);
    for conn in listener.incoming() {
        let Ok(conn) = conn else { continue };
        if live.fetch_add(1, Ordering::SeqCst) >= opts.max_conns {
            live.fetch_sub(1, Ordering::SeqCst);
            let mut c = conn;
            let _ = c.write_all(b"err busy too many connections\n");
            continue;
        }
        let (store, opts, live) = (store.clone(), opts.clone(), live.clone());
        std::thread::spawn(move || {
            handle(conn, &store, &opts);
            live.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

struct Conn {
    sock: UnixStream,
    buf: Vec<u8>,
    fds: Vec<OwnedFd>,
}

impl Conn {
    /// The next line, without its newline; `None` when the peer is gone.
    fn line(&mut self) -> Result<Option<String>, Refusal> {
        loop {
            if let Some(i) = self.buf.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.buf.drain(..=i).collect();
                let s = std::str::from_utf8(&line[..i])
                    .map_err(|_| Refusal::new("bad-request", "not text"))?;
                return Ok(Some(s.trim_end_matches('\r').to_string()));
            }
            if self.buf.len() > MAX_LINE {
                return Err(Refusal::new("bad-request", "that line is too long"));
            }
            let mut chunk = [0u8; 512];
            let n = fdpass::recv(&self.sock, &mut chunk, &mut self.fds)
                .map_err(|e| Refusal::new("io", e.to_string()))?;
            if n == 0 {
                return Ok(None);
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }

    fn reply(&mut self, text: &str) -> bool {
        self.sock.write_all(text.as_bytes()).is_ok()
    }
}

fn handle(sock: UnixStream, store: &Store, opts: &Options) {
    let _ = sock.set_read_timeout(Some(opts.idle));
    let _ = sock.set_write_timeout(Some(opts.idle));
    let Ok(uid) = fdpass::peer_uid(&sock) else {
        return;
    };
    let mut c = Conn {
        sock,
        buf: Vec::new(),
        fds: Vec::new(),
    };
    loop {
        let line = match c.line() {
            Ok(Some(l)) => l,
            Ok(None) => return,
            Err(e) => {
                let _ = c.reply(&format!("err {} {}\n", e.word, e.msg));
                return;
            }
        };
        let words: Vec<&str> = line.split(' ').collect();
        let out = request(&mut c, store, opts, uid, &words);
        // A descriptor that came with a request that did not want it is
        // closed here, never kept for the next one.
        c.fds.clear();
        match out {
            Ok(Some(text)) => {
                if !c.reply(&text) {
                    return;
                }
            }
            Ok(None) => return,
            Err(e) => {
                if !c.reply(&format!("err {} {}\n", e.word, e.msg.replace('\n', " "))) {
                    return;
                }
            }
        }
    }
}

fn want_kind(words: &[&str], i: usize) -> Result<Kind, Refusal> {
    words
        .get(i)
        .and_then(|w| Kind::parse(w))
        .ok_or_else(|| Refusal::new("bad-request", "name a kind: o, r or s"))
}

fn want_id<'a>(words: &[&'a str], i: usize, kind: Kind) -> Result<&'a str, Refusal> {
    match words.get(i) {
        Some(id) if kind.id_ok(id) => Ok(id),
        _ => Err(Refusal::new(
            "bad-request",
            "that is not an id of this kind",
        )),
    }
}

fn request(
    c: &mut Conn,
    store: &Store,
    opts: &Options,
    uid: u32,
    words: &[&str],
) -> Result<Option<String>, Refusal> {
    match words[0] {
        "hello" => {
            if words.get(1).and_then(|v| v.parse::<u32>().ok()) != Some(VERSION) {
                return Err(Refusal::new(
                    "version",
                    format!("this vault speaks protocol {VERSION}"),
                ));
            }
            Ok(Some(format!(
                "ok cig-vault {VERSION} {}\n",
                env!("CARGO_PKG_VERSION")
            )))
        }
        "store" => {
            let kind = want_kind(words, 1)?;
            let id = want_id(words, 2, kind)?;
            let len: u64 = words
                .get(3)
                .and_then(|w| w.parse().ok())
                .ok_or_else(|| Refusal::new("bad-request", "name the length"))?;
            if c.fds.len() != 1 {
                return Err(Refusal::new("no-fd", "a store carries one open file"));
            }
            let fd = c.fds.remove(0);
            let r = store.put(uid, kind, id, len, fd.as_fd())?;
            Ok(Some(match r {
                Put::Stored => "ok stored\n".to_string(),
                Put::Present => "ok present\n".to_string(),
            }))
        }
        "fetch" => {
            let kind = want_kind(words, 1)?;
            let id = want_id(words, 2, kind)?;
            if c.fds.len() != 1 {
                return Err(Refusal::new(
                    "no-fd",
                    "a fetch carries one file open for writing",
                ));
            }
            let fd = c.fds.remove(0);
            let n = store.get(uid, kind, id, fd.as_fd())?;
            Ok(Some(format!("ok {n}\n")))
        }
        "has" => {
            let kind = want_kind(words, 1)?;
            let id = want_id(words, 2, kind)?;
            Ok(Some(format!(
                "ok {}\n",
                if store.has(uid, kind, id) {
                    "yes"
                } else {
                    "no"
                }
            )))
        }
        "verify" => {
            let kind = want_kind(words, 1)?;
            let id = want_id(words, 2, kind)?;
            Ok(Some(format!("ok {}\n", store.verify(uid, kind, id)?)))
        }
        "list" => {
            let kind = want_kind(words, 1)?;
            let items = store.list(uid, kind);
            let mut s = format!("ok {}\n", items.len());
            for (id, len) in items {
                s.push_str(&format!("{id} {len}\n"));
            }
            Ok(Some(s))
        }
        "gc" => {
            let n: usize = words
                .get(1)
                .and_then(|w| w.parse().ok())
                .ok_or_else(|| Refusal::new("bad-request", "name how many live ids follow"))?;
            if n > opts.max_live {
                return Err(Refusal::new("too-large", "too many live ids"));
            }
            let mut live = HashSet::new();
            for _ in 0..n {
                let l = c
                    .line()?
                    .ok_or_else(|| Refusal::new("bad-request", "the list ended early"))?;
                let mut w = l.split(' ');
                let kind = w.next().and_then(Kind::parse);
                let id = w.next();
                match (kind, id) {
                    (Some(k), Some(id)) if k.id_ok(id) => {
                        live.insert((k, id.to_string()));
                    }
                    _ => return Err(Refusal::new("bad-request", "a live line is `<kind> <id>`")),
                }
            }
            let plan = store.gc_plan(uid, &live, opts.grace);
            let mut s = format!("ok {}\n", plan.len());
            for (k, id) in plan {
                s.push_str(&format!("{} {id}\n", k.letter()));
            }
            Ok(Some(s))
        }
        "bye" => Ok(None),
        _ => Err(Refusal::new("bad-request", "unknown request")),
    }
}
