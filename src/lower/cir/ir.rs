// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
//! CIR's data (cig-compile#3): structured SSA. `burn`, `try`, `if`, loops and the
//! short-circuit operators own nested blocks, so "inside a burn" is lexical
//! nesting under a `Burn` node, and moving an effect out of its region
//! means editing two block vectors, which the verifier sees.
//! Temporaries (`ValueId`) are defined once; `roll`, `stick`, parameters
//! and loop variables are slots (`VarId`) that closures share, as the
//! interpreter's environments are.
//!
//! What a back end must honour, because the interpreter does:
//! - a slot exists from its block's entry (hoisted pulls capture each
//!   other before their `decl`); `decl` initialises it;
//! - a slot declared in a loop body, and a `foreach` variable, is a fresh
//!   cell each iteration (`Scope::child` per iteration), so a closure made
//!   in one iteration keeps that iteration's value;
//! - `call.user @f` calls the closure most recently made for `f` in scope
//!   (a pull made inside a loop or a recursion has one closure per entry);
//! - every `step` and `step.iter` is one tick of the step budget
//!   (`--max-steps`), counted where the interpreter counts it and raising
//!   E515 at the mark's own span once the budget is spent.

use crate::syntax::ast::{AssignOp, BinOp, BurnClass, UnOp};
use crate::syntax::span::Span;
use crate::value::Effect;

macro_rules! id {
    ($name:ident) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
        pub struct $name(pub u32);
    };
}
id!(InstrId);
id!(ValueId);
id!(VarId);
id!(FuncId);
id!(RegionId);

impl RegionId {
    /// Outside every burn.
    pub const ROOT: RegionId = RegionId(0);
}

pub struct Module {
    pub file: String,
    /// `funcs[0]` is the script's top level.
    pub funcs: Vec<Func>,
    /// Indexed by `VarId`.
    pub vars: Vec<VarInfo>,
    pub next_instr: u32,
    pub next_region: u32,
}

impl Module {
    pub fn fresh_instr(&mut self) -> InstrId {
        self.next_instr += 1;
        InstrId(self.next_instr)
    }
}

