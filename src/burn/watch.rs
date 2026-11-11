// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The box's watch: what a boxed hop opens, connects to and runs, seen
//! from outside it (cig-script#106, step 3).
//!
//! A seccomp filter installed in the hop answers `SECCOMP_RET_USER_NOTIF`
//! for the calls worth seeing (a file opened for writing, `connect`,
//! `sendto` with an address, `execve`) and `ALLOW` for everything else, so
//! an ordinary call costs one filter run and nothing more. A supervisor
//! thread in cig reads each notification, appends one JSON line to the
//! run's event log (the file `$CIG_EVENTS` names; `r.events` in the
//! script), and lets the call go on.
//!
//! **The watch is for seeing, not for enforcing.** A path read out of
//! another process's memory can change before the call uses it, so nothing
//! here is a boundary: the boundaries are the namespaces and mounts. The
//! one call the watch does answer itself is `io_uring`, whose operations
//! pass no syscall the filter can see: it gets `EPERM` and an event
//! (`E776`).
//!
//! Where a kernel has no user notification (before 5.0, or a container
//! that forbids it) the same calls are traced with `ptrace` instead: the
//! hop is seized from a thread of cig and stops at `SECCOMP_RET_TRACE`.
//! It sees the same calls, slower, and says so once (`E779`). There is no
//! third way: a watch that was asked for and cannot be had refuses the hop.

use std::path::Path;

/// How much the watch sees.
///
/// `Refusals` is what a contained hop always has: only the calls the box
/// refuses by itself (io_uring), so the refusal leaves its event. `Calls`
/// is `watch: true`. `Reads` adds every file the hop opens, reads included.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Mode {
    Off,
    Refusals,
    Calls,
    Reads,
}

/// The event log as lines of JSON, in the order they were written; a line
/// that is not JSON is skipped. The proxy's events share the file.
pub fn read_events(path: &Path) -> Vec<serde_json::Value> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// The codes the log holds (`E776` for an io_uring refusal, `E779` for the
/// ptrace route), each once, in the order they first appear.
pub fn codes(events: &[serde_json::Value]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for e in events {
        if let Some(code) = e["code"].as_str() {
            if !out.iter().any(|c| c == code) {
                out.push(code.to_string());
            }
        }
    }
    out
}

#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "linux")]
mod linux {
    use super::Mode;
    use std::fs::File;
    use std::io::Write;
    use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::path::Path;
    use std::sync::mpsc;
    use std::thread::JoinHandle;
    use std::time::Duration;

    const SECCOMP_SET_MODE_FILTER: libc::c_uint = 1;
    const FILTER_FLAG_NEW_LISTENER: libc::c_ulong = 1 << 3;
    pub const RET_USER_NOTIF: u32 = 0x7fc0_0000;
    pub const RET_TRACE: u32 = 0x7ff0_0000;
    const RET_ALLOW: u32 = 0x7fff_0000;
    const RET_KILL: u32 = 0x8000_0000;
    const IOCTL_RECV: libc::c_ulong = 0xc050_2100;
    const IOCTL_SEND: libc::c_ulong = 0xc018_2101;
    const IOCTL_ID_VALID: libc::c_ulong = 0x4008_2102;
    const RESP_FLAG_CONTINUE: u32 = 1;

    const PTRACE_SEIZE: libc::c_uint = 0x4206;
    const PTRACE_CONT: libc::c_uint = 7;
    const PTRACE_GETREGSET: libc::c_uint = 0x4204;
    const PTRACE_SETREGSET: libc::c_uint = 0x4205;
    const PTRACE_EVENT_SECCOMP: i32 = 7;
    const OPT_TRACEFORK: usize = 0x2;
    const OPT_TRACEVFORK: usize = 0x4;
    const OPT_TRACECLONE: usize = 0x8;
    const OPT_TRACESECCOMP: usize = 0x80;
    const OPT_EXITKILL: usize = 0x10_0000;
    const NT_PRSTATUS: usize = 1;

    /// What the child holds to hand the watch over: the socket's child end,
    /// the filter for each way, and whether a hop without a watch is better
    /// refused than run.
    pub struct WatchChild {
        pub sock: RawFd,
        pub mode: Mode,
        pub notify: Vec<libc::sock_filter>,
        pub trace: Vec<libc::sock_filter>,
        pub required: bool,
        /// Skip the fast route and trace with ptrace (`CIG_WATCH_ROUTE=ptrace`,
        /// a switch for checking the slower route, which says what it is).
        pub force_ptrace: bool,
    }

