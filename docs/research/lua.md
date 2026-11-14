# Lua

What cig took from Lua 5.4, adapted, or rejected, and what was measured
each time. The study served #235 ("take Lua 5.4's tricks into the VM and
measure against Lua itself"); #243 is the round after it.

## Status

- **Pins.** Lua `v5.4.9` (commit `312b9efaa1061c2c4cad08554dbc1351c3270eef`);
  LuaJIT branch `v2.1` at `63c8f1a9b479ba5518cf6a170ff7729178bc3cfd`, read
  for contrast and run as `luajit -joff` (its interpreter alone). The test
  machine's comparator binaries are these versions.
- **Where cig stands, as it ships.** Run `w30-join-t1` (`main` with #240,
  the figures 1.24.0 reports; static musl on mimalloc, as released; the 21
  compute programs of `bench/compute`, 11 interleaved rounds, medians):

  | the VM against | time | instructions |
  |---|---:|---:|
  | Lua 5.4.9, geometric mean of 21 | 1.37 | 1.23 |
  | Lua 5.4.9, median of 21 | 1.73 | |
  | Lua 5.4.9, like for like (18: without `json`, `csv`, `json-deep`) | 1.74 | 1.64 |
  | `luajit -joff`, geometric mean of 21 | 1.93 | |

  It was 2.06 in 1.22.0, 1.49 in 1.23.0 (`w29-join-t2`). Furthest from
  Lua: `fib` 5.35, `spectral` 3.36, `closures` 3.15, `sieve` 2.91,
  `dispatch` 2.67, `nbody` 2.61, `richards` 2.58, `mandel` 2.43, `trees`
  2.07. Ahead of Lua: `strings` 0.60, `regex-ish` 0.46, `maps` 0.64,
  `lists` 0.83.
- **The study's first finding.** Lua is not fast per instruction (117.6
  machine instructions a turn of `for i = 1, n do s = s + i % 7 end`). It
  executes few bytecode instructions and, around them, allocates nothing,
  hashes nothing twice and looks nothing up by name.
