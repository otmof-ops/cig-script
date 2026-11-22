// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
//! CIR: the intermediate representation (cig-compile#3), between the
//! checked AST and code generation, where the burn rule is structural: an
//! effect is a `burn.op` node, and the verifier proves every one sits in a
//! burn region (or in a function that may only be called from one) after
//! every pass. Stage B's bytecode is lowered from it.

pub mod effects;
pub mod ir;
pub mod lower;
pub mod pass;
pub mod print;
pub mod verify;

pub use ir::Module;
pub use verify::{verify, VerifyError};

/// Parse, check and lower a script. A script that does not parse or
/// check comes back as its first diagnostic, as `cig check` reports it.
pub fn lower_source(source: &str, file: &str) -> Result<Module, LowerError> {
    let program = crate::syntax::parse(source).map_err(LowerError::Diagnostic)?;
    let rep = crate::check::check(&program, &[]);
    if let Some(d) = rep.errors.into_iter().next() {
        return Err(LowerError::Diagnostic(d));
    }
    lower_program(&program, file)
}

/// Lower a script that has already been parsed and checked.
pub fn lower_program(
    program: &crate::syntax::ast::Program,
    file: &str,
) -> Result<Module, LowerError> {
    let resolver = effects::Resolver::new();
    lower::lower(program, file, &resolver).map_err(LowerError::Internal)
}

#[derive(Debug)]
pub enum LowerError {
    /// The script does not parse or check.
    Diagnostic(crate::diagnostics::Diagnostic),
    /// The checker let through something lowering cannot place: a bug.
    Internal(String),
}

/// Lower, run the default passes, print: `--emit=cir`. The verifier's
/// warnings (V6) come back beside the text.
pub fn emit(source: &str, file: &str) -> Result<(String, Vec<VerifyError>), EmitError> {
    let mut m = lower_source(source, file).map_err(EmitError::Lower)?;
    let warnings = pass::run_passes(&mut m, &pass::default_passes()).map_err(EmitError::Verify)?;
    Ok((print::print(&m), warnings))
}

#[derive(Debug)]
pub enum EmitError {
    Lower(LowerError),
    Verify(pass::PassError),
}
