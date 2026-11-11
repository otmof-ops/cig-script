// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Windows access control for the state directory and for cig's own process
//! (#105, K2).
//!
//! The state directory (tin, runs, keys) gets a *protected* DACL, one that
//! does not inherit from its parent, with exactly two allowed entries: the
//! current user and SYSTEM, each full control, inherited by what is made
//! beneath it. And the process gets a DACL that leaves the user every right
//! except the ones another process needs to read or write cig's memory
//! (`PROCESS_VM_READ`, `PROCESS_VM_WRITE`, `PROCESS_VM_OPERATION`,
//! `PROCESS_CREATE_THREAD`, `PROCESS_DUP_HANDLE`), so a script running as
//! the same user cannot open cig to read the key out of it. Neither stops an
//! administrator; `cig tin head` makes that detectable.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    GetNamedSecurityInfoW, SetNamedSecurityInfoW, SetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT,
    SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{
    AclSizeInformation, GetAce, GetAclInformation, GetSecurityDescriptorControl,
    GetSecurityDescriptorDacl, GetTokenInformation, TokenUser, ACCESS_ALLOWED_ACE, ACL,
    ACL_SIZE_INFORMATION, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, SE_DACL_PROTECTED, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// SYSTEM, by its well-known SID.
pub const SYSTEM_SID: &str = "S-1-5-18";
/// What a same-user process may not do to cig: PROCESS_CREATE_THREAD (0x2),
/// VM_OPERATION (0x8), VM_READ (0x10), VM_WRITE (0x20), DUP_HANDLE (0x40).
const DENIED_PROCESS_RIGHTS: u32 = 0x2 | 0x8 | 0x10 | 0x20 | 0x40;
/// PROCESS_ALL_ACCESS.
const PROCESS_ALL_ACCESS: u32 = 0x001F_FFFF;

/// What a directory's DACL says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Inheritance from the parent is off.
    pub protected: bool,
    /// The allowed entries: the SID as a string, and the access mask.
    pub allowed: Vec<(String, u32)>,
}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

fn wide_str(s: &str) -> Vec<u16> {
    wide(OsStr::new(s))
}

fn string_of(p: *const u16) -> String {
    let mut n = 0;
    // SAFETY: `p` is a NUL-terminated wide string the system returned.
    unsafe {
        while *p.add(n) != 0 {
            n += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
    }
}

/// The current user's SID, as a string (`S-1-5-21-...`).
pub fn user_sid() -> Result<String, String> {
    let mut token: HANDLE = null_mut();
    // SAFETY: opens the process token for reading; closed below.
    unsafe {
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err("cannot open the process token".to_string());
        }
        let mut need = 0u32;
        GetTokenInformation(token, TokenUser, null_mut(), 0, &mut need);
        let mut buf = vec![0u64; (need as usize).div_ceil(8).max(1)];
        let ok = GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), need, &mut need);
        CloseHandle(token);
        if ok == 0 {
            return Err("cannot read the user from the process token".to_string());
        }
        let user = &*(buf.as_ptr().cast::<TOKEN_USER>());
        sid_string(user.User.Sid)
    }
}

fn sid_string(sid: *mut core::ffi::c_void) -> Result<String, String> {
    let mut out: *mut u16 = null_mut();
    // SAFETY: converts a valid SID; the string is freed with LocalFree.
    unsafe {
        if ConvertSidToStringSidW(sid, &mut out) == 0 {
            return Err("cannot write a SID as text".to_string());
        }
        let s = string_of(out);
        LocalFree(out.cast());
        Ok(s)
    }
}

/// The DACL of an SDDL string; the descriptor must be freed with LocalFree.
fn dacl_of(sddl: &str) -> Result<(PSECURITY_DESCRIPTOR, *mut ACL), String> {
    let text = wide_str(sddl);
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: parses a NUL-terminated SDDL string into a descriptor the
    // system allocates; the DACL pointer points inside it.
    unsafe {
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            null_mut(),
        ) == 0
        {
            return Err(format!("the access list {sddl} is not valid"));
        }
        let (mut present, mut defaulted) = (0i32, 0i32);
        let mut acl: *mut ACL = null_mut();
        if GetSecurityDescriptorDacl(sd, &mut present, &mut acl, &mut defaulted) == 0
            || present == 0
            || acl.is_null()
        {
            LocalFree(sd);
            return Err("the access list has no DACL".to_string());
        }
        Ok((sd, acl))
    }
}

