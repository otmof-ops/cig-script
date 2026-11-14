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
//! - the call depth is [`Interp::enter_call`], its limit and its E506;
//! - `exhale` builds its line with [`exhale_part`] and writes it with
//!   [`Interp::write_exhaled`]; field, index and method access are the
//!   interpreter's [`Interp::member`], [`Interp::index`],
//!   [`Interp::call_method`], [`set_field`] and [`set_index`]; a loop over
//!   data iterates what [`Interp::iterate`] gives.
//!
//! A compiled function is a [`Value::Func`] like any other: the VM calls an
//! interpreted one through the interpreter, and the interpreter (a builtin's
//! callback, the tree-walker) calls a compiled one through [`call_compiled`].
//!
//! A [`Program`] holds each function decoded once into a per-pc table
//! (the table the verifier's rules read, through the crate's one decoder),
//! with its constants made into values and its line rows made into
//! positions. [`load`] does that for every function of a parsed module;
//! [`open`], a compiled program's start, does it for the top level and
//! leaves every other function to its first entry (`Program::ready`), so
//! that no function runs before the verifier accepted it and no function
//! a run does not enter is read. Loading also checks what the verifier
//! cannot know: every `callb` index against the builtin catalogue, every
//! `loadg` name against the prelude and every `burn.op` kind against the
//! burn-op catalogue, each refused with E1050.
//!
//! Captured variables live in cells. A register can carry a cell (after
//! `cellnew`): every read of that register reads the cell and every write
//! writes it, so the function and the closures that captured the register
//! share one variable. Every upvalue is a cell: capturing a register that
//! carries one shares it, capturing a plain register gives the closure a
//! cell of its own holding a copy, and capturing an upvalue shares that.
//!
//! An error raised anywhere (an instruction, a builtin, a callee, a
//! callback) is offered to the tries open in the frame that raised it, then
//! in each caller at the pc of its call, innermost first, by the
//! tree-walker's rule: a catch takes any error unless `exit()` was asked
//! for, a finally runs on every way out, and a finally's own error, `break`,
//! `continue` or `snuff` replaces what was pending. The error keeps the
//! position it was raised at, as the tree-walker's does: there is no other
//! frame list to print. `throw` is the interpreter's `cough`, and `loadg`
//! reads a builtin, a module or `args` from the interpreter's own prelude.
//!
//! A burn opens and closes through the interpreter's one burn entry
//! ([`Interp::burn_open`], [`Interp::compensate_open`],
//! [`Interp::compensate_close`], [`Interp::burn_close`],
//! [`Interp::record_compensation`]), the functions the tree-walker's
//! `burn { }` calls; a `burn.op` calls its builtin through
//! [`Interp::call_builtin`], so the kernel gets the op the builtin builds,
//! through [`Interp::effect`], as in the tree-walker. Each frame keeps the
//! burns it opened, innermost last; an error leaving a burn closes it (as
//! the tree-walker's block does on its way out) before a try outside it
//! sees the error. `pack` and `chain.make` check and build through the
//! tree-walker's [`pack_root`], [`Interp::declare_pack`], [`chain_step`]
//! and [`make_chain`].

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
use crate::interp::ops::{binary, fast_binary, negate, range_fits};
use crate::interp::{
    arity_mismatch, burn_label, chain_step, error_value, exhale_part, make_chain, map_key,
    outside_burn, pack_root, set_field, set_index, Interp,
};
use crate::syntax::ast::BinOp;
use crate::syntax::span::Span;
use crate::value::{Builtin, FnBody, Function, Value};
use cig_bytecode::{
    decode_table, verify_function, verify_upvalues, Constant, DecodedInstr, FuncKind,
    Function as BcFunction, HandlerEntry, LineEntry, Op, ParseError, Reader, RegionLabel, SpanBase,
    UpvalDesc, UpvalKind, Verified, VerifyError, RESERVED_U32,
};
use indexmap::IndexMap;
use std::cell::{OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

#[cfg(test)]
mod tests;

/// `CIG_VM_STATS=1`: instructions executed and time by opcode, written to
/// the run's error stream when a script ends. Built only with the
/// `bench-stats` feature, which a release never has: without it the VM does
/// not read the variable at all.
#[cfg(feature = "bench-stats")]
mod stats {
    use super::{Interp, Op};
    use std::cell::RefCell;
    use std::time::Instant;

    struct Table {
        count: Vec<u64>,
        nanos: Vec<u128>,
    }

    thread_local! {
        static TABLE: RefCell<Option<Table>> = const { RefCell::new(None) };
    }

    fn on() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| std::env::var("CIG_VM_STATS").is_ok_and(|v| v == "1"))
    }

    /// The start of one instruction, when the counter is on.
    #[inline(always)]
    pub(super) fn start(op: Op) -> Option<(u8, Instant)> {
        on().then(|| (op.as_byte(), Instant::now()))
    }

    /// The end of the instruction `start` began.
    #[inline(always)]
    pub(super) fn stop(timer: Option<(u8, Instant)>) {
        let Some((op, t0)) = timer else { return };
        let ns = t0.elapsed().as_nanos();
        TABLE.with(|t| {
            let mut t = t.borrow_mut();
            let t = t.get_or_insert_with(|| Table {
                count: vec![0; 256],
                nanos: vec![0; 256],
            });
            if let (Some(c), Some(n)) = (t.count.get_mut(op as usize), t.nanos.get_mut(op as usize))
            {
                *c += 1;
                *n += ns;
            }
        });
    }

    /// Write the table to the run's error stream and start a new one.
    pub(super) fn report(interp: &mut Interp) {
        use std::io::Write as _;
        let Some(t) = TABLE.with(|t| t.borrow_mut().take()) else {
            return;
        };
        let total: u64 = t.count.iter().sum();
        let mut rows: Vec<(u8, u64, u128)> = (0..=255u8)
            .filter_map(|b| {
                let c = t.count.get(b as usize).copied().unwrap_or(0);
                (c > 0).then(|| (b, c, t.nanos.get(b as usize).copied().unwrap_or(0)))
            })
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.2));
        let mut out = format!("vm stats: {total} instructions\n");
        out.push_str("  opcode       executed        share   total ms   ns/op\n");
        for (b, c, ns) in rows {
            let name = Op::from_byte(b).map_or("?", Op::mnemonic);
            out.push_str(&format!(
                "  {name:<10} {c:>12} {:>11.1}% {:>10.2} {:>7.1}\n",
                c as f64 * 100.0 / total.max(1) as f64,
                ns as f64 / 1e6,
                ns as f64 / c as f64
            ));
        }
        let _ = interp.err.write_all(out.as_bytes());
    }
}

/// The burn-op kinds a `burn.op` may name, by index. The VM refuses a
/// module whose `burn.op` names a kind past the end, or a callee that is
/// not a builtin of that kind ([`burn_op_kind`]), with E1050. Kinds are
/// only ever appended.
pub const BURN_OP_KINDS: [&str; 11] = [
    "write",
    "append",
    "delete",
    "move",
    "copy",
    "mkdir",
    "proc",
    "env_set",
    "env_unset",
    "layer_apply",
    "layer_discard",
];

/// The kind (an index into [`BURN_OP_KINDS`]) of the builtin `callee`
/// when it changes the world, the kind a `burn.op` calling it carries.
/// Every builtin whose effect needs a burn has one.
pub fn burn_op_kind(callee: &str) -> Option<usize> {
    let kind = match callee {
        "fs.write_text" | "json.save" | "csv.write" => "write",
        "fs.append_text" => "append",
        "fs.rm" => "delete",
        "fs.mv" => "move",
        "fs.cp" => "copy",
        "fs.mkdir" => "mkdir",
        "proc.run" | "proc.text" | "proc.lines" | "proc.json" | "proc.csv" | "proc.kv"
        | "proc.pipe" | "proc.shell" => "proc",
        "env.set" => "env_set",
        "env.unset" => "env_unset",
        "proc.apply_layer" => "layer_apply",
        "proc.discard_layer" => "layer_discard",
        _ => return None,
    };
    BURN_OP_KINDS.iter().position(|k| *k == kind)
}

/// The instructions this VM does not run yet. Empty: every instruction of
/// format version 1 runs. An opcode a later format adds stops with an
/// internal error that names it until the VM runs it, and goes here.
pub const NOT_BUILT: [Op; 0] = [];

const RECOMPILE_HINT: &str =
    "compile it again (cigcompile build), or fetch it again from where it came from";

/// A loaded module. Each function is verified and decoded once, the first
/// time a run enters it (`Program::ready`, from the four places a
/// function is entered), or every one at once by [`load`].
pub struct Program {
    /// The module's functions in the verifier's pre-order: 0 is the top
    /// level, so `fn<i>` in an error names `funcs[i]`. A slot is filled
    /// when its function has been verified and loaded.
    funcs: Funcs,
    /// Every function's name, kind, arity and upvalues: what `closure`
    /// reads of the function it makes, which it does not enter.
    heads: Vec<Head>,
    /// The module a function is read from at its first entry; `None` when
    /// [`load`] filled every slot.
    reader: Option<Reader>,
    /// What a function's operands may name beyond what the verifier
    /// checks, looked up by name.
    known: Known,
    /// The builtin behind `pow`/`powk`: `math.pow`, the language's only
    /// power operation.
    pow: Option<&'static Builtin>,
}

/// The loaded functions, by pre-order index.
struct Funcs(Vec<OnceCell<Proto>>);

impl Funcs {
    /// Function `i`, if it has been verified and loaded. A frame only ever
    /// runs a function `Program::ready` loaded.
    fn get(&self, i: usize) -> Option<&Proto> {
        self.0.get(i).and_then(OnceCell::get)
    }
}

/// What `closure` reads of a function: checked for every function when the
/// module is opened (its upvalues against its parent's counts, the one
/// verifier rule that looks across functions).
struct Head {
    /// The name a closure of it carries (`<pull name>`): `None` for a
    /// lambda or a function without a name. Made once, shared by every
    /// closure.
    label: Option<Rc<str>>,
    arity: usize,
    /// What each upvalue captures from the function that makes the closure.
    upvalues: Vec<UpvalDesc>,
}

/// One function of a loaded module.
struct Proto {
    /// No register of a frame of it ever carries a cell: it has no
    /// `cellnew`, the only instruction that gives a register one. Its
    /// registers are read and written without looking at `cells`.
    plain: bool,
    /// Per pc, the field cache of a `getfield` or `setfield` there: the
    /// position its key last had in the map, as the tree-walker keeps one
    /// per field expression. At a `callm`, the method cache: the
    /// receiver's type and the builtin method it resolved to
    /// ([`crate::stdlib::method_slot`]; 0 before the first call).
    hints: Vec<std::cell::Cell<u32>>,
    reg_count: usize,
    arity: usize,
    /// The decoded instruction at each first-word pc; `None` at the second
    /// word of a `WIDE` pair, which nothing may jump to.
    code: Vec<Option<DecodedInstr>>,
    /// The source position of each pc, from the line table.
    spans: Spans,
    consts: Vec<Value>,
    /// Indices into [`Program::funcs`] of this function's children.
    children: Vec<usize>,
    /// The function's try/catch/finally regions, as the module has them.
    handlers: Vec<HandlerEntry>,
    /// A bare `burn.op` is allowed anywhere in it (the caller's burn).
    requires_burn: bool,
    /// The burn regions: unlit or not, and the label.
    regions: Vec<Region>,
    /// For each constant a `burn.op` names as its callee, the builtin.
    effects: Vec<Option<&'static Builtin>>,
}

/// A function's source positions: one span per line row (the function's
/// first token before them), and for each pc the row it falls under. A
/// span is built once per row, not once per instruction word.
struct Spans {
    rows: Vec<Span>,
    at: Vec<u32>,
}

impl Spans {
    /// The source position of every pc: its own line row, else the
    /// nearest row before it, else the function's first token. The rows
    /// are strictly ascending (verified).
    fn new(base: &SpanBase, lines: &[LineEntry], n: usize) -> Spans {
        let mut rows = Vec::with_capacity(lines.len() + 1);
        rows.push(Span::new(
            base.byte_offset as usize,
            base.byte_offset as usize,
            base.line,
            base.col,
        ));
        rows.extend(lines.iter().map(|row| row_span(base, row)));
        let mut at = Vec::with_capacity(n);
        let mut next = lines.iter().map(|row| row.pc as usize).peekable();
        let mut current = 0u32;
        for pc in 0..n {
            while next.next_if(|&p| p <= pc).is_some() {
                current += 1;
            }
            at.push(current);
        }
        Spans { rows, at }
    }

    /// The position of `pc`; `None` past the function.
    fn get(&self, pc: usize) -> Option<&Span> {
        self.at.get(pc).and_then(|&r| self.rows.get(r as usize))
    }

    /// The position of the function's first instruction.
    fn first(&self) -> Option<&Span> {
        self.get(0)
    }
}

/// One burn region of a function.
struct Region {
    unlit: bool,
    label: Label,
}

/// Where a burn region's label comes from.
enum Label {
    None,
    /// A string constant, read at load.
    Text(String),
    /// A register, read when `burn.enter` runs.
    Reg(u32),
}

/// An error on its way through the VM: boxed, so every handler's result
/// stays a word or two wide and the dispatch loop moves no 150-byte
/// [`Diagnostic`] through each instruction's return. It is unboxed where
/// it leaves the VM.
type Fault = Box<Diagnostic>;

/// A variable a closure shares with the function that made it.
type Cell = Rc<RefCell<Value>>;

