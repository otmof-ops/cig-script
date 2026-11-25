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
use crate::shape::{self, Entries};
use crate::syntax::ast::BinOp;
use crate::syntax::span::Span;
use crate::value::{Builtin, FnBody, Function, Value};
use cig_bytecode::opcode::Fmt;
use cig_bytecode::{
    decode_table, verify_function, verify_upvalues, Constant, DecodedInstr, FuncKind,
    Function as BcFunction, HandlerEntry, LineEntry, Op, ParseError, Reader, RegionLabel, SpanBase,
    UpvalDesc, UpvalKind, Verified, VerifyError, RESERVED_U32,
};
use indexmap::IndexMap;
use std::cell::{OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

mod native;
#[cfg(test)]
mod tests;

use native::{has_body_room, BodyEntry, Has};
pub use native::{stack_starts_here, Native};

use cig_native::{Context as NativeCtx, Host, Stop as NativeStop};

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
    /// The module's integrity hash, when it was opened from its bytes
    /// ([`open`]): what native code compiled from it names.
    hash: Option<[u8; 32]>,
    /// A stage `c` program's native code, loaded the first time a call
    /// could use it: `Some(None)` once it is known there is none this
    /// run can use.
    native: OnceCell<Option<Native>>,
    /// Where that code lies, until it is loaded.
    native_source: std::cell::Cell<Option<crate::native::Source>>,
    /// The native code has a body for some function: the VM looks for
    /// one when a frame comes on top. `false` keeps a program without
    /// bodies from loading its section before a call needs it.
    has_bodies: std::cell::Cell<bool>,
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
    /// What the program's native code has for this function: an entry,
    /// a body, both or neither, and both while the program has native
    /// code it has not loaded ([`Has::UNKNOWN`]). Kept as the code is
    /// loaded ([`Program::stamp`]), so a call and a frame switch read
    /// it here and ask the native code only about a function it has
    /// something for.
    native: std::cell::Cell<Has>,
    /// Per pc, the field cache of a `getfield` or `setfield` there: the
    /// position its key last had in the map, as the tree-walker keeps one
    /// per field expression. At a `callm`, the method cache: the
    /// receiver's type and the builtin method it resolved to
    /// ([`crate::stdlib::method_slot`]; 0 before the first call).
    hints: Vec<std::cell::Cell<u32>>,
    /// Per pc, what a `getfield` or `setfield` there last found of a map
    /// that has a shape: the shape in the high half, the field's place
    /// in the low ([`shaped`]). True of every map of that shape for the
    /// whole run, so it is never taken back; zero (no shape) until then.
    /// Native code reads these cells where they lie.
    sites: Vec<std::cell::Cell<u64>>,
    reg_count: usize,
    arity: usize,
    /// The decoded instruction at each first-word pc; `None` at the second
    /// word of a `WIDE` pair, which nothing may jump to.
    code: Vec<Option<DecodedInstr>>,
    /// Each one-word instruction's word, by pc: what the fast loop
    /// reads ([`Word`]).
    words: Vec<Word>,
    /// The same words with two `steps` in a row counted in one
    /// ([`Word::paired`]): what the fast loop of a function without a
    /// body reads. A loop's first statement makes such a pair at every
    /// turn.
    paired: Vec<Word>,
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

impl Proto {
    /// The function's instructions as a fast loop reads them.
    #[inline(always)]
    fn lines(&self) -> Lines<'_> {
        Lines {
            words: &self.words,
            code: &self.code,
        }
    }

    /// The function's instructions as the fast loop of a function
    /// without a body reads them: [`Proto::lines`] with each pair of
    /// `steps` counted at its first. A loop that stops between two
    /// instructions it is told of (a body's resume point, the end of a
    /// range a body's `run` names) reads [`Proto::lines`], where every
    /// instruction is one.
    #[inline(always)]
    fn paired_lines(&self) -> Lines<'_> {
        Lines {
            words: &self.paired,
            code: &self.code,
        }
    }
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
    load_as(v, None)
}

/// [`load`], for a module parsed from bytes whose header carries `hash`
/// (`cig_bytecode::module_hash`, which `parse_module` checked): the
/// program can then take native code compiled from that module, as one
/// [`open`] made can.
pub fn load_hashed(v: Verified<'_>, hash: [u8; 32]) -> Result<Rc<Program>, Diagnostic> {
    load_as(v, Some(hash))
}

fn load_as(v: Verified<'_>, hash: Option<[u8; 32]>) -> Result<Rc<Program>, Diagnostic> {
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
        hash,
        native: OnceCell::new(),
        native_source: std::cell::Cell::new(None),
        has_bodies: std::cell::Cell::new(false),
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
        hash: Some(reader.hash()),
        reader: Some(reader),
        pow: known.pow(),
        known,
        native: OnceCell::new(),
        native_source: std::cell::Cell::new(None),
        has_bodies: std::cell::Cell::new(false),
    });
    program.ready(0, Span::default())?;
    Ok(program)
}

impl Program {
    /// Give a stage `c` program's module where its native code lies. The
    /// code is loaded the first time the run calls one of the script's
    /// functions; a run that calls none never reads it.
    pub fn offer_native(&self, source: crate::native::Source) {
        // One read of the section's footer: a program with no body
        // loads its native code at its first call, as before, and one
        // with a body before its first frame runs.
        self.has_bodies.set(source.bodies() > 0);
        self.native_source.set(Some(source));
        self.stamp();
    }

    /// What the native code has for function `index`, as far as it is
    /// known now: the loaded code's answer; neither, with no code to
    /// load; not known, with code offered and not loaded yet.
    fn has_now(&self, index: usize) -> Has {
        match self.native.get() {
            Some(Some(native)) => native.has(index),
            Some(None) => Has::NEITHER,
            None => {
                let source = self.native_source.take();
                let offered = source.is_some();
                self.native_source.set(source);
                match offered {
                    true => Has::UNKNOWN,
                    false => Has::NEITHER,
                }
            }
        }
    }

    /// Tell every loaded function what the native code has for it
    /// ([`Proto::native`]): when code is offered, and when it is
    /// loaded. A function loaded later is told as it is loaded
    /// ([`Program::ready`]).
    fn stamp(&self) {
        for (index, slot) in self.funcs.0.iter().enumerate() {
            if let Some(proto) = slot.get() {
                proto.native.set(self.has_now(index));
            }
        }
    }

    /// Load the offered native code now, as `CIG_VALIDATE=1` does before
    /// anything runs. `Err` when the section is damaged; a section this
    /// machine cannot load is not an error, and the program runs its
    /// bytecode.
    pub fn load_native(&self) -> Result<(), crate::native::Unused> {
        let loaded = self.native_from_source();
        let damaged = match &loaded {
            Err(e @ crate::native::Unused::Damaged(_)) => Err(e.clone()),
            Ok(Some(native)) => self.check_bodies(native),
            _ => Ok(()),
        };
        // A section with a body that cannot be entered is not used.
        let loaded = match &damaged {
            Ok(()) => loaded,
            Err(e) => Err(e.clone()),
        };
        // A second load finds it loaded, or finds no source.
        let _ = self.native.set(loaded.ok().flatten());
        self.stamp();
        damaged
    }

    /// The native code from the offered source: `None` with nothing
    /// offered, or for a module that was not opened from its bytes.
    fn native_from_source(&self) -> Result<Option<Native>, crate::native::Unused> {
        let (Some(source), Some(hash)) = (self.native_source.take(), self.hash) else {
            return Ok(None);
        };
        let arity = |i: u32| self.heads.get(i as usize).map(|h| h.arity);
        let (code, tables) = source.load(&hash, arity)?;
        Ok(Some(Native::new(code, tables, self.heads.len())))
    }

    /// Every body of `native` can be entered: its function loads, and
    /// each of its resume points starts an instruction of it. What a
    /// run finds out one function at a time (a body that cannot be
    /// entered is not used), `CIG_VALIDATE=1` finds out before anything
    /// runs.
    fn check_bodies(&self, native: &Native) -> Result<(), crate::native::Unused> {
        for func in native.with_bodies() {
            let damaged = |what: String| crate::native::Unused::Damaged(what);
            self.ready(func, Span::default()).map_err(|d| {
                damaged(format!(
                    "the body of fn{func}: the function does not load: {}",
                    d.message
                ))
            })?;
            let entered = self
                .funcs
                .get(func)
                .and_then(|proto| native.body_of(func, &proto.code));
            if entered.is_none() {
                return Err(damaged(format!(
                    "the body of fn{func} has a resume point that starts no instruction of it"
                )));
            }
        }
        Ok(())
    }

    /// The program's native code, loaded at the first call that asks.
    #[inline(always)]
    fn native(&self) -> Option<&Native> {
        match self.native.get() {
            Some(n) => n.as_ref(),
            None => self.first_native(),
        }
    }

    /// For tests only: give every function a body in plain Rust that
    /// has the interpreter run everything, with every instruction a
    /// resume point ([`Native::for_tests_every_pc`]). A run of the
    /// program then goes through the driver at every instruction:
    /// entering, `run`, leaving, resuming, raising and unwinding.
    #[cfg(test)]
    pub(crate) fn attach_test_bodies_for_every_pc(&self) {
        let _ = self
            .native
            .set(Some(Native::for_tests_every_pc(self.heads.len())));
        self.has_bodies.set(true);
        self.stamp();
    }

    /// For tests only: [`Program::attach_test_bodies_for_every_pc`]
    /// for the functions `with` names alone. The others have no body:
    /// the VM's own loop runs them, in a program that has bodies.
    #[cfg(test)]
    pub(crate) fn attach_test_bodies_to(&self, with: &[usize]) {
        let _ = self
            .native
            .set(Some(Native::for_tests_every_pc_of(self.heads.len(), with)));
        self.has_bodies.set(true);
        self.stamp();
    }

    #[cold]
    #[inline(never)]
    fn first_native(&self) -> Option<&Native> {
        let native = self
            .native
            .get_or_init(|| self.native_from_source().ok().flatten());
        self.stamp();
        native.as_ref()
    }

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
                proto.native.set(self.has_now(index));
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

/// The runtime side of the helpers [`cig_native::Helpers`] (Interface
/// 2.3): the VM gives a body one of these, borrowing the machine, and
/// the body calls `run` and `list` through it.
///
/// `run(ctx, from, to)` runs the frame's bytecode from pc `from` while
/// the pc stays in `from..to`, with the VM's own code: its fast loop,
/// and the general path for what that does not run. A `call` of a
/// compiled function of this program is carried through to its return
/// ([`BodyHost::call_through`]): the VM's loop runs the callee on the
/// frames above this one, entering its body where it has one. `run`
/// stops `At` a `ret` and a `tailcall`, `At` such a `call` where the
/// stack has no room to carry it (the VM's own loop then makes it), and
/// `Raised(pc)` on an error.
/// `list(ctx, reg)` gives a view on a list register that carries no
/// cell and is not borrowed. `push(ctx, list, value, dst)` is
/// `list.push(value)` with the list in `dst` after it, for registers
/// that carry no cell and a list nothing is walking. `copy(ctx, dst,
/// src)` is `mov` and `clear(ctx, reg)` sets a register to null, both
/// for registers that carry no cell: where native code would have to
/// share or free a value that owns memory, which is the runtime's.
struct BodyHost<'a, 'i, 'p> {
    m: &'a mut Machine<'i, Bodied>,
    program: &'p Rc<Program>,
    proto: &'p Proto,
    base: usize,
    /// The body that is running, and the resume point it was entered
    /// at: where its failed guesses are counted.
    body: &'p BodyEntry,
    at: u32,
    /// The body is entered whatever its guesses do
    /// ([`Native::patient`]).
    patient: bool,
    /// What was last given to the body as `ctx.steps_left`: the host
    /// counts the difference since, so one mark is never counted twice.
    last_given: i64,
    /// An error the VM raised inside `run`; `Code::enter` carries only
    /// `Stop::Raised(pc)` back, so the error travels through the host.
    raised: Option<Fault>,
    /// The error was raised by `quick`, which takes nothing from the
    /// context: the steps the body counted up to the instruction are
    /// still in it when the body returns.
    quick_raised: bool,
}

