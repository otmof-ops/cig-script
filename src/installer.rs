// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! The seam to `ciginstaller`: installing, updating, uninstalling and the
//! tool half of doctor are the installer's job, and the `cig` commands that
//! used to do them are stubs that hand over to it.
//!
//! Every stub behaves the same way. It looks for `ciginstaller` on `PATH`,
//! then in `$CIGSCRIPT_HOME/bin`, then in `~/.cigscript/bin`; runs it with
//! the mapped arguments, stdio inherited; and passes its exit code through.
//! When there is none it says whose job this is and how to get the
//! installer, and exits 127. Nothing here ever downloads or installs.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Exit code when the installer cannot be found or started (the shell's own
/// "command not found").
pub const MISSING: i32 = 127;

/// How to get the installer on a machine that has none. One line, shown
/// after every missing-installer message and in the retired install scripts.
pub const BOOTSTRAP: &str = "https://github.com/otmof-ops/cig-installer#install";

#[cfg(windows)]
const EXE: &str = "ciginstaller.exe";
#[cfg(not(windows))]
const EXE: &str = "ciginstaller";

fn is_program(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// The directories to look in, in order: `PATH`, `$CIGSCRIPT_HOME/bin`,
/// `~/.cigscript/bin`.
fn search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(h) = std::env::var_os("CIGSCRIPT_HOME") {
        dirs.push(PathBuf::from(h).join("bin"));
    }
    if let Some(h) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        dirs.push(PathBuf::from(h).join(".cigscript").join("bin"));
    }
    dirs
}

/// Where `ciginstaller` is, if it is anywhere.
pub fn find() -> Option<PathBuf> {
    search_dirs()
        .into_iter()
        .filter(|d| !d.as_os_str().is_empty())
        .map(|d| d.join(EXE))
        .find(|c| is_program(c))
}

/// The two lines a stub prints when there is no installer.
pub fn missing_message(what: &str, command: &str) -> String {
    format!("{what} is ciginstaller's job now: {command}\nget ciginstaller: {BOOTSTRAP}")
}

/// Hand `what` over to `ciginstaller args...` and return its exit code.
/// `command` is the human form of the same call, for the missing case.
pub fn run(what: &str, args: &[&str]) -> i32 {
    let command = format!("ciginstaller {}", args.join(" "));
    let Some(exe) = find() else {
        eprintln!("{}", missing_message(what, command.trim_end()));
        return MISSING;
    };
    match Command::new(&exe).args(args).status() {
        Ok(status) => match status.code() {
            Some(code) => code,
            None => {
                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt;
                    status.signal().map_or(1, |s| 128 + s)
                }
                #[cfg(not(unix))]
                {
                    1
                }
            }
        },
        Err(e) => {
            eprintln!("cannot start {}: {e}", exe.display());
            eprintln!("{}", missing_message(what, command.trim_end()));
            MISSING
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_missing_message_names_the_job_and_the_way_to_get_the_installer() {
        let m = missing_message("cig update", "ciginstaller update --check");
        assert_eq!(
            m.lines().next(),
            Some("cig update is ciginstaller's job now: ciginstaller update --check")
        );
        assert!(m.lines().nth(1).unwrap().starts_with("get ciginstaller: "));
    }
}
