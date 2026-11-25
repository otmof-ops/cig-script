// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Native code under the VM (cig-compile's decisions 0005 and 0006).
//!
//! A stage `c` program holds machine code for some of its functions,
//! each compiled for one row of argument types (ints, floats, bools and
//! null, as raw 64-bit slots). At a `call` of such a function with
//! arguments of one of its rows the VM runs the native code instead of
//! pushing a frame ([`super::Machine::call_here`]); for any other call
//! it runs the bytecode, as it does for every other function.
//!
//! **Native code never makes an error.** Where the tree-walker would
//! raise one, it leaves with the place and the operands, and the error
//! is made here by the interpreter's own function from them
//! ([`Native::run`]): the operator, the range check, the step counter,
//! the depth error. So its code, message, hint and position are the
//! tree-walker's.
//!
//! **The counts stay exact.** Native code is given the steps left of the
//! budget and takes one at each step mark; what it took is added to the
//! interpreter's count when it comes back, however it comes back. It is
//! given the calls left before the depth limit, and is entered only
//! where the stack has room for that many ([`has_room`]).

use super::Word;
use crate::diagnostics::{internal, Diagnostic};
use crate::interp::ops::{binary, negate, range_fits};
use crate::interp::{depth_exceeded, Interp};
use crate::syntax::ast::BinOp;
use crate::syntax::span::Span;
use crate::value::Value;
use cig_bytecode::DecodedInstr;
use cig_native::{
    op, Code, Context, Exit, Host, Rule, Site, Stop, Tables, Ty, MAX_ARGS, MAX_BODY_FRAME,
    MAX_FRAME,
};
use std::cell::{Cell, OnceCell, Ref, RefCell};

/// A pc that is no resume point, in [`BodyEntry::resume_at`]. `u32::MAX`,
/// so a `.get(pc).copied().filter(...)` is a load and a compare.
pub(crate) const NO_RESUME: u32 = u32::MAX;

/// How many failed guesses of a body are counted before their rate is
/// looked at.
const MISS_WINDOW: u32 = 64;

/// A body's guesses fail too often when more than one fails in this
/// many steps of its own: the steps its machine code counted, which is
/// the work the body did in place of the interpreter.
const STEPS_A_MISS: u64 = 16;

/// A function's instructions with none at a resume point of its body:
/// the decoded code, and the words the fast loop reads, kept alike.
pub(crate) struct Between {
    pub(super) code: Vec<Option<DecodedInstr>>,
    pub(super) words: Vec<Word>,
}

/// One function's body as the VM finds it: where it lies in [`Code`], the
/// pc of each resume point, and (indexed by first-word pc) the resume
/// index at that pc, or [`NO_RESUME`].
pub(crate) struct BodyEntry {
    /// The body's index in the mapped code.
    code_index: usize,
    /// For each resume index, the first-word pc of that resume point.
    pcs: Vec<u32>,
    /// For each first-word pc of the function, the resume index at that
    /// pc, else [`NO_RESUME`]: the driver's lookup is one load and one
    /// compare per instruction. A resume point the VM gave up
    /// ([`BodyEntry::missed`]) is [`NO_RESUME`] from then on.
    resume_at: Box<[Cell<u32>]>,
    /// The function's instructions as the fast loop reads them between
    /// two resume points: its decoded code, with no instruction where a
    /// resume point is. The loop stops where it finds none, so it stops
    /// before a resume point with no test of its own, and a function
    /// with a body runs the rest of its bytecode at the speed of one
    /// without. A resume point the VM gave up has its instruction back.
    between: RefCell<Between>,
    /// For each resume index, the instruction `between` has none for.
    taken: Vec<Option<DecodedInstr>>,
    /// The guesses of this body that failed since `misses` was last
    /// zero.
    misses: Cell<u32>,
    /// The steps this body's machine code has counted since then: its
    /// own work, which its failed guesses are set against. Not the
    /// run's steps: what the rest of a program does says nothing about
    /// whether this body pays for itself.
    own: Cell<u64>,
    /// How many words the function's code has: where the tests'
    /// plain-Rust body runs to.
    #[cfg_attr(not(test), allow(dead_code))]
    code_len: u32,
}

impl BodyEntry {
    /// The body with index `code_index` in the mapped code and resume
    /// points `pcs`, held against its function's instructions: `code`
    /// is the function's decoded code, one place a word, with an
    /// instruction where one starts. `None` when a resume point starts
    /// none: such a body is never entered.
    fn resolve(code_index: usize, pcs: &[u32], code: &[Option<DecodedInstr>]) -> Option<BodyEntry> {
        let code_len = code.len();
        let resume_at: Box<[Cell<u32>]> = (0..code_len).map(|_| Cell::new(NO_RESUME)).collect();
        let mut between = code.to_vec();
        let mut taken = Vec::with_capacity(pcs.len());
        for (at, pc) in pcs.iter().enumerate() {
            let p = *pc as usize;
            let here = (*code.get(p)?)?;
            resume_at.get(p)?.set(u32::try_from(at).ok()?);
            *between.get_mut(p)? = None;
            taken.push(Some(here));
        }
        Some(BodyEntry {
            code_index,
            pcs: pcs.to_vec(),
            resume_at,
            between: RefCell::new(Between {
                words: Word::table(&between),
                code: between,
            }),
            taken,
            misses: Cell::new(0),
            own: Cell::new(0),
            code_len: u32::try_from(code_len).ok()?,
        })
    }

