# Roadmap

What the next updates hold, in the order the first operations scripts asked for them
([issue #18](https://github.com/otmof-ops/cig-script/issues/18), from a Go project whose
operations are CigScript files, and [cig-orchestrator #12 and #13](https://github.com/otmof-ops/cig-orchestrator/issues)).
Each lands with a wall row and a test named after it, like everything else.

| # | Feature | Closes |
|---|---|---|
| 1 | `proc.run(cmd, args, {stream: true})`: the child's stdout and stderr teed to the parent's stderr as they arrive, the captured value unchanged | a ten-minute build silent until it ends (#18 item 10; cig-orchestrator #12) |
| 2 | `net.fetch(url, dest, {timeout_ms, max_bytes, headers})`: a burn that writes into the pack, journaled like `fs.write_text`, simulated in a dry run as a write of unknown size; HTTPS unless told otherwise | every download a `curl` hop the dry run cannot see (#18 item 1) |
| 3 | A raw multi-line string literal (`'''…'''`, no interpolation) | unit files and config as `"\n"`-joined lists (#18 item 3) |
| 4 | `proc.jsonl`, or `proc.json` with `{lines: true}`: one checked value per non-empty line | `go test -json`, `docker inspect` streams (#18 item 4) |
| 5 | `time.parse_iso(s)` and `time.parse_duration("30d")` | a certificate's remaining life only checkable by exit status (#18 item 5) |
| 6 | `script.path`, `script.dir` (pure); `run.id` landed in 1.2.0 as `cig.run_id()` (null in a dry run), beside `cig.version()`, `cig.os()`, `cig.arch()` | a script that cannot find its templates (#18 item 6) |
| 7 | `fs.write_text(path, text, {sensitive: true})`: the before-state's hash journaled, the op irreversible and said so in the plan | an overwritten private key kept as a plain snapshot copy (#18 item 9) |
| 8 | Lighting another file's chain in-process, so a `.cig` file run from a chain shares the journal: its burns, compensations and plan become the outer run's | nested `cig run` whose compensations the outer rollback cannot reach (cig-orchestrator #13) |
| 9 | The tin in chunks: content-defined chunking (FastCDC) of the files `touches` names, so a large file edited in one place stores the chunk that changed, not the file again | a 200 MB lockfile or asset stashed whole on every run (cig-orchestrator #18 step 5; 1.2.0 stashes whole files, one object per distinct content) |
| 10 | Git blob reuse: a file whose bytes git already holds (`git hash-object`) is referenced from the repository's object store instead of copied into the tin | the tin doubling what `.git/objects` keeps (cig-orchestrator #18 step 6) |
| 11 | The hash chain: each journal line carries the hash of the one before it, `cig runs <id> --verify` walks it | a journal edited after the fact passing as the record (cig-orchestrator #18 step 7) |
| 12 | The burn protocol: a foreign tool tells cig what it changed (`$CIG_JOURNAL`), so a hop that speaks it is reversible without `touches`; the tin's objects stay **pressed** (written once, named by their hash, a second write refused) | the kernel trusting a hop's word about what it touched (cig-orchestrator #18 steps 9 and 10) |
| 13 | **The parcel**: one run's record, tin objects and compensations exported as a single file and imported on another machine for `cig unburn` there | undoing a deploy from the machine that did not run it (cig-orchestrator #18 step 11) |
| 14 | **The box**: a hop run over a copy-on-write overlay of the pack (overlayfs in a user namespace on Linux, a reflink clone on macOS and Windows), so a failing hop's changes never land | restoring what could have been left unwritten (cig-orchestrator #18's "box") |
| 15 | **Sealed** objects: the tin encrypted at rest with a key from the OS keyring, before the tin is on by default for real projects | a tin that holds old `.env` files and private keys in the clear (cig-orchestrator #18's "sealed") |

One word per idea, settled in LEXICON.md before any of rows 12 to 15 lands:
**the box** is the overlay, **the parcel** is the export, **sealed** is
encrypted at rest, **pressed** is write-once. Chunking (row 9) comes first:
it makes `touches: ["**"]` cheap enough to be cig-orchestrator's default;
sealing (row 15) comes before the tin is on by default for real projects.

Done in 1.6.0: rows 12 (the burn protocol, #89), 13 (the parcel, #86) and
14 (the box, #85; overlayfs on Linux, the reflink clone for macOS and
Windows still to come, the hop falling back to the tin there and saying
so), with hop watching that scales (#84). Every row of this table has
landed; the tin is complete end to end (#92).

Done in 1.5.0: rows 9 (the tin in chunks, #82), 10 (git blob reuse, #87),
11 (the hash chain, #88) and 15 (sealed, #83), each with its tests and wall
rows; the words settled in LEXICON.md as planned.

Done in 1.7.0: the tool reports its own bugs (#101): tripwires after the work, the
ashtray, a verified anonymised repro from the journal, one issue per bug with live stats,
the fix announced after `ciginstaller update`, and one pipeline for the family.

Done in 1.2.0 instead of waiting: the tin for whole files (`touches`, `cig tin`, `.cigignore`;
cig-orchestrator #18 steps 1 to 4 and 8), the pin, the humidor and the trial (#22), crash
reports that name the function and put the run back (#23, #24), and the `cig.*` namespace.

Done in 1.1.2 instead of waiting: `HOPS.md` says what `sudo` does from a hop and
what to do about it (#18 item 2); `LANGUAGE.md` says a keyword cannot be a
declared name and names the error (#18 item 7); `DESIGN.md` says heavy data work
belongs in a program the script hops to (#18 item 8).

The longer list that was already here is in `docs/DESIGN.md` under "What comes next".
