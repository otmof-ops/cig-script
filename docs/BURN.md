# Burns: effects, modes, the journal and rollback

<p align="center"><img src="assets/burn-model.svg" alt="The burn model" width="820"></p>

The runtime is split into a language, which is pure, and a kernel, which owns
every side effect. This document is the contract for the kernel. The words,
with the manual's name attached (all of them in [LEXICON.md](LEXICON.md)):
*the kernel* (the effect executor, the one piece of code allowed to touch the
world), *a snapshot* (a before-image: a copy taken before the op that will
change it), *the journal* (a write-ahead log: written before each op, so a
kill mid-burn still leaves a record), *newest first* (the order rollback
replays it), *reversible* (the kernel did it itself and has the snapshot to
prove it) and *irreversible* (something the kernel could not watch, and the
plan says so out loud).

## Effects

Every standard-library function declares one effect:

| effect | examples | needs a burn |
|---|---|---|
| pure | `len`, `json.parse`, `text.replace`, all methods | no |
| read | `fs.read_text`, `fs.glob`, `env.get`, `time.now_ms`, `hash.sha256_file` | no |
| write | `fs.write_text`, `fs.rm`, `fs.mv`, `json.save`, `csv.write` | yes |
| proc | `proc.run` | yes |
| env | `env.set`, `env.unset` | yes |

Reads are free because a dry run has to be able to look at the world to decide
what it would do. Writes, processes and environment changes are the world
changing, and they are only allowed inside a `burn { }` block.

The rule is enforced twice. `cig check` (which `cig run` performs first) walks
the script and refuses any effectful call that is syntactically outside a burn
at the top level, and warns about one inside a function body. At run time every
effectful call asks the kernel for permission, and the kernel refuses unless a
burn block is active on the call stack. A function called from inside a burn
may burn.

## Modes

| mode | effectful calls | run record |
|---|---|---|
| `cig run` | executed after being journaled | `~/.cigscript/runs/<id>/` |
| `cig run --dry-run` | simulated, collected into a plan printed at the end; reads see the pretend writes through the ghost filesystem | none |
| `cig check`, `cig eval`, `cig repl` | `check` never executes; `eval` and `repl` behave like `run` without a run directory | none |

Inside `burn unlit { }` every effectful call is simulated whatever the mode,
and the call is recorded as an *intent* rather than a plan entry. Intents are
printed after the run and saved to `intents.jsonl` in the run directory.

A simulated call returns a plausible value so the script can continue:
`fs.write_text` returns the byte count, `proc.run` returns
`{code: 0, out: "", err: "", simulated: true, ...}`.