impl BodyHost<'_, '_, '_> {
    /// Count what the body took from the budget since the host last
    /// gave it, and set the frame's loop nesting from `ctx.nest`. The
    /// "last given" mark is then moved to the current `ctx.steps_left`,
    /// so a later `run` return sees only the body's work since here.
    #[inline]
    fn absorb_ctx(&mut self, ctx: &NativeCtx) {
        let took = self.last_given.saturating_sub(ctx.steps_left);
        if took > 0 {
            self.m.interp.steps = self.m.interp.steps.saturating_add(took.max(0) as u64);
        }
        self.last_given = ctx.steps_left;
        self.m.set_nest(ctx.nest);
    }

    /// Give the body a fresh view of the context: `steps_left` set to
    /// what the interpreter has left, `nest` to the frame's, and
    /// `frame` to where the frame's `R(0)` is right now (the register
    /// stack may have grown).
    #[inline]
    fn refresh_ctx(&mut self, ctx: &mut NativeCtx) {
        let budget = native::budget_for(self.m.interp);
        ctx.steps_left = budget;
        self.last_given = budget;
        ctx.nest = self.m.frames.last().map_or(0, |f| f.nest);
        if let Some(ptr) = frame_r0(&mut self.m.stack, self.base) {
            ctx.frame = ptr;
        }
    }

    /// `R(A)` at `d.a` holds a compiled function of this program.
    #[inline]
    fn callee_is_this_program(&self, d: &DecodedInstr) -> bool {
        let at = self.base.saturating_add(d.a as usize);
        if !self.proto.plain {
            if let Some(Some(cell)) = self.m.cells.get(at) {
                return compiled_parts(&cell.borrow(), self.program).is_some();
            }
        }
        self.m
            .stack
            .get(at)
            .and_then(|v| compiled_parts(v, self.program))
            .is_some()
    }

    /// A call may be carried through to its return under this body: the
    /// stack has room for the VM's loop once more, and for the body it
    /// may enter.
    #[inline]
    fn may_carry(&self) -> bool {
        #[cfg(test)]
        if native::LEAVE_AT_CALLS.with(std::cell::Cell::get) {
            return false;
        }
        has_body_room()
    }

    /// Run the `call` `d` of this body's frame, whose callee is a
    /// compiled function of this program, to its return: what the VM's
    /// loop does at that instruction ([`Machine::call_here`]: the arity,
    /// the depth, the stack's ceiling, a typed entry or a new frame),
    /// and then that loop itself on the frames above this one until the
    /// callee has returned. Its result is in the call's `R(A)` then, and
    /// this frame is on top again, at the instruction after the call.
    ///
    /// An error is the callee's to catch first, in any frame above this
    /// one, exactly as the VM's loop offers it. One that none of them
    /// takes leaves those frames and comes back here, raised at the
    /// call: the body returns it, and the frame it runs on is offered
    /// the error there, as it is at stage `b`.
    fn call_through(&mut self, d: &DecodedInstr) -> Result<(), Fault> {
        #[cfg(test)]
        native::CARRIED.with(|n| n.set(n.get() + 1));
        let floor = self.m.frames.len();
        // Where an error the call itself raises is offered to the tries.
        if let Some(f) = self.m.frames.last_mut() {
            f.pc = d.pc as usize;
        }
        // The ordinary call is made as the fast loop makes it; any
        // other by the instruction's own code, with its errors.
        let made =
            self.proto.plain && quick_call(&mut *self.m, self.program, self.base, d).is_some();
        if !made && !self.m.call_here(self.program, self.proto, d, self.base)? {
            // `run` looked before it asked: the register holds a
            // compiled function of this program.
            return Err(Box::new(
                internal("a body's `run` carried a call of what is not a function".to_string())
                    .at(span_at(self.proto, d.pc)),
            ));
        }
        // A typed entry has returned already, with no frame made.
        while self.m.frames.len() > floor {
            match self.m.run_frame_code(self.program, floor) {
                // A return brought the stack back down to this frame.
                Ok(None) => break,
                // Only a run's first frame returns out of the loop, and
                // that one is below every body.
                Ok(Some(_)) => {
                    return Err(Box::new(
                        internal("a call under a body returned out of the run".to_string())
                            .at(span_at(self.proto, d.pc)),
                    ));
                }
                Err(e) => self.m.unwind_above(self.program, *e, floor)?,
            }
        }
        Ok(())
    }
}

impl BodyHost<'_, '_, '_> {
    /// Register `r` of the body's frame, to write.
    #[inline(always)]
    fn slot(&mut self, r: u8) -> Option<&mut Value> {
        self.m.stack.get_mut(self.base.checked_add(usize::from(r))?)
    }

    /// Registers `dst` and `src` of the body's frame, the first to
    /// write and the second to read. None when they are one register:
    /// that is the fast path's own function's to run.
    #[inline(always)]
    fn two(&mut self, dst: u8, src: u8) -> Option<(&mut Value, &Value)> {
        let d = self.base.checked_add(usize::from(dst))?;
        let s = self.base.checked_add(usize::from(src))?;
        match d.cmp(&s) {
            std::cmp::Ordering::Less => {
                let (low, high) = self.m.stack.split_at_mut_checked(s)?;
                Some((low.get_mut(d)?, high.first()?))
            }
            std::cmp::Ordering::Greater => {
                let (low, high) = self.m.stack.split_at_mut_checked(d)?;
                Some((high.first_mut()?, low.get(s)?))
            }
            std::cmp::Ordering::Equal => None,
        }
    }

    /// `getup A, B` and `cellget A, B`, as the fast path runs them.
    #[inline(always)]
    fn quick_upvalue(&mut self, w: Word) -> Option<()> {
        let up = self.m.frames.last()?.up;
        if up == NO_UPVALS {
            return None;
        }
        let slot = self
            .m
            .stack
            .get_mut(self.base.checked_add(usize::from(w.a))?)?;
        let cell = self.m.upvals.get(up as usize)?.get(usize::from(w.b))?;
        copy_into(slot, &*cell.try_borrow().ok()?);
        Some(())
    }

    /// `getfield A, B, K`, as the fast path runs it.
    #[inline(always)]
    fn quick_getfield(&mut self, at: usize, w: Word) -> Option<()> {
        let proto = self.proto;
        let (slot, object) = self.two(w.a, w.b)?;
        let Value::Map(map) = object else {
            return None;
        };
        let map = map.try_borrow().ok()?;
        let site = proto.sites.get(at)?;
        let i = match shaped(&map, site) {
            Some(i) => i,
            None => {
                let Value::Str(name) = proto.consts.get(usize::from(w.c))? else {
                    return None;
                };
                let i = field_index(&map, name, proto.hints.get(at)?)?;
                learn(&map, site, i);
                i
            }
        };
        copy_into(slot, map.get_index(i)?.1);
        Some(())
    }

    /// `setfield A, K, C` for a field the map has, as the fast path
    /// runs it.
    #[inline(always)]
    fn quick_setfield(&mut self, at: usize, w: Word) -> Option<()> {
        let proto = self.proto;
        let regs = self.m.stack.get(self.base..)?;
        let Value::Map(map) = regs.get(usize::from(w.a))? else {
            return None;
        };
        let value = regs.get(usize::from(w.c))?;
        let mut map = map.try_borrow_mut().ok()?;
        let site = proto.sites.get(at)?;
        let i = match shaped(&map, site) {
            Some(i) => i,
            None => {
                let Value::Str(name) = proto.consts.get(usize::from(w.b))? else {
                    return None;
                };
                let i = field_index(&map, name, proto.hints.get(at)?)?;
                learn(&map, site, i);
                i
            }
        };
        copy_into(map.get_index_mut(i)?.1, value);
        Some(())
    }

    /// `getindex A, B, C` for a list and an int it has an element at,
    /// as the fast path runs it.
    #[inline(always)]
    fn quick_getindex(&mut self, w: Word) -> Option<()> {
        let Value::Int(i) = self.m.stack.get(self.base.checked_add(usize::from(w.c))?)? else {
            return None;
        };
        let at = usize::try_from(*i).ok()?;
        let (slot, object) = self.two(w.a, w.b)?;
        let Value::List(list) = object else {
            return None;
        };
        copy_into(slot, list.try_borrow().ok()?.get(at)?);
        Some(())
    }

    /// `loadk A, Bx`, as the fast path runs it.
    #[inline(always)]
    fn quick_constant(&mut self, w: Word) -> Option<()> {
        let proto = self.proto;
        copy_into(self.slot(w.a)?, proto.consts.get(Operands::bx(w) as usize)?);
        Some(())
    }

    /// [`Host::quick`] for an instruction of the fast path's second
    /// tier alone (a builtin or a method called, a string built, a list
    /// or a map made or added to): that tier's function, without the
    /// first tier's in front of it.
    fn quick_second(&mut self, at: usize) -> u32 {
        let Some(Some(d)) = self.proto.code.get(at) else {
            return 0;
        };
        let Some(regs) = self.m.stack.get_mut(self.base..) else {
            return 0;
        };
        match fast_call(regs, self.m.interp, self.proto, d) {
            Some(Ok(_)) => 1,
            Some(Err(e)) => {
                if let Some(frame) = self.m.frames.last_mut() {
                    frame.pc = at;
                }
                self.raised = Some(e);
                self.quick_raised = true;
                2
            }
            None => 0,
        }
    }

    /// [`Host::quick`] for every other instruction, and for the
    /// uncommon cases of those above: the fast path's own function,
    /// both its tiers. Out of line: its frame is large, and the arms
    /// above must not pay for it.
    #[inline(never)]
    fn quick_rest(&mut self, at: usize) -> u32 {
        let Some(Some(d)) = self.proto.code.get(at) else {
            return 0;
        };
        if !quick_runs(d.op) {
            return 0;
        }
        let Machine {
            interp,
            stack,
            frames,
            upvals,
            ..
        } = &mut *self.m;
        let interp: &mut Interp = interp;
        // The frame on top is the body's: a call it carried has
        // returned by the time it asks for anything.
        let Some(frame) = frames.last_mut() else {
            return 0;
        };
        let ups: &[Cell] = if frame.up == NO_UPVALS {
            &[]
        } else {
            upvals.get(frame.up as usize).map_or(&[], |u| u)
        };
        let Some(regs) = stack.get_mut(self.base..) else {
            return 0;
        };
        match fast_one(regs, interp, frame, self.proto, d, ups) {
            Some(Ok(_)) => 1,
            Some(Err(e)) => {
                // As the fast loop leaves it: the frame's pc is where
                // the error is offered to the tries.
                frame.pc = at;
                self.raised = Some(e);
                self.quick_raised = true;
                2
            }
            None => 0,
        }
    }
}

/// `*slot = value.clone()`, with each kind's copy written straight into
/// the slot: its tag and its payload, and for a value that owns memory
/// its count taken first.
///
/// A copy made as a value and then moved in is built on the stack a
/// field at a time and read back whole, sixteen bytes at once, and a
/// processor cannot forward two narrow stores to one wide load: it
/// waits for them to reach the cache, some fifteen cycles, at every
/// copy. Written here in place there is no such load.
#[inline(always)]
fn copy_into(slot: &mut Value, value: &Value) {
    match value {
        Value::Null => overwrite(slot, Value::Null),
        Value::Bool(b) => overwrite(slot, Value::Bool(*b)),
        Value::Int(n) => overwrite(slot, Value::Int(*n)),
        Value::Float(x) => overwrite(slot, Value::Float(*x)),
        Value::Str(s) => overwrite(slot, Value::Str(Rc::clone(s))),
        Value::List(l) => overwrite(slot, Value::List(Rc::clone(l))),
        Value::Map(m) => overwrite(slot, Value::Map(Rc::clone(m))),
        Value::Func(f) => overwrite(slot, Value::Func(Rc::clone(f))),
        Value::Builtin(b) => overwrite(slot, Value::Builtin(Rc::clone(b))),
        Value::Module(m) => overwrite(slot, Value::Module(Rc::clone(m))),
        Value::Chain(c) => overwrite(slot, Value::Chain(Rc::clone(c))),
    }
}

