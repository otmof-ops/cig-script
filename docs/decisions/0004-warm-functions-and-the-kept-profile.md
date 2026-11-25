<!-- SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script) -->
<!-- SPDX-License-Identifier: CC-BY-4.0 -->

# 0004: warm functions and the kept profile

- Status: accepted
- Date: 2026-11-25
- Issue: otmof-ops/cig-script#279, decision (c)

## Context

cig-cherry (#256) compiles what runs long. To tell a function that runs long from one that does not, the VM has to count something; to spend the count on a run worth compiling, the count must survive the process. Decision 0003 of this repository writes what a site saw; this record decides how a function is found warm and the shape of the profile the run keeps for the next.

What was measured, on 2026-11-25 on a development laptop:

- **M1** ran 116 scripts twice each on the VM and 14 cases of cigo. **Outside the compute set, 71 of 79 scripts (90%) finish in under 10 ms on the VM, with a median of 1.0 ms; the part of a run that executes the script is 2% of that median.** cigo's commands take 11.5 to 170 ms; `cigo --version` is 9.6 ms, of which **4.6 ms is loading the kept module** and 1.9 ms is the run itself. A first run on the VM pays the lowering once (277 ms for cigo's 20,992 lines).
- **M3** measured what counting on the fast loop costs: a flag tested at every arithmetic and comparison instruction and found off costs **2.4% at the geometric mean, 8.6% at the worst, in machine instructions** over 21 compute programs. Recording at every such site costs 4.4% and 18.1%.
- **M4** measured compile time: the median function compiles in 1.86 ms; cigo's 1,038 functions take 6.6 s and make 16.1 MB.
- **M6** measured warm start: the kept module turns cigo's 277 ms lowering into a 4.4 ms load. Native code loads at 0.84 ms a megabyte, all at the first call: with every function native `cigo --version` goes from 7.5 ms to 22.1 ms, and the plan's 30 functions add 0.93 ms for 0.29 MB.

What cig has today, checked:

- **The fast loop's `steps` arms count down.** The interpreter keeps, beside the count of steps, the number of steps the fast paths may still count before they must stop and ask (`left`): the distance to the budget's end, or `u64::MAX` when there is no budget. The two fast arms (`Op::STEPS`, and the `nop` that counts a pair of `steps` in one, both in `fast` in `src/vm/mod.rs`) subtract what they charge from it and fall back to the general path when it would pass zero: a subtraction and a branch, with a budget or without. That arrived with this record. Before it the arms tested for a budget and compared the count with `max_steps` when there was one, so a run with a budget (an ordinary `cig run` has one) paid for the comparison at every statement: measured on 2026-11-25, `fannkuch` took 5.5% more machine instructions with a budget than without, and now takes the same.
- **The general `Op::STEPS` arm** (in `step`, `src/vm/mod.rs`) runs `set_nest`, then the budget's logic against `max_steps`: the whole charge in one add if there is no budget or it holds all of it, otherwise step by step, so `E515` is raised by `tick` (`src/interp/mod.rs`) on the step that overspends.
- **Native code counts its own steps and the VM takes them into the run's count.** `budget_for` (`src/vm/native.rs`) gives native code the distance to `max_steps`; the generated code decrements `steps_left` in its context at each mark; the VM adds what was taken to the run's count where an entry returns (`Native::run`), in `absorb_ctx`, and after `enter_body` (`src/vm/mod.rs`). A budget that ran out anywhere but at the budget's end would be an error to the runtime: native code that leaves on its budget with steps left to spend is reported as "the step budget is not spent" (`src/vm/native.rs`).
- **The module cache** (`src/burn/modcache.rs`) writes an entry to a private temporary file and renames it, under a keyed-BLAKE3 MAC whose key is `module.key`, a random file in the keys directory made the first time an entry is stored. `key_for` hashes a domain tag, a digest of the build, `lower::VERSION`, `LEVEL` and the script's SHA-256; the digest of the build (`build()`, private) is made from cig's version and the running executable's size and modification time, so another executable computes a different one and finds no entry `cig` wrote. Its sweep (`sweep`, called by `cig tin gc`) lists only entries (a bare 64-hex name) and its own leftovers, and removes entries of another build, entries not loaded for 30 days, and leftovers older than an hour; a file by any other name is never listed and never removed.

## The decision

### 1. A function is found warm by a sample, taken where the countdown already ends

The countdown the fast arms keep runs to the lesser of two points: the budget's end, and the next **sample point**. When it would pass zero the fast arm falls back to the general arm, as it does at the budget's end today. The general arm runs the budget's own logic against `max_steps`, unchanged, and then, if the count has reached the sample point, takes one **sample**: it credits the function on top of the frame stack by one, and sets the next sample point to the count plus the quantum.

