// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! How the recorder and the verifier see the file system: a file's
//! identity, its digest, a folder's listing, a path as bytes, and the
//! clocks. The recorder and the verifier use these same functions, so what
//! was recorded is read again the same way.

use crate::encode::{Identity, Mono, PresentKind};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// A path as the bytes a pass carries: the raw bytes on Unix, UTF-8 on
/// Windows (`None` for a Windows path that is not valid Unicode).
pub fn path_bytes(p: &Path) -> Option<Vec<u8>> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Some(p.as_os_str().as_bytes().to_vec())
    }
    #[cfg(not(unix))]
    {
        p.to_str().map(|s| s.as_bytes().to_vec())
    }
}

/// The path a pass names, on this system (`None` when the bytes cannot be
/// a path here: invalid UTF-8 on Windows).
pub fn bytes_path(b: &[u8]) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(b)))
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(b).ok().map(PathBuf::from)
    }
}

/// A path's bytes for a message: lossy, never fails.
pub fn shown(b: &[u8]) -> String {
    clean(&String::from_utf8_lossy(b))
}

/// A file's identity from its metadata.
pub fn identity(md: &fs::Metadata) -> Identity {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Identity {
            dev: md.dev(),
            ino: md.ino(),
            size: md.size(),
            mtime_ns: md.mtime().saturating_mul(1_000_000_000) + md.mtime_nsec(),
            ctime_ns: md.ctime().saturating_mul(1_000_000_000) + md.ctime_nsec(),
            mode: md.mode(),
        }
    }
    #[cfg(not(unix))]
    {
        let mtime_ns = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
            .unwrap_or(0);
        Identity {
            size: md.len(),
            mtime_ns,
            ..Identity::default()
        }
    }
}

/// Can identity alone vouch for a file on this system? Only where it has
/// an inode and a change time: on Unix.
pub fn identity_is_enough() -> bool {
    cfg!(unix)
}

/// The racy rule: a file whose mtime is within two seconds of the moment
/// the pass was issued (or after it) is hashed even when its identity
/// matches, so an edit in the same tick as the issue is never missed.
pub fn is_racy(mtime_ns: i64, issued_ms: i64) -> bool {
    let issued_ns = issued_ms.saturating_mul(1_000_000);
    mtime_ns >= issued_ns.saturating_sub(2_000_000_000)
}

/// BLAKE3 of a file's bytes, streamed.
pub fn hash_file(path: &Path) -> io::Result<[u8; 32]> {
    let mut f = fs::File::open(path)?;
    hash_reader(&mut f)
}

/// BLAKE3 of everything a reader yields.
pub fn hash_reader(r: &mut dyn Read) -> io::Result<[u8; 32]> {
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(*h.finalize().as_bytes())
}

/// BLAKE3 of bytes in hand.
pub fn hash_bytes(b: &[u8]) -> [u8; 32] {
    *blake3::hash(b).as_bytes()
}

/// A variable's value as a pass records it.
pub fn env_digest(value: &[u8]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"cigpass1-env");
    h.update(value);
    *h.finalize().as_bytes()
}

/// The type of one folder entry, without following a link.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EntryType {
    File = 1,
    Dir = 2,
    Link = 3,
    Other = 4,
}

impl EntryType {
    pub fn of(ft: &fs::FileType) -> EntryType {
        if ft.is_symlink() {
            EntryType::Link
        } else if ft.is_dir() {
            EntryType::Dir
        } else if ft.is_file() {
            EntryType::File
        } else {
            EntryType::Other
        }
    }
}

/// A folder's entries: names and types, sorted by name bytes.
pub fn list_dir(path: &Path) -> io::Result<Vec<(Vec<u8>, EntryType)>> {
    let mut out = Vec::new();
    for e in fs::read_dir(path)? {
        let e = e?;
        let name = path_bytes(Path::new(&e.file_name())).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "a name that is not UTF-8")
        })?;
        let ft = e.file_type()?;
        out.push((name, EntryType::of(&ft)));
    }
    out.sort();
    Ok(out)
}

/// The digest of a folder's listing.
pub fn listing_digest(entries: &[(Vec<u8>, EntryType)]) -> [u8; 32] {
    let mut sorted: Vec<&(Vec<u8>, EntryType)> = entries.iter().collect();
    sorted.sort();
    sorted.dedup();
    let mut h = blake3::Hasher::new();
    h.update(b"cigpass1-dir");
    for (name, ty) in sorted {
        h.update(&(name.len() as u32).to_le_bytes());
        h.update(name);
        h.update(&[*ty as u8]);
    }
    *h.finalize().as_bytes()
}

/// What is at a path now, following links, for a presence check.
pub fn present_kind(path: &Path) -> io::Result<PresentKind> {
    let md = fs::metadata(path)?;
    Ok(if md.is_file() {
        PresentKind::File
    } else if md.is_dir() {
        PresentKind::Dir
    } else {
        PresentKind::Other
    })
}

