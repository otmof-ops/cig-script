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

use crate::diagnostics::{internal, Diagnostic};
use crate::interp::ops::{binary, negate, range_fits};
use crate::interp::{depth_exceeded, Interp};
use crate::syntax::ast::BinOp;
use crate::syntax::span::Span;
use crate::value::Value;
use cig_native::{op, Code, Context, Exit, Site, Tables, Ty, MAX_ARGS, MAX_FRAME};
use std::cell::Cell;

/// A program's native code, as the VM calls it.
pub struct Native {
    code: Code,
    sites: Vec<Site>,
    /// Every entry's row and result type, in the tables' order: an
    /// entry's index here is its index for [`Code::call`].
    rows: Vec<(Vec<Ty>, Ty)>,
    /// For each function of the module, where its entries start in
    /// `rows` and how many it has: none, for most.
    of: Vec<(usize, usize)>,
}

impl Native {
    /// `code` armed with `tables`, for a module of `funcs` functions.
    /// The tables were checked against that module when they were
    /// loaded (`crate::native::Source::load`): every entry is for one of
    /// its functions, with that function's parameter count.
    pub fn new(code: Code, tables: Tables, funcs: usize) -> Native {
        let mut of = vec![(0, 0); funcs];
        let mut rows = Vec::with_capacity(tables.entries.len());
        for (i, e) in tables.entries.into_iter().enumerate() {
            // Entries are sorted by function: one function's are one run.
            if let Some((first, count)) = of.get_mut(e.func as usize) {
                if *count == 0 {
                    *first = i;
                }
                *count += 1;
            }
            rows.push((e.args, e.ret));
        }
        Native {
            code,
            sites: tables.sites,
            rows,
            of,
        }
    }

    /// How many entries it has.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// It has no entry.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
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
        // The steps left of the budget: all there are, when there is no
        // budget or more is left than a count holds.
        let budget = match interp.max_steps {
            0 => i64::MAX,
            max => i64::try_from(max.saturating_sub(interp.steps)).unwrap_or(i64::MAX),
        };
        let depth = i64::try_from(interp.calls_left()).unwrap_or(i64::MAX);
        let mut ctx = Context::new(budget, depth);
        let Some((status, slot)) = self.code.call(entry, &mut ctx, args) else {
            return Err(internal(format!(
                "the VM called native entry {entry} with {} arguments it does not take",
                args.len()
            )));
        };
        let before = interp.steps;
        // One step for each mark native code passed.
        let took = u64::try_from(budget.saturating_sub(ctx.steps_left)).unwrap_or(0);
        if status == 0 {
            interp.steps = interp.steps.saturating_add(took);
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
            interp.steps = interp.steps.saturating_add(took);
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
            interp.steps = interp.steps.saturating_add(took);
            return Err(wrong("that is not the site's kind"));
        }
        if site.kind == Exit::Budget {
            // Native code takes a step at a mark and leaves when that
            // took it below zero: with -1, having passed every mark the
            // budget had steps for and the one that spent it.
            if ctx.steps_left != -1 {
                interp.steps = interp.steps.saturating_add(took);
                return Err(wrong("and not with the budget one step overspent"));
            }
            // Every mark but that last is counted here. The last is the
            // interpreter's own tick, which raises E515 at the mark, and
            // words it by whether a loop is running: one of the VM's
            // frames' (the interpreter's count), the mark's own, or one
            // of a native caller's on the way out (the flag).
            interp.steps = before.saturating_add(u64::try_from(budget).unwrap_or(0));
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
        interp.steps = interp.steps.saturating_add(took);
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

/// The stack has room for native code whose calls nest `calls` deep, at
/// [`MAX_FRAME`] bytes a call, with [`SPARE`] to spare. Native code is
/// entered only when it has: its depth is not cut to fit (that would
/// move E506), so a call that finds no room runs the bytecode.
#[inline(never)]
pub(super) fn has_room(calls: usize) -> bool {
    let mark = 0u8;
    let here = std::ptr::addr_of!(mark) as usize;
    let Some((start, size)) = STACK.with(Cell::get) else {
        return false;
    };
    // The stack grows down from its start.
    let left = size.saturating_sub(start.saturating_sub(here));
    left >= calls.saturating_mul(MAX_FRAME).saturating_add(SPARE)
}

#[cfg(test)]
mod tests {
    use super::*;

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
