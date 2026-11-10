<!--
SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
SPDX-License-Identifier: CC-BY-4.0
-->

# The tool reports its own bugs

cig notices when it is at fault, including bugs nobody saw, saves the
report locally, builds an anonymised repro from the journal and checks that
it reproduces, files it (or adds to the issue already open for it) with no
typing, and tells the person who hit it when the fix ships. Your script's
failures are yours and are never reported. This page is the whole pipeline.

```
the tool notices its own bug (a crash, an internal error, a tripwire)
 → saves it in the ashtray
 → builds an anonymised repro from the journal, verifies it reproduces, shrinks it
 → files a new issue or updates the existing one's stats block
 → the repro becomes a failing test → fix → release
 → the reporter's cig says "a bug you reported (#N) is fixed in X: ciginstaller update"
```

## Whose fault it is

| failure | whose | reported |
|---|---|---|
| a panic (`E901`) | cig's | yes, kind `crash` |
| an internal error (`E900`: a condition cig believes impossible) | cig's | yes, kind `internal` |
| a tripwire (`E902`, below) | cig's | yes, kind `tripwire` |
| `cough`, a runtime or type error in the script, a hop that failed, a usage error | the script's or the person's | never |
| a run bundled with `cig report <run> --send` | the person's choice | yes, kind `report` |

## Tripwires

After the work is done, cig checks its own promises. Cheap checks, never
changing the run's outcome: a broken one becomes a report and one line,
`tripwire: cig noticed something it got wrong; a report is queued (cig crash
show <id>; cig explain E902)`.

| after | the check |
|---|---|
| a rollback | every path it says it restored is read back and compared with the recorded before-state: a file's bytes (re-hashed, 256 MB at most; compared by size past that, and the report says so), its mode, a link's target, a directory's presence, an absent path's absence |
| a run | the journal on disk read back: its chain intact, as many entries as the kernel wrote, the head the kernel holds |
| a run | up to three tin objects the journal refers to hash to their names |
| a run that burned | `verify_sample` objects (3 by default; `0` for none) drawn at random from the whole tin hash to their names: a random bucket, then a random object in it, so a big tin costs no more to sample than a small one, and an object an earlier run left that has rotted since is found by the next run |
| a box apply | the first eight files applied hash as the layer's |

The check the issue asked for that compares a run with the plan a dry run
would give is not here: a real run has no plan beside it without running
the dry run too, and the trial already makes that comparison when the
humidor holds the script's last good version. The journal re-read covers
the half of it that is cig's own bookkeeping.

## Ask once, then automatic

`reports = ask | always | never` is the one setting for every kind of report
(`crash_reports`, the older name, is read when `reports` is not set).

- **ask** (the default): the first time, in a terminal, the report is shown
  and the question asked once; the answer is remembered as `always` or
  `never`. Without a terminal the report is saved and the line says how to
  send it.
- **always**: reports are sent without asking; one line says a report was
  queued and where it goes.
- **never**: reports stay local.
- Under **CI** (`CI` set, no terminal) with nothing set: `never`, so nobody's
  pipeline starts filing public issues by surprise. `cig config reports
  always` holds there too.

## The ashtray

Every report is written to `~/.cigscript/ashtray/` before anything is sent,
like the journal before a burn (`crashes/`, from before 1.7.0, is read too).
A run never waits on the network: at the end of a command, if anything is
waiting and due, cig starts `cig crash flush --quiet` in the background and
carries on. A send that fails leaves the reason on the report and a retry
time (the next run, then five minutes, half an hour, two hours, twelve, a
day); `cig crash list` shows each report as `local`, `pending`, `failed:
reason` or `filed` with its issue. One sender holds the ashtray at a time, so
a run's background flush and a `cig crash send` typed meanwhile never file
the same report twice.

```
cig crash list                 the ashtray
cig crash show <id>            exactly what would be sent
cig crash send <id>            file this one now (a yes for it alone)
cig crash flush                send everything waiting and due
cig crash delete <id> | --all
cig report <run> --send        file a run's bundle the same way
```

## The repro

The journal records each op, its paths and before-states. From it cig
builds a script that reproduces the finding without any of the person's
data, and proves it before attaching it:

1. **Placeholders for paths.** Every path is relative to its pack root
   (`root0`, `root1`, ...) with each component replaced by a stable
   placeholder (`d1`, `f3.txt`) that keeps what a filesystem bug may depend
   on: depth, the extension, a leading dot, a capital letter (`D1`,
   `F3.txt`), a character outside ASCII (`d2é`). A path outside every root
   goes under `out/`.
2. **Generated bytes.** A seed file has the recorded size and bytes derived
   from the recorded hash, so two files that were equal stay equal and a
   deduplication bug survives; its mode travels; a link's target is mapped.
3. **The same sequence.** The burns in order, as `fs.*` calls; each hop
   becomes a stub shell script that performs the file operations the journal
   saw after it (created, changed, deleted), with the hop's `cwd`, `touches`
   and `box` mapped; the command the hop ran is never copied. A failing run
   ends in `cough "repro"` so the rollback runs.
4. **Verified before sending.** The repro runs in a sandbox (its own state
   directory, the roots laid out from the seeds) and must show the same
   finding: the same tripwire, an `E900`, a crash. One that does not is not
   attached, and the report says so.
5. **Shrunk.** Ops are dropped one at a time, newest first, while the finding
   still shows (sixteen tries or forty-five seconds at most), so the
   maintainer gets the smallest case.

The report's own text goes through the same placeholders: a path a tripwire
names is written as `root0/d1/f3.txt`, never as yours; the working
directory is its shape (`root0`, `out/d1/D2/d3`); the command line keeps the
words cig knows and gives everything else a shape (`cig run <script>`); and
anything else in a message that is shaped like a path or a file name is
mapped too, before the fingerprint is taken, so two people hitting one bug
in differently named folders land on one issue. The repro is the regression
test, ready to be named and added to `tests/`.

## One issue per bug

The fingerprint is the kind, the location and the message with its numbers
removed. The first report opens the issue, labelled by kind (`crash`,
`internal`, `tripwire`, `report`) and area (`tin`, `box`, `hop`, `parcel`,
`journal`); its body carries a stats block (count, versions, OSes, first and
last seen, smallest verified repro). A later report with the same
fingerprint updates the block instead of commenting, and comments only when
it brings something the issue did not have: a new version, a new OS, or a
smaller repro (which it pastes).

## Closing the loop

A filed report remembers its issue. `cig crash announce-fixed` (hidden; the
installer runs it after an update that changed cig, and a failure there does
not fail the update) asks the tracker whether the issue is closed and reads the releases' notes
for the one that names it (`#N`); the first time that is known, one line
says `a bug you reported (#N, what it was) is fixed in X: ciginstaller update`, or
`fixed in X, which you run`. Said once per report.

## One pipeline for the family

cig owns the ashtray, the redaction, the repro builder and the sending.
Another tool hands its report in and gets every route, the queue, the
consent and the privacy rules:

```
cig crash submit --origin cig-orchestrator --repo otmof-ops/cig-orchestrator --file report.json [--now]
```

The report is read as a report (its `kind`, `panic_message`/`title`,
`panic_location`/`location`, `version` and `active_run_id`; everything else
travels under `details`), carries the origin and the repository the tool
named, and joins the ashtray under cig's `reports` setting: `always` queues
it (or sends it now with `--now`), `ask` saves it and says how to send it,
`never` keeps it local. Everything in it is anonymised again on the way in:
every path and file name its shape, a field named for a path (`cwd`,
`path`, `file`) mapped whole, a command line reduced to the words cig knows.
What cig cannot tell from a plain word is the tool's to anonymise before it
hands the report in: a task's or a project's name, say, which
cig-orchestrator writes as `t1` and `<project>`.

## Never fails to send

In order: `gh` with your own credentials; `curl` with `CIG_GITHUB_TOKEN`
(`Content-Type: application/json`, the token in a config file, never on the
command line); a prefilled issue link printed for you; and the ashtray until
one of them works.

## Privacy, in one sentence

An allowlist, twice over: the body carries a fixed list of fields and no
other (the ashtray's own bookkeeping stays home), and within them no file
contents, no path component of yours, no environment values, no script
source, no hop commands, a command line reduced to the words cig knows;
`cig crash show <id>` prints exactly what travels, and nothing travels
without the one yes.