/// A function value of a loaded module: what `closure` makes and `call`
/// calls. It lives in a [`Value::Func`] as [`FnBody::Compiled`].
#[derive(Clone)]
pub struct CompiledFn {
    program: Rc<Program>,
    index: usize,
    /// The function's parameter count: its arity, which the function
    /// value carries no parameter names for.
    pub(crate) arity: usize,
    /// The cells the closure captured, one per upvalue.
    upvals: Rc<[Cell]>,
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

/// The internal error of a compiled function that reached the
/// tree-walker's own frame instead of [`call_compiled`].
pub(crate) fn misrouted(span: Span) -> Diagnostic {
    internal("a compiled function reached the tree-walker's frame instead of the VM").at(span)
}

/// The internal error of an instruction whose operands do not hold what
/// it works on (a lowering wrote it wrong): `appendlist` on a value that
/// is not a list, a loop window that does not hold its loop's state.
fn misuse(op: Op, what: &str, span: Span) -> Diagnostic {
    internal(format!("`{op}` {what}")).at(span)
}

/// An invariant the verifier guarantees did not hold. A verified module
/// never gets here; the VM stops with an internal error instead of
/// panicking if one ever does.
#[cold]
fn broken(what: &str, span: Span) -> Diagnostic {
    internal(format!("the VM found {what} in a verified module")).at(span)
}

/// E1050 for a module whose bytes do not read, worded as a start words it.
fn unreadable(e: &ParseError) -> Diagnostic {
    usage(format!("the program's bytecode does not read: {e}"))
        .code("E1050")
        .with_hint(RECOMPILE_HINT)
}

/// Load a verified module: decode every function once and check what the
/// verifier leaves to the VM (E1050 for an unknown builtin or burn-op
/// kind). Every function is loaded here, so the run enters none for the
/// first time.
pub fn load(v: Verified<'_>) -> Result<Rc<Program>, Diagnostic> {
    let known = Known::new();
    let mut funcs = Vec::new();
    let mut heads = Vec::new();
    flatten(&v.module().main, &mut funcs, &mut heads, &known)?;
    Ok(Rc::new(Program {
        funcs: Funcs(funcs.into_iter().map(OnceCell::from).collect()),
        heads,
        reader: None,
        pow: known.pow(),
        known,
    }))
}

/// Open a module's bytes for a run (a stage `b` program's start). What is
/// checked here, before the first instruction: everything [`Reader::open`]
/// checks (the header, the BLAKE3 over every byte after it, the head, the
/// string pool, the directory's offsets and lengths, the tree and its
/// depth); every function's name, kind, flags and upvalue descriptors, and
/// the upvalues against the parent's registers and upvalues
/// ([`verify_upvalues`], the one rule that looks across functions); and the
/// top level, read, verified and loaded. Every other function is read,
/// verified and loaded by `Program::ready` the first time it is entered,
/// before its first instruction runs.
pub fn open(bytes: Vec<u8>) -> Result<Rc<Program>, Diagnostic> {
    let reader = Reader::open(bytes).map_err(|e| unreadable(&e))?;
    let n = reader.len();
    let mut heads = Vec::with_capacity(n);
    for i in 0..n as u32 {
        let h = reader.head(i).map_err(|e| unreadable(&e))?;
        let place = reader
            .place(i)
            .ok_or_else(|| broken("a function outside its directory", Span::default()))?;
        verify_upvalues(i, &h.upvalues, place.parent).map_err(|e| refusal(&e))?;
        heads.push(Head {
            label: label(&h.name, h.kind),
            arity: h.arity as usize,
            upvalues: h.upvalues,
        });
    }
    let known = Known::new();
    let program = Rc::new(Program {
        funcs: Funcs((0..n).map(|_| OnceCell::new()).collect()),
        heads,
        reader: Some(reader),
        pow: known.pow(),
        known,
    });
    program.ready(0, Span::default())?;
    Ok(program)
}

impl Program {
    /// Make function `index` ready to run: nothing when it is loaded; at
    /// its first entry, read its body, verify it ([`verify_function`]) and
    /// load it, before its first instruction runs. A function that does
    /// not read or does not verify is E1050, an error of the run raised at
    /// `span` (the call), and stays unloaded: none of its instructions
    /// ever run.
    #[inline]
    fn ready(&self, index: usize, span: Span) -> Result<(), Diagnostic> {
        match self.funcs.0.get(index) {
            Some(slot) if slot.get().is_some() => Ok(()),
            Some(slot) => {
                let proto = self.first_entry(index).map_err(|d| {
                    if span == Span::default() {
                        d
                    } else {
                        d.at(span)
                    }
                })?;
                // `first_entry` enters no function, so the slot is still
                // empty here.
                let _ = slot.set(proto);
                Ok(())
            }
            None => Err(broken("no such function", span)),
        }
    }

    #[cold]
    #[inline(never)]
    fn first_entry(&self, index: usize) -> Result<Proto, Diagnostic> {
        let Some(reader) = &self.reader else {
            return Err(broken("a function that was not loaded", Span::default()));
        };
        let i = index as u32;
        let f = reader.function(i).map_err(|e| unreadable(&e))?;
        let place = reader
            .place(i)
            .ok_or_else(|| broken("a function outside its directory", Span::default()))?;
        let v = verify_function(&f, place).map_err(|e| refusal(&e))?;
        let children = reader.children(i).iter().map(|&c| c as usize).collect();
        let f = v.function();
        build(f, index, v.into_code(), children, &self.known)
    }
}

/// The names `loadg` may load: the interpreter's prelude (its global
/// builtins and its modules) and `args`.
pub fn prelude_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = crate::stdlib::prelude()
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    names.push("args");
    names
}

/// What a module's operands may name beyond what the verifier checks:
/// built once per program, each name found by a lookup.
struct Known {
    builtins: usize,
    prelude: HashSet<&'static str>,
    /// The builtin catalogue by name (the first of a name, as a search
    /// from the start finds it).
    by_name: HashMap<&'static str, &'static Builtin>,
}

impl Known {
    fn new() -> Known {
        let catalogue = crate::stdlib::builtin_catalogue();
        let mut by_name = HashMap::with_capacity(catalogue.len());
        for b in catalogue.iter().copied() {
            by_name.entry(b.name).or_insert(b);
        }
        Known {
            builtins: catalogue.len(),
            prelude: prelude_names().into_iter().collect(),
            by_name,
        }
    }

    fn pow(&self) -> Option<&'static Builtin> {
        self.by_name.get("math.pow").copied()
    }
}

/// The name a closure of a function carries: none for a lambda or an
/// empty name.
fn label(name: &str, kind: FuncKind) -> Option<Rc<str>> {
    (kind != FuncKind::Lambda && !name.is_empty()).then(|| Rc::from(name))
}

/// Append `f` and then its children, recursively, in the verifier's
/// pre-order. The verifier bounds the nesting at 64 levels.
fn flatten(
    f: &BcFunction,
    out: &mut Vec<Proto>,
    heads: &mut Vec<Head>,
    known: &Known,
) -> Result<usize, Diagnostic> {
    let index = out.len();
    let code = match decode_table(&f.instrs) {
        Ok((_, code)) => code,
        Err(e) => {
            return Err(internal(format!(
                "the VM could not decode a verified function (fn{index}): {e:?}"
            )))
        }
    };
    heads.push(Head {
        label: label(&f.name, f.kind),
        arity: f.arity as usize,
        upvalues: f.upvalues.clone(),
    });
    let proto = build(f, index, code, Vec::new(), known)?;
    out.push(proto);
    let mut children = Vec::with_capacity(f.children.len());
    for c in &f.children {
        children.push(flatten(c, out, heads, known)?);
    }
    if let Some(p) = out.get_mut(index) {
        p.children = children;
    }
    Ok(index)
}

/// Load one verified function from its decoded table: check what the
/// verifier leaves to the VM (every `callb` index against the builtin
/// catalogue, every `loadg` name against the prelude, every `burn.op`
/// kind against the burn-op catalogue, each refused with E1050) and build
/// its constants, regions and positions.
fn build(
    f: &BcFunction,
    index: usize,
    code: Vec<Option<DecodedInstr>>,
    children: Vec<usize>,
    known: &Known,
) -> Result<Proto, Diagnostic> {
    let mut effects: Vec<Option<&'static Builtin>> = vec![None; f.constants.len()];
    let mut regions = Vec::with_capacity(f.regions.len());
    for r in &f.regions {
        let label = match r.label {
            RegionLabel::None => Label::None,
            RegionLabel::Const(k) => match f.constants.get(k as usize) {
                Some(Constant::Str(s)) => Label::Text(s.clone()),
                _ => return Err(refused("region-label", index, 0)),
            },
            RegionLabel::Reg(r) => Label::Reg(r),
        };
        regions.push(Region {
            unlit: r.class == 1,
            label,
        });
    }
    let mut plain = true;
    for d in code.iter().flatten() {
        match d.op {
            Op::CALLB if d.b_u as usize >= known.builtins => {
                return Err(refused("unknown-builtin", index, d.pc));
            }
            Op::LOADG => {
                let named = match f.constants.get(d.b_u as usize) {
                    Some(Constant::Str(s)) => known.prelude.contains(s.as_str()),
                    _ => false,
                };
                if !named {
                    return Err(refused("unknown-prelude-name", index, d.pc));
                }
            }
            Op::BURN_OP => {
                let kind = if d.wide { d.c & 0xFF } else { d.c & 0x0F };
                let callee = match f.constants.get(d.b_u as usize) {
                    Some(Constant::Str(s)) => known
                        .by_name
                        .get(s.as_str())
                        .copied()
                        .filter(|b| burn_op_kind(b.name) == Some(kind as usize)),
                    _ => None,
                };
                let Some(callee) = callee else {
                    return Err(refused("unknown-burn-op", index, d.pc));
                };
                if let Some(slot) = effects.get_mut(d.b_u as usize) {
                    *slot = Some(callee);
                }
            }
            Op::CELLNEW => plain = false,
            _ => {}
        }
    }
    Ok(Proto {
        plain,
        hints: (0..f.instrs.len())
            .map(|_| std::cell::Cell::new(0))
            .collect(),
        reg_count: f.reg_count as usize,
        arity: f.arity as usize,
        spans: Spans::new(&f.span_base, &f.lines, f.instrs.len()),
        code,
        consts: f.constants.iter().map(constant).collect(),
        children,
        handlers: f.handlers.clone(),
        requires_burn: f.requires_burn,
        regions,
        effects,
    })
}