    #[repr(C)]
    struct Notif {
        id: u64,
        pid: u32,
        flags: u32,
        nr: i32,
        arch: u32,
        ip: u64,
        args: [u64; 6],
    }

    #[repr(C)]
    struct Resp {
        id: u64,
        val: i64,
        error: i32,
        flags: u32,
    }

    /// The watch's classic-BPF filter for `mode`: `action` (user
    /// notification, or a trace stop for the ptrace route) for the calls
    /// the mode sees, allow for the rest, kill for another architecture.
    pub fn filter(mode: Mode, action: u32) -> Result<Vec<libc::sock_filter>, String> {
        #[cfg(target_arch = "x86_64")]
        const ARCH: u32 = 0xC000_003E;
        #[cfg(target_arch = "aarch64")]
        const ARCH: u32 = 0xC000_00B7;
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            let _ = (mode, action);
            return Err("the watch needs x86_64 or aarch64".to_string());
        }
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        {
            const LD_W_ABS: u16 = 0x20;
            const JEQ: u16 = 0x15;
            const JSET: u16 = 0x45;
            const RET: u16 = 0x06;
            let st = |code: u16, k: u32| libc::sock_filter {
                code,
                jt: 0,
                jf: 0,
                k,
            };
            let jmp = |code: u16, k: u32, jt: u8, jf: u8| libc::sock_filter { code, jt, jf, k };
            const ARCH_OFF: u32 = 4;
            const ARG2: u32 = 32;
            const ARG4_LO: u32 = 48;
            const ARG4_HI: u32 = 52;
            let mut f = vec![
                st(LD_W_ABS, ARCH_OFF),
                jmp(JEQ, ARCH, 1, 0),
                st(RET, RET_KILL),
                st(LD_W_ABS, 0),
            ];
            #[cfg(target_arch = "x86_64")]
            {
                const JGE: u16 = 0x35;
                f.push(jmp(JGE, 0x4000_0000, 0, 1));
                f.push(st(RET, RET_KILL));
            }
            // A call that is always seen.
            let mut always: Vec<libc::c_long> = vec![
                libc::SYS_io_uring_setup,
                libc::SYS_io_uring_enter,
                libc::SYS_io_uring_register,
            ];
            if mode >= Mode::Calls {
                always.extend([
                    libc::SYS_connect,
                    libc::SYS_execve,
                    libc::SYS_execveat,
                    libc::SYS_openat2,
                ]);
                #[cfg(target_arch = "x86_64")]
                always.push(libc::SYS_creat);
            }
            for nr in always {
                f.push(jmp(JEQ, nr as u32, 0, 1));
                f.push(st(RET, action));
            }
            if mode >= Mode::Calls {
                // An open: all of them under `Reads`, otherwise those that
                // can write (a write mode, create or truncate).
                let writes = (libc::O_ACCMODE | libc::O_CREAT | libc::O_TRUNC) as u32;
                // `open` itself exists only on x86_64; other targets have `openat` alone
                #[cfg_attr(not(target_arch = "x86_64"), allow(unused_mut))]
                let mut opens = vec![(libc::SYS_openat, ARG2)];
                #[cfg(target_arch = "x86_64")]
                opens.push((libc::SYS_open, 24));
                for (nr, flags_at) in opens {
                    if mode >= Mode::Reads {
                        f.push(jmp(JEQ, nr as u32, 0, 1));
                        f.push(st(RET, action));
                    } else {
                        f.push(jmp(JEQ, nr as u32, 0, 3));
                        f.push(st(LD_W_ABS, flags_at));
                        f.push(jmp(JSET, writes, 0, 1));
                        f.push(st(RET, action));
                        // The rest of the program needs the number again.
                        f.push(st(LD_W_ABS, 0));
                    }
                }
                // sendto with an address (a datagram to somewhere).
                f.push(jmp(JEQ, libc::SYS_sendto as u32, 0, 6));
                f.push(st(LD_W_ABS, ARG4_LO));
                f.push(jmp(JEQ, 0, 0, 2));
                f.push(st(LD_W_ABS, ARG4_HI));
                f.push(jmp(JEQ, 0, 1, 0));
                f.push(st(RET, action));
                f.push(st(RET, RET_ALLOW));
            }
            f.push(st(RET, RET_ALLOW));
            Ok(f)
        }
    }

    /// Install `prog` and take the notification descriptor it makes, or -1.
    ///
    /// # Safety
    ///
    /// Async-signal-safe: one system call on a program planned in the
    /// parent. Needs `no_new_privs` set already.
    pub unsafe fn try_listener(prog: &[libc::sock_filter]) -> libc::c_int {
        let p = libc::sock_fprog {
            len: prog.len() as libc::c_ushort,
            filter: prog.as_ptr() as *mut libc::sock_filter,
        };
        libc::syscall(
            libc::SYS_seccomp,
            SECCOMP_SET_MODE_FILTER,
            FILTER_FLAG_NEW_LISTENER,
            &p as *const libc::sock_fprog,
        ) as libc::c_int
    }

    /// Install `prog` as an ordinary filter.
    ///
    /// # Safety
    ///
    /// As `try_listener`.
    pub unsafe fn install(prog: &[libc::sock_filter]) -> bool {
        let p = libc::sock_fprog {
            len: prog.len() as libc::c_ushort,
            filter: prog.as_ptr() as *mut libc::sock_filter,
        };
        libc::prctl(
            libc::PR_SET_SECCOMP,
            libc::SECCOMP_MODE_FILTER,
            &p as *const libc::sock_fprog,
            0,
            0,
        ) == 0
    }

    /// One byte and, with it, a descriptor (`fd >= 0`) or the sender's
    /// credentials (so the receiver learns the sender's pid in its own
    /// namespace). Child side.
    ///
    /// # Safety
    ///
    /// Async-signal-safe: stack buffers and `sendmsg`.
    pub unsafe fn send_msg(sock: RawFd, tag: u8, fd: RawFd) -> bool {
        let mut ctl = [0u64; 16];
        let mut byte = tag;
        let mut iov = libc::iovec {
            iov_base: &mut byte as *mut u8 as *mut libc::c_void,
            iov_len: 1,
        };
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = ctl.as_mut_ptr() as *mut libc::c_void;
        let cmsg = ctl.as_mut_ptr() as *mut libc::cmsghdr;
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        if fd >= 0 {
            msg.msg_controllen = libc::CMSG_SPACE(4) as _;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg).cmsg_len = libc::CMSG_LEN(4) as _;
            std::ptr::write(libc::CMSG_DATA(cmsg) as *mut libc::c_int, fd);
        } else {
            let size = std::mem::size_of::<libc::ucred>() as u32;
            msg.msg_controllen = libc::CMSG_SPACE(size) as _;
            (*cmsg).cmsg_type = libc::SCM_CREDENTIALS;
            (*cmsg).cmsg_len = libc::CMSG_LEN(size) as _;
            std::ptr::write(
                libc::CMSG_DATA(cmsg) as *mut libc::ucred,
                libc::ucred {
                    pid: libc::getpid(),
                    uid: libc::getuid(),
                    gid: libc::getgid(),
                },
            );
        }
        libc::sendmsg(sock, &msg, libc::MSG_NOSIGNAL) == 1
    }

    /// Wait for the byte the supervisor answers a trace request with.
    ///
    /// # Safety
    ///
    /// Async-signal-safe: `read`.
    pub unsafe fn wait_ack(sock: RawFd) -> bool {
        let mut b = 0u8;
        loop {
            let n = libc::read(sock, &mut b as *mut u8 as *mut libc::c_void, 1);
            if n < 0 && *libc::__errno_location() == libc::EINTR {
                continue;
            }
            return n == 1 && b == b'1';
        }
    }

    enum First {
        Nothing,
        Notify(OwnedFd),
        Trace(i32),
    }

    fn recv_first(sock: RawFd) -> First {
        // SAFETY: recvmsg into stack buffers; the descriptor it returns is
        // owned from here.
        unsafe {
            let mut ctl = [0u64; 16];
            let mut byte = 0u8;
            let mut iov = libc::iovec {
                iov_base: &mut byte as *mut u8 as *mut libc::c_void,
                iov_len: 1,
            };
            let mut msg: libc::msghdr = std::mem::zeroed();
            msg.msg_iov = &mut iov;
            msg.msg_iovlen = 1;
            msg.msg_control = ctl.as_mut_ptr() as *mut libc::c_void;
            msg.msg_controllen = std::mem::size_of_val(&ctl) as _;
            if libc::recvmsg(sock, &mut msg, libc::MSG_CMSG_CLOEXEC) != 1 {
                return First::Nothing;
            }
            let mut fd = -1;
            let mut pid = 0;
            let mut c = libc::CMSG_FIRSTHDR(&msg);
            while !c.is_null() {
                if (*c).cmsg_level == libc::SOL_SOCKET {
                    if (*c).cmsg_type == libc::SCM_RIGHTS {
                        fd = std::ptr::read(libc::CMSG_DATA(c) as *const libc::c_int);
                    } else if (*c).cmsg_type == libc::SCM_CREDENTIALS {
                        pid = std::ptr::read(libc::CMSG_DATA(c) as *const libc::ucred).pid;
                    }
                }
                c = libc::CMSG_NXTHDR(&msg, c);
            }
            match byte {
                b'N' if fd >= 0 => First::Notify(OwnedFd::from_raw_fd(fd)),
                b'P' if pid > 0 => First::Trace(pid),
                _ => {
                    if fd >= 0 {
                        drop(OwnedFd::from_raw_fd(fd));
                    }
                    First::Nothing
                }
            }
        }
    }

    /// The watch of one hop, held by the parent from before the hop starts
    /// until it is dropped: dropping stops the supervisor (bounded) and the
    /// log is complete, since an event is written before the call it
    /// describes is let go.
    pub struct Watch {
        stop: OwnedFd,
        child_end: OwnedFd,
        done: mpsc::Receiver<()>,
        thread: Option<JoinHandle<()>>,
    }

    impl Watch {
        /// Start the supervisor, waiting for the hop's first word. The hop
        /// gets `child_fd()` (close-on-exec; the filter's installer uses it
        /// before the exec).
        pub fn start(events: &Path, mode: Mode) -> std::io::Result<Watch> {
            let mut pair = [0 as libc::c_int; 2];
            // SAFETY: socketpair writes two descriptors; eventfd makes one.
            let (parent, child, stop) = unsafe {
                if libc::socketpair(
                    libc::AF_UNIX,
                    libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
                    0,
                    pair.as_mut_ptr(),
                ) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                let on: libc::c_int = 1;
                libc::setsockopt(
                    pair[0],
                    libc::SOL_SOCKET,
                    libc::SO_PASSCRED,
                    &on as *const libc::c_int as *const libc::c_void,
                    4,
                );
                let stop = libc::eventfd(0, libc::EFD_CLOEXEC);
                if stop < 0 {
                    libc::close(pair[0]);
                    libc::close(pair[1]);
                    return Err(std::io::Error::last_os_error());
                }
                (
                    OwnedFd::from_raw_fd(pair[0]),
                    OwnedFd::from_raw_fd(pair[1]),
                    OwnedFd::from_raw_fd(stop),
                )
            };
            if let Some(dir) = events.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut opts = std::fs::OpenOptions::new();
            opts.create(true).append(true);
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let log = opts.open(events)?;
            let stop_raw = stop.as_raw_fd();
            let (tx, rx) = mpsc::channel();
            let thread = std::thread::Builder::new()
                .name("cig-box-watch".to_string())
                .spawn(move || {
                    supervise(parent, stop_raw, log, mode);
                    let _ = tx.send(());
                })?;
            Ok(Watch {
                stop,
                child_end: child,
                done: rx,
                thread: Some(thread),
            })
        }

        pub fn child_fd(&self) -> RawFd {
            self.child_end.as_raw_fd()
        }
    }

    impl Drop for Watch {
        fn drop(&mut self) {
            let one: u64 = 1;
            // SAFETY: writing eight bytes to an eventfd this value owns.
            unsafe {
                libc::write(
                    self.stop.as_raw_fd(),
                    &one as *const u64 as *const libc::c_void,
                    8,
                );
            }
            // Bounded: a tracee that outlived its hop (no PID namespace) can
            // hold the ptrace route's wait; the thread is then left to end
            // with it.
            if self.done.recv_timeout(Duration::from_secs(5)).is_ok() {
                if let Some(t) = self.thread.take() {
                    let _ = t.join();
                }
            }
        }
    }

    fn now() -> String {
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    fn write_event(log: &mut File, mut ev: serde_json::Value, pid: i32) {
        if let Some(m) = ev.as_object_mut() {
            let mut full = serde_json::Map::new();
            full.insert("t".to_string(), serde_json::Value::String(now()));
            full.insert("pid".to_string(), serde_json::Value::from(pid));
            for (k, v) in std::mem::take(m) {
                full.insert(k, v);
            }
            *m = full;
        }
        let _ = log.write_all(format!("{ev}\n").as_bytes());
    }

    fn supervise(parent: OwnedFd, stop: RawFd, mut log: File, mode: Mode) {
        if !readable(parent.as_raw_fd(), stop) {
            return;
        }
        match recv_first(parent.as_raw_fd()) {
            First::Nothing => {}
            First::Notify(l) => serve_notify(&l, stop, &mut log, mode),
            First::Trace(pid) => {
                write_event(
                    &mut log,
                    serde_json::json!({
                        "call": "watch",
                        "result": "note",
                        "code": "E779",
                        "via": "ptrace",
                        "note": "seccomp notification unavailable; the watch runs on ptrace, slower",
                    }),
                    0,
                );
                serve_trace(pid, parent.as_raw_fd(), &mut log, mode);
            }
        }
    }

    /// Wait until `fd` has something or the stop is written; true for `fd`.
    fn readable(fd: RawFd, stop: RawFd) -> bool {
        loop {
            let mut p = [
                libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: stop,
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: poll over two descriptors, no timeout (the stop wakes it).
            let r = unsafe { libc::poll(p.as_mut_ptr(), 2, -1) };
            if r < 0 {
                if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return false;
            }
            if p[1].revents != 0 {
                return false;
            }
            return p[0].revents != 0;
        }
    }

    fn serve_notify(listener: &OwnedFd, stop: RawFd, log: &mut File, mode: Mode) {
        let fd = listener.as_raw_fd();
        loop {
            let mut p = [
                libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: stop,
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: as in `readable`.
            let r = unsafe { libc::poll(p.as_mut_ptr(), 2, -1) };
            if r < 0 {
                if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return;
            }
            if p[1].revents != 0 {
                return;
            }
            if p[0].revents & libc::POLLIN == 0 {
                // Hung up: no process holds the filter any more.
                return;
            }
            // SAFETY: a zeroed notification is valid; the kernel fills it.
            let mut n: Notif = unsafe { std::mem::zeroed() };
            // SAFETY: ioctl with a pointer to that buffer.
            if unsafe { libc::ioctl(fd, IOCTL_RECV, &mut n as *mut Notif) } < 0 {
                let e = std::io::Error::last_os_error().raw_os_error();
                if matches!(e, Some(libc::EINTR) | Some(libc::ENOENT)) {
                    continue;
                }
                return;
            }
            let mut resp = Resp {
                id: n.id,
                val: 0,
                error: 0,
                flags: RESP_FLAG_CONTINUE,
            };
            let pid = n.pid as i32;
            let nr = n.nr as i64;
            if is_io_uring(nr) {
                resp.flags = 0;
                resp.error = -libc::EPERM;
                write_event(log, io_uring_event(nr), pid);
            } else {
                let mut id = n.id;
                // SAFETY: ioctl with a pointer to the id.
                let valid = unsafe { libc::ioctl(fd, IOCTL_ID_VALID, &mut id as *mut u64) } == 0;
                if valid {
                    if let Some(ev) = describe(pid, nr, &n.args, mode) {
                        write_event(log, ev, pid);
                    }
                }
            }
            // SAFETY: ioctl with a pointer to the answer.
            unsafe { libc::ioctl(fd, IOCTL_SEND, &mut resp as *mut Resp) };
        }
    }

    fn is_io_uring(nr: i64) -> bool {
        nr == libc::SYS_io_uring_setup
            || nr == libc::SYS_io_uring_enter
            || nr == libc::SYS_io_uring_register
    }

    fn io_uring_event(nr: i64) -> serde_json::Value {
        let call = if nr == libc::SYS_io_uring_setup {
            "io_uring_setup"
        } else if nr == libc::SYS_io_uring_enter {
            "io_uring_enter"
        } else {
            "io_uring_register"
        };
        serde_json::json!({
            "call": call,
            "result": "denied",
            "code": "E776",
            "note": "io_uring refused in the box",
        })
    }

    // ---- reading what the hop passed ----

    fn read_mem(pid: i32, addr: u64, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len.min(4096));
        let mut a = addr;
        while out.len() < len {
            let left = 4096 - (a % 4096) as usize;
            let want = left.min(len - out.len());
            let mut buf = vec![0u8; want];
            let local = libc::iovec {
                iov_base: buf.as_mut_ptr() as *mut libc::c_void,
                iov_len: want,
            };
            let remote = libc::iovec {
                iov_base: a as *mut libc::c_void,
                iov_len: want,
            };
            // SAFETY: reading the hop's memory into a buffer of `want` bytes.
            let n = unsafe { libc::process_vm_readv(pid, &local, 1, &remote, 1, 0) };
            if n <= 0 {
                break;
            }
            let n = n as usize;
            out.extend_from_slice(&buf[..n]);
            a += n as u64;
            if n < want {
                break;
            }
        }
        out
    }

    fn read_cstr(pid: i32, addr: u64) -> Option<String> {
        if addr == 0 {
            return None;
        }
        let mut out = Vec::new();
        let mut a = addr;
        while out.len() < 4096 {
            let left = 4096 - (a % 4096) as usize;
            let chunk = read_mem(pid, a, left);
            if chunk.is_empty() {
                break;
            }
            if let Some(z) = chunk.iter().position(|b| *b == 0) {
                out.extend_from_slice(&chunk[..z]);
                return Some(show(&out));
            }
            out.extend_from_slice(&chunk);
            a += chunk.len() as u64;
        }
        if out.is_empty() {
            None
        } else {
            Some(show(&out))
        }
    }

    fn show(bytes: &[u8]) -> String {
        crate::burn::overlay::printable_path(&String::from_utf8_lossy(bytes))
    }

    /// An address as a connect or sendto passes it: `(host, port)` for the
    /// internet, the path for a Unix socket.
    pub fn parse_sockaddr(b: &[u8]) -> serde_json::Value {
        if b.len() < 2 {
            return serde_json::json!({});
        }
        let family = u16::from_ne_bytes([b[0], b[1]]) as i32;
        if family == libc::AF_INET && b.len() >= 8 {
            let port = u16::from_be_bytes([b[2], b[3]]);
            let ip = std::net::Ipv4Addr::new(b[4], b[5], b[6], b[7]);
            return serde_json::json!({"host": ip.to_string(), "port": port});
        }
        if family == libc::AF_INET6 && b.len() >= 24 {
            let port = u16::from_be_bytes([b[2], b[3]]);
            let mut o = [0u8; 16];
            o.copy_from_slice(&b[8..24]);
            let ip = std::net::Ipv6Addr::from(o);
            return serde_json::json!({"host": ip.to_string(), "port": port});
        }
        if family == libc::AF_UNIX {
            let rest = &b[2..];
            let path = if rest.first() == Some(&0) {
                format!("@{}", show(&rest[1..]))
            } else {
                let end = rest.iter().position(|c| *c == 0).unwrap_or(rest.len());
                show(&rest[..end])
            };
            return serde_json::json!({"path": path});
        }
        serde_json::json!({"family": family})
    }

    fn describe(pid: i32, nr: i64, a: &[u64; 6], mode: Mode) -> Option<serde_json::Value> {
        let mut ev = serde_json::Map::new();
        let put =
            |ev: &mut serde_json::Map<String, serde_json::Value>, k: &str, v: serde_json::Value| {
                ev.insert(k.to_string(), v);
            };
        let write_flags = (libc::O_ACCMODE | libc::O_CREAT | libc::O_TRUNC) as u64;
        let open = |path: Option<String>, flags: u64| -> Option<serde_json::Value> {
            let writes = flags & write_flags != 0;
            if !writes && mode < Mode::Reads {
                return None;
            }
            Some(serde_json::json!({
                "call": "open",
                "path": path.unwrap_or_default(),
                "mode": if writes { "write" } else { "read" },
                "result": "seen",
            }))
        };
        let v = if nr == libc::SYS_openat {
            open(read_cstr(pid, a[1]), a[2] & 0xffff_ffff)?
        } else if nr == libc::SYS_openat2 {
            let how = read_mem(pid, a[2], 8);
            let flags = how
                .get(..8)
                .map(|b| u64::from_ne_bytes(b.try_into().unwrap_or([0; 8])))
                .unwrap_or(0);
            open(read_cstr(pid, a[1]), flags)?
        } else if is_open_legacy(nr) {
            let flags = if is_creat(nr) {
                write_flags
            } else {
                a[1] & 0xffff_ffff
            };
            open(read_cstr(pid, a[0]), flags)?
        } else if nr == libc::SYS_connect || nr == libc::SYS_sendto {
            let (addr, len) = if nr == libc::SYS_connect {
                (a[1], a[2])
            } else {
                (a[4], a[5])
            };
            let bytes = read_mem(pid, addr, (len as usize).min(128));
            let mut m = parse_sockaddr(&bytes);
            if let Some(o) = m.as_object_mut() {
                o.insert(
                    "call".to_string(),
                    if nr == libc::SYS_connect {
                        "connect"
                    } else {
                        "sendto"
                    }
                    .into(),
                );
                o.insert("result".to_string(), "seen".into());
            }
            m
        } else if nr == libc::SYS_execve || nr == libc::SYS_execveat {
            let (p, argv) = if nr == libc::SYS_execve {
                (a[0], a[1])
            } else {
                (a[1], a[2])
            };
            let argv0 = read_mem(pid, argv, 8)
                .get(..8)
                .map(|b| u64::from_ne_bytes(b.try_into().unwrap_or([0; 8])))
                .and_then(|ptr| read_cstr(pid, ptr));
            let mut m = serde_json::json!({
                "call": "exec",
                "path": read_cstr(pid, p).unwrap_or_default(),
                "result": "seen",
            });
            if let (Some(o), Some(z)) = (m.as_object_mut(), argv0) {
                o.insert("argv0".to_string(), z.into());
            }
            m
        } else {
            return None;
        };
        if let Some(o) = v.as_object() {
            for (k, x) in o {
                put(&mut ev, k, x.clone());
            }
        }
        if let Ok(p) = std::fs::read_link(format!("/proc/{pid}/exe")) {
            put(
                &mut ev,
                "exe",
                show(p.as_os_str().as_encoded_bytes()).into(),
            );
        }
        Some(serde_json::Value::Object(ev))
    }

    #[cfg(target_arch = "x86_64")]
    fn is_open_legacy(nr: i64) -> bool {
        nr == libc::SYS_open || nr == libc::SYS_creat
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn is_open_legacy(_nr: i64) -> bool {
        false
    }

    #[cfg(target_arch = "x86_64")]
    fn is_creat(nr: i64) -> bool {
        nr == libc::SYS_creat
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn is_creat(_nr: i64) -> bool {
        false
    }

    // ---- the ptrace route ----

    #[cfg(target_arch = "x86_64")]
    fn syscall_of(r: &[u64; 64]) -> (i64, [u64; 6]) {
        (r[15] as i64, [r[14], r[13], r[12], r[7], r[9], r[8]])
    }

    #[cfg(target_arch = "aarch64")]
    fn syscall_of(r: &[u64; 64]) -> (i64, [u64; 6]) {
        (r[8] as i64, [r[0], r[1], r[2], r[3], r[4], r[5]])
    }

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    fn syscall_of(_r: &[u64; 64]) -> (i64, [u64; 6]) {
        (-1, [0; 6])
    }

    /// Make the stopped call return `-EPERM` without running.
    ///
    /// # Safety
    ///
    /// `ptrace` on a tracee that is stopped.
    unsafe fn deny(pid: i32, regs: &mut [u64; 64], len: usize) {
        #[cfg(target_arch = "x86_64")]
        {
            regs[15] = u64::MAX;
            regs[10] = (-(libc::EPERM as i64)) as u64;
        }
        #[cfg(target_arch = "aarch64")]
        {
            regs[0] = (-(libc::EPERM as i64)) as u64;
            let mut nr: u32 = u32::MAX;
            let v = libc::iovec {
                iov_base: &mut nr as *mut u32 as *mut libc::c_void,
                iov_len: 4,
            };
            libc::ptrace(PTRACE_SETREGSET, pid, 0x404usize, &v);
        }
        let v = libc::iovec {
            iov_base: regs.as_mut_ptr() as *mut libc::c_void,
            iov_len: len,
        };
        libc::ptrace(PTRACE_SETREGSET, pid, NT_PRSTATUS, &v);
    }

    fn serve_trace(pid: i32, sock: RawFd, log: &mut File, mode: Mode) {
        // SAFETY: ptrace and wait on a hop this thread seizes; every
        // buffer is on this stack.
        unsafe {
            let opts =
                OPT_TRACESECCOMP | OPT_TRACEFORK | OPT_TRACEVFORK | OPT_TRACECLONE | OPT_EXITKILL;
            let seized = libc::ptrace(PTRACE_SEIZE, pid, 0usize, opts) == 0;
            let ack = if seized { b'1' } else { b'0' };
            libc::write(sock, &ack as *const u8 as *const libc::c_void, 1);
            if !seized {
                return;
            }
            loop {
                let mut status: libc::c_int = 0;
                // Only this thread's tracees: the thread that started the
                // hop waits for the hop itself.
                let w = libc::waitpid(-1, &mut status, libc::__WALL | libc::__WNOTHREAD);
                if w < 0 {
                    if *libc::__errno_location() == libc::EINTR {
                        continue;
                    }
                    return;
                }
                if !libc::WIFSTOPPED(status) {
                    continue;
                }
                let sig = libc::WSTOPSIG(status);
                let event = status >> 16;
                if sig == libc::SIGTRAP && event == PTRACE_EVENT_SECCOMP {
                    let mut regs = [0u64; 64];
                    let v = libc::iovec {
                        iov_base: regs.as_mut_ptr() as *mut libc::c_void,
                        iov_len: std::mem::size_of_val(&regs),
                    };
                    if libc::ptrace(PTRACE_GETREGSET, w, NT_PRSTATUS, &v) == 0 {
                        let (nr, args) = syscall_of(&regs);
                        if is_io_uring(nr) {
                            write_event(log, io_uring_event(nr), w);
                            deny(w, &mut regs, v.iov_len);
                        } else if let Some(ev) = describe(w, nr, &args, mode) {
                            write_event(log, ev, w);
                        }
                    }
                    libc::ptrace(PTRACE_CONT, w, 0usize, 0usize);
                } else if event != 0 {
                    // A fork's or clone's report, or a new tracee's first stop.
                    libc::ptrace(PTRACE_CONT, w, 0usize, 0usize);
                } else {
                    // A signal on its way to the hop: delivered as it was.
                    libc::ptrace(PTRACE_CONT, w, 0usize, sig as usize);
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn every_filter_jump_lands_inside_the_program() {
            for mode in [Mode::Refusals, Mode::Calls, Mode::Reads] {
                for action in [RET_USER_NOTIF, RET_TRACE] {
                    let f = filter(mode, action).unwrap();
                    for (i, ins) in f.iter().enumerate() {
                        if ins.code & 0x07 == 0x05 {
                            assert!(
                                i + 1 + (ins.jt.max(ins.jf) as usize) < f.len(),
                                "{mode:?}: jump {i} leaves the program"
                            );
                        }
                    }
                    assert!(f.len() < 255);
                    // It ends in an allow.
                    assert_eq!(f.last().unwrap().k, RET_ALLOW);
                }
            }
        }

        #[test]
        fn a_refusals_watch_sees_io_uring_and_nothing_else() {
            let f = filter(Mode::Refusals, RET_USER_NOTIF).unwrap();
            let sees = f.iter().filter(|i| i.k == RET_USER_NOTIF).count();
            assert_eq!(sees, 3);
            let g = filter(Mode::Calls, RET_USER_NOTIF).unwrap();
            assert!(g.iter().filter(|i| i.k == RET_USER_NOTIF).count() > 3);
        }

        #[test]
        fn an_address_is_read_as_the_kernel_lays_it_out() {
            let mut v4 = vec![0u8; 16];
            v4[..2].copy_from_slice(&(libc::AF_INET as u16).to_ne_bytes());
            v4[2..4].copy_from_slice(&443u16.to_be_bytes());
            v4[4..8].copy_from_slice(&[45, 12, 0, 7]);
            assert_eq!(
                parse_sockaddr(&v4),
                serde_json::json!({"host": "45.12.0.7", "port": 443})
            );
            let mut un = vec![0u8; 2];
            un[..2].copy_from_slice(&(libc::AF_UNIX as u16).to_ne_bytes());
            un.extend_from_slice(b"/run/x.sock\0");
            assert_eq!(
                parse_sockaddr(&un),
                serde_json::json!({"path": "/run/x.sock"})
            );
        }
    }
}
