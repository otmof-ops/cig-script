# The bytecode format

CigScript's stage-B VM runs a register bytecode. This file is the
format's **contract**: everything in `crates/cig-bytecode/` implements
it, the VM core runs it, CigCompile's lowering writes it, and future
packages can rely on the rules below without reading the crate's code.
See otmof-ops/cig-compile#31 for the stage-B issue.

The format's version is `2` (layout 2: a directory of functions read in
place). A reader opens exactly its own version and refuses any other with
`E1050`; see "Versioning". No release has carried the format yet.

## File layout

A module file is a header, a head, a directory with one fixed-width
entry per function, the function bodies, one string pool, and the line
region. Everything is little-endian. Every offset is from the module's
first byte, so a module that starts at an aligned address (a program's
`bytecode` section starts at a multiple of 16) keeps its bodies aligned.

```text
0      header        40 bytes
40     head          32 bytes
72     directory     80 bytes per function, in pre-order
       bodies        each a multiple of 4 bytes, back to back
       string pool   UTF-8, every name and string constant once
       line region   each function's line rows, delta-coded, back to back
```

Each part has exactly one place: the parser computes where each region,
body and line stream must start and how long it must be, and refuses any
offset or length that differs (`ParseError::BadOffset`, naming the field
and the function). Nothing lies between the parts and nothing follows the
line region (`ParseError::TrailingBytes`). A module is at most 4 GiB.

### Header (40 bytes)

| offset | size | field | notes |
|--------|------|-------|-------|
| 0 | 4 | magic | ASCII `CIGB` |
| 4 | 2 | format version | `2` in this revision |
| 6 | 2 | flags | none defined; `0` in this revision |
| 8 | 32 | integrity hash | BLAKE3-256 of every byte after the header |

A file whose magic is not `CIGB` is refused as not a bytecode module
(`ParseError::BadMagic`). A file whose version is not the reader's is
refused (`ParseError::BadVersion`). A file whose flags are
not zero is refused (`ParseError::BadFlags`): no flag is defined, so a
set one is never ignored. A file whose integrity hash does not match
the hash of its payload is refused (`ParseError::BadIntegrity`).
Together these leave no byte of a module unchecked: the magic, the
version and the flags are compared, and the hash covers itself and
every byte after the header (the head, the directory, the bodies and
their padding, the pool and the line region). A program's start relies
on this and checks its `bytecode` section by this hash alone
(`docs/PROGRAMS.md`). The hash is checked before any other byte is read.

### Payload

The payload is everything after the header. It starts with the head:

| offset | size | field | rule |
|--------|------|-------|------|
| 40 | 4 | function count `n` | at least 1; `72 + 80 × n` within the file (`BadCount`, `functions`) |
| 44 | 4 | source name: pool offset | the source file name, or empty (a string reference, below) |
| 48 | 4 | source name: length | |
| 52 | 4 | bodies offset | `72 + 80 × n` |
| 56 | 4 | pool offset | the end of the last body |
| 60 | 4 | pool length | |
| 64 | 4 | line region offset | pool offset + pool length |
| 68 | 4 | line region length | line region offset + length = the file's length |

The head holds what the regions' positions already imply, so a reader
finds each region with one read and no sum; the parser checks every one
of them against the layout.

**The directory** follows at offset 72: one 80-byte entry per function,
in **pre-order** (a function, then each of its children's subtrees in
order). A function's index in the directory is its `fn<i>`, the number
the verifier's and the loader's errors give. Function 0 is the top level.

**A string reference** is a pool offset and a length (`u32` each). It is
refused when it runs past the pool (`ParseError::BadIndex`, naming the
reference: `source name`, `function name` or `string constant`) or when
it starts or ends inside a UTF-8 character (`ParseError::BadString`). The
pool as a whole is UTF-8 (`ParseError::BadString`, `string pool`, with the
offset of the first byte that is not). The writer stores each distinct
string once, in the order first used: the source name, then for each
function in directory order its name and its string constants.

**The line region** holds each function's line rows, in directory order,
back to back; a function's directory entry gives its stream's offset (from
the line region's start), its length in bytes and its row count. A start
that does not need a position never reads it.

### Per-function tables (self-contained)

Each function still owns its tables, and **operand indices never cross a
function boundary**:

- `constants` — the function's constant pool (`LOADK`, `GETFIELD`,
  `SETFIELD`, `CALLM`, `BURN_OP` index it);
- `regions` — the function's burn-region table;
- `handlers` — try/catch/finally regions;
- `instrs` — the instruction stream;
- `span_base` — the function's anchor for line rows;
- `lines` — one row per instruction pc that can fault;
- `region_rows` — pc range to region id (into this function's
  `regions`);
- `children` — the functions this one may `CLOSURE`: `CLOSURE A, Bx`
  names the child at `children[Bx]`, and the table holds that child's
  directory index.