    /// The function's instructions with none at a resume point, for the
    /// fast loop between two of them. `None` while a resume point is
    /// being given up, which no run of that loop overlaps.
    #[inline]
    pub(crate) fn between(&self) -> Option<Ref<'_, Between>> {
        self.between.try_borrow().ok()
    }

    /// Count `steps` this body's machine code took from the budget:
    /// the work its failed guesses are set against ([`Self::missed`]).
    /// The VM calls it wherever it takes a body's count of steps into
    /// the run's.
    #[inline]
    pub(crate) fn counted(&self, steps: u64) {
        self.own.set(self.own.get().saturating_add(steps));
    }

    /// Count a guess that failed in native code entered at resume index
    /// `at` (an inline form met operands it does not cover, and the
    /// interpreter ran the instruction). `true` when this body's
    /// guesses fail too often for its machine code to be worth entering
    /// there: more than one in [`STEPS_A_MISS`] steps of its own, over
    /// the last [`MISS_WINDOW`] failures. The resume point is then one
    /// no longer, and the caller gives the frame back to the bytecode.
    ///
    /// What a failed guess costs is a call into the runtime where the
    /// interpreter would have run the instruction in a few nanoseconds;
    /// a loop that fails one every time round is slower compiled than
    /// not. So the loss is bounded: at most [`MISS_WINDOW`] such calls
    /// at that rate, twice over, before the bytecode has the loop back.
    ///
    /// The steps are the body's own ([`Self::counted`]), never the
    /// run's. Set against the run's, a body that failed at every entry
    /// kept its place in any program that did other work between two
    /// entries: the rest of the run supplied steps faster than the body
    /// could fail.
    pub(crate) fn missed(&self, at: u32) -> bool {
        let n = self.misses.get().saturating_add(1);
        if n < MISS_WINDOW {
            self.misses.set(n);
            return false;
        }
        self.misses.set(0);
        let own = self.own.replace(0);
        if own >= u64::from(MISS_WINDOW).saturating_mul(STEPS_A_MISS) {
            return false;
        }
        let pc = self.pcs.get(at as usize).copied();
        if let Some(cell) = pc.and_then(|pc| self.resume_at.get(pc as usize)) {
            cell.set(NO_RESUME);
        }
        // The fast loop no longer stops there: the instruction is back.
        // While that loop holds the table (it runs no body, so it never
        // does here) the instruction stays out, and the loop stops at a
        // pc that is no resume point: the VM runs it and goes on.
        if let (Some(pc), Some(instr), Ok(mut between)) = (
            pc,
            self.taken.get(at as usize).copied().flatten(),
            self.between.try_borrow_mut(),
        ) {
            let Between { code, words } = &mut *between;
            if let (Some(slot), Some(word)) =
                (code.get_mut(pc as usize), words.get_mut(pc as usize))
            {
                *slot = Some(instr);
                *word = Word::of(&instr);
            }
        }
        true
    }

    /// The resume index at `pc`, when `pc` is one of the body's resume
    /// points.
    #[inline(always)]
    pub(crate) fn at(&self, pc: usize) -> Option<u32> {
        match self.resume_at.get(pc).map(Cell::get) {
            Some(x) if x != NO_RESUME => Some(x),
            _ => None,
        }
    }
}

/// What a program's native code has for one of its functions: an
/// entry, a body, both or neither ([`Native::has`]). One byte, which
/// each loaded function carries, so that a call (it may run as native
/// code) and a frame that comes on top (the driver may have a body to
/// enter) read it of the function they hold already.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Has(u8);

impl Has {
    /// Neither: the function is bytecode alone.
    pub(super) const NEITHER: Has = Has(0);
    /// Not known yet: the program was offered native code it has not
    /// loaded. Taken to have both, so that whoever asks goes the exact
    /// way, which loads the code; every function is told again then.
    pub(super) const UNKNOWN: Has = Has(Has::ENTRY | Has::BODY);
    const ENTRY: u8 = 1;
    const BODY: u8 = 2;

    /// The function has an entry: a call of it may run as native code
    /// ([`Native::entry`] says whether one fits the arguments).
    #[inline(always)]
    pub(super) fn entry(self) -> bool {
        self.0 & Has::ENTRY != 0
    }

    /// The function has a body in the section ([`Native::body_of`]
    /// says whether it is one that can be entered).
    #[inline(always)]
    pub(super) fn body(self) -> bool {
        self.0 & Has::BODY != 0
    }
}