fn constant(c: &Constant) -> Value {
    match c {
        Constant::Bool(b) => Value::Bool(*b),
        Constant::Int(i) => Value::Int(*i),
        Constant::Float(f) => Value::Float(*f),
        Constant::Str(s) => Value::str(s),
    }
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

/// Run a loaded module as a script, as [`Interp::run_program`] runs a
/// syntax tree: the run's output flushed, and an `exit()` a normal end.
///
/// The top level's names: a `chain` statement of the top level is declared
/// in the interpreter's globals when it runs (`chain.make` with C = 1), and
/// the top level ends by returning a map of every name it declares, which
/// is declared there too (names already there keep their value). After
/// the run the globals hold what the tree-walker's hold, so `--light`
/// answers the same at either stage. A top level that returns anything
/// else declares nothing.
pub fn run_script(interp: &mut Interp, program: &Rc<Program>) -> Result<(), Diagnostic> {
    use std::io::Write as _;
    let result = run(interp, program);
    #[cfg(feature = "bench-stats")]
    stats::report(interp);
    interp.out.flush().ok();
    match result {
        Ok(Value::Map(names)) => {
            for (name, value) in names.borrow().iter() {
                interp.globals.declare(name, value.clone(), false);
            }
            Ok(())
        }
        Ok(_) => Ok(()),
        Err(_) if interp.exit_requested.is_some() => Ok(()),
        Err(d) => Err(d),
    }
}

/// Run one function of a loaded module with `args` as its parameters.
pub(crate) fn run_function(
    interp: &mut Interp,
    program: &Rc<Program>,
    index: usize,
    args: Vec<Value>,
) -> Result<Value, Diagnostic> {
    run_frame(interp, program, index, Rc::from(Vec::new()), args)
}

/// Call a compiled function from outside the VM: a builtin's callback
/// (`xs.map(f)`, a sort key), the tree-walker, or another program's VM.
/// The arity (E401) and the depth (E506) are checked as for any call, and
/// the function runs in a VM of its own until it returns.
pub(crate) fn call_compiled(
    interp: &mut Interp,
    f: &Function,
    cf: &CompiledFn,
    args: Vec<Value>,
    span: Span,
) -> Result<Value, Diagnostic> {
    cf.program.ready(cf.index, span)?;
    let proto = cf
        .program
        .funcs
        .get(cf.index)
        .ok_or_else(|| broken("no such function", span))?;
    if args.len() != proto.arity {
        return Err(arity_mismatch(f, args.len(), span));
    }
    interp.enter_call(span)?;
    let result = run_frame(interp, &cf.program, cf.index, Rc::clone(&cf.upvals), args);
    interp.leave_call();
    result
}

fn run_frame(
    interp: &mut Interp,
    program: &Rc<Program>,
    index: usize,
    upvals: Rc<[Cell]>,
    args: Vec<Value>,
) -> Result<Value, Diagnostic> {
    program.ready(index, Span::default())?;
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
    let regs = interp.vm_regs.take().unwrap_or_default();
    let mut m = Machine {
        interp,
        stack: regs.stack,
        cells: regs.cells,
        frames: regs.frames,
        tries: regs.tries,
        burns: regs.burns,
        upvals: regs.upvals,
    };
    let first = proto.spans.first().copied().unwrap_or_default();
    let result = match m.grow_to(proto.reg_count, first) {
        Ok(()) => {
            for (slot, a) in m.stack.iter_mut().zip(args) {
                *slot = a;
            }
            let up = if upvals.is_empty() {
                NO_UPVALS
            } else {
                m.upvals.push(upvals);
                0
            };
            let mut entry = Frame::new(index, 0, 0, up, 0, 0);
            entry.held_calls = 0;
            m.frames.push(entry);
            let result = m.exec(program);
            // Whatever stopped the run, the interpreter's depth and loop
            // counters go back to what they were, as the tree-walker's do
            // on its way out.
            while let Some(f) = m.frames.pop() {
                m.release(&f);
            }
            result
        }
        Err(e) => Err(e),
    };
    m.give_back();
    result.map_err(|e| *e)
}

/// `Frame::up` of a frame whose closure captured nothing.
const NO_UPVALS: u32 = u32::MAX;

/// One call's record, 64 bytes, owning nothing: ending a frame drops
/// nothing. The tries and burns a frame opens are on the machine's two
/// stacks from `tries` and `burns` up, and its closure's upvalues, when it
/// has any, are `Machine::upvals[up]`. Its result goes to the caller's
/// `R(A)`, the register just below `base` (the frame starts at the
/// caller's `R(A+1)`); the machine's first frame returns out of the VM
/// instead.
#[derive(Clone, Copy)]
struct Frame {
    func: usize,
    pc: usize,
    /// The pc of the call this frame is waiting on (`pc` is where it goes
    /// on after it): where an error from the callee is offered to this
    /// frame's tries.
    site: usize,
    /// The frame's `R(0)` in the register stack.
    base: usize,
    /// How many registers after `R(A)` the call's result row covers (`C`).
    extra: u32,
    /// Calls this frame holds on the interpreter's depth count: its own
    /// call, and each tail call that replaced a frame in it.
    held_calls: u32,
    /// The frame's loop nesting now, as its last `steps` set it.
    nest: u32,
    /// Loop nesting held for frames a tail call replaced.
    held_nest: u32,
    /// Its closure's upvalues in `Machine::upvals`, or [`NO_UPVALS`].
    up: u32,
    /// Where its open tries begin in `Machine::tries`.
    tries: u32,
    /// Where its open burns begin in `Machine::burns`.
    burns: u32,
}

impl Frame {
    /// A frame of function `func` at `base`, with nothing open yet.
    fn new(func: usize, base: usize, extra: u32, up: u32, tries: usize, burns: usize) -> Frame {
        Frame {
            func,
            pc: 0,
            site: 0,
            base,
            extra,
            held_calls: 1,
            nest: 0,
            held_nest: 0,
            up,
            tries: tries as u32,
            burns: burns as u32,
        }
    }
}

/// A burn the frame has entered and not yet left.
struct OpenBurn {
    unlit: bool,
    label: Option<String>,
    /// `burn.enter`'s position: the burn statement's, where recording its
    /// compensation fails.
    span: Span,
    comp: Option<Comp>,
}

/// The compensation of an open burn, from `comp.enter`.
struct Comp {
    /// Between `comp.enter` and `comp.leave`: the kernel covers its ops.
    open: bool,
    /// The journal mark `comp.leave` took.
    mark: usize,
    source: String,
    state_name: Option<String>,
    state: Option<Value>,
    /// The `unburn` block's line: `comp.enter`'s position.
    line: u32,
}

/// A try the frame has entered and not yet left.
struct Open {
    /// Index into the function's handlers.
    handler: usize,
    /// The frame's loop nesting at `try.enter`: what the catch and the
    /// finally run at.
    nest: u32,
    stage: Stage,
    /// The frame's open burns at `try.enter`: an error the try takes, and
    /// a `try.leave`, close the burns opened since.
    burns: usize,
    /// What the finally goes on to when it ends.
    pending: Pending,
    /// The cough payload the finally set aside, given back when it ends.
    payload: Option<Value>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Body,
    Catch,
    Finally,
}

enum Pending {
    /// Go on at this pc.
    Next(usize),
    /// Raise this error again, as it was.
    Raise(Diagnostic),
}

struct Machine<'i> {
    interp: &'i mut Interp,
    /// Every frame's registers, frame after frame.
    stack: Vec<Value>,
    /// The cell each register carries, if any: beside `stack`, the same
    /// length. A register with a cell reads and writes the cell.
    cells: Vec<Option<Cell>>,
    frames: Vec<Frame>,
    /// The tries open in every frame, frame after frame, innermost last.
    tries: Vec<Open>,
    /// The burns open in every frame, frame after frame, innermost last.
    burns: Vec<OpenBurn>,
    /// The upvalues of each frame whose closure captured any.
    upvals: Vec<Rc<[Cell]>>,
}

/// A machine's vectors between two runs. The interpreter keeps one set
/// ([`Interp::vm_regs`]): a run takes it when it starts and gives it back,
/// emptied, when it ends, so a builtin's callbacks (`xs.map(f)`) reuse
/// one set of allocations instead of making three per element. A run
/// inside a run (a callback inside a callback) finds none there and makes
/// its own: two live machines never share one.
#[derive(Default)]
pub(crate) struct Regs {
    stack: Vec<Value>,
    cells: Vec<Option<Cell>>,
    frames: Vec<Frame>,
    tries: Vec<Open>,
    burns: Vec<OpenBurn>,
    upvals: Vec<Rc<[Cell]>>,
}

/// The most registers a kept set holds room for: a run that grew its stack
/// past this gives its vectors back to the allocator instead.
const KEPT_REGISTERS: usize = 1 << 14;

/// What an instruction did to the flow.
enum Flow {
    Next(usize),
    Return(Value),
    /// A call pushed or replaced a frame, which carries its own pc.
    Switched,
}

