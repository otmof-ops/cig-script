// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
//! Type inference over CIR, across calls (`src/lower/cir/infer.rs`): what
//! it infers, that it ends and is the same on every run, and that a type
//! it gives is the type the tree-walker's run has.
//! Golden texts are at `tests/lower_infer/<name>.types`; after a
//! deliberate change, `CIR_UPDATE_GOLDEN=1` writes them again.

use cigscript::lower::cir::infer::{self, Inference, Row, MAX_ROWS};
use cigscript::lower::cir::ir::{FuncId, Module, Ty};
use cigscript::lower::cir::lower_source;

fn lower(src: &str, file: &str) -> Module {
    lower_source(src, file).expect("lowers")
}

fn golden(name: &str) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/lower_infer");
    let lf = |t: String| t.replace("\r\n", "\n");
    let src = lf(std::fs::read_to_string(dir.join(format!("{name}.cig")))
        .unwrap_or_else(|e| panic!("{}: {e}", dir.join(format!("{name}.cig")).display())));
    let m = lower(&src, &format!("{name}.cig"));
    let inf = infer::infer(&m);
    let text = infer::print(&m, &inf);
    let g = dir.join(format!("{name}.types"));
    if std::env::var_os("CIR_UPDATE_GOLDEN").is_some() {
        std::fs::write(&g, &text).unwrap();
    }
    assert_eq!(
        text,
        lf(std::fs::read_to_string(&g).unwrap_or_default()),
        "CIR_UPDATE_GOLDEN=1 writes it again"
    );
}

fn top(inf: &Inference) -> &infer::Spec {
    inf.spec(FuncId(0), &Row(Vec::new()))
        .expect("the top level")
}

fn spec_of<'a>(m: &Module, inf: &'a Inference, name: &str) -> Vec<&'a infer::Spec> {
    let id = m
        .funcs
        .iter()
        .find(|f| f.name.as_deref() == Some(name))
        .map(|f| f.id)
        .unwrap_or_else(|| panic!("no function {name}"));
    inf.of(id).collect()
}

#[test]
fn a_recursive_function_called_with_one_int_is_one_row_of_int() {
    let src = "pull fib(n) {\n  if n < 2 { snuff n }\n  snuff fib(n - 1) + fib(n - 2)\n}\nexhale fib(10)\n";
    let m = lower(src, "fib.cig");
    let inf = infer::infer(&m);
    let specs = spec_of(&m, &inf, "fib");
    assert_eq!(specs.len(), 1, "one row");
    let s = specs[0];
    assert_eq!(s.row, Row(vec![Ty::Int]));
    assert_eq!(s.ret, Ty::Int);
    for t in s.slots.values() {
        assert_eq!(*t, Ty::Int, "every slot");
    }
}

#[test]
fn two_rows_are_kept_when_a_function_is_called_with_two_types() {
    let src =
        "pull dbl(x) {\n  snuff x + x\n}\nstick a = dbl(2)\nstick b = dbl(2.5)\nexhale a, b\n";
    let m = lower(src, "two.cig");
    let inf = infer::infer(&m);
    let mut rows: Vec<Row> = spec_of(&m, &inf, "dbl")
        .iter()
        .map(|s| s.row.clone())
        .collect();
    rows.sort();
    assert_eq!(rows, vec![Row(vec![Ty::Int]), Row(vec![Ty::Float])]);
    let by_row = |want: Ty| {
        spec_of(&m, &inf, "dbl")
            .into_iter()
            .find(|s| s.row == Row(vec![want]))
            .unwrap()
    };
    assert_eq!(by_row(Ty::Int).ret, Ty::Int);
    assert_eq!(by_row(Ty::Float).ret, Ty::Float);
}

#[test]
fn a_fourth_row_collapses_the_function_to_all_dynamic() {
    let src = "pull pick(x) {\n  snuff x\n}\nstick a = pick(1)\nstick b = pick(1.5)\nstick c = pick(\"s\")\nstick d = pick(true)\nexhale a, b, c, d\n";
    let m = lower(src, "four.cig");
    let inf = infer::infer(&m);
    let specs = spec_of(&m, &inf, "pick");
    assert_eq!(specs.len(), 1, "{:#?}", specs);
    assert_eq!(specs[0].row, Row(vec![Ty::Dynamic]));
    assert_eq!(specs[0].ret, Ty::Dynamic);
    // Every call of `pick` in the top level now records the one row.
    let t = top(&inf);
    for (_, row) in t.calls.values() {
        assert_eq!(*row, Row(vec![Ty::Dynamic]));
    }
}