- **Instruments.** Counts are callgrind on the test machine (exact) or
  `perf stat -e instructions:u`; compare a program within one of them. The
  first wall-clock table was void (seven `fsync`s of a run record, run
  `w28-base-s2`, removed in 1.24.0 by #242): later tables time dry runs.

## The eight tricks of #235

### 1. Interned short strings with a cached hash

- **What Lua does, and why.** Every string of 40 bytes or less goes through
  `lstring.c: internshrstr`: hash every byte with a per-state seed
  (`luaS_hash`), walk the chain, return the existing object or make one, so
  equality is a pointer compare (`eqshrstr`). A long string's hash is made
  the first time it is a key and kept (`luaS_hashlongstr`).
- **Adapt.** Take what costs nothing at run time: a string object that keeps
  its hash, constants made once at load, and map keys that are that object
  (trick 2). Not the table of every live string (entry 12). With tricks 2, 5
  and 6 this is one type, `Value::Str(Rc<StrObj>)`, best introduced once.
- **The experiment.** Not tried yet. Lua's own cost (callgrind):
  `internshrstr` is 17.0M of `strings`' 171.0M machine instructions, 23.7M
  of `maps`' 243.6M.
- **Pinned tag and files read.** Lua `v5.4.9`: `lstring.c` (`luaS_newlstr`,
  `internshrstr`, `luaS_hash`, `luaS_hashlongstr`, `luaS_remove`),
  `lstring.h` (`eqshrstr`), `llimits.h`, `lobject.h` (`TString`).

### 2. Table keys looked up by their cached hash

- **What Lua does, and why.** `ltable.c: luaH_getshortstr` indexes the nodes
  by the key's kept hash and compares tag and pointer: no hashing, no byte
  compare. `luaH_newkey` is a chained scatter table with Brent's variation
  (`getfreepos`). LuaJIT switches a chain to a keyed dense hash past 32
  collisions (`lj_str.c`); Lua 5.4 has no such fallback.
- **Adapt.** Keys become trick 1's object. The map stays an `IndexMap`
  (insertion order is the language's rule), with a hasher that passes the
  kept hash through and hashes a bare `&str` probe the same way. Two steps:
  SipHash-1-3 once per string (no change to what an attacker-chosen key can
  do), then perhaps a faster keyed hash, a security trade for the owner.
- **The experiment.** Not tried yet. SipHash at every lookup is 25.0M
  machine instructions of `maps`, 51.8M of `json` (callgrind, at the start).
- **Pinned tag and files read.** Lua `v5.4.9`: `ltable.c`
  (`luaH_getshortstr`, `luaH_get`, `mainposition`, `luaH_newkey`,
  `getfreepos`), `lobject.h` (`Node`). LuaJIT `v2.1` at `63c8f1a`:
  `src/lj_str.c`, `src/lj_obj.h`.

### 3. The array part

- **What Lua does, and why.** Integer keys 1..n live in a plain array when
  more than half the slots would be used (`ltable.c: computesizes`);
  `luaH_getint` is a compare and an index, inlined through `luaV_fastgeti`.
  Appending is `OP_LEN`, `OP_ADDI`, `OP_SETTABLE`: no call.
- **Adapt.** A cig list is already an array and an int index was already in
  the fast loop; append and length were method calls through the general
  path. Trick 7's site cache makes them cheap; `append`, `len` and
  index-and-compare forms wait for module version 3 and the counts.
- **The experiment.** Landed through trick 7 in #238 (1.23.0): on the VM
  `xs.push(i)` 1,741 to 425 and `xs.len()` 947 to 271 machine instructions
  (`w29-names-c1` against `w29-pair-c0`). The forms are not tried yet.
- **Pinned tag and files read.** Lua `v5.4.9`: `ltable.c` (`computesizes`,
  `luaH_getint`, `rehash`), `lvm.h` (`luaV_fastgeti`), `lvm.c` (`OP_LEN`,
  `OP_GETI`, `OP_SETI`, `OP_SETTABLE`).

### 4. Open upvalues

- **What Lua does, and why.** A captured local stays in its stack slot and
  the closure's `UpVal` points at it (`lfunc.c: luaF_findupval`); when the
  scope ends `luaF_closeupval` copies the value in. The owning function
  reads a plain register; only closures pay an indirection.
- **Adapt.** Not open upvalues themselves: in safe Rust a reference into the
  stack is an index, valid only if every frame that could read it shares
  one stack, and a callback runs in a machine of its own (entry 18). Take
  the effect: captured registers get their own instructions and a function
  with a capture runs in the fast loop. Today any `cellnew` sends the whole
  function to the general path.
- **The experiment.** Not tried yet. A closure made and called: VM 2,310
  machine instructions, tree-walker 1,781 (`w29-calls-c1`).
- **Pinned tag and files read.** Lua `v5.4.9`: `lfunc.c` (`luaF_findupval`,
  `luaF_closeupval`, `luaF_close`), `lobject.h` (`UpVal`), `lvm.c`
  (`OP_GETUPVAL`, `OP_CLOSE`).

### 5. A 16-byte value

- **What Lua does, and why.** `TValue` is an 8-byte union and a tag, 16
  bytes (`lobject.h`); a copy is two stores (`setobj`).
- **Take.** `Value` is 24 bytes because `Rc<str>` is a fat pointer; with
  `Value::Str(Rc<StrObj>)` it compiles to 16 (and `Option` of it to 16): a
  register, list element or argument a third smaller, a map entry 32 bytes
  instead of 56 plus a key block. The count on every copy stays: it is the
  price of deterministic cleanup.
- **The experiment.** Not tried yet; sizes by compiling only. A result is
  written in 11 stores (`w30-loop-v2`); 16 bytes is estimated at about 30
  fewer a `loop` turn (not measured).
- **Pinned tag and files read.** Lua `v5.4.9`: `lobject.h` (`TValue`,
  `setobj`).

### 6. String building

- **What Lua does, and why.** `a .. b .. c` is one `OP_CONCAT`:
  `lvm.c: luaV_concat` adds the lengths and writes one block (`copy2buff`);
  the library uses `luaL_Buffer` (`lauxlib.c: newbuffsize`). `s = s .. x`
  in a loop copies the whole string each turn.
- **Adapt.** One block for interpolation, `+` and `join`, numbers written
  without `core::fmt`. Beyond Lua: when the left operand of `s + x` has one
  owner (`Rc::get_mut`), append in place, making `strbuild` linear.
- **The experiment.** One block landed in #238 (1.23.0): `"item-${i}"`
  1,209 to 636 machine instructions on the tree-walker, 2,198 to 745 on the
  VM (`w29-names-c1` against `w29-pair-c0`). Append in place is not tried
  yet; `strbuild` takes 1.35 times Lua's time (`w30-join-t1`).
- **Pinned tag and files read.** Lua `v5.4.9`: `lvm.c` (`OP_CONCAT`,
  `luaV_concat`, `copy2buff`), `lauxlib.c` (`newbuffsize`), `lauxlib.h`.

### 7. Specialized instructions, and no lookups by name

- **What Lua does, and why.** Constant keys are operands (`OP_GETFIELD`),
  small integers immediates (`OP_ADDI`, `OP_EQI`, `OP_LTI`); `OP_SELF`
  fetches a method and passes the receiver at once; a compare runs its jump
  without a dispatch (`docondjump`). No inline caches: interned keys make
  every lookup a pointer compare.
- **Adapt.** cig scanned the type's method table by name on every call. It
  takes an inline cache instead: each `callm` site keeps the receiver's type
  and the built-in found, and a hit calls it on the register window.
  `stdlib::method` is a `match`; built-ins that cannot call back run in the
  fast loop. `loadg` by index (#230 step 5) is still to do.
- **The experiment.** Landed in #238 (1.23.0). `w29-names-c1` against
  `w29-pair-c0`, tree-walker then VM: `m.has("k")` 2,334 to 933 and 2,352 to
  471; VM `strings` 300.2M to 101.2M. With the calls package, VM time
  against 1.22.0's (`w29-join-t2`): `strings` 40.1 to 15.2 ms, `maps` 66.3
  to 30.4, `words` 50.8 to 23.1.
- **Pinned tag and files read.** Lua `v5.4.9`: `lvm.c` (`luaV_execute`,
  `OP_GETFIELD`, `OP_SELF`, `OP_ADDI`, `docondjump`), `lopcodes.h`.

### 8. Dispatch

- **What Lua does, and why.** Computed goto (`vmfetch`, `ljumptab.h`): 6 to
  8 machine instructions to jump, 10 to 17 to decode, every execution.
  LuaJIT copies the jump into each handler (`src/vm_x64.dasc`).
- **Reject.** Computed goto needs `unsafe` or unstable Rust (`become`,
  `loop_match`); the VM and the bytecode crate forbid `unsafe`. Keep the
  `match` over decoded instructions (no decode) and measure.
- **The experiment.** #240 (1.24.0) dropped the two extra jump tables in
  each arithmetic instruction (`w30-loop-c0` to `w30-loop-v2`): `add` about
  75 to about 30, `loop` 194.4M to 142.7M; VM time against 1.23.0's
  (`w30-join-t1`) `spectral` 123.9 to 76.1 ms, `loop` 23.1 to 17.5.
- **Pinned tag and files read.** Lua `v5.4.9`: `lvm.c` (`luaV_execute`,
  `vmfetch`, `op_arith`, `op_arithK`), `ljumptab.h`. LuaJIT `v2.1` at
  `63c8f1a`: `src/vm_x64.dasc`.

## Calls and loops

### 9. The call (`CallInfo`, `luaD_precall`)

- **What Lua does, and why.** `ldo.c: luaD_precall` checks the stack once
  (`checkstackGCp`), takes the next `CallInfo` (64 bytes, reused, never
  freed: `next_ci`), fills four fields and starts the callee in the same C
  frame; the arguments already are its registers (`func + 1`). `OP_RETURN1`
  copies one value down. 248 machine instructions a call and return in
  `fib`.
- **Adapt.** Arguments in place (the callee starts at the caller's
  `R(A+1)`), a record that owns nothing (tries and burns on the machine's
  lists, the callee read where it lies), and call, return and tail call in
  the frame loop. Unlike Lua, values keep their count and registers still
  start null (entry 17).
- **The experiment.** A call and return on the VM: 1,386 machine
  instructions at the start; 1,200 after #236 (1.22.0, `w26-frames-c1`);
  781 after #238 (1.23.0, record 136 to 64 bytes; tree-walker 951;
  `w29-calls-c1` against `w29-pair-c0`): `fib` 382.5M to 293.4M, and 50.2
  to 34.4 ms against 1.22.0's VM (`w29-join-t2`). **Dropped:** a variant
  leaving the fast loop untouched won 1% on plain loops, lost 2.5 to 3% on
  call-heavy ones (`w29-calls-c2`). **Not measured:** call and return inside
  the fast loop (drafted). #243 aims at `fib` under 3 times Lua; it is 5.35.
- **Pinned tag and files read.** Lua `v5.4.9`: `ldo.c` (`luaD_precall`,
  `prepCallInfo`, `next_ci`, `checkstackGCp`, `luaD_poscall`), `lvm.c`
  (`OP_CALL`, `OP_RETURN1`), `lstate.h` (`CallInfo`).

### 10. The numeric `for` loop (`OP_FORPREP`, `OP_FORLOOP`)

- **What Lua does, and why.** `lvm.c: forprep` computes the trip count once;
  `OP_FORLOOP` counts it down, with no limit compare or overflow check a
  turn: 32 machine instructions in all.
- **Adapt.** cig's `forprep` already counts turns for `E512`, but `forloop`
  does a checked add, a limit compare and recomputes the direction each
  turn. A count from `forprep` removes those, same values and errors.
- **The experiment.** Not tried yet. `forloop` is about 50 after #240, 8 of
  them the direction (`w30-loop-v2`); estimated about 25 fewer a turn (not
  measured). `loop` takes 1.73 times Lua's time (`w30-join-t1`).
- **Pinned tag and files read.** Lua `v5.4.9`: `lvm.c` (`forprep`,
  `OP_FORPREP`, `OP_FORLOOP`).

## What cig does not take from Lua

### 11. The garbage collector

- **What Lua does, and why.** The collector, not a count, keeps objects
  alive, so values copy with two stores.
- **Reject.** Burns, rollback and passes rely on cleanup at a known point.
  #243 cuts count traffic instead (elision and reuse).
- **The experiment.** None: not a candidate.
- **Pinned tag and files read.** Lua `v5.4.9`: `lobject.h`; not `lgc.c`.

### 12. Interning every short string

- **What Lua does, and why.** Trick 1: every short string, a key or not, is
  hashed and looked up in the string table.
- **Reject.** 10% of Lua's string programs; needs a `Drop` that unlinks; and
  `Rc::get_mut` refuses a string a `Weak` points at, so no append in place.
- **The experiment.** Lua's cost only: 17.0M of `strings`' 171.0M.
- **Pinned tag and files read.** Lua `v5.4.9`: `lstring.c` (`internshrstr`).

### 13. Integer arithmetic that wraps

- **What Lua does, and why.** `l_addi` is an unsigned add: overflow wraps.
- **Reject.** cig raises `E503`, on both engines; the add with its overflow
  test is 3 machine instructions (`w30-loop-c0`).
- **The experiment.** None needed.
- **Pinned tag and files read.** Lua `v5.4.9`: `lvm.c` (`l_addi`).

### 14. Numbers to strings through `snprintf`

- **What Lua does, and why.** `lua_integer2str` uses `snprintf`: 15% of
  Lua's `strings`.
- **Reject.** cig writes an integer with its own arithmetic, not `core::fmt`.
- **The experiment.** Landed in #238 (1.23.0): `"${i}"` on the VM 1,554 to
  627 machine instructions (`w29-names-c1` against `w29-pair-c0`).
- **Pinned tag and files read.** Lua `v5.4.9`: `luaconf.h`, `lobject.c`.

### 15. `table.sort` through the C API

- **What Lua does, and why.** The sort goes through `lua_geti`, `lua_seti`
  and `lua_compare`: about 190M of `lists`' 290.5M machine instructions.
- **Reject.** cig's sort is in Rust over the list itself.
- **The experiment.** None needed: `lists` is 0.83 of Lua (`w30-join-t1`).
- **Pinned tag and files read.** Lua `v5.4.9`: `ltablib.c` (`sort`), `lapi.c`.

### 16. Globals as a table lookup

- **What Lua does, and why.** A global is `OP_GETTABUP` on `_ENV`, a field
  lookup at every use.
- **Reject.** cig can resolve prelude names to indexes at load (#230 step 5).
- **The experiment.** Not tried yet: `loadg` reads the prelude by name.
- **Pinned tag and files read.** Lua `v5.4.9`: `lvm.c` (`OP_GETTABUP`).

## What the source shows beyond #235

Arguments in place, the call record and the trip count are entries 9, 10.

### 17. Registers never cleared

- **What Lua does, and why.** A call and a return clear no register: the
  compiler guarantees a write before any read.
- **Adapt.** The verifier's `read-before-write` (#236), extended to every
  register past the parameters as a per-function flag at load, would let a
  frame skip its null fill; a function that breaks it keeps the fill. The
  lowering breaks it today (`cellnew` before `loadnull` for a captured local
  in a block), so that changes first.
- **The experiment.** Tried and kept (`w29-calls-nf1`, `fib` and `trees`):
  the fill costs 22 machine instructions on a one-parameter call, about 119
  a call in `fib` (9.8%), about 157 in `trees` (7.5%). Removing it safely
  needs a second dataflow walk at load and the lowering change.
- **Pinned tag and files read.** Lua `v5.4.9`: `ldo.c` (`luaD_precall`),
  `lvm.c` (`OP_RETURN1`).

### 18. One machine

- **What Lua does, and why.** A C function calling back into Lua runs the
  callee on the same stack: `ldo.c: luaD_call` re-enters `luaV_execute`
  with `CIST_FRESH`.
- **Adapt.** cig built a machine per callback. One register stack per
  interpreter removes those allocations and is the precondition for open
  upvalues (trick 4).
- **The experiment.** Half landed in #238 (1.23.0): callbacks reuse one kept
  set of registers. A callback 1,734 to 1,037 machine instructions
  (tree-walker 741), `lists` 370.8M to 309.5M (`w29-calls-c1`). The shared
  stack is not tried yet.
- **Pinned tag and files read.** Lua `v5.4.9`: `ldo.c` (`luaD_call`,
  `ccall`), `lstate.h` (`CIST_FRESH`).

### 19. Table constructors carry their sizes

- **What Lua does, and why.** `OP_NEWTABLE` carries the sizes the
  constructor will fill; `OP_SETLIST` stores a run of registers at once.
- **Adapt.** `newmap` has a capacity; constant keys (trick 2) are the rest.
- **The experiment.** Literals moved into the fast loop in #238 (1.23.0): on
  the VM `[null, null]` 1,117 to 633, `{a: i, b: 2}` 2,788 to 2,153
  (`w29-names-c1` against `w29-pair-c0`). Constant keys not tried yet.
- **Pinned tag and files read.** Lua `v5.4.9`: `lvm.c` (`OP_NEWTABLE`,
  `OP_SETLIST`).

### 20. Slow paths as separate instructions

- **What Lua does, and why.** An arithmetic instruction's slow path is the
  next instruction (`OP_MMBIN`), skipped on the fast path, so the hot
  handler holds no call.
- **Take**, in cig's existing form: the fast loop changes nothing and
  returns, and the general path reruns the instruction with its error.
- **The experiment.** #240 (1.24.0) keeps int `/` and int-and-float
  arithmetic in the fast loop (part of `w30-loop-v2`, entry 8).
- **Pinned tag and files read.** Lua `v5.4.9`: `lvm.c` (`OP_MMBIN`),
  `lopcodes.h` (`OP_MMBINI`, `OP_MMBINK`).

### 21. Results returned small

- **What Lua does, and why.** A result stays in a stack slot: `OP_RETURN1`
  copies one value to `base - 1`.
- **Take.** cig's built-ins and tree-walker return `Result<Value,
  Diagnostic>`, 144 bytes through memory; with the error boxed, 24. A cost
  for #213, not the VM.
- **The experiment.** Not tried yet; the copies are 7 to 23M a program.
- **Pinned tag and files read.** Lua `v5.4.9`: `lvm.c` (`OP_RETURN1`).

## Not studied yet

- Lua 5.5's array part (values and tags apart, 9 bytes an element).
- LuaJIT's interpreter beyond its value, dispatch and string hashing (it is
  1.2 to 1.9 times leaner than Lua 5.4 on the string, map, list and closure
  programs; its NaN-boxed value needs `unsafe` and is rejected in #243).
- The fetch of a decoded instruction (`proto.code.get(pc)`), named under
  trick 8 as a thing to measure; and a faster keyed map hash (trick 2).
