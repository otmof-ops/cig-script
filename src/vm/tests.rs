// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The VM core against hand-built modules, and against the tree-walker.

use super::*;
use crate::burn::{Kernel, Mode};
use crate::diagnostics::Kind;
use crate::syntax::ast::Stmt;
use cig_bytecode::{
    verify_module, wide_prefix, BurnRegion, HandlerEntry, Instr, Module, UpvalDesc, UpvalKind,
    RESERVED_U32,
};
use indexmap::IndexMap;

// ----- builders --------------------------------------------------------------------

fn interp() -> Interp {
    let mut i = Interp::new(Kernel::ephemeral(Mode::Run));
    i.out = Box::new(std::io::sink());
    i.err = Box::new(std::io::sink());
    i.set_max_steps(0);
    i
}

/// A top-level function with `regs` registers and `code`, and a line row
/// at every pc: line `pc + 1`, column `pc + 2`, so a position names its pc.
fn main_fn(regs: u32, code: Vec<Instr>) -> BcFunction {
    let mut f = BcFunction::empty_main();
    f.reg_count = regs;
    f.span_base = SpanBase {
        byte_offset: 0,
        line: 1,
        col: 1,
    };
    f.lines = cig_bytecode::first_word_pcs(&code)
        .map(|pcs| {
            pcs.iter()
                .map(|pc| LineEntry {
                    pc,
                    span_off: pc * 10,
                    span_len: 3,
                    line_off: pc,
                    col: pc + 2,
                })
                .collect()
        })
        .unwrap_or_default();
    f.instrs = code;
    f
}

/// A child function (a pull named `name`), with no line rows.
fn child_fn(name: &str, arity: u16, regs: u32, code: Vec<Instr>) -> BcFunction {
    let mut f = BcFunction::empty_main();
    f.name = name.to_string();
    f.kind = FuncKind::Pull;
    f.arity = arity;
    f.reg_count = regs;
    f.instrs = code;
    f
}

fn module(main: BcFunction) -> Module {
    Module {
        source_name: "t.cig".to_string(),
        main,
    }
}

fn program(m: &Module) -> Rc<Program> {
    let v = verify_module(m).unwrap_or_else(|e| panic!("does not verify: {e}"));
    load(v).unwrap_or_else(|e| panic!("does not load: {}", e.message))
}

fn exec(m: &Module) -> Result<Value, Diagnostic> {
    run(&mut interp(), &program(m))
}

fn exec_with(m: &Module, args: Vec<Value>) -> Result<Value, Diagnostic> {
    run_function(&mut interp(), &program(m), 0, args)
}

fn ok(m: &Module) -> String {
    match exec(m) {
        Ok(v) => v.repr(),
        Err(e) => panic!("{} {}", e.code, e.message),
    }
}

fn err(m: &Module) -> Diagnostic {
    match exec(m) {
        Ok(v) => panic!("ran to {}", v.repr()),
        Err(e) => e,
    }
}

/// Where an error was raised, as `main_fn`'s line rows name a pc.
fn at_pc(d: &Diagnostic) -> Option<u32> {
    d.line.map(|l| l - 1)
}

fn abc(op: Op, a: u8, b: u8, c: u8) -> Instr {
    Instr::abc(op, a, b, c)
}

fn ret(r: u8) -> Instr {
    abc(Op::RET, r, 0, 0)
}

fn catalogue_index(name: &str) -> usize {
    crate::stdlib::builtin_catalogue()
        .iter()
        .position(|b| b.name == name)
        .unwrap_or_else(|| panic!("no builtin {name}"))
}

// ----- moves and constants ---------------------------------------------------------

#[test]
fn moves_and_constants_load_what_they_say() {
    let mut f = main_fn(
        4,
        vec![
            Instr::abx(Op::LOADK, 0, 1),
            abc(Op::MOV, 1, 0, 0),
            abc(Op::NOP, 0, 0, 0),
            ret(1),
        ],
    );
    f.constants = vec![Constant::Int(5), Constant::Str("é漢".into())];
    assert_eq!(ok(&module(f)), "\"é漢\"");

    let f = main_fn(2, vec![abc(Op::LOADNULL, 0, 0, 0), ret(0)]);
    assert_eq!(ok(&module(f)), "null");
    for (b, want) in [(0u8, "false"), (1, "true"), (7, "true")] {
        let f = main_fn(2, vec![abc(Op::LOADBOOL, 0, b, 0), ret(0)]);
        assert_eq!(ok(&module(f)), want);
    }
    for n in [-5i16, 0, i16::MAX, i16::MIN] {
        let f = main_fn(2, vec![Instr::asbx(Op::LOADINT, 0, n), ret(0)]);
        assert_eq!(ok(&module(f)), n.to_string());
    }
    let mut f = main_fn(2, vec![Instr::abx(Op::LOADK, 0, 0), ret(0)]);
    f.constants = vec![Constant::Float(2.0)];
    assert_eq!(ok(&module(f.clone())), "2.0");
    f.constants = vec![Constant::Bool(true)];
    assert_eq!(ok(&module(f)), "true");
}

#[test]
fn wide_moves_and_constants_reach_past_eight_and_sixteen_bits() {
    // mov R(300), R(0) then ret R(300): A is 16 bits through WIDE.
    let f = main_fn(
        301,
        vec![
            Instr::asbx(Op::LOADINT, 0, 9),
            wide_prefix(1, 0),
            abc(Op::MOV, 44, 0, 0), // A = 1 << 8 | 44 = 300
            wide_prefix(1, 0),
            ret(44),
        ],
    );
    assert_eq!(ok(&module(f)), "9");
    // mov R(0), R(258): B is 16 bits.
    let f = main_fn(
        260,
        vec![
            wide_prefix(1, 0),
            Instr::asbx(Op::LOADINT, 2, 4), // R(258) = 4
            wide_prefix(0, 0x0001),
            abc(Op::MOV, 0, 2, 0), // B = 1 << 8 | 2 = 258
            ret(0),
        ],
    );
    assert_eq!(ok(&module(f)), "4");
    // loadk with a 32-bit Bx: constant 65_541.
    let mut f = main_fn(
        1,
        vec![wide_prefix(0, 1), Instr::abx(Op::LOADK, 0, 5), ret(0)],
    );
    f.constants = (0..65_542).map(Constant::Int).collect();
    assert_eq!(ok(&module(f)), "65541");
    // loadint with a 32-bit sBx: -100_000.
    let v: i32 = -100_000;
    let raw = v as u32;
    let f = main_fn(
        1,
        vec![
            wide_prefix(0, (raw >> 16) as u16),
            Instr::abx(Op::LOADINT, 0, (raw & 0xFFFF) as u16),
            ret(0),
        ],
    );
    assert_eq!(ok(&module(f)), "-100000");
}

// ----- arithmetic, comparison, logic -----------------------------------------------

/// `op R2, R0, R1; ret R2`, with the operands as the entry's arguments.
fn binop_module(op: Op) -> Module {
    let mut f = main_fn(3, vec![abc(op, 2, 0, 1), ret(2)]);
    f.arity = 2;
    module(f)
}

