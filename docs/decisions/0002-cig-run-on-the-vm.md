<!-- SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script) -->
<!-- SPDX-License-Identifier: CC-BY-4.0 -->

# 0002: `cig run` on the VM

- Status: accepted
- Date: 2026-11-22
- Issue: otmof-ops/cig-script#254

## Context

cig has two engines: the tree-walker, and a bytecode VM that the
differential suite holds to the tree-walker's behavior. Until now only a
program built by `cigcompile build --stage b` reached the VM, because the
lowering (source to CIR to bytecode) lived in cig-compile. `cig run
script.cig`, the way nearly everyone runs cig, always took the tree-walker.
#254 asks for `cig run` to use the VM. Five things had to be decided.

## Decisions

### 1. The lowering is a module of the `cigscript` crate, `cigscript::lower`

The issue sketches a crate of its own in this workspace. That cannot be
built as the code stands: the lowering needs the standard library's
catalogue (which builtin has which effect), the checker, the VM's table of
burn operations and `Value`, all in `cigscript`; and `cig run`, in
`cigscript`, needs the lowering back. Two crates that need each other are
a cycle, which Cargo refuses.

So the lowering is `src/lower/` in this crate: `cir` (the IR, its passes
and its verifier) and `bc` (CIR to bytecode), moved from cig-compile
without change. `#![forbid(unsafe_code)]` sits at the top of the module, as
it does on the bytecode crate. cig-compile calls `cigscript::lower` and
carries no copy, so `cig run` and `cigcompile build` cannot drift.

A crate of its own becomes possible when the syntax tree, the checker and
the catalogue are crates below it. That is a larger move with its own
reasons (the plug-in API, #257, and a code generator shared with
cig-cherry, #256); the public path `cigscript::lower` is chosen so that it
can stay the same when that happens.

`lower::VERSION` is the lowering's version. It is bumped whenever the bytes
the lowering makes for a given script can change.

### 2. The engine is chosen per run, and the choice is recorded

`cig run --engine vm|tree` chooses for one run, `CIG_ENGINE` for a shell,
and `cig config engine` sets the default. The flag wins over the variable,
which wins over the config: one run can always choose, whatever the default
says. A value that names no engine is a usage error (E800) before anything
runs. `cig light` takes the same flag.

A compiled program is not affected: its engine is its stage's.

Every run's record carries `engine`: which engine ran (`vm` or `tree`);
for the VM, where its module came from (`cached` from the tin, or
`lowered` by this run); and, when a run asked for the VM and the
tree-walker ran the script, `asked` and `why`. `cig runs <id>` shows it.
The field is an object so that cig-cherry (#256) can add which functions
ran native.

### 3. A script the VM cannot take runs on the tree-walker

No script fails to run because of the VM. The tree-walker runs the script,
and the record says why, when:

- the lowering refuses a construct it does not take yet;
- the lowering or the verifier fails (its own bug, which the record names);
- the module does not load;
- the run has `--no-check`: a script that is not checked runs as written,
  which only the tree-walker does.

A script that does not parse or check fails as it always did, with the
same errors, before any engine is chosen.

### 4. Lowered modules are kept under the tin, and trusted by a MAC

A run on the VM looks for the script's module in `<state>/tin/modules/`
before it lowers anything.

**The key** is the SHA-256 over this cig's version, the lowering's version,
the optimization level and the script's own SHA-256 (the one the run
record carries). The same bytes on the same cig find the same entry; an
edit, an update of cig or a new lowering finds none, and the old entry is
never asked for again. When scripts can use other scripts (#247), each
one's hash joins the key. The module is named by what is in it, not by
where the script lies: the same script in two folders is one entry.

**Trust.** The tin is not where trust lives: the ledger sits in the keys
directory for that reason. An entry is used only if its MAC verifies under
this machine's module key, 32 random bytes in the keys directory
(`module.key`, owner-only, made the first time an entry is stored). The
MAC is a keyed BLAKE3 over the entry's header and the module's own header,
which carries the BLAKE3 of the module's body; the VM checks the body
against it when it opens the module. So bytes written into the tin by
something that cannot read the keys directory are never run. Whoever can
read the keys directory can already change the script or cig itself; the
cache gives them nothing new.

The seal key was not used for this, on purpose. A run that burns nothing
never loads the seal key today, and on most machines loading it is a call
to the system keyring. A MAC under a seal subkey would put that call in
front of every run on the VM.

**Nothing in the cache can fail a run.** An entry that is missing, cut
short, made by another cig, or that does not verify is a miss: the script
is lowered again and the entry rewritten. A cache that cannot be written
costs the lowering on every run and nothing else.

**A hit skips the parse and the check** when there is nothing to say
again. The entry remembers whether the check gave warnings (a pin's
included, when the pin is the script's own header line). If it did, or the
script is pinned by a version file (which is not in the script's bytes),
the script is parsed and checked as on every run, so its warnings are
printed as always, and the module is still not lowered again.

**Housekeeping.** `cig tin` counts the modules and their bytes, `cig
doctor` lists them, and `cig tin gc` removes the ones nothing loads:
another cig's, and any not loaded for 30 days. Every entry is safe to
remove.

### 5. The VM becomes the default when the measurements say so

The default stays `tree` until, on the benchmark paper's programs:

- the VM is faster than the tree-walker on every compute program, with
  the module in the tin;
- a first run of `hello` and of a 500-line script, with an empty tin, is
  within 10% of the tree-walker's;
- the whole suite and the engine suite (`tests/engines.rs`) pass with the
  VM as the default.

`--engine tree` stays, and the tree-walker stays the oracle of the
differential suites.

**As first measured (2026-11-22), the default stays `tree`.** On a
development machine with this change's build
([the run](../ENGINES-2026-11-22.md),
[the first runs](../ENGINES-FIRST-RUNS-2026-11-22.md)): with its module in
the tin, over the compute
programs it is faster on most, level on some and slower on none, so the
first condition is short of "every"; and a first run of the 500-line
script takes well over the tree-walker's time, so the second does not
hold. The two documents have the numbers. The cost is the lowering itself. The
default changes when a first run stops paying for it up front: a cheaper
lowering, or a first run on the tree-walker with the module made for the
next one.

## Options considered

| Option | Why not |
|---|---|
| The lowering as its own crate now | A dependency cycle with `cigscript` (decision 1). |
| The `cig` binary as a crate above both | Moves the command line out of `cigscript`, which cig-compile's runtime and every test of the binary build on; a larger change than the issue, with nothing gained for `cig run`. |
| No cache: lower on every run | Lowering costs more than it saves on a short script, and far more on a long one (cigo's 20,000 lines). |
| Entries trusted because the tin is private | The tin is where files a hop may change are kept, and it is copied, backed up and restored; the ledger already treats it as something to check against, not to believe. |
| A MAC under a seal subkey | A keyring call before every run (decision 4). |
| Compare the entry with a fresh lowering | That is the lowering the cache exists to skip. `cig doctor` can do it out of band later. |

## Consequences

- cig-compile depends on cig for its lowering; a change to the lowering is
  one pull request here, proven by cig-compile's differential suite before
  cig-compile takes the release.
- The cache key includes the cig version, so every update lowers each
  script once more.
- A machine restored from a backup of the state directory without the keys
  directory lowers each script again; nothing else changes.
