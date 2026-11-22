// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
//! CIR (cig-compile#3): lowering, the verifier, the passes, the text form.

use cigscript::lower::cir::ir::{Class, Const, Kind, VarId};
use cigscript::lower::cir::pass::{self, broken, run_passes, Pass};
use cigscript::lower::cir::verify::{errors, verify, Severity};
use cigscript::lower::cir::{lower_source, print, LowerError};

const DEMO: &str = "pull greet(name) {
  fs.write_text(\"out.txt\", \"hi \" + name)
}
roll n = 0
burn (\"demo\") {
  greet(\"a\")
  n += 1
}
exhale n
";

#[test]
fn the_demo_lowers_and_verifies_and_reads_as_the_spec_shows() {
    let mut m = lower_source(DEMO, "demo.cig").unwrap();
    let found = verify(&m);
    assert!(errors(&found).is_empty(), "{found:#?}");
    run_passes(&mut m, &pass::default_passes()).unwrap();
    let text = print::print(&m);
    for want in [
        "cir 1 \"demo.cig\"",
        "pull requires_burn",
        "burn.op write fs.write_text(",
        "burn.begin r1 normal label=",
        "call.user @1(",
        "} burn.end",
        "exhale %",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
    // Every line of an instruction carries its span.
    assert!(
        text.lines()
            .filter(|l| l.contains(" = ") || l.contains("burn.op"))
            .all(|l| l.contains(" @")),
        "{text}"
    );
}

/// The text form, byte for byte: tests/lower/cir/demo.cir. After a deliberate
/// change, CIR_UPDATE_GOLDEN=1 writes it again.
#[test]
fn the_demo_matches_its_golden_text() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/lower/cir");
    // A checkout that ignored .gitattributes still compares by lines.
    let lf = |t: String| t.replace("\r\n", "\n");
    let src = lf(std::fs::read_to_string(dir.join("demo.cig")).unwrap());
    let (text, _) = cigscript::lower::cir::emit(&src, "demo.cig").unwrap();
    let golden = dir.join("demo.cir");
    if std::env::var_os("CIR_UPDATE_GOLDEN").is_some() {
        std::fs::write(&golden, &text).unwrap();
    }
    assert_eq!(
        text,
        lf(std::fs::read_to_string(&golden).unwrap_or_default()),
        "CIR_UPDATE_GOLDEN=1 writes it again"
    );
}

#[test]
fn an_effect_hoisted_out_of_its_burn_is_caught_v4() {
    let mut m = lower_source(
        DEMO.replace("greet(\"a\")", "fs.write_text(\"x\", \"y\")")
            .as_str(),
        "demo.cig",
    )
    .unwrap();
    let e = run_passes(&mut m, &[&broken::HoistBurnOp as &dyn Pass]).unwrap_err();
    assert_eq!(e.pass, "hoist-burn-op");
    let v4: Vec<_> = e.errors.iter().filter(|x| x.code == "V4").collect();
    assert_eq!(v4.len(), 1, "{:#?}", e.errors);
    assert!(v4[0].message.contains("fs.write_text"), "{}", v4[0]);
    assert_eq!(
        v4[0].span.line, 6,
        "the error points at the effect's own line"
    );
}

#[test]
fn effects_swapped_in_their_region_are_caught_only_by_the_transition_check_t2() {
    let src = "burn {\n  fs.write_text(\"a\", \"1\")\n  fs.write_text(\"b\", \"2\")\n}\n";
    let mut m = lower_source(src, "s.cig").unwrap();
    let e =
        run_passes(&mut m, &[&broken::SwapBurnOps as &dyn Pass]).expect_err("the swap is caught");
    assert!(e.errors.iter().any(|x| x.code == "T2"), "{:#?}", e.errors);
    assert!(
        !e.errors.iter().any(|x| x.code == "V4"),
        "a swap is structurally fine: {:#?}",
        e.errors
    );
}

#[test]
fn an_effect_dropped_by_a_pass_is_caught_t3() {
    let src = "burn {\n  fs.mkdir(\"d\")\n}\n";
    let mut m = lower_source(src, "s.cig").unwrap();
    let e = run_passes(&mut m, &[&broken::DropBurnOp as &dyn Pass]).unwrap_err();
    assert!(e.errors.iter().any(|x| x.code == "T3"), "{:#?}", e.errors);
}

#[test]
fn constants_fold_where_the_interpreter_cannot_fail_and_nowhere_else() {
    let src =
        "stick a = 2 + 3 * 4\nstick b = 1 / 0\nstick c = 9223372036854775807 + 1\nexhale a, b, c\n";
    let mut m = lower_source(src, "f.cig").unwrap();
    run_passes(&mut m, &pass::default_passes()).unwrap();
    let text = print::print(&m);
    assert!(text.contains("const 14"), "{text}");
    assert!(text.contains("bin / "), "division is never folded: {text}");
    assert!(
        text.contains("bin + "),
        "an overflowing add is left to the runtime (E503): {text}"
    );
}

#[test]
fn closures_capture_and_effects_in_lambdas_are_theirs() {
    let src = "roll total = 0\nstick add = pack(x) => total + x\nburn {\n  [1, 2].each(pack(p) => fs.write_text(\"f${p}\", \"x\"))\n}\nexhale add(1)\n";
    let mut m = lower_source(src, "c.cig").unwrap();
    let found = run_passes(&mut m, &pass::default_passes()).unwrap();
    assert!(errors(&found).is_empty());
    let text = print::print(&m);
    assert!(text.contains("captures($0:total!)"), "{text}");
    // The lambda's effect is legal only when it runs in a burn: the lambda
    // requires one.
    assert!(text.contains("lambda requires_burn"), "{text}");
}

#[test]
fn an_effectful_pull_called_outside_a_burn_is_a_warning_not_an_error() {
    let src = "pull w() {\n  fs.mkdir(\"d\")\n}\nw()\n";
    let mut m = lower_source(src, "w.cig").unwrap();
    let found = run_passes(&mut m, &pass::default_passes()).unwrap();
    assert!(
        found
            .iter()
            .any(|w| w.code == "V6" && w.severity == Severity::Warn),
        "{found:#?}"
    );
}

#[test]
fn a_script_that_does_not_check_is_not_lowered() {
    assert!(matches!(
        lower_source("exhale nope\n", "x.cig"),
        Err(LowerError::Diagnostic(_))
    ));
    assert!(matches!(
        lower_source("fs.rm(\"x\")\n", "x.cig"),
        Err(LowerError::Diagnostic(_))
    ));
}

#[test]
fn an_effect_moved_out_of_its_if_is_caught_only_by_the_places_t1() {
    let src = "roll c = true\nburn {\n  if c {\n    fs.rm(\"a\")\n  }\n}\n";
    let mut m = lower_source(src, "i.cig").unwrap();
    let e =
        run_passes(&mut m, &[&broken::HoistOutOfIf as &dyn Pass]).expect_err("the move is caught");
    assert_eq!(e.pass, "hoist-out-of-if");
    assert!(!e.errors.is_empty());
    // The same order, the same region, every value defined before use:
    // only the effect's place says it is no longer conditional.
    assert!(e.errors.iter().all(|x| x.code == "T1"), "{:#?}", e.errors);
    assert!(
        e.errors.iter().any(|x| x.message.contains("if.then")),
        "{:#?}",
        e.errors
    );
}

#[test]
fn a_burn_keeps_its_unburn_blocks_span() {
    // The line the journal records for a compensation, which a back end
    // writes at run time.
    let src = "burn (s) {\n  s.n = 1\n  fs.mkdir(\"d\")\n}\n\nunburn {\n  exhale s.n\n}\nburn {\n  fs.mkdir(\"e\")\n}\n";
    let m = lower_source(src, "u.cig").unwrap();
    let at: Vec<Option<(u32, u32)>> = m.funcs[0]
        .body
        .instrs
        .iter()
        .filter_map(|i| match &i.kind {
            Kind::Burn(b) => Some(b.unburn_at.map(|s| (s.line, s.col))),
            _ => None,
        })
        .collect();
    assert_eq!(at.len(), 2);
    assert_eq!(at[0].map(|a| a.0), Some(6));
    assert_eq!(at[1], None);
}

#[test]
fn a_burn_keeps_its_labels_span() {
    // Where the tree-walker raises a label that is not a string, which a
    // back end that reads the label at run time needs.
    let src = "stick n = \"x\"\nburn (s,   n + \"y\") {\n  fs.mkdir(\"d\")\n}\nburn {\n  fs.mkdir(\"e\")\n}\n";
    let m = lower_source(src, "l.cig").unwrap();
    let at: Vec<Option<(u32, u32)>> = m.funcs[0]
        .body
        .instrs
        .iter()
        .filter_map(|i| match &i.kind {
            Kind::Burn(b) => Some(b.label_at.map(|s| (s.line, s.col))),
            _ => None,
        })
        .collect();
    assert_eq!(at, vec![Some((2, 12)), None]);
}

#[test]
fn a_compensation_that_reaches_outside_its_state_is_caught_v8() {
    let src = "roll n = 1\nburn (s) {\n  s.n = n\n  fs.mkdir(\"d\")\n} unburn {\n  exhale s.n\n}\n";
    let mut m = lower_source(src, "u.cig").unwrap();
    let found = verify(&m);
    assert!(errors(&found).is_empty(), "{found:#?}");
    // Read `n` in the compensation instead of the state: the checker's
    // E754, which a pass must not bring back.
    let n = VarId(m.vars.iter().position(|v| v.name == "n").unwrap() as u32);
    let comp = m.funcs[0]
        .body
        .instrs
        .iter_mut()
        .find_map(|i| match &mut i.kind {
            Kind::Burn(b) => b.compensate.as_mut(),
            _ => None,
        })
        .unwrap();
    let load = comp
        .instrs
        .iter_mut()
        .find(|i| matches!(i.kind, Kind::Load(_)))
        .unwrap();
    load.kind = Kind::Load(n);
    let found = verify(&m);
    assert!(
        found
            .iter()
            .any(|e| e.code == "V8" && e.message.contains(":n, which")),
        "{found:#?}"
    );
}

#[test]
fn a_capture_the_flags_do_not_record_is_caught_v9() {
    let src = "roll total = 0\nstick add = pack(x) => total + x\nexhale add(1)\n";
    let mut m = lower_source(src, "c.cig").unwrap();
    let total = m.vars.iter().position(|v| v.name == "total").unwrap();
    assert!(m.vars[total].captured);
    m.vars[total].captured = false;
    let found = verify(&m);
    assert!(
        found
            .iter()
            .any(|e| e.code == "V9" && e.message.contains("total")),
        "{found:#?}"
    );
}

#[test]
fn a_safe_member_is_null_for_a_missing_key_too() {
    // eval.rs member(): `?.` gives null for a null subject and for a map
    // without the key. One instruction carries both; a short.safe around
    // it would leave the second to a back end to forget.
    let src = "roll cfg = {a: 1}\nexhale cfg?.missing\n";
    let mut m = lower_source(src, "s.cig").unwrap();
    run_passes(&mut m, &pass::default_passes()).unwrap();
    let text = print::print(&m);
    assert!(text.contains("?.missing"), "{text}");
    assert!(!text.contains("short.safe"), "{text}");
}

#[test]
fn a_bare_name_piped_into_resolves_as_a_call_would() {
    let src = "pull dbl(x) {\n  snuff x * 2\n}\nburn {\n  \"x\" >> fs.rm\n}\nexhale 3 >> dbl\n";
    let mut m = lower_source(src, "p.cig").unwrap();
    run_passes(&mut m, &pass::default_passes()).unwrap();
    let text = print::print(&m);
    assert!(
        text.contains("burn.op rm fs.rm("),
        "the effect stays an effect: {text}"
    );
    assert!(text.contains("call.user @1("), "{text}");
    assert!(!text.contains("call.value"), "{text}");
}

/// Golden: `copy-prop` alone over `stick a = 1; stick b = a; exhale b`
/// turns both loads into direct uses of the first constant.
#[test]
fn copy_prop_matches_its_golden_text() {
    golden_for_pass("copy-prop", "stick-chain", &[&pass::CopyProp as &dyn Pass]);
}

#[test]
fn copy_prop_propagates_through_chains_and_leaves_mutables_alone() {
    // A chain of immutable copies collapses to the first value.
    let src = "stick a = 1\nstick b = a\nexhale b\n";
    let mut m = lower_source(src, "cp.cig").unwrap();
    run_passes(&mut m, &[&pass::CopyProp as &dyn Pass]).unwrap();
    let text = print::print(&m);
    assert!(!text.contains("load "), "copies removed: {text}");
    assert_eq!(
        text.matches("const 1").count(),
        1,
        "and only the one constant is the source: {text}"
    );
    // A mutable slot is never propagated through: between a declaration
    // and a use another store may land, and the load would read it.
    let src = "roll a = 1\na = 2\nexhale a\n";
    let mut m = lower_source(src, "cp.cig").unwrap();
    run_passes(&mut m, &[&pass::CopyProp as &dyn Pass]).unwrap();
    let text = print::print(&m);
    assert!(text.contains("load $0:a!"), "mutable load kept: {text}");
}

/// Negative: a value `copy-prop` propagates must be visible where it
/// lands. A broken twin that lifts a value out of an `if.then` into the
/// enclosing block is caught by V1 (`%r used where it is not defined`).
#[test]
fn a_copy_propagation_out_of_scope_is_caught_v1() {
    let src = "stick a = 1\nif true {\n  stick b = 2\n  exhale b\n}\nexhale a\n";
    let mut m = lower_source(src, "neg.cig").unwrap();
    let found = verify(&m);
    assert!(errors(&found).is_empty(), "the input itself verifies");
    let e = run_passes(&mut m, &[&broken::PropOutOfNestedScope as &dyn Pass])
        .expect_err("the propagation is caught");
    assert_eq!(e.pass, "prop-out-of-nested-scope");
    assert!(e.errors.iter().any(|x| x.code == "V1"), "{:#?}", e.errors);
}

/// Golden: `sccp` alone over `roll n; if true { n = 1 } else { n = 2 }`
/// empties the dead `else` branch; the `if` and its cond stay.
#[test]
fn sccp_matches_its_golden_text() {
    golden_for_pass("sccp", "if-true", &[&pass::Sccp as &dyn Pass]);
}

#[test]
fn sccp_empties_only_the_dead_side_and_only_when_it_carries_no_effect() {
    // `if true` with a dead `else` that has no effect: emptied.
    let src = "roll n = 0\nif true {\n  n = 1\n} else {\n  n = 2\n}\nexhale n\n";
    let mut m = lower_source(src, "s.cig").unwrap();
    let before = print::print(&m);
    run_passes(&mut m, &[&pass::Sccp as &dyn Pass]).unwrap();
    let after = print::print(&m);
    assert_ne!(before, after, "the pass ran");
    assert!(
        after.contains("const true") && after.contains("if %"),
        "the cond and its branch stay: {after}"
    );
    // The `else` is now empty, so no `const 2` from `n = 2` survives.
    assert!(
        !after.contains("const 2"),
        "the dead side is empty: {after}"
    );
    // `if false` with a dead `then` that has a `burn.op` is left alone:
    // emptying it would drop an effect, and T3 would catch that.
    let src = "burn {\n  if false {\n    fs.rm(\"x\")\n  }\n}\n";
    let mut m = lower_source(src, "s.cig").unwrap();
    run_passes(&mut m, &[&pass::Sccp as &dyn Pass]).unwrap();
    let text = print::print(&m);
    assert!(
        text.contains("burn.op rm fs.rm("),
        "the dead effect is kept: {text}"
    );
}

/// Negative: `sccp` never drops a branch whose body has a `burn.op`. A
/// broken twin that clears an `if.then` holding one is caught by T3.
#[test]
fn an_sccp_that_drops_a_branch_with_an_effect_is_caught_t3() {
    let src = "burn {\n  if true {\n    fs.rm(\"gone\")\n  }\n}\n";
    let mut m = lower_source(src, "neg.cig").unwrap();
    let found = verify(&m);
    assert!(errors(&found).is_empty(), "the input itself verifies");
    let e = run_passes(&mut m, &[&broken::DropBranchWithEffect as &dyn Pass])
        .expect_err("the drop is caught");
    assert_eq!(e.pass, "drop-branch-with-effect");
    assert!(e.errors.iter().any(|x| x.code == "T3"), "{:#?}", e.errors);
}

/// `-O1` runs `fold-consts, copy-prop, fold-consts, sccp, dead-pure`, so
/// `stick a = 1; if a == 1 { exhale 1 } else { exhale 2 }` has its
/// `else` emptied and its compare gone.
#[test]
fn default_passes_fold_copy_through_sccp() {
    let src = "stick a = 1\nif a == 1 {\n  exhale 1\n} else {\n  exhale 2\n}\n";
    let mut m = lower_source(src, "o1.cig").unwrap();
    run_passes(&mut m, &pass::default_passes()).unwrap();
    let text = print::print(&m);
    assert!(
        text.contains("const true"),
        "fold-consts after copy-prop sees the compare: {text}"
    );
    assert!(!text.contains("bin == "), "the compare is gone: {text}");
    assert!(!text.contains("const 2"), "the else is empty: {text}");
    assert!(
        text.contains("const 1"),
        "the then's `exhale 1` stays: {text}"
    );
}

/// Lower, run `passes` one at a time (each between the verifier's
/// checks), print.
fn emit_passes(src: &str, file: &str, passes: &[&'static dyn Pass]) -> String {
    let mut m = lower_source(src, file).unwrap();
    run_passes(&mut m, &[]).unwrap();
    for p in passes {
        run_passes(&mut m, &[*p]).unwrap();
    }
    print::print(&m)
}

/// The CIR text each pass prints matches its golden file; after a
/// deliberate change, `CIR_UPDATE_GOLDEN=1` writes it again.
fn golden_for_pass(sub: &str, name: &str, passes: &[&'static dyn Pass]) {
    let lf = |t: String| t.replace("\r\n", "\n");
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/lower/cir")
        .join(sub);
    let cig = dir.join(format!("{name}.cig"));
    let src =
        lf(std::fs::read_to_string(&cig).unwrap_or_else(|e| panic!("{}: {e}", cig.display())));
    let file = format!("{name}.cig");
    let text = emit_passes(&src, &file, passes);
    let golden = dir.join(format!("{name}.cir"));
    if std::env::var_os("CIR_UPDATE_GOLDEN").is_some() {
        std::fs::write(&golden, &text).unwrap();
    }
    assert_eq!(
        text,
        lf(std::fs::read_to_string(&golden).unwrap_or_default()),
        "CIR_UPDATE_GOLDEN=1 writes it again"
    );
}

/// Every script in a CigScript checkout of the pinned version lowers and
/// verifies through the default passes; the corpus scripts the checker
/// refuses are refused here too.
#[test]
fn every_script_cigscript_carries_lowers_and_verifies() {
    let Some(src) = std::env::var_os("CIGSCRIPT_SRC").map(std::path::PathBuf::from) else {
        eprintln!("CIGSCRIPT_SRC is not set; skipped");
        return;
    };
    let mut lowered = 0;
    let mut refused = Vec::new();
    for dir in ["examples", "tests/scripts", "tests/corpus"] {
        let mut files: Vec<_> = std::fs::read_dir(src.join(dir))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "cig"))
            .collect();
        files.sort();
        for f in files {
            let text = std::fs::read_to_string(&f).unwrap();
            let name = format!("{dir}/{}", f.file_name().unwrap().to_string_lossy());
            match lower_source(&text, &name) {
                Ok(mut m) => {
                    if let Err(e) = run_passes(&mut m, &pass::default_passes()) {
                        panic!("{name}: {} broke a rule:\n{:#?}", e.pass, e.errors);
                    }
                    lowered += 1;
                }
                Err(LowerError::Diagnostic(_)) => refused.push(name),
                Err(LowerError::Internal(e)) => panic!("{name}: {e}"),
            }
        }
    }
    refused.sort();
    assert_eq!(
        refused,
        [
            "tests/corpus/e104-too-big.cig",
            "tests/corpus/e202-unclosed.cig",
            "tests/corpus/e301-typo.cig",
            "tests/corpus/e303-outside-burn.cig",
        ],
        "exactly the corpus scripts the checker refuses"
    );
    assert!(lowered >= 24, "only {lowered} scripts lowered");
}

/// A script that runs every tick site the interpreter has: statements at
/// the top level, in pulls, in a block lambda, in `if` and `else if` arms,
/// in `try`, `ashtray` and `finally`; `while` with `continue` and
/// `break`; `for` over a range, a list and an empty list; a recursion; an
/// expression lambda, which counts nothing of its own.
const STEPPY: &str = "pull fib(n) {
  if n < 2 {
    snuff n
  }
  snuff fib(n - 1) + fib(n - 2)
}
roll total = 0
roll i = 0
while i < 5 {
  i += 1
  if i == 2 {
    continue
  } else if i == 4 {
    break
  }
  total += i
}
for j in 0..3 {
  pull twice(x) {
    snuff x * 2
  }
  for k in [1, 2] {
    total += twice(j * k)
  }
  try {
    if j == 1 {
      cough \"one\"
    }
    total += 1
  } ashtray e {
    total += 10
  } finally {
    total += 100
  }
}
for e in [] {
  total += 1
}
stick sq = pack(x) => x * x
stick add = pack(a, b) {
  stick s = a + b
  snuff s
}
total += sq(3) + add(1, 2) + fib(5)
exhale total
";

