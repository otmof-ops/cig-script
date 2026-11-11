#!/bin/sh
# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: FSL-1.1-ALv2
#
# The vault's account and process for the attack tests. Runs only in the
# isolated VM (it creates an account and uses sudo); never on a workstation.
#
#   vault-ctl.sh setup   make the _cigtin account and the store and socket folders
#   vault-ctl.sh start   run `cig-vault serve` as _cigtin, in the background
#   vault-ctl.sh stop    stop it (the socket file is left behind, as a crash would)
#   vault-ctl.sh reset   remove the account's store contents (between runs of the tests)
#
# CIG_VAULT_BIN names the cig-vault binary; CIG_VAULT_VM=1 must be set.
set -eu
[ "${CIG_VAULT_VM:-}" = 1 ] || { echo "vault-ctl.sh: runs only in the VM (CIG_VAULT_VM=1)" >&2; exit 2; }
bin="${CIG_VAULT_BIN:?CIG_VAULT_BIN names the cig-vault binary}"
case "${1:-}" in
  setup)
    getent passwd _cigtin >/dev/null || sudo useradd --system --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin _cigtin
    sudo install -d -m 0700 -o _cigtin -g _cigtin /var/lib/cig-vault
    sudo install -d -m 0755 -o _cigtin -g _cigtin /run/cig-vault
    # The binary must be runnable by the account: a copy outside the home.
    sudo install -m 0755 "$bin" /usr/local/bin/cig-vault
    ;;
  start)
    sudo -u _cigtin /usr/local/bin/cig-vault serve --detach --grace-days 7
    ;;
  stop)
    sudo pkill -x -u _cigtin cig-vault || true
    ;;
  reset)
    sudo pkill -x -u _cigtin cig-vault || true
    if [ -d /var/lib/cig-vault ]; then sudo find /var/lib/cig-vault -mindepth 1 -delete; fi
    ;;
  *)
    echo "usage: vault-ctl.sh setup|start|stop|reset" >&2
    exit 2
    ;;
esac
