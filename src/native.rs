// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! A stage `c` program's native code, from the program's file to code
//! the VM can call (cig-compile's decision 0006).
//!
//! A stage `c` program is a stage `b` program with a `native` section:
//! machine code for some of its functions. This module finds the
//! section ([`Source::of`]) and, when a run first calls one of the
//! script's functions, loads it ([`Source::load`]): reads it, checks it,
//! maps its code from the program's own file and makes the mapping
//! callable. Whatever stops that is an [`Unused`], and the program runs
//! its bytecode: nothing a program does depends on its native code
//! being used.
//!
//! This is the one place cig answers for bytes it runs
//! ([`cig_native::Mapped::arm`]). The VM, which calls the code, has no
//! `unsafe` of its own.

use crate::program::{Opened, Stage, NATIVE_SECTION};
use cig_native::{Arch, Code, MapError, Mapped, Tables};
use sha2::{Digest, Sha256};

/// Why a program runs without its native code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unused {
    /// This cig has no loader for the machine it runs on.
    Unsupported,
    /// The section is not one the compiler wrote for this program on
    /// this machine: what is wrong with it.
    Damaged(String),
    /// The system would not give the mapping: its reason.
    Refused(String),
}

impl std::fmt::Display for Unused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unused::Unsupported => f.write_str("this cig loads native code on Linux only"),
            Unused::Damaged(what) => write!(f, "section `native` is damaged: {what}"),
            Unused::Refused(why) => write!(f, "the system did not map section `native`: {why}"),
        }
    }
}

/// Where a program's `native` section lies in its own file: what loads
/// it when a run first needs it.
pub struct Source {
    file: std::fs::File,
    offset: u64,
    len: u64,
    sha256: [u8; 32],
}

impl Source {
    /// A stage `c` program's native section. `None` for any other
    /// program; when `CIG_NATIVE=0` says to run the bytecode alone; and
    /// when the program's file cannot be shown to be the image this
    /// process runs: only that file is ever mapped.
    pub fn of(program: &Opened) -> Option<Source> {
        if program.stage() != Stage::C || !crate::program::native_by_env() {
            return None;
        }
        let place = program.place(NATIVE_SECTION)?;
        Some(Source {
            file: running_image(place.file)?,
            offset: place.offset,
            len: place.len,
            sha256: place.sha256,
        })
    }

    /// The section as it lies in the file, checked against the sha256
    /// the program's section table gives, and its tables.
    fn read(&self) -> Result<(Vec<u8>, Tables), Unused> {
        let body = crate::program::read_at(&mut &self.file, self.offset, self.len)
            .map_err(Unused::Damaged)?;
        if Sha256::digest(&body)[..] != self.sha256 {
            return Err(Unused::Damaged(
                "its bytes do not match the sha256 in the section table".to_string(),
            ));
        }
        let tables = Tables::parse(&body).map_err(|e| Unused::Damaged(e.to_string()))?;
        Ok((body, tables))
    }

    /// What the section holds, read and checked, without loading it:
    /// the machine its code is for, and how many entries it has.
    pub fn describe(&self) -> Result<(Arch, usize), Unused> {
        let (_, tables) = self.read()?;
        Ok((tables.arch, tables.entries.len()))
    }

