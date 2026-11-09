// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! A pass's recording, inside the interpreter (cig-script#186).
//!
//! While `pass.issue` runs its work, every input the work depends on must
//! be recorded, and nothing the recording cannot see may reach the result:
//!
//! - every builtin is classified in one table: recorded (the builtin notes
//!   what it read), pure, or open-world. A builtin not in the table is
//!   open-world, so one added later cannot slip past a recording;
//! - a builtin that changes the world (a burn inside the work) is refused;
//! - a variable of any scope made before the work began (the function
//!   around it, a closure's captured scope) is refused (E732);
//! - a global (or `args`) the work reads is recorded by a digest of the
//!   value it held, and a pass found later holds only while the running
//!   script's global holds the same value. However the global was built or
//!   changed, the pass follows it.

use super::env::Env;
use super::Interp;
use crate::diagnostics::{burn, Diagnostic};
use crate::syntax::span::Span;
use crate::value::{Builtin, Effect, Value};

/// One `pass.issue` being recorded.
pub(crate) struct RecFrame {
    pub rec: cig_pass::Recorder,
    /// Scopes with a serial below this existed before the work began.
    pub barrier: u64,
    /// The pass will carry a time limit, so the work may read the clock.
    pub clock: bool,
    /// Globals already recorded in this work, by name.
    pub globals: std::collections::HashMap<String, [u8; 32]>,
}

impl RecFrame {
    pub fn new(barrier: u64, clock: bool) -> RecFrame {
        RecFrame {
            rec: cig_pass::Recorder::new(),
            barrier,
            clock,
            globals: std::collections::HashMap::new(),
        }
    }
}

/// Builtins that record what they read (each notes its inputs itself).
pub(crate) const RECORDED: &[&str] = &[
    "fs.read_text",
    "fs.read_lines",
    "fs.exists",
    "fs.is_file",
    "fs.is_dir",
    "fs.size",
    "fs.list",
    "fs.glob",
    "fs.cwd",
    "fs.home",
    "env.get",
    "env.has",
    "env.all",
    "hash.sha256_file",
    "proc.which",
    "path.abs",
    "json.load",
    "csv.read",
    "proc.run",
    "proc.text",
    "proc.lines",
    "proc.json",
    "proc.csv",
    "proc.kv",
    "proc.pipe",
    "proc.shell",
    "pass.find",
    "pass.check",
    "pass.show",
    "pass.issue",
    "pass.memo",
    "pass.narrow",
    "pass.recording",
];

/// Builtins that depend on nothing outside their arguments.
pub(crate) const PURE: &[&str] = &[
    "len",
    "str",
    "repr",
    "int",
    "float",
    "bool",
    "type_of",
    "range",
    "keys",
    "values",
    "entries",
    "assert",
    "exit",
    "light",
    "log.debug",
    "log.info",
    "log.warn",
    "log.error",
    "cig.version",
    "cig.os",
    "cig.arch",
    "path.join",
    "path.base",
    "path.dir",
    "path.ext",
    "path.stem",
    "path.with_ext",
    "path.normalize",
    "path.is_abs",
    "path.parts",
    "math.abs",
    "math.min",
    "math.max",
    "math.floor",
    "math.ceil",
    "math.round",
    "math.sqrt",
    "math.pow",
    "math.clamp",
    "csv.parse",
    "csv.stringify",
    "json.parse",
    "json.stringify",
    "text.matches",
    "text.find",
    "text.find_all",
    "text.captures",
    "text.replace",
    "text.split",
    "text.template",
    "time.sleep_ms",
    "time.format_ms",
    "hash.sha256",
    "hash.short",
];

/// Clock reads: allowed only when the pass carries a time limit.
pub(crate) const CLOCK: &[&str] = &["time.now_ms", "time.now_iso", "time.stamp"];

/// How a recording treats a builtin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    /// It notes its own inputs.
    Recorded,
    /// It depends on its arguments alone.
    Pure,
    /// It reads the clock.
    Clock,
    /// Nothing a recording can see into.
    OpenWorld,
    /// It changes the world.
    Refused,
}