impl Host for BodyHost<'_, '_, '_> {
    fn run(&mut self, ctx: &mut NativeCtx, from: u32, to: u32) -> NativeStop {
        // 2.3: count what the body took, and sync the frame's nesting.
        self.absorb_ctx(ctx);
        let (from, to) = (from as usize, to as usize);
        let mut pc = from;
        loop {
            // The pc left the range, by its end or back past its start:
            // stop At the new pc. What lies outside the range is the
            // body's to run, or to hand over again.
            if pc < from || pc >= to {
                break;
            }
            let Some(Some(d)) = self.proto.code.get(pc) else {
                // Past the function, a wide's second word, or an invalid
                // pc: stop At the pc, the driver sees it.
                break;
            };
            match d.op {
                // What ends the frame is the VM's own loop's to run.
                Op::RET | Op::TAILCALL => break,
                // A call of a compiled function of this program is
                // carried through to its return, on frames above this
                // one. Where the stack has no room for the VM's loop
                // once more, `run` stops At the call instead, and the
                // VM's own loop makes it: the body is told either way.
                Op::CALL if self.callee_is_this_program(d) => {
                    if !self.may_carry() {
                        break;
                    }
                    match self.call_through(d) {
                        Ok(()) => {
                            pc = pc.saturating_add(d.width as usize);
                            continue;
                        }
                        Err(e) => {
                            self.raised = Some(e);
                            return NativeStop::Raised(pc as u32);
                        }
                    }
                }
                _ => {}
            }
            // The VM's fast loop first, as in a function without a body:
            // it comes back at the first instruction it does not run, or
            // where the pc leaves the range.
            if self.proto.plain {
                match self.m.run_fast_inside(self.proto, pc, self.base, from, to) {
                    // It went on: the range and the instruction it
                    // stands at are looked at again.
                    Ok(next) if next != pc => {
                        pc = next;
                        continue;
                    }
                    // It does not run this one: the general path does.
                    Ok(_) => {}
                    Err(e) => {
                        // The loop left the frame's pc at the instruction.
                        let at = self.m.frames.last().map_or(pc, |f| f.pc);
                        self.raised = Some(e);
                        return NativeStop::Raised(u32::try_from(at).unwrap_or(u32::MAX));
                    }
                }
            }
            // The frame's pc is where an error raised by this
            // instruction is offered to the tries.
            if let Some(f) = self.m.frames.last_mut() {
                f.pc = pc;
            }
            let span = self.proto.spans.get(pc).copied().unwrap_or_default();
            let flow = self
                .m
                .step_for_body(self.program, self.proto, d, self.base, span);
            match flow {
                Ok(Flow::Next(next)) => pc = next,
                Ok(Flow::Return(_)) | Ok(Flow::Switched) => {
                    // 2.3: `run` runs no instruction that can do this.
                    // That it did is an internal error, kept and raised.
                    self.raised = Some(Box::new(
                        internal(format!(
                            "`run` saw a flow out of an instruction it does not run (pc {pc})"
                        ))
                        .at(span),
                    ));
                    return NativeStop::Raised(pc as u32);
                }
                Err(e) => {
                    self.raised = Some(e);
                    return NativeStop::Raised(pc as u32);
                }
            }
        }
        // 2.3: write steps_left, nest and frame to the context again.
        self.refresh_ctx(ctx);
        NativeStop::At(pc as u32)
    }

    fn again(&mut self, ctx: &mut NativeCtx, pc: u32) -> NativeStop {
        let at = pc as usize;
        let Some(Some(d)) = self.proto.code.get(at) else {
            return NativeStop::At(pc);
        };
        // `At(pc)` says "nothing ran", so an instruction that can go on
        // at its own pc is never run here: the VM's loop runs it.
        if jump_target(d) == Some(at) {
            self.absorb_ctx(ctx);
            self.refresh_ctx(ctx);
            return NativeStop::At(pc);
        }
        if !self.patient {
            self.absorb_ctx(ctx);
            if self.body.missed(self.at, self.m.interp.steps) {
                // The guesses of this body keep failing: the bytecode
                // has the frame back, and the body is not entered at
                // that resume point again.
                self.refresh_ctx(ctx);
                return NativeStop::At(pc);
            }
        }
        self.run(ctx, pc, pc.saturating_add(d.width))
    }

    fn list(&mut self, ctx: &mut NativeCtx, reg: u32) -> bool {
        let i = self.base.saturating_add(reg as usize);
        // Must carry no cell.
        if let Some(Some(_)) = self.m.cells.get(i) {
            return false;
        }
        let Some(Value::List(list)) = self.m.stack.get(i) else {
            return false;
        };
        // try_borrow_mut: refuse a list that is already borrowed.
        let mut v = match list.try_borrow_mut() {
            Ok(v) => v,
            Err(_) => return false,
        };
        let ptr = v.as_mut_ptr() as u64;
        let len = v.len() as u64;
        drop(v);
        ctx.left = ptr;
        ctx.right = len;
        true
    }

    fn push(&mut self, _ctx: &mut NativeCtx, list: u32, value: u32, dst: u32) -> bool {
        let at = |r: u32| self.base.saturating_add(r as usize);
        let (list, value, dst) = (at(list), at(value), at(dst));
        // A register that carries a cell is read and written through
        // it: the interpreter's.
        if !self.proto.plain
            && [list, value, dst]
                .iter()
                .any(|i| matches!(self.m.cells.get(*i), Some(Some(_))))
        {
            return false;
        }
        let Some(Value::List(items)) = self.m.stack.get(list) else {
            return false;
        };
        let items = Rc::clone(items);
        let Some(v) = self.m.stack.get(value).cloned() else {
            return false;
        };
        if dst >= self.m.stack.len() {
            return false;
        }
        // What the list's own `push` does with one value. A list
        // something is walking is the interpreter's to answer for.
        match items.try_borrow_mut() {
            Ok(mut elements) => elements.push(v),
            Err(_) => return false,
        }
        // `list.push(v)` is the list: the call's register holds it.
        if let Some(slot) = self.m.stack.get_mut(dst) {
            *slot = Value::List(items);
        }
        true
    }

    fn copy(&mut self, _ctx: &mut NativeCtx, dst: u32, src: u32) -> bool {
        let (dst, src) = (
            self.base.saturating_add(dst as usize),
            self.base.saturating_add(src as usize),
        );
        if !self.proto.plain
            && [dst, src]
                .iter()
                .any(|i| matches!(self.m.cells.get(*i), Some(Some(_))))
        {
            return false;
        }
        let Some(v) = self.m.stack.get(src).cloned() else {
            return false;
        };
        match self.m.stack.get_mut(dst) {
            Some(slot) => {
                *slot = v;
                true
            }
            None => false,
        }
    }

    fn clear(&mut self, _ctx: &mut NativeCtx, reg: u32) -> bool {
        let reg = self.base.saturating_add(reg as usize);
        if !self.proto.plain && matches!(self.m.cells.get(reg), Some(Some(_))) {
            return false;
        }
        match self.m.stack.get_mut(reg) {
            Some(slot) => {
                *slot = Value::Null;
                true
            }
            None => false,
        }
    }

    fn fields(&mut self, ctx: &mut NativeCtx, reg: u32) -> bool {
        let i = self.base.saturating_add(reg as usize);
        // Must carry no cell.
        if let Some(Some(_)) = self.m.cells.get(i) {
            return false;
        }
        let Some(Value::Map(map)) = self.m.stack.get(i) else {
            return false;
        };
        // try_borrow_mut: refuse a map that is already borrowed.
        let Ok(mut entries) = map.try_borrow_mut() else {
            return false;
        };
        let shape = entries.shape();
        if shape == shape::NONE {
            return false;
        }
        // The values lie in one block, each entry after the last: the
        // first one's address and the distance to the second place
        // every one of them.
        let mut values = entries.values_mut();
        let Some(first) = values.next() else {
            return false;
        };
        let first = first as *mut Value as u64;
        let step = values.next().map_or(0, |second| {
            (second as *mut Value as u64).wrapping_sub(first)
        });
        let Ok(step) = u32::try_from(step) else {
            return false;
        };
        ctx.left = first;
        ctx.right = u64::from(shape) | u64::from(step) << 32;
        true
    }

    fn quick(&mut self, _ctx: &mut NativeCtx, pc: u32) -> u32 {
        let at = pc as usize;
        // The fast path's own code, on registers that carry no cell.
        if !self.proto.plain {
            return 0;
        }
        let Some(w) = self.proto.words.get(at).copied() else {
            return 0;
        };
        // What a body asks for most, each in a few instructions of its
        // own: the fast path's arm for it, without the loop's function
        // around it (whose frame is the price of every arm it holds).
        let done = match w.op {
            Op::GETUP | Op::CELLGET => self.quick_upvalue(w),
            Op::GETFIELD => self.quick_getfield(at, w),
            Op::SETFIELD => self.quick_setfield(at, w),
            Op::GETINDEX => self.quick_getindex(w),
            Op::LOADK => self.quick_constant(w),
            // What only the fast path's second tier runs goes straight
            // to it.
            Op::CALLB
            | Op::CALLM
            | Op::INTERP
            | Op::NEWLIST
            | Op::NEWMAP
            | Op::APPENDLIST
            | Op::INSERTMAP => return self.quick_second(at),
            _ => return self.quick_rest(at),
        };
        match done {
            Some(()) => 1,
            // The arm's uncommon case may still be the fast path's.
            None => self.quick_rest(at),
        }
    }

    fn elem(&mut self, _ctx: &mut NativeCtx, dst: u32, list: u32, index: u32) -> bool {
        let at = |r: u32| self.base.saturating_add(r as usize);
        let (dst, list, index) = (at(dst), at(list), at(index));
        if !self.proto.plain
            && [dst, list, index]
                .iter()
                .any(|i| matches!(self.m.cells.get(*i), Some(Some(_))))
        {
            return false;
        }
        let (Some(Value::List(items)), Some(Value::Int(n))) =
            (self.m.stack.get(list), self.m.stack.get(index))
        else {
            return false;
        };
        let Ok(n) = usize::try_from(*n) else {
            return false;
        };
        let v = match items.try_borrow() {
            Ok(elements) => match elements.get(n) {
                Some(v) => v.clone(),
                None => return false,
            },
            Err(_) => return false,
        };
        match self.m.stack.get_mut(dst) {
            Some(slot) => {
                *slot = v;
                true
            }
            None => false,
        }
    }
}

/// Whether [`Host::quick`] may run an instruction of this kind: one the
/// fast path runs that goes on at the next instruction and nowhere
/// else, counts no step and ends no frame. What jumps, `steps` and the
/// loops' own instructions change what the body itself keeps (its
/// place in the bytecode, the steps left, the loop's registers), and
/// are the body's or the interpreter's.
fn quick_runs(op: Op) -> bool {
    !matches!(
        op,
        Op::JMP
            | Op::LOOP
            | Op::EQJ
            | Op::NEJ
            | Op::LTJ
            | Op::LEJ
            | Op::TEST
            | Op::TESTN
            | Op::TESTSET
            | Op::STEPS
            | Op::FORPREP
            | Op::FORLOOP
            | Op::ITERPREP
            | Op::ITERNEXT
            | Op::CALL
            | Op::TAILCALL
            | Op::RET
            | Op::WIDE
    )
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
        native: std::cell::Cell::new(Has::NEITHER),
        hints: (0..f.instrs.len())
            .map(|_| std::cell::Cell::new(0))
            .collect(),
        sites: (0..f.instrs.len())
            .map(|_| std::cell::Cell::new(0))
            .collect(),
        reg_count: f.reg_count as usize,
        arity: f.arity as usize,
        spans: Spans::new(&f.span_base, &f.lines, f.instrs.len()),
        words: Word::table(&code),
        paired: Word::paired(&code),
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
    run_frame(interp, program, index, Rc::from(Vec::new()), args, true)
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
    let result = run_frame(
        interp,
        &cf.program,
        cf.index,
        Rc::clone(&cf.upvals),
        args,
        false,
    );
    interp.leave_call();
    result
}

fn run_frame(
    interp: &mut Interp,
    program: &Rc<Program>,
    index: usize,
    upvals: Rc<[Cell]>,
    args: Vec<Value>,
    window: bool,
) -> Result<Value, Diagnostic> {
    // The machine with bodies in it only for a program that has some.
    match program.has_bodies.get() {
        true => run_frame_on::<Bodied>(interp, program, index, upvals, args, window),
        false => run_frame_on::<Plain>(interp, program, index, upvals, args, window),
    }
}

/// [`run_frame`] on one of the two machines. Never inlined: each
/// machine's loop is a function of its own, so nothing that changes on
/// the machine with bodies moves a line of the code a program without
/// them runs.
#[inline(never)]
fn run_frame_on<T: Tier>(
    interp: &mut Interp,
    program: &Rc<Program>,
    index: usize,
    upvals: Rc<[Cell]>,
    args: Vec<Value>,
    window: bool,
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
    let mut m: Machine<'_, T> = Machine {
        tier: std::marker::PhantomData,
        interp,
        stack: regs.stack,
        cells: regs.cells,
        frames: regs.frames,
        tries: regs.tries,
        burns: regs.burns,
        upvals: regs.upvals,
        window,
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

/// Which of the VM's two machines this is. The machine and every
/// function of it are compiled once for each, from one source: a
/// program without bodies runs the one that has no word of them in it,
/// which is the machine it always ran on.
trait Tier: Sized {
    /// The machine's loop looks for a body when a frame comes on top.
    const BODIED: bool;
    /// The fast loop from `pc` in a function with a body: as
    /// [`Machine::run_fast`], and it stops before a resume point of the
    /// body as well. Only a bodied machine is ever asked.
    fn fast_to_resume(
        m: &mut Machine<'_, Self>,
        proto: &Proto,
        pc: usize,
        base: usize,
        body: &BodyEntry,
    ) -> Result<usize, Fault>;
    /// Enter `body` at its resume point `at`, on the frame on top at
    /// `pc`. Only a bodied machine is ever asked.
    fn enter(
        m: &mut Machine<'_, Self>,
        program: &Rc<Program>,
        proto: &Proto,
        base: usize,
        pc: usize,
        body: &BodyEntry,
        at: u32,
    ) -> Result<BodyFlow, Fault>;
}

/// The machine of a program without bodies.
struct Plain;

/// The machine of a program with a body for some function.
struct Bodied;

impl Tier for Plain {
    const BODIED: bool = false;

    fn fast_to_resume(
        _: &mut Machine<'_, Self>,
        _: &Proto,
        pc: usize,
        _: usize,
        _: &BodyEntry,
    ) -> Result<usize, Fault> {
        // Never asked: the general path runs the instruction.
        Ok(pc)
    }

    fn enter(
        _: &mut Machine<'_, Self>,
        _: &Rc<Program>,
        _: &Proto,
        _: usize,
        pc: usize,
        _: &BodyEntry,
        _: u32,
    ) -> Result<BodyFlow, Fault> {
        // Never asked: the bytecode goes on where it is.
        Ok(BodyFlow::At(pc))
    }
}

impl Tier for Bodied {
    const BODIED: bool = true;

    fn fast_to_resume(
        m: &mut Machine<'_, Self>,
        proto: &Proto,
        pc: usize,
        base: usize,
        body: &BodyEntry,
    ) -> Result<usize, Fault> {
        m.run_fast_between(proto, pc, base, body)
    }

    fn enter(
        m: &mut Machine<'_, Self>,
        program: &Rc<Program>,
        proto: &Proto,
        base: usize,
        pc: usize,
        body: &BodyEntry,
        at: u32,
    ) -> Result<BodyFlow, Fault> {
        m.enter_body_at(program, proto, base, pc, body, at)
    }
}

struct Machine<'i, T: Tier> {
    /// Which machine it is: nothing is kept for it.
    tier: std::marker::PhantomData<T>,
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
    /// The fast loop keeps [`REG_WINDOW`] registers in the stack from the
    /// running frame's base, growing it once for each new depth: a run's
    /// own machine. A builtin's callback runs in a machine that does not
    /// (its stack starts empty each time, and the window would be filled
    /// at every callback).
    window: bool,
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

/// What a body returned from `Machine::enter_body_at`: a stop that did
/// not raise. A [`Stop::Raised`] is returned as a `Fault` instead; a
/// [`Stop::Defect`] becomes an internal error.
enum BodyFlow {
    /// The body stopped `At` this pc: the bytecode goes on here, and
    /// the driver runs this one instruction before entering the body
    /// again (2.4).
    At(usize),
}

/// The body of the function on top, when the frame has one.
#[inline]
fn body_of_top<'p>(
    program: &'p Rc<Program>,
    frame: Option<&Frame>,
    proto: &Proto,
) -> Option<&'p BodyEntry> {
    if !program.has_bodies.get() {
        return None;
    }
    program.native()?.body_of(frame?.func, &proto.code)
}

/// The address of `R(0)` in a frame at `base`: the first register of
/// the frame, as a byte pointer. The register stack may grow during a
/// helper call, so the caller reads this again after every call that
/// returns. No `unsafe`: a mutable reference to the slot is a safe,
/// bounds-checked operation, and a raw pointer from a reference is
/// safe as long as it is not dereferenced here. `None` when the stack
/// does not reach that register, which never happens in a well-pushed
/// frame.
#[inline]
fn frame_r0(stack: &mut [Value], base: usize) -> Option<*mut u8> {
    stack.get_mut(base).map(|v| (v as *mut Value).cast::<u8>())
}

