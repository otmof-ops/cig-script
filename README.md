<p align="center">
  <img src="docs/assets/logo.svg" alt="CigScript" width="440">
</p>

<p align="center">
  <b>Automation scripts that can't hurt you by accident.</b><br>
  <i>Nothing real happens unless you burn.</i>
</p>

<p align="center">
  <a href="LICENSE"><img alt="license FSL-1.1-ALv2" src="https://img.shields.io/badge/license-FSL--1.1--ALv2-6b7280"></a>
  <a href="https://github.com/otmof-ops/cig-script/releases/latest"><img alt="release" src="https://img.shields.io/github/v/release/otmof-ops/cig-script?color=f97316&label=release"></a>
  <img alt="built with Rust" src="https://img.shields.io/badge/built%20with-Rust-6b7280">
  <img alt="burns journaled" src="https://img.shields.io/badge/burns-journaled%20%26%20reversible-f97316">
  <img alt="nicotine 0 mg" src="https://img.shields.io/badge/nicotine-0%20mg-2ea44f">
</p>

> Computers spend their whole lives scrounging ciggies off each other. One
> walks over, covers its eyes, and goes "can't see any ciggies bro, where are
> they at." The other one leans on the wall and says "nar mate, never seen
> any ciggies over HERE before," and neither of them tells you where to
> point. Then something gets deleted, and it's your fault, apparently, and
> you're on trial for it.
>
> **CigScript is the one that points.**
>
> Every effect is a **burn** (a side effect, fenced). Every burn is written
> down *before* it happens (a write-ahead journal), so you can read the
> **plan** first and **unburn** it after (rollback). When it says no, it
> tells you which of the **four kinds of no** it is and where to look, and
> when it doesn't know, it says "I think this, because I checked that."
> Chains light from each other and put everything back when one goes out.
>
> It's a language for people who are sick of piping dry-run in by hand.
> Nobody's on trial. Nobody gets to be a smug bastard. Nicotine: 0 mg.

CigScript is a small scripting language for the jobs you'd otherwise hand to
a Python file or a bash script: sort a folder, rewrite some JSON, run a
pipeline of commands, ship a build. It has one rule the others don't: **nothing
changes the world outside a `burn { }` block.** Everything else follows from
that rule: a dry-run you can trust, rollback you never wrote, and a run record
that reads like a lab notebook.

```cig
#!/usr/bin/env cig
# Sort loose downloads into folders by extension.
stick root = args[0]
roll plan = {}
for file in fs.list(root) {
  if fs.is_file(file) {
    plan[file] = path.join(root, path.ext(file).lower(), path.base(file))
  }
}
burn {
  for file in plan {
    fs.mkdir(path.dir(plan[file]))
    fs.mv(file, plan[file])
  }
}
exhale "moved ${plan.len()} files"
```

```
$ cig run --dry-run tidy.cig -- ~/Downloads
dry-run plan: 14 ops
     1  reversible    mkdir /home/you/Downloads/pdf
     2  reversible    move /home/you/Downloads/paper.pdf -> /home/you/Downloads/pdf/paper.pdf
     ...
nothing was changed

$ cig run tidy.cig -- ~/Downloads
moved 13 files
burned 14 ops (0 irreversible), run 20260926T031500-3fa9c1

$ cig unburn 20260926T031500-3fa9c1      # changed your mind? put it all back
```

## Why you'd use it

**Dry-run that you can trust.** `--dry-run` runs your script for real and only
*plans* the burns, so the preview is exactly what the real run will do, not a
guess printed by a `--verbose` flag you had to add yourself. The pretend
writes live in a *ghost filesystem* (an in-memory overlay), so a step that
reads what an earlier step wrote works in the plan too. In the hallway:
*the plan*.

**Rollback without writing any.** Every burn is journaled with a snapshot (a
copy taken *before*) of what it's about to change, and the journal is written
*before* the burn, so a kill mid-burn still leaves a record. A script that
fails halfway is rolled back automatically, newest op first. A run you regret
is reversed with `cig unburn <id>`, minutes or days later. Processes you ran
can't be undone, and the plan marks them *irreversible* so you see them
coming. That label is the honesty guarantee the whole model is trusted
through, and it is never laundered.

<p align="center"><img src="docs/assets/burn-model.svg" alt="The burn model: check, run, burn, kernel, journal, rollback" width="820"></p>

**Chains: automation you can light.** Declare an ordered set of sticks and
run them by name from the command line, like a task runner, except the steps
are ordinary functions and the whole chain inherits dry-run and rollback: one
stick goes out, everything the earlier sticks burned is put back. Each stick
lit from the last. Yes, it's called chain smoking. No, it isn't bad for you.

```cig
chain release { fmt, lint, test, build, stamp }
```
```
$ cig chains tasks.cig          # what can I light?
$ cig light tasks.cig release   # light it: timed, logged, stops at the first failure
```

<p align="center"><img src="docs/assets/chain.svg" alt="A chain: fetch, build, test, ship, lit one from the last" width="820"></p>

**Errors that point.** Every diagnostic carries a stable code, points at the
source with a caret, and ends with *the hint*: what to type next, not what
went wrong. `cig explain E504` prints the long version. `cig check` catches
typos, reassigned constants and side effects outside a burn before anything
runs. And every failure leads with the same line, because a runtime should
have a catchphrase, and because it is the tool admitting its own eyes are
covered, not you:

```
Don't see any cigarettes.
error[E701 burn]: fs.rm changes the world, so it must be inside a burn block
  --> tidy.cig:14:5
   |
14 |     fs.rm(stale)
   |     ^^^^^^^^^^^^
  = hint: wrap it: burn { fs.rm(...) }
  = explain: cig explain E701
```

**Doctor, the one that points.** Under every error, doctor says what it
thinks happened and what it checked to think so, in the only sentence it is
allowed: *I think X, because I checked Y and found Z.* Then the fix, then what
to check if that is not it. Probes are read-only; doctor never fixes anything
on its own. `--no-doctor` gives you the bare diagnostic; `--plain` gives you
the same facts without the jokes. The whole model is in
[docs/DOCTOR.md](docs/DOCTOR.md).

```
  = doctor: I think the program is not on PATH for this run (can't see any ciggies bro), because I checked each directory on PATH, looking for the program named in the message and found no `kubectl` in the 7 directories on PATH.
  = fix: proc.which("name") to check; give the full path, or fix PATH in the environment the run inherits
```

**Checked once, trusted exactly as long as it is true.** A *pass* is a note
that something was checked, with every file, folder, variable and tool the
check looked at recorded while it ran, never listed by hand. Ask again and
cig checks one signature and looks at each input: if nothing moved, the work
does not run twice; if anything did, the pass stops holding. Whoever holds a
pass can make it smaller (`within ~/work/site`, `for 2h`, `once`) and never
bigger, so it is also something to hand an agent, cut to what it may do.
[docs/PASS.md](docs/PASS.md) is the guide.

**Batteries that match the job.** `fs`, `path`, `json`, `csv`, `text`
(regex), `proc`, `env`, `time`, `hash`, `math`, `log`, plus 64 methods on
strings, lists and maps.

**Hops that hold the line.** Calling another program is a *hop* (a process
boundary), and the boundary is where the logic is hardest. One call shape,
`proc.run(cmd, args, opts)`, with `proc.json`, `proc.lines`, `proc.csv`,
`proc.kv` and `proc.text` parsing stdout into real values and failing at the
hop that produced a bad shape. Exit codes are a contract (`{ok: [0, 1]}` for
`grep`), never a boolean. No shell unless you say `proc.shell`; `proc.pipe`
for pipelines without one. Both streams read concurrently and never merged,
a timeout on every hop, the whole process group killed when it fires,
explicit encoding, and a distinct code for every way a hop can fail. Every
rule has a test; they are in [docs/HOPS.md](docs/HOPS.md).

```cig
stick prs = proc.json("gh", ["pr", "list", "--json", "number,title"])
exhale prs.filter(pack(p) => p.title.contains("fix")).len()
```

