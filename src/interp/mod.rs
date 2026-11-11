// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The tree-walking evaluator.

pub mod env;
mod eval;
pub(crate) mod record;

pub use eval::{closest, error_value, normalize_index};

use crate::burn::{Decision, Kernel, Op};
use crate::diagnostics::{burn as burn_error, Diagnostic, Kind};
use crate::syntax::ast::Program;
use crate::syntax::span::Span;
use crate::value::Value;
use env::{Env, Scope};
use std::io::Write;
use std::path::Path;

/// Deepest nesting of script calls before the interpreter refuses.
pub const MAX_CALL_DEPTH: usize = 4000;

pub struct Interp {
    /// Builtins and `args`: what a top-level `roll` may shadow.
    prelude: Env,
    pub globals: Env,
    pub kernel: Kernel,
    pub out: Box<dyn Write>,
    pub err: Box<dyn Write>,
    burn_depth: u32,
    unlit_depth: u32,
    call_depth: usize,
    /// Set by `cough`, consumed by the nearest `ashtray`.
    pub(crate) cough_payload: Option<Value>,
    /// Name of the script being run, for diagnostics.
    pub script_name: Option<String>,
    /// Exit code requested by `exit(n)`, if any.
    pub exit_requested: Option<i32>,
    /// Statements and loop iterations executed so far.
    pub steps: u64,
    /// The step budget; 0 means none. `--max-steps` and `CIG_MAX_STEPS`.
    pub max_steps: u64,
    pub(crate) loop_depth: u32,
    /// Chains being lit right now, outermost first, to catch a chain that
    /// lights itself (E604) and to tag ops with their owner.
    pub(crate) chain_stack: Vec<String>,
    /// Passes being issued, innermost last (cig-script#186).
    pub(crate) recs: Vec<record::RecFrame>,
    /// The script's own file, recorded by a pass that depends on its code.
    pub script_path: Option<std::path::PathBuf>,
}