impl Machine<'_, Bodied> {
    /// [`Machine::run_fast`] for a function with a body: the instruction
    /// at `pc` always runs (when the fast path runs it), from the
    /// function's own code, and every one after it is read from
    /// `later`. The loop stops before the first of those that `later`
    /// has none for, or that `stops` names. A second copy of
    /// [`Machine::run_fast`], on this machine alone: the one a program
    /// without bodies runs on is the code it was before there were
    /// bodies, instruction for instruction. The tests hold the two to
    /// each other (every VM test runs on both).
    #[inline(always)]
    fn run_fast_to<S: StopAt>(
        &mut self,
        proto: &Proto,
        later: Lines<'_>,
        mut pc: usize,
        base: usize,
        stops: &S,
    ) -> Result<usize, Fault> {
        let end = base.saturating_add(REG_WINDOW);
        if self.window && self.stack.len() < end {
            // Once for each new depth a run reaches. Refused (the ceiling
            // on the registers' memory), the checked registers serve, and
            // the machine stops asking: every later entry would be
            // refused the same way, at the price of an error built and
            // dropped each time.
            if self.grow_to(end, Span::default()).is_err() {
                self.window = false;
            }
        }
        let Machine {
            interp,
            stack,
            frames,
            upvals,
            ..
        } = self;
        let interp: &mut Interp = interp;
        let Some(frame) = frames.last_mut() else {
            return Ok(pc);
        };
        // The frame's closure's upvalues, found through the frame once
        // here (none when it captured nothing): a call, a return and a
        // tail call all leave this loop, so the frame is the same one
        // until it does.
        let upvals: &[Cell] = if frame.up == NO_UPVALS {
            &[]
        } else {
            upvals.get(frame.up as usize).map_or(&[], |u| u)
        };
        // The first instruction is the function's own, whatever `later`
        // has there.
        let mut code = proto.lines();
        loop {
            let stop = match stack
                .get_mut(base..end)
                .and_then(|s| <&mut Window>::try_from(s).ok())
            {
                Some(w) => fast_run_to(w, interp, frame, proto, code, later, pc, upvals, stops),
                None => match stack.get_mut(base..) {
                    Some(regs) => {
                        fast_run_to(regs, interp, frame, proto, code, later, pc, upvals, stops)
                    }
                    None => Stop::Here(pc),
                },
            };
            match stop {
                Stop::Here(at) => return Ok(at),
                // A two-word instruction names registers past a byte: it
                // runs on the checked registers, and the window again
                // after it.
                Stop::Wide(at) => {
                    let (Some(regs), Some(Some(d))) = (stack.get_mut(base..), proto.code.get(at))
                    else {
                        return Ok(at);
                    };
                    #[cfg(feature = "bench-stats")]
                    let timer = stats::start(d.op);
                    let done = fast_one(regs, interp, frame, proto, d, upvals);
                    #[cfg(feature = "bench-stats")]
                    if done.is_some() {
                        stats::stop(timer);
                    }
                    match done {
                        Some(Ok(next)) => {
                            pc = next;
                            if stops.stops(pc) {
                                return Ok(pc);
                            }
                            // It ran: what follows is read from `later`.
                            code = later;
                        }
                        Some(Err(e)) => {
                            frame.pc = at;
                            return Err(e);
                        }
                        None => return Ok(at),
                    }
                }
                // As on the general path: the frame's pc is where the
                // error is offered to the tries.
                Stop::Raised(at, e) => {
                    frame.pc = at;
                    return Err(e);
                }
            }
        }
    }

    /// The fast loop of a function with a body, between two resume
    /// points of it: it reads the body's own table of the function's
    /// instructions, which has none where a resume point is, so it
    /// stops there with no test an instruction. A function of its own,
    /// as [`Machine::run_fast`] is.
    #[inline(never)]
    fn run_fast_between(
        &mut self,
        proto: &Proto,
        pc: usize,
        base: usize,
        body: &BodyEntry,
    ) -> Result<usize, Fault> {
        match body.between() {
            Some(later) => {
                let later = Lines {
                    words: &later.words,
                    code: &later.code,
                };
                self.run_fast_to(proto, later, pc, base, &Nowhere)
            }
            // The table is held elsewhere: the general path runs the
            // instruction at `pc`, which is always right.
            None => Ok(pc),
        }
    }

    /// The fast loop inside `from..to`, for a body's `run`: it stops
    /// where the pc leaves the range, either way.
    #[inline(never)]
    fn run_fast_inside(
        &mut self,
        proto: &Proto,
        pc: usize,
        base: usize,
        from: usize,
        to: usize,
    ) -> Result<usize, Fault> {
        self.run_fast_to(proto, proto.lines(), pc, base, &Outside { from, to })
    }

    /// [`Machine::unwind`] for a call a body's `run` is carrying: the
    /// error is offered to the frames above `floor`, innermost first.
    /// `Ok` when a try of one of them took it (the loop goes on in that
    /// frame). `Err` when none did: every frame above `floor` is gone,
    /// as an error leaves a frame, and the error is the body's frame's
    /// to be offered, at its call.
    fn unwind_above(
        &mut self,
        program: &Program,
        mut e: Diagnostic,
        floor: usize,
    ) -> Result<(), Fault> {
        let mut at: Option<usize> = None;
        while self.frames.len() > floor {
            let Some(frame) = self.frames.last() else {
                break;
            };
            let pc = at.unwrap_or(frame.pc);
            let offered = match program.funcs.get(frame.func) {
                Some(proto) => self.offer(proto, pc, e),
                None => Ok(Some(e)),
            };
            e = match offered {
                Ok(Some(e)) => self.unwind_burns(0, e),
                Ok(None) => return Ok(()),
                // The offer itself failed (a handler table that does not
                // hold together): that error is the run's, and no frame
                // above the floor outlives it.
                Err(broken) => {
                    self.drop_frames_above(program, floor);
                    return Err(broken);
                }
            };
            if let Some(f) = self.frames.pop() {
                self.release(&f);
                self.end_cells(program, &f);
            }
            at = self.frames.last().map(|f| f.site);
        }
        Err(e.into())
    }

    /// End every frame above `floor`, as a run that stops ends its
    /// frames.
    fn drop_frames_above(&mut self, program: &Program, floor: usize) {
        while self.frames.len() > floor {
            let Some(f) = self.frames.pop() else {
                break;
            };
            self.release(&f);
            self.end_cells(program, &f);
        }
    }

    /// Enter the body `body` at resume index `at` on the frame on top,
    /// at pc `pc`, and come back with how it stopped (2.3, 2.4).
    ///
    /// Before: the context is set up with the steps left of the budget,
    /// the depth left, the frame's address (R(0)) and the frame's loop
    /// nesting. After: the steps the body took are counted, and the
    /// frame's nesting is set from `ctx.nest`. On [`Stop::At`], the
    /// frame's pc is set to `pc` and the driver runs the instruction at
    /// `pc` itself. On [`Stop::Raised`], the frame's pc is set to the
    /// raising instruction's and the error the host kept is returned.
    /// On [`Stop::Defect`], an internal error names the function and pc.
    #[inline(never)]
    fn enter_body_at(
        &mut self,
        program: &Rc<Program>,
        proto: &Proto,
        base: usize,
        pc: usize,
        body: &BodyEntry,
        at: u32,
    ) -> Result<BodyFlow, Fault> {
        #[cfg(test)]
        native::ENTERED.with(|n| n.set(n.get() + 1));
        // The instruction's pc is where an error raised inside the body
        // is offered to the tries.
        if let Some(f) = self.frames.last_mut() {
            f.pc = pc;
        }
        let budget = native::budget_for(self.interp);
        let depth = i64::try_from(self.interp.calls_left()).unwrap_or(i64::MAX);
        let nest = self.frames.last().map_or(0, |f| f.nest);
        let Some(frame_ptr) = frame_r0(&mut self.stack, base) else {
            return Err(Box::new(broken(
                "a frame whose R(0) is past its stack",
                proto.spans.get(pc).copied().unwrap_or_default(),
            )));
        };
        let mut ctx = NativeCtx::new(budget, depth);
        ctx.frame = frame_ptr;
        // A `Cell<u64>` is laid out as its `u64`: the cells are read
        // where they lie, and they stay where they are for the run.
        ctx.sites = proto.sites.as_ptr().cast::<u64>();
        ctx.nest = nest;
        let native = program.native().ok_or_else(|| {
            broken(
                "a body entered on a program without native code",
                proto.spans.get(pc).copied().unwrap_or_default(),
            )
        })?;
        // The host borrows this machine exclusively for the body's run.
        let (stop, raised, last_given, quick_raised) = {
            let mut host = BodyHost {
                m: self,
                program,
                proto,
                base,
                body,
                at,
                patient: native.patient(),
                last_given: budget,
                raised: None,
                quick_raised: false,
            };
            let stop = native.enter_body(body, at, &mut ctx, &mut host);
            (stop, host.raised, host.last_given, host.quick_raised)
        };
        // Count the steps the body took since the host last gave it a
        // budget (`last_given`): any earlier ones were counted when
        // `run` was called. Not after `Raised` by `run` or `again`:
        // each took the context's count and nesting when it was called,
        // and what the interpreter counted and set since is its own
        // (the context is stale). After `Raised` by `quick` they are
        // counted: `quick` runs one instruction that counts no step and
        // takes nothing from the context, so what the body counted up
        // to that instruction is in it, as at any other return.
        if !matches!(stop, NativeStop::Raised(_)) || quick_raised {
            let took = last_given.saturating_sub(ctx.steps_left);
            if took > 0 {
                self.interp.steps = self.interp.steps.saturating_add(took.max(0) as u64);
            }
            self.set_nest(ctx.nest);
        }
        if let NativeStop::Raised(err_pc) = stop {
            if let Some(f) = self.frames.last_mut() {
                f.pc = err_pc as usize;
            }
            let err = raised.unwrap_or_else(|| {
                Box::new(
                    internal(format!(
                        "a native body returned Raised({err_pc}) with no error stored"
                    ))
                    .at(proto
                        .spans
                        .get(err_pc as usize)
                        .copied()
                        .unwrap_or_default()),
                )
            });
            return Err(err);
        }
        match stop {
            NativeStop::At(new_pc) => {
                if let Some(f) = self.frames.last_mut() {
                    f.pc = new_pc as usize;
                }
                Ok(BodyFlow::At(new_pc as usize))
            }
            NativeStop::Raised(_) => unreachable!("handled above"),
            NativeStop::Defect(err_pc) => {
                let func = self.frames.last().map(|f| f.func).unwrap_or(0);
                let span = proto
                    .spans
                    .get(err_pc as usize)
                    .copied()
                    .unwrap_or_default();
                Err(Box::new(
                    internal(format!(
                        "the native body of fn{func} found a register that is not of its proven type at pc {err_pc}: a defect in the compiler that built this program"
                    ))
                    .at(span),
                ))
            }
        }
    }
}

