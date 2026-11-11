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

/// Scopes come from a pool with their name buffers: past the first few
/// calls, a call (or a loop turn) makes the scope's one `Rc` and the
/// argument list, and no slot array and no name `String`. Measured as the
/// difference between a script that makes 2,000 more calls than another.
/// Before the pool a call and a turn made nine.
#[test]
fn no_allocation_per_call_in_steady_state() {
    let script = |n: u32| {
        format!(
            "pull f(a, b) {{\n  roll c = a + b\n  snuff c\n}}\nroll t = 0\nfor i in 0..{n} {{\n  t = f(t, i) % 1000\n}}\nexhale t\n"
        )
    };
    let (small, large) = (allocs_for(&script(100)), allocs_for(&script(2100)));
    let per_turn = (large - small) as f64 / 2000.0;
    eprintln!("allocations per call + loop turn: {per_turn}");
    // Per turn: the loop's scope Rc, the call's scope Rc and its argument list.
    assert!(per_turn <= 3.0, "{per_turn} allocations per call and turn");
}
