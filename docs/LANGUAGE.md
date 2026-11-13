# The CigScript language

This is the contract for CigScript 1.x. `cig language` prints the short
version.

## The pin

A script says which cig it was written for, in a comment before its first
statement (after the shebang, if there is one):

```cig
#!/usr/bin/env cig
# cig 1.1
```

`# cig 1.1` means any 1.1.x, `# cig 1.1.2` exactly that one, `# cig >=1.1.2`
that or newer within 1.x; a pin never crosses a major version, because a
major version may change this contract. A project pins every script at once
with a `.cig-version` file beside them or in a directory above, holding the
same constraint; a header in the script wins over the file. A comment is
the pin so that every cig ever shipped reads past it; `# cig run s.cig` and
other usage lines in comments are not pins, because a pin starts like a
version.

An unpinned script runs on whatever cig is installed, as it always did. A
pinned script runs on the cig it asks for: when the installed cig does not
satisfy the pin, it hands the whole run (`run`, `--dry-run`, `check`,
`light`, `chains`) to that version from the humidor, `~/.cigscript/humidor/`,
fetching and verifying the release first when it is not there, and says so
(`run lit by cig 1.1.1 (pinned in s.cig); installed is 1.2.0`). No network
and nothing on the shelf is a refusal (`E807`) with the command that fetches
it, never a silent run on the wrong version. `cig versions` lists the
shelf, `cig versions add 1.1.1` stocks it, `cig pin s.cig` writes a pin for
the installed version. A run is put back by the cig that wrote its journal:
`cig unburn` reads the record's version and hands over the same way
(`E808` when that cig is not on the shelf).

Every part of the language and library knows the release it arrived in.
`cig check` reads a pinned script against that table and warns (`E309`)
when the script uses something newer than its pin allows, before anything
runs; `cig pin <file>` with no `--to` writes `# cig >=X.Y.Z`, the lowest
version that has everything the script uses.

**The trial.** A script's *last good* version is the cig that lit the newest
run of that exact source which went through. Before a run on a newer cig,
when the last good version is in the humidor, cig dry-runs the script on
both and compares the plans op by op (`trial = auto`, the default). Identical
plans: the run goes ahead on the installed cig, and the report says so.
Different plans: the difference is printed and the proved version runs the
script. If the run on the newer cig fails and every op it burned came back,
cig runs the script once more on the last good version and names both
outcomes; after an irreversible op it stops instead, because a trial never
repeats one. `cig run --trial` fetches the last good version when it is
missing, `--no-trial` runs here regardless, and `cig config trial never`
turns it off.

## The levels, in one line each

A **stick** is one step. A **chain** is order: sticks that light from each
other, fail together, roll back together. A **pack** is scope: the roots a
script's writes stay inside (`pack { "./build" }`, the first statement), the
boundary hop watching and the tin keep, one journal for everything inside;
the composition sense, a chain with its own root lit from anywhere, is
still to come, as are parameters and cartons. A **carton** (planned) is
distribution. Nobody has to be taught the hierarchy; the words are in
[LEXICON.md](LEXICON.md) with the manual's names.

## Files and statements

A script is a `.cig` file, UTF-8, one statement per line. A `#` starts a
comment, so `#!/usr/bin/env cig` works as a shebang. A `;` separates
statements on one line. Blocks are `{ ... }`. Newlines are ignored inside
parentheses, brackets and map braces, and after a binary operator or a comma,
so long expressions wrap naturally.

Statements execute top to bottom. `pull` declarations are hoisted within their
block, so a function can be called above the line that defines it.

## Values

| type | literals | notes |
|---|---|---|
| `null` | `null` | absence; `??` and `?.` are built for it |
| `bool` | `true`, `false` | |
| `int` | `42`, `1_000`, `-7` | 64-bit, `-9223372036854775808` to `9223372036854775807`; overflow is an error (`E5xx`), a literal that does not fit is `E104` |
| `float` | `2.5`, `1e9` | 64-bit; prints as `2.0`, never `2` |
| `string` | `"text ${expr}"`, `'raw'` | UTF-8; indexing and `len` count characters |
| `list` | `[1, "two", [3]]` | ordered, mutable, shared by reference |
| `map` | `{name: 1, "with space": 2, [expr]: 3}` | string keys, insertion order kept, shared by reference |
| `function` | `pull f() { }`, `pack(x) => x` | closures capture their scope |
| `chain` | `chain name { a, b }` | an ordered list of steps; see `docs/CHAINS.md` |

Double-quoted strings understand `\n \t \r \0 \\ \" \$ \u{1F6AC}` and
`${expr}` interpolation. Single-quoted strings are raw: no escapes, no
interpolation, handy for regexes and Windows paths.

**Truthiness.** `null`, `false`, `0`, `0.0`, `""`, `[]` and `{}` are false;
everything else is true.