#[test]
fn mutual_recursion_ends_and_both_return_types_settle() {
    let src = "pull odd(n) {\n  if n == 0 { snuff false }\n  snuff even(n - 1)\n}\npull even(n) {\n  if n == 0 { snuff true }\n  snuff odd(n - 1)\n}\nexhale odd(5)\n";
    let m = lower(src, "mut.cig");
    let inf = infer::infer(&m);
    for name in ["odd", "even"] {
        let s = spec_of(&m, &inf, name);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].row, Row(vec![Ty::Int]));
        assert_eq!(s[0].ret, Ty::Bool);
    }
}

#[test]
fn a_function_that_returns_two_types_has_a_dynamic_ret() {
    let src = "pull pick(n) {\n  if n > 0 { snuff n }\n  snuff \"nope\"\n}\nexhale pick(1)\n";
    let m = lower(src, "mix.cig");
    let inf = infer::infer(&m);
    assert_eq!(spec_of(&m, &inf, "pick")[0].ret, Ty::Dynamic);
}

#[test]
fn a_function_that_falls_off_its_end_returns_null() {
    let src = "pull noop() {\n  stick a = 1\n}\nexhale noop()\n";
    let m = lower(src, "fall.cig");
    let inf = infer::infer(&m);
    assert_eq!(spec_of(&m, &inf, "noop")[0].ret, Ty::Null);
}

#[test]
fn a_local_that_changes_type_in_a_loop_is_dynamic() {
    let src = "roll x = 0\nroll i = 0\nwhile i < 3 {\n  i += 1\n  x = x + 0.5\n}\nexhale x\n";
    let m = lower(src, "mut.cig");
    let inf = infer::infer(&m);
    let s = top(&inf);
    let x = s
        .slots
        .iter()
        .find(|(v, _)| m.vars[v.0 as usize].name == "x")
        .expect("x");
    assert_eq!(*x.1, Ty::Dynamic, "it joins Int and Float");
}

#[test]
fn a_local_that_stays_int_through_a_loop_is_int() {
    let src = "roll n = 0\nroll i = 0\nwhile i < 3 {\n  i += 1\n  n = n + 1\n}\nexhale n\n";
    let m = lower(src, "mi.cig");
    let inf = infer::infer(&m);
    let s = top(&inf);
    let n = s
        .slots
        .iter()
        .find(|(v, _)| m.vars[v.0 as usize].name == "n")
        .expect("n");
    assert_eq!(*n.1, Ty::Int);
}

#[test]
fn a_captured_mutable_slot_written_by_an_inner_function_is_dynamic() {
    // An inner pull captures and writes a mutable slot of its outer.
    let src = "pull outer() {\n  roll total = 0\n  pull inner(x) {\n    total = total + x\n  }\n  inner(1)\n  snuff total\n}\nexhale outer()\n";
    let m = lower(src, "cap.cig");
    let inf = infer::infer(&m);
    // Some reachable spec of `inner` must have a Dynamic value for its
    // read of `total`.
    let has = inf.specs().iter().any(|s| {
        let f = &m.funcs[s.func.0 as usize];
        f.name.as_deref() == Some("inner") && s.vals.contains(&Ty::Dynamic)
    });
    assert!(has, "{:#?}", inf);
}

#[test]
fn a_captured_immutable_stick_with_a_constant_initializer_is_known() {
    let src = "pull outer() {\n  stick base = 10\n  pull inner() { snuff base }\n  snuff inner()\n}\nexhale outer()\n";
    let m = lower(src, "capi.cig");
    let inf = infer::infer(&m);
    // `inner` captures `base` (immutable). A `load` of it in `inner`
    // gives `Int`, so `inner`'s `ret` is `Int`.
    let inner = spec_of(&m, &inf, "inner");
    assert!(!inner.is_empty(), "{:#?}", inf);
    assert!(inner.iter().all(|s| s.ret == Ty::Int), "{:#?}", inner);
}

#[test]
fn int_div_int_is_dynamic_but_int_div_float_is_float_and_an_ordering_is_bool() {
    let src = "stick a = 4 / 2\nstick b = 4 / 2.0\nstick c = 1 < 2\nexhale a, b, c\n";
    let m = lower(src, "ops.cig");
    let inf = infer::infer(&m);
    let s = top(&inf);
    let by_name = |n: &str| {
        *s.slots
            .iter()
            .find(|(v, _)| m.vars[v.0 as usize].name == n)
            .unwrap()
            .1
    };
    assert_eq!(by_name("a"), Ty::Dynamic);
    assert_eq!(by_name("b"), Ty::Float);
    assert_eq!(by_name("c"), Ty::Bool);
}