pub struct VarInfo {
    pub name: String,
    pub mutable: bool,
    pub owner: FuncId,
    /// Some other function reads or writes it: a back end boxes it.
    pub captured: bool,
    pub span: Span,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FuncKind {
    Main,
    Pull,
    Lambda,
}

pub struct Func {
    pub id: FuncId,
    pub kind: FuncKind,
    pub name: Option<String>,
    pub params: Vec<VarId>,
    /// Free variables, owned by an enclosing function.
    pub captures: Vec<VarId>,
    /// Derived (verifier rule V5): it effects outside any burn of its own,
    /// so it is legal only when called inside one.
    pub requires_burn: bool,
    /// Indexed by `ValueId`.
    pub vals: Vec<ValueInfo>,
    pub body: Block,
    pub span: Span,
}

pub struct ValueInfo {
    pub ty: Ty,
    pub def: InstrId,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum Ty {
    Dynamic,
    Null,
    Bool,
    Int,
    Float,
    Str,
    List,
    Map,
    Func,
}

impl Ty {
    /// The set of run-time tags a value of this type may carry, as
    /// decision 0005 section 2 names it: what is read where a proven
    /// type meets a recorded one.
    pub fn tags(self) -> cig_native::tagset::TagSet {
        use cig_native::tagset::TagSet;
        match self {
            Ty::Dynamic => TagSet::ALL,
            Ty::Null => TagSet::NULL,
            Ty::Bool => TagSet::BOOL,
            Ty::Int => TagSet::INT,
            Ty::Float => TagSet::FLOAT,
            Ty::Str => TagSet::STR,
            Ty::List => TagSet::LIST,
            Ty::Map => TagSet::MAP,
            Ty::Func => TagSet::FUNC,
        }
    }
}

#[derive(Default)]
pub struct Block {
    pub instrs: Vec<Instr>,
}

pub struct Instr {
    pub id: InstrId,
    pub span: Span,
    pub result: Option<ValueId>,
    pub kind: Kind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Const {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShortOp {
    And,
    Or,
    Coalesce,
    /// `?.`: the rest is not evaluated on a null subject.
    Safe,
}

pub enum MapKey {
    Static(String),
    /// A computed key: a `key` instruction's value, always a string, and
    /// the span of the key expression, which no pass changes.
    Dyn(ValueId, Span),
}

pub enum Piece {
    Lit(String),
    Val(ValueId),
}

/// What an effectful stdlib call does to the world.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpKind {
    Write,
    Append,
    Mkdir,
    Rm,
    Cp,
    Mv,
    Hop,
    EnvSet,
    EnvUnset,
    LayerApply,
    LayerDiscard,
}

impl OpKind {
    pub fn name(self) -> &'static str {
        match self {
            OpKind::Write => "write",
            OpKind::Append => "append",
            OpKind::Mkdir => "mkdir",
            OpKind::Rm => "rm",
            OpKind::Cp => "cp",
            OpKind::Mv => "mv",
            OpKind::Hop => "hop",
            OpKind::EnvSet => "env.set",
            OpKind::EnvUnset => "env.unset",
            OpKind::LayerApply => "layer.apply",
            OpKind::LayerDiscard => "layer.discard",
        }
    }
}

/// Where a step mark sits, and so which of the interpreter's ticks it is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StepSite {
    /// A statement starts: the first instruction of every statement but a
    /// `pull` (hoisted, never run as a statement), before anything the
    /// statement evaluates. Its span is the statement's. An `if` with
    /// `else if` arms is one statement and has one mark.
    Stmt,
    /// A loop iteration starts: the first instruction of a `loop.body` or
    /// `foreach.body`, after the condition held (or the next item was
    /// taken) and before the body's first statement. Its span is the loop
    /// statement's. A loop whose condition fails, or whose list is empty,
    /// runs none.
    Iter,
}

pub struct BurnNode {
    pub region: RegionId,
    pub class: BurnClass,
    pub state: Option<VarId>,
    pub label: Option<ValueId>,
    pub body: Block,
    pub compensate: Option<Block>,
    pub unburn_src: Option<String>,
    /// The `unburn { }` block's own span: the line the journal records for
    /// the compensation (the tree-walker's), which a back end needs at run
    /// time.
    pub unburn_at: Option<Span>,
    /// The label expression's own span: where the tree-walker raises a
    /// label that is not a string, which a back end that reads the label
    /// at run time needs.
    pub label_at: Option<Span>,
}

pub enum Kind {
    /// One tick of the interpreter's step counter. No operands, no result.
    /// Ordered: it never moves, and it goes only when its whole block goes.
    Step(StepSite),
    Const(Const),
    /// A name that resolves to the prelude: a builtin, a module, `args`.
    Prelude(String),
    Load(VarId),
    /// Declare and initialise.
    Decl(VarId, ValueId),
    Store(VarId, ValueId),
    Unary(UnOp, ValueId),
    /// Never `And`, `Or` or `Coalesce`: those are `Short`.
    Binary(BinOp, ValueId, ValueId),
    /// The result is `subject` when the guard does not enter (`And`: falsy,
    /// `Or`: truthy, `Coalesce`: non-null), null for `Safe` on a null
    /// subject, else what `rhs` yields.
    Short {
        op: ShortOp,
        subject: ValueId,
        rhs: Block,
    },
    MakeList(Vec<ValueId>),
    /// The entries in order. Each computed key is a `key` instruction's
    /// value, made before the entry's value is evaluated.
    MakeMap(Vec<(MapKey, ValueId)>),
    /// A computed map key, checked where the tree-walker checks it: after
    /// the key expression and before the entry's value. A string stays, an
    /// int becomes its decimal text, anything else raises a type error
    /// ("map keys must be strings") at this instruction's span, the key
    /// expression's. Its value is a string.
    Key(ValueId),
    Interp(Vec<Piece>),
    /// `safe`: `?.`, null for a null subject and for a map without the key.
    Member {
        obj: ValueId,
        name: String,
        safe: bool,
    },
    /// `obj[idx]`. A wrong index type is raised at `idx_at`, the index
    /// expression's span; a value that cannot be indexed, and a missing
    /// key or an index out of range, at the instruction's own span.
    Index {
        obj: ValueId,
        idx: ValueId,
        idx_at: Span,
    },
    /// The combined read-modify-write: the type errors live in it. A wrong
    /// index type is raised at `idx_at`, a value that cannot be indexed at
    /// `obj_at` (the object expression's span), the rest at the statement.
    /// Both spans are the expressions' own and no pass changes them, so
    /// they hold when a pass replaces an operand with a value made
    /// elsewhere.
    SetIndex {
        obj: ValueId,
        idx: ValueId,
        op: AssignOp,
        val: ValueId,
        obj_at: Span,
        idx_at: Span,
    },
    /// `obj.name op= val`. A value that is not a map is an error at
    /// `obj_at`, the object expression's own span, which no pass changes;
    /// the rest at the statement.
    SetMember {
        obj: ValueId,
        name: String,
        op: AssignOp,
        val: ValueId,
        obj_at: Span,
    },
    Closure(FuncId),
    BurnOp {
        kind: OpKind,
        callee: String,
        effect: Effect,
        args: Vec<ValueId>,
    },
    CallBuiltin {
        callee: String,
        args: Vec<ValueId>,
    },
    CallUser {
        func: FuncId,
        args: Vec<ValueId>,
    },
    CallValue {
        f: ValueId,
        args: Vec<ValueId>,
    },
    CallMethod {
        recv: ValueId,
        name: String,
        args: Vec<ValueId>,
    },
    MakeChain {
        name: VarId,
        steps: Vec<(Option<String>, ValueId, Span)>,
    },
    PackDecl(Vec<ValueId>),
    Exhale(Vec<ValueId>),
    Burn(Box<BurnNode>),
    Try {
        body: Block,
        catch: Option<(Option<VarId>, Block)>,
        finally: Option<Block>,
    },
    If {
        cond: ValueId,
        then: Block,
        otherwise: Option<Block>,
    },
    Loop {
        cond: Block,
        body: Block,
    },
    ForEach {
        iter: ValueId,
        var: VarId,
        body: Block,
    },
    // Terminators.
    Yield(ValueId),
    Ret(ValueId),
    Break,
    Continue,
    /// `cough`.
    Throw(ValueId),
}

/// What a pass may do with an instruction (cig-compile#33). Read from the kind and
/// the types of its operands: an operation that cannot fail for the types
/// it is given is pure, and the same operation over `dynamic` operands
/// may raise.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    /// Raises nothing, writes nothing, reads nothing that changes: may
    /// move, merge or vanish. A fresh list, map or closure is pure but has
    /// an identity of its own: a pass never merges two of them.
    Pure,
    /// Pure but for the error the interpreter raises for some operands
    /// (E503 overflow, E403 a type error, a missing key, an index out of
    /// range, a NaN ordered): keeps its place and its order (T1, T2), and
    /// goes only where the verifier proves it cannot raise (T4).
    MayRaise,
    /// Writes a slot, an element or the world, reads a slot that can
    /// change, calls, or carries control: keeps its place and its order,
    /// and is never lost (T1 to T3).
    Effect,
    /// A step mark: keeps its place and its order, and goes only with its
    /// whole block (T3).
    Step,
}