    /// Load the section for the module this process runs: `module` is
    /// the module's hash and `arity` gives function `i`'s parameter
    /// count (`None` past the module's last function).
    ///
    /// Refused as damaged: bytes that do not match the table's sha256, a
    /// body that does not read, code for another machine or compiled
    /// from another module, and an entry for the top level, for a
    /// function the module does not have, or with another argument count
    /// than its function's.
    pub fn load(
        &self,
        module: &[u8; 32],
        arity: impl Fn(u32) -> Option<usize>,
    ) -> Result<(Code, Tables), Unused> {
        let Some(host) = Arch::host() else {
            return Err(Unused::Unsupported);
        };
        let (body, tables) = self.read()?;
        let damaged = |what: String| Err(Unused::Damaged(what));
        if tables.arch != host {
            return damaged("its code is for another machine".to_string());
        }
        if &tables.module != module {
            return damaged("it was compiled from another bytecode module".to_string());
        }
        for (at, e) in tables.entries.iter().enumerate() {
            match arity(e.func) {
                _ if e.func == 0 => return damaged(format!("entry {at} is for the top level")),
                None => {
                    return damaged(format!(
                        "entry {at} is for function {}, which the module does not have",
                        e.func
                    ))
                }
                Some(n) if n != e.args.len() => {
                    return damaged(format!(
                        "entry {at} takes {} arguments, and function {} has {n} parameters",
                        e.args.len(),
                        e.func
                    ))
                }
                Some(_) => {}
            }
        }
        let code_len = tables.code_len as usize;
        let Some(code) = body.get(..code_len) else {
            return damaged("its code is longer than it is".to_string());
        };
        let refused = |e: MapError| match e {
            MapError::Unsupported => Unused::Unsupported,
            MapError::Refused(e) => Unused::Refused(e.to_string()),
            e => Unused::Damaged(e.to_string()),
        };
        let mapped = Mapped::map(&self.file, self.offset, code_len).map_err(refused)?;
        // SAFETY: `arm` asks its caller for two things.
        //
        // The file does not change under the mapping. `self.file` is the
        // image this process is running ([`running_image`] made sure of
        // it): the system maps this process's own code from that same
        // file, private and executable, exactly as this mapping is. If
        // that file could change under the process, the code now running
        // would change with it; this mapping is exposed to nothing the
        // process is not already exposed to, and where the system
        // refuses writes to a running executable, as Linux does, to
        // nothing at all. The mapping lives until the process ends.
        //
        // The bytes are code to run. Nothing here can show that: no
        // check tells generated code from any other, and the hashes
        // below show that the file is whole, not who made it. What makes
        // it sound to run these bytes is whose they are: they are part
        // of the executable this process is, put there by whoever built
        // it, as the runtime's own code was, which is already running.
        // Running them trusts nobody the process does not already trust.
        // What the checks add is that they are the bytes that builder
        // meant: the section matches the sha256 its table gives (checked
        // in `read`), the program's head covers the table (checked when
        // the program was opened), `arm` checks that the mapping holds
        // the bytes that were read, the footer names the bytecode module
        // this process loaded, and every entry is for a function of that
        // module with its parameter count (checked above). `cigcompile`
        // makes a section whose entries follow the convention `arm`
        // states; a builder that put other code there has made a program
        // that does something else, as one that changed the runtime has.
        let code = unsafe { mapped.arm(code, &tables) }.map_err(refused)?;
        Ok((code, tables))
    }
}

/// A new handle on the image this process runs, when `file` is that
/// image: the one file whose bytes are as stable as this process's own
/// code. `None` when it is not, or when the system gives no way to tell.
#[cfg(target_os = "linux")]
fn running_image(file: &std::fs::File) -> Option<std::fs::File> {
    use std::os::unix::fs::MetadataExt;
    let image = std::fs::File::open("/proc/self/exe").ok()?;
    let (a, b) = (image.metadata().ok()?, file.metadata().ok()?);
    (a.dev() == b.dev() && a.ino() == b.ino()).then_some(image)
}

/// No target but Linux has a loader: there is nothing to map.
#[cfg(not(target_os = "linux"))]
fn running_image(_file: &std::fs::File) -> Option<std::fs::File> {
    None
}

#[cfg(test)]
mod tests {
    use crate::value::Value;

    /// `cig_native::value`'s constants describe cig's own `Value`: a
    /// null, a true, an int and a float carry their tag and payload
    /// where the constants say they do, so native code and the runtime
    /// read the same bytes for the same values.
    #[test]
    fn cigs_value_agrees_with_cig_natives_constants() {
        assert!(cig_native::value::agrees(
            &Value::Null,
            &Value::Bool(true),
            (&Value::Int(-123_456_789), -123_456_789),
            (&Value::Float(2.5_f64), 2.5_f64),
        ));
        // Every tag of a value that owns memory sorts at or above
        // OWNS: native code never writes over such a register.
        assert!(cig_native::value::OWNS >= 4);
    }
}