**A runtime that looks after itself.** `ciginstaller doctor --tools` (what
`cig doctor` runs) reports on the install, cig's own state included, and names
the fix for each finding. `ciginstaller update` (what `cig update` runs)
fetches and verifies the newest carton. It reports its
own bugs, never yours: a crash, an internal error, or a tripwire that found one
of its promises broken after the work, each written to the ashtray with an
anonymised repro built from the journal and proved in a sandbox, filed as one
issue per bug when you have said yes once, and the fix announced back by
`ciginstaller update`. `ciginstaller bug cig` (or `cig report <run>`, the short
way in) reports a run that went wrong *without* crashing the same way.

**The pack and the compensation, two halves of one feature.** `pack {
"./build" }` at the top of a script declares the scope every write must stay
in; a literal path outside it is refused before the run, a computed one at
run time, and a child process is watched: the files it creates inside the
pack are removed on rollback, and anything it writes outside is reported,
never hidden. For what no pack can reverse, `burn (s) { git_push() } unburn
{ force_push_back(s.before) }` journals the undo with its state; the plan
says `compensated` where it used to say `irreversible`, and `cig unburn` runs
it weeks later from the journal alone.

**Determinism you can build on.** Ordered maps, sorted directory listings,
checked integer arithmetic, no random numbers. The same inputs give the same
plan, which is what makes the dry-run worth reading.

## Meet the lexicon

Ten themed words carry the ideas CigScript adds. Everything structural (`if`,
`while`, `for`, `and`, `or`, `not`) is the word you'd expect. Every word here
has an entry in [docs/LEXICON.md](docs/LEXICON.md) with the manual's name
attached, so you can walk into any forum and recognise what people are
talking about.

| word | means | in the hallway | because |
|---|---|---|---|
| `roll x = 1` | a variable (mutable binding) | "the one you'll change" | you roll one when you plan to do something with it later |
| `stick x = 1` | a constant | "the one you won't" | a finished stick is not re-rolled |
| `pull f(a) { }` | a function definition | "pull on it, get something back" | you pull on it, you get something back |
| `pack(x) => x` | a lambda (anonymous function) | "small, portable, hands you one" | small, portable, hands you one when asked |
| `snuff v` | return | "put it out, take what you got" | put it out, walk off with what you got |
| `ashtray` | where cig keeps the reports of its own bugs before and after they are sent | "it goes in the ashtray first" | nothing leaves until it has landed somewhere you can look at |
| `tripwire` | a check of cig's own promise, run after the work, that turns a silent bug into a report | "the tripwire went" | you only know it was there when it trips |
| `repro` | the smallest script that still shows the bug, built from the journal with none of your data in it | "here's the repro" | the maintainer gets the case, you keep your files |
| `exhale v` | print to stdout | "what leaves you and enters the room" | everyone can see it |
| `cough e` | raise an error | "from somewhere deep; interrupts everything" | the body's exception handler |
| `try { } ashtray e { }` | catch | "where the mess lands" | so it doesn't land on the carpet |
| `burn { }` | side effects allowed (the effect fence) | "the only place anything real happens" | the only moment anything actually happens |
| `burn unlit { }` | an intent, never executed (rehearsal) | "in the mouth, never lit" | you look like you mean it, and nothing happens |
| `chain name { a, b }` | automation (an ordered pipeline) | "lighting the next one from the last" | each stick lit from the last; one goes out, the rest are put back |

And at the command line: `cig run --dry-run` is *the plan*, `cig unburn` is
*put it back*, `cig runs` is *the lab notebook*, `cig doctor` is *the one that
points*, `cig report` is *the way into the bug report*. The full field guide, with
the frequently muttered questions, is [docs/SMOKE.md](docs/SMOKE.md). Health
note: CigScript contains 0 mg nicotine and the only thing it's bad for is
running scripts you haven't read.

## In the lab

Reads are free, writes need a burn, and every run records the script's hash,
its arguments and every file it touched with the hash of what was there
before. That is a lab notebook that writes itself, and it means a pipeline
that reads `raw/` cannot corrupt `raw/` by accident. Three runnable examples
(summary statistics with a citable report, a dataset manifest with drift
checking, and an analysis pipeline as a chain) are walked through in
[docs/SCIENCE.md](docs/SCIENCE.md).

```
$ cig light examples/lab-pipeline.cig analysis
chain analysis: step 1/5 seed ok (1 ms)
chain analysis: step 2/5 ingest ok (0 ms)
info: clean: kept 6, dropped 1 out-of-range row(s)
chain analysis: step 3/5 clean ok (1 ms)
chain analysis: step 4/5 analyse ok (0 ms)
| series      | n | mean    | sd    |
| pressure    | 3 | 101.2   | 0.3   |
| temperature | 3 | 21.8    | 0.361 |
chain analysis: step 5/5 report ok (1 ms)
```

## Install

Every release ships static binaries for Linux (x86_64, aarch64, armv7),
macOS (Apple silicon and Intel), Windows (x86_64, ARM64) and FreeBSD x86_64,
each with its SHA-256 and a build attestation, and the licence files beside
them; anything else builds from source in about a minute.

`ciginstaller` installs it, one verified transaction that puts `cig` on your
`PATH` through a shim and keeps what you installed together:

```sh
ciginstaller install stick cig      # cig alone
ciginstaller install pack dev       # cig and cigo
ciginstaller install carton         # everything
```

