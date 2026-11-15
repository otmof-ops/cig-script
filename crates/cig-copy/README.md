# cig-copy

`memcpy` and `memmove` for cig's static build on x86_64.

A release of cig for Linux is a static binary on musl. musl's `memcpy` for
x86_64 is the string instruction (`rep movsq`), which costs tens of cycles
to start however little it copies, and cig copies little, often: a 24-byte
value, a short string, a handful of arguments. These routines have no
start-up cost:

- a size up to 128 bytes is copied with loads that overlap, every one made
  before the first store, so one piece of code serves both functions;
- a larger size goes 64 bytes at a time, with the far end loaded first;
- from 2048 bytes a forward copy uses `rep movsb` where the processor says
  that is fast (ERMS).

Nothing needs more than SSE2, which every x86_64 has.

The root crate's build script passes `--wrap=memcpy` and `--wrap=memmove`
to the linker for the `cig` binary on x86_64 musl, so every reference in
the program, in its allocator and in the C library goes to
`__wrap_memcpy` and `__wrap_memmove` here. A build where they are missing
does not link. Every other target keeps its C library's routines, and on
those this crate is only its tests.

The tests copy every size from 0 to 400 bytes and sizes around each
threshold up to 300,001, at every alignment of source and destination
within 16, and move inside one buffer in both directions at every distance
from 1 to 150 and around the size itself, each way of a large forward copy
forced in turn.
