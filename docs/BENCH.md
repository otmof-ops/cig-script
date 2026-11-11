# `cig bench`

One benchmark tool for the whole cig family. A repository describes its
cases in `bench/bench.toml`; `cig bench` reads that manifest, checks each
case's output first, samples it in batches, and reports median, p10 to
p90, CV and peak resident set in one of three formats.

This page covers steps 1 to 3 of the issue (#179): the manifest, the
harness, `--json` and `--markdown`; `--save` and `--against` (A and B
interleaved, with a significance test); and `--watch`. Later steps bring
the debugging flags (`--timings`, `--stats`, `--syscalls`, `--profile`),
the family runner (`--family`, `--with`, `--matrix`) and the reusable CI
workflow. A manifest written with fields reserved for those steps still
parses today: the reserved keys are accepted and ignored for now.

## The manifest

`bench/bench.toml` in the repository being measured:

```toml
format = 1                                       # the manifest's version; see below

[bench]
product = "cig"                                  # required
version = ["cig", "--version"]                   # optional, recorded in reports
home    = "isolated"                             # the default; also "shared"
build   = ["cargo", "build", "--release"]        # what --against <git ref> and --watch run first
binary  = "target/release/cig"                   # the product's binary once built
watch   = ["src/**", "Cargo.toml"]               # what --watch follows

[defaults]
samples = 9                                      # samples per case
per     = 5                                      # back-to-back runs that make one sample
warmup  = 1                                      # untimed invocations before the first sample
env     = { NO_COLOR = "1", CIG_DOCTOR = "0" }   # env for every sampled command

[[case]]
name    = "hello"                                # required
group   = "start-up"                             # optional, filterable with --group
run     = ["cig", "run", "fixtures/hello.cig"]   # required
stdin   = "fixtures/hello.in"                    # optional, a file path
expect  = { exit = 0, stdout = "fixtures/hello.out" }  # exit defaults to 0; stdout is a file
setup   = ["sh", "-c", "fixtures/make-tree.sh"]  # once per case, not timed
before  = ["cig", "run", "fixtures/hop.cig"]     # once per sample, not timed
budget  = "5 ms"                                 # reported; gating comes with step 6
measure = ["wall", "rss"]                        # wall and rss so far
watch   = ["src/interp/**"]                      # --watch: only these changes rerun this case
env     = { EXTRA = "value" }                    # optional per-case overrides
```

### Rules

- **Output is checked first.** A case whose stdout does not match the
  expected file, or whose exit code is not what `expect.exit` says, is
  `WRONG` and gets no time: a fast wrong answer is a bug, not a result.
- **Unknown keys refuse the manifest** with their line number. A typo in
  `sampels = 9` fails before any case runs.
- **The manifest has a version.** `format = 1` at the top (before any
  table) says which manifest this is. A manifest with no `format` is
  format 1; any other value is refused, naming it, instead of being
  guessed at. Saved baselines carry the same `format`, and `--json`
  output does too.
- **Reserved keys are accepted and ignored** (a case's `config`, `gate`
  and `score`) so a manifest written for a later step still parses
  today.
- **`build`, `binary` and `watch` are relative to the directory `cig
  bench` runs in** (the repository's root, normally), not to the manifest.
- **A Windows path goes in single quotes.** TOML reads a backslash in
  double quotes as an escape, so write `'C:\tools\cig.exe'` (a literal
  string), or use forward slashes.
- **Paths are relative to the manifest.** `stdin` and `expect.stdout`
  resolve against the folder that holds `bench.toml`; `run` and the
  rest resolve against that folder as the working directory.
- **The working directory is the manifest's folder.** Fixtures and
  scripts are reached as written.

### `home = "isolated"` (the default)

Each sample gets a fresh scratch folder, and the sampled command sees
it as `HOME`, `USERPROFILE`, `CIGSCRIPT_HOME` and `CIGO_HOME`. The folder
is removed after the sample. The folders live under one private folder
per run (mode 0700, a name nobody can guess, made fresh), so each
sample's tin and keys are this user's alone. A case cannot write to the
developer's real `~/.cigscript` or `~/.cig-orchestrator` while it is
measured.

`home = "shared"` passes the invoking shell's `HOME`, `USERPROFILE`,
`CIGSCRIPT_HOME` and `CIGO_HOME` through unchanged: useful when a case's
cost depends on an existing cache or key.

Either way the command starts from an empty environment. It gets the
manifest's `env`, the case's `env`, `PATH`, and only what a process needs
to start at all: the temp folder (`TMPDIR`, `TEMP`, `TMP`) and, on
Windows, `SystemRoot`, `windir`, `PATHEXT` and `COMSPEC`.

## Statistics

For each case, after one warm-up run (which must match `expect` for the
case to continue):

- **`samples`** sample batches (at least 1), each of **`per`** (at least 1) back-to-back
  invocations of `run`, wall-clocked end to end and divided by `per`:
  so a 3 ms start-up is not rounded away when the clock's resolution is
  milliseconds.
- **Median**, **p10**, **p90** and **coefficient of variation (CV)** of
  the per-sample times. A CV over 5 % is noisy, marked with `~` in the
  table and `noisy: true` in the JSON. Noise is reported, never gated
  on its own.
- **Peak resident set** from `wait4` on Unix (`ru_maxrss`, converted to
  bytes), the max across the batch. On Windows the column is `-`.

Every report starts with the machine's conditions: CPU, cores, OS,
kernel release, and (on Linux) the CPU governor.

## Running it

```
cig bench                           # every case in ./bench/bench.toml
cig bench --manifest path/to.toml   # another manifest
cig bench --only hello              # cases whose name contains the string
cig bench --group start-up          # cases in this group
cig bench --samples 3 --per 1       # override the defaults (quick smoke)
cig bench --markdown                # a Markdown table (for a PR comment)
cig --json bench                    # for a tool or an agent
cig bench --save main               # keep this run as the baseline "main"
cig bench --against main            # A/B against that baseline
cig bench --against origin/main     # A/B against a git ref, built in a worktree
cig bench --against v1.16.0         # A/B against a release (sha256 checked)
cig bench --against ./old/cig       # A/B against another binary
cig bench --watch                   # rerun when a watched file changes
```

### Exit codes

- `0` every case was timed.
- `1` any case was `WRONG` or could not be measured (`setup`, `before`
  or `run` failed before a sample landed).
- `3` the manifest or the usage is wrong (unknown key, unknown `format`,
  missing required field, bad flag), or `--against` could not make its
  other side (a release whose sha256 does not match, a ref that does not
  build).

A comparison that finds a regression still exits `0`: the verdict is in
the report. Failing a build on one is the gate, which comes with step 6.

### Example

With a one-case manifest beside a `fixtures/hello.cig` that prints
`hello`:

```
$ cig bench
cig bench: cig, cig 1.18.0
Intel(R) Core(TM) Ultra 7 255H, 16 cores, linux 6.17.0-1033-oem, governor powersave

case                           group       median ms  p10 to p90 ms      CV   RSS MB
hello                          start-up         20.8   20.5 to 21.4    1.8%     20.8
```

The same numbers in Markdown:

```
$ cig bench --markdown
**cig bench**: cig, cig 1.18.0 — Intel(R) Core(TM) Ultra 7 255H, 16 cores, ...

| case | group | median ms | p10 to p90 ms | CV | RSS MB |
|---|---|---:|---|---:|---:|
| `hello` | start-up | 20.8 | 20.5 to 21.4 | 1.8% | 20.8 |
```

Or as JSON:

```
$ cig --json bench
{
  "product": "cig",
  "cig_version": "1.18.0",
  "conditions": { "cpu": "...", "cores": 16, "os": "linux", ... },
  "cases": [
    {
      "name": "hello",
      "group": "start-up",
      "budget": null,
      "outcome": {
        "state": "timed",
        "median_ms": 20.79,
        "p10_ms": 20.54,
        "p90_ms": 21.44,
        "cv": 0.018,
        "noisy": false,
        "samples": 2,
        "rss_bytes": 20869120
      }
    }
  ]
}
```

## Saving and comparing: `--save` and `--against`

`--save NAME` keeps the run as a baseline: `.cig-bench/baselines/NAME.json`
in the directory `cig bench` runs in. A name is letters, digits, `-`, `_`
and `.`, not starting with a dot. The file holds `format`, the product, the
cig version, the machine's conditions, the binary that was measured (path
and sha256) and every sample of every case, so a later run compares with
the samples, not with a summary.

`--against WHAT` measures the other side as well and compares. `WHAT` is
one of these (a prefix `baseline:`, `release:`, `ref:` or `bin:` says which;
without one, a baseline of that name wins, then a path to a file, then a
version number, then a git ref):

| `--against` | the A side is | how |
|---|---|---|
| `main` | a saved baseline | its binary, if it is still there and its sha256 is unchanged; otherwise the saved samples (see below) |
| `v1.16.0` | a release | downloaded the way `cig versions add` fetches a release, its sha256 checked against the release's `.sha256` file **before anything runs**; a mismatch, or no checksum, refuses and runs nothing. cig products only |
| `origin/main` | a git ref | checked out in a temporary worktree (removed afterwards), built with `[bench].build`, and run from `[bench].binary` and the worktree's own copy of the manifest's folder |
| `./old/cig` | a binary | as it is |

Side B is this checkout: `[bench].binary` when the manifest names one,
otherwise the running cig for the product `cig`. On either side, a command
whose file name is the product's (`cig`, `bin/cigo`, `/usr/bin/cig`) in
`run`, `setup` or `before` is replaced by that side's binary, so the same
case measures both.

**The samples are interleaved.** A and B take their samples in turn, never
all of A and then all of B, and the order inside each pair alternates (A B,
B A, A B, ...). A CPU warming up or a neighbour starting lands on both
sides, so a drift cancels instead of showing as a regression; with a fixed
A-then-B order, B would always run one step later than A. Each side is set
up and output-checked first, and a side that fails its check ends the case
as `WRONG` (`side A` or `side B`): there are no timings to compare.

**The verdict is a Mann-Whitney U test** on the two sets of per-sample times:

- `faster` or `SLOWER` only when p < 0.01 **and** the median moved by more
  than 2%; otherwise `same`;
- the p-value is exact when no two timings tie and the sample counts are
  small (at most 400 pairs), otherwise the normal approximation with the
  tie correction;
- the effect is the rank-biserial correlation, -1 to 1: positive means B
  is the slower one in most (A, B) pairs, 1 means in every pair;
- the change is the median of B over the median of A, as a percentage
  (shown as `x2.34` from a factor of two up).

```
$ cig bench --against v1.16.0
cig bench: cig, A = cig 1.16.0 against B = this checkout (cig 1.20.0)
...
interleaved A/B, Mann-Whitney

case                           group            A ms       B ms    change        p  effect  verdict
hello                          start-up         21.4       20.9     -2.3%  p=0.412   -0.21  same
```

When a baseline's binary is gone or has changed, the saved samples are
compared with fresh ones, one after the other. The report says "not
interleaved" and the JSON has `"interleaved": false`: take that verdict
with the drift in mind. `--save` next to `--against` keeps this run's side
B as the new baseline.

With `--json` each case has `outcome.state` `compared` (or `wrong`,
`absent`) with `a` and `b` (median, p10, p90, CV, samples, RSS), `change`,
`p`, `effect`, `verdict` and `interleaved`; the top level has
`against.kind` (`baseline`, `release`, `ref`, `binary`) and `against.label`.

## Watching: `--watch`

`cig bench --watch` runs the cases, then waits, and reruns when a file
matching `[bench].watch` changes. The globs are relative to the directory
`cig bench` runs in (`.git`, `target` and `.cig-bench` are not walked). A
change is acted on once the files have held still for a moment, so a save
that writes twice reruns once.

- **On a change** the manifest's `build` runs first. A failed build shows
  its output, keeps the last numbers and measures nothing.
- **Then the cases the change could affect** rerun, cheapest first (the
  quickest median of the last build first; ones never measured before
  everything). A case with its own `watch` globs reruns only for changes
  that match them; one without reruns for every change.
- **What a build is compared with:** the `--against` side when there is
  one (interleaved, every build), otherwise the session's first build: a
  Mann-Whitney verdict against the first build's samples.

Three outputs:

- **A terminal:** a table redrawn in place, with the case that is running,
  base and now, the change, a sparkline of the last ten builds' medians
  and the verdict.
- **Plain** (stdout is not a terminal, or `--plain`): one line per case per
  build, no redraws and no escape codes, so it can be piped to a log or read
  by a person or a program in a second terminal:

  ```
  bench: watching src/** (12 cases, against this session's first build); q or Ctrl-C quits
  bench: build 1 hello: 20.8 ms (p10 20.5 to p90 21.4, CV 1.8%) first run
  bench: build 1 done, 12 cases
  bench: build 2: changed src/main.rs
  bench: build 2 hello: 41.3 ms (p10 40.9 to p90 42.0, CV 1.1%) vs 20.8 ms +98.6% SLOWER p<0.001
  bench: build 2 done, 12 cases
  ```
- **`--json`:** one JSON object per line: `start`, then per build `change`,
  `build` (when there is a build step; `ok` and `error`), `case` (`state`,
  the timings, and with a comparison `base_median_ms`, `change`, `p`,
  `effect`, `verdict`) and `cycle` when the build's cases are done. An agent
  can read the stream while it edits code.

**Keys** (only when stdin is a terminal): `q` quits (so does Ctrl-C), `r`
reruns everything now, space pauses and resumes the watching. With stdin
from a file or a pipe no key is read, and the end of that input does not
end the watch: it runs until it is killed, or its stdout closes.
`--save` does not combine with `--watch`.

## What is here now, and what is next

This page describes **steps 1 to 3** of #179. The upcoming steps, with the
same statistics and the same manifest, add:

- **step 4** `--timings`, `--stats` and `--syscalls` for one case;
- **step 5** cig's own suite in `bench/`;
- **step 6** the reusable CI workflow and the gates;
- **step 7** `--family`, `--with` and `--matrix`;
- **step 8** `--profile` and `--diff`.

## Passes (`cig-pass`)

The budgets for #186, measured one call at a time by the crate's harness:

```
cargo run --release -p cig-pass --example pass-bench            # the table
cargo run --release -p cig-pass --example pass-bench -- --json  # for a tool
cargo run --release -p cig-pass --example pass-bench -- --gate  # exit 1 on a miss
```

Each measure runs in batches of about 200 us, the rounds interleaved so a
CPU governor ramping up or down lands on all of them alike, and the output
is checked before it is timed (a fresh pass verifies, a changed file makes
it fail, a narrowing adds exactly one caveat, a lookup hits). "Warm" means
the files are older than the racy window, so verify trusts their identity.

Measured 2026-11-09 on an Intel Core Ultra 7 255H, powersave governor, 401
rounds, medians:

| measure | median | p90 | budget |
|---|---:|---:|---:|
| verify a local pass with 10 file inputs, warm | 29.3 us | 30.3 us | <= 30 us |
| narrow (attenuate) by one caveat | 0.37 us | 0.39 us | <= 5 us |
| find a hit among 100,000 stored passes | 5.9 us | 6.1 us | <= 50 us |
| decode a 400-byte pass | 0.06 us | 0.06 us | |
| read 10 files and record them, then issue | 23.8 us | 24.5 us | |
| read the same 10 files, no recording | 11.1 us | 11.6 us | |

The issue overhead budget (<= 5% on top of the work) is not met when the
work is only reading ten small files: recording and issuing add about 12.7
us, which is 114% of 11.1 us. On any work worth remembering (a `cig check`
of a large script is tens of milliseconds) the same 12.7 us is far under 1%.