- **The path that takes no sample executes what it executes today:** the same subtraction and branch. Shown by counting machine instructions on the compute programs with sampling off and on, with a budget and without.
- **The quantum is a number of steps,** about 2^14, set by measuring a sample's cost. It is never a time: the same run on the same input takes the same samples, so a profile does not depend on how fast or how busy the machine was.
- **A run that takes no sample allocates nothing and opens no file.** The samples table (function index to count) is made at the first sample.
- **Whatever changes the budget or the count keeps the countdown true,** so a budget set or cleared in the middle of a run cannot leave the fast arms counting towards a stale point.
- **A function is warm** when a run has sampled it a few times (about four, set with the quantum). What happens then is record 0003's.

### 2. Native code takes no sample

Native code is given the distance to the budget's end, as today, and never the distance to a sample point: it cannot tell the two apart, and the runtime treats a budget that ran out early as an error. So native code runs through sample points. The steps it counted arrive in bulk, and the next `steps` the interpreter runs finds the count past the sample point and takes **one** sample, for the function on top; the next point is set from the count, so a long stretch of native code is one sample and not a run of them. A stretch of native code is therefore under-sampled: a limit, stated. (Only a compiled program has native code today, and section 3 keeps those out; the limit will matter when code compiled from a profile runs under `cig run`.)

### 3. Only `cig run` samples

A program `cigcompile build` made (stage `b` or stage `c`) takes no sample, makes no samples table and writes no profile. The profile of its script comes from `cig run` of the script.

### 4. Off by default until something reads the profile

Slice 1 of #279 builds sampling and the table behind a setting. With it off, the countdown runs to the budget's end and nothing else happens. The setting is turned on when the next slice gives the profile a reader; its name is decided where it is built.

### 5. The profile is one file beside the kept module

The profile of a script is kept where its module is kept (decision 0002) and found the same way: by the module cache's key, which `cig` computes. It is a file of its own, not a second entry of the module cache.

- **Its header carries the module's own hash,** so whoever holds a module can tell a profile is for it: function indices and pcs mean nothing for any other module. The module cache's key cannot serve for that, because it holds a digest of `cig`'s own build that no other executable can compute.
- **Another executable is handed the file by `cig`,** never asked to find it. `cigcompile build` with a profile takes it as a file among its inputs, in a canonical form `cig` exports, so a build's bytes depend on its inputs and not on a machine's run history. How it is exported is `otmof-ops/cig-compile`#33's to build.
- **A MAC over the whole file** (keyed BLAKE3: header and every record), under a key file of its own in the keys directory, made the first time a profile is written, as `module.key` is for the module cache. Not derived from the tin's master key: a run that burns nothing never loads that key (decision 0002, section 4), and asking the OS keyring for it would cost a run more than the profile is worth. A profile whose MAC does not hold is no profile.

### 6. The profile is read at the first sample of a run

Not at start. A run that takes no sample never opens the profile and pays nothing for its existence. At the first sample the file is opened and checked, and what it holds joins the run's own table. A function the kept profile already has a record for is not recorded again (record 0003, section 3), so a script that is run again and again stops recording.

### 7. The profile is written at the end of a run only when the run changed it

Changed means one of:

- a site was recorded in this run that the kept profile lacks; or
- a function that is in the profile, or that turned warm in this run, was sampled in this run to a higher power-of-two bucket than in any run before it.

A function that was sampled but never turned warm is not in the profile and changes nothing. So a script that is run again and again settles: the writes stop when the profile has what its runs find. A run that is killed loses what it learned and nothing else.

### 8. What is kept for a function

- **The most samples a run took in it, as a power-of-two bucket.** A merge of two profiles is a maximum, so two runs merging in either order agree, and a run that does not write (because it took fewer samples than the kept maximum) loses nothing.
- **Its recorded sites** as decision 0003 names them.
- **Nothing a script computed.** No value, no string, no map. Tags (see decision (i), record 0005), shapes, function indices and counts only.

### 9. Written whole, to a private temporary file, then renamed

As the module cache writes its entries (`src/burn/modcache.rs` 417 to 446). Two runs at once: the last to finish wins after merging what it read; a profile is a hint. Nothing is corrupted by a crash or a kill: either the old file stands or the new one does.

### 10. Bounded

The profile holds the functions that turned warm in some run, with their records, and the functions a recording passed through (record 0003, section 4). A limit on the file's size is kept by the writer: past it, the least sampled functions are dropped. The limit is set by measurement when the file is built.

### 11. Removed with its module by the tin's sweep

A profile is for one module and goes when that module goes. **The sweep has to be taught this:** today it lists only entries and its own leftovers, so a profile would never be removed. It will list profiles too, and remove one whose module is gone, one of another build, and a leftover of a write that did not finish. A profile holds nothing a hop's compensation needs and no secret.

## What stays exact