/// A program's native code, as the VM calls it.
pub struct Native {
    /// The code mapped from the section, when there is one: entries
    /// ([`Code::call`]) and bodies ([`Code::enter`]). `None` in a
    /// test-only [`Native`] that holds no machine code and runs its
    /// bodies as plain Rust.
    code: Option<Code>,
    sites: Vec<Site>,
    /// Every entry's row and result type, in the tables' order: an
    /// entry's index here is its index for [`Code::call`].
    rows: Vec<(Vec<Ty>, Ty)>,
    /// For each function of the module, where its entries start in
    /// `rows` and how many it has: none, for most.
    of: Vec<(usize, usize)>,
    /// For each function of the module, what the section has for it:
    /// nothing, for most. Each loaded function is told its own
    /// (`Program::stamp`), so the calls between two functions that are
    /// bytecode alone cost a program with native code what they cost
    /// one without.
    has: Vec<Has>,
    /// For each function of the module, its body as the section gives
    /// it: its index in the mapped code and its resume points.
    raw: Vec<Option<(usize, Vec<u32>)>>,
    /// For each function, its body held against the function's own
    /// instructions, the first time a frame of it is on top: `None`
    /// when one of its resume points starts no instruction of the
    /// function, and the body is then never entered. A function is read
    /// and verified at its first entry, so its body is looked at then
    /// and not when the section is loaded.
    bodies: Vec<OnceCell<Option<BodyEntry>>>,
    /// The section was built with a body for every function
    /// ([`Rule::Every`], a build for tests): its bodies are entered
    /// whatever their guesses do, so that every one of them runs.
    patient: bool,
    /// The tests' native code: every function has a body, in plain
    /// Rust, and every instruction of it is a resume point.
    #[cfg(test)]
    every_pc: bool,
}

impl Native {
    /// `code` armed with `tables`, for a module of `funcs` functions.
    /// The tables were checked against that module when they were
    /// loaded (`crate::native::Source::load`): every entry is for one of
    /// its functions, with that function's parameter count, and every
    /// body is for one of its functions.
    pub(crate) fn new(code: Code, tables: Tables, funcs: usize) -> Native {
        let mut raw: Vec<Option<(usize, Vec<u32>)>> = (0..funcs).map(|_| None).collect();
        for (index, body) in tables.bodies.into_iter().enumerate() {
            if let Some(slot) = raw.get_mut(body.func as usize) {
                *slot = Some((index, body.resumes));
            }
        }
        let bodies = (0..funcs).map(|_| OnceCell::new()).collect();
        let mut native =
            Native::build(Some(code), tables.sites, tables.entries, funcs, raw, bodies);
        native.patient = tables.rule == Rule::Every;
        native
    }

    fn build(
        code: Option<Code>,
        sites: Vec<Site>,
        entries: Vec<cig_native::Entry>,
        funcs: usize,
        raw: Vec<Option<(usize, Vec<u32>)>>,
        bodies: Vec<OnceCell<Option<BodyEntry>>>,
    ) -> Native {
        let mut of = vec![(0, 0); funcs];
        let mut rows = Vec::with_capacity(entries.len());
        for (i, e) in entries.into_iter().enumerate() {
            // Entries are sorted by function: one function's are one run.
            if let Some((first, count)) = of.get_mut(e.func as usize) {
                if *count == 0 {
                    *first = i;
                }
                *count += 1;
            }
            rows.push((e.args, e.ret));
        }
        let has = (0..funcs)
            .map(|func| {
                let entry = of.get(func).is_some_and(|&(_, count)| count != 0);
                let body = raw.get(func).is_some_and(Option::is_some);
                Has((if entry { Has::ENTRY } else { 0 }) | (if body { Has::BODY } else { 0 }))
            })
            .collect();
        Native {
            code,
            sites,
            rows,
            of,
            has,
            raw,
            bodies,
            patient: false,
            #[cfg(test)]
            every_pc: false,
        }
    }

    /// The tests' [`Native`], for a module of `funcs` functions: no
    /// mapped code, and for every function a body in plain Rust that
    /// has the interpreter run everything (one call of `Host::run` from
    /// where it is entered to the function's end). Every pc that starts
    /// an instruction is a resume point, so every instruction the VM
    /// runs is reached through the driver. A function's body is made
    /// when a frame of it first comes on top, as a mapped body's table
    /// is: nothing is read or verified earlier than without it.
    #[cfg(test)]
    pub(crate) fn for_tests_every_pc(funcs: usize) -> Native {
        Native::for_tests_every_pc_of(funcs, &(0..funcs).collect::<Vec<_>>())
    }

    /// [`Native::for_tests_every_pc`] with a body for the functions
    /// `with` names and none for the others: a program whose native
    /// code covers part of it, as most do.
    #[cfg(test)]
    pub(crate) fn for_tests_every_pc_of(funcs: usize, with: &[usize]) -> Native {
        let raw = (0..funcs)
            .map(|i| with.contains(&i).then(|| (i, Vec::new())))
            .collect();
        let bodies = (0..funcs).map(|_| OnceCell::new()).collect();
        let mut native = Native::build(None, Vec::new(), Vec::new(), funcs, raw, bodies);
        native.every_pc = true;
        native.patient = true;
        native
    }

    /// How many entries it has.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// It has no entry.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Its bodies are entered whatever their guesses do.
    pub(crate) fn patient(&self) -> bool {
        #[cfg(test)]
        if IMPATIENT.with(Cell::get) {
            return false;
        }
        self.patient
    }

    /// How many bodies it has.
    pub fn bodies(&self) -> usize {
        self.raw.iter().filter(|b| b.is_some()).count()
    }