/// The SDDL of the state directory's list: protected, user and SYSTEM only,
/// full control, passed on to files and folders made beneath.
pub fn dir_sddl(user: &str) -> String {
    format!("D:PAI(A;OICI;FA;;;{user})(A;OICI;FA;;;{SYSTEM_SID})")
}

/// The SDDL of the process's list: the user keeps every right but the ones
/// that reach into memory; SYSTEM keeps all.
pub fn process_sddl(user: &str) -> String {
    let mask = PROCESS_ALL_ACCESS & !DENIED_PROCESS_RIGHTS;
    format!("D:(A;;0x{mask:x};;;{user})(A;;GA;;;{SYSTEM_SID})")
}

/// Give `dir` the protected two-entry DACL.
pub fn protect_dir(dir: &Path) -> Result<(), String> {
    let user = user_sid()?;
    let (sd, acl) = dacl_of(&dir_sddl(&user))?;
    let path = wide(dir.as_os_str());
    // SAFETY: applies a DACL we built to a named folder; the descriptor is
    // freed after.
    let rc = unsafe {
        SetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            acl,
            null(),
        )
    };
    // SAFETY: frees what ConvertString... allocated.
    unsafe { LocalFree(sd) };
    if rc != 0 {
        return Err(format!(
            "cannot set the access list on {} (error {rc})",
            dir.display()
        ));
    }
    Ok(())
}

/// What `path`'s DACL holds.
pub fn report(path: &Path) -> Result<Report, String> {
    let name = wide(path.as_os_str());
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    let mut acl: *mut ACL = null_mut();
    // SAFETY: reads the DACL of a named object; the descriptor is freed below.
    unsafe {
        let rc = GetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut acl,
            null_mut(),
            &mut sd,
        );
        if rc != 0 {
            return Err(format!(
                "cannot read the access list of {} (error {rc})",
                path.display()
            ));
        }
        let mut control = 0u16;
        let mut revision = 0u32;
        GetSecurityDescriptorControl(sd, &mut control, &mut revision);
        let protected = control & SE_DACL_PROTECTED != 0;
        let mut allowed = Vec::new();
        let mut result = Ok(());
        if !acl.is_null() {
            let mut size: ACL_SIZE_INFORMATION = std::mem::zeroed();
            if GetAclInformation(
                acl,
                (&mut size as *mut ACL_SIZE_INFORMATION).cast(),
                std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            ) == 0
            {
                result = Err("cannot count the entries".to_string());
            } else {
                for i in 0..size.AceCount {
                    let mut ace: *mut core::ffi::c_void = null_mut();
                    if GetAce(acl, i, &mut ace) == 0 {
                        result = Err("cannot read an entry".to_string());
                        break;
                    }
                    let a = &*(ace.cast::<ACCESS_ALLOWED_ACE>());
                    // Type 0 is ACCESS_ALLOWED; anything else is not ours.
                    let sid = (&a.SidStart as *const u32).cast_mut().cast();
                    let text = sid_string(sid).unwrap_or_else(|_| "?".to_string());
                    let tag = if a.Header.AceType == 0 {
                        text
                    } else {
                        format!("!{text}")
                    };
                    allowed.push((tag, a.Mask));
                }
            }
        }
        LocalFree(sd);
        result.map(|()| Report { protected, allowed })
    }
}

/// Is `dir` private to the user: protected, and holding exactly the user's
/// entry and SYSTEM's, both allowing?
pub fn is_private(dir: &Path) -> Result<bool, String> {
    let user = user_sid()?;
    let r = report(dir)?;
    let mut sids: Vec<&str> = r.allowed.iter().map(|(s, _)| s.as_str()).collect();
    sids.sort_unstable();
    let mut want = [user.as_str(), SYSTEM_SID];
    want.sort_unstable();
    Ok(r.protected && sids == want)
}