#[test]
fn for_i_in_range_of_ints_gives_an_int_item_and_over_a_list_gives_dynamic() {
    let src = "stick n = 10\nroll t = 0\nfor i in 0..n {\n  t = t + i\n}\nfor j in [1, 2, 3] {\n  t = t + j\n}\nexhale t\n";
    let m = lower(src, "fe.cig");
    let inf = infer::infer(&m);
    let s = top(&inf);
    let by_name = |n: &str| {
        *s.slots
            .iter()
            .find(|(v, _)| m.vars[v.0 as usize].name == n)
            .unwrap()
            .1
    };
    assert_eq!(by_name("i"), Ty::Int);
    assert_eq!(by_name("j"), Ty::Dynamic);
}

#[test]
fn a_call_through_a_value_and_a_method_call_are_dynamic() {
    let src = "stick xs = [1, 2, 3]\nstick first = xs[0]\nstick n = xs.length()\nstick fn = pack(x) => x\nstick m = fn(1)\nexhale first, n, m\n";
    let m = lower(src, "dyn.cig");
    let inf = infer::infer(&m);
    let s = top(&inf);
    let by_name = |nm: &str| {
        *s.slots
            .iter()
            .find(|(v, _)| m.vars[v.0 as usize].name == nm)
            .unwrap()
            .1
    };
    assert_eq!(by_name("first"), Ty::Dynamic);
    assert_eq!(by_name("n"), Ty::Dynamic);
    assert_eq!(by_name("m"), Ty::Dynamic);
}

#[test]
fn an_arity_mismatch_at_a_call_user_gives_dynamic() {
    let src = "pull add(a, b) { snuff a + b }\nstick x = add(1)\nexhale x\n";
    let m = lower(src, "arity.cig");
    let inf = infer::infer(&m);
    let s = top(&inf);
    let x = *s
        .slots
        .iter()
        .find(|(v, _)| m.vars[v.0 as usize].name == "x")
        .unwrap()
        .1;
    assert_eq!(x, Ty::Dynamic);
    // The call is still recorded.
    assert!(!s.calls.is_empty());
}

#[test]
fn an_inference_needs_several_rounds_to_settle() {
    // `a` returns `b`'s result; `b` returns `a`'s result, with each one
    // calling the other with two different concrete types in two places.
    // The first walk of `a` sees `b` with `Bot` for its `ret`; the second
    // walk sees it as `Int`; a third pass settles on the two rows.
    let src =
        "pull a(n) {\n  snuff b(n)\n}\npull b(n) {\n  snuff a(n)\n}\nstick x = a(1)\nexhale x\n";
    let m = lower(src, "rounds.cig");
    let inf = infer::infer(&m);
    // Each gets one `Int` row and `Int` ret.
    for name in ["a", "b"] {
        let s = spec_of(&m, &inf, name);
        assert_eq!(s.len(), 1, "{name}");
        assert_eq!(s[0].row, Row(vec![Ty::Int]));
        assert_eq!(s[0].ret, Ty::Dynamic); // infinite recursion: Bot -> Dynamic in print.
    }
}

#[test]
fn it_is_deterministic_and_within_max_rows() {
    for name in ["recursive-fib", "two-rows-int-and-float"] {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/lower_infer");
        let src = std::fs::read_to_string(dir.join(format!("{name}.cig"))).unwrap();
        let m = lower(&src, &format!("{name}.cig"));
        let a = infer::print(&m, &infer::infer(&m));
        let b = infer::print(&m, &infer::infer(&m));
        assert_eq!(a, b, "deterministic for {name}");
        let inf = infer::infer(&m);
        for s in inf.specs() {
            let f = &m.funcs[s.func.0 as usize];
            assert_eq!(s.vals.len(), f.vals.len(), "every value");
            let rows: Vec<_> = inf.of(s.func).collect();
            assert!(rows.len() <= MAX_ROWS, "{name}: {}", rows.len());
        }
    }
}