/// The table: every builtin's class while recording. Anything not in it is
/// open-world.
pub(crate) fn class_of(b: &Builtin) -> Class {
    if matches!(b.effect, Effect::Write | Effect::Env) || b.name == "pass.spend" {
        return Class::Refused;
    }
    if RECORDED.contains(&b.name) {
        Class::Recorded
    } else if PURE.contains(&b.name) {
        Class::Pure
    } else if CLOCK.contains(&b.name) {
        Class::Clock
    } else {
        Class::OpenWorld
    }
}

fn e732(msg: String, span: Span) -> Diagnostic {
    burn(msg).code("E732").at(span).with_hint(
        "put what the work needs in its subject (pack(s) reads s.name), or read it inside the work",
    )
}

impl Interp {
    /// Is a pass's work being recorded?
    pub(crate) fn recording(&self) -> bool {
        !self.recs.is_empty()
    }

    /// The innermost recorder, while recording.
    pub(crate) fn recorder(&mut self) -> Option<&mut cig_pass::Recorder> {
        self.recs.last_mut().map(|f| &mut f.rec)
    }

    /// A name's value, checked against the recording: a variable from
    /// before the work began is E732; a global (or `args`) is recorded by
    /// the digest of what it holds.
    pub(crate) fn recorded_get(
        &mut self,
        env: &Env,
        name: &str,
        span: Span,
    ) -> Option<Result<Value, Diagnostic>> {
        let (value, serial, _mutable) = env.get_traced(name)?;
        let (prelude, globals) = (self.prelude.serial(), self.globals.serial());
        let frame = self.recs.last_mut()?;
        if serial >= frame.barrier {
            return Some(Ok(value));
        }
        if serial == prelude || serial == globals {
            // Code: a pull, a pack, a builtin, a module or a chain. Its
            // text is the script's (recorded with it); what a closure
            // captured is checked where its body reads it.
            if matches!(
                value,
                Value::Func(_) | Value::Builtin(_) | Value::Module(_) | Value::Chain(_)
            ) {
                return Some(Ok(value));
            }
            if !frame.globals.contains_key(name) {
                match crate::pass::digest_value(&value) {
                    Some(d) => {
                        frame.globals.insert(name.to_string(), d);
                        frame.rec.rules(&format!("global:{name}"), d);
                    }
                    None => frame.rec.open_world(format!(
                        "the work read `{name}`, which holds something no digest can name"
                    )),
                }
            }
            return Some(Ok(value));
        }
        Some(Err(e732(
            format!("the work read `{name}` from outside it: a pass found later would vouch for whatever it held then"),
            span,
        )))
    }

    /// May the work assign to this name? Only to its own variables.
    pub(crate) fn recorded_assign(
        &self,
        env: &Env,
        name: &str,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let Some(frame) = self.recs.last() else {
            return Ok(());
        };
        match env.owner_serial(name) {
            Some(s) if s < frame.barrier => Err(e732(
                format!("the work changed `{name}`, which lives outside it: a pass found later would skip that change"),
                span,
            )),
            _ => Ok(()),
        }
    }

    /// May this builtin run inside the work? Open-world calls are noted
    /// (the pass will not be issued); world changes are refused.
    pub(crate) fn recording_admits(&mut self, b: &Builtin, span: Span) -> Result<(), Diagnostic> {
        let clock = self.recs.last().is_some_and(|f| f.clock);
        match class_of(b) {
            Class::Recorded | Class::Pure => Ok(()),
            Class::Clock if clock => Ok(()),
            Class::Clock => {
                if let Some(r) = self.recorder() {
                    r.open_world(format!("`{}` read the clock and the pass has no `until`", b.name));
                }
                Ok(())
            }
            Class::OpenWorld => {
                if let Some(r) = self.recorder() {
                    r.open_world(format!("`{}` was called, and a pass cannot see what it depends on", b.name));
                }
                Ok(())
            }
            Class::Refused => Err(burn(format!(
                "the work called `{}`, which changes the world: a pass found later would skip that change",
                b.name
            ))
            .code("E733")
            .at(span)
            .with_hint("keep the work to reads; change the world outside pass.issue")),
        }
    }
}