/// The step marks a run passes through, in order, from a small evaluator
/// over CIR: enough of the language for `STEPPY`, and nothing else. With a
/// budget, a mark past it raises E515 at its own span as the interpreter's
/// `tick` does: an ordinary error, which `ashtray` catches and a `finally`
/// that raises again replaces.
mod marks {
    use cigscript::lower::cir::ir::*;
    use cigscript::syntax::ast::{BinOp, UnOp};
    use cigscript::syntax::span::Span;
    use std::collections::HashMap;

    #[derive(Clone, Debug, PartialEq)]
    enum V {
        Null,
        Bool(bool),
        Int(i64),
        Str(String),
        List(Vec<V>),
        Map(Vec<(String, V)>),
        Func(FuncId),
    }

    impl V {
        fn show(&self) -> String {
            match self {
                V::Null => "null".into(),
                V::Bool(b) => b.to_string(),
                V::Int(i) => i.to_string(),
                V::Str(s) => s.clone(),
                other => panic!("{other:?} is not shown in this evaluator"),
            }
        }
    }

    impl V {
        fn truthy(&self) -> bool {
            !matches!(self, V::Null | V::Bool(false))
        }
        fn int(&self) -> i64 {
            match self {
                V::Int(i) => *i,
                other => panic!("not an int: {other:?}"),
            }
        }
    }

