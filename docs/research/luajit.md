# LuaJIT

What cig takes from LuaJIT, adapts, or rejects, with an entry for every file
of its `src/`. LuaJIT comes first among the languages #243 studies: its
interpreter is the fastest there is, and its trace compiler is where the
decisions of cig's stage C start (cig-compile#32). `lua.md` is the study
before this one.

## Status

- **Pin.** LuaJIT branch `v2.1` at
  `63c8f1a9b479ba5518cf6a170ff7729178bc3cfd` (MIT). The test machine's
  `luajit` is built from it and run as `luajit -joff`, its interpreter
  alone. Read with the author's own write-ups beside the source: the wiki
  pages on the bytecode, the SSA IR, the optimizations and allocation
  sinking, and his mailing-list posts on the interpreter and the trace
  compiler (`papers/luajit/` in the reference library).
- **Tools.** `luajit -bl` beside `cigcompile build --emit bc` for the
  listings; a count hook on Lua 5.4 and on `luajit -joff` beside the VM's
  own statistics for instructions executed; callgrind for machine
  instructions. LuaJIT's handlers are assembly without symbols, so callgrind
  sees its interpreter as one block: a count by handler is owed (see the
  end).
- **Where cig stands, as it ships.** cig 1.24.0's VM takes 1.93 times
  `luajit -joff`'s time at the geometric mean of the 21 compute programs
  (`w30-join-t1`). #243 aims Part 1A at 1.3 times at most, and at a call and
  return within 2 times LuaJIT's interpreter's.
- **One operation, in machine instructions** (run `w32-ljcount-j1`: cig
  1.24.0, Lua 5.4.9 and LuaJIT at the pin; each operation in a loop of
  100,000 turns, minus the same loop without it; callgrind):

  | operation | cig's tree-walker | cig's VM | Lua 5.4 | `luajit -joff` | VM over LuaJIT |
  |---|---:|---:|---:|---:|---:|
  | a call and return (`f(x)` returning `x`) | 960 | 900 | 162 | 61 | 14.7 |
  | a function value called once by `.map()` | 749 | 1,123 | 162 | 61 | 18.4 |
  | a call that makes a closure | 1,786 | 2,356 | 1,083 | 834 | 2.8 |
  | a field read | 171 | 88 | 38 | 21 | 4.2 |
  | a turn of `for .. in` over a list | 800 | 717 | 322 | 91 | 7.9 |
  | an integer made into a string (`"${i}"`) | 603 | 628 | 1,666 | 919 | 0.7 |
  | a string built from a literal and an integer | 635 | 753 | 1,795 | 927 | 0.8 |
  | a list's length (`.len()`) | 417 | 259 | 45 | 49 | 5.3 |
  | a list read by index | 217 | 38 | 16 | 15 | 2.5 |
  | a list literal of two | 437 | 614 | 848 | 450 | 1.4 |
  | a map read by key | 363 | 97 | 55 | 29 | 3.4 |
  | a key test (`.has()`) | 938 | 466 | 100 | 46 | 10.1 |
  | a map literal of two | 1,635 | 2,157 | 1,223 | 754 | 2.9 |
  | a map write by key | 1,157 | 261 | 79 | 40 | 6.5 |
  | an append to a list (`.push()`) | 637 | 426 | 223 | 252 | 1.7 |
  | a string comparison | 366 | about 0 | 50 | 33 | |

  (Lua has no `.map()`: its version of that row is a plain call for each
  element.) The VM is behind LuaJIT's interpreter on every operation but
  building a string, and behind its own tree-walker on a callback, on making
  a closure, on building a string from two parts, and on the two literals.
  **A call and return is 900 against 61:** 14.7 times, where the target is 2
  (122). It was 781 on 1.23.0 by the same method (`w29-calls-c1`).
- **Bytecode, as compiled and as executed** (the same run; twelve programs
  whose Lua versions are written the same way: the other nine do in a
  builtin on cig what their Lua versions do in Lua, or print differently):

  | program | instructions in the listing: LuaJIT | cig | executed: cig's VM | Lua 5.4 | `luajit -joff` | VM over LuaJIT |
  |---|---:|---:|---:|---:|---:|---:|
  | `loop` | 13 | 23 | 2,400,017 | 1,600,014 | 1,600,014 | 1.50 |
  | `fib` | 20 | 29 | 2,185,075 | 1,335,326 | 1,578,112 | 1.38 |
  | `closures` | 24 | 42 | 2,100,021 | 1,300,015 | 1,400,015 | 1.50 |
  | `dispatch` | 36 | 65 | 2,450,042 | 2,150,026 | 2,150,022 | 1.14 |
  | `mandel` | 53 | 86 | 15,729,301 | 10,388,770 | 12,292,025 | 1.28 |
  | `sieve` | 50 | 88 | 7,612,450 | 5,169,179 | 6,685,378 | 1.14 |
  | `nbody` | 315 | 457 | 17,881,222 | 14,881,013 | 14,881,002 | 1.20 |
  | `spectral` | 178 | 199 | 8,458,608 | 7,638,183 | 7,238,285 | 1.17 |
  | `fannkuch` | 123 | 201 | 12,416,903 | 9,902,096 | 10,791,323 | 1.15 |
  | `trees` | 85 | 133 | 16,878,487 | 10,122,759 | 9,777,339 | 1.73 |
  | `maps` | 27 | 51 | 900,030 | 575,019 | 575,019 | 1.57 |
  | `strings` | 21 | 35 | 480,027 | 420,019 | 420,019 | 1.14 |

  cig compiles to 1.1 to 1.9 times LuaJIT's instructions and executes 1.1 to
  1.7 times as many. **The step-budget instruction `steps` is a large part
  of the difference:** 22.2% of the instructions `fib` executes, 33.3% of
  `loop`'s (two for each turn), 23.8% of `closures`'. Without it `loop`
  executes exactly Lua's and LuaJIT's 1,600,014 (see A1 and A8).
- **Whole programs, in machine instructions** (the same run): `fib` 305.8M
  on cig's VM, 58.8M on Lua 5.4, 30.0M on `luajit -joff`; `loop` 149.2M,
  60.4M, 38.5M. For each bytecode executed that is 140 machine instructions
  on cig's VM in `fib` and 62 in `loop`, 44 and 38 on Lua 5.4, 19 and 24 on
  LuaJIT.
- **How to read the entries.** Part 1A is the interpreter, for cig-script's
  VM and the lowering in cig-compile. Part 1B is the trace compiler, read as
  decisions for stage C, which compiles ahead of time: nothing in it is a
  plan for a JIT at run time. Then what cig does not take, and an entry for
  every one of the 194 files of `src/`.

## Part 1A: the interpreter

### A1. A bytecode built for its interpreter

- **What LuaJIT does, and why.** Instructions are a fixed 32-bit word in two
  layouts, ABC and AD (`lj_bc.h`). `BCDEF` carries three families:
  operand-kind variants (`BC_ADDVN`, `BC_ADDNV`, `BC_ADDVV`), constant-key
  table instructions (`BC_TGETS`, `BC_TSETS`, `BC_TGETB`), and compares
  (`BC_ISLT`, `BC_ISEQV`, `BC_ISEQS`) paired with a `BC_JMP`. The numeric
  `for` is a four-slot window driven by `BC_FORI` and `BC_FORL`.
  `lj_parse.c` picks each form (`bcemit_arith`, `bcemit_comp`).
