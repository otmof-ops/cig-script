// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! `pass`: one authenticated "already checked" (cig-script#186).
//!
//! `pass.issue(kind, subject, pack(s) { ... })` runs the work with a
//! recorder: every file, folder, variable and program the work reads is
//! noted, and the pass vouches for the work's result for exactly those
//! inputs. `pass.find` returns the newest pass for a subject that still
//! verifies; `pass.memo` is find, else issue. A pass travels as a token
//! (`cigpass1.`...) or as the map these functions return.

use super::b;
use crate::burn::Mode;
use crate::diagnostics::{burn, type_error, usage, Diagnostic};
use crate::interp::env::next_serial;
use crate::interp::record::RecFrame;
use crate::interp::Interp;
use crate::pass::{self, CigCtx, IssueOpts, Keys};
use crate::syntax::span::Span;
use crate::value::{expect_str, opt_map, Builtin, Effect, Value};
use cig_pass::{Action, IssueError, Network, Pass};
use indexmap::IndexMap;

pub const TABLE: &[Builtin] = &[
    b("pass.issue", Effect::Read, 3, Some(4), issue),
    b("pass.find", Effect::Read, 2, Some(3), find),
    b("pass.memo", Effect::Read, 3, Some(4), memo),
    b("pass.check", Effect::Read, 1, Some(2), check),
    b("pass.narrow", Effect::Pure, 2, Some(2), narrow),
    b("pass.spend", Effect::Read, 1, Some(2), spend),
    b("pass.show", Effect::Pure, 1, Some(1), show),
    b("pass.recording", Effect::Pure, 0, Some(0), recording),
];

// ----- conversions -----------------------------------------------------------------

fn e(code: &'static str, msg: String, s: Span) -> Diagnostic {
    burn(msg).code(code).at(s)
}

/// A pass from what a script holds: its map (with `token`) or its token.
fn pass_arg(a: &[Value], i: usize, name: &str, s: Span) -> Result<Pass, Diagnostic> {
    let token = match a.get(i) {
        Some(Value::Str(t)) => t.to_string(),
        Some(Value::Map(m)) => match m.borrow().get("token") {
            Some(Value::Str(t)) => t.to_string(),
            _ => {
                return Err(
                    type_error(format!("{name}: that map is not a pass (it has no token)")).at(s),
                )
            }
        },
        Some(other) => {
            return Err(type_error(format!(
                "{name}: takes a pass (its map or its token), not {}",
                other.type_name()
            ))
            .at(s))
        }
        None => return Err(type_error(format!("{name}: takes a pass")).at(s)),
    };
    Pass::from_token(&token).map_err(|d| {
        e("E736", format!("{name}: {d}"), s)
            .with_hint("pass the map pass.issue returned, or its token, unchanged")
    })
}

/// A pass as a script sees it.
fn pass_value(p: &Pass, hit: Option<bool>) -> Value {
    let mut m = IndexMap::new();
    m.insert("id".to_string(), Value::str(cig_pass::hex(&p.id())));
    m.insert("root".to_string(), Value::str(cig_pass::hex(&p.root_id())));
    m.insert("kind".to_string(), Value::str(&p.kind));
    m.insert("subject".to_string(), Value::str(cig_pass::hex(&p.subject)));
    let result = std::str::from_utf8(&p.result)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok())
        .map(super::json::to_value)
        .unwrap_or(Value::Null);
    m.insert("result".to_string(), result);
    m.insert("issued".to_string(), Value::str(cig_pass::iso(p.issued_ms)));
    m.insert(
        "caveats".to_string(),
        Value::list(
            p.caveats
                .iter()
                .map(|c| Value::str(cig_pass::caveat_text(c)))
                .collect(),
        ),
    );
    m.insert("inputs".to_string(), Value::Int(p.inputs.len() as i64));
    m.insert(
        "form".to_string(),
        Value::str(match p.form {
            cig_pass::Form::Local => "local",
            cig_pass::Form::Shareable => "shareable",
        }),
    );
    m.insert("token".to_string(), Value::str(p.token()));
    if let Some(h) = hit {
        m.insert("hit".to_string(), Value::Bool(h));
    }
    Value::map(m)
}