    enum Flow {
        Next,
        Break,
        Continue,
        Ret(V),
        Yield(V),
        Throw,
    }

    struct Frame {
        func: FuncId,
        vals: HashMap<ValueId, V>,
        slots: HashMap<VarId, V>,
    }

    pub struct Run<'m> {
        m: &'m Module,
        frames: Vec<Frame>,
        marks: Vec<Span>,
        out: Vec<String>,
        max: u64,
        /// The error in flight: where it was raised.
        err: Option<Span>,
    }

    /// Every mark a run of the module passes through, in order, and where
    /// the error that ends it was raised, if one does. `max` is the step
    /// budget, 0 for none.
    pub fn trace(m: &Module, max: u64) -> (Vec<Span>, Option<Span>) {
        let (marks, _, stop) = run(m, max);
        (marks, stop)
    }

    /// `trace`, and the lines the run's `exhale`s wrote.
    pub fn run(m: &Module, max: u64) -> (Vec<Span>, Vec<String>, Option<Span>) {
        let mut r = Run {
            m,
            frames: Vec::new(),
            marks: Vec::new(),
            out: Vec::new(),
            max,
            err: None,
        };
        let stop = r
            .call(FuncId(0), Vec::new())
            .is_none()
            .then(|| r.err.expect("raised"));
        (r.marks, r.out, stop)
    }

    impl Run<'_> {
        fn frame(&mut self) -> &mut Frame {
            self.frames.last_mut().unwrap()
        }

        /// A slot lives in the newest frame of the function that owns it.
        fn slot(&mut self, v: VarId) -> &mut V {
            let owner = self.m.vars[v.0 as usize].owner;
            let f = self
                .frames
                .iter_mut()
                .rev()
                .find(|f| f.func == owner)
                .expect("the owner is running");
            f.slots.entry(v).or_insert(V::Null)
        }

        fn val(&mut self, v: ValueId) -> V {
            self.frame().vals[&v].clone()
        }

        /// `None`: an error escaped the call.
        fn call(&mut self, func: FuncId, args: Vec<V>) -> Option<V> {
            let f = &self.m.funcs[func.0 as usize];
            let mut slots = HashMap::new();
            for (p, a) in f.params.iter().zip(args) {
                slots.insert(*p, a);
            }
            self.frames.push(Frame {
                func,
                vals: HashMap::new(),
                slots,
            });
            let out = match self.block(&f.body) {
                Flow::Ret(v) => Some(v),
                Flow::Next => Some(V::Null),
                Flow::Throw => None,
                _ => panic!("a stray break, continue or yield"),
            };
            self.frames.pop();
            out
        }

        fn block(&mut self, b: &Block) -> Flow {
            for i in &b.instrs {
                match self.instr(i) {
                    Flow::Next => {}
                    other => return other,
                }
            }
            Flow::Next
        }

        fn raise(&mut self, at: Span) -> Flow {
            self.err = Some(at);
            Flow::Throw
        }

        fn set(&mut self, i: &Instr, v: V) {
            if let Some(r) = i.result {
                self.frame().vals.insert(r, v);
            }
        }

        /// A call's result, or the error that escaped it, raised on here.
        fn called(&mut self, i: &Instr, v: Option<V>) -> Flow {
            match v {
                Some(v) => {
                    self.set(i, v);
                    Flow::Next
                }
                None => Flow::Throw,
            }
        }

        fn instr(&mut self, i: &Instr) -> Flow {
            match &i.kind {
                Kind::Step(_) => {
                    self.marks.push(i.span);
                    if self.max != 0 && self.marks.len() as u64 > self.max {
                        self.err = Some(i.span);
                        return Flow::Throw;
                    }
                }
                Kind::Const(c) => {
                    let v = match c {
                        Const::Null => V::Null,
                        Const::Bool(b) => V::Bool(*b),
                        Const::Int(n) => V::Int(*n),
                        Const::Str(s) => V::Str(s.clone()),
                        Const::Float(_) => panic!("no floats here"),
                    };
                    self.set(i, v);
                }
                Kind::Load(v) => {
                    let x = self.slot(*v).clone();
                    self.set(i, x);
                }
                Kind::Decl(v, x) | Kind::Store(v, x) => {
                    let x = self.val(*x);
                    *self.slot(*v) = x;
                }
                Kind::Unary(op, x) => {
                    let x = self.val(*x);
                    let v = match op {
                        UnOp::Neg => V::Int(-x.int()),
                        UnOp::Not => V::Bool(!x.truthy()),
                    };
                    self.set(i, v);
                }
                Kind::Binary(op, a, b) => {
                    let (a, b) = (self.val(*a), self.val(*b));
                    let v = match op {
                        BinOp::Add => V::Int(a.int() + b.int()),
                        BinOp::Sub => V::Int(a.int() - b.int()),
                        BinOp::Mul => V::Int(a.int() * b.int()),
                        BinOp::Div if b.int() == 0 => return self.raise(i.span),
                        BinOp::Div => V::Int(a.int() / b.int()),
                        BinOp::Eq => V::Bool(a == b),
                        BinOp::Ne => V::Bool(a != b),
                        BinOp::Lt => V::Bool(a.int() < b.int()),
                        BinOp::Le => V::Bool(a.int() <= b.int()),
                        BinOp::Gt => V::Bool(a.int() > b.int()),
                        BinOp::Ge => V::Bool(a.int() >= b.int()),
                        BinOp::Range => V::List((a.int()..b.int()).map(V::Int).collect()),
                        other => panic!("{other:?} is not in this evaluator"),
                    };
                    self.set(i, v);
                }
                Kind::MakeList(xs) => {
                    let v = xs.iter().map(|x| self.val(*x)).collect();
                    self.set(i, V::List(v));
                }
                Kind::Closure(f) => self.set(i, V::Func(*f)),
                // The tree-walker's checks and the positions it raises at.
                Kind::Key(k) => match self.val(*k) {
                    V::Str(s) => self.set(i, V::Str(s)),
                    V::Int(n) => self.set(i, V::Str(n.to_string())),
                    _ => return self.raise(i.span),
                },
                Kind::MakeMap(kv) => {
                    let mut map = Vec::new();
                    for (k, x) in kv {
                        let k = match k {
                            MapKey::Static(s) => s.clone(),
                            MapKey::Dyn(d, _) => self.val(*d).show(),
                        };
                        map.push((k, self.val(*x)));
                    }
                    self.set(i, V::Map(map));
                }
                Kind::Member { obj, name, .. } => {
                    let V::Map(map) = self.val(*obj) else {
                        panic!("not a map")
                    };
                    let v = map
                        .iter()
                        .find(|(k, _)| k == name)
                        .expect("the key")
                        .1
                        .clone();
                    self.set(i, v);
                }
                Kind::Index { obj, idx, idx_at } => {
                    let v = match (self.val(*obj), self.val(*idx)) {
                        (V::List(xs), V::Int(n)) => xs[n as usize].clone(),
                        (V::Map(map), V::Str(k)) => map
                            .iter()
                            .find(|(x, _)| *x == k)
                            .expect("the key")
                            .1
                            .clone(),
                        (V::List(_) | V::Map(_) | V::Str(_), _) => return self.raise(*idx_at),
                        _ => return self.raise(i.span),
                    };
                    self.set(i, v);
                }
                Kind::SetIndex {
                    obj,
                    idx,
                    obj_at,
                    idx_at,
                    ..
                } => match (self.val(*obj), self.val(*idx)) {
                    (V::List(_), V::Int(_)) | (V::Map(_), V::Str(_)) => {}
                    (V::List(_) | V::Map(_), _) => return self.raise(*idx_at),
                    _ => return self.raise(*obj_at),
                },
                Kind::SetMember { obj, obj_at, .. } => {
                    if !matches!(self.val(*obj), V::Map(_)) {
                        return self.raise(*obj_at);
                    }
                }
                Kind::CallUser { func, args } => {
                    let args = args.iter().map(|a| self.val(*a)).collect();
                    let v = self.call(*func, args);
                    return self.called(i, v);
                }
                Kind::CallValue { f, args } => {
                    let V::Func(func) = self.val(*f) else {
                        panic!("not a function")
                    };
                    let args = args.iter().map(|a| self.val(*a)).collect();
                    let v = self.call(func, args);
                    return self.called(i, v);
                }
                Kind::Exhale(vs) => {
                    let line: Vec<String> = vs.iter().map(|v| self.val(*v).show()).collect();
                    self.out.push(line.join(" "));
                }
                Kind::If {
                    cond,
                    then,
                    otherwise,
                } => {
                    if self.val(*cond).truthy() {
                        return self.block(then);
                    } else if let Some(o) = otherwise {
                        return self.block(o);
                    }
                }
                Kind::Loop { cond, body } => loop {
                    match self.block(cond) {
                        Flow::Yield(v) if v.truthy() => {}
                        Flow::Yield(_) => break,
                        Flow::Throw => return Flow::Throw,
                        _ => panic!("a condition ends in yield"),
                    }
                    match self.block(body) {
                        Flow::Next | Flow::Continue => {}
                        Flow::Break => break,
                        other => return other,
                    }
                },
                Kind::ForEach { iter, var, body } => {
                    let V::List(items) = self.val(*iter) else {
                        panic!("not a list")
                    };
                    for item in items {
                        *self.slot(*var) = item;
                        match self.block(body) {
                            Flow::Next | Flow::Continue => {}
                            Flow::Break => break,
                            other => return other,
                        }
                    }
                }
                Kind::Try {
                    body,
                    catch,
                    finally,
                } => {
                    let mut flow = self.block(body);
                    if let (Flow::Throw, Some((var, handler))) = (&flow, catch) {
                        if let Some(v) = var {
                            *self.slot(*v) = V::Null;
                        }
                        flow = self.block(handler);
                    }
                    if let Some(f) = finally {
                        // Its own signal wins; otherwise the pending one
                        // carries on.
                        let pending = self.err;
                        match self.block(f) {
                            Flow::Next => self.err = pending,
                            other => return other,
                        }
                    }
                    return flow;
                }
                Kind::Yield(v) => return Flow::Yield(self.val(*v)),
                Kind::Ret(v) => return Flow::Ret(self.val(*v)),
                Kind::Break => return Flow::Break,
                Kind::Continue => return Flow::Continue,
                Kind::Throw(_) => {
                    self.err = Some(i.span);
                    return Flow::Throw;
                }
                _ => panic!("instruction #{} is not in this evaluator", i.id.0),
            }
            Flow::Next
        }
    }
}