impl Class {
    /// T1 and T2 hold it in place; anything else may move.
    pub fn is_pinned(self) -> bool {
        self != Class::Pure
    }
}

/// What the interpreter computes for `a op b` over two constants, where
/// it cannot fail: integer `+ - *` that do not overflow, string `+`,
/// `==` and `!=` of like constants, the order of two ints. `None` for
/// everything else, which is left to run (and maybe raise) at its span.
/// Both `fold-consts` and the verifier's T4 read it, so a fold the
/// verifier accepts is a fold the interpreter agrees with.
pub fn fold_binary(op: BinOp, a: &Const, b: &Const) -> Option<Const> {
    use Const::*;
    let same_kind = matches!(
        (a, b),
        (Null, Null) | (Bool(_), Bool(_)) | (Int(_), Int(_)) | (Str(_), Str(_))
    );
    Some(match (op, a, b) {
        (BinOp::Add, Int(x), Int(y)) => Int(x.checked_add(*y)?),
        (BinOp::Sub, Int(x), Int(y)) => Int(x.checked_sub(*y)?),
        (BinOp::Mul, Int(x), Int(y)) => Int(x.checked_mul(*y)?),
        (BinOp::Add, Str(x), Str(y)) => Str(format!("{x}{y}")),
        (BinOp::Eq, x, y) if same_kind => Bool(x == y),
        (BinOp::Ne, x, y) if same_kind => Bool(x != y),
        (BinOp::Lt, Int(x), Int(y)) => Bool(x < y),
        (BinOp::Le, Int(x), Int(y)) => Bool(x <= y),
        (BinOp::Gt, Int(x), Int(y)) => Bool(x > y),
        (BinOp::Ge, Int(x), Int(y)) => Bool(x >= y),
        _ => return None,
    })
}

/// `fold_binary` for the unary operators: `not` of a boolean.
pub fn fold_unary(op: UnOp, x: &Const) -> Option<Const> {
    match (op, x) {
        (UnOp::Not, Const::Bool(b)) => Some(Const::Bool(!b)),
        _ => None,
    }
}

