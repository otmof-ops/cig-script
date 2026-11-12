// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The bytecode VM's core (stage B, otmof-ops/cig-compile#31).
//!
//! The VM runs a module of the register bytecode `crates/cig-bytecode`
//! defines (`docs/BYTECODE.md` is the contract). It takes only a
//! [`Verified`] module, which only the verifier makes, and it runs every
//! operator, builtin, step and error through the interpreter's own code:
//!
//! - the operators are [`crate::interp::ops`], which the tree-walker calls
//!   too;
//! - a builtin is called through [`Interp::call_builtin`], the same entry
//!   as a call in the tree-walker;
//! - the step budget is [`Interp::tick`], its counter and its E515;
//! - the call depth is [`Interp::enter_call`], its limit and its E506.
//!
//! A [`Program`] is the module decoded once, at load, into a per-pc table
//! through [`cig_bytecode::instr::walk`] (which goes through the crate's
//! one decoder), with each function's constants made into values and each
//! pc's source position read from its line table. Loading also checks
//! what the verifier cannot know: every `callb` index against the builtin
//! catalogue and every `burn.op` kind against the burn-op catalogue, both
//! refused with E1050.
//!
//! The instructions of later packages (closures that capture values,
//! cells, lists and maps, `try`, burns, `pack`, `exhale`, chains,
//! interpolation, the iterating loops) stop with one internal error that
//! names the instruction; [`NOT_BUILT`] lists them.

#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing
    )
)]

use crate::diagnostics::{internal, usage, Diagnostic};
use crate::interp::ops::{binary, fast_binary, negate};
use crate::interp::{arity_mismatch, Interp};
use crate::syntax::ast::BinOp;
use crate::syntax::span::Span;
use crate::value::{Builtin, FnBody, Function, Value};
use cig_bytecode::instr::walk;
use cig_bytecode::{
    Constant, DecodedInstr, FuncKind, Function as BcFunction, LineEntry, Op, SpanBase, Verified,
    VerifyError,
};
use std::rc::Rc;

#[cfg(test)]
mod tests;

/// The burn-op kinds a `burn.op` may name, by index. The VM refuses a
/// module whose `burn.op` names a kind past the end (E1050). The burn
/// instructions are built by a later package, which owns this table.
pub const BURN_OP_KINDS: [&str; 9] = [
    "write",
    "append",
    "delete",
    "move",
    "copy",
    "mkdir",
    "proc",
    "env_set",
    "env_unset",
];

/// The instructions this VM does not run yet: each stops with an internal
/// error that names it. A later package builds them, one at a time.
pub const NOT_BUILT: [Op; 30] = [
    Op::FORPREP,
    Op::FORLOOP,
    Op::ITERPREP,
    Op::ITERNEXT,
    Op::NEWLIST,
    Op::NEWMAP,
    Op::GETFIELD,
    Op::SETFIELD,
    Op::GETINDEX,
    Op::SETINDEX,
    Op::APPENDLIST,
    Op::INSERTMAP,
    Op::INTERP,
    Op::CALLM,
    Op::GETUP,
    Op::SETUP,
    Op::CELLNEW,
    Op::CELLGET,
    Op::CELLSET,
    Op::TRY_ENTER,
    Op::TRY_LEAVE,
    Op::THROW,
    Op::BURN_ENTER,
    Op::BURN_OP,
    Op::BURN_LEAVE,
    Op::COMP_ENTER,
    Op::COMP_LEAVE,
    Op::PACK,
    Op::EXHALE,
    Op::CHAIN_MAKE,
];

const RECOMPILE_HINT: &str =
    "compile it again (cigcompile build), or fetch it again from where it came from";

/// A loaded module: every function decoded once, ready to run.
pub struct Program {
    /// The module's functions in the verifier's pre-order: 0 is the top
    /// level, so `fn<i>` in an error names `funcs[i]`.
    funcs: Vec<Proto>,
    /// The builtin behind `pow`/`powk`: `math.pow`, the language's only
    /// power operation.
    pow: Option<&'static Builtin>,
}