    /// The functions that have a body.
    pub(crate) fn with_bodies(&self) -> impl Iterator<Item = usize> + '_ {
        self.raw
            .iter()
            .enumerate()
            .filter_map(|(func, b)| b.as_ref().map(|_| func))
    }

    /// The body of function `func`, when it has one that can be
    /// entered: `code` is the function's decoded code, one place a
    /// word, with an instruction where one starts. The body is held
    /// against it the first time, and found by one load after that.
    #[inline]
    pub(crate) fn body_of(&self, func: usize, code: &[Option<DecodedInstr>]) -> Option<&BodyEntry> {
        self.bodies
            .get(func)?
            .get_or_init(|| {
                let (index, pcs) = self.raw.get(func)?.as_ref()?;
                #[cfg(test)]
                if self.every_pc {
                    let all: Vec<u32> = (0..code.len())
                        .filter(|pc| code.get(*pc).is_some_and(Option::is_some))
                        .filter_map(|pc| u32::try_from(pc).ok())
                        .collect();
                    return BodyEntry::resolve(*index, &all, code);
                }
                BodyEntry::resolve(*index, pcs, code)
            })
            .as_ref()
    }

    /// Enter `body` at resume index `at`: machine code when a section
    /// is mapped, or a plain-Rust body (test-only) otherwise. Returns
    /// the stop the body stopped with. A stop's bits that are no stop
    /// come back as [`Stop::Defect`]`(0)`.
    #[inline]
    pub(crate) fn enter_body<H: Host>(
        &self,
        body: &BodyEntry,
        at: u32,
        ctx: &mut Context,
        host: &mut H,
    ) -> Stop {
        match &self.code {
            Some(code) => code
                .enter(body.code_index, at, ctx, host)
                .unwrap_or(Stop::Defect(0)),
            None => {
                // Test-only body: the interpreter runs everything, again
                // from wherever `run` stops inside the function's range.
                // Every pc is a resume point for the driver; one call of
                // `host.run` from the entered pc covers the function.
                #[cfg(test)]
                {
                    if let Some(pc) = DEFECT.with(Cell::take) {
                        return Stop::Defect(pc);
                    }
                    let pc = body.pcs.get(at as usize).copied().unwrap_or(0);
                    // What a test has its body ask the host for.
                    if let Some((here, ask, next)) = ASK_AT.with(Cell::get) {
                        let asked_here =
                            ASK_IN.with(Cell::get).is_none_or(|f| f == body.code_index);
                        if asked_here && pc <= here {
                            // The interpreter runs up to the
                            // instruction, the host is asked in its
                            // place, and the interpreter runs the rest
                            // from where that leaves the bytecode.
                            let stop = match pc < here {
                                true => host.run(ctx, pc, here),
                                false => Stop::At(here),
                            };
                            if stop != Stop::At(here) {
                                return stop;
                            }
                            let done = match ask {
                                Ask::Push(list, value, dst) => host.push(ctx, list, value, dst),
                                Ask::Copy(dst, src) => host.copy(ctx, dst, src),
                                Ask::Clear(reg) => host.clear(ctx, reg),
                                Ask::Elem(dst, list, index) => host.elem(ctx, dst, list, index),
                                Ask::Quick => {
                                    // Steps a body's machine code
                                    // would have counted by now.
                                    ctx.steps_left -= ASK_STEPS.with(Cell::get);
                                    let answer = host.quick(ctx, here);
                                    QUICK.with(|q| q.set(Some(answer)));
                                    match answer {
                                        1 => true,
                                        2 => return Stop::Raised(here),
                                        _ => false,
                                    }
                                }
                                Ask::Fields(reg) => {
                                    let given = host.fields(ctx, reg).then_some((
                                        ctx.left,
                                        ctx.right,
                                        ctx.sites as usize,
                                    ));
                                    VIEW.with(|v| v.set(Some(given)));
                                    false
                                }
                            };
                            let from = if done { next } else { here };
                            return host.run(ctx, from, body.code_len);
                        }
                    }
                    if THROUGH_AGAIN.with(Cell::get) {
                        // One instruction, as a guess that failed.
                        return host.again(ctx, pc);
                    }
                    let to = match ONE_AT_A_TIME.with(Cell::get) {
                        true => pc.saturating_add(1),
                        false => body.code_len,
                    };
                    host.run(ctx, pc, to)
                }
                #[cfg(not(test))]
                {
                    let _ = (body, at, ctx, host);
                    Stop::Defect(0)
                }
            }
        }
    }

    /// What the section has for function `func`: neither an entry nor
    /// a body, for most, and for one the module does not have.
    #[inline(always)]
    pub(super) fn has(&self, func: usize) -> Has {
        self.has.get(func).copied().unwrap_or(Has::NEITHER)
    }

    /// The entry of function `func` that takes the `n` arguments
    /// `slot(k, ty)` gives a slot for (`None` when argument `k` is not a
    /// `ty`): its index, its result type and the arguments' slots.
    /// `None` when the function has no entry of that row.
    #[inline]
    pub(super) fn entry(
        &self,
        func: usize,
        n: usize,
        slot: impl Fn(usize, Ty) -> Option<u64>,
    ) -> Option<(usize, Ty, [u64; MAX_ARGS])> {
        let &(first, count) = self.of.get(func)?;
        if count == 0 {
            return None;
        }
        self.rows
            .get(first..first.saturating_add(count))?
            .iter()
            .enumerate()
            .find_map(|(k, (row, ret))| {
                if row.len() != n {
                    return None;
                }
                let mut slots = [0u64; MAX_ARGS];
                for ((to, ty), arg) in slots.iter_mut().zip(row).zip(0..) {
                    *to = slot(arg, *ty)?;
                }
                Some((first.saturating_add(k), *ret, slots))
            })
    }

    /// Run entry `entry` on `args`: its result, or the error the
    /// tree-walker raises where native code left. The caller has counted
    /// the call's own depth ([`Interp::enter_call`]) and undoes it; the
    /// interpreter's step count is brought up to date here either way.
    pub(super) fn run(
        &self,
        interp: &mut Interp,
        entry: usize,
        ret: Ty,
        args: &[u64],
    ) -> Result<Value, Diagnostic> {
        let budget = budget_for(interp);
        let depth = i64::try_from(interp.calls_left()).unwrap_or(i64::MAX);
        let mut ctx = Context::new(budget, depth);
        let Some(code) = self.code.as_ref() else {
            return Err(internal(
                "the VM called a native entry on a code mapping it does not have".to_string(),
            ));
        };
        let Some((status, slot)) = code.call(entry, &mut ctx, args) else {
            return Err(internal(format!(
                "the VM called native entry {entry} with {} arguments it does not take",
                args.len()
            )));
        };
        let before = interp.steps();
        // One step for each mark native code passed.
        let took = u64::try_from(budget.saturating_sub(ctx.steps_left)).unwrap_or(0);
        if status == 0 {
            interp.add_steps(took);
            // It passed no more marks than the budget had steps for, and
            // gave none back.
            if ctx.steps_left < 0 || ctx.steps_left > budget {
                return Err(internal(format!(
                    "native entry {entry} returned with {} steps left of {budget}",
                    ctx.steps_left
                )));
            }
            return Ok(value(ret, slot));
        }
        let Some(site) = self.sites.get(ctx.site as usize) else {
            interp.add_steps(took);
            return Err(internal(format!(
                "native code left with status {status} at site {}, which the program does not have",
                ctx.site
            )));
        };
        let span = Span::new(site.start as usize, site.end as usize, site.line, site.col);
        let wrong = |what: &str| {
            internal(format!(
                "native code left at site {} with status {status} {what}",
                ctx.site
            ))
            .at(span)
        };
        if u8::try_from(status).ok().and_then(Exit::from_u8) != Some(site.kind) {
            interp.add_steps(took);
            return Err(wrong("that is not the site's kind"));
        }
        if site.kind == Exit::Budget {
            // Native code takes a step at a mark and leaves when that
            // took it below zero: with -1, having passed every mark the
            // budget had steps for and the one that spent it.
            if ctx.steps_left != -1 {
                interp.add_steps(took);
                return Err(wrong("and not with the budget one step overspent"));
            }
            // Every mark but that last is counted here. The last is the
            // interpreter's own tick, which raises E515 at the mark, and
            // words it by whether a loop is running: one of the VM's
            // frames' (the interpreter's count), the mark's own, or one
            // of a native caller's on the way out (the flag).
            interp.set_steps(before.saturating_add(u64::try_from(budget).unwrap_or(0)));
            let in_loop = site.in_loop || ctx.flags & 1 != 0;
            if in_loop {
                interp.loop_depth = interp.loop_depth.saturating_add(1);
            }
            let spent = interp.tick(span);
            if in_loop {
                interp.loop_depth = interp.loop_depth.saturating_sub(1);
            }
            return Err(spent
                .err()
                .unwrap_or_else(|| wrong("and the step budget is not spent")));
        }
        interp.add_steps(took);
        let raised = match site.kind {
            Exit::Operator => match bin_op(site.op) {
                Some(op) => binary(
                    op,
                    value(site.left, ctx.left),
                    value(site.right, ctx.right),
                    span,
                )
                .err(),
                None => None,
            },
            Exit::Negate => negate(value(Ty::Int, ctx.left), span).err(),
            Exit::Range => range_fits(int(ctx.left), int(ctx.right), span).err(),
            Exit::Depth => Some(depth_exceeded(span)),
            Exit::Budget => None,
        };
        Err(raised.unwrap_or_else(|| wrong("where the interpreter raises nothing")))
    }
}