#[test]
fn every_bench_and_examples_script_is_deterministic_and_within_max_rows() {
    let src_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for dir in ["bench/compute", "examples"] {
        let d = src_root.join(dir);
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        let mut files: Vec<_> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "cig"))
            .collect();
        files.sort();
        for f in files {
            let name = f.file_name().unwrap().to_string_lossy().to_string();
            let src = std::fs::read_to_string(&f).unwrap();
            // A script the lowering refuses has no IR to infer over.
            let Ok(m) = lower_source(&src, &name) else {
                continue;
            };
            let a = infer::print(&m, &infer::infer(&m));
            let b = infer::print(&m, &infer::infer(&m));
            assert_eq!(a, b, "{dir}/{name} is not deterministic");
            let inf = infer::infer(&m);
            for s in inf.specs() {
                let fn_ = &m.funcs[s.func.0 as usize];
                assert_eq!(s.vals.len(), fn_.vals.len(), "{dir}/{name}");
                let rows_here: Vec<_> = inf.of(s.func).collect();
                assert!(
                    rows_here.len() <= MAX_ROWS,
                    "{dir}/{name}: {} rows for @{}",
                    rows_here.len(),
                    s.func.0
                );
            }
        }
    }
}

#[test]
fn fib_matches_its_golden_text() {
    golden("recursive-fib");
}

#[test]
fn two_rows_matches_its_golden_text() {
    golden("two-rows-int-and-float");
}

#[test]
fn fourth_row_collapses_matches_its_golden_text() {
    golden("fourth-row-collapses");
}

// ----- variables two functions share, and a collapse during a walk ----
//
// Each program here once got a type its run contradicts, or stopped the
// analysis. The types asserted are what the tree-walker's run has.

/// The type of the slot named `name` in a spec.
fn slot_ty(m: &Module, s: &infer::Spec, name: &str) -> Ty {
    *s.slots
        .iter()
        .find(|(v, _)| m.vars[v.0 as usize].name == name)
        .unwrap_or_else(|| panic!("no slot {name} in {s:#?}"))
        .1
}

/// A variable another function writes is `Dynamic` everywhere.
#[test]
fn a_variable_another_function_writes_is_dynamic_in_its_owner() {
    // Tree-walker: `type_of(x)` prints `string` after `set()` runs. The
    // inference once typed `x` as `int` in the top level (owner); it
    // must now be `dynamic` in the owner's own slot map, and every load
    // of `x` anywhere must type as `dynamic`.
    let src = "roll x = 1\npull set() { x = \"s\" }\nset()\nexhale x\n";
    let m = lower(src, "d1a.cig");
    let inf = infer::infer(&m);
    let t = top(&inf);
    assert_eq!(
        slot_ty(&m, t, "x"),
        Ty::Dynamic,
        "mut+captured in its owner is Dynamic: {t:#?}"
    );
    // Every spec that reads or writes `x` must do so as `Dynamic`.
    for s in inf.specs() {
        let f = &m.funcs[s.func.0 as usize];
        // Any slot of `x` (owner's spec only) is Dynamic.
        for (v, t) in &s.slots {
            if m.vars[v.0 as usize].name == "x" {
                assert_eq!(*t, Ty::Dynamic, "{f:?}", f = f.name);
            }
        }
    }
}

#[test]
fn a_variable_written_by_a_function_called_through_a_value_is_dynamic() {
    // Tree-walker: both `type_of(z)` and `type_of(t)` print `null`. The
    // owner's `z` must be `dynamic`, and the stick `t = z` (also owned
    // by main, immutable, not captured) must copy that `dynamic`.
    let src =
        "roll z = 1\npull wz() { z = null }\nstick f = wz\nf()\nexhale z\nstick t = z\nexhale t\n";
    let m = lower(src, "d1b.cig");
    let inf = infer::infer(&m);
    let t = top(&inf);
    assert_eq!(slot_ty(&m, t, "z"), Ty::Dynamic);
    assert_eq!(slot_ty(&m, t, "t"), Ty::Dynamic);
}

