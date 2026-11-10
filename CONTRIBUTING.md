# Contributing to CigScript

Bug reports, fixes, examples and documentation are welcome. This page is
short because the tooling does most of the checking.

## Reporting a cheat that worked

CigScript is built to be too robust to break without cheating, so the
invitation on the README is to cheat. A good report has four things: the
script (or the smallest one that still does it), the command you ran, what
the plan said, and what actually happened. `cig runs` gives you the run id;
`ciginstaller bug cig` (or `cig report <run>`, the short way in) writes up the
plan, the journal, the diagnostic and doctor's diagnosis, anonymised the same way crash reports are (every
path a placeholder, the command line reduced to the words cig knows,
secret-shaped values removed, file contents never included), and it's fine
to attach it after you've read it (`cig report <run> --out FILE` writes the
bundle alone); `cig report <run> --send` files it for you as an issue labelled
`report`. Run `ciginstaller status` first and paste what it says. The run record
under `~/.cigscript/runs/<id>/` has the journal and the snapshots if more is
needed. Use the [cheat issue
template](https://github.com/otmof-ops/cig-script/issues/new?template=cheat.yml).

cig also reports its own bugs: a crash, an internal error, or a tripwire
that found one of its promises broken after the work. Every such report
lands in the ashtray (`cig crash list`) before anything is sent, redacted,
with an anonymised repro built from the journal when one reproduces; it
goes only when you say so (`reports = ask` asks once and remembers;
`always`, `never`; under CI with nothing set, never). You can read the file
first; it's plain JSON. [docs/REPORTS.md](docs/REPORTS.md) has the whole
pipeline.

The reply you'll get is the same shape every time: what broke, why, the
fix, and the test that now covers it. No trial.

**Every fix for a cheat ships with a regression test named after the cheat**
(`symlinks_roll_back_as_symlinks`, `unburn_refuses_a_second_time_without_force`)
and a row on the README's wall that names that test. The wall and the test
suite are the same list; `tests/docs.rs` refuses a row whose test does not
exist. The wall never claims a cheat failed unless the test exists: honest or
nothing.

## Before you start

- **Bugs that are not cheats** (a wrong message, a missing hint): open an
  issue with the script, the command you ran, and the full error, including
  its code (`E502` and so on). If `cig` crashed or a tripwire went, `cig
  crash show <id>` has everything we need; send it with `cig crash send
  <id>` or paste it into the issue.
- **Features:** open an issue first and say what problem it solves. The
  language is kept deliberately small; `docs/DESIGN.md` explains why and lists
  what is already planned.
- **Security problems:** do not open a public issue. See `SECURITY.md`.

## Building from a checkout

Users install cig with `ciginstaller` (`ciginstaller install stick cig`; the
[README](README.md#install) has the details). The developer route is a build
from a checkout, with a Rust toolchain (1.82 or newer):

```sh
cargo build --release          # target/release/cig, run it from there
cargo install --path .         # or put it on your PATH from the checkout
cargo install --git https://github.com/otmof-ops/cig-script --tag vX.Y.Z cigscript
```

A build from a checkout is not managed by the installer: `cig update`,
`cig uninstall` and `cig doctor` hand over to `ciginstaller`, which does not
know that binary, and `cargo uninstall cigscript` removes it. The root
`install.sh` and `install.ps1` are redirects to the installer and install
nothing. The installer calls `cig __doctor --json` for cig's own checks, and
`cig __doctor` is how you run them from a checkout.

## The contributor agreement

CigScript uses a Contributor License Agreement, not a DCO. Before a pull
request from you can be merged, you sign the agreement once:

- individuals: [`.github/CLA/ICLA.md`](.github/CLA/ICLA.md)
- companies whose employees contribute: [`.github/CLA/CCLA.md`](.github/CLA/CCLA.md)

You keep your copyright. The agreement gives the project a licence to use,
relicense and distribute your contribution, a patent licence for what you
contribute, and your confirmation that the work is yours to give. It exists so
the project can, for example, offer a commercially licensed edition or move to
a newer licence later without tracking down every past contributor. Signing
is a comment on your first pull request:

> I have read the CLA Document and I hereby sign the CLA

A bot records it; you will not be asked again.

## The workflow

1. Fork, branch from `main`, make the change.
2. Every source file carries SPDX headers; `REUSE.toml` maps the rest.
   Code is `Apache-2.0`, documentation is `CC-BY-4.0`, examples are `CC0-1.0`.
   New files get the header of their kind (copy one from a neighbour).
3. Run the gate locally, exactly as CI does:

   ```
   cargo fmt --all --check
   cargo clippy --all-targets -- -D warnings
   cargo test
   ```

   Behaviour changes need a test. Kernel changes need a test that inspects the
   file system after a rollback, not just an exit code; the property test in
   `src/burn/journal.rs` should still pass with your op in its generator. New
   library functions need an entry in `docs/STDLIB.md`, which is generated:
   `python3 scripts/gen-stdlib-doc.py target/debug/cig`. New error codes go in
   `errors/registry.toml` (a node: title, meaning, what to type next, which
   kind of no, at least one cause with a probe), then
   `python3 scripts/gen-errors-doc.py target/debug/cig` regenerates
   `docs/ERRORS.md` and the schema; `tests/registry.rs` fails on a code that
   is emitted but not registered, or registered but never emitted. Doctor's
   wording is golden-tested: `CIG_UPDATE_GOLDEN=1 cargo test` accepts a
   change after you have read the diff.
4. Open the pull request against `main`. Describe what changed and why; link
   the issue. One logical change per pull request.

## Style

Rust: rustfmt and clippy decide. Prose: plain words, short sentences, no
hype. Error messages say what happened, and the hint says what to type next,
not what went wrong; every message gets a stable code. Nothing in a message,
a doc, a help text or a commit blames the reader or withholds the pointer.
Doctor says one sentence: *I think X, because I checked Y and found Z.*

## Coining a term

The vernacular lives in `docs/LEXICON.md`, three columns: your word, what it
means, the manual's name. Rules, from the lexicon itself:

1. **Name it by what it feels like, then attach the real name.** The joke is
   the mnemonic; the real name is the receipt. A term without a manual name
   is a tweet, not a doc.
2. **It has to map cleanly** to one real mechanism.
3. **It has to work in a hallway.** If you wouldn't say it out loud to a
   colleague to explain the problem, it's not a keyword.
4. **Plain mode is a subtraction, not a translation.** `--plain` keeps every
   code, span, hint and fact and removes the catchphrases; a term that cannot
   be subtracted cleanly does not go in.
5. **Never smug, never on trial.**

The lexicon's gap list is an open invitation; the [coined-a-term
template](https://github.com/otmof-ops/cig-script/issues/new?template=coined-a-term.yml)
requires the manual's name. A vernacular term used in any doc must resolve to
a lexicon entry; `tests/docs.rs` checks the README's table.
