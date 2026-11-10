# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: Apache-2.0
# A redirect, kept for one release. cig is installed by ciginstaller now.
# It prints how, and stops: it installs nothing and exits 1 so a script that
# still calls it fails loudly instead of carrying on without cig.
#
# Building from a checkout is the developer route: see CONTRIBUTING.md.

[Console]::Error.WriteLine('cig is installed by ciginstaller now: ciginstaller install stick cig')
[Console]::Error.WriteLine('get ciginstaller: https://github.com/otmof-ops/cig-installer#install')
# `exit` inside `irm ... | iex` would close the caller's shell; a script file
# exits 1, the piped form throws.
if ($PSCommandPath) { exit 1 } else { throw 'install.ps1: cig is installed by ciginstaller now' }
