// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The operators, once: what `+`, `-`, `*`, `/`, `%`, the comparisons,
//! `==` and unary `-` do to two values. The tree-walker and the bytecode
//! VM both call these functions, so the two cannot drift apart.

use crate::diagnostics::{runtime, type_error, Diagnostic};
use crate::syntax::ast::BinOp;
use crate::syntax::span::Span;
use crate::value::Value;

type Eval = Result<Value, Diagnostic>;

/// A binary operator on two evaluated operands. The short-circuit
/// operators (`&&`, `||`, `??`) never reach here: they decide whether to
/// evaluate their right side at all.
#[inline(never)]
pub(crate) fn binary(op: BinOp, l: Value, r: Value, span: Span) -> Eval {
    use Value::*;
    let type_err = |what: &str| {
        type_error(format!(
            "cannot {what} {} and {}",
            l.type_name(),
            r.type_name()
        ))
        .code("E403")
        .at(span)
    };
    match op {
        BinOp::Add => match (&l, &r) {
            (Int(a), Int(b)) => a
                .checked_add(*b)
                .map(Int)
                .ok_or_else(|| runtime("integer overflow in `+`").code("E503").at(span)),
            // One block: both texts copied once into the new string.
            (Str(a), Str(b)) => Ok(crate::value::build_str(|out| {
                out.push_str(a);
                out.push_str(b);
            })),
            (List(a), List(b)) => {
                let mut v = a.borrow().clone();
                v.extend(b.borrow().iter().cloned());
                Ok(Value::list(v))
            }
            (Str(_), other) | (other, Str(_)) if !matches!(other, Str(_)) => Err(type_err("add")
                .with_hint(
                    "build strings with interpolation: \"total: ${n}\", or convert with str(n)",
                )),
            _ => match (l.as_f64(), r.as_f64()) {
                (Some(a), Some(b)) => Ok(Float(a + b)),
                _ => Err(type_err("add")),
            },
        },
        BinOp::Sub => match (&l, &r) {
            (Int(a), Int(b)) => a
                .checked_sub(*b)
                .map(Int)
                .ok_or_else(|| runtime("integer overflow in `-`").code("E503").at(span)),
            _ => match (l.as_f64(), r.as_f64()) {
                (Some(a), Some(b)) => Ok(Float(a - b)),
                _ => Err(type_err("subtract")),
            },
        },
        BinOp::Mul => match (&l, &r) {
            (Int(a), Int(b)) => a
                .checked_mul(*b)
                .map(Int)
                .ok_or_else(|| runtime("integer overflow in `*`").code("E503").at(span)),
            (Str(s), Int(n)) | (Int(n), Str(s)) => {
                if *n < 0 {
                    return Err(
                        runtime("cannot repeat a string a negative number of times").at(span)
                    );
                }
                crate::value::check_alloc(
                    s.len() as u128 * *n as u128,
                    "repeating this string",
                    span,
                )?;
                Ok(Value::str(s.repeat(*n as usize)))
            }
            _ => match (l.as_f64(), r.as_f64()) {
                (Some(a), Some(b)) => Ok(Float(a * b)),
                _ => Err(type_err("multiply")),
            },
        },
        BinOp::Div => match (&l, &r) {
            (Int(_), Int(0)) => Err(runtime("division by zero").code("E502").at(span)),
            // The one quotient that does not fit: `%` would overflow
            // before `checked_div` got to say so.
            (Int(i64::MIN), Int(-1)) => {
                Err(runtime("integer overflow in `/`").code("E503").at(span))
            }
            (Int(a), Int(b)) if a % b == 0 => a
                .checked_div(*b)
                .map(Int)
                .ok_or_else(|| runtime("integer overflow in `/`").code("E503").at(span)),
            _ => match (l.as_f64(), r.as_f64()) {
                (Some(a), Some(b)) => {
                    if b == 0.0 {
                        Err(runtime("division by zero").code("E502").at(span))
                    } else {
                        Ok(Float(a / b))
                    }
                }
                _ => Err(type_err("divide")),
            },
        },
        BinOp::Rem => match (&l, &r) {
            (Int(_), Int(0)) => Err(runtime("modulo by zero").code("E502").at(span)),
            (Int(a), Int(b)) => Ok(Int(a.wrapping_rem(*b))),
            _ => match (l.as_f64(), r.as_f64()) {
                (Some(a), Some(b)) => {
                    if b == 0.0 {
                        Err(runtime("modulo by zero").code("E502").at(span))
                    } else {
                        Ok(Float(a % b))
                    }
                }
                _ => Err(type_err("take the remainder of")),
            },
        },
        BinOp::Eq => Ok(Bool(l.equals(&r))),
        BinOp::Ne => Ok(Bool(!l.equals(&r))),
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            let ord = match (&l, &r) {
                (Int(a), Int(b)) => a.cmp(b),
                (Str(a), Str(b)) => a.cmp(b),
                _ => match (l.as_f64(), r.as_f64()) {
                    (Some(a), Some(b)) => a
                        .partial_cmp(&b)
                        .ok_or_else(|| runtime("cannot order a NaN").at(span))?,
                    _ => return Err(type_err("compare")),
                },
            };
            Ok(Bool(match op {
                BinOp::Lt => ord.is_lt(),
                BinOp::Le => ord.is_le(),
                BinOp::Gt => ord.is_gt(),
                _ => ord.is_ge(),
            }))
        }
        BinOp::Range => match (&l, &r) {
            (Int(a), Int(b)) => {
                range_fits(*a, *b, span)?;
                Ok(Value::list((*a..*b).map(Int).collect()))
            }
            _ => Err(type_err("make a range from").with_hint("write the range as `int..int`")),
        },
        BinOp::And | BinOp::Or | BinOp::Coalesce => {
            unreachable!("short-circuit ops are handled in eval")
        }
    }
}

