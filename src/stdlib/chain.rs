// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Lighting chains: run sticks in order, with timing, retries and a report.
//!
//! A chain is automation. Each step is a function (a stick, a pull, an
//! inline pack) or another chain. Steps run one after another; a step that
//! takes an argument receives the previous step's result. The first failure
//! stops the chain and raises an error that names the step, unless
//! `continue_on_error` is set. Because steps burn like any other code, a
//! chain is previewable with `--dry-run` and rolled back like any run.

use crate::diagnostics::{runtime, type_error, Diagnostic, Kind};
use crate::interp::Interp;
use crate::syntax::span::Span;
use crate::value::{expect_int, opt_map, Chain, ChainStep, Value};
use indexmap::IndexMap;
use std::io::Write;
use std::rc::Rc;
use std::time::Instant;

pub struct Options {
    pub continue_on_error: bool,
    pub retries: i64,
    pub retry_delay_ms: i64,
    /// Retry a step even when its failed attempt ran an irreversible hop.
    pub retry_irreversible: bool,
    pub quiet: bool,
    /// Labels for the steps of a chain built at run time (`light([..])`),
    /// which otherwise read `step 1`, `step 2` in the narration, the report
    /// and the error.
    pub names: Vec<String>,
}

impl Options {
    fn parse(args: &[Value], span: Span) -> Result<Self, Diagnostic> {
        let mut o = Options {
            continue_on_error: false,
            retries: 0,
            retry_delay_ms: 0,
            retry_irreversible: false,
            quiet: false,
            names: Vec::new(),
        };
        if let Some(m) = opt_map(args, 1, "light", span)? {
            for (k, v) in m.borrow().iter() {
                match (k.as_str(), v) {
                    ("continue_on_error", v) => o.continue_on_error = v.truthy(),
                    ("quiet", v) => o.quiet = v.truthy(),
                    ("retries", Value::Int(n)) => o.retries = (*n).clamp(0, 100),
                    ("retry_delay_ms", Value::Int(n)) => {
                        o.retry_delay_ms = (*n).clamp(0, 3_600_000)
                    }
                    ("retry_irreversible", v) => o.retry_irreversible = v.truthy(),
                    ("names", Value::List(l)) => {
                        o.names = l.borrow().iter().map(|n| n.display()).collect();
                    }
                    ("names", other) => {
                        return Err(type_error(format!(
                            "light: option `names` is a list of strings, got {}",
                            other.type_name()
                        ))
                        .at(span))
                    }
                    ("retries" | "retry_delay_ms", other) => {
                        return Err(type_error(format!(
                            "light: option `{k}` must be an int, got {}",
                            other.type_name()
                        ))
                        .at(span))
                    }
                    (other, _) => {
                        return Err(runtime(format!("light: unknown option `{other}`"))
                            .at(span)
                            .with_hint(
                                "use one of continue_on_error, retries, retry_delay_ms, retry_irreversible, quiet, names",
                            ))
                    }
                }
            }
        }
        let _ = expect_int; // keep the helper import stable for future options
        Ok(o)
    }
}

/// `light(chain | [steps], opts?)`
pub fn light(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let opts = Options::parse(a, s)?;
    let chain: Rc<Chain> = match &a[0] {
        Value::Chain(c) => c.clone(),
        Value::List(items) => {
            let steps = items
                .borrow()
                .iter()
                .enumerate()
                .map(|(n, v)| ChainStep {
                    label: opts
                        .names
                        .get(n)
                        .cloned()
                        .unwrap_or_else(|| label_for(v, n)),
                    value: v.clone(),
                    span: s,
                })
                .collect();
            Rc::new(Chain {
                name: "chain".to_string(),
                steps,
            })
        }
        other => {
            return Err(type_error(format!(
                "light: expected a chain or a list of steps, got {}",
                other.type_name()
            ))
            .at(s)
            .with_hint("declare one with `chain name { step, step }` or pass `[step, step]`"))
        }
    };
    run_chain(i, &chain, &opts, s, 0)
}

/// The label a step shows in logs and reports.
pub fn label_for(v: &Value, index: usize) -> String {
    match v {
        Value::Func(f) => f
            .name
            .as_deref()
            .map_or_else(|| format!("step {}", index + 1), str::to_string),
        Value::Builtin(b) => b.name.to_string(),
        Value::Chain(c) => c.name.clone(),
        _ => format!("step {}", index + 1),
    }
}