Every table but the line rows is fixed-width and lies in the function's
body, so its `k`th entry is read at `body + table offset + k × width`
without reading the entries before it. The text of a string constant is
in the module's pool; a function's bytes still change only when the
function does, apart from where its strings and its body sit.

### Function (the directory entry, 80 bytes)

| offset | size | field | rule |
|--------|------|-------|------|
| 0 | 4 | name: pool offset | a string reference; empty for a lambda |
| 4 | 4 | name: length | |
| 8 | 4 | parent | its parent's directory index; `0xFFFF_FFFF` for function 0 |
| 12 | 1 | kind | `0` main, `1` pull, `2` lambda (`BadTag`, `function kind`) |
| 13 | 1 | requires_burn | `0` or `1` (`BadTag`, `requires_burn`) |
| 14 | 2 | arity | parameter count |
| 16 | 4 | reg_count | registers in the frame |
| 20 | 4 | body offset | from the module's start: the bodies offset for function 0, the end of the body before for each later one |
| 24 | 4 | body length | exactly what the counts below give, rounded up to a multiple of 4 |
| 28 | 4 | instruction count | |
| 32 | 4 | constant count | |
| 36 | 4 | handler count | |
| 40 | 4 | region-row count | |
| 44 | 4 | region count | |
| 48 | 4 | upvalue count | |
| 52 | 4 | child count | |
| 56 | 4 | line rows offset | from the line region's start: 0 for function 0, the end of the stream before for each later one |
| 60 | 4 | line rows length | bytes |
| 64 | 4 | line row count | at most a fifth of the length (`BadCount`, `lines`): a row is at least five bytes |
| 68 | 12 | span_base | `(byte_offset, line, col)` of the first token, three `u32` |

**The tree.** The parser checks, before it builds any function, that the
directory is one tree in pre-order (`ParseError::BadTree`, naming the rule
and the function): function 0's parent is `0xFFFF_FFFF`; every later
function's parent is an earlier function on the path from function 0 to
the function just before it (`parent`, `pre-order`); each function's
children table lists exactly the functions that name it as their parent,
in increasing order (`child`, `child order`, and `child count` when a
function is listed by none); a child index past the directory is
`BadIndex`, `child`. No function is deeper than 64 below function 0
(`ParseError::TooDeep`). The parser builds the tree without recursion.

**The body**, at the entry's body offset (always a multiple of 4), holds
the tables in this order, each entry at its fixed width:

| part | width | each entry |
|------|-------|------------|
| instructions | 4 | the instruction word (see `Instr`); first, so the code words of a body are 4-aligned and read where they lie |
| constant values | 8 | by the constant's tag: `Bool` 0 or 1, `Int` an `i64`, `Float` the `f64`'s bits, `Str` a string reference (pool offset in the low 32 bits, length in the high 32) |
| handlers | 20 | `(u32 pc_start, u32 pc_end, u32 handler_pc, u32 catch_reg, u32 finally_pc)` |
| region rows | 12 | `(u32 pc_start, u32 pc_end, u32 region)` |
| children | 4 | the child's directory index |
| regions | 8 | `(u8 class, u8 label kind, two zero bytes, u32 label)` |
| upvalues | 4 | `(u8 kind, a zero byte, u16 index)` |
| constant tags | 1 | `1` Bool, `2` Int, `3` Float, `4` Str |
| padding | 0 to 3 | zero bytes, to a multiple of 4 |

Every reserved byte and every padding byte is zero (`ParseError::BadPadding`,
naming it). A constant tag outside 1 to 4 is `BadTag`, `constant`; a
`Bool` value other than 0 or 1 is `BadTag`, `bool`.

**Line rows.** Each row is five LEB128 numbers (seven bits a byte, low
bits first, the top bit set on every byte but the last; at most five
bytes, in the shortest form):

1. `pc`, as a zigzag delta from the row before (0, -1, 1, -2 ... written
   as 0, 1, 2, 3 ...);
2. `span_off`, as a zigzag delta from the row before;
3. `span_len`, as it is;
4. `line_off`, as a zigzag delta from the row before;
5. `col`, as it is.

The first row's deltas are from zero. A number longer than five bytes,
not in its shortest form, or whose field leaves `0..=u32::MAX`, is
`ParseError::BadVarint`, naming the function and the offset. A stream must
hold exactly its row count in exactly its length (`Truncated`, `line
rows`, or `BadOffset`, `line rows length`).

### What the fields mean

- **Constants** are of four kinds; a float's bits are stored, so a NaN
  payload round-trips. A `null` constant is not needed: `LOADNULL` loads
  null directly. Rich values (lists, maps, closures) are built by
  instructions.
- **A burn region**: class `0` normal, `1` unlit; label kind `0` none
  (its label must be `0xFFFF_FFFF`), `1` a string constant of this
  function's pool (the label is its index), `2` a register of the frame,
  read when `burn.enter` runs. A label of kind `0` with any other label,
  or an unknown kind, is refused (`ParseError::BadTag`, `region label
  kind`): each label has one encoding. A class above 1 is `BadTag`,
  `region class`.
