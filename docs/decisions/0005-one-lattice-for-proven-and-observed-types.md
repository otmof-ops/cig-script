<!-- SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script) -->
<!-- SPDX-License-Identifier: CC-BY-4.0 -->

# 0005: one lattice for proven and observed types

- Status: accepted
- Date: 2026-11-25
- Issue: otmof-ops/cig-script#279, decision (i)

## Context

Decision 0008 of `otmof-ops/cig-compile` runs a stage `c` body on the VM's own frame and holds a register in a machine register when its type is proven. Decision 0003 of this repository writes what a site saw, as one record a function. Both speak of a value's type; cherry (#256) writes code that guesses when stage `c` could not prove, under a guard that exits through 0008's path where the guess did not hold. **One lattice has to serve all three**: the stage `c` compiler's proof, the VM's observation, and the guard.

What was measured, on 2026-11-25 on a development laptop:

- **M2** found that every one of cigo's arithmetic sites (527) saw one pair of operand types, every index site (317) one kind of container indexed by one kind of index, and every method-call site (1,172) one type of receiver; 98.6% of comparison sites saw one pair, and 99.7% of call sites one callee. The observation at those sites fits in a small set over run-time tags. 16% of field sites saw two shapes; a shape is a map's own, not a tag, and belongs to decision 0003's record for a field, not to this lattice.
- One benchmark call site saw between three and eight callees; the lattice does not have to hold a call's callee (decision 0003 keeps it beside the record), only the receiver's tag when the site is a method call.

What cig has today, checked:

- **`Value` has eleven kinds** (`src/value.rs`): `Null, Bool, Int, Float, Str, List, Map, Func, Builtin, Module, Chain`. It is `#[repr(C, u8)]`, 24 bytes, with its tag at offset 0 and its payload at offset 8, and its size, alignment and those two offsets are held to `cig-native`'s constants by assertions at compile time. **The tag numbers are stated in two places today:** `cig_native::value` (`crates/cig-native/src/value.rs`) names five (`NULL`, `BOOL`, `INT`, `FLOAT`, and `OWNS`, the first tag of a kind that owns memory), and `Value` takes its first five discriminants from them (`Str` is `OWNS`); the six after it (`List` to `Chain`) are numbered only in cigscript's own `Value`, as `OWNS + 1` to `OWNS + 6`. Nothing in `cig-native` names those six.
- **`cig-codegen`'s `Ty`** (`otmof-ops/cig-compile` `crates/cig-codegen/src/types.rs`, top of file): `None, Null, Bool, Int, Float, Any`, with `None < {Null, Bool, Int, Float} < Any`. `cig-codegen` depends on `cigscript` and on `cig-native`.
- **The CIR's `Ty`** (`src/lower/cir/ir.rs` 98 to 109): `Dynamic, Null, Bool, Int, Float, Str, List, Map, Func`. `Dynamic` is never a run-time tag.

## The decision

### 1. An observed type is a set over run-time tags

Sixteen bits, joined by OR. Each of the eleven tags of `Value` has one bit; the remaining five bits are reserved. **One set an operand:** an instruction that reads two registers (arithmetic, comparison, index) carries two sets, so a site that saw `int + float` is told from one that saw `int + int` and `float + float`.

### 2. A proven type maps into the same sets

- `cig-codegen`'s `Int` maps to `{int}`, `Float` to `{float}`, `Bool` to `{bool}`, `Null` to `{null}`, `None` to the empty set, `Any` to every tag.
- The CIR's `Int`, `Float`, `Bool`, `Null` map the same way; `Str` to `{str}`, `List` to `{list}`, `Map` to `{map}`, `Func` to `{func}`.
- **The CIR's `Dynamic` is not a tag and gets no bit.** `Dynamic` maps to every tag (the "I do not know" element of the lattice).

### 3. A guess is a set that was observed and not proven

A guess is compiled with a guard, and whatever a guard that fails does, the interpreter runs the instruction as it would have: decision 0008 of `otmof-ops/cig-compile` (`docs/decisions/0008-native-code-on-the-vms-frame.md`) built the ways back, and which of them a guess takes is decision (d)'s. So a guess can be wrong and cannot be unsound. A proven set needs no guard: it was proven.

### 4. There is no "generic" member

A set with several members is what it is: `{int, float}`, or `{int, str}`, or every bit. What cherry does with a set (how many members before it compiles no guess) is decision (d)'s. The lattice names the sets; it does not pick among them.

### 5. What a tag cannot show is not in the lattice until a value can carry it

- What a list holds (int, float, anything) is a property of the list, not of a register. A register's tag says it is a list.
- What a field holds is a property of the map's shape, not of the tag. The shape goes in decision 0003's record for a field, not here.
- `otmof-ops/cig-compile` #35 ("values") decides when and how a value carries a sharper type. Until then the lattice is what one register's own tag can say.

### 6. The lattice lives in `cig-native`, and so do all eleven tag numbers

Beside the tag numbers both sides share (`crates/cig-native/src/value.rs`). Plain data: a `u16` with a named bit a tag, and functions that join two sets, test a tag's membership, give the set of one tag, and format a set for printing. They take a tag (a `u8`), not a `Value`: `cig-native` sits under cigscript and cannot name `Value`, and cigscript gives a value's tag by a `match`. No `unsafe` is needed or added.

**`cig-native` names all eleven tags, and cigscript's `Value` takes each discriminant from its name there,** as it does for the first five today. A bit in the lattice then means the same kind on both sides because there is one list of numbers, not two that agree by care.

`cig-native` is the crate for it because the generator (`cig-codegen`) and the runtime both depend on it and it already states the `Value` layout for both.

### 7. The profile's byte format lives beside the lattice

Decision 0004 keeps a file with the recorded sites for each function. A site's record holds observed sets, by the shape decision 0003 names; the exact byte form of a set in the file is the lattice's own bits, little-endian, with its reserved bits zero. The reader (`cigcompile build` with a profile) looks up the same constants.

## What stays exact

- **Rule 5** (every engine behaves identically). The lattice holds tag membership; a value has a tag at run time whatever runs it. The join is bit-OR, so two engines reading the same bytes compute the same set.
- **Rule 7** (only verified bytecode runs, and its bytes never change). The lattice is beside the bytecode, in the profile file and in decision 0003's per-function record; never in the bytecode.
- **Rule 9** (`unsafe` only in `cig-native`). The lattice is in `cig-native` as plain data; no `unsafe` is added.
- **The two sides cannot drift,** once section 6 is built: every tag number is `cig-native`'s and `Value`'s discriminants are those constants. Today that is so for the layout and the first five tags.

## What it costs

- **Two bytes an observed operand.** A site that reads two operands records four bytes of lattice; the record itself (decision 0003) is small.
- **Nothing at a guard that a guard does not already cost:** a guard tests a value's tag against a set, and what that test is in machine code is decision (d)'s.
- **A map from each side's `Ty` to a set.** A small table a side, in code; no run-time cost.

## Weighed and not taken

- **A set per site, not per operand.** A single set over both operands conflates `int + float` with `int + int` plus `float + float`, which compiles the same guard for two different mixes and gives up for the harder one when the easier one would still fit. One an operand costs the same two bytes an operand and names the cases cherry's guard tests.
- **A "generic" member of the lattice, and bits over the CIR's nine kinds** (what study C2, entry 17 proposed for cig, and this record departs from). The CIR's kinds are not the run-time tags: `Dynamic` is never a value's tag, and `Builtin`, `Module` and `Chain` have no kind there. "Any" in `cig-codegen` and `Dynamic` in the CIR are already the full set, so a member that means "anything" adds a bit and no information. A mark on a record that more was seen than it keeps (a third shape) is record 0003's, not a member of a set.
- **A set over cig's higher-level types** (list of int, map with a shape). A shape is a map's own, not a register's tag; `otmof-ops/cig-compile` #35 is the issue that may change what a value carries. Until then the lattice names what one register's own tag can say.
- **A set as a bitmask over `cig-codegen`'s six-point lattice.** The six are a coarser partition than the eleven tags: `Any` covers `str`, `list`, `map`, `func`, `builtin`, `module` and `chain` as one bit, so a site that saw only strings would look to cherry like one that saw strings and functions. One bit a tag is the smallest set that tells cherry what it needs.
- **JavaScriptCore's `SpeculatedType`** (study C2, entry 4, verdict take; entry 17, verdict adapt). The shape (bits, OR-join) is taken. The breadth (50 bits over JSC's run-time types) is not: cig has eleven, so sixteen bits fits in a `u16`.
- **A separate lattice for each of the three readers** (cig-compile's stage `c`, the VM's observer, cherry's guard). Three ways to say "this is an int" would drift apart; one definition in the crate all three read cannot.

## What would change it

- **A new `Value` variant.** The lattice grows a bit, from the sixteen bits; past sixteen it moves to `u32`. The reserved five bits say when this happens cheaply.
- **A sharper type a value can carry** (`otmof-ops/cig-compile` #35). Then the lattice grows a layer under the tag bit: the tag bit stays at the top, and the under-layer says what the list holds, what the field holds, what the function expects. Decision 0003's record and decision 0004's profile grow in step.
- **A measurement that a site that saw `{int, float}` fails as often as one that saw every bit.** Then the lattice carries a "few members" flag and decision (d) uses it. M5 (as #279 words it) is the measurement that would say so.

## What is not decided here

- **Decision 0003** owns the record a site keeps.
- **Decision 0004** owns the file the lattice is written to.
- **Decision (d)** owns what cherry does with a set at compile time: how many members before no guess is compiled, what a wider recompile reads, when a guess is given up.
- **Decision (f)** owns the native cache's key and refusal rules; a set is in a site's record in the profile, never in a cache entry's key.
- **`otmof-ops/cig-compile`'s stage `c`** owns how a proven set reaches the generator: the CIR carries `Ty` as it does today, and `cig-codegen` carries its `Ty`; each maps to a set at the boundary. Neither existing `Ty` goes.

## Done when

- [x] `cig-native` names all eleven tags, and cigscript's `Value` takes every discriminant from them. (`cig_native::value`: `STR` to `CHAIN` beside the first four, and `TAG_COUNT`.)
- [x] `cig-native` gains a module beside `value` with the eleven bits, the five reserved, the join and the membership test over a tag. A `u16` carries one set. No `unsafe`. (`cig_native::tagset::TagSet`. It prints a tag by the name `cig_native::value::name` gives it: `string` and `function` where this record writes `str` and `func`, and `builtin` for a built-in function.)
- [ ] The maps from `cig-codegen`'s `Ty` and the CIR's `Ty` into the lattice are named and read where a proven type crosses into the record (decision 0003) or the profile (decision 0004). `Dynamic` and `Any` map to every bit; `None` to the empty set.
- [ ] The profile's byte format for a set is this lattice's bits, little-endian, reserved bits zero. Every reader of a profile reads the same constants.