/// Unary `-`: ints (with overflow) and floats; anything else is E403.
pub(crate) fn negate(v: Value, span: Span) -> Eval {
    match v {
        Value::Int(i) => i.checked_neg().map(Value::Int).ok_or_else(|| {
            runtime("integer overflow in negation")
                .code("E503")
                .at(span)
        }),
        Value::Float(f) => Ok(Value::Float(-f)),
        other => Err(type_error(format!("cannot negate {}", other.type_name()))
            .code("E403")
            .at(span)),
    }
}

/// The common cases of a binary operator, on two ints or two floats, without
/// the general match. `None` is "not here": the general `binary` runs, and
/// gives the same answer or the same error (overflow, division by zero, a
/// NaN in an ordering) as before.
#[inline(always)]
pub(crate) fn fast_binary(op: BinOp, l: &Value, r: &Value) -> Option<Value> {
    use Value::{Bool, Float, Int};
    match (l, r) {
        (Int(a), Int(b)) => match op {
            BinOp::Add => a.checked_add(*b).map(Int),
            BinOp::Sub => a.checked_sub(*b).map(Int),
            BinOp::Mul => a.checked_mul(*b).map(Int),
            BinOp::Rem if *b != 0 => Some(Int(a.wrapping_rem(*b))),
            BinOp::Lt => Some(Bool(a < b)),
            BinOp::Le => Some(Bool(a <= b)),
            BinOp::Gt => Some(Bool(a > b)),
            BinOp::Ge => Some(Bool(a >= b)),
            BinOp::Eq => Some(Bool(a == b)),
            BinOp::Ne => Some(Bool(a != b)),
            _ => None,
        },
        (Float(a), Float(b)) => match op {
            BinOp::Add => Some(Float(a + b)),
            BinOp::Sub => Some(Float(a - b)),
            BinOp::Mul => Some(Float(a * b)),
            BinOp::Div if *b != 0.0 => Some(Float(a / b)),
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let ord = a.partial_cmp(b)?;
                Some(Bool(match op {
                    BinOp::Lt => ord.is_lt(),
                    BinOp::Le => ord.is_le(),
                    BinOp::Gt => ord.is_gt(),
                    _ => ord.is_ge(),
                }))
            }
            _ => None,
        },
        _ => None,
    }
}

/// A range holds at most 10,000,000 numbers. The width is computed without
/// overflow, so `i64::MIN..i64::MAX` is refused too, not wrapped.
pub(crate) fn range_fits(a: i64, b: i64, span: Span) -> Result<(), Diagnostic> {
    if i128::from(b) - i128::from(a) > 10_000_000 {
        return Err(runtime("range is too large (limit 10,000,000)")
            .code("E512")
            .at(span));
    }
    Ok(())
}
