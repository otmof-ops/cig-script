# `cig bench`

One benchmark tool for the whole cig family. A repository describes its
cases in `bench/bench.toml`; `cig bench` reads that manifest, checks each
case's output first, samples it in batches, and reports median, p10 to
p90, CV and peak resident set in one of three formats.

This page covers step 1 of the issue (#179): the manifest, the harness,
`--json` and `--markdown`. Later steps bring `--save` and `--against` (A
and B interleaved, with a significance test), `--watch`, the debugging
flags (`--timings`, `--stats`, `--syscalls`, `--profile`), the family
runner (`--family`, `--with`, `--matrix`) and the reusable CI workflow.
A manifest written with fields reserved for those steps still parses
today: the reserved keys are accepted and ignored for now.

## The manifest

`bench/bench.toml` in the repository being measured:

```toml
[bench]
product = "cig"                                  # required
version = ["cig", "--version"]                   # optional, recorded in reports
home    = "isolated"                             # the default; also "shared"

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
budget  = "5 ms"                                 # reported; gating comes in step 2
measure = ["wall", "rss"]                        # step 1 honours wall and rss
env     = { EXTRA = "value" }                    # optional per-case overrides
```

### Rules

- **Output is checked first.** A case whose stdout does not match the
  expected file, or whose exit code is not what `expect.exit` says, is
  `WRONG` and gets no time: a fast wrong answer is a bug, not a result.
- **Unknown keys refuse the manifest** with their line number. A typo in
  `sampels = 9` fails before any case runs.
- **Reserved keys are accepted and ignored** (`[bench].build`,
  `[bench].watch`, a case's `config`, `gate` and `score`) so a manifest
  written for a later step still parses today.
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
```

### Exit codes

- `0` every case was timed.
- `1` any case was `WRONG` or could not be measured (`setup`, `before`
  or `run` failed before a sample landed).
- `3` the manifest or the usage is wrong (unknown key, missing
  required field, bad flag).

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

## What is here now, and what is next

This page describes **step 1** of #179. Numbers from `cig bench` can be
compared across runs by eye or by script today. The upcoming steps,
with the same statistics and the same manifest, add:

- **step 2** `cig bench --save <name>` saves the current numbers under
  the cig home; `cig bench --against <name>` reruns A and B interleaved
  (never all of A then all of B) and gives a verdict with a Mann-Whitney
  U test;
- **step 3** `--watch` rebuilds on a source change and updates a live
  table;
- **step 4** `--timings`, `--stats` and `--syscalls` for one case;
- **step 5** cig's own suite in `bench/`;
- **step 6** the reusable CI workflow;
- **step 7** `--family`, `--with` and `--matrix`;
- **step 8** `--profile` and `--diff`.
