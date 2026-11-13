# The bytecode format

CigScript's stage-B VM runs a register bytecode. This file is the
format's **contract**: everything in `crates/cig-bytecode/` implements
it, the VM core runs it, CigCompile's lowering writes it, and future
packages can rely on the rules below without reading the crate's code.
See otmof-ops/cig-compile#31 for the stage-B issue.

The format's version is `1`. A later reader opens an earlier module; an
earlier reader refuses a later one with `E1050`.

## File layout

A module file is a header followed by a payload. Everything is
little-endian.

### Header (40 bytes)

| offset | size | field | notes |
|--------|------|-------|-------|
| 0 | 4 | magic | ASCII `CIGB` |
| 4 | 2 | format version | `1` in this revision |
| 6 | 2 | flags | reserved; `0` in this revision |
| 8 | 32 | integrity hash | BLAKE3-256 of the payload |

A file whose magic is not `CIGB` is refused as not a bytecode module
(`ParseError::BadMagic`). A file whose version is higher than the
reader's is refused (`ParseError::BadVersion`). A file whose integrity
hash does not match the hash of its payload is refused
(`ParseError::BadIntegrity`).

### Payload

The payload is the module's `source_name`, then its one top-level
function. Each function is self-contained (see "Per-function tables"
below), so a function's bytes do not depend on anything above it in
the module.

1. `source_name: str` — the source file name, or empty.
2. `main: Function` — the top-level function. All other functions live
   under it as children.

The parser refuses trailing bytes (`ParseError::TrailingBytes`).

A `str` is a `u32` length in bytes followed by that many UTF-8 bytes.

A `[T]` is a `u32` count followed by that many `T`s.

### Per-function tables (self-contained)

Each function carries its own:

- `constants: [Constant]` — the function's constant pool;
- `regions: [BurnRegion]` — the function's burn-region table;
- `handlers: [HandlerEntry]` — try/catch/finally regions;
- `instrs: [Instr]` — the instruction stream;
- `span_base: SpanBase` — the function's anchor for line rows;
- `lines: [LineEntry]` — one row per instruction pc that can fault;
- `region_rows: [RegionRow]` — pc-range to region id (into this
  function's `regions`);
- `children: [Function]` — the functions this one may `CLOSURE`
  (indices are local to this function).

`CLOSURE A, Bx` names the child at `children[Bx]`. A constant
reference names `constants[idx]`. A region reference names
`regions[idx]`. **Operand indices never cross a function boundary.**
A function's bytes therefore depend on the function alone; adding a
constant above it does not renumber anything inside it.

### Constant

A tagged union.

| tag | type | body |
|-----|------|------|
| `1` | `Bool` | `u8` (0 or 1) |
| `2` | `Int` | `i64` little-endian |
| `3` | `Float` | `u64` little-endian, treated as IEEE-754 bits (NaN payload round-trips) |
| `4` | `Str` | `str` |

A `null` constant is not needed: `LOADNULL` loads null directly.

### BurnRegion

| field | type | notes |
|-------|------|-------|
| class | `u8` | `0` = normal, `1` = unlit |
| label | `u32` | string-constant index into this function's pool, or `0xFFFF_FFFF` for none |

### Function

| field | type | notes |
|-------|------|-------|
| name | `str` | pretty name, empty for lambdas |
| kind | `u8` | `0` main, `1` pull, `2` lambda |
| reg_count | `u32` | number of registers in the frame |
| arity | `u16` | parameter count |
| requires_burn | `u8` | `0` or `1` |
| upvalues | `[Upvalue]` | u16-count prefix |
| constants | `[Constant]` | u32-count prefix |
| regions | `[BurnRegion]` | u32-count prefix |
| handlers | `[Handler]` | u32-count prefix |
| instrs | `[Instr]` | u32-count prefix, each 32 bits |
| span_base | 3 × `u32` | `(byte_offset, line, col)` of the first token |
| lines | `[LineEntry]` | u32-count prefix |
| region_rows | `[RegionRow]` | u32-count prefix |
| children | `[Function]` | u32-count prefix; recursive |

An `Upvalue` is `(u8 kind, u16 index)` where `kind` is `0`
parent-local (parent's register) or `1` parent-upvalue (parent's
upvalue).

A `Handler` is `(u32 pc_start, u32 pc_end, u32 handler_pc,
u32 catch_reg, u32 finally_pc)`. `catch_reg` and `finally_pc` are
`0xFFFF_FFFF` when absent. Every `pc` field must be a first-word pc
(see Instructions below).

A `LineEntry` is `(u32 pc, u32 span_off, u32 span_len, u32 line_off,
u32 col)` where `span_off` is bytes from `span_base.byte_offset`,
`line_off` is lines down from `span_base.line` (0 = same line) and
`col` is 1-based (absolute, since columns reset).

A `RegionRow` is `(u32 pc_start, u32 pc_end, u32 region)`. `region`
is an index into this function's `regions`, or `0xFFFF_FFFF` to mark
a pc range as outside every burn.

### Instr

A fixed 32-bit word, little-endian on the wire.

```
  bits 0-7   : opcode
  bits 8-15  : A (u8)
  bits 16-31 : B+C  (two u8 for ABC, one u16 for ABx, one i16 for AsBx)
```

The opcode table is in `crates/cig-bytecode/src/opcode.rs`. Each
opcode declares a format (ABC / ABx / AsBx) and per-slot operand
kinds: register, constant, string constant, function (child), upvalue,
handler, region, builtin index, burn class, packed burn op, pc-relative
signed jump, a plain immediate, an argument count, or the wide-prefix
extension.

### The `WIDE` prefix

`WIDE` is itself an ABx-layout word whose `A` and `Bx` are the **high
bits** of the next instruction's operands. Prefixed operands widen to:

- ABC next: `A = wide.A << 8 | next.A` (16-bit `A`), `B = wide.Bx[0..8]
  << 8 | next.B` (16-bit `B`), `C = wide.Bx[8..16] << 8 | next.C`
  (16-bit `C`);
- ABx next: `A = wide.A << 8 | next.A` (16-bit `A`), `Bx = wide.Bx <<
  16 | next.Bx` (32-bit `Bx`);
- AsBx next: the same, with the 32-bit value read as signed.

The whole crate decodes through one function,
`crate::instr::decode(instrs, pc)`, which returns a `DecodedInstr`
with full operands and a `pc` that is the first-word pc. The
verifier, the disassembler and the VM all call it.

**`WIDE` is never** the last word of a function, and may not be
followed by another `WIDE`. The verifier checks both.

**Nothing may point at the second word of a prefixed pair.** Jump
targets, every handler pc field, every region row's `pc_start`, and
every line row's `pc` must each be a first-word pc. Exclusive `pc_end`
fields may be any instruction start or the function's length.

## Instructions, by group

Each entry says which operands carry what. `R(x)` is a register,
`K[i]` is a constant in this function's pool, `pc + offset` is a
signed pc-relative jump, `U(i)` is an upvalue.

### Moves and constants

| op | fmt | effect |
|----|-----|--------|
| `nop` | ABC | — |
| `mov` A, B | ABC | `R(A) = R(B)` |
| `loadk` A, Bx | ABx | `R(A) = K[Bx]` |
| `loadnull` A | ABC | `R(A) = null` |
| `loadbool` A, B | ABC | `R(A) = (B != 0)` |
| `loadint` A, sBx | AsBx | `R(A) = sBx as int` |
| `loadg` A, Bx | ABx | `R(A) = the prelude value named K[Bx]` (a builtin, a module or `args`; `K[Bx]` is a string) |

### Arithmetic

| op | fmt | effect |
|----|-----|--------|
| `add`/`sub`/`mul`/`div`/`mod`/`pow` A, B, C | ABC | `R(A) = R(B) ∘ R(C)` |
| `neg` A, B | ABC | `R(A) = -R(B)` |
| `addk`/`subk`/`mulk`/`divk`/`modk`/`powk` A, B, K | ABC | `R(A) = R(B) ∘ K[C]` |

Integer overflow raises `E503`.

### Comparison and logic

| op | fmt | effect |
|----|-----|--------|
| `eq`/`lt`/`le` A, B, C | ABC | `R(A) = R(B) ∘ R(C)` |
| `not` A, B | ABC | `R(A) = !R(B)` |
| `eqk`/`ltk`/`lek` A, B, K | ABC | `R(A) = R(B) ∘ K[C]` |
| `eqj`/`ltj`/`lej`/`nej` A, B, sC | ABC (C signed jump) | if `R(A) ∘ R(B)` (or `!=` for `nej`), `pc += 1 + sC` |

**NaN note.** `!(a < b)` is not `b <= a` when a float may be NaN. A
lowering that wants the opposite of a comparison does the compare
followed by `TESTN`, not a swapped operand pair.

### Control

| op | fmt | effect |
|----|-----|--------|
| `jmp` sBx | AsBx | `pc += 1 + sBx` |
| `test` A, sBx | AsBx | if truthy(`R(A)`), `pc += 1 + sBx` |
| `testn` A, sBx | AsBx | if falsy(`R(A)`), `pc += 1 + sBx` |
| `testset` A, B, sC | ABC (C signed jump) | if truthy(`R(B)`), `R(A) = R(B)`; else `pc += 1 + sC` |
| `forprep`/`forloop` A, sBx | AsBx | integer `for i in a..b` fast path; uses the four-slot window `R(A..A+4)` (start, limit, step, induction variable) |
| `loop` sBx | AsBx | back-edge; the step-budget check hangs off this |
| `iterprep` A | ABC | initialize iterator from `R(A..A+4)` (source, state, control, item) |
| `iternext` A, sBx | AsBx | step the iterator through `R(A..A+4)`; if done, `pc += 1 + sBx` |

### Data

| op | fmt | effect |
|----|-----|--------|
| `newlist` A, B | ABC | `R(A) = new list, cap = B` |
| `newmap` A, B | ABC | `R(A) = new map, cap = B` |
| `getfield` A, B, K | ABC | `R(A) = R(B).K[C]` (inline cache on `B`'s shape) |
| `setfield` A, K, B | ABC | `R(A).K[B] = R(C)` |
| `getindex` A, B, C | ABC | `R(A) = R(B)[R(C)]` |
| `setindex` A, B, C | ABC | `R(A)[R(B)] = R(C)` |
| `appendlist` A, B | ABC | `list R(A).push(R(B))` |
| `insertmap` A, B, C | ABC | `map R(A)[R(B)] = R(C)` |
| `interp` A, B, C | ABC | string interpolation from `R(B..B+C+1)`; result in `R(A)` |

### Calls

| op | fmt | effect |
|----|-----|--------|
| `call` A, B, C | ABC | `R(A..A+C+1) = call R(A)(R(A+1..A+1+B))` |
| `callb` A, B, C | ABC | call builtin `B[u8]` with `C` args from `R(A+1..A+1+C)`; result in `R(A)` |
| `callm` A, K, C | ABC | call method `K[B]` on `R(A)` with `C` args |
| `ret` A | ABC | return `R(A)` |
| `tailcall` A, B | ABC | tail-call, `B` args from `R(A+1..A+1+B)` |

### Closures

| op | fmt | effect |
|----|-----|--------|
| `closure` A, Fn | ABx | `R(A) = new closure of this function's children[Bx]` |
| `getup` A, U | ABC | `R(A) = upvalue(B)` |
| `setup` U, A | ABC | `upvalue(A) = R(B)` |
| `cellnew` A | ABC | `R(A) = fresh cell with R(A)` (per-iteration cell) |
| `cellget` A, U | ABC | `R(A) = cell(upvalue(B))` |
| `cellset` U, A | ABC | `cell(upvalue(A)) = R(B)` |

### Errors

| op | fmt | effect |
|----|-----|--------|
| `try.enter` H | ABx | open handler `H` (of this function's `handlers`) |
| `try.leave` A | ABC | leave the innermost open try: `A = 0` normally (into its finally, if it has one), `A != 0` dropping what its finally had pending |
| `throw` A | ABC | throw `R(A)` (`cough`) |

### Burns

| op | fmt | effect |
|----|-----|--------|
| `burn.enter` class, Rg | ABx | enter region `Bx` (of this function's `regions`) with class `A` |
| `burn.op` A, K, packed | ABC | `R(A) = burn_op(callee=K[B], kind=low 4 of C, args=R(A+1..A+1+(C>>4)))` (unwidened); with `WIDE`, `C` is 16 bits, low 8 are the kind and high 8 are the arg count |
| `burn.leave` | ABC | leave the current burn |
| `comp.enter` | ABC | enter the current burn's compensation block |
| `comp.leave` | ABC | leave the compensation block |

### The rest

| op | fmt | effect |
|----|-----|--------|
| `pack` A, B | ABC | `pack(R(A..A+B))` (once, before any burn) |
| `exhale` A, B | ABC | `exhale(R(A..A+B))` |
| `chain.make` A, B | ABC | `R(A) = chain(R(A+1..A+1+B))` |
| `steps` A, Bx | ABx | set the frame's loop nesting to `A`, then charge the step budget by `Bx` ticks ("How the VM reads it" says where a lowering puts one) |

## Jump semantics (one rule)

Every jump instruction in every format uses a **signed** offset.
The target is:

```
target = pc + 1 + offset
```

where `pc` is the first-word pc of the (possibly-prefixed)
instruction. The offset is:

- `sBx` (16 bits, 32 with `WIDE`) for AsBx opcodes (`jmp`, `test`,
  `testn`, `forprep`, `forloop`, `loop`, `iternext`);
- signed `C` (8 bits, 16 with `WIDE`) for ABC fused jumps (`eqj`,
  `ltj`, `lej`, `nej`, `testset`).

The verifier requires `0 <= target < n_instr` and that `target` is a
first-word pc.

## The verifier's rules

The verifier runs on every module before the VM executes it. It
returns the first failing rule as a `VerifyError`; the VM wraps the
result as `E1050` with the text
`the program's bytecode does not verify: <rule> at fn<i>+<pc>`.

The rules (one bullet per `VerifyError` variant, matched by its
`rule()` method):

- **`bad-opcode`** — an instruction's opcode byte is not one of the
  defined values.
- **`register-out-of-range`** — an operand of kind `Reg` or
  `BurnClass` is past the function's `reg_count`.
- **`register-range-out-of-range`** — the inclusive end of a register
  range (`CALL`, `CALLB`, `CALLM`, `TAILCALL`, `PACK`, `EXHALE`,
  `CHAIN.MAKE`, `INTERP`, `BURN.OP`) is past `reg_count`.
- **`const-out-of-range`** — a constant index is past this function's
  constant pool.
- **`const-not-string`** — an operand of kind `StrConst` indexes a
  non-string constant.
- **`jump-out-of-range`** — `target < 0` or `target >= n_instr`. All
  jump offsets are signed.
- **`func-out-of-range`** — a `CLOSURE` indexes past this function's
  `children`.
- **`upvalue-out-of-range`** — a `GETUP`/`SETUP`/`CELLGET`/`CELLSET`
  indexes past the function's upvalue list.
- **`handler-out-of-range`** — a `TRY.ENTER` indexes past the
  function's handler slice.
- **`handler-bad-range`** — a handler entry's `pc_start >= pc_end`,
  its `pc_end > n_instr`, its `handler_pc >= n_instr`, its
  `catch_reg >= reg_count` (when not sentinel), or its `finally_pc
  >= n_instr` (when not sentinel).
- **`handler-ranges-interleave`** — two handlers overlap without one
  containing the other. Handlers nest.
- **`burn-unbalanced`** — a forward walk through the function ends
  with a non-zero burn stack (`enter-without-leave`), or a
  `BURN.LEAVE` is reached without a matching `BURN.ENTER`
  (`leave-without-enter`).
- **`burn-op-outside-region`** — a `BURN.OP` is not inside a
  `BURN.ENTER`/`BURN.LEAVE` pair in this function, is not covered by
  a region row whose `region` is not the sentinel, and the function
  is not marked `requires_burn`.
- **`region-out-of-range`** / **`region-bad-range`** — the region
  table references an unknown region id in this function, or has
  `pc_start >= pc_end`, or `pc_end > n_instr`.
- **`call-arity-too-high`** — the argument count of a `CALL`/`CALLB`/
  `CALLM`/`TAILCALL` plus the base register runs past the frame.
- **`line-table-bad-row`** — a line table row names a pc past the end
  of the function, or two rows are not strictly ascending.
- **`not-terminated`** — the last decoded instruction of a function
  is not one of `RET`, `TAILCALL`, `THROW`, or a `JMP` (whose target
  the jump rule has already checked).
- **`arity-exceeds-reg-count`** — a function's `arity` is greater
  than its `reg_count`.
- **`wide-dangling`** — a `WIDE` prefix is the last word of a
  function, or is followed by another `WIDE`.
- **`mid-pair-reference`** — a jump target, a handler pc field, a
  region-row `pc_start`, or a line-row `pc` points at the second
  word of a `WIDE` pair.
- **`empty-function`** — the function has no instructions at all.
- **`frame-too-large`** — `reg_count` is greater than 65 536 (the
  widest register index the format can carry).
- **`upvalue-capture-out-of-range`** — a child function's upvalue
  descriptor names a parent register or a parent upvalue that is not
  there, or the module's top-level function declares an upvalue (it
  has no parent).
- **`nested-too-deep`** — the function nesting exceeds 64 levels.
- **`uncharged-cycle`** — within a function, the instructions that are
  not a `steps` with `Bx >= 1` form a cycle: some way back to an
  instruction already run passes no `steps` that charges the step
  budget. Every edge the VM can take counts: the fall-through (except
  after `ret`, `tailcall`, `throw`, `jmp` and `loop`), every jump operand
  (the plain and fused jumps, `forprep`, `forloop`, `iternext`), and an
  edge from every pc inside a handler's range to its `handler_pc` and
  `finally_pc` and on to every handler around it. A charging `steps`
  ends every edge out of it but one: inside a handler's range it keeps
  its edge to that handler, because once the budget is spent it raises
  each time it runs. The error names the lowest pc on the cycle. The
  check is linear in the function's size and does not recurse.

A module that satisfies every rule is safe to run with no further
bounds checks — with the one carve-out named in "What the verifier
cannot know" below: the burn stack is kept at run time, not here.

## What the verifier cannot know

Some rules are the VM's, not the format's. The verifier accepts the
operand; the VM checks it once at module load, or at run time, and
refuses the module (or the call) with the interpreter's own error.

- **`callb`'s builtin index.** `B` is 8 bits unwidened, 16 bits with
  `WIDE`. The VM checks every `callb` index against its builtin
  catalogue when it loads a module (cig has about 230 builtins today;
  a growing set means the format allows 16 bits). An unknown builtin
  refuses the module with `E1050`.
- **`loadg`'s name.** The verifier checks that `K[Bx]` is a string.
  The VM checks the name against the interpreter's prelude (its global
  builtins, its modules, and `args`) when it loads a module, and
  refuses an unknown one with `E1050`.
- **`burn.op`'s kind.** The low 4 bits (or low 8 with `WIDE`) of
  `burn.op`'s packed `C` operand name a burn-op kind. The VM checks
  it against the kernel's catalogue at load time.
- **`requires_burn`'s bargain.** A function marked `requires_burn`
  verifies a bare `BURN.OP` wherever it sits in that function's body.
  In exchange, the VM refuses a call to such a function from outside
  an open burn, with the interpreter's own error. The format does not
  attempt to prove the call site through static means.
- **Burn state at run time.** The verifier's burn-balance check is
  **lexical**: it walks the instruction stream in order. A jump can
  enter or leave a burn's pc range, so lexical balance does not
  imply a burn is open at the moment `burn.op` runs. **The VM carries
  the burn stack at run time** and refuses:
  - a `burn.leave` with no burn open;
  - a `burn.op` in a function that is not `requires_burn` while no
    burn is open.

  Both land on the interpreter's error, not on `E1050`. The
  verifier's check is there to catch a malformed module early, not
  to be the VM's guarantee.
- **Frame-size and nesting ceilings.** The verifier refuses a
  `reg_count` greater than **65 536** (the widest register index is
  16 bits with a `WIDE` prefix), and refuses a module whose function
  nesting exceeds **64** levels. The parser enforces the same
  nesting limit on the wire; a recursive reader would otherwise
  overflow the stack.
- **Upvalue captures.** Every `UpvalDesc` in a child function is
  checked against the direct parent: a parent-local capture must
  index inside the parent's `reg_count`, a parent-upvalue capture
  inside the parent's upvalue count. The module's top-level
  function has no parent, so it has no upvalues.

## How the VM reads it

The VM core (`src/vm/`) settles these points the tables above leave
open. A lowering writes to them.

- **Only a verified module runs.** `verify_module` returns a
  `Verified`, which nothing else can make, and `vm::load` takes only
  that. `vm::refusal` words a `VerifyError` as the `E1050` above.
- **Load-time checks.** Every function is decoded once, through
  `crate::instr::decode`, into a table by pc. A `callb` index past the
  builtin catalogue refuses the module with `E1050` (`unknown-builtin
  at fn<i>+<pc>`), and so do a `loadg` name the prelude does not have
  (`unknown-prelude-name`) and a `burn.op` kind past the burn-op
  catalogue (`unknown-burn-op`).
- **The builtin catalogue** is `stdlib::builtin_catalogue()`: the
  global functions, then the functions of `fs`, `path`, `json`,
  `text`, `proc`, `time`, `hash`, `cig`, `pass`, `math`, `env`, `csv`
  and `log`, each in its table's order. An index is valid for the cig
  the program was compiled against.
- **Positions.** An error at a pc takes its position from the line
  row at that pc; a pc with no row takes the nearest row before it,
  and a pc before every row takes `span_base`. A lowering gives a row
  to every pc whose error the interpreter positions differently from
  the instruction before it.
- **The step budget.** `steps A, Bx` sets the frame's loop nesting to
  `A`, then counts `Bx` steps one at a time on the interpreter's
  counter, each checked against the budget with the pc's position.
  The interpreter's loop depth (which picks the wording of `E515`) is
  the sum of the nestings of the frames on the stack. To stop on the
  same step and line as the tree-walker, a lowering emits `steps k, 1`
  where the tree-walker counts a step (each statement, at the
  statement's span; each turn of a `while` after its condition holds,
  and each item of a `for`, at the loop's span), `steps k, 0` where a
  loop starts (before its condition, with `k` the nesting inside it)
  and `steps k-1, 0` where it ends (the nesting outside it).
  A `steps` with `Bx > 1` stops at that one instruction, so folding is
  exact only where nothing between the folded steps can be observed.
- **`loop`** is a back-edge jump. It counts nothing; the step a loop
  turn costs is the `steps` on the loop's path, and the verifier
  refuses a cycle without one (`uncharged-cycle`).
- **Operators.** Every arithmetic and comparison instruction calls the
  interpreter's own operator (`interp::ops`). `pow`/`powk` are
  `math.pow`: CigScript has no power operator. There is no `gt` or
  `ge`: `a > b` is `le a, b` then `not`, and `a >= b` is `lt a, b`
  then `not` (or the fused jump with the branches swapped). Keeping
  the operand order keeps the interpreter's `E403` wording (`cannot
  compare int and string`); ordering a NaN is an error either way.
- **Calls.** `call A, B, C` with a compiled function in `R(A)` checks
  the arity (`E401`) and the depth (`E506`, at 4000) as the
  interpreter does, then runs the callee in a new frame whose `R(0..B)`
  are the arguments. Anything else in `R(A)` (a builtin, a module, a
  number) goes to the interpreter's own call. The result goes to
  `R(A)`; `R(A+1..A+C+1)`, the rest of the row's window, is set to
  null. `tailcall` reuses the frame, and still counts one call of
  depth and keeps the replaced frame's loop nesting until the chain
  returns, so `E506` and `E515` read as they would after a plain call.
- **`closure`** makes a function value (`<pull name>`, or `<pack>` for a
  lambda), a value like any other: the tree-walker and a builtin's
  callback (`xs.map(f)`, a sort key) call a compiled function, and the
  VM calls an interpreted one, each with the other's arity (`E401`) and
  depth (`E506`) rules. A callback into a compiled function runs in a
  VM of its own until it returns. A call from compiled code to compiled
  code of the same program takes no Rust stack (the VM's frames are a
  list); a callback level takes some, no more than the tree-walker
  takes for the same recursion, so `E506`'s 4000 levels fit in the
  stack `cig` gives its evaluator.
- **Cells.** A register can carry a cell. `cellnew A` gives `R(A)` a
  fresh cell holding `R(A)`'s value; from then on every read of `R(A)`
  reads the cell and every write writes it, until the frame ends.
  `closure` gives the child one cell per upvalue: a parent register
  that carries a cell is shared, a parent register that does not is
  copied into a cell of the child's own, and a parent upvalue is
  shared. `getup`/`cellget` read an upvalue's cell and `setup`/`cellset`
  write it. A lowering gives a captured variable's register `cellnew`
  before its first write in each instance of its scope: for a variable
  made fresh each turn of a loop, at the top of the turn, before the
  turn's item or declaration is written, so the closures of earlier
  turns keep theirs. A register that held a captured variable takes
  `cellnew` before it is used for another.
- **`forprep A, sBx` / `forloop A, sBx`** run `for i in a..b`. The window
  is `R(A)` start (then the running value), `R(A+1)` limit, `R(A+2)`
  step, `R(A+3)` the item. `forprep` checks the bounds as `a..b` does
  (`E403` when they are not ints, `E512` past 10,000,000 turns; a step
  other than 1 counts its turns the same way, and a step that is 0 or
  not an int is an internal error), jumps to `pc + 1 + sBx` when there
  is no turn, and otherwise sets `R(A+3)` to the start. `forloop` steps
  the running value and, while it is before the limit (above it for a
  negative step), sets `R(A+3)` and jumps to `pc + 1 + sBx`. A write to
  `R(A+3)` in the body does not move the loop.
- **`iterprep A` / `iternext A, sBx`** run `for x in v`. `iterprep`
  takes the interpreter's snapshot of `R(A)` (a list's items, a map's
  keys in order, a string's characters; `E405` for anything else) into
  `R(A+1)` and sets `R(A+2)` to 0. `iternext` puts the next item in
  `R(A+3)`, or jumps to `pc + 1 + sBx` when there is none. A change to
  the collection during the loop does not change what it visits.
  The loop instructions write `R(A+3)` as a plain register: a cell
  there stays with the closures that captured it.
- **Data.** `getfield`, `getindex`, `setfield`, `setindex`, `insertmap`
  and `callm` are the interpreter's own access, store and method call,
  with its errors (`E504`, `E505`, `E404`, `E405`, the type errors and
  their hints) at the instruction's position. `getfield` is `.name`,
  not `?.name`. `insertmap` takes a key as a map literal does (a string,
  or an int as its digits). There are no inline caches yet.
- **`exhale A, B`** writes `R(A..A+B)` through the interpreter's writer
  (a space between values, a newline after, `E513` when the reader has
  gone), and `interp A, B, C` joins `R(B..B+C+1)` as string
  interpolation shows each part. Each value is shown when the
  instruction runs, after every part was evaluated; the tree-walker
  shows each value as soon as it is evaluated, so a lowering whose
  later parts can change an earlier part's list or map turns each part
  to text first (a one-part `interp`).
- **`loadg A, Bx`** puts in `R(A)` the value the interpreter's prelude
  holds under the name `K[Bx]`: the same builtin, module or `args` list a
  tree-walked script gets when it names it (`roll f = len`, `stick p =
  path`, `args`). It reads the prelude itself, so a script's own global
  of the same name does not hide it: the lowering uses `loadg` only for
  a name that resolves to the prelude. `args` before the script's
  arguments are set is the interpreter's `E501`.
- **Errors and handlers.** Each frame keeps the tries it has opened and
  not left, innermost last. `try.enter H` opens handler `H` in its body
  and records the frame's loop nesting; opening `H` again (a loop around
  the try) first closes `H` and everything opened inside it. A handler
  has a catch when `handler_pc != finally_pc` (`try { } finally { }`
  sets both to the finally), a finally when `finally_pc` is not the
  sentinel, and a binding when `catch_reg` is not.

  An error raised at a pc (by an instruction, a builtin, a callee or a
  builtin's callback; in each caller, at the pc of its call) is offered
  to the frame's open tries, innermost first:

  - a try in its **body** takes it only when the pc is inside its
    `[pc_start, pc_end)`; one whose range does not hold the pc was left
    without `try.leave` and is closed. With a catch, and unless `exit()`
    was asked for, the error becomes the value an `ashtray` binding
    reads (the interpreter's: `message`, `kind`, `line`, a `cough`
    map's own fields, `value`), written to `catch_reg` as a plain
    register, and the run goes on at `handler_pc`. Otherwise, with a
    finally, the run goes on at `finally_pc` with the error pending.
    Otherwise the try is closed and the next one is asked;
  - a try in its **catch** sends the error to its finally, pending, or is
    closed when it has none. The catch's code may lie anywhere: it is
    the code the run reaches from `handler_pc`;
  - a try in its **finally** is closed: the finally's own error replaces
    the pending one.

  When no try in the frame takes it, the frame is left and its caller
  is asked; past the VM's entry the error is the run's. Every move to
  `handler_pc` or `finally_pc` sets the frame's loop nesting back to
  the try's, as the tree-walker leaves each loop on its way out, so the
  step budget's wording in a handler is the tree-walker's. The finally
  takes the pending `cough` payload aside and gives it back when it
  ends, as the tree-walker does.

  `try.leave 0` leaves the innermost open try. From its body or its
  catch, with a finally: the finally runs, and when it ends the run goes
  on after this `try.leave`; without one, the try is closed and the run
  goes on. At the end of a finally: the try is closed and what was
  pending goes on: the pc after the `try.leave` that entered it, or the
  error, raised again unchanged (code, message, position). `try.leave 1`
  closes the innermost try and drops what it had pending, for a
  finally's own way out.

  A lowering writes a `try` as `try.enter H`, the body (the handler's
  range, which takes in the body's `try.leave 0`), a jump past the rest,
  the catch (ending `try.leave 0` and a jump past the finally), and the
  finally (ending `try.leave 0`); the catch and the finally lie outside
  the range. `break`, `continue` and `snuff` out of a try are one
  `try.leave 0` per try they cross, innermost first, then the jump or
  the `ret` (with the snuffed value already in a register the finally
  does not write). Out of a finally they are `try.leave 1` for that
  try, then the same. A captured binding takes `cellnew catch_reg` at
  `handler_pc`. A `ret` or a tail call closes the frame's tries without
  running anything, so a lowering does not tail-call inside a try.
- **`throw A`** is `cough R(A)`: a map's `message` (or the map shown) is
  the message, any other value is shown, and the value is the payload
  the catch reads.
- **Positions through calls and handlers.** An error keeps the
  position it was raised at, through every frame it leaves, a catch it
  passes, a finally that raises it again, a builtin's callback and a
  call into or out of the tree-walker. The tree-walker prints no frame
  list, only that position, so the VM adds none.
- The burn, `pack` and chain instructions stop with an internal error
  that names the instruction until the package that builds them lands
  (`vm::NOT_BUILT` lists them).

## Versioning

The format version lives in the file's header. The rule is simple:

- The field increments only on a change that an earlier reader could
  not interpret correctly.
- A later reader opens an earlier module.
- An earlier reader refuses a later one with `BadVersion`, surfaced
  as `E1050`.

## What is fixed, what the VM decides

**Fixed** (next packages rely on these):

- the header layout and the integrity hash;
- the instruction word is 4 bytes, little-endian, bit layout
  `op/A/B/C` or `op/A/Bx`;
- the opcode table, including the discriminants, in `src/opcode.rs`;
- the module layout (`source_name` + `main`), each function
  self-contained with its own constants, children, regions,
  handlers, instrs, span_base, lines and region_rows;
- every verifier rule named above;
- one decoder (`crate::instr::decode`): no instruction is ever
  decoded any other way.

**Decided by the VM, outside this crate:**

- the step-budget counter's exact encoding (this crate declares the
  `STEPS` instruction and its position rules; the VM owns the
  counter);
- inline-cache slots on `GETFIELD`/`SETFIELD`;
- the builtin catalogue (bound of `callb`'s index) and the burn-op
  catalogue (bound of `burn.op`'s kind);
- the interpretation of `BURN.OP` kinds (`src/opcode.rs` enforces
  only the packed shape);
- the exact semantics of `TAILCALL` (when it may replace a `CALL`).
