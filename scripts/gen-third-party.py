#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: FSL-1.1-ALv2
"""Regenerate THIRD-PARTY-NOTICES.md from `cargo license --avoid-dev-deps --json`."""
import json, subprocess, collections, pathlib
root = pathlib.Path(__file__).resolve().parent.parent
rows = json.loads(subprocess.check_output(["cargo", "license", "--avoid-dev-deps", "--json"], cwd=root))
rows = [r for r in rows if r["name"] != "cigscript"]
fam = collections.OrderedDict()
for r in rows:
    fam.setdefault(r.get("license") or "unrecorded", []).append(r)
out = ["# Third-party notices", "",
       "CigScript's binary links the Rust crates below, pulled from crates.io as packages (nothing is vendored). "
       "Every one is under a permissive licence compatible with Apache-2.0. Holder names come from each crate's metadata; "
       "`unrecorded` means the crate publishes no author field. Regenerate with `cargo license --avoid-dev-deps --json` "
       "(cargo-license) and `scripts/gen-third-party.py`.", ""]
for lic, items in fam.items():
    out += [f"## {lic}", "", "| crate | version | holder | source |", "|---|---|---|---|"]
    for r in sorted(items, key=lambda r: r["name"]):
        holder = (r.get("authors") or "unrecorded").replace("|", ", ")
        out.append(f"| {r['name']} | {r['version']} | {holder} | {r.get('repository') or 'crates.io'} |")
    out.append("")
(root / "THIRD-PARTY-NOTICES.md").write_text("\n".join(out))
print("wrote THIRD-PARTY-NOTICES.md")