**The ghost filesystem.** In a dry-run the pretend writes live in memory (an
overlay; a shadow filesystem), and every read consults it before the disk:
`fs.read_text`, `fs.read_lines`, `fs.exists`, `fs.is_file`, `fs.is_dir`,
`fs.size`, `fs.modified_ms`, `fs.list`, `fs.glob`, `json.load`, `csv.read`
and `hash.sha256_file`. A step that reads what an earlier step wrote works in
the dry-run exactly as it will for real; a simulated `fs.rm` or `fs.mv`
leaves a tombstone, so a later read of that path is `E520` ("removed earlier
in this dry-run by op 3 (delete notes.txt)") rather than a misleading
not-found, and a simulated `fs.cp`, `fs.mv` or `fs.rm` of a path that exists
nowhere fails in the plan the way it would fail for real. What the ghost
cannot see is what a hop would have produced: `proc.run` is simulated as an
empty success, so a step that reads a program's output file will not find it
in a dry-run. `burn unlit` never enters the ghost, in any mode: rehearsal
changes nothing, not even a pretend disk.

## The journal

Before an executed op touches the disk, the kernel writes a journal entry with
the state of every path the op will change:

| op | recorded before-state |
|---|---|
| write, append | the path: absent, a file (copied to a snapshot with its SHA-256), or a symlink (its target); if the path is a symlink, also the place the bytes land |
| delete, mkdir | the path: absent, a file, a directory (copied as a tree, links kept as links), or a symlink |
| copy | the destination, as for a write |
| move | the source as "moved to", and the destination |
| proc, env | nothing; these are marked irreversible |

A symlink is always recorded as a link, whatever it points at and whether or
not the target exists, and it is restored as a link. Writing through a link
changes the file it points at, so both the link and that file are recorded.

The journal is `journal.jsonl`, one entry per line. Snapshots live in
`snapshots/` next to it. A last line cut short by a crash mid-append does
not spoil the rest: `cig unburn` loads the entries before it, says the
journal was cut, and restores them. An op the operating system refused
before anything changed (a rename into its own subdirectory) is marked
undone with the same kind of marker, so rollback never "moves back" a file
that never left. A path that is not a file, a directory or a link (a named
pipe, a socket, a device) cannot be snapshotted and the op is refused
(`E702`) rather than hung on. If the kernel cannot journal an op (disk full,
permissions), the op is refused and nothing is changed.

**Durability.** The journal is write-ahead: the snapshot is copied and synced
to disk, the snapshot directory is synced, then the entry is appended and
synced, and only then does the effect happen. A process killed at any point,
or a machine that loses power, leaves a journal that `cig unburn` can replay.
The cost is measured, not guessed: 500 small `fs.write_text` burns in one run
on an ext4 NVMe laptop take about 0.48 s (roughly 1 ms per op), against
0.02 s in 1.0.0, which flushed but never synced. Reads, dry runs and pure
code pay nothing.

Directory deletes snapshot the whole tree, so deleting a large directory costs
a copy of it. Moves cost nothing: rollback moves the file back.

### Durability, by platform

The journal is write-ahead: the snapshot and the entry are flushed before the
effect happens, so a crash between them leaves a record that `cig unburn`
can act on. What "flushed" means is the platform's:

- **Linux and the BSDs:** `fsync` on the snapshot, `fdatasync` on the
  appended entry, `fsync` on the directory that gained the file.
- **macOS:** `fsync` stops at the drive's cache; every flush here is
  `F_FULLFSYNC` (what the standard library's `sync_all` already is on Apple
  targets), which asks the drive to write it out, with `fsync` as the
  fallback on a filesystem that refuses it.
- **Windows:** `FlushFileBuffers` on the snapshot and the entry; the
  directory is flushed through a handle opened with backup semantics where
  NTFS allows it, and left to the filesystem's own journal where it does not.

## Rollback

Rollback replays the journal newest entry first. For each recorded before-state:

- *absent* → the path is removed if it now exists;
- *file* → the snapshot is copied to a staging file beside the path and swapped
  in, so a failure half-way leaves what is there untouched;
- *directory* → the snapshot tree is staged beside the path and swapped in;
- *symlink* → a link to the recorded target is recreated;
- *moved to X* → X is renamed back to the original path.

Irreversible ops (processes, environment changes) are listed under "cannot
undo" so nothing is hidden. Rollback happens automatically when a `cig run`
fails with an uncaught error (disable with `--no-rollback`), and on demand with
`cig unburn <id>`, which works on a run that finished successfully too. The run
record's status becomes `rolled_back` or `unburned`.

`cig unburn` checks before it touches anything. Every snapshot the journal
needs must exist and match its recorded SHA-256, and every moved file must
still be where the run put it (or be put back by a later entry's own
rollback). If anything is missing, the whole rollback is refused with `E703`
and nothing is restored; `--dry-run` lists each problem, and `--force`
restores what can be restored and reports the rest.

`cig unburn` is idempotent: a run that was rolled back already is refused with
`E705`, because restoring the old snapshots a second time would overwrite
whatever happened since. `--force` overrides that too, deliberately. A
rollback that left some restores failed is the exception: a plain
`cig unburn <id>` retries those alone and leaves what was put back as it is.

**What a rollback displaces is kept.** A hop's write set was observed, not
performed, so the kernel cannot tell the hop's bytes from a save made while
the hop ran. For a hop entry, a file the rollback removes or rewrites is
moved into the tin first, the report line says `its bytes went to the tin`,
`cig runs <id>` lists them under `took`, and `cig unburn <id> --give-back`
puts them back, moving what is there now into the tin in turn, so the step
can be reversed again; `--give-back --dry-run` lists what it would put back
and what is there now, and a path changed since the rollback is refused
without `--force` (`E704`). A directory a hop made is removed only when it is
empty: what was put in it since stays, and so does it. A file the hop left
that changed again after the hop ended is left alone, listed with the
`--force` that restores over it. `cig unburn` holds the pack's lock while
it works, as a run does (`E759` for a run that is using it).

**A stretch of history (1.17.0).** `cig unburn --last N` puts back the
newest N runs that changed something, `--since 2h` every run started in the
last two hours (`90s`, `30m`, `3d`, `1w`), and `--to <id>` every run after
that one, which stays. `--pack DIR` keeps to the runs that worked in that
folder. A stretch takes each run's own work: a child goes back through its
parent, and a run that burned nothing, or is back already, is skipped. It
shows one plan first (the runs, the ops, the files, what is irreversible),
asks at a terminal (`-y` does not), and `--dry-run` stops at the plan.
Then each run is put back as `cig unburn <id>` would, with the same checks,
newest first. The first run refused stops the stretch: the runs after it stay
undone, it and the ones before it are untouched, and the message names it and
why. Once that is resolved, `cig unburn --continue` takes up the same stretch,
and a run back already is never undone twice. A stretch will not start under a
run that is still running.

**Interrupted runs.** A run whose process died mid-burn is left with the
status `running` and no totals. `cig runs` recognises it (the pid is gone,
the journal is intact), shows it as `interrupted` with its burn count taken
from the journal, and `cig doctor` reports it with `E706` and, under `--fix`,
offers `cig unburn <id>`.


**A file another program holds open (Windows).** A restore stages the
before-image beside the path and renames it into place, which Windows
refuses while another program holds the file without delete sharing (an
editor, an antivirus scanner, an indexer). The swap is retried for about a
second and a half, then the content is written back in place through a
handle the other program's sharing does allow, and the restore line says
`in place: another program held it open`. When even that is refused, the
run stays `rollback_incomplete` and the line says to close the program,
after which `cig unburn <run>` finishes the job.
## The pack, hops, and compensations

A **pack** (`pack { "./build" }`, `LANGUAGE.md`) is the declared scope: the
kernel refuses any native write outside its roots (`E751`; the checker
catches literal paths first, `E750`) and prints the pack in the plan header.
Paths are resolved before they are judged: a symlink inside the pack that
points outside is outside, and the refusal says where the write would land.
The same holds for a hop: a link inside the pack that leads outside it is a
door, not a part of the pack, so nothing behind it is walked or journaled,
a write through it is reported as outside the pack (`E752`), and a rollback
never removes a path whose real place is outside the pack. Where a door
leads is watched shallowly (three levels, five thousand entries); a door
to the pack's own ancestors (a Wine prefix's `z:` leads to `/`) or into
`/proc`, `/sys`, `/dev` or `/run` is not watched, since the machine changes
there by itself, and past the cap only what both walks saw is compared. An empty root
is refused (`E755`): it would have meant the whole working directory.
A **hop** inside a pack is *observed*: the pack's files (mtime, size,
mode), directories (mode) and links (target, and what the link leads to)
are listed before the child runs and again after, and the difference is
journaled as the hop's write set: files the child created are reversible
(`hop created`, rollback removes them), files it modified or deleted are
irreversible with the detail (`hop modified`, `hop deleted`) unless the tin
kept them, a directory it deleted or replaced with a file is made again on
rollback (`(directory)`), and changes in the child's working directory
outside the pack are reported as `E752`. The scan skips `.git`,
`node_modules`, `target` and `.cigscript`, never hashes content, reads the
tree on every core into a compact sorted list (about 60 bytes an entry, so a
million entries is a laptop's business), reuses the scan before the hop for
every directory whose modification time has not moved, and stops only at
`watch_cap` entries (five million, a memory guard): past the cap it warns up
front, journals nothing as created and asks the disk before calling a file
deleted, so a rollback never removes a file that existed before the run. One run at a
time holds a pack: a second run on the same or an overlapping pack is
refused with `E759` while the first runs, because hop watching would read
its writes as the hop's.

A **compensation** is the other half. `burn (s) { ... } unburn { ... }`
journals the `unburn` block's source and the state map `s` when the block
completes (`Op::Compensate`, reversible: it *is* the reversal), and only
then stamps the block's irreversible ops `compensated`, in the journal as
on disk. Rollback,
here or in `cig unburn` later, runs each compensation at its place in the
newest-first order, in a fresh interpreter with effects allowed and nothing
journaled. Hops inside such a block are labelled `compensated` in the plan,
the journal and the rollback report, never `irreversible`; a block that
fails half-way records no compensation, so its hops stay honestly
irreversible. No pack reverses `git push`; that is what the compensation is
for, and the pack and the compensation are two halves of one feature. A
hop can supply the compensation itself through the burn protocol
([PROTOCOL.md](PROTOCOL.md)): a `compensate` line in `CIG_JOURNAL` is
journaled like a block's `unburn` and runs the same way, as a command.

## Chains

A chain (`docs/CHAINS.md`) adds nothing to this model and needs nothing from
it: its steps burn like any code, so `cig light --dry-run` plans every step,
a failed chain rolls back everything its earlier steps burned, and
`cig unburn` reverses a chain that finished. The chain report records timing
per step; the journal records the effects.

## Run records

```
~/.cigscript/runs/20260926T021500-3fa9c1/
  run.json        script, its SHA-256, args, cwd, pid, timings, status, burn counts, the failing diagnostic
  journal.jsonl   executed ops with before-states; compensations; `undone` markers left by a retry
  snapshots/      copies of files and trees as they were before each op
  after.json      what the run left behind, for the changed-since check on unburn
  intents.jsonl   unlit burns (only when there were any)
```

Beside `runs/`, the state directory holds `ashtray/` (every report cig made
about its own bugs, yours until you send them; `crashes/` from before 1.7.0
is read too), `reports/` (`cig report <run>` bundles), `humidor/`
(every other version of cig kept for pinned scripts and old journals, one
directory per version with `index.json` beside them; see `cig versions`),
`tin/` (below) and `update-check.json`.

## The tin

`~/.cigscript/tin/objects/<sha256>` holds the before-bytes of files a hop
may change, outside the project where `git clean` or a task gone wrong
cannot reach them. A hop with `touches` has every existing file under its
globs stashed there before it runs, one object per distinct content, so a
formatter that changes three lines of a file stores that file about once
and fifty runs of the same chain store it once. On rollback a modified or
deleted file comes back from its object, which is re-hashed against its
name first: a corrupt object is refused, never written.

**Chunks and recipes.** A file of 64 KiB or more is not one object but a
*recipe* (`tin/recipes/<sha256 of the file>`): the ordered list of its
chunks, each an object named by its own hash, with the hash and size of
the whole. The chunks are content-defined (FastCDC, 16 KiB at least, 64 KiB
on average, 256 KiB at most), so their boundaries depend on the bytes and
not on offsets: a one-byte change in a 200 MB lockfile stores the chunk it
landed in, an insert at the start moves one boundary, and fifty runs that
each change the file a little cost a tin a small multiple of what changed.
A restore streams the chunks into a staging file beside the destination,
checking each against its name and the whole against the hash the journal
recorded, and a missing or corrupt chunk refuses the restore before
anything at the path is touched. `cig tin` reports the bytes stored next to
the bytes the kept files would take written out, so the saving is visible;
`cig tin verify` checks every recipe names chunks the tin holds; `gc` keeps
every chunk a live recipe names. Smaller files cost exactly what they did
before, and a large file a 1.4 cig kept whole restores as it always did.
Measured in [FINDINGS.md](FINDINGS.md): twenty one-byte edits of a 200 MB
file cost the tin 202 MB, where 1.4 stored 200 MB a run.

**Sealed.** A tin that is on for a whole project would keep every old
`.env`, private key and kubeconfig it ever saw in the clear, for as long as
a run refers to it. So a secret is sealed: encrypted with
XChaCha20-Poly1305 in the STREAM construction (the `chacha20poly1305`
crate; nothing home-made), a megabyte a segment, every segment
authenticated and the last one marked, so a byte flipped, a tail cut off or
a different key is refused on read and nothing is written. A sealed object
(`tin/sealed/<random id>`) is named by a random id, bound to that name, and
never shared between files, so two equal secrets do not reveal that they
are equal; the journal keeps the plaintext's hash and size, so the bytes
are checked on the way back whatever the ciphertext says. What is a secret:
a built-in list of names (`.env` and `.env.*`, `*.pem`, `*.key`, `id_rsa*`,
`id_ed25519*`, `id_ecdsa*`, `id_dsa*`, `*.p12`, `*.pfx`, `credentials*`,
`*secret*`, `.npmrc`, `.pypirc`, `kubeconfig`, `*.kdbx`, `.netrc`,
`.git-credentials`, `*.ppk`, `*.jks`, `*.keystore`), whatever a pack root's
`.cigseal` lists (one glob per line, like `.cigignore`, applying to the tree
below it), a hop's `touches_sealed: ["data/*.db"]` (its files are kept, and
kept sealed), and a native write that says so,
`fs.write_text(p, t, {sensitive: true})`. The `seal` setting is `secrets`
by default, `all` seals everything the tin keeps, `never` turns it off. The
run directory's own snapshots of secrets are sealed the same way
(`<name>.cig-sealed`, the mode in the journal), a secret inside a tree
snapshot too, and what a rollback displaces from a sealed path stays
sealed. The key is 32 random bytes made on first use and kept in the OS
keyring (Secret Service on Linux and the BSDs, Keychain on macOS, Credential
Manager on Windows) under `cigscript` / `tin-seal`; where no keyring
answers within five seconds, in `~/.cigscript/tin/seal.key`, readable by
its owner alone, and a note says so once (`seal_key = file` makes the file
the rule, and tests use it). The plan says `(sealed)` and counts sealed
files in a hop's `touches` line; `cig runs <id>` labels the entry
`(sealed)`; `cig tin` counts sealed objects apart; `cig tin verify` opens
every one with the key (`--quick` checks the shape alone). When the key is
gone while sealed objects or snapshots depend on it, no new key is made
behind your back: a restore refuses naming the key and where it was
(`E703`), a run that would seal says so and keeps the change irreversible
(`E757`), and the message says what to do, put the key back or move the
sealed objects away to start over; on a machine with nothing sealed (a new
one) the first use makes the key. A key file found when the setting asks
for the keyring is used, never shadowed by a fresh keyring key. `cig tin gc
--sealed-orphans` clears the sealed objects the key does not open (a parcel
imported with `--allow-sealed-missing` leaves such), and refuses to guess
while the key itself is missing. In memory the key lives in a type that
is not `Copy` and whose `Drop` writes zeros over the bytes through a
volatile store, so a move leaves no stack copy behind and the allocator
cannot hand the bytes out again (a compile-time assertion refuses a `Copy`
impl on it).