/// `opk R1, R0, K0; ret R1`.
fn binopk_module(op: Op, k: Constant) -> Module {
    let mut f = main_fn(2, vec![abc(op, 1, 0, 0), ret(1)]);
    f.arity = 1;
    f.constants = vec![k];
    module(f)
}

#[test]
fn arithmetic_gives_the_interpreters_values_and_errors() {
    let run2 = |op, a, b| exec_with(&binop_module(op), vec![a, b]);
    assert_eq!(
        run2(Op::ADD, Value::Int(2), Value::Int(3)).unwrap().repr(),
        "5"
    );
    assert_eq!(
        run2(Op::SUB, Value::Float(2.5), Value::Int(3))
            .unwrap()
            .repr(),
        "-0.5"
    );
    assert_eq!(
        run2(Op::MUL, Value::str("ab"), Value::Int(3))
            .unwrap()
            .repr(),
        "\"ababab\""
    );
    assert_eq!(
        run2(Op::DIV, Value::Int(7), Value::Int(2)).unwrap().repr(),
        "3.5"
    );
    assert_eq!(
        run2(Op::MOD, Value::Int(-7), Value::Int(3)).unwrap().repr(),
        "-1"
    );
    let e = run2(Op::ADD, Value::Int(i64::MAX), Value::Int(1))
        .err()
        .expect("an error");
    assert_eq!(
        (e.code, e.message.as_str()),
        ("E503", "integer overflow in `+`")
    );
    assert_eq!(at_pc(&e), Some(0));
    assert_eq!(e.col, Some(2));
    let e = run2(Op::DIV, Value::Int(1), Value::Int(0))
        .err()
        .expect("an error");
    assert_eq!((e.code, e.message.as_str()), ("E502", "division by zero"));
    let e = run2(Op::MOD, Value::Float(1.0), Value::Float(0.0))
        .err()
        .expect("an error");
    assert_eq!((e.code, e.message.as_str()), ("E502", "modulo by zero"));
    let e = run2(Op::SUB, Value::str("a"), Value::Int(1))
        .err()
        .expect("an error");
    assert_eq!(
        (e.code, e.message.as_str()),
        ("E403", "cannot subtract string and int")
    );
    // The span is the line row's: offset 0, three bytes, line 1, column 2.
    assert_eq!(e.span, Some(Span::new(0, 3, 1, 2)));

    let e = exec_with(
        &binopk_module(Op::DIVK, Constant::Int(-1)),
        vec![Value::Int(i64::MIN)],
    )
    .err()
    .expect("an error");
    assert_eq!(
        (e.code, e.message.as_str()),
        ("E503", "integer overflow in `/`")
    );
    for (op, k, want) in [
        (Op::ADDK, Constant::Int(1), "11"),
        (Op::SUBK, Constant::Float(0.5), "9.5"),
        (Op::MULK, Constant::Int(-2), "-20"),
        (Op::DIVK, Constant::Int(5), "2"),
        (Op::MODK, Constant::Int(3), "1"),
        (Op::POWK, Constant::Int(2), "100"),
        (Op::EQK, Constant::Float(10.0), "true"),
        (Op::LTK, Constant::Int(10), "false"),
        (Op::LEK, Constant::Int(10), "true"),
    ] {
        let got = exec_with(&binopk_module(op, k), vec![Value::Int(10)]).unwrap();
        assert_eq!(got.repr(), want, "{op}");
    }
}

#[test]
fn pow_is_math_pow() {
    let run2 = |a, b| exec_with(&binop_module(Op::POW), vec![a, b]);
    assert_eq!(run2(Value::Int(2), Value::Int(10)).unwrap().repr(), "1024");
    assert_eq!(run2(Value::Int(2), Value::Int(-1)).unwrap().repr(), "0.5");
    let e = run2(Value::Int(10), Value::Int(40))
        .err()
        .expect("an error");
    assert_eq!(e.message, "math.pow: integer overflow");
    assert_eq!(at_pc(&e), Some(0));
    let e = run2(Value::str("x"), Value::Int(1))
        .err()
        .expect("an error");
    assert_eq!(e.code, "E402");
}

#[test]
fn negation_and_not() {
    let mut f = main_fn(2, vec![abc(Op::NEG, 1, 0, 0), ret(1)]);
    f.arity = 1;
    let m = module(f);
    assert_eq!(exec_with(&m, vec![Value::Int(4)]).unwrap().repr(), "-4");
    assert_eq!(
        exec_with(&m, vec![Value::Float(0.0)]).unwrap().repr(),
        "-0.0"
    );
    let e = exec_with(&m, vec![Value::Int(i64::MIN)])
        .err()
        .expect("an error");
    assert_eq!(
        (e.code, e.message.as_str()),
        ("E503", "integer overflow in negation")
    );
    let e = exec_with(&m, vec![Value::str("a")])
        .err()
        .expect("an error");
    assert_eq!(
        (e.code, e.message.as_str()),
        ("E403", "cannot negate string")
    );
    assert_eq!(at_pc(&e), Some(0));

    let mut f = main_fn(2, vec![abc(Op::NOT, 1, 0, 0), ret(1)]);
    f.arity = 1;
    let m = module(f);
    for (v, want) in [
        (Value::Null, "true"),
        (Value::Int(0), "true"),
        (Value::str(""), "true"),
        (Value::str("x"), "false"),
        (Value::list(vec![]), "true"),
    ] {
        assert_eq!(exec_with(&m, vec![v]).unwrap().repr(), want);
    }
}

#[test]
fn comparisons_and_their_errors() {
    let run2 = |op, a, b| exec_with(&binop_module(op), vec![a, b]);
    assert_eq!(
        run2(Op::EQ, Value::Int(1), Value::Float(1.0))
            .unwrap()
            .repr(),
        "true"
    );
    assert_eq!(
        run2(Op::LT, Value::str("a"), Value::str("b"))
            .unwrap()
            .repr(),
        "true"
    );
    assert_eq!(
        run2(Op::LE, Value::Int(2), Value::Int(1)).unwrap().repr(),
        "false"
    );
    let e = run2(Op::LT, Value::Float(f64::NAN), Value::Int(1))
        .err()
        .expect("an error");
    assert_eq!((e.code, e.message.as_str()), ("E500", "cannot order a NaN"));
    let e = run2(Op::LE, Value::Int(1), Value::str("1"))
        .err()
        .expect("an error");
    assert_eq!(
        (e.code, e.message.as_str()),
        ("E403", "cannot compare int and string")
    );
    assert_eq!(at_pc(&e), Some(0));
}

#[test]
fn wide_arithmetic_names_registers_past_255() {
    // add R(300), R(256), R(257)
    let f = main_fn(
        301,
        vec![
            wide_prefix(1, 0),
            Instr::asbx(Op::LOADINT, 0, 40), // R(256)
            wide_prefix(1, 0),
            Instr::asbx(Op::LOADINT, 1, 2), // R(257)
            wide_prefix(1, (1 << 8) | 1),
            abc(Op::ADD, 44, 0, 1),
            wide_prefix(1, 0),
            ret(44),
        ],
    );
    assert_eq!(ok(&module(f)), "42");
}

