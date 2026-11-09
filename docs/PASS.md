# Passes: checked once, trusted exactly as long as it is true

A **pass** is a small, signed note that says "this was checked, and here is
what the answer depended on". It carries what was checked, every file,
folder, variable and tool the check looked at (recorded while the check ran,
never listed by hand), the result, and any limits it was handed with. Later,
asking "is this still good?" is one signature check and a look at each
recorded input, however long the original work took. If any input moved, the
pass stops holding, and the work is done again.

A cached "yes" that outlives what it was about is the worst bug a safety tool
can have, so cig has one place that decides whether a "yes" still holds
(`cig_pass::verify`, in `crates/cig-pass`), and everything that remembers a
check uses it.

The error pages are in [ERRORS.md](ERRORS.md) (`E730` to `E739`, `E809`), the
`pass` functions in [STDLIB.md](STDLIB.md), and the threat model beside the
code in `crates/cig-pass/THREAT-MODEL.md`.

## Contents

- [From a script: the `pass` module](#from-a-script-the-pass-module)
- [What is recorded, and what makes work open-world](#what-is-recorded-and-what-makes-work-open-world)
- [Caveats: narrowing a pass](#caveats-narrowing-a-pass)
- [Local and shareable passes](#local-and-shareable-passes)
- [From the command line: `cig pass`](#from-the-command-line-cig-pass)
- [Revoking passes](#revoking-passes)
- [Where things live](#where-things-live)
- [What a pass can and cannot protect against](#what-a-pass-can-and-cannot-protect-against)

## From a script: the `pass` module

| function | what it does |
|---|---|
| `pass.issue(kind, subject, work, opts?)` | runs `work` (a `pack(s) { ... }`) with a recorder, and returns a pass over what it read; `work`'s return value is the pass's result |
| `pass.find(kind, subject, opts?)` | the newest pass for that subject that holds now, or `null` |
| `pass.memo(kind, subject, work, opts?)` | `find`, else `issue`: the map says `hit: true` or `false` |
| `pass.check(p, action?)` | `{ok, why}`: does it hold now, and if not, the first thing that failed |
| `pass.narrow(p, caveat)` | the same pass with one more limit |
| `pass.spend(p, action?)` | checks it and counts one use of each `once` or `max n` it carries |
| `pass.show(p)` | everything in it: inputs, caveats, ids |
| `pass.recording()` | `true` inside a pass's work (a cache of your own can step aside) |

A pass is a map: `id`, `root`, `kind`, `subject`, `result`, `issued`,
`caveats`, `inputs` (a count; `pass.show` lists them), `form`, and `token`
(the text form, `cigpass1.`...). Anything that takes a pass takes the map or
its token.

A script makes any kind but `revoked` and `human-yes`, which only come from
elsewhere. The `subject` is any value of strings, integers, lists and
maps; it is canonical (key order never matters), and a number with a fraction
is refused, because it could compare equal in one place and not in another.

```cig
# Check the site's config once; later runs reuse the answer while nothing it read has changed.
stick p = pass.memo("check", {config: "site.yml"}, pack(s) {
  stick text = fs.read_text(s.config)
  stick guides = fs.list("docs")
  snuff {lines: text.split("\n").len(), guides: guides.len()}
})
exhale "hit:", p.hit, "result:", p.result
```

Three runs, the third after `site.yml` gained a line:

```
hit: false result: {guides: 1, lines: 3}
hit: true result: {guides: 1, lines: 3}
hit: false result: {guides: 1, lines: 4}
```

**Identity.** By default (`code: true`) a pass is about its subject *from
this script*: `pass.find` in the same script finds what `pass.issue` made, and
another script with the same subject does not. A memo is also about its call,
so two `pass.memo` calls with one subject never return each other's result.
`code: false` drops the script from both (for a pass whose meaning is its
inputs alone, such as an approval of a task).

**Options.** `until` (a span such as `2h`, a UTC time, or unix ms: the pass
expires, and the work may read the clock), `code`, `cig` (record cig's own
version, on by default and always on for `check`, `trial`, `verdict` and
`doctor`), `tools` (`{cigo: "1.18.0"}`: versions the result depends on),
`rules` (`{blacklist: "<hex digest>"}`), `store` (keep it; on by default).

**Dry runs.** A dry run reads through its simulated changes and simulates its
hops, so what the work saw never happened: `pass.issue` and `pass.memo`
return the result with `token: null` and nothing is kept. `pass.spend` in a
dry run checks, and counts nothing.

## What is recorded, and what makes work open-world

Inside the work, every read goes through the recorder:

| the work calls | recorded as |
|---|---|
| `fs.read_text`, `fs.read_lines`, `fs.size`, `hash.sha256_file`, `json.load`, `csv.read` | the file: its identity and the BLAKE3 of the bytes the work got (from the same read) |
| `fs.exists`, `fs.is_file`, `fs.is_dir` | what is at the path, or that nothing is |
| `fs.list`, `fs.glob` | each folder listed: its names and their types |
| a relative path | the working folder too; `~` records HOME |
| `env.get`, `env.has`, `env.all` | the variable (its value or that it is unset) |
| `proc.which` | PATH, every candidate before the one found, and the program found |
| a global, or `args` | a digest of what it held: the pass holds only while the running script's global holds the same |
| a hop with `reads: [paths]` | each path (a folder: every file and folder under it, up to 10,000), the program, and the hop's environment |

The pass also records the script itself (with `code: true`) and cig's
version.

**A file that changed as it was read** (written within two seconds of the
read) is always hashed when the pass is checked, whatever its size and times
say.

**Hops.** A program the work runs cannot be seen into, so it must say what
it reads: `proc.run(cmd, args, {reads: ["package.json", "src"]})`, inside a
`burn { }` as every hop is. The child gets only PATH, HOME, LANG, LC_ALL, TZ
and TMPDIR, plus the variables `env_reads: [names]` lists, each recorded. The
declared paths are looked at again after the hop, so a file that changed
while it ran gives no pass. A pass that rests on a declaration says so
(`cig pass show`: "includes a hop's declared reads"), and `approval`,
`verdict` and `rules` passes never rest on one.

### Open-world work gets no pass

These make the work impossible to vouch for. `pass.issue` stops with `E731`
naming the call; `pass.memo` returns the result with `token: null` and the
reason in `why`:

- a hop without `reads: [..]`, and `proc.pipe`;
- `stdin()`, `cig.run_id()`, `fs.modified_ms` (a time, not the bytes);
- the clock (`time.now_ms` and the rest) without `until`;
- a read that failed for any reason but the file not being there;
- `pass.find` finding nothing, `pass.check` saying no, `pass.narrow`: each is
  an answer that can change;
- any builtin the recorder does not know: a new one is open-world until it is
  classified.

These stop the work outright:

- **`E732`**: the work read a variable of the function around it (or one a
  closure captured before the work began), or changed a variable outside it.
  Put the value in the subject and read it from the work's argument:
  `pass.issue("check", {n: n}, pack(s) => s.n + 1)`.
- **`E733`**: the work called something that changes the world (a write, an
  `env.set`). Check inside the work; change the world outside it.
- **`E734`**: the same input gave two answers while the work ran.

The work gets a copy of its subject, so what it changes there changes nothing
outside.

## Caveats: narrowing a pass

Anyone holding a pass can add a limit, and nobody can take one away: each
caveat is chained into the pass's tag (local) or signed into its chain
(shareable). Narrowing never needs a key.

| caveat | holds when |
|---|---|
| `within <folder>` | every folder the action works in is under it (both resolved, links followed, `..` refused) |
| `until <UTC time>` / `for <span>` | the time has not passed; a clock set back on the same boot does not extend it |
| `boxed` | the action runs in a box |
| `network none` / `network <host,host>` | the action's network is at least that narrow |
| `args <pattern> <pattern> [...]` | one pattern per argument, in order; a last `...` takes the rest; `*` never crosses into the next argument |
| `cig >= <version>` / `build <id>` | the running cig |
| `agent <name>` | the action was started under that agent (a label the caller states, not authentication) |
| `once` / `max <n>` | spendable once, or n times, counted in the ledger |
| `needs <kind>` | valid only with a discharge pass of that kind for this pass |

A caveat about the action holds only when the caller says what the action is
(`pass.check(p, {boxed: true, agent: "hivemind", roots: ["."]})`, or
`cig pass verify --boxed --agent hivemind --root .`): a field not given never
holds. The caller is the one about to run the action, and it must build that
description from what it will really run.

A narrowed pass keeps its spends: adding a caveat after `once` gives no
second use.

## Local and shareable passes

**Local** passes are checked with an HMAC chain under a subkey of the tin's
key: fast, private, and they hold only on the machine (and tin) that made
them. Their digests of file contents and variables are keyed too, so a token
handed to someone does not let them test guesses at a short secret it
depended on.

**Shareable** passes carry a chain of Ed25519 signatures another machine
checks against its allowed signers. `cig pass export` turns a local pass that
holds here into a shareable one: sealed (nobody can narrow it further;
`--unsealed` leaves it open), valid for 7 days unless `--for` says otherwise,
re-hashing every input first. Only `check`, `program`, `parcel` and `archive`
passes travel; a pass with `once`, `max` or `needs`, or one that read a
variable, is not exported.

Another machine trusts this one for the kinds it names:

```
$ cig pass key
you@laptop namespaces="cigpass-check,cigpass-program" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIENzudDBOT724xwLMphldBwwkgt2L7qA3Q+hD/DKIYb5
```

That line goes in the other machine's allowed signers (the OpenSSH format
`cigcompile verify --allowed-signers` reads): `<keys>/allowed_signers`, or
`cig config pass_allowed_signers <file>`. A line without a `cigpass-` namespace
allows no pass at all, the file is read only when it is yours and nobody else
can write it, and `valid-after` / `valid-before` hold. A `cig tin rekey`
changes this machine's key: give the other machines the new line.

**`human-yes`** passes (a person said yes) come only from a human authority:
an Ed25519 key in `/etc/cigscript/authority.pub`, a file root owns, installed
by an administrator, whose private key sits behind the platform's own
authentication. No variable, flag or other file stands in for it. On a
machine with no authority installed, `needs human-yes` cannot be met (`E737`).

## From the command line: `cig pass`

```
cig pass list [--kind K] [--json]
cig pass show <pass> [--json]
cig pass verify <pass> [--allowed-signers F] [--root DIR] [--boxed] [--network P] [--arg A]... [--agent N]
cig pass narrow <pass> <caveat>... [-o FILE]
cig pass revoke <pass> | --kind K
cig pass export <pass> [-o FILE] [--for SPAN] [--unsealed]
cig pass import <file> [--allowed-signers F]
cig pass log | stats | key [--kinds K,K] | gc
```

`<pass>` is a file holding a token, `-` for stdin, or an id (or the start of
one) from `cig pass list`. A token is a bearer credential: pass it as a file
or on stdin, where others on the machine cannot read it from a command line.
`-o` writes it 0600, never through a link. `cig pass verify` exits 0 when the
pass holds and 1 when it does not.

```
$ cig pass show c865a299
pass c865a299bf147fbb63d56756bb40e93b51bcb4059633dc7e9108b12fb6d27e7a
  root     c865a299bf147fbb63d56756bb40e93b51bcb4059633dc7e9108b12fb6d27e7a
  kind     check (local, epoch 0)
  issued   2026-11-09T11:38:20Z
  inputs   5
    file     ~/project/check.cig
    file     ~/project/site.yml
    folder   ~/project/docs
    env      the working folder
    tool     cig 1.20.0 (dev)
  holds now
```

## Revoking passes

- `cig pass revoke <pass>` revokes that pass and every pass narrowed from it
  (a narrowing's root is the pass it came from). Revoking a narrowing leaves
  the pass it came from alone.
- `cig pass revoke --kind verdict` moves the kind to its next epoch: every
  `verdict` pass made before stops holding, and nothing else does.
- `cig tin rekey` changes every subkey: every local pass made before stops
  holding.
- An exported pass has an id of its own (`cig pass export` prints it):
  revoke that one on the machines that hold it.

Spends, revocations and epochs are lines in the ledger (`pass-spend`,
`pass-revoke`, `pass-epoch`), so `cig pass log` shows each, and deleting a
cache never brings a revoked pass back.

## Where things live

| what | where |
|---|---|
| the passes | `<keys>/passes/` (the keys folder beside the cig home: private, ours, 0700, and covered with an empty folder inside a box, so a boxed hop sees no pass and no key) |
| the pass state | `<keys>/passes/marks-<key id>`: a cache of the ledger's pass lines, MAC'd and pinned to the ledger's length; `cig pass gc` folds it again |
| allowed signers | `<keys>/allowed_signers`, or `cig config pass_allowed_signers <file>` |
| the human authority | `/etc/cigscript/authority.pub` (root's) |
| the budget | `cig config pass_budget 128MB` (default 64 MB; approvals, human-yes and revocation lists have their own 4 MB and are never pushed out by others) |

## What a pass can and cannot protect against

A pass is only as strong as who can hold the tin's key.

- It stops anything that cannot reach the key from forging or widening a
  pass: another user, a boxed hop (the box hides the keys folder), a copied
  home, the holder of a token.
- It does not stop a process of yours, outside a box, that reads the key
  (the key file, or the keyring): that process can make any local pass. An
  approval says cigo asked and you answered; it is not a wall against such a
  process.
- Every local pass carries the ledger's length when it was made, so a
  ledger deleted or cut below that refuses it. A whole keys folder put back
  from an older copy cannot be told apart on this machine: keep
  `cig tin head` somewhere else when that matters.
- Spends are counted per machine: a shareable `once` pass checked on two
  machines is used twice.
- A hop's `reads` is a declaration: the program is assumed to read only
  those paths, and its libraries, interpreter and network are not seen.