/// What the tree-walker writes for a script, and where an error stops it.
fn walker_run(src: &str) -> (Vec<String>, Option<(u32, u32)>) {
    use cigscript::burn::{Kernel, Mode};
    use cigscript::interp::Interp;
    use std::cell::RefCell;
    use std::rc::Rc;
    #[derive(Clone, Default)]
    struct Sink(Rc<RefCell<Vec<u8>>>);
    impl std::io::Write for Sink {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let program = cigscript::syntax::parse(src).expect("parses");
    let sink = Sink::default();
    let mut interp = Interp::new(Kernel::ephemeral(Mode::Run));
    interp.out = Box::new(sink.clone());
    interp.set_max_steps(0);
    let stop = interp
        .run_program(&program)
        .err()
        .map(|d| (d.line.unwrap_or(0), d.col.unwrap_or(0)));
    let text = String::from_utf8(sink.0.borrow().clone()).unwrap();
    (text.lines().map(str::to_string).collect(), stop)
}

/// The tree-walker's own count for a script, and where it stops when the
/// budget is `max` (0: no budget).
fn walker_steps(src: &str, max: u64) -> (u64, Option<(u32, u32, String)>) {
    use cigscript::burn::{Kernel, Mode};
    use cigscript::interp::Interp;
    let program = cigscript::syntax::parse(src).expect("parses");
    let mut interp = Interp::new(Kernel::ephemeral(Mode::Run));
    interp.out = Box::new(std::io::sink());
    interp.set_max_steps(max);
    let stop = interp
        .run_program(&program)
        .err()
        .map(|d| (d.line.unwrap_or(0), d.col.unwrap_or(0), d.code.to_string()));
    (interp.steps, stop)
}

#[test]
fn the_marks_a_run_passes_are_the_steps_the_tree_walker_counts() {
    let (total, stop) = walker_steps(STEPPY, 0);
    assert_eq!(stop, None, "the script runs to the end");
    for passes in [pass::level(0), pass::level(1)] {
        let mut m = lower_source(STEPPY, "steppy.cig").unwrap();
        run_passes(&mut m, &passes).unwrap();
        let (marks, end) = marks::trace(&m, 0);
        assert_eq!(end, None);
        assert_eq!(
            marks.len() as u64,
            total,
            "one mark per step the interpreter counts"
        );
        // With the budget at each value short of the total, both stop at
        // the same span: the first mark past the budget, or, where a `try`
        // caught that, the mark in its `ashtray` or `finally` that raised
        // again.
        let mut caught = 0;
        for max in 1..total {
            let (_, stop) = walker_steps(STEPPY, max);
            let (_, at) = marks::trace(&m, max);
            let at = at.expect("the budget ends the run");
            assert_eq!(
                stop,
                Some((at.line, at.col, "E515".to_string())),
                "--max-steps {max}"
            );
            if at != marks[max as usize] {
                caught += 1;
            }
        }
        assert!(caught > 0, "some budget runs out inside a try");
    }
    let (with_budget, stop) = walker_steps(STEPPY, total);
    assert_eq!(
        (with_budget, stop),
        (total, None),
        "a budget of exactly the total is enough"
    );
}

#[test]
fn every_statement_opens_with_a_step_and_every_loop_body_with_a_step_iter() {
    let mut m = lower_source(STEPPY, "steppy.cig").unwrap();
    run_passes(&mut m, &[]).unwrap();
    let text = print::print(&m);
    // A `pull` is hoisted and counts nothing; an `else if` is part of its
    // `if`; an expression lambda has no statement.
    assert_eq!(text.matches("step.iter").count(), 4, "{text}");
    assert_eq!(
        text.lines()
            .filter(|l| l.trim_start().starts_with("step "))
            .count(),
        29,
        "{text}"
    );
    let sq = text
        .split("func ")
        .find(|f| f.contains("lambda") && f.contains("bin * "))
        .expect("the expression lambda");
    assert!(!sq.contains("step"), "{sq}");
}

#[test]
fn a_step_mark_dropped_from_a_block_that_stays_is_caught_t3() {
    let src = "roll n = 0\nn += 1\nexhale n\n";
    let mut m = lower_source(src, "d.cig").unwrap();
    let e = run_passes(&mut m, &[&broken::DropStepMark as &dyn Pass]).unwrap_err();
    assert_eq!(e.pass, "drop-step-mark");
    assert!(
        e.errors
            .iter()
            .any(|x| x.code == "T3" && x.message.contains("step mark")),
        "{:#?}",
        e.errors
    );
}

#[test]
fn step_marks_go_with_their_whole_block() {
    // `sccp` empties the dead `else`, marks and all: the block is gone as
    // a whole, so nothing is miscounted.
    let src = "roll n = 0\nif true {\n  n = 1\n} else {\n  n = 2\n  n = 3\n}\nexhale n\n";
    let mut m = lower_source(src, "w.cig").unwrap();
    run_passes(&mut m, &[&pass::Sccp as &dyn Pass]).unwrap();
    let text = print::print(&m);
    assert_eq!(text.matches("step").count(), 4, "{text}");
}

#[test]
fn a_loop_body_without_its_step_iter_is_caught_v11() {
    let src = "roll i = 0\nwhile i < 3 {\n  i += 1\n}\n";
    let mut m = lower_source(src, "l.cig").unwrap();
    let e = run_passes(&mut m, &[&broken::EmptyLoopBody as &dyn Pass]).unwrap_err();
    assert_eq!(e.pass, "empty-loop-body");
    assert!(e.errors.iter().any(|x| x.code == "V11"), "{:#?}", e.errors);
    // A `step.iter` moved out of its loop body is V11 and T1 both.
    let mut m = lower_source(src, "l.cig").unwrap();
    let e = run_passes(&mut m, &[&broken::HoistStepIter as &dyn Pass]).unwrap_err();
    assert!(e.errors.iter().any(|x| x.code == "V11"), "{:#?}", e.errors);
    assert!(e.errors.iter().any(|x| x.code == "T1"), "{:#?}", e.errors);
}

/// Golden: `fold-consts` alone folds through a chain (`2 + 3 * 4`), a
/// string `+` and the compare on it, and leaves an overflow and a
/// division by zero to raise at run time.
#[test]
fn fold_consts_matches_its_golden_text() {
    golden_for_pass("fold-consts", "chain", &[&pass::FoldConsts as &dyn Pass]);
}

/// Golden: `dead-pure` alone removes an unused list, an unused
/// interpolation and the read of a `stick` it alone used; it keeps an
/// unused `a / b`, which may raise, and the reads of the `roll` slots it
/// needs.
#[test]
fn dead_pure_matches_its_golden_text() {
    golden_for_pass("dead-pure", "unused", &[&pass::DeadPure as &dyn Pass]);
}

/// Every pass `--passes` can name has a broken twin, and the verifier
/// catches the twin by the rule it names, on a script the real pass gets
/// through.
#[test]
fn each_pass_has_a_negative_test_that_is_caught() {
    for name in pass::NAMES {
        let real = pass::by_name(name).unwrap_or_else(|| panic!("{name} has no pass"));
        let twin = broken::twin(name).unwrap_or_else(|| panic!("{name} has no broken twin"));
        assert_ne!(twin.pass.name(), *name);
        let mut m = lower_source(twin.script, "twin.cig").unwrap();
        run_passes(&mut m, &[real]).unwrap_or_else(|e| panic!("{name} itself: {e:#?}"));
        let mut m = lower_source(twin.script, "twin.cig").unwrap();
        let e = run_passes(&mut m, &[twin.pass]).expect_err(&format!(
            "{}, the twin of {name}, is caught",
            twin.pass.name()
        ));
        assert_eq!(e.pass, twin.pass.name());
        assert!(
            e.errors.iter().any(|x| x.code == twin.rule),
            "{} is caught by {}: {:#?}",
            twin.pass.name(),
            twin.rule,
            e.errors
        );
    }
}

/// The class of each instruction in `main`, by the mnemonic it prints as.
fn classes(src: &str) -> Vec<(String, Class)> {
    let m = lower_source(src, "k.cig").unwrap();
    let f = &m.funcs[0];
    let text = print::print(&m);
    let lines: Vec<&str> = text
        .lines()
        .skip_while(|l| !l.starts_with("func @0"))
        .skip(1)
        .collect();
    let mut out = Vec::new();
    for (i, line) in f.body.instrs.iter().zip(lines) {
        let shown = line.split(" @").next().unwrap().trim();
        let shown = shown.split(" = ").last().unwrap().to_string();
        out.push((shown, i.kind.class(f, &m.vars)));
    }
    out
}

#[test]
fn the_class_of_an_operation_is_read_from_its_operand_types() {
    let got = classes(
        "roll r = 1\nstick s = 2\nexhale 1 + 2, 1.5 + 2.5, \"a\" < \"b\", 1.5 < 2.5, 1 == \"x\", 4 / 2, -1.5, -1, not r, r, s, [s], {k: s}\n",
    );
    let class = |want: &str| {
        got.iter()
            .find(|(shown, _)| shown.starts_with(want))
            .unwrap_or_else(|| panic!("{want} in {got:#?}"))
            .1
    };
    // Integer arithmetic overflows (E503); floats do not.
    assert_eq!(class("bin + %"), Class::MayRaise);
    let floats: Vec<_> = got.iter().filter(|(s, _)| s.starts_with("bin +")).collect();
    assert_eq!(floats[1].1, Class::Pure, "{got:#?}");
    // Strings order; a float may be a NaN, which raises.
    let orders: Vec<_> = got.iter().filter(|(s, _)| s.starts_with("bin <")).collect();
    assert_eq!((orders[0].1, orders[1].1), (Class::Pure, Class::MayRaise));
    // `==` never raises; `/` always may.
    assert_eq!(class("bin =="), Class::Pure);
    assert_eq!(class("bin /"), Class::MayRaise);
    let negs: Vec<_> = got
        .iter()
        .filter(|(s, _)| s.starts_with("un neg"))
        .collect();
    assert_eq!((negs[0].1, negs[1].1), (Class::Pure, Class::MayRaise));
    assert_eq!(class("un not"), Class::Pure);
    // A `roll` slot changes; a `stick` slot read where it lives does not.
    assert_eq!(class("load $0:r!"), Class::Effect);
    assert_eq!(class("load $1:s"), Class::Pure);
    assert_eq!(class("list"), Class::Pure);
    assert_eq!(class("map"), Class::Pure);
    assert_eq!(class("step"), Class::Step);
    assert_eq!(class("exhale"), Class::Effect);
    assert_eq!(class("decl"), Class::Effect);
}

/// Moves the instruction at `from` in the top level's own block to `to`.
struct Move {
    from: usize,
    to: usize,
}

impl Pass for Move {
    fn name(&self) -> &'static str {
        "move"
    }

    fn run(&self, m: &mut cigscript::lower::cir::Module) {
        let b = &mut m.funcs[0].body.instrs;
        let i = b.remove(self.from);
        b.insert(self.to, i);
    }
}

