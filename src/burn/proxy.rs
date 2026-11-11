// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The way out of a boxed hop's network namespace: a CONNECT proxy the
//! parent runs on a Unix socket, which the forwarder inside the namespace
//! relays to. It allows the hosts the hop was given (`allow_hosts`: a
//! name, `*.suffix`, or `host:port`), resolves them itself, splices the
//! bytes and counts them; everything else (another host, a raw address not
//! listed, a method other than CONNECT) is refused and recorded as an
//! event. It reads one request line and the headers, nothing more.

#[cfg(unix)]
mod imp {
    use std::io::{self, BufRead, BufReader, Read, Write};
    use std::net::{Shutdown, TcpStream, ToSocketAddrs};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// A running proxy; `stop` ends it and removes its socket.
    pub struct Proxy {
        sock: PathBuf,
        fd: i32,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    #[derive(Clone)]
    struct Shared {
        allow: Arc<Vec<String>>,
        events: Arc<Mutex<std::fs::File>>,
    }

    /// Start the proxy on `sock`, recording to `events` (one JSON line per
    /// connection). Each entry of `allow` is a host name, `*.suffix`, or
    /// `host:port`; the match is case-insensitive.
    pub fn start(sock: &Path, allow: Vec<String>, events: &Path) -> io::Result<Proxy> {
        let _ = std::fs::remove_file(sock);
        if let Some(parent) = events.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let file = opts.open(events)?;
        let listener = UnixListener::bind(sock)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(sock, std::fs::Permissions::from_mode(0o600));
        }
        let fd = {
            use std::os::unix::io::AsRawFd;
            listener.as_raw_fd()
        };
        let shared = Shared {
            allow: Arc::new(allow.into_iter().map(|h| h.to_ascii_lowercase()).collect()),
            events: Arc::new(Mutex::new(file)),
        };
        let thread = std::thread::Builder::new()
            .name("cig-box-proxy".to_string())
            .spawn(move || {
                for conn in listener.incoming() {
                    let Ok(conn) = conn else { break };
                    let shared = shared.clone();
                    let _ = std::thread::Builder::new()
                        .name("cig-box-proxy-conn".to_string())
                        .spawn(move || serve(conn, &shared));
                }
            })?;
        Ok(Proxy {
            sock: sock.to_path_buf(),
            fd,
            thread: Some(thread),
        })
    }

    impl Proxy {
        /// End the proxy: the listener is shut down (its accept loop ends),
        /// the thread joined, the socket removed. Connections still open end
        /// with the hop's processes, which are gone by now.
        pub fn stop(mut self) {
            // SAFETY: shutting down a descriptor this process owns; the
            // listener's accept then fails and the thread returns.
            unsafe {
                libc::shutdown(self.fd, libc::SHUT_RDWR);
            }
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
            let _ = std::fs::remove_file(&self.sock);
        }
    }

    /// A refusal's answer. Its event is written first: the client may exit
    /// the moment it reads this.
    fn answer(writer: &mut UnixStream, reply: &[u8]) {
        let _ = writer.write_all(reply);
        // The test's stand-in for this thread losing the CPU right here.
        #[cfg(test)]
        std::thread::sleep(Duration::from_millis(
            PAUSE_AFTER_ANSWER.load(std::sync::atomic::Ordering::Relaxed),
        ));
    }

    #[cfg(test)]
    pub static PAUSE_AFTER_ANSWER: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(0);

    /// Does `host:port` pass the allow list?
    /// Does `allow` let `host:port` through? A `host:port` entry is that
    /// pair exactly; a name or address alone is that host on 443; `*.suffix`
    /// is any name under it, on 443.
    pub fn allowed(allow: &[String], host: &str, port: u16) -> bool {
        admission(allow, host, port).is_some()
    }