/// Put cig's own process behind a DACL that keeps another process of the
/// same user from reading or writing its memory. Best effort; the caller
/// says nothing when it fails (a locked-down account may not allow it).
pub fn protect_process() -> Result<(), String> {
    let user = user_sid()?;
    let (sd, acl) = dacl_of(&process_sddl(&user))?;
    // SAFETY: sets the DACL of this process's own handle; the descriptor is
    // freed after.
    let rc = unsafe {
        SetSecurityInfo(
            GetCurrentProcess(),
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            acl,
            null(),
        )
    };
    // SAFETY: frees what ConvertString... allocated.
    unsafe { LocalFree(sd) };
    if rc != 0 {
        return Err(format!("cannot protect the process (error {rc})"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_protected_directory_holds_exactly_the_user_and_system() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        std::fs::create_dir(&dir).unwrap();
        protect_dir(&dir).unwrap();
        let r = report(&dir).unwrap();
        assert!(r.protected, "{r:?}: the list still inherits");
        assert_eq!(r.allowed.len(), 2, "{r:?}");
        let user = user_sid().unwrap();
        assert!(r.allowed.iter().any(|(s, _)| *s == user), "{r:?}");
        assert!(r.allowed.iter().any(|(s, _)| s == SYSTEM_SID), "{r:?}");
        assert!(is_private(&dir).unwrap());
        // What is made beneath it inherits the same two and no more.
        let child = dir.join("tin");
        std::fs::create_dir(&child).unwrap();
        let c = report(&child).unwrap();
        let mut got: Vec<&str> = c.allowed.iter().map(|(s, _)| s.as_str()).collect();
        got.sort_unstable();
        let mut want = [user.as_str(), SYSTEM_SID];
        want.sort_unstable();
        assert_eq!(got, want, "{c:?}");
    }

    #[test]
    fn an_ordinary_directory_is_not_private() {
        let tmp = tempfile::tempdir().unwrap();
        // A fresh folder inherits its parent's list: not protected.
        let dir = tmp.path().join("plain");
        std::fs::create_dir(&dir).unwrap();
        assert!(!is_private(&dir).unwrap());
    }

    /// Does this process's token get past any process DACL: SYSTEM, or
    /// SeDebugPrivilege enabled?
    fn token_passes_any_dacl() -> bool {
        use windows_sys::Win32::Foundation::LUID;
        use windows_sys::Win32::Security::{
            LookupPrivilegeValueW, TokenPrivileges, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED,
            TOKEN_PRIVILEGES,
        };
        if user_sid().map(|u| u == SYSTEM_SID).unwrap_or(false) {
            return true;
        }
        let name = wide_str("SeDebugPrivilege");
        let mut luid = LUID {
            LowPart: 0,
            HighPart: 0,
        };
        let mut token: HANDLE = null_mut();
        // SAFETY: reads this process's own token into a buffer sized by the
        // first call; the token is closed before the buffer is read.
        unsafe {
            if LookupPrivilegeValueW(null(), name.as_ptr(), &mut luid) == 0
                || OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0
            {
                return false;
            }
            let mut need = 0u32;
            GetTokenInformation(token, TokenPrivileges, null_mut(), 0, &mut need);
            let mut buf = vec![0u64; (need as usize).div_ceil(8).max(1)];
            let ok = GetTokenInformation(
                token,
                TokenPrivileges,
                buf.as_mut_ptr().cast(),
                need,
                &mut need,
            );
            CloseHandle(token);
            if ok == 0 {
                return false;
            }
            let tp = &*(buf.as_ptr().cast::<TOKEN_PRIVILEGES>());
            let all: &[LUID_AND_ATTRIBUTES] =
                std::slice::from_raw_parts(tp.Privileges.as_ptr(), tp.PrivilegeCount as usize);
            all.iter().any(|p| {
                p.Luid.LowPart == luid.LowPart
                    && p.Luid.HighPart == luid.HighPart
                    && p.Attributes & SE_PRIVILEGE_ENABLED != 0
            })
        }
    }

    #[test]
    fn the_process_list_keeps_memory_rights_from_the_same_user() {
        use windows_sys::Win32::System::Threading::{GetCurrentProcessId, OpenProcess};
        let user = user_sid().unwrap();
        assert!(
            process_sddl(&user).contains("0x1fff85"),
            "{}",
            process_sddl(&user)
        );
        protect_process().unwrap();
        if token_passes_any_dacl() {
            // SYSTEM, or SeDebugPrivilege switched on (some CI runners): such
            // a token opens any process whatever its list says.
            eprintln!("this token passes every process DACL; the open proves nothing here");
            return;
        }
        // SAFETY: asks for a handle to this very process; closed if given.
        let h = unsafe { OpenProcess(0x0010, 0, GetCurrentProcessId()) };
        if !h.is_null() {
            unsafe { CloseHandle(h) };
        }
        assert!(
            h.is_null(),
            "another handle to this process could read its memory"
        );
    }
}
