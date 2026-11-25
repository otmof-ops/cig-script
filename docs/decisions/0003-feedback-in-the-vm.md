<!-- SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script) -->
<!-- SPDX-License-Identifier: CC-BY-4.0 -->

# 0003: feedback in the VM

- Status: accepted
- Date: 2026-11-25
- Issue: otmof-ops/cig-script#279, decision (b)

## Context

cig-cherry (#256) compiles a function from what the VM saw it do, so the VM has to see it. Decision 0008 of `otmof-ops/cig-compile` already runs a stage `c` body on the VM's own frame and leaves it at an exact instruction, so a guess the body made that did not hold goes back to the interpreter at that instruction; what is missing is the record of what the frame held there, from which cherry would compile a better guess on the next run.

What was measured, on 2026-11-25 on a development laptop:

- **M2** ran the compute census on cigo's thirteen commands (5,037 sites, 491,134 executions). Every arithmetic site (527), every index site (317) and every method-call site (1,172) saw one observation; 98.6% of comparison sites did, and 99.7% of call sites. **16% of field sites saw two shapes, none three:** guessing the commoner shape is wrong at 2.7% of field executions, guessing both never wrong. Every site that ran more than 128 times had seen everything it would see by then; of the 64 field sites that saw two shapes, 19 had both by their 8th execution and all 64 by their 32nd. One benchmark call site saw between three and eight callees.
- **M3** ran a prototype that records operand types at every arithmetic and comparison instruction of the fast loop. Over the 21 compute programs it costs **4.4% more machine instructions at the geometric mean and 18.1% at the worst** (`mandel` 1.181). A flag tested at every arithmetic and comparison instruction and found off costs 2.4% at the mean, and each recorded operation adds 14.9 machine instructions to the loop (a load, two shifts, an or and a store, with the cell reached through a checked index and its operands kept live in cig's one large `fast` function).

What cig has today, checked:

- Two fast loops, one source: `Machine<'i, T: Tier>` in `src/vm/mod.rs` (about 1961 to 2073) compiles for `Plain` and for `Bodied`, picked once per run per program from `program.has_bodies` (about 1777 to 1781) and never switched while frames are live. The per-instruction code (`fast_at`, `fast`, at 5296 and 5368) is generic over the registers only, so the plain and the bodied loops run the same arithmetic arms. `Bodied` adds a second copy of the loop (`fast_run_to` at 5240) that runs between a body's resume points. Every VM unit test runs plain and under four body styles (`STYLES`, `src/vm/tests.rs` 92 to 118).
- Each function keeps two per-pc tables on its `Proto` (`src/vm/mod.rs` 296 to 346): `hints: Vec<Cell<u32>>` (the field position a `getfield` or `setfield` key last had, or the receiver type and method row at a `callm`), and `sites: Vec<Cell<u64>>` (the shape in the high half and the slot in the low, kept by a `getfield` or `setfield` and read by native code as one `u64` per pc through `ctx.sites`). Native code reads `sites` in place, so its layout is part of the interface `cig-native` carries.
- Stage `c` bodies already leave at a failed guess: `Host::again` on `cig-native`'s `Host` trait (`crates/cig-native/src/loader.rs` 205, called through `extern "C"` glue at loader.rs 663) runs the one instruction on the runtime's side and returns a stop. `BodyEntry::missed` (`src/vm/native.rs`) counts the failure for the body. Native code itself never raises (`otmof-ops/cig-compile` `docs/decisions/0006-native-code-in-a-program.md`, section 5) and never records.

## The decision

Recording is done outside both fast loops, on the general path, for a short stretch of the run, when a function has been found warm. Neither fast loop gains a word, no flag is tested on the hot path, and the record is a table of the function's own, allocated when a recording first reaches it; it is never written into the bytecode.

### 1. The warm trigger comes from record 0004

A function is found warm when it has been sampled a few times in a run and nothing is recorded for it yet. How it is sampled, and how many samples make it warm (about four), are record 0004's. What this record settles is what happens next: the run starts a recording on that function.

### 2. A recording is W steps on the general path

The driver (which is where a fast loop returns to between its runs) starts the recording on whatever is on top of the frame stack and runs it on the general path. In front of each instruction the general path runs, the observer writes what the frame holds at the operands the instruction reads. When W steps of the run have passed, the recording ends and the fast loop runs the frame again. The driver carries the recording, so no fast loop ever tests a flag.

**The general path is not only a slow path:** a function whose registers carry cells (one whose variables a closure captured) runs on it always. So what the driver does to know that no recording is open has to cost such a function nothing a count can show. It is measured when it is built, on `closures` at least; if a test in the driver's loop shows, a recording gets a loop of its own (the general path's `step` with the observer in front) and the driver's loop is left as it is.

W is a number of steps, about a thousand, set by measurement when it is built. M2 says what bounds it from below: by its 32nd execution a site had seen everything it would see, but for four comparison sites that took until the 128th, so W has to cover the sites of a loop that many times. What bounds it from above is what a recording costs: W steps at the difference between the general path and the fast loop, once.

### 3. A run makes a bounded number of recordings

About eight recordings a run, each W steps; the number is set with W, and the driver keeps to it. A function a recording was started on gets no second one; the kept profile (record 0004) says which functions that already happened to in earlier runs, so a script that is run again and again stops recording.

### 4. Everything that runs in a recording is recorded, callees included

The general path makes calls and returns as it always has, and the recording follows. A callee the warm function reaches runs on the general path too, and its instructions write to its own record. **A function that was only passed through may have been seen a few times, fewer than M2's 32:** it stays a candidate for a recording of its own if it turns warm, and until then its record is what it is. A guess made from a thin record is still a guess under a guard (record 0005, section 3): it can be wrong and cannot be unsound.

### 5. The record is a table of the function's own

Allocated when a recording first reaches the function. Per pc, by the kind of instruction:

- **An arithmetic, comparison, or index instruction** (two register reads) records what each operand has been as one observed set an operand (see decision (i), record 0005). A site that saw `int + float` is told from one that saw `int + int` and `float + float`.
- **A field read or write** records the shapes seen, up to two, and a mark when there were more. `Proto.sites` (the shape and slot of the last map found) stays as it is for the runtime: native code reads it in place at `ctx.sites`, and the general path writes the second shape, when a second appears, to the record's own cell. Two is the cap M2 says every cigo field site in the pool fits.
- **A call** records the callee (a function of the program); a method call records what `Proto.hints` already holds at a `callm`, the receiver's type and the method row.

Each record a kind is small (a few bytes per site) and lives on the function's `Proto` as a sibling of `hints` and `sites`, not in the instruction's own bytes (rule 7 forbids rewriting them).

### 6. A site no recording reached has no record

Record 0004 writes nothing to the profile for a site no recording visited, and cherry compiles no guess there. A function that is never sampled is never recorded.

### 7. What is left fixed for a failed guess

Only a compiled program has native bodies today, and a compiled program records nothing (record 0004, section 3). When native code compiled from a profile runs under `cig run` (a later slice of #279), a guess of its that fails comes back to the runtime through `again`, which runs the one instruction on the general path while the body goes on. That is where the same observer will write what the operands were, on the runtime's side. **No helper is added to `cig-native` for recording, and native code itself never records.** How a failed guess is counted and what it leads to is decision (d)'s.

## What stays exact

- **Rule 1** (burns never move). A recording runs instructions in the order the tree-walker would, through the one kernel: it is the general path, not a copy of it.
- **Rule 2** (errors are identical). An instruction that raises in a recording raises as it would without one, at the same position, with the same code and the same message: the general path is what runs it.
- **Rule 3** (the step budget is exact). A recording runs W steps on the general path, whose `Op::STEPS` arm (`src/vm/mod.rs` 3268 to 3281) counts them as it counts any others; `E515` fires at the same step on the same line. The observer charges nothing.
- **Rule 4** (maps keep insertion order; output byte for byte the tree-walker's). The observer writes to its own tables and reads registers, never values a script can see.
- **Rule 5** (every engine behaves identically). A recording changes nothing a script observes. The differential suites (`tests/engines.rs`, `tests/differential.rs`) hold with recording on and off.
- **Rule 7** (only verified bytecode runs, and its bytes never change). The record is beside the bytecode, not in it. No instruction is rewritten.
- **Rule 9** (`unsafe` only in `cig-native`). The observer lives on the general path of the VM, which keeps `forbid(unsafe_code)`.

## What it costs

- **Nothing on the fast loops.** Not a flag, not a counter, not a load (M3's 2.4% is the figure this avoids).
- **Nothing a count can show on the general path while no recording is open** (section 2): to be shown by counting, not assumed.
- **W steps on the general path in place of the fast loop, for each recording,** at most about eight times in a run. How much slower the general path is over those steps is measured when it is built, and W is set from it. A short script pays it never: it is not sampled often enough to start a recording (record 0004).
- **A small allocation when a recording first reaches a function:** the function's record, a few bytes a site.
- **The observer itself,** in front of the general path's instructions while a recording is open.

## Weighed and not taken

- **A recording copy of the fast loop, for warm functions or for a whole run chosen at start from the last run's profile.** This is what M3 itself proposed from its figures (a warm function run on a second copy of the loop), and this record departs from it. A third copy of the interpreter beside the plain and the bodied loops, with its tests (every VM unit test already runs five ways); nothing switches a function between loops while its frames are live, so that would have to be built too; chosen at start, it needs a file read at every start, which M1 rules out, and it records a run late. M2 says what has to be seen is seen inside 32 executions for all but four sites, so a few hundred steps on the general path cost less than a third loop does.
- **A count at every execution of a site** (hit counts). A write in the fast loop at every execution, which M3 measured: 14.9 machine instructions for each recorded operation (a load, two shifts, an or and a store, reached through a checked index). A function's hotness is read from samples (record 0004), not from a counter a site keeps.
- **A flag in the loop that turns recording on.** M3: 2.4% at the geometric mean for a flag that is tested at every arithmetic and comparison instruction and found off: over half of what recording itself costs.
- **A per-site record the fast loop writes, in the spirit of V8's embedded feedback** (study C2, entry 5). V8 writes a byte of its own bytecode through an encode table; rule 7 forbids the bytecode write, and M3's 14.9 instructions an operation is what a side table costs in this loop.
- **Recording only where a native body's guess failed.** A script under `cig run` has no bodies, and those scripts are what cherry is for. So the source of a record is the VM running a script; `again` is a second way in, later.
- **A new `Host::record` helper on `cig-native`'s `Host` trait.** `Host::again` already brings the runtime back for one instruction; writing the record there is the interpreter's arm, not a new slot on the helpers table. Adding a slot would bump `cig-native`'s interface and refuse every existing section (`crates/cig-native/src/format.rs` 43: `INTERFACE = 2`; the loader at `format.rs` about 1113 refuses any `interface != INTERFACE`).
- **Recording in the fast loop by compiling an inline form that writes on the side**, in the spirit of JavaScriptCore's value-profile store (study C2, entry 4, verdict take). Taken for its shape (one write a site, merged by OR), but placed on the general path, not the fast one, for M3's cost.

## What would change it

- **A workload whose sites keep changing after W steps.** M2 found none on the largest real script. If one exists, W rises, or a function gets more than one recording.
- **A measurement that eight recordings cost a run something that matters.** Then W or the number falls; the fast loops are still not the place.
- **A new instruction kind** (today's list is in section 5) that cherry will guess on and whose operands do not fit the shapes above. Decision (i) extends first, then this record's section 5.

## What is not decided here

- **How a function is sampled and how many samples make it warm** (record 0004). W and the number of recordings a run are this record's, and their values are set by measurement when it is built.
- **What cherry does with a record** (how many members in a set before cherry compiles no guess; what the guard looks like): decision (i) owns the lattice and decision (d) owns the exit.
- **How a failed guess is counted and when a body is given up:** decision (d). This record names the write (`again` writes the operands) and leaves the counting to (d).
- **The native cache, the policy, how cherry runs:** decisions (f), (policy), (run).

## Done when

- [ ] With record 0004's setting on and a function warm, a recording runs W steps on the general path and ends; a run makes no more than its bound of them.
- [ ] Counted on `closures` and the other compute programs: with no recording open, the general path executes what it did before.
- [ ] The observer writes one per-pc record a function, of the shape section 5 names, on the runtime's side.
- [ ] An internal command prints a function's record (what cherry would read).
- [ ] The VM's unit tests pass under all four body styles with recording on. The differential suite (`tests/engines.rs`, `tests/differential.rs`) passes with recording on and off, on every script it holds.