    /// How an entry admits the host: `Some(false)` by name or address,
    /// `Some(true)` by a wildcard, `None` not at all. An exact entry is taken
    /// as given wherever it points; a wildcard is not trusted to stay off
    /// this machine and its network (see `internal`).
    fn admission(allow: &[String], host: &str, port: u16) -> Option<bool> {
        let host = host.to_ascii_lowercase();
        let with_port = format!("{host}:{port}");
        let mut by_wildcard = false;
        for a in allow {
            let a = a.to_ascii_lowercase();
            if a.contains(':') && !a.starts_with('[') {
                if a == with_port {
                    return Some(false);
                }
            } else if let Some(suffix) = a.strip_prefix("*.") {
                if port == 443 && (host == suffix || host.ends_with(&format!(".{suffix}"))) {
                    by_wildcard = true;
                }
            } else if a == host && port == 443 {
                return Some(false);
            }
        }
        by_wildcard.then_some(true)
    }

    /// An address inside this machine or its network: loopback, private,
    /// link-local, shared (CGNAT), unspecified, multicast or broadcast. A
    /// name admitted by a wildcard that resolves to one is refused, so a name
    /// someone else controls cannot turn the proxy on a service here.
    pub fn internal(ip: &std::net::IpAddr) -> bool {
        use std::net::IpAddr;
        match ip {
            IpAddr::V4(v4) => {
                let o = v4.octets();
                v4.is_loopback()
                    || v4.is_private()
                    || v4.is_link_local()
                    || v4.is_unspecified()
                    || v4.is_multicast()
                    || v4.is_broadcast()
                    || (o[0] == 100 && (o[1] & 0xc0) == 64)
                    || o[0] == 0
            }
            IpAddr::V6(v6) => {
                if let Some(v4) = v6.to_ipv4_mapped() {
                    return internal(&IpAddr::V4(v4));
                }
                let seg = v6.segments();
                v6.is_loopback()
                    || v6.is_unspecified()
                    || v6.is_multicast()
                    || (seg[0] & 0xfe00) == 0xfc00
                    || (seg[0] & 0xffc0) == 0xfe80
            }
        }
    }

    /// A hop-chosen name, fit to print: letters, digits and `.-_:[]` kept,
    /// anything else (a control character, an escape) shown as `?`, and
    /// capped, so a name cannot move the cursor or recolour the terminal.
    pub fn printable(s: &str) -> String {
        let mut out: String = s
            .chars()
            .take(100)
            .map(|c| {
                if c.is_ascii_alphanumeric() || ".-_:[]".contains(c) {
                    c
                } else {
                    '?'
                }
            })
            .collect();
        if s.chars().count() > 100 {
            out.push('…');
        }
        out
    }

    fn event(shared: &Shared, line: serde_json::Value) {
        if let Ok(mut f) = shared.events.lock() {
            let _ = writeln!(f, "{line}");
        }
    }