impl<T: Tier> Machine<'_, T> {
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
            window: _,
            tier: _,
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
            match self.run_frame_code(program, 0) {
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
    ///
    /// **Bodies.** When the frame on top has a body ([`Native::body_of`])
    /// and its pc is a resume point of it, the VM enters the body at that
    /// pc (2.1), except right after a body returned [`Stop::At`] at this
    /// pc (2.4): so one instruction runs between the body's stop and the
    /// next entry. Between two resume points a function with a body
    /// runs on the same fast loop as one without
    /// ([`Machine::run_fast_between`]), reading a table of its
    /// instructions that has none where a resume point is: the loop
    /// stops there with no test an instruction. The test for "has this
    /// function a body" is done when the frame on top changes, not per
    /// instruction.
    ///
    /// **A call under a body.** `floor` is how many frames are not this
    /// loop's to run: none for a run's own loop. A body's `run` that
    /// carries a call through to its return ([`BodyHost::call_through`])
    /// runs this loop again with the frames below the callee's as its
    /// floor, and the loop comes back with `None` when a return has
    /// brought the stack down to them. Only a machine with bodies is ever
    /// given a floor.
    fn run_frame_code(
        &mut self,
        program: &Rc<Program>,
        floor: usize,
    ) -> Result<Option<Value>, Fault> {
        let (mut pc, mut base, mut proto) = self.top(program)?;
        let mut body = match T::BODIED {
            true => body_of_top(program, self.frames.last(), proto),
            false => None,
        };
        // When the last thing that happened was a body coming back `At`
        // this pc, the VM runs that instruction before entering the body
        // again: it is cleared the moment any instruction runs or the
        // frame on top changes.
        let mut just_at: Option<usize> = None;
        loop {
            if T::BODIED {
                // Enter the body when the pc is a resume point of it.
                if let Some(b) = body {
                    if just_at != Some(pc) {
                        if let Some(at) = b.at(pc) {
                            if has_body_room() {
                                match T::enter(self, program, proto, base, pc, b, at)? {
                                    BodyFlow::At(new_pc) => {
                                        pc = new_pc;
                                        just_at = Some(new_pc);
                                        continue;
                                    }
                                }
                            }
                            // No room on the stack: the bytecode runs,
                            // which is always right.
                        }
                    }
                }
                just_at = None;
            }
            if proto.plain {
                match body {
                    None => {
                        let (at, switched) =
                            self.run_fast_frames(program, proto, pc, base, floor)?;
                        pc = at;
                        if switched {
                            // The callee a body's `run` was carrying
                            // has returned: the frame on top is that
                            // body's.
                            if T::BODIED && self.frames.len() <= floor {
                                return Ok(None);
                            }
                            // Calls and returns went by in the loop: the
                            // frame on top is another, and carries the pc.
                            (pc, base, proto) = self.top(program)?;
                            if T::BODIED {
                                body = body_of_top(program, self.frames.last(), proto);
                                if body.is_some() {
                                    continue;
                                }
                            }
                        }
                    }
                    // A function with a body: the same loop, which stops
                    // before the next resume point as well. The
                    // instruction at `pc` runs first, whatever `pc` is:
                    // the body was not entered here (it has just come
                    // back to this pc, or the stack has no room).
                    Some(b) => {
                        // A body most often comes back at a call or a
                        // return, which the fast loop never runs: it is
                        // not started for one.
                        let switches = matches!(
                            proto.code.get(pc),
                            Some(Some(d)) if matches!(d.op, Op::CALL | Op::RET | Op::TAILCALL)
                        );
                        if !switches {
                            let from = pc;
                            pc = T::fast_to_resume(self, proto, pc, base, b)?;
                            if pc != from && b.at(pc).is_some() {
                                continue;
                            }
                        }
                    }
                }
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
            #[cfg(feature = "bench-stats")]
            let timer =
                matches!(d.op, Op::CALL | Op::RET | Op::TAILCALL).then(|| stats::start(d.op));
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
            // One left to the general path (`Ok(None)`) is counted there.
            #[cfg(feature = "bench-stats")]
            if let (Some(timer), false) = (timer, matches!(switched, Ok(None))) {
                stats::stop(timer);
            }
            match switched {
                Ok(None) => {}
                Ok(Some(Some(v))) => return Ok(Some(v)),
                Ok(Some(None)) => {
                    // The callee a body's `run` was carrying has
                    // returned: the frame on top is that body's.
                    if T::BODIED && self.frames.len() <= floor {
                        return Ok(None);
                    }
                    (pc, base, proto) = self.top(program)?;
                    if T::BODIED {
                        body = body_of_top(program, self.frames.last(), proto);
                    }
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
                    if T::BODIED && self.frames.len() <= floor {
                        return Ok(None);
                    }
                    (pc, base, proto) = self.top(program)?;
                    if T::BODIED {
                        body = body_of_top(program, self.frames.last(), proto);
                    }
                }
                Flow::Switched => {
                    (pc, base, proto) = self.top(program)?;
                    if T::BODIED {
                        body = body_of_top(program, self.frames.last(), proto);
                    }
                }
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
    ///
    /// The registers are taken once here, not at every instruction: the
    /// [`Window`] of [`REG_WINDOW`] registers from the frame's base when
    /// the stack holds that many, which a one-word instruction's register
    /// numbers index with no check, and otherwise the stack from the base
    /// on, read with a check. The interpreter and the frame are borrowed
    /// beside them, so nothing in the loop goes back through the machine.
    #[inline(never)]
    fn run_fast(&mut self, proto: &Proto, mut pc: usize, base: usize) -> Result<usize, Fault> {
        let end = base.saturating_add(REG_WINDOW);
        if self.window && self.stack.len() < end {
            // Once for each new depth a run reaches. Refused (the ceiling
            // on the registers' memory), the checked registers serve, and
            // the machine stops asking: every later entry would be
            // refused the same way, at the price of an error built and
            // dropped each time.
            if self.grow_to(end, Span::default()).is_err() {
                self.window = false;
            }
        }
        let Machine {
            interp,
            stack,
            frames,
            upvals,
            ..
        } = self;
        let interp: &mut Interp = interp;
        let Some(frame) = frames.last_mut() else {
            return Ok(pc);
        };
        // The frame's closure's upvalues, found through the frame once
        // here (none when it captured nothing): a call, a return and a
        // tail call all leave this loop, so the frame is the same one
        // until it does.
        let upvals: &[Cell] = if frame.up == NO_UPVALS {
            &[]
        } else {
            upvals.get(frame.up as usize).map_or(&[], |u| u)
        };
        loop {
            let stop = match stack
                .get_mut(base..end)
                .and_then(|s| <&mut Window>::try_from(s).ok())
            {
                Some(w) => fast_run(w, interp, frame, proto, pc, upvals),
                None => match stack.get_mut(base..) {
                    Some(regs) => fast_run(regs, interp, frame, proto, pc, upvals),
                    None => Stop::Here(pc),
                },
            };
            match stop {
                Stop::Here(at) => return Ok(at),
                // A two-word instruction names registers past a byte: it
                // runs on the checked registers, and the window again
                // after it.
                Stop::Wide(at) => {
                    let (Some(regs), Some(Some(d))) = (stack.get_mut(base..), proto.code.get(at))
                    else {
                        return Ok(at);
                    };
                    #[cfg(feature = "bench-stats")]
                    let timer = stats::start(d.op);
                    let done = fast_one(regs, interp, frame, proto, d, upvals);
                    #[cfg(feature = "bench-stats")]
                    if done.is_some() {
                        stats::stop(timer);
                    }
                    match done {
                        Some(Ok(next)) => pc = next,
                        Some(Err(e)) => {
                            frame.pc = at;
                            return Err(e);
                        }
                        None => return Ok(at),
                    }
                }
                // As on the general path: the frame's pc is where the
                // error is offered to the tries.
                Stop::Raised(at, e) => {
                    frame.pc = at;
                    return Err(e);
                }
            }
        }
    }

    /// [`Machine::run_fast`] across calls and returns. A `call` of a
    /// function of this program and a `ret` to one are the two
    /// instructions a script runs most that the fast loop leaves, and
    /// each used to cost the loop, the driver's general path and the
    /// loop again. Here both switch frames with no more than that takes
    /// ([`quick_call`], [`quick_ret`]) and the fast loop is started
    /// again on the frame that is on top then, when it is this loop's
    /// to run: a function with no cell and no body.
    ///
    /// Anything else about a call or a return is left where it was,
    /// unchanged, for the general path, which does it or raises its
    /// error: a function not loaded yet, a wrong count of arguments, the
    /// depth or the stack at its limit, a native entry, a burn to close,
    /// a caller whose registers carry cells, the run's first frame, and
    /// a frame a body's `run` waits on (`floor` and the frames below it
    /// are not this loop's).
    ///
    /// The fast loop itself is not touched: it is the function it was,
    /// instruction for instruction, and this one stands around it. The
    /// second value says a frame was switched: the frame on top is not
    /// the one the loop was started on, and the pc is written in it.
    #[inline(never)]
    fn run_fast_frames<'p>(
        &mut self,
        program: &'p Rc<Program>,
        mut proto: &'p Proto,
        mut pc: usize,
        mut base: usize,
        floor: usize,
    ) -> Result<(usize, bool), Fault> {
        let mut switched = false;
        loop {
            let at = self.run_fast(proto, pc, base)?;
            #[cfg(test)]
            if QUICK_OFF.with(std::cell::Cell::get) {
                return Ok((at, switched));
            }
            // What the fast loop left: a call or a return that switches
            // frames here, or an instruction of the general path.
            let on_top = match proto.code.get(at) {
                Some(Some(d)) if d.op == Op::CALL => {
                    #[cfg(feature = "bench-stats")]
                    let timer = stats::start(d.op);
                    let callee = quick_call(self, program, base, d);
                    #[cfg(feature = "bench-stats")]
                    if callee.is_some() {
                        stats::stop(timer);
                    }
                    callee
                }
                Some(Some(d)) if d.op == Op::RET => {
                    #[cfg(feature = "bench-stats")]
                    let timer = stats::start(d.op);
                    let caller = quick_ret(self, program, floor, d);
                    #[cfg(feature = "bench-stats")]
                    if caller.is_some() {
                        stats::stop(timer);
                    }
                    caller
                }
                _ => None,
            };
            let Some(next) = on_top else {
                if switched {
                    if let Some(f) = self.frames.last_mut() {
                        f.pc = at;
                    }
                }
                return Ok((at, switched));
            };
            // The frame on top now: the callee's, at its first
            // instruction, or the caller's, after its call.
            switched = true;
            let Some(top) = self.frames.last() else {
                return Ok((at, switched));
            };
            (pc, base, proto) = (top.pc, top.base, next);
            // A return brought the stack down to the frame a body's
            // `run` waits on: that frame is not this loop's.
            if T::BODIED && self.frames.len() <= floor {
                return Ok((pc, switched));
            }
            // A function whose registers carry cells is the general
            // path's, and one with a body is entered by the driver. The
            // function says itself whether the native code has one for
            // it: most have none, and nothing more is asked of them.
            if !proto.plain
                || (T::BODIED
                    && proto.native.get().body()
                    && body_of_top(program, Some(top), proto).is_some())
            {
                return Ok((pc, switched));
            }
        }
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

    /// Run one instruction. The groups are functions of their own, so the
    /// frame each re-entry (a builtin calling back into the VM) leaves on
    /// the Rust stack is a small one: `step` and the group it went to.
    ///
    /// Always part of the loop that calls it: a body's `run` reaches it
    /// through [`Machine::step_for_body`], so the loop keeps the one
    /// copy it had when it was the only caller.
    #[inline(always)]
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

    /// [`Machine::step`] for a body's `run`: out of line, so the VM's own
    /// loop is the code it was before bodies.
    #[inline(never)]
    fn step_for_body(
        &mut self,
        program: &Rc<Program>,
        proto: &Proto,
        d: &DecodedInstr,
        base: usize,
        span: Span,
    ) -> Result<Flow, Fault> {
        self.step(program, proto, d, base, span)
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
                let key = self.get(base, d.b_u, span)?;
                let v = self.get(base, d.c, span)?;
                match &key {
                    Value::Str(k) if literal_key(proto, d, k) => {
                        map.borrow_mut().add_field(k, v);
                    }
                    _ => {
                        let key = map_key(key, span)?;
                        map.borrow_mut().insert(key, v);
                    }
                }
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
            nest_frame(&mut self.interp.loop_depth, f, nest);
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
        if let Some(native) = program.native() {
            if let Some(done) = self.call_native(native, index, n, proto, d, base) {
                return done.map(|()| true);
            }
        }
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

    /// `call A, B, C` of function `func` as native code, when the program
    /// has an entry of it for these `n` arguments' types and the stack
    /// has room for the calls native code may nest (`native::has_room`).
    /// `None`, with nothing changed, when it does not: the call pushes a
    /// frame as any other.
    ///
    /// What a frame's call does, this does: the call's depth is counted
    /// at the call's position (`E506`) and given back; the cells of the
    /// caller's registers from the first argument up are gone
    /// ([`Machine::push`]); the result goes to `R(A)` with the rest of
    /// its row null; and an error is raised at the call, where the
    /// caller's tries are offered it.
    ///
    /// What a frame needs and this does not, it does not do. The callee
    /// gets no registers, so the register stack does not grow and its
    /// ceiling (`E514`, the VM's own limit, which the tree-walker has
    /// not) is not reached by this call. And the closure's upvalues are
    /// not read: an entry's code depends on its arguments alone (the
    /// compiler makes one only for a function that reads no variable of
    /// another), so every closure of the function is the same to it.
    ///
    /// Out of line: its frame holds the arguments' slots, and
    /// `run_frame_code`'s must stay small (a builtin's callback re-enters
    /// it).
    #[inline(never)]
    fn call_native(
        &mut self,
        native: &Native,
        func: usize,
        n: usize,
        proto: &Proto,
        d: &DecodedInstr,
        base: usize,
    ) -> Option<Result<(), Fault>> {
        let at = base.saturating_add(d.a as usize);
        let first = at.saturating_add(1);
        let (entry, ret, slots) = native.entry(func, n, |k, ty| {
            let i = first.saturating_add(k);
            if !proto.plain {
                if let Some(Some(cell)) = self.cells.get(i) {
                    return native::slot(ty, &cell.borrow());
                }
            }
            native::slot(ty, self.stack.get(i)?)
        })?;
        let args = slots.get(..n)?;
        if !native::has_room(self.interp.calls_left()) {
            return None;
        }
        if let Err(e) = self.interp.enter_call(Span::default()) {
            return Some(Err(e.at(span_at(proto, d.pc)).into()));
        }
        if !proto.plain {
            let params = first.saturating_add(n);
            for i in first..params {
                if let Some(cell) = self.cells.get_mut(i).and_then(Option::take) {
                    if let Some(slot) = self.stack.get_mut(i) {
                        *slot = cell.borrow().clone();
                    }
                }
            }
            self.forget_cells(params, base.saturating_add(proto.reg_count));
        }
        let result = native.run(self.interp, entry, ret, args);
        self.interp.leave_call();
        let v = match result {
            Ok(v) => v,
            Err(e) => return Some(Err(e.into())),
        };
        if !self.write_result(at, d.c, v) {
            let span = span_at(proto, d.pc);
            return Some(Err(broken("a result register past its frame", span).into()));
        }
        // The caller goes on after the call.
        if let Some(f) = self.frames.last_mut() {
            f.pc = d.pc as usize + d.width as usize;
        }
        Some(Ok(()))
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
            // One test of its kind a register, and no call for one that
            // owns nothing: `fill` clones into each and drops what was
            // there through a call each.
            for slot in regs {
                overwrite(slot, Value::Null);
            }
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
            for slot in regs {
                overwrite(slot, Value::Null);
            }
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

impl<T: Tier> Machine<'_, T> {
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

/// `call A, B, C` between two runs of the fast loop, from a frame whose
/// registers carry no cell: what [`Machine::call_here`] does when `R(A)`
/// holds a function of this program that is loaded, takes these `B`
/// arguments and has no native entry, the depth has room for one more
/// call and the stack already holds the callee's registers. The
/// callee's frame is on top then, at its first instruction, and the
/// caller's says where it goes on and where the call is; the callee's
/// function is returned. The callee says itself whether the native
/// code has an entry for it ([`Proto::native`]): one that has, or does
/// not know yet, is the general path's, which loads the code.
///
/// `None`, with nothing changed, for any other call: the general path
/// makes it, or raises its error at the call.
#[inline(always)]
fn quick_call<'p, T: Tier>(
    m: &mut Machine<'_, T>,
    program: &'p Rc<Program>,
    base: usize,
    d: &DecodedInstr,
) -> Option<&'p Proto> {
    let at = base.checked_add(d.a as usize)?;
    let n = d.b_u as usize;
    let (below, above) = m
        .stack
        .split_at_mut_checked(at.checked_add(1)?.checked_add(n)?)?;
    let Value::Func(f) = below.get(at)? else {
        return None;
    };
    let cf = match &f.body {
        FnBody::Compiled(cf) if Rc::ptr_eq(&cf.program, program) => cf,
        _ => return None,
    };
    let target = program.funcs.get(cf.index)?;
    if n != target.arity || target.native.get().entry() {
        return None;
    }
    // The callee's registers past its arguments.
    let rest = above.get_mut(..target.reg_count.checked_sub(n)?)?;
    let caller = m.frames.last_mut()?;
    if !m.interp.try_enter_call() {
        return None;
    }
    // The call is made from here on: nothing below fails.
    for slot in rest {
        overwrite(slot, Value::Null);
    }
    let up = if cf.upvals.is_empty() {
        NO_UPVALS
    } else {
        m.upvals.push(Rc::clone(&cf.upvals));
        (m.upvals.len() - 1) as u32
    };
    let pc = d.pc as usize;
    caller.pc = pc + d.width as usize;
    caller.site = pc;
    let (tries, burns) = (m.tries.len(), m.burns.len());
    m.frames
        .push(Frame::new(cf.index, at + 1, d.c, up, tries, burns));
    #[cfg(test)]
    QUICK_CALLS.with(|n| n.set(n.get() + 1));
    Some(target)
}

/// `ret A` between two runs of the fast loop, from a frame whose
/// registers carry no cell: what [`Machine::ret_here`] does when the
/// frame has no burn to close and returns to a frame of this loop whose
/// registers carry no cell either (the result is written to a register,
/// never to a cell). The caller's frame is on top then, with its call's
/// result row written; the caller's function is returned.
///
/// `None`, with nothing changed, for any other return: the run's first
/// frame (it returns out of the VM), an open burn, a caller with cells.
/// A return to the frame a body's `run` waits on (the last of the
/// `floor` frames that are not this loop's) is made here too; the loop
/// then stops, since that frame is the body's to go on in.
#[inline(always)]
fn quick_ret<'p, T: Tier>(
    m: &mut Machine<'_, T>,
    program: &'p Rc<Program>,
    floor: usize,
    d: &DecodedInstr,
) -> Option<&'p Proto> {
    let below = m.frames.len().checked_sub(2)?;
    // The frame returned to is this loop's, or the one a body's `run`
    // waits on (the last of the frames that are not): never one below
    // that.
    if below.saturating_add(1) < floor {
        return None;
    }
    let f = *m.frames.last()?;
    if m.burns.len() > f.burns as usize {
        return None;
    }
    let caller = program.funcs.get(m.frames.get(below)?.func)?;
    if !caller.plain {
        return None;
    }
    // The call's result row, `R(A..=A+C)` of the caller: `R(A)` is the
    // register just below the frame that ends.
    let row = f.base.checked_sub(1)?;
    let (kept, ended) = m.stack.split_at_mut_checked(f.base)?;
    let result = kept.get_mut(row)?;
    ended.get(..f.extra as usize)?;
    // The frame ends here, and no register of it is read again before a
    // write: the value is moved out, not copied.
    let v = std::mem::replace(ended.get_mut(d.a as usize)?, Value::Null);
    // The return is made from here on: nothing below fails.
    m.frames.pop();
    if m.tries.len() > f.tries as usize {
        m.tries.truncate(f.tries as usize);
    }
    if f.up != NO_UPVALS {
        m.upvals.truncate(f.up as usize);
    }
    let held = f.nest.saturating_add(f.held_nest);
    if held != 0 {
        m.interp.loop_depth = m.interp.loop_depth.saturating_sub(held);
    }
    m.interp.leave_calls(f.held_calls);
    *result = v;
    for slot in ended.iter_mut().take(f.extra as usize) {
        overwrite(slot, Value::Null);
    }
    #[cfg(test)]
    QUICK_RETURNS.with(|n| n.set(n.get() + 1));
    Some(caller)
}

#[cfg(test)]
thread_local! {
    /// Calls this thread's machines made between two runs of the fast
    /// loop ([`quick_call`]).
    pub(crate) static QUICK_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Returns this thread's machines made between two runs of the fast
    /// loop ([`quick_ret`]).
    pub(crate) static QUICK_RETURNS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Every call and return is left to the general path, as all were
    /// before the fast loop had its own.
    pub(crate) static QUICK_OFF: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
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

/// How many registers from a frame's base the fast loop takes as one
/// [`Window`]: a one-word instruction names registers below 256 (each
/// number a byte) and `forloop` reaches `R(A+3)`, so every register such
/// an instruction names is in it, and the remainder by this power of two
/// is the register number itself, which the compiler sees is in range.
const REG_WINDOW: usize = 512;

/// The registers of the running frame from its base, when the stack holds
/// [`REG_WINDOW`] of them there.
type Window = [Value; REG_WINDOW];

/// One instruction as the fast loop reads it: the instruction's own
/// word, four bytes, with its opcode decoded. A decoded instruction is
/// thirty-two bytes of fields widened for the verifier and the general
/// path; the loop needs a byte of each, so it reads the word, as the
/// file has it, and widens what an arm uses.
///
/// `op` is [`Op::WIDE`] where the loop reads no instruction: both words
/// of a `WIDE` pair (its operands do not fit a byte; the loop is handed
/// the decoded instruction), and, in a body's table, a resume point.
/// `WIDE` is a prefix, never an instruction a frame runs, so the loop's
/// arms have none for it and it costs the loop no test of its own.
#[derive(Clone, Copy)]
struct Word {
    op: Op,
    a: u8,
    b: u8,
    c: u8,
}

const _: () = assert!(std::mem::size_of::<Word>() == 4);

impl Word {
    /// No instruction for the fast loop here.
    const NONE: Word = Word {
        op: Op::WIDE,
        a: 0,
        b: 0,
        c: 0,
    };

    /// The word of a decoded instruction; none for a two-word one. A
    /// `nop` has no operand here whatever its word holds: in these
    /// words a `nop` with a count is the loop's own ([`Word::paired`]),
    /// and never one a module wrote.
    fn of(d: &DecodedInstr) -> Word {
        if d.wide {
            return Word::NONE;
        }
        if d.op == Op::NOP {
            return Word {
                op: Op::NOP,
                a: 0,
                b: 0,
                c: 0,
            };
        }
        // `Bx` lies where `B` and `C` do: its low byte, then its high.
        let (b, c) = match d.fmt {
            Fmt::ABC => (d.b_u as u8, d.c as u8),
            Fmt::ABx | Fmt::AsBx => (d.b_u as u8, (d.b_u >> 8) as u8),
        };
        Word {
            op: d.op,
            a: d.a as u8,
            b,
            c,
        }
    }

    /// The words of a decoded function, one a pc.
    fn table(code: &[Option<DecodedInstr>]) -> Vec<Word> {
        code.iter()
            .map(|d| d.as_ref().map_or(Word::NONE, Word::of))
            .collect()
    }

    /// [`Word::table`] with two `steps` in a row counted in one: where
    /// a `steps` is followed by another of the same loop nesting, the
    /// first's place holds a `nop` with their nesting in `A` and the
    /// steps of both in `Bx`, which the fast loop counts and then goes
    /// on two words further. One instruction runs where there were
    /// two, and a `steps` that stands alone is the instruction it was.
    ///
    /// The second keeps its own word: a jump that lands on it counts
    /// what it counts. A pair whose steps would pass the run's budget
    /// is not run by the fast path at all, as a single `steps` is not:
    /// the general path runs the first from its decoded form, alone,
    /// and stops where the tree-walker does. Two `steps` that count
    /// nothing between them stay two (a count of none is a `nop`).
    fn paired(code: &[Option<DecodedInstr>]) -> Vec<Word> {
        let plain = Word::table(code);
        let mut words = plain.clone();
        for (pc, pair) in plain.windows(2).enumerate() {
            let [first, second] = pair else { continue };
            if first.op != Op::STEPS || second.op != Op::STEPS || first.a != second.a {
                continue;
            }
            let both = Operands::bx(*first) + Operands::bx(*second);
            let Ok(count @ 1..) = u16::try_from(both) else {
                continue;
            };
            let [b, c] = count.to_le_bytes();
            if let Some(word) = words.get_mut(pc) {
                *word = Word {
                    op: Op::NOP,
                    a: first.a,
                    b,
                    c,
                };
            }
        }
        words
    }
}

/// A function's instructions as a fast loop reads them: each one's
/// word, and beside it the decoded instruction, for what the word does
/// not hold.
#[derive(Clone, Copy)]
struct Lines<'a> {
    words: &'a [Word],
    code: &'a [Option<DecodedInstr>],
}

/// An instruction's operands as the fast path's arms read them, from
/// its word or from its decoded form: one source for both, so a
/// two-word instruction is run by the code a one-word one is.
trait Operands: Copy {
    /// `A`.
    fn a(self) -> u32;
    /// `B` of an `ABC` instruction.
    fn b(self) -> u32;
    /// `C` of an `ABC` instruction.
    fn c(self) -> u32;
    /// `Bx` of an `ABx` instruction.
    fn bx(self) -> u32;
    /// `sBx` of an `AsBx` instruction.
    fn sbx(self) -> i32;
    /// `C` as a signed jump.
    fn sc(self) -> i32;
    /// How many words the instruction is.
    fn width(self) -> usize;
    /// Of a `nop`: the steps of the pair of `steps` it stands for in
    /// the fast loop's own words ([`Word::paired`]), and none for a
    /// `nop` of the function.
    #[inline(always)]
    fn pair(self) -> u64 {
        0
    }
}

impl Operands for Word {
    #[inline(always)]
    fn a(self) -> u32 {
        u32::from(self.a)
    }
    #[inline(always)]
    fn b(self) -> u32 {
        u32::from(self.b)
    }
    #[inline(always)]
    fn c(self) -> u32 {
        u32::from(self.c)
    }
    #[inline(always)]
    fn bx(self) -> u32 {
        u32::from(u16::from_le_bytes([self.b, self.c]))
    }
    #[inline(always)]
    fn sbx(self) -> i32 {
        i32::from(i16::from_le_bytes([self.b, self.c]))
    }
    #[inline(always)]
    fn sc(self) -> i32 {
        i32::from(self.c as i8)
    }
    #[inline(always)]
    fn width(self) -> usize {
        1
    }
    #[inline(always)]
    fn pair(self) -> u64 {
        u64::from(self.bx())
    }
}

impl Operands for &DecodedInstr {
    #[inline(always)]
    fn a(self) -> u32 {
        self.a
    }
    #[inline(always)]
    fn b(self) -> u32 {
        self.b_u
    }
    #[inline(always)]
    fn c(self) -> u32 {
        self.c
    }
    #[inline(always)]
    fn bx(self) -> u32 {
        self.b_u
    }
    #[inline(always)]
    fn sbx(self) -> i32 {
        self.b_s
    }
    #[inline(always)]
    fn sc(self) -> i32 {
        self.c_s
    }
    #[inline(always)]
    fn width(self) -> usize {
        self.width as usize
    }
}

/// The registers of the running frame, `R(0)` on, as the fast loop reads
/// and writes them: a [`Window`], or the stack from the frame's base.
/// The verifier proved every register number of the function below its
/// register count, and the frame's registers are in the stack before it
/// runs; what is checked here is only what the form cannot show.
trait FrameRegs {
    /// Indexed with no check: runs one-word instructions only.
    const BYTE_INDEXED: bool;
    fn reg(&self, x: u32) -> Option<&Value>;
    fn reg_mut(&mut self, x: u32) -> Option<&mut Value>;
    /// `R(first..first+n)`.
    fn regs(&self, first: u32, n: usize) -> Option<&[Value]>;
}

impl FrameRegs for Window {
    const BYTE_INDEXED: bool = true;
    #[inline(always)]
    fn reg(&self, x: u32) -> Option<&Value> {
        debug_assert!((x as usize) < REG_WINDOW);
        self.get(x as usize % REG_WINDOW)
    }
    #[inline(always)]
    fn reg_mut(&mut self, x: u32) -> Option<&mut Value> {
        debug_assert!((x as usize) < REG_WINDOW);
        self.get_mut(x as usize % REG_WINDOW)
    }
    #[inline(always)]
    fn regs(&self, first: u32, n: usize) -> Option<&[Value]> {
        let first = first as usize;
        self.get(first..first.saturating_add(n))
    }
}

impl FrameRegs for [Value] {
    const BYTE_INDEXED: bool = false;
    #[inline(always)]
    fn reg(&self, x: u32) -> Option<&Value> {
        self.get(x as usize)
    }
    #[inline(always)]
    fn reg_mut(&mut self, x: u32) -> Option<&mut Value> {
        self.get_mut(x as usize)
    }
    #[inline(always)]
    fn regs(&self, first: u32, n: usize) -> Option<&[Value]> {
        let first = first as usize;
        self.get(first..first.saturating_add(n))
    }
}

/// Where the fast loop of a function with a body stops beside the
/// instructions it does not run: asked of the pc each instruction it
/// ran goes on to.
trait StopAt {
    fn stops(&self, pc: usize) -> bool;
}

/// Nowhere: the table the loop reads says where it stops (it has no
/// instruction at a resume point of the function's body).
struct Nowhere;

impl StopAt for Nowhere {
    #[inline(always)]
    fn stops(&self, _: usize) -> bool {
        false
    }
}

/// Where the pc leaves a range of instructions, by its end or back past
/// its start: what a body's `run` runs.
struct Outside {
    from: usize,
    to: usize,
}

impl StopAt for Outside {
    #[inline(always)]
    fn stops(&self, pc: usize) -> bool {
        pc < self.from || pc >= self.to
    }
}

/// Where a run of the fast loop stopped.
enum Stop {
    /// At this pc: an instruction the fast path does not run here (the
    /// general path runs it from the start), or no instruction at all.
    Here(usize),
    /// At a two-word instruction, on registers indexed with no check.
    Wide(usize),
    /// A builtin called at this pc raised.
    Raised(usize, Fault),
}

/// The fast loop over `regs` from `pc`, until an instruction it does not
/// run here.
#[inline(always)]
fn fast_run<R: FrameRegs + ?Sized>(
    regs: &mut R,
    interp: &mut Interp,
    frame: &mut Frame,
    proto: &Proto,
    mut pc: usize,
    upvals: &[Cell],
) -> Stop {
    let lines = proto.paired_lines();
    loop {
        match fast_at(regs, interp, frame, proto, lines, pc, upvals) {
            Ok(Some(Ok(next))) => pc = next,
            Ok(Some(Err(e))) => return Stop::Raised(pc, e),
            Ok(None) => return Stop::Here(pc),
            Err(stop) => return stop,
        }
    }
}

/// [`fast_run`] for a function with a body: the instruction at `pc` is
/// read from `first` and every one after it from `later`, and the loop
/// stops as well where `stops` says, before the first instruction after
/// the one at `pc` that it names. A second copy of the loop, so the one
/// a function without a body runs on is the code it was before there
/// were bodies.
#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn fast_run_to<R: FrameRegs + ?Sized, S: StopAt>(
    regs: &mut R,
    interp: &mut Interp,
    frame: &mut Frame,
    proto: &Proto,
    first: Lines<'_>,
    later: Lines<'_>,
    mut pc: usize,
    upvals: &[Cell],
    stops: &S,
) -> Stop {
    let mut lines = first;
    loop {
        let done = fast_at(regs, interp, frame, proto, lines, pc, upvals);
        lines = later;
        match done {
            Ok(Some(Ok(next))) => {
                pc = next;
                if stops.stops(pc) {
                    return Stop::Here(pc);
                }
            }
            Ok(Some(Err(e))) => return Stop::Raised(pc, e),
            Ok(None) => return Stop::Here(pc),
            Err(stop) => return stop,
        }
    }
}

/// One instruction on the fast path: the first tier here, the second
/// ([`fast_call`]) for what the first does not run. `None` when neither
/// ran it, having changed nothing.
#[inline(always)]
fn fast_one<R: FrameRegs + ?Sized>(
    regs: &mut R,
    interp: &mut Interp,
    frame: &mut Frame,
    proto: &Proto,
    d: &DecodedInstr,
    upvals: &[Cell],
) -> Option<Result<usize, Fault>> {
    match fast(regs, interp, frame, proto, d.op, d.pc as usize, d, upvals) {
        Some(next) => Some(Ok(next)),
        // What switches frames is the machine's loop's: the second tier
        // has nothing for it, and is not called to say so.
        None if matches!(d.op, Op::CALL | Op::RET | Op::TAILCALL) => None,
        None => fast_call(regs, interp, proto, d),
    }
}

/// One instruction of a fast loop, at `pc` of `lines`: read from its
/// word where it has one, and from its decoded form where it is two
/// words long and the registers are read with a check. `Err` is where
/// the loop stops without having run it.
#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn fast_at<R: FrameRegs + ?Sized>(
    regs: &mut R,
    interp: &mut Interp,
    frame: &mut Frame,
    proto: &Proto,
    lines: Lines<'_>,
    pc: usize,
    upvals: &[Cell],
) -> Result<Option<Result<usize, Fault>>, Stop> {
    let Some(w) = lines.words.get(pc) else {
        return Err(Stop::Here(pc));
    };
    #[cfg(feature = "bench-stats")]
    let timer = (w.op != Op::WIDE).then(|| stats::start(w.op));
    let done = match fast(regs, interp, frame, proto, w.op, pc, *w, upvals) {
        Some(next) => Some(Ok(next)),
        None => fast_rest(regs, interp, frame, proto, lines, pc, w.op, upvals)?,
    };
    // An instruction left to the general path is counted there, once.
    #[cfg(feature = "bench-stats")]
    if let (Some(timer), true) = (timer, done.is_some()) {
        stats::stop(timer);
    }
    Ok(done)
}

/// What [`fast_at`] does with an instruction the first tier did not run
/// from its word: the second tier, which is handed the decoded
/// instruction; nothing, for what switches frames (that is the
/// machine's loop's); and, where the word is none, the decoded
/// instruction of two words on checked registers, or the loop's stop.
#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn fast_rest<R: FrameRegs + ?Sized>(
    regs: &mut R,
    interp: &mut Interp,
    frame: &mut Frame,
    proto: &Proto,
    lines: Lines<'_>,
    pc: usize,
    op: Op,
    upvals: &[Cell],
) -> Result<Option<Result<usize, Fault>>, Stop> {
    match op {
        Op::CALL | Op::RET | Op::TAILCALL => Ok(None),
        Op::WIDE => match lines.code.get(pc) {
            Some(Some(_)) if R::BYTE_INDEXED => Err(Stop::Wide(pc)),
            Some(Some(d)) => {
                #[cfg(feature = "bench-stats")]
                let timer = stats::start(d.op);
                let done = fast_one(regs, interp, frame, proto, d, upvals);
                #[cfg(feature = "bench-stats")]
                if done.is_some() {
                    stats::stop(timer);
                }
                Ok(done)
            }
            _ => Err(Stop::Here(pc)),
        },
        _ => match proto.code.get(pc) {
            Some(Some(d)) => Ok(fast_call(regs, interp, proto, d)),
            _ => Err(Stop::Here(pc)),
        },
    }
}