impl Machine<'_> {
    /// Make the register stack `len` long when it is shorter. It is never
    /// shrunk: a frame that returns leaves its registers to the next frame
    /// that starts there. Its length is at most the deepest chain of
    /// frames' registers (each frame starts inside its caller's), so the
    /// depth limit (`E506`) times the largest frame; `check_alloc`'s
    /// ceiling (`E514`) refuses it first.
    fn grow_to(&mut self, len: usize, span: Span) -> Result<(), Fault> {
        if len <= self.stack.len() {
            return Ok(());
        }
        crate::value::check_alloc(
            (len as u128).saturating_mul(std::mem::size_of::<Value>() as u128),
            "the VM's registers",
            span,
        )?;
        self.stack.resize(len, Value::Null);
        self.cells.resize(len, None);
        Ok(())
    }

    /// The registers `from..to` carry no cell from now on. Only a frame
    /// whose function has a `cellnew` makes cells, and only in its own
    /// registers, so this runs for those frames alone: when one ends, and
    /// for a caller's registers above a call's result when it calls.
    fn forget_cells(&mut self, from: usize, to: usize) {
        let to = to.min(self.cells.len());
        if let Some(cells) = self.cells.get_mut(from.min(to)..to) {
            cells.fill(None);
        }
    }

    /// The run is over: every vector emptied (each register's value and
    /// cell dropped here, as a machine of its own dropped them) and given
    /// back to the interpreter for the next run, unless the stack grew
    /// past [`KEPT_REGISTERS`].
    fn give_back(self) {
        let Machine {
            interp,
            mut stack,
            mut cells,
            mut frames,
            mut tries,
            mut burns,
            mut upvals,
        } = self;
        stack.clear();
        cells.clear();
        frames.clear();
        tries.clear();
        burns.clear();
        upvals.clear();
        if stack.capacity() <= KEPT_REGISTERS {
            interp.vm_regs = Some(Regs {
                stack,
                cells,
                frames,
                tries,
                burns,
                upvals,
            });
        }
    }

    /// A frame ends (it returned, an error left it, or the run stopped):
    /// what it opened and what it holds on the interpreter's counts go.
    #[inline]
    fn release(&mut self, f: &Frame) {
        if self.burns.len() > f.burns as usize {
            self.release_burns(f.burns as usize);
        }
        if self.tries.len() > f.tries as usize {
            self.tries.truncate(f.tries as usize);
        }
        if f.up != NO_UPVALS {
            self.upvals.truncate(f.up as usize);
        }
        let held = f.nest.saturating_add(f.held_nest);
        if held != 0 {
            self.interp.loop_depth = self.interp.loop_depth.saturating_sub(held);
        }
        for _ in 0..f.held_calls {
            self.interp.leave_call();
        }
    }

    /// Burns still open in a frame that ends were left by an internal
    /// error: given back as an error leaves them, with nothing recorded.
    #[cold]
    fn release_burns(&mut self, from: usize) {
        while self.burns.len() > from {
            let Some(b) = self.burns.pop() else { break };
            if b.comp.as_ref().is_some_and(|c| c.open) {
                self.interp.compensate_close();
            }
            self.interp.burn_close(b.unlit);
        }
    }

    /// The tries open in the current frame.
    fn frame_tries(&self) -> usize {
        let from = self.frames.last().map_or(0, |f| f.tries as usize);
        self.tries.len().saturating_sub(from)
    }

    /// The burns open in the current frame.
    fn frame_burns(&self) -> usize {
        let from = self.frames.last().map_or(0, |f| f.burns as usize);
        self.burns.len().saturating_sub(from)
    }

    /// The current frame's closure's upvalues (none when it captured
    /// nothing).
    #[inline(always)]
    fn frame_upvals(&self) -> &[Cell] {
        match self.frames.last() {
            Some(f) if f.up != NO_UPVALS => self.upvals.get(f.up as usize).map_or(&[], |u| u),
            _ => &[],
        }
    }

    /// `R(r)`: the register's value, or its cell's when it carries one.
    fn get(&self, base: usize, r: u32, span: Span) -> Result<Value, Fault> {
        self.get_abs(base.saturating_add(r as usize), span)
    }

    fn get_abs(&self, i: usize, span: Span) -> Result<Value, Fault> {
        if let Some(Some(cell)) = self.cells.get(i) {
            return Ok(cell.borrow().clone());
        }
        self.stack
            .get(i)
            .cloned()
            .ok_or_else(|| broken("a register past its frame", span).into())
    }

    /// `R(r) = v`: into the register's cell when it carries one.
    fn set(&mut self, base: usize, r: u32, v: Value, span: Span) -> Result<(), Fault> {
        self.set_abs(base.saturating_add(r as usize), v, span)
    }

    fn set_abs(&mut self, i: usize, v: Value, span: Span) -> Result<(), Fault> {
        if let Some(Some(cell)) = self.cells.get(i) {
            *cell.borrow_mut() = v;
            return Ok(());
        }
        let slot = self
            .stack
            .get_mut(i)
            .ok_or_else(|| broken("a register past its frame", span))?;
        *slot = v;
        Ok(())
    }

    /// `R(r) = v` as a plain register: a cell it carried stays with the
    /// closures that captured it. The loop instructions write their item
    /// this way, so each turn starts free of the last turn's cell.
    fn put(&mut self, base: usize, r: u32, v: Value, span: Span) -> Result<(), Fault> {
        let i = base.saturating_add(r as usize);
        if let Some(cell) = self.cells.get_mut(i) {
            *cell = None;
        }
        let slot = self
            .stack
            .get_mut(i)
            .ok_or_else(|| broken("a register past its frame", span))?;
        *slot = v;
        Ok(())
    }

    /// `R(first..first+n)`, read: a call's arguments, an `exhale`'s values.
    fn window(&self, base: usize, first: u32, n: u32, span: Span) -> Result<Vec<Value>, Fault> {
        let start = base.saturating_add(first as usize);
        (start..start.saturating_add(n as usize))
            .map(|i| self.get_abs(i, span))
            .collect()
    }

    /// The current frame's upvalue `u`.
    fn upval(&self, u: u32, span: Span) -> Result<Cell, Fault> {
        self.frame_upvals()
            .get(u as usize)
            .cloned()
            .ok_or_else(|| broken("an upvalue past the closure's", span).into())
    }

    /// Whether every upvalue in `ds` can be captured in the frame at
    /// `base`: its register is in the stack, or its upvalue in the
    /// closure's list.
    fn can_capture(&self, base: usize, ds: &[UpvalDesc]) -> bool {
        let ups = self.frame_upvals().len();
        ds.iter().all(|d| match d.kind {
            UpvalKind::ParentLocal => base.saturating_add(usize::from(d.index)) < self.stack.len(),
            UpvalKind::ParentUpvalue => usize::from(d.index) < ups,
        })
    }

    /// The cell a new closure's upvalue `d` captures in the frame at
    /// `base`, which [`Machine::can_capture`] said it can.
    fn capture(&self, base: usize, d: &UpvalDesc) -> Cell {
        match d.kind {
            UpvalKind::ParentLocal => {
                let i = base.saturating_add(usize::from(d.index));
                if let Some(Some(cell)) = self.cells.get(i) {
                    return Rc::clone(cell);
                }
                Rc::new(RefCell::new(
                    self.stack.get(i).cloned().unwrap_or(Value::Null),
                ))
            }
            UpvalKind::ParentUpvalue => self
                .frame_upvals()
                .get(usize::from(d.index))
                .cloned()
                .unwrap_or_else(|| Rc::new(RefCell::new(Value::Null))),
        }
    }

    fn exec(&mut self, program: &Rc<Program>) -> Result<Value, Fault> {
        loop {
            match self.run_frame_code(program) {
                Ok(Some(v)) => return Ok(v),
                Ok(None) => {}
                Err(e) => self.unwind(program, *e)?,
            }
        }
    }

    /// Run the current frame's instructions until it calls, returns or
    /// raises. `Some(v)` when the entry frame returned `v`. The frame and
    /// its function are looked up once, not per instruction; the frame's
    /// pc is kept current after each one, which is where an error raised
    /// by the next is offered to the tries.
    fn run_frame_code(&mut self, program: &Rc<Program>) -> Result<Option<Value>, Fault> {
        let (mut pc, mut base, mut proto) = self.top(program)?;
        loop {
            if proto.plain {
                pc = self.run_fast(proto, pc, base)?;
            }
            let Some(Some(d)) = proto.code.get(pc) else {
                let span = proto.spans.get(pc).copied().unwrap_or_default();
                return Err(broken("a pc that starts no instruction", span).into());
            };
            // A call of a compiled function of this program, a return and
            // a tail call switch frames here, without `step`: the frame's
            // pc is written when it is left (at a call) or when an error
            // leaves the instruction, and the position is looked up only
            // for an error.
            let switched = match d.op {
                Op::CALL => self
                    .call_here(program, proto, d, base)
                    .map(|c| c.then_some(None)),
                Op::RET => self.ret_here(proto, d, base).map(Some),
                Op::TAILCALL => self
                    .tail_here(program, proto, d, base)
                    .map(|c| c.then_some(None)),
                _ => Ok(None),
            };
            match switched {
                Ok(None) => {}
                Ok(Some(Some(v))) => return Ok(Some(v)),
                Ok(Some(None)) => {
                    (pc, base, proto) = self.top(program)?;
                    continue;
                }
                Err(e) => {
                    // `ret_here` places its own error (the frame may be
                    // gone by then).
                    if d.op != Op::RET {
                        if let Some(f) = self.frames.last_mut() {
                            f.pc = pc;
                        }
                    }
                    return Err(e);
                }
            }
            #[cfg(feature = "bench-stats")]
            let timer = stats::start(d.op);
            // The general path: the frame's pc is where an error from this
            // instruction is offered to the tries.
            if let Some(f) = self.frames.last_mut() {
                f.pc = pc;
            }
            let span = proto.spans.get(pc).copied().unwrap_or_default();
            let flow = self.step(program, proto, d, base, span);
            #[cfg(feature = "bench-stats")]
            stats::stop(timer);
            match flow? {
                Flow::Next(next) => {
                    pc = next;
                    if let Some(f) = self.frames.last_mut() {
                        f.pc = next;
                    }
                }
                // A call or a return: the loop goes on in the frame on top
                // now, without going back through `exec`.
                Flow::Return(v) => {
                    if let Some(v) = self.ret(program, v, span)? {
                        return Ok(Some(v));
                    }
                    (pc, base, proto) = self.top(program)?;
                }
                Flow::Switched => (pc, base, proto) = self.top(program)?,
            }
        }
    }

    /// The frame's instructions from `pc` for as long as the fast path
    /// runs them: the pc of the first one it does not run (or of no
    /// instruction at all), or the error a builtin it called raised. A
    /// function of its own, never inlined: the fast path's frame holds
    /// every arm's locals, and it must not be on the stack while the
    /// general path runs a call. A builtin that calls back into the VM
    /// (`Builtin::calls_back`) is therefore never called here: it costs
    /// `run_frame_code`, `step` and the group, as it did before there was
    /// a fast path, which is what keeps a level of re-entry no deeper than
    /// the tree-walker's.
    #[inline(never)]
    fn run_fast(&mut self, proto: &Proto, mut pc: usize, base: usize) -> Result<usize, Fault> {
        while let Some(Some(d)) = proto.code.get(pc) {
            #[cfg(feature = "bench-stats")]
            let timer = stats::start(d.op);
            let next = match self.fast(proto, d, base) {
                Some(next) => next,
                None => match self.fast_call(proto, d, base) {
                    Some(Ok(next)) => next,
                    Some(Err(e)) => {
                        // As on the general path: the frame's pc is where
                        // the error is offered to the tries.
                        if let Some(f) = self.frames.last_mut() {
                            f.pc = pc;
                        }
                        #[cfg(feature = "bench-stats")]
                        stats::stop(timer);
                        return Err(e);
                    }
                    None => break,
                },
            };
            #[cfg(feature = "bench-stats")]
            stats::stop(timer);
            pc = next;
        }
        Ok(pc)
    }

    /// The fast loop's second tier, for a frame whose registers carry no
    /// cell: the calls of builtins that cannot call back into the VM
    /// (`callb`, and `callm` on a site whose cache holds the receiver's
    /// type), `interp`, string `+`, and the list and map builders and
    /// stores. `None`
    /// when `d` is not one of them or not its common case, having changed
    /// nothing, so the general path runs it from the start; a builtin's
    /// error comes back as the general path raises it.
    #[inline(never)]
    fn fast_call(
        &mut self,
        proto: &Proto,
        d: &DecodedInstr,
        base: usize,
    ) -> Option<Result<usize, Fault>> {
        let pc = d.pc as usize;
        let next = pc + d.width as usize;
        let r = |x: u32| base.saturating_add(x as usize);
        let v = match d.op {
            Op::CALLB => {
                let b = *crate::stdlib::builtin_catalogue().get(d.b_u as usize)?;
                if b.calls_back {
                    return None;
                }
                // `R(A)`, where the result goes, then the arguments.
                let start = r(d.a);
                let args = self
                    .stack
                    .get(start..start.saturating_add(d.c as usize).saturating_add(1))?
                    .get(1..)?;
                let span = span_of(proto, pc);
                match self.interp.call_builtin(b, args, span) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e.into())),
                }
            }
            Op::CALLM => {
                let start = r(d.a);
                let argv = self
                    .stack
                    .get(start..start.saturating_add(d.c as usize).saturating_add(1))?;
                let recv = argv.first()?;
                let b = crate::stdlib::method_at(recv, proto.hints.get(pc)?.get())?;
                if b.calls_back || !b.arity_ok(argv.len()) {
                    return None;
                }
                if let Value::Map(map) = recv {
                    // A function the map holds under the name is called
                    // instead: the general path's case.
                    let Value::Str(name) = proto.consts.get(d.b_u as usize)? else {
                        return None;
                    };
                    let held = map
                        .try_borrow()
                        .ok()?
                        .get(&**name)
                        .is_some_and(|f| matches!(f, Value::Func(_) | Value::Builtin(_)));
                    if held {
                        return None;
                    }
                }
                let span = span_of(proto, pc);
                match (b.func)(self.interp, argv, span) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e.or_at(span).into())),
                }
            }
            Op::INTERP => {
                let first = r(d.b_u);
                let parts = self
                    .stack
                    .get(first..first.saturating_add(d.c as usize).saturating_add(1))?;
                interpolate(parts)
            }
            // String `+`: the operator's own result, made in one block.
            Op::ADD => {
                let (Value::Str(l), Value::Str(rv)) =
                    (self.stack.get(r(d.b_u))?, self.stack.get(r(d.c))?)
                else {
                    return None;
                };
                crate::value::build_str(|out| {
                    out.reserve(l.len() + rv.len());
                    out.push_str(l);
                    out.push_str(rv);
                })
            }
            Op::NEWLIST => Value::list(Vec::with_capacity(d.b_u as usize)),
            Op::NEWMAP => Value::map(IndexMap::with_capacity(d.b_u as usize)),
            Op::APPENDLIST => {
                let v = self.stack.get(r(d.b_u))?.clone();
                let Value::List(list) = self.stack.get(r(d.a))? else {
                    return None;
                };
                list.try_borrow_mut().ok()?.push(v);
                return Some(Ok(next));
            }
            Op::INSERTMAP => {
                let Value::Str(key) = self.stack.get(r(d.b_u))? else {
                    return None;
                };
                let v = self.stack.get(r(d.c))?.clone();
                let Value::Map(map) = self.stack.get(r(d.a))? else {
                    return None;
                };
                let old = map.try_borrow_mut().ok()?.insert(key.to_string(), v);
                drop(old);
                return Some(Ok(next));
            }
            Op::SETINDEX => {
                let (Value::Map(map), Value::Str(key)) =
                    (self.stack.get(r(d.a))?, self.stack.get(r(d.b_u))?)
                else {
                    return None;
                };
                let v = self.stack.get(r(d.c))?.clone();
                let mut map = map.try_borrow_mut().ok()?;
                // A key already there keeps its place (as `insert` does)
                // and needs no `String` made for it.
                let old = match map.get_mut(&**key) {
                    Some(slot) => Some(std::mem::replace(slot, v)),
                    None => map.insert(key.to_string(), v),
                };
                drop(map);
                drop(old);
                return Some(Ok(past_nops(proto, next, 2)));
            }
            _ => return None,
        };
        // The register exists: each arm above read it or its window.
        if let Some(slot) = self.stack.get_mut(r(d.a)) {
            *slot = v;
        }
        Some(Ok(next))
    }

    /// The frame on top: its pc, its base and its function. An
    /// instruction that reads an upvalue takes it from the frame.
    #[inline]
    fn top<'p>(&self, program: &'p Program) -> Result<(usize, usize, &'p Proto), Fault> {
        let (fi, pc, base) = match self.frames.last() {
            Some(f) => (f.func, f.pc, f.base),
            None => return Err(broken("an empty call stack", Span::default()).into()),
        };
        let proto = program
            .funcs
            .get(fi)
            .ok_or_else(|| broken("no such function", Span::default()))?;
        Ok((pc, base, proto))
    }

    /// The common cases of the hot instructions in a frame whose registers
    /// carry no cell: the next pc when `d` ran here, `None` when it did not
    /// and changed nothing, so the general path runs it from the start and
    /// raises its error, if any, with the tree-walker's code and position.
    #[inline(always)]
    fn fast(&mut self, proto: &Proto, d: &DecodedInstr, base: usize) -> Option<usize> {
        let pc = d.pc as usize;
        let next = pc + d.width as usize;
        let r = |x: u32| base.saturating_add(x as usize);
        let v = match d.op {
            Op::NOP => return Some(next),
            Op::MOV => self.stack.get(r(d.b_u))?.clone(),
            Op::LOADK => proto.consts.get(d.b_u as usize)?.clone(),
            Op::LOADNULL => Value::Null,
            Op::LOADBOOL => Value::Bool(d.b_u != 0),
            Op::LOADINT => Value::Int(i64::from(d.b_s)),
            Op::ADD | Op::SUB | Op::MUL | Op::DIV | Op::MOD | Op::EQ | Op::LT | Op::LE => {
                let l = self.stack.get(r(d.b_u))?;
                let rv = self.stack.get(r(d.c))?;
                fast_compare(op_of(d.op), l, rv)?
            }
            Op::ADDK | Op::SUBK | Op::MULK | Op::DIVK | Op::MODK | Op::EQK | Op::LTK | Op::LEK => {
                let l = self.stack.get(r(d.b_u))?;
                let k = proto.consts.get(d.c as usize)?;
                fast_compare(op_of(d.op), l, k)?
            }
            Op::NOT => Value::Bool(!self.stack.get(r(d.b_u))?.truthy()),
            Op::EQJ | Op::NEJ | Op::LTJ | Op::LEJ => {
                let op = match d.op {
                    Op::EQJ => BinOp::Eq,
                    Op::NEJ => BinOp::Ne,
                    Op::LTJ => BinOp::Lt,
                    _ => BinOp::Le,
                };
                let l = self.stack.get(r(d.a))?;
                let rv = self.stack.get(r(d.b_u))?;
                let Value::Bool(t) = fast_compare(op, l, rv)? else {
                    return None;
                };
                return if t { fast_jump(pc, d.c_s) } else { Some(next) };
            }
            Op::JMP | Op::LOOP => return fast_jump(pc, d.b_s),
            Op::TEST | Op::TESTN => {
                let truthy = self.stack.get(r(d.a))?.truthy();
                return if truthy == (d.op == Op::TEST) {
                    fast_jump(pc, d.b_s)
                } else {
                    Some(next)
                };
            }
            Op::STEPS => {
                let n = u64::from(d.b_u);
                let i = &*self.interp;
                if i.max_steps != 0 && i.steps.saturating_add(n) > i.max_steps {
                    return None;
                }
                self.set_nest(d.a);
                self.interp.steps += n;
                return Some(next);
            }
            Op::FORLOOP => {
                let (Value::Int(cur), Value::Int(limit), Value::Int(step)) = (
                    self.stack.get(r(d.a))?,
                    self.stack.get(r(d.a + 1))?,
                    self.stack.get(r(d.a + 2))?,
                ) else {
                    return None;
                };
                let (cur, limit, step) = (*cur, *limit, *step);
                let target = fast_jump(pc, d.b_s)?;
                if self.stack.len() <= r(d.a + 3) {
                    return None;
                }
                let more =
                    cur.checked_add(step)
                        .filter(|n| if step > 0 { *n < limit } else { *n > limit });
                return match more {
                    Some(n) => {
                        *self.stack.get_mut(r(d.a))? = Value::Int(n);
                        *self.stack.get_mut(r(d.a + 3))? = Value::Int(n);
                        Some(target)
                    }
                    None => Some(next),
                };
            }
            Op::GETUP | Op::CELLGET => self
                .frame_upvals()
                .get(d.b_u as usize)?
                .try_borrow()
                .ok()?
                .clone(),
            Op::GETINDEX => {
                let obj = self.stack.get(r(d.b_u))?;
                let idx = self.stack.get(r(d.c))?;
                let v = match (obj, idx) {
                    (Value::List(list), Value::Int(i)) => {
                        let at = usize::try_from(*i).ok()?;
                        list.try_borrow().ok()?.get(at)?.clone()
                    }
                    (Value::Map(map), Value::Str(key)) => {
                        map.try_borrow().ok()?.get(&**key)?.clone()
                    }
                    _ => return None,
                };
                *self.stack.get_mut(r(d.a))? = v;
                return Some(past_nops(proto, next, 1));
            }
            Op::GETFIELD => {
                let Value::Map(map) = self.stack.get(r(d.b_u))? else {
                    return None;
                };
                let Value::Str(name) = proto.consts.get(d.c as usize)? else {
                    return None;
                };
                let map = map.try_borrow().ok()?;
                let i = field_index(&map, name, proto.hints.get(pc)?)?;
                let v = map.get_index(i)?.1.clone();
                v
            }
            Op::SETFIELD => {
                let Value::Map(map) = self.stack.get(r(d.a))? else {
                    return None;
                };
                let Value::Str(name) = proto.consts.get(d.b_u as usize)? else {
                    return None;
                };
                let v = self.stack.get(r(d.c))?.clone();
                let mut map = map.try_borrow_mut().ok()?;
                let i = field_index(&map, name, proto.hints.get(pc)?)?;
                let old = std::mem::replace(map.get_index_mut(i)?.1, v);
                drop(map);
                drop(old);
                return Some(next);
            }
            Op::SETINDEX => {
                let (Value::List(list), Value::Int(i)) =
                    (self.stack.get(r(d.a))?, self.stack.get(r(d.b_u))?)
                else {
                    return None;
                };
                let at = usize::try_from(*i).ok()?;
                let v = self.stack.get(r(d.c))?.clone();
                let mut list = list.try_borrow_mut().ok()?;
                let slot = list.get_mut(at)?;
                let old = std::mem::replace(slot, v);
                drop(list);
                drop(old);
                return Some(past_nops(proto, next, 2));
            }
            _ => return None,
        };
        *self.stack.get_mut(r(d.a))? = v;
        Some(next)
    }

    /// Run one instruction. The groups are functions of their own, so the
    /// frame each re-entry (a builtin calling back into the VM) leaves on
    /// the Rust stack is a small one: `step` and the group it went to.
    fn step(
        &mut self,
        program: &Rc<Program>,
        proto: &Proto,
        d: &DecodedInstr,
        base: usize,
        span: Span,
    ) -> Result<Flow, Fault> {
        match d.op {
            Op::CALL | Op::CALLB | Op::CALLM | Op::RET | Op::TAILCALL | Op::CLOSURE => {
                self.call_op(program, proto, d, base, span)
            }
            Op::GETUP | Op::CELLGET | Op::SETUP | Op::CELLSET | Op::CELLNEW => {
                self.cell_op(d, base, span)
            }
            Op::EXHALE | Op::INTERP => self.text_op(d, base, span),
            Op::FORPREP | Op::FORLOOP | Op::ITERPREP | Op::ITERNEXT => self.loop_op(d, base, span),
            Op::NEWLIST
            | Op::NEWMAP
            | Op::APPENDLIST
            | Op::INSERTMAP
            | Op::GETFIELD
            | Op::SETFIELD
            | Op::GETINDEX
            | Op::SETINDEX => self.data_op(proto, d, base, span),
            Op::TRY_ENTER | Op::TRY_LEAVE | Op::THROW => self.try_op(proto, d, base, span),
            Op::BURN_ENTER
            | Op::BURN_OP
            | Op::BURN_LEAVE
            | Op::COMP_ENTER
            | Op::COMP_LEAVE
            | Op::PACK
            | Op::CHAIN_MAKE => self.burn_op(proto, d, base, span),
            _ => self.basic_op(program, proto, d, base, span),
        }
    }

    /// Moves, constants, operators, jumps and the step budget.
    fn basic_op(
        &mut self,
        program: &Rc<Program>,
        proto: &Proto,
        d: &DecodedInstr,
        base: usize,
        span: Span,
    ) -> Result<Flow, Fault> {
        let pc = d.pc as usize;
        let next = pc + d.width as usize;
        match d.op {
            Op::NOP => {}
            // ----- moves and constants ---------------------------------------
            Op::MOV => {
                let v = self.get(base, d.b_u, span)?;
                self.set(base, d.a, v, span)?;
            }
            Op::LOADK => {
                let v = konst(proto, d.b_u, span)?;
                self.set(base, d.a, v, span)?;
            }
            Op::LOADNULL => self.set(base, d.a, Value::Null, span)?,
            Op::LOADBOOL => self.set(base, d.a, Value::Bool(d.b_u != 0), span)?,
            Op::LOADINT => self.set(base, d.a, Value::Int(i64::from(d.b_s)), span)?,
            Op::LOADG => {
                let v = self
                    .interp
                    .prelude_value(&name(proto, d.b_u, span)?, span)?;
                self.set(base, d.a, v, span)?;
            }

            // ----- arithmetic, comparison, logic ------------------------------
            Op::ADD | Op::SUB | Op::MUL | Op::DIV | Op::MOD | Op::EQ | Op::LT | Op::LE => {
                let l = self.get(base, d.b_u, span)?;
                let r = self.get(base, d.c, span)?;
                let v = operate(op_of(d.op), l, r, span)?;
                self.set(base, d.a, v, span)?;
            }
            Op::ADDK | Op::SUBK | Op::MULK | Op::DIVK | Op::MODK | Op::EQK | Op::LTK | Op::LEK => {
                let l = self.get(base, d.b_u, span)?;
                let r = konst(proto, d.c, span)?;
                let v = operate(op_of(d.op), l, r, span)?;
                self.set(base, d.a, v, span)?;
            }
            Op::POW | Op::POWK => {
                let l = self.get(base, d.b_u, span)?;
                let r = if d.op == Op::POW {
                    self.get(base, d.c, span)?
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
                let v = negate(self.get(base, d.b_u, span)?, span)?;
                self.set(base, d.a, v, span)?;
            }
            Op::NOT => {
                let v = Value::Bool(!self.get(base, d.b_u, span)?.truthy());
                self.set(base, d.a, v, span)?;
            }

            // ----- control ----------------------------------------------------
            Op::EQJ | Op::NEJ | Op::LTJ | Op::LEJ => {
                let l = self.get(base, d.a, span)?;
                let r = self.get(base, d.b_u, span)?;
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
                let v = self.get(base, d.b_u, span)?;
                if v.truthy() {
                    self.set(base, d.a, v, span)?;
                } else {
                    return Ok(Flow::Next(jump(pc, d.c_s, span)?));
                }
            }

            // ----- the step budget ------------------------------------------------
            Op::STEPS => {
                self.set_nest(d.a);
                // The charge in one add when the budget holds all of it;
                // otherwise step by step, so E515 is `tick`'s own.
                let n = u64::from(d.b_u);
                let i = &mut *self.interp;
                if i.max_steps == 0 || i.steps.saturating_add(n) <= i.max_steps {
                    i.steps += n;
                } else {
                    for _ in 0..d.b_u {
                        self.interp.tick(span)?;
                    }
                }
            }

            Op::WIDE => return Err(broken("a bare `wide`", span).into()),

            op => return Err(not_built(op, span).into()),
        }
        Ok(Flow::Next(next))
    }

    /// Calls, returns and `closure`.
    fn call_op(
        &mut self,
        program: &Rc<Program>,
        proto: &Proto,
        d: &DecodedInstr,
        base: usize,
        span: Span,
    ) -> Result<Flow, Fault> {
        let pc = d.pc as usize;
        let next = pc + d.width as usize;
        match d.op {
            Op::CALL => {
                // `run_frame_code` has made a frame for a compiled callee
                // of this program already; this is every other callee.
                if self.call_here(program, proto, d, base)? {
                    return Ok(Flow::Switched);
                }
                let callee = self.get(base, d.a, span)?;
                let args = self.window(base, d.a + 1, d.b_u, span)?;
                let v = self.interp.call(&callee, args, span)?;
                let reg = base.saturating_add(d.a as usize);
                if !self.write_result(reg, d.c, v) {
                    return Err(broken("a result register past its frame", span).into());
                }
            }
            Op::CALLB => {
                let b = crate::stdlib::builtin_catalogue()
                    .get(d.b_u as usize)
                    .copied()
                    .ok_or_else(|| broken("a builtin index past the catalogue", span))?;
                // The arguments are read in place when no register of the
                // window carries a cell; a callback runs in a VM of its own,
                // so nothing writes these registers during the call.
                let start = base.saturating_add(d.a as usize + 1);
                let end = start.saturating_add(d.c as usize);
                let plain = self
                    .cells
                    .get(start..end)
                    .is_some_and(|c| c.iter().all(Option::is_none));
                let v = match self.stack.get(start..end) {
                    Some(args) if plain => self.interp.call_builtin(b, args, span)?,
                    _ => {
                        let args = self.window(base, d.a + 1, d.c, span)?;
                        self.interp.call_builtin(b, &args, span)?
                    }
                };
                self.set(base, d.a, v, span)?;
            }
            Op::RET => {
                let v = self.get(base, d.a, span)?;
                return Ok(Flow::Return(v));
            }
            Op::TAILCALL => {
                if self.tail_here(program, proto, d, base)? {
                    return Ok(Flow::Switched);
                }
                let callee = self.get(base, d.a, span)?;
                let args = self.window(base, d.a + 1, d.b_u, span)?;
                let v = self.interp.call(&callee, args, span)?;
                return Ok(Flow::Return(v));
            }
            Op::CALLM => {
                let name = name(proto, d.b_u, span)?;
                // The receiver and its arguments, `R(A)` onwards: the
                // layout a method takes, read in place when no register
                // of it carries a cell (a callback runs in a VM of its
                // own, so nothing writes these registers during the call).
                let start = base.saturating_add(d.a as usize);
                let n = (d.c as usize).saturating_add(1);
                let v = match in_place(&self.stack, &self.cells, start, n) {
                    Some(argv) => {
                        if let (Some(recv), Some(hint)) = (argv.first(), proto.hints.get(pc)) {
                            // The site's cache, for the fast loop's next call.
                            if let Some(slot) = crate::stdlib::method_slot(recv, &name) {
                                hint.set(slot);
                            }
                        }
                        self.interp.call_method(argv, &name, span)?
                    }
                    None => {
                        let argv = self.window(base, d.a, d.c.saturating_add(1), span)?;
                        self.interp.call_method(&argv, &name, span)?
                    }
                };
                self.set(base, d.a, v, span)?;
            }
            Op::CLOSURE => {
                let child = proto
                    .children
                    .get(d.b_u as usize)
                    .copied()
                    .ok_or_else(|| broken("a child function past the table", span))?;
                let target = program
                    .heads
                    .get(child)
                    .ok_or_else(|| broken("no such function", span))?;
                if !self.can_capture(base, &target.upvalues) {
                    return Err(broken("a capture past its frame", span).into());
                }
                // The upvalue list in one allocation (an exact-size
                // iterator), the name shared with the function's head, and
                // no parameter names: a compiled function's arity is its
                // own (`Function::arity`).
                let upvals: Rc<[Cell]> = target
                    .upvalues
                    .iter()
                    .map(|u| self.capture(base, u))
                    .collect();
                let f = Function {
                    name: target.label.clone(),
                    params: Vec::new(),
                    body: FnBody::Compiled(CompiledFn {
                        program: Rc::clone(program),
                        index: child,
                        arity: target.arity,
                        upvals,
                    }),
                    closure: self.interp.globals.clone(),
                };
                self.set(base, d.a, Value::Func(Rc::new(f)), span)?;
            }

            _ => return Err(broken("an instruction sent to the wrong group", span).into()),
        }
        Ok(Flow::Next(next))
    }

    /// Upvalues and cells.
    fn cell_op(&mut self, d: &DecodedInstr, base: usize, span: Span) -> Result<Flow, Fault> {
        let next = d.pc as usize + d.width as usize;
        match d.op {
            Op::GETUP | Op::CELLGET => {
                let v = self.upval(d.b_u, span)?.borrow().clone();
                self.set(base, d.a, v, span)?;
            }
            Op::SETUP | Op::CELLSET => {
                let v = self.get(base, d.b_u, span)?;
                *self.upval(d.a, span)?.borrow_mut() = v;
            }
            Op::CELLNEW => {
                let v = self.get(base, d.a, span)?;
                let i = base.saturating_add(d.a as usize);
                let slot = self
                    .cells
                    .get_mut(i)
                    .ok_or_else(|| broken("a register past its frame", span))?;
                *slot = Some(Rc::new(RefCell::new(v)));
            }

            _ => return Err(broken("an instruction sent to the wrong group", span).into()),
        }
        Ok(Flow::Next(next))
    }

    /// `exhale` and `interp`.
    fn text_op(&mut self, d: &DecodedInstr, base: usize, span: Span) -> Result<Flow, Fault> {
        let next = d.pc as usize + d.width as usize;
        match d.op {
            Op::EXHALE => {
                let values = self.window(base, d.a, d.b_u, span)?;
                let mut line = String::new();
                for (i, v) in values.iter().enumerate() {
                    exhale_part(&mut line, i, v);
                }
                self.interp.write_exhaled(line)?;
            }
            Op::INTERP => {
                let first = base.saturating_add(d.b_u as usize);
                let n = (d.c as usize).saturating_add(1);
                let s = match in_place(&self.stack, &self.cells, first, n) {
                    Some(parts) => interpolate(parts),
                    None => interpolate(&self.window(base, d.b_u, d.c.saturating_add(1), span)?),
                };
                self.set(base, d.a, s, span)?;
            }

            _ => return Err(broken("an instruction sent to the wrong group", span).into()),
        }
        Ok(Flow::Next(next))
    }

    /// The loop instructions.
    fn loop_op(&mut self, d: &DecodedInstr, base: usize, span: Span) -> Result<Flow, Fault> {
        let pc = d.pc as usize;
        let next = pc + d.width as usize;
        match d.op {
            Op::FORPREP => {
                let (start, turns) = self.for_window(base, d.a, span)?;
                if turns == 0 {
                    return Ok(Flow::Next(jump(pc, d.b_s, span)?));
                }
                self.put(base, d.a + 3, Value::Int(start), span)?;
            }
            Op::FORLOOP => {
                let cur = self.int_at(base, d.a, Op::FORLOOP, span)?;
                let limit = self.int_at(base, d.a + 1, Op::FORLOOP, span)?;
                let step = self.int_at(base, d.a + 2, Op::FORLOOP, span)?;
                let more =
                    cur.checked_add(step)
                        .filter(|n| if step > 0 { *n < limit } else { *n > limit });
                if let Some(n) = more {
                    self.set(base, d.a, Value::Int(n), span)?;
                    self.put(base, d.a + 3, Value::Int(n), span)?;
                    return Ok(Flow::Next(jump(pc, d.b_s, span)?));
                }
            }
            Op::ITERPREP => {
                let source = self.get(base, d.a, span)?;
                let items = self.interp.iterate(source, span)?;
                self.put(base, d.a + 1, Value::list(items), span)?;
                self.put(base, d.a + 2, Value::Int(0), span)?;
            }
            Op::ITERNEXT => {
                let Value::List(items) = self.get(base, d.a + 1, span)? else {
                    return Err(
                        misuse(d.op, "found no `iterprep` state in its window", span).into(),
                    );
                };
                let Value::Int(at) = self.get(base, d.a + 2, span)? else {
                    return Err(
                        misuse(d.op, "found no `iterprep` state in its window", span).into(),
                    );
                };
                let item = usize::try_from(at)
                    .ok()
                    .and_then(|i| items.borrow().get(i).cloned());
                match item {
                    Some(item) => {
                        self.put(base, d.a + 3, item, span)?;
                        self.set(base, d.a + 2, Value::Int(at.saturating_add(1)), span)?;
                    }
                    None => return Ok(Flow::Next(jump(pc, d.b_s, span)?)),
                }
            }

            _ => return Err(broken("an instruction sent to the wrong group", span).into()),
        }
        Ok(Flow::Next(next))
    }

    /// Lists and maps.
    fn data_op(
        &mut self,
        proto: &Proto,
        d: &DecodedInstr,
        base: usize,
        span: Span,
    ) -> Result<Flow, Fault> {
        let next = d.pc as usize + d.width as usize;
        match d.op {
            Op::NEWLIST => {
                self.set(
                    base,
                    d.a,
                    Value::list(Vec::with_capacity(d.b_u as usize)),
                    span,
                )?;
            }
            Op::NEWMAP => {
                let m = IndexMap::with_capacity(d.b_u as usize);
                self.set(base, d.a, Value::map(m), span)?;
            }
            Op::APPENDLIST => {
                let Value::List(list) = self.get(base, d.a, span)? else {
                    return Err(misuse(d.op, "names a register that holds no list", span).into());
                };
                let v = self.get(base, d.b_u, span)?;
                list.borrow_mut().push(v);
            }
            Op::INSERTMAP => {
                let Value::Map(map) = self.get(base, d.a, span)? else {
                    return Err(misuse(d.op, "names a register that holds no map", span).into());
                };
                let key = map_key(self.get(base, d.b_u, span)?, span)?;
                let v = self.get(base, d.c, span)?;
                map.borrow_mut().insert(key, v);
            }
            Op::GETFIELD => {
                let obj = self.get(base, d.b_u, span)?;
                let name = name(proto, d.c, span)?;
                let hit = match &obj {
                    Value::Map(map) => {
                        let map = map.borrow();
                        proto
                            .hints
                            .get(d.pc as usize)
                            .and_then(|h| field_index(&map, &name, h))
                            .and_then(|i| map.get_index(i))
                            .map(|(_, v)| v.clone())
                    }
                    _ => None,
                };
                let v = match hit {
                    Some(v) => v,
                    None => self.interp.member(obj, &name, false, span)?,
                };
                self.set(base, d.a, v, span)?;
            }
            Op::SETFIELD => {
                let obj = self.get(base, d.a, span)?;
                let name = name(proto, d.b_u, span)?;
                let v = self.get(base, d.c, span)?;
                let hint = proto
                    .hints
                    .get(d.pc as usize)
                    .ok_or_else(|| broken("a pc past the function", span))?;
                set_field(&obj, &name, v, hint, span)?;
            }
            // A trailing `nop` gives the index its position, a second one
            // the object's (`setindex`): `index` and `set_index` raise a
            // wrong index type there, and a store into what is not a list
            // or a map at the object's, as the tree-walker does. The run
            // goes on past them.
            Op::GETINDEX => {
                let obj = self.get(base, d.b_u, span)?;
                let idx = self.get(base, d.c, span)?;
                let idx_span = part_span(proto, next, 0, span);
                let v = self.interp.index(obj, idx, span, idx_span)?;
                self.set(base, d.a, v, span)?;
                return Ok(Flow::Next(past_nops(proto, next, 1)));
            }
            Op::SETINDEX => {
                let obj = self.get(base, d.a, span)?;
                let idx = self.get(base, d.b_u, span)?;
                let v = self.get(base, d.c, span)?;
                let idx_span = part_span(proto, next, 0, span);
                let obj_span = part_span(proto, next, 1, span);
                set_index(&obj, &idx, v, span, idx_span, obj_span)?;
                return Ok(Flow::Next(past_nops(proto, next, 2)));
            }

            _ => return Err(broken("an instruction sent to the wrong group", span).into()),
        }
        Ok(Flow::Next(next))
    }

    /// `try.enter`, `try.leave` and `throw`.
    fn try_op(
        &mut self,
        proto: &Proto,
        d: &DecodedInstr,
        base: usize,
        span: Span,
    ) -> Result<Flow, Fault> {
        let next = d.pc as usize + d.width as usize;
        match d.op {
            Op::TRY_ENTER => {
                let handler = d.b_u as usize;
                if let Some(f) = self.frames.last().copied() {
                    // Entering a try again (a loop around it) ends the
                    // earlier entry and everything opened inside it.
                    let from = f.tries as usize;
                    let again = self
                        .tries
                        .get(from..)
                        .and_then(|own| own.iter().position(|o| o.handler == handler));
                    if let Some(at) = again {
                        self.tries.truncate(from + at);
                    }
                    let burns = self.frame_burns();
                    self.tries.push(Open {
                        handler,
                        nest: f.nest,
                        burns,
                        stage: Stage::Body,
                        pending: Pending::Next(next),
                        payload: None,
                    });
                }
            }
            Op::TRY_LEAVE => return self.leave(proto, d, next, span),
            Op::THROW => {
                let v = self.get(base, d.a, span)?;
                return Err(self.interp.cough(v, span).into());
            }

            _ => return Err(broken("an instruction sent to the wrong group", span).into()),
        }
        Ok(Flow::Next(next))
    }

    /// Set the current frame's loop nesting, keeping the interpreter's loop
    /// depth (the sum over the frames) in step.
    fn set_nest(&mut self, nest: u32) {
        if let Some(f) = self.frames.last_mut() {
            if f.nest == nest && self.interp.loop_depth >= nest {
                return;
            }
            let depth = self.interp.loop_depth.saturating_sub(f.nest);
            self.interp.loop_depth = depth.saturating_add(nest);
            f.nest = nest;
        }
    }

    /// `try.leave A`. With `A = 0`: out of the body or the catch, the
    /// finally runs (then goes on after this instruction), or the try is
    /// closed when it has none; at the end of the finally, the try is
    /// closed and what was pending goes on: the next pc, or the error
    /// raised again. With `A != 0`: the try is closed and whatever it had
    /// pending is dropped (a finally's own `break`, `continue`, `snuff`).
    fn leave(
        &mut self,
        proto: &Proto,
        d: &DecodedInstr,
        next: usize,
        span: Span,
    ) -> Result<Flow, Fault> {
        let open = match self.frame_tries() {
            0 => None,
            _ => self.tries.pop(),
        };
        let Some(open) = open else {
            return Err(misuse(d.op, "found no open try in its frame", span).into());
        };
        // Out of the burns opened inside the try first, as `break`,
        // `continue` and `snuff` leave them in the tree-walker; their own
        // error is raised inside the try.
        if let Err(e) = self.close_burns(open.burns, true) {
            self.tries.push(open);
            return Err(e);
        }
        if d.a != 0 {
            return Ok(Flow::Next(next));
        }
        if open.stage == Stage::Finally {
            self.interp.cough_payload = open.payload;
            return match open.pending {
                Pending::Next(pc) => Ok(Flow::Next(pc)),
                Pending::Raise(e) => Err(e.into()),
            };
        }
        let h = proto
            .handlers
            .get(open.handler)
            .ok_or_else(|| broken("a handler past the table", span))?;
        if h.finally_pc == RESERVED_U32 {
            return Ok(Flow::Next(next));
        }
        Ok(Flow::Next(self.enter_finally(open, h, Pending::Next(next))))
    }

    /// Put `open` back in the finally stage with `pending` to go on to, at
    /// the try's loop nesting: the pc to go to.
    fn enter_finally(&mut self, mut open: Open, h: &HandlerEntry, pending: Pending) -> usize {
        open.stage = Stage::Finally;
        open.pending = pending;
        open.payload = self.interp.cough_payload.take();
        let nest = open.nest;
        if !self.frames.is_empty() {
            self.tries.push(open);
        }
        self.set_nest(nest);
        h.finally_pc as usize
    }

    /// An error was raised: offer it to the open tries, innermost first,
    /// from the frame that raised it out through its callers (each at the
    /// pc of its call). A try that takes it moves the run to its catch or
    /// its finally; a frame with none that does is left. Past the VM's
    /// entry frame the error is the run's.
    fn unwind(&mut self, program: &Program, mut e: Diagnostic) -> Result<(), Fault> {
        let mut at: Option<usize> = None;
        loop {
            let Some(frame) = self.frames.last() else {
                return Err(e.into());
            };
            let pc = at.unwrap_or(frame.pc);
            let entry = self.frames.len() == 1;
            let Some(proto) = program.funcs.get(frame.func) else {
                return Err(e.into());
            };
            e = match self.offer(proto, pc, e)? {
                Some(e) => self.unwind_burns(0, e),
                None => return Ok(()),
            };
            if entry {
                // The caller of `run_frame` gives the frame back.
                return Err(e.into());
            }
            if let Some(f) = self.frames.pop() {
                self.release(&f);
                self.end_cells(program, &f);
            }
            at = self.frames.last().map(|f| f.site);
        }
    }

    /// Offer `e`, raised at `pc` of the current frame, to the frame's open
    /// tries. `None` when one took it (the frame's pc is now its catch or
    /// its finally); `Some(e)` when none did, every try passed over closed.
    fn offer(
        &mut self,
        proto: &Proto,
        pc: usize,
        e: Diagnostic,
    ) -> Result<Option<Diagnostic>, Fault> {
        let span = e.span.unwrap_or_default();
        loop {
            let Some(base) = self.frames.last().map(|f| f.base) else {
                return Ok(Some(e));
            };
            let open = match self.frame_tries() {
                0 => None,
                _ => self.tries.pop(),
            };
            let Some(open) = open else {
                return Ok(Some(e));
            };
            let Some(h) = proto.handlers.get(open.handler) else {
                return Err(broken("a handler past the table", span).into());
            };
            let in_range = (h.pc_start as usize..h.pc_end as usize).contains(&pc);
            let catches = h.handler_pc != h.finally_pc;
            let finally = h.finally_pc != RESERVED_U32;
            match open.stage {
                // Raised in the body, outside the try's range: the body
                // was left without `try.leave`, and the try with it.
                Stage::Body if !in_range => continue,
                Stage::Body if catches && self.interp.exit_requested.is_none() => {
                    let e = self.unwind_burns(open.burns, e);
                    let payload = self.interp.cough_payload.take();
                    if h.catch_reg != RESERVED_U32 {
                        self.put(base, h.catch_reg, error_value(&e, payload), span)?;
                    }
                    let nest = open.nest;
                    let to = h.handler_pc as usize;
                    self.tries.push(Open {
                        stage: Stage::Catch,
                        ..open
                    });
                    if let Some(f) = self.frames.last_mut() {
                        f.pc = to;
                    }
                    self.set_nest(nest);
                    return Ok(None);
                }
                Stage::Body | Stage::Catch if finally => {
                    let e = self.unwind_burns(open.burns, e);
                    let to = self.enter_finally(open, h, Pending::Raise(e));
                    if let Some(f) = self.frames.last_mut() {
                        f.pc = to;
                    }
                    return Ok(None);
                }
                // No catch and no finally, or raised in the finally: its
                // own error replaces what was pending.
                _ => continue,
            }
        }
    }

    /// `call A, B, C` when `R(A)` holds a compiled function of this
    /// program: arity, depth, then a new frame that starts at the caller's
    /// `R(A+1)` ([`Machine::push`]). The callee is tested where it lies;
    /// only its upvalue list, when it has one, is counted once more.
    /// `false` when `R(A)` holds anything else, with nothing changed.
    #[inline]
    fn call_here(
        &mut self,
        program: &Rc<Program>,
        proto: &Proto,
        d: &DecodedInstr,
        base: usize,
    ) -> Result<bool, Fault> {
        let at = base.saturating_add(d.a as usize);
        let Some((index, upvals)) = self.callee(program, proto, at) else {
            return Ok(false);
        };
        let n = d.b_u as usize;
        let target = self.target(program, index, at, n, proto, d.pc)?;
        let caller = (!proto.plain).then(|| base.saturating_add(proto.reg_count));
        let first = at.saturating_add(1);
        self.push(target, index, upvals, first, n, d.c, caller, proto, d.pc)?;
        // The caller goes on after the call; an error from the callee is
        // offered to its tries at the call.
        let k = self.frames.len().saturating_sub(2);
        if let Some(c) = self.frames.get_mut(k) {
            let pc = d.pc as usize;
            c.pc = pc + d.width as usize;
            c.site = pc;
        }
        Ok(true)
    }

    /// What a call needs of `R(at)` when it holds a compiled function of
    /// this program: its index and its upvalue list (`None` when it
    /// captured nothing). A register of a plain frame carries no cell.
    #[inline(always)]
    fn callee(
        &self,
        program: &Rc<Program>,
        proto: &Proto,
        at: usize,
    ) -> Option<(usize, Option<Rc<[Cell]>>)> {
        if !proto.plain {
            if let Some(Some(cell)) = self.cells.get(at) {
                return compiled_parts(&cell.borrow(), program);
            }
        }
        compiled_parts(self.stack.get(at)?, program)
    }

    /// The function a call of `R(at)` enters, made ready at its first
    /// entry, with its arity checked against the call's `n` arguments
    /// (`E401`, worded from the function value).
    #[allow(clippy::too_many_arguments)]
    fn target<'p>(
        &self,
        program: &'p Program,
        index: usize,
        at: usize,
        n: usize,
        proto: &Proto,
        pc: u32,
    ) -> Result<&'p Proto, Fault> {
        let target = match program.funcs.get(index) {
            Some(t) => t,
            None => {
                let span = span_at(proto, pc);
                program.ready(index, span)?;
                program
                    .funcs
                    .get(index)
                    .ok_or_else(|| broken("no such function", span))?
            }
        };
        if n != target.arity {
            return Err(self.arity_error(at, n, span_at(proto, pc)));
        }
        Ok(target)
    }

    /// `E401` for the function in `R(at)` called with `n` arguments.
    #[cold]
    #[inline(never)]
    fn arity_error(&self, at: usize, n: usize, span: Span) -> Fault {
        match self.get_abs(at, span) {
            Ok(Value::Func(f)) => arity_mismatch(&f, n, span).into(),
            Ok(_) => broken("a callee that is not a function", span).into(),
            Err(e) => e,
        }
    }

    /// A new frame of `target` (function `index`, its closure's upvalues
    /// `upvals`) that starts at `first`, the caller's `R(A+1)`: its first
    /// `n` registers are the arguments where they are, nothing is copied,
    /// and the rest of its registers are set to null. `caller` is the end
    /// of the caller's registers when its function makes cells: its cells
    /// from `first` up are gone (an argument's value is read out of its
    /// cell), as `docs/BYTECODE.md` says of what a call leaves undefined.
    /// The depth (`E506`) and the stack's growth (`E514`) are checked
    /// first, at the call's position (`pc` of `proto`).
    #[allow(clippy::too_many_arguments)]
    #[inline]
    fn push(
        &mut self,
        target: &Proto,
        index: usize,
        upvals: Option<Rc<[Cell]>>,
        first: usize,
        n: usize,
        extra: u32,
        caller: Option<usize>,
        proto: &Proto,
        pc: u32,
    ) -> Result<(), Fault> {
        if let Err(e) = self.interp.enter_call(Span::default()) {
            return Err(e.at(span_at(proto, pc)).into());
        }
        let base = first;
        let params = base.saturating_add(n);
        let end = base.saturating_add(target.reg_count);
        if end > self.stack.len() {
            if let Err(e) = self.grow_to(end, span_at(proto, pc)) {
                self.interp.leave_call();
                return Err(e);
            }
        }
        if let Some(caller_end) = caller {
            for i in base..params {
                if let Some(cell) = self.cells.get_mut(i).and_then(Option::take) {
                    if let Some(slot) = self.stack.get_mut(i) {
                        *slot = cell.borrow().clone();
                    }
                }
            }
            self.forget_cells(params, caller_end);
        }
        if let Some(regs) = self.stack.get_mut(params..end) {
            regs.fill(Value::Null);
        }
        let up = match upvals {
            Some(u) => {
                self.upvals.push(u);
                (self.upvals.len() - 1) as u32
            }
            None => NO_UPVALS,
        };
        let (tries, burns) = (self.tries.len(), self.burns.len());
        self.frames
            .push(Frame::new(index, base, extra, up, tries, burns));
        Ok(())
    }

    /// A frame that ends gives up the cells it made.
    fn end_cells(&mut self, program: &Program, f: &Frame) {
        if let Some(p) = program.funcs.get(f.func).filter(|p| !p.plain) {
            self.forget_cells(f.base, f.base.saturating_add(p.reg_count));
        }
    }

    /// `tailcall A, B` when `R(A)` holds a compiled function of this
    /// program: the frame is reused, and keeps holding the depth and loop
    /// nesting of the frame it replaces, so the interpreter's counts are
    /// the ones a plain call would leave. The arguments, `R(A+1..A+1+B)`,
    /// move down to the frame's `R(0..B)` (each register is read before
    /// one below it is written over), and the rest of the new function's
    /// registers are set to null. `false` when `R(A)` holds anything else,
    /// with nothing changed.
    fn tail_here(
        &mut self,
        program: &Rc<Program>,
        proto: &Proto,
        d: &DecodedInstr,
        base: usize,
    ) -> Result<bool, Fault> {
        let at = base.saturating_add(d.a as usize);
        let Some((index, upvals)) = self.callee(program, proto, at) else {
            return Ok(false);
        };
        let n = d.b_u as usize;
        let target = self.target(program, index, at, n, proto, d.pc)?;
        // The replaced function ends: its burns close as a `snuff` leaves them.
        self.close_burns(0, true)?;
        if let Err(e) = self.interp.enter_call(Span::default()) {
            return Err(e.at(span_at(proto, d.pc)).into());
        }
        let Some(frame) = self.frames.last_mut() else {
            return Err(broken("an empty call stack", span_at(proto, d.pc)).into());
        };
        frame.held_calls += 1;
        frame.held_nest = frame.held_nest.saturating_add(frame.nest);
        frame.nest = 0;
        frame.func = index;
        frame.pc = 0;
        let (tries, old) = (frame.tries as usize, frame.up);
        // The replaced function's tries end with it, and its upvalues.
        self.tries.truncate(tries);
        let up = match (old, upvals) {
            (NO_UPVALS, None) => NO_UPVALS,
            (NO_UPVALS, Some(u)) => {
                self.upvals.push(u);
                (self.upvals.len() - 1) as u32
            }
            (old, Some(u)) => {
                if let Some(slot) = self.upvals.get_mut(old as usize) {
                    *slot = u;
                }
                old
            }
            (old, None) => {
                self.upvals.truncate(old as usize);
                NO_UPVALS
            }
        };
        if let Some(frame) = self.frames.last_mut() {
            frame.up = up;
        }
        let first = at.saturating_add(1);
        for k in 0..n {
            let from = first.saturating_add(k);
            let v = if proto.plain {
                // Read once and never again: the frame starts over it.
                match self.stack.get_mut(from) {
                    Some(slot) => std::mem::replace(slot, Value::Null),
                    None => {
                        return Err(broken("a register past its frame", span_at(proto, d.pc)).into())
                    }
                }
            } else {
                self.get_abs(from, span_at(proto, d.pc))?
            };
            if let Some(slot) = self.stack.get_mut(base.saturating_add(k)) {
                *slot = v;
            }
        }
        if !proto.plain {
            self.forget_cells(base, base.saturating_add(proto.reg_count));
        }
        let params = base.saturating_add(n);
        let end = base.saturating_add(target.reg_count);
        if end > self.stack.len() {
            self.grow_to(end, span_at(proto, d.pc))?;
        }
        if let Some(regs) = self.stack.get_mut(params..end) {
            regs.fill(Value::Null);
        }
        Ok(true)
    }

    /// `ret A`: leave the current frame with `R(A)`. `Some(v)` when it was
    /// the machine's first frame: the run is over. An error closing the
    /// frame's burns is raised in the frame, at the `ret`.
    #[inline]
    fn ret_here(
        &mut self,
        proto: &Proto,
        d: &DecodedInstr,
        base: usize,
    ) -> Result<Option<Value>, Fault> {
        // `snuff` out of a burn: each burn is left as completed first, and
        // an error recording one is raised here, in the frame.
        if self.frame_burns() != 0 {
            if let Err(e) = self.close_burns(0, true) {
                if let Some(f) = self.frames.last_mut() {
                    f.pc = d.pc as usize;
                }
                return Err(e);
            }
        }
        let i = base.saturating_add(d.a as usize);
        let carried = match proto.plain {
            true => None,
            false => self
                .cells
                .get(i)
                .and_then(|c| c.as_ref().map(|c| c.borrow().clone())),
        };
        let v = match carried {
            Some(v) => v,
            // The frame ends here, and no register of it is read again
            // before a write: the value is moved out, not copied.
            None => match self.stack.get_mut(i) {
                Some(slot) => std::mem::replace(slot, Value::Null),
                None => {
                    if let Some(f) = self.frames.last_mut() {
                        f.pc = d.pc as usize;
                    }
                    return Err(broken("a register past its frame", span_at(proto, d.pc)).into());
                }
            },
        };
        let Some(frame) = self.frames.pop() else {
            return Err(broken("an empty call stack", span_at(proto, d.pc)).into());
        };
        self.release(&frame);
        if !proto.plain {
            self.forget_cells(frame.base, frame.base.saturating_add(proto.reg_count));
        }
        if self.frames.is_empty() {
            return Ok(Some(v));
        }
        if !self.write_result(frame.base.saturating_sub(1), frame.extra, v) {
            return Err(broken("a result register past its frame", span_at(proto, d.pc)).into());
        }
        Ok(None)
    }

    /// Leave the current frame with `v` (a tail call of what is not a
    /// compiled function). `Some(v)` when it was the entry frame: the run
    /// is over.
    fn ret(&mut self, program: &Program, v: Value, span: Span) -> Result<Option<Value>, Fault> {
        // `snuff` out of a burn: each burn is left as completed first, and
        // an error recording one is raised here, in the frame.
        self.close_burns(0, true)?;
        let frame = self
            .frames
            .pop()
            .ok_or_else(|| broken("an empty call stack", span))?;
        self.release(&frame);
        self.end_cells(program, &frame);
        if self.frames.is_empty() {
            return Ok(Some(v));
        }
        if !self.write_result(frame.base.saturating_sub(1), frame.extra, v) {
            return Err(broken("a result register past its frame", span).into());
        }
        Ok(None)
    }

    /// A call's result: `R(A)` (the absolute register `reg`) is the value;
    /// the rest of the row's result window, `R(A+1..A+C+1)`, is null (a
    /// CigScript call has one result). `false`, with nothing written, when
    /// the row is past the stack.
    #[inline]
    fn write_result(&mut self, reg: usize, extra: u32, v: Value) -> bool {
        let end = reg.saturating_add(extra as usize).saturating_add(1);
        if end > self.stack.len() {
            return false;
        }
        self.store(reg, v);
        for i in reg.saturating_add(1)..end {
            self.store(i, Value::Null);
        }
        true
    }

    /// `R = v` at absolute register `i`, inside the stack: into its cell
    /// when it carries one.
    #[inline(always)]
    fn store(&mut self, i: usize, v: Value) {
        if let Some(Some(cell)) = self.cells.get(i) {
            *cell.borrow_mut() = v;
        } else if let Some(slot) = self.stack.get_mut(i) {
            *slot = v;
        }
    }
}

