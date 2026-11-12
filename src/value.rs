// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Runtime values.
//!
//! Lists and maps are reference types (shared, mutable), everything else is
//! a value type. Maps keep insertion order so output is deterministic.

use crate::diagnostics::{type_error, Diagnostic};
use crate::interp::env::Env;
use crate::interp::Interp;
use crate::syntax::ast::{Block, Expr};
use crate::syntax::span::Span;
use indexmap::IndexMap;
use std::cell::RefCell;
use std::fmt::Write as _;
use std::rc::Rc;

pub type List = Rc<RefCell<Vec<Value>>>;
pub type Map = Rc<RefCell<IndexMap<String, Value>>>;

#[derive(Clone)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Rc<str>),
    List(List),
    Map(Map),
    Func(Rc<Function>),
    Builtin(Rc<Builtin>),
    Module(Rc<Module>),
    Chain(Rc<Chain>),
}

/// An ordered list of steps declared with `chain name { ... }`.
pub struct Chain {
    pub name: String,
    pub steps: Vec<ChainStep>,
}

pub struct ChainStep {
    pub label: String,
    pub value: Value,
    /// Where the step was written, for diagnostics.
    pub span: Span,
}

/// A script-defined function (`pull` or `pack`).
pub struct Function {
    pub name: Option<String>,
    pub params: Vec<String>,
    pub body: FnBody,
    pub closure: Env,
}

pub enum FnBody {
    Block(Rc<Block>),
    Expr(Rc<Expr>),
    /// A function of a bytecode module, run by the VM (cig-compile#31).
    Compiled(crate::vm::CompiledFn),
}

/// What a builtin does to the world. Anything except `Pure` and `Read`
/// requires an enclosing `burn` block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Pure,
    Read,
    Write,
    Proc,
    Env,
}

impl Effect {
    pub fn needs_burn(self) -> bool {
        !matches!(self, Effect::Pure | Effect::Read)
    }
}

pub type BuiltinFn = fn(&mut Interp, &[Value], Span) -> Result<Value, Diagnostic>;

#[derive(Clone, Copy)]
pub struct Builtin {
    /// Fully qualified name, e.g. `fs.write_text` or `len`.
    pub name: &'static str,
    pub effect: Effect,
    pub min_args: usize,
    /// `None` means variadic.
    pub max_args: Option<usize>,
    pub func: BuiltinFn,
}

impl Builtin {
    pub fn check_arity(&self, n: usize, span: Span) -> Result<(), Diagnostic> {
        let ok = n >= self.min_args && self.max_args.is_none_or(|m| n <= m);
        if ok {
            return Ok(());
        }
        let expected = match (self.min_args, self.max_args) {
            (a, Some(b)) if a == b => format!("{a}"),
            (a, Some(b)) => format!("{a} to {b}"),
            (a, None) => format!("at least {a}"),
        };
        Err(type_error(format!(
            "{} expects {expected} argument{}, got {n}",
            self.name,
            if expected == "1" { "" } else { "s" }
        ))
        .code("E401")
        .at(span))
    }
}

/// A namespace of builtins such as `fs` or `json`.
pub struct Module {
    pub name: &'static str,
    pub items: IndexMap<&'static str, Value>,
}

impl Value {
    pub fn str(s: impl AsRef<str>) -> Value {
        Value::Str(Rc::from(s.as_ref()))
    }

    pub fn list(items: Vec<Value>) -> Value {
        Value::List(Rc::new(RefCell::new(items)))
    }