- **An upvalue**: kind `0` parent-local (the index is a register of the
  parent) or `1` parent-upvalue (an upvalue of the parent); any other kind
  is `BadTag`, `upvalue kind`.
- **A handler**: `catch_reg` and `finally_pc` are `0xFFFF_FFFF` when
  absent. Every `pc` field must be a first-word pc (see Instructions
  below).
- **A line row**: at `pc` the running instruction came from the span
  starting `span_off` bytes after `span_base.byte_offset`, `span_len`
  bytes long, `line_off` lines below `span_base.line` (0 = same line), at
  1-based column `col` (absolute, since columns reset).
- **A region row**: every pc in `pc_start..pc_end` (half-open) belongs to
  region `region` of this function, or to none when `region` is
  `0xFFFF_FFFF`.

### Why these choices

- **A directory of fixed-width entries.** A loader finds any function's
  body, its counts and its parent with one read at `72 + 80 × i`, without
  parsing the functions before it. 80 bytes keep every field a plain
  `u32` (or smaller, at its natural alignment); on a module of a thousand
  functions the directory is 80 KB, under 6% of the bytes.
- **Pre-order, with both the parent and the children table.** Pre-order
  is the numbering the verifier and the loader already use (`fn<i>`). The
  parent lets the verifier check a function's upvalues against its parent
  without the parent's body; the children table turns `CLOSURE A, Bx` into
  a directory index in one read. Both are stored and checked against each
  other, so neither can disagree with the tree.
- **4-byte alignment, instructions first.** A body starts at a multiple of
  4 and its first table is the code words, so they are read where they lie
  (`u32::from_le_bytes`, no `unsafe`, no copy into a staging buffer). The
  8-byte constant values are read the same way at a 4-byte alignment.
- **Constant values in 8 bytes, tags apart.** Every constant is read at
  `k × 8` without decoding the ones before it; the one-byte tags sit after
  the other tables so they cost no alignment padding.
- **One string pool.** A string used in many functions (a key, a builtin
  name) is stored once: on cigo, 16,619 string constants and 1,016 names
  hold 7,946 distinct strings.
- **Line rows in a region of their own, delta-coded.** The line table was
  more than half of a module's bytes, and a run only reads it to place an
  error. Most rows sit on the same line as the row before and a few bytes
  after it, so deltas take one byte where `u32`s took four. A start never
  has to touch the region.

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

**What a call leaves undefined.** `call A, B, C` runs the callee in a
frame that starts at `R(A+1)`: its parameters are the arguments where
they already are, and its other registers lie over the caller's
registers from `R(A+1+B)` up. So after `call A, B, C`, every register
past the call's result row (`R(A+C+1)` and up; `C` is the count of
result registers the row covers after `R(A)`, `0` for one result)
holds no value a run may read before an instruction writes it, and a
cell that register carried is gone: a write after the call is a plain
write, and the closures that shared the cell keep it. `R(A)` is the
result and `R(A+1..A+C+1)` are null, as before. This holds on every way
on from the call: the next pc, and a catch or a finally an error from
the callee reaches. A lowering places a call's window above every
register that holds a value across the call; the verifier holds a
module to it (`read-before-write`). `tailcall` ends its frame and
leaves nothing behind to read. `callb`, `callm` and `burn.op` make no
frame in this machine (a builtin, a method or a callback that runs
compiled code runs it in a VM of its own) and leave every register but
their result as it was.

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
| `burn.enter` class, Rg | ABx | enter region `Bx` (of this function's `regions`) with class `A` (the region's own class); a register label is read here |
| `burn.op` A, K, packed | ABC | `R(A) = burn_op(callee=K[B], kind=low 4 of C, args=R(A+1..A+1+(C>>4)))` (unwidened); with `WIDE`, `C` is 16 bits, low 8 are the kind and high 8 are the arg count |
| `burn.leave` | ABC | leave the current burn |
| `comp.enter` A, K, K | ABC | the current burn has an `unburn { }`: `R(A)` is its state map, `K[B]` the compensation's source, `K[C]` the state's name (empty for none) |
| `comp.leave` | ABC | the burn's body ends: the compensation stops covering its ops |

### The rest

| op | fmt | effect |
|----|-----|--------|
| `pack` A, B | ABC | `pack(R(A..A+B))` (once, before any burn); trailing `nop`s may give each root its position |
| `exhale` A, B | ABC | `exhale(R(A..A+B))` |
| `chain.make` A, B, C | ABC | `R(A) = chain(R(A+1..A+1+B))`: the name, then a label and a value per step; trailing `nop`s may give each step its position; `C = 1` also declares it as a top-level name |
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

