#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
# SPDX-License-Identifier: Apache-2.0
"""Regenerate docs/STDLIB.md from `cig --json language` so the reference can never drift."""
import json, subprocess, sys, pathlib
root = pathlib.Path(__file__).resolve().parent.parent
cig = sys.argv[1] if len(sys.argv) > 1 else str(root / "target/release/cig")
data = json.loads(subprocess.check_output([cig, "--json", "language"]))
out = ["# Standard library reference", "",
       f"Generated from `cig --json language` (CigScript {data['version']}) by `scripts/gen-stdlib-doc.py`. Do not edit by hand.", "",
       "Functions marked **burn** change the world and can only be called inside a `burn { }` block. "
       "`read` functions look at the world without changing it; `pure` functions touch nothing.", "",
       "Methods are called on a value (`\"x\".upper()`, `list.map(f)`, `map.has(\"k\")`); the receiver is not counted in the arity column.", ""]
INTROS = {
    "pass": "**Passes.** `pass.issue(kind, subject, pack(s) { ... })` runs its work with a recorder: every file, folder, variable and program it reads is noted, and the pass it returns holds while all of them stay as they were. `pass.memo` finds a pass that still holds, else does the work. A hop inside the work needs `reads: [paths]`; anything a recording cannot see makes the work open-world (E731). See docs/PASS.md.",
    "proc": "**Hops.** Every `proc.*` call crosses a process boundary (a *hop*, in the lexicon): the kernel cannot see inside, "
            "so a hop is journaled as **irreversible** and the plan says so out loud. `proc.run` is the one call shape; "
            "`proc.text`, `proc.lines`, `proc.json`, `proc.csv` and `proc.kv` parse stdout into a real value under the "
            "contract `ok: [0]`; `proc.pipe` connects stages without a shell; `proc.shell` is the shell, by name. "
            "Never a shell otherwise, both streams read concurrently and never merged, a timeout on every hop, the whole "
            "process group killed on timeout, explicit encoding, and a distinct code (E550-E557) for every way a hop can "
            "fail. The rules, each with a test, are in [HOPS.md](HOPS.md).",
    "fs": "Reads are free; anything that changes a file is a **burn**, snapshotted before and put back by `cig unburn`. "
          "Symlinks are seen as links (`fs.exists` is true for a dangling one) and restored as links.",
    "cig": "The cig a script runs under: its version as `cig --version` prints it, the platform it was built for "
           "(`linux`, `macos`, `windows`; `x86_64`, `aarch64`), and the id of the run in progress, the one `cig unburn` "
           "takes (null in a dry run and under `cig check`). A tool written in CigScript checks the cig it was handed "
           "against what it was written for with these, and names its own run; the pin (`# cig 1.2` under the shebang) "
           "is how a script asks for the right one in the first place.",
    "env": "Reading the environment is free; setting or unsetting it is a burn, journaled as irreversible because the "
           "kernel cannot restore another process's view of it.",
}
for group in data["library"]:
    out.append(f"## {group['group']}"); out.append("")
    if group["group"] in INTROS:
        out.append(INTROS[group["group"]]); out.append("")
    out.append("| function | args | effect |"); out.append("|---|---|---|")
    for f in group["functions"]:
        lo, hi = f["min_args"], f["max_args"]
        is_method = group["group"].endswith("methods")
        if is_method:
            lo -= 1
            hi = None if hi is None else hi - 1
        arity = f"{lo}" if hi == lo else (f"{lo}+" if hi is None else f"{lo}-{hi}")
        eff = "**burn**" if f["needs_burn"] else f["effect"]
        name = f["name"].split(".", 1)[1] if is_method else f["name"]
        out.append(f"| `{name}` | {arity} | {eff} |")
    out.append("")
(root / "docs/STDLIB.md").write_text("\n".join(out))
print("wrote docs/STDLIB.md")