**Rekeying.** `cig tin rekey` makes a fresh master key and walks every
sealed object the tin holds and every sealed snapshot a run keeps,
re-sealing each under the new key; afterwards the keeper (the keyring or
the key file, whichever `seal_key` says) carries the new key. A rekey
waits for no one and refuses while a run is in progress: every cig
process that will seal or sign takes the `rekey.lock` file's shared
lock at its first key read and keeps it for the lifetime of the
process; `cig tin rekey` then tries that lock in exclusive
non-blocking mode and refuses with `E720` as soon as any live run
holds it, saying so. A run started while a rekey holds the exclusive
lock blocks on the same key read (with one note on stderr) until the
rekey finishes, and then uses the new key: nothing is ever signed or
sealed under a key the rekey is about to retire. A second
`cig tin rekey` while one is held is refused with the same code,
saying "another rekey". Each object is re-sealed with the plaintext
kept in memory, never beside the file; the result is written to a
`.rekey` temp beside the original, decrypted with the new key to
verify, then renamed onto it. The walk is crash-safe: while it runs,
the old key sits at `seal.key.old` and the new one at `seal.key.new`
beside the keeper's primary file, with a `rekey.in-progress` marker
next to them; so any sealed object is readable under one of the
stashes no matter when a crash lands, and `cig tin verify` is quiet.
A second `cig tin rekey` after a crash picks up where the walk left
off and finishes without starting over. On resume, the keeper's
current key must equal one of the stash keys (a constant-time
compare): legitimate mid-rekey state always satisfies it; a planted
`seal.key.old`/`seal.key.new` pair from someone who does not know the
primary key cannot, so a planted pair never makes the rekey adopt an
attacker's key. The stashes are honoured as fallback keys only under
the same constant-time match (and only while the marker is on disk
and each stash is a private 0600 regular file), so a planted marker +
stash cannot make a journal, ledger line or sealed object signed under
an attacker's key verify or open either. Before anything is re-signed,
every format-3 journal and the ledger are verified under the current
key: a chain that does not fit, a journal signed by another key or one
that cannot be verified under any key this tin has refuses the rekey
with `E720` naming the run, and nothing on disk is changed (so
`cig tin verify` after keeps reporting the same break — nothing is
laundered). Unsigned (format-2) journals are left as they are, never
quietly upgraded to signed. The signed journals and the ledger are
re-signed under the new master's subkeys in the same pass; the ledger
rewrites `head` only on the last record for each run, not every line
(an older line keeps its historical head). Note: a ledger `head` kept
somewhere else stops matching after a rekey, since the subkey
changed, and `cig tin verify --head` needs the value from
`cig tin head` taken after the rekey. The keeper swap goes last and
is read back to prove it carries the new key: when it does not, both
stashes stay on disk, every sealed object is still readable, and a
later `cig tin rekey` finishes the job. The stashes are wiped and
unlinked on a clean finish, the fsyncs along the way are checked
(nothing is swallowed), and a leftover is reported, never ignored.
Refuses when the current key is not available (`E720`) or when the
key was handed over on a descriptor for the run only (same code:
there is no keeper to swap). Carrying the key to another machine by
hand: `cig tin key export` and `cig tin key import`, or `cig parcel
--with-key` below.