/// One function of a loaded module.
struct Proto {
    name: String,
    lambda: bool,
    reg_count: usize,
    arity: usize,
    upvalues: usize,
    /// The decoded instruction at each first-word pc; `None` at the second
    /// word of a `WIDE` pair, which nothing may jump to.
    code: Vec<Option<DecodedInstr>>,
    /// The source position of each pc, from the line table.
    spans: Vec<Span>,
    consts: Vec<Value>,
    /// Indices into [`Program::funcs`] of this function's children.
    children: Vec<usize>,
}

/// A function value of a loaded module: what `closure` makes and `call`
/// calls. It lives in a [`Value::Func`] as [`FnBody::Compiled`].
#[derive(Clone)]
pub struct CompiledFn {
    program: Rc<Program>,
    index: usize,
}

/// E1050 for a module the verifier refused, worded as `docs/BYTECODE.md`
/// says: `the program's bytecode does not verify: <rule> at fn<i>+<pc>`.
pub fn refusal(e: &VerifyError) -> Diagnostic {
    usage(format!("the program's bytecode does not verify: {e}"))
        .code("E1050")
        .with_hint(RECOMPILE_HINT)
}

/// E1050 for a rule the VM checks at load.
fn refused(rule: &str, func: usize, pc: u32) -> Diagnostic {
    usage(format!(
        "the program's bytecode does not verify: {rule} at fn{func}+{pc}"
    ))
    .code("E1050")
    .with_hint(RECOMPILE_HINT)
}

/// The internal error of an instruction a later package builds.
pub(crate) fn not_built(op: Op, span: Span) -> Diagnostic {
    internal(format!(
        "the VM cannot run `{op}` yet: the package that builds it has not landed (cig-compile#31)"
    ))
    .at(span)
}

/// The internal error of a compiled function called from outside the VM
/// (a builtin's callback, the tree-walker): re-entry is a later package.
pub(crate) fn not_built_reentry(span: Span) -> Diagnostic {
    internal(
        "the VM cannot be re-entered to run a compiled function yet: the package that builds it has not landed (cig-compile#31)",
    )
    .at(span)
}

/// An invariant the verifier guarantees did not hold. A verified module
/// never gets here; the VM stops with an internal error instead of
/// panicking if one ever does.
#[cold]
fn broken(what: &str, span: Span) -> Diagnostic {
    internal(format!("the VM found {what} in a verified module")).at(span)
}

/// Load a verified module: decode every function once and check what the
/// verifier leaves to the VM (E1050 for an unknown builtin or burn-op
/// kind).
pub fn load(v: Verified<'_>) -> Result<Rc<Program>, Diagnostic> {
    let catalogue = crate::stdlib::builtin_catalogue();
    let mut funcs = Vec::new();
    flatten(&v.module().main, &mut funcs, catalogue.len())?;
    let pow = catalogue.iter().copied().find(|b| b.name == "math.pow");
    Ok(Rc::new(Program { funcs, pow }))
}