fn run_chain(
    i: &mut Interp,
    chain: &Rc<Chain>,
    opts: &Options,
    span: Span,
    depth: usize,
) -> Result<Value, Diagnostic> {
    if i.chain_stack.iter().any(|n| n == &chain.name) {
        let mut loop_path = i.chain_stack.clone();
        loop_path.push(chain.name.clone());
        return Err(Diagnostic::new(
            Kind::Cough,
            format!(
                "chain `{}` lights itself: {}",
                chain.name,
                loop_path.join(" -> ")
            ),
        )
        .code("E604")
        .at(span)
        .with_hint("take the chain out of its own steps"));
    }
    i.chain_stack.push(chain.name.clone());
    let result = run_chain_inner(i, chain, opts, span, depth);
    i.chain_stack.pop();
    result
}

fn run_chain_inner(
    i: &mut Interp,
    chain: &Rc<Chain>,
    opts: &Options,
    span: Span,
    depth: usize,
) -> Result<Value, Diagnostic> {
    if depth > 32 {
        return Err(runtime(format!(
            "chain `{}` is nested too deeply; is it lighting itself?",
            chain.name
        ))
        .at(span));
    }
    let started = Instant::now();
    let total = chain.steps.len();
    let mut reports: Vec<Value> = Vec::with_capacity(total);
    let mut failed: Vec<Value> = Vec::new();
    let mut prev = Value::Null;
    let indent = "  ".repeat(depth);

    for (index, step) in chain.steps.iter().enumerate() {
        if !opts.quiet {
            let _ = writeln!(
                i.err,
                "{indent}chain {}: step {}/{} {} ...",
                chain.name,
                index + 1,
                total,
                step.label
            );
        }
        let step_started = Instant::now();
        i.kernel
            .push_owner(format!("{}/{}", chain.name, step.label));
        let mut attempt = 0;
        let outcome = loop {
            let mark = i.kernel.mark();
            let result = match &step.value {
                Value::Chain(inner) => run_chain(i, inner, opts, span, depth + 1),
                Value::Func(f) => {
                    let args = if f.arity() == 0 {
                        vec![]
                    } else {
                        vec![prev.clone()]
                    };
                    i.call(&step.value, args, span)
                }
                Value::Builtin(b) => {
                    let args = if b.min_args == 0 {
                        vec![]
                    } else {
                        vec![prev.clone()]
                    };
                    i.call(&step.value, args, span)
                }
                other => Err(type_error(format!(
                    "chain `{}`: step {} ({}) is a {}, not something that can be lit",
                    chain.name,
                    index + 1,
                    step.label,
                    other.type_name()
                ))
                .at(span)),
            };
            match result {
                Ok(v) => break Ok(v),
                Err(e) if attempt < opts.retries && i.exit_requested.is_none() => {
                    // A hop the kernel cannot undo is never retried on its own:
                    // the retry would run against a world the first attempt
                    // already changed.
                    if i.kernel.irreversible_since(mark) > 0 && !opts.retry_irreversible {
                        if !opts.quiet {
                            let _ = writeln!(
                                i.err,
                                "{indent}chain {}: step {} {} failed ({}); not retried: it ran a hop the kernel cannot undo",
                                chain.name,
                                index + 1,
                                step.label,
                                e.message
                            );
                        }
                        break Err(e.with_hint(
                            "this step ran a hop (a child process) the kernel cannot undo, so it was not retried; light with {retry_irreversible: true} to retry anyway",
                        ));
                    }
                    // The retry runs against the rolled-back state.
                    let undone = i.kernel.rollback_since(mark);
                    for comp in &undone.compensations {
                        if let Err(e) = i.run_compensation(comp) {
                            let _ = writeln!(
                                i.err,
                                "{indent}chain {}: step {} {}: compensation failed before the retry: {}",
                                chain.name,
                                index + 1,
                                step.label,
                                e.message
                            );
                        }
                    }
                    if !opts.quiet && !undone.restored.is_empty() {
                        let _ = writeln!(
                            i.err,
                            "{indent}chain {}: step {} {} rolled back {} op{} before the retry",
                            chain.name,
                            index + 1,
                            step.label,
                            undone.restored.len(),
                            if undone.restored.len() == 1 { "" } else { "s" }
                        );
                    }
                    attempt += 1;
                    if !opts.quiet {
                        let _ = writeln!(
                            i.err,
                            "{indent}chain {}: step {} {} failed ({}); retry {}/{}",
                            chain.name,
                            index + 1,
                            step.label,
                            e.message,
                            attempt,
                            opts.retries
                        );
                    }
                    if opts.retry_delay_ms > 0 {
                        std::thread::sleep(std::time::Duration::from_millis(
                            opts.retry_delay_ms as u64,
                        ));
                    }
                }
                Err(e) => break Err(e),
            }
        };
        i.kernel.pop_owner();
        let ms = step_started.elapsed().as_millis() as i64;
        match outcome {
            Ok(value) => {
                if !opts.quiet {
                    let _ = writeln!(
                        i.err,
                        "{indent}chain {}: step {}/{} {} ok ({ms} ms)",
                        chain.name,
                        index + 1,
                        total,
                        step.label
                    );
                }
                reports.push(step_report(
                    &step.label,
                    index,
                    true,
                    ms,
                    Some(value.clone()),
                    None,
                    attempt,
                ));
                prev = value;
            }
            Err(e) => {
                if i.exit_requested.is_some() {
                    return Err(e);
                }
                let cause = i.cough_payload.take();
                if !opts.quiet {
                    let _ = writeln!(
                        i.err,
                        "{indent}chain {}: step {}/{} {} FAILED ({ms} ms): {}",
                        chain.name,
                        index + 1,
                        total,
                        step.label,
                        e.message
                    );
                }
                let report = step_report(&step.label, index, false, ms, None, Some(&e), attempt);
                reports.push(report.clone());
                failed.push(report);
                if !opts.continue_on_error {
                    let mut payload = IndexMap::new();
                    payload.insert("chain".to_string(), Value::str(&chain.name));
                    payload.insert("step".to_string(), Value::str(&step.label));
                    payload.insert("index".to_string(), Value::Int(index as i64));
                    payload.insert("cause".to_string(), crate::interp::error_value(&e, cause));
                    payload.insert("steps".to_string(), Value::list(reports));
                    i.cough_payload = Some(Value::map(payload));
                    // A chain that lights itself is a structural fault, not a
                    // step failure: it surfaces as itself through every level.
                    if e.code == "E604" {
                        return Err(e);
                    }
                    // Point at the step as written, not at the light() call;
                    // an ad-hoc step (a list of packs) falls back to the call.
                    let at = if step.span.line == 0 { span } else { step.span };
                    let raised = match e.line {
                        Some(l) if l > 0 && l != at.line => format!(" (raised at line {l})"),
                        _ => String::new(),
                    };
                    return Err(Diagnostic::new(
                        Kind::Cough,
                        format!(
                            "chain `{}` failed at step {} ({}): {}{raised}",
                            chain.name,
                            index + 1,
                            step.label,
                            e.message
                        ),
                    )
                    .code("E602")
                    .at(at)
                    .with_hint(
                        "catch it with try/ashtray, or light with {continue_on_error: true}",
                    ));
                }
                prev = Value::Null;
            }
        }
    }

    let ms = started.elapsed().as_millis() as i64;
    if !opts.quiet {
        let _ = writeln!(
            i.err,
            "{indent}chain {}: {} of {} steps ok ({ms} ms)",
            chain.name,
            total - failed.len(),
            total
        );
    }
    let mut m = IndexMap::new();
    m.insert("chain".to_string(), Value::str(&chain.name));
    m.insert("ok".to_string(), Value::Bool(failed.is_empty()));
    m.insert("ms".to_string(), Value::Int(ms));
    m.insert("steps".to_string(), Value::list(reports));
    m.insert("failed".to_string(), Value::list(failed));
    m.insert("result".to_string(), prev);
    Ok(Value::map(m))
}

fn step_report(
    label: &str,
    index: usize,
    ok: bool,
    ms: i64,
    result: Option<Value>,
    error: Option<&Diagnostic>,
    retries: i64,
) -> Value {
    let mut m = IndexMap::new();
    m.insert("name".to_string(), Value::str(label));
    m.insert("index".to_string(), Value::Int(index as i64));
    m.insert("ok".to_string(), Value::Bool(ok));
    m.insert("ms".to_string(), Value::Int(ms));
    m.insert("retries".to_string(), Value::Int(retries));
    m.insert("result".to_string(), result.unwrap_or(Value::Null));
    m.insert(
        "error".to_string(),
        error.map(error_map).unwrap_or(Value::Null),
    );
    Value::map(m)
}

fn error_map(e: &Diagnostic) -> Value {
    let mut m = IndexMap::new();
    m.insert("message".to_string(), Value::str(&e.message));
    m.insert("kind".to_string(), Value::str(e.kind.as_str()));
    m.insert(
        "line".to_string(),
        e.line.map(|l| Value::Int(l as i64)).unwrap_or(Value::Null),
    );
    Value::map(m)
}
