// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! #146: names resolved at check time (`check::check` writes a `Res` into
//! the tree, `Scope::get_at` reads by position). The scripts are in
//! `tests/walker`, each with the output the lookup-by-name interpreter
//! gave before the change.

use super::env::{resolved_checked, resolved_missed};
use super::Interp;
use crate::burn::{Kernel, Mode};
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

#[derive(Clone, Default)]
struct Sink(Rc<RefCell<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Run `src` and return what it printed. `checked` runs `cig check` over
/// the tree first, as `cig run` does, so names are resolved; unchecked, every
/// name is looked up by name.
fn run(src: &str, checked: bool) -> String {
    let program = crate::syntax::parse(src).expect("parses");
    if checked {
        let report = crate::check::check(&program, &[]);
        assert!(report.ok(), "{:?}", report.errors);
    }
    let sink = Sink::default();
    let mut interp = Interp::new(Kernel::ephemeral(Mode::Run));
    interp.out = Box::new(sink.clone());
    interp.set_max_steps(0);
    interp.run_program(&program).expect("runs");
    let bytes = sink.0.borrow().clone();
    String::from_utf8(bytes).expect("utf-8")
}

const SCRIPTS: [(&str, &str, &str); 3] = [
    (
        "resolve-mixed",
        include_str!("../../tests/walker/resolve-mixed.cig"),
        include_str!("../../tests/walker/resolve-mixed.out"),
    ),
    (
        "resolve-cells",
        include_str!("../../tests/walker/resolve-cells.cig"),
        include_str!("../../tests/walker/resolve-cells.out"),
    ),
    (
        "resolve-shadow",
        include_str!("../../tests/walker/resolve-shadow.cig"),
        include_str!("../../tests/walker/resolve-shadow.out"),
    ),
];

/// Every resolved read and write is checked against the lookup by name as
/// it happens (a mismatch panics in `Scope::get_at`); none may fall back;
/// and the tree run with its names resolved prints what the same tree
/// prints when every name is looked up.
#[test]
fn resolved_reads_equal_named_reads() {
    for (name, src, want) in SCRIPTS {
        let (checked_before, missed_before) = (resolved_checked(), resolved_missed());
        let resolved = run(src, true);
        assert!(
            resolved_checked() > checked_before,
            "{name}: nothing was resolved"
        );
        assert_eq!(
            resolved_missed(),
            missed_before,
            "{name}: a resolved name was not where the checker said"
        );
        assert_eq!(resolved, want, "{name}: resolved output");
        assert_eq!(run(src, false), want, "{name}: named output");
    }
}

/// A closure made in a loop sees its own turn's cell: a `for` variable and
/// a block's names are fresh each turn, and one turn's closures share the
/// turn's cell.
#[test]
fn per_iteration_cells_hold() {
    let (_, src, want) = SCRIPTS[1];
    assert_eq!(run(src, true), want);
    let src = "\
roll fs = []
for i in 0..4 {
  roll seen = i
  fs.push(pack() => i * 100 + seen)
  seen = seen + 1
}
roll w = 0
while w < 3 {
  roll own = w
  fs.push(pack() => own)
  w += 1
}
exhale fs.map(pack(f) => f())
";
    assert_eq!(run(src, true), "[1, 102, 203, 304, 0, 1, 2]\n");
    assert_eq!(run(src, false), "[1, 102, 203, 304, 0, 1, 2]\n");
}

/// A name a function body read from outside, then declared by the scope it
/// was made in, reads the new one on a later call, as the lookup by name
/// does: the checker leaves those reads unresolved.
#[test]
fn a_later_declaration_still_shadows_a_closure_read() {
    let (_, src, want) = SCRIPTS[2];
    assert_eq!(run(src, true), want);
}

/// A field read by the same node keeps giving the right answer as keys go
/// from the map (the position the node remembered is no longer the key's),
/// as other maps with other layouts come through it, and when the key is
/// gone altogether.
#[test]
fn field_cache_survives_key_removal() {
    let src = "\
roll m = {a: 1, b: 2, c: 3}
pull getc(x) { snuff x.c }
roll seen = []
for i in 0..4 {
  seen.push(getc(m))
  if i == 0 { m.remove(\"a\") }
  if i == 1 { m.remove(\"b\") }
  if i == 2 {
    m.a = 9
    m.c = 30
  }
}
exhale seen
exhale [getc({c: 1}), getc({a: 0, c: 2}), getc({a: 0, b: 0, c: 3}), getc({c: 4, a: 0})]
pull getd(x) { snuff x.d }
exhale [getd({d: 1, e: 2}), getd({e: 5, d: 6})]
pull maybe(x) { snuff x?.c }
roll g = {c: 1}
roll one = maybe(g)
g.remove(\"c\")
exhale [one, maybe(g)]
roll h = {p: 1, q: 2}
h.q += 5
h.remove(\"p\")
h.q += 5
h.p = 0
exhale h
";
    let want = "[3, 3, 3, 30]\n[1, 2, 3, 4]\n[1, 6]\n[1, null]\n{q: 12, p: 0}\n";
    assert_eq!(run(src, true), want);
    assert_eq!(run(src, false), want);
}

mod counting {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    thread_local! {
        pub static ALLOCS: Cell<u64> = const { Cell::new(0) };
    }

    pub struct Counting;

    // SAFETY: every call is passed straight to the system allocator.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
            System.alloc(l)
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            System.dealloc(p, l)
        }
        unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
            let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
            System.realloc(p, l, n)
        }
    }
}

#[global_allocator]
static COUNTING: counting::Counting = counting::Counting;

fn allocs_for(src: &str) -> u64 {
    let program = crate::syntax::parse(src).expect("parses");
    assert!(crate::check::check(&program, &[]).ok());
    let mut interp = Interp::new(Kernel::ephemeral(Mode::Run));
    interp.out = Box::new(Sink::default());
    interp.set_max_steps(0);
    let before = counting::ALLOCS.with(|c| c.get());
    interp.run_program(&program).expect("runs");
    counting::ALLOCS.with(|c| c.get()) - before
}

/// A call and a loop turn take their scope from the free list and the
/// arguments go straight into it: past the first few calls nothing is
/// allocated. Measured as the difference between a script that makes 2,000
/// more calls than another, counting allocations, never time. Before the
/// pool a call and a turn made nine; with it, three.
#[test]
fn no_allocation_per_call_in_steady_state() {
    let script = |n: u32| {
        format!(
            "pull f(a, b) {{\n  roll c = a + b\n  snuff c\n}}\nroll t = 0\nfor i in 0..{n} {{\n  t = f(t, i) % 1000\n}}\nexhale t\n"
        )
    };
    let (small, large) = (allocs_for(&script(100)), allocs_for(&script(2100)));
    let per_turn = (large as i64 - small as i64) as f64 / 2000.0;
    eprintln!("allocations per call + loop turn: {per_turn}");
    assert!(per_turn <= 0.01, "{per_turn} allocations per call and turn");
    // Recursion too: fib(16) makes 3,193 calls, fib(10) makes 177.
    let fib = |n: u32| {
        format!(
            "pull fib(n) {{\n  if n < 2 {{\n    snuff n\n  }}\n  snuff fib(n - 1) + fib(n - 2)\n}}\nexhale fib({n})\n"
        )
    };
    let (small, large) = (allocs_for(&fib(10)), allocs_for(&fib(16)));
    let per_call = (large as i64 - small as i64) as f64 / (3_193.0 - 177.0);
    eprintln!("allocations per fib call: {per_call}");
    assert!(per_call <= 0.01, "{per_call} allocations per fib call");
}

/// What `check` refuses, run without it (a compiled program built by an
/// older compiler, a hand-made one, `--no-check`): `check::resolve` places
/// what it can and leaves the rest to the lookup by name, so each script
/// prints and fails as it does with every name looked up: the same output,
/// the same error (code, message, position), no panic. A debug build checks
/// every resolved read against the lookup by name as it happens.
#[test]
fn a_refused_script_runs_as_it_does_unresolved() {
    let cases = [
        "exhale 1\nexhale x\nroll x = 1\n",
        "roll a = 1\nroll a = 2\nexhale a\n",
        "stick s = 1\nexhale s\ns = 2\nexhale s\n",
        "pull g() {\n  snuff z\n}\nroll z = 1\nexhale g()\nroll z = 2\nexhale g()\n",
        "pull f() {\n  snuff 1\n}\npull f() {\n  snuff 2\n}\nexhale f()\n",
        "roll y = 1\nburn {\n  exhale y\n} unburn {\n  exhale y\n}\nexhale y\n",
        "exhale fs.read_txt(\"x\")\n",
        "roll n = 0\nwhile n < 3 {\n  n += 1\n  roll k = n\n  roll k = 2\n}\n",
        "roll f = pack() => missing + 1\nroll missing = 1\nexhale f()\nexhale undefined_name\n",
        "if true {\n  roll inner = 1\n}\nexhale inner\n",
    ];
    let outcome = |src: &str, resolved: bool| {
        let program = crate::syntax::parse(src).expect("parses");
        if resolved {
            crate::check::resolve(&program);
        }
        let sink = Sink::default();
        let mut interp = Interp::new(Kernel::ephemeral(Mode::Run));
        interp.out = Box::new(sink.clone());
        interp.set_max_steps(0);
        let err = interp
            .run_program(&program)
            .err()
            .map(|d| (d.code, d.message, d.line, d.col));
        let out = String::from_utf8(sink.0.borrow().clone()).expect("utf-8");
        (out, err)
    };
    for src in cases {
        assert!(
            !crate::check::check(&crate::syntax::parse(src).unwrap(), &[]).ok(),
            "check accepts {src:?}"
        );
        assert_eq!(outcome(src, true), outcome(src, false), "{src:?}");
    }
}

/// A run that was not checked (a compiled program, `--interp`,
/// `--no-check`) resolves its names with `check::resolve`, each function's
/// body when it is first called: every resolved read and write is checked
/// against the lookup by name as it happens, none falls back, and the
/// output is the checked run's.
#[test]
fn resolved_without_check_reads_as_checked() {
    for (name, src, want) in SCRIPTS {
        let program = crate::syntax::parse(src).expect("parses");
        crate::check::resolve(&program);
        let sink = Sink::default();
        let mut interp = Interp::new(Kernel::ephemeral(Mode::Run));
        interp.out = Box::new(sink.clone());
        interp.set_max_steps(0);
        let (checked_before, missed_before) = (resolved_checked(), resolved_missed());
        interp.run_program(&program).expect("runs");
        assert!(
            resolved_checked() > checked_before,
            "{name}: nothing was resolved"
        );
        assert_eq!(resolved_missed(), missed_before, "{name}: a read fell back");
        let out = String::from_utf8(sink.0.borrow().clone()).expect("utf-8");
        assert_eq!(out, want, "{name}");
    }
}