// ----- control flow ----------------------------------------------------------------

/// `<jump op> ...; loadint R2, 0; ret R2; loadint R2, 1; ret R2`: 1 when
/// the jump is taken.
fn jump_module(first: Instr) -> Module {
    let mut f = main_fn(
        3,
        vec![
            first,
            Instr::asbx(Op::LOADINT, 2, 0),
            ret(2),
            Instr::asbx(Op::LOADINT, 2, 1),
            ret(2),
        ],
    );
    f.arity = 2;
    module(f)
}

#[test]
fn fused_compare_and_jump() {
    // offset 2: target = 0 + 1 + 2 = 3.
    for (op, a, b, taken) in [
        (Op::EQJ, Value::Int(1), Value::Float(1.0), true),
        (Op::EQJ, Value::Int(1), Value::Int(2), false),
        (Op::NEJ, Value::Int(1), Value::Int(2), true),
        (Op::NEJ, Value::str("a"), Value::str("a"), false),
        (Op::LTJ, Value::Int(1), Value::Int(2), true),
        (Op::LTJ, Value::Int(2), Value::Int(2), false),
        (Op::LEJ, Value::Int(2), Value::Int(2), true),
        (Op::LEJ, Value::Float(3.0), Value::Int(2), false),
    ] {
        let m = jump_module(abc(op, 0, 1, 2));
        let got = exec_with(&m, vec![a, b]).unwrap().repr();
        assert_eq!(got, if taken { "1" } else { "0" }, "{op}");
    }
    let m = jump_module(abc(Op::LTJ, 0, 1, 2));
    let e = exec_with(&m, vec![Value::Null, Value::Int(1)])
        .err()
        .expect("an error");
    assert_eq!(
        (e.code, e.message.as_str()),
        ("E403", "cannot compare null and int")
    );
    assert_eq!(at_pc(&e), Some(0));
}

#[test]
fn jumps_test_testn_testset() {
    let m = jump_module(Instr::asbx(Op::JMP, 0, 2));
    assert_eq!(
        exec_with(&m, vec![Value::Null, Value::Null])
            .unwrap()
            .repr(),
        "1"
    );
    let m = jump_module(Instr::asbx(Op::TEST, 0, 2));
    assert_eq!(
        exec_with(&m, vec![Value::Int(3), Value::Null])
            .unwrap()
            .repr(),
        "1"
    );
    assert_eq!(
        exec_with(&m, vec![Value::Int(0), Value::Null])
            .unwrap()
            .repr(),
        "0"
    );
    let m = jump_module(Instr::asbx(Op::TESTN, 0, 2));
    assert_eq!(
        exec_with(&m, vec![Value::Int(3), Value::Null])
            .unwrap()
            .repr(),
        "0"
    );
    assert_eq!(
        exec_with(&m, vec![Value::str(""), Value::Null])
            .unwrap()
            .repr(),
        "1"
    );

    // testset R2, R0, +1: R2 = R0 when truthy, else jump past `ret R2`.
    let mut f = main_fn(
        3,
        vec![
            abc(Op::TESTSET, 2, 0, 1),
            ret(2),
            Instr::asbx(Op::LOADINT, 2, -1),
            ret(2),
        ],
    );
    f.arity = 2;
    let m = module(f);
    assert_eq!(
        exec_with(&m, vec![Value::str("x"), Value::Null])
            .unwrap()
            .repr(),
        "\"x\""
    );
    assert_eq!(
        exec_with(&m, vec![Value::Null, Value::Null])
            .unwrap()
            .repr(),
        "-1"
    );
}

#[test]
fn a_backward_loop_counts_down() {
    // R0 = 5; R1 = 0; top: testn R0 -> out; R1 += 2; R0 -= 1; loop top; out: ret R1
    let mut f = main_fn(
        2,
        vec![
            Instr::asbx(Op::LOADINT, 0, 5),
            Instr::asbx(Op::LOADINT, 1, 0),
            Instr::asbx(Op::TESTN, 0, 3), // pc 2 -> 6
            abc(Op::ADDK, 1, 1, 0),
            abc(Op::SUBK, 0, 0, 1),
            Instr::asbx(Op::LOOP, 0, -4), // pc 5 -> 2
            ret(1),
        ],
    );
    f.constants = vec![Constant::Int(2), Constant::Int(1)];
    assert_eq!(ok(&module(f)), "10");
}

#[test]
fn wide_jumps_measure_from_the_first_word() {
    // wide jmp at pc 0 with offset 3: target 0 + 1 + 3 = 4.
    let f = main_fn(
        1,
        vec![
            wide_prefix(0, 0),
            Instr::asbx(Op::JMP, 0, 3),
            Instr::asbx(Op::LOADINT, 0, 0),
            ret(0),
            Instr::asbx(Op::LOADINT, 0, 7),
            ret(0),
        ],
    );
    assert_eq!(ok(&module(f)), "7");
    // A wide fused jump: C is 16 bits signed.
    let mut f = main_fn(
        2,
        vec![
            wide_prefix(0, 0),
            abc(Op::EQJ, 0, 1, 3), // pc 0 -> 4
            Instr::asbx(Op::LOADINT, 0, 0),
            ret(0),
            Instr::asbx(Op::LOADINT, 0, 7),
            ret(0),
        ],
    );
    f.arity = 2;
    let got = exec_with(&module(f), vec![Value::Int(1), Value::Int(1)]).unwrap();
    assert_eq!(got.repr(), "7");
}

// ----- calls -------------------------------------------------------------------------

/// `pull add(a, b) { snuff a + b }`.
fn add_fn() -> BcFunction {
    child_fn("add", 2, 3, vec![abc(Op::ADD, 2, 0, 1), ret(2)])
}

#[test]
fn call_and_ret_pass_arguments_and_the_result() {
    // R0 = closure add; R1 = 2; R2 = 40; call R0, 2, 0; ret R0
    let mut f = main_fn(
        3,
        vec![
            Instr::abx(Op::CLOSURE, 0, 0),
            Instr::asbx(Op::LOADINT, 1, 2),
            Instr::asbx(Op::LOADINT, 2, 40),
            abc(Op::CALL, 0, 2, 0),
            ret(0),
        ],
    );
    f.children = vec![add_fn()];
    assert_eq!(ok(&module(f)), "42");
}

#[test]
fn a_call_row_with_more_results_nulls_the_rest() {
    // call R0, 2, 2 writes R0 and nulls R1 and R2.
    let mut f = main_fn(
        3,
        vec![
            Instr::abx(Op::CLOSURE, 0, 0),
            Instr::asbx(Op::LOADINT, 1, 2),
            Instr::asbx(Op::LOADINT, 2, 40),
            abc(Op::CALL, 0, 2, 2),
            ret(2),
        ],
    );
    f.children = vec![add_fn()];
    assert_eq!(ok(&module(f)), "null");
}

