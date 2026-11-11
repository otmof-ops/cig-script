// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Passing an open file descriptor over a Unix socket (`SCM_RIGHTS`), and
//! asking who is on the other end.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;

/// Room for a handful of descriptors; a request carries one.
const MAX_FDS: usize = 4;
const CMSG_WORDS: usize = 16;

/// Send `data`, with `fd` attached to its first byte when given.
pub fn send(sock: &UnixStream, data: &[u8], fd: Option<RawFd>) -> io::Result<()> {
    let mut sent = 0;
    if let Some(fd) = fd {
        let mut iov = libc::iovec {
            iov_base: data.as_ptr() as *mut libc::c_void,
            iov_len: data.len(),
        };
        let mut control = [0u64; CMSG_WORDS];
        // SAFETY: the message points at `iov` and `control`, both alive and
        // large enough (CMSG_SPACE of one fd is far under CMSG_WORDS * 8),
        // and the one header written is for one descriptor.
        let n = unsafe {
            let mut msg: libc::msghdr = std::mem::zeroed();
            msg.msg_iov = &mut iov;
            msg.msg_iovlen = 1;
            msg.msg_control = control.as_mut_ptr().cast();
            msg.msg_controllen = libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) as _;
            let cmsg = libc::CMSG_FIRSTHDR(&msg);
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as _;
            std::ptr::copy_nonoverlapping(
                (&fd as *const RawFd).cast::<u8>(),
                libc::CMSG_DATA(cmsg),
                std::mem::size_of::<RawFd>(),
            );
            libc::sendmsg(sock.as_raw_fd(), &msg, send_flags())
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        sent = n as usize;
    }
    while sent < data.len() {
        // SAFETY: writes from a live slice to an open socket.
        let n = unsafe {
            libc::send(
                sock.as_raw_fd(),
                data[sent..].as_ptr().cast(),
                data.len() - sent,
                send_flags(),
            )
        };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        sent += n as usize;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn send_flags() -> libc::c_int {
    libc::MSG_NOSIGNAL
}
#[cfg(not(target_os = "linux"))]
fn send_flags() -> libc::c_int {
    0
}

/// Read what is there into `buf`; descriptors that came with it are pushed
/// onto `fds`. Returns the byte count (0 = the peer closed).
pub fn recv(sock: &UnixStream, buf: &mut [u8], fds: &mut Vec<OwnedFd>) -> io::Result<usize> {
    let mut iov = libc::iovec {
        iov_base: buf.as_mut_ptr().cast(),
        iov_len: buf.len(),
    };
    let mut control = [0u64; CMSG_WORDS];
    loop {
        // SAFETY: the message points at `iov` and `control`, both alive for
        // the call; every descriptor the kernel installs is wrapped in an
        // OwnedFd at once, so none is leaked.
        let n = unsafe {
            let mut msg: libc::msghdr = std::mem::zeroed();
            msg.msg_iov = &mut iov;
            msg.msg_iovlen = 1;
            msg.msg_control = control.as_mut_ptr().cast();
            msg.msg_controllen = (CMSG_WORDS * 8) as _;
            let n = libc::recvmsg(sock.as_raw_fd(), &mut msg, recv_flags());
            if n >= 0 {
                let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
                while !cmsg.is_null() {
                    if (*cmsg).cmsg_level == libc::SOL_SOCKET
                        && (*cmsg).cmsg_type == libc::SCM_RIGHTS
                    {
                        let bytes = (*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize;
                        let count = bytes / std::mem::size_of::<RawFd>();
                        for i in 0..count.min(MAX_FDS * 4) {
                            let mut fd: RawFd = -1;
                            std::ptr::copy_nonoverlapping(
                                libc::CMSG_DATA(cmsg).add(i * std::mem::size_of::<RawFd>()),
                                (&mut fd as *mut RawFd).cast::<u8>(),
                                std::mem::size_of::<RawFd>(),
                            );
                            if fd >= 0 {
                                let owned = OwnedFd::from_raw_fd(fd);
                                set_cloexec(owned.as_raw_fd());
                                fds.push(owned);
                            }
                        }
                    }
                    cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
                }
            }
            n
        };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        return Ok(n as usize);
    }
}

#[cfg(target_os = "linux")]
fn recv_flags() -> libc::c_int {
    libc::MSG_CMSG_CLOEXEC
}
#[cfg(not(target_os = "linux"))]
fn recv_flags() -> libc::c_int {
    0
}

fn set_cloexec(fd: RawFd) {
    // SAFETY: F_SETFD on a descriptor this process owns.
    unsafe {
        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
    }
}

/// The uid of the process on the other end of the socket.
#[cfg(target_os = "linux")]
pub fn peer_uid(sock: &UnixStream) -> io::Result<u32> {
    // SAFETY: SO_PEERCRED fills a ucred of the size given.
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let r = unsafe {
        libc::getsockopt(
            sock.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(cred.uid)
}

#[cfg(all(unix, not(target_os = "linux")))]
pub fn peer_uid(sock: &UnixStream) -> io::Result<u32> {
    let (mut uid, mut gid): (libc::uid_t, libc::gid_t) = (0, 0);
    // SAFETY: getpeereid fills the two integers.
    if unsafe { libc::getpeereid(sock.as_raw_fd(), &mut uid, &mut gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(uid)
}
