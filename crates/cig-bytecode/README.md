# cig-bytecode

The register bytecode that CigScript's stage-B VM runs
(otmof-ops/cig-compile#31): the instruction set, the module format, a
non-panicking parser, a verifier that makes a module safe to run with no
further checks, and a text disassembler.

This crate is the format's **one authoritative definition**. The VM core
reads these structures; CigCompile's lowering writes them. Neither party
reaches into the private byte layout: both go through the types declared
in [`module`] and the (de)serializers in [`serialize`] and [`parse`].

No `unsafe` code. No dependency on the interpreter (`cigscript::interp`),
the burn kernel (`cigscript::burn`) or any other cig crate except
`blake3` for the module integrity hash.

The format contract (what next packages may rely on without reading this
code) is in [`docs/BYTECODE.md`](../../docs/BYTECODE.md).