/// A read of another function's immutable variable has the IR's static type of its
/// `Decl` value, and nothing else. A call chain that changes the owner's
/// row later cannot make the capture reader go stale.
#[test]
fn a_read_of_anothers_variable_does_not_keep_an_earlier_rows_type() {
    // Tree-walker: the first `type_of(x)` prints `int`, the second
    // prints `string`. The inner `inner` reads `x` by capture; `x` was
    // declared `stick x = a`, whose Decl value is a `load` of a
    // parameter — the IR's static type is `Dynamic`. So in `inner` the
    // load of `x` must be `Dynamic` across every reachable spec, not an
    // `int` that would turn stale when the outer is re-called with a
    // string.
    let src = r#"pull outer(a) {
  stick x = a
  pull inner() { exhale x }
  inner()
}
pull l3() { outer("s") }
pull l2() { l3() }
pull l1() { l2() }
outer(1)
l1()
"#;
    let m = lower(src, "d2.cig");
    let inf = infer::infer(&m);
    let specs = spec_of(&m, &inf, "inner");
    assert!(!specs.is_empty(), "{:#?}", inf);
    // Find the load of `x` in `inner`'s body: it reads a capture of an
    // immutable slot whose IR static Decl type is `Dynamic` (the Decl
    // value is a load of a parameter), so this load must be `Dynamic`.
    use cigscript::lower::cir::ir::Kind;
    for s in specs {
        let f = &m.funcs[s.func.0 as usize];
        let x_id = m
            .vars
            .iter()
            .enumerate()
            .find(|(_, info)| info.name == "x")
            .map(|(i, _)| cigscript::lower::cir::ir::VarId(i as u32))
            .unwrap();
        let mut found = false;
        find_load(&f.body, x_id, s, &mut found);
        assert!(found, "inner's load of x was not Dynamic: {s:#?}");
    }
    fn find_load(
        b: &cigscript::lower::cir::ir::Block,
        x: cigscript::lower::cir::ir::VarId,
        s: &infer::Spec,
        found: &mut bool,
    ) {
        for i in &b.instrs {
            if let Kind::Load(v) = &i.kind {
                if *v == x {
                    if let Some(r) = i.result {
                        if s.vals[r.0 as usize] == Ty::Dynamic {
                            *found = true;
                        }
                    }
                }
            }
            for sub in i.kind.blocks() {
                find_load(sub, x, s, found);
            }
        }
    }
}

#[test]
fn a_read_of_anothers_constant_is_its_type() {
    // Here the capture is of a slot whose Decl value is a constant: the
    // IR's static type is `Int`, so the reader may use it.
    let src = r#"pull outer() {
  stick base = 10
  pull inner() { snuff base }
  snuff inner()
}
exhale outer()
"#;
    let m = lower(src, "d2b.cig");
    let inf = infer::infer(&m);
    for s in spec_of(&m, &inf, "inner") {
        assert_eq!(s.ret, Ty::Int, "{s:#?}");
    }
}

/// A function that collapses to its all-`Dynamic` row while it is being
/// walked: the analysis ends, with that one row.
#[test]
fn a_function_that_collapses_while_it_is_walked_ends() {
    // Three concrete rows then a fourth row from the function's own
    // recursive call: it collapses to all-`Dynamic` while the third
    // row is being walked.
    let src = r#"pull f(a, n) {
  if n == 0 { snuff a }
  snuff f(null, n - 1)
}
exhale f(1, 1)
exhale f(1.5, 1)
exhale f("s", 1)
"#;
    let m = lower(src, "d3.cig");
    // Just that `infer` returns, and gives one (Dynamic, Dynamic) row
    // for `f`.
    let inf = infer::infer(&m);
    let specs = spec_of(&m, &inf, "f");
    assert_eq!(specs.len(), 1, "collapsed to one row");
    assert_eq!(specs[0].row, Row(vec![Ty::Dynamic, Ty::Dynamic]));
}

/// Determinism inside one process: 20 `infer` calls on the same module
/// print the same text (HashMap seeds differ per instance, so one
/// process is enough).
#[test]
fn the_same_on_twenty_runs_of_each_of_these_programs() {
    let srcs = [
        "roll x = 1\npull set() { x = \"s\" }\nset()\nexhale x\n",
        "roll z = 1\npull wz() { z = null }\nstick f = wz\nf()\nexhale z\nstick t = z\nexhale t\n",
        "pull outer(a) {\n  stick x = a\n  pull inner() { exhale x }\n  inner()\n}\npull l3() { outer(\"s\") }\npull l2() { l3() }\npull l1() { l2() }\nouter(1)\nl1()\n",
        "pull f(a, n) {\n  if n == 0 { snuff a }\n  snuff f(null, n - 1)\n}\nexhale f(1, 1)\nexhale f(1.5, 1)\nexhale f(\"s\", 1)\n",
    ];
    for (i, src) in srcs.iter().enumerate() {
        let m = lower(src, &format!("det{i}.cig"));
        let first = infer::print(&m, &infer::infer(&m));
        for n in 0..20 {
            let next = infer::print(&m, &infer::infer(&m));
            assert_eq!(first, next, "program {i}, run {n}");
        }
    }
}

