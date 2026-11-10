# Doctor: automatic diagnosis

<!-- SPDX-License-Identifier: CC-BY-4.0 -->

When an error code fires, doctor fires with it. Under the diagnostic (the code,
the caret, the hint) you get a diagnosis: what doctor thinks happened, what it
checked to reach that conclusion, and what to type next. Behind it sits the
error registry (`errors/registry.toml`, rendered in [ERRORS.md](ERRORS.md)):
a code is a node with ranked causes, a read-only **probe** for each, and a
remedy for each.

```
error[E551 runtime]: proc.run: could not start `kubectl`: no such file or directory
  --> deploy.cig:14:3
  = hint: is it installed and on PATH? proc.which(name) tells you
  = explain: cig explain E551
  = doctor: I think the program is not on PATH for this run (can't see any ciggies bro), because I checked each directory on PATH, looking for the program named in the message and found no `kubectl` in the 7 directories on PATH; a `kubectl` exists at /home/me/bin/kubectl, and that directory is not on PATH for this run.
  = fix: proc.which("name") to check; give the full path, or fix PATH in the environment the run inherits
```

## The two rules

**Doctor diagnoses; it never fixes.** A remedy is text. Anything that changes
the world still goes through a fix that asks before each change (`cig __doctor --fix`, which `ciginstaller doctor` runs for cig's own state),
and a fix that needs a burn shows up as a plan.

**Doctor only claims what it verified.** The one sentence it is allowed:
*I think X, because I checked Y and found Z.* A cause no probe confirmed is
listed under *if not*, with the check that would confirm it. When nothing
confirms, doctor says so in one line and lists what it looked at under
*maybe*. When no probe could run at all, doctor says nothing.

## Output shape

Layered, the same for every code, so it is scanned once and learned:

| line | what it carries |
|---|---|
| `= doctor: I think …, because I checked … and found …` | the verdict: the confirmed cause, its kind of no in brackets, and the evidence |
| `= fix:` | the remedy for that cause, ready to type or paste |
| `= if not:` | the next ranked causes still standing, each with the one check that would confirm it |
| `= doctor: no known cause matched; cig explain Ennn for the general case` | nothing confirmed |
| `= maybe:` | after that line: every cause still standing, with what was seen |

The kind of no is the lexicon's ([LEXICON.md](LEXICON.md#the-four-kinds-of-no)):
*can't see any ciggies bro* (wrong address), *never heard of you* (401),
*these are MY ciggies* (403, a lock, permission denied), *don't have any over
here* (empty). In plain mode the manual's name stands in its place.

## Firing model

- **One hook.** Every diagnostic a command reports passes through one emit
  point; doctor subscribes there. No command remembers to call it.
- **Read-only, always.** Probes stat paths, read mode bits, read PATH, read the
  journal and the run records. They never write, never re-run the failing
  command, never touch the network. `tests/cli.rs` re-runs a diagnosis against
  a tree it then compares byte for byte, with the state directory read-only.
- **Budget.** Probes stop after 80 ms; the rest are reported as not run. An
  error never becomes slower than the run that produced it.
- **Offline.** Nothing here needs the internet. Crash reports are a separate,
  opt-in pipeline.
- **Off switch.** `--no-doctor` for one command, `CIG_DOCTOR=0` for a shell or
  a CI job. With doctor off the output is byte-identical to the bare
  diagnostic. In `--json` the diagnosis is a field (`diagnosis`), never prose;
  its shape is in [diagnostic.schema.json](diagnostic.schema.json).
- **Later, elsewhere.** `cig doctor <run>` diagnoses a stored run's error
  again from its record, so what CI saw can be looked at on a laptop. The run
  record keeps the diagnostic in its `--json` form for this.

## Probes doctor can run today

| probe | reads | confirms |
|---|---|---|
| `path-exists` | stat of the path in the message | the path is absent and its parent is there |
| `parent-exists` | stat of the parent | the directory itself is missing |
| `path-permissions` | mode bits and owner of the nearest existing ancestor | a permission denial, with who owns what |
| `path-is-dir` | file, directory or link | a directory where a file was expected, or the reverse |
| `on-path` | every directory on PATH, then the usual places off it | a program that is not on PATH, and where a copy of it lives |
| `state-dir-writable` | mode bits and owner of `~/.cigscript` | the state directory cannot be written |
| `similar-names` | the names in scope, by edit distance | a typo, with the closest name |
| `brace-balance` | `{` and `}` per line | the line whose block never closes |
| `int-magnitude` | the literal against the 64-bit range | a literal past the largest int |
| `env-var` | `CIG_MAX_ALLOC`, `CIG_MAX_STEPS` | context for a ceiling (never a verdict on its own) |
| `loop-condition` | the `while` on the line the step budget ran out | a condition that is literally `true`, so only a break can end the loop |
| `plan-order` | the dry-run plan | the earlier simulated delete or move that took the path a step then read |
| `chain-report` | the step, its label, the inner error | which step raised, and where |
| `network-tools` | `gh` and `curl` on PATH | no transport for an update |
| `run-list`, `run-record`, `pid-alive` | the run records | no such run; an interrupted run |
| `snapshot-integrity` | every snapshot a journal needs, and its hash | a rollback that would be refused |

Every other probe named in the registry is declared and not yet implemented;
a cause that names one appears under *if not* as a manual check. Adding a
cause is a registry edit: `why`, `probe`, `remedy`, and optionally `no`.
`tests/registry.rs` refuses an unknown probe name, and
`tests/corpus/*.cig` pins doctor's words for each cause that has a probe.

## Plain mode

`--plain` (or `CIG_PLAIN=1`) is a subtraction, not a translation: the same
codes, spans, hints, verdicts and evidence, without the banner and with the
manual's name where the hallway phrase was. Schools, CI logs and procurement
get the same tool with the jokes off. Golden-tested against the themed output.