    pub fn map(entries: IndexMap<String, Value>) -> Value {
        Value::Map(Rc::new(RefCell::new(entries)))
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Str(_) => "string",
            Value::List(_) => "list",
            Value::Map(_) => "map",
            Value::Func(_) | Value::Builtin(_) => "function",
            Value::Module(_) => "module",
            Value::Chain(_) => "chain",
        }
    }

    pub fn truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Int(i) => *i != 0,
            Value::Float(f) => *f != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::List(l) => !l.borrow().is_empty(),
            Value::Map(m) => !m.borrow().is_empty(),
            Value::Func(_) | Value::Builtin(_) | Value::Module(_) | Value::Chain(_) => true,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// Structural equality. Ints and floats compare numerically.
    pub fn equals(&self, other: &Value) -> bool {
        self.equals_in(other, &mut Vec::new())
    }

    /// Structural equality that ends on values that contain themselves: a
    /// pair of lists or maps already being compared further up the walk
    /// counts as equal, since nothing in them has differed so far.
    fn equals_in(&self, other: &Value, seen: &mut Vec<(usize, usize)>) -> bool {
        match (self, other) {
            (Value::Null, Value::Null) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Int(a), Value::Float(b)) | (Value::Float(b), Value::Int(a)) => {
                (*a as f64) == *b
            }
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::List(a), Value::List(b)) => {
                if Rc::ptr_eq(a, b) {
                    return true;
                }
                let key = (
                    Rc::as_ptr(a) as *const () as usize,
                    Rc::as_ptr(b) as *const () as usize,
                );
                if seen.contains(&key) {
                    return true;
                }
                seen.push(key);
                let (a, b) = (a.borrow(), b.borrow());
                let eq =
                    a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| x.equals_in(y, seen));
                seen.pop();
                eq
            }
            (Value::Map(a), Value::Map(b)) => {
                if Rc::ptr_eq(a, b) {
                    return true;
                }
                let key = (
                    Rc::as_ptr(a) as *const () as usize,
                    Rc::as_ptr(b) as *const () as usize,
                );
                if seen.contains(&key) {
                    return true;
                }
                seen.push(key);
                let (a, b) = (a.borrow(), b.borrow());
                let eq = a.len() == b.len()
                    && a.iter()
                        .all(|(k, v)| b.get(k).is_some_and(|w| v.equals_in(w, seen)));
                seen.pop();
                eq
            }
            (Value::Func(a), Value::Func(b)) => Rc::ptr_eq(a, b),
            (Value::Builtin(a), Value::Builtin(b)) => Rc::ptr_eq(a, b),
            (Value::Module(a), Value::Module(b)) => Rc::ptr_eq(a, b),
            (Value::Chain(a), Value::Chain(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }

    /// How `exhale` and string interpolation show a value: strings raw,
    /// everything else as its literal form.
    pub fn display(&self) -> String {
        match self {
            Value::Str(s) => s.to_string(),
            other => other.repr(),
        }
    }

    /// The literal form of a value, round-trippable for data types.
    pub fn repr(&self) -> String {
        let mut out = String::new();
        self.write_repr(&mut out, 0);
        out
    }

    /// Nesting past which a printed value is elided: a list that contains
    /// itself, or a value built deeper than anyone reads, prints `…` there
    /// instead of overflowing the stack.
    pub const MAX_REPR_DEPTH: usize = 512;

    fn write_repr(&self, out: &mut String, depth: usize) {
        if depth >= Self::MAX_REPR_DEPTH && matches!(self, Value::List(_) | Value::Map(_)) {
            out.push('…');
            return;
        }
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Int(i) => {
                let _ = write!(out, "{i}");
            }
            Value::Float(f) => out.push_str(&format_float(*f)),
            Value::Str(s) => write_quoted(out, s),
            Value::List(l) => {
                out.push('[');
                for (i, v) in l.borrow().iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    v.write_repr(out, depth + 1);
                }
                out.push(']');
            }
            Value::Map(m) => {
                out.push('{');
                for (i, (k, v)) in m.borrow().iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    if is_plain_ident(k) {
                        out.push_str(k);
                    } else {
                        write_quoted(out, k);
                    }
                    out.push_str(": ");
                    v.write_repr(out, depth + 1);
                }
                out.push('}');
            }
            Value::Func(f) => match &f.name {
                Some(n) => {
                    let _ = write!(out, "<pull {n}>");
                }
                None => out.push_str("<pack>"),
            },
            Value::Builtin(b) => {
                let _ = write!(out, "<builtin {}>", b.name);
            }
            Value::Module(m) => {
                let _ = write!(out, "<module {}>", m.name);
            }
            Value::Chain(c) => {
                let labels: Vec<&str> = c.steps.iter().map(|s| s.label.as_str()).collect();
                let _ = write!(out, "<chain {}: {}>", c.name, labels.join(" > "));
            }
        }
    }
}

pub fn format_float(f: f64) -> String {
    if f.is_nan() {
        "nan".to_string()
    } else if f.is_infinite() {
        if f > 0.0 { "inf" } else { "-inf" }.to_string()
    } else if f.abs() >= 1e16 {
        // Past sixteen digits Display prints a bare integer that reads as
        // an int; Debug keeps the exponent form, which is still a float.
        format!("{f:?}")
    } else if f.fract() == 0.0 {
        format!("{f:.1}")
    } else {
        format!("{f}")
    }
}

fn is_plain_ident(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn write_quoted(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '$' => out.push_str("\\$"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:x}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

// ----- resource ceilings -----------------------------------------------------------

/// Largest value the runtime will build in one go, in bytes. `CIG_MAX_ALLOC`
/// overrides the default of 256 MiB. A request above it is a diagnostic
/// (E514), never an allocation failure that aborts the process.
pub fn max_alloc() -> usize {
    use std::sync::OnceLock;
    static LIMIT: OnceLock<usize> = OnceLock::new();
    *LIMIT.get_or_init(|| {
        std::env::var("CIG_MAX_ALLOC")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(256 * 1024 * 1024)
    })
}

/// Refuse to build `bytes` bytes for `what` when it is over the ceiling.
pub fn check_alloc(bytes: u128, what: &str, span: Span) -> Result<(), Diagnostic> {
    let limit = max_alloc() as u128;
    if bytes > limit {
        return Err(crate::diagnostics::runtime(format!(
            "{what} would need {bytes} bytes, over the ceiling of {limit}"
        ))
        .code("E514")
        .at(span)
        .with_hint("work in smaller pieces, or raise the ceiling with CIG_MAX_ALLOC=<bytes>"));
    }
    Ok(())
}

// ----- argument helpers for builtins ------------------------------------------

pub fn expect_str<'a>(
    args: &'a [Value],
    i: usize,
    fname: &str,
    span: Span,
) -> Result<&'a str, Diagnostic> {
    match args.get(i) {
        Some(Value::Str(s)) => Ok(s),
        Some(other) => Err(arg_type(fname, i, "a string", other, span)),
        None => Err(type_error(format!("{fname}: missing argument {}", i + 1))
            .code("E401")
            .at(span)),
    }
}