impl Machine<'_> {
    /// The burn instructions, `pack` and `chain.make`.
    fn burn_op(
        &mut self,
        proto: &Proto,
        d: &DecodedInstr,
        base: usize,
        span: Span,
    ) -> Result<Flow, Fault> {
        let next = d.pc as usize + d.width as usize;
        match d.op {
            Op::BURN_ENTER => {
                let region = proto
                    .regions
                    .get(d.b_u as usize)
                    .ok_or_else(|| broken("a region past the table", span))?;
                let unlit = region.unlit;
                // A computed label is read when the burn is entered, before
                // it opens; its error takes the row of a `nop` right after.
                let label = match &region.label {
                    Label::None => None,
                    Label::Text(s) => Some(s.clone()),
                    Label::Reg(r) => {
                        let v = self.get(base, *r, span)?;
                        let at = part_spans(proto, next, 1, span);
                        Some(burn_label(&v, at.first().copied().unwrap_or(span))?)
                    }
                };
                if self.frames.is_empty() {
                    return Err(broken("an empty call stack", span).into());
                }
                self.burns.push(OpenBurn {
                    unlit,
                    label,
                    span,
                    comp: None,
                });
                self.interp.burn_open(unlit);
            }
            Op::COMP_ENTER => {
                let source = name(proto, d.b_u, span)?.to_string();
                let state_name =
                    Some(name(proto, d.c, span)?.to_string()).filter(|n| !n.is_empty());
                let state = match state_name {
                    Some(_) => Some(self.get(base, d.a, span)?),
                    None => None,
                };
                let open = self.frame_burns() != 0;
                let burn = open
                    .then(|| self.burns.last_mut())
                    .flatten()
                    .ok_or_else(|| misuse(d.op, "found no open burn in its frame", span))?;
                // An unlit burn changes nothing, so the tree-walker records
                // no compensation for one.
                if !burn.unlit {
                    if burn.comp.is_some() {
                        return Err(misuse(
                            d.op,
                            "found its burn's compensation entered already",
                            span,
                        )
                        .into());
                    }
                    burn.comp = Some(Comp {
                        open: true,
                        mark: 0,
                        source,
                        state_name,
                        state,
                        line: span.line,
                    });
                    self.interp.compensate_open();
                }
            }
            Op::COMP_LEAVE => {
                let open = self.frame_burns() != 0;
                let burn = open
                    .then(|| self.burns.last_mut())
                    .flatten()
                    .ok_or_else(|| misuse(d.op, "found no open burn in its frame", span))?;
                if !burn.unlit {
                    let comp = burn.comp.as_mut().filter(|c| c.open).ok_or_else(|| {
                        misuse(d.op, "found no compensation entered in its burn", span)
                    })?;
                    comp.open = false;
                    comp.mark = self.interp.compensate_close();
                }
            }
            Op::BURN_LEAVE => {
                let burn = match self.frame_burns() {
                    0 => None,
                    _ => self.burns.pop(),
                };
                let burn =
                    burn.ok_or_else(|| misuse(d.op, "found no open burn in its frame", span))?;
                self.close_burn(burn, true)?;
            }
            Op::BURN_OP => {
                let n = if d.wide { d.c >> 8 } else { d.c >> 4 };
                let b = proto
                    .effects
                    .get(d.b_u as usize)
                    .copied()
                    .flatten()
                    .ok_or_else(|| broken("a burn.op callee that did not load", span))?;
                let in_own_burn = self.frame_burns() != 0;
                if !in_own_burn && !proto.requires_burn {
                    return Err(outside_burn(b.name, span).into());
                }
                let args = self.window(base, d.a + 1, n, span)?;
                let v = self.interp.call_builtin(b, &args, span)?;
                self.set(base, d.a, v, span)?;
            }
            Op::PACK => {
                self.pack_place(span)?;
                let values = self.window(base, d.a, d.b_u, span)?;
                let at = part_spans(proto, next, values.len(), span);
                let mut roots = Vec::with_capacity(values.len());
                for (v, s) in values.iter().zip(at) {
                    roots.push(pack_root(v, s)?);
                }
                self.interp.declare_pack(roots, span)?;
            }
            Op::CHAIN_MAKE => {
                let window = self.window(base, d.a + 1, d.b_u, span)?;
                let Some((Value::Str(chain), pairs)) = window.split_first() else {
                    return Err(misuse(d.op, "found no chain name in its window", span).into());
                };
                if pairs.len() % 2 != 0 {
                    return Err(misuse(d.op, "found a step without its label", span).into());
                }
                let at = part_spans(proto, next, pairs.len() / 2, span);
                let mut steps = Vec::with_capacity(pairs.len() / 2);
                for (n, (pair, s)) in pairs.chunks_exact(2).zip(at).enumerate() {
                    let (label, value) = match pair {
                        [Value::Str(l), v] => (Some(l.to_string()), v.clone()),
                        [Value::Null, v] => (None, v.clone()),
                        _ => {
                            return Err(misuse(
                                d.op,
                                "found a step label that is not a string",
                                span,
                            )
                            .into())
                        }
                    };
                    steps.push(chain_step(chain, n, label, value, s)?);
                }
                let made = make_chain(chain, steps);
                // C = 1: a `chain` statement of the script's top level,
                // declared in the interpreter's globals as the
                // tree-walker declares it, so `--light` finds it there.
                match d.c {
                    0 => {}
                    1 if matches!(self.frames.as_slice(), [f] if f.func == 0) => {
                        if !self.interp.globals.declare(chain, made.clone(), false) {
                            return Err(crate::diagnostics::check(format!(
                                "`{chain}` is already declared in this scope"
                            ))
                            .code("E306")
                            .at(span)
                            .into());
                        }
                    }
                    _ => {
                        return Err(misuse(
                            d.op,
                            "marks a top-level chain outside the top level",
                            span,
                        )
                        .into())
                    }
                }
                self.set(base, d.a, made, span)?;
            }

            _ => return Err(broken("an instruction sent to the wrong group", span).into()),
        }
        Ok(Flow::Next(next))
    }

    /// `pack` where the checker allows `pack { }`: at the top level, outside
    /// every block, once, before anything burned. Otherwise the checker's
    /// own E753.
    fn pack_place(&self, span: Span) -> Result<(), Fault> {
        use crate::check::{pack_misplaced, PackPlace};
        let top = matches!(self.frames.as_slice(), [f] if f.func == 0) && self.tries.is_empty();
        let kernel = &self.interp.kernel;
        if !top || self.interp.in_burn() {
            return Err(pack_misplaced(PackPlace::InBlock, span).into());
        }
        if kernel.pack().is_some() {
            return Err(pack_misplaced(PackPlace::Second, span).into());
        }
        if !kernel.journal().is_empty() || !kernel.planned.is_empty() {
            return Err(pack_misplaced(PackPlace::NotFirst, span).into());
        }
        Ok(())
    }

    /// Close `b`, the current frame's innermost burn, as the tree-walker's
    /// `burn { }` ends: the compensation's mark, the depths, then (when the
    /// block `completed`) the compensation recorded.
    fn close_burn(&mut self, b: OpenBurn, completed: bool) -> Result<(), Fault> {
        let mark = match &b.comp {
            Some(c) if c.open => self.interp.compensate_close(),
            Some(c) => c.mark,
            None => 0,
        };
        self.interp.burn_close(b.unlit);
        match b.comp {
            Some(c) if completed => self
                .interp
                .record_compensation(
                    crate::burn::journal::Compensation {
                        source: c.source,
                        state_name: c.state_name,
                        state: serde_json::Value::Null,
                        line: c.line,
                        label: b.label,
                        command: None,
                        cwd: None,
                    },
                    c.state.as_ref(),
                    mark,
                    b.span,
                )
                .map_err(Into::into),
            _ => Ok(()),
        }
    }

    /// Leave the current frame's burns down to `keep`, innermost first, as
    /// completed blocks (`snuff`, `break`, `continue`, `try.leave`). The
    /// first error stops it; the burns left open close as it unwinds.
    fn close_burns(&mut self, keep: usize, completed: bool) -> Result<(), Fault> {
        loop {
            let b = match self.frame_burns() > keep {
                true => self.burns.pop(),
                false => None,
            };
            match b {
                Some(b) => self.close_burn(b, completed)?,
                None => return Ok(()),
            }
        }
    }

    /// An error leaves the current frame's burns down to `keep`, innermost
    /// first: completed only when `exit()` was asked for. An error recording
    /// a compensation replaces `e`, as it does in the tree-walker.
    fn unwind_burns(&mut self, keep: usize, mut e: Diagnostic) -> Diagnostic {
        let completed = self.interp.exit_requested.is_some();
        while let Err(e2) = self.close_burns(keep, completed) {
            e = *e2;
        }
        e
    }

    /// `R(r)` of a loop window, which holds an int when the lowering wrote
    /// the loop right.
    fn int_at(&self, base: usize, r: u32, op: Op, span: Span) -> Result<i64, Fault> {
        match self.get(base, r, span)? {
            Value::Int(i) => Ok(i),
            _ => Err(misuse(op, "found a loop window that holds no int", span).into()),
        }
    }

    /// `forprep`'s window `R(A..A+3)` (start, limit, step): the start, the
    /// step and how many turns the loop makes. With step 1 it is `a..b`
    /// exactly: the tree-walker's E403 for bounds that are not ints and its
    /// E512 for more than 10,000,000 numbers. Another step counts its turns
    /// the same way, under the same limit.
    fn for_window(&self, base: usize, a: u32, span: Span) -> Result<(i64, u64), Fault> {
        let start = self.get(base, a, span)?;
        let limit = self.get(base, a + 1, span)?;
        let (Value::Int(start), Value::Int(limit)) = (&start, &limit) else {
            return match binary(BinOp::Range, start, limit, span) {
                Err(e) => Err(e.into()),
                Ok(_) => Err(misuse(Op::FORPREP, "made a range it cannot run", span).into()),
            };
        };
        let step = match self.get(base, a + 2, span)? {
            Value::Int(s) if s != 0 => s,
            _ => {
                return Err(
                    misuse(Op::FORPREP, "found a step that is not a nonzero int", span).into(),
                )
            }
        };
        let (start, limit) = (*start, *limit);
        let width = if step > 0 {
            i128::from(limit) - i128::from(start)
        } else {
            i128::from(start) - i128::from(limit)
        };
        let turns = if width <= 0 {
            0
        } else {
            let s = i128::from(step).abs();
            (width + s - 1) / s
        };
        range_fits(0, i64::try_from(turns).unwrap_or(i64::MAX), span)?;
        Ok((start, u64::try_from(turns).unwrap_or(u64::MAX)))
    }
}