/// The common cases of the hot instructions in a frame whose registers
/// carry no cell: the next pc when `d` ran here, `None` when it did not
/// and changed nothing, so the general path runs it from the start and
/// raises its error, if any, with the tree-walker's code and position.
#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn fast<R: FrameRegs + ?Sized, I: Operands>(
    regs: &mut R,
    interp: &mut Interp,
    frame: &mut Frame,
    proto: &Proto,
    op: Op,
    pc: usize,
    d: I,
    upvals: &[Cell],
) -> Option<usize> {
    let next = pc + d.width();
    let v = match op {
        Op::NOP => {
            // A `nop` of the function, or in the fast loop's own words
            // a pair of `steps` counted in one, two words long.
            let n = d.pair();
            if n == 0 {
                return Some(next);
            }
            if interp.max_steps != 0 && interp.steps.saturating_add(n) > interp.max_steps {
                return None;
            }
            nest_frame(&mut interp.loop_depth, frame, d.a());
            interp.steps += n;
            return Some(pc + 2);
        }
        Op::MOV => regs.reg(d.b())?.clone(),
        Op::LOADK => proto.consts.get(d.bx() as usize)?.clone(),
        Op::LOADNULL => return put(regs, d.a(), Value::Null, next),
        Op::LOADBOOL => return put_bool(regs, d.a(), d.b() != 0, next),
        Op::LOADINT => return put_num(regs, d.a(), Num::Int(i64::from(d.sbx())), next),
        // Each operator where its opcode is matched, as `op_arith` does
        // in Lua: two ints, two floats, or an int and a float (which
        // `binary` computes as two floats), and `None` for the rest
        // (overflow, a zero divisor, a NaN in an ordering, any other
        // type), which the general path runs with `binary`.
        Op::ADD => {
            let n = num_add(regs.reg(d.b())?, regs.reg(d.c())?)?;
            return put_num(regs, d.a(), n, next);
        }
        Op::SUB => {
            let n = num_sub(regs.reg(d.b())?, regs.reg(d.c())?)?;
            return put_num(regs, d.a(), n, next);
        }
        Op::MUL => {
            let n = num_mul(regs.reg(d.b())?, regs.reg(d.c())?)?;
            return put_num(regs, d.a(), n, next);
        }
        Op::DIV => {
            let n = num_div(regs.reg(d.b())?, regs.reg(d.c())?)?;
            return put_num(regs, d.a(), n, next);
        }
        Op::MOD => {
            let n = num_mod(regs.reg(d.b())?, regs.reg(d.c())?)?;
            return put_num(regs, d.a(), n, next);
        }
        Op::EQ => {
            let t = cmp_eq(regs.reg(d.b())?, regs.reg(d.c())?);
            return put_bool(regs, d.a(), t, next);
        }
        Op::LT => {
            let t = cmp_lt(regs.reg(d.b())?, regs.reg(d.c())?)?;
            return put_bool(regs, d.a(), t, next);
        }
        Op::LE => {
            let t = cmp_le(regs.reg(d.b())?, regs.reg(d.c())?)?;
            return put_bool(regs, d.a(), t, next);
        }
        Op::ADDK => {
            let n = num_add(regs.reg(d.b())?, proto.consts.get(d.c() as usize)?)?;
            return put_num(regs, d.a(), n, next);
        }
        Op::SUBK => {
            let n = num_sub(regs.reg(d.b())?, proto.consts.get(d.c() as usize)?)?;
            return put_num(regs, d.a(), n, next);
        }
        Op::MULK => {
            let n = num_mul(regs.reg(d.b())?, proto.consts.get(d.c() as usize)?)?;
            return put_num(regs, d.a(), n, next);
        }
        Op::DIVK => {
            let n = num_div(regs.reg(d.b())?, proto.consts.get(d.c() as usize)?)?;
            return put_num(regs, d.a(), n, next);
        }
        Op::MODK => {
            let n = num_mod(regs.reg(d.b())?, proto.consts.get(d.c() as usize)?)?;
            return put_num(regs, d.a(), n, next);
        }
        Op::EQK => {
            let t = cmp_eq(regs.reg(d.b())?, proto.consts.get(d.c() as usize)?);
            return put_bool(regs, d.a(), t, next);
        }
        Op::LTK => {
            let t = cmp_lt(regs.reg(d.b())?, proto.consts.get(d.c() as usize)?)?;
            return put_bool(regs, d.a(), t, next);
        }
        Op::LEK => {
            let t = cmp_le(regs.reg(d.b())?, proto.consts.get(d.c() as usize)?)?;
            return put_bool(regs, d.a(), t, next);
        }
        Op::NOT => {
            let t = !regs.reg(d.b())?.truthy();
            return put_bool(regs, d.a(), t, next);
        }
        Op::EQJ | Op::NEJ | Op::LTJ | Op::LEJ => {
            let (l, rv) = (regs.reg(d.a())?, regs.reg(d.b())?);
            let t = match op {
                Op::EQJ => cmp_eq(l, rv),
                Op::NEJ => !cmp_eq(l, rv),
                Op::LTJ => cmp_lt(l, rv)?,
                _ => cmp_le(l, rv)?,
            };
            return if t { fast_jump(pc, d.sc()) } else { Some(next) };
        }
        Op::JMP | Op::LOOP => return fast_jump(pc, d.sbx()),
        Op::TEST | Op::TESTN => {
            let truthy = regs.reg(d.a())?.truthy();
            return if truthy == (op == Op::TEST) {
                fast_jump(pc, d.sbx())
            } else {
                Some(next)
            };
        }
        Op::STEPS => {
            let n = u64::from(d.bx());
            if interp.max_steps != 0 && interp.steps.saturating_add(n) > interp.max_steps {
                return None;
            }
            nest_frame(&mut interp.loop_depth, frame, d.a());
            interp.steps += n;
            return Some(next);
        }
        Op::FORLOOP => {
            let (Value::Int(cur), Value::Int(limit), Value::Int(step)) =
                (regs.reg(d.a())?, regs.reg(d.a() + 1)?, regs.reg(d.a() + 2)?)
            else {
                return None;
            };
            let (cur, limit, step) = (*cur, *limit, *step);
            let target = fast_jump(pc, d.sbx())?;
            regs.reg(d.a() + 3)?;
            let more = cur
                .checked_add(step)
                .filter(|n| if step > 0 { *n < limit } else { *n > limit });
            return match more {
                Some(n) => {
                    overwrite(regs.reg_mut(d.a())?, Value::Int(n));
                    overwrite(regs.reg_mut(d.a() + 3)?, Value::Int(n));
                    Some(target)
                }
                None => Some(next),
            };
        }
        Op::GETUP | Op::CELLGET => upvals.get(d.b() as usize)?.try_borrow().ok()?.clone(),
        Op::GETINDEX => {
            let obj = regs.reg(d.b())?;
            let idx = regs.reg(d.c())?;
            let v = match (obj, idx) {
                (Value::List(list), Value::Int(i)) => {
                    let at = usize::try_from(*i).ok()?;
                    list.try_borrow().ok()?.get(at)?.clone()
                }
                (Value::Map(map), Value::Str(key)) => map.try_borrow().ok()?.get(&**key)?.clone(),
                _ => return None,
            };
            overwrite(regs.reg_mut(d.a())?, v);
            return Some(past_nops(proto, next, 1));
        }
        Op::GETFIELD => {
            let Value::Map(map) = regs.reg(d.b())? else {
                return None;
            };
            let map = map.try_borrow().ok()?;
            let site = proto.sites.get(pc)?;
            let v = match shaped(&map, site) {
                Some(i) => map.get_index(i)?.1.clone(),
                None => {
                    let Value::Str(name) = proto.consts.get(d.c() as usize)? else {
                        return None;
                    };
                    let i = field_index(&map, name, proto.hints.get(pc)?)?;
                    learn(&map, site, i);
                    map.get_index(i)?.1.clone()
                }
            };
            v
        }
        Op::SETFIELD => {
            let Value::Map(map) = regs.reg(d.a())? else {
                return None;
            };
            let v = regs.reg(d.c())?.clone();
            let mut map = map.try_borrow_mut().ok()?;
            let site = proto.sites.get(pc)?;
            let i = match shaped(&map, site) {
                Some(i) => i,
                None => {
                    let Value::Str(name) = proto.consts.get(d.b() as usize)? else {
                        return None;
                    };
                    let i = field_index(&map, name, proto.hints.get(pc)?)?;
                    learn(&map, site, i);
                    i
                }
            };
            let old = std::mem::replace(map.get_index_mut(i)?.1, v);
            drop(map);
            drop(old);
            return Some(next);
        }
        Op::SETINDEX => {
            let (Value::List(list), Value::Int(i)) = (regs.reg(d.a())?, regs.reg(d.b())?) else {
                return None;
            };
            let at = usize::try_from(*i).ok()?;
            let v = regs.reg(d.c())?.clone();
            let mut list = list.try_borrow_mut().ok()?;
            let slot = list.get_mut(at)?;
            let old = std::mem::replace(slot, v);
            drop(list);
            drop(old);
            return Some(past_nops(proto, next, 2));
        }
        _ => return None,
    };
    overwrite(regs.reg_mut(d.a())?, v);
    Some(next)
}

