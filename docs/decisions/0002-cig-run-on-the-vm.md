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
and its verifier) and `bc` (CIR to bytecode), moved from cig-compile.
`#![forbid(unsafe_code)]` sits at the top of the module, as it does on the
bytecode crate. cig-compile is to call `cigscript::lower` and carry no
copy (a change of its own, after this one), so that `cig run` and
`cigcompile build` cannot drift. Two defects were found in the lowering on
the way and are fixed here; the changelog has both.

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
says. A `CIG_ENGINE` that names no engine is a usage error (E800) before
anything runs; `--engine` and `cig config engine` take only `vm` or `tree`,
and a config file edited by hand to hold anything else counts as `tree`.
`cig light` takes the same flag.

A compiled program is not affected: its engine is its stage's.

A script pinned to another cig still runs on that cig, with that cig's
engine. The flag is taken out of what is handed on, because a cig from
before it refuses it as unknown, and the choice goes with the run as
`CIG_ENGINE`, which such a cig ignores and a later one reads in the flag's
place.

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
- the lowering crashes (a bug it did not find): the crash is said and
  reported as any crash is, and the run goes on;
- the module does not load;
- the run has `--no-check`: a script that is not checked runs as written,
  which only the tree-walker does.

A script that does not parse or check fails as it always did, with the
same errors, before any engine is chosen.

### 4. Lowered modules are kept under the tin, and trusted by a MAC

A run on the VM looks for the script's module in `<state>/tin/modules/`
before it lowers anything.