/// Append `f` and then its children, recursively, in the verifier's
/// pre-order. The verifier bounds the nesting at 64 levels.
fn flatten(f: &BcFunction, out: &mut Vec<Proto>, n_builtins: usize) -> Result<usize, Diagnostic> {
    let index = out.len();
    let mut code: Vec<Option<DecodedInstr>> = vec![None; f.instrs.len()];
    for d in walk(&f.instrs) {
        let d = d.map_err(|e| {
            internal(format!(
                "the VM could not decode a verified function (fn{index}): {e:?}"
            ))
        })?;
        match d.op {
            Op::CALLB if d.b_u as usize >= n_builtins => {
                return Err(refused("unknown-builtin", index, d.pc));
            }
            Op::BURN_OP => {
                let kind = if d.wide { d.c & 0xFF } else { d.c & 0x0F };
                if kind as usize >= BURN_OP_KINDS.len() {
                    return Err(refused("unknown-burn-op", index, d.pc));
                }
            }
            _ => {}
        }
        if let Some(slot) = code.get_mut(d.pc as usize) {
            *slot = Some(d);
        }
    }
    let proto = Proto {
        name: f.name.clone(),
        lambda: f.kind == FuncKind::Lambda,
        reg_count: f.reg_count as usize,
        arity: f.arity as usize,
        upvalues: f.upvalues.len(),
        code,
        spans: spans(&f.span_base, &f.lines, f.instrs.len()),
        consts: f.constants.iter().map(constant).collect(),
        children: Vec::new(),
    };
    out.push(proto);
    let mut children = Vec::with_capacity(f.children.len());
    for c in &f.children {
        children.push(flatten(c, out, n_builtins)?);
    }
    if let Some(p) = out.get_mut(index) {
        p.children = children;
    }
    Ok(index)
}

fn constant(c: &Constant) -> Value {
    match c {
        Constant::Bool(b) => Value::Bool(*b),
        Constant::Int(i) => Value::Int(*i),
        Constant::Float(f) => Value::Float(*f),
        Constant::Str(s) => Value::str(s),
    }
}

/// The source position of every pc: its own line row, else the nearest
/// row before it, else the function's first token. The rows are strictly
/// ascending (verified).
fn spans(base: &SpanBase, lines: &[LineEntry], n: usize) -> Vec<Span> {
    let first = Span::new(
        base.byte_offset as usize,
        base.byte_offset as usize,
        base.line,
        base.col,
    );
    let mut out = Vec::with_capacity(n);
    let mut rows = lines.iter().peekable();
    let mut current = first;
    for pc in 0..n {
        while let Some(row) = rows.peek() {
            if row.pc as usize > pc {
                break;
            }
            current = row_span(base, row);
            rows.next();
        }
        out.push(current);
    }
    out
}

fn row_span(base: &SpanBase, row: &LineEntry) -> Span {
    let start = (base.byte_offset as usize).saturating_add(row.span_off as usize);
    Span::new(
        start,
        start.saturating_add(row.span_len as usize),
        base.line.saturating_add(row.line_off),
        row.col,
    )
}

/// Run a loaded module's top level and return what it returns.
pub fn run(interp: &mut Interp, program: &Rc<Program>) -> Result<Value, Diagnostic> {
    run_function(interp, program, 0, Vec::new())
}

/// Run one function of a loaded module with `args` as its parameters.
pub(crate) fn run_function(
    interp: &mut Interp,
    program: &Rc<Program>,
    index: usize,
    args: Vec<Value>,
) -> Result<Value, Diagnostic> {
    let proto = program
        .funcs
        .get(index)
        .ok_or_else(|| broken("no such function", Span::default()))?;
    if args.len() != proto.arity {
        return Err(internal(format!(
            "the VM was asked to run fn{index} with {} arguments; it takes {}",
            args.len(),
            proto.arity
        )));
    }
    let mut m = Machine {
        interp,
        stack: Vec::new(),
        frames: Vec::new(),
    };
    let first = proto.spans.first().copied().unwrap_or_default();
    m.grow(proto.reg_count, first)?;
    for (slot, a) in m.stack.iter_mut().zip(args) {
        *slot = a;
    }
    m.frames.push(Frame {
        func: index,
        pc: 0,
        base: 0,
        ret: None,
        held_calls: 0,
        nest: 0,
        held_nest: 0,
    });
    let result = m.exec(program);
    // Whatever stopped the run, the interpreter's depth and loop counters
    // go back to what they were, as the tree-walker's do on its way out.
    while let Some(f) = m.frames.pop() {
        m.release(&f);
    }
    result
}

/// Where a call's result goes in the caller: the absolute register of
/// `R(A)` and how many registers after it the call row also covers.
#[derive(Clone, Copy)]
struct Ret {
    reg: usize,
    extra: usize,
}

