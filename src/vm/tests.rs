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
    // R0 = 5; R1 = 0; top: testn R0 -> out; steps; R1 += 2; R0 -= 1;
    // loop top; out: ret R1
    let mut f = main_fn(
        2,
        vec![
            Instr::asbx(Op::LOADINT, 0, 5),
            Instr::asbx(Op::LOADINT, 1, 0),
            Instr::asbx(Op::TESTN, 0, 4), // pc 2 -> 7
            steps(1, 1),
            abc(Op::ADDK, 1, 1, 0),
            abc(Op::SUBK, 0, 0, 1),
            Instr::asbx(Op::LOOP, 0, -5), // pc 6 -> 2
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

/// The instructions the VM runs.
const BUILT: [Op; 63] = [
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
    Op::FORPREP,
    Op::FORLOOP,
    Op::ITERPREP,
    Op::ITERNEXT,
    Op::NEWLIST,
    Op::NEWMAP,
    Op::GETFIELD,
    Op::SETFIELD,
    Op::GETINDEX,
    Op::SETINDEX,
    Op::APPENDLIST,
    Op::INSERTMAP,
    Op::INTERP,
    Op::CALLM,
    Op::GETUP,
    Op::SETUP,
    Op::CELLNEW,
    Op::CELLGET,
    Op::CELLSET,
    Op::EXHALE,
    Op::LOADG,
    Op::TRY_ENTER,
    Op::TRY_LEAVE,
    Op::THROW,
];

/// A module whose first instruction is `op`, with operands that verify,
/// and the function to enter.
fn first_is(op: Op) -> (Module, usize) {
    let mut f = main_fn(8, vec![]);
    f.constants = vec![Constant::Str("x".into())];
    let code = match op {
        Op::COMP_ENTER | Op::COMP_LEAVE => vec![abc(op, 0, 0, 0), ret(0)],
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
        Op::PACK | Op::CHAIN_MAKE => vec![abc(op, 0, 2, 0), ret(0)],
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
    assert_eq!(
        NOT_BUILT,
        [
            Op::BURN_ENTER,
            Op::BURN_OP,
            Op::BURN_LEAVE,
            Op::COMP_ENTER,
            Op::COMP_LEAVE,
            Op::PACK,
            Op::CHAIN_MAKE,
        ]
    );
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

/// A random function: registers, constants, children (`depth` levels)
/// that capture from it, a handler table, and code whose jumps go forward,
/// except a `loop` back to a `steps` that charges, so every cycle spends
/// budget. `parent` is the enclosing function's register and upvalue
/// counts.
fn random_fn(rng: &mut Rng, depth: u32, arity: u16, parent: Option<(u32, usize)>) -> BcFunction {
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
    if let Some((regs, ups)) = parent {
        for _ in 0..rng.below(3) {
            let local = ups == 0 || rng.below(2) == 0;
            f.upvalues.push(if local {
                UpvalDesc {
                    kind: UpvalKind::ParentLocal,
                    index: rng.below(u64::from(regs)) as u16,
                }
            } else {
                UpvalDesc {
                    kind: UpvalKind::ParentUpvalue,
                    index: rng.below(ups as u64) as u16,
                }
            });
        }
    }
    // Names for getfield, setfield and callm; `loadg` names K0 to K3 (a
    // builtin, a name the prelude does not have, a module, `args`).
    f.constants.push(Constant::Str("len".into()));
    f.constants.push(Constant::Str("a".into()));
    f.constants.push(Constant::Str("math".into()));
    f.constants.push(Constant::Str("args".into()));
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
            let here = (f.reg_count, f.upvalues.len());
            f.children.push(random_fn(rng, depth + 1, a, Some(here)));
        }
    }
    let n = 1 + rng.below(24) as usize;
    let n_handlers = rng.below(4);
    let reg = |rng: &mut Rng| rng.below(u64::from(f.reg_count) + 1) as u8;
    let safe: Vec<usize> = SAFE_BUILTINS
        .iter()
        .map(|n| catalogue_index(n))
        .filter(|i| *i < 256)
        .collect();
    let mut code: Vec<Instr> = Vec::new();
    let mut loop_heads: Vec<usize> = Vec::new();
    if !f.upvalues.is_empty() && rng.below(3) == 0 {
        // Every upvalue instruction once, on operands that verify.
        let u = rng.below(f.upvalues.len() as u64) as u8;
        code = vec![
            abc(Op::GETUP, 0, u, 0),
            abc(Op::CELLNEW, 0, 0, 0),
            abc(Op::CELLSET, u, 0, 0),
            abc(Op::CELLGET, 0, u, 0),
            abc(Op::SETUP, u, 0, 0),
        ];
    }
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
            Op::LOADK | Op::CLOSURE | Op::BURN_ENTER => {
                code.push(Instr::abx(op, reg(rng), rng.below(4) as u16))
            }
            Op::TRY_ENTER => code.push(Instr::abx(op, 0, rng.below(n_handlers + 1) as u16)),
            Op::TRY_LEAVE => code.push(abc(op, rng.below(2) as u8, 0, 0)),
            Op::LOADG => code.push(Instr::abx(op, reg(rng), rng.below(5) as u16)),
            Op::CALLB => {
                let b = safe[rng.below(safe.len() as u64) as usize] as u8;
                code.push(abc(op, reg(rng), b, rng.below(3) as u8));
            }
            Op::GETFIELD | Op::CALLM => {
                let k = rng.below(2) as u8;
                let (b, c) = if op == Op::GETFIELD {
                    (reg(rng), k)
                } else {
                    (k, rng.below(3) as u8)
                };
                code.push(abc(op, reg(rng), b, c));
            }
            Op::SETFIELD => code.push(abc(op, reg(rng), rng.below(2) as u8, reg(rng))),
            Op::GETUP | Op::CELLGET => {
                let u = rng.below(f.upvalues.len() as u64 + 1) as u8;
                code.push(abc(op, reg(rng), u, 0));
            }
            Op::SETUP | Op::CELLSET => {
                let u = rng.below(f.upvalues.len() as u64 + 1) as u8;
                code.push(abc(op, u, reg(rng), 0));
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
    // Handlers: a range, then a catch and a finally after it (sometimes
    // the same pc: a try with no catch), on first-word pcs.
    let starts: Vec<u32> = cig_bytecode::first_word_pcs(&code)
        .map(|p| p.iter().collect())
        .unwrap_or_default();
    let pick = |rng: &mut Rng, from: u32| -> u32 {
        let later: Vec<u32> = starts.iter().copied().filter(|p| *p >= from).collect();
        later
            .get(rng.below(later.len() as u64) as usize)
            .copied()
            .unwrap_or(from)
    };
    for _ in 0..n_handlers {
        let start = pick(rng, 0);
        let end = pick(rng, start + 1).max(start + 1);
        let handler_pc = pick(rng, end);
        let finally_pc = match rng.below(3) {
            0 => RESERVED_U32,
            1 => handler_pc,
            _ => pick(rng, end),
        };
        f.handlers.push(HandlerEntry {
            pc_start: start,
            pc_end: end,
            handler_pc,
            catch_reg: if rng.below(3) == 0 {
                RESERVED_U32
            } else {
                rng.below(u64::from(f.reg_count)) as u32
            },
            finally_pc,
        });
    }
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

fn has_handlers(f: &BcFunction) -> bool {
    !f.handlers.is_empty() || f.children.iter().any(has_handlers)
}

/// Every opcode in `f` and its children.
fn ops_in(f: &BcFunction, out: &mut std::collections::HashSet<Op>) {
    out.extend(
        cig_bytecode::instr::walk(&f.instrs)
            .filter_map(|d| d.ok())
            .map(|d| d.op),
    );
    for c in &f.children {
        ops_in(c, out);
    }
}

#[test]
fn a_verified_module_never_panics() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut ran = 0;
    let mut tried = 0;
    let mut seen = std::collections::HashSet::new();
    // At least 2,000 modules, and on until every built instruction has been
    // in one that verified.
    while (ran < 2_000 || !BUILT.iter().all(|op| seen.contains(op))) && tried < 400_000 {
        tried += 1;
        let m = module(random_fn(&mut rng, 0, 0, None));
        let Ok(v) = verify_module(&m) else {
            continue;
        };
        ops_in(&m.main, &mut seen);
        let budget = 1 + rng.below(60);
        let handlers = has_handlers(&m.main);
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
        // A handler can catch E515; the next charge raises it again, so a
        // run with handlers counts a few more steps on its way out.
        let slack = if handlers {
            4 * crate::interp::MAX_CALL_DEPTH as u64
        } else {
            1
        };
        assert!(
            steps <= budget + slack,
            "ran past its budget: {steps} > {budget}"
        );
        assert_eq!(held, 0, "depth held after the run");
        ran += 1;
    }
    assert!(ran >= 2_000, "only {ran} of {tried} verified");
    for op in BUILT {
        assert!(seen.contains(&op), "no verified module ran `{op}`");
    }
}

// ----- output and text -------------------------------------------------------------------

#[derive(Clone, Default)]
struct Buf(Rc<std::cell::RefCell<Vec<u8>>>);

impl std::io::Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Buf {
    fn text(&self) -> String {
        String::from_utf8(self.0.borrow().clone()).expect("utf-8")
    }
}

/// Run `src` on the tree-walker with `vars` declared as globals: what it
/// printed, and how it ended.
fn walker_run(src: &str, vars: &[(&str, Value)]) -> (String, Option<Diagnostic>) {
    let program = crate::syntax::parse(src).expect("parses");
    let buf = Buf::default();
    let mut i = interp();
    i.out = Box::new(buf.clone());
    for (name, v) in vars {
        i.globals.declare(name, v.clone(), true);
    }
    let r = i.run_program(&program);
    (buf.text(), r.err())
}

/// Run `m`'s top level with `args`: what it printed, and how it ended.
fn vm_run(m: &Module, args: Vec<Value>) -> (String, Result<Value, Diagnostic>) {
    let buf = Buf::default();
    let mut i = interp();
    i.out = Box::new(buf.clone());
    let r = run_function(&mut i, &program(m), 0, args);
    assert_eq!(i.loop_depth, 0, "the loop depth is given back");
    assert_eq!(i.call_depth_for_tests(), 0, "the call depth is given back");
    (buf.text(), r)
}

/// An error as the two engines are compared on it: code, kind, message
/// and hint (positions are the lowering's: each test checks its pc).
fn said(d: &Diagnostic) -> String {
    format!("{} {:?} {} {:?}", d.code, d.kind, d.message, d.hint)
}

/// Every kind of value `exhale` can be handed.
fn printable_values() -> Vec<Value> {
    let mut v = operand_table();
    let mut nested = IndexMap::new();
    nested.insert("b c".to_string(), Value::str("q\"$\n\t\u{7}"));
    nested.insert(
        "n".to_string(),
        Value::list(vec![Value::Null, Value::Float(1e20)]),
    );
    v.push(Value::map(nested));
    let looped = Value::list(vec![Value::Int(1)]);
    if let Value::List(l) = &looped {
        l.borrow_mut().push(looped.clone());
    }
    v.push(looped);
    let len = crate::stdlib::builtin_catalogue()[catalogue_index("len")];
    v.push(Value::Builtin(Rc::new(*len)));
    let math = crate::stdlib::prelude()
        .into_iter()
        .find(|(n, _)| *n == "math")
        .map(|(_, m)| m)
        .expect("math");
    v.push(math);
    let mut f = main_fn(1, vec![Instr::abx(Op::CLOSURE, 0, 0), ret(0)]);
    f.children = vec![add_fn()];
    v.push(exec(&module(f)).expect("a closure"));
    v
}

#[test]
fn exhale_writes_the_interpreters_bytes() {
    // `exhale v` and `exhale v, v`, then interpolation: `"<${v}|${v}>"`.
    // `--json` and `--plain` shape diagnostics and cig's own reports, not
    // the script's output, so one writer serves all three.
    let mut f = main_fn(
        8,
        vec![
            abc(Op::EXHALE, 0, 1, 0),
            abc(Op::MOV, 1, 0, 0),
            abc(Op::EXHALE, 0, 2, 0),
            Instr::abx(Op::LOADK, 2, 0),
            abc(Op::MOV, 3, 0, 0),
            Instr::abx(Op::LOADK, 4, 1),
            abc(Op::MOV, 5, 0, 0),
            Instr::abx(Op::LOADK, 6, 2),
            abc(Op::INTERP, 7, 2, 4),
            abc(Op::EXHALE, 7, 1, 0),
            ret(7),
        ],
    );
    f.arity = 1;
    f.constants = vec![
        Constant::Str("<".into()),
        Constant::Str("|".into()),
        Constant::Str(">".into()),
    ];
    let m = module(f);
    let src = "exhale v\nexhale v, v\nexhale \"<${v}|${v}>\"\n";
    for v in printable_values() {
        let (want, e) = walker_run(src, &[("v", v.clone())]);
        assert!(e.is_none(), "{}", v.repr());
        let (got, r) = vm_run(&m, vec![v.clone()]);
        assert_eq!(got, want, "{}", v.repr());
        r.expect("ran");
    }
}

/// A writer whose reader went away.
struct Closed;

impl std::io::Write for Closed {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::BrokenPipe.into())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn exhale_to_a_closed_pipe_stops_as_the_interpreter_does() {
    let ast = crate::syntax::parse("exhale 1\n").expect("parses");
    let mut w = interp();
    w.out = Box::new(Closed);
    let walker = w.run_program(&ast);
    assert!(walker.is_ok(), "the tree-walker stops quietly");
    assert_eq!(w.exit_requested, Some(0));

    let m = module(main_fn(
        1,
        vec![
            Instr::asbx(Op::LOADINT, 0, 1),
            abc(Op::EXHALE, 0, 1, 0),
            ret(0),
        ],
    ));
    let mut i = interp();
    i.out = Box::new(Closed);
    let e = run(&mut i, &program(&m)).err().expect("stops");
    assert_eq!(e.code, "E513");
    assert_eq!(e.message, "output pipe closed");
    assert_eq!(i.exit_requested, Some(0));
}

#[test]
fn interp_shows_each_part_as_interpolation_does() {
    // "n=${n} f=${f} s=${s} l=${l}" over every operand.
    let mut f = main_fn(
        12,
        vec![
            Instr::abx(Op::LOADK, 2, 0),
            abc(Op::MOV, 3, 0, 0),
            Instr::abx(Op::LOADK, 4, 1),
            abc(Op::MOV, 5, 1, 0),
            abc(Op::INTERP, 0, 2, 3),
            ret(0),
        ],
    );
    f.arity = 2;
    f.constants = vec![Constant::Str("a=".into()), Constant::Str(" b=".into())];
    let m = module(f);
    for a in operand_table() {
        for b in [Value::str("é"), Value::Float(1.0), Value::Null] {
            let vars = [("a", a.clone()), ("b", b.clone())];
            let (want, e) = walker_run("exhale \"a=${a} b=${b}\"\n", &vars);
            assert!(e.is_none());
            let got = exec_with(&m, vec![a.clone(), b.clone()]).expect("ran");
            assert_eq!(format!("{}\n", got.display()), want);
        }
    }
    // An interp of no parts but one gives that part as text.
    let f = main_fn(
        2,
        vec![
            Instr::asbx(Op::LOADINT, 1, 42),
            abc(Op::INTERP, 0, 1, 0),
            ret(0),
        ],
    );
    assert_eq!(ok(&module(f)), "\"42\"");
}

// ----- loops ---------------------------------------------------------------------------

/// `for i in R0..R1 step K[0] { exhale i }`, the window at R2..R6:
/// forprep jumps past the loop when it makes no turn.
fn for_module(step: i64) -> Module {
    let mut f = main_fn(
        7,
        vec![
            abc(Op::MOV, 2, 0, 0),
            abc(Op::MOV, 3, 1, 0),
            Instr::abx(Op::LOADK, 4, 0),
            steps(1, 0),
            Instr::asbx(Op::FORPREP, 2, 3),  // pc 4 -> 8
            steps(1, 1),                     // pc 5: one item
            abc(Op::EXHALE, 5, 1, 0),        // pc 6
            Instr::asbx(Op::FORLOOP, 2, -3), // pc 7 -> 5
            steps(0, 0),                     // pc 8
            ret(5),
        ],
    );
    f.arity = 2;
    f.constants = vec![Constant::Int(step)];
    module(f)
}

#[test]
fn forprep_and_forloop_run_a_range() {
    let m = for_module(1);
    for (a, b) in [(0, 0), (0, 1), (3, 1), (-2, 3), (i64::MAX - 2, i64::MAX)] {
        let (want, e) = walker_run(
            "for i in a..b {\n  exhale i\n}\n",
            &[("a", Value::Int(a)), ("b", Value::Int(b))],
        );
        assert!(e.is_none());
        let (got, r) = vm_run(&m, vec![Value::Int(a), Value::Int(b)]);
        assert!(r.is_ok(), "{a}..{b}");
        assert_eq!(got, want, "{a}..{b}");
    }
    // Other steps, as `range(a, b, step)` lists them.
    for (step, a, b) in [
        (-1, 5, 0),
        (-2, 5, 0),
        (3, 0, 10),
        (-1, 0, 5),
        (i64::MAX, 0, i64::MAX),
        (-1, i64::MIN + 2, i64::MIN),
    ] {
        let (want, e) = walker_run(
            "for i in range(a, b, s) {\n  exhale i\n}\n",
            &[
                ("a", Value::Int(a)),
                ("b", Value::Int(b)),
                ("s", Value::Int(step)),
            ],
        );
        assert!(e.is_none());
        let (got, r) = vm_run(&for_module(step), vec![Value::Int(a), Value::Int(b)]);
        assert!(r.is_ok(), "{a}..{b} by {step}");
        assert_eq!(got, want, "{a}..{b} by {step}");
    }
    // The induction variable is a copy: writing it does not move the loop.
    let f = main_fn(
        6,
        vec![
            Instr::asbx(Op::LOADINT, 5, 0),
            Instr::asbx(Op::LOADINT, 0, 0),
            Instr::asbx(Op::LOADINT, 1, 3),
            Instr::asbx(Op::LOADINT, 2, 1),
            Instr::asbx(Op::FORPREP, 0, 4),   // pc 4 -> 9
            steps(1, 1),                      // pc 5
            abc(Op::ADD, 5, 5, 3),            // pc 6: R5 += i
            Instr::asbx(Op::LOADINT, 3, 100), // pc 7
            Instr::asbx(Op::FORLOOP, 0, -4),  // pc 8 -> 5
            ret(5),
        ],
    );
    assert_eq!(ok(&module(f)), "3");
    // The errors of `a..b`: bounds that are not ints (E403), more than
    // 10,000,000 numbers (E512), each at the forprep.
    let m = for_module(1);
    for (a, b) in [
        (Value::Float(1.0), Value::Int(3)),
        (Value::Int(0), Value::str("x")),
        (Value::Int(0), Value::Int(10_000_001)),
        (Value::Int(i64::MIN), Value::Int(i64::MAX)),
    ] {
        let (_, w) = walker_run(
            "for i in a..b {\n  exhale i\n}\n",
            &[("a", a.clone()), ("b", b.clone())],
        );
        let w = w.expect("the tree-walker stops");
        let (_, r) = vm_run(&m, vec![a.clone(), b.clone()]);
        let v = r.err().expect("the VM stops");
        assert_eq!(said(&v), said(&w), "{}..{}", a.repr(), b.repr());
        assert_eq!(at_pc(&v), Some(4));
    }
    // A wide forprep/forloop names its window past 255 and jumps from its
    // first word.
    let code = vec![
        wide_prefix(1, 0),
        Instr::asbx(Op::LOADINT, 0, 0), // pc 0: R256 = 0
        wide_prefix(1, 0),
        Instr::asbx(Op::LOADINT, 1, 4), // pc 2: R257 = 4
        wide_prefix(1, 0),
        Instr::asbx(Op::LOADINT, 2, 1), // pc 4: R258 = 1
        Instr::asbx(Op::LOADINT, 0, 0), // pc 6: R0 = 0
        wide_prefix(1, 0),
        Instr::asbx(Op::FORPREP, 0, 6), // pc 7 -> 14
        steps(1, 1),                    // pc 9
        wide_prefix(0, 0x0100),
        abc(Op::ADD, 0, 0, 3), // pc 10: R0 += R259
        wide_prefix(1, 0xFFFF),
        Instr::asbx(Op::FORLOOP, 0, -4), // pc 12 -> 9
        ret(0),
    ];
    let f = main_fn(260, code);
    assert_eq!(ok(&module(f)), "6");
}

/// `for x in R0 { exhale x; <body> }` then `exhale R0`, the window at
/// R1..R5; `body` may use R5.. and the constants `"push"` (K0) and `"!"`
/// (K1).
fn iter_module(body: Vec<Instr>) -> Module {
    let n = body.len() as i16;
    let mut code = vec![
        abc(Op::MOV, 1, 0, 0),
        steps(1, 0),
        abc(Op::ITERPREP, 1, 0, 0),          // pc 2
        Instr::asbx(Op::ITERNEXT, 1, n + 3), // pc 3 -> exit
        steps(1, 1),                         // pc 4
        abc(Op::EXHALE, 4, 1, 0),            // pc 5
    ];
    code.extend(body);
    code.push(Instr::asbx(Op::LOOP, 0, -(n + 4))); // -> pc 3
    code.extend([steps(0, 0), abc(Op::EXHALE, 0, 1, 0), ret(0)]);
    let mut f = main_fn(10, code);
    f.arity = 1;
    f.constants = vec![Constant::Str("push".into()), Constant::Str("!".into())];
    module(f)
}

#[test]
fn iterprep_and_iternext_walk_the_interpreters_snapshot() {
    let m = iter_module(Vec::new());
    let (out, r) = vm_run(&m, vec![Value::str("aé😀")]);
    assert!(r.is_ok());
    assert_eq!(out, "a\né\n😀\naé😀\n");
    let (out, _) = vm_run(&m, vec![Value::list(Vec::new())]);
    assert_eq!(out, "[]\n");
    for v in [Value::Int(3), Value::Null, Value::Float(1.5)] {
        let (_, w) = walker_run("for x in v {\n  exhale x\n}\n", &[("v", v.clone())]);
        let w = w.expect("the tree-walker stops");
        let (out, r) = vm_run(&m, vec![v.clone()]);
        let e = r.err().expect("the VM stops");
        assert_eq!(out, "");
        assert_eq!(said(&e), said(&w));
        assert_eq!(e.code, "E405");
        assert_eq!(at_pc(&e), Some(2));
    }
}

#[test]
fn a_loop_over_data_visits_what_the_interpreter_visits() {
    let list = || Value::list(vec![Value::Int(1), Value::str("é"), Value::Int(3)]);
    let map = || {
        let mut m = IndexMap::new();
        m.insert("a".to_string(), Value::Int(1));
        m.insert("b".to_string(), Value::Int(2));
        Value::map(m)
    };
    let plain = iter_module(Vec::new());
    // v.push(x) after each item: the loop still visits the three it began with.
    let pushes = iter_module(vec![
        abc(Op::MOV, 5, 0, 0),
        abc(Op::MOV, 6, 4, 0),
        abc(Op::CALLM, 5, 0, 1),
    ]);
    // v[x + "!"] = 1 after each key: new keys are not visited.
    let stores = iter_module(vec![
        Instr::abx(Op::LOADK, 7, 1),
        abc(Op::ADD, 6, 4, 7),
        Instr::asbx(Op::LOADINT, 8, 1),
        abc(Op::SETINDEX, 0, 6, 8),
    ]);
    let text = || Value::str("aé😀\u{301}");
    let empty = || Value::str("");
    let no_map = || Value::map(IndexMap::new());
    let cases: Vec<(&str, &Module, &dyn Fn() -> Value)> = vec![
        ("", &plain, &list),
        ("", &plain, &map),
        ("", &plain, &text),
        ("", &plain, &empty),
        ("", &plain, &no_map),
        ("  v.push(x)\n", &pushes, &list),
        ("  v[x + \"!\"] = 1\n", &stores, &map),
    ];
    // Each engine gets its own copy: the loops change what they walk.
    for (body, m, v) in cases {
        let src = format!("for x in v {{\n  exhale x\n{body}}}\nexhale v\n");
        let (want, e) = walker_run(&src, &[("v", v())]);
        assert!(e.is_none(), "{src}");
        let (got, r) = vm_run(m, vec![v()]);
        assert!(r.is_ok(), "{src}: {:?}", r.err().map(|e| e.message));
        assert_eq!(got, want, "{src}");
    }
    // Ranges, including the empty one and a negative step, are in
    // `forprep_and_forloop_run_a_range`.
}

// ----- lists and maps --------------------------------------------------------------------

#[test]
fn lists_and_maps_build_and_store() {
    // [7, "x"]
    let mut f = main_fn(
        2,
        vec![
            abc(Op::NEWLIST, 0, 2, 0),
            Instr::asbx(Op::LOADINT, 1, 7),
            abc(Op::APPENDLIST, 0, 1, 0),
            Instr::abx(Op::LOADK, 1, 0),
            abc(Op::APPENDLIST, 0, 1, 0),
            ret(0),
        ],
    );
    f.constants = vec![Constant::Str("x".into())];
    assert_eq!(ok(&module(f)), "[7, \"x\"]");
    // {a: 9, "5": 2}: an int key is its digits; a key set again keeps its place.
    let mut f = main_fn(
        3,
        vec![
            abc(Op::NEWMAP, 0, 0, 0),
            Instr::abx(Op::LOADK, 1, 0),
            Instr::asbx(Op::LOADINT, 2, 1),
            abc(Op::INSERTMAP, 0, 1, 2),
            Instr::asbx(Op::LOADINT, 1, 5),
            Instr::asbx(Op::LOADINT, 2, 2),
            abc(Op::INSERTMAP, 0, 1, 2),
            Instr::abx(Op::LOADK, 1, 0),
            Instr::asbx(Op::LOADINT, 2, 9),
            abc(Op::INSERTMAP, 0, 1, 2),
            ret(0),
        ],
    );
    f.constants = vec![Constant::Str("a".into())];
    assert_eq!(ok(&module(f)), "{a: 9, \"5\": 2}");
    // A map literal's key that is neither a string nor an int.
    let (_, w) = walker_run("roll m = {[k]: 1}\n", &[("k", Value::Float(1.5))]);
    let f = main_fn(
        3,
        vec![
            abc(Op::NEWMAP, 0, 0, 0),
            Instr::asbx(Op::LOADNULL, 1, 0),
            abc(Op::INSERTMAP, 0, 1, 2),
            ret(0),
        ],
    );
    let e = err(&module(f));
    if let Some(w) = w {
        assert_eq!(e.message, w.message.replace("float", "null"));
    }
    assert_eq!(e.message, "map keys must be strings, got null");
    assert_eq!(at_pc(&e), Some(2));
    // appendlist and insertmap on what is not a list or a map: a lowering
    // that wrote them wrong, an internal error at the instruction.
    for (op, what) in [(Op::APPENDLIST, "no list"), (Op::INSERTMAP, "no map")] {
        let f = main_fn(3, vec![abc(op, 0, 1, 2), ret(0)]);
        let e = err(&module(f));
        assert_eq!(e.kind, Kind::Internal);
        assert!(e.message.contains(what), "{}", e.message);
        assert_eq!(at_pc(&e), Some(0));
    }
}

#[test]
fn wide_data_instructions_name_registers_past_255() {
    let mut f = main_fn(
        300,
        vec![
            Instr::asbx(Op::LOADINT, 0, 0), // pc 0: R0 = 0
            wide_prefix(1, 0),
            abc(Op::NEWMAP, 0, 0, 0), // pc 1: R256 = {}
            wide_prefix(1, 0),
            abc(Op::NEWLIST, 1, 0, 0), // pc 3: R257 = []
            wide_prefix(1, 0),
            Instr::asbx(Op::LOADINT, 2, 5), // pc 5: R258 = 5
            wide_prefix(1, 0x0001),
            abc(Op::APPENDLIST, 1, 2, 0), // pc 7: R257.push(R258)
            wide_prefix(1, 0x0100),
            abc(Op::SETFIELD, 0, 0, 2), // pc 9: R256.k = R258
            wide_prefix(1, 0x0001),
            abc(Op::GETFIELD, 3, 0, 0), // pc 11: R259 = R256.k
            wide_prefix(1, 0x0001),
            abc(Op::GETINDEX, 4, 1, 0), // pc 13: R260 = R257[R0]
            wide_prefix(1, 0x0100),
            abc(Op::SETINDEX, 1, 0, 3), // pc 15: R257[R0] = R259
            wide_prefix(1, 0),
            Instr::abx(Op::LOADK, 5, 1), // pc 17: R261 = "z"
            wide_prefix(1, 0x0101),
            abc(Op::INSERTMAP, 0, 5, 4), // pc 19: R256[R261] = R260
            wide_prefix(1, 0x0001),
            abc(Op::INTERP, 6, 0, 1), // pc 21: R262 = "${R256}${R257}"
            wide_prefix(1, 0),
            abc(Op::EXHALE, 6, 1, 0), // pc 23
            wide_prefix(1, 0x0001),
            abc(Op::MOV, 7, 1, 0), // pc 25: R263 = R257
            wide_prefix(1, 0),
            abc(Op::CALLM, 7, 2, 0), // pc 27: R263 = R263.len()
            wide_prefix(1, 0),
            ret(7),
        ],
    );
    f.constants = vec![
        Constant::Str("k".into()),
        Constant::Str("z".into()),
        Constant::Str("len".into()),
    ];
    let (out, r) = vm_run(&module(f), Vec::new());
    assert_eq!(out, "{k: 5, z: 5}[5]\n");
    assert_eq!(r.ok().map(|v| v.repr()), Some("1".to_string()));
}

/// Objects for the data table, made fresh for each engine in every case
/// (stores change them).
fn data_objects() -> Vec<Value> {
    let mut m = IndexMap::new();
    m.insert("a".to_string(), Value::Int(1));
    m.insert("b c".to_string(), Value::list(vec![Value::Int(2)]));
    let math = crate::stdlib::prelude()
        .into_iter()
        .find(|(n, _)| *n == "math")
        .map(|(_, m)| m)
        .expect("math");
    vec![
        Value::list(vec![Value::Int(1), Value::str("x"), Value::list(vec![])]),
        Value::list(vec![]),
        Value::map(m),
        Value::map(IndexMap::new()),
        Value::str("héllo😀"),
        Value::str(""),
        Value::Null,
        Value::Bool(true),
        Value::Int(7),
        Value::Float(2.5),
        math,
        Value::Builtin(Rc::new(
            *crate::stdlib::builtin_catalogue()[catalogue_index("len")],
        )),
    ]
}

#[test]
fn the_vm_and_the_interpreter_agree_on_every_data_operation() {
    let keys = [
        Value::Int(0),
        Value::Int(1),
        Value::Int(-1),
        Value::Int(5),
        Value::Int(-9),
        Value::Int(i64::MIN),
        Value::str("a"),
        Value::str("zz"),
        Value::str("b c"),
        Value::Float(1.0),
        Value::Null,
        Value::Bool(true),
    ];
    let getindex = {
        let mut f = main_fn(3, vec![abc(Op::GETINDEX, 2, 0, 1), ret(2)]);
        f.arity = 2;
        module(f)
    };
    let setindex = {
        let mut f = main_fn(
            3,
            vec![
                Instr::asbx(Op::LOADINT, 2, 9),
                abc(Op::SETINDEX, 0, 1, 2),
                ret(0),
            ],
        );
        f.arity = 2;
        module(f)
    };
    let n_objects = data_objects().len();
    let mut compared = 0;
    let show = |r: Result<Value, Diagnostic>| match r {
        Ok(v) => v.repr(),
        Err(e) => said(&e),
    };
    let walked = |src: &str, vars: &[(&str, Value)]| match walker_run(src, vars) {
        (out, None) => out.trim_end_matches('\n').to_string(),
        (_, Some(e)) => said(&e),
    };
    for i in 0..n_objects {
        for k in &keys {
            let o = || data_objects().swap_remove(i);
            let want = walked("exhale repr(o[k])\n", &[("o", o()), ("k", k.clone())]);
            let got = show(vm_run(&getindex, vec![o(), k.clone()]).1);
            assert_eq!(
                got,
                want,
                "getindex {} {}",
                data_objects()[i].repr(),
                k.repr()
            );

            let want = walked(
                "o[k] = 9\nexhale repr(o)\n",
                &[("o", o()), ("k", k.clone())],
            );
            let got = show(vm_run(&setindex, vec![o(), k.clone()]).1);
            assert_eq!(
                got,
                want,
                "setindex {} {}",
                data_objects()[i].repr(),
                k.repr()
            );
            compared += 2;
        }
        for name in ["a", "zz", "len", "abs", "name"] {
            let mut f = main_fn(2, vec![abc(Op::GETFIELD, 1, 0, 0), ret(1)]);
            f.arity = 1;
            f.constants = vec![Constant::Str(name.into())];
            let o = || data_objects().swap_remove(i);
            let want = walked(&format!("exhale repr(o.{name})\n"), &[("o", o())]);
            let got = show(vm_run(&module(f), vec![o()]).1);
            assert_eq!(got, want, "getfield {} .{name}", data_objects()[i].repr());

            let mut f = main_fn(
                2,
                vec![
                    Instr::asbx(Op::LOADINT, 1, 9),
                    abc(Op::SETFIELD, 0, 0, 1),
                    ret(0),
                ],
            );
            f.arity = 1;
            f.constants = vec![Constant::Str(name.into())];
            let want = walked(&format!("o.{name} = 9\nexhale repr(o)\n"), &[("o", o())]);
            let got = show(vm_run(&module(f), vec![o()]).1);
            assert_eq!(got, want, "setfield {} .{name}", data_objects()[i].repr());
            compared += 2;
        }
    }
    assert_eq!(compared, n_objects * (keys.len() + 5) * 2);
    // Each error is at the instruction.
    let e = vm_run(&getindex, vec![Value::list(vec![]), Value::Int(0)])
        .1
        .err()
        .expect("E504");
    assert_eq!((e.code, at_pc(&e)), ("E504", Some(0)));
    let e = vm_run(&setindex, vec![Value::Int(1), Value::Int(0)])
        .1
        .err()
        .expect("E405");
    assert_eq!((e.code, at_pc(&e)), ("E405", Some(1)));
}

// ----- method calls ------------------------------------------------------------------------

#[test]
fn callm_calls_the_interpreters_methods() {
    let mut fm = IndexMap::new();
    fm.insert(
        "f".to_string(),
        Value::Builtin(Rc::new(
            *crate::stdlib::builtin_catalogue()[catalogue_index("len")],
        )),
    );
    let math = crate::stdlib::prelude()
        .into_iter()
        .find(|(n, _)| *n == "math")
        .map(|(_, m)| m)
        .expect("math");
    let list = |xs: &[i64]| Value::list(xs.iter().map(|x| Value::Int(*x)).collect());
    // Each engine gets its own copy of the receiver: `push` changes it.
    let copy = |v: &Value| match v {
        Value::List(l) => Value::list(l.borrow().clone()),
        other => other.clone(),
    };
    let cases: Vec<(Value, &str, Vec<Value>)> = vec![
        (list(&[3, 1, 2]), "sort", vec![]),
        (Value::str("abc"), "upper", vec![]),
        (list(&[1, 2]), "push", vec![Value::Int(3)]),
        (list(&[1, 2]), "push", vec![]),
        (Value::map(fm), "f", vec![Value::str("abcd")]),
        (math.clone(), "abs", vec![Value::Int(-3)]),
        (math, "nope", vec![]),
        (Value::str("abc"), "uper", vec![]),
        (Value::Null, "upper", vec![]),
        (list(&[1, 2]), "slice", vec![Value::Int(1), Value::Int(2)]),
    ];
    for (recv, name, args) in cases {
        let n = args.len();
        let mut f = main_fn(1 + n as u32, vec![abc(Op::CALLM, 0, 0, n as u8), ret(0)]);
        f.arity = 1 + n as u16;
        f.constants = vec![Constant::Str(name.into())];
        let names: Vec<String> = (0..n).map(|i| format!("a{i}")).collect();
        let mut vars = vec![("r", copy(&recv))];
        for (i, a) in args.iter().enumerate() {
            vars.push((names[i].as_str(), a.clone()));
        }
        let src = format!("exhale repr(r.{name}({}))\n", names.join(", "));
        let want = match walker_run(&src, &vars) {
            (out, None) => out.trim_end_matches('\n').to_string(),
            (_, Some(e)) => said(&e),
        };
        let mut argv = vec![recv];
        argv.extend(args);
        let got = match vm_run(&module(f), argv).1 {
            Ok(v) => v.repr(),
            Err(e) => {
                assert_eq!(at_pc(&e), Some(0), "{}", e.message);
                said(&e)
            }
        };
        assert_eq!(got, want, "{src}");
    }
}

// ----- closures and cells -----------------------------------------------------------------

fn captures(kind: UpvalKind, index: u16) -> UpvalDesc {
    UpvalDesc { kind, index }
}

/// `pack() => <upvalue 0>`.
fn reads_upvalue(kind: UpvalKind, index: u16) -> BcFunction {
    let mut c = child_fn("", 0, 1, vec![abc(Op::CELLGET, 0, 0, 0), ret(0)]);
    c.kind = FuncKind::Lambda;
    c.upvalues = vec![captures(kind, index)];
    c
}

/// `for i in 0..3 { <per-turn> fs.push(pack() => <i or k>) }` then
/// `for f in fs { exhale f() }`. `fresh` gives each turn its own cell for
/// `i` (the tree-walker's scope per turn); otherwise the body writes `i`
/// into `k` (R12), one variable declared before the loop.
fn closures_in_a_loop(fresh: bool) -> Module {
    let body = if fresh {
        vec![abc(Op::CELLNEW, 4, 0, 0), abc(Op::NOP, 0, 0, 0)]
    } else {
        vec![abc(Op::NOP, 0, 0, 0), abc(Op::MOV, 12, 4, 0)]
    };
    let mut code = vec![
        abc(Op::NEWLIST, 0, 0, 0),      // pc 0: fs
        abc(Op::CELLNEW, 12, 0, 0),     // pc 1: roll k (a captured variable)
        Instr::asbx(Op::LOADINT, 1, 0), // pc 2
        Instr::asbx(Op::LOADINT, 2, 3), // pc 3
        Instr::asbx(Op::LOADINT, 3, 1), // pc 4
        steps(1, 0),                    // pc 5
        Instr::asbx(Op::FORPREP, 1, 7), // pc 6 -> 14
        steps(1, 1),                    // pc 7
    ];
    code.extend(body); // pc 8, 9
    code.extend([
        Instr::abx(Op::CLOSURE, 6, 0),   // pc 10
        abc(Op::MOV, 5, 0, 0),           // pc 11
        abc(Op::CALLM, 5, 0, 1),         // pc 12: fs.push(R6)
        Instr::asbx(Op::FORLOOP, 1, -7), // pc 13 -> 7
        steps(0, 0),                     // pc 14
        abc(Op::MOV, 7, 0, 0),           // pc 15
        steps(1, 0),                     // pc 16
        abc(Op::ITERPREP, 7, 0, 0),      // pc 17
        Instr::asbx(Op::ITERNEXT, 7, 5), // pc 18 -> 24
        steps(1, 1),                     // pc 19
        abc(Op::MOV, 11, 10, 0),         // pc 20
        abc(Op::CALL, 11, 0, 0),         // pc 21
        abc(Op::EXHALE, 11, 1, 0),       // pc 22
        Instr::asbx(Op::LOOP, 0, -6),    // pc 23 -> 18
        steps(0, 0),                     // pc 24
        ret(0),
    ]);
    let mut f = main_fn(13, code);
    f.constants = vec![Constant::Str("push".into())];
    f.children = vec![reads_upvalue(
        UpvalKind::ParentLocal,
        if fresh { 4 } else { 12 },
    )];
    module(f)
}

#[test]
fn closures_capture_per_iteration() {
    let (want, e) = walker_run(
        "roll fs = []\nfor i in 0..3 {\n  fs.push(pack() => i)\n}\nfor f in fs {\n  exhale f()\n}\n",
        &[],
    );
    assert!(e.is_none());
    assert_eq!(want, "0\n1\n2\n");
    let (got, r) = vm_run(&closures_in_a_loop(true), Vec::new());
    assert!(r.is_ok(), "{:?}", r.err().map(|e| e.message));
    assert_eq!(got, want);
    // One variable from before the loop: every closure sees its last value.
    let (want, e) = walker_run(
        "roll fs = []\nroll k = 0\nfor i in 0..3 {\n  k = i\n  fs.push(pack() => k)\n}\nfor f in fs {\n  exhale f()\n}\n",
        &[],
    );
    assert!(e.is_none());
    assert_eq!(want, "2\n2\n2\n");
    let (got, r) = vm_run(&closures_in_a_loop(false), Vec::new());
    assert!(r.is_ok());
    assert_eq!(got, want);
}

#[test]
fn cells_share_a_variable_between_a_function_and_its_closures() {
    // roll n = 0; pull inc() { n = n + 1 }; inc(); inc(); exhale n
    let (want, _) = walker_run(
        "roll n = 0\npull inc() {\n  n = n + 1\n}\ninc()\ninc()\nexhale n\n",
        &[],
    );
    let mut inc = child_fn(
        "inc",
        0,
        2,
        vec![
            abc(Op::CELLGET, 0, 0, 0),
            abc(Op::ADDK, 0, 0, 0),
            abc(Op::CELLSET, 0, 0, 0),
            abc(Op::LOADNULL, 1, 0, 0),
            ret(1),
        ],
    );
    inc.constants = vec![Constant::Int(1)];
    inc.upvalues = vec![captures(UpvalKind::ParentLocal, 0)];
    let mut f = main_fn(
        3,
        vec![
            Instr::asbx(Op::LOADINT, 0, 0),
            abc(Op::CELLNEW, 0, 0, 0),
            Instr::abx(Op::CLOSURE, 1, 0),
            abc(Op::MOV, 2, 1, 0),
            abc(Op::CALL, 2, 0, 0),
            abc(Op::MOV, 2, 1, 0),
            abc(Op::CALL, 2, 0, 0),
            abc(Op::EXHALE, 0, 1, 0),
            ret(0),
        ],
    );
    f.children = vec![inc];
    let (got, r) = vm_run(&module(f), Vec::new());
    assert_eq!(got, want);
    assert_eq!(r.ok().map(|v| v.repr()), Some("2".to_string()));

    // The parent's writes reach the closure too, and a closure of a closure
    // shares through its parent's upvalue (getup/setup, kind 1).
    let mut grand = child_fn(
        "",
        0,
        1,
        vec![
            Instr::asbx(Op::LOADINT, 0, 40),
            abc(Op::SETUP, 0, 0, 0),
            abc(Op::GETUP, 0, 0, 0),
            ret(0),
        ],
    );
    grand.kind = FuncKind::Lambda;
    grand.upvalues = vec![captures(UpvalKind::ParentUpvalue, 0)];
    let mut middle = child_fn("", 0, 1, vec![Instr::abx(Op::CLOSURE, 0, 0), ret(0)]);
    middle.kind = FuncKind::Lambda;
    middle.upvalues = vec![captures(UpvalKind::ParentLocal, 0)];
    middle.children = vec![grand];
    let mut f = main_fn(
        3,
        vec![
            abc(Op::CELLNEW, 0, 0, 0),
            Instr::abx(Op::CLOSURE, 1, 0),  // middle, over R0's cell
            Instr::asbx(Op::LOADINT, 0, 2), // the parent writes the cell
            abc(Op::CALL, 1, 0, 0),         // R1 = grand
            abc(Op::CALL, 1, 0, 0),         // grand sets the cell to 40
            abc(Op::ADD, 2, 0, 1),          // 40 + 40
            ret(2),
        ],
    );
    f.children = vec![middle];
    assert_eq!(ok(&module(f)), "80");

    // A register with no cell is captured as a copy: the closure's writes
    // stay its own, and later writes of the parent do not reach it.
    let mut own = child_fn(
        "",
        0,
        1,
        vec![
            abc(Op::GETUP, 0, 0, 0),
            abc(Op::ADDK, 0, 0, 0),
            abc(Op::SETUP, 0, 0, 0),
            ret(0),
        ],
    );
    own.kind = FuncKind::Lambda;
    own.constants = vec![Constant::Int(1)];
    own.upvalues = vec![captures(UpvalKind::ParentLocal, 0)];
    let mut f = main_fn(
        4,
        vec![
            Instr::asbx(Op::LOADINT, 0, 10),
            Instr::abx(Op::CLOSURE, 1, 0),
            Instr::asbx(Op::LOADINT, 0, 100),
            abc(Op::MOV, 2, 1, 0),
            abc(Op::CALL, 2, 0, 0), // 11
            abc(Op::MOV, 3, 1, 0),
            abc(Op::CALL, 3, 0, 0), // 12
            abc(Op::ADD, 2, 2, 3),
            abc(Op::ADD, 2, 2, 0), // 11 + 12 + 100
            ret(2),
        ],
    );
    f.children = vec![own];
    assert_eq!(ok(&module(f)), "123");

    // A pull that calls itself: its closure captures its own cell.
    let mut fact = child_fn(
        "fact",
        1,
        4,
        vec![
            Instr::asbx(Op::LOADINT, 1, 1),
            abc(Op::LEJ, 0, 1, 4), // n <= 1 -> ret 1
            abc(Op::CELLGET, 2, 0, 0),
            abc(Op::SUBK, 3, 0, 0),
            abc(Op::CALL, 2, 1, 0),
            abc(Op::MUL, 1, 0, 2),
            ret(1),
        ],
    );
    fact.constants = vec![Constant::Int(1)];
    fact.upvalues = vec![captures(UpvalKind::ParentLocal, 0)];
    let mut f = main_fn(
        3,
        vec![
            abc(Op::CELLNEW, 0, 0, 0),
            Instr::abx(Op::CLOSURE, 0, 0),
            abc(Op::MOV, 1, 0, 0),
            Instr::asbx(Op::LOADINT, 2, 10),
            abc(Op::CALL, 1, 1, 0),
            ret(1),
        ],
    );
    f.children = vec![fact];
    assert_eq!(ok(&module(f)), "3628800");
}

// ----- re-entry ------------------------------------------------------------------------------

/// `pack(x) => x * 10 + k`, `k` captured from the parent's R(k_reg).
fn times_ten_plus(k_reg: u16) -> BcFunction {
    let mut c = child_fn(
        "",
        1,
        3,
        vec![
            abc(Op::MULK, 1, 0, 0),
            abc(Op::CELLGET, 2, 0, 0),
            abc(Op::ADD, 1, 1, 2),
            ret(1),
        ],
    );
    c.kind = FuncKind::Lambda;
    c.constants = vec![Constant::Int(10)];
    c.upvalues = vec![captures(UpvalKind::ParentLocal, k_reg)];
    c
}

#[test]
fn a_builtin_calls_back_a_compiled_function() {
    // R0 = xs (argument); R1 = k = 5; R3 = xs.map(f) with f over k.
    let mut f = main_fn(
        5,
        vec![
            Instr::asbx(Op::LOADINT, 1, 5),
            Instr::abx(Op::CLOSURE, 4, 0),
            abc(Op::MOV, 3, 0, 0),
            abc(Op::CALLM, 3, 0, 1),
            ret(3),
        ],
    );
    f.arity = 1;
    f.constants = vec![Constant::Str("map".into())];
    f.children = vec![times_ten_plus(1)];
    let xs = Value::list(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
    let (_, r) = vm_run(&module(f.clone()), vec![xs]);
    assert_eq!(r.ok().map(|v| v.repr()), Some("[15, 25, 35]".to_string()));
    // sort_by with a compiled key.
    f.constants = vec![Constant::Str("sort_by".into())];
    let xs = Value::list(vec![Value::Int(-3), Value::Int(1), Value::Int(-2)]);
    let (_, r) = vm_run(&module(f.clone()), vec![xs]);
    assert_eq!(r.ok().map(|v| v.repr()), Some("[-3, -2, 1]".to_string()));
    // An error inside the callback comes out with its own code.
    f.constants = vec![Constant::Str("map".into())];
    let xs = Value::list(vec![Value::Int(1), Value::str("x")]);
    let (_, r) = vm_run(&module(f), vec![xs]);
    let e = r.err().expect("an error");
    assert_eq!(e.code, "E403");
}

#[test]
fn compiled_and_interpreted_functions_are_interchangeable() {
    // A compiled `add` and a compiled closure, as values.
    let mut f = main_fn(1, vec![Instr::abx(Op::CLOSURE, 0, 0), ret(0)]);
    f.children = vec![add_fn()];
    let add = exec(&module(f)).expect("add");
    let mut f = main_fn(
        3,
        vec![
            Instr::asbx(Op::LOADINT, 1, 7),
            Instr::abx(Op::CLOSURE, 0, 0),
            ret(0),
        ],
    );
    f.children = vec![times_ten_plus(1)];
    let scaled = exec(&module(f)).expect("a closure");

    // The tree-walker calls them as it calls its own.
    let src = "exhale add(2, 40)\nexhale [1, 2].map(scaled)\nexhale [[1, 2], [3, 4]].map(pack(p) => add(p[0], p[1]))\nexhale repr(add), type_of(scaled)\n";
    let (out, e) = walker_run(src, &[("add", add.clone()), ("scaled", scaled)]);
    assert!(e.is_none(), "{:?}", e.map(|e| e.message));
    assert_eq!(out, "42\n[17, 27]\n[3, 7]\n<pull add> function\n");
    // ... with the same errors: arity (E401) at the call.
    let (_, compiled) = walker_run("add(1)\n", &[("add", add.clone())]);
    let (_, walked) = walker_run("pull add(a, b) {\n  snuff a + b\n}\nadd(1)\n", &[]);
    let (c, w) = (compiled.expect("E401"), walked.expect("E401"));
    assert_eq!(said(&c), said(&w));
    assert_eq!(c.line, Some(1));
    // A compiled function's error inside a callback comes out of the
    // tree-walker with its code.
    let (_, e) = walker_run(
        "[\"x\"].map(pack(s) => add(s, 1))\n",
        &[("add", add.clone())],
    );
    assert_eq!(e.expect("E403").code, "E403");

    // The VM calls an interpreted function handed to it, and a compiled
    // function of another program, through the interpreter.
    let (_, e) = walker_run("roll g = pack(x) => x * 3\n", &[]);
    assert!(e.is_none());
    let program = crate::syntax::parse("roll g = pack(x) => x * 3\nexhale g\n").expect("parses");
    let mut i = interp();
    i.run_program(&program).expect("runs");
    let g = i.globals.get("g").expect("g");
    let mut f = main_fn(
        3,
        vec![
            Instr::asbx(Op::LOADINT, 2, 14),
            abc(Op::MOV, 1, 0, 0),
            abc(Op::CALL, 1, 1, 0),
            ret(1),
        ],
    );
    f.arity = 1;
    let m = module(f);
    assert_eq!(
        exec_with(&m, vec![g]).ok().map(|v| v.repr()),
        Some("42".to_string())
    );
    let mut f = main_fn(
        4,
        vec![
            Instr::asbx(Op::LOADINT, 2, 14),
            Instr::asbx(Op::LOADINT, 3, 28),
            abc(Op::MOV, 1, 0, 0),
            abc(Op::CALL, 1, 2, 0),
            ret(1),
        ],
    );
    f.arity = 1;
    assert_eq!(
        exec_with(&module(f), vec![add]).ok().map(|v| v.repr()),
        Some("42".to_string())
    );
}

#[test]
fn a_reentered_call_counts_depth_as_a_call() {
    // pull f(n) { snuff [n].map(f) } recurses through a builtin until E506.
    let mut rec = child_fn(
        "f",
        1,
        4,
        vec![
            abc(Op::NEWLIST, 1, 1, 0),
            abc(Op::APPENDLIST, 1, 0, 0),
            abc(Op::CELLGET, 2, 0, 0),
            abc(Op::CALLM, 1, 0, 1),
            ret(1),
        ],
    );
    rec.constants = vec![Constant::Str("map".into())];
    rec.upvalues = vec![captures(UpvalKind::ParentLocal, 0)];
    let mut f = main_fn(
        3,
        vec![
            abc(Op::CELLNEW, 0, 0, 0),
            Instr::abx(Op::CLOSURE, 0, 0),
            abc(Op::MOV, 1, 0, 0),
            Instr::asbx(Op::LOADINT, 2, 0),
            abc(Op::CALL, 1, 1, 0),
            ret(1),
        ],
    );
    f.children = vec![rec];
    let m = module(f);
    // Each level is a builtin and a VM again on the Rust stack: 4000 of
    // them fit in the stack `cig` gives its evaluator (256 MiB), even
    // unoptimized.
    let e = std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let mut i = interp();
            let r = run(&mut i, &program(&m));
            assert_eq!(i.call_depth_for_tests(), 0, "the depth is given back");
            r.err().map(|e| (e.code, e.message))
        })
        .expect("thread")
        .join()
        .expect("joined")
        .expect("E506");
    assert_eq!(e.0, "E506");
    assert_eq!(
        e.1,
        "call depth exceeded 4000; is there unbounded recursion?"
    );
}

// ----- try, catch, finally ------------------------------------------------------------------

/// A function written with labels: jumps and handler pcs name a label,
/// resolved by [`Asm::finish`]. Positions are given as source spans, each
/// a line row at the next instruction.
#[derive(Default)]
struct Asm {
    code: Vec<Instr>,
    fixups: Vec<(usize, &'static str)>,
    labels: std::collections::HashMap<&'static str, u32>,
    lines: Vec<LineEntry>,
    consts: Vec<Constant>,
    /// (start, end, catch, catch register, finally).
    handlers: Vec<(
        &'static str,
        &'static str,
        &'static str,
        u32,
        Option<&'static str>,
    )>,
    children: Vec<BcFunction>,
}

impl Asm {
    fn k(&mut self, c: Constant) -> u16 {
        let i = self.consts.iter().position(|x| *x == c).unwrap_or_else(|| {
            self.consts.push(c);
            self.consts.len() - 1
        });
        i as u16
    }
    fn s(&mut self, s: &str) -> u16 {
        self.k(Constant::Str(s.to_string()))
    }
    fn i(&mut self, i: Instr) -> &mut Self {
        self.code.push(i);
        self
    }
    fn op(&mut self, op: Op, a: u8, b: u8, c: u8) -> &mut Self {
        self.i(abc(op, a, b, c))
    }
    fn label(&mut self, l: &'static str) -> &mut Self {
        self.labels.insert(l, self.code.len() as u32);
        self
    }
    /// An AsBx jump (`jmp`, `loop`, `test`, `testn`) to `l`.
    fn jump(&mut self, op: Op, a: u8, l: &'static str) -> &mut Self {
        self.fixups.push((self.code.len(), l));
        self.i(Instr::asbx(op, a, 0))
    }
    /// A line row for the next instruction.
    fn at(&mut self, s: Span) -> &mut Self {
        let pc = self.code.len() as u32;
        self.lines.retain(|r| r.pc != pc);
        self.lines.push(row(pc, s));
        self
    }
    fn loadk(&mut self, r: u8, c: Constant) -> &mut Self {
        let k = self.k(c);
        self.i(Instr::abx(Op::LOADK, r, k))
    }
    fn exhale_str(&mut self, r: u8, s: &str) -> &mut Self {
        self.loadk(r, Constant::Str(s.to_string()))
            .op(Op::EXHALE, r, 1, 0)
    }
    fn getfield(&mut self, a: u8, b: u8, name: &str) -> &mut Self {
        let k = self.s(name) as u8;
        self.op(Op::GETFIELD, a, b, k)
    }
    fn addk(&mut self, a: u8, b: u8, n: i64) -> &mut Self {
        let k = self.k(Constant::Int(n)) as u8;
        self.op(Op::ADDK, a, b, k)
    }
    fn eqk(&mut self, a: u8, b: u8, n: i64) -> &mut Self {
        let k = self.k(Constant::Int(n)) as u8;
        self.op(Op::EQK, a, b, k)
    }
    fn try_enter(&mut self, h: u16) -> &mut Self {
        self.i(Instr::abx(Op::TRY_ENTER, 0, h))
    }
    fn try_leave(&mut self) -> &mut Self {
        self.op(Op::TRY_LEAVE, 0, 0, 0)
    }
    fn handler(
        &mut self,
        start: &'static str,
        end: &'static str,
        catch: &'static str,
        catch_reg: u32,
        finally: Option<&'static str>,
    ) -> &mut Self {
        self.handlers.push((start, end, catch, catch_reg, finally));
        self
    }
    fn finish(&mut self, name: &str, arity: u16, regs: u32) -> BcFunction {
        let at = |l: &str| *self.labels.get(l).unwrap_or_else(|| panic!("no label {l}"));
        let mut code = self.code.clone();
        for &(pc, l) in &self.fixups {
            let off = at(l) as i64 - pc as i64 - 1;
            let i = code[pc];
            code[pc] = Instr::asbx(i.op().expect("op"), i.a(), off as i16);
        }
        let mut f = if name.is_empty() {
            main_fn(regs, vec![])
        } else {
            child_fn(name, arity, regs, vec![])
        };
        f.span_base = SpanBase {
            byte_offset: 0,
            line: 1,
            col: 1,
        };
        f.instrs = code;
        let mut lines = self.lines.clone();
        lines.sort_by_key(|r| r.pc);
        f.lines = lines;
        f.constants = self.consts.clone();
        f.children = std::mem::take(&mut self.children);
        f.handlers = self
            .handlers
            .iter()
            .map(|&(s, e, c, r, fin)| HandlerEntry {
                pc_start: at(s),
                pc_end: at(e),
                handler_pc: at(c),
                catch_reg: r,
                finally_pc: fin.map(at).unwrap_or(RESERVED_U32),
            })
            .collect();
        f
    }
}

/// The span of the first `needle` in `src` (the n-th with `nth`), as the
/// parser gives it to the expression or statement that is that text (a
/// statement's span takes in the newline that ends it).
fn span_of(src: &str, needle: &str) -> Span {
    span_nth(src, needle, 0)
}

fn span_nth(src: &str, needle: &str, nth: usize) -> Span {
    let start = src
        .match_indices(needle)
        .nth(nth)
        .unwrap_or_else(|| panic!("no {needle:?} in the source"))
        .0;
    let line = src[..start].matches('\n').count() as u32 + 1;
    let col = src[..start].rfind('\n').map_or(start, |nl| start - nl - 1) as u32 + 1;
    Span::new(start, start + needle.len(), line, col)
}

/// How a run ended, as the two engines are compared on it: an `exit()`
/// by its code; an error by code, kind, message, hint and position.
fn ending(r: Option<Diagnostic>, exit: Option<i32>) -> String {
    match (exit, r) {
        (Some(code), _) => format!("exit {code}"),
        (None, None) => "ok".to_string(),
        (None, Some(d)) => format!(
            "{} {:?} {} {:?} {:?}:{:?} {:?}",
            d.code, d.kind, d.message, d.hint, d.line, d.col, d.span
        ),
    }
}

/// Run `src` on the tree-walker and `m` on the VM; both must print the
/// same bytes and end the same way. Returns what was printed.
fn agree(src: &str, m: &Module) -> String {
    let ast = crate::syntax::parse(src).expect("parses");
    let report = crate::check::check(&ast, &[]);
    assert!(report.ok(), "{src}: {:?}", report.errors);
    let wbuf = Buf::default();
    let mut w = interp();
    w.out = Box::new(wbuf.clone());
    let wr = w.run_program(&ast).err();
    let vbuf = Buf::default();
    let mut v = interp();
    v.out = Box::new(vbuf.clone());
    let vr = run(&mut v, &program(m)).err();
    assert_eq!(v.loop_depth, 0, "{src}: the loop depth is given back");
    assert_eq!(
        v.call_depth_for_tests(),
        0,
        "{src}: the depth is given back"
    );
    assert_eq!(vbuf.text(), wbuf.text(), "{src}: printed");
    assert_eq!(
        ending(vr, v.exit_requested),
        ending(wr, w.exit_requested),
        "{src}: ending"
    );
    wbuf.text()
}

/// The `try` programs: CigScript, and the same by hand in bytecode.
fn try_programs() -> Vec<(&'static str, Module, &'static str)> {
    let mut out = Vec::new();
    let r = RESERVED_U32;

    // Fall-through: the finally runs, then the code after the try.
    let src = "try {\n  exhale \"a\"\n} finally {\n  exhale \"f\"\n}\nexhale \"end\"\n";
    let mut a = Asm::default();
    a.try_enter(0)
        .label("s")
        .exhale_str(0, "a")
        .try_leave()
        .label("e");
    a.jump(Op::JMP, 0, "end");
    a.label("fin").exhale_str(0, "f").try_leave();
    a.label("end")
        .exhale_str(0, "end")
        .op(Op::LOADNULL, 0, 0, 0)
        .i(ret(0));
    a.handler("s", "e", "fin", r, Some("fin"));
    out.push((src, module(a.finish("", 0, 2)), "a\nf\nend\n"));

    // An error: the catch reads it, then the finally runs.
    let src = "try {\n  exhale \"a\"\n  cough \"boom\"\n  exhale \"no\"\n} ashtray e {\n  exhale e.message\n} finally {\n  exhale \"f\"\n}\n";
    let mut a = Asm::default();
    a.try_enter(0).label("s").exhale_str(0, "a");
    a.loadk(0, Constant::Str("boom".into()))
        .at(span_of(src, "cough \"boom\"\n"))
        .op(Op::THROW, 0, 0, 0);
    a.exhale_str(0, "no")
        .try_leave()
        .label("e")
        .jump(Op::JMP, 0, "end");
    a.label("catch")
        .getfield(0, 1, "message")
        .op(Op::EXHALE, 0, 1, 0);
    a.try_leave().jump(Op::JMP, 0, "end");
    a.label("fin").exhale_str(0, "f").try_leave();
    a.label("end").op(Op::LOADNULL, 0, 0, 0).i(ret(0));
    a.handler("s", "e", "catch", 1, Some("fin"));
    out.push((src, module(a.finish("", 0, 2)), "a\nboom\nf\n"));

    // An error no catch takes: the finally runs and the error goes on,
    // from where it was raised.
    let src = "try {\n  exhale 1 / 0\n} finally {\n  exhale \"f\"\n}\nexhale \"no\"\n";
    let mut a = Asm::default();
    a.try_enter(0).label("s");
    a.i(Instr::asbx(Op::LOADINT, 1, 1))
        .i(Instr::asbx(Op::LOADINT, 2, 0));
    a.at(span_of(src, "1 / 0")).op(Op::DIV, 0, 1, 2);
    a.op(Op::EXHALE, 0, 1, 0)
        .try_leave()
        .label("e")
        .jump(Op::JMP, 0, "end");
    a.label("fin").exhale_str(0, "f").try_leave();
    a.label("end")
        .exhale_str(0, "no")
        .op(Op::LOADNULL, 0, 0, 0)
        .i(ret(0));
    a.handler("s", "e", "fin", r, Some("fin"));
    out.push((src, module(a.finish("", 0, 3)), "f\n"));

    // `break` and `continue` out of a try in a loop: the finally runs on
    // the way out of the turn.
    for (word, want) in [
        ("break", "1\nf\nf\nend\n"),
        ("continue", "1\nf\nf\n3\nf\nend\n"),
    ] {
        let src: &'static str = Box::leak(
            format!(
                "roll i = 0\nwhile i < 3 {{\n  i = i + 1\n  try {{\n    if i == 2 {{\n      {word}\n    }}\n    exhale i\n  }} finally {{\n    exhale \"f\"\n  }}\n}}\nexhale \"end\"\n"
            )
            .into_boxed_str(),
        );
        let mut a = Asm::default();
        a.i(Instr::asbx(Op::LOADINT, 0, 0)).i(steps(1, 0));
        a.label("top")
            .i(Instr::asbx(Op::LOADINT, 1, 3))
            .op(Op::LT, 2, 0, 1);
        a.jump(Op::TESTN, 2, "out").i(steps(1, 1)).addk(0, 0, 1);
        a.try_enter(0)
            .label("s")
            .eqk(2, 0, 2)
            .jump(Op::TESTN, 2, "skip");
        a.try_leave()
            .jump(Op::JMP, 0, if word == "break" { "out" } else { "cont" });
        a.label("skip").op(Op::EXHALE, 0, 1, 0).try_leave();
        a.label("e").jump(Op::JMP, 0, "cont");
        a.label("fin").exhale_str(1, "f").try_leave();
        a.label("cont").jump(Op::LOOP, 0, "top");
        a.label("out").i(steps(0, 0)).exhale_str(1, "end");
        a.op(Op::LOADNULL, 1, 0, 0).i(ret(1));
        a.handler("s", "e", "fin", r, Some("fin"));
        out.push((src, module(a.finish("", 0, 3)), want));
    }

    // `snuff` out of a try in a loop in a pull: the finally runs, and the
    // value snuffed is the one before it ran.
    let src = "pull f() {\n  roll i = 0\n  while true {\n    i = i + 1\n    try {\n      if i == 3 {\n        snuff i\n      }\n    } finally {\n      exhale \"f\", i\n      i = i + 10\n    }\n    i = i - 10\n  }\n}\nexhale f()\n";
    let mut c = Asm::default();
    c.i(Instr::asbx(Op::LOADINT, 0, 0)).i(steps(1, 0));
    c.label("top").i(steps(1, 1)).addk(0, 0, 1);
    c.try_enter(0)
        .label("s")
        .eqk(1, 0, 3)
        .jump(Op::TESTN, 1, "skip");
    c.op(Op::MOV, 3, 0, 0).try_leave().i(ret(3));
    c.label("skip")
        .try_leave()
        .label("e")
        .jump(Op::JMP, 0, "cont");
    c.label("fin")
        .loadk(1, Constant::Str("f".into()))
        .op(Op::MOV, 2, 0, 0);
    c.op(Op::EXHALE, 1, 2, 0).addk(0, 0, 10).try_leave();
    c.label("cont").addk(0, 0, -10).jump(Op::LOOP, 0, "top");
    c.op(Op::LOADNULL, 1, 0, 0).i(ret(1));
    c.handler("s", "e", "fin", r, Some("fin"));
    let mut a = Asm::default();
    a.children.push(c.finish("f", 0, 4));
    a.i(Instr::abx(Op::CLOSURE, 0, 0)).op(Op::CALL, 0, 0, 0);
    a.op(Op::EXHALE, 0, 1, 0).i(ret(0));
    out.push((src, module(a.finish("", 0, 1)), "f 1\nf 2\nf 3\n3\n"));

    // Nested: the inner catch raises, the inner finally runs, the outer
    // catch takes the new error.
    let src = "try {\n  try {\n    cough \"in\"\n  } ashtray e {\n    cough \"re \" + e.message\n  } finally {\n    exhale \"inner f\"\n  }\n} ashtray e {\n  exhale e.message, e.line\n}\nexhale \"end\"\n";
    let mut a = Asm::default();
    a.try_enter(0).label("os").try_enter(1).label("is");
    a.loadk(0, Constant::Str("in".into()))
        .at(span_of(src, "cough \"in\"\n"))
        .op(Op::THROW, 0, 0, 0);
    a.try_leave().label("ie").jump(Op::JMP, 0, "iend");
    a.label("icatch").loadk(0, Constant::Str("re ".into()));
    a.getfield(1, 2, "message")
        .at(span_of(src, "\"re \" + e.message"));
    a.op(Op::ADD, 0, 0, 1);
    a.at(span_of(src, "cough \"re \" + e.message\n"))
        .op(Op::THROW, 0, 0, 0);
    a.try_leave().jump(Op::JMP, 0, "iend");
    a.label("ifin").exhale_str(0, "inner f").try_leave();
    a.label("iend")
        .try_leave()
        .label("oe")
        .jump(Op::JMP, 0, "end");
    a.label("ocatch")
        .getfield(0, 3, "message")
        .getfield(1, 3, "line");
    a.op(Op::EXHALE, 0, 2, 0)
        .try_leave()
        .jump(Op::JMP, 0, "end");
    a.label("end")
        .exhale_str(0, "end")
        .op(Op::LOADNULL, 0, 0, 0)
        .i(ret(0));
    a.handler("os", "oe", "ocatch", 3, None);
    a.handler("is", "ie", "icatch", 2, Some("ifin"));
    out.push((src, module(a.finish("", 0, 4)), "inner f\nre in 5\nend\n"));

    // A finally that raises replaces the pending error: caught, and not.
    let src = "try {\n  try {\n    cough \"first\"\n  } finally {\n    cough \"second\"\n  }\n} ashtray e {\n  exhale e.message, e.line\n}\n";
    let mut a = Asm::default();
    a.try_enter(0).label("os").try_enter(1).label("is");
    a.loadk(0, Constant::Str("first".into()))
        .at(span_of(src, "cough \"first\"\n"))
        .op(Op::THROW, 0, 0, 0);
    a.try_leave().label("ie").jump(Op::JMP, 0, "iend");
    a.label("ifin").loadk(0, Constant::Str("second".into()));
    a.at(span_of(src, "cough \"second\"\n"))
        .op(Op::THROW, 0, 0, 0)
        .try_leave();
    a.label("iend")
        .try_leave()
        .label("oe")
        .jump(Op::JMP, 0, "end");
    a.label("ocatch")
        .getfield(0, 2, "message")
        .getfield(1, 2, "line");
    a.op(Op::EXHALE, 0, 2, 0).try_leave();
    a.label("end").op(Op::LOADNULL, 0, 0, 0).i(ret(0));
    a.handler("os", "oe", "ocatch", 2, None);
    a.handler("is", "ie", "ifin", r, Some("ifin"));
    out.push((src, module(a.finish("", 0, 3)), "second 5\n"));

    let src = "try {\n  cough \"first\"\n} finally {\n  exhale \"f\"\n  cough \"second\"\n}\n";
    let mut a = Asm::default();
    a.try_enter(0)
        .label("s")
        .loadk(0, Constant::Str("first".into()));
    a.at(span_of(src, "cough \"first\"\n"))
        .op(Op::THROW, 0, 0, 0);
    a.try_leave().label("e").jump(Op::JMP, 0, "end");
    a.label("fin")
        .exhale_str(0, "f")
        .loadk(0, Constant::Str("second".into()));
    a.at(span_of(src, "cough \"second\"\n"))
        .op(Op::THROW, 0, 0, 0)
        .try_leave();
    a.label("end").op(Op::LOADNULL, 0, 0, 0).i(ret(0));
    a.handler("s", "e", "fin", r, Some("fin"));
    out.push((src, module(a.finish("", 0, 1)), "f\n"));

    // An error raised in a callee, and in a builtin's callback, is caught
    // by the caller's try, with the position it was raised at.
    let bad_src = "pull bad(x) {\n  snuff 10 / x\n}\n";
    let bad = |src: &str| {
        let mut b = Asm::default();
        b.i(Instr::asbx(Op::LOADINT, 1, 10));
        b.at(span_of(src, "10 / x")).op(Op::DIV, 2, 1, 0).i(ret(2));
        b.finish("bad", 1, 3)
    };
    let src: &'static str = Box::leak(
        format!(
            "{bad_src}try {{\n  exhale bad(0)\n}} ashtray e {{\n  exhale e.message, e.line\n}}\n"
        )
        .into_boxed_str(),
    );
    let mut a = Asm::default();
    a.children.push(bad(src));
    a.i(Instr::abx(Op::CLOSURE, 0, 0)).try_enter(0).label("s");
    a.op(Op::MOV, 1, 0, 0).i(Instr::asbx(Op::LOADINT, 2, 0));
    a.at(span_of(src, "bad(0)")).op(Op::CALL, 1, 1, 0);
    a.op(Op::EXHALE, 1, 1, 0)
        .try_leave()
        .label("e")
        .jump(Op::JMP, 0, "end");
    a.label("catch")
        .getfield(1, 5, "message")
        .getfield(2, 5, "line");
    a.op(Op::EXHALE, 1, 2, 0).try_leave();
    a.label("end").op(Op::LOADNULL, 0, 0, 0).i(ret(0));
    a.handler("s", "e", "catch", 5, None);
    out.push((src, module(a.finish("", 0, 6)), "division by zero 2\n"));

    let src: &'static str =
        Box::leak(format!("{bad_src}try {{\n  exhale [1, 0].map(bad)\n}} ashtray e {{\n  exhale e.message, e.line\n}}\n").into_boxed_str());
    let mut a = Asm::default();
    a.children.push(bad(src));
    a.i(Instr::abx(Op::CLOSURE, 0, 0)).try_enter(0).label("s");
    a.op(Op::NEWLIST, 1, 2, 0).i(Instr::asbx(Op::LOADINT, 2, 1));
    a.op(Op::APPENDLIST, 1, 2, 0)
        .i(Instr::asbx(Op::LOADINT, 2, 0));
    a.op(Op::APPENDLIST, 1, 2, 0).op(Op::MOV, 2, 0, 0);
    let map = a.s("map") as u8;
    a.at(span_of(src, "[1, 0].map(bad)"))
        .op(Op::CALLM, 1, map, 1);
    a.op(Op::EXHALE, 1, 1, 0)
        .try_leave()
        .label("e")
        .jump(Op::JMP, 0, "end");
    a.label("catch")
        .getfield(1, 5, "message")
        .getfield(2, 5, "line");
    a.op(Op::EXHALE, 1, 2, 0).try_leave();
    a.label("end").op(Op::LOADNULL, 0, 0, 0).i(ret(0));
    a.handler("s", "e", "catch", 5, None);
    out.push((src, module(a.finish("", 0, 6)), "division by zero 2\n"));

    // A map's fields reach the catch, through a finally between them.
    let src = "try {\n  try {\n    cough {\"message\": \"m\", \"code\": 7}\n  } finally {\n    exhale \"f\"\n  }\n} ashtray e {\n  exhale e.code, e.message, e.kind\n}\n";
    let mut a = Asm::default();
    a.try_enter(0).label("os").try_enter(1).label("is");
    a.op(Op::NEWMAP, 0, 2, 0)
        .loadk(1, Constant::Str("message".into()));
    a.loadk(2, Constant::Str("m".into()))
        .op(Op::INSERTMAP, 0, 1, 2);
    a.loadk(1, Constant::Str("code".into()))
        .i(Instr::asbx(Op::LOADINT, 2, 7));
    a.op(Op::INSERTMAP, 0, 1, 2);
    a.at(span_of(src, "cough {\"message\": \"m\", \"code\": 7}\n"))
        .op(Op::THROW, 0, 0, 0);
    a.try_leave().label("ie").jump(Op::JMP, 0, "iend");
    a.label("ifin").exhale_str(0, "f").try_leave();
    a.label("iend")
        .try_leave()
        .label("oe")
        .jump(Op::JMP, 0, "end");
    a.label("ocatch")
        .getfield(0, 3, "code")
        .getfield(1, 3, "message");
    a.getfield(2, 3, "kind").op(Op::EXHALE, 0, 3, 0).try_leave();
    a.label("end").op(Op::LOADNULL, 0, 0, 0).i(ret(0));
    a.handler("os", "oe", "ocatch", 3, None);
    a.handler("is", "ie", "ifin", r, Some("ifin"));
    out.push((src, module(a.finish("", 0, 4)), "f\n7 m cough\n"));

    // exit() is not caught; the finally still runs.
    let src = "try {\n  exit(3)\n} ashtray {\n  exhale \"caught\"\n} finally {\n  exhale \"f\"\n}\nexhale \"after\"\n";
    let mut a = Asm::default();
    let exit = catalogue_index("exit") as u8;
    a.try_enter(0).label("s").i(Instr::asbx(Op::LOADINT, 1, 3));
    a.at(span_of(src, "exit(3)")).op(Op::CALLB, 0, exit, 1);
    a.try_leave().label("e").jump(Op::JMP, 0, "end");
    a.label("catch")
        .exhale_str(0, "caught")
        .try_leave()
        .jump(Op::JMP, 0, "end");
    a.label("fin").exhale_str(0, "f").try_leave();
    a.label("end")
        .exhale_str(0, "after")
        .op(Op::LOADNULL, 0, 0, 0)
        .i(ret(0));
    a.handler("s", "e", "catch", r, Some("fin"));
    out.push((src, module(a.finish("", 0, 2)), "f\n"));

    // A finally's own `break` drops the pending error.
    let src = "roll i = 0\nwhile i < 2 {\n  i = i + 1\n  try {\n    cough \"lost\"\n  } finally {\n    break\n  }\n}\nexhale \"end\", i\n";
    let mut a = Asm::default();
    a.i(Instr::asbx(Op::LOADINT, 0, 0)).i(steps(1, 0));
    a.label("top")
        .i(Instr::asbx(Op::LOADINT, 1, 2))
        .op(Op::LT, 2, 0, 1);
    a.jump(Op::TESTN, 2, "out").i(steps(1, 1)).addk(0, 0, 1);
    a.try_enter(0)
        .label("s")
        .loadk(1, Constant::Str("lost".into()));
    a.at(span_of(src, "cough \"lost\"\n"))
        .op(Op::THROW, 1, 0, 0);
    a.try_leave().label("e").jump(Op::JMP, 0, "cont");
    a.label("fin")
        .op(Op::TRY_LEAVE, 1, 0, 0)
        .jump(Op::JMP, 0, "out");
    a.label("cont").jump(Op::LOOP, 0, "top");
    a.label("out")
        .i(steps(0, 0))
        .loadk(1, Constant::Str("end".into()));
    a.op(Op::MOV, 2, 0, 0).op(Op::EXHALE, 1, 2, 0);
    a.op(Op::LOADNULL, 1, 0, 0).i(ret(1));
    a.handler("s", "e", "fin", r, Some("fin"));
    out.push((src, module(a.finish("", 0, 3)), "end 1\n"));

    out
}

#[test]
fn try_catch_finally_agree_with_the_interpreter() {
    let programs = try_programs();
    assert_eq!(programs.len(), 14);
    for (src, m, want) in &programs {
        assert_eq!(agree(src, m), *want, "{src}");
    }
}

#[test]
fn the_step_budget_can_be_caught_and_comes_back() {
    // E515 inside a loop inside a try: the catch takes it, its first
    // statement's step raises it again (now outside the loop: the other
    // wording), and the finally's once more, which is the run's error.
    let src = "roll n = 0\ntry {\n  while n < 2 {\n    n = n + 1\n  }\n} ashtray e {\n  n = n + 100\n} finally {\n  n = n + 10\n}\nexhale n\n";
    let ast = crate::syntax::parse(src).expect("parses");
    let roll = ast.body[0].span();
    let Stmt::Try {
        body,
        handler: Some(handler),
        finally: Some(finally),
        span: try_span,
        ..
    } = &ast.body[1]
    else {
        panic!("a try");
    };
    let Stmt::While {
        span: wh,
        body: wbody,
        ..
    } = &body.stmts[0]
    else {
        panic!("a while");
    };
    let (inc, caught, fin, ex) = (
        wbody.stmts[0].span(),
        handler.stmts[0].span(),
        finally.stmts[0].span(),
        ast.body[2].span(),
    );
    let mut a = Asm::default();
    a.at(roll).i(steps(0, 1)).i(Instr::asbx(Op::LOADINT, 0, 0));
    a.at(*try_span).i(steps(0, 1)).try_enter(0).label("s");
    a.at(*wh).i(steps(0, 1)).i(steps(1, 0));
    a.label("top")
        .i(Instr::asbx(Op::LOADINT, 1, 2))
        .op(Op::LT, 2, 0, 1);
    a.jump(Op::TESTN, 2, "wout");
    a.at(*wh)
        .i(steps(1, 1))
        .at(inc)
        .i(steps(1, 1))
        .addk(0, 0, 1);
    a.jump(Op::LOOP, 0, "top");
    a.label("wout").i(steps(0, 0)).try_leave().label("e");
    a.jump(Op::JMP, 0, "end");
    a.label("catch").at(caught).i(steps(0, 1)).addk(0, 0, 100);
    a.try_leave().jump(Op::JMP, 0, "end");
    a.label("fin")
        .at(fin)
        .i(steps(0, 1))
        .addk(0, 0, 10)
        .try_leave();
    a.label("end").at(ex).i(steps(0, 1)).op(Op::EXHALE, 0, 1, 0);
    a.op(Op::LOADNULL, 0, 0, 0).i(ret(0));
    a.handler("s", "e", "catch", 3, Some("fin"));
    let m = module(a.finish("", 0, 4));
    let total = same_stop(src, &m);
    assert_eq!(total, 9);

    // Budget 5 runs out on the loop's second turn: caught, raised again
    // by the catch's statement and again by the finally's, whose position
    // and wording (no loop running) are the run's.
    let (d, steps_taken) = vm_stop(&m, 5);
    let d = d.expect("stopped");
    assert_eq!(d.code, "E515");
    assert_eq!(d.span, Some(fin));
    assert_eq!(d.message, "the script needs more than 5 steps");
    assert_eq!(steps_taken, 8);
}

// ----- positions and backtraces ---------------------------------------------------------------

/// `pull <name>(x) { snuff 10 / x }` at the top of `src`, compiled.
fn divides_ten(src: &str, name: &str) -> BcFunction {
    let mut b = Asm::default();
    b.i(Instr::asbx(Op::LOADINT, 1, 10));
    b.at(span_of(src, "10 / x")).op(Op::DIV, 2, 1, 0).i(ret(2));
    b.finish(name, 1, 3)
}

#[test]
fn backtraces_match_the_interpreters() {
    // The tree-walker's error names one place, where it was raised, through
    // every call, closure and callback; the VM's must be the same error.
    let deep = std::thread::Builder::new()
        .stack_size(1 << 30)
        .spawn(|| {
            // Deep recursion, raised at the bottom.
            let src = "pull down(n) {\n  if n == 0 {\n    snuff 1 / n\n  }\n  snuff down(n - 1)\n}\nexhale down(1000)\n";
            let mut c = Asm::default();
            c.eqk(1, 0, 0).jump(Op::TESTN, 1, "rec");
            c.i(Instr::asbx(Op::LOADINT, 1, 1));
            c.at(span_of(src, "1 / n")).op(Op::DIV, 2, 1, 0).i(ret(2));
            c.label("rec").op(Op::CELLGET, 3, 0, 0).addk(4, 0, -1);
            c.at(span_of(src, "down(n - 1)")).op(Op::CALL, 3, 1, 0).i(ret(3));
            let mut down = c.finish("down", 1, 5);
            down.upvalues = vec![captures(UpvalKind::ParentLocal, 0)];
            let mut a = Asm::default();
            a.children.push(down);
            a.op(Op::CELLNEW, 0, 0, 0).i(Instr::abx(Op::CLOSURE, 0, 0));
            a.op(Op::MOV, 1, 0, 0).i(Instr::asbx(Op::LOADINT, 2, 1000));
            a.at(span_of(src, "down(1000)")).op(Op::CALL, 1, 1, 0);
            a.op(Op::EXHALE, 1, 1, 0).i(ret(1));
            agree(src, &module(a.finish("", 0, 3)));
        })
        .expect("thread")
        .join();
    assert!(deep.is_ok(), "deep recursion");

    // An error in a closure.
    let src = "roll k = 0\nroll f = pack(x) => x / k\nexhale f(1)\n";
    let mut c = Asm::default();
    c.op(Op::GETUP, 1, 0, 0);
    c.at(span_of(src, "x / k")).op(Op::DIV, 2, 0, 1).i(ret(2));
    let mut lambda = c.finish("lambda", 1, 3);
    lambda.name = String::new();
    lambda.kind = FuncKind::Lambda;
    lambda.upvalues = vec![captures(UpvalKind::ParentLocal, 0)];
    let mut a = Asm::default();
    a.children.push(lambda);
    a.i(Instr::asbx(Op::LOADINT, 0, 0))
        .i(Instr::abx(Op::CLOSURE, 1, 0));
    a.op(Op::MOV, 2, 1, 0).i(Instr::asbx(Op::LOADINT, 3, 1));
    a.at(span_of(src, "f(1)")).op(Op::CALL, 2, 1, 0);
    a.op(Op::EXHALE, 2, 1, 0).i(ret(2));
    agree(src, &module(a.finish("", 0, 4)));

    // An error in a builtin's callback.
    let src = "exhale [1, 2].map(pack(x) => x / 0)\n";
    let mut c = Asm::default();
    c.i(Instr::asbx(Op::LOADINT, 1, 0));
    c.at(span_of(src, "x / 0")).op(Op::DIV, 2, 0, 1).i(ret(2));
    let mut lambda = c.finish("lambda", 1, 3);
    lambda.name = String::new();
    lambda.kind = FuncKind::Lambda;
    let mut a = Asm::default();
    a.children.push(lambda);
    a.op(Op::NEWLIST, 0, 2, 0).i(Instr::asbx(Op::LOADINT, 1, 1));
    a.op(Op::APPENDLIST, 0, 1, 0)
        .i(Instr::asbx(Op::LOADINT, 1, 2));
    a.op(Op::APPENDLIST, 0, 1, 0)
        .i(Instr::abx(Op::CLOSURE, 1, 0));
    let map = a.s("map") as u8;
    a.at(span_of(src, "[1, 2].map(pack(x) => x / 0)"));
    a.op(Op::CALLM, 0, map, 1).op(Op::EXHALE, 0, 1, 0).i(ret(0));
    agree(src, &module(a.finish("", 0, 2)));

    // Compiled code calls an interpreted pull, which raises.
    let src = "pull g(x) {\n  snuff 10 / x\n}\nexhale g(0)\n";
    let (_, walked) = walker_run(src, &[]);
    let walked = walked.expect("E502");
    let defined = crate::syntax::parse("pull g(x) {\n  snuff 10 / x\n}\n").expect("parses");
    let mut i = interp();
    i.run_program(&defined).expect("runs");
    let g = i.globals.get("g").expect("g");
    let mut a = Asm::default();
    a.op(Op::MOV, 1, 0, 0).i(Instr::asbx(Op::LOADINT, 2, 0));
    a.at(span_of(src, "g(0)")).op(Op::CALL, 1, 1, 0);
    a.op(Op::EXHALE, 1, 1, 0).i(ret(1));
    let mut f = a.finish("", 0, 3);
    f.arity = 1;
    let (_, vm) = vm_run(&module(f), vec![g]);
    let vm = vm.err().expect("E502");
    assert_eq!(ending(Some(vm), None), ending(Some(walked.clone()), None));

    // The tree-walker calls a compiled pull, which raises.
    let mut a = Asm::default();
    a.children.push(divides_ten(src, "g"));
    a.i(Instr::abx(Op::CLOSURE, 0, 0)).i(ret(0));
    let compiled = exec(&module(a.finish("", 0, 1))).expect("a pull");
    let (_, e) = walker_run("exhale g(0)\n", &[("g", compiled)]);
    assert_eq!(ending(e, None), ending(Some(walked), None));
}

// ----- prelude values --------------------------------------------------------------------------

#[test]
fn a_prelude_value_loads_by_name() {
    // `roll f = len`, `stick p = path`, `args`: the tree-walker's own values.
    let src = "roll f = len\nstick p = path\nexhale f(\"abc\"), p.join(\"a\", \"b\"), args, repr(f), repr(p)\n";
    let walker = {
        let ast = crate::syntax::parse(src).expect("parses");
        let buf = Buf::default();
        let mut w = interp();
        w.set_args(&["x".to_string(), "y z".to_string()]);
        w.out = Box::new(buf.clone());
        w.run_program(&ast).expect("runs");
        buf.text()
    };
    let mut a = Asm::default();
    let (len, path, args) = (a.s("len"), a.s("path"), a.s("args"));
    let join = a.s("join") as u8;
    a.i(Instr::abx(Op::LOADG, 0, len))
        .i(Instr::abx(Op::LOADG, 1, path));
    a.op(Op::MOV, 2, 0, 0).loadk(3, Constant::Str("abc".into()));
    a.op(Op::CALL, 2, 1, 0);
    a.op(Op::MOV, 3, 1, 0).loadk(4, Constant::Str("a".into()));
    a.loadk(5, Constant::Str("b".into()))
        .op(Op::CALLM, 3, join, 2);
    a.i(Instr::abx(Op::LOADG, 4, args));
    let repr = catalogue_index("repr") as u8;
    a.op(Op::MOV, 6, 0, 0).op(Op::CALLB, 5, repr, 1);
    a.op(Op::MOV, 7, 1, 0).op(Op::CALLB, 6, repr, 1);
    a.op(Op::EXHALE, 2, 5, 0)
        .op(Op::LOADNULL, 0, 0, 0)
        .i(ret(0));
    let m = module(a.finish("", 0, 8));
    let buf = Buf::default();
    let mut v = interp();
    v.set_args(&["x".to_string(), "y z".to_string()]);
    v.out = Box::new(buf.clone());
    run(&mut v, &program(&m)).expect("runs");
    assert_eq!(buf.text(), walker);
    // `path.join` writes the system's own separator.
    let joined = std::path::Path::new("a").join("b");
    assert_eq!(
        walker,
        format!(
            "3 {} [\"x\", \"y z\"] <builtin len> <module path>\n",
            joined.display()
        )
    );

    // The very values the interpreter's prelude holds: the same builtin,
    // the same module, the same `args` list.
    let mut a = Asm::default();
    let (len, math, args) = (a.s("len"), a.s("math"), a.s("args"));
    a.i(Instr::abx(Op::LOADG, 0, len))
        .i(Instr::abx(Op::LOADG, 1, math));
    a.i(Instr::abx(Op::LOADG, 2, args)).op(Op::NEWLIST, 3, 3, 0);
    for r in 0..3 {
        a.op(Op::APPENDLIST, 3, r, 0);
    }
    a.i(ret(3));
    let m = module(a.finish("", 0, 4));
    let mut v = interp();
    v.set_args(&["q".to_string()]);
    let got = run(&mut v, &program(&m)).expect("runs");
    let Value::List(got) = got else {
        panic!("a list")
    };
    let got = got.borrow();
    let same = |name: &str, x: &Value| {
        let want = v.prelude_value(name, Span::default()).expect(name);
        match (x, &want) {
            (Value::Builtin(a), Value::Builtin(b)) => Rc::ptr_eq(a, b),
            (Value::Module(a), Value::Module(b)) => Rc::ptr_eq(a, b),
            (Value::List(a), Value::List(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    };
    assert!(same("len", &got[0]), "len");
    assert!(same("math", &got[1]), "math");
    assert!(same("args", &got[2]), "args");

    // `args` before the script's arguments are set: the tree-walker's E501.
    let mut a = Asm::default();
    let args = a.s("args");
    a.at(span_of("exhale args\n", "args"));
    a.i(Instr::abx(Op::LOADG, 0, args))
        .op(Op::EXHALE, 0, 1, 0)
        .i(ret(0));
    let e = err(&module(a.finish("", 0, 1)));
    let (_, w) = walker_run("exhale args\n", &[]);
    let w = w.expect("E501");
    assert_eq!((e.code, e.line, e.col), ("E501", Some(1), Some(8)));
    assert_eq!((e.code, &e.message), (w.code, &w.message));
}

#[test]
fn an_unknown_prelude_name_is_refused_at_load() {
    for (name, ok) in [
        ("len", true),
        ("fs", true),
        ("args", true),
        ("lenn", false),
        ("fs.read", false),
        ("", false),
    ] {
        let mut f = main_fn(2, vec![]);
        f.constants = vec![Constant::Int(1), Constant::Str(name.into())];
        f.instrs = vec![abc(Op::NOP, 0, 0, 0), Instr::abx(Op::LOADG, 1, 1), ret(0)];
        let m = module(f);
        let v = verify_module(&m).expect("verifies");
        match load(v) {
            Ok(_) => assert!(ok, "{name:?} loaded"),
            Err(e) => {
                assert!(!ok, "{name:?}: {}", e.message);
                assert_eq!(e.code, "E1050");
                assert_eq!(
                    e.message,
                    "the program's bytecode does not verify: unknown-prelude-name at fn0+1"
                );
            }
        }
    }
    // In a child function too, named by its own index.
    let mut child = child_fn("c", 0, 1, vec![Instr::abx(Op::LOADG, 0, 0), ret(0)]);
    child.constants = vec![Constant::Str("nope".into())];
    let mut f = main_fn(1, vec![abc(Op::LOADNULL, 0, 0, 0), ret(0)]);
    f.children = vec![child];
    let e = load(verify_module(&module(f)).expect("verifies"))
        .err()
        .expect("refused");
    assert!(
        e.message.ends_with("unknown-prelude-name at fn1+0"),
        "{}",
        e.message
    );
}

// ----- the Rust stack a level of re-entry costs -------------------------------------------------

thread_local! {
    /// Where on the Rust stack each `mark()` ran.
    static MARKS: std::cell::RefCell<Vec<usize>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn mark(_: &mut Interp, _: &[Value], _: Span) -> Result<Value, Diagnostic> {
    let here = 0u8;
    let at = std::hint::black_box(std::ptr::addr_of!(here)) as usize;
    MARKS.with(|m| m.borrow_mut().push(at));
    Ok(Value::Null)
}

fn mark_value() -> Value {
    Value::Builtin(Rc::new(crate::value::Builtin {
        name: "mark",
        effect: crate::value::Effect::Pure,
        min_args: 0,
        max_args: Some(0),
        func: mark,
    }))
}

/// The Rust stack, in bytes, between one `mark()` and the next, averaged
/// over the levels of a recursion that marks once per level.
fn stack_per_level(run: impl FnOnce()) -> usize {
    MARKS.with(|m| m.borrow_mut().clear());
    run();
    let marks = MARKS.with(|m| m.borrow().clone());
    assert!(marks.len() > 2, "{} marks", marks.len());
    let (first, last) = (marks[1], marks[marks.len() - 1]);
    first.abs_diff(last) / (marks.len() - 2)
}

/// `pull f(n) { mark(); if n == 0 { snuff 0 }; snuff [n - 1].map(f) }`,
/// called with `levels`: each level is a builtin calling back into a
/// compiled function.
fn vm_reentry(levels: i16) -> usize {
    stack_per_level(|| {
        let mut c = Asm::default();
        let map = c.s("map") as u8;
        c.op(Op::GETUP, 1, 1, 0).op(Op::CALL, 1, 0, 0);
        c.eqk(1, 0, 0).jump(Op::TESTN, 1, "rec");
        c.i(Instr::asbx(Op::LOADINT, 1, 0)).i(ret(1));
        c.label("rec").op(Op::NEWLIST, 1, 1, 0).addk(2, 0, -1);
        c.op(Op::APPENDLIST, 1, 2, 0).op(Op::CELLGET, 2, 0, 0);
        c.op(Op::CALLM, 1, map, 1).i(ret(1));
        let mut f = c.finish("f", 1, 3);
        f.upvalues = vec![
            captures(UpvalKind::ParentLocal, 1),
            captures(UpvalKind::ParentLocal, 0),
        ];
        let mut a = Asm::default();
        a.children.push(f);
        a.op(Op::CELLNEW, 1, 0, 0).i(Instr::abx(Op::CLOSURE, 1, 0));
        a.op(Op::MOV, 2, 1, 0)
            .i(Instr::asbx(Op::LOADINT, 3, levels));
        a.op(Op::CALL, 2, 1, 0).i(ret(2));
        let mut main = a.finish("", 0, 4);
        main.arity = 1;
        exec_with(&module(main), vec![mark_value()]).expect("runs");
    })
}

/// The same recursion on the tree-walker.
fn walker_reentry(levels: i16) -> usize {
    stack_per_level(|| {
        let src = format!(
            "pull f(n) {{\n  mark()\n  if n == 0 {{\n    snuff 0\n  }}\n  snuff [n - 1].map(f)\n}}\nf({levels})\n"
        );
        let (_, e) = walker_run(&src, &[("mark", mark_value())]);
        assert!(e.is_none(), "{:?}", e.map(|e| e.message));
    })
}

#[test]
fn a_reentered_call_costs_no_more_stack_than_the_interpreters() {
    let (vm, walker) = std::thread::Builder::new()
        .stack_size(1 << 30)
        .spawn(|| (vm_reentry(200), walker_reentry(200)))
        .expect("thread")
        .join()
        .expect("joined");
    eprintln!(
        "a level of builtin re-entry: VM {vm} bytes, tree-walker {walker} bytes; \
         4000 levels: VM {} MiB, tree-walker {} MiB",
        (vm * 4000) >> 20,
        (walker * 4000) >> 20
    );
    assert!(vm <= walker, "VM {vm} bytes a level, tree-walker {walker}");
}