- **Adapt.** cig has the constant-operand forms (`ADDK` to `MODK`; the
  lowering turns a left constant into them for `==`, `!=`, `+` and `*`), a
  fused compare-and-jump (`EQJ`, `LTJ`, `LEJ`, `NEJ`), `LOADINT`, and the
  four-slot `for` (`FORPREP`, `FORLOOP`). Where LuaJIT's listing is shorter
  (`loop` 13 against 23; `fib`'s function 12 against 15): the step charges
  (two `steps` a turn of `loop`, two a call of `fib`); a `jmp` over an
  `if`'s body, since the fused compare jumps into the body; a loop's bounds
  copied into its registers. The items, each a change in cig-compile's
  `src/bc/mod.rs` with an opcode, a VM arm and a verifier rule: a `steps`
  fused with the instruction after it, charging first so the order of errors
  holds (it needs a second position at its pc, and `UnchargedCycle` taught
  to accept it); a fused compare with its sense inverted (operand order
  kept, for `E403`'s wording); `POWK`; a left constant for `-`, `/`, `%`,
  `<`, `<=`; a compare-and-jump against a constant; a small-integer key in
  `GETINDEX` and `SETINDEX`; a run of nulls in one `LOADNULL`.
- **The experiment.** The listings and the instructions executed are in the
  Status tables (`w32-ljcount-j1`): `steps` is 22.2 percent of what `fib`
  executes and 33.3 percent of `loop`'s, and without it `loop` executes what
  Lua and LuaJIT do. Not measured yet: each item alone.
- **Pinned tag and files read.** LuaJIT `v2.1` at `63c8f1a`: `lj_bc.h`,
  `lj_parse.c`. Papers: `papers/luajit/wiki-bytecode-2.0.html`. cig:
  `crates/cig-bytecode/src/opcode.rs`, `src/vm/mod.rs`, `src/bc/mod.rs`.

### A2. Frames that live on the value stack

- **What LuaJIT does, and why.** Under `LJ_FR2`, the callee's base sits two
  slots above the saved function and an `ftsz` word holding the previous PC,
  the frame kind in its low bits. `BC_CALL` reads the function, runs
  `checkfunc`, writes the caller PC through `ins_call`, dispatches into
  `BC_IFUNCF`. The prologue tests stack room (`vm_growstack_f`) and fills
  missing parameters. `BC_RET1` reloads PC, rebases, reloads KBASE,
  dispatches. No frame record exists.

- **Adapt.** cig keeps a record for every frame (`src/vm/mod.rs`: `Frame`,
  64 bytes), where the issue's row would keep one only where a try or a burn
  needs it: it also holds the function, the return pc, the base and the
  counts `E506` and `E515` read. What a cig call does that LuaJIT's does
  not: it leaves the fast loop (`CALL`, `RET` and `TAILCALL` are not arms of
  `run_fast`: `run_frame_code` switches frames through `call_here` and
  `ret_here` and restarts `run_fast`); it clones the callee and pushes its
  upvalues, releasing them at the return; it writes the record field by
  field. In `fib`'s cycles (`w32-gap-r6`) `call_here` is 15 percent and the
  frame loop 14, and the count's increment and decrement are the hottest
  lines of `call_here` and `release`. The work, in order: call and return
  inside the fast loop (`docs/research/lua.md` entry 9), the clones, the
  record's stores.

- **The experiment.** A call and return of a one-parameter function
  (`w32-ljcount-j1`): cig's VM 900 machine instructions, Lua 5.4 162,
  `luajit -joff` 61: the 53 read from `vm_x64.dasc` (`BC_CALL` 16,
  `BC_IFUNCF` 14, `BC_RET1` 23; `LJ_FR2`, no JIT, as many arguments as
  parameters, one result) plus the argument's move and the body. cig is at
  14.7 times; the target is 2 times (122). Not measured yet: each of the
  three items alone.

- **Pinned tag and files read.** LuaJIT `v2.1` at `63c8f1a`:
  `src/lj_frame.h`, `src/vm_x64.dasc`, `src/lj_state.c`. cig:
  `src/vm/mod.rs`.

### A3. The dispatch loop, laid out by hand

- **What LuaJIT does, and why.** Every handler of `vm_x64.dasc` ends with
  its own copy of the dispatch (`ins_NEXT`: fetch four bytes at PC, split
  out the opcode and the operands, jump through DISPATCH). The source pins
  state in callee-saved registers (on POSIX: PC in RBX, KBASE in R15,
  DISPATCH in R14); BASE is refetched after every C call. Slow paths sit in
  a trailing block reached by a conditional jump (`vmeta_arith_vv`,
  `vm_growstack_f`). The author's write-up
  (`papers/luajit/pall-2011-02-15-interpreter-cfg-replication.html`) names
  what a C compiler cannot do here: keep a fixed register assignment across
  the dispatch, or know which paths are cold.

- **Adapt.** cig has some of this already and some is the work item.
  Already: `#[cold]` on slow paths off `run_frame_code`, `#[inline(never)]`
  on `fast_call`, a load-time decode into `DecodedInstr`, a byte-indexed
  `Window` of `REG_WINDOW` cells so one-word reads skip the bounds check, a
  `match d.op` in `fast` that rustc compiles to a jump table. The work:
  `perf annotate` of `run_fast` beside LuaJIT's handlers for the same
  bytecode, to move what is cold out of each arm; and a release built from a
  profile, then laid out by BOLT (#243's step 0), which gives the compiler
  the layout the assembly source has by hand.

- **The experiment.** Not measured yet: `run_fast`'s arms beside LuaJIT's
  handlers for the same bytecode, instruction by instruction (LuaJIT's
  handlers have no symbols, so a count by address is owed). What exists: the
  VM takes 1.93 times `luajit -joff`'s time at the geometric mean
  (`w30-join-t1`); for each bytecode executed, cig's VM runs 140 machine
  instructions on `fib` and 62 on `loop`, LuaJIT 19 and 24
  (`w32-ljcount-j1`); a turn of `loop` is 352 on cig's VM (`w30-loop-v2`).

- **Pinned tag and files read.** LuaJIT `v2.1` at `63c8f1a`:
  `src/vm_x64.dasc` (`ins_NEXT`, `BC_ADDVV`, `BC_FORL`, `vmeta_arith_vv`),
  `src/lj_vm.h`. Write-up:
  `papers/luajit/pall-2011-02-15-interpreter-cfg-replication.html`. Against
  cig: `src/vm/mod.rs`.

### A4. Fast functions: the hottest built-ins written into the interpreter

- **What LuaJIT does, and why.** `LJLIB_ASM` in `src/lj_lib.h` marks a
  library function with its fast path in `src/vm_x64.dasc` under a `ff_*`
  label. It is reached through a per-function pseudo-opcode past `BC__MAX`:
  at state init, `src/lj_dispatch.c` fills `bcff` in `GG_State`, one entry
  per fast function. `GG_NUM_ASMFF` is 57. On a miss, control jumps to
  `fff_fallback` and calls the C fallback that `LJLIB_ASM` expands to; the
  fallback returns `FFH_RES`, `FFH_TAILCALL` or `FFH_RETRY`.
- **Adapt.** The fast loop's second tier (`src/vm/mod.rs`: `fast_call`)
  already runs, without leaving the loop and in a function whose registers
  carry no cell, every builtin that cannot call back (`calls_back` false):
  under `Op::CALLB`, and under `Op::CALLM` where the site's cache holds the
  receiver's type. It calls each through the one signature every builtin has
  (`src/value.rs`: `BuiltinFn`, an argument slice in and a `Result` out),
  where LuaJIT gives each fast function a handler of its own. The work item:
  the hottest get arms of their own in `fast_call`, one at a time, each
  measured and held to the tree-walker by the differential suite. The four
  `lib_*.c` entries below name the 57; the table groups them.

| LuaJIT | fallback rule | cig counterpart | in `fast_call` | programs |
|---|---|---|---|---|
| `lib_base.c`, 16 | a wrong type or count of arguments; a metatable | globals `assert`, `type_of`, `int`, `float`, `str` in `src/stdlib/core.rs`; iteration (LuaJIT's `ipairs`, `pairs`, `next`) is `Op::ITERPREP` and `Op::ITERNEXT` | the globals yes, where no register carries a cell; `ITERPREP` and `ITERNEXT` are not arms of the fast loop (`loop_op` runs them): a turn of `for .. in` is 717 machine instructions against 91 for `ipairs` on `luajit -joff` | `float()` in `csv`; `int()` in five programs; `len()`, `str()` in `startup-10` |
| `lib_math.c`, 24 | a non-number argument, or the wrong number of them | `math.abs`, `math.floor`, `math.ceil`, `math.sqrt`, `math.pow`, `math.min`, `math.max` in `src/stdlib/math.rs` | the same | `math.abs` in `startup-1`, `startup-10`; `math.sqrt` in `nbody`, `spectral`; `math.pow` in `trees` |
| `lib_string.c`, 6 | `string.byte`: anything but one string argument; `string.char`: anything but one number up to 255; `string.sub`: a non-string, fewer than two arguments, or an index that is not a number; `string.lower`, `string.upper`, `string.reverse`: a non-string | `.upper()`, `.lower()`, `.reverse()`, `.slice()` in `src/stdlib/methods.rs` | the same | none in `bench/compute` |
| `lib_bit.c`, 11 | a non-number argument | none | | none |

- **The experiment.** No fast function has been measured against its cig
  counterpart yet. Measured (`w32-ljcount-j1`), cig's method calls against
  Lua's and LuaJIT's instructions for the same job: `.len()` 259 against the
  length operator at 45 and 49; `.has()` 466 against a field test at 100 and
  46; `.push()` 426 against an append by index at 223 and 252, and against
  `table.insert`, a C function, at 462 and 317. Not measured yet:
  `math.sqrt`, `math.floor`, `math.abs`, `math.pow`, `float()` and `int()`;
  and which builtins cigo's program calls.
- **Pinned tag and files read.** LuaJIT `v2.1` at `63c8f1a`: `src/lj_lib.h`,
  `src/lj_lib.c`, `src/lj_ff.h`, `src/lj_dispatch.h`, `src/lj_dispatch.c`,
  `src/lib_base.c`, `src/lib_math.c`, `src/lib_string.c`, `src/lib_table.c`,
  `src/lib_bit.c`, `src/vm_x64.dasc`. cig: `src/vm/mod.rs`, `src/value.rs`,
  `src/stdlib/core.rs`, `src/stdlib/math.rs`, `src/stdlib/methods.rs`.

### A5. NaN-tagged values

- **What LuaJIT does, and why.** A value is an 8-byte `TValue` union of a
  `lua_Number`, a `u64`, and, on `LJ_GC64`, an `it64` overlaying a double's
  upper bits with a 4-bit tag over a special NaN (`lj_obj.h`). A non-number
  holds its tag there and a pointer or integer in the low 47 bits. Type
  tests are a shift and a compare (`itype`, `tvisnil`, `tvisnum`, `tvisint`,
  `tvisstr`, `tvistab`), with tag constants (`LJ_TNIL`, `LJ_TSTR`,
  `LJ_TFUNC`, `LJ_TTAB`, `LJ_TISNUM`) read by sign. A set is one store
  (`setnumV`, `setintV`, `setgcV`, `setstrV`); a copy is one load-store
  (`copyTV`). On x64 under single-number (`lj_arch.h` `LJ_ARCH_NUMMODE` is
  `LJ_NUMMODE_SINGLE_DUAL`) the arithmetic handler tests the number tag
  (`checknumtp`) then runs an SSE add (`ins_arithpre`); under dual-number it
  tests the integer tag (`checkint`) first and on either miss jumps to the C
  fallback `vmeta_arith_vvo` (`lj_meta_arith`), not a float arm. The
  constant-key field handler hashes by the key's interned id and compares
  the full tagged pointer (`BC_TGETS`).
- **Reject.** An 8-byte value needs `unsafe` in Rust, which the VM and the
  bytecode crate forbid. cig's form is its tagged `Value` in `src/value.rs`:
  24 bytes today, 16 once the value of `docs/research/lua.md` entry 5 is
  built. Take the order of tag tests: integer first for arithmetic
  (`num_add`, `num_sub`, `num_mul` in `src/vm/mod.rs`), container first for
  field and index.
- **The experiment.** A field read is 88 machine instructions on cig's VM
  against LuaJIT's 21 (`w32-ljcount-j1`). Not measured yet: the breakdown of
  the 88 (how many go to the `match` on `Value`, the inline-cache slot, the
  map lookup) and the order-of-tag-tests cost on `Value`.
- **Pinned tag and files read.** LuaJIT `v2.1` at `63c8f1a`: `lj_obj.h`,
  `lj_arch.h`, `vm_x64.dasc` (tag-test order of `BC_ADDVV` and `BC_TGETS`).
  Write-ups: `papers/luajit/pall-2009-11-02-luajit-2.0-ip-disclosure.html`.
  cig side: `src/value.rs`, `src/vm/mod.rs`.

### A6. Tables and strings: every string interned, every string key found by its kept id

- **What LuaJIT does, and why.** Every string enters through `src/lj_str.c:
  lj_str_new`: `hash_sparse` reads at most four 32-bit words, whatever the
  length. The string table is a doubling array of chains linked through
  `nextgc`; a match compares the kept hash, the length and the bytes. A
  chain past `LJ_STR_MAXCOLL` collisions is rehashed with `hash_dense`.
  `GCstr` (`src/lj_obj.h`) carries `StrID` and `StrHash` ahead of the bytes.
  `GCtab` has an array part and a hash part of `Node` entries;
  `lj_tab_getstr` finds a string key by masking its `sid` (`hashstr` in
  `src/lj_tab.h`) and comparing tag and pointer along the chain. Strings are
  built in `SBuf`, which doubles.
- **Reject.** Interning is out for two reasons true of any interning
  (`docs/research/lua.md` entry 12): a `Drop` that unlinks, and no append in
  place (`Rc::get_mut` refused). Its cost figure is Lua 5.4's, whose hash
  reads every byte; LuaJIT's is not measured. cig takes the kept hash alone
  (entries 1 and 2, not built): a string object keeping a SipHash-1-3 made
  once, so no dense fallback, and an `IndexMap` whose hasher passes it
  through. One two-part table is rejected: list and map are separate types
  (`src/value.rs`; entry 3). String building is one block since 1.23.0,
  integers written without `core::fmt` (entries 6 and 14).
- **The experiment.** Not measured yet: cig builds interning every string,
  short strings only, or none (hash kept), on the string programs and cigo.
  On record: a map read by key is 97 machine instructions on cig's VM, 55 on
  Lua 5.4, 29 on `luajit -joff` (`w32-ljcount-j1`); SipHash at every lookup
  was 25.0M of `maps`, 51.8M of `json` at its start (entry 2).
- **Pinned tag and files read.** LuaJIT `v2.1` at `63c8f1a`: `src/lj_str.c`,
  `src/lj_str.h`, `src/lj_obj.h`, `src/lj_tab.c`, `src/lj_tab.h`,
  `src/vm_x64.dasc`, `src/lj_buf.h`, `src/lj_buf.c`, `src/lj_strfmt.c`,
  `src/lj_strfmt_num.c`. Write-ups: `papers/luajit/wiki-bytecode-2.0.html`.
  cig: `docs/research/lua.md` entries 1, 2, 3, 6, 12, 14; `src/value.rs`.

### A7. Numbers

- **What LuaJIT does, and why.** `lj_arch.h` names four number modes
  (`LJ_NUMMODE_SINGLE`, `LJ_NUMMODE_SINGLE_DUAL`, `LJ_NUMMODE_DUAL`,
  `LJ_NUMMODE_DUAL_SINGLE`) and sets `LJ_ARCH_NUMMODE` per target: x86 and
  x64 default to single-number; ARM, ARM64, MIPS and PPC default to
  dual-number. `LJ_DUALNUM` derives from the mode and `LUAJIT_NUMMODE`. In
  single-number every number is a double, `tvisint` is always false, and
  `setintV` stores a double. In dual-number an integer carries tag
  `LJ_TISNUM` and a 32-bit payload. On x64 under `LJ_DUALNUM`, the macro
  `ins_arith` expands to `ins_arithdn` alone: `BC_ADDVV` runs `checkint` on
  each operand, does a native integer add with overflow, and on either miss
  jumps to the C fallback `vmeta_arith_vvo` (`lj_meta_arith`), not to a
  sibling float arm. The integer-arm-beside-float-arm shape is
  `vm_arm64.dasc`'s (`ins_arithdn` and `ins_arithfp`). Non-asm fallbacks sit
  in `lj_vmmath.c` (`lj_vm_modi`, `lj_vm_log2`, `lj_vm_foldarith`,
  `lj_vm_foldfpm`). Number-to-integer conversion goes through macros in
  `lj_obj.h` (`lj_num2int`, `lj_num2i64`, `lj_num2u64`, `lj_num2bit`).
- **Adapt.** cig keeps `Value::Int(i64)` and `Value::Float(f64)` distinct
  (`src/value.rs`), so the mode toggle, the bit-for-bit num-to-int rules and
  the integer-plus-float arm sibling (only `vm_arm64.dasc` carries it)
  disappear. The overflow check cig already does (`num_add`, `num_sub`,
  `num_mul` in `src/vm/mod.rs` through `checked_add`, `checked_sub`,
  `checked_mul`), with `E503` (`docs/ERRORS.md`). The take is
  `vm_arm64.dasc`'s handler shape, which cig has: `num_add`, `num_sub` and
  `num_mul` match on `Int` first and fall to `Float` and the mixed path. The
  rules for when a number can stay an integer in compiled code are B4's;
  integer-specialized instruction forms belong to #243's Part 2.
- **The experiment.** An `add` is about 30 machine instructions on the VM
  and a turn of `loop` about 352 (`w30-loop-v2`). `loop` takes 1.73 times
  Lua 5.4's time (`w30-join-t1`), and executes 2,400,017 bytecodes against
  Lua's and LuaJIT's 1,600,014 (`w32-ljcount-j1`). Not measured yet:
  dual-number on ARM64 beside single-number on x64.
- **Pinned tag and files read.** LuaJIT `v2.1` at `63c8f1a`: `lj_arch.h`,
  `lj_obj.h`, `lj_vmmath.c`, `vm_x64.dasc` (`ins_arithdn`, `ins_arithpre`).
  cig side: `src/value.rs`, `src/vm/mod.rs`, `docs/ERRORS.md`.

### A8. Hot counters

- **What LuaJIT does, and why.** Sixty-four `uint16_t` slots, hashed from
  the PC, live in `GG_State` beside the dispatch table (`GG_DISP2HOT`);
  `hotcount_get` and `hotcount_set` do the hash. `hotloop` and `hotcall` in
  `vm_x64.dasc` each inline five instructions: the PC is moved, shifted and
  masked, the slot decremented by `HOTCOUNT_LOOP` or `HOTCOUNT_CALL`, and a
  branch takes the cold path. On underflow `vm_hotloop` calls
  `lj_trace_hot`; the trace attaches at `BC_JFORL` and `BC_JLOOP`.
  `HOTCOUNT_LOOP` is two and `HOTCOUNT_CALL` one, so a loop trips in half as
  many events as a call.

- **Adapt.** cig counts one thing today, the step budget: a `steps` at each
  statement and each loop item (cig-compile's `src/cir/lower.rs`), which
  also sets the frame's loop nesting (it picks `E515`'s wording), and the
  verifier refuses a cycle with no charging `steps` (`UnchargedCycle`). That
  tick stays separate and exact: a hot counter never replaces it. The
  counter is proposed, not built: one count for each loop's back edge and
  each function's entry, decremented where those are taken (the `FORLOOP`
  and `LOOP` arms of `run_fast`; `ITERNEXT` in `loop_op`, which the fast
  loop does not run; calls in `run_frame_code`), which on reaching zero
  records the site for the specializing forms of #243's Part 2 and for stage
  C's profile (B1). It changes no instruction and nothing a program can
  observe. A `steps` may be fused with the instruction after it only if it
  still charges first (A1).

- **The experiment.** Measured (`w32-ljcount-j1`): `steps` is 22.2 percent
  of `fib`'s executed instructions, 33.3 percent of `loop`'s. Without them
  `loop` executes Lua 5.4's and `luajit -joff`'s 1,600,014. LuaJIT's counter
  cost is read from the listing (five instructions a hot op), not measured:
  `-joff` installs `BC_IFORL`.

- **Pinned tag and files read.** LuaJIT `v2.1` at `63c8f1a`:
  `src/lj_dispatch.h`, `src/lj_dispatch.c`, `src/vm_x64.dasc` (`hotloop`,
  `BC_FORL`, `BC_JFORL`, `BC_IFORL`). Against cig: `src/vm/mod.rs`,
  `cig-compile/src/cir/lower.rs`.

## Part 1B: the trace compiler

### B1. Trace recording (bytecode to typed SSA, while the interpreter runs)

- **What LuaJIT does, and why.** A loop or call whose down-counter in
  `lj_dispatch.c` trips calls `lj_trace_hot`, which enters the state machine
  `trace_state`. `trace_start` calls `lj_record_setup`, which takes the
  first snapshot through `lj_snap_add`. Every executed bytecode then
  re-enters `lj_record_ins`, which emits typed IR for the path the VM is
  taking. Calls go through `rec_call_specialize`, loops through `rec_for`,
  built-ins through `lj_ffrecord_func`. Recording ends on return to the
  start pc or at an inner trace; a refused built-in or a snapshot limit
  aborts (`trace_abort`, reasons in `lj_traceerr.h`). The IR carries the
  types seen.
- **Adapt.** Stage C (cig-compile#32) is planned ahead of time, through
  Cranelift, with no memory both written and executed at run time. Nothing
  in cig-compile records today: `src/cir/lower.rs: lower` makes CIR from the
  source and `src/bc/mod.rs: lower` emits bytecode from it, once, at build.
  Proposed, for stage C: a path record written by the VM in a build step,
  and a lowering of each recorded path. cig-script's verifier
  (`verify_module`) checks operand ranges, jumps, read-before-write and
  charged cycles; it types nothing, so a path's IR types each slot it reads
  itself. The hot paths would be found by A8's counter. What a record says
  of a value's type is a guess: compiled code guards it, with a snapshot
  before the guard (B7), and anything else stays on the VM (B8). A JIT tier
  at run time is a later, separate, opt-in decision.
- **The experiment.** Not measured yet: compile one hot path of
  `bench/compute/loop.cig` through Cranelift from a path record; compare
  with `run_fast` on the same program (`loop` whole is 149.2M machine
  instructions on cig's VM and 38.5M on `luajit -joff`, `w32-ljcount-j1`)
  and with LuaJIT's JIT on.
- **Pinned tag and files read.** LuaJIT `v2.1` at `63c8f1a`:
  `src/lj_trace.c`, `src/lj_record.c`, `src/lj_ffrecord.c`, `src/lj_snap.c`,
  `src/lj_dispatch.c`, `src/lj_traceerr.h`. cig: cig-compile
  `src/bc/mod.rs`, `src/cir/lower.rs`, `src/cir/ir.rs`; cig-script
  `crates/cig-bytecode/src/verify.rs`.

### B2. The SSA IR, with FOLD, CSE and DCE on every emit

- **What LuaJIT does, and why.** The IR's shape is a 64-bit `IRIns` with two
  16-bit operand references, an 8-bit `IRType1`, an 8-bit `IROp1`, and a CSE
  skip-list `prev`. `IRDEF` names every opcode: guards, constants, bit ops,
  arithmetic, overflow arithmetic (`ADDOV`, `SUBOV`, `MULOV`), typed loads
  and stores, allocations, buffer ops, barriers, conversions and calls.
  `lj_ir_emit` writes the pending instruction into the per-op chain. Emits
  from the recorder pass through `emitir` (in `lj_record.c`) and so through
  `lj_opt_fold`; `emitir_raw` bypasses the fold. `lj_opt_fold` hashes a
  24-bit key of the pending opcode plus its operands' opcodes and runs the
  first matching rule from a table (`LJFOLD` plus `LJFOLDF`, generated to
  `lj_folddef.h` by `host/buildvm_fold.c`). Rules do constant folding,
  simplification, reassociation and load forwarding through `lj_opt_mem.c`;
  on a miss, `lj_opt_fold` calls `lj_opt_cse`. `lj_opt_dce` runs once before
  loop, only when loop optimization is on and the trace links to itself.
- **Adapt.** CIR has four passes in cig-compile `src/cir/pass/`:
  `fold_consts.rs` (`FoldConsts`), `copy_prop.rs`, `sccp.rs` (`Sccp`),
  `dead_pure.rs`. `FoldConsts` folds operators only where the interpreter
  cannot fail (via `fold_binary` and `fold_unary` in `src/cir/ir.rs`);
  `Sccp` empties a side of an `if` whose condition is constant. T4 in
  `src/cir/pass/mod.rs`'s header says an operation that may raise goes only
  where the verifier proves it cannot raise; a fold cannot specialize by an
  observed type because cig-compile does not type values (`Ty::Dynamic` on
  almost every lowered value). What cig-compile lacks that LuaJIT's FOLD
  adds: a rule table keyed on operand opcodes, load forwarding tied to it,
  and CSE on every emit.
- **The experiment.** Not measured yet: build cig-compile for the loop-heavy
  `bench/compute/` programs under a rule-table fold; count surviving CIR
  instructions.
- **Pinned tag and files read.** LuaJIT `v2.1` at `63c8f1a`: `src/lj_ir.h`
  (`IRDEF`, `IRIns`, `TRef`), `src/lj_ir.c`, `src/lj_iropt.h`,
  `src/lj_opt_fold.c` (`lj_opt_fold`, `lj_opt_cse`), `src/lj_opt_dce.c`.
  cig: cig-compile `src/cir/pass/fold_consts.rs: FoldConsts`,
  `src/cir/pass/sccp.rs: Sccp`, `src/cir/ir.rs: fold_binary`,
  `src/cir/pass/mod.rs`.

### B3. Loop optimization by copy-substitution

- **What LuaJIT does, and why.** `lj_opt_loop.c` opens with the design:
  loop-invariant code motion is useless on an IR of guards, because the
  first non-hoistable guard blocks everything after it. `lj_opt_loop`
  re-emits the recorded stream through the optimizer with a substitution
  table mapping each original reference to the one the second emit returns
  (`loop_unroll`). The result is two sections separated by the `LOOP`
  instruction: a pre-roll that holds invariant and variant instructions and
  performs one iteration, with its guards and effects, then the loop body,
  which holds only variant work. CSE, FOLD and forwarding then hoist
  invariants to the pre-roll. `loop_emit_phi` adds PHIs for loop-carried
  values.
- **Adapt.** CIR is structured (`src/cir/ir.rs`: `Kind::If`, `Kind::Loop`
  and `Kind::ForEach` hold nested `Block`s; variables are slots): no branch,
  no guard, no PHI. Today `src/bc/mod.rs: lower` hoists a constant read in a
  loop above the outermost loop (`hoist`), aliases a slot read to its
  register when nothing writes between (`alias`), and skips loading a
  constant every reader takes as a `K` form (`unloaded`). Copy-substitution
  would add a first copy of one full turn, taking pure work and type guards
  out of the body. That copy clones the turn's pinned instructions (effects,
  step marks, raising operations), which rule T3 of cig-compile's passes
  forbids (`src/cir/pass/mod.rs`), so it is no pass over CIR: proposed below
  it, in stage C's lowering, held to B7's rule. In the body, the first two
  keep their place and order; a raising operation leaves only where the
  first copy proves it cannot raise (T4).
- **The experiment.** Not measured yet: one `for` loop in
  `bench/compute/loop.cig`, compiled with and without the first copy,
  against that loop's turn on the VM today, 352 machine instructions
  (`w30-loop-v2`).
- **Pinned tag and files read.** LuaJIT `v2.1` at `63c8f1a`:
  `src/lj_opt_loop.c`, `src/lj_ir.h`. cig: cig-compile `src/cir/ir.rs`,
  `src/cir/pass/mod.rs`, `src/cir/verify.rs`, `src/bc/mod.rs`.

### B4. Narrowing (keep doubles unless integers pay)

- **What LuaJIT does, and why.** The opening comment of `lj_opt_narrow.c`
  gives the rationale: narrowing does not pay in the interpreter (the
  integer and floating-point units run in parallel); compiled code gains
  from it, but narrowing every load eagerly loses negative zero and puts an
  overflow check, a control dependency, on every integer operation. So
  LuaJIT keeps doubles and narrows only where it pays. `lj_opt_narrow_forl`
  picks an integer type for `FORL` when the recorded start, stop and step
  are integers and the result fits int. `lj_opt_narrow_arith` emits `ADDOV`
  and `SUBOV` in the default build only when both inputs are integer at
  record time and the result fits int; it does not narrow `MUL` (the comment
  says it would lose signed zero). `narrow_stripov` strips
  `ADDOV`/`SUBOV`/`MULOV` to the wrapping `ADD`/`SUB`/`MUL` where the
  consumer is an index, a bit op or a `TOBIT` (the value is used modulo two
  to the thirty-two).
- **Adapt.** cig's integers are `Value::Int` (cig-script `src/value.rs`);
  overflow raises `E503`. cig-compile's rule T4 (`src/cir/pass/mod.rs`) says
  an op that may raise goes only where the verifier proves it cannot. Today
  cig-compile has no overflow-strip pass. Proposed, for stage C: strip
  `ADD`, `SUB`, `MUL`, `ADDK`, `SUBK`, `MULK`
  (`crates/cig-bytecode/src/opcode.rs: Op`) only where a static proof holds
  (an induction variable with a verifier-known bound, an index already
  checked against a list's length in the same region); otherwise the check
  stays. A float that is provably whole: cig has no opcode that narrows one
  and no stage-C proof covers it; it stays a `Value::Float`.
- **The experiment.** Not measured yet: for `bench/compute/loop.cig` and
  `mandel.cig`, count how many arithmetic sites a stage-C proof would reach
  against how many keep their check.
- **Pinned tag and files read.** LuaJIT `v2.1` at `63c8f1a`:
  `src/lj_opt_narrow.c` (`narrow_stripov`, `lj_opt_narrow_arith`,
  `lj_opt_narrow_forl`), `src/lj_ir.h` (`ADDOV`, `SUBOV`, `MULOV`). cig:
  cig-script `src/value.rs`, `crates/cig-bytecode/src/opcode.rs: Op`;
  cig-compile `src/cir/pass/mod.rs`.

### B5. Memory optimizations (alias analysis, load forwarding, dead-store elimination)

- **What LuaJIT does, and why.** `lj_opt_mem.c` carries three analyses.
  Alias analysis (`aa_table`, `aa_ahref`, `aa_uref`, `aa_fref`, `aa_xref`)
  returns `ALIAS_NO`, `ALIAS_MAY` or `ALIAS_MUST` by walking back to each
  reference's origin. Load forwarding (`lj_opt_fwd_aload`,
  `lj_opt_fwd_hload`, `lj_opt_fwd_uload`, `lj_opt_fwd_fload`) feeds a
  must-alias store's value forward. Dead-store elimination
  (`lj_opt_dse_ahstore`, `lj_opt_dse_ustore`, `lj_opt_dse_fstore`) removes a
  store overwritten before the trace ends.
- **Adapt.** cig-compile `src/bc/mod.rs: lower` already aliases a slot read
  to its register (`alias`) and skips loading a constant every reader takes
  as `K` (`unloaded`), so one kind of load-forwarding is live. What it does
  not do: forward across list, map or cell stores, remove a dead store, or
  reassociate indices. `Rc::strong_count` and `Rc::get_mut` are run-time
  facts: an ahead-of-time compiler cannot use them to prove non-aliasing.
  Proposed, for stage C: a load-forwarding pass. A forwarded load dies at
  any store that may alias it (`SETFIELD`, `SETINDEX`, `APPENDLIST`,
  `INSERTMAP`, a cell's write), at every call (a builtin marked `Pure` may
  change its receiver in place, and a closure may hold the list) and at
  every burn (B7); with the shapes of #243's Part 2, a field read in a loop
  becomes one load; a uniqueness check is used only as a run-time guard,
  with a snapshot before it (B7), and does not cover a value also reachable
  through a `Cell` captured by a closure.
- **The experiment.** Not measured yet: how many list, map and field loads
  in `bench/compute/*.cig` a forwarding pass with this kill set would
  remove. What a load costs on the VM today (`w32-ljcount-j1`, interpreters,
  in machine instructions): a field read 88 against 21 on `luajit -joff` and
  38 on Lua 5.4; a map read 97, 29, 55.
- **Pinned tag and files read.** LuaJIT `v2.1` at `63c8f1a`:
  `src/lj_opt_mem.c`, `src/lj_ir.h`. cig: cig-script `src/value.rs`,
  `src/vm/mod.rs`, `src/stdlib/methods.rs`; cig-compile `src/bc/mod.rs`.

### B6. Allocation sinking (an object that never escapes is never allocated)

- **What LuaJIT does, and why.** `lj_opt_sink.c` has three phases driven by
  `lj_opt_sink`. `sink_checkalloc` admits a `TNEW`, `TDUP` or `CNEW` whose
  stores have constant keys. `sink_mark_ins` walks backwards and marks every
  ineligible allocation: roots are each remaining typed load, each store
  with a non-constant key, each stored value, each guard, each call that may
  store, and `TBAR`. `sink_mark_snap` adds the last snapshot's references as
  roots when the trace is not a loop; a marked reference is kept (not sunk).
  `sink_sweep_ins` tags the unmarked ones as sunk (`RID_SINK`). On exit,
  `snap_unsink` rebuilds it from its sunk stores.
- **Adapt.** cig-compile has no sinking pass today. Proposed, for stage C: a
  sinking pass over `Op::NEWLIST` and `Op::NEWMAP` (cig-script
  `src/vm/mod.rs`), for an allocation that never escapes. It escapes when
  its value is stored anywhere but a register that carries no cell
  (`INSERTMAP`, `APPENDLIST`, `SETFIELD`, `SETINDEX`, `CELLSET`, `SETUP`),
  passed to any call (`CALL`, `TAILCALL`, `CALLB`, `CALLM`), returned
  (`RET`), thrown (`THROW`), captured (`CLOSURE`, `CELLNEW`), reaches a
  burn, its compensation (`COMP_ENTER` keeps the state map for the journal)
  or a chain (`BURN_OP`, `CHAIN_MAKE`), or is read in a way its sunk stores
  cannot answer (`GETINDEX` with a key that is not a constant, `ITERPREP`,
  `INTERP`, `EQ`, `LT`). It is the stronger form of Part 2's escape analysis
  (#243). No cig `Value` has a `Drop` a program can observe
  (`docs/BYTECODE.md`); a sunk allocation saves its heap and reference-count
  work. At exit, the snapshot rebuilds it into the register it would have
  held.
- **The experiment.** Not measured yet: count `Op::NEWLIST` and `Op::NEWMAP`
  sites in `bench/compute/*.cig` that never escape (today a list literal is
  614 and a map literal 2,157 on the VM, against `luajit -joff` 450 and 754,
  `w32-ljcount-j1`).
- **Pinned tag and files read.** LuaJIT `v2.1` at `63c8f1a`:
  `src/lj_opt_sink.c`, `src/lj_snap.c`, `src/lj_ir.h`. cig: cig-script
  `src/value.rs`, `src/vm/mod.rs`, `docs/BYTECODE.md`.

### B7. Snapshots and exits (the piece cig must get exactly right)

- **What LuaJIT does, and why.** A `SnapShot` in `lj_jit.h` is 12 bytes. The
  recorder sets `J->needsnap` **after** each store, **after** every
  side-effecting call, at a return, and on a loop; `lj_snap_add` takes the
  snapshot at the next bytecode. A guard exits to the latest snapshot before
  it, and `lj_snap_restore` rebuilds the stack.
- **Adapt.** After any exit, the VM and compiled code agree on output,
  errors, positions, the journal and the step budget. **Burns.** Compiled
  code never moves, merges or skips a burn: `BURN_ENTER`, `BURN_OP`,
  `BURN_LEAVE`, `COMP_ENTER`, `COMP_LEAVE`, and a `RET` inside a burn (which
  it closes), are calls into the one kernel, and no value is forwarded, no
  guard hoisted and no allocation sunk across one. **Snapshots.** One before
  every guard, every call and every `THROW`, and one after every instruction
  that does more than write registers carrying no cell: a store into a list,
  a map or a cell (a write to a register that carries a cell is one); every
  call (`CALL`, `TAILCALL`, `CALLB`, `CALLM`: a builtin marked `Pure` may
  change its receiver in place, and one that calls back runs script code);
  `EXHALE`, `TRY_ENTER`, `TRY_LEAVE`, `PACK`, `CHAIN_MAKE`, and those above.
  No exit runs an effect twice. **State.** At an exit the VM finds what it
  would have left: the machine's `stack`, `cells`, `upvals`, `frames` (each
  `Frame` whole: `nest`, `held_nest` and `held_calls` decide `E515`'s
  wording and `E506`'s depth), `tries` (every `Open` whole) and `burns`, and
  the interpreter's `loop_depth`, `call_depth`, `burn_depth` and
  `unlit_depth`. Never restored, and in no snapshot: the step count and the
  journal (they only grow, and compiled code keeps both exact; the issue's
  row lists them), and `cough_payload` and `exit_requested` (only a call or
  the VM writes them, so what a failed call left stays). **Steps and
  errors.** `STEPS` is charged where the VM charges it, one tick at a time,
  never in bulk, never later (a later charge lets an error or a burn come
  first). Where an instruction would raise, compiled code exits at the
  snapshot before it and the VM raises; where a call returns an error, it
  exits at the snapshot before that call, carrying the error, and the VM
  raises it there without calling again. The offer to the tries, the closing
  of burns, `exit()` and `finally` are the VM's.
- **The experiment.** Not measured yet: a cig snapshot's width against
  LuaJIT's 12 bytes.
- **Pinned tag and files read.** LuaJIT `v2.1` at `63c8f1a`:
  `src/lj_snap.c`, `src/lj_record.c`, `src/lj_ir.c`, `src/lj_jit.h`. cig:
  cig-script `src/vm/mod.rs`, `src/interp/mod.rs`, `src/value.rs`,
  `src/stdlib/chain.rs`, `docs/BYTECODE.md`, `docs/BURN.md`.

### B8. Side traces, stitching and blacklisting

- **What LuaJIT does, and why.** A trace ends with a link from `TraceLink`
  in `lj_jit.h` (loop, root, tail-recursion, up-recursion, down-recursion,
  interpreter, return, stitch). `lj_record_stop` tags the link; `trace_stop`
  patches the start bytecode and, for a call-form stitch, writes the link
  into the previous trace. Side traces grow from hot exits: every `SnapShot`
  carries a count byte; `trace_hotside` bumps it and, past `JIT_P_hotexit`,
  starts a side trace; when it finishes, the parent's exit is patched to
  jump into it (`lj_asm_patchexit`). Blacklisting: `blacklist_pc` rewrites
  `BC_LOOP` to `BC_ILOOP` and kin. `penalty_pc` counters each abort in a
  round-robin `HotPenalty` cache; on repeat the penalty doubles plus jitter;
  once it exceeds the maximum, `blacklist_pc` runs.
- **Adapt.** Anything stage C cannot compile stays on the cig-script VM
  (`src/vm/mod.rs`: `run_fast` where the function's registers carry no cell,
  the general path otherwise). Proposed: the build names each region left on
  the VM and why. Today cig-compile has no partial result: a program lowers
  completely or is refused (`Error::Refused` in `src/bc/mod.rs`). What would
  fall back: a branch or a try handler the record never took; a path T4
  cannot clear. A burn is not a fallback: compiled code calls the kernel
  (B7). Stitching has no counterpart. Inlining a call must not change
  `E506`'s depth or `E515`'s wording: stage C either keeps the call or holds
  the depth count in the snapshot (B7); an inlined function never meets the
  VM's first-call check (`E1050`), so the build verifies it. Blacklisting
  does not transfer: an ahead-of-time retry sees the same input and fails
  the same way.
- **The experiment.** Not measured yet: for the 25 compute programs in
  `bench/compute/`, count the exits a compiled path would take back to the
  VM.
- **Pinned tag and files read.** LuaJIT `v2.1` at `63c8f1a`:
  `src/lj_trace.c`, `src/lj_record.c`, `src/lj_jit.h`. cig: cig-script
  `src/vm/mod.rs`, `docs/BYTECODE.md`; cig-compile `src/bc/mod.rs`.

### B9. The back end: a hand-written assembler, with register allocation done backwards

- **What LuaJIT does, and why.** `lj_asm.c: lj_asm_trace` reserves memory
  through `lj_mcode_reserve`, then walks the IR from the last reference to
  the first through `asm_ir`. The allocator rides the same walk:
  `ra_alloc1`, `ra_evict`, `ra_restore`, `ra_spill`, and `ra_rematk`, which
  re-emits a constant load at each use rather than spilling. A guard is a
  compare plus `asm_guardcc`: one conditional jump to its snapshot's exit
  stub; only the guard just before the loop's closing jump is inverted, so
  that the conditional jump closes the loop. `asm_exitstub_setup` and
  `asm_exitstub_gen` in `lj_asm_x86.h` lay groups of `EXITSTUBS_PER_GROUP`
  stubs at the bottom of the reserved area: each pushes its exit number's
  low byte; the group's tail pushes the high byte and jumps to
  `lj_vm_exit_handler`. Loops close through `asm_loop` and the `asm_phi_*`
  family; x86 operands fold through `asm_fuseload`. One pass, no separate
  liveness, no spill where a value can be re-emitted.
- **Reject.** Not taken: stage C is planned on Cranelift and its register
  allocator, ahead of time (cig-compile's `docs/decisions/0001-backend.md`:
  a plan, "decided again by measurement"). The rule is the issue's: no
  memory both written and executed at run time. `lj_mcode.c` works that way:
  an area is mapped writable, flipped to executable when a trace is
  committed (`lj_mcode_commit`) and flipped back to be patched
  (`lj_mcode_patch`); with `LUAJIT_SECURITY_MCODE` off it stays
  `MCPROT_RWX`. What stage C takes is the shape of a guard: a compare and
  one branch in the path, the exit's code out of line and shared, the state
  rebuilt from the snapshot by one routine (B7).
- **The experiment.** Not measured yet: for a guard-heavy path in
  `bench/compute/nbody.cig`, fast-path instructions and exit-stub bytes
  LuaJIT's `-jdump` shows against a Cranelift build of the same CIR.
- **Pinned tag and files read.** LuaJIT `v2.1` at `63c8f1a`: `src/lj_asm.c`,
  `src/lj_asm_x86.h`, `src/lj_emit_x86.h`, `src/lj_target.h`,
  `src/lj_target_x86.h`, `src/lj_mcode.c`. Write-ups:
  `papers/luajit/pall-2009-11-02-luajit-2.0-ip-disclosure.html`.
  cig-compile: `docs/decisions/0001-backend.md`.

## What cig does not take directly

| LuaJIT | why not | cig's form | where |
|---|---|---|---|
| the interpreter written in assembly | no `unsafe` in the VM, and one VM for every target | Rust handlers laid out by evidence, and a profile-guided build | A3 |
| NaN-tagged values | needs `unsafe` | the tagged value, 24 bytes today, 16 with `lua.md` entry 5 | A5 |
| the incremental garbage collector | rollback stands on deterministic cleanup | reference counting | the entry below |
| the FFI | a native call goes around the kernel | rejected | the entry below |
| a JIT at run time, by default | no memory both written and executed at run time | compiled ahead of time, from a profile; a tier at run time is a later, separate decision | B1, B9 |

### The incremental garbage collector (rejected)

- **What LuaJIT does, and why.** `src/lj_gc.c` implements an incremental
  tri-color mark-and-sweep. The six states are `GCSpause`, `GCSpropagate`,
  `GCSatomic`, `GCSsweepstring`, `GCSsweep`, `GCSfinalize`. Each object's
  `marked` byte carries the `LJ_GC_WHITE0`, `LJ_GC_WHITE1`, `LJ_GC_BLACK`,
  `LJ_GC_FINALIZED`, `LJ_GC_WEAKKEY`, `LJ_GC_WEAKVAL`, `LJ_GC_FIXED` or
  `LJ_GC_SFIXED` bits. `lj_gc_step` runs `gc_onestep` under a
  `stepmul`-scaled budget derived from `GCSTEPSIZE`. Write barriers:
  `lj_gc_barrierf` (forward), `lj_gc_barrierback` (backward, for tables
  going black to gray through `grayagain`), `lj_gc_barrieruv`
  (closed-upvalue specialization), and `lj_gc_barriertrace` (JIT);
  `lj_gc_closeuv` closes an upvalue and applies a barrier on the way.
  `src/lj_alloc.c` bundles a customized dlmalloc: inside `#if LJ_64`,
  `LJ_ALLOC_MBITS` is 31 on x64 without `LJ_GC64`, 32 on other 64-bit
  architectures, 47 under `LJ_GC64`.
- **Reject.** cig's verdict follows `docs/research/lua.md` entry 11 ("the
  collector, not a count, keeps objects alive ... burns, rollback and passes
  rely on cleanup at a known point") and the rollback contract in
  `docs/BURN.md`: the journal is a write-ahead log (written before each op);
  rollback replays entries newest first; automatic rollback on an uncaught
  error is the default. cig's form: reference counting (`Rc` in every heap
  `Value`, not atomic: `src/value.rs`), with the count's traffic to be
  elided at a value's last use (Perceus, in #243's Part 2: proposed, and
  cig-compile has no such pass) and B6 for allocations in compiled code.
- **The experiment.** Not measured yet: the count's traffic (clones and
  drops) on `fib`, `trees`, `closures` and `strings`.
- **Pinned tag and files read.** LuaJIT `v2.1` at `63c8f1a`: `src/lj_gc.c`,
  `src/lj_gc.h`, `src/lj_alloc.c` (`LJ_ALLOC_MBITS`), `src/lj_alloc.h`.
  Write-ups: `papers/luajit/wiki-optimizations.html`,
  `papers/luajit/wiki-allocation-sinking-optimization.html`. cig:
  `docs/research/lua.md` entry 11; `src/value.rs`; `crates/cig-bytecode/`.

### The FFI: calling C directly from the script (`lib_ffi.c`, `lj_ccall.c`, `lj_crecord.c`)

- **What LuaJIT does, and why.** `lib_ffi.c` exposes `ffi.cdef`, `ffi.new`,
  `ffi.cast`, `ffi.typeof`, `ffi.C`, `ffi.load`, `ffi.string`, `ffi.copy`,
  `ffi.errno`, each going through `ffi_checkctype` into the pieces beside
  it: `lj_cparse.c` is a minimal C declaration parser (not validating, per
  its own header), `lj_ctype.c` manages types (`CTTDDEF` seeds the stdint
  aliases), `lj_cdata.c` makes C data as script objects, `lj_cconv.c`
  converts, `lj_ccall.c` marshals arguments through `ccall_set_args` per ABI
  and enters the assembly symbol `lj_vm_ffi_call`, `lj_ccallback.c` writes
  trampolines into an executable page (`CALLBACK_MCODE_SIZE`, see B9),
  `lj_clib.c` loads shared libraries through `dlopen` or `LJ_WIN_LOADLIBA`
  and `GetProcAddress` behind `CLIB_DEFHANDLE`, `lj_carith.c` carries C-data
  metamethods through `lj_carith_op`, and `lj_crecord.c` records every
  operation into IR. Zero-copy access to host structs and a direct call to
  any native function.
- **Reject (recorded).** In cig's documents: the kernel is the one piece of
  code allowed to touch the world, a burn is an effect it owns, the journal
  a write-ahead log replayed newest first on rollback (`docs/BURN.md`). A
  hop is a program run as another process; the kernel "performed nothing,
  saw nothing and can undo nothing on its own" (`docs/HOPS.md`), and watches
  the pack around it. Containment is the opt-in `box` option on a hop, a
  copy-on-write layer in a user namespace (`docs/HOPS.md`). A native call
  through `ffi.load` is neither: inside the script's own process, no process
  boundary, no journal entry, code from outside the verified module. The
  callback page in `lj_ccallback.c` is memory written then executed (see
  B9). cig's form: a hop through `proc.run`, and in-tree modules under
  `src/stdlib/` (`core.rs`, `csv.rs`, `fs.rs`, `hash.rs`, `json.rs`,
  `math.rs`, `text.rs`, `proc.rs`).
- **The experiment.** None: the rejection rests on a rule, and no figure
  would change it.
- **Pinned tag and files read.** LuaJIT `v2.1` at `63c8f1a`:
  `src/lib_ffi.c`, `src/lj_cparse.c`, `src/lj_ctype.c`, `src/lj_cdata.c`,
  `src/lj_cconv.c`, `src/lj_ccall.c`, `src/lj_ccallback.c`, `src/lj_clib.c`,
  `src/lj_carith.c`, `src/lj_crecord.c`. Write-ups: `doc/ext_ffi.html`.
  cig-script: `docs/BURN.md`, `docs/HOPS.md`, `src/stdlib/proc.rs`.

## Every file in `src/`

One entry for each of the 194 files of `src/` at the pin (`src/host/` and
`src/jit/` included), grouped by what the files are for. Each says what the
file is, by the names that find it, and gives cig's verdict: **take** (cig
does this the same way, or will), **adapt** (cig takes the idea in another
form), **reject** (cig will not, by a rule it names), or **not applicable**
(no design in the file that cig could take or refuse: a header that only
declares, a wrapper, build glue). A file that does one machine's share of
another file's job says which, and what that machine's version does
differently. Line counts are the file's at the pin.

### The bytecode, the front end and values

**Public headers**

#### `lauxlib.h` (161 lines)

- **What it is.** The LuaJIT 2.1 copy of the Lua 5.1 "auxiliary library"
  header: `luaL_Reg`, `luaL_openlib`, `luaL_register`, the argument checkers
  (`luaL_checklstring`, `luaL_checknumber`, `luaL_optnumber`), `luaL_ref`,
  `luaL_unref`, and the streaming string builder (`luaL_Buffer`,
  `luaL_buffinit`, `luaL_addchar`, `luaL_addlstring`, `luaL_addvalue`,
  `luaL_pushresult`).
- **For cig: not applicable.** cig has no embedding C API; the nearest
  surface is the `Builtin` table each module registers under `src/stdlib/`.

#### `lua.h` (402 lines)

- **What it is.** The public Lua C API header, held at the Lua 5.1 shape so
  LuaJIT is binary-compatible with 5.1 embedders: the thread status codes,
  the type tags, the `lua_CFunction`, `lua_Reader`, `lua_Writer`,
  `lua_Alloc` callback types, every stack push and read entry
  (`lua_pushnil`, `lua_pushnumber`, `lua_pushstring`, `lua_tonumber`,
  `lua_tolstring`), and the debug surface (`lua_getstack`, `lua_getinfo`,
  `lua_sethook`).
- **For cig: not applicable.** Public ABI for a C embedding cig does not
  have.

#### `lua.hpp` (9 lines)

- **What it is.** The C++ include wrapper: an `extern "C"` block that pulls
  in `lua.h`, `lauxlib.h`, `lualib.h` and the generated `luajit.h`.
- **For cig: not applicable.**

#### `luaconf.h` (156 lines)

- **What it is.** Embedder-facing build knobs for the C interpreter: default
  package search paths, path and directory separators, the `lua_Number`
  alias.
- **For cig: not applicable.** Build-time configuration for a C consumer.

#### `luajit_rolling.h` (80 lines)

- **What it is.** The LuaJIT-specific public header for the rolling branch:
  the version string (`LUAJIT_VERSION`, `LUAJIT_VERSION_NUM`,
  `LUAJIT_VERSION_SYM`), the modes passed to `luaJIT_setmode`
  (`LUAJIT_MODE_ENGINE`, `LUAJIT_MODE_DEBUG`, `LUAJIT_MODE_FUNC`,
  `LUAJIT_MODE_ALLFUNC`, `LUAJIT_MODE_ALLSUBFUNC`, `LUAJIT_MODE_TRACE`,
  `LUAJIT_MODE_WRAPCFUNC`), and the sampling-profiler hooks
  (`luaJIT_profile_start`, `luaJIT_profile_stop`).
- **For cig: not applicable.** `luajit_rolling.h` is the source of the
  generated `luajit.h` shipped to embedders.

#### `lualib.h` (44 lines)

- **What it is.** The names of the standard libraries (`LUA_COLIBNAME`,
  `LUA_MATHLIBNAME`, `LUA_STRLIBNAME`, `LUA_TABLIBNAME`, `LUA_IOLIBNAME`,
  `LUA_OSLIBNAME`, `LUA_LOADLIBNAME`, `LUA_DBLIBNAME`, `LUA_BITLIBNAME`,
  `LUA_JITLIBNAME`, `LUA_FFILIBNAME`) and their `luaopen_*` entries.
- **For cig: not applicable.**

**Bytecode format, reader and writer**

#### `lj_bc.h` (279 lines)

- **What it is.** The instruction format (A1), the field readers (`bc_op`,
  `bc_a`, `bc_b`, `bc_c`, `bc_d`, `bc_j`), the composers (`BCINS_ABC`,
  `BCINS_AD`, `BCINS_AJ`), the opcode table (`BCDEF`), the opcode enum, the
  mode enum (`BCMode`), the mode readers (`bcmode_a`, `bcmode_b`,
  `bcmode_c`, `bcmode_mm`), the four-slot numeric-for layout, and a ladder
  of `LJ_STATIC_ASSERT` invariants (`ORDER OPR`, `ORDER ARITH`, pairs of
  `BC_ISEQV` and `BC_ISNEV` opposites).
- **For cig: adapt.** cig's layout is ABC, ABx, AsBx with a `WIDE` prefix
  (`docs/BYTECODE.md`, `crates/cig-bytecode/src/opcode.rs`). Design point A1
  holds the open lowering items.

#### `lj_bc.c` (14 lines)

- **What it is.** Two generated tables the format in `lj_bc.h` derives from
  its `BCDEF` macro: the per-opcode operand-mode table and the per-opcode
  handler-offset table, filled from the dispatch build and read elsewhere.
- **For cig: adapt.** cig has the same split:
  `crates/cig-bytecode/src/opcode.rs` holds the opcode enum and its operand
  mode per column.

#### `lj_bcdump.h` (71 lines)

- **What it is.** The on-disk bytecode-dump format for `luajit -b`: header
  bytes (`BCDUMP_HEAD1`, `BCDUMP_HEAD2`, `BCDUMP_HEAD3`), dump version
  (`BCDUMP_VERSION`), flags (`BCDUMP_F_BE`, `BCDUMP_F_STRIP`,
  `BCDUMP_F_FFI`, `BCDUMP_F_FR2`, `BCDUMP_F_KNOWN`), and the constant-pool
  tag space the reader walks.
- **For cig: reject.** cig's module layout 2
  (`crates/cig-bytecode/src/serialize.rs`, `docs/BYTECODE.md`) is a 40-byte
  header, a 32-byte head, a directory of fixed-width function entries
  (`ENTRY_LEN` 80), bodies, one string pool and a line region, with BLAKE3
  at the header's tail: not a length-delimited prototype stream.

#### `lj_bcread.c` (458 lines)

- **What it is.** The reader for a `luajit -b` dump: `bcread_header`,
  `lj_bcread_proto`, `bcread_bytecode`, `bcread_kgc`, `bcread_knum`,
  `bcread_ktab`, `lj_bcread`. Errors: `LJ_ERR_BCFMT`, `LJ_ERR_BCBAD`.
- **For cig: reject.** The loading model checks only the header (magic,
  version, flags, `LJ_FR2`) and each prototype's length; it does not check
  that opcodes are defined, operands in bounds, jump targets on an
  instruction, or registers written before read. The first instruction of
  each prototype is written by the reader itself (`BC_FUNCF`, `BC_FUNCV`, or
  `BC_NOT` for a non-native dump). cig's form is the verifier
  (`crates/cig-bytecode/src/verify.rs`, `docs/BYTECODE.md`).

#### `lj_bcwrite.c` (454 lines)

- **What it is.** The writer for the same dump format. `lj_bcwrite` is the
  entry; `bcwrite_proto` writes the header, bytecode, upvalue refs,
  constants and debug into an `SBuf` and calls a `lua_Writer`.
  `bcwrite_bytecode` omits the first (header) word of each prototype and
  unpatches interpreter-only forms to their generic opcodes.
- **For cig: reject.** cig has no interpreter-only header opcode to strip
  (`crates/cig-bytecode/src/opcode.rs`'s 73 opcodes carry no `BC_FUNCF`
  counterpart). `crates/cig-bytecode/src/serialize.rs: serialize_module`
  writes layout 2 in one pass, not a length-delimited prototype stream.

**Lexer, parser and bytecode compiler**

#### `lj_lex.h` (105 lines)

- **What it is.** The token table (`TKDEF`, the reserved words and the
  multi-character symbols), the virtual tokens the lexer emits, the
  `LexChar` and `LexToken` typedefs, the `BCInsLine` pair the parser uses to
  tie each emitted instruction to a source line, and the `LexState`
  definition (the current and the lookahead token).
- **For cig: not applicable.** cig's tokens are Rust enums in `src/syntax/`.

#### `lj_lex.c` (611 lines)

- **What it is.** The lexer. Entries `lj_lex_setup`, `lj_lex_cleanup`,
  `lj_lex_next`. The number path is `lex_number`; strings are `lex_string`
  and `lex_longstring` (the equals-bracketed form, with `lex_skipeq`);
  newlines are `lex_newline`; the main loop `lex_scan` also carries the
  identifier path (the reserved-word check reads the per-state `reserved`
  byte on the interned name).
- **For cig: adapt.** cig's lexer in `src/syntax/` is Rust-native. What
  carries over is the one-input one-output loop and named tokens. cig does
  not take LuaJIT's `const` soft-keyword path.

#### `lj_parse.h` (18 lines)

- **What it is.** Exports of the parser: `lj_parse` (source to prototype),
  `lj_parse_keepstr` (intern a string so the parser keeps it as a key),
  `lj_parse_keepcdata` (the FFI companion).
- **For cig: not applicable.** Mirror of `lj_parse.c`.

#### `lj_parse.c` (3136 lines)

- **What it is.** The front-end: lex, parse and emit bytecode in one pass,
  no intermediate IR. Expression descriptors (`VKNUM`, `VKSTR`, `VNONRELOC`,
  `VLOCAL`, `VUPVAL`, `VINDEXED`, `VCALL`) carry a value or a slot, and each
  emit picks a form from the descriptor (`bcemit_arith`, `bcemit_comp`,
  `bcemit_store`, `bcemit_AD`, `bcemit_AJ`, `bcemit_ABC`). Constants pass
  through `const_num`, `const_gc`, `const_str`. Forward branches are patched
  at the site (`jmp_patch`). `parse_for_num` emits the numeric `for`;
  `parse_for_iter` the generic one, with `predict_next` picking `BC_ISNEXT`.
  Calls are emitted in `parse_args` as `BC_CALL` or `BC_CALLM`; the
  tail-call rewrite is in `parse_return`.
- **For cig: adapt.** cig lowers in two passes (`cig-compile src/cir/` the
  verified IR; `src/bc/mod.rs` the lowering). The emit choices carry over:
  pooled constants, form from both sides of a binary operator, jump patching
  through the `patches` list in `cig-compile src/bc/mod.rs`.

**Prototypes, closures and state**

#### `lj_func.h` (24 lines)

- **What it is.** The exports: `lj_func_freeproto`, `lj_func_closeuv` (close
  open upvalues at or above a level), `lj_func_freeuv`, `lj_func_newC`,
  `lj_func_newL_empty`, `lj_func_newL_gc` (upvalue array filled from the
  parent), `lj_func_free`.
- **For cig: not applicable.** Mirror of `lj_func.c`.

#### `lj_func.c` (191 lines)

- **What it is.** Prototype and closure helpers. `lj_func_freeproto`
  releases a prototype. `func_finduv` inserts into the thread's `openupval`
  in stack-slot order; `func_emptyuv` allocates a closed-but-empty upvalue.
  `lj_func_closeuv` walks the list from the top down. `lj_func_newC` and
  `lj_func_newL_empty` allocate closures; `lj_func_newL_gc` fills a
  closure's upvalue array from the parent through the prototype's upvalue
  map.
- **For cig: reject.** The open-upvalue indirection does not fit; cig
  captures through a heap cell (`CELLNEW`, `CELLGET`, `CELLSET` in
  `crates/cig-bytecode/src/opcode.rs`; the `Cell` alias in `src/vm/mod.rs`),
  paid on capture only (`docs/research/lua.md` entry 4).

#### `lj_state.h` (38 lines)

- **What it is.** Exports of state: `lj_state_new`, `lj_state_growstack`,
  `lj_state_growstack1`, `lj_state_relimitstack`, `lj_state_shrinkstack`,
  `lj_state_free`, the inlined `lj_state_checkstack`, and the
  `savestack`/`restorestack` macros that index the stack through its base so
  a resize that moves the stack keeps every open reference valid.
- **For cig: adapt.** The idiom "save an offset, not a pointer" is the one
  cig uses: `src/vm/mod.rs: Frame` fields such as `tries` and `burns` are
  indices into side lists, and `base` is an index into the register stack.

#### `lj_state.c` (397 lines)

- **What it is.** The thread lifecycle. `resizestack` reallocates the value
  stack and rebases `base`, `top`, `jit_base` and every open upvalue;
  `lj_state_growstack` grows; `lj_state_shrinkstack` reclaims. `stack_init`
  first-allocates. `lj_state_new` creates a child thread sharing the
  `global_State`. `cpluaopen` bootstraps; `close_state`, `cpfinalize`,
  `lj_state_free` tear down.
- **For cig: adapt.** cig's VM has one register stack per machine
  (`src/vm/mod.rs: Machine` with a `stack` field) and a frame `base` that is
  an index, so growing the stack does no pointer rebasing. A child thread is
  work for the shared-stack half of `docs/research/lua.md` entry 18.

**Values and objects**

#### `lj_obj.h` (1109 lines)

- **What it is.** The central header: `TValue` (A5), whose `ftsz` member is
  gated on `LJ_FR2`; the unconditional `FrameLink` union; the GC header and
  every GC type (`GCstr`, `GCproto`, `GCupval`, `GCfunc`, `GCtab`,
  `GCudata`); `Node`; accessors and setters (`itype`, `tvis*`, `set*V`,
  `copyTV`); `nomm`; `lua_State` and `global_State`; number conversions
  (`lj_num2int`, `lj_num2i64`, `lj_num2u64`, `lj_num2bit`).
- **For cig: adapt (see A5, A7).** `nomm` is a per-table bitmask of fast
  metamethods known absent; cig has no metamethods, so no direct carry-over.
  Dispatch tables live in `lj_dispatch.h`'s `GG_State`.

#### `lj_obj.c` (51 lines)

- **What it is.** Two tables and two helpers: `lj_obj_typename` (external
  type names for error messages and the C API), `lj_obj_itypename` (internal
  type names indexed by the tag), `lj_obj_equal` (shared raw equality, used
  by `lj_tab.c` and the embedding surface), `lj_obj_ptr` (canonical pointer
  for a tagged value).
- **For cig: adapt.** cig's `Value::type_name` in `src/value.rs` does the
  same job in one `match`.

**Errors, debug, state**

#### `lj_err.h` (59 lines)

- **What it is.** The `ErrMsg` enum (built from `lj_errmsg.h` with an
  `ERRDEF` macro), the `lj_err_allmsg` string table, and the exported throw
  and format entries: `lj_err_throw`, `lj_err_mem`, `lj_err_stkov`,
  `lj_err_run`, `lj_err_msg`, `lj_err_lex`, `lj_err_optype`,
  `lj_err_optype_call`, `lj_err_comp`, `lj_err_callermsg`, `lj_err_callerv`,
  `lj_err_caller`, `lj_err_arg`, `lj_err_argt`, `lj_err_argtype`,
  `lj_err_argv`, `lj_err_trace`.
- **For cig: not applicable.** Mirror of `lj_err.c`.

#### `lj_err.c` (1164 lines)

- **What it is.** The error machinery. The throw and format path
  (`lj_err_throw`, `lj_err_run`, `lj_err_stkov`, `lj_err_trace`,
  `lj_err_mem`, `lj_err_msg`, `lj_err_lex`, `lj_err_optype`, `lj_err_comp`,
  `lj_err_callermsg`, `lj_err_callerv`, `lj_err_arg`, `lj_err_argt`,
  `lj_err_argtype`, `lj_err_argv`). The unwind paths: a Windows SEH branch
  (`lj_err_unwind_win`), a DWARF external-unwind branch
  (`lj_err_unwind_dwarf`), an ARM EHABI branch (`lj_err_unwind_arm`).
  JIT-mcode registration helpers (`lj_err_register_mcode`,
  `lj_err_deregister_mcode`).
- **For cig: reject.** cig uses `Result` and the try/throw opcodes
  (`TRY_ENTER`, `TRY_LEAVE`, `THROW`) with diagnostics in Rust
  (`src/diagnostics.rs`). No external unwinder is needed beyond what Rust
  supplies.

#### `lj_errmsg.h` (206 lines)

- **What it is.** The master list of error-message templates, one `ERRDEF`
  per line, grouped by kind: basic (`ERRMEM`, `ERRERR`, `ERRCPP`),
  allocation and overflow (`STROV`, `UDATAOV`, `STKOV`, `STKOVM`, `TABOV`),
  table indexing (`NANIDX`, `NILIDX`, `NEXTIDX`), metamethod and comparison
  (`BADCALL`, `BADOPRT`, `BADOPRV`, `BADCMPT`, `BADCMPV`), bytecode-dump
  (`BCFMT`, `BCBAD`), and the FFI's.
- **For cig: adapt.** cig's errors are named codes paired with a template
  (`docs/ERRORS.md`, `src/errors.rs`) and a diagnostic schema
  (`docs/diagnostic.schema.json`). The pattern of one line per code, built
  once at compile time, is in use.

#### `lj_debug.h` (66 lines)

- **What it is.** The `lj_Debug` struct (the public debug info extended with
  `nparams` and `isvararg`), and the exported frame-walk and info entries:
  `lj_debug_frame`, `lj_debug_line`, `lj_debug_getinfo`, `lj_debug_pushloc`,
  `lj_debug_dumpstack`.
- **For cig: not applicable.** cig's debug surface is Rust
  (`src/diagnostics.rs: Diagnostic`).

#### `lj_debug.c` (710 lines)

- **What it is.** Debug and introspection. Frame walkers (`lj_debug_frame`,
  `lj_debug_pushloc`); the compressed line-info reader (`lj_debug_line`);
  variable and upvalue name lookup (`lj_debug_uvname`, `lj_debug_uvnamev`,
  `lj_debug_slotname`, `lj_debug_funcname`); the public entries
  `lua_getinfo`, `lua_getstack`, `lua_getlocal`, `lua_setlocal`; and the
  stack-dump helper `lj_debug_dumpstack`.
- **For cig: adapt.** cig's running call stack is `Machine::frames`
  (`src/vm/mod.rs`); diagnostics come through `Diagnostic`
  (`src/diagnostics.rs`) on top of the verifier's line and region rows. What
  carries over is the compressed line-info format (per-entry width chosen by
  the range of line numbers in the function).

**Loading, driver, build**

#### `lj_load.c` (185 lines)

- **What it is.** The three entries that turn source or bytecode into a
  prototype. `lua_load` reads through a `lua_Reader`, sniffs the first byte
  for the dump signature from `lj_bcdump.h`, and dispatches to `lj_bcread`
  or to `lj_parse`. `luaL_loadfile` and `luaL_loadfilex` open the file,
  decide the mode, and chain through. `lua_dump` round-trips to
  `lj_bcwrite`.
- **For cig: adapt.** cig has `src/syntax/` for text and the verifier for
  bytes. One `load` entry dispatching on the first byte is already cig's
  shape (`src/program.rs`, `crates/cig-bytecode/src/module.rs`).

#### `luajit.c` (601 lines)

- **What it is.** The `luajit` driver: argument parsing (`-b` for bytecode
  mode with its own `-l` sub-flag that lists; `-e` to run an expression;
  `-j` and `-O` for JIT control; `-l name` requires a library through
  `dolibrary`; `-i`, `-v`, `-V`, `-E`), `handle_script`, `handle_luainit`
  (reads `LUA_INIT`), `dotty` (the REPL), `dojitcmd` and `dojitopt`, signal
  handling, version banner.
- **For cig: adapt.** cig's driver is `src/main.rs`. One-shot `-e` carries
  over; the bytecode listing is `cigcompile build --emit bc` (role of
  LuaJIT's `-bl`). `-l` library-require and `-j`/`-O` have no counterpart.

#### `ljamalg.c` (91 lines)

- **What it is.** The amalgamated single-translation-unit build: one
  `include` per core and library C file in a pinned order, so the C compiler
  inlines across files.
- **For cig: not applicable.** Rust's compilation unit is a crate.

**Common definitions and assertion**

#### `lj_def.h` (394 lines)

- **What it is.** The common internal defines: fixed-width integer typedefs;
  the static-assert, likely and unlikely macros; function-attribute macros
  (`LJ_FUNC`, `LJ_FUNCA`, `LJ_DATA`, `LJ_AINLINE`, `LJ_NOINLINE`,
  `LJ_FASTCALL`, `LJ_NORET`); the memory-allocation caps (`LJ_MAX_MEM32`,
  `LJ_MAX_MEM64`, `LJ_MAX_ALLOC`, `LJ_MAX_STR`, `LJ_MAX_BUF`,
  `LJ_MAX_UDATA`, `LJ_MAX_STRTAB`, `LJ_MAX_HBITS`, `LJ_MAX_ABITS`); and the
  bit-twiddle helpers (`lj_bswap`, `lj_fls`, `lj_rol`).
- **For cig: not applicable.** C portability scaffolding Rust does not need.

#### `lj_assert.c` (28 lines)

- **What it is.** One function, `lj_assert_fail` (format a message to
  standard error and `abort`), defined entirely inside a guard on
  `LUA_USE_ASSERT` or `LUA_USE_APICHECK`; when neither is set the file
  compiles to nothing (no stub).
- **For cig: not applicable.** cig uses Rust's assertion machinery.

**Target and build knobs**

#### `lj_arch.h` (778 lines)

- **What it is.** Target and build-time switches: the architectures (x86,
  x64, ARM, ARM64, PPC, MIPS, MIPS64), the operating systems, the
  endianness, and the derived toggles that drive the interpreter and the JIT
  (`LJ_GC64`, `LJ_FR2`, `LJ_ARCH_NUMMODE`, `LJ_DUALNUM`, `LJ_HASJIT`,
  `LJ_HASFFI`, `LJ_HASBUFFER`, `LJ_ABI_SOFTFP`, `LJ_ABI_BRANCH_TRACK`,
  `LJ_ABI_SHADOW_STACK`).
- **For cig: adapt.** cig builds one VM per target via `Cargo.toml` features
  and `cfg` flags; what carries over from `lj_arch.h` is the
  matrix-of-switches shape (which knob is a cfg flag and which is a Cargo
  feature).

**The C API**

#### `lj_api.c` (1303 lines)

- **What it is.** The Lua 5.1 C API: stack (`lua_gettop`, `lua_settop`,
  `lua_insert`, `lua_remove`, `lua_pushvalue`), pushers (`lua_pushnil`,
  `lua_pushnumber`, `lua_pushstring`, `lua_pushcclosure`), converters
  (`lua_tonumber`, `lua_tolstring`, `lua_toboolean`, `lua_touserdata`), raw
  and keyed access (`lua_rawget`, `lua_rawset`, `lua_getfield`,
  `lua_setfield`), calls (`lua_call`, `lua_pcall`, `lua_cpcall`), and one
  trace flush (`lj_trace_flushall`, invoked by `lua_setmetatable`).
- **For cig: not applicable.** Embedders call `src/vm/mod.rs` and
  `src/interp/` directly from Rust with built-ins under `src/stdlib/`; a C
  embedding API for cig is not decided in `docs/research/lua.md` or in the
  issue. The issue's FFI reject targets language-level `lib_ffi.c`, not an
  embedding API.

### The interpreter proper: frames, dispatch, the VM's source and its build

#### `.gitignore` (9 lines)

- **What it is.** Git's ignore list for `src/`: the built `luajit` binary,
  the generated headers (`luajit.h`, `lj_bcdef.h`, `lj_ffdef.h`,
  `lj_libdef.h`, `lj_recdef.h`, `lj_folddef.h`) and `lj_vm.[sS]`.
- **For cig: not applicable.** A build-artifact ignore list.

#### `Makefile` (757 lines)

- **What it is.** The core makefile under `src/`. Runs the host stage
  (builds `minilua`, then `buildvm`, which emits `buildvm_arch.h`,
  `lj_bcdef.h`, `lj_ffdef.h`, `lj_libdef.h`, `lj_recdef.h` and `vmdef.lua`
  from the chosen `vm_*.dasc` and the library C files) and the target stage
  (compiles the LJ core against those generated headers and links
  `libluajit.a` and `luajit`). Detects `TARGET_SYS` and `TARGET_LJARCH` by
  preprocessing `lj_arch.h`.
- **For cig: not applicable.** cig's build is one `cargo` run with no
  code-generation step.

#### `Makefile.dep` (261 lines)

- **What it is.** A generated header-dependency fragment the main makefile
  `include`s. One stanza an object, each listing the header closure;
  produced by `make depend`.
- **For cig: not applicable.** Cargo resolves this.

#### `host/.gitignore` (3 lines)

- **What it is.** Ignores the `minilua`, `buildvm` and `buildvm_arch.h`
  artifacts of the host stage.
- **For cig: not applicable.** Build-artifact ignore list.

#### `host/README` (4 lines)

- **What it is.** Four-line note that these host-stage programs run on the
  build machine when cross-compiling.
- **For cig: not applicable.** cig has no cross-build host stage.

#### `host/buildvm.c` (529 lines)

- **What it is.** The LuaJIT build's code-generation driver. `main` calls
  `parseargs` and `build_code`, which runs the DynASM encoder over the
  action list for the target arch (compiled by `minilua` running
  `dynasm.lua` over the chosen `vm_*.dasc`, emitted as the generated
  `buildvm_arch.h`) and writes one of eleven output modes listed by
  `BUILDDEF` in `host/buildvm.h`. `collect_reloc` records relocations,
  `sym_decorate` handles per-ABI name mangling, `emit_bcdef` and
  `emit_vmdef` synthesize the headers the target stage reads.
- **For cig: not applicable.** cig has no DynASM and no equivalent driver.

#### `host/buildvm.h` (105 lines)

- **What it is.** Header for `buildvm.c`. Defines the `BuildCtx` struct
  (DynASM state, output file, action list, symbol and relocation tables,
  names), `BuildReloc` and `BuildSym`, `BUILD_MAX_RELOC`, `BUILD_MAX_FOLD`,
  and the `BUILDDEF` list of modes.
- **For cig: not applicable.** Peer of `host/buildvm.c`.

#### `host/buildvm_asm.c` (355 lines)

- **What it is.** The ELF, COFF and Mach-O text-assembly back end for
  `buildvm`. `emit_asm` writes the directives, symbols and the hex bytes of
  the DynASM-generated code with relocations expressed as assembly
  expressions.
- **For cig: not applicable.** Peer of `host/buildvm.c`.

#### `host/buildvm_fold.c` (236 lines)

- **What it is.** Reads the fold rules written in the `LJFOLD` mini-DSL
  inside `lj_opt_fold.c`, parses them into a hash-indexed table and emits
  `lj_folddef.h` for the SSA optimizer to read at run time.
- **For cig: not applicable.** cig's passes are not table-driven; a fold DSL
  belongs to the trace-compiler study.

#### `host/buildvm_lib.c` (466 lines)

- **What it is.** Scans the standard-library C files (`lib_*.c`) for the
  `LIBDEF_PREFIX` markers defined in `host/buildvm.h` to extract function
  declarations, fast-function shims and library tables, and emits the
  generated `lj_libdef.h`, `lj_ffdef.h` and `lj_recdef.h`. `emit_lib` is its
  entry.
- **For cig: not applicable.** cig's stdlib is registered directly in Rust
  under `src/stdlib/`.

#### `host/buildvm_libbc.h` (81 lines)

- **What it is.** A generated header holding one `libbc_code[]` byte array
  of Lua helper bytecode compiled by `host/genlibbc.lua` at build time, with
  `libbc_map[]` naming each helper's offset.
- **For cig: not applicable.** Peer of the library stage.

#### `host/buildvm_peobj.c` (473 lines)

- **What it is.** The PE/COFF object-file back end for `buildvm`.
  `emit_peobj` writes a valid COFF header, section table, relocation table
  and symbol table carrying the DynASM-generated interpreter for Windows and
  the console targets.
- **For cig: not applicable.** Peer of `host/buildvm_asm.c`.

#### `host/genlibbc.lua` (234 lines)

- **What it is.** A Lua script, run by LuaJIT itself through the Makefile's
  bytecode-regeneration target, that compiles library-side helper routines
  into the committed `host/buildvm_libbc.h`. Uses `require("ffi")`,
  `require("bit")` and `require("jit.vmdef")`, so it cannot run under
  `minilua`.
- **For cig: not applicable.** cig has no compile-time Lua code generator.

#### `host/genminilua.lua` (436 lines)

- **What it is.** A maintainer-only Lua script, run with a system Lua
  against a Lua 5.1 source tree, that subsets the Lua 5.1 interpreter to
  what DynASM needs, strips it, and writes a fresh `host/minilua.c`. Not
  called from the Makefile: `minilua.c` is committed and the build compiles
  it directly.
- **For cig: not applicable.** cig has no bootstrap of this kind.

#### `host/genversion.lua` (45 lines)

- **What it is.** A short Lua script that reads the `LUAJIT_VERSION`
  template from `luajit_rolling.h` and the rolling tag from a `relver` file
  (or Git) and writes the generated `luajit.h`.
- **For cig: not applicable.** Cargo carries the version in `Cargo.toml`.

#### `host/minilua.c` (7771 lines)

- **What it is.** A one-file, cut-down Lua 5.1 interpreter generated by
  `host/genminilua.lua` from mainline Lua. It is used only during the LuaJIT
  build to run `dynasm.lua` (DynASM's driver) over `vm_*.dasc`. Not linked
  into the LuaJIT binary.
- **For cig: not applicable.** A build bootstrap; the pattern of shipping a
  tiny host interpreter to run a build DSL has no place in cig.

#### `lj_dispatch.c` (569 lines)

- **What it is.** Dispatch-table management. `lj_dispatch_init` lays out the
  two halves of the table; `lj_dispatch_init_hotcount` seeds the `HotCount`
  slots; `lj_dispatch_update` is the mode-swap trick A8 reads, selecting
  which entries point at the real handler and which at `lj_vm_inshook`,
  `lj_vm_rethook`, `lj_vm_callhook`, `lj_vm_profhook` or `lj_vm_record`.
  `lj_dispatch_ins` and `lj_dispatch_call` service hook call-outs.
- **For cig: adapt.** See A8. In cig the step counter is
  `Interp::steps`/`Interp::max_steps` (`src/interp/mod.rs`); the register
  stack, cells, frames, tries, burns and upvals vectors live on `Machine`
  (`src/vm/mod.rs`), which holds `interp: &mut Interp`.

#### `lj_dispatch.h` (165 lines)

- **What it is.** The header A8 reads: `HotCount` as `uint16_t`,
  `HOTCOUNT_SIZE`, `HOTCOUNT_PCMASK`, `HOTCOUNT_LOOP`, `HOTCOUNT_CALL`,
  `hotcount_get`, `hotcount_set`; the `GG_State` struct that places `L`,
  `g`, `J`, `hotcount` and `dispatch` in one block so one register reaches
  them all; the `GG_DISP2G`, `GG_DISP2J`, `GG_DISP2HOT` and `GG_DISP2STATIC`
  offset macros; `GG_LEN_SDISP`, `GG_LEN_DDISP` and `GG_LEN_DISP` sizes; and
  the prototypes `lj_dispatch_init`, `lj_dispatch_update`,
  `lj_dispatch_ins`, `lj_dispatch_call`, `lj_dispatch_stitch`,
  `lj_dispatch_profile`. The MIPS-only `GOTDEF` list names every external
  function the asm VM may call through the GOT.
- **For cig: adapt.** See A8.

#### `lj_frame.h` (297 lines)

- **What it is.** The frame-layout header. Frame kinds `FRAME_LUA`,
  `FRAME_C`, `FRAME_CONT`, `FRAME_VARG`, `FRAME_PCALL` sit in two bits
  (`FRAME_TYPE` is three) when the slot below the callee holds a PC, three
  bits (`FRAME_TYPEP` is seven) when it holds a delta. Two layouts: `LJ_FR2`
  two-slot on 64-bit (function and packed `ftsz` two slots below the
  callee's base), one-slot on 32-bit. Accessors `frame_pc`, `frame_type`,
  `frame_islua`, `frame_prevl`, `frame_prev` walk them. The C stack frame is
  a separate chain under `CFRAME_SIZE`, `cframe_prev`.
- **For cig: adapt.** See A2.

#### `lj_meta.c` (558 lines)

- **What it is.** Metamethod fallbacks called out of the asm VM's slow
  paths. `lj_meta_cache` and `lj_meta_lookup` resolve a method by its `MMS`
  index with pointer-compare on the interned name. `lj_meta_tget` handles
  `__index`, `lj_meta_tset` `__newindex`, `lj_meta_arith` the arithmetic,
  `lj_meta_cat` the chained `__concat`, `lj_meta_len`, `lj_meta_equal`,
  `lj_meta_comp`, `lj_meta_call`. The hot handler jumps here on tag or chain
  miss.
- **For cig: reject.** The language has no user-defined metamethods. The
  slow-path pattern is already taken in `fast_call`; `lj_meta_cat`'s
  counterpart is `Op::INTERP` (`src/vm/mod.rs: interpolate`) and
  `value::build_str`.

#### `lj_meta.h` (40 lines)

- **What it is.** Prototypes for `lj_meta.c`. The `lj_meta_fast` and
  `lj_meta_fastg` macros do the metatable-null and `nomm` bitmask fast path;
  the `LJ_FUNCA` entry points `lj_meta_tget`, `lj_meta_tset`,
  `lj_meta_arith`, `lj_meta_bitop`, `lj_meta_cat`, `lj_meta_len`,
  `lj_meta_equal`, `lj_meta_comp`, `lj_meta_call`, `lj_meta_for` are what
  the asm VM jumps to.
- **For cig: reject.** Peer of `lj_meta.c`; cig values fix behavior by type
  at `src/value.rs`.

#### `lj_vm.h` (122 lines)

- **What it is.** The C-visible interface to the asm VM. Call entries
  `lj_vm_call`, `lj_vm_pcall`, `lj_vm_cpcall`, `lj_vm_resume`; unwind
  `lj_vm_unwind_c`, `lj_vm_unwind_ff`; dispatch targets `lj_vm_record`,
  `lj_vm_inshook`, `lj_vm_callhook`, `lj_vm_profhook`, `lj_vm_IITERN`;
  trace-exit `lj_vm_exit_handler`; continuations `lj_cont_cat`,
  `lj_cont_ra`, `lj_cont_condt`; and `lj_vm_asm_begin`, marking the start of
  the assembled interpreter. `lj_vm_floor` and `lj_vm_ceil` are macros for
  stdlib `floor`/`ceil` on some targets; `lj_vm_tobit` appears only in a
  comment.
- **For cig: adapt.** cig takes a small named C-side surface over a larger
  private VM. Equivalent: the `pub` surface at `src/vm/mod.rs` plus the
  stdlib callback boundary.

#### `lj_vmevent.c` (63 lines)

- **What it is.** VM-event dispatcher. `lj_vmevent_prepare` looks up the
  user's handler table under the `LJ_VMEVENTS_REGKEY` registry entry, finds
  the Lua function registered for an event, pushes it on the stack and
  returns the stack mark. `lj_vmevent_call` protects the call with
  `lj_vm_pcall`, disabling the event mask so events do not recurse.
- **For cig: reject.** The language exposes no VM events; `src/vm/mod.rs`
  has no equivalent.

#### `lj_vmevent.h` (62 lines)

- **What it is.** The `VMEvent` enum (BC, TRACE, RECORD, TEXIT, ERRFIN), the
  registry key `LJ_VMEVENTS_REGKEY`, the `VMEVENT_MASK` and `VMEVENT_HASH`
  macros, and the `lj_vmevent_send` / `lj_vmevent_send_` wrappers that gate
  the mechanism on a bit of `g->vmevmask`, so an event with no handler costs
  one `and` and one branch.
- **For cig: reject.** Peer of `lj_vmevent.c`; `src/vm/mod.rs` has no
  analogue.

#### `lj_vmmath.c` (123 lines)

- **What it is.** C-level math helpers reached from the asm VM. ELF/x86 PIC
  wrappers `lj_wrap_log`, `lj_wrap_log10`, `lj_wrap_exp`, `lj_wrap_sin`,
  `lj_wrap_cos`, `lj_wrap_tan` (only on ELF/x86/PIC); `lj_vm_foldarith`
  unconditionally; `lj_vm_foldfpm` and `lj_vm_errno` under `LJ_HASJIT`;
  `lj_vm_log2` only under `LUAJIT_NO_LOG2`; `lj_vm_modi` only under JIT and
  not ARM/ARM64/PPC, or under MIPS. `lj_vm_num2i64_ptr` and
  `lj_vm_num2u64_ptr` are function-pointer indirections routing the x86
  soft-float path.
- **For cig: reject.** cig's arithmetic is Rust: `i64::checked_add` with
  `E503` on overflow (`lua.md` entry 13); integer remainder is
  `a.wrapping_rem(*b)` in `src/interp/ops.rs`, float remainder is `a % b`,
  both truncating. No wrapper layer.

#### `msvcbuild.bat` (172 lines)

- **What it is.** The MSVC/Windows build script.
- **For cig: not applicable.** Console build script.

#### `nxbuild.bat` (165 lines)

- **What it is.** Build script for the Nintendo Switch (NX) console target.
- **For cig: not applicable.** Console build script.

#### `ps4build.bat` (127 lines)

- **What it is.** Build script for the PlayStation 4 console target.
- **For cig: not applicable.** Console build script.

#### `ps5build.bat` (126 lines)

- **What it is.** Build script for the PlayStation 5 console target.
- **For cig: not applicable.** Console build script.

#### `psvitabuild.bat` (96 lines)

- **What it is.** Build script for the PlayStation Vita console target.
- **For cig: not applicable.** Console build script.

#### `vm_arm.dasc` (4885 lines)

- **What it is.** Twin of `vm_x64.dasc` for 32-bit ARM. One-slot frame; the
  DynASM switch `.if FPU` splits every arithmetic and conversion handler
  into a hard-FP and a soft-FP path (ABI change).
- **For cig: adapt (see A3).** Soft-float is a cost cig does not pay.

#### `vm_arm64.dasc` (4399 lines)

- **What it is.** Twin of `vm_x64.dasc` for ARM64. Eight callee-saved GPRs
  are pinned across the loop (`BASE`, `KBASE`, `PC`, `GLREG`, `LREG`,
  `TISNUM`, `TISNUMhi`, `TISNIL`) plus the frame pointer; `LJ_DUALNUM` is on
  by default per `lj_arch.h`'s `LJ_NUMMODE_DUAL` setting.
- **For cig: adapt (see A3).**

#### `vm_mips.dasc` (5614 lines)

- **What it is.** Twin of `vm_x64.dasc` for 32-bit MIPS. Introduces a `.FPU`
  macro that no-ops FP ops when `FPU` is clear, and resolves external calls
  through a per-state GOT: `GG_DISP2GOT` and `DISPATCH_GOT` in the file, the
  `GOTDEF` list in `lj_dispatch.h`.
- **For cig: adapt (see A3).**

#### `vm_mips64.dasc` (5770 lines)

- **What it is.** Twin of `vm_x64.dasc` for 64-bit MIPS. Same soft-float and
  GOT story as `vm_mips.dasc`, wider GPRs, 64-bit PC.
- **For cig: adapt (see A3).**

#### `vm_ppc.dasc` (6320 lines)

- **What it is.** Twin of `vm_x64.dasc` for 32-bit (and 32-on-64) PowerPC.
  `LJ_DUALNUM` paths throughout, the DynASM switch `.if FPU` for the
  soft-float branch, and TOC handling for the Xbox 360 and PS3 variants:
  function pointers are `code + TOC + env` triples that stubs must load
  before each external call.
- **For cig: adapt (see A3).** Writing one interpreter per console is the
  lesson; cig keeps one.

#### `vm_x64.dasc` (5196 lines)

- **What it is.** The DynASM source for the x86-64 bytecode interpreter,
  canonical for this study. Register assignments at the top (BASE in RDX
  refetched after C calls, PC in RBX callee-save, DISPATCH in R14
  callee-save, KBASE in R15 callee-save). Macros `ins_NEXT`, `ins_call`,
  `ins_callt`, `ins_arith`, `hotloop`, `hotcall`, `checkfunc`, `checktab`.
  Subroutine pool `code_sub` holds `vm_return`, `vm_call`, `vmeta_arith_vv`,
  `vmeta_tgets`, `vmeta_call`, `vm_hotloop`, `vm_hotcall`, `vm_growstack_f`.
  Per-opcode handlers: arithmetic (`BC_ADDVV`, `BC_MULVV`), tables
  (`BC_TGETS`, `BC_TSETV`), calls (`BC_CALL`, `BC_CALLT`), returns
  (`BC_RET1`), loops (`BC_FORL`, `BC_IFORL`, `BC_LOOP`), branches
  (`BC_JMP`), function headers (`BC_FUNCF`, `BC_IFUNCF`).
- **For cig: adapt (see A2, A3, A8).** cig takes the shape: one handler an
  opcode, slow paths out of line, operands never spilled past the first
  access, counting opcodes separate from their non-counting twins. cig
  cannot take hand assembly, replicated dispatch, or PC-hashed counters.

#### `vm_x86.dasc` (6123 lines)

- **What it is.** Twin of `vm_x64.dasc` for 32-bit x86. One-slot frame (no
  `LJ_FR2`). Like x64 it is single-number by default (`lj_arch.h` sets
  `LJ_NUMMODE_SINGLE_DUAL` on both); `.if DUALNUM` paths are present but
  take effect only with `LUAJIT_NUMMODE=2`.
- **For cig: adapt (see A3).**

#### `xb1build.bat` (104 lines)

- **What it is.** Build script for the Xbox One console target.
- **For cig: not applicable.** Console build script.

#### `xedkbuild.bat` (95 lines)

- **What it is.** Build script for the Xbox 360 (XEDK) console target.
- **For cig: not applicable.** Console build script.

### Data types and the library: strings, tables, buffers, the collector, `lib_*.c`, `jit/*.lua`

#### `lj_str.c` (370 lines)

- **What it is.** String handling and interning. `lj_str_cmp` compares by
  word with a trailing adjustment. `lj_str_find` is a bounded `memchr` plus
  `memcmp`. `hash_sparse` is the primary ARX hash; `hash_dense` is the
  fallback under `LUAJIT_SECURITY_STRHASH`. `lj_str_resize` grows the chain
  table; `lj_str_rehash_chain` switches one chain to dense past
  `LJ_STR_MAXCOLL`. `lj_str_alloc` builds a `GCstr`; `lj_str_new` is the
  entry point.
- **For cig: adapt.** Take the kept hash alone, in a string object under
  `Value::Str` (`src/value.rs`); it stays SipHash-1-3, so no dense fallback;
  interning is rejected. See A6.

#### `lj_str.h` (31 lines)

- **What it is.** Declarations for `lj_str_cmp`, `lj_str_find`,
  `lj_str_haspattern`, `lj_str_resize`, `lj_str_new`, `lj_str_free`,
  `lj_str_init`, plus the convenience macros `lj_str_newz`, `lj_str_newlit`
  and the size helper `lj_str_size`.
- **For cig: not applicable.** Header for `lj_str.c`.

#### `lj_tab.c` (687 lines)

- **What it is.** Table handling. `newtab` colocates a small array part with
  the header. `lj_tab_resize` sizes array and hash together; `countarray`,
  `counthash` and `bestasize` decide the split; `rehashtab` runs them on
  overflow. `lj_tab_getinth` is the hash-part integer lookup;
  `lj_tab_getstr` masks `sid` and compares tag and pointer. `lj_tab_newkey`
  uses Brent's variation. `lj_tab_len` binary-searches the array part only
  when the last slot is nil; otherwise `tab_len_slow`. `lj_tab_keyindex`
  serves `lj_tab_next`.
- **For cig: reject.** `Value::List` and `Value::Map` are separate types in
  `src/value.rs`: no array-plus-hash dual shape. The kept-hash-on-string
  idea stays with A6.

#### `lj_tab.h` (93 lines)

- **What it is.** The inline `hashmask` and the macros `hashstr`,
  `hashlohi`, `hashnum`, `arrayslot`, `lj_tab_getint` and `lj_tab_setint`.
  Declarations for every public function in `lj_tab.c`.
- **For cig: not applicable.** Header for `lj_tab.c`; the fast-path shape is
  in A6.

#### `lj_buf.c` (303 lines)

- **What it is.** Resizable string buffers. `buf_grow` doubles size and
  handles copy-on-write and borrowed buffers. `lj_buf_need2` and
  `lj_buf_more2` grow out of line. `lj_buf_tmp` returns the per-state
  scratch. `lj_buf_putmem` and the `lj_buf_putstr_*` family write into one.
  `lj_buf_tostr` interns the result; `lj_buf_cat2str` is a two-string fast
  path.
- **For cig: adapt.** cig already builds in one block for `+`, `.join()` and
  interpolation through `build_str` in `src/value.rs`, which holds a
  thread-local `SCRATCH` `String` kept between calls; `Op::INTERP` in
  `src/vm/mod.rs` calls `interpolate`. `docs/research/lua.md` entry 6
  records the measured win under `w29-names-c1` against `w29-pair-c0`.

#### `lj_buf.h` (198 lines)

- **What it is.** Flag bits `SBUF_FLAG_EXT`, `SBUF_FLAG_COW`,
  `SBUF_FLAG_BORROW` packed into the low three bits of `L`; `SBufExt` adds
  `cowref`, `r`, `dict_str`, `dict_mt`, `depth`; accessors `sbufsz`,
  `sbuflen`, `sbufleft`, `sbufxlen`, `sbufxslack`; inline `lj_buf_init`,
  `lj_buf_reset`, `lj_buf_need`, `lj_buf_more`.
- **For cig: not applicable.** Header for `lj_buf.c`.

#### `lj_gc.c` (911 lines)

- **What it is.** The incremental tri-color mark-and-sweep: see the
  collector entry. `gc_mark`, `gc_traverse_tab`, `gc_traverse_func`,
  `gc_traverse_trace`, `gc_traverse_proto`, `gc_traverse_thread`,
  `propagatemark`, `gc_sweep`, `gc_sweepstr`, `gc_mayclear`, `gc_clearweak`,
  `gc_finalize`, `lj_gc_finalize_udata`, `lj_gc_finalize_cdata`, `atomic`,
  `gc_onestep`, `lj_gc_step`, `lj_gc_step_fixtop`, `lj_gc_step_jit`,
  `lj_gc_fullgc`, `lj_gc_barrierf`, `lj_gc_barrieruv`, `lj_gc_closeuv`,
  `lj_gc_barriertrace`, `lj_mem_realloc`, `lj_mem_newgco`, `lj_mem_grow`.
- **For cig: reject.** Rollback in `docs/BURN.md` and the exact step budget
  in `src/vm/mod.rs: Op::STEPS` rule out a tracing collector; cig's `Value`
  holds `Rc` for every heap object (`src/value.rs`). See the collector
  entry.

#### `lj_gc.h` (136 lines)

- **What it is.** `GCS*` state enum, color bitmask constants, predicates
  `iswhite`, `isblack`, `isgray`, `otherwhite`, and setters `newwhite`,
  `makewhite`, `flipwhite`, `fixstring`, `markfinalized`. The `lj_gc_check`
  and `lj_gc_check_fixtop` driver macros gate `lj_gc_step` on the threshold.
  Declarations for every collector entry point.
- **For cig: not applicable.** Header for the collector.

#### `lj_alloc.c` (1485 lines)

- **What it is.** A customized dlmalloc. Inside `#if LJ_64`,
  `LJ_ALLOC_MBITS` caps usable address bits: 31 on x64 without `LJ_GC64`, 32
  on other 64-bit architectures without `LJ_GC64`, 47 under `LJ_GC64`.
  `LJ_ALLOC_MMAP_PROBE` scans for a free low region on x64;
  `LJ_ALLOC_MMAP32` uses `MAP_32BIT` where available. Provides
  `lj_alloc_create`, `lj_alloc_destroy` and the pluggable `lj_alloc_f` form
  called from `src/lj_gc.c`.
- **For cig: reject.** cig has no tag-bit-in-pointer constraint (`Value` in
  `src/value.rs` is a plain enum); the released binary links the system
  allocator or mimalloc as `docs/research/lua.md` records.

#### `lj_alloc.h` (18 lines)

- **What it is.** Four declarations: `lj_alloc_create`, `lj_alloc_setprng`,
  `lj_alloc_destroy`, `lj_alloc_f`.
- **For cig: not applicable.** Header for `lj_alloc.c`.

#### `lj_udata.c` (62 lines)

- **What it is.** Userdata objects. `lj_udata_new` allocates a `GCudata`
  with inline payload, chains it after the main thread in `g->gc.root`, and
  clears the metatable. `lj_udata_free` frees it. `lj_lightud_intern`
  returns a canonical light-userdata pointer on 64-bit by interning the
  upper bits.
- **For cig: reject.** `Value` in `src/value.rs` has no `Userdata` variant
  and no light-userdata form; foreign state enters cig through `fs`, `proc`,
  `env` and `time` builtins (`src/stdlib/`), each with its own `Effect`,
  measured and journaled.

#### `lj_udata.h` (17 lines)

- **What it is.** Three declarations: `lj_udata_new`, `lj_udata_free`,
  `lj_lightud_intern`.
- **For cig: not applicable.** Header for `lj_udata.c`.

#### `lj_prng.c` (259 lines)

- **What it is.** A Tausworthe generator of period 2 to the 223.
  `lj_prng_u64` is the API. `lj_prng_seed_secure` reads entropy with
  `getentropy`, falls back to the `getrandom` syscall and `/dev/urandom`,
  and on Windows uses `BCryptGenRandom` or `RtlGenRandom`.
  `lj_prng_seed_fixed` is a baked-in constant. Not cryptographic; drives
  `math.random`, string-id reseeding and the string-hash seed.
- **For cig: reject.** No `random` module is declared in
  `src/stdlib/mod.rs`, and `src/stdlib/math.rs` has no `math.random`. cig
  has no string interning at all (`Value::Str` is `Rc<str>` in
  `src/value.rs`), so no seeded hash and no string-id reseeding are owed.

#### `lj_prng.h` (24 lines)

- **What it is.** Declarations for `lj_prng_seed_secure`, `lj_prng_u64`,
  `lj_prng_u64d`; inline `lj_prng_seed_fixed`.
- **For cig: not applicable.** Header for `lj_prng.c`.

#### `lj_strfmt.c` (601 lines)

- **What it is.** Integer and format writing. `lj_strfmt_parse` walks a
  printf-style directive; `lj_strfmt_wint` writes a signed integer with a
  digit-pair table. `lj_strfmt_putint`, `lj_strfmt_putnum`,
  `lj_strfmt_putfxint` and the other `lj_strfmt_put*` functions back
  `string.format` through `lj_strfmt_putarg`. `lj_strfmt_int`,
  `lj_strfmt_number` return a `GCstr`. No `snprintf`.
- **For cig: take.** cig writes integers through its own digit loop in
  `src/value.rs` (`push_int`), not through `core::fmt`;
  `docs/research/lua.md` entry 14 records the measured win under
  `w29-names-c1` against `w29-pair-c0`.

#### `lj_strfmt.h` (131 lines)

- **What it is.** `SFormat` type, `FormatState`, the `FormatType` enum
  (`STRFMT_EOF`, `STRFMT_INT`, `STRFMT_UINT`, `STRFMT_NUM`, `STRFMT_STR`,
  `STRFMT_CHAR`, `STRFMT_PTR`), subtype bits (`STRFMT_T_HEX`,
  `STRFMT_T_OCT`, the floating subtypes), flag bits (`STRFMT_F_LEFT`,
  `STRFMT_F_PLUS`, `STRFMT_F_ZERO`, `STRFMT_F_SPACE`); declarations for
  every function in `lj_strfmt.c` and `lj_strfmt_num.c`.
- **For cig: not applicable.** Header for the two formatter files.

#### `lj_strfmt_num.c` (630 lines)

- **What it is.** IEEE-754 double to decimal with no libc dependency.
  `nd_mul2k` and `nd_div2k` shift a bignum; `nd_round` rounds to a target
  precision; `lj_strfmt_wfnum` dispatches by G, E, F, A; `lj_strfmt_num`
  returns a `GCstr`. Rescale and threshold tables are precomputed.
- **For cig: reject.** cig's `format_float` in `src/value.rs` uses Rust's
  own float formatting (the shortest form that reads back, an exponent form
  from sixteen digits), not a bignum path of its own. Take the design only
  if `format_float` shows on a profile.

#### `lj_strscan.c` (558 lines)

- **What it is.** Builtin string-to-number scanning, replacing `strtod`.
  Accepts decimal, hex, hex-float, octal, long long and imaginary, always
  with a dot as the decimal separator. Returns one of `STRSCAN_ERROR`,
  `STRSCAN_NUM`, `STRSCAN_INT`, `STRSCAN_U32`, `STRSCAN_I64`, `STRSCAN_U64`,
  `STRSCAN_IMAG`. Entry points `lj_strscan_scan` and `lj_strscan_num` do no
  allocation.
- **For cig: reject.** cig's literals are parsed in `src/syntax/lexer.rs`
  and `src/syntax/parser.rs`, and `string.to_int` and `string.to_float` in
  `src/stdlib/methods.rs` cover the user conversions; no hex, octal,
  long-long or imaginary syntax, so LuaJIT's wider scanner has nothing to
  take. Revisit only if measurement names `str::parse` as a cost.

#### `lj_strscan.h` (40 lines)

- **What it is.** The options flags `STRSCAN_OPT_TOINT`,
  `STRSCAN_OPT_TONUM`, `STRSCAN_OPT_IMAG`, `STRSCAN_OPT_LL`,
  `STRSCAN_OPT_C`; the returned-format enum; declarations for
  `lj_strscan_scan` and `lj_strscan_num`.
- **For cig: not applicable.** Header for `lj_strscan.c`.

#### `lj_char.c` (43 lines)

- **What it is.** One table, `lj_char_bits`, 257 entries giving ASCII
  character-class bits (control, space, punctuation, digit, alpha,
  identifier-byte). Hard-coded ASCII, never calls `setlocale`. Used by the
  lexer, by `lib_string.c` and by pattern matching.
- **For cig: adapt.** cig's lexer in `src/syntax/lexer.rs` classifies bytes
  with `is_ascii_alphanumeric` and the other byte predicates from `u8`; the
  lesson is the same (small, baked-in, never a libc call), the helpers are
  Rust's.

#### `lj_char.h` (42 lines)

- **What it is.** Eight mask bits (`LJ_CHAR_CNTRL`, `LJ_CHAR_SPACE`,
  `LJ_CHAR_PUNCT`, `LJ_CHAR_DIGIT`, `LJ_CHAR_XDIGIT`, `LJ_CHAR_UPPER`,
  `LJ_CHAR_LOWER`, `LJ_CHAR_IDENT`) and three composites (`LJ_CHAR_ALPHA`,
  `LJ_CHAR_ALNUM`, `LJ_CHAR_GRAPH`); the `lj_char_bits` declaration; and
  macro predicates `lj_char_isident`, `lj_char_isdigit`, `lj_char_isalpha`,
  `lj_char_isspace`, `lj_char_ispunct`, `lj_char_isxdigit` through
  `lj_char_isa`.
- **For cig: not applicable.** Header for `lj_char.c`.

#### `lj_ff.h` (18 lines)

- **What it is.** The `FastFunc` enum: `FF_LUA_` is zero, `FF_C_` is one,
  then one `FF_<name>` per fast function through the generated `lj_ffdef.h`;
  `FF__MAX` closes it.
- **For cig: not applicable.** The identifier space for the fast functions;
  A4 covers the mechanism.

#### `lj_lib.c` (377 lines)

- **What it is.** Library infrastructure. `lj_lib_register` reads a
  per-module init byte stream and registers each function;
  `lib_create_table` resolves the module's table; `lib_read_lfunc` reads a
  bytecode-serialized Lua helper. Argument checkers `lj_lib_checkstr`,
  `lj_lib_checknum`, `lj_lib_checkint`, `lj_lib_checkfunc`,
  `lj_lib_checktab`, `lj_lib_checkany` raise through `lj_err_arg`.
  `lj_lib_pushcc` installs a C closure under a fast-function id.
- **For cig: adapt.** cig's registration is the `b(name, effect, min, max,
  func)` helper in `src/stdlib/mod.rs` and each module's `TABLE`; the
  argument helpers `expect_str`, `expect_int`, `expect_num`, `expect_list`,
  `expect_map` live in `src/value.rs` and return a `Diagnostic` instead of
  raising.

#### `lj_lib.h` (117 lines)

- **What it is.** Registration declarations, the return protocol
  (`FFH_RETRY` is zero, `FFH_UNREACHABLE` is an alias for it, `FFH_RES(n)`
  returns `n+1`, `FFH_TAILCALL` is minus one), the scanner markers
  `LJLIB_CF`, `LJLIB_ASM`, `LJLIB_ASM_`, `LJLIB_LUA`, `LJLIB_SET`,
  `LJLIB_PUSH`, `LJLIB_REC`, `LJLIB_NOREGUV`, `LJLIB_NOREG`, and the init
  tags `LIBINIT_CF`, `LIBINIT_ASM`, `LIBINIT_ASM_`, `LIBINIT_STRING`,
  `LIBINIT_LUA`, `LIBINIT_SET`, `LIBINIT_NUMBER`, `LIBINIT_COPY`,
  `LIBINIT_LASTCL`, `LIBINIT_FFID`, `LIBINIT_END` used by `host/buildvm`.
- **For cig: not applicable.** Header for the fast-function protocol; A4
  covers it.

#### `lj_profile.c` (388 lines)

- **What it is.** A sampling profiler with three timer back-ends:
  `LJ_PROFILE_SIGPROF` (Linux and BSD, `setitimer` and `SIGPROF`),
  `LJ_PROFILE_PTHREAD` (`nanosleep` in a sampler thread),
  `LJ_PROFILE_WTHREAD` (Windows timer-queue callback). The tick calls
  `profile_trigger` which sets a dispatch flag. `luaJIT_profile_start`,
  `luaJIT_profile_stop`, `luaJIT_profile_dumpstack` back `jit/p.lua`.
- **For cig: reject.** cig uses callgrind and `perf stat` from outside
  (`docs/research/lua.md` status). No in-VM sampler is wanted; the
  measurement kit lives beside the VM.

#### `lj_profile.h` (23 lines)

- **What it is.** Declarations for `lj_profile_interpreter`,
  `lj_profile_hook_enter`, `lj_profile_hook_leave`, `lj_profile_lock`,
  `lj_profile_unlock`, gated on `LJ_HASPROFILE`. The public
  `luaJIT_profile_start`, `luaJIT_profile_stop`, `luaJIT_profile_dumpstack`
  are declared in `luajit.h`.
- **For cig: not applicable.** Header for `lj_profile.c`.

#### `lj_serialize.c` (539 lines)

- **What it is.** Binary serializer for the `string.buffer` library. A
  one-byte tag per value (`SER_TAG_NIL`, `SER_TAG_FALSE`, `SER_TAG_TRUE`,
  `SER_TAG_NULL`, `SER_TAG_LIGHTUD32`, `SER_TAG_LIGHTUD64`, `SER_TAG_INT`,
  `SER_TAG_NUM`, `SER_TAG_TAB`, `SER_TAG_DICT_MT`, `SER_TAG_DICT_STR`,
  `SER_TAG_INT64`, `SER_TAG_UINT64`). `serialize_wu124` and
  `serialize_ru124` are a one-to-five-byte length prefix.
  `lj_serialize_put`, `lj_serialize_get`, `lj_serialize_encode`,
  `lj_serialize_decode` are the entries; `lj_serialize_peektype` looks at
  the next tag. `LJ_SERIALIZE_DEPTH` caps recursion.
- **For cig: reject.** cig's value interchange is text through `json.parse`,
  `json.stringify`, `csv.parse`, `csv.stringify` (`src/stdlib/json.rs`,
  `src/stdlib/csv.rs`); verified bytecode has its own layout in
  `crates/cig-bytecode/src/module.rs` and is not a value serializer. A
  binary value format would duplicate both.

#### `lj_serialize.h` (28 lines)

- **What it is.** `LJ_SERIALIZE_DEPTH` and declarations for
  `lj_serialize_dict_prep_str`, `lj_serialize_dict_prep_mt`,
  `lj_serialize_put`, `lj_serialize_get`, `lj_serialize_encode`,
  `lj_serialize_decode`, `lj_serialize_peektype`. Gated on `LJ_HASBUFFER`.
- **For cig: not applicable.** Header for `lj_serialize.c`.

#### `lib_base.c` (709 lines)

- **What it is.** The base library. The 16 `LJLIB_ASM` fast functions:
  `assert`, `type`, `next`, `pairs`, `ipairs`, `ipairs_aux`, `getmetatable`,
  `setmetatable`, `rawget`, `tonumber`, `tostring`, `pcall`, `xpcall`,
  `coroutine_yield`, `coroutine_resume`, `coroutine_wrap_aux`. `LJLIB_CF`
  plain C: `getfenv`, `rawset`, `rawequal`, `rawlen`, `unpack`, `select`,
  `error`, `load`, `collectgarbage`, `newproxy`, and the coroutine family.
- **For cig: adapt.** Globals in `src/stdlib/core.rs` are the counterparts:
  `assert`, `type_of` (for `type`), `int`/`float` (for `tonumber`), `str`
  (for `tostring`). No metatables, coroutines or `load`; rollback through
  `docs/BURN.md` replaces `pcall`/`xpcall`.

#### `lib_math.c` (205 lines)

- **What it is.** `math.abs`, `math.floor`, `math.ceil`, `math.sqrt`,
  `math.log`, `math.log10`, `math.exp`, `math.sin`, `math.cos`, `math.tan`,
  `math.asin`, `math.acos`, `math.atan`, `math.sinh`, `math.cosh`,
  `math.tanh`, `math.atan2`, `math.pow`, `math.fmod`, `math.ldexp`,
  `math.frexp`, `math.modf`, `math.min`, `math.max`, `math.random`,
  `math.randomseed`, plus `math.pi` and `math.huge`. The 24 fast functions
  are all of these but `math.random` and `math.randomseed`. `math_min` and
  `math_max` are variadic with a running SSE compare in `src/vm_x64.dasc`.
- **For cig: adapt.** cig's `math` in `src/stdlib/math.rs` registers
  `math.abs`, `math.min`, `math.max`, `math.floor`, `math.ceil`,
  `math.round`, `math.sqrt`, `math.pow`, `math.clamp`. No `log`, `exp`, trig
  or hyperbolic forms yet; listed in A4.

#### `lib_string.c` (690 lines)

- **What it is.** `string.byte`, `string.char`, `string.sub`,
  `string.reverse`, `string.lower`, `string.upper` as `LJLIB_ASM`;
  `string.rep`, `string.format`, `string.find`, `string.match`,
  `string.gmatch`, `string.gsub`, `string.dump` as `LJLIB_CF`; `string.len`
  is a `LJLIB_LUA` helper. The pattern matcher is recursive-descent, built
  on `match_class`, `matchbracketclass`, `singlematch`, `matchbalance`,
  `match_capture`, `match`. `string.format` dispatches through
  `src/lj_strfmt.c`.
- **For cig: adapt.** The LuaJIT fast functions have method counterparts in
  `src/stdlib/methods.rs`: `.upper()`, `.lower()`, `.reverse()`, `.slice()`
  (no single-byte or single-char accessor). Lua patterns are not
  implemented; regular expressions live in the `text` module
  (`src/stdlib/text.rs`). Listed in A4.

#### `lib_table.c` (327 lines)

- **What it is.** `table.foreachi`, `table.foreach`, `table.getn`,
  `table.maxn`, `table.insert`, `table.remove`, `table.move`,
  `table.concat`, `table.sort`, `table.pack`; `table.new` and `table.clear`
  are not registered as globals. The file has no fast-function declarations:
  `table.insert` and the others are plain C handlers. `table.foreachi`,
  `table.foreach`, `table.getn`, `table.remove`, `table.move` are
  Lua-sourced helpers compiled at build time.
- **For cig: adapt.** cig's list methods `.push()`, `.insert()`,
  `.remove()`, `.len()`, `.sort()`, `.sort_by()`, `.reverse()`, `.join()`,
  `.copy()`, `.slice()` and the rest live in `src/stdlib/methods.rs`; sort
  is Rust's `Vec::sort`, not a C-API trampoline (`docs/research/lua.md`
  entry 15). Listed in A4.

#### `lib_bit.c` (181 lines)

- **What it is.** The bit library: `bit.tobit`, `bit.bnot`, `bit.bswap`,
  `bit.band`, `bit.bor`, `bit.bxor`, `bit.lshift`, `bit.rshift`,
  `bit.arshift`, `bit.rol`, `bit.ror`, `bit.tohex`. The 11 fast functions
  are all of these but `bit.tohex`. The matching macros in `src/vm_x64.dasc`
  do an SSE2-to-GPR move, the operation, then back.
- **For cig: reject.** No bit module exists in `src/stdlib/mod.rs`'s module
  list; the programs of `bench/compute` call no bit operations. If one is
  added later it sits beside `math.rs`.

#### `lib_io.c` (554 lines)

- **What it is.** The `io` library. Module-level `io.open`, `io.close`,
  `io.read`, `io.write`, `io.lines`, `io.input`, `io.output`, `io.popen`,
  `io.tmpfile`, `io.type`; file methods `io_method_read`, `io_method_write`,
  `io_method_flush`, `io_method_seek`, `io_method_setvbuf`,
  `io_method_lines`. Each file is a userdata wrapping a `FILE *`; `fgets`,
  `fread`, `fwrite`.
- **For cig: reject.** cig's file I/O is `fs.read_text`, `fs.write_text`,
  `fs.list`, `fs.cp`, `fs.mv` and the rest in `src/stdlib/fs.rs`, each
  effect-tagged and journaled through `docs/BURN.md`; a script reads
  standard input through the global `stdin` in `src/stdlib/core.rs`. A `FILE
  *` userdata would break effect accounting and rollback.

#### `lib_os.c` (295 lines)

- **What it is.** The `os` library. `os_execute`, `os_remove`, `os_rename`,
  `os_tmpname`, `os_getenv`, `os_exit`, `os_clock`, `os_date`, `os_time`,
  `os_difftime`, `os_setlocale`. Thin wrappers over `time`, `clock_gettime`,
  `gmtime_r`, `getenv`, `unlink`, `rename`, `setlocale`, `system`, `exit`,
  `mkstemp`.
- **For cig: reject.** cig has `time.now_ms`, `time.now_iso`, `time.stamp`,
  `time.sleep_ms` and `time.format_ms` in `src/stdlib/time.rs`; `env.get`,
  `env.has`, `env.all`, `env.set`, `env.unset` in `src/stdlib/env.rs`;
  `proc.run`, `proc.shell` and friends in `src/stdlib/proc.rs`; the global
  `exit` in `src/stdlib/core.rs`. Each is effect-tagged, journaled and
  rolled back; `os.execute`'s plain libc `system` has no analogue.

#### `lib_package.c` (628 lines)

- **What it is.** The `require` implementation. `package.loaders`,
  `package.path`, `package.cpath`, `package.loaded`, `package.preload`,
  `package.config`. `ll_loadfunc` does the dynamic load through `dlopen` on
  POSIX or the Windows loader; converts `foo.bar.baz` to a path pattern,
  searches, loads, caches.
- **For cig: reject.** cig has no imports at all: `docs/LANGUAGE.md` lists
  "no imports" among the deliberate omissions, and
  `crates/cig-bytecode/src/module.rs` ties a run to a single `Module`
  structure (`MAGIC`, `FORMAT_VERSION`, no runtime linking); the verified
  bytecode (`crates/cig-bytecode/src/verify.rs: verify_module`) is what
  runs.

#### `lib_debug.c` (406 lines)

- **What it is.** `debug.getregistry`, `debug.getmetatable`,
  `debug.setmetatable`, `debug.getfenv`, `debug.setfenv`, `debug.getinfo`,
  `debug.getlocal`, `debug.setlocal`, `debug.getupvalue`,
  `debug.setupvalue`, `debug.upvalueid`, `debug.upvaluejoin`,
  `debug.getuservalue`, `debug.setuservalue`, `debug.sethook`,
  `debug.gethook`, `debug.debug`, `debug.traceback`. Walks the frame chain
  through helpers in `src/lj_debug.c`.
- **For cig: reject.** No variant of `Value` in `src/value.rs` exposes a
  frame or an upvalue; the verifier enforces per-function registers and ties
  upvalues to `UpvalDesc` at build (`crates/cig-bytecode/src/module.rs:
  UpvalDesc`, `verify_upvalues` in `verify.rs`). Error reporting goes
  through `Diagnostic` and `Span` (`src/vm/mod.rs`); no hook mechanism.

#### `lib_init.c` (55 lines)

- **What it is.** `luaL_openlibs` walks `lj_lib_load` (base, package, table,
  io, os, string, math, debug, bit, jit) and `lj_lib_preload` (ffi, under
  `LJ_HASFFI`), calling each `luaopen_*` in turn.
- **For cig: adapt.** cig's wiring is `builtin_catalogue` and `method_at` in
  `src/stdlib/mod.rs`; the module list in `src/stdlib/mod.rs` names `cig`,
  `csv`, `env`, `fs`, `hash`, `json`, `log`, `math`, `pass`, `path`, `proc`,
  `text`, `time` (not `base`, `package`, `io`, `os`, `string`, `debug`,
  `bit`, `jit`, `ffi`).

#### `lib_aux.c` (401 lines)

- **What it is.** The auxiliary `luaL_*` C API, declared in `src/lauxlib.h`.
  This file defines `luaL_openlib`, `luaL_register`, `luaL_pushresult`,
  `luaL_newstate` with allocator wiring, the libc I/O error reporter and the
  panic callback; the arg-checking shims are elsewhere in the family.
- **For cig: reject.** No embedding C API: a cig builtin is the `BuiltinFn`
  type alias in `src/value.rs`, registered through `b(...)` in each module.
  No stack API, no `luaL_newstate`-shaped lifecycle; cig is a library crate,
  not a shared C library.

#### `lib_buffer.c` (360 lines)

- **What it is.** `string.buffer`, a user-visible wrapper for `SBuf` and
  `SBufExt`. Methods `buffer.new`, `buffer.reset`, `buffer.free`,
  `buffer.put`, `buffer.putf`, `buffer.putcdata`, `buffer.set`,
  `buffer.get`, `buffer.ref`, `buffer.commit`, `buffer.skip`,
  `buffer.encode`, `buffer.decode`. Each buffer is userdata whose payload is
  an `SBufExt`. Serializer methods go through `lj_serialize.c`.
- **For cig: reject.** `Value` in `src/value.rs` has no buffer variant: `+`
  and interpolation (`Op::INTERP`, `interpolate` in `src/vm/mod.rs`) and
  `.join()` cover the measured string building (`docs/research/lua.md` entry
  6). A user-visible buffer would reintroduce userdata.

#### `lib_jit.c` (775 lines)

- **What it is.** The `jit` library. `jit.on`, `jit.off`, `jit.flush`,
  `jit.status`, `jit.security`, `jit.attach` as module functions;
  `jit.util.funcinfo`, `jit.util.funcbc`, `jit.util.funck`,
  `jit.util.funcuvname`, `jit.util.traceinfo`, `jit.util.traceir`,
  `jit.util.tracek`, `jit.util.tracesnap`, `jit.util.tracemc`,
  `jit.util.traceexitstub`, `jit.util.ircalladdr` for querying traces;
  `jit.opt.start` for flags; `jit.profile.start` for the profiler.
- **For cig: reject.** cig has no JIT at run time and so no `jit` library:
  stage C is planned ahead of time, on Cranelift (cig-compile's
  `docs/decisions/0001-backend.md`). The verifier
  (`crates/cig-bytecode/src/verify.rs`) runs ahead of execution; nothing
  lives behind an `on`/`off` switch at run time.

#### `jit/.gitignore` (1 lines)

- **What it is.** A one-line ignore for `vmdef.lua`, generated here by
  `host/buildvm`.
- **For cig: not applicable.** Repository metadata.

#### `jit/bc.lua` (188 lines)

- **What it is.** A bytecode lister. `bc.dump` prints every bytecode of a
  function; `bc.line` returns one line. Run as `luajit -jbc` it hooks the
  parser and lists every function as it is compiled. Default output is
  standard error; `LUAJIT_LISTFILE` or a filename argument redirects. Uses
  `jit.util.funcbc`, `jit.util.funcinfo`, `jit.util.funck`,
  `jit.util.funcuvname`. The `bcline` format string uses spaces.
- **For cig: adapt.** cig's bytecode lister is `disassemble_module` in
  `crates/cig-bytecode/src/disasm.rs`. This file is the tool the study reads
  LuaJIT's bytecode through.

#### `jit/bcsave.lua` (638 lines)

- **What it is.** Backend for `luajit -b`. Compiles a Lua source to `.o`,
  `.obj`, `.raw`, `.c` or `.h` holding its bytecode, plus a stub function
  named `luaJIT_BC_<modname>`. Handles Mach-O, ELF, COFF for x86, x64, arm,
  arm64, ppc and mips; big-endian and little-endian; the deterministic mode
  under flag `d`. Reads a source, calls `string.dump`, emits the object.
- **For cig: reject.** cig's compiled form is a verified module serialized
  by `crates/cig-bytecode/src/serialize.rs` (`serialize_module`), not an ELF
  or Mach-O file. Native code is stage C's, through Cranelift (cig-compile's
  `docs/decisions/0001-backend.md`), not built yet.

#### `jit/dump.lua` (730 lines)

- **What it is.** The `-jdump` module. Flags select features: `t` for trace
  start, end, abort; `b` for traced bytecode; `i` for the IR; `r` for IR
  with registers; `s` for the snapshot map; `m` for machine code; `x`/`X`
  for exits; `a` for aborted traces. Attaches through `jit.attach`;
  disassembles through the matching `jit/dis_*` module.
- **For cig: reject.** No trace compiler at run time: stage C is planned
  ahead of time, on Cranelift (cig-compile's
  `docs/decisions/0001-backend.md`).

#### `jit/v.lua` (174 lines)

- **What it is.** The `-jv` module. One line per trace generated: internal
  trace number, origin as file and line, parent-trace and exit for side
  traces, and the link target. A loop trace reads like a trace-start line
  with the `loop` suffix; a side trace reads like a start line with a parent
  reference. Output goes to standard error or to `LUAJIT_VERBOSEFILE`.
- **For cig: reject.** No runtime tracer in cig (see
  `decisions/0001-backend.md` in cig-compile). This file is one of the tools
  the study reads LuaJIT through.

#### `jit/p.lua` (309 lines)

- **What it is.** The front end for the sampling profiler. Flags `f`, `F`
  and `l` name a stack frame (function name; module and name;
  `module:line`); a signed number is stack-dump depth; `s` splits after the
  first level; `p` shows full paths; `v` shows VM states; `z` groups by zone
  from `jit/zone.lua`; `r` is raw counts; `G` emits raw output for flame
  graphs. Backed by `jit.profile.start`, `.stop` and `.dumpstack` in
  `lib_jit.c`.
- **For cig: reject.** cig profiles externally with callgrind and `perf`
  (`docs/research/lua.md` status note); no in-VM sampler.

#### `jit/zone.lua` (45 lines)

- **What it is.** A stack-of-strings module for the `-jp` zone flag. Calling
  the module with a string pushes; calling it with nothing pops; indexing
  `get` reads the top. About twenty lines of code behind a metatable with
  `__call`.
- **For cig: reject.** cig has no scripted profiler zones:
  `src/stdlib/log.rs` provides `log.debug`, `log.info`, `log.warn`,
  `log.error` only, with no scope stack.

#### `jit/dis_x86.lua` (967 lines)

- **What it is.** A complete x86 and x64 disassembler, from Intel and AMD
  opcode manuals. 32-bit and 64-bit modes, MMX, SSE through SSE4.2, SSE4a,
  AVX, AVX2, privileged and hypervisor VMX and SVM. Output format matches
  `ndisasm`. Fed by `jit/dump.lua` to annotate machine code for a trace. One
  primary opcode table (`map_opc1_32`), prefix tables, ModRM and SIB
  decoders.
- **For cig: reject.** cig has no machine-code disassembler and no such
  dependency; the back end planned for stage C is Cranelift (cig-compile's
  `docs/decisions/0001-backend.md`). The bytecode-level `disassemble_module`
  in `crates/cig-bytecode/src/disasm.rs` is separate.

#### `jit/dis_x64.lua` (17 lines)

- **What it is.** A wrapper that re-exports the 64-bit decoder from
  `jit/dis_x86.lua`.
- **For cig: not applicable.** Wrapper only.

#### `jit/dis_arm.lua` (688 lines)

- **What it is.** An ARMv7 user-mode disassembler. Advanced SIMD and VFP are
  not covered. The design mirrors `jit/dis_x86.lua`.
- **For cig: not applicable.** Not relevant to cig's tooling.

#### `jit/dis_arm64.lua` (1223 lines)

- **What it is.** An ARM64 disassembler contributed by Djordje Kovacevic and
  Stefan Pejic; design mirrors `jit/dis_x86.lua`.
- **For cig: not applicable.** Not relevant to cig's tooling.

#### `jit/dis_arm64be.lua` (12 lines)

- **What it is.** A wrapper that forwards to `jit/dis_arm64.lua`; ARM64
  instructions are always little-endian regardless of data endianness.
- **For cig: not applicable.** Wrapper only.

#### `jit/dis_mips.lua` (693 lines)

- **What it is.** A MIPS32 Release 1 and Release 2 disassembler, big-endian
  by default.
- **For cig: not applicable.** Not relevant to cig's tooling.

#### `jit/dis_mips64.lua` (17 lines)

- **What it is.** A wrapper that re-exports the big-endian decoder of
  `jit/dis_mips.lua`.
- **For cig: not applicable.** Wrapper only.

#### `jit/dis_mips64el.lua` (17 lines)

- **What it is.** A wrapper that re-exports the little-endian decoder of
  `jit/dis_mips.lua`.
- **For cig: not applicable.** Wrapper only.

#### `jit/dis_mips64r6.lua` (17 lines)

- **What it is.** A wrapper for MIPS64 Release 6 big-endian over
  `jit/dis_mips.lua`.
- **For cig: not applicable.** Wrapper only.

#### `jit/dis_mips64r6el.lua` (17 lines)

- **What it is.** A wrapper for MIPS64 Release 6 little-endian over
  `jit/dis_mips.lua`.
- **For cig: not applicable.** Wrapper only.

#### `jit/dis_mipsel.lua` (17 lines)

- **What it is.** A wrapper that re-exports the little-endian decoder of
  `jit/dis_mips.lua`.
- **For cig: not applicable.** Wrapper only.

#### `jit/dis_ppc.lua` (590 lines)

- **What it is.** A 32-bit and 64-bit PowerPC disassembler covering the
  common non-privileged instruction set, the e500 SPE instructions and Cell
  and Xenon extensions. VMX and VMX128 are not supported.
- **For cig: not applicable.** Not relevant to cig's tooling.

### The trace compiler: recording, the IR, the optimizations, snapshots

#### `lj_record.c` (2976 lines)

- **What it is.** The trace recorder. `lj_record_ins` is the dispatcher;
  `sload` and `sloadt` emit typed slot reads; `lj_record_constify` folds a
  slot to a constant; `lj_record_stop` tags the link; `rec_for`, `rec_iterl`
  and `rec_loop` are the loop recognizers; `rec_call_specialize`,
  `rec_call_setup`, `lj_record_call`, `lj_record_tailcall` and
  `lj_record_ret` record frames; `lj_record_mm_lookup` and `rec_mm_arith`,
  `rec_mm_len`, `rec_mm_equal`, `rec_mm_comp` handle metamethods;
  `lj_record_idx` and `lj_record_next` record indexed access; `rec_upvalue`,
  `rec_func_setup` and `rec_func_lua` record entries. `RecordIndex` is
  declared in `lj_record.h`.
- **For cig: adapt (see B1).** cig records nothing at run time. A cig
  `Op::CALL` targets a cig-level function and could be specialized on the
  callee reference, as `rec_call_specialize` does for Lua; `Op::CALLB`'s
  callee is already a static builtin index in `cig-script` and needs no
  specialization.

#### `lj_record.h` (47 lines)

- **What it is.** Header for `lj_record.c`. Declares `RecordIndex` (twelve
  fields: the runtime values `tabv`, `keyv`, `valv`, `mobjv`, `mtv`, `oldv`,
  and the `TRef` references `tab`, `key`, `val`, `mt`, `mobj`, plus
  `idxchain`) and the public API (`lj_record_objcmp`, `lj_record_stop`,
  `lj_record_constify`, `lj_record_vload`, `lj_record_call`,
  `lj_record_tailcall`, `lj_record_ret`, `lj_record_mm_lookup`,
  `lj_record_idx`, `lj_record_next`, `lj_record_ins`, `lj_record_setup`).
- **For cig: not applicable.** `RecordIndex` holds live-recorder `TValue`
  and `TRef` fields of LuaJIT's run; cig has no run-time recorder and no
  counterpart type (see B1).

#### `lj_trace.c` (1013 lines)

- **What it is.** Trace management. `trace_state` drives `trace_start`,
  `lj_trace_ins`, `trace_stop`, `trace_abort` and `lj_trace_exit`.
  `trace_findfree` picks a slot; `trace_save` compacts the trace.
  `lj_trace_flush` and `lj_trace_flushall` evict. `lj_trace_hot`,
  `trace_hotside`, `lj_trace_stitch` are the hot-trip entry points;
  `penalty_pc` and `blacklist_pc` are the abort and blacklist policy.
- **For cig: adapt (see B1 and B8).** A model for stage C's build-time
  compile loop (nothing today in cig-compile `src/cir/pass/`). The
  `lj_jit.h: HotPenalty` cache does not map to a build counter: an
  ahead-of-time retry sees the same input and fails the same way.

#### `lj_trace.h` (58 lines)

- **What it is.** Header for trace management. Defines `TraceError` through
  `lj_traceerr.h`; exposes `lj_trace_err`, `lj_trace_err_info`,
  `lj_trace_alloc`, `lj_trace_free`, `lj_trace_reenableproto`,
  `lj_trace_flushproto`, `lj_trace_flush`, `lj_trace_flushall`,
  `lj_trace_initstate`, `lj_trace_freestate`, `lj_trace_ins`,
  `lj_trace_hot`, `lj_trace_stitch`, `lj_trace_exit`, `lj_trace_unwind`,
  plus the macros `lj_trace_abort` and `lj_trace_end`.
- **For cig: reject (see B1 and B8).** The entry points are run-time JIT
  shaped; cig's rule against memory both written and executed at run time
  keeps these out, and the equivalents live inside cig-compile's build
  pipeline (`src/bc/mod.rs`).

#### `lj_traceerr.h` (61 lines)

- **What it is.** The `TREDEF` list of trace errors: recording (`RECERR`,
  `TRACEOV`, `SNAPOV`, `BLACKL`, `NYIBC`), loops (`LLEAVE`, `LINNER`,
  `LUNROLL`), calls (`BADTYPE`, `CUNROLL`, `DOWNREC`, `NYIFFU`), indexed
  access (`STORENN`, `NOMM`, `IDXLOOP`), FFI (`NOCACHE`, `NYICONV`),
  optimizations (`GFAIL`, `PHIOV`, `TYPEINS`) and the assembler (`MCODEAL`,
  `MCODEOV`, `SPILLOV`, `BADRA`, `NYIIR`, `NYIPHI`). Included twice with
  different `TREDEF` macros.
- **For cig: adapt (see B8).** The list is the model for the reasons a build
  keeps a region on the VM; today cig-compile names its refusals through
  `refused_kind` in `src/bc/mod.rs: refused_kind`, not a trace-error family.

#### `lj_ffrecord.c` (1670 lines)

- **What it is.** Fast-function recorder. The opening comment states the
  convention: argument slots start at the recorder's base, every one typed;
  a handler may lean on the interpreter to raise. `lj_ffrecord_func`
  dispatches by fast-function id through generated `lj_recdef.h` to a
  handler (`recff_math_abs`, `recff_math_unary`, `recff_string_range`,
  `recff_string_rep`, `recff_string_format`). `LJLIB_REC` tags pick the
  handler.
- **For cig: adapt (see B1).** The convention and the per-fast-function
  shape model a cig path record for a `CALLB`; cig's fast-call path today is
  `fast_call` in cig-script `src/vm/mod.rs`, which dispatches to builtins
  from `src/stdlib/`.

#### `lj_ffrecord.h` (24 lines)

- **What it is.** Header. `RecordFFData` carries `argv` (runtime arguments),
  `nres` (default 1) and `data` (per-function auxiliary). Two public
  functions: `lj_ffrecord_select_mode` and `lj_ffrecord_func`.
- **For cig: not applicable.** `RecordFFData` holds live-recorder `TValue`s;
  cig has no run-time recorder (see B1).

#### `lj_ir.c` (487 lines)

- **What it is.** IR emitter. `lj_ir_emit` writes the pending `IRIns` into
  the buffer and updates the per-opcode CSE chain. The constant-interning
  family (`lj_ir_kint`, `lj_ir_kgc`, `lj_ir_k64`, `lj_ir_knum_u64`,
  `lj_ir_kint64`, `lj_ir_kptr_`, `lj_ir_knull`, `lj_ir_kslot`) places
  constants below `REF_BIAS`. `lj_ir_growtop` grows the buffer.
  `lj_ir_rollback` undoes an emit. `lj_ir_tonum`, `lj_ir_tonumber` and
  `lj_ir_tostr` coerce operands. `lj_ir_kvalue` reads a constant back.
- **For cig: adapt (see B2).** The constant-interning design (one cached
  value per bit pattern) is a proposed extension to cig-compile
  `src/cir/ir.rs` (the `Const` enum there has no dedup cache today).

#### `lj_ir.h` (615 lines)

- **What it is.** IR layout and master opcode list. `IRDEF` enumerates every
  IR opcode. `IRType` is a byte over LuaJIT's value types with flag bits
  `IRT_MARK`, `IRT_ISPHI`, `IRT_GUARD`. `IRRef`, `IRRef1` and `TRef` are
  reference types; constants grow downwards from `REF_BIAS`, instructions
  upwards. `IRIns` is a 64-bit union.
- **For cig: not applicable.** CIR today is structured nested blocks
  (`src/cir/ir.rs: Kind::If`, `Kind::Loop`); a `TRef`-style
  reference-and-tag is a design to revisit if stage C needs a flat SSA.

#### `lj_ircall.h` (353 lines)

- **What it is.** `CCallInfo` and macros packing a C runtime call's shape
  (opcode, type, calling convention, argument count, variadic flag,
  soft-float extras) into a 32-bit word. `IRCALLDEF` enumerates every host
  call the IR can emit (for example `lj_str_new`, `lj_tab_dup`,
  `lj_tab_newkey`, `lj_buf_putstr`), each with its signature flags.
- **For cig: adapt (see B1).** Stage C needs a table like it for every host
  call (a built-in, a count write, a kernel entry); cig-compile has none
  today (`src/bc/mod.rs: lower` emits only cig opcodes).

#### `lj_iropt.h` (168 lines)

- **What it is.** The emitter and optimizer surface. The fold return codes
  (`NEXTFOLD`, `RETRYFOLD`, `KINTFOLD`, `FAILFOLD`, `DROPFOLD`) and helper
  names (`LEFTFOLD`, `RIGHTFOLD`, `CSEFOLD`, `EMITFOLD`) are the on-emit
  fold surface. The declarations cover fold, CSE, load forwarding,
  dead-store elimination, narrowing and the top-level passes `lj_opt_dce`,
  `lj_opt_loop`, `lj_opt_sink`, `lj_opt_split`.
- **For cig: adapt (see B2).** The five fold return codes are a model for a
  cig fold-rule surface; cig-compile's `Pass` trait in `src/cir/pass/mod.rs`
  has `name`, `run` and `run_remarking`, not an on-emit fold hook.

#### `lj_opt_fold.c` (2666 lines)

- **What it is.** The fold-and-CSE engine (see B2). The file holds hundreds
  of `LJFOLD(opname LEFT RIGHT)` rules driving `LJFOLDF(name)` functions
  (`kfold_numarith`, `kfold_numabsneg`, `kfold_ldexp`, `kfold_fpmath`,
  `kfold_numpow`, `kfold_numcomp`, `kfold_intarith`, and more).
  `lj_opt_fold` dispatches by hashing a 24-bit key of the pending opcode
  plus its operands' opcodes, masking from most-specific to least-specific;
  on a miss it calls `lj_opt_cse`.
- **For cig: adapt (see B2).** The declarative rule-table source (generated
  `lj_folddef.h` from `host/buildvm_fold.c`) is a cleaner form than
  per-opcode match arms; proposed model for a build-generated rule table
  beside cig-compile `src/cir/pass/fold_consts.rs: FoldConsts`.

#### `lj_opt_dce.c` (75 lines)

- **What it is.** Pre-loop dead-code elimination (see B2). `dce_marksnap`
  marks every snapshot's live references as roots; `dce_propagate` walks
  backwards and replaces unmarked instructions with `NOP` only when they are
  not side-effecting (`ir_sideeff` keeps guards and stores in). Runs only
  when loop optimization is on and the trace links to itself.
- **For cig: not applicable.** cig-compile `src/cir/pass/dead_pure.rs` is a
  different pass: it removes a `Class::Pure` instruction whose result has no
  use (`find_dead`, `prune`); it has no root set and never touches
  `Class::Effect`, `MayRaise` or `Step`.

#### `lj_opt_loop.c` (453 lines)

- **What it is.** The copy-substitution loop pass (see B3). The opening
  comment is the design. `loop_emit_phi` emits PHIs for loop-carried values;
  `loop_subst_snap` rewrites snapshots; `loop_unroll` re-emits against a
  substitution table; `loop_undo` rolls back on failure; `lj_opt_loop` is
  the entry point.
- **For cig: adapt (see B3).** Stage C's loop pass is designed from this.
  CIR has no branch, guard or PHI, and its pass rules forbid cloning a
  turn's effects (T3 in cig-compile's `src/cir/pass/mod.rs`), so the first
  copy is made below CIR, in stage C's lowering.

#### `lj_opt_mem.c` (1018 lines)

- **What it is.** Alias analysis, load forwarding and dead-store elimination
  (see B5). The three-valued `AliasRet` and the shape-aware oracles
  (`aa_table`, `aa_ahref`, `aa_uref`, `aa_fref`, `aa_cnew`, `aa_xref`) feed
  the forwarders and the dead-store eliminators. `reassoc_trycse` and
  `reassoc_xref` reassociate pointer arithmetic.
- **For cig: adapt (see B5).** cig-compile `src/bc/mod.rs` already does a
  limited form (`alias`, `unloaded`, `hoist`); a cross-store forwarder is
  proposed on top.

#### `lj_opt_narrow.c` (614 lines)

- **What it is.** Narrowing and overflow-check stripping (see B4).
  `lj_opt_narrow_forl` picks an integer type for a `FORL` with integer
  bounds; `lj_opt_narrow_index` narrows demand-side; `lj_opt_narrow_arith`
  emits `ADDOV`/`SUBOV` in the default build only when the recorded values
  fit. `narrow_stripov` and `narrow_stripov_backprop` strip the checks where
  the consumer is an index, a bit op or a `TOBIT`.
- **For cig: adapt (see B4).** cig's integers are already integers
  (`Value::Int`); a cig overflow-strip would run only where cig-compile's T4
  proves the operation cannot raise.

#### `lj_opt_sink.c` (258 lines)

- **What it is.** Allocation sinking (see B6). `sink_checkalloc` tests
  eligibility; `sink_mark_ins` marks ineligible allocations;
  `sink_mark_snap` marks the last snapshot's references as roots when the
  trace is not a loop (a marked reference is kept); `sink_sweep_ins` tags
  sunk allocations by writing `prev` to the `RID_SINK` slot; `lj_opt_sink`
  is the entry point.
- **For cig: adapt (see B6).** cig has no sinking pass today; a proposed
  pass over `Op::NEWLIST` and `Op::NEWMAP` in cig-script `src/vm/mod.rs`. No
  cig `Value` has an observable `Drop`, so no drop-with-effect class is
  needed.

#### `lj_opt_split.c` (843 lines)

- **What it is.** The 64-bit split pass for 32-bit and soft-float targets. A
  64-bit IR instruction is replaced with a 32-bit one plus an `HIOP`
  carrying the high-word operands and result; soft-float arithmetic is
  rewritten as runtime calls. The pass runs between the main optimizations
  and the assembler.
- **For cig: not applicable.** The split serves 32-bit and soft-float
  targets; stage C is planned on Cranelift for x86_64 and aarch64
  (cig-compile's `docs/decisions/0001-backend.md`).

#### `lj_snap.c` (1034 lines)

- **What it is.** Snapshot and exit (see B7). `lj_snap_add` takes a new
  snapshot; `snap_usedef` is the reaching-definitions walk; `lj_snap_purge`
  and `lj_snap_shrink` remove dead slots. `snap_renameref` handles renames;
  `snap_sunk_store` identifies stores for a sunk allocation.
  `lj_snap_replay` seeds a side trace. `snap_unsink` rebuilds sunk
  allocations; `snap_restoreval` fills a slot; `lj_snap_restore` is the
  top-level exit.
- **For cig: adapt (see B7).** cig has no snapshot today; the proposed cig
  snapshot ties each field to state the VM already holds in `Machine`
  (`src/vm/mod.rs`) and `Interp` (`src/interp/mod.rs`).

#### `lj_snap.h` (35 lines)

- **What it is.** Snapshot header. Public API: `lj_snap_add`,
  `lj_snap_purge`, `lj_snap_shrink`, `lj_snap_regspmap`, `lj_snap_replay`,
  `lj_snap_restore`, `lj_snap_grow_buf_`, `lj_snap_grow_map_`, and the
  inlined wrappers `lj_snap_grow_buf` and `lj_snap_grow_map`.
- **For cig: adapt (see B7).** The declared surface is the proposed shape of
  cig's internal snapshot module.

#### `lj_jit.h` (529 lines)

- **What it is.** Shared JIT engine definitions. The `JIT_F_*` flags and
  `-O` levels; the `JIT_PARAMDEF` tunables (`hotloop`, `hotexit`,
  `maxtrace`, `maxrecord`, `maxside`, `maxsnap`, `instunroll`, `sizemcode`,
  `maxmcode`); `TraceState`; `PostProc` modes; `SnapShot` (12 bytes) and
  `SnapEntry` with `SNAP_FRAME`, `SNAP_CONT`, `SNAP_NORESTORE`,
  `SNAP_KEYINDEX`; `TraceLink` values; `GCtrace`; `HotPenalty` with
  `PENALTY_SLOTS`, `PENALTY_MAX`; `BPropEntry`; `FoldState`; `jit_State`.
- **For cig: adapt (see B1, B7, B8).** `JIT_PARAMDEF` is the model for stage
  C tunables (cig-compile has none today); `GCtrace`'s fields show what
  metadata a cig compile unit would carry; `SnapShot` is the shape cig's own
  snapshot is sized against.

### The back end and the FFI

**The back end**

#### `lj_asm.c` (2644 lines)

- **What it is.** The target-neutral driver of the IR to machine-code pass.
  `lj_asm_trace` sets up `ASMState`, reserves memory through
  `lj_mcode_reserve`, lays out the tail, then walks the IR from the last
  reference to the first calling `asm_ir`. The allocator and rematerializer
  are in this file: `ra_setup`, `ra_rematk`, `ra_spill`, `ra_restore`,
  `ra_evict`, `ra_allocref`, `ra_alloc1`. Snapshot keep-alive
  (`asm_snap_alloc`, `asm_snap_prep`), loop head and tail (`asm_loop`,
  `asm_head_root`, `asm_head_side`, `asm_tail_link`, `asm_tail_prep`), PHIs
  (`asm_phi_shuffle`, `asm_phi_copyspill`, `asm_phi_fixup`), allocation
  stubs (`asm_snew`, `asm_tnew`, `asm_tdup`) and the GC check
  (`asm_gc_check`, `asm_gcstep`) sit here too. `asm_mclimit` traps a buffer
  overrun.
- **For cig: reject.** See B9. The backward walk, the embedded allocator and
  the per-target `asm_ir` dispatch are not taken; stage C is planned on
  Cranelift (cig-compile's `docs/decisions/0001-backend.md`).

#### `lj_asm.h` (17 lines)

- **What it is.** Public header: declarations of `lj_asm_trace` and
  `lj_asm_patchexit` (used by `lj_trace.c` to redirect a hot side exit into
  a side trace).
- **For cig: not applicable.** Two declarations behind `LJ_HASJIT`; see B9
  for the verdict on what they drive.

#### `lj_asm_x86.h` (3165 lines)

- **What it is.** The x86 and x64 half of the assembler, included into
  `lj_asm.c`. Guard handling: `asm_exitstub_gen` lays `EXITSTUBS_PER_GROUP`
  stubs through `lj_vm_exit_handler`, `asm_exitstub_setup`, and
  `asm_guardcc`, which inverts the guard at the loop branch. Memory-operand
  fusion: `asm_fuseabase`, `asm_fusearef`, `asm_fuseahuref`, `asm_fusefref`,
  `asm_fusestrref`, `asm_fusexref`, `asm_fuseloadk64`, `asm_fuseload`.
  Per-IR handlers: `asm_href`, `asm_hrefk`, the macros `asm_fload`,
  `asm_xload`, `asm_fstore` and `asm_xstore`, the arithmetic and compare
  families. Target setup: `asm_setup_target`. The port's `lj_asm_patchexit`
  lives here too.
- **For cig: reject.** See B9. Operand fusion and the inverted loop branch
  are instruction selection, which stage C would leave to Cranelift
  (planned: cig-compile's `docs/decisions/0001-backend.md`).

#### `lj_emit_x86.h` (590 lines)

- **What it is.** The byte-level x86 and x64 emitter. The stream writers
  `emit_i8` and `emit_i32` push into `as->mcp` and move it down, so the
  buffer grows downward. Composite emitters: `emit_op`, `emit_rr`,
  `emit_rmro`, `emit_mrm`, `emit_loadi`, `emit_loadu64`, `emit_loadk64`,
  `emit_jcc`, `emit_sjcc`. The `MODRM(mode, r1, r2)` and `REXRB(p, rr, rb)`
  macros build the mod-r/m byte and the REX prefix.
- **For cig: reject.** Hand-written x86 encoding is what Cranelift would
  replace (planned: cig-compile's `docs/decisions/0001-backend.md`).

#### `lj_target.h` (164 lines)

- **What it is.** Target-neutral register and spill definitions. The
  register id type `Reg`, the high-bit encoding (`RID_NONE`, `RID_MASK`,
  `RID_INIT`, `RID_SINK`, `RID_SUNK`), the hint helpers (`ra_noreg`,
  `ra_hasreg`, `ra_hashint`, `ra_gethint`, `ra_sethint`, `ra_samehint`),
  register-set primitives (`rset_set`, `rset_clear`, `RSET_RANGE`,
  `RID2RSET`), spill-slot helpers (`SPS_NONE`, `ra_hasspill`), and the
  include of the per-arch header.
- **For cig: reject.** See B9. The hint, sink and sunk bits are what the
  backward walk and rematerializer in `src/lj_asm.c` read and write.

#### `lj_target_x86.h` (361 lines)

- **What it is.** x86 and x64 target description. Register ids are built by
  `GPRDEF` and `FPRDEF`, with pseudo-ids `RID_MRM` and `RID_RIP` and the
  calling-convention aliases (`RID_SP`, `RID_RET`, `RID_FPRET`, `RID_BASE`,
  `RID_LPC`, `RID_DISPATCH`). Register sets: `RSET_GPR`, `RSET_FPR`,
  `RSET_ALL`, `RSET_INIT`, `RSET_SCRATCH`, `RSET_ACD`. Spill layout:
  `SPS_FIXED`, `SPS_FIRST`, `SPOFS_TMP`, `sps_scale`, `sps_align`. The
  `ExitState` struct (`fpr`, `gpr`, `spill`), exit-stub layout
  (`EXITSTUB_SPACING`, `EXITSTUBS_PER_GROUP`), and the x86 opcode builders
  `XO_`, `XO_FPU`, `XO_0f`, `XO_66`, `XO_660f`, `XO_f20f`, `XO_f30f`.
- **For cig: reject.** The `ExitState` layout is the shape B7's cig snapshot
  restore writes back; see B9.

#### `lj_mcode.c` (470 lines)

- **What it is.** Memory for machine code. `mcode_alloc_at` maps anonymous
  pages with `MCPROT_GEN`, which is `MCPROT_RW` by default;
  `mcode_allocarea` wraps one region; `mcode_protect` and `mcode_setprot`
  flip the live area between `MCPROT_RW` and `MCPROT_RX`; with
  `LUAJIT_SECURITY_MCODE` off the region is `MCPROT_RWX` with no flip.
  `lj_mcode_reserve`, `lj_mcode_commit`, `lj_mcode_abort`, `lj_mcode_patch`
  frame the lifecycle. `lj_mcode_sync` flushes caches on non-x86.
  `lj_mcode_free` returns the pages. `lj_mcode_limiterr` aborts the trace.
- **For cig: reject.** See B9. Memory written then executed in one process
  is the security risk the file itself calls out.

#### `lj_mcode.h` (30 lines)

- **What it is.** Public prototypes: `lj_mcode_sync`, `lj_mcode_free`,
  `lj_mcode_reserve`, `lj_mcode_commit`, `lj_mcode_abort`, `lj_mcode_patch`,
  `lj_mcode_limiterr`, plus the macro `lj_mcode_commitbot`.
- **For cig: not applicable.** Public interface of the rejected
  `lj_mcode.c`.

#### `lj_gdbjit.c` (818 lines)

- **What it is.** Client for the GDB JIT API, compiled in only with
  `LUAJIT_USE_GDBJIT`. Each compiled trace gets a symbol name, a `.text`
  section wrapping its machine code, and minimal DWARF unwind info, handed
  to GDB through the published registration symbol. Carries non-negligible
  overhead even when nothing is attached. Depends on the pages `lj_mcode.c`
  manages.
- **For cig: reject.** See B9. Presumes JIT-compiled code at run time, which
  cig-compile's `docs/decisions/0001-backend.md` does not plan.

#### `lj_gdbjit.h` (22 lines)

- **What it is.** Two declarations: `lj_gdbjit_addtrace` and
  `lj_gdbjit_deltrace`. Stubbed to `UNUSED(T)` without `LUAJIT_USE_GDBJIT`.
- **For cig: not applicable.** Public interface of the above.

**Back-end twins**

#### `lj_asm_arm.h` (2345 lines)

- **What it is.** The twin of `lj_asm_x86.h` for 32-bit ARM: fixed 32-bit
  encoding, a link register as `RID_TMP`, an optional `LJ_SOFTFP` soft-float
  path. Carries its own `lj_asm_patchexit`.
- **For cig: reject.** See B9.

#### `lj_asm_arm64.h` (2101 lines)

- **What it is.** The twin for AArch64: `emit_loadk` in
  `src/lj_emit_arm64.h` has several paths for a 64-bit constant
  (`emit_kdelta`, `emit_kadrp`, and `MOVZ`/`MOVK`/`MOVN` through
  `A64I_MOVZw`, `A64I_MOVKw`, `A64I_MOVNw`, plus LDR literal); link register
  as `RID_TMP`.
- **For cig: reject.** See B9.

#### `lj_asm_ppc.h` (2338 lines)

- **What it is.** The twin for 32-bit PowerPC: link register through
  `PPCI_BL`, a soft-float build gated by the `LJ_SOFTFP` guards in the file,
  same branch reach as ARM.
- **For cig: reject.** See B9. Not a cig target.

#### `lj_asm_mips.h` (2821 lines)

- **What it is.** The twin for MIPS (MIPS32 and MIPS64, both endians, hard
  and soft float). Soft-float plumbing under `LJ_SOFTFP`; the MIPSR6 build
  uses the compact `MIPSI_BALC` call, with delay-slot rules kept in
  `lj_emit_mips.h` for the pre-R6 forms.
- **For cig: reject.** See B9. Not a cig target.

#### `lj_emit_arm.h` (366 lines)

- **What it is.** The twin of `lj_emit_x86.h` for 32-bit ARM. Fixed 32-bit
  encodings (`ARMI_*`), `LJ_SOFTFP` guards, `ARMI_BL` for a relative call.
- **For cig: reject.** See B9.

#### `lj_emit_arm64.h` (480 lines)

- **What it is.** The twin for AArch64: `A64I_*` encodings; `emit_loadk`
  tries a 13-bit ORR immediate, `emit_kdelta` off another constant,
  `emit_kadrp` (`A64I_ADR` or `A64I_ADRP` plus optional `ADD`), and falls
  back to `MOVZ`/`MOVN` with `MOVK` fragments.
- **For cig: reject.** See B9.

#### `lj_emit_mips.h` (313 lines)

- **What it is.** The twin for MIPS: `MIPSI_*` encodings with delay-slot
  filling (`MIPSI_NOP` in the delay slot of `MIPSI_JAL`/`MIPSI_JALR`); on
  the `LJ_TARGET_MIPSR6` build, the compact `MIPSI_BALC` replaces the
  delay-slot forms.
- **For cig: reject.** See B9.

#### `lj_emit_ppc.h` (241 lines)

- **What it is.** The twin for 32-bit PowerPC: `PPCI_*` encodings,
  link-register call `PPCI_BL`, fixed branch forms.
- **For cig: reject.** See B9.

#### `lj_target_arm.h` (273 lines)

- **What it is.** The twin of `lj_target_x86.h` for ARM: `RID_R0` through
  `RID_PC`, `RID_TMP` set to `RID_LR`, `LJ_SOFTFP` branches removing FPR
  ids, and `EXITSTATE_PCREG` set to `RID_PC` so `lj_trace.c` reads the
  exiting PC out of the `ExitState` GPR slot to find the trace.
- **For cig: reject.** See B9.

#### `lj_target_arm64.h` (351 lines)

- **What it is.** The twin for AArch64: 31 GPRs plus `RID_SP`, `RID_LR` as
  `RID_TMP`, 32 FPRs; fixed reserved registers for frame pointer, link
  register and stack pointer kept out of the allocator.
- **For cig: reject.** See B9.

#### `lj_target_ppc.h` (281 lines)

- **What it is.** The twin for 32-bit PowerPC: 32 GPRs, 32 FPRs, an
  interpreter `L` kept in one GPR, condition-register pieces.
- **For cig: reject.** See B9. Not a cig target.

#### `lj_target_mips.h` (417 lines)

- **What it is.** The twin for MIPS32 and MIPS64: heavy `LJ_SOFTFP`
  branching, `RID_ZERO` for the always-zero register, an interpreter `L`
  kept in one GPR.
- **For cig: reject.** See B9. Not a cig target.

**The FFI**

#### `lib_ffi.c` (860 lines)

- **What it is.** The script-visible `ffi` library. `ffi_checkctype` parses
  a C type string through `lj_cparse` into the shared `CTState`. The
  exported entries (`ffi_cdef`, `ffi_new`, `ffi_cast`, `ffi_typeof`,
  `ffi_sizeof`, `ffi_alignof`, `ffi_offsetof`, `ffi_string`, `ffi_copy`,
  `ffi_fill`, `ffi_errno`, `ffi_abi`, `ffi_load`, `ffi_metatype`, `ffi_gc`)
  wrap the pieces beside it: cdata allocation goes through `lj_cdata`,
  conversions through `lj_cconv`, calls through `lj_ccall`, arithmetic
  metamethods through `lj_carith`, shared libraries through `lj_clib`.
- **For cig: reject.** See the FFI's entry above.

#### `lj_cparse.c` (1934 lines)

- **What it is.** The C declaration parser, hand-written recursive descent.
  Its header comment states it is not a validating parser: it may accept
  some invalid declarations and return nonsense. The output is entries in
  `CTState`. Entry points: `lj_cparse` (one declaration), with modes
  `CPARSE_MODE_ABSTRACT` and `CPARSE_MODE_NOIMPLICIT`.
- **For cig: reject.** See the FFI's entry. cig has no parser for foreign
  type annotations at run time.

#### `lj_cparse.h` (67 lines)

- **What it is.** `CPState`, parser mode flags, the limits `CPARSE_MAX_BUF`,
  `CPARSE_MAX_DECLSTACK`, `CPARSE_MAX_DECLDEPTH`, `CPARSE_MAX_PACKSTACK`,
  and the `lj_cparse` entry.
- **For cig: not applicable.** Header for the rejected parser.

#### `lj_ctype.c` (673 lines)

- **What it is.** The C type table. `CTTDDEF` holds the stdint and stddef
  aliases. `lj_ctype_new` and `lj_ctype_intern` add types; `lj_ctype_repr`
  prints one; the namespace is a hash-consed table on `CTypeID`.
- **For cig: reject.** See the FFI's entry. cig has no type intern pool and
  no C-type table.

#### `lj_ctype.h` (485 lines)

- **What it is.** The C type encoding. The `CTInfo` bitfield (kind,
  qualifiers, size), the `CType` struct, the `CTState` struct, and the kind
  enumeration: `CT_NUM`, `CT_STRUCT`, `CT_PTR`, `CT_ARRAY`, `CT_FUNC`,
  `CT_TYPEDEF`, `CT_FIELD`, with predicates `ctype_isnum`, `ctype_isstruct`,
  `ctype_isptr`, `ctype_isarray`, `ctype_isfunc`, `ctype_istypedef`.
- **For cig: not applicable.** Header for the type table.

#### `lj_cdata.c` (299 lines)

- **What it is.** C data as script-visible objects. `lj_cdata_newv` makes a
  variable-size or specially aligned one with an aligned header;
  `lj_cdata_newref` wraps a pointer to another object; `lj_cdata_newx` picks
  between the two; `lj_cdata_free` releases one.
- **For cig: reject.** See the FFI's entry.

#### `lj_cdata.h` (79 lines)

- **What it is.** Inline helpers `cdata_getptr` and `cdata_setptr`; the
  inline fixed-size allocators `lj_cdata_new` and `lj_cdata_new_`;
  declarations for `lj_cdata_newref`, `lj_cdata_newv`, `lj_cdata_newx`,
  `lj_cdata_free`, `lj_cdata_setfin`, `lj_cdata_index`, `lj_cdata_get`,
  `lj_cdata_set`. The `GCcdata` struct itself lives in `lj_obj.h`.
- **For cig: not applicable.** Header for the above.

#### `lj_cconv.c` (770 lines)

- **What it is.** Conversions between a TValue and a C type and between two
  C types. `cconv_err_conv` and `cconv_err_convtv` report a bad conversion.
  `lj_cconv_tv_ct`, `lj_cconv_ct_tv`, `lj_cconv_ct_ct` are the three
  directions, keyed on the compressed index `CCX_*` and the flag word
  `CCF_*`.
- **For cig: reject.** See the FFI's entry. The one-switch-over-`CCX_*`
  shape keeps the conversion matrix linear.

#### `lj_cconv.h` (71 lines)

- **What it is.** The `CCX_*` compressed index, the `CCF_*` flag word, and
  the `lj_cconv_*` prototypes.
- **For cig: not applicable.** Header for the above.

#### `lj_ccall.c` (1263 lines)

- **What it is.** The C-call shim. Per-ABI blocks (`LJ_TARGET_X86`,
  `LJ_TARGET_X64` with `LJ_ABI_WIN` and without, and the ARM, ARM64, PPC,
  MIPS paths) define `CCALL_HANDLE_STRUCTRET`, `CCALL_HANDLE_STRUCTARG`,
  `CCALL_HANDLE_COMPLEXRET`, `CCALL_HANDLE_COMPLEXARG` and
  `CCALL_HANDLE_REGARG`. `ccall_set_args` moves arguments into GPRs, FPRs
  and the stack per ABI; `ccall_get_results` reads the return.
  `lj_ccall_func` sets up the call and hands off to the assembly symbol
  `lj_vm_ffi_call` (declared in `src/lj_ccall.h`).
- **For cig: reject.** See the FFI's entry.

#### `lj_ccall.h` (201 lines)

- **What it is.** The `CCallState` layout and per-ABI constants:
  `CCALL_NARG_GPR`, `CCALL_NARG_FPR`, plus the `lj_ccall_func` and
  `lj_vm_ffi_call` declarations.
- **For cig: not applicable.** Header for the above.

#### `lj_ccallback.c` (839 lines)

- **What it is.** C calling back into Lua. `callback_mcode_new` makes the
  callback page once, sized `CALLBACK_MCODE_SIZE`: `mmap` as writable,
  `callback_mcode_init` writes the per-target trampolines that push the slot
  id and jump to the assembly symbol `lj_vm_ffi_callback`, and one
  `mprotect` turns the page read-execute. `lj_ccallback_new` only reserves a
  slot and records the Lua function; `lj_ccallback_enter` and
  `lj_ccallback_leave` marshal arguments in and the result out.
- **For cig: reject.** See the FFI's entry and B9. The page is memory
  written, then executed in the same process.

#### `lj_ccallback.h` (25 lines)

- **What it is.** Declarations for `lj_ccallback_ptr2slot`,
  `lj_ccallback_enter`, `lj_ccallback_leave`, `lj_ccallback_new`;
  declaration of the assembler symbol `lj_vm_ffi_callback`.
- **For cig: not applicable.** Header for the above.

#### `lj_clib.c` (434 lines)

- **What it is.** `ffi.load` and `ffi.C`. On Linux, BSD and macOS, `dlopen`
  and `dlsym` behind `CLIB_DEFHANDLE` (which maps to `RTLD_DEFAULT` or a
  sentinel); on Windows, the `LJ_WIN_LOADLIBA` wrapper and `GetProcAddress`
  through `GetModuleHandleExA`. `clib_error_` surfaces the loader's
  last-error string as a Lua error.
- **For cig: reject.** See the FFI's entry. This is where the
  verified-module boundary dissolves: foreign code that nothing in cig has
  hashed can run on behalf of the script.

#### `lj_clib.h` (29 lines)

- **What it is.** The `CLibrary` struct (opaque handle and symbol cache),
  the `CLNS_INDEX` namespace mask, and declarations for `lj_clib_load`,
  `lj_clib_unload`, `lj_clib_default`, `lj_clib_index`.
- **For cig: not applicable.** Header for the above.

#### `lj_carith.c` (465 lines)

- **What it is.** C-data arithmetic metamethods. `CDArith` holds two operand
  pointers and types; `carith_checkarg` reads them off the stack;
  `carith_ptr` and `carith_int64` carry pointer arithmetic and 64-bit
  integer arithmetic; `lj_carith_op` dispatches by `MMS`. Also here:
  `lj_carith_check64`, `lj_carith_checkbit64`, `lj_carith_shift64`, and the
  `lj_carith_divu64`, `lj_carith_divi64`, `lj_carith_modu64`,
  `lj_carith_modi64`, `lj_carith_powu64`, `lj_carith_powi64` helpers.
- **For cig: reject.** See the FFI's entry.

#### `lj_carith.h` (38 lines)

- **What it is.** Declarations of `lj_carith_op`, `lj_carith_shift64`,
  `lj_carith_check64`, `lj_carith_checkbit64` and the 64-bit
  `lj_carith_divu64`, `lj_carith_divi64`, `lj_carith_modu64`,
  `lj_carith_modi64`, `lj_carith_powu64`, `lj_carith_powi64` helpers; the
  shift helpers `lj_carith_shl64`, `lj_carith_shr64`, `lj_carith_sar64`,
  `lj_carith_rol64`, `lj_carith_ror64` are declared only under the 32-bit
  guard, and `lj_carith_mul64` only under the 32-bit JIT guard.
- **For cig: not applicable.** Header for the above.

#### `lj_crecord.c` (2024 lines)

- **What it is.** The trace recorder for C-data operations. Every FFI entry
  is translated into IR so the JIT can inline the call and see through the
  struct: `recff_cdata_index`, `recff_cdata_call`, `recff_cdata_arith`,
  `recff_clib_index`, `recff_ffi_new`, `recff_ffi_errno`,
  `recff_ffi_string`, `recff_ffi_copy`, `recff_ffi_fill`,
  `recff_ffi_typeof`, `recff_ffi_istype`. `crec_tv_ct` is the conversion
  recorder.
- **For cig: reject.** See the FFI's entry. The record-trace shape that B6
  would need against the same cig programs is proposed, not here today.

#### `lj_crecord.h` (46 lines)

- **What it is.** The `recff_*` prototypes for every FFI recorder.
- **For cig: not applicable.** Header for the above.

## Measurements owed

Each is for the test machine, on a release build, and gets its run id here
when it is taken.

- **The call path, one item at a time** (A2): call and return inside the
  fast loop; then without the clone and release of the callee and its
  upvalues; then with a smaller frame record. Each on the call micro program
  (900 machine instructions today) and on `fib`.
- **The step charge fused** (A1, A8): `steps` fused with a loop's back edge
  and with a conditional jump, and the verifier's cycle rule extended to
  accept those forms as charging; the instructions `loop` and `fib` execute,
  and their time.
- **The other instruction forms of A1**, each alone: a left constant for
  `-`, `/`, `%`, `<` and `<=`; a compare-and-jump against a constant; a
  small-integer key in the index instructions; a run of nulls in one load.
- **LuaJIT's interpreter by handler** (A3): its handlers have no symbols, so
  a count by function sees the whole interpreter as one block. Owed: a count
  by address range against the labels of the generated assembly, beside
  `run_fast`'s arms for the same bytecode.
- **The hottest builtins as arms of their own** (A4): `.len()`, `.has()`,
  `.push()` and a map write first, then `math.sqrt`, `math.floor`,
  `math.abs`, `math.pow`, `float()` and `int()`; and which builtins cigo's
  program calls.
- **A turn of `for .. in` inside the fast loop** (A4, A8): `ITERPREP` and
  `ITERNEXT` as arms of the fast loop; a turn is 717 machine instructions
  today.
- **A field read's 88 instructions** (A5, A6): how many are the tag test,
  how many the cache check, how many the key's comparison.
- **Interning** (A6): builds that intern every string, short strings only,
  or none (only a key's hash kept), on the string programs and on cigo.
- **The counter's cost** (A8): LuaJIT's is read from its source (five
  instructions a loop turn), not measured; owed is its counting dispatch
  against its non-counting one.
- **Five Lua reference programs** print differently from cig's (`deltablue`,
  `json-deep`, `regex-ish`, `richards`, `strbuild`), so there is no
  executed-instruction count for them yet.
- **Part 1B** has no measurement of its own. Each of B1 to B8 names the
  first experiment stage C would run.