/// The position of part `i` of the instruction ending at `next` (an
/// index's, an object's): the line row of the `i`-th `nop` right after
/// the instruction when the run of `nop`s reaches it, else the
/// instruction's own. [`part_spans`]'s rule, for one part and with
/// nothing allocated.
fn part_span(proto: &Proto, next: usize, i: usize, span: Span) -> Span {
    let all_nops = (0..=i).all(
        |k| matches!(proto.code.get(next.saturating_add(k)), Some(Some(d)) if d.op == Op::NOP),
    );
    if !all_nops {
        return span;
    }
    proto
        .spans
        .get(next.saturating_add(i))
        .copied()
        .unwrap_or(span)
}

/// The position of the instruction at `pc` (the function's first token's
/// when the line table gives none), as the general path takes it.
#[inline]
fn span_of(proto: &Proto, pc: usize) -> Span {
    proto.spans.get(pc).copied().unwrap_or_default()
}

/// `R(start..start+n)` read in place: `None` when a register of it
/// carries a cell (or it runs past the stack), so the caller reads it
/// through the cells.
#[inline]
fn in_place<'s>(
    stack: &'s [Value],
    cells: &[Option<Cell>],
    start: usize,
    n: usize,
) -> Option<&'s [Value]> {
    let end = start.saturating_add(n);
    let plain = cells
        .get(start..end)
        .is_some_and(|c| c.iter().all(Option::is_none));
    if !plain {
        return None;
    }
    stack.get(start..end)
}