impl Const {
    pub fn ty(&self) -> Ty {
        match self {
            Const::Null => Ty::Null,
            Const::Bool(_) => Ty::Bool,
            Const::Int(_) => Ty::Int,
            Const::Float(_) => Ty::Float,
            Const::Str(_) => Ty::Str,
        }
    }
}

impl Kind {
    /// What a pass may do with this instruction in `func`: `vals` gives
    /// its operands' types, `vars` its slots.
    pub fn class(&self, func: &Func, vars: &[VarInfo]) -> Class {
        use Ty::*;
        let ty = |v: &ValueId| func.vals.get(v.0 as usize).map_or(Dynamic, |x| x.ty);
        let raises_unless = |pure: bool| if pure { Class::Pure } else { Class::MayRaise };
        match self {
            Kind::Step(_) => Class::Step,
            Kind::Const(_) | Kind::Prelude(_) | Kind::Closure(_) | Kind::MakeList(_) => Class::Pure,
            // Display never fails.
            Kind::Interp(_) => Class::Pure,
            // A slot that never changes once declared, read in the function
            // that owns it (where lowering never reads it before its
            // `decl`); any other read sees writes, or a capture made
            // before its declaration ran.
            Kind::Load(v) => match vars.get(v.0 as usize) {
                Some(x) if x.owner == func.id && !x.mutable && !x.captured => Class::Pure,
                _ => Class::Effect,
            },
            Kind::Unary(UnOp::Not, _) => Class::Pure,
            Kind::Unary(UnOp::Neg, x) => raises_unless(ty(x) == Float),
            Kind::Binary(op, a, b) => {
                let (a, b) = (ty(a), ty(b));
                raises_unless(match op {
                    BinOp::Eq | BinOp::Ne => true,
                    BinOp::Add => matches!((a, b), (Float, Float) | (Str, Str) | (List, List)),
                    BinOp::Sub | BinOp::Mul => (a, b) == (Float, Float),
                    // Floats are ordered too, but a NaN raises.
                    BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                        matches!((a, b), (Int, Int) | (Str, Str))
                    }
                    _ => false,
                })
            }
            Kind::MakeMap(kv) => raises_unless(kv.iter().all(|(k, _)| match k {
                MapKey::Static(_) => true,
                MapKey::Dyn(d, _) => ty(d) == Str,
            })),
            Kind::Member { obj, safe, .. } => raises_unless(*safe && matches!(ty(obj), Map | Null)),
            Kind::Index { .. } => Class::MayRaise,
            Kind::Key(k) => raises_unless(matches!(ty(k), Str | Int)),
            _ => Class::Effect,
        }
    }

    pub fn is_terminator(&self) -> bool {
        matches!(
            self,
            Kind::Yield(_) | Kind::Ret(_) | Kind::Break | Kind::Continue | Kind::Throw(_)
        )
    }

    /// The nested blocks, in evaluation order.
    pub fn blocks(&self) -> Vec<&Block> {
        self.slots().into_iter().map(|(_, b)| b).collect()
    }

    /// The nested blocks with where each one is (`if.then`,
    /// `burn.compensate`), in evaluation order. The transition check
    /// compares these, so moving an instruction out of a branch, or from a
    /// burn's body into its compensation, is seen.
    pub fn slots(&self) -> Vec<(&'static str, &Block)> {
        match self {
            Kind::Short { rhs, .. } => vec![("short.rhs", rhs)],
            Kind::Burn(b) => {
                let mut v = vec![("burn.body", &b.body)];
                if let Some(c) = &b.compensate {
                    v.push(("burn.compensate", c));
                }
                v
            }
            Kind::Try {
                body,
                catch,
                finally,
            } => {
                let mut v = vec![("try.body", body)];
                if let Some((_, c)) = catch {
                    v.push(("try.catch", c));
                }
                if let Some(f) = finally {
                    v.push(("try.finally", f));
                }
                v
            }
            Kind::If {
                then, otherwise, ..
            } => {
                let mut v = vec![("if.then", then)];
                if let Some(o) = otherwise {
                    v.push(("if.else", o));
                }
                v
            }
            Kind::Loop { cond, body } => vec![("loop.cond", cond), ("loop.body", body)],
            Kind::ForEach { body, .. } => vec![("foreach.body", body)],
            _ => Vec::new(),
        }
    }

    pub fn blocks_mut(&mut self) -> Vec<&mut Block> {
        match self {
            Kind::Short { rhs, .. } => vec![rhs],
            Kind::Burn(b) => {
                let b = &mut **b;
                let mut v = vec![&mut b.body];
                if let Some(c) = &mut b.compensate {
                    v.push(c);
                }
                v
            }
            Kind::Try {
                body,
                catch,
                finally,
            } => {
                let mut v = vec![body];
                if let Some((_, c)) = catch {
                    v.push(c);
                }
                if let Some(f) = finally {
                    v.push(f);
                }
                v
            }
            Kind::If {
                then, otherwise, ..
            } => {
                let mut v = vec![then];
                if let Some(o) = otherwise {
                    v.push(o);
                }
                v
            }
            Kind::Loop { cond, body } => vec![cond, body],
            Kind::ForEach { body, .. } => vec![body],
            _ => Vec::new(),
        }
    }

    /// Every value it reads, nested blocks not included.
    pub fn uses(&self) -> Vec<ValueId> {
        match self {
            Kind::Step(_)
            | Kind::Const(_)
            | Kind::Prelude(_)
            | Kind::Load(_)
            | Kind::Closure(_)
            | Kind::Break
            | Kind::Continue
            | Kind::Loop { .. }
            | Kind::Try { .. } => Vec::new(),
            Kind::Decl(_, v) | Kind::Store(_, v) | Kind::Unary(_, v) => vec![*v],
            Kind::Binary(_, a, b) => vec![*a, *b],
            Kind::Short { subject, .. } => vec![*subject],
            Kind::MakeList(v) | Kind::PackDecl(v) | Kind::Exhale(v) => v.clone(),
            Kind::MakeMap(kv) => {
                let mut v = Vec::new();
                for (k, x) in kv {
                    if let MapKey::Dyn(k, _) = k {
                        v.push(*k);
                    }
                    v.push(*x);
                }
                v
            }
            Kind::Interp(p) => p
                .iter()
                .filter_map(|p| match p {
                    Piece::Val(v) => Some(*v),
                    Piece::Lit(_) => None,
                })
                .collect(),
            Kind::Member { obj, .. } => vec![*obj],
            Kind::Index { obj, idx, .. } => vec![*obj, *idx],
            Kind::Key(k) => vec![*k],
            Kind::SetIndex { obj, idx, val, .. } => vec![*obj, *idx, *val],
            Kind::SetMember { obj, val, .. } => vec![*obj, *val],
            Kind::BurnOp { args, .. }
            | Kind::CallBuiltin { args, .. }
            | Kind::CallUser { args, .. } => args.clone(),
            Kind::CallValue { f, args } => {
                let mut v = vec![*f];
                v.extend(args);
                v
            }
            Kind::CallMethod { recv, args, .. } => {
                let mut v = vec![*recv];
                v.extend(args);
                v
            }
            Kind::MakeChain { steps, .. } => steps.iter().map(|s| s.1).collect(),
            Kind::Burn(b) => b.label.into_iter().collect(),
            Kind::If { cond, .. } => vec![*cond],
            Kind::ForEach { iter, .. } => vec![*iter],
            Kind::Yield(v) | Kind::Ret(v) | Kind::Throw(v) => vec![*v],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cig_native::tagset::TagSet;
    use cig_native::value;

    #[test]
    fn ty_tags_maps_each_kind_to_its_tag_or_every_tag() {
        let one = |t: u8| TagSet::of(t).expect("every listed tag is a tag");
        let cases: [(Ty, TagSet); 9] = [
            (Ty::Dynamic, TagSet::ALL),
            (Ty::Null, one(value::NULL)),
            (Ty::Bool, one(value::BOOL)),
            (Ty::Int, one(value::INT)),
            (Ty::Float, one(value::FLOAT)),
            (Ty::Str, one(value::STR)),
            (Ty::List, one(value::LIST)),
            (Ty::Map, one(value::MAP)),
            (Ty::Func, one(value::FUNC)),
        ];
        for (ty, expected) in cases {
            assert_eq!(ty.tags(), expected, "tags for {ty:?}");
        }
        // Dynamic reaches every tag, each singleton reaches one.
        assert_eq!(Ty::Dynamic.tags().len(), value::TAG_COUNT as u32);
        for (ty, _) in &cases[1..] {
            assert_eq!(ty.tags().len(), 1);
            assert!(ty.tags().within(TagSet::ALL));
        }
    }
}