pub fn expect_int(args: &[Value], i: usize, fname: &str, span: Span) -> Result<i64, Diagnostic> {
    match args.get(i) {
        Some(Value::Int(v)) => Ok(*v),
        Some(Value::Float(f)) if f.fract() == 0.0 => Ok(*f as i64),
        Some(other) => Err(arg_type(fname, i, "an int", other, span)),
        None => Err(type_error(format!("{fname}: missing argument {}", i + 1))
            .code("E401")
            .at(span)),
    }
}

pub fn expect_num(args: &[Value], i: usize, fname: &str, span: Span) -> Result<f64, Diagnostic> {
    match args.get(i) {
        Some(v) => v
            .as_f64()
            .ok_or_else(|| arg_type(fname, i, "a number", v, span)),
        None => Err(type_error(format!("{fname}: missing argument {}", i + 1))
            .code("E401")
            .at(span)),
    }
}

pub fn expect_list(args: &[Value], i: usize, fname: &str, span: Span) -> Result<List, Diagnostic> {
    match args.get(i) {
        Some(Value::List(l)) => Ok(l.clone()),
        Some(other) => Err(arg_type(fname, i, "a list", other, span)),
        None => Err(type_error(format!("{fname}: missing argument {}", i + 1))
            .code("E401")
            .at(span)),
    }
}

pub fn expect_map(args: &[Value], i: usize, fname: &str, span: Span) -> Result<Map, Diagnostic> {
    match args.get(i) {
        Some(Value::Map(m)) => Ok(m.clone()),
        Some(other) => Err(arg_type(fname, i, "a map", other, span)),
        None => Err(type_error(format!("{fname}: missing argument {}", i + 1))
            .code("E401")
            .at(span)),
    }
}

pub fn expect_func(args: &[Value], i: usize, fname: &str, span: Span) -> Result<Value, Diagnostic> {
    match args.get(i) {
        Some(v @ (Value::Func(_) | Value::Builtin(_))) => Ok(v.clone()),
        Some(other) => Err(arg_type(fname, i, "a function", other, span)),
        None => Err(type_error(format!("{fname}: missing argument {}", i + 1))
            .code("E401")
            .at(span)),
    }
}

pub fn opt_str<'a>(
    args: &'a [Value],
    i: usize,
    fname: &str,
    span: Span,
) -> Result<Option<&'a str>, Diagnostic> {
    match args.get(i) {
        None | Some(Value::Null) => Ok(None),
        _ => expect_str(args, i, fname, span).map(Some),
    }
}

pub fn opt_map(
    args: &[Value],
    i: usize,
    fname: &str,
    span: Span,
) -> Result<Option<Map>, Diagnostic> {
    match args.get(i) {
        None | Some(Value::Null) => Ok(None),
        _ => expect_map(args, i, fname, span).map(Some),
    }
}

fn arg_type(fname: &str, i: usize, expected: &str, got: &Value, span: Span) -> Diagnostic {
    type_error(format!(
        "{fname}: argument {} must be {expected}, got {} ({})",
        i + 1,
        got.type_name(),
        truncate(&got.repr(), 40)
    ))
    .code("E402")
    .at(span)
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repr_round_trips_data() {
        let mut m = IndexMap::new();
        m.insert("a".to_string(), Value::Int(1));
        m.insert(
            "b c".to_string(),
            Value::list(vec![Value::Float(2.0), Value::str("x\"y")]),
        );
        let v = Value::map(m);
        assert_eq!(v.repr(), r#"{a: 1, "b c": [2.0, "x\"y"]}"#);
        assert_eq!(Value::Float(2.5).repr(), "2.5");
        assert_eq!(Value::str("plain").display(), "plain");
    }

    #[test]
    fn equality_and_truthiness() {
        assert!(Value::Int(1).equals(&Value::Float(1.0)));
        assert!(!Value::Int(1).equals(&Value::str("1")));
        assert!(Value::list(vec![Value::Int(1)]).equals(&Value::list(vec![Value::Int(1)])));
        assert!(!Value::list(vec![]).truthy());
        assert!(Value::str("x").truthy());
        assert!(!Value::Null.truthy());
    }
}
