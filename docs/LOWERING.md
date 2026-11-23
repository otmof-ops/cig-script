# The lowering

The reference for `cigscript::lower` (`src/lower/`): CIR, its passes and
verifier, and the lowering to the register bytecode. `cig run` and
cig-compile share it. These sections moved here from cig-compile's
`docs/how-it-works.md`; `--emit`, `-O` and `--passes` are cig-compile's command line.

## The IR and the passes

`--emit cir` prints the IR ([#3](https://github.com/otmof-ops/cig-compile/issues/3)): CigScript lowered to an SSA-like
form that each pass verifies before the next runs.

The IR carries the step budget (`--max-steps`, `CIG_MAX_STEPS`) the
way the interpreter counts it. A `step` opens every statement except
a `pull`, and a `step.iter` opens every loop body: one mark for each
tick of the interpreter's counter, at the same span. The verifier
holds them in place. A mark never moves, a loop body always opens
with its `step.iter` (V11), and a pass removes a mark only together
with everything else in its block (T3). A compiled program stops on
the same step, at the same line, as the interpreter.

Errors come out where the interpreter puts them. A map literal is
lowered in the interpreter's order: each computed key, then its
`key` check, then the entry's value. A key that is not a string or
an int raises before a later value can. An index, an indexed store, a
member store and a map's computed keys carry the spans of their own
operands
(`--emit cir` prints them beside the instruction's span, as
`idx@3:10`). A wrong index type is reported at the index, and a
value that cannot be indexed at the object, even after a pass has
replaced either operand with a value made somewhere else. The
verifier checks every such span lies inside its instruction (V2).

The passes today are
`fold-consts`, `copy-prop`, `fold-consts`, `sccp`, `dead-pure`, in
that order. Each is read as:

- **fold-consts** replaces constant expressions with their value.
- **copy-prop** replaces `stick a = b; use a` with `use b`, through
  slots that are neither mutable nor captured.
- **sccp** empties the dead side of an `if` whose condition is a
  known boolean constant, unless an effect in the dead branch means
  emptying it would delete work (T3 catches that shape).
- **dead-pure** prunes pure expressions whose result is unused.

Each instruction has a class, read from its kind and the types of its
operands. A **pure** one (a constant, a fresh list, `==`, float
arithmetic, a read of a `stick` where it lives) may move, merge or
vanish. One that **may raise** (integer arithmetic that can overflow,
`/`, an index, a member that can be missing, a float ordered against
a NaN) keeps its place and its order, and goes only where the
verifier proves it cannot raise: its operands are constants that the
interpreter's arithmetic folds, and the constant left in its place is
that result (T4). **Effects** (slot writes, calls, `exhale`, burn
ops) and step marks keep their place and order and are never lost
(T1 to T3), except with a whole `if` arm that a constant condition
never takes. Each pass is its own module, with a golden folder under
`tests/lower/cir/` and a deliberately broken twin that the verifier must
catch.

`--passes-diff` adds a unified diff of what each pass changed, or a
line that says it did not; `--remarks` says the same thing in
prose. Neither changes what a program does: the verifier re-reads
the IR after each pass. A stage `b` program's bytecode is lowered at
`-O`'s level (`--passes` shapes `--emit` and `--remarks` only), and the
program records the level, so `verify --script` lowers it again the
same way; a stage `a` program runs the script, whatever the level.

### Types across calls

`cigscript::lower::cir::infer` is an analysis of CIR that assigns a type
(one of `null`, `bool`, `int`, `float`, `str`, `list`, `map`, `func` or
`dynamic`, with `dynamic` on top) to every value and slot of every
function reachable from the top level, under each row of argument types
the top level makes that function see.

The unit is a specialization: one function and one row of argument types.
A function keeps at most three rows; when a fourth is asked for, the
function collapses to a single row of all `dynamic`, and the three
previous rows are dropped. The cap is cig-compile's
[decision 0005](https://github.com/otmof-ops/cig-compile/blob/main/docs/decisions/0005-what-goes-native-first.md)'s.

A call is asked for once every argument of it has a type: the result of
a function the analysis has not reached yet is no type, and asks for no
row. A call an argument of which never gets one (the result of a
function that never returns) is in code that cannot run. The result
holds the specializations the top level reaches through its calls, and
nothing an earlier pass left behind, so it is the same whatever order
the analysis took.

**What a type promises.** When the analysis gives a value, a variable or
a function's result any type but `dynamic`, then in every run in which
that function is entered with arguments of that row, it has that type.
`dynamic` promises nothing. Code compiled from the analysis holds values
as their type with no check, so wherever a rule cannot be shown to hold
the answer is `dynamic`.

The analysis is structural. A value's type comes from the operands, by
the same rule the interpreter follows in `src/interp/ops.rs`: an `Int`
plus an `Int` is `Int`, an `Int` plus a `Float` is `Float`, an `Int`
divided by an `Int` is `dynamic` (it is `Int` when the division is exact
and `Float` when it is not, so neither answer is sound), the orderings
return `Bool` when they return at all, `==` and `!=` are `Bool` whatever
the operands. A slot's type in its owner is the join of every `decl` and
`store` to it in the owner, and for a parameter also its row type. A
`foreach` variable is `Int` when its iterator is `int..int` and
`dynamic` otherwise; a `catch` variable is `dynamic`.

Captures follow one rule each, decided from `VarInfo` alone, before any
walk, so the answer cannot depend on the order the worklist happens to
process functions in:

- A slot that is **`mutable` and `captured`** is `dynamic` everywhere:
  in its owner's own slots (set before any walk) and at every `load` of
  it by any function. A `store` from an inner function to such a slot
  touches nothing in the inner function's spec (the slot does not
  belong to it); it does not raise the owner's slot above `dynamic`
  either, because the owner's slot is already there.
- A `load` of a slot by a function that does not own it reads the IR's
  own static type of the slot's one `decl` value
  (`Func::vals[val].ty`): a constant stays its constant type, a
  closure is `func`, a list is `list`; anything the lowering could not
  type statically is `dynamic`. That type depends on no row and no
  spec, so a later call that gives the owner a new row cannot make the
  reader go stale. The owner's own loads of the same immutable slot
  still read the owner's spec slot (which is the `decl`'s value type
  under the owner's row): V1 and `Builder::early_use`
  (`src/lower/cir/lower.rs`, "A name read before its declaration")
  make every such load run after the `decl`.

Recursion and loops reach a fixed point: a specialization under
inference answers its current return type (which starts as "nothing
reached yet" and is reported `dynamic` if the walk ends with no result
reaching it), and dependents are re-walked when the answer moves up.
Every move is one step up a lattice of height two, and rows are
capped at three per function, so the walk ends.

A `Spec`'s type for a value or a slot is sound: in every run that
enters the function with its row of arguments, the value is of the
listed type (or the type is `dynamic`, which promises nothing). What
`dynamic` means: the analysis could not prove a single type for every
run. Code that cannot run is walked (nothing is pruned), and a value
left at bottom there is reported `dynamic`.

Nothing in `cig run` or stage `a` and stage `b` reads the inference yet:
stage `c` (otmof-ops/cig-script#256) uses it to pick the
specializations a program carries native code for; the VM's call
chooses between the bytecode and a native entry by the tags of the
arguments, independent of what the inference said.

### The bytecode lowering

`src/lower/bc` lowers verified IR to the register bytecode of
[#31](https://github.com/otmof-ops/cig-compile/issues/31), the format `docs/BYTECODE.md` sets out, and
runs the format crate's verifier on every module it makes (a module
that does not verify is the compiler's bug). A slot is a register for
the whole function; a value has a register from its definition to its
last use. A read of a slot no closure captures is the slot's register
itself, with no copy, when nothing writes the slot before the read's
last reader, and a value stored into such a slot right after it is made
is made in that slot's register (`add R1, R1, R8`), also when other
readers read it later (a `stick` that `-O1` forwards), as long as each
of them reads the slot as it was stored: after the store in the same
block, or inside a later statement of that block that writes the slot
nowhere, and nothing in between writes it. An operation that fails
writes nothing, so the slot keeps its value as in the interpreter. A
constant right operand is the instruction's own (`addk` to `modk`,
`eqk`, `ltk`, `lek`; a bool constant too); a constant left operand
swaps over only where the value and the error are the same either way
(`==`, `!=`, and `+` or `*` of two numbers), and a constant every
reader takes this way is never loaded. A constant inside a loop that
an instruction reads from a register is loaded once, before the
outermost loop around it, into a register held until that loop is
done and below 256 (past that it is loaded where it is read): a
literal is the same value every time and loading it cannot fail, so
the loop cannot tell. A call's callee and arguments sit in consecutive
registers, its window; a callee, a receiver or an argument only the
call reads, made in the call's block, is made in its place there, the
window taken before it is evaluated and kept until the call, so
nothing else writes it in between; the rest are moved in. A value
block (`and`, `or`, `??`, `?.`) makes its subject and the value its
right side yields in its own result register when nothing else reads
them.
A `call`'s window is taken above every register in
use, because the callee's frame starts at the window's second register
and runs over everything above it (`docs/BYTECODE.md`, what
a call leaves undefined; its verifier refuses a read of such a register
before a write): a captured slot's register is given at the function's
start, below every window, and a window taken early for an argument is
moved to the top, its arguments with it, when a value made since sits
above it and outlives the call. `callb`, `callm` and `burn.op` make no
frame and keep the lowest free run.
`a > b` is `le a, b` then `not`, so the operands keep the
order the interpreter's errors name. A comparison read only by the
`if` or loop condition after it is that branch's jump (`nej`, `eqj`,
`lej`, `ltj`; `a < b` and `a <= b` jump over a `jmp`), with the same
operator, operands and position; one too far for the jump's 8-bit
offset is a comparison and a `test`. A step mark is one `steps`
instruction, a loop sets its nesting before its condition and back
after it (folded into the next statement's mark when that follows at
once), and the line table keeps the IR's span for every instruction.
A slot a closure captures lives in a cell: a fresh one each time its
block runs (each turn of a loop), so a closure made in a turn keeps
that turn's value, and a pull calls itself or a sibling through the
cell its closure was declared into. `for` over `a..b` counts with
`forprep`/`forloop`, and over a list, a map or a string walks the
interpreter's snapshot with `iterprep`/`iternext`; the item is fresh
each turn. An index read is one `getindex` and a plain indexed store
one `setindex`, each followed by a `nop` carrying the index's position
(and a store's, a second carrying the object's), taken from the spans
the IR stores: the VM reports a wrong index type at the index and a
value that cannot be indexed at the object, as the interpreter does,
and never runs the `nop`s. A compound store (`xs[i] += v`, `m.k += v`)
checks its operands' types first, so its error stays the store's
rather than the read's. An `exhale` part or interpolation piece is
turned to text where the interpreter shows it, before a later part
can change it. A computed map key is
checked by its own `key` instruction, before the entry's value runs.
A `try` is laid out as the format sets out: `try.enter`, the body
(the handler's range), the `ashtray` block, then the `finally` block,
each ending with `try.leave`; `break`, `continue` and `snuff` leave
every `try` they cross first, so each `finally` runs, and a snuffed
value waits in a register no `finally` writes. `cough` is `throw`, and
a builtin, a module or `args` used as a value is `loadg`, read from
the prelude by name. A burn is its state map, `burn.enter` with its
region (class and label), `comp.enter` with the `unburn` block's source
and line, the body, `comp.leave` and `burn.leave`. A label that is not
a string literal (`burn (s, name)`) is evaluated before the state map,
as the interpreter evaluates it, and the region reads it from that
register each time `burn.enter` runs, so a burn in a loop takes each
turn's label; a `nop` after `burn.enter` carries the label's position,
where a label that is not a string is reported. `break` and `continue`
leave every `try` and burn they cross, innermost first, then jump, and
`snuff` leaves the tries (its `ret` closes the burns); nothing follows
a way out that no run would reach, and a body that ends in one ends
its burn's row there. A call that
changes the world is `burn.op` with the VM's own kind for its builtin,
and a function with one outside its own burns is marked
`requires_burn`, so it runs inside a caller's burn and is refused with
`E701` outside one, as in the interpreter. `pack` and `chain` check each
root and step right after it is evaluated, as the interpreter does, so
a bad root is reported before a later one runs. A `chain` statement of
the top level is also declared as a top-level name (`chain.make` with
`C = 1`), and the top level's normal end returns a map of every name
its statements declare, each with its value then, which the runtime
declares after the run, so `--light <name>` answers as it does for the
interpreter. One thing the bytecode cannot carry is refused with its
construct and position: a `pack` root that runs a pull or a function.
`tests/lower_bc.rs` runs
what it lowers in the VM and holds its output, errors, positions and
step count to the tree-walker's, and for burns the files, the journal,
a dry run's plan and what a rollback puts back.

## A name read before its declaration

The interpreter looks a name up as the script runs. A function that reads
a name before the name's own statement has run finds the next one out, or
none:

```cig
main()                          # E501: unknown name `LIMIT`
stick LIMIT = 10
pull main() { exhale LIMIT }
```

```cig
roll x = 1
pull outer() {
  pull f() { snuff x }
  roll before = f()             # the outer x: 1
  roll x = 2
  snuff [before, f()]           # then the inner one: [1, 2]
}
```

A lowered function reads a slot, and a slot has no "not declared yet". So
the lowering takes a function that captures a name its block has not
declared yet only where the function cannot run before the declaration:

- a `pull` is hoisted, and may read any name its block declares, as long
  as no statement up to and including that declaration names the pull,
  directly or through the block's other pulls;
- a function literal bound to a name may read that name (it cannot run
  before its own declaration is done);
- any other function made before a declaration it would capture is
  declined.

Naming a pull is enough: the lowering does not follow a value to where it
is called. What is declined is not an error. `cig run --engine vm` runs
the script on the tree-walker and its record says why; `cigcompile build
--stage b` refuses the build with the same text, and stage `a` builds it.
`tests/lower_names.rs` holds each shape, and `tests/engines.rs` holds the
two engines to each other on whole scripts.

## A direct call reads the pull's own slot

A call that names a `pull` is a direct call: the lowering knows the
function, and the bytecode reads its closure from the slot the `pull` was
declared into, at the top of its block. That slot is the first declaration
of the closure's value, and only that one. A pass may give the same value
to a later declaration (`roll g = f`, once the read of `f` is propagated
away), and `g` is not the pull's slot: it is empty until its statement
runs, and it may be assigned again.

```cig
pull o() {
  pull f() { snuff 1 }
  exhale f()                    # reads f's slot, not g's
  roll g = f
  g = pack() => 2
  exhale [f(), g()]             # [1, 2]
}
```

`tests/walker/resolve-alias.cig` and the `pull-aliased` programs of
`tests/lower_bc.rs` hold it.