/// The wall clock, in unix milliseconds.
pub fn wall_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

/// A reading of a clock that never goes back, with the boot it belongs
/// to. Linux only: the boot id from the kernel, the time since boot from
/// `CLOCK_BOOTTIME`. Elsewhere `None`, and time caveats rest on the wall
/// clock alone.
pub fn mono_now() -> Option<Mono> {
    #[cfg(target_os = "linux")]
    {
        let id = fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
        let hexd: String = id.trim().chars().filter(|c| *c != '-').collect();
        if hexd.len() != 32 {
            return None;
        }
        let mut boot_id = [0u8; 16];
        for (i, b) in boot_id.iter_mut().enumerate() {
            *b = u8::from_str_radix(hexd.get(i * 2..i * 2 + 2)?, 16).ok()?;
        }
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: clock_gettime writes one timespec through a valid pointer.
        let rc = unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) };
        if rc != 0 {
            return None;
        }
        let ns = (ts.tv_sec as u64)
            .saturating_mul(1_000_000_000)
            .saturating_add(ts.tv_nsec as u64);
        Some(Mono { boot_id, ns })
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Must this path's file always be hashed, whatever its identity says?
/// Network and FUSE file systems cache attributes, so a stat there can be
/// stale; on pseudo file systems (`/proc`, `/sys` and their kin) a file's
/// times do not move when its contents do, so a sysctl written or a module
/// loaded leaves the identity as it was. Linux reads the file system's
/// type; elsewhere identity alone is trusted only where
/// `identity_is_enough` says.
pub fn always_hash(path: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return true;
        };
        // SAFETY: statfs writes one struct through a valid pointer, and `c`
        // is a NUL-terminated path that outlives the call.
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::statfs(c.as_ptr(), &mut st) };
        if rc != 0 {
            return true;
        }
        const ALWAYS: &[u32] = &[
            0x6969,      // NFS
            0x517B,      // SMB
            0xFF53_4D42, // CIFS
            0xFE53_4D42, // SMB2
            0x6573_5546, // FUSE
            0x0102_1997, // 9P
            0x0BD0_0BD0, // Lustre
            0x5346_414F, // AFS
            0x9FA0,      // proc
            0x6265_6572, // sysfs
            0x0027_E0EB, // cgroup
            0x6367_7270, // cgroup2
            0x6462_6720, // debugfs
            0x7472_6163, // tracefs
            0x7363_6673, // securityfs
            0x6265_6570, // configfs
            0xDE5E_81E4, // efivarfs
            0xCAFE_4A11, // bpf
        ];
        // The magic is 32 bits; `f_type` is wider or signed on some
        // targets, so compare its low 32 bits.
        #[allow(clippy::unnecessary_cast)]
        let t = st.f_type as u32;
        ALWAYS.contains(&t)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        false
    }
}

/// What hashing a file for a check found.
pub struct Hashed {
    pub digest: [u8; 32],
    pub identity: Identity,
    pub bytes: u64,
}

/// Hash a regular file through one handle, opened without blocking (a FIFO
/// or a device at the path never stalls the verifier) and checked on that
/// handle: a regular file, at most `cap` bytes.
pub fn hash_regular(path: &Path, cap: u64) -> Result<Hashed, String> {
    let mut opts = fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NONBLOCK);
    }
    let mut f = opts.open(path).map_err(|e| e.to_string())?;
    let md = f.metadata().map_err(|e| e.to_string())?;
    if !md.is_file() {
        return Err("it is not a regular file".to_string());
    }
    if md.len() > cap {
        return Err("it is larger than this check will read".to_string());
    }
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut n_total = 0u64;
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        n_total += n as u64;
        if n_total > cap {
            return Err("it is larger than this check will read".to_string());
        }
        h.update(&buf[..n]);
    }
    Ok(Hashed {
        digest: *h.finalize().as_bytes(),
        identity: identity(&md),
        bytes: n_total,
    })
}

/// Text that came from a pass, made safe to print: control characters,
/// escapes, bidirectional overrides and zero-width characters are shown as
/// `\u{..}`, and anything past 300 characters is cut with `...`.
pub fn clean(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(320));
    for (i, ch) in s.chars().enumerate() {
        if i >= 300 {
            out.push_str("...");
            break;
        }
        let bad = ch.is_control()
            || matches!(ch, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}');
        if bad {
            out.push_str(&format!("\\u{{{:x}}}", ch as u32));
        } else {
            out.push(ch);
        }
    }
    out
}

/// Is this file racy right now: changed within two seconds of `observed_ms`
/// (or after it), by its mtime or its ctime?
pub fn racy_at(identity: &Identity, observed_ms: i64) -> bool {
    let newest = identity.mtime_ns.max(identity.ctime_ns);
    is_racy(newest, observed_ms)
}