The verifier runs on every function before the VM executes any of its
instructions: on the whole module at once (`verify_module`), or on one
function at its place in the module (`verify_function`, with the counts
it needs from its parent and its children taken from the directory; "How
the VM reads it" says when a run does which). It returns the first
failing rule as a `VerifyError`; the VM wraps the result as `E1050` with
the text `the program's bytecode does not verify: <rule> at fn<i>+<pc>`.
Each function's rules read one decode of its instructions, which the VM
loads from.

The rules (one bullet per `VerifyError` variant, matched by its
`rule()` method):

- **`bad-opcode`** — an instruction's opcode byte is not one of the
  defined values.
- **`register-out-of-range`** — an operand of kind `Reg` is past the
  function's `reg_count`, or a `burn.enter` enters a region whose label
  is a register past it. `burn.enter`'s class is not a register: a
  one-register function may hold an unlit burn.
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
- **`burn-unbalanced`** — the burn instructions do not fit the
  function's **burn rows**: its region rows that name a region (a row
  whose `region` is the sentinel is outside every burn and takes no part).
  A burn row is one burn's pcs, from its `burn.enter` to the pc after its
  last instruction. The rule names what failed (`VerifyError`'s `why`):
  - `rows-interleave`: two burn rows overlap without one holding the
    other, or start at the same pc. A burn's code is one range, and burns
    nest as their blocks do;
  - `row-without-enter`: a row's first instruction is not a `burn.enter`
    of the row's region. The only way into a burn is its `burn.enter`;
  - `row-falls-out`: a row's last instruction is not a `burn.leave` and
    falls through (anything but `ret`, `tailcall`, `throw`, `jmp`,
    `loop`). The run cannot walk off a burn's end with it still open;
  - `enter-outside-row`: a `burn.enter` lies in no row of its region. Every
    burn a run can enter is in the table. A `burn.enter` inside its row
    but not at its start (a re-entry after a jump, which no run reaches)
    is accepted;
  - `leave-outside-row`: a run of `burn.leave`s, with only `comp.leave`s
    between them, has more `burn.leave`s than there are burn rows around
    its first. Each closes one burn around it: leaving two burns at once
    on a `break` is two, and a leave outside every row is too many;
  - `jump-into-row`: a jump operand, or a handler's `handler_pc` or
    `finally_pc`, lands inside a burn row other than at its start, from
    outside that row (for a handler: its range is not inside the row).
    Only the `burn.enter` is entered from outside.

  These are rules on the table, not on the order of instructions: a
  `burn.leave` on a way out (`comp.leave`, `burn.leave`, then the jump a
  `break` or `continue` makes) needs nothing after the jump, and a body
  that ends in a terminator needs no `burn.leave` after it. They refuse
  a malformed module early; they do not prove a burn is open when a
  `burn.op` runs. The VM's burn stack does that (below).
- **`burn-class-mismatch`** — a `burn.enter`'s class `A` is not its
  region's class.
- **`burn-op-outside-region`** — a `BURN.OP` lies in no burn row and the
  function is not marked `requires_burn`.
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
- **`read-before-write`** — an instruction reads a register whose value
  is undefined on some way a run can reach it, and nothing writes the
  register first. The one case is a call's (`after-call`): after `call
  A, B, C`, `R(A+C+1)` and up are undefined (see "What a call leaves
  undefined"). The rule follows every way the VM can go: jumps and
  fall-throughs (a fused jump, `testset`, `forprep`, `forloop` and
  `iternext` write their registers only on the way they write them), an
  error offered to the tries open at its pc as the VM offers it (to a
  catch, whose `catch_reg` is then written, or a finally; an error from
  a call's callee comes with the callee's frame having run), a `try.leave`
  into its finally, and a finally's end back to the pc after the
  `try.leave` that entered it, or out with its error. What counts as a
  read is what the VM reads: every register operand an instruction takes
  a value from (a call's callee and arguments, the windows of `interp`,
  `exhale`, `pack`, `chain.make` and `burn.op`, the `for` and iterator
  windows, `cellnew`'s own register), a `closure`'s captured parent
  registers, a `burn.enter`'s register label, and `comp.enter`'s state
  register when it names one. The error names the read's pc, the
  register, and a call that leaves it undefined. A read the run never
  reaches is not refused.
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
- **`burn.op`'s kind and callee.** The low 4 bits (or low 8 with
  `WIDE`) of `burn.op`'s packed `C` operand name a burn-op kind, and
  `K[B]` names the builtin. The VM checks at load time that `K[B]` is a
  builtin of its catalogue whose kind is that kind (`vm::burn_op_kind`),
  and refuses anything else with `E1050` (`unknown-burn-op`).
- **A region's constant label** must be a string constant
  (`region-label`), checked at load. A register label is read when
  `burn.enter` runs: a string is the label, anything else the
  tree-walker's own error (below).
- **`requires_burn`'s bargain.** A function marked `requires_burn`
  verifies a bare `BURN.OP` wherever it sits in that function's body.
  In exchange, its `burn.op` is refused at run time when no burn is open
  anywhere on the stack, with the interpreter's own `E701` at the op,
  exactly where and when the tree-walker raises it. The VM does not
  refuse the call itself: a pull whose effect is conditional runs
  outside a burn in the tree-walker as long as the effect is not taken,
  and so it does on the VM. The format does not attempt to prove the
  call site through static means.
- **Burn state at run time.** The verifier's burn rules are on the
  region rows; a jump can still leave a burn without `burn.leave`, or
  reach a `burn.op` after its burn's `burn.leave`, so they do not imply a
  burn is open at the moment `burn.op` runs. **The VM carries the burn
  stack at run time** and refuses:
  - a `burn.leave`, `comp.enter` or `comp.leave` with no burn (or no
    compensation) open in its frame (an internal error that names the
    instruction: the tree-walker has no such state);
  - a `burn.op` in a function that is not `requires_burn` while its
    frame has no burn open: the interpreter's `E701`.

  Neither lands on `E1050`. The verifier's rules are there to catch a
  malformed module early, not to be the VM's guarantee.
- **Frame-size and nesting ceilings.** The verifier refuses a
  `reg_count` greater than **65 536** (the widest register index is
  16 bits with a `WIDE` prefix), and refuses a module whose function
  nesting exceeds **64** levels. The parser enforces the same
  nesting limit on the directory's parents (the parser does not
  recurse, but the verifier, the VM and a module's drop walk the tree).
- **Upvalue captures.** Every `UpvalDesc` in a child function is
  checked against the direct parent: a parent-local capture must
  index inside the parent's `reg_count`, a parent-upvalue capture
  inside the parent's upvalue count. The module's top-level
  function has no parent, so it has no upvalues.

## How the VM reads it

The VM core (`src/vm/`) settles these points the tables above leave
open. A lowering writes to them.

- **Only verified code runs: each function is verified before its
  first instruction runs.** `verify_module` returns a `Verified` and
  `verify_function` a `VerifiedFunction`, which nothing else can make;
  `vm::load` takes only the first, and the VM loads a function only from
  the second. `vm::refusal` words a `VerifyError` as the `E1050` above.
- **When.** `vm::load` verifies and loads every function of a parsed
  module before anything runs. A compiled program's start opens its
  module with `vm::open` instead, which reads only what the start needs:
  - at the start, before the first instruction: the header, the BLAKE3
    over every byte after it, the head, the string pool (UTF-8 as a
    whole), every directory entry's offsets and lengths, the tree and its
    depth (`cig_bytecode::Reader::open`); every function's name, kind,
    flags and upvalue descriptors, and its upvalues against its parent's
    registers and upvalues (`verify_upvalues`: the one rule that reads
    another function, checked for every function); and the top level,
    read, verified and loaded;
  - every other function the first time a run enters it (a call, a tail
    call, a callback from a builtin), before its first instruction: its
    body and line rows read with every check the parser makes of them,
    every verifier rule at its place, then the load-time checks below.
    A function that fails is `E1050` (`does not verify: <rule> at
    fn<i>+<pc>`, or `does not read: <what>`), raised at the call as an
    error of the run, so the run's burns roll back; it stays unloaded,
    and none of its instructions ever run. `closure` makes a closure
    without entering its function: it reads only the head checked at the
    start;
  - `CIG_VALIDATE=1` parses the whole module and verifies and loads every
    function at the start, as `vm::load` does: a module any function of
    which fails is refused before anything runs.

  Damage is never deferred: the start checks the hash over every byte of
  the module, so a changed byte anywhere is refused before the first
  instruction. Only a module whose hash was recomputed after a change can
  reach a refusal at a first call.
- **Load-time checks.** Each function is decoded once, through
  `crate::instr::decode`, into a table by pc (the verifier's table). A `callb` index past the
  builtin catalogue refuses the module with `E1050` (`unknown-builtin
  at fn<i>+<pc>`), and so do a `loadg` name the prelude does not have
  (`unknown-prelude-name`), a `burn.op` whose callee is not a builtin of
  its kind (`unknown-burn-op`), and the burn rules below.
- **The builtin catalogue** is `stdlib::builtin_catalogue()`: the
  global functions, then the functions of `fs`, `path`, `json`,
  `text`, `proc`, `time`, `hash`, `cig`, `pass`, `math`, `env`, `csv`
  and `log`, each in its table's order. An index is valid for the cig
  the program was compiled against.
- **Positions.** An error at a pc takes its position from the line
  row at that pc; a pc with no row takes the nearest row before it,
  and a pc before every row takes `span_base`. The VM builds one
  position per row, not one per instruction. A lowering gives a row
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
  number) goes to the interpreter's own call. The callee's frame starts
  at the caller's `R(A+1)`: its parameters are the arguments in place,
  nothing is copied, and its registers past them start null. The result
  goes to `R(A)`; `R(A+1..A+C+1)`, the rest of the row's window, is set
  to null. Registers above the row may hold anything after the call
  (what the callee left there); the verifier has refused every read of
  one before a write (`read-before-write`). A frame that makes cells
  (its function has a `cellnew`) gives them up when it ends, and its
  call takes them off its registers from `R(A+1)` up (an argument is
  read out of its cell first), so a callee's frame never starts on a
  cell. The register stack grows when a frame needs more than there is
  and is not shrunk when one returns: it is at most the deepest chain of
  frames' registers (each frame starts inside its caller's), so `E506`'s
  depth times the largest frame, and the allocation ceiling (`E514`)
  refuses it before that. What a returned frame left in its registers
  stays there until a later frame starts over them (setting them null)
  or the run ends, where the tree-walker drops a call's values when it
  returns. No program can see the difference: no value has a drop that
  does anything a program can observe (values are strings, lists, maps,
  functions, builtins, modules and chains; a closure's cell is kept by
  the closures that share it), and no register left that way is read
  again before it is written. It is memory held a little longer, bounded
  by the deepest the stack has been. `tailcall` moves its arguments down
  to the frame's `R(0)` and sets the rest of the new function's registers
  null. `tailcall` reuses the frame, and still counts one call of
  depth and keeps the replaced frame's loop nesting until the chain
  returns, so `E506` and `E515` read as they would after a plain call.
  A frame's record holds no value and owns nothing: the callee is read
  where it lies in `R(A)` (the register below the frame, where its result
  goes), the tries and burns a frame opens are kept on two lists of the
  machine from where that frame's begin, and the upvalues of a closure
  that captured any on a third, so a call and a return move a small
  record and drop nothing of it. `call` of a compiled function, `ret`
  and `tailcall` change frames in the frame loop itself; an instruction's
  position is looked up only when it raises an error.
- **`closure`** makes a function value (`<pull name>`, or `<pack>` for a
  lambda) with its upvalue list in one allocation and the name every
  closure of that function shares (its arity is the function's; it carries
  no parameter names), a value like any other: the tree-walker and a builtin's
  callback (`xs.map(f)`, a sort key) call a compiled function, and the
  VM calls an interpreted one, each with the other's arity (`E401`) and
  depth (`E506`) rules. A callback into a compiled function runs in a
  VM of its own until it returns. The interpreter keeps one set of a
  VM's vectors (registers, cells, frames, tries, burns, upvalues) between
  runs: a callback takes it when it starts and gives it back emptied when
  it ends, so the callbacks of `xs.map(f)` allocate none of them. Its
  registers start null, as a new machine's do, and a callback inside a
  callback finds the set taken and makes its own: two live machines never
  share one. A set whose registers grew past 16,384 is given back to the
  allocator instead. A call from compiled code to compiled
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
  or an int as its digits). `getfield` and `setfield` keep, per site,
  the position their key last had in the map, as the tree-walker keeps
  one per field expression; the position is only a guess, checked
  against the key at it.
- **`callm A, K, C`** calls method `K[B]` on `R(A)` with the arguments
  `R(A+1..A+C+1)`: the registers `R(A..A+C+1)` are the receiver and then
  the arguments, the layout a method takes, and a builtin method gets
  them in place: no vector is built for them on the VM, nor in the
  tree-walker for up to three arguments. A vector is still built in the
  tree-walker for more than three arguments and for the pipe form, and on
  both engines when a map holds a function under the method's name or the
  receiver is a module (the arguments are copied into the call of that
  function). A method is found by its name with a `match` per
  receiver type, never by scanning a table, and each `callm` site keeps
  the receiver type and the method it found last. When the next call's
  receiver is of that type, the method is called without its name being
  looked at; when it is not, the method is found again and the site
  keeps the new one. A map receiver that holds a function under the
  method's name calls that function, checked on every call, cache or
  not; a module receiver calls its member. Errors (`E404` and its hint,
  the arity message without the receiver, the method's own) are the
  interpreter's, at the instruction.
- **`exhale A, B`** writes `R(A..A+B)` through the interpreter's writer
  (a space between values, a newline after, `E513` when the reader has
  gone), and `interp A, B, C` joins `R(B..B+C+1)` as string
  interpolation shows each part. `interp` reads its parts in place and
  writes each one straight into one buffer (a string's text copied once,
  an int written digit by digit), and the string value is made from that
  buffer with one copy: one allocation for the whole string. Each value is shown when the
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
- **The fast loop.** A frame whose function has no `cellnew` (so no
  register of it carries a cell) runs in a loop that reads and writes
  registers directly. It runs the moves, constants, arithmetic and
  compares on numbers, jumps, `steps`, `forloop`, `getup`/`cellget`,
  reads of a list item, a map key or a field, stores into a list item
  or a field, and, in a second tier, `newlist`, `appendlist`, `newmap`,
  `insertmap` with a string key, `setindex` on a map with a string key,
  `interp`, `add` of two strings, and `callb` and `callm` of a builtin that cannot call a
  script function (every builtin except `map`, `filter`, `reduce`,
  `each`, `any`, `all`, `find`, `sort_by`, `light`, `pass.issue` and
  `pass.memo`; for `callm`, only when the site's method fits the
  receiver's type). Anything else, or an uncommon case of these (a
  wrong type, a register held by another borrow, an arity error), the
  loop does not run: it changes nothing and leaves the instruction to
  the general path, which runs it from the start with its error. A
  builtin's own error raised in the loop is raised as the general path
  raises it, at the call, with the frame's pc on the call for the
  tries. A builtin that can call a script function always takes the
  general path: the fast loop's Rust frame is never on the stack while
  the VM runs again inside a call.
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
- **Burns.** A burn opens and closes through the interpreter's one burn
  entry (`Interp::burn_open`, `compensate_open`, `compensate_close`,
  `burn_close`, `record_compensation`), the functions the tree-walker's
  `burn { }` calls, in its order. A lowering writes
  `burn (s, "deploy") { body } unburn { comp }` as: `newmap R(s)`,
  `burn.enter 0, Rg` (the region labeled `"deploy"`; its row is the burn
  statement's position, where recording the compensation fails),
  `comp.enter s, K[source], K["s"]` (its row is the `unburn` block's
  position: its line is the one the journal records), the body,
  `comp.leave`, `burn.leave`. `K[source]` is the text between the
  `unburn` block's braces, as the parser slices it. A burn without
  `unburn { }`, or an unlit one (`burn.enter 1, Rg`), has no `comp.*`;
  in an unlit burn they do nothing, as the tree-walker records no
  compensation for one. The burn's region row runs from `burn.enter` to
  the pc after the body's last instruction.

  A **computed label**, `burn (s, name) { }`, is a region of label kind
  `2`: the lowering evaluates the label into a register before
  `newmap R(s)` (the tree-walker reads it before the state map is made,
  in the enclosing scope), then writes `burn.enter 0, Rg` and, right
  after it, a `nop` whose row is the label expression's position.
  `burn.enter` reads the register when it runs, each time (a burn in a
  loop takes each turn's label): a string is the label in the journal's
  compensation and the plan; anything else is the tree-walker's own
  error, `a burn's label is a string, got <type>` with its hint
  (`crate::interp::eval::burn_label`, the one function both engines
  call), at the `nop`'s row, else at `burn.enter`'s own, and the burn is
  not entered.

  Each frame keeps the burns it opened, innermost last; an open try
  keeps how many were open at its `try.enter`. Leaving a burn closes it:
  the compensation stops covering, the depths go down, and, when the
  block completed, its compensation is recorded with its state map as it
  is then. `burn.leave` closes the innermost burn as completed, and so do
  a `try.leave` (every burn opened inside the try, before its finally),
  a `ret` and a tail call (every burn of the frame), as `snuff`, `break`
  and `continue` leave them in the tree-walker. An error closes every
  burn opened inside the try that takes it, before its catch or finally
  runs, and every burn of a frame it leaves, innermost first: completed
  only when `exit()` was asked for, as in the tree-walker. An error
  recording a compensation replaces the one going on, at the burn's
  position. A lowering writes `comp.leave` and `burn.leave` on every
  other way out of a burn (a `jmp` out of it, after them), and does not
  tail-call inside one. It writes no instruction that no run reaches: a
  `break` out of a burn is `comp.leave`, `burn.leave`, `jmp` (two burns:
  both pairs, innermost first, then one `jmp`) with nothing after the
  jump, and a body that ends in a terminator (`break`, `continue`,
  `snuff`, `cough`) ends its row there, with no `comp.leave` or
  `burn.leave` after it.
- **`burn.op A, K, packed`** calls the builtin `K[B]` with
  `R(A+1..A+1+n)` through the interpreter's own builtin call, so the
  kernel gets the op the builtin builds (`Interp::effect`), with its plan,
  dry run, journal line and errors; the result is `R(A)`. The kinds, by
  index: `write` (`fs.write_text`, `json.save`, `csv.write`), `append`
  (`fs.append_text`), `delete` (`fs.rm`), `move` (`fs.mv`), `copy`
  (`fs.cp`), `mkdir` (`fs.mkdir`), `proc` (`proc.run`, `.text`,
  `.lines`, `.json`, `.csv`, `.kv`, `.pipe`, `.shell`), `env_set`,
  `env_unset`, `layer_apply` (`proc.apply_layer`) and `layer_discard`
  (`proc.discard_layer`). Kinds are only appended.
- **`pack A, B`** declares the pack from the roots `R(A..A+B)`, each
  checked as `pack { }` checks it (a string, not empty: `E755`), then
  the kernel's `set_pack` (its errors at the instruction). Root `i`'s
  error takes the line row of the `i`-th `nop` right after `pack`, when
  the run of `nop`s reaches it; else the instruction's own. The
  checker's `E753`s hold at run time too, in its words: a `pack` outside
  the top level or inside a burn or a try, a second `pack`, and a `pack`
  after anything was journaled or planned. The roots are evaluated
  before `pack` runs; the tree-walker checks each as soon as it is
  evaluated, so a lowering whose later roots have effects keeps that in
  mind.
- **`getindex A, B, C`** may be followed by one `nop`, whose row is the
  index's position: a wrong index type for the object (a list or a string
  and not an int, a map and not a string) is raised there, and every other
  error at the instruction's own. **`setindex A, B, C`** may be followed by
  two: a wrong index type for a list or a map is raised at the first, an
  object that is neither a list nor a map at the second, and every other
  error at the instruction's own; without them everything is at the
  instruction's. The run goes on past them.
