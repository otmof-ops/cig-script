# Compiled programs

<!-- SPDX-License-Identifier: CC-BY-4.0 -->

A **program** is a CigScript script appended to the `cig` of the same version. It is one executable that runs its script and takes no command. A stage `a` program runs its script's text on the tree-walker; a stage `b` program runs the bytecode cigcompile lowered it to, on the VM. Either runs through the same run as `cig run`, with the same kernel, journal, plan and rollback.

## How one is made

`cigcompile build tools/tidy.cig` checks the script and writes the program. Its errors are `E1000` to `E1010` (`cig explain E1000`). Anyone writing a tool that makes or reads programs needs the format. A program is written in format 2; cig still reads and runs format 1, the older format, for as long as such programs exist.

### Format 2

The payload starts where the runtime's image ends (`P`) and runs to the end of the file, apart from the zero padding or PE certificate table that may follow the trailer. Integers are little-endian; offsets are from `P`.

```text
... cig ...        the runtime, as released; its image ends at P
record             JSON, {"format":2,...
zero padding       to the first body's alignment
section bodies     in table order, each at a file offset that is a multiple of 16
section table      count x 64 bytes
trailer            56 bytes, "CIGPROG2" first
```

**The trailer** (56 bytes): `CIGPROG2` (8), the payload's length from `P` to the trailer (u64), the record's length (u32, at most 65,536), the section count (u16, 1 to 32), flags (u16, `0`), and the head sha256 (32): the sha256 of the record's bytes followed by the table's. The table sits directly before the trailer; the record sits at `P`.

**A table entry** (64 bytes): the id (8, lower-case ASCII letters, digits and `-`, a letter first, zero-padded), the body's version (u16), the encoding (u8: `0` stored as is, `1` raw DEFLATE, RFC 1951), flags (u8: bit 0 required, other bits `0`), the decoded length (u32), the offset from `P` (u64), the stored length (u64), and the sha256 of the stored bytes (32).

