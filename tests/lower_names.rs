// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! A name read before its declaration has run (#254). The interpreter looks
//! a name up as the script runs: a function that reads a name before the
//! name's statement has run finds the next one out, or none (E501). A
//! lowered function reads a slot, which has no such answer. So the lowering
//! takes a function that captures a not-yet-declared name only where the
//! function cannot run before the declaration, and declines every other
//! script; `cig run` then runs it on the tree-walker.
//!
//! `tests/engines.rs` holds the two engines to each other on whole
//! scripts. These are the shapes, one at a time: what must be declined (it
//! would run differently), what is declined to be safe (naming a function
//! is enough, it need not be called), and what must still be taken.

use cigscript::lower::cir::{lower_source, LowerError};

/// The refusal for `source`: the name it is about, the line the refusal
/// points at, and its text.
fn declined(source: &str) -> (String, u32, String) {
    match lower_source(source, "t.cig") {
        Err(LowerError::Refused { what, span }) => {
            let name = what
                .split('`')
                .nth(1)
                .expect("the refusal names a name")
                .to_string();
            (name, span.line, what)
        }
        Ok(_) => panic!("lowered, and should have been declined:\n{source}"),
        Err(other) => panic!("declined for another reason ({other:?}):\n{source}"),
    }
}

fn taken(source: &str) {
    if let Err(e) = lower_source(source, "t.cig") {
        panic!("should lower ({e:?}):\n{source}");
    }
}

#[test]
fn a_pull_called_before_a_name_it_reads_is_declared() {
    // The interpreter: E501, unknown name `LIMIT`.
    let (name, line, what) = declined(
        "main()\n\
         stick LIMIT = 10\n\
         pull main() { exhale LIMIT }\n",
    );
    assert_eq!((name.as_str(), line), ("LIMIT", 2));
    assert!(
        what.contains("after `main` can first run (line 1)"),
        "{what}"
    );
    assert!(what.contains("is looked up as the script runs"), "{what}");
}

#[test]
fn a_pull_called_before_a_chain_it_names_is_declared() {
    // The interpreter: E501, unknown name `lit`.
    let (name, line, _) = declined(
        "stick lite = pack() => 1\n\
         pull late() {\n\
         \x20 snuff lit\n\
         }\n\
         exhale late()\n\
         chain lit { lite }\n",
    );
    assert_eq!((name.as_str(), line), ("lit", 6));
}

#[test]
fn a_pull_that_reads_a_name_declared_again_further_in() {
    // The interpreter: the first call reads the outer `x`, the second the
    // inner one.
    let (name, line, _) = declined(
        "roll x = 1\n\
         pull outer() {\n\
         \x20 pull f() {\n\
         \x20   snuff x\n\
         \x20 }\n\
         \x20 roll before = f()\n\
         \x20 roll x = 2\n\
         \x20 snuff [before, f()]\n\
         }\n\
         exhale outer()\n",
    );
    assert_eq!((name.as_str(), line), ("x", 7));
}

#[test]
fn a_pull_that_writes_a_name_declared_again_further_in() {
    // The interpreter: the write lands on the outer `x`.
    let (name, line, _) = declined(
        "roll x = 1\n\
         pull outer() {\n\
         \x20 pull f() {\n\
         \x20   x = 10\n\
         \x20 }\n\
         \x20 f()\n\
         \x20 roll x = 2\n\
         \x20 snuff x\n\
         }\n\
         exhale outer()\n",
    );
    assert_eq!((name.as_str(), line), ("x", 7));
}

#[test]
fn a_pull_reached_through_another_pull_before_the_declaration() {
    // `first` is named before `V`; it calls `second`, which reads `V`.
    let (name, line, what) = declined(
        "stick started = first()\n\
         stick V = 5\n\
         pull first() { snuff second() }\n\
         pull second() { snuff V }\n",
    );
    assert_eq!((name.as_str(), line), ("V", 2));
    assert!(what.contains("after `first` can first run"), "{what}");
}