- **Rule 3** (the step budget is exact). Native code is given the distance to `max_steps`, unchanged. The fast arms fall back no later than the budget's end, because the countdown never runs past it; the general arm then runs the unchanged budget logic against `max_steps`, and `E515` is raised by `tick` on the step that overspends, before any sample is looked at. A sample charges no step and changes no count.
- **Rule 5** (every engine behaves identically). A sample changes nothing a script can observe, and samples are taken by the count of steps, not by time. The differential suites hold with sampling on and off.
- **Rule 6** (deterministic cleanup; reference counting, no tracing collector). The samples table is dropped with the interpreter; the profile is a file.
- **Rule 7** (only verified bytecode runs, and its bytes never change). A profile holds counts and references by function index; the bytecode is not touched.
- **Rule 8** (no memory is both writable and executable). A profile is a plain data file; nothing is mapped executable.
- **Rule 9** (`unsafe` only in the one audited crate for the job). The sampler is in the VM (`src/vm/`), which forbids `unsafe`; the profile's reader and writer need none and add none.
- **Rule 10** (`cig` never links a compiler). The profile reader lives in `cig`; the compiler that reads it does not.

## What it costs

- **Nothing on the path that takes no sample:** the fast arms do what they do with sampling off.
- **A sample:** one trip through the general arm, a count and a new sample point, once a quantum.
- **Opening the profile at the first sample,** in a run that takes one: a read and a MAC over a file that holds only warm functions.
- **A write at the end, when something changed.** A run that already has what the kept profile holds pays nothing. M1's median script of 1.0 ms takes no sample at all; cigo's longest command (170 ms, M1) does.
- **Disk.** One file a module. The limit is enforced by the writer (section 10), and the sweep removes profiles with their modules.

## Weighed and not taken

- **A counter in the `steps` arms,** the usual hot counter: a load, an add and a store at every statement of every script, where M3 measured what far less costs. The arm already counts down to a point; the sample point is one more thing that point can be.
- **A sampler driven by a timer** (a signal, a thread). Not deterministic: the same run would give a different profile each time.
- **A flag in the fast loop that turns sampling on.** M3: a flag tested at every arithmetic and comparison instruction and found off costs 2.4% at the mean and 8.6% at the worst. The countdown carries the setting: with sampling off it runs to the budget's end.
- **A profile written at the end of every run.** M1: the median run is 1.0 ms and takes no sample; a file written by every run is a cost to exactly the runs that have nothing to say.
- **Sums of samples across runs.** Either a write at every sampled run (which section 7 avoids) or a count that silently caps, which a compiler reader cannot tell from a hot function.
- **A profile that says which module it is for by `cig`'s build digest,** as the module cache's key does. No other executable can compute that digest. The module's own hash says it to anyone.
- **A MAC under the tin's master key.** That key is loaded only by a run that burns; the module cache keeps a key file of its own for that reason, and the profile does the same.
- **Hit counts a site.** A write at every execution; hotness is a function's, from samples.
- **Sampling under a compiled program.** A compiled program's profile is the profile from its `cig run`; samples under the program would be a second source and a second format cherry would have to merge.

## What would change it

- **A workload whose warm function does not reach the sample point** during its interesting run. The quantum falls, bounded by what a sample costs.
- **A compiled-program path that benefits from its own profile**, found by measurement. Then this record is extended; a compiled program's profile is still a file beside its module.
- **A sweep rule that moves a profile's lifetime off its module's**, if measurement finds a profile worth keeping past its module. None does today.

## What is not decided here

- **Record 0003** owns the recording, the observer and what a site records.
- **Decision (f)** (the native cache) owns the native entry's key, signature and refusal rules. The profile this record names is not keyed by a native entry.
- **Decision (d)** (exits for guesses) owns how a body is given up. A sample credits a function; a failure counts against a body.
- **Decision (policy)** owns what cherry may do on each profile. The profile this record names is on only where that decision allows.
- **How cherry runs** (decision (run)) owns who reads the profile file and when; this record says what the file is.
- **The values of the numbers.** The quantum and the number of samples that makes a function warm are this record's; W and the number of recordings a run are record 0003's. Each is set by measurement when its part is built.

## Done when

- [ ] The fast arms count down, and are at or under what they cost before, with a budget and without (counted).
- [ ] With the setting on, the countdown runs to the lesser of the budget's end and the next sample point; the general `Op::STEPS` arm takes a sample after the budget's logic and sets the next point from the count; the samples table is made at the first sample.
- [ ] Counted: with sampling on, a program's fast arms execute what they do with it off.
- [ ] A profile is read at the first sample, joined with the run's table, and written at the end only when the run changed it: whole, to a private temporary file, renamed, under a MAC over the whole file keyed by its own key file.
- [ ] An internal command prints a kept profile.
- [ ] The sweep removes a profile with its module.
- [ ] The budget tests pass unchanged; the differential suites pass with sampling on and off.