/// The action a caveat is checked against, from a script's map:
/// `{boxed, network, args, agent, roots}`. A field not given is not
/// assumed: a caveat about it does not hold.
fn action_arg(a: &[Value], i: usize, name: &str, s: Span) -> Result<Action, Diagnostic> {
    let mut act = Action::default();
    let Some(m) = opt_map(a, i, name, s)? else {
        return Ok(act);
    };
    let m = m.borrow().clone();
    for (k, v) in m.iter() {
        match (k.as_str(), v) {
            ("boxed", v) => act.boxed = v.truthy(),
            ("network", Value::Str(n)) if n.as_ref() == "none" => act.network = Some(Network::None),
            ("network", Value::List(hosts)) => {
                let hs: Vec<String> = hosts.borrow().iter().map(Value::display).collect();
                act.network = Some(Network::Hosts(hs));
            }
            ("args", Value::List(items)) => {
                act.argv = Some(items.borrow().iter().map(Value::display).collect());
            }
            ("agent", Value::Str(n)) => act.agent = Some(n.to_string()),
            ("roots", Value::List(items)) => {
                for r in items.borrow().iter() {
                    let p = std::path::PathBuf::from(r.display());
                    let abs = if p.is_absolute() {
                        p
                    } else {
                        std::env::current_dir().unwrap_or_default().join(p)
                    };
                    if let Some(b) = cig_pass::path_bytes(&abs) {
                        act.roots.push(b);
                    }
                }
            }
            (other, _) => {
                return Err(type_error(format!(
                    "{name}: the action takes boxed, network (\"none\" or a list of hosts), args (a list), agent and roots, not `{other}`"
                ))
                .at(s))
            }
        }
    }
    Ok(act)
}

/// What `opts` says: `{until, code, cig, tools, rules, store}`.
struct Opts {
    until: Option<i64>,
    code: bool,
    cig: bool,
    tools: Vec<(String, String, String)>,
    rules: Vec<(String, [u8; 32])>,
    store: bool,
}

fn opts_arg(a: &[Value], i: usize, name: &str, s: Span) -> Result<Opts, Diagnostic> {
    let mut o = Opts {
        until: None,
        code: true,
        cig: true,
        tools: Vec::new(),
        rules: Vec::new(),
        store: true,
    };
    let Some(m) = opt_map(a, i, name, s)? else {
        return Ok(o);
    };
    let bad = |what: String| type_error(format!("{name}: {what}")).at(s);
    let m = m.borrow().clone();
    for (k, v) in m.iter() {
        match k.as_str() {
            "until" => {
                let t = match v {
                    Value::Int(ms) => ms.to_owned(),
                    Value::Str(t) => {
                        let now = cig_pass::wall_ms();
                        cig_pass::parse_span(t.as_ref())
                            .map(|d| now.saturating_add(d))
                            .or_else(|_| cig_pass::parse_time(t.as_ref()))
                            .map_err(|w| {
                                bad(format!("`until` takes a span (2h) or a UTC time: {w}"))
                            })?
                    }
                    _ => {
                        return Err(bad(
                            "`until` takes a span (2h), a UTC time or unix ms".to_string()
                        ))
                    }
                };
                o.until = Some(t);
            }
            "code" => o.code = v.truthy(),
            "cig" => o.cig = v.truthy(),
            "store" => o.store = v.truthy(),
            "tools" => {
                let Value::Map(tm) = v else {
                    return Err(bad("`tools` takes a map of name to version".to_string()));
                };
                for (n, ver) in tm.borrow().iter() {
                    let (version, build) = match ver {
                        Value::List(l) if l.borrow().len() == 2 => {
                            let l = l.borrow();
                            (l[0].display(), l[1].display())
                        }
                        other => (other.display(), String::new()),
                    };
                    o.tools.push((n.clone(), version, build));
                }
            }
            "rules" => {
                let Value::Map(rm) = v else {
                    return Err(bad("`rules` takes a map of name to a digest".to_string()));
                };
                for (n, d) in rm.borrow().iter() {
                    let text = d.display();
                    let digest = cig_pass::from_hex32(&text)
                        .unwrap_or_else(|| cig_pass::hash_bytes(text.as_bytes()));
                    o.rules.push((n.clone(), digest));
                }
            }
            other => {
                return Err(bad(format!(
                    "unknown option `{other}` (until, code, cig, tools, rules, store)"
                )))
            }
        }
    }
    Ok(o)
}