**Equality.** `==` is structural: lists and maps compare by content, `1 == 1.0`
is true, `"1" == 1` is false. Functions compare by identity.

## Declarations and assignment

`roll` declares a variable (a mutable binding); `stick` declares a constant.

```cig
roll x = 1          # variable
stick y = 2         # constant; `y = 3` is a check error
x = 5
x += 1              # also -= *= /=
list[0] = "a"
map.key = "b"
map["key"] = "c"
```

A name can be declared once per scope; shadowing in an inner scope is fine.
Blocks, loops and functions each open a scope. Assigning to a name that was
never declared is an error, not a silent global.

## Functions

`pull` defines a named function; `pack` is a lambda (an anonymous function);
`snuff` returns.

```cig
pull add(a, b) {
  snuff a + b          # snuff returns; a bare `snuff` returns null
}
stick inc = pack(x) => x + 1              # expression body
stick log = pack(x) { exhale x; snuff x } # block body
```

Calls take exactly the declared number of arguments. Functions close over the
variables around them, including mutable ones, so counters and accumulators
work. Recursion is fine up to a depth of 4,000 calls, after which the run fails
with a clear error instead of crashing.

Calling a function stored in a map works as you expect: `obj.greet("x")` calls
`obj.greet` with `"x"`. There is no implicit `self`.

## Control flow

Every statement and every loop iteration is a *step*, and a run has a step
budget: ten million by default, which a tree-walking interpreter spends in a
few seconds. A loop that never ends therefore ends, with `E515`, *your loop
never ends*, and the line of the `while`. `--max-steps N` (on `cig run` and
`cig light`) or `CIG_MAX_STEPS=N` raises it; `0` disables it. Burns made
before the budget ran out are rolled back like any other failure.


```cig
if a > 1 { } else if a < 0 { } else { }
while cond { }
for item in xs { }        # lists; maps give their keys; strings their characters
for i in 0..10 { }        # ranges are half-open
break
continue
```

`for` binds a fresh variable per iteration, so closures created inside the
loop see the value from their own iteration.

## Errors

```cig
cough "message"                       # raise with a string
cough {message: "no space", code: 28} # or a map; extra keys ride along
try {
  risky()
} ashtray err {
  exhale err.message, err.kind, err.line, err.code
}
```

`finally { }` after the `ashtray` block (or straight after `try`, with no
`ashtray`) runs whatever happened: after a normal exit, after a caught or
uncaught error, after `snuff`, `break`, `continue` and `exit()`. Its own
error or return wins over the one in flight; otherwise the pending one
carries on, with its payload intact.

```cig
try {
  stick fh = open_something()
  use(fh)
} finally {
  close_something()        # runs either way
}
```

`ashtray` catches everything raised inside its `try`, including runtime errors
from the library (`err.kind` is then `runtime`, `type` or `burn`). The variable
is optional. An uncaught error ends the run with exit code 1 and rolls back the
run's burns. `exit(code)` stops the script cleanly and is not catchable.

## Operators, highest precedence first

| operators | meaning |
|---|---|
| `f(x)`, `a.b`, `a?.b`, `a.m(x)`, `a[i]` | call, member, null-safe member, method, index |
| `-x` | negation |
| `* / %` | `/` on two ints stays an int when exact, otherwise gives a float |
| `+ -` | `+` also joins strings and lists; `"s" * 3` repeats |
| `a..b` | range as a list of ints |
| `== != < <= > >=` | comparisons; not chainable |
| `??` | left value unless it is null |
| `not` | |
| `and` | short-circuit, returns the deciding value |
| `or` | short-circuit, returns the deciding value |
| `>>` | pipeline: `x >> f(a)` is `f(x, a)`; `x >> f` is `f(x)` |

Member access on a missing map key is an error; use `m?.key` to get null, or
`m.get("key", default)`. Negative indexes count from the end.

## Output

`exhale a, b` prints the values separated by a space, with a newline, to
stdout. Strings print raw; everything else prints as its literal form, so
`exhale [1, "a"]` shows `[1, "a"]`. Use `log.info` and friends for commentary
on stderr, keeping stdout for the program's real output.

## Burn blocks

`burn { }` is the effect fence: the only place the world changes. `burn unlit
{ }` is rehearsal: every motion, no burn, even in a real run; the intent is
recorded instead.

**Compensations.** `burn (state) { ... } unburn { ... }` attaches the undo
for what the kernel cannot see (a push, an API call, a database write). The
burn block declares `state`, a fresh map it fills; when the block completes,
the compensation's source and the state map are journaled together. On
rollback, in this run or by `cig unburn` weeks later in another process, the
compensation runs with that state, newest first. Because it runs from the
journal alone, an `unburn { }` block may use only its state map, the prelude
modules and its own names; anything else is `E754`, before the run. The plan
shows hops inside such a block as `compensated` instead of `irreversible`.