/// The steps left of the budget as native code sees it: all there are
/// when there is no budget or more is left than a count holds. `Native`
/// gives this to an entry at a call and to a body at [`enter_body`].
pub(crate) fn budget_for(interp: &Interp) -> i64 {
    match interp.max_steps() {
        0 => i64::MAX,
        max => i64::try_from(max.saturating_sub(interp.steps())).unwrap_or(i64::MAX),
    }
}

/// A slot's bits as an int.
fn int(slot: u64) -> i64 {
    i64::from_ne_bytes(slot.to_ne_bytes())
}

/// The value a slot of type `ty` holds.
fn value(ty: Ty, slot: u64) -> Value {
    match ty {
        Ty::Null => Value::Null,
        Ty::Bool => Value::Bool(slot != 0),
        Ty::Int => Value::Int(int(slot)),
        Ty::Float => Value::Float(f64::from_bits(slot)),
    }
}

/// `v` as a slot of type `ty`, when it is of that type.
#[inline(always)]
pub(super) fn slot(ty: Ty, v: &Value) -> Option<u64> {
    match (ty, v) {
        (Ty::Int, Value::Int(i)) => Some(u64::from_ne_bytes(i.to_ne_bytes())),
        (Ty::Float, Value::Float(f)) => Some(f.to_bits()),
        (Ty::Bool, Value::Bool(b)) => Some(u64::from(*b)),
        (Ty::Null, Value::Null) => Some(0),
        _ => None,
    }
}