fn kind_arg(a: &[Value], name: &str, s: Span) -> Result<String, Diagnostic> {
    let k = expect_str(a, 0, name, s)?;
    match cig_pass::policy::policy(k) {
        Some(p) if p.local => Ok(k.to_string()),
        Some(_) => Err(usage(format!(
            "{name}: a script does not make {k} passes (they come from elsewhere)"
        ))
        .code("E809")
        .at(s)),
        None => Err(usage(format!(
            "{name}: `{k}` is not a kind of pass ({})",
            cig_pass::KINDS.join(", ")
        ))
        .code("E809")
        .at(s)),
    }
}

fn subject_arg(a: &[Value], name: &str, s: Span) -> Result<Vec<u8>, Diagnostic> {
    pass::canonical_json(&a[1]).map_err(|w| type_error(format!("{name}: {w}")).at(s))
}

fn state_err(name: &str, w: String, s: Span) -> Diagnostic {
    let code = if w.starts_with(pass::STATE_E739) {
        "E739"
    } else {
        "E721"
    };
    e(code, format!("{name}: {w}"), s).with_hint("cig doctor says what it found")
}

/// Keys, the pass state and the store, or why not.
fn open(
    name: &str,
    s: Span,
) -> Result<(Keys, cig_pass::marks::Marks, Option<cig_pass::store::Store>), Diagnostic> {
    let keys = Keys::load().map_err(|w| state_err(name, w, s))?;
    let marks = pass::state(&keys).map_err(|w| state_err(name, w, s))?;
    let store = pass::store().ok();
    Ok((keys, marks, store))
}

fn ctx_with<'a>(
    keys: &'a Keys,
    marks: &'a cig_pass::marks::Marks,
    store: Option<&'a cig_pass::store::Store>,
    o: &Opts,
) -> CigCtx<'a> {
    let mut ctx = CigCtx::new(keys, marks, store);
    for (n, v, b) in &o.tools {
        ctx.tools.insert(n.clone(), (v.clone(), b.clone()));
    }
    for (n, d) in &o.rules {
        ctx.rules.insert(n.clone(), *d);
    }
    ctx
}

// ----- the builtins ------------------------------------------------------------------

fn issue(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    run(i, a, s, "pass.issue", false)
}

fn memo(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    run(i, a, s, "pass.memo", true)
}