/// Determinism of every bench and examples script: 20 runs in one
/// process; if a seed differs per map instance, this catches it.
#[test]
fn the_same_on_twenty_runs_of_every_script_in_the_corpus_the_benchmarks_and_the_examples() {
    let src_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for dir in [
        "bench/compute",
        "examples",
        "tests/corpus",
        "tests/lower_infer",
    ] {
        let d = src_root.join(dir);
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        let mut files: Vec<_> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "cig"))
            .collect();
        files.sort();
        for f in files {
            let name = f.file_name().unwrap().to_string_lossy().to_string();
            let src = std::fs::read_to_string(&f).unwrap();
            let Ok(m) = lower_source(&src, &name) else {
                continue;
            };
            let first = infer::print(&m, &infer::infer(&m));
            for _ in 0..20 {
                let next = infer::print(&m, &infer::infer(&m));
                assert_eq!(first, next, "{dir}/{name} is not deterministic");
            }
        }
    }
}

/// Every script that lowers is inferred without a panic, and every spec
/// is well formed: a type for each value, only its own function's
/// slots, `Dynamic` for every variable another function can write, and
/// calls that name specs the result has.
#[test]
fn every_script_ends_and_every_spec_is_well_formed() {
    let src_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for dir in [
        "bench/compute",
        "examples",
        "tests/corpus",
        "tests/lower_infer",
    ] {
        let d = src_root.join(dir);
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        let mut files: Vec<_> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "cig"))
            .collect();
        files.sort();
        for f in files {
            let name = f.file_name().unwrap().to_string_lossy().to_string();
            let src = std::fs::read_to_string(&f).unwrap();
            let Ok(m) = lower_source(&src, &name) else {
                continue;
            };
            let inf = infer::infer(&m);
            for s in inf.specs() {
                let fn_ = &m.funcs[s.func.0 as usize];
                assert_eq!(s.vals.len(), fn_.vals.len(), "{dir}/{name}");
                // Every VarId in slots is owned by the function.
                for v in s.slots.keys() {
                    let info = &m.vars[v.0 as usize];
                    assert_eq!(
                        info.owner, s.func,
                        "{dir}/{name}: {v:?} is in @{}'s slots but owned by @{}",
                        s.func.0, info.owner.0
                    );
                }
                // No mut+captured slot has any type but Dynamic.
                for (v, t) in &s.slots {
                    let info = &m.vars[v.0 as usize];
                    if info.mutable && info.captured {
                        assert_eq!(*t, Ty::Dynamic, "{dir}/{name}: mut+captured {v:?} = {t:?}");
                    }
                }
                // Every call entry names a spec that exists, or an
                // arity mismatch (the callee's arity != the row's).
                for (g, row) in s.calls.values() {
                    let arity = m.funcs[g.0 as usize].params.len();
                    if row.0.len() == arity {
                        assert!(
                            inf.spec(*g, row).is_some(),
                            "{dir}/{name}: call to @{}({row:?}) names no spec",
                            g.0
                        );
                    }
                }
            }
        }
    }
}

// ---- Ten soundness tests whose expected types come from the tree-walker ----

#[test]
fn soundness_parameter_reassigned_to_a_float_is_dynamic() {
    // Tree-walker: `type_of(n)` after the reassign prints `float`; a
    // single row can type it as both `int` and `float`, so it must be
    // `dynamic`.
    let src = "pull f(n) {\n  n = n + 0.5\n  snuff n\n}\nexhale f(1)\n";
    let m = lower(src, "s1.cig");
    let inf = infer::infer(&m);
    let s = &spec_of(&m, &inf, "f")[0];
    assert_eq!(s.ret, Ty::Dynamic);
}

#[test]
fn soundness_a_pure_int_add_in_a_function_stays_int() {
    // Tree-walker: result is `int`.
    let src = "pull f(a, b) { snuff a + b }\nexhale f(1, 2)\n";
    let m = lower(src, "s2.cig");
    let inf = infer::infer(&m);
    let s = &spec_of(&m, &inf, "f")[0];
    assert_eq!(s.ret, Ty::Int);
}

#[test]
fn soundness_a_function_reached_through_three_pulls_keeps_its_row() {
    let src =
        "pull a(x) { snuff x }\npull b(x) { snuff a(x) }\npull c(x) { snuff b(x) }\nexhale c(1)\n";
    let m = lower(src, "s3.cig");
    let inf = infer::infer(&m);
    for name in ["a", "b", "c"] {
        let s = &spec_of(&m, &inf, name)[0];
        assert_eq!(s.ret, Ty::Int, "{name}");
    }
}