    fn now() -> String {
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    fn serve(conn: UnixStream, shared: &Shared) {
        let _ = conn.set_read_timeout(Some(Duration::from_secs(30)));
        let mut reader = BufReader::new(match conn.try_clone() {
            Ok(c) => c,
            Err(_) => return,
        });
        let mut writer = conn;
        let mut request = String::new();
        if reader.read_line(&mut request).is_err() {
            return;
        }
        // The headers, read and dropped (capped: this is a tunnel, not a web server).
        let mut total = request.len();
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(n) => {
                    total += n;
                    if line == "\r\n" || line == "\n" || total > 8192 {
                        break;
                    }
                }
                Err(_) => return,
            }
        }
        // Each refusal is recorded before it is answered: the client may
        // exit the moment it has its answer, the hop is judged as soon as
        // it does, and an event written after that was never seen (E771
        // missing, so a run that should have waited went on).
        let mut parts = request.split_whitespace();
        let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        if method != "CONNECT" {
            event(
                shared,
                serde_json::json!({"t": now(), "call": "proxy", "method": method, "target": target, "result": "refused", "why": "not CONNECT"}),
            );
            answer(
                &mut writer,
                b"HTTP/1.1 405 Method Not Allowed\r\nConnection: close\r\n\r\n",
            );
            return;
        }
        let (host, port) = match split_host_port(target) {
            Some(hp) => hp,
            None => {
                event(
                    shared,
                    serde_json::json!({"t": now(), "call": "connect", "target": target, "result": "refused", "why": "malformed"}),
                );
                answer(
                    &mut writer,
                    b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n",
                );
                return;
            }
        };
        let Some(by_wildcard) = admission(&shared.allow, &host, port) else {
            event(
                shared,
                serde_json::json!({"t": now(), "call": "connect", "host": host, "port": port, "result": "refused", "why": "not in allow_hosts"}),
            );
            answer(
                &mut writer,
                b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n",
            );
            return;
        };
        let addrs: Vec<std::net::SocketAddr> = (host.as_str(), port)
            .to_socket_addrs()
            .map(|a| a.collect())
            .unwrap_or_default();
        // IPv4 first: on a network where IPv6 is not routed a v6 address only
        // times out, and a registry name has several of them.
        let mut outside: Vec<std::net::SocketAddr> = addrs
            .iter()
            .copied()
            .filter(|a| !by_wildcard || !internal(&a.ip()))
            .collect();
        outside.sort_by_key(|a| a.is_ipv6());
        if by_wildcard && !addrs.is_empty() && outside.is_empty() {
            event(
                shared,
                serde_json::json!({"t": now(), "call": "connect", "host": host, "port": port, "result": "refused", "why": "a wildcard entry does not reach an address on this machine or its network"}),
            );
            answer(
                &mut writer,
                b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n",
            );
            return;
        }
        let upstream = outside
            .into_iter()
            .find_map(|a| TcpStream::connect_timeout(&a, Duration::from_secs(5)).ok());
        let Some(upstream) = upstream else {
            event(
                shared,
                serde_json::json!({"t": now(), "call": "connect", "host": host, "port": port, "result": "unreachable"}),
            );
            answer(
                &mut writer,
                b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n",
            );
            return;
        };
        if writer
            .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
            .is_err()
        {
            return;
        }
        // Whatever the client sent after its headers, buffered already.
        let mut up_w = match upstream.try_clone() {
            Ok(u) => u,
            Err(_) => return,
        };
        let pending = reader.buffer().to_vec();
        if !pending.is_empty() && up_w.write_all(&pending).is_err() {
            return;
        }
        let mut down_r = upstream;
        let up_thread = std::thread::spawn(move || {
            let mut n = pending.len() as u64;
            n += io::copy(&mut reader, &mut up_w).unwrap_or(0);
            let _ = up_w.shutdown(Shutdown::Write);
            n
        });
        let down = copy_to(&mut down_r, &mut writer);
        let _ = writer.shutdown(Shutdown::Write);
        let up = up_thread.join().unwrap_or(0);
        event(
            shared,
            serde_json::json!({"t": now(), "call": "connect", "host": host, "port": port, "result": "allowed", "bytes_up": up, "bytes_down": down}),
        );
    }

    fn copy_to(r: &mut dyn Read, w: &mut dyn Write) -> u64 {
        io::copy(r, w).unwrap_or(0)
    }

    /// `host:port`, with a bracketed IPv6 host allowed.
    fn split_host_port(target: &str) -> Option<(String, u16)> {
        let (host, port) = if let Some(rest) = target.strip_prefix('[') {
            let (h, p) = rest.split_once(']')?;
            (h.to_string(), p.strip_prefix(':')?)
        } else {
            let (h, p) = target.rsplit_once(':')?;
            (h.to_string(), p)
        };
        if host.is_empty() {
            return None;
        }
        Some((host, port.parse().ok()?))
    }

    /// The refused connections a hop's events record, as `host:port` lines.
    pub fn refused(events: &Path) -> Vec<String> {
        let Ok(text) = std::fs::read_to_string(events) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if v["result"] == "refused" {
                let what = match (v["host"].as_str(), v["port"].as_u64()) {
                    (Some(h), Some(p)) => format!("{}:{p}", printable(h)),
                    _ => printable(v["target"].as_str().unwrap_or("?")),
                };
                if !out.contains(&what) {
                    out.push(what);
                }
            }
        }
        out
    }
}

#[cfg(unix)]
pub use imp::{allowed, refused, start, Proxy};

/// Off Unix there is no box and so no proxy: the calls exist so the hop's
/// code reads the same everywhere, and `start` says why it cannot.
#[cfg(not(unix))]
mod imp {
    use std::io;
    use std::path::Path;