#[test]
fn pure_code_moves_freely_and_code_that_may_raise_keeps_its_order_t2() {
    // step, %0 = const 1, decl a, step, %1 = const 0, decl b, step,
    // load a, load b, bin /, step, const "x", exhale.
    let src = "roll a = 1\nroll b = 0\na / b\nexhale \"x\"\n";
    let mut m = lower_source(src, "mv.cig").unwrap();
    let names: Vec<String> = print::print(&m)
        .lines()
        .skip(2)
        .map(|l| l.split(" @").next().unwrap().trim().to_string())
        .collect();
    assert!(names[9].ends_with("bin / %2, %3"), "{names:#?}");
    assert!(names[11].ends_with("const \"x\""), "{names:#?}");
    // The constant moves up, past the division, a step mark and a slot
    // write: pure, so nothing is said.
    run_passes(&mut m, &[&Move { from: 11, to: 2 } as &dyn Pass]).unwrap();
    // The division moves down past a step mark and the `exhale`: it would
    // now raise after the output, not before.
    let mut m = lower_source(src, "mv.cig").unwrap();
    let e = run_passes(&mut m, &[&Move { from: 9, to: 12 } as &dyn Pass]).unwrap_err();
    assert!(e.errors.iter().any(|x| x.code == "T2"), "{:#?}", e.errors);
}