#[test]
fn a_closure_is_a_pull_value() {
    let mut f = main_fn(1, vec![Instr::abx(Op::CLOSURE, 0, 0), ret(0)]);
    f.children = vec![add_fn()];
    let v = exec(&module(f)).unwrap();
    assert_eq!(v.repr(), "<pull add>");
    assert_eq!(v.type_name(), "function");
    let mut lambda = add_fn();
    lambda.kind = FuncKind::Lambda;
    lambda.name = String::new();
    let mut f = main_fn(1, vec![Instr::abx(Op::CLOSURE, 0, 0), ret(0)]);
    f.children = vec![lambda];
    assert_eq!(ok(&module(f)), "<pack>");
}

#[test]
fn call_errors_are_the_interpreters() {
    // Wrong arity: E401 with the pull's name.
    let mut f = main_fn(
        3,
        vec![
            Instr::abx(Op::CLOSURE, 0, 0),
            Instr::asbx(Op::LOADINT, 1, 2),
            abc(Op::CALL, 0, 1, 0),
            ret(0),
        ],
    );
    f.children = vec![add_fn()];
    let e = err(&module(f));
    assert_eq!(
        (e.code, e.message.as_str()),
        ("E401", "add expects 2 arguments, got 1")
    );
    assert_eq!(at_pc(&e), Some(2));

    // Calling an int: E404.
    let f = main_fn(
        2,
        vec![
            Instr::asbx(Op::LOADINT, 0, 3),
            abc(Op::CALL, 0, 0, 0),
            ret(0),
        ],
    );
    let e = err(&module(f));
    assert_eq!((e.code, e.message.as_str()), ("E404", "cannot call int"));
    assert_eq!(at_pc(&e), Some(1));

    // An error inside the callee carries the callee's position.
    let mut child = add_fn();
    child.span_base = SpanBase {
        byte_offset: 100,
        line: 20,
        col: 1,
    };
    child.lines = vec![LineEntry {
        pc: 0,
        span_off: 4,
        span_len: 5,
        line_off: 1,
        col: 12,
    }];
    let mut f = main_fn(
        3,
        vec![
            Instr::abx(Op::CLOSURE, 0, 0),
            Instr::asbx(Op::LOADINT, 1, 2),
            abc(Op::LOADNULL, 2, 0, 0),
            abc(Op::CALL, 0, 2, 0),
            ret(0),
        ],
    );
    f.children = vec![child];
    let e = err(&module(f));
    assert_eq!(e.code, "E403");
    assert_eq!(e.span, Some(Span::new(104, 109, 21, 12)));
}

#[test]
fn a_builtin_value_is_called_through_the_interpreter() {
    // call R0 (the builtin passed in), 1 arg.
    let mut f = main_fn(2, vec![abc(Op::CALL, 0, 1, 0), ret(0)]);
    f.arity = 2;
    let len = crate::stdlib::builtin_catalogue()[catalogue_index("len")];
    let got = exec_with(
        &module(f),
        vec![Value::Builtin(Rc::new(*len)), Value::str("abcd")],
    )
    .unwrap();
    assert_eq!(got.repr(), "4");
}

/// `pull f(g, n) { if n == 0 { snuff 0 }; snuff g(g, n - 1) + 1 }`, as a
/// plain call, or a tail call of `g(g, n - 1)` when `tail` (then it
/// returns 0 at the bottom).
fn recursive_fn(tail: bool) -> BcFunction {
    let mut code = vec![
        Instr::asbx(Op::TESTN, 1, 4), // n == 0 -> pc 5
        abc(Op::MOV, 2, 0, 0),        // callee g
        abc(Op::MOV, 3, 0, 0),        // arg g
        abc(Op::SUBK, 4, 1, 0),       // arg n - 1
    ];
    if tail {
        code.push(abc(Op::TAILCALL, 2, 2, 0));
    } else {
        code.push(Instr::asbx(Op::JMP, 0, 1)); // -> pc 6
    }
    code.push(ret(1)); // pc 5: n is 0
    if !tail {
        code.push(abc(Op::CALL, 2, 2, 0)); // pc 6
        code.push(abc(Op::ADDK, 2, 2, 1));
        code.push(ret(2));
    }
    let mut f = child_fn("f", 2, 5, code);
    f.constants = vec![Constant::Int(1), Constant::Int(1)];
    f
}

fn recursion_module(tail: bool, n: i16) -> Module {
    let mut f = main_fn(
        3,
        vec![
            Instr::abx(Op::CLOSURE, 0, 0),
            abc(Op::MOV, 1, 0, 0),
            Instr::asbx(Op::LOADINT, 2, n),
            abc(Op::CALL, 0, 2, 0),
            ret(0),
        ],
    );
    f.children = vec![recursive_fn(tail)];
    module(f)
}

#[test]
fn recursion_runs_and_stops_at_the_interpreters_depth() {
    assert_eq!(ok(&recursion_module(false, 100)), "100");
    let e = err(&recursion_module(false, 5000));
    assert_eq!(e.code, "E506");
    assert_eq!(
        e.message,
        "call depth exceeded 4000; is there unbounded recursion?"
    );
}

#[test]
fn a_tail_call_returns_and_counts_depth_as_a_call() {
    assert_eq!(ok(&recursion_module(true, 100)), "0");
    // The tree-walker nests every one of these calls, so the VM's tail
    // calls count toward the same limit.
    let e = err(&recursion_module(true, 5000));
    assert_eq!(e.code, "E506");
    // And the depth is given back when the run ends.
    let mut i = interp();
    let _ = run(&mut i, &program(&recursion_module(true, 5000)));
    assert_eq!(i.call_depth_for_tests(), 0);
    assert_eq!(i.loop_depth, 0);
}

#[test]
fn a_tail_call_of_a_builtin_returns_its_value() {
    let mut f = main_fn(2, vec![abc(Op::TAILCALL, 0, 1, 0)]);
    f.arity = 2;
    let len = crate::stdlib::builtin_catalogue()[catalogue_index("len")];
    let got = exec_with(
        &module(f),
        vec![Value::Builtin(Rc::new(*len)), Value::str("ab")],
    )
    .unwrap();
    assert_eq!(got.repr(), "2");
}

#[test]
fn callb_calls_the_catalogue() {
    let len = catalogue_index("len") as u8;
    let mut f = main_fn(
        2,
        vec![
            Instr::abx(Op::LOADK, 1, 0),
            abc(Op::CALLB, 0, len, 1),
            ret(0),
        ],
    );
    f.constants = vec![Constant::Str("é漢x".into())];
    assert_eq!(ok(&module(f.clone())), "3");
    // Its arity error is the builtin's own.
    f.instrs[1] = abc(Op::CALLB, 0, len, 0);
    let e = err(&module(f));
    assert_eq!(
        (e.code, e.message.as_str()),
        ("E401", "len expects 1 argument, got 0")
    );
    assert_eq!(at_pc(&e), Some(1));
    // A wide index reaches the end of the catalogue.
    let last = crate::stdlib::builtin_catalogue().len() - 1;
    let f = main_fn(
        2,
        vec![
            wide_prefix(0, (last >> 8) as u16),
            abc(Op::CALLB, 0, (last & 0xFF) as u8, 0),
            ret(0),
        ],
    );
    let _ = exec(&module(f));
    // A world-changing builtin outside a burn: E701, as in the tree-walker.
    let write = catalogue_index("fs.write_text");
    let mut f = main_fn(
        3,
        vec![
            Instr::abx(Op::LOADK, 1, 0),
            Instr::abx(Op::LOADK, 2, 0),
            wide_prefix(0, (write >> 8) as u16),
            abc(Op::CALLB, 0, (write & 0xFF) as u8, 2),
            ret(0),
        ],
    );
    f.constants = vec![Constant::Str("x".into())];
    let e = err(&module(f));
    assert_eq!(e.code, "E701");
    assert_eq!(at_pc(&e), Some(2));
}