/// The fast loop's second tier, for a frame whose registers carry no
/// cell: the calls of builtins that cannot call back into the VM
/// (`callb`, and `callm` on a site whose cache holds the receiver's
/// type), `interp`, string `+`, and the list and map builders and
/// stores. `None` when `d` is not one of them or not its common case,
/// having changed nothing, so the general path runs it from the start; a
/// builtin's error comes back as the general path raises it. A builtin
/// that cannot call back runs no script code, so it neither reads nor
/// moves the step count or the loop depth.
#[inline(never)]
fn fast_call<R: FrameRegs + ?Sized>(
    regs: &mut R,
    interp: &mut Interp,
    proto: &Proto,
    d: &DecodedInstr,
) -> Option<Result<usize, Fault>> {
    let pc = d.pc as usize;
    let next = pc + d.width as usize;
    match d.op {
        Op::CALLB => {
            let b = *crate::stdlib::builtin_catalogue().get(d.b_u as usize)?;
            if b.calls_back {
                return None;
            }
            // `R(A)`, where the result goes, then the arguments.
            let args = regs.regs(d.a, (d.c as usize).saturating_add(1))?.get(1..)?;
            let span = span_of(proto, pc);
            match interp.call_builtin(b, args, span) {
                Ok(v) => {
                    put_result(regs, d.a, v);
                    Some(Ok(next))
                }
                Err(e) => Some(Err(e.into())),
            }
        }
        Op::CALLM => {
            let argv = regs.regs(d.a, (d.c as usize).saturating_add(1))?;
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
            match (b.func)(interp, argv, span) {
                Ok(v) => {
                    put_result(regs, d.a, v);
                    Some(Ok(next))
                }
                Err(e) => Some(Err(e.or_at(span).into())),
            }
        }
        Op::INTERP => {
            let made = interpolate(regs.regs(d.b_u, (d.c as usize).saturating_add(1))?);
            put_result(regs, d.a, made);
            Some(Ok(next))
        }
        // String `+`: the operator's own result, made in one block.
        Op::ADD => {
            let (Value::Str(l), Value::Str(rv)) = (regs.reg(d.b_u)?, regs.reg(d.c)?) else {
                return None;
            };
            let made = crate::value::build_str(|out| {
                out.reserve(l.len() + rv.len());
                out.push_str(l);
                out.push_str(rv);
            });
            put_result(regs, d.a, made);
            Some(Ok(next))
        }
        Op::NEWLIST => {
            let made = Value::list(Vec::with_capacity(d.b_u as usize));
            put_result(regs, d.a, made);
            Some(Ok(next))
        }
        Op::NEWMAP => {
            let made = Value::map(IndexMap::with_capacity(d.b_u as usize));
            put_result(regs, d.a, made);
            Some(Ok(next))
        }
        Op::APPENDLIST => {
            let v = regs.reg(d.b_u)?.clone();
            let Value::List(list) = regs.reg(d.a)? else {
                return None;
            };
            list.try_borrow_mut().ok()?.push(v);
            Some(Ok(next))
        }
        Op::INSERTMAP => {
            let Value::Str(key) = regs.reg(d.b_u)? else {
                return None;
            };
            let v = regs.reg(d.c)?.clone();
            let Value::Map(map) = regs.reg(d.a)? else {
                return None;
            };
            let mut map = map.try_borrow_mut().ok()?;
            let old = if literal_key(proto, d, key) {
                // A literal's key: the site keeps the shape the map had
                // and the one this key made of it, and the next map of
                // that shape (the literal's next map) takes the second
                // without asking the tree. A map of the first shape
                // holds its keys and no other, so the key is new to it.
                let site = proto.sites.get(pc)?;
                let (found, from) = (site.get(), map.shape());
                if from != shape::NONE && (found >> 32) as u32 == from {
                    map.add_known(key, v, found as u32);
                    None
                } else {
                    let len = map.len();
                    let old = map.add_field(key, v);
                    if from != shape::NONE && map.shape() != shape::NONE && map.len() > len {
                        site.set(u64::from(from) << 32 | u64::from(map.shape()));
                    }
                    old
                }
            } else {
                map.insert(key.to_string(), v)
            };
            drop(map);
            drop(old);
            Some(Ok(next))
        }
        Op::SETINDEX => {
            let (Value::Map(map), Value::Str(key)) = (regs.reg(d.a)?, regs.reg(d.b_u)?) else {
                return None;
            };
            let v = regs.reg(d.c)?.clone();
            let mut map = map.try_borrow_mut().ok()?;
            // A key already there keeps its place (as `insert` does)
            // and needs no `String` made for it.
            let old = match map.get_mut(key) {
                Some(slot) => Some(std::mem::replace(slot, v)),
                None => map.insert(key.to_string(), v),
            };
            drop(map);
            drop(old);
            Some(Ok(past_nops(proto, next, 2)))
        }
        _ => None,
    }
}