/// The default step budget: `CIG_MAX_STEPS`, else ten million, which a
/// tree-walking interpreter spends in a few seconds. `0` disables it.
pub fn default_max_steps() -> u64 {
    std::env::var("CIG_MAX_STEPS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(10_000_000)
}

impl Interp {
    pub fn new(kernel: Kernel) -> Self {
        let prelude = Scope::root();
        for (name, value) in crate::stdlib::prelude() {
            prelude.force_declare(name, value, false);
        }
        let globals = Scope::child(&prelude);
        Self {
            prelude,
            globals,
            kernel,
            out: Box::new(std::io::stdout()),
            err: Box::new(std::io::stderr()),
            burn_depth: 0,
            unlit_depth: 0,
            call_depth: 0,
            cough_payload: None,
            script_name: None,
            exit_requested: None,
            steps: 0,
            max_steps: default_max_steps(),
            loop_depth: 0,
            chain_stack: Vec::new(),
            recs: Vec::new(),
            script_path: None,
        }
    }

    /// Run a journaled compensation: its `unburn { }` source with the state
    /// map it was recorded with, effects allowed, nothing journaled. Used
    /// by rollback in this process and by `cig unburn` in another.
    pub fn run_compensation(
        &mut self,
        comp: &crate::burn::journal::Compensation,
    ) -> Result<(), Diagnostic> {
        // A hop's own undo (the burn protocol) is a command, not source.
        if comp.command.is_some() {
            return crate::burn::protocol::run_command(comp)
                .map_err(|m| Diagnostic::new(Kind::Runtime, m));
        }
        let program = crate::syntax::parse(&comp.source)?;
        let scope = Scope::child(&self.globals);
        if let Some(name) = &comp.state_name {
            scope.declare(
                name,
                crate::stdlib::json::to_value(comp.state.clone()),
                false,
            );
        }
        self.burn_depth += 1;
        let result = self.exec_block_in(&program.body, &scope);
        self.burn_depth -= 1;
        self.out.flush().ok();
        match result {
            Ok(()) | Err(eval::Signal::Return(_)) => Ok(()),
            Err(eval::Signal::Error(d)) => Err(d),
            Err(eval::Signal::Break(span)) | Err(eval::Signal::Continue(span)) => Err(
                Diagnostic::new(Kind::Runtime, "`break` or `continue` outside of a loop").at(span),
            ),
        }
    }

    /// Set the step budget (0 disables it).
    pub fn set_max_steps(&mut self, n: u64) {
        self.max_steps = n;
    }

    /// Count one step; over the budget, the loop (or the script) is stopped
    /// with E515 at `span`.
    #[inline(always)]
    pub(crate) fn tick(&mut self, span: Span) -> Result<(), Diagnostic> {
        self.steps += 1;
        if self.max_steps != 0 && self.steps > self.max_steps {
            return Err(self.over_budget(span));
        }
        Ok(())
    }

    #[cold]
    #[inline(never)]
    fn over_budget(&self, span: Span) -> Diagnostic {
        let message = if self.loop_depth > 0 {
            format!(
                "your loop never ends: {} steps and still going",
                self.max_steps
            )
        } else {
            format!("the script needs more than {} steps", self.max_steps)
        };
        crate::diagnostics::runtime(message)
            .code("E515")
            .at(span)
            .with_hint(
                "check what changes the loop condition at this line; --max-steps N or CIG_MAX_STEPS=N raises the budget (0 disables it)",
            )
    }

    /// Expose the script's own arguments as `args`.
    pub fn set_args(&mut self, args: &[String]) {
        let list = Value::list(args.iter().map(Value::str).collect());
        // In the prelude, beside the builtins: a script may shadow it with
        // its own `args`, as the checker allows.
        self.prelude.force_declare("args", list, false);
    }

    pub fn in_burn(&self) -> bool {
        self.burn_depth > 0
    }

    pub fn in_unlit(&self) -> bool {
        self.unlit_depth > 0
    }

    /// The ghost filesystem, when reads should consult it (dry-run).
    pub fn ghost(&self) -> Option<&crate::burn::ghost::Ghost> {
        self.kernel.ghost()
    }

    /// After a simulated write in a dry-run, give the ghost the bytes.
    /// Unlit burns never enter the ghost: rehearsal changes nothing, not
    /// even a pretend disk.
    pub fn ghost_put(&mut self, path: &Path, bytes: &[u8], append: bool) {
        if self.unlit_depth == 0 {
            self.kernel.ghost_put(path, bytes, append);
        }
    }

    /// Called by effectful builtins before they act.
    pub fn effect(&mut self, name: &str, op: Op, span: Span) -> Result<Decision, Diagnostic> {
        // Test hook for the reporter: an internal error (E900) at the first
        // effect, the state a bug in cig's own code would leave.
        if std::env::var("CIG_INTERNAL_FAULT").as_deref() == Ok("internal") {
            return Err(crate::diagnostics::internal(format!(
                "deliberate internal error at {name} (CIG_INTERNAL_FAULT=internal)"
            ))
            .at(span));
        }
        if self.burn_depth == 0 {
            return Err(burn_error(format!(
                "{name} changes the world, so it must be inside a burn block"
            ))
            .code("E701")
            .at(span)
            .with_hint(format!("wrap it: burn {{ {name}(...) }}")));
        }
        self.kernel
            .decide(op, self.unlit_depth > 0)
            .map_err(|e| e.or_at(span))
    }

    /// For a builtin that changes the world through the kernel without an
    /// op of its own (`proc.apply_layer`): the burn-block rule, and whether
    /// it runs for real (not in a dry run, not under `unlit`).
    pub(crate) fn burning(&self, name: &str, span: Span) -> Result<bool, Diagnostic> {
        if self.burn_depth == 0 {
            return Err(burn_error(format!(
                "{name} changes the world, so it must be inside a burn block"
            ))
            .code("E701")
            .at(span)
            .with_hint(format!("wrap it: burn {{ {name}(...) }}")));
        }
        Ok(self.unlit_depth == 0 && self.kernel.mode != crate::burn::Mode::DryRun)
    }

    pub fn run_program(&mut self, program: &Program) -> Result<(), Diagnostic> {
        if std::env::var("CIG_INTERNAL_PANIC").as_deref() == Ok("eval") {
            // Test hook for the crash reporter: a panic from inside the
            // evaluator, deeper than `main`, so a report's backtrace has
            // something to name.
            panic!("deliberate test panic in the evaluator (CIG_INTERNAL_PANIC=eval)");
        }
        let globals = self.globals.clone();
        let result = self.exec_block_in(&program.body, &globals);
        self.out.flush().ok();
        match result {
            Ok(()) => Ok(()),
            Err(eval::Signal::Error(_)) if self.exit_requested.is_some() => Ok(()),
            Err(eval::Signal::Error(d)) => Err(d),
            Err(eval::Signal::Return(_)) => Ok(()),
            Err(eval::Signal::Break(span)) => {
                Err(Diagnostic::new(Kind::Runtime, "`break` outside of a loop").at(span))
            }
            Err(eval::Signal::Continue(span)) => {
                Err(Diagnostic::new(Kind::Runtime, "`continue` outside of a loop").at(span))
            }
        }
    }

    /// Run statements in the global scope and return the value of the last
    /// expression statement, for `cig eval` and the REPL.
    pub fn eval_program(&mut self, program: &Program) -> Result<Option<Value>, Diagnostic> {
        let globals = self.globals.clone();
        let result = self.eval_block_last(&program.body, &globals);
        self.out.flush().ok();
        match result {
            Ok(v) => Ok(v),
            Err(eval::Signal::Error(_)) if self.exit_requested.is_some() => Ok(None),
            Err(eval::Signal::Error(d)) => Err(d),
            Err(eval::Signal::Return(v)) => Ok(Some(v)),
            Err(eval::Signal::Break(span)) => {
                Err(Diagnostic::new(Kind::Runtime, "`break` outside of a loop").at(span))
            }
            Err(eval::Signal::Continue(span)) => {
                Err(Diagnostic::new(Kind::Runtime, "`continue` outside of a loop").at(span))
            }
        }
    }
}

#[cfg(all(test, debug_assertions))]
mod resolve_tests;