/// The operator a site names, among those that can leave.
fn bin_op(o: cig_native::Op) -> Option<BinOp> {
    Some(match o {
        op::ADD => BinOp::Add,
        op::SUB => BinOp::Sub,
        op::MUL => BinOp::Mul,
        op::DIV => BinOp::Div,
        op::REM => BinOp::Rem,
        op::LT => BinOp::Lt,
        op::LE => BinOp::Le,
        op::GT => BinOp::Gt,
        op::GE => BinOp::Ge,
        _ => return None,
    })
}

#[cfg(test)]
thread_local! {
    /// How many times this thread entered a body: a test that runs a
    /// program through the driver reads it to know the driver ran.
    pub(crate) static ENTERED: Cell<u64> = const { Cell::new(0) };
    /// A pc the tests' body reports a defect at, once, when it is next
    /// entered: how a test sees what the VM does with one.
    pub(crate) static DEFECT: Cell<Option<u32>> = const { Cell::new(None) };
    /// The tests' body hands the interpreter one instruction and comes
    /// back, instead of the rest of the function: the body and the
    /// bytecode then take turns at every instruction.
    pub(crate) static ONE_AT_A_TIME: Cell<bool> = const { Cell::new(false) };
    /// The tests' bodies run the instruction they are entered at
    /// through `Host::again`, as a guess that failed, and come back.
    pub(crate) static THROUGH_AGAIN: Cell<bool> = const { Cell::new(false) };
    /// The tests' bodies are given up when their guesses keep failing,
    /// as a program's are.
    pub(crate) static IMPATIENT: Cell<bool> = const { Cell::new(false) };
    /// A body's `run` stops at a call of a compiled function instead of
    /// carrying it through, as it does where the stack has no room: the
    /// VM's own loop makes every call.
    pub(crate) static LEAVE_AT_CALLS: Cell<bool> = const { Cell::new(false) };
    /// How many calls this thread's bodies carried through to their
    /// return (`run` made the call and ran the callee under the body).
    pub(crate) static CARRIED: Cell<u64> = const { Cell::new(0) };
    /// The tests' body, when it reaches this pc, asks the host for
    /// this and, when the host did it, goes on at the last pc: as
    /// machine code does where it has the runtime do what an
    /// instruction would.
    pub(crate) static ASK_AT: Cell<Option<(u32, Ask, u32)>> = const { Cell::new(None) };
    /// The function whose body asks ([`ASK_AT`]); every function's
    /// when none is named.
    pub(crate) static ASK_IN: Cell<Option<usize>> = const { Cell::new(None) };
    /// How many steps a test's body takes from the context, as its
    /// machine code would have counted them, before it asks for
    /// `quick`.
    pub(crate) static ASK_STEPS: Cell<i64> = const { Cell::new(0) };
    /// What the host last answered a test's body that asked for
    /// `quick`.
    pub(crate) static QUICK: Cell<Option<u32>> = const { Cell::new(None) };
    /// What the host last answered a test's body that asked for a
    /// map's `fields`: `None` inside when it gave no view, else
    /// `ctx.left`, `ctx.right` and the address in `ctx.sites`.
    pub(crate) static VIEW: Cell<Option<Option<(u64, u64, usize)>>> = const { Cell::new(None) };
}

/// What a test's body asks its host for ([`ASK_AT`]).
#[cfg(test)]
#[derive(Clone, Copy)]
pub(crate) enum Ask {
    /// `push(list, value, dst)`.
    Push(u32, u32, u32),
    /// `copy(dst, src)`.
    Copy(u32, u32),
    /// `clear(reg)`.
    Clear(u32),
    /// `quick(pc)`, at the pc the body asks at. What it answered is
    /// kept in [`QUICK`].
    Quick,
    /// `elem(dst, list, index)`.
    Elem(u32, u32, u32),
    /// `fields(reg)`: what it gave is kept in [`VIEW`], and the
    /// bytecode goes on as if the host had done nothing.
    Fields(u32),
}

thread_local! {
    /// Where this thread's stack starts and how long it is, once the
    /// thread has said ([`stack_starts_here`]).
    static STACK: Cell<Option<(usize, usize)>> = const { Cell::new(None) };
}

/// The stack kept free under native code beyond what its own calls may
/// use: for the entry's frame, and for the VM to go on with whatever
/// comes back.
const SPARE: usize = 1 << 20;

/// Say that the calling frame is at the start of a stack of `size`
/// bytes. The thread a run is on says it once, before the run. A thread
/// that never says it runs no native code: every call runs the bytecode.
pub fn stack_starts_here(size: usize) {
    let mark = 0u8;
    let here = std::ptr::addr_of!(mark) as usize;
    STACK.with(|s| s.set(Some((here, size))));
}