- **`chain.make A, B`** builds the chain `chain name { steps }` declares:
  `R(A+1)` is its name (a string), then a pair per step, the step's label
  (its name in the source for an identifier or a member, else null) and
  the step's value, so `B = 1 + 2n`. Each step is checked and labeled as
  the tree-walker does it; step `i`'s error takes the `i`-th trailing
  `nop`'s row, as `pack`'s roots do. `light` and a stick used as a step
  run on the interpreter's chain runner, which calls a compiled step
  through `call_compiled`.
  `C` is `1` for a `chain` statement of the script's top level (directly
  in the top-level block, not in an `if`, a loop, a `try`, a `burn` or a
  pull) and `0` for every other: with `1` the VM also declares the chain
  in the interpreter's globals, as the tree-walker's `chain` does, and a
  name already there is the tree-walker's `E306`. `C = 1` outside the
  top level's own frame, or `C` above `1`, stops with an internal error.
- **The top level's names.** The top level's only normal end returns a
  map of every name a top-level statement declares (`stick`, `roll`,
  `pull`, `chain`, ... in the order they are declared), each with its
  value at the end: `newmap R, n`, then `loadk K, "<name>"` and
  `insertmap R, K, <its register>` per name, then `ret R`. A program's
  run declares each in the interpreter's globals (a name already there,
  a top-level chain, keeps its value), so after the run they hold what
  the tree-walker's hold and `--light <name>` answers the same: a chain
  is lit, another value is `` `<name>` is a <type>, not a chain ``, and a
  name the top level does not declare is `E804`. A top level that
  returns anything else declares nothing. Since a top-level `snuff` is
  a check error (`E304`), the top level's return value has no other use.
  After `exit()` the map is not returned: top-level chains declared
  before it are still in the globals, other names are not.