// ----- the shared operators -----------------------------------------------------------

fn operand_table() -> Vec<Value> {
    let mut m = IndexMap::new();
    m.insert("a".to_string(), Value::Int(1));
    vec![
        Value::Null,
        Value::Bool(true),
        Value::Bool(false),
        Value::Int(0),
        Value::Int(1),
        Value::Int(-1),
        Value::Int(7),
        Value::Int(i64::MAX),
        Value::Int(i64::MIN),
        Value::Float(0.0),
        Value::Float(-0.0),
        Value::Float(2.5),
        Value::Float(f64::NAN),
        Value::Float(f64::INFINITY),
        Value::Float(f64::NEG_INFINITY),
        Value::str(""),
        Value::str("abc"),
        Value::str("é漢🚬"),
        Value::list(vec![]),
        Value::list(vec![Value::Int(1), Value::str("x")]),
        Value::map(m),
        Value::map(IndexMap::new()),
    ]
}

fn outcome(r: Result<Value, Diagnostic>) -> String {
    match r {
        Ok(v) => format!("{} {}", v.type_name(), v.repr()),
        Err(e) => format!("{} {:?} {} {:?}", e.code, e.kind, e.message, e.span),
    }
}

#[test]
fn the_vm_and_the_interpreter_agree_on_every_operator() {
    let span = Span::new(0, 3, 1, 2); // the line row of pc 0
    let table = operand_table();
    let binops = [
        (Op::ADD, BinOp::Add),
        (Op::SUB, BinOp::Sub),
        (Op::MUL, BinOp::Mul),
        (Op::DIV, BinOp::Div),
        (Op::MOD, BinOp::Rem),
        (Op::EQ, BinOp::Eq),
        (Op::LT, BinOp::Lt),
        (Op::LE, BinOp::Le),
    ];
    let mut compared = 0;
    for (op, bin) in binops {
        let m = binop_module(op);
        for l in &table {
            for r in &table {
                let vm = outcome(exec_with(&m, vec![l.clone(), r.clone()]));
                let shared = outcome(binary(bin, l.clone(), r.clone(), span));
                assert_eq!(vm, shared, "{op} {} {}", l.repr(), r.repr());
                compared += 1;
            }
        }
    }
    // The fused jumps: taken exactly when the shared operator says true,
    // and its error when it errs.
    for (op, bin) in [
        (Op::EQJ, BinOp::Eq),
        (Op::NEJ, BinOp::Ne),
        (Op::LTJ, BinOp::Lt),
        (Op::LEJ, BinOp::Le),
    ] {
        let m = jump_module(abc(op, 0, 1, 2));
        for l in &table {
            for r in &table {
                let vm = outcome(exec_with(&m, vec![l.clone(), r.clone()]));
                let shared = outcome(
                    binary(bin, l.clone(), r.clone(), span)
                        .map(|v| Value::Int(i64::from(v.truthy()))),
                );
                assert_eq!(vm, shared, "{op} {} {}", l.repr(), r.repr());
                compared += 1;
            }
        }
    }
    // Unary: neg is the shared negate, not is truthiness.
    let mut neg = main_fn(2, vec![abc(Op::NEG, 1, 0, 0), ret(1)]);
    neg.arity = 1;
    let mut not = main_fn(2, vec![abc(Op::NOT, 1, 0, 0), ret(1)]);
    not.arity = 1;
    let (neg, not) = (module(neg), module(not));
    for v in &table {
        let vm = outcome(exec_with(&neg, vec![v.clone()]));
        assert_eq!(vm, outcome(negate(v.clone(), span)), "neg {}", v.repr());
        let vm = outcome(exec_with(&not, vec![v.clone()]));
        assert_eq!(
            vm,
            outcome(Ok(Value::Bool(!v.truthy()))),
            "not {}",
            v.repr()
        );
        compared += 2;
    }
    // pow is math.pow, called as the tree-walker calls it.
    let pow = crate::stdlib::builtin_catalogue()[catalogue_index("math.pow")];
    let m = binop_module(Op::POW);
    for l in &table {
        for r in &table {
            let vm = outcome(exec_with(&m, vec![l.clone(), r.clone()]));
            let shared = outcome(interp().call_builtin(pow, &[l.clone(), r.clone()], span));
            assert_eq!(vm, shared, "pow {} {}", l.repr(), r.repr());
            compared += 1;
        }
    }
    // The constant forms, over every operand a constant can hold.
    let consts = [
        Constant::Bool(true),
        Constant::Int(0),
        Constant::Int(-1),
        Constant::Int(i64::MAX),
        Constant::Int(i64::MIN),
        Constant::Float(-0.0),
        Constant::Float(f64::NAN),
        Constant::Float(f64::INFINITY),
        Constant::Str(String::new()),
        Constant::Str("é漢".into()),
    ];
    let kops = [
        (Op::ADDK, BinOp::Add),
        (Op::SUBK, BinOp::Sub),
        (Op::MULK, BinOp::Mul),
        (Op::DIVK, BinOp::Div),
        (Op::MODK, BinOp::Rem),
        (Op::EQK, BinOp::Eq),
        (Op::LTK, BinOp::Lt),
        (Op::LEK, BinOp::Le),
    ];
    for (op, bin) in kops {
        for k in &consts {
            let m = binopk_module(op, k.clone());
            for l in &table {
                let vm = outcome(exec_with(&m, vec![l.clone()]));
                let shared = outcome(binary(bin, l.clone(), constant(k), span));
                assert_eq!(vm, shared, "{op} {} {k:?}", l.repr());
                compared += 1;
            }
        }
    }
    assert!(compared >= 8_000, "{compared}");
}

// ----- the step budget ------------------------------------------------------------------

fn row(pc: u32, s: Span) -> LineEntry {
    LineEntry {
        pc,
        span_off: s.start as u32,
        span_len: (s.end - s.start) as u32,
        line_off: s.line - 1,
        col: s.col,
    }
}

fn steps(nest: u8, n: u16) -> Instr {
    Instr::abx(Op::STEPS, nest, n)
}

