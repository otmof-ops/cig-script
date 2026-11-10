#!/bin/sh
# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: Apache-2.0
# A redirect, kept for one release. cig is installed by ciginstaller now.
# It prints how, and stops: it installs nothing and exits 1 so a script that
# still calls it fails loudly instead of carrying on without cig.
#
# Building from a checkout is the developer route: see CONTRIBUTING.md.

cat >&2 <<'MSG'
cig is installed by ciginstaller now: ciginstaller install stick cig
get ciginstaller: https://github.com/otmof-ops/cig-installer#install
MSG
exit 1