struct Frame {
    func: usize,
    pc: usize,
    /// The frame's `R(0)` in the register stack.
    base: usize,
    ret: Option<Ret>,
    /// Calls this frame holds on the interpreter's depth count: its own
    /// call, and each tail call that replaced a frame in it.
    held_calls: usize,
    /// The frame's loop nesting now, as its last `steps` set it.
    nest: u32,
    /// Loop nesting held for frames a tail call replaced.
    held_nest: u32,
}

struct Machine<'i> {
    interp: &'i mut Interp,
    /// Every frame's registers, frame after frame.
    stack: Vec<Value>,
    frames: Vec<Frame>,
}

/// What an instruction did to the flow.
enum Flow {
    Next(usize),
    Return(Value),
    /// A call pushed or replaced a frame, which carries its own pc.
    Switched,
}

impl Machine<'_> {
    fn grow(&mut self, by: usize, span: Span) -> Result<(), Diagnostic> {
        let len = self.stack.len().saturating_add(by);
        crate::value::check_alloc(
            (len as u128).saturating_mul(std::mem::size_of::<Value>() as u128),
            "the VM's registers",
            span,
        )?;
        self.stack.resize(len, Value::Null);
        Ok(())
    }

    fn release(&mut self, f: &Frame) {
        let held = f.nest.saturating_add(f.held_nest);
        self.interp.loop_depth = self.interp.loop_depth.saturating_sub(held);
        for _ in 0..f.held_calls {
            self.interp.leave_call();
        }
    }

    fn get(&self, base: usize, r: u32, span: Span) -> Result<&Value, Diagnostic> {
        self.stack
            .get(base.saturating_add(r as usize))
            .ok_or_else(|| broken("a register past its frame", span))
    }

    fn set(&mut self, base: usize, r: u32, v: Value, span: Span) -> Result<(), Diagnostic> {
        let slot = self
            .stack
            .get_mut(base.saturating_add(r as usize))
            .ok_or_else(|| broken("a register past its frame", span))?;
        *slot = v;
        Ok(())
    }

    /// `R(first..first+n)`, cloned: a call's arguments.
    fn window(
        &self,
        base: usize,
        first: u32,
        n: u32,
        span: Span,
    ) -> Result<Vec<Value>, Diagnostic> {
        let start = base.saturating_add(first as usize);
        self.stack
            .get(start..start.saturating_add(n as usize))
            .map(<[Value]>::to_vec)
            .ok_or_else(|| broken("a register range past its frame", span))
    }

    fn exec(&mut self, program: &Rc<Program>) -> Result<Value, Diagnostic> {
        loop {
            let (fi, pc, base) = match self.frames.last() {
                Some(f) => (f.func, f.pc, f.base),
                None => return Err(broken("an empty call stack", Span::default())),
            };
            let proto = program
                .funcs
                .get(fi)
                .ok_or_else(|| broken("no such function", Span::default()))?;
            let span = proto.spans.get(pc).copied().unwrap_or_default();
            let d = proto
                .code
                .get(pc)
                .copied()
                .flatten()
                .ok_or_else(|| broken("a pc that starts no instruction", span))?;
            match self.step(program, proto, &d, base, span)? {
                Flow::Next(next) => {
                    if let Some(f) = self.frames.last_mut() {
                        f.pc = next;
                    }
                }
                Flow::Return(v) => {
                    if let Some(v) = self.ret(v, span)? {
                        return Ok(v);
                    }
                }
                Flow::Switched => {}
            }
        }
    }

    fn step(
        &mut self,
        program: &Rc<Program>,
        proto: &Proto,
        d: &DecodedInstr,
        base: usize,
        span: Span,
    ) -> Result<Flow, Diagnostic> {
        let pc = d.pc as usize;
        let next = pc + d.width as usize;
        match d.op {
            Op::NOP => {}

            // ----- moves and constants ---------------------------------------
            Op::MOV => {
                let v = self.get(base, d.b_u, span)?.clone();
                self.set(base, d.a, v, span)?;
            }
            Op::LOADK => {
                let v = konst(proto, d.b_u, span)?;
                self.set(base, d.a, v, span)?;
            }
            Op::LOADNULL => self.set(base, d.a, Value::Null, span)?,
            Op::LOADBOOL => self.set(base, d.a, Value::Bool(d.b_u != 0), span)?,
            Op::LOADINT => self.set(base, d.a, Value::Int(i64::from(d.b_s)), span)?,

            // ----- arithmetic, comparison, logic ------------------------------
            Op::ADD | Op::SUB | Op::MUL | Op::DIV | Op::MOD | Op::EQ | Op::LT | Op::LE => {
                let l = self.get(base, d.b_u, span)?.clone();
                let r = self.get(base, d.c, span)?.clone();
                let v = operate(op_of(d.op), l, r, span)?;
                self.set(base, d.a, v, span)?;
            }
            Op::ADDK | Op::SUBK | Op::MULK | Op::DIVK | Op::MODK | Op::EQK | Op::LTK | Op::LEK => {
                let l = self.get(base, d.b_u, span)?.clone();
                let r = konst(proto, d.c, span)?;
                let v = operate(op_of(d.op), l, r, span)?;
                self.set(base, d.a, v, span)?;
            }
            Op::POW | Op::POWK => {
                let l = self.get(base, d.b_u, span)?.clone();
                let r = if d.op == Op::POW {
                    self.get(base, d.c, span)?.clone()
                } else {
                    konst(proto, d.c, span)?
                };
                let pow = program
                    .pow
                    .ok_or_else(|| broken("no math.pow in the catalogue", span))?;
                let v = self.interp.call_builtin(pow, &[l, r], span)?;
                self.set(base, d.a, v, span)?;
            }
            Op::NEG => {
                let v = negate(self.get(base, d.b_u, span)?.clone(), span)?;
                self.set(base, d.a, v, span)?;
            }
            Op::NOT => {
                let v = Value::Bool(!self.get(base, d.b_u, span)?.truthy());
                self.set(base, d.a, v, span)?;
            }

            // ----- control ----------------------------------------------------
            Op::EQJ | Op::NEJ | Op::LTJ | Op::LEJ => {
                let l = self.get(base, d.a, span)?.clone();
                let r = self.get(base, d.b_u, span)?.clone();
                let op = match d.op {
                    Op::EQJ => BinOp::Eq,
                    Op::NEJ => BinOp::Ne,
                    Op::LTJ => BinOp::Lt,
                    _ => BinOp::Le,
                };
                if operate(op, l, r, span)?.truthy() {
                    return Ok(Flow::Next(jump(pc, d.c_s, span)?));
                }
            }
            Op::JMP | Op::LOOP => return Ok(Flow::Next(jump(pc, d.b_s, span)?)),
            Op::TEST | Op::TESTN => {
                let truthy = self.get(base, d.a, span)?.truthy();
                if truthy == (d.op == Op::TEST) {
                    return Ok(Flow::Next(jump(pc, d.b_s, span)?));
                }
            }
            Op::TESTSET => {
                let v = self.get(base, d.b_u, span)?.clone();
                if v.truthy() {
                    self.set(base, d.a, v, span)?;
                } else {
                    return Ok(Flow::Next(jump(pc, d.c_s, span)?));
                }
            }

            // ----- the step budget ------------------------------------------------
            Op::STEPS => {
                let nest = d.a;
                if let Some(f) = self.frames.last_mut() {
                    let depth = self.interp.loop_depth.saturating_sub(f.nest);
                    self.interp.loop_depth = depth.saturating_add(nest);
                    f.nest = nest;
                }
                for _ in 0..d.b_u {
                    self.interp.tick(span)?;
                }
            }

            // ----- calls ------------------------------------------------------------
            Op::CALL => {
                let callee = self.get(base, d.a, span)?.clone();
                let args = self.window(base, d.a + 1, d.b_u, span)?;
                let ret = Ret {
                    reg: base.saturating_add(d.a as usize),
                    extra: d.c as usize,
                };
                if let Some((f, cf)) = compiled(&callee, program) {
                    if let Some(caller) = self.frames.last_mut() {
                        caller.pc = next;
                    }
                    self.push(program, f, cf.index, args, ret, span)?;
                    return Ok(Flow::Switched);
                }
                let v = self.interp.call(&callee, args, span)?;
                self.write_result(ret, v, span)?;
            }
            Op::CALLB => {
                let b = crate::stdlib::builtin_catalogue()
                    .get(d.b_u as usize)
                    .copied()
                    .ok_or_else(|| broken("a builtin index past the catalogue", span))?;
                let args = self.window(base, d.a + 1, d.c, span)?;
                let v = self.interp.call_builtin(b, &args, span)?;
                self.set(base, d.a, v, span)?;
            }
            Op::RET => {
                let v = self.get(base, d.a, span)?.clone();
                return Ok(Flow::Return(v));
            }
            Op::TAILCALL => {
                let callee = self.get(base, d.a, span)?.clone();
                let args = self.window(base, d.a + 1, d.b_u, span)?;
                if let Some((f, cf)) = compiled(&callee, program) {
                    self.replace(program, f, cf.index, args, span)?;
                    return Ok(Flow::Switched);
                }
                let v = self.interp.call(&callee, args, span)?;
                return Ok(Flow::Return(v));
            }
            Op::CLOSURE => {
                let child = proto
                    .children
                    .get(d.b_u as usize)
                    .copied()
                    .ok_or_else(|| broken("a child function past the table", span))?;
                let target = program
                    .funcs
                    .get(child)
                    .ok_or_else(|| broken("no such function", span))?;
                if target.upvalues > 0 {
                    return Err(internal(format!(
                        "the VM cannot run `{}` of a function that captures values yet: the package that builds it has not landed (cig-compile#31)",
                        Op::CLOSURE
                    ))
                    .at(span));
                }
                let name = (!target.lambda && !target.name.is_empty()).then(|| target.name.clone());
                let f = Function {
                    name,
                    params: vec![String::new(); target.arity],
                    body: FnBody::Compiled(CompiledFn {
                        program: Rc::clone(program),
                        index: child,
                    }),
                    closure: self.interp.globals.clone(),
                };
                self.set(base, d.a, Value::Func(Rc::new(f)), span)?;
            }

            Op::WIDE => return Err(broken("a bare `wide`", span)),

            op => return Err(not_built(op, span)),
        }
        Ok(Flow::Next(next))
    }

    /// A call of a compiled function: arity, depth, then a new frame.
    fn push(
        &mut self,
        program: &Program,
        f: &Function,
        index: usize,
        args: Vec<Value>,
        ret: Ret,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let target = program
            .funcs
            .get(index)
            .ok_or_else(|| broken("no such function", span))?;
        if args.len() != target.arity {
            return Err(arity_mismatch(f, args.len(), span));
        }
        self.interp.enter_call(span)?;
        let base = self.stack.len();
        if let Err(e) = self.grow(target.reg_count, span) {
            self.interp.leave_call();
            return Err(e);
        }
        for (slot, a) in self.stack.iter_mut().skip(base).zip(args) {
            *slot = a;
        }
        self.frames.push(Frame {
            func: index,
            pc: 0,
            base,
            ret: Some(ret),
            held_calls: 1,
            nest: 0,
            held_nest: 0,
        });
        Ok(())
    }

    /// A tail call of a compiled function: the frame is reused, and keeps
    /// holding the depth and loop nesting of the frame it replaces, so the
    /// interpreter's counts are the ones a plain call would leave.
    fn replace(
        &mut self,
        program: &Program,
        f: &Function,
        index: usize,
        args: Vec<Value>,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let target = program
            .funcs
            .get(index)
            .ok_or_else(|| broken("no such function", span))?;
        if args.len() != target.arity {
            return Err(arity_mismatch(f, args.len(), span));
        }
        self.interp.enter_call(span)?;
        let base = match self.frames.last_mut() {
            Some(frame) => {
                frame.held_calls += 1;
                frame.held_nest = frame.held_nest.saturating_add(frame.nest);
                frame.nest = 0;
                frame.func = index;
                frame.pc = 0;
                frame.base
            }
            None => return Err(broken("an empty call stack", span)),
        };
        self.stack.truncate(base);
        self.grow(target.reg_count, span)?;
        for (slot, a) in self.stack.iter_mut().skip(base).zip(args) {
            *slot = a;
        }
        Ok(())
    }

    /// Leave the current frame with `v`. `Some(v)` when it was the entry
    /// frame: the run is over.
    fn ret(&mut self, v: Value, span: Span) -> Result<Option<Value>, Diagnostic> {
        let frame = self
            .frames
            .pop()
            .ok_or_else(|| broken("an empty call stack", span))?;
        self.release(&frame);
        self.stack.truncate(frame.base);
        match frame.ret {
            None => Ok(Some(v)),
            Some(ret) => {
                self.write_result(ret, v, span)?;
                Ok(None)
            }
        }
    }

    /// A call's result: `R(A)` is the value; the rest of the row's result
    /// window, `R(A+1..A+C+1)`, is null (a CigScript call has one result).
    fn write_result(&mut self, ret: Ret, v: Value, span: Span) -> Result<(), Diagnostic> {
        let end = ret.reg.saturating_add(ret.extra).saturating_add(1);
        let window = self
            .stack
            .get_mut(ret.reg..end)
            .ok_or_else(|| broken("a result register past its frame", span))?;
        let mut v = Some(v);
        for slot in window {
            *slot = v.take().unwrap_or(Value::Null);
        }
        Ok(())
    }
}