**The key** is the SHA-256 over this cig's build (its version, and its
executable's size and time), the lowering's version, the optimization
level and the script's own SHA-256 (the one the run record carries). The
same bytes on the same cig find the same entry; an edit, an update of cig,
a build of the same version from other sources, or a new lowering finds
none, and the old entry is never asked for again. When scripts can use
other scripts (#247), each one's hash joins the key. The module is named
by what is in it, not by where the script lies: the same script in two
folders is one entry.

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

**What an entry holds** is the script in another form: its names and its
literals can be read in it. It is kept as the tin keeps any file a burn
changed that is not marked a secret: owner-only, not encrypted.

**Nothing in the cache can fail a run, or hold one up.** An entry that is
missing, cut short, larger than a module may be, made by another build, or
that does not verify is a miss: the script is lowered again and the entry
rewritten. Only a plain file is opened, and nothing past its two headers
is read before the MAC over them verifies, so a pipe, a link or a file of
any size put in an entry's place is a miss at once. A cache that cannot be
written costs the lowering on every run and nothing else; nothing is
written through a link put where one of its folders goes.

**A hit skips the parse and the check** when there is nothing to say
again. The entry remembers whether the check gave warnings (a pin's
included, when the pin is the script's own header line). If it did, or the
script is pinned by a version file (which is not in the script's bytes),
the script is parsed and checked as on every run, so its warnings are
printed as always, and the module is still not lowered again.

**Housekeeping.** `cig tin` and `cig doctor` count the modules and their
bytes, and `cig tin gc` removes another build's and any not loaded for 30 days
(a load moves an entry's time forward at most once a day, so to within a
day). Every entry is safe
to remove. `cig doctor` also says when the cache is off because something
it will not use stands in the place of its key or its folder; nothing else
would, since an unusable cache fails no run.

### 5. The VM becomes the default when the measurements say so

#254 sets the rule. The VM becomes the default when, on the benchmark
paper's programs and the test machine, with cig as released:

- it is faster than the tree-walker on every compute program, with the
  module in the tin;
- a first run of `hello` and of a 500-line script, with an empty tin, is
  within 10% of the tree-walker's;
- the whole suite and the differential suite pass with it as the default.

`--engine tree` stays, and the tree-walker stays the oracle of the
differential suites.

The third is held from now on, on every pull request: CI runs the suites
that run scripts a second time with `CIG_ENGINE=vm`, and the differential
suite against the 1.20.0 release the same way; `tests/engines.rs` compares
the two engines on what a run leaves, journals included.

**As first measured (2026-11-22), the default stays `tree`.** That was on
a development machine with this change's own build, not the test machine
or a release ([the run](../ENGINES-2026-11-22.md),
[the first runs](../ENGINES-FIRST-RUNS-2026-11-22.md)). With its module in
the tin the VM is faster on most of the compute programs, level on the
rest and slower on none, which is short of "every"; and a first run of
the 500-line script takes several times the tree-walker's, where a first
run of `hello` sits at the edge of the 10%. The two documents have the
numbers. What a first run pays
for is what it does before the script starts: lowering it, verifying and
loading the module, and keeping it. The default changes when a first run
stops paying that up front: a cheaper lowering, or a first run on the
tree-walker with the module made for the next one.

**Measured again on cig 1.25.0 as released, the same holds.** The same
runs with the release's own binary, on the same development machine
([the run](../ENGINES-1.25.0.md),
[the first runs](../ENGINES-FIRST-RUNS-1.25.0.md)): the compute programs
come out as they did, and a first run of the 500-line script still takes
several times the tree-walker's. What differs is cigo's script, the
longest of the three: the released build's tree-walker starts it in about
two thirds of the development build's time, so the VM's gain there is
smaller and a first run costs more against it. The test machine has not
measured either yet.

## Options considered

| Option | Why not |
|---|---|
| The lowering as its own crate now | A dependency cycle with `cigscript` (decision 1). |
| The `cig` binary as a crate above both | Moves the command line out of `cigscript`, which cig-compile's runtime and every test of the binary build on; a larger change than the issue, with nothing gained for `cig run`. |
| No cache: lower on every run | Lowering costs more than it saves on a short script, and far more on a long one (cigo's 20,664 lines). |
| Entries trusted because the tin is private | The tin is where files a hop may change are kept, and it is copied, backed up and restored; the ledger already treats it as something to check against, not to believe. |
| A MAC under a seal subkey | A keyring call before every run (decision 4). |
| Entries encrypted under the module key | The tin keeps a file in the clear unless it is marked a secret, and a script is not. Opening an encrypted entry costs time for every byte of it on every start, which is the cost the cache is there to remove; and a later cache of native code (#256) has to be mapped as it lies. |
| Compare the entry with a fresh lowering | That is the lowering the cache exists to skip. `cig doctor` can do it out of band later. |

## Consequences

- cig-compile depends on cig for its lowering; a change to the lowering is
  one pull request here, proven by cig-compile's differential suite before
  cig-compile takes the release.
- The cache key includes the build of cig, so every update lowers each
  script once more.
- A machine restored from a backup of the state directory without the keys
  directory lowers each script again; nothing else changes.
- A copy of the tin carries the lowered form of every script run on the
  VM, as it carries every file a burn changed.

## Addendum (#259): a first run that does not pay for the lowering

**Decision.** `--engine` takes a third setting, `auto`, and `auto` is the
default. The table:

| setting | a module is kept for the script | no module is kept |
|---|---|---|
| `tree` | the tree-walker | the tree-walker |
| `vm` (unchanged) | the VM | lowered now, kept, run on the VM |
| `auto` (new, the default) | the VM | the tree-walker runs it now, and a `__lower` subprocess left behind lowers the script and keeps the module for the next run |

A first run on `auto` pays the tree-walker's start-up and nothing more.
M6 (warm starts, cig-script#279) measured cigo's script at 28 ms on the
tree-walker every time and 308 ms on a first run on the VM, of which
277 ms was the lowering; from a kept module the VM starts it in 9.2 ms.
M1 (run times across 116 scripts) gave the shape of a cig script's work:
the median script finishes in 1.0 ms on the VM, and 90% of them in under
10 ms. The next run finds the module and starts on the VM. The two
engines' behaviour is held to the tree-walker by the differential
suites, which is unchanged.

**The alternatives #259 lists, and why each was not taken.**

- *A cheaper lowering.* M6 says the lowering of the released build's
  500-line script is 4.0 ms. A linear cut of 50% in the lowering still
  leaves a 500-line first run at 2.5 ms (the tree-walker's start-up, as
  measured) + 2.0 ms of lowering = 4.5 ms, 1.80 times the tree-walker's
  and so far past the 10% bar. Faster lowering is still worth having;
  it cannot by itself pay for the first run.
- *Lower in the same process after the script runs.* The 500-line
  script's first run then takes 2.5 + 4.0 = 6.5 ms where the
  tree-walker takes 2.5: a user-visible slowdown on the first run,
  which fails #259's own bar. A child process is what git does for
  `gc --auto` for the same reason.
- *Lower lazily per function.* The cost moves into the first call of
  each function; a script that calls many functions once still pays
  most of the lowering, and cold-start cases remain the ones this
  change is for. The alternative is left open for cig-cherry (#256).

**The left-behind process.** One spawn after the script's own work has
ended, all three streams on the null device, in a process group of its
own (`std::process::Command::process_group(0)` on Unix; the creation
flags `std::os::windows::process::CommandExt` gives on Windows), never
waited for. `forbid(unsafe_code)` stays: no `pre_exec`. A spawn that
fails changes nothing about the run.

**The marker.** A file beside the modules, taken with `create_new`,
removed when the lowering keeps a module or exits. Twenty first runs
that start together keep one module and leave no leftover marker; a
`__lower` that crashed or was killed leaves a marker that is treated
as stale after ten minutes and taken over on the next run. `cig tin
gc` removes markers older than an hour, as it does the leftovers of an
interrupted write.

**Refusals are recorded.** A script the lowering does not take runs on
the tree-walker, as it does under `--engine vm`. Under `auto` the
reason is stored as an entry under the same key, so a later run reads
it and never spawns another lowering until the script or the build
changes. A `cig tin gc` or a `cig tin clear` removes it; the next run
on `auto` tries the lowering again.

**The off switch.** `auto_lower` (and `CIG_AUTO_LOWER`) takes `on` (the
default: spawn), `off` (never spawn; `auto` stays the tree-walker
until something else keeps a module, such as a single `cig run
--engine vm`) and `wait` (run `cig __lower` as a child, waited for
and reaped; for a deterministic test). No test sleeps on a background
process.

**The record.** `engine` already carried which engine ran and, when a
run asked for the VM but got the tree-walker, why. Under `auto` a run
on the tree-walker carries the refusal reason as `why` without
`asked`, so a reader of a run on `auto` sees why the module was not
used. The field is still an object.

**The lowering shares `cig run --engine vm`'s path.** `__lower` is a
hidden subcommand (`cig __lower <script>`) that reads the script,
parses and checks it, and calls the same `cigscript::lower::module_loaded`
that `--engine vm` calls, storing the result through the same
`modcache`. No second mechanism, no new key: the module keyed by
`auto`'s subprocess is the module `--engine vm` would have made.