/// Issue (or, for `memo`, find first): record the work, then issue a pass
/// over what it read.
fn run(i: &mut Interp, a: &[Value], s: Span, name: &str, memo: bool) -> Result<Value, Diagnostic> {
    let kind = kind_arg(a, name, s)?;
    let subject = subject_arg(a, name, s)?;
    let work = a[2].clone();
    if !matches!(work, Value::Func(_)) {
        return Err(type_error(format!(
            "{name}: the work is a pack (pack(s) {{ ... }}), not {}",
            work.type_name()
        ))
        .at(s));
    }
    let o = opts_arg(a, 3, name, s)?;
    let (keys, marks, store) = open(name, s)?;
    if memo {
        let ctx = ctx_with(&keys, &marks, store.as_ref(), &o);
        if let Some((p, _)) = pass::find(&ctx, &kind, &subject) {
            if let Some(outer) = i.recorder() {
                outer.pass(p.id());
            }
            return Ok(pass_value(&p, Some(true)));
        }
    }
    let frame = RecFrame {
        rec: cig_pass::Recorder::new(),
        barrier: next_serial(),
        clock: o.until.is_some(),
    };
    i.recs.push(frame);
    if o.code {
        if let Some(p) = i.script_path.clone() {
            if let Some(r) = i.recorder() {
                if let Err(err) = r.file_seen(&p) {
                    r.open_world(format!(
                        "the script {} could not be read ({err})",
                        p.display()
                    ));
                }
            }
        }
    }
    let out = i.call(&work, vec![a[1].clone()], s);
    let frame = i.recs.pop().expect("the frame pushed above");
    let value = out?;
    if let Some(outer) = i.recorder() {
        outer.absorb(&frame.rec);
    }
    let result = pass::canonical_json(&value)
        .map_err(|w| type_error(format!("{name}: the work's result: {w}")).at(s))?;
    let dry = i.kernel.mode == Mode::DryRun;
    let iopts = IssueOpts {
        until: o.until,
        cig: o.cig,
        tools: o.tools.clone(),
        rules: o.rules.clone(),
        store: o.store && !dry,
    };
    match pass::issue(&keys, &marks, &kind, &subject, frame.rec, result, &iopts) {
        Ok(p) => Ok(pass_value(&p, if memo { Some(false) } else { None })),
        Err(IssueError::OpenWorld(w)) if memo => Ok(unissued(value, &w)),
        Err(IssueError::Changed(w)) if memo => Ok(unissued(value, &format!("{w} changed while the work ran"))),
        Err(IssueError::OpenWorld(w)) => Err(e(
            "E731",
            format!("{name}: {w}: a pass cannot vouch for what it cannot see, so none was issued"),
            s,
        )
        .with_hint("read through fs, env and proc.which inside the work; give a hop `reads: [the paths it reads]`; pass `until` to read the clock")),
        Err(IssueError::Changed(w)) => Err(e(
            "E734",
            format!("{name}: {w} changed while the work ran: two reads, two answers, so no pass was issued"),
            s,
        )
        .with_hint("run it again once nothing is writing there")),
        Err(other) => Err(e("E730", format!("{name}: {other}"), s)),
    }
}

/// A memo whose work could not be vouched for: its result, no pass.
fn unissued(result: Value, why: &str) -> Value {
    let mut m = IndexMap::new();
    m.insert("result".to_string(), result);
    m.insert("hit".to_string(), Value::Bool(false));
    m.insert("issued".to_string(), Value::Bool(false));
    m.insert("why".to_string(), Value::str(why));
    Value::map(m)
}

fn find(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let kind = expect_str(a, 0, "pass.find", s)?.to_string();
    let subject = subject_arg(a, "pass.find", s)?;
    let o = opts_arg(a, 2, "pass.find", s)?;
    let (keys, marks, store) = open("pass.find", s)?;
    let ctx = ctx_with(&keys, &marks, store.as_ref(), &o);
    Ok(match pass::find(&ctx, &kind, &subject) {
        Some((p, _)) => {
            if let Some(r) = i.recorder() {
                r.pass(p.id());
            }
            pass_value(&p, None)
        }
        None => Value::Null,
    })
}

fn check(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let p = pass_arg(a, 0, "pass.check", s)?;
    let action = action_arg(a, 1, "pass.check", s)?;
    let (keys, marks, store) = open("pass.check", s)?;
    let mut ctx = CigCtx::new(&keys, &marks, store.as_ref());
    ctx.action = action;
    let mut m = IndexMap::new();
    match cig_pass::verify(&p, &ctx) {
        Ok(_) => {
            if let Some(r) = i.recorder() {
                r.pass(p.id());
            }
            m.insert("ok".to_string(), Value::Bool(true));
            m.insert("why".to_string(), Value::Null);
        }
        Err(w) => {
            m.insert("ok".to_string(), Value::Bool(false));
            m.insert("why".to_string(), Value::str(w.to_string()));
        }
    }
    Ok(Value::map(m))
}