/// The stack has room for `bytes` of native code and [`SPARE`] to
/// spare under it, from here down.
#[inline(never)]
fn has_room_for(bytes: usize) -> bool {
    let mark = 0u8;
    let here = std::ptr::addr_of!(mark) as usize;
    let Some((start, size)) = STACK.with(Cell::get) else {
        return false;
    };
    // The stack grows down from its start.
    let left = size.saturating_sub(start.saturating_sub(here));
    left >= bytes.saturating_add(SPARE)
}

/// The stack has room for native code whose calls nest `calls` deep, at
/// [`MAX_FRAME`] bytes a call, with [`SPARE`] to spare. Native code is
/// entered only when it has: its depth is not cut to fit (that would
/// move E506), so a call that finds no room runs the bytecode.
#[inline]
pub(super) fn has_room(calls: usize) -> bool {
    has_room_for(calls.saturating_mul(MAX_FRAME))
}

/// The stack has room for one body's call, at [`MAX_BODY_FRAME`] bytes,
/// with [`SPARE`] to spare. A body is entered only where it has.
#[inline]
pub(super) fn has_body_room() -> bool {
    has_room_for(MAX_BODY_FRAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A body of a ten-word function with resume points at pcs 0, 4
    /// and 7.
    fn entry() -> BodyEntry {
        BodyEntry::resolve(0, &[0, 4, 7], &ten_nops()).expect("a body")
    }

    /// A function of ten `nop`s.
    fn ten_nops() -> Vec<Option<DecodedInstr>> {
        let words = vec![cig_bytecode::Instr::abc(cig_bytecode::Op::NOP, 0, 0, 0); 10];
        let (_, code) = cig_bytecode::decode_table(&words).expect("decodes");
        code
    }

    #[test]
    fn one_load_says_what_the_section_has_for_a_function() {
        // Five functions: 1 has two entries, 2 a body, 3 both, 0 and 4
        // nothing.
        let entry = |func, args: Vec<Ty>| cig_native::Entry {
            func,
            offset: 0,
            args,
            ret: Ty::Int,
        };
        let entries = vec![
            entry(1, vec![Ty::Int]),
            entry(1, vec![Ty::Float]),
            entry(3, vec![Ty::Int]),
        ];
        let raw = vec![None, None, Some((0, vec![0])), Some((1, vec![0])), None];
        let bodies = (0..5).map(|_| OnceCell::new()).collect();
        let native = Native::build(None, Vec::new(), entries, 5, raw, bodies);
        let entries: Vec<bool> = (0..6).map(|f| native.has(f).entry()).collect();
        let bodied: Vec<bool> = (0..6).map(|f| native.has(f).body()).collect();
        // The sixth is no function of the module: it has nothing.
        assert_eq!(entries, [false, true, false, true, false, false]);
        assert_eq!(bodied, [false, false, true, true, false, false]);
        assert_eq!(native.with_bodies().collect::<Vec<_>>(), [2, 3]);
        assert_eq!((native.len(), native.bodies()), (3, 2));

        // The tests' own: a body for every function, no entry.
        let every = Native::for_tests_every_pc(3);
        assert!((0..3).all(|f| every.has(f).body() && !every.has(f).entry()));
        assert_eq!(every.has(3), Has::NEITHER);

        // Not known yet is taken to have both: the exact way is gone.
        assert!(Has::UNKNOWN.entry() && Has::UNKNOWN.body());
        assert!(!Has::NEITHER.entry() && !Has::NEITHER.body());
    }

    #[test]
    fn a_body_whose_guesses_keep_failing_loses_the_resume_point_it_was_entered_at() {
        let b = entry();
        // One short of the window, a step of its own for each: nothing
        // is decided.
        for n in 0..MISS_WINDOW - 1 {
            b.counted(1);
            assert!(!b.missed(1), "miss {n}");
        }
        assert_eq!(b.at(4), Some(1));
        // The window's last, with a step a miss: given up.
        b.counted(1);
        assert!(b.missed(1));
        assert_eq!(b.at(4), None, "the resume point is one no longer");
        assert_eq!((b.at(0), b.at(7)), (Some(0), Some(2)), "the others stay");
    }

    #[test]
    fn the_fast_loops_table_has_no_instruction_at_a_resume_point_until_it_is_given_up() {
        let b = entry();
        {
            let table = b.between().expect("nothing holds it");
            let blank: Vec<usize> = (0..10)
                .filter(|pc| {
                    table.code[*pc].is_none() && table.words[*pc].op == cig_bytecode::Op::WIDE
                })
                .collect();
            assert_eq!(
                blank,
                vec![0, 4, 7],
                "the loop stops before each resume point"
            );
        }
        // The resume point at pc 4 is given up: its instruction is back,
        // and the loop runs through it.
        for _ in 0..MISS_WINDOW - 1 {
            assert!(!b.missed(1));
        }
        assert!(b.missed(1));
        let table = b.between().expect("nothing holds it");
        let blank: Vec<usize> = (0..10)
            .filter(|pc| table.code[*pc].is_none() && table.words[*pc].op == cig_bytecode::Op::WIDE)
            .collect();
        assert_eq!(blank, vec![0, 7]);
        assert_eq!(table.code[4].map(|d| d.pc), Some(4));
        assert!(
            table.words[4].op != cig_bytecode::Op::WIDE,
            "and its word with it"
        );
    }

    #[test]
    fn a_body_whose_resume_point_starts_no_instruction_is_not_one() {
        // The second word of a two-word instruction, and a pc past the
        // function.
        let mut code = ten_nops();
        code[3] = None;
        assert!(BodyEntry::resolve(0, &[0, 3], &code).is_none());
        assert!(BodyEntry::resolve(0, &[10], &code).is_none());
        assert!(BodyEntry::resolve(0, &[0, 4], &code).is_some());
    }

    #[test]
    fn a_body_whose_guesses_fail_now_and_then_is_kept() {
        let b = entry();
        // A miss every STEPS_A_MISS steps of its own, for ten windows:
        // never given up, and the count starts again each time.
        for n in 0..u64::from(MISS_WINDOW) * 10 {
            b.counted(STEPS_A_MISS);
            assert!(!b.missed(2), "miss {n}");
        }
        assert_eq!(b.at(7), Some(2));
        // Then one a step: the next full window gives it up.
        let mut gave_up = false;
        for _ in 0..MISS_WINDOW * 2 {
            b.counted(1);
            if b.missed(2) {
                gave_up = true;
                break;
            }
        }
        assert!(gave_up);
        assert_eq!(b.at(7), None);
    }

    #[test]
    fn a_bodys_failures_are_set_against_its_own_steps_not_the_runs() {
        // A body entered now and then by a program that does other
        // work: two steps of its own and a failed guess at each entry,
        // whatever the rest of the run counts meanwhile. Its own rate is
        // one failure in two steps, and the first window gives it up.
        let b = entry();
        for n in 0..MISS_WINDOW - 1 {
            b.counted(2);
            assert!(!b.missed(0), "miss {n}");
        }
        b.counted(2);
        assert!(b.missed(0), "the window's last");
        assert_eq!(b.at(0), None);
        // A window starts from nothing: the steps of the one before it
        // are not carried over to excuse the next.
        let b = entry();
        b.counted(u64::from(MISS_WINDOW) * STEPS_A_MISS);
        for _ in 0..MISS_WINDOW {
            assert!(!b.missed(1), "a window with enough work of its own");
        }
        let mut gave_up = false;
        for _ in 0..MISS_WINDOW {
            gave_up = b.missed(1);
        }
        assert!(gave_up, "and the next, with none");
        assert_eq!(b.at(4), None);
    }

    const SIZE: usize = 64 << 20;

    /// `has_room(calls)` from `frames` frames further down the stack,
    /// each holding a page.
    fn from_deeper(frames: usize, calls: usize) -> bool {
        let page = [0u8; 4096];
        if frames == 0 {
            return has_room(calls);
        }
        let room = from_deeper(frames - 1, calls);
        std::hint::black_box(&page);
        room
    }

    #[test]
    fn native_code_is_entered_only_where_the_stack_is_known_to_have_room() {
        // A thread that never said where its stack starts has none.
        std::thread::spawn(|| assert!(!has_room(0))).join().unwrap();
        std::thread::Builder::new()
            .stack_size(SIZE)
            .spawn(|| {
                stack_starts_here(SIZE);
                // The deepest recursion the depth limit allows, with the
                // spare, is a quarter of this stack.
                assert!(has_room(0) && has_room(4000));
                // More calls than the whole stack holds, with or without
                // the spare.
                assert!(!has_room(SIZE / MAX_FRAME));
                assert!(!has_room(SIZE / MAX_FRAME - 200));
                assert!(!has_room(usize::MAX));
                // Calls that, with the spare, need all but a mebibyte of
                // the stack: they fit from here, and not from 4 MiB
                // further down.
                let most = (SIZE - (2 << 20)) / MAX_FRAME;
                assert!(has_room(most));
                assert!(!from_deeper(1024, most));
                assert!(from_deeper(1024, 4000));
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn a_slot_is_its_values_bits_and_only_of_its_own_type() {
        let cases = [
            (Ty::Int, Value::Int(-2)),
            (Ty::Int, Value::Int(i64::MIN)),
            (Ty::Float, Value::Float(-0.0)),
            (Ty::Float, Value::Float(f64::INFINITY)),
            (Ty::Bool, Value::Bool(true)),
            (Ty::Bool, Value::Bool(false)),
            (Ty::Null, Value::Null),
        ];
        for (ty, v) in &cases {
            let s = slot(*ty, v).expect("of its type");
            assert!(value(*ty, s).equals(v), "{ty:?}");
            // And of no other type.
            for other in [Ty::Null, Ty::Bool, Ty::Int, Ty::Float] {
                assert_eq!(
                    slot(other, v).is_some(),
                    other == *ty,
                    "{ty:?} as {other:?}"
                );
            }
        }
        assert_eq!(slot(Ty::Float, &Value::Float(-0.0)), Some(1 << 63));
        assert!(matches!(value(Ty::Float, f64::NAN.to_bits()), Value::Float(f) if f.is_nan()));
        assert!(slot(Ty::Int, &Value::str("7")).is_none());
        // Every operator a site can name is one of cig's.
        for o in 0..=255u8 {
            let named = matches!(o, 0..=4 | 7..=10);
            assert_eq!(bin_op(o).is_some(), named, "{o}");
        }
    }
}
