#!/bin/sh
# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: FSL-1.1-ALv2
#
# The vault's attack tests, in the isolated VM only: they make an account
# (`_cigtin`) and run a service as it. From the repository root:
#
#   rsync -a --delete ./ cigvm:work/cig-script/ && ssh cigvm 'cd work/cig-script && sh tests/attack/run-in-vm.sh'
set -eu
export PATH="$HOME/.cargo/bin:$PATH"
export CIG_VAULT_VM=1
cargo build -p cig-vault --bin cig-vault
target="${CARGO_TARGET_DIR:-target}"
export CIG_VAULT_BIN="$PWD/$target/debug/cig-vault"
export CIG_VAULT_CTL="$PWD/tests/attack/vault-ctl.sh"
"$CIG_VAULT_CTL" reset
"$CIG_VAULT_CTL" setup
"$CIG_VAULT_CTL" start
timeout 900 cargo test --test attack -- --test-threads=1 --nocapture
"$CIG_VAULT_CTL" stop
