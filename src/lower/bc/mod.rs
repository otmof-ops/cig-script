// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
//! Stage B's lowering (cig-compile#31): a verified CIR module to the register
//! bytecode the cig runtime's VM runs (`docs/BYTECODE.md` in cig-script is
//! the contract), for the language the VM runs today: constants, slots
//! and moves; arithmetic, comparison and logic; `if`, `while`, `for`,
//! `break`, `continue`; pulls and lambdas, closures that capture (cells),
//! calls by name, by value, to builtins and to methods, `snuff`; lists,
//! maps, members, indexes and their stores; interpolation and `exhale`;
//! `try`/`ashtray`/`finally` and `cough`; a prelude name used as a value
//! (`loadg`); burns, their ops and compensations; `pack`; chains; step
//! marks.
//!
//! A program lowers completely or is refused: the one construct the
//! bytecode cannot carry, a `pack` root that runs a pull or a function,
//! stops the lowering with [`Error::Refused`], which names it and its
//! span. Every module that leaves is verified by the format crate's
//! verifier; one that does not verify is [`Error::Internal`], a bug here.
//!
//! How each piece reads:
//! - a slot (`roll`, `stick`, a parameter) is one register for the whole
//!   function; a CIR value is a register from its definition to its last
//!   use, then the register is free again. A value defined before a loop
//!   and read inside it lives to the loop's end. A read of a slot no
//!   closure captures is the slot's register, with no copy, and a value
//!   stored into such a slot right after it is made is made in the
//!   slot's register, also when it has other readers that read the slot
//!   as stored, where that is exact (`plan`);
//! - a call's callee and arguments sit in a window of consecutive
//!   registers (`call A, B, 0`, `callb A, idx, n`): a callee, receiver or
//!   argument only the call reads, made in its block, is made in its
//!   place there (the window taken before it is made), the rest are
//!   moved in; a pull called by name has its closure read straight into
//!   the window;
//! - a constant operand is the instruction's `K` (`addk` ... `lek`); a
//!   constant inside a loop that is read from a register is loaded once,
//!   before the outermost loop around it, into a register held until the
//!   loop is done (`plan`);
//! - a value block (`and`, `or`, `??`, `?.`) makes its subject and the
//!   value its right side yields in its result register, where only it
//!   reads them;
//! - `a > b` is `le a, b` then `not`, `a >= b` is `lt a, b` then `not`,
//!   `a != b` is `eq` then `not`: the operands keep their order, so the
//!   interpreter's error wording does too. A comparison only a branch
//!   reads is the branch's fused jump, with the same operator on the
//!   same operands (`compare_jump`);
//! - a step mark is `steps k, 1` (k the loop nesting where it sits), a
//!   loop starts with `steps k, 0` before its condition and ends with
//!   `steps k-1, 0`, where `break` lands, folded into the next
//!   statement's mark when that follows at once. The iteration's tick is
//!   the body's own first mark, after the condition's test;
//! - a captured slot's register carries a cell, fresh each time its
//!   block runs, in a register nothing else uses;
//! - a `try` is `try.enter`, the body (the handler's range), the catch
//!   and the finally, each ending with `try.leave 0`; `break`, `continue`
//!   and `snuff` leave each `try` they cross first (`try.leave 1` for one
//!   whose finally they leave), a snuffed value waiting in a register no
//!   finally writes. No tail call is emitted, so none sits in a try;
//! - a burn is the state map, `burn.enter`, `comp.enter` (with an
//!   `unburn`), the body, `comp.leave`, `burn.leave`, with its region and
//!   the region's row. A string literal label is the region's constant;
//!   any other is the region's register, the label's value, evaluated
//!   before the state map, with a `nop` after `burn.enter` at the label's
//!   position. `break` and `continue` leave every try and burn they cross,
//!   innermost first, then jump, with nothing after the jump; `snuff`
//!   leaves only the tries (its `ret` closes the burns, and a `try.leave`
//!   the burns inside its try). A body that ends in one of those (or in
//!   `cough`) ends the row there, with no closing pair no run reaches. A
//!   `burn.op` outside every burn of its function marks the function
//!   `requires_burn`;
//! - the top level's `chain` statements are `chain.make` with `C = 1`, and
//!   its one normal end returns a map of every name its statements
//!   declare, so a run can answer `--light` as the tree-walker does;
//! - each `pack` root and chain step but the last is checked where the
//!   tree-walker checks it, right after it is evaluated: one that fails
//!   goes straight to `pack` or `chain.make`, which raises its error;
//! - an index read is `getindex` and a plain index store `setindex`, at
//!   the expression's or the statement's position, each followed by the
//!   `nop`s that carry the index's position (and a store's, the
//!   object's): the VM raises each error at the position the tree-walker
//!   gives it, and never dispatches the `nop`s. A compound index store
//!   and a compound member store check their operands' types first
//!   (`type_of`): they read before they store, and their errors are the
//!   store's;
//! - every instruction gets the span of the CIR instruction it came from,
//!   and the line table keeps a row wherever the span changes from the
//!   instruction before it.

use crate::lower::cir::ir::{
    self, Block, BurnNode, Const, Func, FuncId, Instr, Kind, Piece, ShortOp, Ty, ValueId, VarId,
};
use crate::syntax::ast::{AssignOp, BinOp, BurnClass, UnOp};
use crate::syntax::span::Span;
use cig_bytecode::{
    self as bc, verify_module, wide_prefix, BurnRegion, Constant, HandlerEntry, Instr as Word,
    LineEntry, Op, RegionLabel, RegionRow, SpanBase, UpvalDesc, UpvalKind, MAX_FUNCTION_DEPTH,
    MAX_REG_COUNT, RESERVED_U32,
};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;

/// Why a module was not lowered.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// The program uses a construct this part of the lowering does not
    /// cover yet.
    Refused { construct: String, span: Span },
    /// A function past a limit of the format: its frame, an operand's
    /// widest form, the nesting of functions.
    Limit { what: String, span: Span },
    /// The lowering made a module the verifier refuses: a bug here.
    Internal(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Refused { construct, span } => write!(
                f,
                "{}:{}: {construct} is not lowered to bytecode yet",
                span.line, span.col
            ),
            Error::Limit { what, span } => write!(f, "{}:{}: {what}", span.line, span.col),
            Error::Internal(e) => write!(f, "the bytecode lowering made a bad module: {e}"),
        }
    }
}

/// The construct a CIR instruction kind is, when this part refuses the
/// kind whatever its operands: `None` for a kind it lowers, which is
/// every kind. One is still refused for what it names: a `pack` whose
/// roots run a pull or a function.
pub fn refused_kind(kind: &Kind) -> Option<&'static str> {
    match kind {
        Kind::Step(_)
        | Kind::Const(_)
        | Kind::Load(_)
        | Kind::Decl(..)
        | Kind::Store(..)
        | Kind::Unary(..)
        | Kind::Binary(..)
        | Kind::Short { .. }
        | Kind::MakeList(_)
        | Kind::MakeMap(_)
        | Kind::Key(_)
        | Kind::Interp(_)
        | Kind::Member { .. }
        | Kind::Index { .. }
        | Kind::SetIndex { .. }
        | Kind::SetMember { .. }
        | Kind::Closure(_)
        | Kind::CallBuiltin { .. }
        | Kind::CallUser { .. }
        | Kind::CallValue { .. }
        | Kind::CallMethod { .. }
        | Kind::Exhale(_)
        | Kind::If { .. }
        | Kind::Loop { .. }
        | Kind::ForEach { .. }
        | Kind::Yield(_)
        | Kind::Ret(_)
        | Kind::Break
        | Kind::Continue
        | Kind::Prelude(_)
        | Kind::Try { .. }
        | Kind::Throw(_)
        | Kind::BurnOp { .. }
        | Kind::MakeChain { .. }
        | Kind::PackDecl(_)
        | Kind::Burn(_) => None,
    }
}

/// Lower a verified CIR module to bytecode, and verify what came out.
pub fn lower(m: &ir::Module) -> Result<bc::Module, Error> {
    let catalogue: HashMap<&'static str, u32> = crate::stdlib::builtin_catalogue()
        .iter()
        .enumerate()
        .map(|(i, b)| (b.name, i as u32))
        .collect();
    // The slot each pull's closure is declared into, module-wide: a
    // function calls a pull of an enclosing one through that slot's cell.
    let mut pull_slot = HashMap::new();
    for f in &m.funcs {
        pulls(&f.body, &mut HashMap::new(), &mut pull_slot);
    }
    let cx = Cx {
        m,
        catalogue: &catalogue,
        pull_slot,
    };
    let main = m
        .funcs
        .first()
        .ok_or_else(|| Error::Internal("the module has no top level".into()))?;
    let module = bc::Module {
        source_name: m.file.clone(),
        main: cx.function(main, 1)?,
    };
    if let Err(e) = verify_module(&module) {
        return Err(Error::Internal(format!(
            "it does not verify: {e} (rule `{}`)",
            e.rule()
        )));
    }
    Ok(module)
}

/// Parse, check, lower to CIR, run `passes`, lower to bytecode.
pub fn compile(
    source: &str,
    file: &str,
    passes: &[&dyn crate::lower::cir::pass::Pass],
) -> Result<bc::Module, CompileError> {
    let m = crate::lower::cir::lower_source(source, file).map_err(CompileError::Lower)?;
    finish(m, passes)
}

/// [`compile`] for a script that has already been parsed and checked:
/// lower to CIR, run `passes`, lower to bytecode.
pub fn compile_program(
    program: &crate::syntax::ast::Program,
    file: &str,
    passes: &[&dyn crate::lower::cir::pass::Pass],
) -> Result<bc::Module, CompileError> {
    let m = crate::lower::cir::lower_program(program, file).map_err(CompileError::Lower)?;
    finish(m, passes)
}

fn finish(
    mut m: ir::Module,
    passes: &[&dyn crate::lower::cir::pass::Pass],
) -> Result<bc::Module, CompileError> {
    crate::lower::cir::pass::run_passes(&mut m, passes).map_err(CompileError::Verify)?;
    lower(&m).map_err(CompileError::Bytecode)
}

#[derive(Debug)]
pub enum CompileError {
    Lower(crate::lower::cir::LowerError),
    Verify(crate::lower::cir::pass::PassError),
    Bytecode(Error),
}

/// `--emit bc`: the format crate's disassembly, then each function's line
/// table (pre-order, as the disassembly numbers functions): `pc line:col`.
pub fn disassemble(m: &bc::Module) -> String {
    let mut out = bc::disasm::disassemble_module(m);
    let mut fns = Vec::new();
    preorder(&m.main, &mut fns);
    out.push('\n');
    for (i, f) in fns.iter().enumerate() {
        out.push_str(&format!("; fn {i} lines:"));
        for row in &f.lines {
            out.push_str(&format!(
                " {}@{}:{}",
                row.pc,
                f.span_base.line.saturating_add(row.line_off),
                row.col
            ));
        }
        out.push('\n');
    }
    out
}

fn preorder<'f>(f: &'f bc::Function, out: &mut Vec<&'f bc::Function>) {
    out.push(f);
    for c in &f.children {
        preorder(c, out);
    }
}

struct Cx<'m> {
    m: &'m ir::Module,
    catalogue: &'m HashMap<&'static str, u32>,
    pull_slot: HashMap<FuncId, VarId>,
}

impl<'m> Cx<'m> {
    /// Lower one function, narrow jumps first; if a jump's offset does not
    /// fit 16 bits, again with every jump wide. A fused compare-and-jump
    /// whose offset does not fit its signed 8-bit field is lowered again
    /// as a compare into a register and a `test`, which reaches as far as
    /// any jump.
    fn function(&self, f: &'m Func, depth: u32) -> Result<bc::Function, Error> {
        if depth > MAX_FUNCTION_DEPTH {
            return Err(Error::Limit {
                what: format!(
                    "functions nest more than {MAX_FUNCTION_DEPTH} deep, the bytecode's limit"
                ),
                span: f.span,
            });
        }
        let (mut wide, mut far) = (false, HashSet::new());
        loop {
            match FnLower::new(self, f, depth, wide, far.clone()).run() {
                Ok(x) => return Ok(x),
                Err(Retry::Error(e)) => return Err(e),
                Err(Retry::Wide) if !wide => wide = true,
                Err(Retry::Wide) => {
                    return Err(Error::Internal("a wide jump that does not fit".into()))
                }
                // Each retry takes at least one more site to the long form,
                // so this ends.
                Err(Retry::Far(sites)) => {
                    let before = far.len();
                    far.extend(sites);
                    if far.len() == before {
                        return Err(Error::Internal("a fused jump that does not fit".into()));
                    }
                }
            }
        }
    }
}

enum Retry {
    Wide,
    /// These fused compare-and-jump sites (numbered in the order they are
    /// lowered) are too far from their targets.
    Far(Vec<usize>),
    Error(Error),
}

