// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! `cig internal-forward <sock> <port>`: the forwarder inside a boxed hop's
//! network namespace. It listens on `127.0.0.1:<port>` and relays each
//! connection to the parent's proxy on the Unix socket; it has no opinion
//! of its own and prints nothing. It dies with the namespace.

#[cfg(unix)]
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::Path;

pub fn forward(sock: &Path, port: u16) -> i32 {
    // Non-dumpable: the hop beside it in the namespace, the same user with
    // the same (no) capabilities, must not trace it into connecting
    // somewhere else.
    #[cfg(target_os = "linux")]
    unsafe {
        libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
    }
    // Test hook: a forwarder slow to start, as a compiled program's is.
    if let Some(ms) = std::env::var("CIG_INTERNAL_FORWARD_DELAY_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    // The listener the box made before the hop started, when it is there:
    // a hop that connected already is in its backlog.
    let listener = match inherited(port) {
        Some(l) => l,
        None => match TcpListener::bind(("127.0.0.1", port)) {
            Ok(l) => l,
            Err(_) => return 1,
        },
    };
    for conn in listener.incoming() {
        let Ok(conn) = conn else { continue };
        let sock = sock.to_path_buf();
        std::thread::spawn(move || relay(conn, &sock));
    }
    0
}

/// Descriptor 3, when it is a socket listening on `127.0.0.1:port`: the
/// listener the box made before the hop (src/burn/overlay.rs).
#[cfg(target_os = "linux")]
fn inherited(port: u16) -> Option<TcpListener> {
    use std::os::unix::io::FromRawFd;
    // SAFETY: getsockname and getsockopt only write into the buffers they
    // are given; fd 3 is taken over only when it is that listener.
    unsafe {
        let mut addr: libc::sockaddr_in = std::mem::zeroed();
        let mut len = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
        if libc::getsockname(
            3,
            &mut addr as *mut libc::sockaddr_in as *mut libc::sockaddr,
            &mut len,
        ) != 0
            || i32::from(addr.sin_family) != libc::AF_INET
            || u16::from_be(addr.sin_port) != port
        {
            return None;
        }
        let mut listening: libc::c_int = 0;
        let mut size = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        if libc::getsockopt(
            3,
            libc::SOL_SOCKET,
            libc::SO_ACCEPTCONN,
            &mut listening as *mut libc::c_int as *mut libc::c_void,
            &mut size,
        ) != 0
            || listening == 0
        {
            return None;
        }
        Some(TcpListener::from_raw_fd(3))
    }
}

#[cfg(not(target_os = "linux"))]
fn inherited(_port: u16) -> Option<TcpListener> {
    None
}

#[cfg(unix)]
fn relay(conn: TcpStream, sock: &Path) {
    use std::os::unix::net::UnixStream;
    let Ok(up) = UnixStream::connect(sock) else {
        let _ = conn.shutdown(Shutdown::Both);
        return;
    };
    let (Ok(mut a_r), Ok(mut b_r)) = (conn.try_clone(), up.try_clone()) else {
        return;
    };
    let mut a_w = conn;
    let mut b_w = up;
    let t = std::thread::spawn(move || {
        let _ = copy(&mut a_r, &mut b_w);
        let _ = b_w.shutdown(Shutdown::Write);
    });
    let _ = copy(&mut b_r, &mut a_w);
    let _ = a_w.shutdown(Shutdown::Write);
    let _ = t.join();
}

#[cfg(not(unix))]
fn relay(conn: TcpStream, _sock: &Path) {
    let _ = conn.shutdown(Shutdown::Both);
}

#[cfg(unix)]
fn copy(r: &mut dyn Read, w: &mut dyn Write) -> io::Result<u64> {
    io::copy(r, w)
}