/// Replaces the first `bin +` in the top level with `const 6`.
struct FoldToSix;

impl Pass for FoldToSix {
    fn name(&self) -> &'static str {
        "fold-to-six"
    }

    fn run(&self, m: &mut cigscript::lower::cir::Module) {
        for i in &mut m.funcs[0].body.instrs {
            if matches!(i.kind, Kind::Binary(..)) {
                i.kind = Kind::Const(Const::Int(6));
                return;
            }
        }
    }
}

#[test]
fn a_fold_the_interpreter_disagrees_with_is_caught_t4() {
    let mut m = lower_source("exhale 2 + 3\n", "f.cig").unwrap();
    let e = run_passes(&mut m, &[&FoldToSix as &dyn Pass]).unwrap_err();
    assert!(
        e.errors
            .iter()
            .any(|x| x.code == "T4" && x.message.contains("Int(5)")),
        "{:#?}",
        e.errors
    );
}

#[test]
fn code_that_may_raise_goes_with_an_arm_that_never_runs() {
    // `sccp` empties the dead arm, its division and its marks with it: the
    // arm never runs, so nothing in it can raise or count a step.
    let src = "if false {\n  exhale 1 / 0\n}\n";
    let mut m = lower_source(src, "d.cig").unwrap();
    run_passes(&mut m, &[&pass::Sccp as &dyn Pass]).unwrap();
    assert!(!print::print(&m).contains("bin /"));
    // Emptying the arm that does run is caught.
    let src = "if true {\n  exhale 1 / 0\n}\n";
    let mut m = lower_source(src, "d.cig").unwrap();
    let e = run_passes(&mut m, &[&EmptyThen as &dyn Pass]).unwrap_err();
    let codes: Vec<_> = e.errors.iter().map(|x| x.code).collect();
    assert!(codes.contains(&"T3") && codes.contains(&"T4"), "{codes:?}");
}

