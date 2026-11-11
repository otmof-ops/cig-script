# Hops: running other programs

<!-- SPDX-License-Identifier: CC-BY-4.0 -->

A **hop** (the lexicon's word for a process boundary) is the one place the
kernel performed nothing, saw nothing and can undo nothing on its own.
Inside the language you have fenced effects, checked arithmetic, a journal
and a plan; the moment a hop happens all of it is gone unless the hop is
designed to carry it. So the boundary is where the logic is hardest: nothing
crosses a hop unchecked, nothing fails silently, and a failure names the hop.

## One call shape

```cig
stick r = proc.run("git", ["status", "--porcelain"])     # {code, out, err, duration_ms, ...}
stick prs = proc.json("gh", ["pr", "list", "--json", "number,title"])   # a list of maps
stick files = proc.lines("git", ["ls-files"])             # a list of strings
stick rows = proc.csv("sqlite3", ["-csv", "-header", "db", "select * from t"])
stick conf = proc.kv("git", ["config", "-l"])             # KEY=value lines as a map (a leading `export` is dropped)
stick text = proc.text("date", ["+%F"])                   # stdout as one string
```

Every verb is sugar on `proc.run` and takes the same options. The parsing
verbs read **stdout only**, enforce the contract `ok: [0]` unless told
otherwise, and fail at the hop that produced a bad shape, quoting the first
200 bytes of what they got, so a wrong shape from step A never sails through
to die in step C.

| option | meaning |
|---|---|
| `cwd` | working directory for the child; it must exist (`E550` names it otherwise) |
| `env` | a map of variables added to the child's environment |
| `clean_env` | start from a small documented environment (`PATH`, `HOME`, `USER`, `LANG`, `TERM`, `TMPDIR`, `TZ` and the Windows equivalents) instead of inheriting |
| `timeout_ms` | wall-clock limit; ten minutes by default |
| `grace_ms` | after SIGTERM, how long before SIGKILL; 500 ms by default |
| `stdin` | text fed to the child's stdin |
| `touches` | globs, relative to the pack root, of the files the child may change (`"*"` the root's own files, `"**"` everything under it, `"src/**"` a tree, `"package.json"` one file). Each existing file under them is kept in the tin before the child runs, so a file it modifies or deletes comes back on rollback, byte for byte and hash-verified, with its mode and modification time, where it would otherwise be listed as irreversible. A glob written with a leading `?` (`"?yarn.lock"`) may match nothing without a word: a file the project does not have yet. A symbolic link comes back as a link with or without `touches`. Needs a `pack { }` (`E757`) |
| `box` | `true` runs the hop in the box: over a copy-on-write layer on the pack (overlayfs in a user namespace; Linux), applied only when the hop succeeds, dropped when it fails, so nothing of a failing hop ever reaches the pack. Where the box cannot be made the hop runs on the tin and the run says so (`E763`). `BURN.md`, "The box" |
| `box` | `true`: run in the box, a copy-on-write layer over the pack applied only when the hop succeeds (`BURN.md`, "The box"). A map asks for more: `{network: "on" / "off" / "registry", allow_hosts: [...] (a name or `*.suffix` on 443, or `host:port`), pidns: true, apply: "on_success" / "ask" / "never", contain: true, expect: [...], quiet: true}` ("Untrusted code"; "Rehearsing code you did not write") |
| `skip` | globs, relative to the pack root, the hop leaves out of the tin: a cache it rebuilds anyway (`"build/**"`, `".cache/"`; a directory's glob covers what is under it). What it changes there is neither kept nor journaled, so it cannot be undone, and `cig check` says so on the hop's line (`E310`). The `hop_skip` setting (`cig config hop_skip "**/.cache/**, *.log"`) adds globs every hop leaves out. In the box `skip` does nothing: the box's layer keeps every change. Since 1.18.0 |
| `touches_sealed` | globs among `touches` whose files are secrets: kept in the tin like the rest, and kept sealed (encrypted at rest; `BURN.md`, "Sealed"). Secret-shaped names (`.env`, `*.pem`, `id_rsa*`, ...) and a pack root's `.cigseal` are sealed without being named here |
| `ok` | the exit-code contract: an int or a list of ints; `null` for none. It wins over `check` whatever the order of the keys |
| `check` | `true` means the contract `[0]`, `false` none; ignored when `ok` is given |
| `encoding` | `utf-8` (strict, the default), `lossy`, `latin1`, `utf-16` |
| `reads` | `true` for a hop that only reads (a check, a lookup): it is not journaled as a change, so a run of such hops prints no burn line. The pack is still watched around it, and anything it does change there is journaled as usual. It takes no `box` or `touches` |
| `stream` | `true` shows the child's output as it arrives, its stdout on cig's stdout and its stderr on cig's stderr, while still capturing both whole: a long install read live. What is shown is display only; the value, the contract and every rule below are the same as without it |
| `header`, `sep` | for `proc.csv` |

Every child gets `NO_COLOR=1` (its output is captured, colour is noise) and
`CIG_RUN_ID` when there is one. The environment is otherwise inherited plus
what `env` adds, so a chain behaves like the shell it replaced; `clean_env`
is there for the chain that must behave the same on the laptop and in CI.

## The rules, each with a test in `tests/hops.rs`

**Exit codes are a contract, not a boolean.** `grep` says 1 for "no match",
`diff` says 1 for "different"; those are answers. `proc.run` alone passes
any code through in `.code`. The parsing verbs, and `proc.run` under `check`
or `ok`, refuse anything outside the contract with `E553`, naming the
command, the code, the contract and stderr's last line; the whole result is
the ashtray value.

```
error[E553 runtime]: proc.text: `kubectl apply -f deploy.yml` exited 1 (contract: 0); stderr: `Error from server (Forbidden)`
  = hint: if that code is an answer, not a failure, say so: {ok: [0, 1]}; read err in the ashtray value otherwise
```

**stdout and stderr are never merged.** Structured data comes from stdout;
stderr is captured separately and shown on failure. A tool that prints
progress to stdout breaks the shape, and `E556` says so: *expected JSON on
stdout, got `Downloading...⏎[1]`*.

**Timeouts always exist.** Every hop has one. `E554` says which hop stalled,
how much it had written to each stream, and quotes the start of it. The
timeout covers the hop's output, not only its child: a hop is over when its
stdout and stderr close, and a process the child left behind holding them
(`server &`) has until the deadline, after which the rest of the process
group is ended and `E554` names the stream. A program a hop starts in the
background closes or redirects its output, or ends before the hop does.

**Partial output is never mistaken for output.** A child killed by the
timeout or by a signal (`E555`, with the signal's number and name) produced
*nothing* as far as the next step is concerned. Its truncated output is in
the message, not in a value.

**Large output does not deadlock.** Both streams are read concurrently; a
child filling stderr while stdout is awaited is the classic hang, and the
test does it with 20 MB each way. `stdin` is written from its own thread
beside the readers, so a child that echoes 8 MB straight back does not hang
either.

**Encoding is explicit.** Output that is not valid UTF-8 is `E557`, with the
byte offset, never a silent replacement. `encoding: "lossy"` asks for
replacement characters; `"latin1"` for single-byte text; `"utf-16"` for a
Windows tool (a byte-order mark is honoured, little-endian assumed without).

**`sudo` cannot ask for a password here, and `E554` is what you see.** A child's stdin is `/dev/null` unless `stdin` is given, and the child is its own process group, not the terminal's foreground group; `sudo` reads the password from `/dev/tty`, and from a background group that read stops the process until the hop's timeout. Run `sudo -n` so it refuses instead of waiting, have the operator run `sudo -v` first, or configure the command in sudoers.

**No zombies, no orphans.** Every child runs in its own process group. On
timeout the group gets SIGTERM, a grace period, then SIGKILL, grandchildren
included; a child that ignores SIGTERM still ends. A Ctrl-C at the terminal
reaches cig, not a child in its own group, so SIGINT, SIGTERM and SIGHUP are
forwarded to every live child group before cig ends the default way.

**On Windows the process tree is a Job Object.** There is no process group
to signal, so every hop's child is put in a job with
`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`: what the child starts joins the job,
a timeout terminates the job (the grandchild holding the pipe included, so
`timeout_ms` ends the task when it says), and cig itself dying closes the
last handle and takes the tree with it. The child is created suspended,
put in the job, then resumed, so a process it starts in its first
instruction is in the job too; when Windows refuses the job, the hop says
so once and runs without one. One difference from Unix follows: what a hop
leaves running in the background ends with the hop.

**No shell by default.** Arguments are a list, never a string handed to `sh
-c`, so there is nothing to inject into. `proc.shell(command)` exists for the
honest cases, by name, with the warning in its name. `proc.which(name)`
resolves a bare name through PATH and a name with a slash against the
current directory, exactly as `proc.run` will; a directory given as the
command is refused as one (`E552`), not as a permissions problem. `cig check` warns
(`E308`) when a shell is smuggled through `proc.run("sh", ["-c", built])`
with a command string built at run time.

**The irreversible label survives the hop.** A hop is journaled as
irreversible and the plan says so; `proc.pipe` and `proc.shell` are one hop
each. Nothing launders it. A pipeline takes `touches` and `touches_sealed`
as a hop does and the pack is watched around the whole of it; `box` is
refused for a pipeline (`E400`), several processes being more than one box
holds, and `proc.shell("a | b", {box: true})` boxes the same thing as one
hop.

**Failure taxonomy with stable codes.** `E550` hop failed (other), `E551`
command not found, `E552` refused to start, `E553` exit outside the
contract, `E554` timed out, `E555` killed by a signal, `E556` bad shape on
stdout, `E557` bad encoding. A CI log tells you which without the message;
each has its kind of no where one fits.

**Retries are per-hop and idempotence-aware.** A chain retry
(`light(chain, {retries: n})`) first rolls back what the failed attempt
burned, so the retry runs against the state the step started from, and the
journal marks the undone ops so `cig unburn` skips them. A step whose failed
attempt ran a hop is **not** retried on its own, because the world may
already have changed; `{retry_irreversible: true}` says you know.

**Determinism holds across hops.** Same inputs, same plan. A child's output
may vary; the plan does not, because CigScript itself did nothing different.

## How a program is found

One resolver answers for `proc.run`, `proc.which`, every `proc.pipe` stage
and the shell behind `proc.shell`, so what `proc.which` names is what starts.

- A name with a path separator is a path, made absolute against the hop's
  `cwd` (`./gradlew` with `cwd: "api"` is `api/gradlew`), on every platform.
  Windows itself would resolve it against the parent's directory. On
  Windows a path without an extension is completed through `PATHEXT` before
  the bare file is tried, as `cmd` does, so `./gradlew` starts `gradlew.bat`
  rather than the shell script beside it.
- A bare name is searched on the `PATH` the child will see: the hop's own
  when its `env` sets one, else the parent's, else the platform's default
  search path. It is never found in the current directory (an empty `PATH`
  entry is skipped, not read as `.`), a relative `PATH` entry
  (`node_modules/.bin`) is read from the hop's `cwd`, and a name the
  resolver cannot find is `E551`: the platform is never asked to search
  again by rules of its own. On Unix the first file this process may
  execute wins, the question `execvp` asks, and `argv[0]` stays the name the
  script typed. On Windows each directory is tried with each `PATHEXT`
  extension in order (`.COM;.EXE;.BAT;.CMD` by default), so `npm` is
  `npm.cmd` and `node` is `node.exe`; a name that already ends in one of
  them is taken as spelled; the system directories are searched after
  `PATH`, as `CreateProcess` does, and a Store app's execution alias is a
  program. A `.bat` or `.cmd` found this way is started by
  its full path, which Rust runs through `cmd.exe` with safe argument
  quoting and refuses arguments it cannot quote; only a native `.exe` or
  `.com` gets the C runtime's own quoting.
- `bash` and `sh` on Windows mean a shell: System32's `bash.exe` (the WSL
  launcher) and `wsl.exe` are never chosen, whatever the order of `PATH`.
  With no other shell on `PATH` the hop fails as `E551` and the hint names
  Git for Windows.
- `proc.shell` runs its line with `sh -c` everywhere an `sh` is found, Git
  for Windows and MSYS2 included; without one it runs `cmd /d /s /c` and
  the line reaches cmd exactly as written, in one pair of quotes that `/s`
  strips. The plan line says which (`run sh -c …` or `run cmd /d /s /c …`),
  so a dry run is honest about the shell.

## Pipelines without a shell

```cig
stick errors = proc.pipe([["cat", "big.log"], ["grep", "ERROR"], ["sort", "-u"]])
exhale errors.out, errors.stages   # every stage's command and exit code
```

Stages are connected stdout to stdin, run in one process group under one
timeout, and every stage must exit 0 (the last one honours `ok`); a failure
names the stage (`E553: stage 2 (grep ERROR) exited 1`), and a missing
program names its stage (`E551`). `stdin` feeds the first stage. A stage
that died of SIGPIPE because the stage after it stopped reading (`yes | head
-1`) is not a failure: its entry in `.stages` says `closed_early: true` and
`signal: 13`, and whatever the consumer did next is what counts.

## Inside a pack: the hop's write set

With `pack { "./build" }` declared, the kernel lists the pack's files (mtime,
size and mode), directories (mode) and links (target, and the time and size
of the file a link leads to) before a hop and again after, and journals the
difference: files the child created are reversible (`hop created`; rollback
removes them), files it modified or deleted are irreversible with the
detail unless `touches` put their before-bytes in the tin, a directory it
deleted or replaced with a file is made again on rollback, and anything the
child changed in its working directory outside the pack is reported as
`E752` and never laundered. `BURN.md` has the rules. Three of them matter
most here:

- **Hop watching sees changes, not who made them.** Everything that
  changed in the pack between the two scans is the hop's as far as the
  journal knows: an editor's save, a file watcher's output, another
  program's write. So a rollback never destroys what it displaces: a file
  it removes or rewrites for a hop entry goes to the tin first, the report
  says so, and `cig unburn <id> --give-back` puts it back. A file the hop
  left that changed again after the hop ended is left alone (`--force`
  restores over it). And one run at a time holds a pack: a second `cig run`
  on the same or an overlapping pack is refused with `E759` while the
  first runs. A cig that a hop starts is the exception: it sees the run's
  id as `CIG_RUN_ID` (every hop does) and works inside the parent's pack,
  since the parent waits for it.
- **The scan is compact and parallel, with a memory guard** (`watch_cap`,
  five million entries). It reads the pack on every core into a sorted list
  of about 60 bytes an entry, and the scan after the hop reuses it for
  every directory whose modification time has not moved. Past the guard the
  kernel cannot tell a file the hop made from one it never reached, so it
  warns up front, journals nothing as created, and asks the disk before it
  calls a file deleted. A `.cigignore` at the pack root keeps a big tree out.
- **A link that leads out of the pack is a door.** What lies behind it is
  not walked and never journaled; a write through it is reported as
  outside the pack (`E752`); a rollback never removes a path whose real
  place is outside the pack. A link that stays inside is seen under its
  real path, and the link itself (dangling or not) comes back as the link
  it was.

A `touches` that matches no file in the pack at all is a warning before the
run (with the nearest glob that would match; one glob among several
matching nothing is ordinary, a lockfile the project may have), each glob
gets a count in the dry run (`touches scr/**: 0 files`), and after the run
the report names the glob that would have kept a file the hop changed
outside every glob. Several such files under one folder are one line, with
the smallest directory that covers them, the glob for it and a few of them
as examples; `cig runs <id>` lists every one.

### The stat cache (opt-in, `hop_statcache`)

A `touches` hop today stashes every file under its globs: it reads each
file, hashes it, and gives the bytes a name in the tin (`tin/objects` or a
chunked recipe). The dedup layer in the tin keeps storage small, but the
reads happen every hop. A 1 ms command over 10,000 files costs about a
second and a half in overhead, every time.

With `cig config hop_statcache on`, the tin keeps a per-pack index at
`~/.cigscript/tin/statcache/<pack hash>` of each touched file's
`(dev, inode, size, mtime_ns, ctime_ns, mode)` next to the sha256 the tin
kept it under. The next hop whose files all match the index reads none of
them: their tin objects are referred to by hash. The index saves the time
it was written at, so **a file whose mtime is within the filesystem's
timestamp granularity of the index's write is re-hashed anyway** (the
racy-git rule), and a same-second edit after the save is never missed.
`ctime` moves on every write a kernel makes, so a size-preserving edit
with a `touch -r` on the way out is still caught by the index's
`ctime_ns`.

**A restore is never trusted to the index.** `cig unburn` re-hashes the
before-bytes and the after-bytes exactly as before, so `E704` holds. The
index decides what needs reading, nothing else: a corrupt or stale index
can only lose the optimisation, never let a wrong restore through. The
index file is written temp-and-rename, so a crash in the middle keeps the
index that was there before.

`cig tin verify --statcache` re-hashes every file the index remembers and
drops the entries whose bytes no longer hash to what the index said: a
filesystem that lies about `mtime` is the tripwire the issue calls out, and
each mismatch is reported through the usual pipeline.

The setting is off by default. Turn it on when the `racy_edit_fuzz` test
has run to a history on your platform (its home is `tests/cli.rs`). The
index tracks only the files `touches` matched: sealed files keep the same
random-id policy, so no secret ever reaches the index. The cache is
Unix-only until Rust stabilises Windows's own file-identity accessors
(rust-lang/rust#63010).

### What makes an unchanged hop cheap

A hop over 10,000 files that nothing touched asks the file system one
question per file per scan and nothing else. Measured on a 16-core machine
(10,000 files of 10 KB in 100 folders, `hop_statcache on`, best of seven
chains of 32 hops, the cost of one hop taken from the difference of two
chain lengths so the process and the journal cancel; load average beside it):

| | before (1.20.0) | now |
|---|---|---|
| `hop_watch off` | 228 ms (load 4), 223 ms (load 10) | 91 ms (load 5), 101 ms (load 10) |
| `hop_watch on` | 145 ms (load 7), 157 ms (load 8) | 87 ms (load 6), 93 ms (load 10) |

The hop's `statx` calls fell from 60,775 to 20,777 and, with the watch, the
scans are skipped as before. Where the time went, and what removed it:

- **One `lstat` per file per scan.** The scan keeps each regular file's
  inode, device and ctime beside what it always kept, so the stat cache
  compares against the scan's own stat instead of making a second one, and
  the check after the hop reads the scan after instead of making two more.
  A kept scan (the watch's) is never used this way: only the scan this hop
  made is as fresh as its own stat.
- **The tin is listed, not asked.** Each hit's object must still be in the
  tin (a `cig tin gc` takes them); with 512 or more hits the directories
  they fall in are read (a few hundred `getdents`) instead of one `stat` per
  object. A name not found, a directory that cannot be read, or more names
  than eight per hit makes it ask that one hash alone, as before.
- **One pass proves nothing moved.** When the two scans agree on every stat
  tuple, inode and ctime included, and no file is inside the racy margin of
  the hop's start, no file was written to, and the per-file hash check after
  the hop is not run. Any difference runs the check as it always did.
- A run's `tin.pending` list is not written and synced again when the run
  already holds the same list; the cache index is read into its map in one
  pass; the cache is probed with the path's own bytes, not a copy.

The 100 ms of the issue is met on a quiet machine and sits at it when other
work loads all 16 cores. What is left of the 91 ms is the two scans (about 25 ms), the glob and seal
test of each file (10), reading the cache (8), listing the tin (16) and
building the hit list (14). None is a syscall per file any more. A regression
to one is what `an_unchanged_hop_asks_each_file_once_per_scan` catches: it
counts the file system questions a hop over 600 unchanged files asks
(`CIG_INTERNAL_HOP_STATS`), never milliseconds.

### Big files, chunked and restored on every core

A file of 16 MiB or more is cut into chunks on all cores at once. Each
8 MiB stretch is cut from its own start; where its chunks meet the ones
before it (a chunk boundary depends only on where its chunk began, so two
streams that share one share all after it) the lists are joined, and where
they never meet the true stream is walked on until they do. The chunks, the
recipe and the hashes are exactly what one pass from the start gives
(`parallel_hash_equals_serial_hash`). The whole-file hash, which no thread
can split, is fed in order from the same bytes the chunks were hashed from,
so a file that changes while it is read cannot give a recipe that disagrees
with its chunks.

A restore of a big recipe reads each chunk, checks it against its name, and
writes it at its own offset (`pwrite`) into a file made its full size first,
on up to eight threads, asking the kernel to start writing each range back
at once so the closing `fsync` finds most of it on its way. A chunk that is
missing or does not hash to its name refuses the restore and nothing is left
where the file was to go; the result is byte for byte what the serial loop
writes (`big_file_restore_is_exact`). Below 8 MiB, or on one core, the serial
loop runs as before.

### Change notifications (opt-in, `hop_watch`)

`cig config hop_watch on` keeps a change watch on the pack between the hops
of one process (a script with several hops, a chain, cigo's guard): inotify
on Linux, FSEvents on macOS, `ReadDirectoryChangesW` on Windows. A hop that
begins when the watch has seen nothing since the last hop ended uses the scan
that hop ended with instead of walking the tree again; with `hop_statcache`
on, the files the watch has not seen change since the cache was written are
not `stat`ed again; and a hop during which the watch saw nothing at all
skips the scan after it too. Nothing is reported to a script: this is not the
box's watch.

The watch only ever removes a walk. On any doubt (a queue overflow, a
directory made, moved or removed, a watch the kernel would not add, a link that
leads out of the tree, another pack or other `skip` globs) the hop does the
walk it always did and the watch arms again. What a notification cannot
see: a file written through a writable `mmap`, or through a hard link kept
outside the pack. That is why the setting is off by default. A cig that
starts and ends with one hop has nothing to reuse and pays only the arming.

## Compensations

`burn (s) { ... } unburn { ... }` is the undo for what no pack can reverse
(`git push`, an API call). The compensation runs from the journal alone, so
it may use only its state map `s`, modules and builtins (`E754` otherwise);
hops inside the block show as `compensated` in the plan. `LANGUAGE.md` has
the shape.

## The burn protocol

A tool that knows what it changed can say so. Every hop a real run starts
gets `CIG_JOURNAL`, a file it may append JSON lines to: `write`, `delete`
and `create` with the path, the hash of the bytes before and where a copy
of them was kept; `compensate` with the command that undoes the work. The
kernel checks every word it can (a path outside the pack is refused, a copy
that does not hash as claimed is not used, a `create` the scan saw before
is dropped) and journals what holds: a change the tool kept the bytes for
is reversible without `touches`, a compensation runs on rollback and
`cig unburn` like a block's. [PROTOCOL.md](PROTOCOL.md) is the contract,
with the lines in Python, Node and bash. A `cig run` started by a hop
speaks it itself, so a parent's rollback undoes the child through the
child's own journal.

## What a hop cannot do yet

Adapters for `git`, `gh`, `docker` and friends as subtools, and shape
assertions at the boundary, are the next seams of `HAMMER.md` (sections 4
and 7). Until then a hop that speaks no protocol is honest about being
opaque: the plan shows it, the label says irreversible, and the ghost
filesystem does not pretend to know what it would have written.
