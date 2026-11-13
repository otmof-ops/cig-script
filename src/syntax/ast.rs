// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The abstract syntax tree.

use super::span::Span;
use std::cell::Cell;
use std::rc::Rc;

/// Where `cig check` found a name: written by the checker, read by the
/// evaluator (cig-script#146). `Unresolved` is the answer for anything the
/// checker did not see or could not be sure of, and means "look the name up".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Res {
    #[default]
    Unresolved,
    /// A builtin or module, held by the prelude.
    Prelude,
    /// `depth` scopes up from the one the read runs in, the `idx`th name
    /// declared there.
    Local { depth: u16, idx: u16 },
}

#[derive(Clone, Debug)]
pub struct Program {
    pub body: Vec<Stmt>,
}

impl Program {
    /// Whether the script's `pack { }` says `own_flags: true`: a program
    /// compiled from it takes every argument as its own, and no run flags
    /// (docs/PROGRAMS.md). Only the literal `true` counts; a script that
    /// does not pass `cig check` is never compiled.
    pub fn own_flags(&self) -> bool {
        self.body.iter().any(|s| match s {
            Stmt::Pack(p) => p
                .option(PACK_OWN_FLAGS)
                .is_some_and(|o| matches!(o.value.kind, ExprKind::Bool(true))),
            _ => false,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

impl Block {
    /// Whether running this block puts a name in the scope it runs in: a
    /// `roll`/`stick`, a `pull` or a chain. A block that does not needs no
    /// scope of its own, and neither the checker nor the evaluator makes one
    /// (cig-script#146), so a name resolved through it is the same number
    /// of scopes up either way.
    pub fn declares(&self) -> bool {
        self.stmts
            .iter()
            .any(|s| matches!(s, Stmt::Declare { .. } | Stmt::Pull(_) | Stmt::Chain(_)))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BurnClass {
    /// Executes in `run`, is simulated in `dry-run`.
    Normal,
    /// Never executes: effects are recorded as intents only.
    Unlit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignOp {
    Set,
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Clone, Debug)]
pub enum AssignTarget {
    /// The name, and where the checker found it.
    Var(String, Cell<Res>),
    Index {
        object: Expr,
        index: Expr,
    },
    Member {
        object: Expr,
        name: String,
        /// Where the key was last found in the map: a hint, checked against
        /// the key before it is trusted (cig-script#146).
        hint: Cell<u32>,
    },
}

#[derive(Clone, Debug)]
pub struct FnDecl {
    pub name: String,
    pub params: Vec<String>,
    pub body: Rc<Block>,
    pub span: Span,
}

/// `chain name { step, step, ... }`: an ordered list of sticks to light.
#[derive(Clone, Debug)]
pub struct ChainDecl {
    pub name: String,
    pub steps: Vec<Expr>,
    pub span: Span,
}

/// `pack { "./build", "~/Downloads" }`: the scope every write must stay in.
/// Beside the roots, options: `pack { ".", own_flags: true }`. A pack with
/// no root declares no scope (`pack { own_flags: true }`).
#[derive(Clone, Debug)]
pub struct PackDecl {
    pub roots: Vec<Expr>,
    /// `name: value` beside the roots, in the order written.
    pub options: Vec<PackOption>,
    pub span: Span,
}

/// The pack options cig knows.
pub const PACK_OWN_FLAGS: &str = "own_flags";
pub const PACK_OPTIONS: &[&str] = &[PACK_OWN_FLAGS];

/// One `name: value` in a `pack { }`.
#[derive(Clone, Debug)]
pub struct PackOption {
    pub name: String,
    /// Where the name is written.
    pub name_span: Span,
    pub value: Expr,
}

impl PackDecl {
    /// The first option called `name`, if the pack gives one.
    pub fn option(&self, name: &str) -> Option<&PackOption> {
        self.options.iter().find(|o| o.name == name)
    }
}

#[derive(Clone, Debug)]
pub enum Stmt {
    /// `roll x = e` (mutable) or `stick x = e` (immutable).
    Declare {
        name: String,
        mutable: bool,
        value: Expr,
        span: Span,
    },
    Assign {
        target: AssignTarget,
        op: AssignOp,
        value: Expr,
        span: Span,
    },
    Pull(FnDecl),
    Chain(ChainDecl),
    Pack(PackDecl),
    Snuff {
        value: Option<Expr>,
        span: Span,
    },
    Exhale {
        values: Vec<Expr>,
        span: Span,
    },
    Cough {
        value: Expr,
        span: Span,
    },
    If {
        branches: Vec<(Expr, Block)>,
        otherwise: Option<Block>,
        span: Span,
    },
    While {
        cond: Expr,
        body: Block,
        span: Span,
    },
    For {
        var: String,
        iter: Expr,
        body: Block,
        span: Span,
    },
    Break(Span),
    Continue(Span),
    Try {
        body: Block,
        catch_var: Option<String>,
        /// The `ashtray` block; absent for `try { } finally { }`.
        handler: Option<Block>,
        /// Runs whatever happened; its own signal wins.
        finally: Option<Block>,
        span: Span,
    },
    Burn {
        class: BurnClass,
        /// `burn (state) { }`: a fresh map, visible in the body and the
        /// compensation, journaled with it.
        state: Option<String>,
        /// `burn ("deploy") { }` or `burn (state, label) { }`: a name for the
        /// burn, a string, used in place of its line wherever the plan, the
        /// record or a rollback names it.
        label: Option<Expr>,
        body: Block,
        /// `unburn { }`: the compensation, run on rollback.
        unburn: Option<Block>,
        /// The compensation's source text, journaled so `cig unburn` can run
        /// it later without the script.
        unburn_src: Option<String>,
        span: Span,
    },
    Expr(Expr),
}

impl Stmt {
    pub fn span(&self) -> Span {
        match self {
            Stmt::Declare { span, .. }
            | Stmt::Assign { span, .. }
            | Stmt::Snuff { span, .. }
            | Stmt::Exhale { span, .. }
            | Stmt::Cough { span, .. }
            | Stmt::If { span, .. }
            | Stmt::While { span, .. }
            | Stmt::For { span, .. }
            | Stmt::Try { span, .. }
            | Stmt::Pack(PackDecl { span, .. })
            | Stmt::Burn { span, .. } => *span,
            Stmt::Pull(f) => f.span,
            Stmt::Chain(c) => c.span,
            Stmt::Break(s) | Stmt::Continue(s) => *s,
            Stmt::Expr(e) => e.span,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Coalesce,
    Range,
}

impl BinOp {
    pub fn symbol(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::And => "and",
            BinOp::Or => "or",
            BinOp::Coalesce => "??",
            BinOp::Range => "..",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

/// A piece of an interpolated string.
#[derive(Clone, Debug)]
pub enum StrPiece {
    Lit(String),
    Expr(Expr),
}

/// A map literal key: `name: v`, `"text": v` or `[expr]: v`.
#[derive(Clone, Debug)]
pub enum MapKey {
    Static(String),
    Dynamic(Expr),
}

#[derive(Clone, Debug)]
pub enum LambdaBody {
    Expr(Rc<Expr>),
    Block(Rc<Block>),
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
    /// Set by the checker on an `Ident`; `Unresolved` everywhere else.
    pub res: Cell<Res>,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Vec<StrPiece>),
    Ident(String),
    List(Vec<Expr>),
    Map(Vec<(MapKey, Expr)>),
    Unary {
        op: UnOp,
        expr: Box<Expr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    /// `recv.name(args)` or `recv?.name(args)`.
    Method {
        receiver: Box<Expr>,
        name: String,
        args: Vec<Expr>,
        safe: bool,
    },
    /// `obj.name` or `obj?.name`.
    Member {
        object: Box<Expr>,
        name: String,
        safe: bool,
        /// Where the key was last found in the map (see `AssignTarget::Member`).
        hint: Cell<u32>,
    },
    Index {
        object: Box<Expr>,
        index: Box<Expr>,
    },
    Lambda {
        params: Vec<String>,
        body: LambdaBody,
    },
    /// `lhs >> rhs`: call `rhs` with `lhs` prepended to its arguments.
    Pipe {
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
}

impl Expr {
    pub fn new(kind: ExprKind, span: Span) -> Self {
        Self {
            kind,
            span,
            res: Cell::new(Res::Unresolved),
        }
    }
}