/// `interp`'s string: every part written into one buffer in place, a
/// literal piece and a string's text copied once, an int without
/// `core::fmt`, and the value made from the buffer with one copy.
#[inline]
fn interpolate(parts: &[Value]) -> Value {
    crate::value::build_str(|out| {
        out.reserve(parts.iter().map(Value::display_len_hint).sum());
        for v in parts {
            v.write_display(out);
        }
    })
}

/// The positions of the `n` parts of the instruction ending at `next`
/// (`pack`'s roots, `chain.make`'s steps): part `i` takes the line row of
/// the `i`-th `nop` right after the instruction, when the run of `nop`s
/// reaches it; else the instruction's own position.
fn part_spans(proto: &Proto, next: usize, n: usize, span: Span) -> Vec<Span> {
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let pc = next.saturating_add(i);
        let nop = matches!(proto.code.get(pc), Some(Some(d)) if d.op == Op::NOP);
        if !nop {
            break;
        }
        out.push(proto.spans.get(pc).copied().unwrap_or(span));
    }
    out.resize(n, span);
    out
}

/// The string constant `K[i]`: a field or method name (verified a string).
fn name(proto: &Proto, i: u32, span: Span) -> Result<Rc<str>, Fault> {
    match proto.consts.get(i as usize) {
        Some(Value::Str(s)) => Ok(Rc::clone(s)),
        _ => Err(broken("a name that is not a string constant", span).into()),
    }
}