/// The callee as a compiled function of this program, when it is one.
fn compiled<'v>(
    callee: &'v Value,
    program: &Rc<Program>,
) -> Option<(&'v Function, &'v CompiledFn)> {
    match callee {
        Value::Func(f) => match &f.body {
            FnBody::Compiled(cf) if Rc::ptr_eq(&cf.program, program) => Some((f, cf)),
            _ => None,
        },
        _ => None,
    }
}

fn konst(proto: &Proto, i: u32, span: Span) -> Result<Value, Diagnostic> {
    proto
        .consts
        .get(i as usize)
        .cloned()
        .ok_or_else(|| broken("a constant past the pool", span))
}

/// The jump rule of `docs/BYTECODE.md`: `target = pc + 1 + offset`, `pc`
/// the first-word pc.
fn jump(pc: usize, offset: i32, span: Span) -> Result<usize, Diagnostic> {
    let target = (pc as i64) + 1 + i64::from(offset);
    usize::try_from(target).map_err(|_| broken("a jump before the first instruction", span))
}

fn op_of(op: Op) -> BinOp {
    match op {
        Op::ADD | Op::ADDK => BinOp::Add,
        Op::SUB | Op::SUBK => BinOp::Sub,
        Op::MUL | Op::MULK => BinOp::Mul,
        Op::DIV | Op::DIVK => BinOp::Div,
        Op::MOD | Op::MODK => BinOp::Rem,
        Op::EQ | Op::EQK => BinOp::Eq,
        Op::LT | Op::LTK => BinOp::Lt,
        _ => BinOp::Le,
    }
}

/// A binary operator exactly as the tree-walker evaluates one: the fast
/// path for two ints or two floats, then the general [`binary`].
#[inline]
fn operate(op: BinOp, l: Value, r: Value, span: Span) -> Result<Value, Diagnostic> {
    match fast_binary(op, &l, &r) {
        Some(v) => Ok(v),
        None => binary(op, l, r, span),
    }
}