**Carrying a sealed run.** A parcel copies the sealed objects a run's
journal refers to as-is: they open on the other machine only when it has
the sealing machine's key. `cig parcel --with-key` is the shortcut: the
parcel's manifest also carries the 32-byte master key in hex. The parcel
file lands at mode 0600 — it is a secret, and the on-disk permissions
say so. `cig unparcel` reads the carried key, validates every sealed
object in the archive against it (not just a sample: one sealed object
not under the carried key refuses the whole parcel), re-seals every
sealed object and snapshot under the local machine's key in the staged
directory before placing the run, so either the whole import lands or
nothing does. The trade is that the parcel is a secret and the key
rides inside it: accepting one takes an explicit opt-in. `cig unparcel`
refuses a `--with-key` parcel unless `--accept-key` is passed, naming
the fingerprint computed from the carried key bytes (not the
manifest's own claim), so a malicious parcel cannot slip its own key
onto the machine silently and the operator sees what they are
accepting; the same fingerprint is printed when the key is accepted,
so a sender's expected fingerprint can be checked against it. The
raw key never leaves this process: `--json` output of both
`cig parcel` and `cig unparcel --show` strips the `sealing_key` field
from the manifest and carries `sealing_key_fingerprint` in its place.
`cig parcel` prints a warning when it writes a `--with-key` parcel;
`cig unparcel --show` says `key inside the parcel` when one is there.
A parcel without the key, on a machine with no matching key, is
refused as before unless `--allow-sealed-missing` is passed. `cig
parcel --with-key` with no readable key refuses outright and writes
nothing — rather than quietly write a parcel that nobody could have
opened.

**Git blob reuse.** Most files a hop touches in a git project are tracked
and unchanged since the last commit, and git already holds their bytes in
`.git/objects`. Such a file (tracked, and clean against the index: the
bytes on disk hash to the blob the index names, by git's own blob hash) is
not copied into the tin: the journal records the repository, the blob and
our SHA-256 (`(git)` in the record), and a restore reads the blob back
through `git cat-file`, checks it against that hash, and refuses other
bytes the way a corrupt tin object is refused. A blob reachable only from a
commit that is later rebased away could be collected by `git gc`, so the
run pins every blob it refers to under `refs/cig/runs/<run id>`, one tree
naming them all, until `cig runs --prune` drops the record and the pin with
it (`git-pins.txt` in the run directory names the repositories). That is
the choice over falling back to a copy when the blob is unreachable from
`HEAD`: reachability changes after the fact, a pin does not. A dirty or
untracked file goes to the tin as before, so does every file in a
repository the run cannot pin in, and git is only ever read, the pin
aside: nothing writes to the index or the working tree. `cig runs <id>
--verify` asks git for each blob's presence and size; a blob git has lost
is refused with the reason, naming the repository and the pin. The file's own mode
and modification time travel in the journal and are put back with the
bytes, so a `chmod` by the hop is undone and `make` sees the time it saw
before. A symbolic link is kept as a link: its target is all there is, the
journal holds it itself (`(link kept)` in the record), and a link the hop
retargeted, replaced or deleted comes back as the link it was, whether or
not `touches` named it. The tin also holds what a rollback displaced (the
hop's own output, or a save made while it ran) until `--give-back` or `gc`.
`cig tin` says what the tin holds, `cig tin verify` re-hashes every object,
and `cig tin gc` removes the objects no run in the history refers to
(`cig runs --prune N` frees more). An object is named by the hash of the
copy as it was written, so a file changing while it was stashed cannot
leave an object that lies about its name, and one already there is reused
only when it still hashes to its name. A hop lists the objects it stashed
in its run directory (`tin.pending`) before any exists under its name, so
a `gc` during the hop keeps them.

A `.cigignore` at a pack root (one glob per line, `#` comments) lists what
hop watching and the tin never look into; `.git`, `node_modules`, `target`,
`.venv`, `venv`, `__pycache__`, `.gradle`, `.terraform` and `.cigscript` are
skipped everywhere. Those trees are rebuilt, not restored. A hop's own
`skip` globs, and the `hop_skip` setting for every hop, leave more out the
same way (`HOPS.md`): what the hop changes there cannot be undone, and
`cig check` says so (`E310`).

**The stat cache** (`hop_statcache`, off by default; `HOPS.md`). The tin
can optionally keep an index of each touched file's
`(dev, inode, size, mtime_ns, ctime_ns, mode)` under
`tin/statcache/<pack hash>`, so a later hop whose files all match the index
reads none of them: their tin objects are known by hash. A restore never
trusts the index (`E704`'s before and after hashes still run), and
`cig tin verify --statcache` re-hashes everything the index remembers and
drops a lying entry as a tripwire report.

Every record says what shape it is in: `run.json` carries `"format": 1`,
`after.json` `"format": 2`, and the first line of `journal.jsonl` is
`{"format":2,"prev":"…"}` (a marker, so a cig from before formats reads
past it). A newer cig reads every older format and never rewrites an old
record in place; a format it does not know is refused by its number with
the version that wrote it (`E808`), and `cig unburn` hands the run to that
version from the humidor instead of guessing.

**The hash chain.** From format 2 (1.5.0) every line of the journal carries
`prev`: the SHA-256 of the line before it exactly as written, and the first
line that of the run's id. When the record is saved, `run.json` keeps the
hash of the last line (`journal_head`). `cig runs <id> --verify` runs every
check `cig unburn` runs before it touches anything, in the same order, and
fails exactly when unburn would refuse, naming the same code: it walks the
chain and reports the first line that does not fit (edited, removed or
reordered; in a signed journal, a line whose signature is not your key's),
compares the head with the record (lines added or removed since), asks the
ledger where the journal ended (`E709`: cut short, or an older copy swapped
in, though the record was made to agree), and checks every snapshot and tin
object a rollback would need against its recorded hash (`E703`). `cig unburn` walks the chain before it changes
anything and refuses a broken one with `E760`, and a head that does not
match the record the same way (the last line has nothing after it to carry
its hash; the record carries it, and an unburn or a give-back saves the head
again after the markers it appends); `--force` restores from the journal as
it is, knowingly. A format 1 journal has no chain, is read as it
always was, and the markers an unburn appends to it carry no `prev`. This
is tamper evidence, not secrecy: anyone who can write the journal can
recompute the chain, unless the head is kept somewhere they cannot write
(a note of `journal_head` elsewhere makes the record checkable against it).

A run's status is one of `running`, `ok`, `failed`, `rolled_back`,
`unburned`, `unburn_failed`, or `interrupted` (derived when a `running`
record's process no longer exists). Burn counts shown by `cig runs` come from
the journal, which is authoritative even when the run never wrote its totals.

Run ids sort chronologically (UTC). `cig runs` lists them, `cig runs <id>`
shows one with its journal, `cig runs --prune N` keeps the newest N, and
`CIGSCRIPT_HOME` relocates the whole state directory (tests use this to stay
isolated).

Two settings prune after every run, as `cig runs --prune` does, each pruned
record a ledger line: `keep_runs N` keeps the newest N, and `keep_days N`
drops records started more than N days ago. Both are 0 by default, which
keeps every run. Neither drops a run with work left: one still running, one
interrupted, or one whose unburn failed. Neither they nor `cig runs --prune`
drop a pinned run: `cig runs --pin <id>` keeps the run before a migration or
a release whatever they say (`cig runs` marks it `(pinned)`), and
`cig runs --unpin <id>` lets it go. `cig runs --pruned` lists the runs that
went on purpose, from the ledger, newest first: when, how (`pruned`, or
`forgotten` by `--forget`, or `budget` when `history_budget` made the
choice) and the status each last had, so a missing run is explained.

**Quick at ten thousand runs (1.19.0).** `cig runs` reads a compact
**run index** (one JSONL file beside the home's run folders), kept in
step as runs start, finish, are pruned, forgotten or unburned, instead
of opening every run folder. Writers serialise on `runs-index.lock`, so
two concurrent runs never interleave; a hand-edited or hand-deleted
`run.json` is caught on a cheap stat; and a torn tail from a crashed
append is a cache miss, so the next listing rebuilds the view from the
folders and says so once. `cig runs --verify` with no id checks each
run's chain head against the ledger (no journal is opened); `--deep`
walks each journal's full chain as `cig runs <id> --verify` does, and
is opt-in because it is slower. E704 attribution keeps the one-pass-
per-path, first-twenty-named behaviour (#128).

`history_budget` caps the history as it grows, the run records and the
tin. It is off by default so nothing is pruned on upgrade; `cig doctor`
shows the history's size and suggests a value (about 5% of the free space,
from 1 GB to 20 GB), and you pick your own. Set to a size (a number of
bytes, or `500MB`, `2GB`, base 1024), it is checked after each run and on
`cig runs --prune`: the oldest runs go until the history fits, then the tin
is collected. A tin object counts as freed only when the last run that
refers to it goes, so no more is pruned than must be. Never pruned: a
pinned run, a run with work left (running, interrupted, an unburn that
failed), a run holding a box layer that waits for approval, a run younger
than `history_keep` (30 days by default; `90s`, `30m`, `2h`, `3d`, `1w`, or
`off`), and the newest run. The budget can be exceeded for those, and `cig
doctor` says so. Each prune is a ledger line with `status = budget`, so
`cig runs --pruned` says `how = budget` for it.

## Keeping the tin private

The tin is your undo, so cig keeps it to you and keeps the key that seals it
out of reach of what cig runs.

- **Owner-only.** The state directory is created `0700`, its files `0600` and
  its objects `0400`, set explicitly. An older directory readable by other
  users is tightened to owner-only. One that other users can *write*, or
  that another user owns, means your undo is not safe there: on a terminal cig
  refuses (`E707`) and names the fix (`chmod 700 ~/.cigscript`); unattended it
  warns once and carries on, so CI and containers are never broken by a
  surprise refusal. `cig config tin_perms` is `strict` (the default), `warn`
  or `off`.
- **The key lives apart from what it opens.** The fallback key file is kept in
  the OS config directory, not in the tin, so a copy of the tin does not carry
  the key. The stored secret is a master key; each use derives its own subkey
  with HKDF-SHA256 under a label, so the seal key, and later the journal and
  ledger MAC keys, are never the same key. `cig tin key export` wraps the key
  with a passphrase to carry to another machine, and `cig tin key import` puts
  it back. `cig doctor` reminds you once to back it up when sealed objects
  exist and it was never exported.
- **The keys folder is private, and checked.** The folder beside the state
  root (`~/.cigscript-keys`) is made `0700` from the start (not a
  `mkdir` followed by a `chmod`), so there is no window where it exists
  with wider permissions, and a folder that appeared while it was being
  made is checked like one that was there before. On every run cig
  verifies, OpenSSH-style, that the folder is owned by you and not
  writable by others, and that `ledger.jsonl` and `rekey.lock` are owned
  by you and are regular files (not symlinks). The seal key is checked
  where it is read, on the file actually opened (no link followed): yours,
  with one name, closed to others. A `seal.key` others could read is
  refused: chmod it 600, and if anyone else could have read it, `cig tin
  rekey` for a key nobody has seen (never move it aside: a new key opens
  nothing the old one sealed). The
  ledger and the lock hold no secret, and the folder keeps others out, so
  one of yours that is more open than 0600 is made 0600 and used. Where
  any of the rest is not so, cig refuses with `E721` and names the path,
  the owner and the mode. With `CIGSCRIPT_HOME` under a
  shared parent (such as `/tmp`, `/var/tmp` or `/dev/shm`), the folder
  is placed under a per-user name (`.cigscript-keys-<uid>`) rather than
  the shared `.cigscript-keys`, so two users under the same parent
  never share one folder (#176). `CIG_KEYS_DIR` and `CIG_SEAL_KEY_FILE`
  point somewhere else but do not weaken the checks. `cig doctor`
  prints the folder's owner and mode and names the problem, without
  creating the folder.
- **The tin guarded.** From 1.10.0 a boxed hop on Linux sees the state
  directory read-only and the key folder empty: in the user and mount
  namespaces the box is made in, the state root is bound over itself and
  remounted read-only after the hop's own layers and the inbox were bound
  writable underneath, and an empty tmpfs goes over the key folder. A hop
  that overwrites or deletes a tin object or a journal gets `EROFS`; one
  that looks for the key finds nothing.
  From 1.19.0 the guard also hides `tin/` and `runs/` from a boxed hop
  **entirely**, not just makes them read-only: an empty tmpfs goes over
  each, and the one file under them that the hop legitimately writes to
  (its burn-protocol journal, the file `CIG_JOURNAL` names) is bound back
  through the fresh `runs/` tmpfs so the protocol keeps working. A hop
  that lists `~/.cigscript/tin` or `~/.cigscript/runs` sees nothing, other
  runs' records and the tin's objects are out of its sight, and its own
  report still lands. When the registry proxy is on, its socket folder
  under `runs/` is bound back the same way, so `network: registry` hops
  reach the proxy as they always did. A guarded unboxed hop under
  `tin_guard = on` gets the same hiding.
  The inbox (`inbox/<run>/` under the
  state directory, **outside** `runs/` and so untouched by the hide) is
  the one place a hop may write: a cig started inside
  the hop has it as its own state root (`CIGSCRIPT_HOME`, `CIG_KEYS_DIR`,
  a key file of its own, the parent's settings copied in), runs as it would
  anywhere, and when the hop ends its runs are taken in as **children** of
  the run (`cig runs <id>` names them; `parent` on the child's record),
  whole: its objects move into this tin, checked against their names on
  the way (a mismatch drops the run, `E713`), its sealed objects are
  re-sealed under this machine's key, its journal's paths relocated and
  the journal re-signed. A child unburns like any run, and the parent's
  rollback undoes it through the burn protocol, as a nested run always
  was. Anything else in the inbox is dropped with a note (`E713`). `tin_guard` is `auto` (boxed hops, wherever the kernel allows;
  one note, `E712`, where it does not), `on` (every hop, boxed or not, and
  a refusal where the guard cannot be made) or `off`; `CIG_TIN_GUARD=0` is
  off for one run. Outside Linux, and where user namespaces are not
  allowed, the tin is checked (the ledger, the hashes, `cig tin verify`)
  rather than protected, and `cig doctor` says which.
  From 1.11.2 the guard also closes the other ways to the key. A key file
  named outright (`CIG_SEAL_KEY_FILE`, the CI setup) is covered with
  `/dev/null` and the variable is not passed on. From 1.17.0 CI can keep
  the key out of files and the environment altogether: `cig
  --seal-key-fd 3 run s.cig 3<key.hex` (or `--seal-key-fd 0` with the key
  piped in) reads the 64 hex characters a key file holds from that
  descriptor at start-up, closes it (stdin becomes `/dev/null`), and holds
  the key in memory only: no key file is made, and no hop, child or
  nested cig inherits it. A nested cig a hop starts finds its own key, as
  before. Programs take the same flag. Unix only; Windows names a key
  file. While the key may be in
  the OS keyring (`seal_key` is not `file`), a guarded hop gets no session
  bus: `DBUS_SESSION_BUS_ADDRESS` is not passed on, and the bus and
  systemd's private socket (named by the variable, by `XDG_RUNTIME_DIR` or
  by your uid's runtime folder) are covered. On Linux that keyring is the
  Secret Service on the session bus, which hands a secret to any process of
  yours that asks for it, and systemd's user manager on the same bus would
  start a process outside the box that could ask. A credential helper that
  needs the session bus (libsecret, `gh`'s keyring) does not work in a
  guarded hop; `cig config seal_key file` keeps the key in a file the guard
  hides and leaves hops the bus. A process in the hop's user namespace
  cannot open `/proc/<pid>/root` or `/proc/<pid>/environ` of your other
  processes (the kernel asks for a capability in theirs), so those are no
  way around a cover. What the guard does not do is confine the hop: it
  keeps the tin read-only and the key out of reach, and the box keeps the
  pack, but a hop that reaches other services you run (an X11 display,
  `docker.sock`, an ssh-agent, systemd's user manager while the key is a
  file, an abstract socket while it shares your network) can act through
  them. Neither the guard nor the box is a sandbox for the rest of the
  machine.
- **A record scripts cannot rewrite.** From 1.9.0 the journal is signed:
  each line's `prev` is an HMAC under a subkey of the tin's key, so a
  recomputed chain does not verify (`E760`); a journal under another
  machine's key is not tampering but *signed by another key* (`E708`,
  `--accept-unsigned`). The ledger, one MACed line per run beside the key
  (each line naming the tin it witnesses, so two state directories beside
  one key keep separate accounts),
  records where each journal ended, so a journal cut short or swapped for an
  older copy is caught (`E709`); pruning is recorded, a run removed by hand
  is a note (`E710`, `cig runs --forget`). `cig tin head` prints the
  ledger's head to keep elsewhere; `cig tin verify --head` checks it.
  `journal_sign off` writes the unsigned format 2.
- **Out of cig's memory.** Once a key is loaded cig blocks a debugger and
  `/proc/<pid>/mem` of the same user (Linux `PR_SET_DUMPABLE`, macOS
  `PT_DENY_ATTACH`), so a hop cannot read the key out of cig. It resets for
  every program cig starts. `CIG_ALLOW_DEBUG=1` turns it off for anyone
  debugging cig, and says so every run.

## The box

The tin makes a hop's changes undoable after they happen; the box makes a
failing hop's changes never happen. `proc.run(cmd, args, {box: true})`
runs the hop, on Linux, in its own user and mount namespaces, with each
pack root mounted as overlayfs over itself: the real root is the read-only
lower layer and an upper directory under the run directory takes every
write, so other programs reading the pack while the hop runs see nothing
of it. A hop that fails, or exits outside its contract, has its layer
dropped: nothing of it reached the pack, whatever the pack's size and
whether or not `touches` named anything, and the run says `layer was
dropped`. A hop that succeeds has its layer applied to the pack as native
burns, outer paths first: a file in the layer is a write (the real file's
before-bytes journaled first, sealed when it is a secret or under the hop's
`touches_sealed`), a whiteout is a delete, an opaque directory a delete,
then the directory itself (empty, when the hop left it so) and the entries
that follow, a new directory a mkdir, a link a write; each is journaled with
its before-state
exactly as `fs.write_text` and `fs.rm` are, so the run is unburnable like
any other and the record reads as native ops. The plan says what the box
was asked for, `(boxed: network registry, allow registry.npmjs.org,
contain)`: the network always, the rest where it is not the default; the
run's record keeps it, and `cig runs <id>` says it (1.17.0). It says before
the run when the box cannot be made here. It cannot where
unprivileged user namespaces are off (Ubuntu's AppArmor restricts them
since 23.10: `sysctl kernel.apparmor_restrict_unprivileged_userns=0`), on
a kernel before 5.11, on a filesystem overlayfs cannot layer over, where the
state directory itself sits on an overlay (a container's layer, or another
box: overlayfs refuses an upper directory there, so the box does not nest;
a `CIGSCRIPT_HOME` on a plain filesystem lets it be made), and on macOS,
Windows and the BSDs, which have no overlayfs; there the hop runs on the
tin as usual, with `touches` keeping what it may change, and the run says
so (`E763`). A hop's own undo (the burn protocol) is journaled for a
boxed hop too. The layers live under the run directory while the hop runs
and are removed once applied or dropped, so the record prunes as any other.
The trees hop watching never looks into (`.git`, `node_modules`, `target`,
`.venv` and the rest of that list) are bound through the overlay from the
real root: a build writes its objects where it always did, nothing of them
is journaled or undone, and the box applies the rest; a tree that is not
there before the hop is made through the box like any new directory. A run
that dies in a boxed hop leaves the layers under its record (overlayfs
makes the work directory unreadable); the next `cig run` sweeps them for
any run that is not alive, `cig unburn` removes them with the rollback, and
a rollback removes any tree a hop made whatever the modes inside it.

### Untrusted code: the box's network, PID namespace and kept layers

From 1.11.0 `box` also takes a map, for a hop whose code you did not write:

```cig
proc.run("npm", ["ci"], {box: {
  network: "registry",                 # "on" (the default), "off", "registry"
  allow_hosts: ["registry.npmjs.org"], # a name or "*.suffix" (on 443), or "host:port"
  pidns: true,                         # its own PID namespace (the default with the box)
  apply: "on_success",                 # "on_success" (the default), "ask", "never"
}})
```

- **The PID namespace.** The hop is pid 2 under an init of cig's (pid 1
  inside), with the namespace's processes alone in `/proc`: it cannot see,
  signal, trace or read cig. Each process dies with its parent, so a cig
  that dies takes the whole namespace with it, and a layer is applied only
  by a live cig after the hop. The layer a dead cig left is dropped by the
  next `cig runs`, which says so; the run's earlier burns unburn as ever.
- **The network.** `off` is a namespace with `lo` alone and an empty
  `/etc/resolv.conf`: a connection anywhere fails at once, a lookup has no
  one to ask. `registry` is the same namespace with one way out: a
  forwarder inside on `127.0.0.1:38080` (`HTTP_PROXY`, `HTTPS_PROXY`,
  `npm_config_proxy`, `PIP_PROXY`, `CARGO_HTTP_PROXY` and friends point at
  it) relays to cig's CONNECT proxy outside, which allows `allow_hosts`,
  resolves them itself, counts the bytes, and refuses and records the rest
  (`E771`, one note per host; the events in `runs/<id>/events/<hop>.jsonl`).
  An entry is `host:port` exactly, or a name (or address) alone or
  `*.suffix`, which mean port 443; a name admitted by a wildcard that
  resolves to an address on this machine or its network (loopback, private,
  link-local) is refused, so a name someone else controls cannot turn the
  proxy on a service here, while an entry that names a host or address
  exactly is taken as given. The proxy's socket sits in a folder the hop
  cannot write (`proxy/` under the run, read-only inside the box whatever
  the guard manages), since the forwarder connects to it by path. A tool
  that ignores proxies has no network at all. A network of its own asked
  for where the box cannot be made is a refusal (`E777`), not a fallback:
  running code you do not trust unprotected to check it defeats the point.
- **No burn protocol for an isolated hop.** A hop with a network of its own
  is code its caller does not trust, so it is given no `CIG_JOURNAL`: an
  undo it declared there would be a command cig runs later, outside every
  namespace, as you. Its changes come back with its layer, which is exact
  anyway.
- **Kept layers.** `apply: "never"` runs the hop and holds its layer back:
  nothing reaches the pack, the hop's result carries `layer: "<run>/<hop>"`,
  and the run says so (`E780`). `apply: "ask"` shows what the layer would
  change and asks at the terminal; away from one it keeps. `cig box list`
  names kept layers; `cig box apply <run> <hop>` puts one in as a run of its
  own, journaled with its before-images, so `cig unburn <that run>` takes
  it out again; `cig box discard <run> <hop>` drops one. An unburn of the
  run a kept layer belongs to drops it too: the base it was made against is
  gone. The record that keeps a layer lives under the run's `kept/`, not
  beside the layer, where the hop could have planted one.
- **A copy for another tool, in the tin.** `cig tin keep <file> --label <l>
  [--name <n>]` keeps a sealed copy of a file under the caller's label, and
  `cig tin give --label <l> --name <n>` prints it back. Each keep is a line
  in the ledger, so a copy deleted from the tin (the record or the object)
  is missed: `give` exits 4 and says when it was kept, where a name never
  kept exits 1. `cig tin gc` leaves kept copies alone. cig-orchestrator
  keeps the last blacklist it verified and its hook's entry there, so
  deleting the file they back does not take them out of force.
- **A key for another tool's records.** `cig tin mac <file> --label <l>`
  prints a MAC over the file under a subkey of the tin's key derived for
  that label; `--check <hex>` verifies one. cig-orchestrator signs its trust
  store with it: a hop in the box cannot make such a MAC under the tin's
  key, because the key folder is empty there, so an approval a script
  writes for itself verifies nowhere.

### Rehearsing code you did not write: `contain` and `expect`

From 1.12.0 a hop can be run so that everything it writes is a layer cig
reads afterwards, and nothing else on the machine can be written at all:

```cig
stick r = proc.run("npm", ["ci"], {box: {
  network: "registry", allow_hosts: ["registry.npmjs.org"],
  contain: true,                       # everything it writes is a layer
  expect: ["node_modules/**", "package-lock.json", "~/.npm/**"],
  apply: "never",                      # a rehearsal: judge it, then decide
}})
for u in r.outside { exhale u.path + " (" + u.kind + ")" }
if r.outside.is_empty() { proc.apply_layer(r.layer) } else { proc.discard_layer(r.layer) }
```

- **What `contain` does.** The pack's roots are layered as in any box, and
  nothing passes through to the real disk (`.git`, `node_modules` and the
  rest are layered too). Your home gets a layer of its own, mounted on the
  home itself, so a write to `~/.bashrc` lands in the layer whether the hop
  names it through `$HOME` or by its full path; the pack's roots and the
  state directory are put back over it. The hop's temp is the box's
  (`TMPDIR`, and `/tmp` itself unless something cig needs lives there).
  Then the whole tree goes read-only in one call (`mount_setattr`), and
  only the layers and the temp are made writable again: a write anywhere
  else fails (`EROFS`). The hop gets a `/dev` of its own (the plain devices,
  a pty instance of its own, a fresh `/dev/shm`; no input devices, cameras,
  sound or disks), System V IPC, a host name and a cgroup view of its own,
  a session of its own (no controlling terminal), `no_new_privs`, and a
  syscall filter: `socket` for the internet and netlink only, so no Unix
  socket is opened (no session bus, no systemd manager, no `docker.sock`,
  no agent), while `socketpair`, which child pipes use, works; no new user
  namespace; no io_uring, ptrace, other processes' memory or descriptors,
  kernel keyrings, BPF, performance counters, userfaultfd or mount calls;
  only this machine's own architecture. The key folder is empty and the
  state directory read-only in the hop's view, as with the guard, which
  `contain` brings with it whatever `tin_guard` says.
- **What it asks for.** Linux 5.12 or later, x86_64 or aarch64, a user that
  is not root (a root hop would keep its capabilities and could make the
  rest writable again), `network: "off"` or `"registry"` (the machine's
  network would reach services on this machine) and the PID namespace.
  Where any of it cannot be had the hop is refused (`E777`), never run
  unprotected.
- **`expect`.** Globs under the pack root, or starting `~/` for the home;
  `*` stays within one folder, `**` crosses them. After the hop, cig reads
  its layers and reports every change no glob names (`r.outside`, each
  `{path, kind}`: `write`, `link`, `delete`, `replace`), and whatever
  `expect` says, a link into your home that `expect` does not name
(`link-out`; a link to a system path, such as a venv's python, is fine), a set-id file
  (`setuid`) and a folder that cannot be read (`unreadable`; a hop cannot
  hide its writes behind its own modes). One note names the first few
  (`E781`). `r.changed` counts what was written and removed, in the pack and
  in the home; `r.refused` lists the hosts the proxy refused. Under
  `contain` a hop that changed what it was not expected to is held back
  rather than applied, whatever `apply` says.
- **Evidence.** A rehearsal (`apply: "never"`) or a contained hop keeps its
  layer even when the hop fails (a non-zero exit outside its contract, a
  timeout): the error's value carries `layer`, `outside`, `refused` and
  `changed`, so what it did can be read.
- **Applying in the same run.** `proc.apply_layer(r.layer)` applies a layer
  this run kept, as native burns journaled with their before-states, so
  the run unburns it like any other; only while the layer still holds
  exactly what it held when it was judged (a digest of every change is
  kept with it), and `E782` otherwise. `proc.discard_layer(r.layer)` drops
  one. The home layer is never applied with it, only by name:
  `proc.apply_layer(r.layer, {home: ["~/.cache/**", "~/.config/tool/**"]})`
  also places the home writes those globs name (a tool's cache, its own
  config), checked against a digest of the home layer taken when it was
  judged (`E782` when it changed). They are outside the pack, so they are
  placed as they are, not journaled, and named in the result's `home`; the
  rest of the home layer goes with the layer.
- **`quiet: true`.** A script that reads the layer, `outside`, `refused`
  and `changed` itself and says what they mean in its own words (a guard
  rating what a package did) leaves out cig's own lines about the layer:
  the `E781` and `E780` notes, `applied` and `dropped`. The result, the
  layer's fate and the journal are the same as without it.
- **A cig inside.** A cig started in a contained hop keeps a throwaway state
  in the box's temp; nothing of it is taken in as a child run (its records
  could name any path with any before-image), and it could not have
  written outside the layers anyway.
- **Where it lives.** The home layer and the temp sit in a scratch folder
  outside your home and the pack (`/var/tmp/cig-box-<uid>-<run>-<hop>`, the
  temporary directory where there is no `/var/tmp`), made fresh and yours
  alone. It goes when the layer is applied or discarded, with an unburn of
  the run, with `cig runs --prune`, and for a cig that died, with the next
  sweep.
- **What it does not do.** `contain` stops writes, not reads: the hop can
  read every file you can, secrets included, and with `network: "registry"`
  its way out is the registries you allowed (publishing to one with a token
  of its own is still a way out). A mount under your home other than the
  pack's roots and the state directory is read-only to the hop, not
  layered. The temp and `/dev/shm` are scratch, not evidence. A stage that
  hands its work to a daemon (a container build) does that work outside
  the box, where nothing here sees it. Kernel exploits and root escape
  namespaces.

## The parcel

A run's undo lives on the machine that ran it: the record under
`~/.cigscript/runs/<id>/`, its snapshots, and the tin objects it refers to.
`cig parcel <run>` writes them as one file, `<run>.cigparcel` (a gzipped
tar): `run/` with the record, the journal and the after-state and every
snapshot; `tin/` with every object, recipe, chunk and sealed object the
journal refers to, and nothing another run holds; `git/` with the bytes of
every blob git held for the run, carried whole so the other machine needs
no repository; and `manifest.json` naming each file's SHA-256, the cig
that wrote it, the pack's roots (each with a name, its last component) and
the working directory. `cig unparcel <file>` checks every file against the
manifest before it writes anything, and a byte changed refuses the whole
parcel (`E762`); every root must be mapped to a place here (`--root
build=/srv/app/build`; `--show` lists them, `--cwd` sets the working
directory when it cannot be told from the roots, which it can when a root
sits under it by the same relative path); sealed objects this machine's
key does not open are refused unless `--allow-sealed-missing`. The objects
go into the tin (a content-named one already there is kept when it hashes
to its name), the run among the runs, and its record says where it came
from and how its paths map (`imported`). The journal itself is never
rewritten, so its chain still verifies; the record's map is applied as it
is read. From then on `cig runs <id>` lists it as imported and
`cig unburn <id>` works here, with the usual drift check (`E704`) against
this machine's files. A parcel written from an imported run carries that
run's map too (`prior` in the manifest), so a run can go from A to B to C:
C's import composes the maps, and a journal that names A's paths, and B's
where B appended to it (an unburn, a give-back), reads through one map.

## What it can't undo, and says so

The irreversible label is the honesty guarantee. It is the one thing that
must never be wrong, because the whole model is trusted through it: a plan
that says *reversible* is a promise the kernel can keep, and a plan that says
*irreversible* is the kernel refusing to launder what it cannot watch. Both
are written before the effect and shown in the plan, the run record and the
rollback report ("cannot undo"). What the label covers:

- **No sandbox.** A script can read anything the user can read, and a burn can
  run any program. The kernel makes effects explicit and reversible where it
  performed them itself; it does not confine untrusted code.
- **No rollback of what processes did, unless you say how.** A hop is
  journaled as irreversible; inside a pack the files it created are removed
  on rollback. What it modified or deleted comes back too when the hop said
  it might touch them (`touches: ["src/**"]`): those files' before-bytes
  went to the tin first. The rest is listed; inside a `burn { } unburn { }`
  block the compensation is the undo and the label says `compensated`.
  Prefer `fs.rm`, which the kernel can undo on its own.
- **Cross-run consistency is checked, not guaranteed.** When a run finishes,
  the kernel records what it left behind (`after.json`: the state of every
  path it touched; a file by hash and size, a directory by a fingerprint of
  its entries' names, kinds, sizes and modification times, so a file you
  added to a directory the run created counts as a change and is not swept
  away with it, while an edit that keeps a file's size and puts its time
  back is not seen). `cig unburn` compares
  that with the disk now and refuses
  with `E704` when anything changed since, naming the later run that touched
  the path when there is one, so runs are unburned newest first; `--force`
  restores over the newer content anyway. `cig unburn --dry-run` prints the
  plan with a *changed since* column. A run that predates after-state records
  says "unknown" and is allowed.
- **Snapshots are plain copies**, under your home directory, except for
  secrets, which are sealed (above): a secret-shaped name, a `.cigseal`
  entry or `{sensitive: true}` keeps the before-bytes encrypted at rest. Prune
  the rest with `cig runs --prune` if it holds anything sensitive the list
  did not know. Snapshots keep the full mode (setuid, setgid and sticky bits
  included), and a restored tree keeps the mode of its root.
- **Restores are atomic where they can be.** A file comes back beside its
  destination and is swapped in, so a failure half-way leaves what is there
  untouched; on Unix a file that has other hard links is restored in place
  instead, so every name sees the old bytes. A file the run moved across
  filesystems is copied back the same way, and when a move back fails the
  only copy is kept, never removed. What a rollback could not restore is
  recorded on the run (`rollback_incomplete`; `cig runs <id>` lists the
  failures), a plain `cig unburn <id>` then retries those alone with the
  rest left as it is (`--force` restores everything again), and `cig unburn`
  refuses to start when it could not record itself.
