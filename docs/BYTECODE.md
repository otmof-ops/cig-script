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
| `try.enter` H | ABx | enter handler `H` |
| `try.leave` | ABC | leave the current handler |
| `throw` A | ABC | throw `R(A)` |

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
| `steps` Bx | ABx | charge the step budget by `Bx` ticks |

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