/// Empties the `then` of the top level's first `if`.
struct EmptyThen;

impl Pass for EmptyThen {
    fn name(&self) -> &'static str {
        "empty-then"
    }

    fn run(&self, m: &mut cigscript::lower::cir::Module) {
        for i in &mut m.funcs[0].body.instrs {
            if let Kind::If { then, .. } = &mut i.kind {
                then.instrs.clear();
                return;
            }
        }
    }
}

#[test]
fn a_pure_read_moved_above_its_declaration_is_caught_v1() {
    // step, const 1, decl s, step, load s, exhale: the read of a `stick`
    // is pure, so only V1 sees it move above the `decl`.
    let mut m = lower_source("stick s = 1\nexhale s\n", "s.cig").unwrap();
    let e = run_passes(&mut m, &[&Move { from: 4, to: 0 } as &dyn Pass]).unwrap_err();
    assert!(
        e.errors
            .iter()
            .any(|x| x.code == "V1" && x.message.contains("before its declaration")),
        "{:#?}",
        e.errors
    );
}

/// A run of `src` through the CIR evaluator after `passes` gives what the
/// tree-walker gives: the same lines out, the same error position.
fn same_as_the_walker(src: &str) {
    let (want_out, want_stop) = walker_run(src);
    for (n, passes) in [pass::level(0), pass::level(1)].into_iter().enumerate() {
        let mut m = lower_source(src, "w.cig").unwrap();
        run_passes(&mut m, &passes).unwrap();
        let (_, out, stop) = marks::run(&m, 0);
        let stop = stop.map(|s| (s.line, s.col));
        assert_eq!((&out, stop), (&want_out, want_stop), "-O{n}: {src}");
    }
}