A burn can carry a name: `burn ("deploy") { ... }`, or `burn (s, name) { ... }`
with a state map, any expression that is a string. The plan, the run record
and a rollback then name the burn by it (`deploy is NOT undone: its
compensation failed: …`) instead of by its line, which for a tool written in
CigScript is the tool's own source and no help to its user.

```cig
burn (s) {
  s.before = proc.text("git", ["rev-parse", "origin/main"]).trim()
  proc.run("git", ["push", "origin", "main"], {check: true})
} unburn {
  proc.run("git", ["push", "--force-with-lease", "origin", "${s.before}:main"], {check: true})
}
```

```cig
burn { ... }         # side effects allowed here, journaled, rolled back on failure
burn unlit { ... }   # side effects recorded as intent and never executed
```

Inside a normal burn, functions marked *burn* in `docs/STDLIB.md` may run. A
function called from inside a burn inherits the permission, so helpers can be
written once and lit by their caller. Outside a burn those calls fail before the
script starts (when the call is visible to the checker) or at the moment of the
call (when it is inside a function). See `docs/BURN.md` for the modes, the
journal and what rollback can and cannot undo.

## The pack

`pack { "./build", "~/Downloads" }`, first statement of the script, declares
the scope every native write must stay inside: the checker refuses a literal
path outside it before the run (`E750`), the kernel refuses a computed one at
run time (`E751`), and the plan prints the pack beside the ops. `~` expands;
relative roots resolve against the working directory. Hops are watched
rather than fenced: the kernel compares the pack and the child's working
directory before and after a child process, journals the files it created
as reversible (rollback removes them) and the files it modified or deleted
as irreversible with the detail, and reports what the child wrote in its
working directory outside the pack as `E752`, so nothing is hidden and
nothing is laundered. What a child writes anywhere else (`/etc`, another
tree) is not watched: the hop's irreversible label is the whole record of
it, and the way to keep such a step honest is `burn (s) { } unburn { }`.
One pack per script (`E753`); parameters and cartons are the next seam of
`HAMMER.md`.

A pack may also carry an option beside its roots, in any order, written as
a name, a colon and a literal: `pack { ".", own_flags: true }`. There is one
option:

- **`own_flags: true`** is a fact about the program a compiler makes from
  the script, read when it is compiled (`docs/PROGRAMS.md`): such a program
  gives every argument on its command line to the script. It changes nothing
  about `cig run`, and nothing about the pack's scope.

An option cig does not know, one given twice, or a value that is not the
literal `true` or `false` is refused before the run (`E766`). A pack with
only options, `pack { own_flags: true }`, declares **no** scope: writes are
exactly as in a script with no `pack`, and a plan prints no pack. A pack
with a root fences the same whether an option is there or not.

## Chains

```cig
chain release { fetch, build, package }
stick report = light(release, {retries: 2})
```

A chain is a constant holding an ordered list of steps, each a function
value or another chain. `light(chain, opts?)` runs them in order, passing
each step's result to the next step that takes an argument, and stops at the
first failure with an error that names the step. `cig light file.cig release`
does the same from the command line, and `cig chains file.cig` lists what a
file declares. The whole story is in `docs/CHAINS.md`.

## Keywords as names

Keywords are reserved as statement starters and as the names of declarations:
a keyword cannot be declared as a variable or a parameter (`stick chain = "a"`
is `E200 expected a name, found \`chain\``, so a certificate chain is `cert_chain`
and a package is `pkg`). After a dot and as a map key they are ordinary names,
so `err.chain`, `m.if`, `m.finally` and `{chain: 1}` are fine. The one place
this matters is the error map a failed chain raises, which has a `chain` key.

## Error codes

Every error `cig` reports carries a stable code, rendered as
`error[E502 runtime]: division by zero`. The first digits name the family:
E1xx lexing, E2xx syntax, E3xx the checker, E4xx types, E5xx the runtime
(E52x the ghost filesystem, E55x hops), E6xx raised by the script, E7xx the
kernel (E75x folds and the pack), E8xx usage, E9xx internal. Every code is a
node in `errors/registry.toml` with its kind of no, what to type next and its
ranked causes; `docs/ERRORS.md` is generated from it, `cig explain E502`
prints the page, `cig explain --schema` prints the JSON Schema `--json`
output follows, `cig explain --layers sales.cigar:12:1` hands the place to
cigar, which peels CigarScript's layers there, and doctor uses the same table
to diagnose. Codes are stable
across versions; a retired code is never reused.

## Script arguments

Everything after `--` on the command line is `args`, a list of strings:

```
cig run tidy.cig -- ~/Downloads --verbose
```

`args.get(0, "default")` reads optional arguments safely.

## What is deliberately not here

No classes, no imports, no `match`, no async, no random numbers, no C-style
`&& || !`, no ternary, and only one syntax for each construct. Every one of
those was tried in the 1.x line and made the language larger without making
scripts shorter. The roadmap in `docs/DESIGN.md` lists what may come back.
