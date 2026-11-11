// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// `program` under a hard limit of `secs` seconds, through `timeout` (GNU
/// coreutils, the BSDs) or `gtimeout` (coreutils on a Mac) where one is on
/// PATH. A Mac with neither runs it unguarded, under the CI job's own limit.
pub fn guarded(secs: u32, program: impl AsRef<std::ffi::OsStr>) -> Command {
    match limiter() {
        Some(t) => {
            let mut c = Command::new(t);
            c.arg(secs.to_string()).arg(program);
            c
        }
        None => Command::new(program),
    }
}

fn limiter() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    ["timeout", "gtimeout"].iter().find_map(|name| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|p| p.is_file())
    })
}

pub struct Sandbox {
    pub dir: tempfile::TempDir,
}

impl Sandbox {
    pub fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("tempdir"),
        }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn home(&self) -> PathBuf {
        self.path().join("home")
    }

    pub fn write(&self, name: &str, content: &str) -> PathBuf {
        let p = self.path().join(name);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, content).unwrap();
        p
    }

    pub fn read(&self, name: &str) -> String {
        fs::read_to_string(self.path().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
    }

    /// cig with `redirect` applied by the shell before exec (a key or
    /// passphrase handed over on a descriptor, as CI would), no controlling
    /// terminal (so nothing can prompt and hang), under a hard timeout.
    pub fn cig_with(&self, redirect: &str, env: &[(&str, &str)], args: &[&str]) -> Output {
        self.command(redirect, env, args).output().expect("run cig")
    }

    /// The command `cig_with` runs, not yet started.
    pub fn command(&self, redirect: &str, env: &[(&str, &str)], args: &[&str]) -> Command {
        use std::os::unix::process::CommandExt;
        let mut c = guarded(120, "sh");
        c.arg("-c")
            .arg(format!("exec \"$0\" \"$@\" {redirect}"))
            .arg(env!("CARGO_BIN_EXE_cig"))
            .args(args)
            .current_dir(self.path())
            .env("CIGSCRIPT_HOME", self.home())
            .env("XDG_CONFIG_HOME", self.path().join("xdg-config"))
            .env("XDG_DATA_HOME", self.path().join("xdg-data"))
            .env("XDG_STATE_HOME", self.path().join("xdg-state"))
            .env("XDG_RUNTIME_DIR", self.path().join("xdg-run"))
            .env("NO_COLOR", "1")
            .env("CIG_INTERNAL_TRACKER_DIR", self.path().join("tracker"))
            .env("CIG_UPDATE_MANIFEST", self.path().join("no-releases.json"))
            .env_remove("CI")
            .env_remove("CIG_VAULT_SOCKET");
        for (k, v) in env {
            c.env(k, v);
        }
        // SAFETY: setsid only detaches from the controlling terminal, in
        // the child between fork and exec.
        unsafe {
            c.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        c
    }
}

pub fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}