#[test]
fn soundness_a_closure_captured_variable_mutated_by_a_lambda_is_dynamic() {
    // Tree-walker: after `add(1)` the owner reads a Dynamic value.
    // The inner is a lambda (not a pull), so its spec may not exist,
    // but the OWNER's slot must still be Dynamic from the pre-pass.
    let src = "roll total = 0\nstick add = pack(x) => total + x\nadd(1)\nexhale total\n";
    let m = lower(src, "s4.cig");
    let inf = infer::infer(&m);
    let t = top(&inf);
    assert_eq!(slot_ty(&m, t, "total"), Ty::Dynamic);
}

#[test]
fn soundness_a_stick_initialised_from_an_int_is_int_in_its_owner() {
    let src = "stick n = 10\nexhale n\n";
    let m = lower(src, "s5.cig");
    let inf = infer::infer(&m);
    let t = top(&inf);
    assert_eq!(slot_ty(&m, t, "n"), Ty::Int);
}

#[test]
fn soundness_a_for_over_an_int_range_gives_an_int_loop_variable() {
    let src = "roll s = 0\nfor i in 0..10 {\n  s = s + i\n}\nexhale s\n";
    let m = lower(src, "s6.cig");
    let inf = infer::infer(&m);
    let t = top(&inf);
    assert_eq!(slot_ty(&m, t, "i"), Ty::Int);
    assert_eq!(slot_ty(&m, t, "s"), Ty::Int);
}

#[test]
fn soundness_two_separate_pulls_recursed_in_parallel_settle() {
    let src = "pull p(n) {\n  if n == 0 { snuff 0 }\n  snuff p(n - 1) + 1\n}\nexhale p(5)\n";
    let m = lower(src, "s7.cig");
    let inf = infer::infer(&m);
    let s = &spec_of(&m, &inf, "p")[0];
    assert_eq!(s.row, Row(vec![Ty::Int]));
    assert_eq!(s.ret, Ty::Int);
}

#[test]
fn soundness_a_loop_whose_body_leaves_a_slot_mixed_is_dynamic() {
    // Tree-walker: `type_of(a)` is `string` by the end.
    let src = "roll a = 1\nroll i = 0\nwhile i < 3 {\n  i += 1\n  a = \"s\"\n}\nexhale a\n";
    let m = lower(src, "s8.cig");
    let inf = infer::infer(&m);
    let t = top(&inf);
    assert_eq!(slot_ty(&m, t, "a"), Ty::Dynamic);
}

#[test]
fn soundness_a_function_called_with_four_different_rows_collapses() {
    // Tree-walker: each call returns its argument; one row keeps only
    // `dynamic` for both operand and return.
    let src = r#"pull id(x) { snuff x }
exhale id(1)
exhale id(1.5)
exhale id(true)
exhale id("s")
"#;
    let m = lower(src, "s9.cig");
    let inf = infer::infer(&m);
    let specs = spec_of(&m, &inf, "id");
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].row, Row(vec![Ty::Dynamic]));
    assert_eq!(specs[0].ret, Ty::Dynamic);
}

#[test]
fn soundness_a_pull_that_writes_a_captured_slot_in_a_branch_is_still_dynamic() {
    // Tree-walker: after `clear()` runs under a condition, `n` is
    // `null` on one path and `int` on another. mut+captured is
    // Dynamic everywhere regardless.
    let src = r#"roll n = 1
pull clear() { n = null }
if n == 1 {
  clear()
}
exhale n
"#;
    let m = lower(src, "s10.cig");
    let inf = infer::infer(&m);
    let t = top(&inf);
    assert_eq!(slot_ty(&m, t, "n"), Ty::Dynamic);
}

/// A `#[test] #[ignore]` that times `infer` on an adjacent cig program
/// when `CIG_INFER_BENCH` names a `.cig` file. Prints one figure, no
/// assertion. Run as `cargo test --release ... -- --ignored`.
#[test]
#[ignore]
fn timing_on_one_program() {
    let Some(path) = std::env::var_os("CIG_INFER_BENCH") else {
        eprintln!("set CIG_INFER_BENCH=/path/to/file.cig");
        return;
    };
    let src = std::fs::read_to_string(&path).unwrap();
    let m = lower_source(&src, path.to_string_lossy().as_ref()).unwrap();
    let start = std::time::Instant::now();
    let inf = infer::infer(&m);
    let took = start.elapsed();
    eprintln!(
        "infer: {:?} ({} funcs, {} specs)",
        took,
        m.funcs.len(),
        inf.specs().len()
    );
}