/// Run `src` on the tree-walker under `budget`: where it stopped (or
/// `None` when it finished) and the steps it counted.
fn walker_stop(src: &str, budget: u64) -> (Option<Diagnostic>, u64) {
    let program = crate::syntax::parse(src).expect("parses");
    let report = crate::check::check(&program, &[]);
    assert!(report.ok(), "{:?}", report.errors);
    let mut i = interp();
    i.set_max_steps(budget);
    let r = i.run_program(&program);
    (r.err(), i.steps)
}

fn vm_stop(m: &Module, budget: u64) -> (Option<Diagnostic>, u64) {
    let mut i = interp();
    i.set_max_steps(budget);
    let r = run(&mut i, &program(m));
    assert_eq!(i.loop_depth, 0, "the loop depth is given back");
    assert_eq!(i.call_depth_for_tests(), 0, "the call depth is given back");
    (r.err(), i.steps)
}

fn same_stop(src: &str, m: &Module) -> u64 {
    let (done, total) = walker_stop(src, 0);
    assert!(done.is_none());
    let (vm_done, vm_total) = vm_stop(m, 0);
    assert!(vm_done.is_none(), "{:?}", vm_done.map(|d| d.message));
    assert_eq!(vm_total, total, "total steps");
    for budget in 1..=total + 1 {
        let (w, ws) = walker_stop(src, budget);
        let (v, vs) = vm_stop(m, budget);
        let key = |d: Option<Diagnostic>| {
            d.map(|d| (d.code, d.kind, d.message, d.hint, d.line, d.col, d.span))
        };
        assert_eq!(key(v), key(w), "budget {budget}");
        assert_eq!(vs, ws, "steps at budget {budget}");
    }
    total
}

#[test]
fn the_step_budget_stops_at_the_same_step() {
    let src = "roll i = 0\nwhile i < 3 {\n  i = i + 1\n}\n";
    let ast = crate::syntax::parse(src).expect("parses");
    let roll = ast.body[0].span();
    let (wh, assign) = match &ast.body[1] {
        Stmt::While { span, body, .. } => (*span, body.stmts[0].span()),
        _ => panic!("a while"),
    };
    let mut f = BcFunction::empty_main();
    f.reg_count = 3;
    f.span_base = SpanBase {
        byte_offset: 0,
        line: 1,
        col: 1,
    };
    f.constants = vec![Constant::Int(1)];
    f.instrs = vec![
        steps(0, 1), // 0: `roll i = 0`
        Instr::asbx(Op::LOADINT, 0, 0),
        steps(0, 1), // 2: the `while` statement
        steps(1, 0), // 3: in the loop from here
        Instr::asbx(Op::LOADINT, 1, 3),
        abc(Op::LT, 2, 0, 1),
        Instr::asbx(Op::TESTN, 2, 4), // 6 -> 11
        steps(1, 1),                  // 7: one turn of the loop
        steps(1, 1),                  // 8: `i = i + 1`
        abc(Op::ADDK, 0, 0, 0),
        Instr::asbx(Op::LOOP, 0, -7), // 10 -> 4
        steps(0, 0),                  // 11: out of the loop
        ret(0),
    ];
    f.lines = vec![row(0, roll), row(2, wh), row(7, wh), row(8, assign)];
    let total = same_stop(src, &module(f));
    assert_eq!(total, 8);
}

#[test]
fn the_step_budget_inside_a_call_from_a_loop_says_loop() {
    let src = "pull f(x) {\n  snuff x + 1\n}\nroll i = 0\nwhile i < 2 {\n  i = f(i)\n}\n";
    let ast = crate::syntax::parse(src).expect("parses");
    let snuff = match &ast.body[0] {
        Stmt::Pull(decl) => decl.body.stmts[0].span(),
        _ => panic!("a pull"),
    };
    let roll = ast.body[1].span();
    let (wh, assign) = match &ast.body[2] {
        Stmt::While { span, body, .. } => (*span, body.stmts[0].span()),
        _ => panic!("a while"),
    };
    let base = SpanBase {
        byte_offset: 0,
        line: 1,
        col: 1,
    };
    let mut child = child_fn("f", 1, 2, vec![steps(0, 1), abc(Op::ADDK, 1, 0, 0), ret(1)]);
    child.constants = vec![Constant::Int(1)];
    child.span_base = base;
    child.lines = vec![row(0, snuff)];
    let mut f = BcFunction::empty_main();
    f.reg_count = 6;
    f.span_base = base;
    f.instrs = vec![
        Instr::abx(Op::CLOSURE, 3, 0), // 0: the hoisted pull
        steps(0, 1),                   // 1: `roll i = 0`
        Instr::asbx(Op::LOADINT, 0, 0),
        steps(0, 1), // 3: the `while`
        steps(1, 0), // 4
        Instr::asbx(Op::LOADINT, 1, 2),
        abc(Op::LT, 2, 0, 1),
        Instr::asbx(Op::TESTN, 2, 7), // 7 -> 15
        steps(1, 1),                  // 8: one turn
        steps(1, 1),                  // 9: `i = f(i)`
        abc(Op::MOV, 4, 3, 0),
        abc(Op::MOV, 5, 0, 0),
        abc(Op::CALL, 4, 1, 0),
        abc(Op::MOV, 0, 4, 0),
        Instr::asbx(Op::LOOP, 0, -10), // 14 -> 5
        steps(0, 0),                   // 15
        ret(0),
    ];
    f.lines = vec![row(1, roll), row(3, wh), row(8, wh), row(9, assign)];
    f.children = vec![child];
    let m = module(f);
    let total = same_stop(src, &m);
    assert_eq!(total, 8);
    // Budget 4 stops on the fifth step: in `f`, on its `snuff`, and the run
    // was in a loop.
    let (d, _) = vm_stop(&m, 4);
    let d = d.expect("stopped");
    assert_eq!(d.code, "E515");
    assert_eq!(d.span, Some(snuff));
    assert!(
        d.message.starts_with("your loop never ends"),
        "{}",
        d.message
    );
}

#[test]
fn a_steps_charge_stops_on_its_own_line() {
    let f = main_fn(1, vec![steps(0, 3), abc(Op::LOADNULL, 0, 0, 0), ret(0)]);
    let m = module(f);
    let mut i = interp();
    i.set_max_steps(2);
    let e = run(&mut i, &program(&m)).err().expect("an error");
    assert_eq!(e.code, "E515");
    assert_eq!(e.message, "the script needs more than 2 steps");
    assert_eq!(at_pc(&e), Some(0));
    assert_eq!(i.steps, 3);
}

// ----- loading -------------------------------------------------------------------------