fn narrow(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let p = pass_arg(a, 0, "pass.narrow", s)?;
    let text = expect_str(a, 1, "pass.narrow", s)?;
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let cwd = std::env::current_dir().unwrap_or_default();
    let c = cig_pass::parse_caveat(text, home.as_deref(), &cwd, cig_pass::wall_ms())
        .map_err(|w| usage(format!("pass.narrow: {w}")).code("E809").at(s))?;
    let n = cig_pass::attenuate(&p, c).map_err(|w| e("E730", format!("pass.narrow: {w}"), s))?;
    Ok(pass_value(&n, None))
}

fn spend(i: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let p = pass_arg(a, 0, "pass.spend", s)?;
    let action = action_arg(a, 1, "pass.spend", s)?;
    if i.kernel.mode == Mode::DryRun {
        // A dry run changes nothing: it checks, and says it would spend.
        let (keys, marks, store) = open("pass.spend", s)?;
        let mut ctx = CigCtx::new(&keys, &marks, store.as_ref());
        ctx.action = action;
        return match cig_pass::verify(&p, &ctx) {
            Ok(_) => Ok(Value::Bool(true)),
            Err(w) => Err(e("E730", format!("pass.spend: {w}"), s)),
        };
    }
    match pass::spend(&p, |ctx| ctx.action = action) {
        Ok(_) => Ok(Value::Bool(true)),
        Err(w) if w.starts_with("spent") => Err(e("E735", format!("pass.spend: {w}"), s)
            .with_hint(
                "a once pass is used once; ask for a new one (cig pass log shows the first use)",
            )),
        Err(w) => Err(e("E730", format!("pass.spend: {w}"), s)),
    }
}

fn show(_: &mut Interp, a: &[Value], s: Span) -> Result<Value, Diagnostic> {
    let p = pass_arg(a, 0, "pass.show", s)?;
    let mut m = match pass_value(&p, None) {
        Value::Map(m) => m.borrow().clone(),
        _ => unreachable!("a pass is a map"),
    };
    let inputs: Vec<Value> = p.inputs.iter().map(input_value).collect();
    m.insert("inputs".to_string(), Value::list(inputs));
    m.insert(
        "prefix_ids".to_string(),
        Value::list(
            p.prefix_ids()
                .iter()
                .map(|id| Value::str(cig_pass::hex(id)))
                .collect(),
        ),
    );
    Ok(Value::map(m))
}

/// One input as a script sees it: its kind and what it names (never a
/// digest of a value).
pub(crate) fn input_value(input: &cig_pass::Input) -> Value {
    use cig_pass::Input;
    let mut m = IndexMap::new();
    let (kind, what) = match input {
        Input::File { path, .. } => ("file", cig_pass::shown(path)),
        Input::Absent { path } => ("absent", cig_pass::shown(path)),
        Input::Dir { path, .. } => ("dir", cig_pass::shown(path)),
        Input::Present { path, .. } => ("present", cig_pass::shown(path)),
        Input::Env { name, .. } => ("env", cig_pass::clean(&String::from_utf8_lossy(name))),
        Input::Tool {
            name,
            version,
            build,
        } => (
            "tool",
            cig_pass::clean(&format!("{name} {version} {build}")),
        ),
        Input::Rules { name, .. } => ("rules", cig_pass::clean(name)),
        Input::Pass { id } => ("pass", cig_pass::hex(id)),
        Input::Clock { not_after_ms } => ("clock", cig_pass::iso(*not_after_ms)),
    };
    m.insert("kind".to_string(), Value::str(kind));
    m.insert("name".to_string(), Value::str(what));
    Value::map(m)
}

fn recording(i: &mut Interp, _: &[Value], _: Span) -> Result<Value, Diagnostic> {
    Ok(Value::Bool(i.recording()))
}