impl From<Error> for Retry {
    fn from(e: Error) -> Retry {
        Retry::Error(e)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum ConstKey<'a> {
    Bool(bool),
    Int(i64),
    Float(u64),
    Str(&'a str),
}

/// A literal a register can be loaded with: the pool's kinds, and null.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Lit<'a> {
    Null,
    Bool(bool),
    Int(i64),
    Float(u64),
    Str(&'a str),
}

impl<'a> Lit<'a> {
    fn of(c: &'a Const) -> Lit<'a> {
        match c {
            Const::Null => Lit::Null,
            Const::Bool(b) => Lit::Bool(*b),
            Const::Int(x) => Lit::Int(*x),
            Const::Float(x) => Lit::Float(x.to_bits()),
            Const::Str(x) => Lit::Str(x),
        }
    }
}

#[derive(Clone, Copy)]
struct Label(usize);

/// A jump whose offset is written once its target is placed.
struct Patch {
    pc: usize,
    op: Op,
    a: u32,
    wide: bool,
    target: Label,
    /// A fused compare-and-jump (ABC, the offset in a signed 8-bit `C`):
    /// its `B` and its site number.
    fused: Option<(u32, usize)>,
}

/// A comparison whose only reader is the branch right after it: lowered
/// there, as a jump, with the comparison's own position.
#[derive(Clone, Copy)]
struct PendingCmp {
    op: BinOp,
    a: ValueId,
    b: ValueId,
    span: Span,
}

struct LoopCx {
    /// Where `continue` goes: back to the condition (`loop`), or on to
    /// the next item (`jmp`).
    cont: Label,
    cont_op: Op,
    exit: Label,
    /// How many tries and burns were open in this function when the loop
    /// started: `break` and `continue` leave every one opened since.
    open: usize,
}

/// A `try` or a burn the code being lowered sits in.
#[derive(Clone, Copy)]
enum OpenCx {
    Try {
        /// In its finally: a way out drops what the finally had pending
        /// (`try.leave 1`) instead of running it (`try.leave 0`).
        in_finally: bool,
    },
    Burn {
        /// It has an `unburn { }` (and is not unlit): `comp.leave` first.
        comp: bool,
    },
}

/// What a `pack` root or a chain step is checked for as soon as it is
/// evaluated, as the tree-walker checks it.
#[derive(Clone, Copy)]
enum Part {
    /// A string that is not empty once trimmed.
    Root,
    /// A function or a chain.
    Step,
}

/// A check of one `pack` root or chain step: when it fails, the run goes
/// to `to`, the `pack` or `chain.make` (its window built), which raises
/// that part's own error before it looks at the parts not evaluated yet.
#[derive(Clone, Copy)]
struct PartCheck {
    v: ValueId,
    part: Part,
    to: Label,
}

/// Where a slot lives in the function that names it.
#[derive(Clone, Copy)]
enum Slot {
    /// Its own register (a cell when a closure captures it).
    Reg(u32),
    /// An enclosing function's: the upvalue at this index.
    Up(u32),
}

/// Where a `yield` sends its value.
enum YieldTo {
    /// A short-circuit's result register.
    Reg(u32),
    /// A loop's condition: falsy leaves the loop.
    Exit(Label),
}

struct FnLower<'c, 'm> {
    cx: &'c Cx<'m>,
    f: &'m Func,
    depth: u32,
    wide_jumps: bool,
    code: Vec<Word>,
    rows: Vec<(u32, Span)>,
    consts: Vec<Constant>,
    const_index: HashMap<ConstKey<'m>, u32>,
    children: Vec<bc::Function>,
    labels: Vec<Option<usize>>,
    patches: Vec<Patch>,
    /// The frame's length so far, and the registers below it that are
    /// free.
    frame: u32,
    free_regs: BTreeSet<u32>,
    var_reg: HashMap<VarId, u32>,
    val_reg: HashMap<ValueId, u32>,
    /// After the instruction with this position finishes, these values
    /// are dead.
    dies_at: HashMap<usize, Vec<ValueId>>,
    start: HashMap<ir::InstrId, usize>,
    end: HashMap<ir::InstrId, usize>,
    /// `a..b` values only a `for` reads: lowered as the loop's bounds.
    ranges: HashMap<ValueId, (ValueId, ValueId, Span)>,
    /// `exhale` parts and interpolation pieces to turn to text before or
    /// after an instruction, where the tree-walker evaluates them (keyed
    /// by the instruction, each with the `exhale` or interpolation that
    /// shows it), and the register holding each text.
    show_before: HashMap<ir::InstrId, Vec<(ir::InstrId, ValueId)>>,
    show_after: HashMap<ir::InstrId, Vec<(ir::InstrId, ValueId)>>,
    text_of: HashMap<(ir::InstrId, ValueId), u32>,
    loops: Vec<LoopCx>,
    yields: Vec<YieldTo>,
    /// The function's handler table, and the tries and burns open where
    /// the lowering is, innermost last.
    handlers: Vec<HandlerEntry>,
    open: Vec<OpenCx>,
    /// The function's burn regions and their pc ranges.
    regions: Vec<BurnRegion>,
    region_rows: Vec<RegionRow>,
    /// A `burn.op` sits outside every burn of this function: it is marked
    /// `requires_burn`.
    bare_op: bool,
    /// Every instruction by id (a burn's label is its constant's).
    instr_of: HashMap<ir::InstrId, &'m Instr>,
    /// `pack` roots and chain steps to check where they are evaluated, and
    /// the label each `pack` or `chain.make` takes for the checks' jumps.
    check_before: HashMap<ir::InstrId, Vec<PartCheck>>,
    check_after: HashMap<ir::InstrId, Vec<PartCheck>>,
    part_to: HashMap<ir::InstrId, Label>,
    /// `pack` statements whose roots run script code.
    pack_runs_code: HashSet<ir::InstrId>,
    /// The statements directly in the top level's block (none in a pull
    /// or a lambda).
    top_level: HashSet<ir::InstrId>,
    /// Where a `snuff` inside a try puts its value before the finallys it
    /// crosses run: a register from `fresh`, which no other value is ever
    /// given, so no finally writes it.
    ret_reg: Option<u32>,
    span: Span,
    /// The fused compare-and-jump sites lowered so far, and the ones to
    /// lower in the long form (too far for a signed 8-bit offset).
    fused_sites: usize,
    far: HashSet<usize>,
    /// Slot reads with no copy: the value is the slot's own register
    /// (`plan` says when that is exact).
    alias: HashSet<ValueId>,
    /// Values made straight into the register of the slot the `decl` or
    /// store right after them writes (their one reader, or the first of
    /// several that may all read the slot).
    dest: HashMap<ValueId, VarId>,
    /// Values whose register is a slot's: never freed with the value.
    pinned: HashSet<ValueId>,
    /// Arguments, and callees and receivers, made straight in their
    /// call's window: the call, the value's place in the window (0 for
    /// the callee or receiver, `1 + k` for argument `k`), the window's
    /// length.
    direct: HashMap<ValueId, (ir::InstrId, u32, u32)>,
    /// Values made straight in the result register of the value block
    /// (`and`, `or`, `??`, `?.`) that reads them: its subject, or the
    /// value its right side yields. Each block's register, taken when the
    /// first of them is made (or the block's own value is), and whether
    /// the block's value owns it (it was taken for the block alone).
    into: HashMap<ValueId, ir::InstrId>,
    short_reg: HashMap<ir::InstrId, (u32, bool)>,
    /// Windows taken before their call, when its first such argument is
    /// made.
    windows: HashMap<ir::InstrId, u32>,
    /// Binary operations lowered with a constant operand (`addk`, `eqk`,
    /// ...): `true` when the constant is the left operand of an operation
    /// whose operands may swap.
    kform: HashMap<ir::InstrId, bool>,
    /// Constants every reader takes as an operand: never loaded.
    unloaded: HashSet<ValueId>,
    /// Constants inside a loop, loaded once before the outermost loop
    /// around them (`plan` says when that is exact): the loop and the
    /// literal; each loop's literals, in order; and the register each
    /// literal holds while its loop is lowered.
    hoist: HashMap<ValueId, (ir::InstrId, Lit<'m>)>,
    hoist_at: HashMap<ir::InstrId, Vec<Lit<'m>>>,
    hoisted: HashMap<(ir::InstrId, Lit<'m>), u32>,
    /// Comparisons and `not`s whose one reader is the branch right after
    /// them, and that reader.
    fused: HashMap<ValueId, ir::InstrId>,
    pending: HashMap<ValueId, PendingCmp>,
    pending_not: HashMap<ValueId, ValueId>,
    /// The last instruction is `steps k, 0` at this pc.
    steps0: Option<(usize, u32)>,
    /// The pc the last label was placed at.
    last_placed: Option<usize>,
}

impl<'c, 'm> FnLower<'c, 'm> {
    fn new(cx: &'c Cx<'m>, f: &'m Func, depth: u32, wide_jumps: bool, far: HashSet<usize>) -> Self {
        FnLower {
            cx,
            f,
            depth,
            wide_jumps,
            code: Vec::new(),
            rows: Vec::new(),
            consts: Vec::new(),
            const_index: HashMap::new(),
            children: Vec::new(),
            labels: Vec::new(),
            patches: Vec::new(),
            frame: 0,
            free_regs: BTreeSet::new(),
            var_reg: HashMap::new(),
            val_reg: HashMap::new(),
            dies_at: HashMap::new(),
            start: HashMap::new(),
            end: HashMap::new(),
            ranges: HashMap::new(),
            show_before: HashMap::new(),
            show_after: HashMap::new(),
            text_of: HashMap::new(),
            loops: Vec::new(),
            yields: Vec::new(),
            handlers: Vec::new(),
            open: Vec::new(),
            regions: Vec::new(),
            region_rows: Vec::new(),
            bare_op: false,
            instr_of: HashMap::new(),
            check_before: HashMap::new(),
            check_after: HashMap::new(),
            part_to: HashMap::new(),
            pack_runs_code: HashSet::new(),
            top_level: HashSet::new(),
            ret_reg: None,
            span: f.span,
            fused_sites: 0,
            far,
            alias: HashSet::new(),
            dest: HashMap::new(),
            pinned: HashSet::new(),
            direct: HashMap::new(),
            into: HashMap::new(),
            short_reg: HashMap::new(),
            windows: HashMap::new(),
            kform: HashMap::new(),
            unloaded: HashSet::new(),
            hoist: HashMap::new(),
            hoist_at: HashMap::new(),
            hoisted: HashMap::new(),
            fused: HashMap::new(),
            pending: HashMap::new(),
            pending_not: HashMap::new(),
            steps0: None,
            last_placed: None,
        }
    }

    fn run(mut self) -> Result<bc::Function, Retry> {
        let f = self.f;
        // Parameters are the frame's first registers.
        for p in &f.params {
            let r = self.alloc();
            self.var_reg.insert(*p, r);
        }
        // Then the captured slots, before any code: a register with a cell
        // sits below every call's window, so no callee's frame lies over it
        // (`alloc_top`), wherever in a loop its slot is first declared. A
        // `for` item is its loop window's (`for_each`).
        let mut items = HashSet::new();
        walk(&f.body, &mut |i| {
            if let Kind::ForEach { var, .. } = &i.kind {
                items.insert(*var);
            }
        });
        for v in 0..self.cx.m.vars.len() {
            let v = VarId(v as u32);
            if self.captured(v) && !self.var_reg.contains_key(&v) && !items.contains(&v) {
                let r = self.fresh(1);
                self.var_reg.insert(v, r);
            }
        }
        let mut counter = 0;
        number(&f.body, &mut self.start, &mut self.end, &mut counter);
        let mut instr_of = HashMap::new();
        walk(&f.body, &mut |i| {
            instr_of.insert(i.id, i);
        });
        self.instr_of = instr_of;
        if f.kind == ir::FuncKind::Main {
            self.top_level = f.body.instrs.iter().map(|i| i.id).collect();
        }
        self.prepass();
        self.plan();
        self.liveness();
        // A captured parameter is a cell from the call's start.
        for p in &f.params {
            if self.captured(*p) {
                let r = self.var_reg[p];
                self.abc(Op::CELLNEW, r, 0, 0)?;
            }
        }
        self.block(&f.body)?;
        if !ends(&f.body) {
            self.top_level_end()?;
        }
        self.patch()?;
        let reg_count = self.frame.max(1);
        if reg_count > MAX_REG_COUNT {
            return Err(Error::Limit {
                what: format!(
                    "the function needs {reg_count} registers, past the frame limit of {MAX_REG_COUNT}"
                ),
                span: f.span,
            }
            .into());
        }
        let (span_base, lines) = self.line_table();
        let kind = match f.kind {
            ir::FuncKind::Main => bc::FuncKind::Main,
            ir::FuncKind::Pull => bc::FuncKind::Pull,
            ir::FuncKind::Lambda => bc::FuncKind::Lambda,
        };
        let arity = u16::try_from(f.params.len()).map_err(|_| Error::Limit {
            what: "a function with more than 65,535 parameters".into(),
            span: f.span,
        })?;
        Ok(bc::Function {
            name: f.name.clone().unwrap_or_default(),
            kind,
            reg_count,
            arity,
            requires_burn: self.bare_op,
            // The parent fills these in at its `closure`.
            upvalues: Vec::new(),
            constants: self.consts,
            children: self.children,
            regions: self.regions,
            handlers: self.handlers,
            instrs: self.code,
            span_base,
            lines,
            region_rows: {
                // Outer regions first, as the source nests them.
                let mut rows = self.region_rows;
                rows.sort_by_key(|r| (r.pc_start, std::cmp::Reverse(r.pc_end)));
                rows
            },
        })
    }

    /// The top level's only normal end (it has no `snuff`): a map of every
    /// name a statement of its block declares, in the order they are
    /// declared, each with its value now (a captured one read through its
    /// cell, as every read of its register is), so a run can declare them
    /// in the interpreter's globals for `--light`. With no such name it
    /// returns null.
    fn top_level_end(&mut self) -> Result<(), Error> {
        let mut names: Vec<(&'m str, u32)> = Vec::new();
        if self.f.kind == ir::FuncKind::Main {
            for i in &self.f.body.instrs {
                if let Kind::Decl(v, _) | Kind::MakeChain { name: v, .. } = &i.kind {
                    let name = self
                        .var_info(*v)
                        .map(|x| x.name.as_str())
                        .ok_or_else(|| Error::Internal(format!("no slot {}", v.0)))?;
                    let r = self.var(*v)?;
                    names.push((name, r));
                }
            }
        }
        let r = self.alloc();
        if names.is_empty() {
            self.abc(Op::LOADNULL, r, 0, 0)?;
        } else {
            self.abc(Op::NEWMAP, r, (names.len() as u32).min(0xFFFF), 0)?;
            let k = self.alloc();
            for (name, x) in names {
                let c = self.konst(ConstKey::Str(name));
                self.abx(Op::LOADK, k, c)?;
                self.abc(Op::INSERTMAP, r, k, x)?;
            }
        }
        self.abc(Op::RET, r, 0, 0)
    }

    // ----- registers -------------------------------------------------------------

    fn alloc(&mut self) -> u32 {
        match self.free_regs.pop_first() {
            Some(r) => r,
            None => {
                self.frame += 1;
                self.frame - 1
            }
        }
    }

    /// `n` consecutive free registers, the lowest run there is: a run of
    /// freed registers, or one that ends where the frame grows.
    fn alloc_window(&mut self, n: u32) -> u32 {
        let mut run: Option<(u32, u32)> = None;
        let mut first = None;
        for &r in &self.free_regs {
            run = match run {
                Some((s, len)) if s + len == r => Some((s, len + 1)),
                _ => Some((r, 1)),
            };
            if let Some((s, len)) = run {
                if len == n {
                    first = Some(s);
                    break;
                }
            }
        }
        let first = first.unwrap_or_else(|| match run {
            // A free run that reaches the frame's end continues past it.
            Some((s, len)) if s + len == self.frame => s,
            _ => self.frame,
        });
        for r in first..first + n {
            if !self.free_regs.remove(&r) && r >= self.frame {
                self.frame = r + 1;
            }
        }
        first
    }

    /// A `call`'s window: `n` consecutive registers above every register
    /// in use. The callee's frame starts at the window's second register
    /// and runs over everything above it, so no register that holds a
    /// value across the call may sit there (docs/BYTECODE.md, `call`).
    fn alloc_top(&mut self, n: u32) -> u32 {
        let mut top = self.frame;
        while top > 0 && self.free_regs.contains(&(top - 1)) {
            top -= 1;
        }
        for r in top..top + n {
            if !self.free_regs.remove(&r) && r >= self.frame {
                self.frame = r + 1;
            }
        }
        top
    }

    /// Whether `call` is lowered to a `call`: its window is a frame's start.
    fn makes_frame(&self, call: ir::InstrId) -> bool {
        self.instr_of
            .get(&call)
            .is_some_and(|i| matches!(i.kind, Kind::CallUser { .. } | Kind::CallValue { .. }))
    }

    /// `n` registers past every register used so far, never freed.
    fn fresh(&mut self, n: u32) -> u32 {
        let first = self.frame;
        self.frame += n;
        first
    }

    fn free(&mut self, r: u32) {
        if r < self.frame {
            self.free_regs.insert(r);
        }
    }

    fn var_info(&self, v: VarId) -> Option<&'m ir::VarInfo> {
        self.cx.m.vars.get(v.0 as usize)
    }

    /// A slot this function owns and some other function reads or writes:
    /// its register carries a cell.
    fn captured(&self, v: VarId) -> bool {
        self.var_info(v)
            .is_some_and(|x| x.captured && x.owner == self.f.id)
    }

    /// The register of a slot this function owns.
    fn var(&mut self, v: VarId) -> Result<u32, Error> {
        match self.slot(v)? {
            Slot::Reg(r) => Ok(r),
            Slot::Up(_) => Err(Error::Internal(format!(
                "slot {} is declared by a function that does not own it",
                v.0
            ))),
        }
    }

    fn slot(&mut self, v: VarId) -> Result<Slot, Error> {
        let owner = self.var_info(v).map(|x| x.owner);
        if owner != Some(self.f.id) {
            return match self.f.captures.iter().position(|c| *c == v) {
                Some(u) => Ok(Slot::Up(u as u32)),
                None => Err(Error::Internal(format!(
                    "slot {} of another function is not among this one's captures",
                    v.0
                ))),
            };
        }
        if let Some(r) = self.var_reg.get(&v) {
            return Ok(Slot::Reg(*r));
        }
        // A captured slot's register carries a cell until the frame ends:
        // a register no other value has used or will use, or a write to
        // that value would land in the cell the closures read.
        let r = if self.captured(v) {
            self.fresh(1)
        } else {
            self.alloc()
        };
        self.var_reg.insert(v, r);
        Ok(Slot::Reg(r))
    }

    /// `R(r) = slot`.
    fn read_slot(&mut self, r: u32, v: VarId) -> Result<(), Error> {
        match self.slot(v)? {
            Slot::Reg(s) => self.abc(Op::MOV, r, s, 0),
            Slot::Up(u) => self.abc(Op::GETUP, r, u, 0),
        }
    }

    fn val(&self, v: ValueId) -> Result<u32, Error> {
        self.val_reg
            .get(&v)
            .copied()
            .ok_or_else(|| Error::Internal(format!("%{} is read before it is defined", v.0)))
    }

    fn define(&mut self, i: &Instr) -> Result<u32, Error> {
        let v = i
            .result
            .ok_or_else(|| Error::Internal(format!("instruction {} has no result", i.id.0)))?;
        // A value block's register: taken already when its subject was
        // made in it, or now. Its value owns it when it was taken for the
        // block alone.
        if matches!(i.kind, Kind::Short { .. }) {
            let r = self.short_home(i.id)?;
            self.val_reg.insert(v, r);
            if self.short_reg.get(&i.id).is_some_and(|x| !x.1) {
                self.pinned.insert(v);
            }
            return Ok(r);
        }
        if let Some(r) = self.placed(v)? {
            self.val_reg.insert(v, r);
            self.pinned.insert(v);
            return Ok(r);
        }
        let r = self.alloc();
        self.val_reg.insert(v, r);
        Ok(r)
    }

    /// The register `plan` made a value's place, when it gave it one:
    /// the slot its `decl` or store writes, its place in a call's window,
    /// or the register of the value block that reads it. A register held
    /// for another value (the slot, the window, the block): the value
    /// never frees it.
    fn placed(&mut self, v: ValueId) -> Result<Option<u32>, Error> {
        if let Some(var) = self.dest.get(&v).copied() {
            if let Slot::Reg(r) = self.slot(var)? {
                return Ok(Some(r));
            }
        }
        if let Some((call, at, n)) = self.direct.get(&v).copied() {
            // The call's window is taken now, before the value is made:
            // no register in it is free again until the call.
            let w = match self.windows.get(&call) {
                Some(w) => *w,
                None => {
                    let w = if self.makes_frame(call) {
                        self.alloc_top(n)
                    } else {
                        self.alloc_window(n)
                    };
                    self.windows.insert(call, w);
                    w
                }
            };
            return Ok(Some(w + at));
        }
        if let Some(block) = self.into.get(&v).copied() {
            return self.short_home(block).map(Some);
        }
        Ok(None)
    }

    /// A value block's result register, taken the first time it is
    /// needed: its own value's place when that has one (a window, an
    /// outer block's register), else a register of its own. It is not
    /// free again until the block's value is dead.
    fn short_home(&mut self, block: ir::InstrId) -> Result<u32, Error> {
        if let Some((r, _)) = self.short_reg.get(&block) {
            return Ok(*r);
        }
        let own = self.instr_of.get(&block).and_then(|i| i.result);
        let (r, owned) = match own {
            Some(v) => match self.placed(v)? {
                Some(r) => (r, false),
                None => (self.alloc(), true),
            },
            None => (self.alloc(), true),
        };
        self.short_reg.insert(block, (r, owned));
        Ok(r)
    }

    /// What the lowering needs to know before it starts: the `a..b`
    /// values only a `for` reads.
    fn prepass(&mut self) {
        let mut range_def: HashMap<ValueId, (ValueId, ValueId, Span)> = HashMap::new();
        let mut for_iters = Vec::new();
        let mut uses: HashMap<ValueId, usize> = HashMap::new();
        walk(&self.f.body, &mut |i| {
            for u in i.kind.uses() {
                *uses.entry(u).or_default() += 1;
            }
            match &i.kind {
                Kind::Binary(BinOp::Range, a, b) => {
                    if let Some(v) = i.result {
                        range_def.insert(v, (*a, *b, i.span));
                    }
                }
                Kind::ForEach { iter, .. } => for_iters.push(*iter),
                _ => {}
            }
        });
        for it in for_iters {
            if uses.get(&it) == Some(&1) {
                if let Some(r) = range_def.get(&it) {
                    self.ranges.insert(it, *r);
                }
            }
        }
    }

    /// Where the lowering writes tighter code than one register per value
    /// and a copy per slot read, decided before it starts. Each rule says
    /// why the code it saves could not be observed:
    /// - a comparison (or a `not`) whose one reader is the `if` or the
    ///   loop condition's `yield` right after it is that branch's jump
    ///   (`fused`): nothing runs between them, and the jump runs the same
    ///   operator on the same operands, in the same order, at the
    ///   comparison's position;
    /// - a binary operation with a constant right operand takes it as `K`
    ///   (`kform`); one with a constant left operand does too when the
    ///   operation gives the same value and the same error either way
    ///   round (`==` and `!=`, which never fail; `+` and `*` of two
    ///   numbers, whose only error, `E503`, names the operator alone). A
    ///   constant every reader takes as `K` is never loaded (`unloaded`);
    /// - a read of a slot this function owns and no function captures is
    ///   the slot's register itself (`alias`) when every reader is in the
    ///   same block, after it, and nothing from the read to the last
    ///   reader writes the slot. Only an instruction of this function can
    ///   write such a slot (a `decl` or a store, a `for` item, a catch
    ///   binding, a burn's state, a chain's name): a call cannot, nor can a
    ///   closure. The last reader may write it after reading it (a store,
    ///   an `if`'s branches, a value block's right side, a `for`'s body);
    /// - a value whose one reader is the `decl` or store right after it,
    ///   into such a slot, is made in the slot's register (`dest`). The
    ///   operations that make it write their result only once they have
    ///   it: one that fails leaves the slot as it was, as the tree-walker
    ///   does. So is a value with other readers too (a `stick` -O1
    ///   forwards) when each of them reads the slot as it was stored: it
    ///   sits after the store in the same block, or in a block nested in
    ///   a later instruction of it that writes the slot nowhere, and
    ///   nothing from the store to the last of them writes the slot (the
    ///   last may write it after reading it, as for a read with no copy).
    ///   They read the slot's register;
    /// - a value whose one reader is a call it is an argument of, made in
    ///   the call's block, is made in its place in the call's window
    ///   (`direct`): the window is taken when the first such argument is
    ///   made, so it exists before the argument is evaluated, and none of
    ///   its registers is free until the call, so nothing between the
    ///   argument's making and the call writes them (no other value, no
    ///   other call's window; a slot's register is never in a window, a
    ///   captured one's least of all). The callee or receiver goes into
    ///   the window's first register at the call, as before; a value read
    ///   with no copy (a slot's own register) is moved, as before. A window
    ///   register is a scratch register no one reads but the call, so one
    ///   made there by an operation that fails is never seen. A callee
    ///   (a call by value) or a receiver read only by its call is made in
    ///   the window's first register by the same rule;
    /// - a value block's subject, and the value its right side yields,
    ///   each read only by the block and made in the block before it, is
    ///   made in the block's result register (`into`): the register is
    ///   taken when the first of them is made and is not free until the
    ///   block's value is dead, and nothing but the block reads it before
    ///   the block ends. A right side that fails leaves it half-written
    ///   and unread. A block is never made in a slot (a store there would
    ///   be seen), only in a window or another block's register;
    /// - a constant inside a loop that some reader takes from a register,
    ///   not made in a slot or a window already, with every reader inside
    ///   the outermost loop around it, is loaded once before that loop
    ///   (`hoist`), one register per literal, taken with `alloc` (as a
    ///   slot is: live, never past the frame's top) and freed after the
    ///   loop's last instruction. A literal is the same value every turn
    ///   and loading it cannot fail, charge a step or be seen; no value,
    ///   window or instruction is given or writes that register while
    ///   the loop runs, and a window placed above the live registers is
    ///   placed above it. A loop run no times still loads it, unseen. A
    ///   register past 255 is given back and the constant made where it
    ///   is read: an operand there would be wide, a fused jump long.
    fn plan(&mut self) {
        let f = self.f;
        let mut readers: HashMap<ValueId, Vec<ir::InstrId>> = HashMap::new();
        walk(&f.body, &mut |i| {
            for u in i.kind.uses() {
                readers.entry(u).or_default().push(i.id);
            }
        });
        let reader_count = |v: ValueId| readers.get(&v).map_or(0, Vec::len);
        let mut blocks: Vec<(&'m Block, bool)> = Vec::new();
        all_blocks(&f.body, false, &mut blocks);
        let mut at: HashMap<ir::InstrId, (usize, usize)> = HashMap::new();
        for (n, (b, _)) in blocks.iter().enumerate() {
            for (j, i) in b.instrs.iter().enumerate() {
                at.insert(i.id, (n, j));
            }
        }
        let def_of = |v: ValueId| {
            f.vals
                .get(v.0 as usize)
                .and_then(|x| self.instr_of.get(&x.def).copied())
        };
        let ty = |v: ValueId| f.vals.get(v.0 as usize).map_or(Ty::Dynamic, |x| x.ty);
        let own = |v: VarId| {
            self.var_info(v)
                .is_some_and(|x| x.owner == f.id && !x.captured)
        };

        // Comparisons and `not`s read only by the branch right after them.
        let mut fused = HashMap::new();
        for (b, is_cond) in &blocks {
            for w in b.instrs.windows(2) {
                let (i, next) = (&w[0], &w[1]);
                let Some(r) = i.result else { continue };
                let compare = matches!(
                    i.kind,
                    Kind::Binary(
                        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge,
                        ..
                    ) | Kind::Unary(UnOp::Not, _)
                );
                if !compare || reader_count(r) != 1 {
                    continue;
                }
                let branch = match &next.kind {
                    Kind::If { cond, .. } => *cond == r,
                    Kind::Yield(v) => *is_cond && *v == r,
                    _ => false,
                };
                if branch {
                    fused.insert(r, next.id);
                }
            }
        }

        // Constant operands.
        let konst = |v: ValueId| {
            matches!(
                def_of(v).map(|d| &d.kind),
                Some(Kind::Const(
                    Const::Bool(_) | Const::Int(_) | Const::Float(_) | Const::Str(_)
                ))
            )
        };
        let numeric = |v: ValueId| {
            matches!(
                def_of(v).map(|d| &d.kind),
                Some(Kind::Const(Const::Int(_) | Const::Float(_)))
            )
        };
        let mut kform = HashMap::new();
        let mut kreads: HashMap<ValueId, usize> = HashMap::new();
        walk(&f.body, &mut |i| {
            let Kind::Binary(op, a, b) = &i.kind else {
                return;
            };
            if i.result.is_some_and(|r| fused.contains_key(&r)) || k_op(*op).is_none() {
                return;
            }
            if konst(*b) {
                kform.insert(i.id, false);
                *kreads.entry(*b).or_default() += 1;
            } else if konst(*a)
                && match op {
                    BinOp::Eq | BinOp::Ne => true,
                    BinOp::Add | BinOp::Mul => numeric(*a) && matches!(ty(*b), Ty::Int | Ty::Float),
                    _ => false,
                }
            {
                kform.insert(i.id, true);
                *kreads.entry(*a).or_default() += 1;
            }
        });
        let unloaded: HashSet<ValueId> = kreads
            .into_iter()
            .filter(|(v, n)| reader_count(*v) == *n)
            .map(|(v, _)| v)
            .collect();

        // The instruction each nested instruction sits in.
        let mut parent: HashMap<ir::InstrId, ir::InstrId> = HashMap::new();
        walk(&f.body, &mut |i| {
            for nested in i.kind.blocks() {
                for x in &nested.instrs {
                    parent.insert(x.id, i.id);
                }
            }
        });
        // Where every reader of `v` reads it, when that is in block `n`
        // after its `j`-th instruction: the last of those places. A fused
        // comparison reads at its branch, a `for`'s range at the `for`; a
        // burn reads a register label again each time it enters, so it
        // keeps its copy.
        let last_reader = |v: ValueId, n: usize, j: usize| -> Option<usize> {
            let b = blocks[n].0;
            let mut last = j;
            for r in readers.get(&v).into_iter().flatten() {
                let reader = self.instr_of.get(r).copied();
                if reader.is_some_and(|x| matches!(x.kind, Kind::Burn(_))) {
                    return None;
                }
                let eff = match reader {
                    Some(x) if x.result.is_some_and(|r| fused.contains_key(&r)) => {
                        x.result.and_then(|r| fused.get(&r).copied())
                    }
                    Some(x)
                        if matches!(x.kind, Kind::Binary(BinOp::Range, ..))
                            && x.result.is_some_and(|r| self.ranges.contains_key(&r)) =>
                    {
                        b.instrs.iter().find_map(|y| match y.kind {
                            Kind::ForEach { iter, .. } if Some(iter) == x.result => Some(y.id),
                            _ => None,
                        })
                    }
                    Some(x) => Some(x.id),
                    None => None,
                };
                match eff.and_then(|e| at.get(&e)) {
                    Some(&(m, k)) if m == n && k > j => last = last.max(k),
                    _ => return None,
                }
            }
            Some(last)
        };
        // The same for a value made in a slot: a reader may also sit in a
        // block nested in a later instruction of block `n`, which then
        // must not write the slot anywhere (a loop runs it again). The
        // last place, and the places whose nested readers need that.
        let slot_readers = |v: ValueId, n: usize, j: usize| -> Option<(usize, Vec<usize>)> {
            let mut last = j;
            let mut nested = Vec::new();
            for r in readers.get(&v).into_iter().flatten() {
                let reader = self.instr_of.get(r).copied()?;
                if matches!(reader.kind, Kind::Burn(_)) {
                    return None;
                }
                let mut eff = match reader.result.and_then(|x| fused.get(&x)) {
                    Some(branch) => *branch,
                    None => reader.id,
                };
                // A `for`'s range: read at the `for`, in the range's block.
                if let (Kind::Binary(BinOp::Range, ..), Some(x)) = (&reader.kind, reader.result) {
                    if self.ranges.contains_key(&x) {
                        let &(m, _) = at.get(&reader.id)?;
                        eff = blocks[m].0.instrs.iter().find_map(|y| match y.kind {
                            Kind::ForEach { iter, .. } if iter == x => Some(y.id),
                            _ => None,
                        })?;
                    }
                }
                let mut deep = false;
                while at.get(&eff).is_some_and(|&(m, _)| m != n) {
                    eff = *parent.get(&eff)?;
                    deep = true;
                }
                match at.get(&eff) {
                    Some(&(m, k)) if m == n && k > j => {
                        last = last.max(k);
                        if deep {
                            nested.push(k);
                        }
                    }
                    _ => return None,
                }
            }
            Some((last, nested))
        };
        // Nothing after the `from`-th instruction of block `n` and before
        // its `last`-th writes `var`, and the `last`-th reads before it
        // writes it.
        let unwritten = |var: VarId, n: usize, from: usize, last: usize| {
            let b = blocks[n].0;
            !b.instrs[from + 1..last]
                .iter()
                .any(|x| writes(x, var, true))
                && !writes(&b.instrs[last], var, false)
        };

        // Slot reads with no copy.
        let mut alias = HashSet::new();
        for (n, (b, _)) in blocks.iter().enumerate() {
            for (j, i) in b.instrs.iter().enumerate() {
                let (Kind::Load(var), Some(v)) = (&i.kind, i.result) else {
                    continue;
                };
                if !own(*var) {
                    continue;
                }
                match last_reader(v, n, j) {
                    Some(last) if last > j && unwritten(*var, n, j, last) => {
                        alias.insert(v);
                    }
                    _ => {}
                }
            }
        }

        // Values made in the slot the `decl` or store right after them
        // writes: their one reader, or the first of several whose others
        // all read the slot after it, as a read of the slot with no copy
        // would.
        let mut dest = HashMap::new();
        for (n, (b, _)) in blocks.iter().enumerate() {
            for (j, w) in b.instrs.windows(2).enumerate() {
                let (i, next) = (&w[0], &w[1]);
                let Some(v) = i.result else { continue };
                let (Kind::Decl(var, x) | Kind::Store(var, x)) = &next.kind else {
                    continue;
                };
                if *x != v || !own(*var) {
                    continue;
                }
                let makes = match &i.kind {
                    Kind::Const(_) => !unloaded.contains(&v),
                    Kind::Load(_) => !alias.contains(&v),
                    Kind::Unary(..) => !fused.contains_key(&v),
                    Kind::Binary(op, ..) => {
                        !matches!(op, BinOp::Range | BinOp::And | BinOp::Or | BinOp::Coalesce)
                            && !fused.contains_key(&v)
                    }
                    // `getindex` and `getfield` read their operands, then
                    // write their result only once they have it.
                    Kind::Index { .. } | Kind::Member { safe: false, .. } => true,
                    _ => false,
                };
                if !makes {
                    continue;
                }
                let one = reader_count(v) == 1;
                let many = || match slot_readers(v, n, j) {
                    Some((last, nested)) if last > j + 1 => {
                        unwritten(*var, n, j + 1, last)
                            && nested.iter().all(|k| !writes(&b.instrs[*k], *var, true))
                    }
                    _ => false,
                };
                if one || many() {
                    dest.insert(v, *var);
                }
            }
        }
        // An instruction that writes only its result register, and only
        // that one value into it (a value block writes its subject, then
        // maybe its right side's value: never seen before the block ends),
        // so the value can be made in a register held for another.
        let makes = |def: &Instr, a: ValueId| match &def.kind {
            Kind::Const(_) => !unloaded.contains(&a),
            Kind::Load(var) => {
                !alias.contains(&a) && !self.var_info(*var).is_some_and(|x| x.captured)
            }
            Kind::Unary(..) => !fused.contains_key(&a),
            Kind::Binary(op, ..) => {
                !matches!(op, BinOp::Range | BinOp::And | BinOp::Or | BinOp::Coalesce)
                    && !fused.contains_key(&a)
            }
            Kind::Index { .. }
            | Kind::Member { .. }
            | Kind::Closure(_)
            | Kind::Prelude(_)
            | Kind::MakeList(_)
            | Kind::MakeMap(_)
            | Kind::Interp(_)
            | Kind::Short { .. } => true,
            _ => false,
        };
        // A value's one reader is in block `b` at `j`, and the value is
        // made earlier in that block, by an instruction that can make it
        // in another's register.
        let made_before = |b: &Block, j: usize, a: ValueId| {
            reader_count(a) == 1
                && !dest.contains_key(&a)
                && b.instrs[..j]
                    .iter()
                    .find(|x| x.result == Some(a))
                    .is_some_and(|def| makes(def, a))
        };
        // Arguments, callees and receivers made in their call's window.
        let mut direct = HashMap::new();
        for (b, _) in &blocks {
            for (j, call) in b.instrs.iter().enumerate() {
                let (callee, args) = match &call.kind {
                    Kind::CallBuiltin { args, .. }
                    | Kind::CallUser { args, .. }
                    | Kind::BurnOp { args, .. } => (None, args),
                    Kind::CallValue { f, args } => (Some(*f), args),
                    Kind::CallMethod { recv, args, .. } => (Some(*recv), args),
                    _ => continue,
                };
                let n = args.len() as u32 + 1;
                let at = callee
                    .into_iter()
                    .map(|c| (c, 0))
                    .chain(args.iter().enumerate().map(|(k, a)| (*a, 1 + k as u32)));
                for (a, at) in at {
                    if made_before(b, j, a) {
                        direct.insert(a, (call.id, at, n));
                    }
                }
            }
        }
        // A value block's subject, and the value its right side yields,
        // made in the block's result register.
        let mut into = HashMap::new();
        for (b, _) in &blocks {
            for (j, block) in b.instrs.iter().enumerate() {
                let Kind::Short { subject, rhs, .. } = &block.kind else {
                    continue;
                };
                if made_before(b, j, *subject) {
                    into.insert(*subject, block.id);
                }
                for (k, y) in rhs.instrs.iter().enumerate() {
                    if let Kind::Yield(v) = y.kind {
                        if made_before(rhs, k, v) {
                            into.insert(v, block.id);
                        }
                    }
                }
            }
        }
        // Constants made once, before the outermost loop around them.
        let mut outer_of: HashMap<ir::InstrId, Option<ir::InstrId>> = HashMap::new();
        loops_around(&f.body, None, &mut outer_of);
        let mut hoist = HashMap::new();
        let mut hoist_at: HashMap<ir::InstrId, Vec<Lit<'m>>> = HashMap::new();
        walk(&f.body, &mut |i| {
            let (Kind::Const(c), Some(v)) = (&i.kind, i.result) else {
                return;
            };
            let Some(Some(l)) = outer_of.get(&i.id).copied() else {
                return;
            };
            if unloaded.contains(&v)
                || dest.contains_key(&v)
                || direct.contains_key(&v)
                || into.contains_key(&v)
            {
                return;
            }
            let inside = readers
                .get(&v)
                .into_iter()
                .flatten()
                .all(|r| outer_of.get(r) == Some(&Some(l)));
            if !inside {
                return;
            }
            let lit = Lit::of(c);
            hoist.insert(v, (l, lit));
            let lits = hoist_at.entry(l).or_default();
            if !lits.contains(&lit) {
                lits.push(lit);
            }
        });
        self.fused = fused;
        self.kform = kform;
        self.unloaded = unloaded;
        self.alias = alias;
        self.dest = dest;
        self.direct = direct;
        self.into = into;
        self.hoist = hoist;
        self.hoist_at = hoist_at;
    }

    /// Every value's last use, as the position after which its register
    /// is free. A use inside a loop of a value defined before the loop
    /// keeps it to the loop's end: the next turn reads it again.
    fn liveness(&mut self) {
        let mut last: HashMap<ValueId, usize> = HashMap::new();
        let def_pos = |v: ValueId, start: &HashMap<ir::InstrId, usize>| {
            self.f
                .vals
                .get(v.0 as usize)
                .and_then(|x| start.get(&x.def))
                .copied()
                .unwrap_or(0)
        };
        let mut uses: Vec<(ValueId, usize)> = Vec::new();
        // A fused comparison reads its operands at its branch.
        let remap: HashMap<ir::InstrId, usize> = self
            .fused
            .iter()
            .filter_map(|(r, at)| {
                let def = self.f.vals.get(r.0 as usize)?.def;
                Some((def, *self.start.get(at)?))
            })
            .collect();
        collect_uses(
            &self.f.body,
            &self.start,
            &self.end,
            &self.ranges,
            &remap,
            &mut Vec::new(),
            &mut |v, pos, loops: &[LoopRange]| {
                uses.push((v, pos));
                let d = def_pos(v, &self.start);
                // The outermost enclosing loop that started after the
                // definition.
                if let Some(&(_, e)) = loops.iter().find(|(s, _)| *s > d) {
                    uses.push((v, e));
                }
            },
        );
        for (v, p) in uses {
            let e = last.entry(v).or_insert(p);
            *e = (*e).max(p);
        }
        for (i, info) in self.f.vals.iter().enumerate() {
            let v = ValueId(i as u32);
            let at = match last.get(&v) {
                Some(p) => *p,
                None => match self.start.get(&info.def) {
                    Some(s) => *s,
                    None => continue,
                },
            };
            self.dies_at.entry(at).or_default().push(v);
        }
    }

    fn release(&mut self, i: &Instr) {
        for pos in [self.start.get(&i.id), self.end.get(&i.id)]
            .into_iter()
            .flatten()
            .copied()
            .collect::<Vec<_>>()
        {
            if let Some(vs) = self.dies_at.remove(&pos) {
                for v in vs {
                    if let Some(r) = self.val_reg.remove(&v) {
                        if !self.pinned.remove(&v) {
                            self.free(r);
                        }
                    }
                }
            }
        }
    }

    // ----- emitting ----------------------------------------------------------------

    fn row(&mut self) {
        self.rows.push((self.code.len() as u32, self.span));
    }

    fn abc(&mut self, op: Op, a: u32, b: u32, c: u32) -> Result<(), Error> {
        if a > 0xFFFF || b > 0xFFFF || c > 0xFFFF {
            return Err(self.too_wide(op));
        }
        self.row();
        if a > 0xFF || b > 0xFF || c > 0xFF {
            self.code.push(wide_prefix(
                (a >> 8) as u8,
                (((c >> 8) << 8) | (b >> 8)) as u16,
            ));
        }
        self.code.push(Word::abc(op, a as u8, b as u8, c as u8));
        Ok(())
    }

    fn abx(&mut self, op: Op, a: u32, bx: u32) -> Result<(), Error> {
        if a > 0xFFFF {
            return Err(self.too_wide(op));
        }
        self.row();
        if a > 0xFF || bx > 0xFFFF {
            self.code
                .push(wide_prefix((a >> 8) as u8, (bx >> 16) as u16));
        }
        self.code.push(Word::abx(op, a as u8, bx as u16));
        Ok(())
    }

    fn too_wide(&self, op: Op) -> Error {
        let what = if self.frame > MAX_REG_COUNT {
            format!("the function needs more than {MAX_REG_COUNT} registers, the frame limit")
        } else {
            format!("`{op}` needs an operand wider than 16 bits")
        };
        Error::Limit {
            what,
            span: self.span,
        }
    }

    fn label(&mut self) -> Label {
        self.labels.push(None);
        Label(self.labels.len() - 1)
    }

    fn place(&mut self, l: Label) {
        self.labels[l.0] = Some(self.code.len());
        self.last_placed = Some(self.code.len());
    }

    /// `steps k, n`. A `steps k, 1` right after a `steps k, 0` (a loop's
    /// end, then the next statement's mark) is that one instruction,
    /// `steps k, 1` at the mark's position: the first only sets the
    /// nesting the second sets again, charges nothing and raises nothing,
    /// so nothing can be observed between them, and no run reaches the
    /// second but through the first (no label sits on it).
    fn steps(&mut self, k: u32, n: u32) -> Result<(), Error> {
        let pc = self.code.len();
        if n == 1 && self.last_placed != Some(pc) {
            if let Some((p, j)) = self.steps0 {
                if p + 1 == pc && j == k {
                    self.code[p] = Word::abx(Op::STEPS, k as u8, 1);
                    if let Some(last) = self.rows.last_mut() {
                        if last.0 as usize == p {
                            last.1 = self.span;
                        }
                    }
                    self.steps0 = None;
                    return Ok(());
                }
            }
        }
        self.abx(Op::STEPS, k, n)?;
        self.steps0 = (n == 0 && k <= 0xFF).then_some((pc, k));
        Ok(())
    }

    /// An AsBx jump (`jmp`, `loop`, `test`, `testn`) to `target`.
    fn jump(&mut self, op: Op, a: u32, target: Label) -> Result<(), Error> {
        if a > 0xFFFF {
            return Err(self.too_wide(op));
        }
        self.row();
        let wide = self.wide_jumps || a > 0xFF;
        let pc = self.code.len();
        if wide {
            self.code.push(wide_prefix(0, 0));
        }
        self.code.push(Word::asbx(op, 0, 0));
        self.patches.push(Patch {
            pc,
            op,
            a,
            wide,
            target,
            fused: None,
        });
        Ok(())
    }

    /// A fused compare-and-jump (`eqj`, `nej`, `ltj`, `lej`) to `target`,
    /// its signed 8-bit offset written once the target is placed.
    fn fused_jump(&mut self, op: Op, a: u32, b: u32, target: Label, site: usize) {
        self.row();
        let pc = self.code.len();
        self.code.push(Word::abc(op, a as u8, b as u8, 0));
        self.patches.push(Patch {
            pc,
            op,
            a,
            wide: false,
            target,
            fused: Some((b, site)),
        });
    }

    /// Go to `to` unless the value `v` is truthy: a comparison read only
    /// here is a fused compare-and-jump, a `not` read only here tests its
    /// operand, anything else is `testn`.
    fn leave_unless(&mut self, v: ValueId, to: Label) -> Result<(), Error> {
        if let Some(c) = self.pending.remove(&v) {
            return self.compare_jump(c, to);
        }
        if let Some(x) = self.pending_not.remove(&v) {
            let x = self.val(x)?;
            return self.jump(Op::TEST, x, to);
        }
        let c = self.val(v)?;
        self.jump(Op::TESTN, c, to)
    }

    /// Go to `to` unless the comparison holds. The operator runs on the
    /// operands in their order, at the comparison's position, as `eq`,
    /// `lt` and `le` run it; there is no `gt`/`ge`, so `a > b` leaves when
    /// `a <= b` holds (`lej`) and `a >= b` when `a < b` does (`ltj`): the
    /// same operator on the same operands, and a NaN raises either way.
    /// `a < b` and `a <= b` jump over a `jmp` to `to` when they hold. A
    /// register past 255, or a target too far for the 8-bit offset, takes
    /// the long form: the comparison into a register, then `test` or
    /// `testn`.
    fn compare_jump(&mut self, c: PendingCmp, to: Label) -> Result<(), Error> {
        let (a, b) = (self.val(c.a)?, self.val(c.b)?);
        let keep = self.span;
        let narrow = a <= 0xFF && b <= 0xFF;
        let (base, leave_if) = match c.op {
            BinOp::Eq => (Op::EQ, false),
            BinOp::Ne => (Op::EQ, true),
            BinOp::Lt => (Op::LT, false),
            BinOp::Le => (Op::LE, false),
            BinOp::Gt => (Op::LE, true),
            BinOp::Ge => (Op::LT, true),
            op => {
                return Err(Error::Internal(format!(
                    "`{}` as a branch's comparison",
                    op.symbol()
                )))
            }
        };
        self.span = c.span;
        let r = if narrow && matches!(c.op, BinOp::Lt | BinOp::Le) {
            // Over the `jmp`: one word, or two with a `WIDE` prefix.
            let over = if self.wide_jumps { 2 } else { 1 };
            let op = if c.op == BinOp::Lt { Op::LTJ } else { Op::LEJ };
            self.abc(op, a, b, over).and_then(|()| {
                self.span = keep;
                self.jump(Op::JMP, 0, to)
            })
        } else {
            let site = self.fused_sites;
            if narrow {
                self.fused_sites += 1;
            }
            if narrow && !self.far.contains(&site) {
                let op = match (base, leave_if) {
                    (Op::EQ, false) => Op::NEJ,
                    (Op::EQ, true) => Op::EQJ,
                    (Op::LT, _) => Op::LTJ,
                    _ => Op::LEJ,
                };
                self.fused_jump(op, a, b, to, site);
                Ok(())
            } else {
                let t = self.alloc();
                self.abc(base, t, a, b).and_then(|()| {
                    self.span = keep;
                    self.jump(if leave_if { Op::TEST } else { Op::TESTN }, t, to)
                })?;
                self.free(t);
                Ok(())
            }
        };
        self.span = keep;
        r
    }

    /// `a op K` with a constant operand. A constant index past 16 bits (a
    /// pool that large) takes the register form, the constant loaded.
    fn binary_k(&mut self, i: &'m Instr, op: BinOp, x: ValueId, k: ValueId) -> Result<(), Error> {
        let key = match self
            .f
            .vals
            .get(k.0 as usize)
            .and_then(|v| self.instr_of.get(&v.def))
            .map(|d| &d.kind)
        {
            Some(Kind::Const(Const::Bool(b))) => ConstKey::Bool(*b),
            Some(Kind::Const(Const::Int(n))) => ConstKey::Int(*n),
            Some(Kind::Const(Const::Float(f))) => ConstKey::Float(f.to_bits()),
            Some(Kind::Const(Const::Str(s))) => ConstKey::Str(s),
            _ => return Err(Error::Internal(format!("%{} is not a constant", k.0))),
        };
        let (code, negate) =
            k_op(op).ok_or_else(|| Error::Internal("a binary operator with no K form".into()))?;
        let xr = self.val(x)?;
        let c = self.konst(key);
        let r = self.define(i)?;
        if c <= 0xFFFF {
            self.abc(code, r, xr, c)?;
        } else {
            // The register form, operands in the source's order.
            let kr = match self.val_reg.get(&k) {
                Some(kr) => (*kr, false),
                None => {
                    let t = self.alloc();
                    self.abx(Op::LOADK, t, c)?;
                    (t, true)
                }
            };
            let (l, rr) = match i.kind {
                Kind::Binary(_, a, _) if a == k => (kr.0, xr),
                _ => (xr, kr.0),
            };
            let base = match code {
                Op::ADDK => Op::ADD,
                Op::SUBK => Op::SUB,
                Op::MULK => Op::MUL,
                Op::DIVK => Op::DIV,
                Op::MODK => Op::MOD,
                Op::EQK => Op::EQ,
                Op::LTK => Op::LT,
                _ => Op::LE,
            };
            self.abc(base, r, l, rr)?;
            if kr.1 {
                self.free(kr.0);
            }
        }
        if negate {
            self.abc(Op::NOT, r, r, 0)?;
        }
        Ok(())
    }

    fn patch(&mut self) -> Result<(), Retry> {
        let mut far = Vec::new();
        for p in &self.patches {
            let target = self.labels[p.target.0]
                .ok_or_else(|| Error::Internal("a jump to a label never placed".into()))?;
            let off = target as i64 - p.pc as i64 - 1;
            if let Some((b, site)) = p.fused {
                match i8::try_from(off) {
                    Ok(o) => self.code[p.pc] = Word::abc(p.op, p.a as u8, b as u8, o as u8),
                    Err(_) => far.push(site),
                }
            } else if p.wide {
                let off = i32::try_from(off).map_err(|_| Error::Limit {
                    what: "a jump past 32 bits".into(),
                    span: self.f.span,
                })?;
                let bits = off as u32;
                self.code[p.pc] = wide_prefix((p.a >> 8) as u8, (bits >> 16) as u16);
                self.code[p.pc + 1] = Word::asbx(p.op, p.a as u8, (bits & 0xFFFF) as u16 as i16);
            } else {
                let Ok(off) = i16::try_from(off) else {
                    return Err(Retry::Wide);
                };
                self.code[p.pc] = Word::asbx(p.op, p.a as u8, off);
            }
        }
        if !far.is_empty() {
            return Err(Retry::Far(far));
        }
        Ok(())
    }

    fn konst(&mut self, k: ConstKey<'m>) -> u32 {
        if let Some(i) = self.const_index.get(&k) {
            return *i;
        }
        let i = self.consts.len() as u32;
        self.consts.push(match k {
            ConstKey::Bool(x) => Constant::Bool(x),
            ConstKey::Int(x) => Constant::Int(x),
            ConstKey::Float(b) => Constant::Float(f64::from_bits(b)),
            ConstKey::Str(s) => Constant::Str(s.to_string()),
        });
        self.const_index.insert(k, i);
        i
    }

    /// The line table: a row wherever the position changes from the
    /// instruction before, relative to the function's first position.
    fn line_table(&self) -> (SpanBase, Vec<LineEntry>) {
        let first = self
            .rows
            .iter()
            .map(|r| r.1)
            .chain(std::iter::once(self.f.span))
            // By line and column: an interpolated piece's byte offsets
            // count from its string, not from the file.
            .min_by_key(|s| (s.line, s.col))
            .unwrap_or(self.f.span);
        let base = SpanBase {
            byte_offset: first.start as u32,
            line: first.line,
            col: first.col,
        };
        let mut lines: Vec<LineEntry> = Vec::new();
        let mut prev: Option<Span> = None;
        for (pc, s) in &self.rows {
            if prev == Some(*s) || lines.last().is_some_and(|l| l.pc == *pc) {
                continue;
            }
            prev = Some(*s);
            lines.push(LineEntry {
                pc: *pc,
                span_off: (s.start as u32).saturating_sub(base.byte_offset),
                span_len: (s.end.saturating_sub(s.start)) as u32,
                line_off: s.line.saturating_sub(base.line),
                col: s.col,
            });
        }
        (base, lines)
    }

    fn nest(&self) -> u32 {
        self.loops.len() as u32
    }

    // ----- lowering -----------------------------------------------------------------

    fn block(&mut self, b: &'m Block) -> Result<(), Error> {
        // A captured slot declared here gets a fresh cell each time the
        // block runs (each turn of a loop), before anything, a hoisted
        // pull included, can capture it.
        let mut fresh = HashSet::new();
        for i in &b.instrs {
            if let Kind::Decl(v, _) | Kind::MakeChain { name: v, .. } = &i.kind {
                if self.captured(*v) && fresh.insert(*v) {
                    self.span = i.span;
                    // The new cell first: a null written to the
                    // register before it would land in the last turn's
                    // cell, which that turn's closures still read.
                    let r = self.var(*v)?;
                    self.abc(Op::CELLNEW, r, 0, 0)?;
                    self.abc(Op::LOADNULL, r, 0, 0)?;
                }
            }
        }
        for j in 0..b.instrs.len() {
            self.schedule_shows(b, j);
            self.schedule_checks(b, j);
        }
        for i in &b.instrs {
            self.instr(i)?;
            self.release(i);
        }
        Ok(())
    }

    fn refuse(&self, construct: impl Into<String>, span: Span) -> Error {
        Error::Refused {
            construct: construct.into(),
            span,
        }
    }

    fn instr(&mut self, i: &'m Instr) -> Result<(), Error> {
        // The constants of a loop nest, made once before it, each in a
        // register held until the loop is done.
        let lits = self.hoist_at.remove(&i.id).unwrap_or_default();
        for lit in &lits {
            let r = self.alloc();
            if r > 0xFF {
                // An operand past 255 is a wider instruction, and a
                // fused jump the long form: made where it is read.
                self.free(r);
                break;
            }
            self.span = i.span;
            self.load_lit(r, *lit)?;
            self.hoisted.insert((i.id, *lit), r);
        }
        self.instr_inner(i)?;
        for lit in &lits {
            if let Some(r) = self.hoisted.remove(&(i.id, *lit)) {
                self.free(r);
            }
        }
        Ok(())
    }

    fn instr_inner(&mut self, i: &'m Instr) -> Result<(), Error> {
        if let Some(vs) = self.show_before.remove(&i.id) {
            self.span = i.span;
            self.show_as_text(&vs)?;
        }
        if let Some(cs) = self.check_before.remove(&i.id) {
            self.span = i.span;
            self.check_parts(&cs)?;
        }
        self.instr_kind(i)?;
        if let Some(vs) = self.show_after.remove(&i.id) {
            self.span = i.span;
            self.show_as_text(&vs)?;
        }
        if let Some(cs) = self.check_after.remove(&i.id) {
            self.span = i.span;
            self.check_parts(&cs)?;
        }
        Ok(())
    }

    /// The tree-walker checks each `pack` root and chain step as soon as it
    /// is evaluated; `pack` and `chain.make` check them all once all are
    /// evaluated. So every part but the last is checked where the
    /// tree-walker checks it (after its own code, before the next part's),
    /// and a part that fails goes straight to the instruction, which raises
    /// that part's error, the first it finds, at the part's position. A
    /// root that is a non-empty string constant needs no check. For a
    /// `pack` whose roots run script code (a pull, a function value), the
    /// statement is marked: such a root can burn, and the VM refuses a
    /// `pack` after anything was journaled where the tree-walker does not.
    fn schedule_checks(&mut self, b: &'m Block, j: usize) {
        let i = &b.instrs[j];
        let (parts, part): (Vec<ValueId>, Part) = match &i.kind {
            Kind::PackDecl(vs) => (vs.clone(), Part::Root),
            Kind::MakeChain { steps, .. } => (steps.iter().map(|s| s.1).collect(), Part::Step),
            _ => return,
        };
        // The parts' own code: from the statement's step mark on.
        let k = (0..j)
            .rev()
            .find(|&k| matches!(b.instrs[k].kind, Kind::Step(_)))
            .map_or(0, |k| k + 1);
        let code = &b.instrs[k..j];
        if matches!(part, Part::Root) {
            let mut runs = false;
            for c in code {
                walk_one(c, &mut |x| {
                    runs |= matches!(
                        x.kind,
                        Kind::CallUser { .. }
                            | Kind::CallValue { .. }
                            | Kind::Closure(_)
                            | Kind::Load(_)
                    );
                });
            }
            if runs {
                self.pack_runs_code.insert(i.id);
                return;
            }
        }
        if parts.len() < 2 {
            return;
        }
        let to = self.label();
        self.part_to.insert(i.id, to);
        let def_of = |v: &ValueId| self.f.vals.get(v.0 as usize).map(|x| x.def);
        let mut anchor: Option<ir::InstrId> = None;
        for (n, v) in parts.iter().enumerate() {
            let def = def_of(v).filter(|d| code.iter().any(|c| c.id == *d));
            if n + 1 < parts.len() && !self.known_good(*v, part) {
                let at = PartCheck { v: *v, part, to };
                match (def, anchor, code.first()) {
                    (Some(d), _, _) => self.check_after.entry(d).or_default().push(at),
                    (None, Some(a), _) => self.check_after.entry(a).or_default().push(at),
                    (None, None, Some(c)) => self.check_before.entry(c.id).or_default().push(at),
                    // Nothing is evaluated between the parts.
                    (None, None, None) => {}
                }
            }
            if def.is_some() {
                anchor = def;
            }
        }
    }

    /// A part whose check cannot fail: a root that is a non-empty string
    /// constant, a step that is a function.
    fn known_good(&self, v: ValueId, part: Part) -> bool {
        let Some(info) = self.f.vals.get(v.0 as usize) else {
            return false;
        };
        match part {
            Part::Root => matches!(
                self.instr_of.get(&info.def).map(|i| &i.kind),
                Some(Kind::Const(Const::Str(s))) if !s.trim().is_empty()
            ),
            Part::Step => info.ty == Ty::Func,
        }
    }

    /// Check parts as the tree-walker does (`pack_root`, `chain_step`),
    /// going to the instruction when one fails.
    fn check_parts(&mut self, cs: &[PartCheck]) -> Result<(), Error> {
        for c in cs {
            let x = self.val(c.v)?;
            let t = self.type_name_of(x)?;
            match c.part {
                Part::Root => {
                    self.jump_if_is(t, "string", c.to, false)?;
                    let w = self.alloc();
                    self.abc(Op::MOV, w, x, 0)?;
                    let trim = self.konst(ConstKey::Str("trim"));
                    self.abc(Op::CALLM, w, trim, 0)?;
                    self.jump_if_is(w, "", c.to, true)?;
                    self.free(w);
                }
                Part::Step => {
                    let ok = self.label();
                    self.jump_if_is(t, "function", ok, true)?;
                    self.jump_if_is(t, "chain", c.to, false)?;
                    self.place(ok);
                }
            }
            self.free(t);
        }
        Ok(())
    }

    fn show_as_text(&mut self, vs: &[(ir::InstrId, ValueId)]) -> Result<(), Error> {
        for (owner, v) in vs {
            let x = self.val(*v)?;
            let t = self.alloc();
            self.abc(Op::INTERP, t, x, 0)?;
            self.text_of.insert((*owner, *v), t);
        }
        Ok(())
    }

    /// The tree-walker shows each `exhale` part and interpolation piece as
    /// soon as it is evaluated; the instruction shows them all at the end.
    /// A part that is not a scalar, with a later part after it, is turned
    /// to text where the tree-walker would have shown it: after the code
    /// that makes it, or, for a value made elsewhere (a pass replaced the
    /// read), after the previous part's code, or before the first part's.
    fn schedule_shows(&mut self, b: &Block, j: usize) {
        let i = &b.instrs[j];
        let parts: Vec<ValueId> = match &i.kind {
            Kind::Interp(ps) => ps
                .iter()
                .filter_map(|p| match p {
                    Piece::Val(v) => Some(*v),
                    Piece::Lit(_) => None,
                })
                .collect(),
            Kind::Exhale(vs) => vs.clone(),
            _ => return,
        };
        // The parts' own code: the instructions just before it that start
        // at or after its own start, up to the statement's step mark.
        // Lines and columns, not bytes: an interpolated piece's byte span
        // counts from its string.
        let mut k = j;
        while k > 0 && {
            let p = &b.instrs[k - 1];
            !matches!(p.kind, Kind::Step(_))
                && (p.span.line, p.span.col) >= (i.span.line, i.span.col)
        } {
            k -= 1;
        }
        let code = &b.instrs[k..j];
        let def_of = |v: &ValueId| self.f.vals.get(v.0 as usize).map(|x| x.def);
        let mut anchor: Option<ir::InstrId> = None;
        for (n, v) in parts.iter().enumerate() {
            let def = def_of(v).filter(|d| code.iter().any(|c| c.id == *d));
            let scalar = self.f.vals.get(v.0 as usize).is_some_and(|x| {
                matches!(x.ty, Ty::Str | Ty::Int | Ty::Float | Ty::Bool | Ty::Null)
            });
            if n + 1 < parts.len() && !scalar {
                let at = (i.id, *v);
                match (def, anchor, code.first()) {
                    (Some(d), _, _) => self.show_after.entry(d).or_default().push(at),
                    (None, Some(a), _) => self.show_after.entry(a).or_default().push(at),
                    (None, None, Some(c)) => self.show_before.entry(c.id).or_default().push(at),
                    // No code runs before the instruction: it shows it.
                    (None, None, None) => {}
                }
            }
            if def.is_some() {
                anchor = def;
            }
        }
    }

    fn instr_kind(&mut self, i: &'m Instr) -> Result<(), Error> {
        self.span = i.span;
        if let Some(c) = refused_kind(&i.kind) {
            return Err(self.refuse(c, i.span));
        }
        match &i.kind {
            Kind::Step(_) => {
                let k = self.nest();
                self.steps(k, 1)?;
            }
            Kind::Const(_) if i.result.is_some_and(|v| self.unloaded.contains(&v)) => {}
            Kind::Const(c) => {
                let made = i
                    .result
                    .and_then(|v| self.hoist.get(&v))
                    .and_then(|key| self.hoisted.get(key))
                    .copied();
                match (made, i.result) {
                    // Made before its loop: that register, never freed
                    // with the value.
                    (Some(r), Some(v)) => {
                        self.val_reg.insert(v, r);
                        self.pinned.insert(v);
                    }
                    _ => {
                        let r = self.define(i)?;
                        self.load_lit(r, Lit::of(c))?;
                    }
                }
            }
            Kind::Load(v) => match i.result.filter(|r| self.alias.contains(r)) {
                Some(r) => {
                    let s = self.var(*v)?;
                    self.val_reg.insert(r, s);
                    self.pinned.insert(r);
                }
                None => {
                    let r = self.define(i)?;
                    self.read_slot(r, *v)?;
                }
            },
            Kind::Decl(v, x) | Kind::Store(v, x) => {
                let x = self.val(*x)?;
                match self.slot(*v)? {
                    // Made in place.
                    Slot::Reg(s) if s == x => {}
                    Slot::Reg(s) => self.abc(Op::MOV, s, x, 0)?,
                    Slot::Up(u) => self.abc(Op::SETUP, u, x, 0)?,
                }
            }
            // A computed map key, checked where the tree-walker checks it
            // (after the key, before the entry's value): `insertmap` into a
            // scratch map raises the tree-walker's own error for anything
            // but a string or an int, at the key's row; then the key is
            // its text (a string stays, an int becomes its digits).
            Kind::Key(k) => {
                let k = self.val(*k)?;
                let r = self.define(i)?;
                let t = self.alloc();
                self.abc(Op::NEWMAP, t, 1, 0)?;
                self.abc(Op::INSERTMAP, t, k, k)?;
                self.free(t);
                self.abc(Op::INTERP, r, k, 0)?;
            }
            Kind::Unary(UnOp::Not, x) if i.result.is_some_and(|r| self.fused.contains_key(&r)) => {
                if let Some(r) = i.result {
                    self.pending_not.insert(r, *x);
                }
            }
            Kind::Unary(op, x) => {
                let x = self.val(*x)?;
                let r = self.define(i)?;
                let op = match op {
                    UnOp::Neg => Op::NEG,
                    UnOp::Not => Op::NOT,
                };
                self.abc(op, r, x, 0)?;
            }
            Kind::Binary(op, a, b) if i.result.is_some_and(|r| self.fused.contains_key(&r)) => {
                if let Some(r) = i.result {
                    self.pending.insert(
                        r,
                        PendingCmp {
                            op: *op,
                            a: *a,
                            b: *b,
                            span: i.span,
                        },
                    );
                }
            }
            Kind::Binary(op, a, b) if self.kform.contains_key(&i.id) => {
                let swap = self.kform[&i.id];
                let (x, k) = if swap { (*b, *a) } else { (*a, *b) };
                self.binary_k(i, *op, x, k)?;
            }
            Kind::Binary(op, a, b) => {
                let (a, b) = (self.val(*a)?, self.val(*b)?);
                let (code, negate) = match op {
                    BinOp::Add => (Op::ADD, false),
                    BinOp::Sub => (Op::SUB, false),
                    BinOp::Mul => (Op::MUL, false),
                    BinOp::Div => (Op::DIV, false),
                    BinOp::Rem => (Op::MOD, false),
                    BinOp::Eq => (Op::EQ, false),
                    BinOp::Ne => (Op::EQ, true),
                    BinOp::Lt => (Op::LT, false),
                    BinOp::Le => (Op::LE, false),
                    BinOp::Gt => (Op::LE, true),
                    BinOp::Ge => (Op::LT, true),
                    // Read by a `for` as its bounds: nothing to make here.
                    BinOp::Range if i.result.is_some_and(|v| self.ranges.contains_key(&v)) => {
                        return Ok(())
                    }
                    BinOp::Range => {
                        let r = self.define(i)?;
                        return self.range_value(r, a, b);
                    }
                    BinOp::And | BinOp::Or | BinOp::Coalesce => {
                        return Err(Error::Internal(format!(
                            "`{}` as a plain binary operator",
                            op.symbol()
                        )))
                    }
                };
                let r = self.define(i)?;
                self.abc(code, r, a, b)?;
                if negate {
                    self.abc(Op::NOT, r, r, 0)?;
                }
            }
            Kind::Short { op, subject, rhs } => {
                let s = self.val(*subject)?;
                let r = self.define(i)?;
                let end = self.label();
                // A subject made in the block's register needs no move.
                if s != r {
                    self.abc(Op::MOV, r, s, 0)?;
                }
                match op {
                    // `and`: a falsy subject is the result.
                    ShortOp::And => self.jump(Op::TESTN, r, end)?,
                    // `or`: a truthy subject is the result.
                    ShortOp::Or => self.jump(Op::TEST, r, end)?,
                    // `??`: a subject that is not null is the result;
                    // `?.`: a null subject is the result.
                    ShortOp::Coalesce | ShortOp::Safe => {
                        let t = self.alloc();
                        self.abc(Op::LOADNULL, t, 0, 0)?;
                        self.abc(Op::EQ, t, r, t)?;
                        let op = if *op == ShortOp::Coalesce {
                            Op::TESTN
                        } else {
                            Op::TEST
                        };
                        self.jump(op, t, end)?;
                        self.free(t);
                    }
                }
                self.yields.push(YieldTo::Reg(r));
                self.block(rhs)?;
                self.yields.pop();
                self.place(end);
            }
            Kind::Closure(fid) => {
                let target = self
                    .cx
                    .m
                    .funcs
                    .get(fid.0 as usize)
                    .ok_or_else(|| Error::Internal(format!("no function {}", fid.0)))?;
                let mut child = self.cx.function(target, self.depth + 1)?;
                self.span = i.span;
                // One upvalue per captured slot, in the child's capture
                // order: this function's register (its cell), or this
                // function's own upvalue.
                for c in &target.captures {
                    child.upvalues.push(match self.slot(*c)? {
                        Slot::Reg(r) => UpvalDesc {
                            kind: UpvalKind::ParentLocal,
                            index: u16::try_from(r).map_err(|_| self.too_wide(Op::CLOSURE))?,
                        },
                        Slot::Up(u) => UpvalDesc {
                            kind: UpvalKind::ParentUpvalue,
                            index: u16::try_from(u).map_err(|_| self.too_wide(Op::CLOSURE))?,
                        },
                    });
                }
                let index = self.children.len() as u32;
                self.children.push(child);
                let r = self.define(i)?;
                self.abx(Op::CLOSURE, r, index)?;
            }
            Kind::CallBuiltin { callee, args } => {
                let index = *self.cx.catalogue.get(callee.as_str()).ok_or_else(|| {
                    self.refuse(
                        format!("`{callee}`, a builtin this cig does not carry"),
                        i.span,
                    )
                })?;
                let w = self.window(i.id, None, args)?;
                self.abc(Op::CALLB, w, index, args.len() as u32)?;
                self.call_result(i, w, args.len());
            }
            Kind::CallUser { func, args } => {
                let slot = *self.cx.pull_slot.get(func).ok_or_else(|| {
                    Error::Internal(format!("no slot holds the closure of function {}", func.0))
                })?;
                // The closure is read straight into the window.
                let w = self.window(i.id, None, args)?;
                self.read_slot(w, slot)?;
                self.abc(Op::CALL, w, args.len() as u32, 0)?;
                self.call_result(i, w, args.len());
            }
            Kind::CallValue { f, args } => {
                let callee = self.val(*f)?;
                let w = self.window(i.id, Some(callee), args)?;
                self.abc(Op::CALL, w, args.len() as u32, 0)?;
                self.call_result(i, w, args.len());
            }
            Kind::If {
                cond,
                then,
                otherwise,
            } => {
                let other = self.label();
                self.leave_unless(*cond, other)?;
                self.block(then)?;
                match otherwise {
                    Some(o) => {
                        let end = self.label();
                        if !ends(then) {
                            self.span = i.span;
                            self.jump(Op::JMP, 0, end)?;
                        }
                        self.place(other);
                        self.block(o)?;
                        self.place(end);
                    }
                    None => self.place(other),
                }
            }
            Kind::Loop { cond, body } => {
                let outer = self.nest();
                let (head, exit) = (self.label(), self.label());
                self.steps(outer + 1, 0)?;
                self.loops.push(LoopCx {
                    cont: head,
                    cont_op: Op::LOOP,
                    exit,
                    open: self.open.len(),
                });
                self.place(head);
                self.yields.push(YieldTo::Exit(exit));
                self.block(cond)?;
                self.yields.pop();
                self.block(body)?;
                if !ends(body) {
                    self.span = i.span;
                    self.jump(Op::LOOP, 0, head)?;
                }
                self.loops.pop();
                self.place(exit);
                self.span = i.span;
                self.steps(outer, 0)?;
            }
            Kind::Yield(v) => match self.yields.last() {
                Some(YieldTo::Reg(r)) => {
                    let r = *r;
                    let x = self.val(*v)?;
                    // Made in the block's register already, or moved.
                    if x != r {
                        self.abc(Op::MOV, r, x, 0)?;
                    }
                }
                Some(YieldTo::Exit(l)) => {
                    let l = *l;
                    self.leave_unless(*v, l)?;
                }
                None => return Err(Error::Internal("a `yield` outside a value block".into())),
            },
            Kind::Ret(v) => {
                let mut x = self.val(*v)?;
                // `ret` closes every burn of the frame, and `try.leave` the
                // ones opened inside its try: only the tries are left here.
                if self.open.iter().any(|o| matches!(o, OpenCx::Try { .. })) {
                    // The finallys the `snuff` crosses run before the
                    // `ret`: the value waits where none of them writes.
                    let r = match self.ret_reg {
                        Some(r) => r,
                        None => {
                            let r = self.fresh(1);
                            self.ret_reg = Some(r);
                            r
                        }
                    };
                    self.abc(Op::MOV, r, x, 0)?;
                    for k in (0..self.open.len()).rev() {
                        if let OpenCx::Try { in_finally } = self.open[k] {
                            self.abc(Op::TRY_LEAVE, u32::from(in_finally), 0, 0)?;
                        }
                    }
                    x = r;
                }
                self.abc(Op::RET, x, 0, 0)?;
            }
            Kind::Break | Kind::Continue => {
                let Some(l) = self.loops.last() else {
                    return Err(Error::Internal(
                        "`break` or `continue` outside a loop".into(),
                    ));
                };
                let (op, to) = match i.kind {
                    Kind::Break => (Op::JMP, l.exit),
                    _ => (l.cont_op, l.cont),
                };
                let opened = l.open;
                self.leave_open(opened)?;
                self.jump(op, 0, to)?;
            }
            Kind::Throw(v) => {
                let x = self.val(*v)?;
                self.abc(Op::THROW, x, 0, 0)?;
            }
            Kind::Prelude(name) => {
                let r = self.define(i)?;
                let k = self.konst(ConstKey::Str(name));
                self.abx(Op::LOADG, r, k)?;
            }
            Kind::Try {
                body,
                catch,
                finally,
            } => self.try_stmt(i, body, catch.as_ref(), finally.as_ref())?,
            Kind::ForEach { iter, var, body } => self.for_each(i, *iter, *var, body)?,
            Kind::MakeList(vs) => {
                let regs = self.vals(vs)?;
                let r = self.define(i)?;
                self.abc(Op::NEWLIST, r, (vs.len() as u32).min(0xFFFF), 0)?;
                for x in regs {
                    self.abc(Op::APPENDLIST, r, x, 0)?;
                }
            }
            Kind::MakeMap(kv) => {
                let r = self.define(i)?;
                self.abc(Op::NEWMAP, r, (kv.len() as u32).min(0xFFFF), 0)?;
                for (k, x) in kv {
                    let x = self.val(*x)?;
                    match k {
                        ir::MapKey::Static(name) => {
                            let t = self.alloc();
                            let c = self.konst(ConstKey::Str(name));
                            self.abx(Op::LOADK, t, c)?;
                            self.abc(Op::INSERTMAP, r, t, x)?;
                            self.free(t);
                        }
                        // A `key`'s value: a string, so this cannot fail.
                        ir::MapKey::Dyn(key, _) => {
                            let key = self.val(*key)?;
                            self.abc(Op::INSERTMAP, r, key, x)?;
                        }
                    }
                }
            }
            Kind::Interp(pieces) => {
                let mut regs = Vec::new();
                for p in pieces {
                    regs.push(match p {
                        Piece::Lit(l) => Err(l.as_str()),
                        Piece::Val(v) => Ok(self.shown(i.id, *v)?),
                    });
                }
                let r = self.define(i)?;
                let n = regs.len().max(1) as u32;
                let w = self.alloc_window(n);
                if regs.is_empty() {
                    let c = self.konst(ConstKey::Str(""));
                    self.abx(Op::LOADK, w, c)?;
                }
                let mut texts = Vec::new();
                for (k, x) in regs.into_iter().enumerate() {
                    match x {
                        Ok((x, text)) => {
                            self.abc(Op::MOV, w + k as u32, x, 0)?;
                            if text {
                                texts.push(x);
                            }
                        }
                        Err(l) => {
                            let c = self.konst(ConstKey::Str(l));
                            self.abx(Op::LOADK, w + k as u32, c)?;
                        }
                    }
                }
                self.abc(Op::INTERP, r, w, n - 1)?;
                for k in 0..n {
                    self.free(w + k);
                }
                for t in texts {
                    self.free(t);
                }
            }
            Kind::Exhale(vs) => {
                let mut regs = Vec::new();
                for v in vs {
                    regs.push(self.shown(i.id, *v)?);
                }
                let w = self.alloc_window((regs.len() as u32).max(1));
                for (k, (x, _)) in regs.iter().enumerate() {
                    self.abc(Op::MOV, w + k as u32, *x, 0)?;
                }
                self.abc(Op::EXHALE, w, regs.len() as u32, 0)?;
                for k in 0..(regs.len() as u32).max(1) {
                    self.free(w + k);
                }
                for (x, text) in regs {
                    if text {
                        self.free(x);
                    }
                }
            }
            Kind::Member { obj, name, safe } => {
                let o = self.val(*obj)?;
                let r = self.define(i)?;
                let k = self.konst(ConstKey::Str(name));
                if *safe {
                    self.safe_member(r, o, name, k)?;
                } else {
                    self.abc(Op::GETFIELD, r, o, k)?;
                }
            }
            Kind::Index { obj, idx, idx_at } => {
                let (o, x) = (self.val(*obj)?, self.val(*idx)?);
                let r = self.define(i)?;
                self.index_read(r, o, x, *idx_at)?;
            }
            Kind::SetIndex {
                obj,
                idx,
                op,
                val,
                obj_at,
                idx_at,
            } => {
                let (o, x, v) = (self.val(*obj)?, self.val(*idx)?, self.val(*val)?);
                self.index_store(o, x, v, *op, *idx_at, *obj_at)?;
            }
            Kind::SetMember {
                obj,
                name,
                op,
                val,
                obj_at,
            } => {
                let (o, v) = (self.val(*obj)?, self.val(*val)?);
                let k = self.konst(ConstKey::Str(name));
                self.member_store(o, name, k, v, *op, *obj_at)?;
            }
            Kind::CallMethod { recv, name, args } => {
                let recv = self.val(*recv)?;
                let k = self.konst(ConstKey::Str(name));
                let w = self.window(i.id, Some(recv), args)?;
                self.abc(Op::CALLM, w, k, args.len() as u32)?;
                self.call_result(i, w, args.len());
            }
            Kind::Burn(b) => self.burn_stmt(i, b)?,
            Kind::BurnOp { callee, args, .. } => {
                let kind = crate::vm::burn_op_kind(callee).ok_or_else(|| {
                    self.refuse(
                        format!("`{callee}`, an effect this cig's VM has no burn-op kind for"),
                        i.span,
                    )
                })?;
                if !self.open.iter().any(|o| matches!(o, OpenCx::Burn { .. })) {
                    self.bare_op = true;
                }
                let w = self.window(i.id, None, args)?;
                let k = self.konst(ConstKey::Str(callee));
                self.burn_op(w, k, args.len() as u32, kind as u32)?;
                self.call_result(i, w, args.len());
            }
            Kind::PackDecl(vs) => {
                if self.pack_runs_code.contains(&i.id) {
                    return Err(self.refuse("a `pack` root that runs a pull or a function", i.span));
                }
                let regs = self.vals(vs)?;
                let n = regs.len() as u32;
                let w = self.alloc_window(n.max(1));
                if let Some(l) = self.part_to.remove(&i.id) {
                    self.place(l);
                }
                for (k, x) in regs.iter().enumerate() {
                    self.abc(Op::MOV, w + k as u32, *x, 0)?;
                }
                self.abc(Op::PACK, w, n, 0)?;
                // Each root's error at the root: its value's own position.
                for v in vs {
                    let at = self
                        .f
                        .vals
                        .get(v.0 as usize)
                        .and_then(|x| self.instr_of.get(&x.def))
                        .map_or(i.span, |d| d.span);
                    self.at(at, |s| s.abc(Op::NOP, 0, 0, 0))?;
                }
                for k in 0..n.max(1) {
                    self.free(w + k);
                }
            }
            Kind::MakeChain { name, steps } => {
                let regs: Vec<u32> = steps
                    .iter()
                    .map(|s| self.val(s.1))
                    .collect::<Result<_, _>>()?;
                let n = steps.len() as u32;
                let w = self.alloc_window(2 + 2 * n);
                if let Some(l) = self.part_to.remove(&i.id) {
                    self.place(l);
                }
                let chain = self
                    .var_info(*name)
                    .map(|x| x.name.as_str())
                    .ok_or_else(|| Error::Internal(format!("no slot {}", name.0)))?;
                let c = self.konst(ConstKey::Str(chain));
                self.abx(Op::LOADK, w + 1, c)?;
                for (k, ((label, _, _), x)) in steps.iter().zip(regs).enumerate() {
                    let at = w + 2 + 2 * k as u32;
                    match label {
                        Some(l) => {
                            let c = self.konst(ConstKey::Str(l));
                            self.abx(Op::LOADK, at, c)?;
                        }
                        None => self.abc(Op::LOADNULL, at, 0, 0)?,
                    }
                    self.abc(Op::MOV, at + 1, x, 0)?;
                }
                // A chain of the script's top level is also declared as a
                // top-level name, for `--light`.
                let top = u32::from(self.top_level.contains(&i.id));
                self.abc(Op::CHAIN_MAKE, w, 1 + 2 * n, top)?;
                for (_, _, at) in steps {
                    self.at(*at, |s| s.abc(Op::NOP, 0, 0, 0))?;
                }
                match self.slot(*name)? {
                    Slot::Reg(r) => self.abc(Op::MOV, r, w, 0)?,
                    Slot::Up(u) => self.abc(Op::SETUP, u, w, 0)?,
                }
                for k in 0..2 + 2 * n {
                    self.free(w + k);
                }
            }
        }
        Ok(())
    }

    /// `burn.op A, K, packed`: `(n << 4) | kind` unwidened, `(n << 8) |
    /// kind` behind a `WIDE` when an operand needs it.
    fn burn_op(&mut self, a: u32, k: u32, n: u32, kind: u32) -> Result<(), Error> {
        if a <= 0xFF && k <= 0xFF && n <= 0xF && kind <= 0xF {
            return self.abc(Op::BURN_OP, a, k, (n << 4) | kind);
        }
        if a > 0xFFFF || k > 0xFFFF || n > 0xFF || kind > 0xFF {
            return Err(self.too_wide(Op::BURN_OP));
        }
        let c = (n << 8) | kind;
        self.row();
        self.code.push(wide_prefix(
            (a >> 8) as u8,
            (((c >> 8) << 8) | (k >> 8)) as u16,
        ));
        self.code
            .push(Word::abc(Op::BURN_OP, a as u8, k as u8, c as u8));
        Ok(())
    }

    /// `burn (s, "label") { body } unburn { comp }` as `docs/BYTECODE.md`
    /// lays it out: the state map made fresh, `burn.enter class, Rg` at
    /// the burn's position, `comp.enter s, K[source], K["s"]` at the
    /// `unburn` block's, the body, `comp.leave`, `burn.leave`. A burn
    /// without `unburn { }`, or an unlit one, has no `comp.*`. A body that
    /// ends in a terminator (or in a burn whose body does) ends the burn's
    /// row there: its way out closed the burn, and nothing follows that no
    /// run reaches. A string literal
    /// label is the region's constant; any other label is the region's
    /// register, the label's value, evaluated before the state map and
    /// kept unwritten to the `burn.enter`, which reads it each time it
    /// runs; a `nop` right after it carries the label's position, where a
    /// label that is not a string is the tree-walker's error.
    fn burn_stmt(&mut self, i: &'m Instr, b: &'m BurnNode) -> Result<(), Error> {
        let (label, label_at) = match b.label {
            None => (RegionLabel::None, None),
            Some(v) => {
                let def = self
                    .f
                    .vals
                    .get(v.0 as usize)
                    .and_then(|x| self.instr_of.get(&x.def).copied());
                match def.map(|d| &d.kind) {
                    Some(Kind::Const(Const::Str(t))) => {
                        (RegionLabel::Const(self.konst(ConstKey::Str(t))), None)
                    }
                    _ => {
                        let at = b.label_at.or(def.map(|d| d.span)).unwrap_or(i.span);
                        (RegionLabel::Reg(self.val(v)?), Some(at))
                    }
                }
            }
        };
        let class = u8::from(b.class == BurnClass::Unlit);
        let comp = b.compensate.is_some() && class == 0;
        let region = self.regions.len() as u32;
        self.regions.push(BurnRegion { class, label });
        self.span = i.span;
        let state = match b.state {
            Some(s) => {
                let r = self.var(s)?;
                if self.captured(s) {
                    self.abc(Op::CELLNEW, r, 0, 0)?;
                }
                self.abc(Op::NEWMAP, r, 0, 0)?;
                Some((r, s))
            }
            None => None,
        };
        let pc_start = self.code.len() as u32;
        self.abx(Op::BURN_ENTER, u32::from(class), region)?;
        if let Some(at) = label_at {
            self.at(at, |s| s.abc(Op::NOP, 0, 0, 0))?;
        }
        if comp {
            let source = self.konst(ConstKey::Str(b.unburn_src.as_deref().unwrap_or("")));
            let (reg, name) = match state {
                Some((r, s)) => (
                    r,
                    self.var_info(s)
                        .map(|x| x.name.as_str())
                        .unwrap_or_default(),
                ),
                None => (0, ""),
            };
            let name = self.konst(ConstKey::Str(name));
            self.at(b.unburn_at.unwrap_or(i.span), |s| {
                s.abc(Op::COMP_ENTER, reg, source, name)
            })?;
        }
        self.open.push(OpenCx::Burn { comp });
        self.block(&b.body)?;
        self.open.pop();
        self.span = i.span;
        if !diverges(&b.body) {
            if comp {
                self.abc(Op::COMP_LEAVE, 0, 0, 0)?;
            }
            self.abc(Op::BURN_LEAVE, 0, 0, 0)?;
        }
        self.region_rows.push(RegionRow {
            pc_start,
            pc_end: self.code.len() as u32,
            region,
        });
        Ok(())
    }

    /// `R(r) = lit`.
    fn load_lit(&mut self, r: u32, lit: Lit<'m>) -> Result<(), Error> {
        match lit {
            Lit::Null => self.abc(Op::LOADNULL, r, 0, 0),
            Lit::Bool(b) => self.abc(Op::LOADBOOL, r, u32::from(b), 0),
            Lit::Int(x) => self.load_int(r, x),
            Lit::Float(x) => {
                let k = self.konst(ConstKey::Float(x));
                self.abx(Op::LOADK, r, k)
            }
            Lit::Str(x) => {
                let k = self.konst(ConstKey::Str(x));
                self.abx(Op::LOADK, r, k)
            }
        }
    }

    /// `R(r) = x`: `loadint` where it fits sixteen bits, else a constant.
    fn load_int(&mut self, r: u32, x: i64) -> Result<(), Error> {
        let Ok(small) = i16::try_from(x) else {
            let k = self.konst(ConstKey::Int(x));
            return self.abx(Op::LOADK, r, k);
        };
        if r > 0xFFFF {
            return Err(self.too_wide(Op::LOADINT));
        }
        self.row();
        if r > 0xFF {
            self.code.push(wide_prefix(
                (r >> 8) as u8,
                if small < 0 { 0xFFFF } else { 0 },
            ));
        }
        self.code.push(Word::asbx(Op::LOADINT, r as u8, small));
        Ok(())
    }

    fn vals(&self, vs: &[ValueId]) -> Result<Vec<u32>, Error> {
        vs.iter().map(|v| self.val(*v)).collect()
    }

    /// The register an `exhale` part or interpolation piece shows: its
    /// text, when it was turned to text where it was made.
    fn shown(&mut self, owner: ir::InstrId, v: ValueId) -> Result<(u32, bool), Error> {
        match self.text_of.remove(&(owner, v)) {
            Some(t) => Ok((t, true)),
            None => Ok((self.val(v)?, false)),
        }
    }

    /// `type_of(R(x))` in a fresh register.
    fn type_name_of(&mut self, x: u32) -> Result<u32, Error> {
        let f = *self
            .cx
            .catalogue
            .get("type_of")
            .ok_or_else(|| Error::Internal("no `type_of` in the catalogue".into()))?;
        let w = self.alloc_window(2);
        self.abc(Op::MOV, w + 1, x, 0)?;
        self.abc(Op::CALLB, w, f, 1)?;
        self.free(w + 1);
        Ok(w)
    }

    /// Jump to `to` when the type name in `R(t)` is (or, with `is`
    /// false, is not) `name`.
    fn jump_if_is(&mut self, t: u32, name: &'static str, to: Label, is: bool) -> Result<(), Error> {
        let k = self.konst(ConstKey::Str(name));
        let c = self.alloc();
        self.abc(Op::EQK, c, t, k)?;
        self.jump(if is { Op::TEST } else { Op::TESTN }, c, to)?;
        self.free(c);
        Ok(())
    }

    /// An instruction at another position than the current one.
    fn at(
        &mut self,
        span: Span,
        f: impl FnOnce(&mut Self) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let keep = self.span;
        self.span = span;
        let r = f(self);
        self.span = keep;
        r
    }

    /// `R(r) = R(o)[R(x)]`: one `getindex` at the expression's position,
    /// then a `nop` at the index's (`at`). The VM raises a wrong index type
    /// for a list, a string or a map at the `nop`'s row and every other
    /// error at the instruction's, through the interpreter's own `index`,
    /// as the tree-walker places them. A run never dispatches the `nop`.
    fn index_read(&mut self, r: u32, o: u32, x: u32, at: Span) -> Result<(), Error> {
        self.abc(Op::GETINDEX, r, o, x)?;
        self.at(at, |s| s.abc(Op::NOP, 0, 0, 0))
    }

    /// `R(o)[R(x)] op= R(v)`. The tree-walker puts a wrong index type for
    /// a list or a map at the index (`at`), a value that cannot be
    /// indexed at the object (`obj_at`), and the rest (`E504`, a missing
    /// key in a compound, the operator's own error) at the statement. A
    /// compound checks the operands' types first (`type_of`) and reads
    /// with `getindex` (the same `E504` and `E505`, with no hint) only
    /// once the store is known to be one that can happen.
    fn index_store(
        &mut self,
        o: u32,
        x: u32,
        v: u32,
        op: AssignOp,
        at: Span,
        obj_at: Span,
    ) -> Result<(), Error> {
        let Some(code) = compound(op) else {
            // One `setindex` at the statement's position, then a `nop` at
            // the index's and one at the object's: the VM raises a wrong
            // index type for a list or a map at the first, a store into
            // anything else at the second, and the rest at the
            // instruction's, through the interpreter's own `set_index`.
            self.abc(Op::SETINDEX, o, x, v)?;
            self.at(at, |s| s.abc(Op::NOP, 0, 0, 0))?;
            return self.at(obj_at, |s| s.abc(Op::NOP, 0, 0, 0));
        };
        // A compound keeps its checks: it reads before it stores, and a
        // wrong index type or object must still be the store's error, at
        // the store's positions, not the read's.
        let to = self.type_name_of(o)?;
        let ti = self.type_name_of(x)?;
        let (seq, map, ok, bad, end) = (
            self.label(),
            self.label(),
            self.label(),
            self.label(),
            self.label(),
        );
        self.jump_if_is(to, "list", seq, true)?;
        self.jump_if_is(to, "map", map, true)?;
        self.at(obj_at, |s| s.abc(Op::SETINDEX, o, x, v))?;
        self.jump(Op::JMP, 0, end)?;
        self.place(seq);
        self.jump_if_is(ti, "int", ok, true)?;
        self.jump(Op::JMP, 0, bad)?;
        self.place(map);
        self.jump_if_is(ti, "string", ok, true)?;
        self.place(bad);
        self.at(at, |s| s.abc(Op::SETINDEX, o, x, v))?;
        self.jump(Op::JMP, 0, end)?;
        self.place(ok);
        let t = self.alloc();
        self.abc(Op::GETINDEX, t, o, x)?;
        self.abc(code, t, t, v)?;
        self.abc(Op::SETINDEX, o, x, t)?;
        self.free(t);
        self.place(end);
        self.free(to);
        self.free(ti);
        Ok(())
    }

    /// `R(o).name op= R(v)`: `setfield`'s type error is at the object. A
    /// compound on a map reads with `getindex`, whose `E505` carries no
    /// hint, as the tree-walker's compound assignment does not.
    fn member_store(
        &mut self,
        o: u32,
        name: &'m str,
        k: u32,
        v: u32,
        op: AssignOp,
        obj_at: Span,
    ) -> Result<(), Error> {
        let Some(code) = compound(op) else {
            return self.at(obj_at, |s| s.abc(Op::SETFIELD, o, k, v));
        };
        let to = self.type_name_of(o)?;
        let (map, end) = (self.label(), self.label());
        self.jump_if_is(to, "map", map, true)?;
        self.at(obj_at, |s| s.abc(Op::SETFIELD, o, k, v))?;
        self.jump(Op::JMP, 0, end)?;
        self.place(map);
        let (t, key) = (self.alloc(), self.alloc());
        let c = self.konst(ConstKey::Str(name));
        self.abx(Op::LOADK, key, c)?;
        self.abc(Op::GETINDEX, t, o, key)?;
        self.abc(code, t, t, v)?;
        self.at(obj_at, |s| s.abc(Op::SETFIELD, o, k, t))?;
        self.place(end);
        self.free(t);
        self.free(key);
        self.free(to);
        Ok(())
    }

    /// `R(r) = R(o)?.name`: null for null and for a map without the key
    /// (`getfield` is the plain `.`), the plain member read otherwise.
    fn safe_member(&mut self, r: u32, o: u32, name: &'m str, k: u32) -> Result<(), Error> {
        let keys = *self
            .cx
            .catalogue
            .get("keys")
            .ok_or_else(|| Error::Internal("no `keys` in the catalogue".into()))?;
        let to = self.type_name_of(o)?;
        let (null, map, end) = (self.label(), self.label(), self.label());
        self.jump_if_is(to, "null", null, true)?;
        self.jump_if_is(to, "map", map, true)?;
        self.abc(Op::GETFIELD, r, o, k)?;
        self.jump(Op::JMP, 0, end)?;
        self.place(map);
        // `keys(o).contains(name)`: a list's method, which a map's own
        // entries cannot stand in for.
        let w = self.alloc_window(2);
        self.abc(Op::MOV, w + 1, o, 0)?;
        self.abc(Op::CALLB, w, keys, 1)?;
        let c = self.konst(ConstKey::Str(name));
        self.abx(Op::LOADK, w + 1, c)?;
        let contains = self.konst(ConstKey::Str("contains"));
        self.abc(Op::CALLM, w, contains, 1)?;
        self.jump(Op::TESTN, w, null)?;
        self.free(w);
        self.free(w + 1);
        self.abc(Op::GETFIELD, r, o, k)?;
        self.jump(Op::JMP, 0, end)?;
        self.place(null);
        self.abc(Op::LOADNULL, r, 0, 0)?;
        self.place(end);
        self.free(to);
        Ok(())
    }

    /// `R(r) = R(a)..R(b)`, the list. `forprep` checks the bounds as
    /// `a..b` does (`E403` for anything but two ints, `E512` past
    /// 10,000,000) and, either way, goes on to the next instruction: no
    /// loop, so no step to charge. Then two ints that fit are `range(a,
    /// b)`, which makes the same list, or an empty one when `b <= a`.
    fn range_value(&mut self, r: u32, a: u32, b: u32) -> Result<(), Error> {
        let range = *self
            .cx
            .catalogue
            .get("range")
            .ok_or_else(|| Error::Internal("no `range` in the catalogue".into()))?;
        let w = self.alloc_window(4);
        self.abc(Op::MOV, w, a, 0)?;
        self.abc(Op::MOV, w + 1, b, 0)?;
        self.load_int(w + 2, 1)?;
        let next = self.label();
        self.jump(Op::FORPREP, w, next)?;
        self.place(next);
        let (empty, end) = (self.label(), self.label());
        let c = self.alloc();
        self.abc(Op::LE, c, b, a)?;
        self.jump(Op::TEST, c, empty)?;
        self.free(c);
        let args = self.alloc_window(3);
        self.abc(Op::MOV, args + 1, a, 0)?;
        self.abc(Op::MOV, args + 2, b, 0)?;
        self.abc(Op::CALLB, args, range, 2)?;
        self.abc(Op::MOV, r, args, 0)?;
        self.jump(Op::JMP, 0, end)?;
        self.place(empty);
        self.abc(Op::NEWLIST, r, 0, 0)?;
        self.place(end);
        for k in 0..4 {
            self.free(w + k);
        }
        for k in 0..3 {
            self.free(args + k);
        }
        Ok(())
    }

    /// `for x in v`: over `a..b` with `forprep`/`forloop`, anything else
    /// with `iterprep`/`iternext`. The item is `R(A+3)`; a captured one
    /// takes a fresh cell at the top of each turn. The body's first
    /// instruction is its `step.iter`, after the exit test, so every turn
    /// is charged.
    fn for_each(
        &mut self,
        i: &'m Instr,
        iter: ValueId,
        var: VarId,
        body: &'m Block,
    ) -> Result<(), Error> {
        let outer = self.nest();
        let (cont, exit) = (self.label(), self.label());
        let captured = self.captured(var);
        // A captured item's window is the item's alone, as a captured
        // slot's register is.
        let a = if captured {
            self.fresh(4)
        } else {
            self.alloc_window(4)
        };
        self.var_reg.insert(var, a + 3);
        if let Some(&(lo, hi, range_span)) = self.ranges.get(&iter) {
            let (lo, hi) = (self.val(lo)?, self.val(hi)?);
            self.abc(Op::MOV, a, lo, 0)?;
            self.abc(Op::MOV, a + 1, hi, 0)?;
            self.load_int(a + 2, 1)?;
            self.steps(outer + 1, 0)?;
            // `E403` and `E512` are the range's, at its position.
            self.span = range_span;
            self.jump(Op::FORPREP, a, exit)?;
            self.span = i.span;
            let top = self.label();
            self.place(top);
            self.loops.push(LoopCx {
                cont,
                cont_op: Op::JMP,
                exit,
                open: self.open.len(),
            });
            if captured {
                self.abc(Op::CELLNEW, a + 3, 0, 0)?;
            }
            self.block(body)?;
            self.loops.pop();
            self.place(cont);
            self.span = i.span;
            self.jump(Op::FORLOOP, a, top)?;
        } else {
            let it = self.val(iter)?;
            self.abc(Op::MOV, a, it, 0)?;
            // `E405` is at the `for`.
            self.abc(Op::ITERPREP, a, 0, 0)?;
            self.steps(outer + 1, 0)?;
            let head = self.label();
            self.place(head);
            self.jump(Op::ITERNEXT, a, exit)?;
            self.loops.push(LoopCx {
                cont,
                cont_op: Op::JMP,
                exit,
                open: self.open.len(),
            });
            if captured {
                self.abc(Op::CELLNEW, a + 3, 0, 0)?;
            }
            self.block(body)?;
            self.loops.pop();
            self.place(cont);
            self.span = i.span;
            self.jump(Op::LOOP, 0, head)?;
        }
        self.place(exit);
        self.span = i.span;
        self.steps(outer, 0)?;
        if !captured {
            self.var_reg.remove(&var);
            for k in 0..4 {
                self.free(a + k);
            }
        }
        Ok(())
    }

    /// Leave the tries and burns opened since `down_to`, innermost first:
    /// a try in its body or catch runs its finally (`try.leave 0`), one in
    /// its finally drops what it had pending (`try.leave 1`); a burn stops
    /// its compensation covering (`comp.leave`, when it has one) and closes
    /// (`burn.leave`). A `try.leave` closes the burns opened inside its try
    /// too, but they are closed already: innermost first.
    fn leave_open(&mut self, down_to: usize) -> Result<(), Error> {
        for k in (down_to..self.open.len()).rev() {
            match self.open[k] {
                OpenCx::Try { in_finally } => {
                    self.abc(Op::TRY_LEAVE, u32::from(in_finally), 0, 0)?;
                }
                OpenCx::Burn { comp, .. } => {
                    if comp {
                        self.abc(Op::COMP_LEAVE, 0, 0, 0)?;
                    }
                    self.abc(Op::BURN_LEAVE, 0, 0, 0)?;
                }
            }
        }
        Ok(())
    }

    /// `try`/`ashtray`/`finally` as `docs/BYTECODE.md` lays it out:
    /// `try.enter H`; the body, which is the handler's range and ends with
    /// its `try.leave 0`; a jump past the rest; the catch, ending with
    /// `try.leave 0` (and a jump past the finally); the finally, ending
    /// with `try.leave 0`. The catch and the finally lie outside the range.
    /// The binding's register is the catch slot's, taken before the body;
    /// a captured binding takes its cell at the catch's first pc.
    fn try_stmt(
        &mut self,
        i: &'m Instr,
        body: &'m Block,
        catch: Option<&'m (Option<VarId>, Block)>,
        finally: Option<&'m Block>,
    ) -> Result<(), Error> {
        if catch.is_none() && finally.is_none() {
            // Nothing to catch or run after: the body as it is.
            return self.block(body);
        }
        let binding = catch.and_then(|c| c.0);
        let catch_reg = match binding {
            Some(v) => self.var(v)?,
            None => RESERVED_U32,
        };
        let h = self.handlers.len();
        self.handlers.push(HandlerEntry {
            pc_start: 0,
            pc_end: 0,
            handler_pc: 0,
            catch_reg,
            finally_pc: RESERVED_U32,
        });
        self.span = i.span;
        self.abx(Op::TRY_ENTER, 0, h as u32)?;
        let pc_start = self.code.len() as u32;
        let end = self.label();
        self.open.push(OpenCx::Try { in_finally: false });
        self.block(body)?;
        let open = !ends(body);
        if open {
            self.span = i.span;
            self.abc(Op::TRY_LEAVE, 0, 0, 0)?;
        }
        let pc_end = self.code.len() as u32;
        if open {
            self.jump(Op::JMP, 0, end)?;
        }
        let mut handler_pc = None;
        if let Some((_, cb)) = catch {
            handler_pc = Some(self.code.len() as u32);
            if binding.is_some_and(|v| self.captured(v)) {
                self.span = i.span;
                self.abc(Op::CELLNEW, catch_reg, 0, 0)?;
            }
            self.block(cb)?;
            if !ends(cb) {
                self.span = i.span;
                self.abc(Op::TRY_LEAVE, 0, 0, 0)?;
                if finally.is_some() {
                    self.jump(Op::JMP, 0, end)?;
                }
            }
        }
        let mut finally_pc = RESERVED_U32;
        if let Some(fb) = finally {
            finally_pc = self.code.len() as u32;
            if let Some(OpenCx::Try { in_finally }) = self.open.last_mut() {
                *in_finally = true;
            }
            self.block(fb)?;
            if !ends(fb) {
                self.span = i.span;
                self.abc(Op::TRY_LEAVE, 0, 0, 0)?;
            }
        }
        self.open.pop();
        self.place(end);
        self.handlers[h] = HandlerEntry {
            pc_start,
            pc_end,
            handler_pc: handler_pc.unwrap_or(finally_pc),
            catch_reg,
            finally_pc,
        };
        Ok(())
    }

    /// A call's window: the callee (when there is one) and the arguments
    /// moved into consecutive registers, but for those made in their place
    /// already (`direct`); `callb` leaves the first one for its result.
    fn window(
        &mut self,
        call: ir::InstrId,
        callee: Option<u32>,
        args: &[ValueId],
    ) -> Result<u32, Error> {
        let regs: Vec<u32> = args
            .iter()
            .map(|a| self.val(*a))
            .collect::<Result<_, _>>()?;
        let n = args.len() as u32;
        let w = match self.windows.remove(&call) {
            // A window taken before its arguments were made: a value made
            // since may sit above it and outlive the call. Then the call
            // takes a window at the top, and its arguments are moved there
            // (the registers of the first are free once they are).
            Some(w) if self.makes_frame(call) && self.held_above(call, w + n) => {
                let top = self.alloc_top(n + 1);
                if let Some(c) = callee {
                    self.abc(Op::MOV, top, c, 0)?;
                }
                for (k, r) in regs.into_iter().enumerate() {
                    self.abc(Op::MOV, top + 1 + k as u32, r, 0)?;
                }
                for r in w..=w + n {
                    self.free(r);
                }
                return Ok(top);
            }
            Some(w) => w,
            None if self.makes_frame(call) => self.alloc_top(n + 1),
            None => self.alloc_window(n + 1),
        };
        // A callee or receiver made in its place needs no move.
        if let Some(c) = callee.filter(|c| *c != w) {
            self.abc(Op::MOV, w, c, 0)?;
        }
        for (k, r) in regs.into_iter().enumerate() {
            let at = w + 1 + k as u32;
            // An argument made in its place needs no move.
            if r != at {
                self.abc(Op::MOV, at, r, 0)?;
            }
        }
        Ok(w)
    }

    /// Whether a register above `last` holds a value that outlives `call`:
    /// one in use that is not freed when the call is done.
    fn held_above(&self, call: ir::InstrId, last: u32) -> bool {
        let dying: HashSet<u32> = [self.start.get(&call), self.end.get(&call)]
            .into_iter()
            .flatten()
            .filter_map(|pos| self.dies_at.get(pos))
            .flatten()
            .filter(|v| !self.pinned.contains(v))
            .filter_map(|v| self.val_reg.get(v).copied())
            .collect();
        (last + 1..self.frame).any(|r| !self.free_regs.contains(&r) && !dying.contains(&r))
    }

    /// The call's result is `R(w)`; the argument registers are free.
    fn call_result(&mut self, i: &Instr, w: u32, n: usize) {
        for k in 0..n {
            self.free(w + 1 + k as u32);
        }
        match i.result {
            Some(v) => {
                self.val_reg.insert(v, w);
            }
            None => self.free(w),
        }
    }
}

/// A binary operator's form with a constant right operand, and whether a
/// `not` follows it.
fn k_op(op: BinOp) -> Option<(Op, bool)> {
    Some(match op {
        BinOp::Add => (Op::ADDK, false),
        BinOp::Sub => (Op::SUBK, false),
        BinOp::Mul => (Op::MULK, false),
        BinOp::Div => (Op::DIVK, false),
        BinOp::Rem => (Op::MODK, false),
        BinOp::Eq => (Op::EQK, false),
        BinOp::Ne => (Op::EQK, true),
        BinOp::Lt => (Op::LTK, false),
        BinOp::Le => (Op::LEK, false),
        BinOp::Gt => (Op::LEK, true),
        BinOp::Ge => (Op::LTK, true),
        _ => return None,
    })
}

/// Every block of a function, nested ones included, each with whether it
/// is a `while` loop's condition.
fn all_blocks<'a>(b: &'a Block, is_cond: bool, out: &mut Vec<(&'a Block, bool)>) {
    out.push((b, is_cond));
    for i in &b.instrs {
        match &i.kind {
            Kind::Loop { cond, body } => {
                all_blocks(cond, true, out);
                all_blocks(body, false, out);
            }
            k => {
                for nested in k.blocks() {
                    all_blocks(nested, false, out);
                }
            }
        }
    }
}

/// Does `i` write the slot `v`: anywhere in it (`deep`), or before it has
/// read its operands (the last reader of a slot read with no copy reads
/// first: a store writes after, an `if`, a value block or a loop's body
/// runs after; only a burn makes its state map before its label is read).
fn writes(i: &Instr, v: VarId, deep: bool) -> bool {
    if !deep {
        return matches!(&i.kind, Kind::Burn(b) if b.state == Some(v));
    }
    let mut hit = false;
    walk_one(i, &mut |x| {
        hit |= match &x.kind {
            Kind::Decl(w, _) | Kind::Store(w, _) => *w == v,
            Kind::MakeChain { name, .. } => *name == v,
            Kind::ForEach { var, .. } => *var == v,
            Kind::Try { catch, .. } => catch.as_ref().is_some_and(|c| c.0 == Some(v)),
            Kind::Burn(b) => b.state == Some(v),
            _ => false,
        };
    });
    hit
}

/// The operator of a compound assignment.
fn compound(op: AssignOp) -> Option<Op> {
    match op {
        AssignOp::Set => None,
        AssignOp::Add => Some(Op::ADD),
        AssignOp::Sub => Some(Op::SUB),
        AssignOp::Mul => Some(Op::MUL),
        AssignOp::Div => Some(Op::DIV),
    }
}

/// Does a block end in a terminator (nothing falls out of it)?
fn ends(b: &Block) -> bool {
    b.instrs.last().is_some_and(|i| i.kind.is_terminator())
}

/// No run falls off the end of `b`: it ends in a terminator, or in a burn
/// whose own body does (that burn's way out is the block's).
fn diverges(b: &Block) -> bool {
    match b.instrs.last().map(|i| &i.kind) {
        Some(Kind::Burn(n)) => diverges(&n.body),
        Some(k) => k.is_terminator(),
        None => false,
    }
}

/// Number the instructions in the order they are lowered: an
/// instruction's start before its nested blocks, its end after them.
fn number(
    b: &Block,
    start: &mut HashMap<ir::InstrId, usize>,
    end: &mut HashMap<ir::InstrId, usize>,
    counter: &mut usize,
) {
    for i in &b.instrs {
        *counter += 1;
        start.insert(i.id, *counter);
        for nested in i.kind.blocks() {
            number(nested, start, end, counter);
        }
        *counter += 1;
        end.insert(i.id, *counter);
    }
}

/// A loop's (start, end) positions.
type LoopRange = (usize, usize);

/// Every use of a value, with the loops (start, end) around it,
/// outermost first.
fn collect_uses(
    b: &Block,
    start: &HashMap<ir::InstrId, usize>,
    end: &HashMap<ir::InstrId, usize>,
    ranges: &HashMap<ValueId, (ValueId, ValueId, Span)>,
    remap: &HashMap<ir::InstrId, usize>,
    loops: &mut Vec<LoopRange>,
    f: &mut dyn FnMut(ValueId, usize, &[LoopRange]),
) {
    for i in &b.instrs {
        let s = remap.get(&i.id).or(start.get(&i.id)).copied().unwrap_or(0);
        for v in i.kind.uses() {
            f(v, s, loops);
            // A `for` over `a..b` reads the bounds itself.
            if let Some((a, b, _)) = ranges.get(&v) {
                f(*a, s, loops);
                f(*b, s, loops);
            }
        }
        let is_loop = matches!(i.kind, Kind::Loop { .. } | Kind::ForEach { .. });
        if is_loop {
            loops.push((s, end.get(&i.id).copied().unwrap_or(s)));
        }
        for nested in i.kind.blocks() {
            collect_uses(nested, start, end, ranges, remap, loops, f);
        }
        if is_loop {
            loops.pop();
        }
    }
}

/// The outermost loop (`while` or `for`) around each instruction of the
/// blocks this function lowers, `None` outside every loop. A burn's
/// compensation is not lowered here: its instructions are left out.
fn loops_around(
    b: &Block,
    outer: Option<ir::InstrId>,
    out: &mut HashMap<ir::InstrId, Option<ir::InstrId>>,
) {
    for i in &b.instrs {
        out.insert(i.id, outer);
        let inner = match i.kind {
            Kind::Loop { .. } | Kind::ForEach { .. } => outer.or(Some(i.id)),
            _ => outer,
        };
        for (slot, nested) in i.kind.slots() {
            if slot != "burn.compensate" {
                loops_around(nested, inner, out);
            }
        }
    }
}

/// Every instruction, nested blocks included, in lowering order.
fn walk<'a>(b: &'a Block, f: &mut dyn FnMut(&'a Instr)) {
    for i in &b.instrs {
        walk_one(i, f);
    }
}

/// An instruction and every one nested in it.
fn walk_one<'a>(i: &'a Instr, f: &mut dyn FnMut(&'a Instr)) {
    f(i);
    for nested in i.kind.blocks() {
        walk(nested, f);
    }
}

/// The slot each hoisted pull's closure is declared into.
fn pulls(b: &Block, closures: &mut HashMap<ValueId, FuncId>, out: &mut HashMap<FuncId, VarId>) {
    for i in &b.instrs {
        match (&i.kind, i.result) {
            (Kind::Closure(f), Some(v)) => {
                closures.insert(v, *f);
            }
            (Kind::Decl(var, v), _) => {
                if let Some(f) = closures.get(v) {
                    out.insert(*f, *var);
                }
            }
            _ => {}
        }
        for nested in i.kind.blocks() {
            pulls(nested, closures, out);
        }
    }
}