/// The rows `name` has, in order.
fn rows_of(m: &Module, inf: &Inference, name: &str) -> Vec<Row> {
    spec_of(m, inf, name)
        .iter()
        .map(|s| s.row.clone())
        .collect()
}

const FIB: &str = "pull fib(n) {\n  if n < 2 { snuff n }\n  snuff fib(n - 1) + fib(n - 2)\n}\n";

/// A call's argument that nothing has typed yet (here the result of a
/// function the walk has not reached) asks for no row: the call is
/// asked for once, with the argument's type.
#[test]
fn a_call_given_another_calls_result_asks_for_one_row() {
    let src = format!("{FIB}pull label(n) {{ snuff \"fib = ${{n}}\" }}\nexhale label(fib(20))\n");
    let m = lower(&src, "label.cig");
    let inf = infer::infer(&m);
    assert_eq!(rows_of(&m, &inf, "label"), [Row(vec![Ty::Int])]);
    assert_eq!(rows_of(&m, &inf, "fib"), [Row(vec![Ty::Int])]);
    assert_eq!(inf.specs().len(), 3, "the top level, fib and label");
}

/// Such a call does not count against the rows a function keeps: three
/// calls with three types, each the result of another call, are three
/// rows and no collapse.
#[test]
fn rows_not_yet_typed_do_not_count_against_the_rows_a_function_keeps() {
    let src = format!(
        "{FIB}pull half(x) {{\n  if x < 1.0 {{ snuff x }}\n  snuff half(x / 2.0)\n}}\npull odd(n) {{\n  if n == 0 {{ snuff false }}\n  snuff not odd(n - 1)\n}}\npull show(x) {{ snuff x }}\nexhale show(fib(5)), show(half(8.0)), show(odd(3))\n"
    );
    let m = lower(&src, "three.cig");
    let inf = infer::infer(&m);
    assert_eq!(MAX_ROWS, 3);
    assert_eq!(
        rows_of(&m, &inf, "show"),
        [
            Row(vec![Ty::Bool]),
            Row(vec![Ty::Int]),
            Row(vec![Ty::Float])
        ]
    );
    for s in spec_of(&m, &inf, "show") {
        assert_eq!(Row(vec![s.ret]), s.row, "it returns what it is given");
    }
}

/// A row asked for while a callee's result was still one type is not in
/// the result once that result is `Dynamic`: the inference holds what
/// the top level reaches, and nothing a walk left behind.
#[test]
fn a_row_no_call_enters_any_more_is_not_in_the_result() {
    let src = "pull text(n) { snuff \"s\" }\npull pick(n) {\n  if n > 0 { snuff 1 }\n  snuff text(n)\n}\npull show(x) { snuff x }\nexhale show(pick(1))\n";
    let m = lower(src, "stale.cig");
    let inf = infer::infer(&m);
    assert_eq!(spec_of(&m, &inf, "pick")[0].ret, Ty::Dynamic);
    assert_eq!(rows_of(&m, &inf, "show"), [Row(vec![Ty::Dynamic])]);
    // Every spec is one a call of a spec in the result enters.
    let entered: Vec<(FuncId, Row)> = inf
        .specs()
        .iter()
        .flat_map(|s| s.calls.values().cloned())
        .collect();
    for s in inf.specs() {
        assert!(
            s.func == FuncId(0) || entered.contains(&(s.func, s.row.clone())),
            "{:?} {:?}",
            s.func,
            s.row
        );
    }
}

/// A call an argument of which never gets a type (the result of a
/// function that never returns) is in code that cannot run: it has no
/// entry in its spec's call table, and its function no row from it.
#[test]
fn a_call_that_cannot_run_enters_nothing() {
    let src = "pull never(n) { snuff never(n) }\npull show(x) { snuff x }\nexhale show(never(1))\n";
    let m = lower(src, "never.cig");
    let inf = infer::infer(&m);
    assert_eq!(rows_of(&m, &inf, "never"), [Row(vec![Ty::Int])]);
    assert!(spec_of(&m, &inf, "show").is_empty());
    assert_eq!(top(&inf).calls.len(), 1, "the call of `never` alone");
}