#[test]
fn a_declaration_whose_own_value_calls_a_pull_that_reads_it() {
    let (name, line, _) = declined(
        "roll x = 1\n\
         pull outer() {\n\
         \x20 pull f() { snuff x }\n\
         \x20 roll x = f() + 1\n\
         \x20 snuff x\n\
         }\n\
         exhale outer()\n",
    );
    assert_eq!((name.as_str(), line), ("x", 4));
}

#[test]
fn a_function_literal_made_before_the_declaration_it_reads() {
    // The interpreter: `f()` reads the outer `y` until the inner one is
    // declared.
    let (name, line, what) = declined(
        "roll y = 1\n\
         pull outer() {\n\
         \x20 roll f = pack() => y\n\
         \x20 roll a = f()\n\
         \x20 roll y = 2\n\
         \x20 snuff [a, f()]\n\
         }\n\
         exhale outer()\n",
    );
    assert_eq!((name.as_str(), line), ("y", 5));
    assert!(
        what.contains("after a function that reads it is made"),
        "{what}"
    );
}

#[test]
fn a_pull_in_an_inner_block_that_reads_a_name_the_outer_block_declares_later() {
    let (name, line, _) = declined(
        "roll x = 1\n\
         pull outer() {\n\
         \x20 if true {\n\
         \x20   pull f() { snuff x }\n\
         \x20   exhale f()\n\
         \x20 }\n\
         \x20 roll x = 2\n\
         \x20 snuff x\n\
         }\n\
         exhale outer()\n",
    );
    assert_eq!((name.as_str(), line), ("x", 7));
}

#[test]
fn naming_a_pull_is_enough_to_decline() {
    // Nothing calls `cmd_a` before `V` is declared, and both engines would
    // print 5. The lowering does not follow a value to where it is called:
    // a pull named before a declaration it reads is declined, and the
    // script runs on the tree-walker as written.
    let (name, line, _) = declined(
        "stick T = {\"a\": cmd_a}\n\
         stick V = 5\n\
         pull cmd_a() { snuff V }\n\
         exhale T[\"a\"]()\n",
    );
    assert_eq!((name.as_str(), line), ("V", 2));
}

#[test]
fn a_pull_used_after_what_it_reads_is_declared_is_taken() {
    taken(
        "stick LIMIT = 10\n\
         pull main() { exhale LIMIT }\n\
         main()\n",
    );
    // The pull is written first and hoisted; its use still comes after.
    taken(
        "pull outer() {\n\
         \x20 roll x = 2\n\
         \x20 pull f() { snuff x }\n\
         \x20 snuff f()\n\
         }\n\
         exhale outer()\n",
    );
    // A chain declared before the first use of the pull that names it.
    taken(
        "stick lite = pack() => 1\n\
         pull late() { snuff lit }\n\
         chain lit { lite }\n\
         exhale late()\n",
    );
}

#[test]
fn pulls_that_name_each_other_are_taken() {
    taken(
        "pull outer() {\n\
         \x20 pull a() { snuff b() }\n\
         \x20 pull b() { snuff 7 }\n\
         \x20 snuff a()\n\
         }\n\
         exhale outer()\n",
    );
    // Named early, and reading nothing the block declares later.
    taken(
        "exhale first()\n\
         stick V = 5\n\
         pull first() { snuff second() }\n\
         pull second() { snuff 7 }\n\
         exhale V\n",
    );
}

#[test]
fn a_function_literal_that_reads_what_is_already_declared_is_taken() {
    taken(
        "pull outer() {\n\
         \x20 roll base = pack(n) => n\n\
         \x20 roll twice = pack(n) => base(n) * 2\n\
         \x20 snuff twice(4)\n\
         }\n\
         exhale outer()\n",
    );
}

#[test]
fn a_loop_body_is_judged_like_any_block() {
    taken(
        "roll total = 0\n\
         for x in [1, 2, 3] {\n\
         \x20 pull add() { total = total + x }\n\
         \x20 add()\n\
         }\n\
         exhale total\n",
    );
    let (name, line, _) = declined(
        "roll step = 1\n\
         for x in [1, 2, 3] {\n\
         \x20 pull add() { snuff x + step }\n\
         \x20 exhale add()\n\
         \x20 roll step = 10\n\
         }\n",
    );
    assert_eq!((name.as_str(), line), ("step", 5));
}