    pub struct Proxy;

    impl Proxy {
        pub fn stop(self) {}
    }

    pub fn start(_sock: &Path, _allow: Vec<String>, _events: &Path) -> io::Result<Proxy> {
        Err(io::Error::other(
            "the box's proxy runs on a Unix socket, which this system does not have",
        ))
    }

    pub fn allowed(_allow: &[String], _host: &str, _port: u16) -> bool {
        false
    }

    pub fn refused(_events: &Path) -> Vec<String> {
        Vec::new()
    }
}

#[cfg(not(unix))]
pub use imp::{allowed, refused, start, Proxy};

#[cfg(all(test, unix))]
mod tests {
    use super::imp::*;

    /// A refusal is on disk before the client has its answer, so a hop
    /// judged the moment its client exits sees it (cig-orchestrator's agent
    /// box once went on where it should have waited). The proxy is held up
    /// just after it answers, so an event written after the answer would
    /// be missing every time.
    // The box, and so its proxy, is Linux's; and only there does shutting
    // the listener down wake its accept loop for `stop` to join.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_refusal_is_recorded_before_it_is_answered() {
        use std::io::{Read, Write};
        let dir = tempfile::tempdir().unwrap();
        // As if the proxy's thread lost the CPU just after it answered.
        PAUSE_AFTER_ANSWER.store(300, std::sync::atomic::Ordering::Relaxed);
        for k in 0..3 {
            let sock = dir.path().join(format!("p{k}.sock"));
            let events = dir.path().join(format!("e{k}.jsonl"));
            let proxy = start(&sock, allow(&["registry.npmjs.org"]), &events).unwrap();
            let mut c = std::os::unix::net::UnixStream::connect(&sock).unwrap();
            c.write_all(b"CONNECT evil.example:443 HTTP/1.1\r\nHost: evil.example:443\r\n\r\n")
                .unwrap();
            // The headers alone, as curl reads them, not to the end of the
            // connection (which closes only once the proxy is done).
            let mut reply = Vec::new();
            let mut byte = [0u8; 1];
            while !reply.ends_with(b"\r\n\r\n") && c.read(&mut byte).unwrap() == 1 {
                reply.push(byte[0]);
            }
            let reply = String::from_utf8_lossy(&reply).to_string();
            assert!(reply.starts_with("HTTP/1.1 403"), "{reply}");
            // Read at once, as cig does when the hop ends: the proxy is not
            // stopped first, so nothing but the order can make it be there.
            assert_eq!(
                refused(&events),
                vec!["evil.example:443".to_string()],
                "attempt {k}"
            );
            proxy.stop();
        }
    }

    fn allow(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn a_bare_name_or_wildcard_is_https_only_and_host_port_is_exact() {
        let a = allow(&["registry.npmjs.org", "*.pythonhosted.org", "127.0.0.1:8080"]);
        assert!(allowed(&a, "registry.npmjs.org", 443));
        assert!(!allowed(&a, "registry.npmjs.org", 22));
        assert!(allowed(&a, "files.pythonhosted.org", 443));
        assert!(!allowed(&a, "files.pythonhosted.org", 80));
        assert!(allowed(&a, "127.0.0.1", 8080));
        assert!(!allowed(&a, "127.0.0.1", 443));
        assert!(!allowed(&a, "evil.example", 443));
        assert!(allowed(&a, "REGISTRY.npmjs.org", 443));
    }

    #[test]
    fn a_wildcard_never_reaches_this_machine_or_its_network() {
        for inside in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.1.1",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fc00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(internal(&inside.parse().unwrap()), "{inside}");
        }
        for outside in ["8.8.8.8", "104.16.0.1", "2606:4700::1111"] {
            assert!(!internal(&outside.parse().unwrap()), "{outside}");
        }
    }

    #[test]
    fn a_hop_chosen_name_prints_plainly() {
        assert_eq!(printable("registry.npmjs.org"), "registry.npmjs.org");
        assert_eq!(printable("a\u{1b}[2Jb"), "a?[2Jb");
        assert_eq!(printable("x\ny"), "x?y");
        assert!(printable(&"a".repeat(300)).ends_with('…'));
    }
}