- `vm::NOT_BUILT` is empty: every instruction of this format runs. An
  opcode a later format adds stops with an internal error that names it
  until the VM runs it.

## Versioning

The format version lives in the file's header, and a program's section
table carries it as the `bytecode` section's version.

- The field increments on any change to the layout or to what a field
  means.
- A reader opens exactly its own version and refuses every other with
  `BadVersion`, surfaced as `E1050`. A program only runs on the cig that
  wrote it, so a reader keeps no code for an earlier version.
- Version 1 (each function nested inside its parent, its tables
  count-prefixed, its strings inline and its line rows as five `u32` each)
  was never carried by a release; this reader refuses it.
- **The format is not final until a release carries it.** Until then a
  change to it does not bump the version again; after the first release
  that carries version 2, every change does.

## What is fixed, what the VM decides

**Fixed** (next packages rely on these):

- the header layout and the integrity hash;
- the instruction word is 4 bytes, little-endian, bit layout
  `op/A/B/C` or `op/A/Bx`;
- the opcode table, including the discriminants, in `src/opcode.rs`;
- the module layout (header, head, directory in pre-order, bodies,
  string pool, line region), each function with its own constants,
  children, regions, handlers, instrs, span_base, lines and
  region_rows, so an operand index never crosses a function;
- every verifier rule named above;
- one decoder (`crate::instr::decode`): no instruction is ever
  decoded any other way.

**Decided by the VM, outside this crate:**

- the step-budget counter's exact encoding (this crate declares the
  `STEPS` instruction and its position rules; the VM owns the
  counter);
- inline-cache slots on `GETFIELD`/`SETFIELD` and `CALLM`;
- the builtin catalogue (bound of `callb`'s index) and the burn-op
  catalogue (`vm::BURN_OP_KINDS`, the bound of `burn.op`'s kind, and
  `vm::burn_op_kind`, the builtins of each kind);
- the interpretation of `BURN.OP` kinds (`src/opcode.rs` enforces
  only the packed shape);
- the exact semantics of `TAILCALL` (when it may replace a `CALL`).