A machine with no installer gets it from the
[cig-installer releases](https://github.com/otmof-ops/cig-installer/releases):
download `install.sh` (Linux and macOS) or `install.ps1` (Windows) with its
`.sha256`, check it, read it, then run it, and it hands over to
`ciginstaller install`. The `install.sh` and `install.ps1` in this repository
are redirects that say the same and install nothing; building from a
checkout is the developer route, in
[CONTRIBUTING](CONTRIBUTING.md#building-from-a-checkout).

Updating, uninstalling and looking the tools over are the installer's too:

```sh
ciginstaller update          # `cig update` runs this
ciginstaller uninstall cig   # `cig uninstall` runs this
ciginstaller doctor --tools  # `cig doctor` runs this
```

Every release asset carries a build attestation from this repository's release
workflow: `gh attestation verify cig-v1.21.0-linux-x86_64 -R otmof-ops/cig-script`
checks it, and the installer checks it itself when `gh` is installed; the
checksum is the floor without it. The Linux assets
are static musl builds (any glibc, Alpine, Android under Termux); there are
assets for Linux x86_64, aarch64 and armv7, macOS on Apple Silicon and Intel,
Windows x86_64 and ARM64, and FreeBSD x86_64.

Then:

```sh
cig doctor          # the installer's check of every tool, cig's own state included
cig language        # the whole language on one screen
cig run examples/hello.cig -- world
```

## Sixty-second tour

```cig
roll count = 0                 # a variable
stick limit = 3                # a constant: sticks never change
pull greet(name) {             # a function
  snuff "hi ${name}"           # snuff returns
}
exhale greet("jay")            # exhale prints
for n in 1..4 {                # ranges, lists, maps, strings iterate
  count += n
}
if count > limit { exhale "over" } else { exhale "under" }
try {
  cough "something broke"      # cough raises
} ashtray err {                # ashtray catches
  exhale err.message, err.line
} finally {                    # runs either way
  exhale "cleanup"
}
stick doubled = [1, 2, 3].map(pack(x) => x * 2)   # pack is a lambda
exhale doubled >> len          # >> pipes the left value into the call
```

Types: `null`, `bool`, `int`, `float`, `string`, `list`, `map`, `function`,
`chain`. Integer overflow, division by zero and out-of-range indexes are
errors, never silent.

## The burn model in one table

| you write | in `cig run` | in `cig run --dry-run` |
|---|---|---|
| `fs.read_text(p)` (a read) | reads | reads |
| `fs.rm(p)` outside a burn | refused before the script starts | refused |
| `burn { fs.rm(p) }` | snapshots `p`, journals, deletes | records "delete p" in the plan |
| `burn unlit { fs.rm(p) }` | records the intent; never deletes | records the intent |
| `burn { proc.run("git", ["gc"]) }` | runs it, journaled as irreversible | records "run git gc" |
| `burn (s) { proc.run("git", ["push"]) } unburn { … }` | runs it; the compensation is journaled with `s` and runs on rollback | records "run git push" as compensated |
| `pack { "./build" }` then `burn { fs.rm("../other") }` | refused before the run (`E750`) or at it (`E751`) | refused |

Run records live under `~/.cigscript/runs/<id>/`. `cig runs` lists them,
`cig runs <id>` shows one with its journal, `cig unburn <id>` restores one.
What rollback covers, and what it cannot and says so, is written down in
[docs/BURN.md](docs/BURN.md).

## The four kinds of no

When CigScript says no, it tells you which no it is, and where to look. The
vocabulary is the [lexicon's](docs/LEXICON.md#the-four-kinds-of-no); every
code's kind of no is on its `cig explain` page, doctor says it in brackets,
and `--plain` swaps in the manual's name.

| what you hear | whose problem | the manual's name | in CigScript, for example |
|---|---|---|---|
| "can't see any ciggies bro" | yours: you're pointed at the wrong place | wrong address; DNS, path, PATH; `ENOENT` | `E301` unknown name, `E504` index out of range, a missing file under `E508`, a program not on PATH, `E551` |
| "never heard of you" | the door: it doesn't know you | `401` | reserved for hops that authenticate |
| "these are MY ciggies" | the door: it knows you and said no | `403`; a lock; permission denied | `E303` and `E701` effect outside burn, `E302` stick reassigned, `E705` already rolled back, permission denied under `E508` |
| "don't have any over here" | nobody's: right place, right you, nothing there | `404` from the server's side; empty | `E505` missing key, `E703` snapshot missing, `E805` no `gh` or `curl` |

## Break it

CigScript is built to be too robust to break without cheating. So cheat.

Symlinks pointing outside the tree. Kill it mid-burn. Fill the disk. Edit a
file under it while it runs. Yank the power. Fold something that shouldn't
fold. If the kernel ever lies about what it can undo, or loses a file, or
says "aww yea" when it should have said no, I want to know that day.

**When it crashes, or catches itself:** it already wrote a report to the
ashtray. After every run the tripwires check its promises (each restored path
re-hashed against the journal, the journal read back, the tin and the box
sampled) and a broken one becomes a report too, with an anonymised repro built
from the journal and proved in a sandbox. `cig crash list` shows what is
saved and what went (a stub for `ciginstaller crash list`); say yes once (`reports = always`) and reports of cig's
own bugs go straight to this repo, one issue per bug (redacted; `cig crash
show` prints exactly what travels). Your script's failures are never reported.
To report a bug by hand, `ciginstaller status` and `ciginstaller bug cig` write
it up with the evidence and show exactly what would be sent first; the issues
page works without the installer.
[docs/REPORTS.md](docs/REPORTS.md) has the whole pipeline.

**When it's wrong without crashing** (the dangerous kind: exit 0, "file
restored", wrong file): `cig runs` for the run id, then `cig report <run>
--send` (which runs `ciginstaller bug --run <run> --part cig --send`) files the
script's hash, the plan, the journal, the diagnostic and doctor's diagnosis as
one redacted issue; `cig report <run> --out FILE` (`ciginstaller report <run>
--cig --out FILE`) writes the bundle alone to a file to attach to [a cheat
issue](https://github.com/otmof-ops/cig-script/issues/new?template=cheat.yml)
with the script, the command, what the plan said, and what happened.

**What you get back:** a fix, a regression test with the cheat's name on it,
and a row on the wall below. It will slap.

### The wall

Every row is backed by the test it names; a row may only claim a cheat failed
when that test exists (`tests/docs.rs` checks). Bold is what you're looking
for.

| cheat | result | since | test |
|---|---|---|---|
| effect hidden in a function, a lambda in `.map`, `proc.run` outside a burn | refused, statically and at runtime | 1.0.0 | `effect_hidden_in_a_function_a_lambda_in_map_and_proc_run_outside_a_burn_are_refused` |
| `cough` mid-burn after a write and a delete | rolled back, newest first | 1.0.0 | `cough_mid_burn_after_a_write_and_a_delete_rolls_back_newest_first` |
| `fs.mv` over an existing file, then `unburn` | both files back | 1.0.0 | `mv_over_an_existing_file_then_unburn_brings_both_back` |
| `fs.rm` on a non-empty dir, then rollback | tree and modes restored | 1.0.0 | `rm_on_a_non_empty_dir_then_rollback_restores_tree_and_modes` |
| killed by a signal mid-burn | journal intact, `unburn` restored everything | 1.0.0 | `killed_mid_burn_leaves_the_journal_intact_and_unburn_restores_everything` |
| a script pinned to a cig that is neither installed nor on the shelf | refused with the version and the command that fetches it, never run on the wrong cig | 1.2.0 | `a_pinned_script_names_the_version_it_needs` |
| a journal from a newer cig, unburned by an older one | refused by its format number, or handed to the cig that wrote it | 1.2.0 | `unburn_hands_an_old_journal_to_the_cig_that_wrote_it` |
| a release asset whose checksum does not match | never kept, never run | 1.2.0 | `a_humidor_binary_with_the_wrong_hash_is_never_run` |
| a script that uses a builtin newer than its pin | caught by `cig check` before anything runs | 1.2.0 | `a_script_pinned_below_a_builtin_it_calls_is_caught_before_it_runs` |
| an upgrade that changes what a script would do | the plans are compared first; the proved version runs it | 1.2.0 | `identical_plans_run_on_the_new_version` |
| a run that fails on the new cig after an irreversible op | not tried again on the old one | 1.2.0 | `a_failed_reversible_run_falls_back_and_an_irreversible_one_stops` |
| a hop edits and deletes files it said it might touch, then the run fails | every one of them back, byte for byte, from the tin | 1.2.0 | `a_modified_file_comes_back_byte_for_byte` |
| an edit that keeps a file's size and puts its mtime back | seen by the hash, restored | 1.2.0 | `a_same_size_edit_inside_touches_is_seen` |
| a virtualenv or a cache inside the pack | not journaled, not restored, left to be rebuilt | 1.2.0 | `a_venv_inside_the_pack_is_not_journaled` |
| a tin object edited after the run | refused before anything is restored from it; `gc` keeps what a run still needs | 1.2.0 | `gc_keeps_every_chunk_a_live_run_needs` |
| a run dies and its pid is reused by another program, or by another cig | the record reads `interrupted`, not `running`: the live process must be a cig started when the record says | 1.3.0 | `a_dead_run_whose_pid_was_reused_is_interrupted_not_running` |
| `fs.write_text("nul", ...)`, `report.COM1.txt` | refused by the checker with `E758` on Windows, a warning elsewhere (the script will not run there); the kernel refuses a computed one before journaling it | 1.3.0 | `writing_to_nul_is_refused_before_anything_runs` |
| a hop replaces, retargets or deletes a symbolic link | each comes back as the link it was, from the journal alone | 1.3.0 | `the_tin_restores_a_symlink_as_a_symlink` |
| a hop runs `chmod` on a file, which changes neither its size nor its time | seen, and put back where the file was in the tin; said to be irreversible where it was not | 1.3.0 | `the_tin_restores_a_mode_changed_by_a_hop` |
| a file dated 2020 is edited by a hop or a native write, then the run fails | it comes back dated 2020, so mtime-driven tools see nothing | 1.3.0 | `the_tin_restores_mtime` |
| a read-only file under `touches` | kept in the tin like any other, and restored read-only | 1.3.0 | `a_read_only_file_under_touches_is_kept_in_the_tin` |
| `proc.run("./tool", [], {cwd: "sub"})` | the program is found beside the hop's own directory on every platform | 1.3.0 | `a_relative_program_resolves_against_the_hops_cwd` |
| `proc.run("npm", ...)` on Windows | `npm.cmd` is found through `PATHEXT` and started; `proc.which` names the same file | 1.3.0 | `npm_resolves_to_npm_cmd_on_windows` |
| `proc.run("bash", ...)` on Windows with WSL installed | Git's bash runs, never System32's WSL launcher | 1.3.0 | `bash_on_windows_is_not_the_wsl_launcher` |
| `proc.shell("x=$((6*7)); echo $x")` on Windows | runs under `sh` when one is on PATH, and the plan says so | 1.3.0 | `proc_shell_on_windows_runs_sh_when_present` |
| a file an editor holds open on Windows, changed by a hop, then the run fails | put back in place through the sharing the editor allows, and the line says so | 1.3.0 | `a_file_held_open_without_share_delete_is_restored_in_place_on_windows` |
| `cmd /c powershell Start-Sleep 40` with `timeout_ms: 1500` on Windows | the whole tree ends at the timeout; the hop does not wait on the grandchild's pipe | 1.3.0 | `a_timeout_kills_the_grandchild_on_windows` |
| unbounded recursion | clean `E506` at depth 4000 | 1.0.0 | `unbounded_recursion_is_a_clean_e506` |
| `MAX + 1`, `(MAX + MAX) / 2`, `1.0 / 0.0`, `0.0 / 0.0` | diagnostics, never silent | 1.0.0 | `arithmetic_edges_are_diagnostics_never_silent` |
| delete a symlink, `unburn` | **came back as a regular file**; now a symlink, dangling or not, inside restored trees too | fixed 1.1.0 | `symlinks_roll_back_as_symlinks` |
| random ops over random trees, modes and links, then rollback | byte-identical every time, 60 seeds | 1.1.0 | `rollback_property_over_random_trees` |
| `"x".repeat(i64::MAX)` | **aborted the process**; now `E514`, a diagnostic | fixed 1.1.0 | `allocation_ceiling_is_a_diagnostic_not_an_abort` |
| `-9223372036854775808` as a literal | **lexer rejected it**; now parses | fixed 1.1.0 | `i64_min_is_a_literal_and_the_overflow_next_to_it_is_a_diagnostic` |
| edit a file by hand after the run, then `unburn` | **restored the old snapshot over the edit**; now refused, `E704`, naming the later run when there is one | fixed 1.1.0 | `unburn_refuses_when_a_file_changed_since_the_run_unless_forced` |
| `while true {}` | **hung until killed**; now `E515` "your loop never ends", with the line, after the step budget | fixed 1.1.0 | `while_true_ends_with_your_loop_never_ends_and_a_line` |
| `unburn` the same run twice | **restored the old snapshots over newer work**; now refused, `E705` | fixed 1.1.0 | `unburn_refuses_a_second_time_without_force` |
| delete one snapshot, then `unburn` | **restored half and stopped**; now refused before touching anything, `E703` | fixed 1.1.0 | `unburn_checks_every_snapshot_before_touching_anything` |
| kill it mid-burn, then look | **invisible: status `running`, 0 burns, doctor silent**; now `interrupted`, counted from the journal, offered for `unburn` | fixed 1.1.0 | `interrupted_run_is_recognised_counted_from_the_journal_and_unburnable` |
| a chain error, from `cig light` | **pointed at `file:0:0`**; now the step's own line | fixed 1.1.0 | `chain_failures_point_at_the_step_not_the_light_call` |
| chain dry-run where step 2 reads step 1's write | **FAILED at step 2**; now the pretend writes live in the ghost filesystem and every read consults it first | fixed 1.1.0 | `chain_dry_run_reads_ghost_write` |
| `yes` piped into `head -c 20M` on stderr while stdout is awaited | no deadlock, both streams read concurrently | 1.0.0 | `large_output_on_either_stream_does_not_deadlock` |
| a child that ignores SIGTERM, with a grandchild in the background, under a timeout | **the grandchild outlived it**; now the whole process group is killed, SIGKILL after the grace period | fixed 1.1.0 | `a_timed_out_child_is_killed_with_its_whole_process_group` |
| a tool that prints progress on stdout, through `proc.json` | **would have been a confusing parse error three steps later**; now `E556` at the hop, quoting the first bytes, no partial value | 1.1.0 | `stdout_and_stderr_are_never_merged_and_shape_is_checked_at_the_hop` |
| a child killed by a signal | **came back as `code: -1` with half its output**; now `E555`, nothing passed on | fixed 1.1.0 | `timeouts_always_exist_and_partial_output_is_never_mistaken_for_output` |
| a child writes inside the pack, then `cough` | the files it created are removed; the hop shows `compensated` when a compensation covers it | 1.1.0 | `plan_three_a_cough_rolls_back_and_runs_the_compensation` |
| a child writes outside the pack, in its working directory | **would have been invisible**; now `E752` names the paths and says the kernel cannot undo them (a write elsewhere is covered only by the hop's irreversible label) | 1.1.0 | `plan_two_child_writes_outside_the_pack_is_labelled_not_hidden` |
| `unburn` weeks later, script deleted | the compensation runs from the journal alone, newest first across the whole run | 1.1.0 | `plan_four_unburn_after_a_success_unfolds_everything_in_reverse` |
| a chain that lights itself through another chain | **ran until the step budget**; now `E604` with the loop | fixed 1.1.0 | `fold_checks_refuse_with_codes` |
| retry a chain step that already pushed | **would have pushed twice**; now not retried unless `retry_irreversible`, and file-only attempts are rolled back before the retry | 1.1.0 | `retries_are_per_hop_and_idempotence_aware` |
| dry-run reads a file a step just pretended to delete | **a misleading not-found**; now `E520`, naming the op that removed it | fixed 1.1.0 | `dry_run_reports_reads_of_ghost_removed_paths_as_e520_and_missing_sources_as_e508` |
| your own file, added to a directory the run created, then `unburn` | **removed the directory with your file in it**; now directories are fingerprinted in `after.json` and `E704` refuses | fixed 1.1.1 | `a_file_added_to_a_run_created_directory_is_not_lost_by_unburn` |
| a symlink inside the pack that points outside | **the write landed outside**; now the pack check resolves links first and the refusal says where it lands | fixed 1.1.1 | `a_symlink_inside_the_pack_pointing_outside_does_not_let_a_write_escape` |
| a journal whose last line a crash cut short | **unloadable; nothing could be unburned**; now the entries before the cut are restored and the cut is reported | fixed 1.1.1 | `a_journal_cut_short_by_a_crash_still_unburns_what_came_before_it` |
| 8 MB through `stdin` to a child that echoes it back | **deadlocked**; now stdin is written beside the readers | fixed 1.1.1 | `large_stdin_does_not_deadlock` |
| Ctrl-C while a hop runs | **the child kept running, orphaned in its own process group**; now SIGINT, SIGTERM and SIGHUP reach every live child group first | fixed 1.1.1 | `a_signal_to_cig_reaches_the_child_process_group` |
| `yes` into `head -1` through `proc.pipe` | **`E553`, "stage 1 exited -1"**; now a consumer closing early is not the producer's failure, and `.stages` says `closed_early` | fixed 1.1.1 | `an_upstream_stage_closed_early_by_its_consumer_is_not_a_failure` |
| a `cwd` that does not exist | **`E551`, blaming the command**; now `E550` names the directory | fixed 1.1.1 | `a_missing_cwd_is_e550_naming_the_directory_not_the_command` |
| 50,000 nested brackets | **stack overflow, abort**; now `E208` at 5,000 levels, and the 100 kB line is windowed around the caret | fixed 1.1.1 | `fifty_thousand_nested_brackets_are_a_diagnostic_not_a_stack_overflow` |
| a list that contains itself, printed | **stack overflow, abort**; now `…` at depth 512 | fixed 1.1.1 | `a_list_that_contains_itself_prints_without_overflowing` |
| a UTF-8 byte-order mark at the top of the file | **`E103` unexpected character**; now skipped | fixed 1.1.1 | `a_utf8_byte_order_mark_is_skipped` |
| `fs.cp("a.txt", "a.txt")` | **truncated the file to 0 bytes and reported success**; now `E508`, nothing changed, same for `fs.mv` and a destination inside the source | fixed 1.1.1 | `a_copy_or_move_onto_itself_or_into_itself_is_refused` |
| `fs.mv` the OS refused (into its own subdirectory, onto a non-empty directory), then rollback | **"moved back" a file that never left, replacing it**; now the failed move is marked undone and the source is untouched | fixed 1.1.1 | `a_move_the_os_refused_is_marked_undone_and_never_moved_back` |
| a move across filesystems, then rollback | **the move back failed and the only copy was removed**; now copied back, and the only copy is kept when that fails | fixed 1.1.1 | `a_cross_device_move_rolls_back_by_copying` |
| write through a file with a second hard link, then rollback | **the other name kept the run's content**; now restored in place so every link sees the old bytes | fixed 1.1.1 | `a_hardlinked_file_is_restored_in_place` |
| a setuid file and a sticky directory, removed, then rollback | **came back as 0755 and 0775**; now the bits survive the snapshot and the restore | fixed 1.1.1 | `setuid_and_sticky_bits_survive_rollback` |
| `fs.rm` on a named pipe | **hung forever in the snapshot**; now refused, `E702` | fixed 1.1.1 | `a_named_pipe_is_refused_not_hung` |
| a rollback that could not restore everything | **status `rolled_back`, the failures nowhere**; now `rollback_incomplete`, listed by `cig runs <id>` | fixed 1.1.1 | `rollback_failures_are_recorded_on_the_run` |
| `chmod 500` the run directory, then `unburn` twice | **the second unburn restored again**; now refused before touching anything unless forced | fixed 1.1.1 | `a_read_only_run_directory_refuses_unburn_until_forced` |
| damage `run.json`, or rename the run's directory | **the run vanished; its journal was unreachable**; now listed as `damaged`, found where it is, unburnable | fixed 1.1.1 | `a_damaged_or_renamed_run_record_still_lists_and_unburns` |
| `burn (s) { proc.run(...); cough } unburn { ... }` | **the hop was labelled `compensated` though no compensation was ever recorded**; now stamped only when the block completes | fixed 1.1.1 | `a_burn_block_that_fails_half_way_leaves_its_hops_honestly_irreversible` |
| a compensation that reads `args`, run weeks later by `cig unburn` | **unknown name `args`**; now the run's arguments are there | fixed 1.1.1 | `a_compensation_that_reads_args_runs_on_a_deferred_unburn` |
| a hop writing through a link inside the pack | **invisible to hop watching**; a link that stays inside the pack is seen under its real path, and one that leads out of the pack is a door: the write is reported outside the pack (`E752`) and never rolled back, so nothing outside the pack is ever removed | fixed 1.1.1, 1.4.0 | `a_hop_writing_through_a_link_out_of_the_pack_is_reported_and_never_rolled_back` |
| dry-run: write through `link/`, read through `real/` | **not found**; now one ghost entry, and every directory a nested write creates is listed | fixed 1.1.1 | `the_ghost_resolves_symlinked_directories_and_lists_created_ancestors` |
| `pack { "" }` | **silently meant the whole working directory**; now `E755` | fixed 1.1.1 | `an_empty_pack_root_is_refused` |
| `-9223372036854775808 / -1` | **crashed (E901)**; now `E503` | fixed 1.1.1 | `i64_min_divided_by_minus_one_is_a_diagnostic` |
| `json.stringify` of a list that contains itself; `==` on two of them | **stack overflow, abort**; now a diagnostic, and the comparison ends | fixed 1.1.1 | `a_value_that_contains_itself_is_refused_by_json_and_compared_without_looping` |
| `exhale 1e16`, `int(1e300)` | **printed `10000000000000000` as if an int; saturated to `9223372036854775807`**; now `1e16`, and `E503` | fixed 1.1.1 | `floats_stay_floats_past_sixteen_digits_and_int_refuses_what_does_not_fit` |
| `roll args = ...`, and a redeclaration under `--no-check` | **passed `cig check`, then `E306 runtime` exit 1**; now shadowable, and a check-family error with exit 2 | fixed 1.1.1 | `a_redeclaration_at_run_time_is_a_check_error_and_args_is_shadowable` |
| a string whose closing quote sits on the next line | **silently a two-line string**; now `E101` on the line it opened | fixed 1.1.1 | `a_string_that_runs_past_its_line_is_e101` |
| 300,000 chained `+ 1`; `CIG_MAX_STEPS=abc` | **stack overflow; silently ignored**; now `E208` past 10,000, and `E800` | fixed 1.1.1 | `chained_operators_are_capped_and_a_bad_step_budget_is_a_usage_error` |
| `{ok: [0], check: false}` versus `{check: false, ok: [0]}` | **the contract depended on key order**; now `ok` wins either way | fixed 1.1.1 | `ok_and_check_resolve_the_same_whatever_their_order` |
| `proc.run("/tmp")`, `proc.kv` of `export FOO=bar`, `proc.which("bin/ls")` | **"permission denied"; key `export FOO`; null**; now named as a directory, `FOO`, the path | fixed 1.1.1 | `a_directory_as_the_command_export_lines_and_a_path_to_which` |
| `snuff` out of a `burn (s) { } unburn { }` block | **the compensation was never recorded; a later rollback or `cig unburn` skipped the undo** | fixed 1.1.1 | `a_compensating_block_left_by_snuff_still_records_its_compensation` |
| `fs.append_text` to a log in a directory that does not exist yet | **`E508` no such file, after a dry run that saw no problem**; now the parent is created, as `fs.write_text` always did | fixed 1.1.1 | `append_text_creates_the_parent_directory_like_write_text_and_the_dry_run_agrees` |
| `env.set` in a dry-run plan | **labelled reversible**, against the docs; now irreversible, which is what the kernel can do about it | fixed 1.1.1 | `env_changes_are_labelled_irreversible_in_the_plan` |
| a script that makes `build/out/` inside the pack, then a rollback | **the files went, the directories stayed, empty**; now hop watching journals the directories a script creates and rollback removes them | fixed 1.1.2 | `directories_a_hop_creates_are_removed_by_rollback_and_unburn` |
| `fs.glob("*")`, `fs.glob("./scripts/*")` | **an empty string among the matches; nothing at all**; now neither, and a pattern without `**` walks no deeper than it can match | fixed 1.1.2 | `glob_star_lists_no_empty_entry_and_dot_slash_patterns_match` |
| `cig run /dev/zero`; `cig frobnicate` | **read forever; exit 2 (a script error's code)**; now `E801`, and exit 3 | fixed 1.1.1 | `a_device_as_the_script_is_refused_and_a_bad_invocation_exits_3` |
| a hop deletes a folder of a thousand files it did not name in `touches` | one warning for the folder, with the glob that would have kept it and a few of the files; `cig runs <id>` has every one | 1.4.3 | `changes_outside_touches_are_reported_once_per_folder` |
| `cig unburn <id> --give-back --dry-run` | the files the rollback set aside, what is at each path now and whether it changed since; nothing written, and a changed path refused without `--force` | 1.4.3 | `give_back_dry_run_lists_what_it_would_restore_and_changes_nothing` |
| a hop changes one byte of a 1 MiB file it said it might touch, fifty times | the tin stores the chunk that changed, not the file again; the file comes back byte for byte from its recipe | 1.5.0 | `a_hop_edit_to_a_large_file_stores_the_chunk_that_changed` |
| delete or corrupt one chunk of a recipe, then restore | refused before anything at the path is touched; nothing half-written | 1.5.0 | `a_missing_chunk_refuses_the_restore_and_writes_nothing` |
| edit a journal line after the run, then `unburn` | the line no longer fits its neighbours: `cig runs <id> --verify` names it, `unburn` refuses with `E760` unless `--force` | 1.5.0 | `unburn_refuses_a_broken_chain` |
| remove or reorder a journal line | the chain breaks at that line | 1.5.0 | `a_removed_journal_line_breaks_the_chain` |
| a hop edits a file git holds, tracked and clean, under `touches` | not copied: the journal names the blob, the run pins it under `refs/cig/runs/<id>`, and the restore checks the bytes git gives back against our hash | 1.5.0 | `a_clean_tracked_file_costs_no_tin_space` |
| git answers with other bytes for a pinned blob (`git replace`) | refused: the bytes do not hash as the journal recorded, nothing written | 1.5.0 | `a_restore_from_a_git_blob_is_hash_checked` |
| the pin is dropped and the blob gc'd, then `unburn` | refused before anything is touched (`E703`), naming the repository and the blob | 1.5.0 | `a_blob_git_has_lost_refuses_the_restore_with_the_reason` |
| a hop under `touches: ["**"]` rewrites `id_rsa` and deletes `.env`, then the run fails | both come back byte for byte, and not a byte of either is in the clear anywhere under the state directory | 1.5.0 | `a_private_key_under_touches_is_never_stored_in_the_clear` |
| flip one byte of a sealed object, then `unburn` | refused before anything is touched; the tag does not fit | 1.5.0 | `a_tampered_sealed_object_refuses_the_restore` |
| the key is gone (a wiped keyring, a new machine), then `unburn` | refused, naming the file; `cig tin gc --sealed-orphans` clears what nothing can open | 1.5.0 | `without_the_key_a_restore_refuses_and_names_the_file` |
| a tool rewrites a file no `touches` named, keeps the bytes before and reports both through `CIG_JOURNAL` | reversible all the same: the copy is hashed, kept, and put back on rollback | 1.6.0 | `a_tool_that_reports_its_write_is_reversible_without_touches` |
| a tool reports a path outside the pack, or a before-hash its copy does not match | refused with the reason (`E761`); nothing journaled on its word | 1.6.0 | `a_reported_path_outside_the_pack_is_refused` |
| a hop starts a `cig run` of its own, then the parent fails | the child reported its undo; the parent's rollback unburns the child through its own journal | 1.6.0 | `a_nested_cig_run_is_undone_by_the_parents_rollback` |
| `cig parcel` a run, carry the file to a machine with a different home and the pack at another path, `cig unparcel --root`, `cig unburn` | every file checked against the manifest, the paths read through the map, the chain still intact; the run is undone there, the original machine untouched | 1.6.0 | `a_parcel_unburns_on_another_machine` |
| change one byte inside a parcel, then `cig unparcel` | the whole parcel refused (`E762`); nothing placed | 1.6.0 | `a_parcel_with_a_wrong_hash_is_refused_whole` |
| a hop creates a file in a 60,000-entry pack, where 1.4 stopped scanning at 50,000 | removed on rollback like any other; the scan has no point past which a change goes irreversible | 1.6.0 | `a_created_file_past_the_old_cap_is_removed_on_rollback` |
| a hop creates, edits and deletes among 500,000 entries | everything undone, under five minutes (heavy; `cargo test --release -- --ignored`) | 1.6.0 | `a_hop_in_a_500k_entry_pack_is_fully_reversible` |
| a hop in the box rewrites, deletes and creates files, then exits outside its contract | nothing of it reached the pack: the layer is dropped, there is nothing to roll back (Linux) | 1.6.0 | `a_failing_hop_in_a_box_changes_nothing` |
| a hop in the box deletes one folder and replaces another whole | applied as native burns from the layer's whiteouts and opaque directory; `unburn` brings both folders back as they were (Linux) | 1.6.0 | `a_boxed_hop_that_deletes_and_replaces_folders_applies_correctly` |
| `{box: true}` where the box cannot be made | the plan says so before the run, the hop runs on the tin, the run says so (`E763`) | 1.6.0 | `when_the_box_cannot_be_made_the_hop_falls_back_to_the_tin_and_says_so` |
| a hop reports a `compensate` line, but the script has no `pack { }` | the undo still runs on rollback; a file claim with nothing to judge it against is said and dropped (`E761`) | 1.6.1 | `a_hops_compensation_counts_without_a_pack` |
| the undo a hop reported writes 300 KB to stdout, or fails with a 3000-character last line | the rollback finishes (both pipes are drained while it runs) and the report quotes 200 characters of the line | 1.6.1 | `a_chatty_undo_neither_hangs_the_rollback_nor_floods_the_report` |
| a boxed hop with `cwd` inside the pack empties a folder and makes it again | the box is made (the working directory is taken from the parent, not through the overlay), the folder is there and empty afterwards, no layer is left under the run, the record prunes (Linux) | 1.6.1 | `a_boxed_hop_that_empties_and_remakes_a_folder_leaves_it_in_place` |
| a boxed hop names `touches_sealed` and rewrites a file under it | the file's before-bytes are sealed in the run's snapshot, never in the clear (Linux) | 1.6.1 | `a_boxed_hops_touches_sealed_seals_what_it_rewrote` |
| a boxed build writes 3,000 object files under `target/` and the burn fails | `target/` and the other build and dependency trees pass through the box as they pass the watch: one path applied, nothing of the build journaled or undone (Linux) | 1.6.1 | `a_boxed_hops_writes_under_a_build_tree_pass_through` |
| a parcel is imported on B, written again from B (before or after B's own unburn and give-back), imported on C | the manifest carries B's map too, so C reads a journal naming A's and B's paths through one map: chain intact, every object there, `unburn` works | 1.6.1 | `a_parcel_written_from_an_imported_run_still_unburns_on_a_third_machine` |
| `proc.pipe` with `touches` rewrites and creates files, then the burn fails | the pack is watched around the whole pipeline: restored and removed like a hop's, and every stage may report through `CIG_JOURNAL`; `box` on a pipeline is refused (`E400`) | 1.6.1 | `a_pipeline_with_touches_is_reversible_like_a_hop` |
| the tin's key file is gone while sealed objects depend on it | no new key is made: the restore refuses naming the key and where it was (`E703`), a run that would seal says so and keeps the change irreversible (`E757`); the key back, the run unburns | 1.6.1 | `a_missing_seal_key_is_refused_rather_than_replaced` |
| a script pinned to `>=1.5.0` uses `{box: true}`, or to `>=1.4.0` uses `touches_sealed` | `E309` names the option and the release it arrived in; `cig pin` writes the version a `{sensitive: true}` write needs | 1.6.1 | `a_script_pinned_below_the_box_or_sealed_touches_is_caught` |
| the last line of a journal is edited (no line follows it to carry its hash) | `--verify` says the head does not match the record, exit 1; `unburn` refuses with `E760` and restores nothing; `--force` proceeds knowingly | 1.6.1 | `a_changed_last_journal_line_refuses_the_unburn` |
| a hop makes a tree and `chmod 000`s a directory inside it, then the burn fails | the tree goes whole on rollback, no `Permission denied` and nothing left above it | 1.6.2 | `a_rollback_removes_a_hop_created_tree_with_unreadable_directories` |
| a boxed hop's run dies in the hop, leaving overlayfs's unreadable work directory under the record | `cig unburn` removes the layers with the rollback, and the record prunes | 1.6.2 | `the_layers_an_interrupted_boxed_hop_left_go_with_the_unburn` |
| the same, and nobody unburns the dead run | the next `cig run`, of anything, sweeps the layers of every run that is not alive | 1.6.2 | `the_layers_a_dead_run_left_are_swept_when_the_next_run_starts` |
| a boxed hop starts a `cig run` whose state directory is inside the box, with a boxed hop of its own | the inner plan says before the run that the box cannot be made here, naming the overlay its layers would sit on; the inner hop runs on the tin (`E763`) and the outer box applies the lot (Linux) | 1.6.3 | `a_box_inside_a_box_is_refused_in_the_plan_with_the_reason` |
| `cig report-bundle <run> --queue` (what `cig report <run> --send` ran before it became a stub for `ciginstaller bug`) | the bundle is filed as an issue through the ashtray, labelled `report`, with the live stats block; an explicit `reports = never` sends nothing | 1.7.0 | `report_send_files_the_bundle` |
| a report is sent while the network is down | it waits in the ashtray with the reason; the next `cig run` of anything sends it in the background, the run itself never waiting | 1.7.0 | `a_report_made_offline_is_sent_on_the_next_run` |
| `cig report-bundle <run> --queue` under `CI` with nothing set | refused, `never` by default there so no pipeline files public issues by surprise; `cig config reports always` holds under CI | 1.7.0 | `ci_never_files_without_the_setting` |
| the script coughs, divides by zero, adds a string to a number | nothing reported: the script's failures are the script's; an internal error (`E900`) is cig's and is filed, labelled `internal` | 1.7.0 | `a_cough_in_the_users_script_is_never_reported` |
| the rollback puts a file back with one mode bit wrong (a fault injected for the test) | the rollback tripwire re-hashes and re-checks every restored path, trips, queues a report (`E902`) and the run's outcome stays what it was | 1.7.0 | `a_rollback_that_restores_the_wrong_mode_trips` |
| the pack holds `books/Clients-Private/Ledger-2026.txt` with account numbers when a tripwire trips | the repro built from the journal names `root0/D1/F1.txt`, generated bytes of the same size, a stub for the hop; neither the issue nor the report carries a name, a byte or a path of yours; the shrinker keeps what still trips | 1.7.0 | `a_repro_contains_no_user_path_or_content` |
| the bug does not reproduce in the sandbox | the report goes without a repro and says so; what was found is still said, the path as its placeholder | 1.7.0 | `an_unverified_repro_is_not_attached` |
| the same bug twice | one issue: its stats block counts two, no comment, no second issue; a comment only brings a new version, a new OS or a smaller repro | 1.7.0 | `a_second_report_updates_the_stats_not_a_comment` |
| the issue a report opened is closed and a release's notes name it | `cig crash announce-fixed` (the installer runs it after an update) says `a bug you reported (#N, …) is fixed in X: ciginstaller update`, once | 1.7.0 | `a_fixed_report_is_announced_by_crash_announce_fixed` |
| a run in `~/clients/AcmeCorp/MySecretProject` trips a tripwire, its bundle is sent, another tool hands a report in with the folder in every field | nothing of a name of yours travels: the working directory is its shape (`out/d1/D2/d3`, `~/d1/D2`), the command line keeps only the words cig knows (`cig run <script>`), every path in a message, a bundle or a handed-in report is its placeholder; `cig crash show` prints exactly that | 1.7.1 | `a_report_never_contains_a_path_component_of_the_users_folder` |
| cig panics in `~/Zqx-Work/sub` | the crash report's `cwd` is `~/D1/d2`, its command `cig run <script>`; the ashtray's bookkeeping (queue, issue, announcement) never travels | 1.7.1 | `a_crash_report_cwd_is_a_placeholder` |
| one run makes a folder, writes into it and deletes it, rewrites then deletes a file, a hop makes a tree that is deleted after; the burn fails | the pack is as it was and no tripwire trips: each path is judged by its state before the first op that touched it, not by the run's own intermediate states (a false alarm in 1.7.0) | 1.7.1 | `a_path_touched_twice_does_not_trip_the_rollback_tripwire` |
| `cig unburn` restores a file with one mode bit wrong (a fault injected for the test) | the rollback tripwire runs after an unburn too: it trips, queues a report naming the run, and the unburn's outcome stays what it was | 1.7.1 | `cig_unburn_runs_the_rollback_tripwire` |
| a report failed twice and waits out a five-minute backoff; the person types `cig crash flush` | it is sent now: typed, the flush is a request for now; the background's pass still honours the backoff | 1.7.1 | `a_typed_flush_sends_a_report_waiting_for_its_retry_time` |
| sixty restored files all come back wrong | the report lists twenty-five mismatches and counts the rest, so a run over hundreds of files still fits an issue; a body past the tracker's limit is cut back, details first | 1.7.1 | `a_report_with_many_mismatches_lists_twenty_five_and_counts_the_rest` |
| the pack holds `link -> one.txt` when a tripwire trips | the repro's seed is a link to a sibling, as it was, not a path from the working directory under `out/` | 1.7.1 | `a_relative_link_target_stays_relative_in_the_repro` |
| a hop under `pack { "./q", "./q/a" }` makes `q/a/h.txt` | one effect journaled, not one per root; the rollback removes it once | 1.7.1 | `a_hop_under_nested_roots_is_journaled_once` |

Add a row. The rules for reporting one are in
[CONTRIBUTING.md](CONTRIBUTING.md#reporting-a-cheat-that-worked).

## Commands

| command | does |
|---|---|
| `cig run script.cig [-- args] [--pin]` | check, then run; roll back on failure; `--pin` keeps the run whatever `--prune` says |
| `cig run --dry-run script.cig` | run with every burn simulated; print the plan |
| `cig check script.cig [--deny-warnings]` | parse and statically check without running |
| `cig chains script.cig` / `cig light script.cig <chain>` | list chains; light one |
| `cig eval "expr"` / `cig repl` | evaluate a snippet; interactive session |
| `cig runs [id] [--all\|--prune N\|--verify [--deep]\|--pruned\|--reindex\|--forget ID]` | the lab notebook: the newest 50 by default, every run with `--all`, one run's journal with an id, the chain checked with `--verify` |
| `cig runs pin <id>` / `cig runs unpin <id>` | keep (or release) a run past `keep_runs`, `keep_days` and `--prune` |
| `cig runs adopt <dir>` | adopt a committed `ciginstaller` transaction as a run, so `cig unburn <id>` undoes the installer update |
| `cig unburn <id> [--dry-run] [--force]` | put the world back the way it was, newest op first; `--force` retries only the entries a failed unburn left |
| `cig explain [code] [--schema] [--layers FILE:LINE:COL]` | the page for an error code; the JSON Schema for a diagnostic; the layers at a `.cigar` file place, handed to `cigar` |
| `cig doctor [run]` | a stored run's error diagnosed again, read-only; with no run it is `ciginstaller doctor --tools` |
| `cig report [run] [--out FILE\|--send]` | a stub for `ciginstaller bug`: no run is `bug cig`, a run is `bug --run <run> --part cig [--send]`; `--out FILE` is `ciginstaller report <run> --cig --out FILE` |
| `cig crash list\|show\|send\|delete` | stubs for `ciginstaller crash ...`; `cig crash flush` and `submit` stay cig's |
| `cig update [--check]` | a stub for `ciginstaller update [--check]`; the installer says which reports are fixed once it has updated cig |
| `cig uninstall` | a stub for `ciginstaller uninstall cig`; your `~/.cigscript` data stays |
| `cig versions [add\|remove\|prune]` | the humidor: every other version of cig, for pinned scripts and old journals |
| `cig pin <file> [--to 1.2]` | write `# cig 1.2` into a script, so a newer cig hands it to that version |
| `cig parcel <id> [--out FILE] [--with-key]` / `cig unparcel FILE [--root NAME=PATH] [--show]` | pack a single run, its tin objects and its compensations into one file; import it on another machine |
| `cig box list\|apply\|discard` | the layers hops kept rather than applied: list, apply (as a new run), discard |
| `cig pass list\|show\|verify\|narrow\|revoke\|export\|import\|log\|stats\|key\|gc` | passes: what was already checked, for exactly which inputs; `export` and `import` move a pass between machines; `key` prints this machine's allowed-signers line |
| `cig tin [du]` | how many objects the tin holds and how many bytes (the default) |
| `cig tin verify [--head]` | re-hash every object against its name, check every recipe, open every sealed object |
| `cig tin gc` | remove every object no run in the history still refers to |
| `cig tin head` | the ledger's head, one line, to keep elsewhere for `cig tin verify --head` |
| `cig tin mac --label L FILE` / `cig tin keep --label L FILE` / `cig tin give --label L --name N` | a keyed MAC over a file, kept under a label; a sealed copy kept under a label and name; the kept copy on stdout |
| `cig tin key export\|import` | back the tin's seal key up (passphrase-wrapped), or put a backed-up one in place |
| `cig tin rekey` | make a new seal key and re-seal every sealed object under it; crash-safe |
| `cig tin parcel [--to DIR\|--git [REF]] [--since PARCEL]` / `cig tin unparcel DIR\|--git REF` | back the whole tin up (every live run and journal, the objects they need, the ledger), checked before anything is written |
| `cig tin vault install\|uninstall\|gc\|update` | the vault: a no-delete store that runs as its own account (`_cigtin`), opt-in |
| `cig bench [manifest] [--only N] [--group G] [--samples N] [--save NAME] [--against WHAT] [--watch] [--profile] [--diff BASE] [--keep] [--repeat-until-slow] [--timings] [--stats] [--syscalls] [--gate GATES] [--family [--with P@V] [--matrix P=V1,V2]]` | measure: manifest cases, checked first, medians and p10 to p90; `--against` is interleaved A/B, `--family` runs every product's suite |
| `cig bench history [CASE] [--file F\|--branch B] [--series S]` | a case's nightly history: sparkline, table, cross-machine changes marked |
| `cig config [key [value]]` | the few settings there are |
| `cig language` | the legend and the standard library |

`--max-steps N` (or `CIG_MAX_STEPS`) is the step budget: a loop that never
ends is stopped with `E515` and its line; `0` disables it.
`--json` on any command gives machine-readable output (diagnostics follow
[docs/diagnostic.schema.json](docs/diagnostic.schema.json)); `--plain` /
`CIG_PLAIN=1` keeps every fact and drops the catchphrases; `--no-doctor` /
`CIG_DOCTOR=0` gives the bare diagnostic; `NO_COLOR` is honoured. Exit codes
are stable: `0` ok, `1` the script failed, `2` it did not parse or check, `3`
usage or environment problem, `70` `cig` itself crashed.

## Measuring cig

cig measures itself with its own benchmark command. The suite is
`bench/bench.toml`: start-up, the compute programs (checked against their
expected output first), and the kernel set (a burn, an unburn, a restore of a
32 MiB file, a hop over 2,000 unchanged files), with the bytes a run leaves on disk and
its syncs reported next to the time. One command runs it:

```
cargo build --release && cig bench bench/bench.toml
```

Medians, p10 to p90 and CV, never a mean alone; sizes are reported and not
gated. [docs/BENCH.md](docs/BENCH.md) lists the cases and what each number is.

## Documentation

- [docs/LEXICON.md](docs/LEXICON.md): every word, what it means, the manual's name. The source of truth for the vocabulary.
- [docs/LANGUAGE.md](docs/LANGUAGE.md): the language, statement by statement.
- [docs/DOCTOR.md](docs/DOCTOR.md): automatic diagnosis on every error, and why it never fixes.
- [docs/SMOKE.md](docs/SMOKE.md): the lexicon, explained by a smoker.
- [docs/REPORTS.md](docs/REPORTS.md): the tool reports its own bugs: whose fault it is, the tripwires, the ashtray, the anonymised repro, one issue per bug, the fix announced back.
- [docs/CHAINS.md](docs/CHAINS.md): chains, the automation layer.
- [docs/HOPS.md](docs/HOPS.md): running other programs, and the rules that hold at the boundary.
- [docs/BURN.md](docs/BURN.md): effects, modes, the journal, rollback, and what it can't undo and says so.
- [docs/PROTOCOL.md](docs/PROTOCOL.md): the burn protocol a tool speaks when it wants to say what it changed and how to undo it, rather than be watched from the outside.
- [docs/PASS.md](docs/PASS.md): passes — one authenticated record for every "already checked", what they carry, and how they are verified later or on another machine.
- [docs/PROGRAMS.md](docs/PROGRAMS.md): compiled programs: a script appended to the `cig` that runs it, its command line, and what differs from `cig run`.
- [docs/SCIENCE.md](docs/SCIENCE.md): CigScript in the lab.
- [docs/BENCH.md](docs/BENCH.md): `cig bench` and the `bench/bench.toml` manifest: samples in batches, medians, p10 to p90, CV and peak RSS; cig's own suite and the kernel measures.
- [docs/FINDINGS.md](docs/FINDINGS.md): the measurements behind the numbers the other documents quote, so each figure can be checked by running the same thing again.
- [docs/STDLIB.md](docs/STDLIB.md): every function with its arity and effect (generated).
- [docs/ERRORS.md](docs/ERRORS.md): every error code, its kind of no, what to type next, its causes (generated from the registry).
- [docs/DESIGN.md](docs/DESIGN.md): why it looks like this, the tone, what came before, and the roadmap.
- [HAMMER.md](HAMMER.md): history — the 1.1.0 "Hammer" update package, kept so its deliverable list and the commits that paid it off can still be read.
- [examples/](examples/): runnable scripts, all checked and run in CI.

## Status

1.21.0 is the current release (2026-11-12). Release assets are built by the
pipeline in [.github/workflows/ci.yml](.github/workflows/ci.yml) for Linux
on x86_64, aarch64 and armv7 (static musl, so they run on any glibc, on
Alpine, and under Termux), macOS on Apple Silicon and Intel, Windows on
x86_64 and ARM64, and FreeBSD on x86_64; every asset carries a build
attestation. `install.sh` and `install.ps1` in this repository redirect to
`ciginstaller`, which installs, updates and uninstalls cig as a verified
transaction (see **Install**).

The language surface in [docs/LANGUAGE.md](docs/LANGUAGE.md) and the effect
rules in [docs/BURN.md](docs/BURN.md) are the contract. Next, from the open
issues: wait times across the board (#188), history that pays you back
(#185), the roadmap for the measured-performance work (#150), more of the
burn kernel's throughput (#147), more of the tree-walker (#146), `cig net`
(#115), and the next steps of the box (#106).

## Licensing

- **Code** (`src/`, `tests/`, the build files): **source-available, not open source.** Under the
  [Functional Source License, version 1.1, with an Apache-2.0 future license
  (FSL-1.1-ALv2)](LICENSE). The Open Source Initiative does not count it as an
  open source license, and this project does not call it one.
  - *What you may do.* Use it, including inside a company for its own work;
    read it, study it, change it and share it with the license attached; and
    help others use it. The source is here for all of that.
  - *What you may not do.* Make it available to others in a commercial product
    or service that substitutes for it or does the same job. Selling it, or
    selling access to it, is the clear case.
  - *It becomes Apache-2.0.* Each version converts to
    [Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0) two years after it is made
    available.
  - *Earlier releases.* Every release through v1.20.0 was made under Apache-2.0 and stays under it. [LICENSING.md](LICENSING.md)
    lists which versions carry which license.
- **Documentation** (`docs/`, this README, the language specification):
  [CC BY 4.0](LICENSES/CC-BY-4.0.txt). Reuse and adapt with credit.
- **Examples** (`examples/`): [CC0 1.0](LICENSES/CC0-1.0.txt). Paste them
  into your own scripts; no attribution needed.
- **The name.** CigScript is a trademark of Jay Taylor. The keywords and the
  language itself are free for anyone to implement; the name is not free to
  reuse for a fork or product. [TRADEMARKS.md](TRADEMARKS.md) has the full
  policy, which is short and mostly says yes.
- **Your scripts are yours.** Nothing about running `cig` gives this project
  any rights in the `.cig` files you write.

Third-party crates in the binary are listed in
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md). Contributions are welcome
under a contributor licence agreement; see [CONTRIBUTING.md](CONTRIBUTING.md).
Security reports: [SECURITY.md](SECURITY.md).

Copyright (c) 2026 Jay Taylor (https://github.com/otmof-ops/cig-script).