#[test]
fn an_unknown_builtin_index_is_refused_at_load() {
    let n = crate::stdlib::builtin_catalogue().len();
    let f = main_fn(
        1,
        vec![
            abc(Op::LOADNULL, 0, 0, 0),
            wide_prefix(0, (n >> 8) as u16),
            abc(Op::CALLB, 0, (n & 0xFF) as u8, 0),
            ret(0),
        ],
    );
    let m = module(f);
    let v = verify_module(&m).expect("the verifier accepts any index");
    let e = load(v).err().expect("refused");
    assert_eq!(e.code, "E1050");
    assert_eq!(
        e.message,
        "the program's bytecode does not verify: unknown-builtin at fn0+1"
    );
    // In a child function, the error names the child.
    let mut f = main_fn(1, vec![abc(Op::LOADNULL, 0, 0, 0), ret(0)]);
    let mut child = child_fn("c", 0, 1, vec![abc(Op::CALLB, 0, 0, 0), ret(0)]);
    child.instrs[0] = abc(Op::NOP, 0, 0, 0);
    f.children = vec![child.clone(), {
        let mut c = child;
        c.instrs = vec![wide_prefix(0, 0xFF), abc(Op::CALLB, 0, 0xFF, 0), ret(0)];
        c
    }];
    let m = module(f);
    let e = load(verify_module(&m).expect("verifies"))
        .err()
        .expect("refused");
    assert_eq!(
        e.message,
        "the program's bytecode does not verify: unknown-builtin at fn2+0"
    );
}

#[test]
fn an_unknown_burn_op_kind_is_refused_at_load() {
    let mut f = main_fn(4, vec![abc(Op::BURN_OP, 0, 0, 0x1F), ret(0)]);
    f.requires_burn = true;
    f.constants = vec![Constant::Str("fs.write_text".into())];
    let m = module(f);
    let e = load(verify_module(&m).expect("verifies"))
        .err()
        .expect("refused");
    assert_eq!(e.code, "E1050");
    assert_eq!(
        e.message,
        "the program's bytecode does not verify: unknown-burn-op at fn0+0"
    );
}

#[test]
fn an_unverified_module_cannot_be_run() {
    // The VM's only entries take a `Verified` (for `load`) or a `Program`
    // made by `load`; `Verified` has no constructor but the verifier (its
    // compile-fail doc test in cig-bytecode shows the field is private).
    let _entry: fn(Verified<'_>) -> Result<Rc<Program>, Diagnostic> = load;
    let mut m = module(main_fn(1, vec![ret(0)]));
    m.main.instrs.clear();
    let e = verify_module(&m).expect_err("an empty function does not verify");
    let d = refusal(&e);
    assert_eq!(d.code, "E1050");
    assert!(
        d.message
            .starts_with("the program's bytecode does not verify: empty-function"),
        "{}",
        d.message
    );
}

// ----- what is left ---------------------------------------------------------------------

/// The instructions this package runs.
const BUILT: [Op; 39] = [
    Op::NOP,
    Op::MOV,
    Op::LOADK,
    Op::LOADNULL,
    Op::LOADBOOL,
    Op::LOADINT,
    Op::ADD,
    Op::SUB,
    Op::MUL,
    Op::DIV,
    Op::MOD,
    Op::POW,
    Op::NEG,
    Op::ADDK,
    Op::SUBK,
    Op::MULK,
    Op::DIVK,
    Op::MODK,
    Op::POWK,
    Op::EQ,
    Op::LT,
    Op::LE,
    Op::NOT,
    Op::EQK,
    Op::LTK,
    Op::LEK,
    Op::EQJ,
    Op::LTJ,
    Op::LEJ,
    Op::NEJ,
    Op::JMP,
    Op::TEST,
    Op::TESTN,
    Op::TESTSET,
    Op::LOOP,
    Op::CALL,
    Op::CALLB,
    Op::RET,
    Op::TAILCALL,
];

/// A module whose first instruction is `op`, with operands that verify.
/// Returns the module and the function to enter (instructions that need
/// an upvalue sit in a child, which only a capturing closure could reach).
fn first_is(op: Op) -> (Module, usize) {
    let mut f = main_fn(8, vec![]);
    f.constants = vec![Constant::Str("x".into())];
    let code = match op {
        Op::FORPREP | Op::FORLOOP | Op::ITERNEXT => vec![Instr::asbx(op, 0, 0), ret(0)],
        Op::ITERPREP | Op::NEWLIST | Op::NEWMAP | Op::CELLNEW | Op::THROW => {
            vec![abc(op, 0, 0, 0), ret(0)]
        }
        Op::GETFIELD | Op::CALLM => vec![abc(op, 0, 0, 0), ret(0)],
        Op::SETFIELD => vec![abc(op, 0, 0, 1), ret(0)],
        Op::GETINDEX | Op::SETINDEX | Op::INSERTMAP => vec![abc(op, 0, 1, 2), ret(0)],
        Op::APPENDLIST => vec![abc(op, 0, 1, 0), ret(0)],
        Op::INTERP => vec![abc(op, 0, 1, 2), ret(0)],
        Op::TRY_ENTER => {
            f.handlers = vec![HandlerEntry {
                pc_start: 0,
                pc_end: 1,
                handler_pc: 1,
                catch_reg: RESERVED_U32,
                finally_pc: RESERVED_U32,
            }];
            vec![Instr::abx(op, 0, 0), ret(0)]
        }
        Op::TRY_LEAVE | Op::COMP_ENTER | Op::COMP_LEAVE => vec![abc(op, 0, 0, 0), ret(0)],
        Op::BURN_ENTER => {
            f.regions = vec![BurnRegion {
                class: 0,
                label: RESERVED_U32,
            }];
            vec![Instr::abx(op, 0, 0), abc(Op::BURN_LEAVE, 0, 0, 0), ret(0)]
        }
        Op::BURN_LEAVE => {
            // Lexically balanced; at run time the jump skips the enter.
            f.regions = vec![BurnRegion {
                class: 0,
                label: RESERVED_U32,
            }];
            vec![
                Instr::asbx(Op::JMP, 0, 1), // 0 -> 2
                Instr::abx(Op::BURN_ENTER, 0, 0),
                abc(op, 0, 0, 0),
                ret(0),
            ]
        }
        Op::BURN_OP => {
            f.requires_burn = true;
            vec![abc(op, 0, 0, 0x10), ret(0)]
        }
        Op::PACK | Op::EXHALE | Op::CHAIN_MAKE => vec![abc(op, 0, 2, 0), ret(0)],
        Op::GETUP | Op::CELLGET => {
            let mut c = child_fn("c", 0, 2, vec![abc(op, 0, 0, 0), ret(0)]);
            c.upvalues = vec![UpvalDesc {
                kind: UpvalKind::ParentLocal,
                index: 0,
            }];
            f.instrs = vec![ret(0)];
            f.children = vec![c];
            return (module(f), 1);
        }
        Op::SETUP | Op::CELLSET => {
            let mut c = child_fn("c", 0, 2, vec![abc(op, 0, 0, 0), ret(0)]);
            c.upvalues = vec![UpvalDesc {
                kind: UpvalKind::ParentLocal,
                index: 0,
            }];
            f.instrs = vec![ret(0)];
            f.children = vec![c];
            return (module(f), 1);
        }
        other => panic!("no module for {other}"),
    };
    let n = code.len() as u32;
    f.instrs = code;
    f.lines.retain(|l| l.pc < n);
    if f.lines.is_empty() {
        f.lines = (0..n)
            .map(|pc| LineEntry {
                pc,
                span_off: pc * 10,
                span_len: 3,
                line_off: pc,
                col: pc + 2,
            })
            .collect();
    }
    (module(f), 0)
}

#[test]
fn instructions_not_built_yet_say_so() {
    // Every opcode is built or on the list; nothing else.
    for byte in 0..=255u8 {
        let Some(op) = Op::from_byte(byte) else {
            continue;
        };
        let listed = NOT_BUILT.contains(&op);
        let built = BUILT.contains(&op) || matches!(op, Op::CLOSURE | Op::STEPS | Op::WIDE);
        assert!(
            listed != built,
            "{op} is {} built and listed",
            if built { "both" } else { "neither" }
        );
    }
    for op in NOT_BUILT {
        let (m, entry) = first_is(op);
        let r = run_function(&mut interp(), &program(&m), entry, Vec::new());
        let e = r.err().unwrap_or_else(|| panic!("{op} ran"));
        assert_eq!(e.kind, Kind::Internal, "{op}: {}", e.message);
        assert_eq!(
            e.message,
            format!(
                "the VM cannot run `{op}` yet: the package that builds it has not landed (cig-compile#31)"
            )
        );
    }
    // A closure over captured values is the next package's too.
    let mut f = main_fn(2, vec![Instr::abx(Op::CLOSURE, 0, 0), ret(0)]);
    let mut c = add_fn();
    c.upvalues = vec![UpvalDesc {
        kind: UpvalKind::ParentLocal,
        index: 1,
    }];
    f.children = vec![c];
    let e = err(&module(f));
    assert_eq!(e.kind, Kind::Internal);
    assert!(e
        .message
        .contains("`closure` of a function that captures values"));
    assert_eq!(at_pc(&e), Some(0));
}

// ----- safety ----------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next() % n
        }
    }
}