/// The source position of `pc` in `proto`, looked up when an error is
/// built.
#[cold]
#[inline(never)]
fn span_at(proto: &Proto, pc: u32) -> Span {
    proto.spans.get(pc as usize).copied().unwrap_or_default()
}

/// A compiled function of this program in `v`: its index and its upvalue
/// list (`None` when it captured nothing).
#[inline(always)]
fn compiled_parts(v: &Value, program: &Rc<Program>) -> Option<(usize, Option<Rc<[Cell]>>)> {
    let (_, cf) = compiled(v, program)?;
    Some((
        cf.index,
        (!cf.upvals.is_empty()).then(|| Rc::clone(&cf.upvals)),
    ))
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

fn konst(proto: &Proto, i: u32, span: Span) -> Result<Value, Fault> {
    proto
        .consts
        .get(i as usize)
        .cloned()
        .ok_or_else(|| broken("a constant past the pool", span).into())
}

/// The jump rule of `docs/BYTECODE.md`: `target = pc + 1 + offset`, `pc`
/// the first-word pc.
fn jump(pc: usize, offset: i32, span: Span) -> Result<usize, Fault> {
    let target = (pc as i64) + 1 + i64::from(offset);
    usize::try_from(target).map_err(|_| broken("a jump before the first instruction", span).into())
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
fn operate(op: BinOp, l: Value, r: Value, span: Span) -> Result<Value, Fault> {
    match fast_binary(op, &l, &r) {
        Some(v) => Ok(v),
        None => binary(op, l, r, span).map_err(Into::into),
    }
}

/// A jump's target in the fast path: `None` (the general path raises the
/// error) before the first instruction.
#[inline(always)]
fn fast_jump(pc: usize, offset: i32) -> Option<usize> {
    usize::try_from((pc as i64) + 1 + i64::from(offset)).ok()
}

/// [`fast_binary`], and the comparisons `binary` gives for other operands
/// without an error: `==` and `!=` of any two values (`equals`, as
/// `binary` computes them) and `<`, `<=` of two strings.
#[inline(always)]
fn fast_compare(op: BinOp, l: &Value, r: &Value) -> Option<Value> {
    if let Some(v) = fast_binary(op, l, r) {
        return Some(v);
    }
    match (op, l, r) {
        (BinOp::Eq, _, _) => Some(Value::Bool(l.equals(r))),
        (BinOp::Ne, _, _) => Some(Value::Bool(!l.equals(r))),
        (BinOp::Lt, Value::Str(a), Value::Str(b)) => Some(Value::Bool(a < b)),
        (BinOp::Le, Value::Str(a), Value::Str(b)) => Some(Value::Bool(a <= b)),
        _ => None,
    }
}

/// The position of `name` in `map`: the cached one when the key there is
/// still `name`, else a lookup, which moves the cache (the tree-walker's
/// field cache, the same rule).
#[inline(always)]
fn field_index(
    map: &IndexMap<String, Value>,
    name: &str,
    hint: &std::cell::Cell<u32>,
) -> Option<usize> {
    let guess = hint.get() as usize;
    if let Some((k, _)) = map.get_index(guess) {
        if k == name {
            return Some(guess);
        }
    }
    let i = map.get_index_of(name)?;
    hint.set(u32::try_from(i).unwrap_or(0));
    Some(i)
}

/// The pc after the instruction ending at `next` and the run of at most
/// `max` `nop`s right after it, which carry its parts' positions and do
/// nothing when run.
#[inline(always)]
fn past_nops(proto: &Proto, next: usize, max: usize) -> usize {
    let mut pc = next;
    while pc < next + max && matches!(proto.code.get(pc), Some(Some(d)) if d.op == Op::NOP) {
        pc += 1;
    }
    pc
}
