# Research notes

One file per language implementation cig has studied, holding every
decision that study made: what that language does, whether cig takes,
adapts or rejects it, the experiment and its numbers, and the pinned
source that was read. These notes are why nobody studies the same thing
twice, and why a rejected idea is not proposed again. Read the file for a
language before proposing anything from it.

Each file has one entry per design point, in four parts:

- what that language does, and why;
- **take, adapt or reject** for cig, and why;
- the experiment, with the test machine's run id and its before and after
  numbers, or that it has not been tried yet;
- the pinned tag and the files read.

## The rules every study follows

1. **Read the design, reimplement it in cig's terms, never paste code.**
   That is cleaner legally, and it gives code that fits cig's rules:
   - no `unsafe` in the VM or the bytecode crate;
   - reference counting and deterministic cleanup;
   - verified bytecode;
   - every burn through the one kernel;
   - identical behavior on both engines.
2. **Every idea ends in a decision with numbers:** take, adapt or reject,
   with the before and after from the test machine (instructions and wall
   time, the differential suite at 0 differences). An idea that measures
   worse is dropped and recorded as dropped.
3. **Point a study at files, not repositories.** For example: "read
   `Python/specialize.c` and `Include/internal/pycore_code.h` and explain
   how `LOAD_ATTR` specializes and falls back", not "study CPython".
4. **Licenses:**

   | license | languages | rule |
   |---|---|---|
   | MIT, BSD, Apache-2.0 | Lua, LuaJIT, QuickJS, Wren, PyPy, Koka, Lean 4, Lobster, Go, Cranelift, V8, Node | ideas freely; if code is ever adapted rather than reimplemented, its notice goes in `THIRD-PARTY.md` beside it |
   | PSF | CPython | the same as above |
   | Ruby's license (BSD-2 option) | Ruby | the same as above |
   | **GPL** | OpenJDK, anything else under it | **ideas only, never code**: GPL code in cig would force cig out under the GPL, and cig is FSL |

## Pins

The sources studied are pinned in the reference library's manifest
(`MANIFEST.tsv`: name, upstream URL, tag, commit, license, date pinned),
and the comparator binaries are built from the same pins. Each file here
names the tag and commit it read, so a number in it can always be traced
to the source and the comparator it was measured against.

## Files

| file | language | studied for |
|---|---|---|
| [lua.md](lua.md) | Lua 5.4 (and LuaJIT 2.1 for contrast) | #235 |
