# cig-native

What a cig program and the compiler that built it agree on about native
code: the `native` section's layout, the context native code shares with
the VM, and the loader that maps a section's code from the program's own
file and calls it.

A program built at stage `c` is a stage `b` program (its script as
bytecode, run by the VM) with one more section, `native`: machine code
for some of its functions, each compiled for one row of argument types.
The VM calls an entry when a call's arguments are of its row. It runs
the bytecode otherwise, and whenever the code cannot be loaded. The
design is cig-compile's decisions
[0005](https://github.com/otmof-ops/cig-compile/blob/main/docs/decisions/0005-what-goes-native-first.md),
[0006](https://github.com/otmof-ops/cig-compile/blob/main/docs/decisions/0006-native-code-in-a-program.md)
and
[0007](https://github.com/otmof-ops/cig-compile/blob/main/docs/decisions/0007-where-the-pieces-live.md).

The compiler writes a section with this crate and the runtime reads it
with this crate, so the two cannot disagree about a byte. It depends on
`libc`, on the targets that have a loader, and on nothing else.

## The layout

A section's body, every integer little-endian:

```text
code           the machine code, from the body's first byte
zero padding   to a multiple of 8
entry table    count x 24 bytes
site table     count x 24 bytes
footer         80 bytes, at the body's end
```

**The footer**

| offset | size | field |
|---:|---:|---|
| 0 | 8 | magic, ASCII `CIGNATV1` |
| 8 | 4 | format version: `1` |
| 12 | 4 | architecture: `1` x86-64, `2` aarch64 |
| 16 | 4 | interface version: `1` (the context, the entry convention, the frame bound) |
| 20 | 4 | code length |
| 24 | 4 | entry count |
| 28 | 4 | site count |
| 32 | 32 | the BLAKE3 in the header of the bytecode module the code was compiled from |
| 64 | 16 | zero |

**An entry:** one function compiled for one row of argument types.

| offset | size | field |
|---:|---:|---|
| 0 | 4 | the function's index in the module |
| 4 | 4 | the offset of its entry in the code |
| 8 | 1 | argument count, at most 8 |
| 9 | 1 | result type |
| 10 | 2 | zero |
| 12 | 8 | argument types, the unused ones zero |
| 20 | 4 | zero |

A type is `0` null, `1` bool, `2` int, `3` float. Entries are sorted by
function index and then by argument types, no two alike; every entry of
one function has the same argument count, and a function has at most 16. An entry's offset is inside
the code, and where instructions have one size (aarch64: 4 bytes) it and
the code's length are multiples of it.

**A site:** a place native code can leave from.

| offset | size | field |
|---:|---:|---|
| 0 | 4 | where the instruction starts in the source |
| 4 | 4 | where it ends |
| 8 | 4 | its line |
| 12 | 4 | its column |
| 16 | 1 | kind: why code leaves there |
| 17 | 1 | operator |
| 18 | 1 | the left operand's type |
| 19 | 1 | the right operand's type |
| 20 | 1 | flags: bit 0, the place is inside a loop of its function |
| 21 | 3 | zero |

A site says only what its kind uses, and every other field is zero:

| kind | when | operator | left | right |
|---|---|---|---|---|
| `1` operator | `+ - *` overflowed | `0` `1` `2` | int | int |
| | `%` by zero | `4` | int | int |
| | `/` by zero, or a NaN in `< <= > >=` | `3`, `7` to `10` | int or float | int or float, not both ints |
| `2` negate | unary `-` overflowed | 0 | int | null |
| `3` range | a range wider than the limit | 0 | int | int |
| `4` budget | the step budget is spent | 0 | null | null |
| `5` depth | the call depth is reached | 0 | null | null |

**There is one encoding of a section.** The writer and the reader apply
the same rules. What `Section::to_bytes` writes, `Section::parse` reads
back equal; what `parse` takes, `to_bytes` writes back as the same
bytes. A body is hostile data to the reader: it never panics, never
reads out of bounds and allocates nothing the body's own length does not
bound.

## The context and the entry convention

An entry has one signature on every target, in the target's C
convention:

```rust
extern "C" fn(ctx: *mut Context, args: *const u64, ret: *mut u64) -> u32
```

`args` is one 64-bit slot an argument (an int, a float's bits, a bool as
0 or 1, zero for null), `ret` one slot for the result, and the status is
0 or the kind of exit. `Context` is 64 bytes:

| offset | size | field |
|---:|---:|---|
| 0 | 8 | `steps_left`: steps before the budget is spent. Code takes one at each step mark and leaves when that took it below zero, with -1 here |
| 8 | 8 | `depth_left`: how deep the code's own calls may nest |
| 16 | 4 | `site`: on an exit, the site it left from |
| 20 | 4 | `flags`: on an exit, bit 0 when a function on the way out was called from inside a loop |
| 24 | 8 | `left`: on an exit, the left operand's slot |
| 32 | 8 | `right`: on an exit, the right operand's slot |
| 40 | 24 | zero |

One call of any function in a section uses at most `MAX_FRAME` (4,096)
bytes of stack, so a caller that sets `depth_left` knows how much stack
it is granting.

## The loader, and who answers for what

No check makes bytes safe to run. The loader separates what it can do
safely from what its caller has to answer for:

1. **`Mapped::map` is safe.** It maps a section's code, and only the
   code, read and execute and private. It gives back the mapping's
   length and nothing else: no reference into the bytes, no way to run
   them. The mapping is never writable, never anonymous and never
   changed with `mprotect`.
2. **`Mapped::arm` is `unsafe`.** Its caller vouches that the bytes are
   the code generator's for this machine, and that the file will not
   change while the code can be called. `arm` checks what can be
   checked: the mapping holds exactly the bytes the caller read and
   verified, and every entry lies in them, on an instruction boundary.
3. **`Code::call` is safe.** It takes an entry's index and exactly as
   many argument slots as that entry reads, or it runs nothing.

A cig program's evidence for step 2 is where the bytes came from: a
section of its own executable, whose sha256 its section table gives and
whose footer names the module it was compiled from. Whoever can write
that file can already replace the runtime in it.

Every `unsafe` block is in `src/loader.rs`: the `mmap`, the comparison
with what was read, the call and the `munmap`.

**Targets.** Linux on x86-64 and aarch64. On every other target the
crate compiles, the format and the context are whole, and `Mapped::map`
returns `MapError::Unsupported`.

**When the system refuses.** A `noexec` mount or a policy that reports
an error gives `MapError::Refused`, and a program runs its bytecode. A
seccomp filter that kills a process for mapping executable memory kills
it here too: nothing can fall back from that.

## Tests

`cargo test -p cig-native` pins the layout byte for byte against the
tables above, holds the writer and the reader to every rule, changes
valid bodies at random, and on Linux runs real machine code through the
loader. `fuzz/` holds a `cargo fuzz` target for the reader.
