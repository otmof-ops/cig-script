# Changelog

All notable changes to CigScript. The format follows Keep a Changelog; the
project follows semantic versioning.

## 1.18.0 — 2026-11-08

What a box was asked for and is made of, the runs that went and a
budget for the rest, a program's own stretch, hops that skip their
caches, and the burn tests for compiled programs.

- **A boxed hop says its box** (#106). The plan and `cig runs <id>` said
  `(boxed)`; they now say what the box was asked for,
  `(boxed: network registry, allow registry.npmjs.org, contain)`: the
  network always, the rest where it is not the default. The terms are
  recorded with the hop and left out when empty, so older cigs read the
  journal as before; a bug report still says only `(boxed)`.
- **`cig doctor` says what the box is made of** (#106). A `box` block
  lists the user, mount, network and PID namespaces and overlayfs, each
  tried on its own, so a box that cannot be made says which part is
  missing; a refused user namespace names the likely cause (AppArmor's
  `kernel.apparmor_restrict_unprivileged_userns` on Ubuntu, or a
  container's seccomp). Off Linux it says the box is Linux's.
- **A hop skips what it rebuilds anyway** (#148). `skip: ["build/**",
  ".cache/"]` on a hop leaves those paths out of the tin, the way a
  `.cigignore` line does: what the hop changes there is neither kept nor
  journaled, so it cannot be undone, and the hop does not pay to keep a
  cache. `cig check` says so on the hop's line (`E310`), or that `skip` does
  nothing in the box, whose layer keeps every change. The `hop_skip`
  setting (`cig config hop_skip "**/.cache/**, *.log"`) adds globs every
  hop leaves out. The plan shows `(skip …)`.
- **`cig runs --pruned`** (#149) lists the runs that went on purpose, from
  the ledger, newest first: when, how (`pruned`, `forgotten` by
  `--forget`, or `budget`), and the status each last had, so a missing run
  is explained.
- **A disk budget for the history, off by default** (#149). `history_budget`
  (a size like `500MB` or `2GB`, base 1024; `off`, the default, so nothing
  is pruned on upgrade) caps the history as it grows: the run records and
  the tin. Past it, after a run and on `cig runs --prune`, the oldest runs
  go and the tin is collected; a tin object counts as freed only when the
  last run that refers to it goes, so no more is pruned than must be.
  Never pruned: a pinned run, a run with work left (running, interrupted,
  an unburn that failed), a run holding a box layer that waits for
  approval, a run younger than `history_keep` (30 days by default), and the
  newest run; the budget can be exceeded for them, and `cig doctor` says
  so. Each prune is listed by `cig runs --pruned` as `budget`. `cig doctor`
  shows the history's size and, with the budget off, suggests one (about
  5% of the free space, from 1 GB to 20 GB) without changing anything.
- **Programs put back a stretch of their own runs** (#149). A compiled
  program takes `--unburn-last N`, `--unburn-since AGE` and
  `--unburn-to <run>`, as `cig unburn` does for any run, and keeps to its
  own runs: another program's, and `cig run`'s, are left alone.
- **The burn tests run against another cig** (otmof-ops/cig-compile#6). The
  pack, hop, wall and CLI suites start cig through `CIG_TEST_BIN` when it
  is set: cig-compile runs them with a cig that builds each script and runs
  the program, so a compiled program's burns are held to the same tests.

## 1.17.0 — 2026-11-08

Stretches of history, a faster interpreter, and more of the tin's guard.

- **A stretch of history back in one command** (#149). `cig unburn --last N`
  puts back the newest N runs that changed something, `--since 2h` every
  run started in the last two hours (`90s`, `30m`, `3d`, `1w`), and
  `--to <id>` every run after that one, which stays; `--pack DIR` keeps to
  the runs that worked there. One plan first (the runs, the ops, the files,
  what is irreversible), a question at a terminal (`-y` skips it), and
  `--dry-run` stops at the plan. Then each run is put back as
  `cig unburn <id>` would, newest first. The first run refused stops the
  stretch, the runs after it undone and the ones before it untouched, and
  `cig unburn --continue` takes up the same stretch once that is resolved:
  no run is undone twice. A child run goes back through its parent, and a
  stretch will not start under a run that is still running.
- **Pins** (#149). `cig runs --pin <id>` keeps a run whatever `keep_runs`,
  `keep_days` and `cig runs --prune` say: the run before a migration or a
  release. `cig runs` marks it `(pinned)` and `--json` says
  `"pinned": true`; `--unpin` lets it go. The pin is a file in the run's
  folder, so the record keeps its format.
- **A faster interpreter** (#146, the first step). Scopes keep their names
  in a short list compared directly, and hash them with a fast hasher only
  past eight (the globals, the prelude), instead of SipHash on every read.
  `for i in a..b` counts instead of building the list of every number and
  copying it. On cig-compile's benchmark set this is 10% to 40% faster
  (`loop` 0.62×, `fib` 0.73×, `closures` 0.59×, `fannkuch` 0.65×, release
  builds on glibc), every output the same. `tests/steps.rs` holds the step
  budget line for line: four scripts, every `--max-steps` from 1 until they
  finish, 640 budgets, the same line and column as 1.16.1.
- **A range too wide is refused, never wrapped.** The width of `a..b` is
  computed without overflow, so `i64::MIN..i64::MAX` is E512 instead of
  wrapping (a release build) or a panic (a debug one).
- **A hop's files are staged on every core** (#148). Before a hop, every
  file it may change is copied into the tin and hashed; that now runs on up
  to eight threads, the results in the files' order, so the journal is as
  before. A hop over 3,020 files on tmpfs: 277 ms to 234 ms.
- **The tin's key on a descriptor** (#105). `cig --seal-key-fd N` reads
  the key (the 64 hex characters a key file holds) from open descriptor
  `N` at start-up, closes it, and holds it in memory only: in CI the key
  sits in no file and no environment variable, no key file is made, and
  nothing cig starts inherits the descriptor (`0` is read and then pointed
  at `/dev/null`). Programs take the same flag. `cig doctor` names the
  keeper and skips the backup reminder. Unix only; on Windows
  `CIG_SEAL_KEY_FILE` stays the headless way.
- **The ledger is a tripwire** (#105, L6). A journal the ledger catches cut
  short or swapped (E709) is also a tripwire report: under `reports`, with
  no path in it, and none from a dry run.
- **The key, wiped and kept out of swap** (#105, L5). The text a key was
  read from (a key file's hex, the keyring's answer) is zeroed before it is
  let go, and the page holding the cached key is `mlock`ed where the system
  allows it. A test reads every file cig writes, and everything it prints,
  for the key and finds none.
- **Spotlight leaves the state directory out** (#105, L2): on macOS it
  carries `.metadata_never_index`.

## 1.16.1 — 2026-11-07

A fix for the box's registry network.

- **A boxed hop that connects at once reaches the forwarder.** In the
  box's registry network the forwarder (a fresh process: a compiled
  program reads its own payload first) bound its port only once it had
  started, and the hop was started beside it, so a hop that connected at
  once could be refused (curl's exit 7). The box now makes the listener
  before the hop exists and hands it to the forwarder, so an early
  connection waits in its backlog. This, not the proxy's refusal order
  fixed in 1.16.0, is what made cig-orchestrator's agent box go on now and
  then in CI where it should have waited.

## 1.16.0 — 2026-11-07

Retention and a wider self-check for the tin, a fix for the box's network,
and a code cigcompile asked for.

- **A refused connection is always seen** (the box's registry network).
  The proxy answered a refused CONNECT first and recorded it after, on its
  own thread; a client that exits on its answer could end the hop before
  the record was written, so E771 was missing and a run that should have
  waited went on (cig-orchestrator's agent box, now and then in CI). Each
  refusal is recorded before it is answered.
- **E1009: a script whose name is not printable** (cigcompile). cig refuses
  to run a program whose record holds an invisible character (E1050, since
  1.15.1); cigcompile now has a code to refuse to make one.
- **`keep_runs` and `keep_days`** (#105): prune run records after every run,
  as `cig runs --prune` does, each in the ledger: the newest N kept, or the
  records older than N days dropped. 0, the default, keeps every run, and
  neither drops a run with work left (running, interrupted, or an unburn
  that failed).
- **SECURITY.md says what protects the tin** (#105): from the scripts cig
  runs, from another user, and from someone with a copy, with the honest
  limits. A test proves cig is not dumpable while it holds the key: a hop
  sees cig's `/proc` entries owned by root, and `CIG_ALLOW_DEBUG=1` opens
  it again.
- **`verify_sample`** (#105): after a run that burned, objects drawn at
  random from the whole tin are checked against their names, beside the
  run's own, so an object an earlier run left that has rotted since is a
  tripwire on the next burn. 3 by default; `0` checks nothing. A random
  bucket, then a random object in it: a big tin costs no more to sample.

## 1.15.1 — 2026-11-07

A fix release for compiled programs and the journal, from review of 1.15.0.
Programs refuse rather than run as `cig` when they are damaged, unreadable
or raised (E1050, E1055, E1054), a run that burns nothing never asks for
the seal key, and two hops over one file roll back.

- **A program never runs with raised rights** (#134). Installed setuid,
  setgid or with file capabilities, a program ran its caller's `PATH` and
  kept its runs and keys where its caller's `CIGSCRIPT_HOME` and `HOME` said,
  with the rights of the file's owner: a caller's own `date` ran as root. It
  now refuses (E1054) before it reads any of that; a sudo rule is the way to
  let others run one as its owner.
- **A damaged program is never `cig`** (#135). A program cut short, or with
  bytes written after its trailer, ran as a full `cig`. A program now knows
  it is one without its trailer (its payload starts where its runtime's
  image ends, read from its ELF, Mach-O, universal or PE headers), and such a
  program is E1050. Zero padding after the trailer is fine, and so is a PE
  certificate table: a Windows program signed after it was built still runs.
- **A program that cannot read itself refuses** (#136). Installed
  execute-only and run by someone else, it could not find its script and ran
  as `cig`, commands and all. It now refuses (E1055), unless it is named as
  cig is (`cig`, `cig-runtime`, a release asset).
- **What a program says about itself shows as it is** (#137). Its `file`,
  `compiler` and `cig` refuse Unicode's invisible characters as well as
  control characters: the format characters, the bidi controls among them
  (U+202E made `tidy\u{202e}gpj.cig` show as `tidygic.jpg`), and the line and
  paragraph separators. `program::printable` is the rule, for cigcompile too.
- **A run that burns nothing never asks for the seal key** (#129). The
  journal writes its first line, and asks for the key, with its first entry,
  not when it is made: `cig run` of a script that burns nothing went from
  12 ms to 4 ms with the keyring (the ask was most of it). Such a run's
  record keeps no head and the ledger no line; a journal under a finished,
  headless record that holds lines after all is E760, and `cig runs <id>
  --verify` fails a journal it cannot read instead of passing it silently.
- **Two hops that change one file roll back to the start** (#125). The older
  hop's entry was checked against what the newer hop left, so `cig unburn`
  put the newer one back and then refused the older one ("changed after the
  hop ran"), and a retry was refused with E704 against the run's final
  state. An entry is now checked against what the newer entry's restore put
  there, in the same pass and on a retry.
- **Reports leave the machine over curl again** (#126). GitHub refuses an
  issue search without `is:issue` (422), so the curl route never filed
  anything; the search now says `is:issue`.
- **A sender that died no longer blocks every later send** (#127). The send
  lock records its holder's pid and start time, and a lock whose holder is
  gone is taken over, not waited on for ten minutes.
- **An E704 refusal on a long history takes as long as the unburn would**
  (#128). Naming the later run that changed each path read every later
  journal again for every path: 30-60 s over 300 runs. Each later journal is
  now read once, and the refusal names the first 20 paths and counts the
  rest (`--dry-run` lists them all).
- **E1008: a runtime that would tie programs to this machine's libc**
  (otmof-ops/cig-compile#27). cigcompile refuses to make a Linux program from
  a dynamically linked cig when it cannot use cig's static release instead;
  `cig explain E1008` says why and what to type next.
- **The repository is otmof-ops/cig-script**, named like the rest of the cig
  family (cig-orchestrator, cig-compile, cig-os). It was otmof-ops/CigScript;
  GitHub redirects the old name for the web, git, the API and release
  downloads, so an installed cig keeps updating. `cig update`, reports and
  the install scripts now name the new one.

## 1.15.0 — 2026-11-07

Compiled programs, the first stage of cigcompile (otmof-ops/cigcompile#2).
See [PROGRAMS.md](docs/PROGRAMS.md).

- **Compiled programs.** A script appended to the `cig` of the same version
  runs as one executable, through the same run as `cig run`: same kernel,
  journal, plan and rollback. It takes the run's flags before its own
  arguments (`--dry-run`, `--no-rollback`, `--max-steps`, `--light`,
  `--chains`, `--unburn`, `--explain`, `--cig-program`), `--` hands the rest
  to the script, and a mistyped flag is refused rather than passed on. It
  does not check itself at start, and it has no pin and no trial.
- **`cig runs` and `unburn` know them.** A program's run is listed by the
  program's name and shows its file, sha256, compiler and binary.
  `--unburn` puts back the program's own runs only (`E1053`).
- **The self-call marker.** `CIG_INTERNAL_SELF` marks cig's calls to itself.
  A program's binary answers only the ashtray's `crash flush --quiet` and
  the box forwarder's `internal-forward` as `cig`, never in a setuid
  process, and removes the marker from its environment. It never updates
  itself and never runs a repro.
- **Refusals.** A damaged program refuses to run (`E1050`) and so does one
  compiled for another cig (`E1051`). The sha256s are integrity, not
  authenticity: programs are unsigned until cigcompile#10.
- **`cigscript::cli::entry`.** The `cli` module is now in the library, and
  `src/main.rs` calls it. Nothing changes for `cig`.
- **Four-digit codes.** Codes compare as numbers, and `E1000` to `E1799`
  are the new families from cigcompile#11 (build, strength, types, layers,
  ownership, foreign, carton). `E1000` to `E1007` are cigcompile's and
  `E1050` to `E1053` are the programs'.
- **The runtime's version in its bytes.** Every cig carries
  `cig-runtime-version=<version>`, so cigcompile can tell which cig a binary
  is without running one built for another machine.
- **`no_rollback` in run records.** A run started with `--no-rollback`
  says so in its record.

## 1.14.0 — 2026-11-06

What cig-orchestrator needs to close the holes found testing it
(cig-orchestrator #80).

- **A contained hop's home writes, applied by name.** The home layer of a
  contained hop was never applied. `proc.apply_layer(layer, {home:
  ["~/.cache/**"]})` now also places the home writes the globs name (a
  tool's cache, its own config), so a guard can box an agent's command,
  keep its writes to startup files and the guard's own files from landing,
  and still let the rest through. They are checked against a digest of the
  home layer taken when it was judged (a home layer changed since applies
  nothing, `E782`), placed outside the pack as they are, and named in the
  result's `home`.
- **`cig tin keep` and `cig tin give`.** Another tool keeps a sealed copy
  of a file in the tin under its own label (the last list it verified, the
  entry it wrote in someone else's file) and reads it back. Each keep is a
  line in the ledger, so a copy deleted from the tin is missed (`give`
  exits 4 and says when it was kept), never taken for nothing kept (exit
  1); `cig tin gc` leaves kept copies alone.
- **A hop that only reads: `reads: true`.** Not journaled as a change, so
  a run of only such hops (a check, a lookup) prints no burn line; the pack
  is still watched around it, and anything it does change there is
  journaled as usual.
- **Codes for #80:** `E588` the blacklist file is gone; `E897` the agent
  went for the guard; `E898` a rehearsal that could not see. `E897` and
  `E898` were reserved for guard_scope's level 5 since 1.13.0 and never
  reported for it; level 5 takes codes of its own when it is built.

## 1.13.2 — 2026-11-06

- **Every run starts in milliseconds again, however long the history.**
  Before each run cig looks up the script's last good version, and that
  read every run's journal in full: on a machine with about a thousand
  runs, each start took two seconds (fifteen on a debug build). The lookup
  now reads each run's record alone, which holds everything it needs; a
  trivial run on that machine starts in 0.05 s.
- **No writes reported that the hop did not make.** A link inside the pack
  that leads outside it is watched for writes through it, shallowly. A
  link to the pack's own ancestors (a Wine prefix's `z:` leads to `/`) made
  cig watch the whole machine, and the system's own writes (`/var/log`,
  `/proc`) and the shallow walk's cap read as the hop's: hundreds of paths
  under `/etc` reported as `E752` for a hop that wrote nothing. Such a link
  is no longer watched, nor one into `/proc`, `/sys`, `/dev` or `/run`, and
  past the shallow walk's cap only what both walks saw is compared.

## 1.13.1 — 2026-11-06

- **A box with no pack is never a silent no-op.** In a script that
  declares no `pack { }`, a hop asked to run in the box ran on the real
  disk without a word, and one asked to be isolated (`box: {network:
  "off"}` or `"registry"`, `contain: true`) ran unprotected: no layer, no
  containment, no warning, against the box's own rule that an isolated hop
  is refused rather than run without it. An isolated hop with no pack is
  now refused before it runs (`E777`, naming the missing pack), and a plain
  `box: true` says it runs on the tin (`E763`), as it does where the kernel
  refuses the box. Scripts that declare a pack, cig-orchestrator among
  them, are unaffected.

## 1.13.0 — 2026-11-06

- **A hop's output, live: `stream: true`.** `proc.run` and `proc.shell`
  take `stream: true` to show the child's output as it arrives (its stdout
  on cig's stdout, its stderr on cig's stderr) while still capturing both
  whole, so a long install that runs through cig reads the way it does in a
  terminal instead of arriving all at once when it ends. What is shown is
  display only: the value, the exit-code contract, the timeout and every
  other rule in HOPS.md are the same as without it. cig-orchestrator's guard
  uses it for the package-manager commands it runs (cig-orchestrator #57).
- **`box: {quiet: true}`** for a script that judges a box's layer itself:
  it reads `layer`, `outside`, `refused` and `changed` and says what they
  mean in its own words, so cig's own lines about the layer (`E781`,
  `E780`, applied, dropped) are left out. A rehearsal that cig-orchestrator
  discarded right after no longer tells you to `cig box apply` it.
- **The guard_scope codes, `E892`–`E899`**, registered for
  cig-orchestrator's guard levels (cig-orchestrator #57) so `cig explain`
  and doctor know them: `E892` a piped download-and-execute cancelled,
  `E894` an agent's command waiting for a person, `E895` an agent that tried
  to approve itself, and `E896` the guard paused or off come from
  cig-orchestrator; the first run of a download, the system-wide gate's
  block, a suspicious launch chain and the boot-loop breaker (`E893`,
  `E897`–`E899`) are reserved for the parts not built yet.

## 1.12.3 — 2026-11-05

- **The blacklist family, `E580`–`E589`**, registered for cig-orchestrator's
  blacklist (cig-orchestrator #62), so `cig explain` and doctor know its
  codes: `E580` on the blacklist and `E587` a list that does not verify come
  from cig-orchestrator's guard; the rest (the two warnings, running
  anyway, a blacklisted server, an entry held for review, an old list) are
  reserved for the parts not built yet.

## 1.12.2 — 2026-11-05

- **A box whose setup is refused says why.** When the contained box could
  not be set up before the hop started (cig running as root, or no scratch
  folder outside your home and the pack, as with `HOME=/` in a service
  account or a container run as any user), the hop failed as `E551` "could
  not start `sh`: no such file or directory" and kept an empty layer
  (`E780`). It is now refused with the reason (`E777`, or `E712` under the
  guard); a plain `box: true` falls back to the tin with `E763`, as it does
  when the kernel refuses the box, and nothing is kept. `cig explain E777`
  names the cause.
- **`cig explain` says which codes come from cig-orchestrator.** The guard's
  findings (`E870`–`E889`) are registered here so explain and doctor know
  them, and every one was marked "planned: reserved for a feature that has
  not shipped yet", including the twelve cig-orchestrator has reported since
  1.3.0. The fourteen it reports through 1.6.0 now read "from
  cig-orchestrator: its guard reports it, cig itself never does" (`(cigo)` in
  the listings and in docs/ERRORS.md); the rest stay planned.
- **The release uploads the licence files once.** Every build job attached
  `LICENSE`, `NOTICE` and `THIRD-PARTY-NOTICES.md`, and two replacing the
  same file at once failed two jobs of the 1.12.1 release (its assets were
  all published and their checksums match). One job attaches them now.

## 1.12.1 — 2026-11-05

From a re-test of the tin's protections as an ordinary user (#112).

- **`cig runs <id> --verify` agrees with `cig unburn`.** It runs every check
  unburn runs before it touches anything, in the same order, and fails
  exactly when unburn would refuse, with the same code: the chain, the head
  the record saved, the ledger's line for the run (`E709`, which `--verify`
  did not consult), and the objects (`E703`). A `ledger` line joins
  `chain`, `head` and `objects`, and the refusal is printed with its code.
  A journal signed by another key no longer reads "intact" there (`E708`),
  and its head is not compared where unburn does not compare it.
- **`E760` names the signature on a signed journal.** In format 3 a line's
  link is a signature under the tin's key, so a broken one says "its
  signature does not match: this line was not signed by your key", and an
  intact chain is "each signed by your key"; format 2 keeps the hash wording.
  `cig explain E760` gains the format 3 cause.
- **A boxed hop with a network of its own sees cig's proxy alone.** The outer
  environment's proxy variables and their exceptions (`*_PROXY`,
  `npm_config_*proxy`, `GIT_PROXY_COMMAND`, `GLOBAL_AGENT_*`, `NO_PROXY`)
  lead nowhere from inside and are no longer passed on.

## 1.12.0 — 2026-11-05

The rehearsal box: a hop whose code you did not write runs so that
everything it writes is a layer cig reads afterwards, and nothing else on
the machine can be written (#106; for cig-orchestrator's rehearsal, its
#55 and #56). Linux.

- **`box: {contain: true}`.** The pack's roots layered with nothing passed
  through; a layer on your home itself, so `~/.bashrc` lands in it by any
  name; the box's own temp; then the whole tree read-only in one call and
  only the layers writable again. A `/dev` of its own (no input devices,
  cameras, sound or disks), its own IPC, host name, cgroup view and
  session, `no_new_privs`, and a syscall filter: no Unix socket, no new user
  namespace, no io_uring, ptrace, keyrings, BPF or mount calls, only this
  machine's architecture. Needs Linux 5.12, x86_64 or aarch64, a user that
  is not root, a network of its own and the PID namespace; refused (`E777`)
  where it cannot be had.
- **`box: {expect: [...]}`.** The hop's footprint as globs (`~/` for the
  home). What the layers hold outside it comes back as `r.outside` (with
  links into the home that it does not name, set-id files and unreadable
  folders reported whatever it says), with `r.changed` and `r.refused`, and one note
  (`E781`). Under `contain`, a hop off its footprint is held back.
- **Evidence.** A rehearsal or a contained hop that fails keeps its layer;
  the error's value carries it and the judgement.
- **`proc.apply_layer(layer)` and `proc.discard_layer(layer)`.** Apply a
  layer this run kept, journaled, only while it holds exactly what was
  judged (`E782`), or drop it.
- **A cig inside a contained hop** keeps a throwaway state; nothing of it is
  taken in.
- **`cig box discard`** now takes the layer's record and scratch folder with
  it; `cig runs --prune` and an unburn take a contained hop's scratch folder
  too.
- **A kept layer leaves no unreadable folder in the state directory.**
  overlayfs leaves its work folder at mode 0; a kept layer kept it, so a
  backup or anything else walking `~/.cigscript` was refused there. The work
  folders go when a layer is kept.
- **Fixed: a contained hop's network could not start when cig itself lived
  under /tmp.** The box's temp goes over /tmp, and the forwarder was started
  by its path after that, so a cig in a build tree or a tool folder under
  /tmp left the hop with no way out (npm then fell back to a stale cache
  after a minute of retries). The forwarder's program is held before any
  mount and started through that handle.
- **The proxy tries a registry's IPv4 addresses first**, five seconds an
  address: on a network that drops IPv6 (some VPNs do, against leaks) a v6
  address only times out.
- **Fixed: two senders could file reports at once.** A send waited five
  seconds for another sender's lock and then went ahead without it, so a
  run's background flush and a `cig crash send` typed meanwhile could both
  file, and against GitHub the same report could be filed twice. It now waits
  up to two minutes, and when the lock is still held the report stays queued
  for the next send. Within one process the lock is re-entrant: a flush no
  longer stalls five seconds on its own lock for every report.
- **Fixed: the same failure reported twice within one second lost a count.**
  A report's id is its time to the second and its fingerprint, so the second
  report overwrote the first in the ashtray. A new report claims a name of its
  own (`<id>-2`, and so on).
- **The test suite can no longer reach a real tracker.** Every cig a test
  starts files into the sandbox's own tracker, background flushes included;
  one started without it could send a test report through an authenticated
  `gh`.
- **Fixed: `proc.shell` dropped a boxed hop's layer.** It gives back the
  layer and the judgement as `proc.run` does (since 1.11.0 a shell hop with
  `apply: "never"` kept its layer without saying where).

## 1.11.2 — 2026-11-05

The box and the guard, hardened after a review of 1.11.1. The keyring
finding was reproduced on a desktop that keeps the tin's key there. The
six fixes and the proxy's rules each have a test that fails without them;
the smaller items are covered by the box and guard tests as they stand.

- **Fixed: a guarded hop could reach the key in the OS keyring.** On Linux
  that keyring is the Secret Service on the session bus, and a boxed hop
  had the bus. While the key may be in the keyring, a guarded hop now gets
  no session bus: the address is not passed on and the bus and systemd's
  private socket are covered. `cig config seal_key file` keeps the bus for
  hops and the key in a file the guard hides.
- **Fixed: a key file named by `CIG_SEAL_KEY_FILE` outside the key folder
  was readable by a hop**, which was also told where it was. It is covered
  and the variable is not passed on.
- **Fixed: a hop on the registry network could swap the proxy's socket**
  for a link to another socket on the machine, which the forwarder would
  then have connected to. The socket lives under the run's `proxy/`, read
  only inside the box whether or not the guard could be made.
- **Fixed: an isolated hop could declare an undo through the burn
  protocol** that cig would later run outside the box. A hop with a network
  of its own gets no `CIG_JOURNAL`.
- **Fixed: a kept layer's record sat in the layer's own folder,** which
  the hop could write, so a planted record could keep a layer a dead cig
  left behind. Records live under the run's `kept/`.
- **Fixed: a kept layer was swept by the next `cig run`.** The sweep of
  dead runs' layers took kept ones too, so `cig box apply` found nothing
  once any other script had run. Only layers a run never got to apply are
  swept now.
- **The proxy:** a name or `*.suffix` in `allow_hosts` means port 443
  (`host:port` for any other); a name admitted by a wildcard that resolves
  to loopback, a private or a link-local address is refused; a host a hop
  asked for is printed with control characters masked.
- **Smaller:** the box's relay and init are non-dumpable once the user
  namespace's maps are written, the forwarder too; descriptors a library
  left without close-on-exec close at the hop's exec; `/etc/resolv.conf`
  is covered with `/dev/null` instead of an empty file in a folder the hop
  could write; the guard's read-only remount keeps the mount's own flags
  (a home mounted `noatime` or `nodev` no longer turns the guard away).
- **Said plainly in BURN.md:** the guard keeps the tin read-only and the
  key out of reach, and the box keeps the pack; a hop that reaches other
  services you run can act through them. Neither is a sandbox for the rest
  of the machine.

## 1.11.1 — 2026-11-05

- **Fixed: a boxed hop's timeout fires.** The box's relay and init (1.11.0's
  PID namespace) inherited the standard library's exec-status pipe and held
  it until the hop ended, so `spawn` returned only then and the hop's
  timeout never fired; cig-orchestrator's suite caught it the same hour.
  Both now put the child-exit signal back to its default and close every
  descriptor above stderr once they have forked, and the forwarder's own
  stdio goes to `/dev/null`.

## 1.11.0 — 2026-11-05

The first half of the box for untrusted code (#106, steps 1, 2 and 6): the
PID namespace, the network, kept layers.

- **`box` takes a map.** `{network, allow_hosts, pidns, apply}`; `box: true`
  is as it was.
- **The PID namespace** (on by default with the box). The hop is pid 2 under
  an init of cig's, the namespace's processes alone in `/proc`; it cannot
  see, signal, trace or read cig. Each process dies with its parent, so a
  cig that dies takes the namespace with it, and a layer is applied only by
  a live cig after the hop; the layer a dead cig left is dropped by the next
  `cig runs`, which says so. `pidns: false` keeps the machine's view.
- **The network.** `off`: a namespace with `lo` alone and an empty
  resolver. `registry`: the same, with one way out through a forwarder on
  `127.0.0.1:38080` to cig's CONNECT proxy, which allows `allow_hosts` (a
  name, `*.suffix`, `host:port`), counts the bytes, and refuses and records
  the rest (`E771`; events under the run). Isolation asked for where the
  box cannot be made is refused (`E777`), not fallen back from.
- **Kept layers.** `apply: "never"` holds the layer back (`E780`; the hop's
  result carries `layer`); `apply: "ask"` asks at the terminal and keeps
  away from one. `cig box list`, `cig box apply <run> <hop>` (a run of its
  own, unburnable), `cig box discard <run> <hop>`; an unburn of the run
  drops its kept layers.
- **Fixed: a child run comes in whole.** 1.10.0 kept a child's record and
  journal but not its objects, and refused `cig unburn <child>` (`E714`),
  so a cig inside a hop that wrote outside the parent's pack was no longer
  undone by the parent's rollback. From 1.11.0 the take-in moves the
  child's objects into the tin (checked against their names), re-seals its
  sealed objects under this machine's key, relocates and re-signs its
  journal and records it in the ledger; a child unburns on its own, the
  compensation through the burn protocol is claimed again, and `E714` is
  retired.
- **`cig tin mac <file> --label <l> [--check <hex>]`.** A keyed MAC under a
  subkey of the tin's key derived for the caller's label: what another tool
  (cig-orchestrator's trust store) signs its records with. A hop in the box
  cannot make one under the tin's key, since the key folder is empty there.
- **The `finding` family, `E870`–`E899`.** The codes cig-orchestrator's
  guard raises for what it finds in someone else's code, registered here so
  `cig explain` and doctor know them.

## 1.10.0 — 2026-11-05

The headline of the tin's protections (#105, L1): the tin read-only to the
scripts it protects, while they run.

- **The tin guard.** A boxed hop on Linux sees the state directory
  read-only and the key folder empty: the state root bound over itself and
  remounted read-only in the box's own mount namespace, after the hop's
  layers and the inbox were bound writable underneath, and an empty tmpfs
  over the key folder (made before the hop, so none can be planted). A hop
  that rewrites or deletes a tin object or a journal gets `EROFS`. `tin_guard`
  is `auto` (boxed hops, wherever the kernel allows; one note, `E712`, where
  it does not), `on` (every hop, boxed or not, in the same namespaces
  without the overlay, and a refusal where the guard cannot be made) or
  `off`; `CIG_TIN_GUARD=0` is off for one run. `cig doctor`'s tin block
  gains a `guard` line, tried rather than guessed.
- **The inbox and child runs.** The one place a guarded hop may write. A cig
  started inside the hop has it as its own state root, with a key file of
  its own and the parent's settings copied in, so `cig run` inside a boxed
  task (cig-orchestrator's, for one) keeps working. When the hop ends its
  runs are taken in as children of the run: listed under it (`cig runs
  <id>`), `parent` on their records, their journals re-signed under this
  machine's key. A child's changes reached the pack through the hop's layer
  and are in the parent's journal with their before-images, so a child keeps
  its record and journal, not its objects, and is unburned through its
  parent (`E714`). Anything else left in the inbox is dropped with a note
  (`E713`).

## 1.9.0 — 2026-11-05

The second of the tin's protections (#105): a record scripts cannot rewrite.

- **The journal is signed.** Format 3 links each line to the one before it
  with an HMAC-SHA256 under a subkey of the tin's key (`journal-mac`) instead
  of a plain hash, so a script that rewrites a journal and recomputes its
  chain cannot make it verify (`E760`). A run under another machine's key
  (a parcel, a key since replaced) reads as *signed by another key*, not as
  tampering: `cig unburn --accept-unsigned` restores it, its objects still
  checked by hash (`E708`). `cig config journal_sign off` keeps writing
  format 2 for anyone who needs older cigs to read new runs; an older cig
  that meets format 3 refuses it by its number (`E711`).
- **The ledger.** One line per run in the keys directory, apart from the
  tin, each MACed under its own subkey (`ledger-mac`) over the line before
  it: where every run's journal ended, when it was unburned, pruned,
  forgotten or brought in by a parcel, each line naming the tin it
  witnesses so that two state directories beside one key keep separate
  accounts. It catches what a per-journal chain
  cannot: a journal cut short, or an older copy swapped in (`E709`). `cig
  runs --prune` is recorded, so it stays quiet; a run removed by hand gets
  one calm note (`E710`), and `cig runs --forget <id>` stops it. `cig tin
  head` prints the ledger's head to keep anywhere else; `cig tin verify
  --head <value>` checks against it, and `cig tin verify` walks the ledger's
  own chain.

## 1.8.0 — 2026-11-05

The first of the tin's protections (#105): the record that lets you undo a
run is kept private, and the key that seals it is moved out of reach. Nothing
to set up, and nothing to notice on a normal machine.

- **The state directory is owner-only.** `~/.cigscript` is created `0700`, set
  explicitly rather than from the umask, and an older one readable by other
  users is tightened to owner-only. A state directory other users can
  write, or one owned by someone else, is refused on a terminal (`E707`) with
  the one command that fixes it; unattended (CI, a container) it warns once
  and carries on, so a build is never broken by a surprise. `cig config
  tin_perms warn` makes it always a warning, `off` stops the check.
- **The key lives apart from what it opens.** The fallback key file moves out
  of the tin into the OS config directory (beside the state root when
  `CIGSCRIPT_HOME` is set), so a copy of the tin no longer takes the key with
  it; an existing `tin/seal.key` is migrated once. `CIG_SEAL_KEY_FILE` names
  it outright for CI and headless use.
- **One key never does two jobs.** The stored secret is a master key, and each
  use derives its own subkey from it with HKDF-SHA256 under a label
  (`tin-seal` for sealing; the MAC labels are reserved for the signed journal
  and ledger). Objects sealed before this (`CIGSEAL1`) still open; new ones are
  `CIGSEAL2`, sealed with the derived subkey.
- **Carry the key between machines.** `cig tin key export` writes it wrapped
  with a passphrase (Argon2id, then XChaCha20-Poly1305), and `cig tin key
  import` puts it back, so a restored backup can open your sealed objects.
- **The key cannot be read out of cig's memory.** Once a key is loaded, cig
  sets `PR_SET_DUMPABLE` to 0 on Linux (and `PT_DENY_ATTACH` on macOS), which
  blocks same-user `ptrace`, `/proc/<pid>/mem` and core dumps. It resets for
  every program cig starts, so hops are unaffected, and the box still works
  after a seal: its child makes itself dumpable again inside its own user
  namespace before writing its maps (a non-dumpable process cannot open its
  own `/proc/self/uid_map`), which the exec would do anyway. `CIG_ALLOW_DEBUG=1`
  skips the protection for contributors debugging cig, with a note on every
  run (`E715`).
- **`cig doctor` shows the tin's protection** in one block: whether the state
  directory is private, where the key is and that it is out of the tin,
  whether a hop could read it from memory, and a one-time reminder to back the
  key up when sealed objects exist and it was never exported (`key_reminder`).

## 1.7.1 — 2026-10-29

**Nothing of yours in a report** (#103), and the reporter debugged against
what 1.7.0 shipped. Reports sent the full working directory, so a client's
or a project's name in a folder reached a public issue; paths inside a
message, a bundle or a handed-in report could too. Every route is now an
allowlist of shapes.

- **Privacy** (#103). The working directory travels as its shape, never its
  names: the placeholder of the run's mapping (`root0`, `out/d1/D2/d3`), or
  `~/d1/D2` and `/d1/d2` when no run is in progress (a panic, a handed-in
  report). The command line keeps only the words cig knows (its
  subcommands, flags, configuration keys and values, versions, error codes,
  numbers) and gives everything else a shape: `cig run <script>`, `<path>`,
  `<run>`, `<flag>`, `<arg>`; `cig report` bundles do the same with their
  `args`. Every string in a report goes through the run's mapping: a path
  it has seen is its placeholder, anything else shaped like a path or a
  file name gets one too, a field named for a path (`cwd`, `path`, `file`,
  `script`) is mapped whole, a command line (`command`, `args`, `cmd`) is
  reduced as above, globs are mapped component by component; a location in
  cig's own source and cig's own file names stay. The bundle's journal,
  diagnostic, diagnosis and intents are mapped the same, its script and
  working directory too, and its state directory says `~/.cigscript` or
  `<custom>`. `cig crash submit` anonymises what another tool hands in the
  same way (what cig cannot tell from a plain word, a task's or a project's
  name, is the tool's; cig-orchestrator 1.1.1 does). The body itself is an
  allowlist of fields: the ashtray's bookkeeping (queue, issue,
  announcement) never travels, and `cig crash show` prints exactly the body
  that would. The anonymising happens before the fingerprint is taken, so
  one bug hit in two differently named folders is one issue.
- **No false alarm from a path touched twice.** The rollback tripwire judged
  every journal entry against its own before-state, so a folder made then
  deleted in one run, a file rewritten then removed, or a hop's tree
  deleted after it, tripped on every failed run though the pack was back
  as it was. Each path is now judged once, by the state before the first
  op that touched it (the later before-states are the run's own
  intermediate states), with a path named two ways (from the working
  directory by the script, whole by a hop's effect) counted as one.
- **`cig unburn` runs the tripwires too.** A later rollback had none: a
  restore that put a mode bit back wrong went unnoticed. The rollback
  tripwire and the tin sample now run after an unburn as after a run's own
  rollback; the unburn's outcome stays what it was.
- **A typed `cig crash flush` sends now.** A report in its backoff after two
  failed tries was skipped with `nothing to send yet` when the person typed
  the command; typed, the flush is a request for now. The background's
  pass still honours the backoff.
- **Big reports fit an issue.** The rollback tripwire lists twenty-five
  mismatches and counts the rest (a run over 300 files made a 100 KB report
  that GitHub's 65,536-character limit would have refused on every retry);
  a body past 60,000 characters is cut back, the details first, then the
  repro's script, seeds and stubs, and says where the whole report is.
- **Links in a repro.** A link's relative target was mapped as a path from
  the working directory and landed under `out/`, dangling; it stays
  relative to the link, mapped through the link's directory.
- **Nested pack roots.** A hop's file under `pack { "./q", "./q/a" }` was
  journaled once per root that saw it, so the rollback removed it twice and
  the repro's stub wrote it twice; one effect, journaled once.
- Tests: `a_report_never_contains_a_path_component_of_the_users_folder`,
  `a_crash_report_cwd_is_a_placeholder`,
  `a_path_touched_twice_does_not_trip_the_rollback_tripwire`,
  `cig_unburn_runs_the_rollback_tripwire`,
  `a_typed_flush_sends_a_report_waiting_for_its_retry_time`,
  `a_report_with_many_mismatches_lists_twenty_five_and_counts_the_rest`,
  `a_relative_link_target_stays_relative_in_the_repro`,
  `a_hop_under_nested_roots_is_journaled_once`; unit:
  `a_body_past_the_issue_limit_is_cut_back`, `redaction_rules` extended.

## 1.7.0 — 2026-10-29

**The tool reports its own bugs** (#101): never the person's. cig notices
when it is at fault, including bugs nobody saw, saves the report locally,
builds an anonymised repro from the journal and checks that it reproduces,
files it (or adds to the issue already open for it) with no typing, and
tells the person who hit it when the fix ships. `docs/REPORTS.md` is the
whole pipeline; `SECURITY.md` says what travels.

- **Whose fault.** Only cig's own failures are reported: a panic (`E901`),
  an internal error (`E900`), a tripwire (`E902`). A `cough`, a runtime or
  type error in the script, a failed hop, a usage error are the script's or
  the person's business and are never reported.
- **Tripwires.** After the work, cig checks its promises, never changing the
  run's outcome: every path a rollback restored is read back against the
  journal (bytes re-hashed, mode, link target, absence); the journal on disk
  is re-read (chain intact, as many entries as written, the kernel's head);
  up to three tin objects the run refers to are re-hashed; the first eight
  files a box applied are hashed against the layer. A broken promise is a
  report and one line: `tripwire: cig noticed something it got wrong; a
  report is queued (cig crash show <id>; cig explain E902)`.
- **The ashtray.** Every report is written to `~/.cigscript/ashtray/` before
  anything is sent (`crashes/` from before 1.7.0 is read too); a run never
  waits on the network. At the end of a command, anything waiting and due
  goes in the background through `cig crash flush --quiet`; a failed send
  keeps the reason and a retry time on the report (the next run, then five
  minutes, half an hour, two hours, twelve, a day), one sender at a time.
  `cig crash list` shows each report as local, pending, failed with its
  reason, or filed with its issue; `cig crash flush` sends what is waiting.
- **One setting.** `reports = ask | always | never` governs every kind
  (`crash_reports` is the older name, read when `reports` is not set).
  `ask`, the default, shows the report once in a terminal and remembers the
  answer; `always` files without asking and says so in a line; `never`
  keeps reports local; under CI with nothing set, `never`.
- **A verified, anonymised repro** travels with every report that has a
  journal to build from: placeholder paths that keep depth, extension, a
  leading dot, a capital letter and a non-ASCII character (`root0/D1/F3.txt`),
  bytes generated from the recorded hash (equal files stay equal), each hop
  a stub shell script doing what the journal saw, the hop's command never
  copied. It is run in a sandbox and attached only when it shows the same
  finding, then shrunk one op at a time while it still does. The report's
  own text names paths by their placeholders.
- **One issue per bug.** A fingerprint (kind, location, the message without
  its numbers) opens one issue, labelled by kind and area, with a stats block
  in its body (count, versions, OSes, first and last seen, smallest verified
  repro). A repeat updates the block; a comment comes only with a new
  version, a new OS or a smaller repro.
- **The loop closed.** A filed report remembers its issue; `cig update
  --check` asks whether it is closed and which release's notes name it, and
  says once: `a bug you reported (#N, what it was) is fixed in X: cig update`.
  The humidor hint (a pinned script on a version with a known fixed bug) is
  not here yet.
- **One pipeline for the family.** `cig crash submit --origin <tool> --repo
  owner/name --file report.json [--now]` hands another tool's report into
  the ashtray under cig's setting, redaction and routes; cig-orchestrator
  1.1.0 sends its own through it.
- **`cig report <run> --send`** files a run's bundle the same way, as a
  report labelled `report`; an explicit `never` sends nothing.
- The curl route sends `Content-Type: application/json` (noted on #25).
  Doctor shows the ashtray.

## 1.6.3 — 2026-10-29

- **The box does not nest, and says so before the run.** overlayfs refuses
  an upper directory that sits on an overlay, so a boxed hop whose state
  directory is inside another box's layer (a cig a boxed task starts, as
  cig-orchestrator's own suite did), or on a container's layer (Docker's
  overlay2 root), could not be made, and the fallback said only `errno 22`
  at the mount. The plan and the `E763` fallback now name the overlay the
  layers would sit on and the way out (a `CIGSCRIPT_HOME` on a plain
  filesystem: a tmpfs, a volume).

## 1.6.2 — 2026-10-29

Two things cig-orchestrator's suite found running on 1.6.1, where a cig home
sits inside the pack of an outer run.

- **A rollback stopped at a directory it could not read.** A hop-created tree
  holding a mode-0 directory (overlayfs's work directory of a boxed hop that
  died, or a `chmod 000`) failed its removal with `Permission denied`, and
  every directory above it stayed too. The rollback opens the modes and
  removes the tree whole, as `cig runs --prune` learned to in 1.6.1: what a
  hop made unreadable is still the hop's to lose.
- **An interrupted boxed hop left its layers under the run.** They were
  never the pack's (a hop that dies has its layer dropped), and the
  unreadable directory inside them was in the way of anything walking the
  home (cig-orchestrator's suite could not remove its own work directory).
  `cig unburn` removes them with the rollback, and every `cig run` sweeps
  them first for any run that is not alive, so nothing unreadable stays
  under the home.

## 1.6.1 — 2026-10-29

A debugging pass over everything 1.4.3 to 1.6.0 shipped, adversarial
scenario by scenario against the installed binary, each gap closed with a
test that fails on 1.6.0.

- **The box with `cwd` inside the pack fell back.** The working directory
  handed to the box was the hop's `cwd` as written (`"p"`), and the child
  had already entered it when the overlay was mounted, so `chdir` looked
  for `p/p` and refused (`errno 2`): every boxed hop with a relative `cwd`
  ran on the tin and said so. The directory is made absolute before the
  fork. The box tests now fail, not shrug, where the machine can make a box
  (`unshare -Urm true` works, overlayfs is there, kernel 5.11 or later):
  on 1.6.0 they passed through the fallback.
- **A folder a boxed hop emptied and made again was gone after the run.**
  An opaque directory in the layer was applied as the delete alone; it is
  the delete, then the directory itself, then its entries.
- **A boxed hop left its layers under the run.** overlayfs makes the work
  layer's inner directory unreadable (mode 0), where `remove_dir_all`
  stopped: the layers stayed, and `cig runs --prune` could not remove the
  record. The modes are opened and the tree removed, after an applied hop
  and a dropped one alike; the parcel skips a `box/` directory a crash left.
- **A boxed hop's `touches_sealed` was ignored.** Its writes were journaled
  as plain; the before-bytes of a file under those globs are sealed now, as
  on the tin.
- **A boxed build wrote its whole `target/` through the journal.** Hop
  watching never looks into `.git`, `node_modules`, `target`, `.venv` and the
  other build and dependency trees; the box applied and journaled every file
  a hop put there (3,000 object files: 3,001 ops and fifteen times the
  wall-clock of the same hop on the watch). Those trees are bound through
  the overlay from the real root now, so what a hop writes there lands as it
  always did and is never journaled; a tree that is not there before the hop
  is made through the box like the rest.
- **A `compensate` line from a hop without a `pack { }` was dropped.** The
  hop's own undo needs no pack; it is journaled and runs on rollback. A
  `write`, `delete` or `create` claim without a pack is said and dropped
  (`E761`), there being nothing to judge it against.
- **An undo that said a lot hung the rollback.** `protocol::run_command`
  piped the undo's stdout and never read it: 64 KiB filled the pipe and
  both sides waited for ever (a run killed at exit 143). Both pipes are
  drained on threads while the undo runs, and the report quotes the first
  200 characters of its last stderr line rather than the whole of it.
- **`proc.pipe` took `touches` and kept nothing.** The pipeline recorded
  its `touches` in the plan and the journal but never watched the pack, so
  a failing burn after a pipeline that rewrote a file could not restore it.
  The pack is watched around the whole pipeline exactly as around one hop
  (before-bytes to the tin, created files removed on rollback, writes
  outside reported), every stage gets `CIG_JOURNAL`, `touches` without a
  pack is `E757` as for a hop, and `box` on a pipeline is refused (`E400`)
  rather than quietly ignored: several processes cannot share one box, and
  `proc.shell("a | b", {box: true})` boxes the same thing as one hop.
- **A missing seal key was replaced behind the user's back.** With the key
  file gone (or the keyring entry), a new key was made on first use, and
  every sealed object and snapshot became unopenable for good, the report
  blaming "a byte was changed". Where anything sealed depends on the key, no
  new one is made: a restore refuses naming the key and where it was
  (`E703`), a run that would seal says so and keeps the change irreversible
  (`E757`), and the message says what to do (put the key back, or move the
  sealed objects away to start over). With the keyring set and only a key
  file present, the file is used rather than a fresh keyring key made.
- **A parcel written from an imported run lost the way back.** Its manifest
  named the importing machine's paths while the journal named the original
  machine's, so a third machine could map nothing. The manifest carries the
  imported run's own map (`prior`), and the import composes the two; a
  journal B appended to after its import (an unburn, a give-back) names B's
  paths too, and both read through one map.
- **The parcel read every file into memory to hash it**, on writing and on
  import; both stream now.
- **A tree snapshot re-read and recompiled `.cigseal` for every file**, up
  every directory above it. Each directory's list is compiled once and
  reused while the file is unchanged.
- **Idle scan workers spun.** A worker with nothing queued yielded in a
  loop, a core each, while another read a wide directory; they wait now.
- **`cig pin` and `E309` did not know the hop options.** `touches_sealed`
  and `fs.write_text(path, text, {sensitive: true})` arrived in 1.5.0 and
  `{box: true}` in 1.6.0; a script pinned below them is caught before it
  runs, and `cig pin` writes the version they need.
- **A changed last journal line was restored from.** The chain covers every
  line but the last, which nothing follows; the record's `journal_head`
  covers that one, and `cig runs <id> --verify` said so, but `cig unburn`
  checked the chain alone. A head that does not match the record refuses the
  unburn as a broken chain does (`E760`, `--force` to proceed knowingly).
- **`cig tin gc --sealed-orphans` without the key** would have taken every
  sealed object for one nothing opens, referred to or not. It refuses until
  the key is there (`E703`); nothing is removed.
- A dead condition in the git-sourced restore (a stand-in object was chosen
  by a check that could never be true) is removed; the behaviour is the one
  the comment described.

## 1.6.0 — 2026-10-28

- **The box** (#85). `proc.run(cmd, args, {box: true})` runs the hop over a
  copy-on-write layer on the pack: on Linux, in its own user and mount
  namespaces with each root mounted as overlayfs over itself (the real root
  the read-only lower layer, an upper directory under the run taking every
  write). A hop that fails (or exits outside its contract) has its layer
  dropped: nothing of it ever reached the pack, whatever the pack's size and
  whether or not `touches` named anything. A hop that succeeds has its layer
  applied as native burns (writes, deletes, mkdirs, whiteouts read as
  deletes, an opaque directory as a replaced one), each journaled with its
  before-state, so the run is unburnable like any other; the plan line says
  `(boxed)`. Where the box cannot be made (no unprivileged user namespaces,
  a kernel before 5.11, macOS, Windows, the BSDs) the hop runs on the tin as
  usual and the run says so (`E763`); the plan says so before the run. The
  reflink clone for macOS and Windows is not made yet: there the box falls
  back, and says so.
- **Hop watching that scales** (#84). The scan of a pack is read on every
  core (up to eight threads), kept compact (one arena of relative paths and
  entries of a few dozen bytes, sorted by path, about 60 bytes an entry),
  and compared with the scan after the hop in one merge walk; the scan after
  reads no directory whose modification time is unchanged and old enough
  for that to mean something, it only stats what is in it. `watch_cap` is a
  memory guard now, five million entries by default (where 1.4 stopped at
  50,000 and left what a hop created past that point in place); the rule at
  the cap is unchanged, nothing that existed is removed. Measured in
  `docs/FINDINGS.md`.
- **The parcel** (#86). `cig parcel <run>` writes `<run>.cigparcel`: the
  run's record, journal and after-state, every snapshot, tin object, recipe,
  chunk and sealed object its journal refers to, the bytes of every blob
  git held for it (carried whole), and a manifest naming each file's
  SHA-256, the cig that wrote it, the pack's roots and the working
  directory. `cig unparcel <file> --root name=path ...` checks every file
  against the manifest before it writes anything (a changed byte refuses the
  whole parcel), needs every root mapped to a place here (`--show` lists
  them; `--cwd` when the working directory cannot be told from the roots),
  refuses sealed objects this machine's key does not open unless
  `--allow-sealed-missing`, places the objects in the tin and the run among
  the runs, and records in the run's record where it came from and how its
  paths map. The journal is never rewritten, so its chain still verifies;
  `cig runs <id>` says `imported`, and `cig unburn <id>` works here with the
  usual drift check against this machine's files. `E762` names a refusal.
- **The burn protocol** (#89). Every hop a real run starts gets
  `CIG_JOURNAL`, a file it may append JSON lines to: `write`, `delete` and
  `create` say what it changed (with the hash of the bytes before, and where
  it kept a copy of them), `compensate` says how to undo it (a command, no
  shell). The kernel never trusts a hop's word about bytes it can check: a
  reported path outside the pack is refused (`E752`), a `create` the scan
  saw before the hop is dropped, a claim about a file the tin kept is
  checked against the tin, and a change not under `touches` becomes
  reversible only when the tool kept the bytes and they hash as claimed
  (`(reported by the hop)` in the record); the rest stays irreversible with
  the reason (`E761`). A reported compensation is journaled like a block's
  and runs on rollback and `cig unburn`, from the directory it names, with
  `CIG_RUN_ID` set. cig is the first speaker: a `cig run` a hop started
  appends its own undo, `cig unburn <its run>`, so the parent's rollback
  puts it back through its own exact journal. `docs/PROTOCOL.md` is the
  contract, with the lines in Python, Node and bash.

## 1.5.0 — 2026-10-28

- **The tin in chunks** (#82). A file of 64 KiB or more under `touches` is
  cut into content-defined chunks (FastCDC: 16 KiB at least, 64 KiB on
  average, 256 KiB at most), each an object named by its own hash, and the
  file becomes a *recipe* (`tin/recipes/<sha256>`): the ordered chunks, the
  hash and the size of the whole. A one-byte change stores the chunk it
  landed in, not the file again; an insert moves one boundary. A restore
  checks every chunk against its name and the whole against the hash the
  journal recorded, and refuses (writing nothing) on a missing or corrupt
  chunk. `cig tin` reports what is stored next to what the kept files would
  take; `gc` keeps every chunk a live recipe names. What a rollback
  displaces into the tin is chunked the same way, so a hop's output that is
  the original with one change shares every other chunk with it. Objects a
  1.4 cig wrote (large files kept whole) restore as they always did.
- **Sealed: the tin encrypted at rest for secrets** (#83). A file the tin
  keeps that is a secret is sealed: XChaCha20-Poly1305 in the STREAM
  construction (no crypto of our own), a megabyte a segment, every segment
  authenticated and the last one marked, so a byte flipped, a tail cut off
  or another key refuses the restore and writes nothing. Sealed objects are
  named by a random id and never shared, so two equal secrets stay two
  objects that tell nothing about each other; the journal keeps the
  plaintext hash, so the bytes are checked on the way back. What counts as a
  secret: a built-in list of names (`.env*`, `*.pem`, `*.key`, `id_rsa*`,
  `credentials*`, `*secret*`, `.npmrc`, `kubeconfig`, `*.kdbx`, and more), a
  pack root's `.cigseal` (the same shape as `.cigignore`), a hop's
  `touches_sealed: [...]`, and `fs.write_text(p, t, {sensitive: true})`;
  the `seal` setting widens it to `all` or turns it off with `never`. The
  key lives in the OS keyring (Secret Service, Keychain, Credential
  Manager), made on first use; where there is none, in
  `~/.cigscript/tin/seal.key`, readable by its owner alone (`seal_key =
  file` makes that the rule). A run's own snapshots of secrets are sealed
  the same way, inside tree snapshots too, and what a rollback displaces
  from a sealed file stays sealed. The plan says `(sealed)`; `cig tin`
  counts sealed objects apart; `cig tin verify` opens every one (`--quick`
  looks at the shape); `cig tin gc --sealed-orphans` clears objects the key
  cannot open any more.
- **Git blob reuse** (#87). A file under `touches` that git already holds,
  tracked and clean against the index (the bytes on disk hash to the blob
  the index names), is referenced from the repository's object store
  instead of copied into the tin: the journal records the repository, the
  blob and our own SHA-256, and the restore reads the blob back through
  `git cat-file`, checks it against that hash and refuses other bytes. The
  run pins every blob it refers to under `refs/cig/runs/<id>` (one tree
  naming them all) so a `git gc` after a rebase cannot take them; `cig runs
  --prune` drops the pin with the record. A dirty or untracked file goes to
  the tin as before; a repository the run cannot pin in is copied too. Git
  is only ever read, the pin aside. `(git)` in the record names the keeper.
- **The hash chain** (#88). Journal format 2: the first line carries the
  hash of the run's id and every later line the hash of the line before it
  as written; `run.json` records the head when the record is saved.
  `cig runs <id> --verify` walks the chain, compares the head with the
  record and checks every snapshot and tin object a rollback would need.
  `cig unburn` refuses a journal whose chain is broken (`E760`) unless
  `--force`. A format 1 journal is read as it always was and gains no
  chain. This is tamper evidence, not secrecy: whoever can write the
  journal can recompute the chain, unless the head is kept somewhere they
  cannot write.

## 1.4.3 — 2026-10-28

- **Changes outside `touches` are reported once per folder** (#90). A hop
  that deleted a thousand-file folder it had not named produced a thousand
  `E757` lines, one per file, and buried the two that mattered. The files
  under one top-level folder are now one line: how many, the smallest
  directory that covers them, the one glob that would have kept them, and a
  few of them as examples; `cig runs <id>` lists every one. A lone file is
  still named itself.
- **`cig unburn <id> --give-back` can be previewed** (#91). `--give-back
  --dry-run` lists each file the rollback moved into the tin, with what the
  tin holds for it, what is at the path now and whether that is still what
  the rollback left, and changes nothing. A path changed again since the
  rollback is refused without `--force`, as a rollback over a later edit is
  (`E704`), and the preview says so.

## 1.4.2 — 2026-10-28

- **A burn can carry a name** (#80): `burn ("deploy") { } unburn { }`, or
  `burn (s, name) { }` with a state map, any expression that is a string.
  The plan, the run record and a rollback name the burn by it instead of by
  its line: `deploy is NOT undone: its compensation failed: …`, where a tool
  written in CigScript used to be quoted by a line of its own source.
  Without a label the line stands in, worded once (`the burn at line 12 was
  not undone: its compensation failed: …`).
- **A `touches` glob written with a leading `?` may match nothing without a
  word** (cig-orchestrator #48): `?yarn.lock` is a file the project may not
  have yet, not a typo; the `?` is not part of the glob, and a file created
  under it is kept like any other.
- Each warning about a hop's watch is printed once; 1.4.0 said the ones from
  before the hop twice.

## 1.4.1 — 2026-10-28

- **A link a hop makes is the pack's entry, wherever it points.** 1.4.0 judged
  a hop-created link by where it led, so one pointing out of the pack was
  reported as a write outside the pack (`E752`) and left in place on
  rollback. It is judged by where it sits: rollback removes it, and what it
  points to is never touched.

## 1.4.0 — 2026-10-28

Found by two passes over everything 1.3.0 and 1.3.1 added, the second an
outside one over hop watching and the tin; every fix has a test on the
platforms that can run it. The minor number moves for three things a user
can see: `cig unburn <id> --give-back`, the pack lock (`E759`) and the
`watch_cap` setting.

- **A rollback removes nothing that existed before the run** (#71). Hop
  watching reads a pack up to a cap; past it a file first seen after the hop
  could not be told from one the scan never reached, and rollback removed
  it. Now a scan that stopped at the cap says so up front, journals nothing
  as created, and asks the disk before it calls a file deleted. The cap is
  the `watch_cap` setting (50,000 entries). A link that leads out of the
  pack is a door, not a part of it: nothing behind it is walked or
  journaled, what a hop writes through it is reported as outside the pack
  (`E752`), and a rollback never removes a path whose real place is
  outside the pack.
- **A rollback destroys nothing it did not put there** (#72). Hop watching
  cannot tell the hop's bytes from a save made while it ran, so a file a
  hop entry's rollback removes or rewrites goes to the tin first, the
  record says so, and `cig unburn <id> --give-back` puts it back (and moves
  what is there now to the tin in turn, so the step is as reversible as the
  rollback was). A file the hop left that changed again after the hop ended
  is left alone, with `--force` for the old bytes on purpose. Two runs on
  one pack would roll back each other's work: a run takes the pack's lock,
  and a second run (or an unburn) on the same or an overlapping pack is
  refused with `E759`, naming the holder. A cig a hop starts (`cig run
  inner.cig` as a task) shares its parent's pack, since the parent waits
  for it; every hop now sees the run's id as `CIG_RUN_ID`.
- **The tin cannot be poisoned** (#73). A file changing while it was being
  stashed left an object that did not hash to its name, and every later
  stash of that content reused it. The copy is hashed as it is written and
  named by what arrived; an object already there is trusted only when it
  hashes to its name, and replaced when it does not.
- **`cig tin gc` during a hop keeps what the hop stashed** (#74). The run
  lists the objects it is about to depend on in its directory before any
  exists under its name, and `gc` reads that list.
- **A folder a hop replaced with a file comes back as the folder** (#75),
  with what was in it, the file gone to the tin; **an empty folder a hop
  deleted comes back** (#76), with its mode.
- **A `touches` that matches nothing is said** (#77): before the run, when
  the whole list covers no file, with the nearest glob that would (one glob
  among several matching nothing is ordinary: a lockfile the project may
  have); in the dry run, as a count beside each glob; and after the run,
  naming the glob that would have kept a file the hop changed outside every
  glob.
- **Links.** A dangling link is watched like any other, so a hop that
  deletes or retargets one is undone; a write through a link to a file
  outside the pack is reported (`E752`) instead of going unseen.
- **Hops.** The timeout covers the hop's output, not only its child: a
  process the child left behind holding the pipes held the hop forever;
  now the hop ends at its deadline (`E554`, naming the stream), the rest of
  its process group with it. `proc.pipe` feeds `stdin` from a thread and
  starts its clock before the first stage, so a first stage that never
  reads its input cannot stop the timeout. On Windows a hop's child joins
  its job object before it runs its first instruction, so what it starts at
  once is in the job too, and a refusal to put it there is said once. The
  resolver is the only word on what starts: a bare name it cannot find is
  `E551`, never a search of the current directory through an empty `PATH`
  entry; with no `PATH` at all the platform's default search is used, and
  on Windows the system directories are searched after `PATH`, as
  `CreateProcess` does, and a Store app's execution alias resolves.
- **Run records.** On Windows a run's start time comes from the kernel, not
  from a PowerShell started for every run (a dry run asks for none); a
  record from another boot with this process's pid no longer passes for
  alive; `run.json` and the tin's objects are synced to disk when written;
  the record carries the pack and how many files the rollback moved into
  the tin, and `cig runs <id>` lists them, with `(put back)` beside the
  entries a rollback restored.

- **A copy onto its own name in another case emptied the file** (#66). The
  case-only exception 1.3.1 added to the same-file guard let `fs.cp` through
  as well, and a copy truncates its destination before it reads its source.
  The exception now belongs to `fs.mv` alone, and it asks the directory, not
  the link count: the destination's spelling must be absent from the
  listing, which is what a case-insensitive filesystem looks like. So a
  directory renames across case too (#67), and a symlink is no longer
  renamed over its own target (#68).
- **Rolling back a case-only move deleted the file.** The move back cleared
  the source name first, which on APFS and NTFS is the file itself, and the
  destination's snapshot, the same file again, was restored after it. A move
  between two spellings of one name is journaled as the move alone now, and
  moved back without clearing anything.
- **A hop's own `PATH` chooses its program again** (#69). Since 1.3.0 the
  resolver searched the parent's `PATH`, so `{env: {PATH: ...}}` started the
  parent's binary with no error. It searches the `PATH` the child will see.
  On Unix it also asks what `execvp` asks, whether this process may run the
  file, instead of reading the mode bits; reads a relative `PATH` entry from
  the hop's working directory; and keeps the name the script typed as
  `argv[0]`. On Windows a path program without an extension is completed
  through `PATHEXT` before the bare file is tried, so `./gradlew` starts
  `gradlew.bat`, and the C runtime's quoting is used only for a native
  `.exe` or `.com`.
- **A partial rollback is finished by a plain `cig unburn`** (#70). The
  entries a rollback put back are stamped in the journal, so the next
  `cig unburn <id>` retries what failed and leaves the rest as it is; the
  record lists the failures after a manual unburn too. `--force` restores
  everything again, as before. A compensation that fails leaves the ops it
  covered standing, and the report says `not undone` of them instead of
  `compensated`.
- **E758 reads every component** of a path, in either separator, and knows
  `com0`, `lpt0`, the superscript digits, `conin$` and `conout$`; the kernel
  refuses a computed device name with or without a `pack`.
- **Windows restores.** A read-only file is swapped without the wait meant
  for a held-open one, and a failed swap leaves no staging file behind; a
  relative symlink target is read from the link's own directory when a
  restore decides between a file link and a directory link.
- **macOS durability.** `sync_all` already is `F_FULLFSYNC` on Apple
  targets, so 1.3.0 added nothing there; the fallback a filesystem that
  refuses it needs, `fsync`, is now a real one.
- `ps` is asked with `LC_ALL=C` and `TZ=UTC` on macOS and the BSDs, so a
  run's start time reads the same from any shell; `clean_env` keeps the
  Windows variables programs expect (`TEMP`, `LOCALAPPDATA`, `PROGRAMFILES`
  and the rest); `install.ps1` no longer ends the caller's shell on an error
  under `irm | iex`, takes a path with brackets literally, and writes the
  user `PATH` through the registry so `%VAR%` entries stay unexpanded.

## 1.3.1 — 2026-10-27

- **`cig unburn` after a run on Windows.** A directory's fingerprint read
  sizes and times from the directory listing, which NTFS refreshes only when
  a file's last handle closes, so a run that had just finished could read as
  changed a moment later and `unburn` refused it (`E704`, the same entry
  count on both sides). The fingerprint and the tin's scan read each file
  itself now. Found by cig-orchestrator's suite. (#64)
- **An apostrophe reaches an MSYS program.** Git's `bash`, `sh` and `perl`
  parse their own command line, where a bare `it's` loses its quote and `*`
  globs; an argument with such a character is quoted the way the C runtime
  reads it, which both parsers agree on. (#63)
- **A case-only rename is a rename.** `fs.mv("readme.md", "README.md")` on a
  case-insensitive filesystem (APFS, NTFS) was refused as a move onto itself;
  it renames now, and only two names for one file that differ in more than
  case (hard links) stay refused. (#62)

## 1.3.0 — 2026-10-27

- **One resolver finds a hop's program.** `proc.run`, `proc.which`, every
  `proc.pipe` stage and the shell behind `proc.shell` agree on what starts:
  a name with a separator is a path against the hop's `cwd` on every
  platform; a bare name is searched on `PATH` with `PATHEXT` on Windows,
  so `npm` is `npm.cmd`; System32's `bash.exe` (the WSL launcher) and
  `wsl.exe` are never chosen for `bash` or `sh`. (#42)
- **`proc.shell` on Windows** runs under `sh -c` when an `sh` is on PATH
  (Git for Windows, MSYS2) and otherwise under `cmd /d /s /c` with the line
  reaching cmd as written; the plan line names the shell. (#43)
- **Windows: a file another program holds open comes back.** A restore
  that cannot rename over a held-open file (an editor, a scanner, an
  indexer without delete sharing) retries for a moment, then writes the
  content back in place and says so; when even that is refused the run
  stays `rollback_incomplete` with the program's hold named and the
  `cig unburn` that finishes the job. (#44)
- **Windows: a timeout ends the whole process tree.** Every hop's child is
  put in a Job Object (`KILL_ON_JOB_CLOSE`); a timeout terminates the job,
  grandchildren holding the pipes included, and cig dying takes the tree
  with it, the way the process group does on Unix. (#45)
- **Windows device names are refused up front.** `con`, `prn`, `aux`,
  `nul`, `com1` to `com9` and `lpt1` to `lpt9`, with any extension, are
  devices on Windows: a literal one is `E758` in `cig check`, an error on
  Windows and a warning elsewhere (the script will not run there), and the
  kernel refuses a computed one on Windows before it is journaled, where
  1.2 failed mid-burn with `E702 Incorrect function`. (#48)
- **The tin keeps what a file is.** A symbolic link a hop retargets,
  replaces or deletes comes back as the link it was, with or without
  `touches` (`(link kept)` in the record); a `chmod` by a hop is seen,
  although it changes neither size nor time, and put back where the file
  was in the tin; a modified file comes back with the modification time it
  had, from the tin and from a native snapshot alike. A read-only file
  under `touches` is stashed like any other. (#41)
- **A dead run never reads as `running` on a reused pid.** The record keeps
  when its process started (`/proc` ticks on Linux, `ps -o lstart` on macOS
  and the BSDs, the creation time on Windows), and a run counts as alive
  only when a cig with that start time holds the pid. (#46)
- **Durability says what each platform does.** Every flush on macOS is
  `F_FULLFSYNC`, which reaches the drive where `fsync` stops at its cache;
  on Windows the directory that gained a snapshot or an entry is flushed
  through a backup-semantics handle where NTFS allows it. BURN.md has the
  table. (#47)
- **Linux assets are static musl builds** (`x86_64`, `aarch64`, and `armv7`
  as `linux-arm`): they run on any glibc, on Alpine, and on Android under
  Termux, where 1.2.1's glibc 2.39 binary would not start. Windows on ARM
  (`windows-aarch64.exe`) and FreeBSD x86_64 (built in a VM) join the
  matrix, and `workflow_dispatch` builds the whole matrix without
  publishing. (#40, #49)
- **`install.ps1`.** A PowerShell installer for Windows: picks the x86_64
  or ARM64 asset, checks the checksum and the attestation, installs to
  `%LOCALAPPDATA%\Programs\cig` and offers to add it to the user PATH;
  `irm … | iex` reads its options from `CIG_*` variables. (#49)
- **Attestations checked at install.** `install.sh` and `cig update` run
  `gh attestation verify` when `gh` is installed and say `attested`; the
  checksum stays the floor. `install.sh` maps `armv7l` and `armv6l`, knows
  FreeBSD, and says so on Android. (#49)
- **`cig update` with a `gh` that is not logged in** falls back to the
  public API through curl instead of failing. (#35)
- **The suite runs where cig runs.** CI tests Ubuntu and macOS on both
  architectures, Windows x86_64 and ARM, Alpine as root (musl, BusyBox),
  and FreeBSD in a VM; the Ubuntu job also runs the suite in release mode,
  where inlining changes the frames a crash report keeps. The portability
  probe lives in `tests/portability/` and runs on every OS in the matrix,
  a `FAIL` line failing the job. The two tests that lock a directory with
  `chmod` step aside as root and say so; `ps -ww` reaches past the BSDs'
  line cut; the symlink rollback has a Windows port. (#50)
- **Docs.** LANGUAGE.md describes the pack as it is (scope, since 1.1),
  not as planned; LEXICON.md says what of the foreign script has landed and
  what the burn protocol still adds; one word per idea for the tin's next
  steps, in LEXICON.md and the roadmap: the box is the overlay, the parcel
  is the export, sealed is encrypted at rest, pressed is write-once. (#51,
  #52)

## 1.2.1 — 2026-10-27

- **A read-only file is snapshotted like any other.** 1.2.0 opened every
  snapshot for writing to flush it (the Windows fix of #26), which Unix
  refuses when the copy carries a read-only mode: a tree holding a tin
  object or a `.git/objects` entry could not be removed or replaced under a
  burn (`E702`, permission denied). The flush now opens read-only on Unix
  and lifts the read-only attribute around the flush on Windows.
- **Release assets for Intel macOS build again.** The release job asked for
  the retired `macos-13` runner and waited on it forever; 1.2.0 shipped
  without `cig-v1.2.0-macos-x86_64`. It asks for `macos-15-intel` now.
- **The read-only rediagnosis test fingerprints files, not directories.**
  On Windows a directory's modified time moved under it between the run
  and the check, twice in four runs; the property it guards (`cig doctor
  <run>` writes nothing) is about files, and a difference now names them.

## 1.2.0 — 2026-10-27

The pin, the humidor, the tin, and crash reports that put the run back.

- **Crash reports name the function.** A release build keeps its symbol
  names (`strip = "debuginfo"`, about 0.7 MB more binary), so the backtrace
  in a report reads `cigscript::interp::…` instead of `<unknown>`. The
  report keeps every frame of cig's own code, never the panic hook itself,
  the one `std` or `core` frame our code called into when the panic came
  from a library, and a closure inlined into a `std` frame under LTO.
  `CIG_INTERNAL_PANIC=eval` and `CIG_INTERNAL_PANIC=after-burn` join `=1`
  as the reporter's test hooks. (#23)
- **A crash mid-run offers to put its burns back.** The crash prompt reads
  the run's journal from disk and, before asking about the report, asks
  whether to roll the reversible burns back (`crash_rollback = ask`, the
  default); `always` does it unasked, `never` and a run without a terminal
  print the `cig unburn <run>` line instead. The report records
  `rolled_back`. (#24)
- **The pin.** A script says which cig it was written for: `# cig 1.1`
  under the shebang (any 1.1.x; `1.1.2` exactly; `>=1.1.2`), or a
  `.cig-version` file for a whole project. When the installed cig does not
  satisfy it, `run`, `--dry-run`, `check`, `light` and `chains` are handed
  to that version from the humidor and the report says so; with no such
  version at hand the run is refused (`E807`), never lit on the wrong cig.
  `cig pin <file>` writes the pin. (#22)
- **The humidor.** `~/.cigscript/humidor/<version>/cig`: other releases,
  fetched on demand and verified against their checksum (and the release
  attestation where `gh` is installed) before they are ever run. `cig
  versions` lists the shelf, `add`, `remove` and `prune` tend it; a
  `YANKED` file in the repository keeps a range pin off a bad version. (#22)
- **The since table and the trial.** Every builtin and clause knows the
  release it arrived in: `cig check` warns (`E309`) when a pinned script
  uses something newer than its pin allows, and `cig pin` infers the
  lowest version a script needs. Before a run on a cig a script has not
  been proved on, its plan here and its plan on its last good version (the
  newest run that went through, from the run history) are compared op by
  op: identical plans run here, different plans run on the proved version
  with the difference printed, and a run that fails here after burns that
  all came back is tried once more on the last good version. Never after an
  irreversible op. `trial = auto | always | never`, `--trial`,
  `--no-trial`. (#22)
- **`cig.version()`, `cig.os()`, `cig.arch()`, `cig.run_id()`.** A script
  can read the cig it runs under and the id of its own run, so a tool
  written in CigScript (cig-orchestrator) checks the cig it was handed and
  prints its own `cig unburn` line. (#22, cig-orchestrator FINDINGS)
- **Records know their format.** `run.json`, `after.json` and the first
  line of `journal.jsonl` carry `format: 1`. A newer cig reads older
  formats; an older cig refuses a newer one by its number (`E808`) and
  `cig unburn` hands the run to the cig that wrote it. (#22)
- **The tin.** A hop can say which files it may change (`proc.run(cmd,
  args, {touches: ["src/**"]})`); their before-bytes are kept outside the
  project, one object per distinct content, before the child runs, and a
  file it modified or deleted comes back on rollback, byte for byte and
  hash-verified, where it was listed as irreversible before. A same-size
  edit that puts the mtime back is seen by the hash. `cig tin [du|verify|gc]`.
  A `.cigignore` at a pack root, and the usual build and dependency trees,
  are never watched. `touches` without a pack is `E757`. (cig-orchestrator #18)

## 1.1.2 — 2026-10-24

Found by building [cig-orchestrator](https://github.com/otmof-ops/cig-orchestrator),
a tool written in CigScript that runs a project's existing scripts.

- Hop watching journals the directories a script creates inside the pack,
  not only the files, so a rollback or `cig unburn` removes `build/out/`
  instead of leaving it empty.
- `fs.glob("*")` no longer lists an empty string (the directory itself);
  `fs.glob("./scripts/*")` matches what `fs.glob("scripts/*")` does and keeps
  the `./` on its results; a pattern without `**` walks no deeper than its
  own components, so `*` at the top of a project no longer reads all of
  `node_modules`.
- A `;` may follow a block statement: `if c { }; x`, `burn { }; x`.

## 1.1.1 — 2026-09-28

The hardening round: 1.1.0 attacked from every side, the failures that
reproduced fixed, each with a row on the wall and a test named after it.

- Kernel: `after.json` fingerprints directories (names, kinds, sizes,
  mtimes), so a file added to a directory the run created is a change and
  `unburn` refuses with `E704` instead of removing the directory with the
  file in it. The pack check resolves symlinks before judging a path; a link
  inside the pack that points outside is outside, and the refusal says where
  the write would have landed. A journal whose last line a crash cut short
  loads up to the cut; `unburn` says so and restores what came before.
- Hops: stdin is written from its own thread beside the two readers (a child
  echoing 8 MB back deadlocked). SIGINT, SIGTERM and SIGHUP are forwarded
  to every live child process group before cig ends (a Ctrl-C left the child
  running, orphaned in its own group). `proc.pipe` no longer fails a
  pipeline because an upstream stage died of SIGPIPE when its consumer closed
  early (`yes | head -1`); the stage reports `closed_early` and `signal`. A
  missing or non-directory `cwd` is `E550`, naming the directory, not `E551`
  blaming the command.
- Language: brackets, parentheses and blocks nested deeper than 5,000 levels
  are `E208`, a diagnostic, instead of a stack overflow; a value that
  contains itself prints `…` at depth 512 instead of overflowing; a UTF-8
  byte-order mark at the top of a file is skipped instead of `E103`.
- Diagnostics: a source line longer than 120 characters is shown as a window
  around the caret with `…` at the cuts.
- Kernel, second round: `fs.cp` and `fs.mv` refuse a destination that is the
  source itself (the copy truncated the file to nothing and reported success)
  or lies inside it. A move the operating system refused (into its own
  subdirectory, onto a non-empty directory, a trailing slash) is marked
  undone in the journal instead of being "moved back" by rollback over the
  untouched source. A cross-device move is rolled back by copying, and the
  only copy is kept when a move back fails. A file with other hard links is
  restored in place, so the other names see the old bytes too. Setuid,
  setgid and sticky bits survive the snapshot and the restore, and a restored
  directory tree keeps the mode of its root. A named pipe, socket or device
  is refused (`E702`) instead of hanging the snapshot. What the automatic
  rollback could not restore is recorded on the run (`rollback_incomplete`,
  listed by `cig runs <id>`), and `cig unburn` checks it can record itself
  before restoring anything. A damaged or renamed run record no longer
  hides the run: it is listed as `damaged`, its journal is found where it
  is, and it can be unburned. A dead run's pid reused by another program no
  longer reads as `running`.
- Compensations: the `compensated` stamp goes on a block's ops only when
  the block completes and its compensation is recorded; a block that fails
  half-way, or a process that dies inside one, leaves them honestly
  irreversible. A block left by `snuff`, `break`, `continue` or `exit()`
  completed its effects and records its compensation (1.1.0 dropped it
  silently, so the undo never ran). A deferred `cig unburn` gives the
  compensation the run's `args`.
- Pack and ghost: a link inside the pack is followed when a hop's writes
  are watched; the dry run resolves a symlinked directory to the same ghost
  entry as its target; every directory a nested write creates is listed;
  an empty pack root is refused (`E755`) instead of silently meaning the
  whole working directory.
- Language: `-9223372036854775808 / -1` is `E503`, not a crash; `int()`
  refuses a float or numeric string that does not fit instead of
  saturating; floats past sixteen digits print in exponent form so they
  still read as floats; a value that contains itself is refused by
  `json.stringify` and compared without recursing forever; a redeclaration
  reached at run time is a check-family error with the checker's exit code,
  and `args` is a prelude name a script may shadow; a string that runs past
  the end of its line is `E101`; more than 10,000 chained operators are
  `E208`; an invalid `CIG_MAX_STEPS` is `E800`.
- Hops: `ok` and `check` resolve the same whatever their order (the
  contract could vanish depending on key order); an empty `ok` list and an
  empty `sep` are refused; a directory given as the command is named as
  one; `proc.kv` reads `export KEY=value`; `proc.which` resolves a name
  with a slash the way `proc.run` does.
- Chains: `light(steps, {names: [...]})` names the steps of a chain built at
  run time; the narration, the report and the error used to say `step 3`.
- `fs.append_text` creates the parent directory, as `fs.write_text` always
  did; a log under a directory that did not exist yet failed for real after
  a dry run that saw no problem.
- Plan: `env.set` and `env.unset` are labelled irreversible, as `BURN.md` and
  `STDLIB.md` have always said; 1.1.0 labelled them reversible although the
  kernel restores nothing for them.
- CLI: a malformed invocation exits 3 like every other usage problem; a
  script larger than 64 MiB, or a device like `/dev/zero`, is refused
  (`E801`) instead of read forever, and `E801` now carries the path for
  doctor; `cig crash send` honours `crash_reports = never`; the update
  downgrade refusal is a coded `E806`.

- Docs: the narrative pages (`DESIGN.md`, `SMOKE.md`, `LANGUAGE.md`,
  `CHAINS.md`, `BURN.md`, `DOCTOR.md`, `SCIENCE.md`, the README's burn-model
  table and the `cig language` legend) say what 1.1.0 does; the last claims
  from before the Hammer update (a dry run that cannot read its writes, a
  self-lighting chain that fails at depth 32, "no `finally` yet", packs as
  something that was cut) are gone.

## 1.1.0 — 2026-09-27

The "Hammer" update: built from an external adversarial read of 1.0.0 and
delivered from its own package (`HAMMER.md`), one pull request per seam
(#1 to #9); the package's manifest carries the state of each deliverable.
Sixteen of its eighteen deliverables ship here; foreign scripts and the
browser playground are the next update, as the package itself frames them.

### Language
- **`finally`.** `try { } ashtray e { } finally { }`, or `try { } finally { }`
  with no `ashtray`: the block runs whatever happened (a normal exit, a caught
  or uncaught error, `snuff`, `break`, `continue`, `exit()`); its own signal
  wins, otherwise the pending one carries on with its payload. `finally` is
  still usable as a map key or member name, like every keyword.
- **The step budget.** Every statement and loop iteration is a step; ten
  million by default (`--max-steps N` on `run` and `light`, `CIG_MAX_STEPS=N`,
  `0` disables). A loop that never ends now ends: `E515` *your loop never
  ends*, pointing at the `while`, and doctor reads the condition (`true`, so
  only a break can end it). Burns before the budget ran out are rolled back.

### Kernel
- **The ghost filesystem.** In a dry-run the pretend writes live in an
  in-memory overlay and every read consults it before the disk (`fs.read_*`,
  `fs.exists`, `fs.is_*`, `fs.size`, `fs.modified_ms`, `fs.list`, `fs.glob`,
  `json.load`, `csv.read`, `hash.sha256_file`). A chain whose step 2 reads
  step 1's write now passes `--dry-run` with the disk untouched. A simulated
  delete or move leaves a tombstone: a later read is `E520`, naming the op
  that removed it, and doctor points at the plan. A simulated `fs.cp`,
  `fs.mv` or `fs.rm` of a path that exists nowhere fails in the plan as it
  would for real. `burn unlit` never enters the ghost.

### The pack and compensations
- **`pack { "./build" }`** declares the scope every native write must stay
  in: literal paths outside it are refused by the checker (`E750`), computed
  ones by the kernel (`E751`), the plan prints the pack, and one pack per
  script, first (`E753`). Hops inside a pack are watched: the files a child
  created are journaled as reversible and removed on rollback, files it
  modified or deleted are listed as irreversible with the detail, and
  writes outside the pack are reported (`E752`), never hidden.
- **`burn (s) { } unburn { }`**: the compensation for what the kernel cannot
  see. Journaled with its state map when the block completes; run on
  rollback here or by `cig unburn` later, from the journal alone (so it may
  use only its state, modules and builtins: `E754`). Hops inside show as
  `compensated`, not `irreversible`.
- The plan tags every op with the chain and step that own it
  (`[release/build]`); a chain that lights itself is `E604` with the loop.

### Hops
- **The boundary holds.** `proc.run` keeps one call shape and gains the
  rules: exit codes are a contract (`ok: [0, 1]`, `check` is `[0]`) and a
  code outside it is `E553` naming the command, the code, the contract and
  stderr's last line; every child runs in its own process group and a
  timeout kills the group, SIGTERM then SIGKILL after `grace_ms` (`E554`,
  with how much it had written); a child killed by a signal is `E555`, not
  a `-1` result; output is decoded explicitly (`encoding`: strict UTF-8,
  `lossy`, `latin1`, `utf-16`) and bad bytes are `E557`; a program that is
  not there is `E551`, one that may not be executed `E552`. Children get
  `NO_COLOR=1` and `CIG_RUN_ID`; `clean_env` starts from a documented
  minimum.
- **Parsing verbs.** `proc.text`, `proc.lines`, `proc.json`, `proc.csv` and
  `proc.kv` read stdout only under the contract `[0]` and fail at the hop
  with `E556`, quoting the first 200 bytes of a bad shape.
- **`proc.pipe`** connects stages without a shell, one group, one timeout,
  every stage checked and the failing stage named. **`proc.shell`** is the
  shell, by name; `cig check` warns (`E308`) when one is smuggled through
  `proc.run("sh", ["-c", built])`.
- **Retries are idempotence-aware.** A chain retry rolls back what the
  failed attempt burned first (the journal marks those ops undone and
  `unburn` skips them), and a step whose attempt ran a hop is not retried
  unless `retry_irreversible: true`.
- `E603` is retired in favour of `E553`; the ashtray value is unchanged.

### Fixed
- A deleted or overwritten symlink now rolls back as a symlink with its
  original target, dangling or not, including links inside a restored
  directory. Writing through a link records the file the bytes land in, so
  that file is restored too. In 1.0.0 the journal could not snapshot a link at
  all and refused the burn with `E702`.
- A run whose process died mid-burn was invisible: status `running`, zero
  burns, nothing from `doctor`. It is now shown as `interrupted`, counted from
  the journal, reported by `cig doctor` (`E706`) and offered for `cig unburn`
  under `--fix`.
- `-9223372036854775808` (`i64::MIN`) parses; the magnitude without the sign
  is `E104` with a hint.
- A chain failure points at the step as written, not at the `light()` call,
  and never at `file:0:0` when lit from the CLI; the message also names the
  line the underlying error was raised on.
- A rollback that failed half-way through restoring a file could remove the
  live file first. Files and directories are now staged beside the path and
  swapped in.
- `fs.exists` sees a dangling symlink, so `if fs.exists(p) { fs.rm(p) }` works.

### Added
- A property test for rollback: random op sequences (writes, appends,
  deletes, moves, copies, mkdirs) over random trees with regular files, modes
  and live and dangling symlinks; the tree after rollback must be identical to
  the tree before, content, names, modes and link-ness included.
- Journal durability: snapshot, snapshot directory and journal entry are
  synced to disk before the effect. Measured at about 1 ms per journaled op
  (see `docs/BURN.md`).
- An allocation ceiling (256 MiB, `CIG_MAX_ALLOC` to change it): `repeat`,
  `*` on strings, padding, and `fs.read_text`/`fs.read_lines`/`json.load`/
  `csv.read` of an oversized file are refused with `E514` instead of aborting
  the process.
- **Unburn knows what changed since.** A finished run records the state of
  every path it touched (`after.json`); `cig unburn` compares it with the
  disk and refuses with `E704` when a file changed since, naming the later
  run that touched it when there is one, so runs are put back newest first.
  `--force` overrides; `--dry-run` shows a *changed since* column beside each
  entry; a run from before after-state records says "unknown" and is allowed.
- `cig unburn` checks every snapshot and moved file before touching anything
  (`E703`), refuses a second rollback of the same run (`E705`), and takes
  `--force` for both; `--dry-run` lists the problems it found.
- Reserved error-code sub-ranges: E51x runtime, E52x ghost filesystem, E55x
  hops, E70x kernel and journal, E75x folds and packs, E80x usage, E85x
  updater and doctor. New codes: `E514`, `E515`, `E520`, `E703`–`E706`.
- `run.json` records the pid of the run.
- The error-code registry is data: `errors/registry.toml` is the one place a
  code is defined, with its family and kind, which of the four kinds of no it
  is (the lexicon's vocabulary), where it arises, its ranked causes each with
  a named read-only probe and a remedy, and its related codes. `cig explain`
  renders it, `--json explain` prints the nodes, `cig explain --schema` prints
  the JSON Schema for a diagnostic, and `docs/ERRORS.md` and
  `docs/diagnostic.schema.json` are generated from it. `tests/registry.rs`
  fails when the source emits a code the registry lacks, when the registry
  holds a code nothing emits (unless tagged `planned`), when a page is
  incomplete, or when the generated docs are stale; the explain pages are
  golden-tested so wording cannot regress silently.
- Doctor fires on the emit hook. Under every reported diagnostic, doctor
  walks the code's ranked causes, runs the read-only probe for each (stat,
  mode bits, PATH, the journal, the run records; never a write, never the
  network) and says the one sentence it is allowed: *I think X, because I
  checked Y and found Z*, then the fix, then what to check if that is not it.
  Nothing confirmed: one line. Nothing to check: silence. 80 ms budget.
  `--no-doctor` or `CIG_DOCTOR=0` gives the bare diagnostic, byte for byte.
  In `--json` the diagnosis is a field. `cig doctor <run>` diagnoses a stored
  run's error again, read-only (the run record now keeps the diagnostic).
  `tests/corpus/` pins the words for every probe. `docs/DOCTOR.md`.
- `--plain` / `CIG_PLAIN=1`: the same codes, spans, hints and evidence
  without the banner or the hallway phrases; the manual's name stands in.
  Golden-tested as a strict subtraction of the themed output.
- Diagnostics carry a `subject` field in `--json`: the path, program or run
  id the message is about.
- `cig check --deny-warnings` for CI: exit 2 when there are warnings.
- **The wall, and the invitation to break it.** The README's "Break it"
  section and the wall: every cheat tried so far, what happened, and the
  test named after it; `tests/docs.rs` refuses a row whose test does not
  exist, and `tests/wall.rs` holds the rows nothing else covered.
- **`cig report <run>`: the bundle you attach.** One redacted JSON file
  (home paths `~`, secret-shaped values removed, file contents never
  included) with the run, the journal and its before-states, the intents,
  the diagnostic, doctor's diagnosis run again, and the install facts. The
  bug that does not crash is now as easy to send as a panic.
- Issue templates for a cheat that worked (four fields) and a coined term
  (the manual's name required), a pull-request template that asks for the
  test named after the cheat, and the `cheat` and `coined-a-term` labels.
- **The voice, everywhere.** The branding statement and the four kinds of
  no on the README, a hallway column in the lexicon table, the tone
  section in `docs/DESIGN.md`, hallway names with the manual's word attached
  in `BURN.md`, `LANGUAGE.md`, `CHAINS.md`, `STDLIB.md` (hops) and the CLI
  `--help` (`unburn: put the world back the way it was, newest op first
  (rollback)`). Hints reshaped to what to type next where they described
  the problem instead. `docs/LEXICON.md` is the source of truth.
- The usage layer carries its codes: `E801` cannot read script, `E802` state
  directory unavailable, `E803` no such run, and `E805` (no `gh` or `curl` at
  all) is told apart from `E806` (the transport failed).

## 1.0.0 — 2026-09-26

First public release.

### Language
- One syntax: `roll`, `stick`, `pull`, `pack`, `snuff`, `exhale`, `cough`,
  `try`/`ashtray`, `burn`, `burn unlit`, `chain`, plus plain `if`/`else`,
  `while`, `for`/`in`, `break`, `continue`, `and`/`or`/`not`. Keywords may be
  used as member names and map keys.
- Values: null, bool, 64-bit int with checked arithmetic, float, UTF-8 string
  with `${}` interpolation and raw `'...'` form, list, insertion-ordered map,
  closures, chains.
- Operators: arithmetic, comparison, `??`, `?.`, `..` ranges, `>>` pipeline,
  compound assignment, indexing with negative offsets, method calls on
  strings, lists and maps.
- Errors carry a stable code, kind, message, line and any keys from a coughed
  map; everything is catchable except `exit`.

### Kernel
- Every library function declares an effect. Write, process and environment
  effects are refused outside a `burn` block, statically where visible and at
  run time always.
- `cig run` journals each burn with before-state snapshots and rolls back on
  failure; `cig unburn <id>` rolls back a finished run; `--no-rollback` opts
  out. `cig run --dry-run` simulates every burn and prints the plan without
  writing anything, not even a run record. `burn unlit` records intents.
- Run records under `$CIGSCRIPT_HOME/runs/<id>/` (default `~/.cigscript`).

### Chains
- `chain name { step, step }` declares an ordered list of sticks;
  `light(chain, opts?)` runs them with timing, retries, value passing and a
  report, stopping at the first failure. `cig chains <file>` lists them,
  `cig light <file> <chain>` runs one under the same dry-run and rollback
  rules as everything else.

### Standard library
- `fs`, `path`, `json`, `text` (regex and templates), `proc` (no shell, timeouts,
  no pipe deadlocks), `time`, `hash`, `math`, `env`, `csv`, `log`; globals
  `len`, `str`, `int`, `float`, `bool`, `type_of`, `range`, `keys`, `values`,
  `entries`, `assert`, `exit`, `stdin`, `light`; 64 methods on strings, lists
  and maps.

### Tooling
- `cig check` with name resolution, stick reassignment, `snuff`/`break`
  placement, module member typos and effect placement, with `did you mean` hints.
- Stable error codes E1xx to E9xx, `cig explain <code>`, generated `docs/ERRORS.md`.
- Crash reports: written locally on a panic, redacted, shown in full by
  `cig crash show`, filed only when you say yes; `crash_reports = ask|always|never`.
- `cig update` (SHA-256 verified, atomic swap, no downgrades, no untrusted
  install directories), `cig doctor --fix`, `install.sh` with a dependency
  preflight and no sudo, `cig config`, `--json` on every command, stable exit codes.

### Licensing
- Apache-2.0 code (canonical text), CC BY 4.0 documentation, CC0 1.0 examples,
  NOTICE, TRADEMARKS.md, a contributor licence agreement, THIRD-PARTY-NOTICES.md,
  REUSE.toml with SPDX headers, SECURITY.md.