#[test]
fn a_computed_key_is_checked_before_the_next_value() {
    // The first key is not a valid key; the second value would raise too.
    // The tree-walker raises the key's error, at the key.
    let src = "roll k = true\nexhale {[k]: 1, b: 1 / 0}\n";
    assert_eq!(walker_run(src).1, Some((2, 10)));
    same_as_the_walker(src);
    let m = lower_source(src, "k.cig").unwrap();
    let text = print::print(&m);
    let key = text.find("key %").expect("a key instruction");
    let div = text.find("bin /").expect("the division");
    assert!(key < div, "the check comes before the next value: {text}");
    // An int key is its text; a list key raises, after copy propagation
    // put a value made elsewhere in its place.
    same_as_the_walker("stick n = 7\nstick m = {[n]: 1}\nexhale m[\"7\"]\n");
    same_as_the_walker("stick k = [1]\nexhale {[k]: 1}\n");
}

#[test]
fn a_computed_key_runs_its_effects_before_the_next_value_reads_them() {
    let src = "roll n = 0\npull bump() {\n  n += 1\n  snuff \"k\"\n}\nstick m = {[bump()]: n, b: n + 1}\nexhale m.k, m.b\n";
    assert_eq!(walker_run(src), (vec!["1 2".to_string()], None));
    same_as_the_walker(src);
}

#[test]
fn an_index_keeps_its_operands_positions_through_the_passes() {
    // Copy propagation puts a constant made on another line in place of
    // each operand: the error positions stay the tree-walker's.
    for src in [
        // A wrong index type: at the index.
        "stick xs = [1, 2]\nstick k = \"a\"\nexhale xs[k]\n",
        // A value that cannot be indexed: at the whole expression.
        "stick n = 5\nstick k = 0\nexhale n[k]\n",
        // A store with a wrong index type: at the index.
        "stick xs = [1]\nstick k = \"a\"\nxs[k] = 2\n",
        // A store into a value that cannot be indexed: at the object.
        "stick n = 5\nn[0] = 1\n",
    ] {
        assert!(walker_run(src).1.is_some(), "{src}");
        same_as_the_walker(src);
    }
    let mut m = lower_source("stick xs = [1]\nstick k = \"a\"\nxs[k] = 2\n", "s.cig").unwrap();
    run_passes(&mut m, &pass::default_passes()).unwrap();
    let text = print::print(&m);
    assert!(
        !text.contains("load "),
        "the operands were replaced: {text}"
    );
    assert!(text.contains("obj@3:1 idx@3:4"), "{text}");
}

#[test]
fn a_member_store_keeps_its_objects_position_through_the_passes() {
    // Copy propagation puts the `5` of line 1 in place of `n`: the error
    // stays at the object, where the tree-walker raises it.
    let src = "stick n = 5\nn.k = 1\n";
    assert_eq!(walker_run(src).1, Some((2, 1)));
    same_as_the_walker(src);
    let mut m = lower_source(src, "m.cig").unwrap();
    run_passes(&mut m, &pass::default_passes()).unwrap();
    let text = print::print(&m);
    assert!(!text.contains("load "), "the object was replaced: {text}");
    assert!(text.contains("obj@2:1"), "{text}");
}

#[test]
fn an_operand_span_outside_its_instruction_is_caught_v2() {
    let mut m = lower_source("stick xs = [1]\nexhale xs[0]\n", "v.cig").unwrap();
    let outside = m.funcs[0].span;
    for i in &mut m.funcs[0].body.instrs {
        if let Kind::Index { idx_at, .. } = &mut i.kind {
            *idx_at = outside;
        }
    }
    let found = verify(&m);
    assert!(
        found
            .iter()
            .any(|e| e.code == "V2" && e.message.contains("index span")),
        "{found:#?}"
    );
}

/// Golden: a map literal lowered in the tree-walker's order (each
/// computed key, its `key` check, then the entry's value), and the
/// operand spans an index and a map's computed keys carry.
#[test]
fn a_map_and_an_index_match_their_golden_text() {
    golden_for_pass("lower", "map-and-index", &[]);
}

/// Golden: `copy-prop` replaces every operand of an index and an indexed
/// store; the operands' own spans stay on the instructions.
#[test]
fn copy_prop_keeps_index_operand_spans_golden() {
    golden_for_pass(
        "copy-prop",
        "index-operands",
        &[&pass::CopyProp as &dyn Pass],
    );
}
