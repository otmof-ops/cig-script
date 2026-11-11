// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The attack tests (#105): what a script, or another account, tries
//! against the tin, and what stops it. The vault tests need the vault
//! running as its own account, so they run only where the harness says
//! so (`CIG_VAULT_VM=1`, set by `tests/attack/run-in-vm.sh` inside the
//! isolated VM) and skip everywhere else.

#[cfg(unix)]
mod common;
#[cfg(unix)]
mod passphrase;
#[cfg(unix)]
mod rekey_wait;
#[cfg(unix)]
mod tin;
#[cfg(unix)]
mod vault;
#[cfg(unix)]
mod vault_admin;