/// `R(a) = v` for a value a function has just returned (a builtin's
/// result, a string built, a list made). It is read out of the call's
/// own result as its tag and its payload and stored as those: moved as
/// a whole value it is read back sixteen bytes at once from where the
/// function wrote it a field at a time, which a processor cannot
/// forward, and every such instruction waits some fifteen cycles for
/// the stores to reach the cache.
#[inline(always)]
fn put_result<R: FrameRegs + ?Sized>(regs: &mut R, a: u32, v: Value) {
    let Some(slot) = regs.reg_mut(a) else {
        return;
    };
    match v {
        Value::Int(n) => overwrite(slot, Value::Int(n)),
        Value::Float(x) => overwrite(slot, Value::Float(x)),
        Value::Bool(b) => overwrite(slot, Value::Bool(b)),
        Value::Null => overwrite(slot, Value::Null),
        Value::Str(s) => overwrite(slot, Value::Str(s)),
        Value::List(l) => overwrite(slot, Value::List(l)),
        Value::Map(m) => overwrite(slot, Value::Map(m)),
        other => overwrite(slot, other),
    }
}

/// `*slot = v`, without the call that drops the old value when it owns
/// nothing (null, a bool, an int, a float: one test of its kind).
#[inline(always)]
fn overwrite(slot: &mut Value, v: Value) {
    if matches!(
        slot,
        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)
    ) {
        std::mem::forget(std::mem::replace(slot, v));
    } else {
        *slot = v;
    }
}

/// Set a frame's loop nesting to `nest`, keeping the interpreter's loop
/// depth (`depth`, the sum over the frames) in step.
#[inline(always)]
fn nest_frame(depth: &mut u32, f: &mut Frame, nest: u32) {
    if f.nest == nest && *depth >= nest {
        return;
    }
    *depth = depth.saturating_sub(f.nest).saturating_add(nest);
    f.nest = nest;
}

/// A jump's target in the fast path: `None` (the general path raises the
/// error) before the first instruction.
#[inline(always)]
fn fast_jump(pc: usize, offset: i32) -> Option<usize> {
    usize::try_from((pc as i64) + 1 + i64::from(offset)).ok()
}

/// The pc an instruction goes on at when it jumps: `None` for one that
/// never jumps, and for a target before the first instruction.
fn jump_target(d: &DecodedInstr) -> Option<usize> {
    let pc = d.pc as usize;
    match d.op {
        Op::JMP | Op::LOOP | Op::TEST | Op::TESTN | Op::FORPREP | Op::FORLOOP | Op::ITERNEXT => {
            fast_jump(pc, d.b_s)
        }
        Op::TESTSET | Op::EQJ | Op::NEJ | Op::LTJ | Op::LEJ => fast_jump(pc, d.c_s),
        _ => None,
    }
}

/// An int and a float, in either order, as the two floats `binary`
/// computes them as (`Value::as_f64`): `None` for any other pair.
#[inline(always)]
fn mixed(l: &Value, r: &Value) -> Option<(f64, f64)> {
    match (l, r) {
        (Value::Int(a), Value::Float(b)) => Some((*a as f64, *b)),
        (Value::Float(a), Value::Int(b)) => Some((*a, *b as f64)),
        _ => None,
    }
}

/// A number an arithmetic instruction makes, before it is a value in a
/// register: two scalars the compiler keeps in machine registers, where
/// a [`Value`] on its way to one was built in memory and copied.
#[derive(Clone, Copy)]
enum Num {
    Int(i64),
    Float(f64),
}

/// `R(a) = n`, a number: the kind and the payload are stored, and
/// nothing else of the register is.
#[inline(always)]
fn put_num<R: FrameRegs + ?Sized>(regs: &mut R, a: u32, n: Num, next: usize) -> Option<usize> {
    let slot = regs.reg_mut(a)?;
    match n {
        Num::Int(i) => overwrite(slot, Value::Int(i)),
        Num::Float(x) => overwrite(slot, Value::Float(x)),
    }
    Some(next)
}

/// `R(a) = t`, a bool.
#[inline(always)]
fn put_bool<R: FrameRegs + ?Sized>(regs: &mut R, a: u32, t: bool, next: usize) -> Option<usize> {
    overwrite(regs.reg_mut(a)?, Value::Bool(t));
    Some(next)
}

/// `R(a) = v`.
#[inline(always)]
fn put<R: FrameRegs + ?Sized>(regs: &mut R, a: u32, v: Value, next: usize) -> Option<usize> {
    overwrite(regs.reg_mut(a)?, v);
    Some(next)
}

/// `l + r` on numbers, as `binary` gives it: `None` on overflow and for
/// any operand that is not a number (strings and lists included).
#[inline(always)]
fn num_add(l: &Value, r: &Value) -> Option<Num> {
    match (l, r) {
        (Value::Int(a), Value::Int(b)) => a.checked_add(*b).map(Num::Int),
        (Value::Float(a), Value::Float(b)) => Some(Num::Float(a + b)),
        _ => mixed(l, r).map(|(a, b)| Num::Float(a + b)),
    }
}

/// `l - r` on numbers, as `binary` gives it.
#[inline(always)]
fn num_sub(l: &Value, r: &Value) -> Option<Num> {
    match (l, r) {
        (Value::Int(a), Value::Int(b)) => a.checked_sub(*b).map(Num::Int),
        (Value::Float(a), Value::Float(b)) => Some(Num::Float(a - b)),
        _ => mixed(l, r).map(|(a, b)| Num::Float(a - b)),
    }
}

/// `l * r` on numbers, as `binary` gives it.
#[inline(always)]
fn num_mul(l: &Value, r: &Value) -> Option<Num> {
    match (l, r) {
        (Value::Int(a), Value::Int(b)) => a.checked_mul(*b).map(Num::Int),
        (Value::Float(a), Value::Float(b)) => Some(Num::Float(a * b)),
        _ => mixed(l, r).map(|(a, b)| Num::Float(a * b)),
    }
}

/// `l / r` on numbers, as `binary` gives it: an int when two ints divide
/// exactly, else the floats' quotient; `None` for a zero divisor and for
/// `i64::MIN / -1`.
#[inline(always)]
fn num_div(l: &Value, r: &Value) -> Option<Num> {
    let (a, b) = match (l, r) {
        (Value::Int(a), Value::Int(b)) => {
            if *b == 0 || (*a == i64::MIN && *b == -1) {
                return None;
            }
            if a % b == 0 {
                return Some(Num::Int(a / b));
            }
            (*a as f64, *b as f64)
        }
        (Value::Float(a), Value::Float(b)) => (*a, *b),
        _ => mixed(l, r)?,
    };
    (b != 0.0).then(|| Num::Float(a / b))
}

/// `l % r` on numbers, as `binary` gives it: `None` for a zero divisor.
#[inline(always)]
fn num_mod(l: &Value, r: &Value) -> Option<Num> {
    let (a, b) = match (l, r) {
        (Value::Int(a), Value::Int(b)) => {
            return (*b != 0).then(|| Num::Int(a.wrapping_rem(*b)));
        }
        (Value::Float(a), Value::Float(b)) => (*a, *b),
        _ => mixed(l, r)?,
    };
    (b != 0.0).then(|| Num::Float(a % b))
}

/// `l == r` for any two values (`Value::equals`, as `binary` computes it).
#[inline(always)]
fn cmp_eq(l: &Value, r: &Value) -> bool {
    match (l, r) {
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::Float(a), Value::Float(b)) => a == b,
        _ => l.equals(r),
    }
}

/// The order of two numbers or two strings, as `binary` gives it: `None`
/// for a NaN and for any other pair.
#[inline(always)]
fn order(l: &Value, r: &Value) -> Option<std::cmp::Ordering> {
    match (l, r) {
        (Value::Int(a), Value::Int(b)) => Some(a.cmp(b)),
        (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
        (Value::Str(a), Value::Str(b)) => Some(a.cmp(b)),
        _ => {
            let (a, b) = mixed(l, r)?;
            a.partial_cmp(&b)
        }
    }
}

/// `l < r`, as `binary` gives it.
#[inline(always)]
fn cmp_lt(l: &Value, r: &Value) -> Option<bool> {
    match (l, r) {
        (Value::Int(a), Value::Int(b)) => Some(a < b),
        _ => order(l, r).map(std::cmp::Ordering::is_lt),
    }
}

/// `l <= r`, as `binary` gives it.
#[inline(always)]
fn cmp_le(l: &Value, r: &Value) -> Option<bool> {
    match (l, r) {
        (Value::Int(a), Value::Int(b)) => Some(a <= b),
        _ => order(l, r).map(std::cmp::Ordering::is_le),
    }
}

/// The place of a site's field in `map`, when the map has the shape the
/// site last found the field in: maps of one shape have the same keys in
/// the same places, so nothing of the key is looked at.
#[inline(always)]
fn shaped(map: &Entries, site: &std::cell::Cell<u64>) -> Option<usize> {
    let found = site.get();
    let shape = map.shape();
    (shape != shape::NONE && (found >> 32) as u32 == shape).then_some(found as u32 as usize)
}

/// A site found its field at `index` of `map`: kept, when the map has a
/// shape, for the next map of that shape.
#[inline(always)]
fn learn(map: &Entries, site: &std::cell::Cell<u64>, index: usize) {
    if let (true, Ok(i)) = (map.shape() != shape::NONE, u32::try_from(index)) {
        site.set(u64::from(map.shape()) << 32 | u64::from(i));
    }
}

/// Whether the key an `insertmap` reads from its register is one the
/// script wrote: the constant the `loadk` just before it put there, as
/// a map literal is lowered. The register's string is held to the
/// constant itself, so a jump that lands on the `insertmap` with
/// another key in the register is not taken for it.
#[inline(always)]
fn literal_key(proto: &Proto, d: &DecodedInstr, key: &Rc<str>) -> bool {
    let Some(w) = (d.pc as usize)
        .checked_sub(1)
        .and_then(|p| proto.words.get(p))
    else {
        return false;
    };
    w.op == Op::LOADK
        && Operands::a(*w) == d.b_u
        && matches!(
            proto.consts.get(Operands::bx(*w) as usize),
            Some(Value::Str(k)) if Rc::ptr_eq(k, key)
        )
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