const SAFE_BUILTINS: [&str; 16] = [
    "len",
    "str",
    "repr",
    "int",
    "float",
    "bool",
    "type_of",
    "math.abs",
    "math.min",
    "math.max",
    "math.floor",
    "math.ceil",
    "math.round",
    "math.sqrt",
    "math.pow",
    "math.clamp",
];

/// A random function: registers, constants, children (`depth` levels),
/// and code whose jumps go forward, except a `loop` back to a `steps`
/// that charges, so every cycle spends budget.
fn random_fn(rng: &mut Rng, depth: u32, arity: u16) -> BcFunction {
    let ops: Vec<Op> = (0..=255u8).filter_map(Op::from_byte).collect();
    let regs = 1 + rng.below(8) as u32;
    let mut f = BcFunction::empty_main();
    f.kind = if depth == 0 {
        FuncKind::Main
    } else {
        FuncKind::Pull
    };
    f.name = format!("f{depth}");
    f.reg_count = regs.max(u32::from(arity));
    f.arity = arity;
    for _ in 0..rng.below(5) {
        f.constants.push(match rng.below(4) {
            0 => Constant::Bool(rng.below(2) == 0),
            1 => Constant::Int((rng.next() as i64) >> (40 + rng.below(24))),
            2 => Constant::Float(f64::from_bits(rng.next())),
            _ => Constant::Str("s".repeat(rng.below(4) as usize)),
        });
    }
    if depth < 2 {
        for _ in 0..rng.below(3) {
            let a = rng.below(3) as u16;
            f.children.push(random_fn(rng, depth + 1, a));
        }
    }
    let n = 1 + rng.below(24) as usize;
    let reg = |rng: &mut Rng| rng.below(u64::from(f.reg_count) + 1) as u8;
    let safe: Vec<usize> = SAFE_BUILTINS
        .iter()
        .map(|n| catalogue_index(n))
        .filter(|i| *i < 256)
        .collect();
    let mut code: Vec<Instr> = Vec::new();
    let mut loop_heads: Vec<usize> = Vec::new();
    while code.len() < n {
        let pc = code.len();
        let left = (n - pc) as u64;
        let op = ops[rng.below(ops.len() as u64) as usize];
        let fwd = |rng: &mut Rng| rng.below(left) as i16;
        match op {
            Op::WIDE => {
                code.push(wide_prefix(0, 0));
                code.push(abc(Op::MOV, reg(rng), reg(rng), 0));
            }
            Op::STEPS => {
                loop_heads.push(pc);
                code.push(steps(rng.below(2) as u8, 1 + rng.below(2) as u16));
            }
            Op::LOOP | Op::JMP if !loop_heads.is_empty() && rng.below(2) == 0 => {
                let head = loop_heads[rng.below(loop_heads.len() as u64) as usize];
                code.push(Instr::asbx(op, 0, head as i16 - pc as i16 - 1));
            }
            Op::LOOP | Op::JMP => code.push(Instr::asbx(op, 0, fwd(rng))),
            Op::TEST | Op::TESTN | Op::FORPREP | Op::FORLOOP | Op::ITERNEXT => {
                code.push(Instr::asbx(op, reg(rng), fwd(rng)))
            }
            Op::EQJ | Op::LTJ | Op::LEJ | Op::NEJ | Op::TESTSET => {
                code.push(abc(op, reg(rng), reg(rng), fwd(rng) as u8))
            }
            Op::LOADINT => code.push(Instr::asbx(op, reg(rng), rng.next() as i16)),
            Op::LOADK | Op::CLOSURE | Op::TRY_ENTER | Op::BURN_ENTER => {
                code.push(Instr::abx(op, reg(rng), rng.below(4) as u16))
            }
            Op::CALLB => {
                let b = safe[rng.below(safe.len() as u64) as usize] as u8;
                code.push(abc(op, reg(rng), b, rng.below(3) as u8));
            }
            _ => code.push(abc(
                op,
                reg(rng),
                rng.below(u64::from(f.reg_count) + 2) as u8,
                rng.below(u64::from(f.reg_count) + 2) as u8,
            )),
        }
    }
    code.push(ret(0));
    f.lines = (0..code.len() as u32)
        .map(|pc| LineEntry {
            pc,
            span_off: pc,
            span_len: 1,
            line_off: pc,
            col: 1,
        })
        .collect();
    f.instrs = code;
    f
}

#[test]
fn a_verified_module_never_panics() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut ran = 0;
    let mut tried = 0;
    while ran < 2_000 && tried < 200_000 {
        tried += 1;
        let m = module(random_fn(&mut rng, 0, 0));
        let Ok(v) = verify_module(&m) else {
            continue;
        };
        let budget = 1 + rng.below(60);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let Ok(p) = load(v) else {
                return (0, 0);
            };
            let mut i = interp();
            i.set_max_steps(budget);
            let _ = run(&mut i, &p);
            (i.steps, i.loop_depth + i.call_depth_for_tests() as u32)
        }));
        let (steps, held) = outcome.unwrap_or_else(|_| panic!("panicked on {m:?}"));
        assert!(
            steps <= budget + 1,
            "ran past its budget: {steps} > {budget}"
        );
        assert_eq!(held, 0, "depth held after the run");
        ran += 1;
    }
    assert!(ran >= 2_000, "only {ran} of {tried} verified");
}