**The record** is one JSON object without whitespace, its keys in this order: `format` (`2`), `name`, `file`, `sha256` (of the script's text, decoded, hex; as in format 1, so `cig runs` and `--unburn` work across formats), `cig`, `compiler`, `stage` (`"a"` runs the source on the tree-walker, `"b"` the bytecode on the VM), `opt` (the level it was built at), `target` (the target triple, at most 64 characters), `runtime_sha256` (the sha256 of the bytes before `P`), and `own_flags` (`true`, written only when set). A reader ignores keys it does not know. `name`, `file`, `compiler` and `cig` follow format 1's limits; `opt` and `target` are printable text.

**The sections:**

| id | holds | stage `a` | stage `b` |
|---|---|---|---|
| `source` | the script's text: raw DEFLATE (level 9) when it is 4 KiB or more, stored as is below that | required | kept |
| `bytecode` | the bytecode module, its `CIGB` header included | absent | required |
| `tree`, `snap` | the checked tree and the top level's constants, archived | optional | absent |
| `sbom` | the program's CycloneDX SBOM, DEFLATE | optional | optional |

The same inputs give the same bytes.

**The rules.** Each is refused with `E1050` and a message that names it: the trailer's magic, its flags, the payload's length (at most 64 MiB, and inside the file), the record's length, the section count, the record and table fitting the payload, the head sha256, a record that starts with `{"format":`, reads and holds format 2; ids that are well formed, not repeated, and in the order `source`, `bytecode`, `tree`, `snap`, `sbom`, then any other id by its bytes; each body between the record and the table, after the one before it, at a file offset that is a multiple of 16; padding between the record, the bodies and the table that is zero and shorter than 16 bytes; an encoding of `0` or `1` and no flag bit but bit 0; a decoded length of at most 64 MiB, equal to the stored length for encoding `0`; a required section this cig does not know; and a stage whose section is missing (`source` for `a`, `bytecode` for `b`).

**How it starts.** A start reads the trailer, the record and the table and checks the head sha256: a constant amount, whatever the script's size. It then refuses raised rights (`E1054`), answers cig's internal calls, refuses another cig's program (`E1051`), and checks the table's rules. `--help`, `--explain` and `--cig-program` answer there and read no body. A run and `--chains` read the `source` body, check its sha256 against the table and its text's sha256 against the record, and run it as `cig run` would. A body that does not match its table, or inflates past its decoded length, is refused with `E1050` when it is used.

**A stage `b` program** runs its bytecode on the VM. Its run reads the `bytecode` body and reads the module (`parse_module`: its magic, its version, its flags, which must be zero, and the BLAKE3 in its header over every byte after the header), verifies it and loads it (builtins, prelude names, burn-op kinds), and runs it through the same run as `cig run`: the same kernel, run record, journal, plan, rollback, `--no-rollback`, `--max-steps`, arguments, output and exit codes. Only the engine differs. Any failure before it runs is `E1050` with the rule and, from the verifier or the loader, where it failed (`the program's bytecode does not verify: <rule> at fn<i>+<pc>`, as `docs/BYTECODE.md` words it); nothing runs, no run is recorded, and the script is never run in its place. What a start checks, byte by byte: the head's sha256 covers the record and the table entry that says where the `bytecode` body lies, how long it is and how it is stored; the module's header is compared field by field; its BLAKE3 covers the stored hash and every byte after the header. A start does not also hash the body with the table's sha256 (decision 0004, section 5: a start never hashes a large body with sha256); `CIG_VALIDATE=1` and `cigcompile verify` do. The run's record carries the script's sha256 from the program's record, so `cig runs` and `--unburn` see one program at either stage.

`--interp` runs a stage `b` program's script on the tree-walker instead: it reads the `source` body, checks it, and runs it as a stage `a` program runs. On a stage `a` program it changes nothing. The source runs only when asked for: a damaged bytecode is never a reason to run it.

The `source` body of a stage `b` program is read only when something uses it, and checked the first time: `--interp`, `--chains`, `--light` when the name is not a chain the program declared (for the list of chains in the hint), and an error that quotes its line. When an error is reported and the `source` body does not match, the program's own error is still reported, with its position and without the quoted line, followed by `= note: the line is not shown: this program's source section is damaged (E1050): ...` (`note` in `--json`); the exit code is the program's error's.

**`CIG_VALIDATE=1`** makes a start read, hash and decode every section before the program runs, so a damaged body anywhere is refused at once, `--help` included.

### Format 1

```text
... cig ...                                the runtime, as released
payload                                    len bytes of JSON
"CIGPROG1"  len (u64, LE)  sha256(payload) the trailer, 48 bytes
```

- **Payload.** One JSON record, fields in a fixed order: `format` (1), `name`, `file`, `source`, `sha256` (of `source`, hex), `cig` (the version it runs on) and `compiler`. A start hashes the whole payload. A format 1 program is stage `a`, without own flags.
- **Limits.** A payload is at most 16 MiB.

The library keeps format 1's writer for one release, so the two formats can be run side by side.

### Both formats

- **The runtime's version.** Every cig from 1.15.0 carries `\0cig-runtime-version=<version>\0` in its bytes, so a tool can tell which cig a binary is without running it (one built for another machine cannot run here).
- **Words.** `name` is a letter or digit, then letters, digits, `.`, `_` or `-`, 64 at most. `file`, `compiler` and `cig` are printable text: no control characters, and none of Unicode's invisible ones (the format characters, the bidi controls among them, and the line and paragraph separators), so no name reads differently on a terminal from what it is.
- **Older cigs.** A cig from 1.15.1 to 1.21.x does not know `CIGPROG2`: it finds `{"format":` where its image ends and refuses a format 2 program as damaged (`E1050`, "its trailer is not at its end"); it never runs it as `cig`.

## Running one

```text
tidy [run flags] [--] [arguments...]
```

Leading flags are the run's. The first word that is not a flag, or everything after `--`, is the program's own. An unknown flag before that is refused (`E1052`), so a mistyped `--dry-run` never becomes a real run.

| flag | does |
|---|---|
| `--dry-run` | plans every burn and prints the plan |
| `--no-rollback` | leaves the burns in place if the program fails |
| `--max-steps N` | the step budget; `0` turns it off |
| `--json`, `--plain`, `--no-color` | output |
| `--no-doctor` | no automatic diagnosis on errors |
| `--light <chain>` | runs the program, then lights a chain it declares |
| `--chains` | lists the chains the program declares |
| `--interp` | runs a stage `b` program's script on the tree-walker instead of its bytecode; changes nothing on a stage `a` program |
| `--unburn <run>` | puts back a past run of this program |
| `--unburn-last N`, `--unburn-since AGE`, `--unburn-to <run>` | puts back a stretch of this program's own runs, newest first, as `cig unburn --last`, `--since` and `--to` do for any: one plan, then run by run, stopping at the first refusal (`cig unburn --continue` carries on) |
| `--seal-key-fd N` | reads the tin's key from open descriptor `N` (Unix): the key in no file and no environment variable |
| `--explain <code>` | what an error code means |
| `--cig-program` | what the program is and what it was compiled from; for format 2 also its stage, level, target, runtime sha256 and section table (id, stored and decoded bytes, encoding, required, sha256), with `--json` as `sections` |
| `--help`, `-h` | the program's usage |

`--max-steps`, `--unburn`, `--unburn-last`, `--unburn-since`, `--unburn-to`, `--explain`, `--light` and `--seal-key-fd` also take `--flag=value`. Exit codes are those of `cig run`: `0` ok, `1` script error, `2` syntax error, `3` usage.

### A program with its own flags

A script that says `own_flags: true` in its `pack` (`pack { ".", own_flags: true }`) compiles to a program whose record carries `own_flags`. Every argument on such a program's command line goes to its script as written, flags included: `tool --version`, `tool --help` and `tool -- x` all reach the script, which is what a program that stands in for a command-line tool needs.

Such a program takes **no run flags**, from its command line or from anywhere else: nothing outside the program can turn its run into a dry run, a help page or the undoing of an earlier run. What the run flags do is still available from outside it:

- `cig runs` lists its runs, and `cig unburn <run>` puts one back;
- `cig run <script>` runs the same script on the interpreter, with every run flag;
- the record and the section table are in the file (the format is above).

A program without `own_flags` is unchanged.

## Same as `cig run`, and what differs

The same: the kernel, burns, rollback, `--dry-run`, error positions (they point at `file` as compiled), and the burn protocol.

- **No check at start.** The script was checked when it was compiled, and the warnings were shown then.
- **No pin, no trial.** The program is its cig, so there is nothing to dispatch to or compare against.
- **Named runs.** `cig runs` lists a run as `<name> (program)` and shows the file it was compiled from, its sha256, its compiler and the binary. The record also carries `no_rollback`.
- **`--unburn` is for its own runs.** A run made by another script or program is refused (`E1053`); `cig unburn <run>` puts back any run. A journal written by a newer cig is not handed on: the program refuses and names `cig unburn`.
- **The undo line.** Run inside another cig's hop, a program reports `[<binary>, "--unburn", <run>]`, where `cig run` reports `[cig, "unburn", <run>]`.

## Integrity, not authenticity

The sha256s catch damage and truncation. They do not catch an edit, because anyone can recompute them, and they do not cover the runtime. Programs are unsigned until signing lands (cig-compile#40). A program whose bytes, record, table, sections, format or script do not match refuses to run (`E1050`). A program compiled for another cig refuses too (`E1051`).

A program knows it is one even when its trailer is gone: its payload starts where its runtime's image ends (the end of the last segment, section or header an ELF, Mach-O, universal or PE header places in the file), so a program cut short, or with bytes written after its trailer, is damaged (`E1050`), never `cig`. Two things may follow a trailer and leave a program whole: zero padding, and a PE certificate table, which Authenticode writes when a Windows program is signed after it is built (cig-compile#10).

## A program is never a general `cig`

A program's binary is `cig` for two calls only: the ashtray's background send (`crash flush --quiet`) and the box's network forwarder (`internal-forward <absolute socket> <port>`). Cig makes them with `CIG_INTERNAL_SELF` set, and the program answers that exact command line and nothing else. The marker is removed from the environment on start. A program never runs `cig update` (it never replaces itself) and never runs a repro.

- **A program never runs with raised rights** (`E1054`). Started setuid, setgid or with file capabilities (`AT_SECURE` on Linux, a real and effective id that differ elsewhere), it refuses before it reads its caller's environment: `PATH` for the commands it runs by name, `CIGSCRIPT_HOME` and `HOME` for its runs and keys. To let other users run one as its owner, give them a sudo rule for it.
- **A program that cannot read itself refuses** (`E1055`). Installed execute-only and run by someone without read permission, it cannot find its script, so it cannot tell that it is a program; rather than act as `cig`, it refuses. A binary named `cig`, `cig-runtime` or as a release asset (`cig-v1.15.1-linux-x86_64`) is `cig`, and runs as `cig` either way.

## Platforms

- **Linux:** the program reads its own image through `/proc/self/exe`, so replacing or removing its path while it runs changes nothing.
- **Windows:** it reads the file `current_exe` names.
- **macOS:** running is tested in CI; signing is cig-compile#40.

## Codes

| code | means |
|---|---|
| `E1050` | a damaged program |
| `E1051` | a program for another cig |
| `E1052` | not one of the program's run flags, a missing value, or an argument that is not UTF-8 |
| `E1053` | `--unburn` of a run this program did not make |
| `E1054` | a program started with raised rights (setuid, setgid, capabilities) |
| `E1055` | a program that cannot read its own file |

`E1000` to `E1010` are cigcompile's. The families `E1000` to `E1799` are listed in `errors/registry.toml`.
