// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
//! Stage B's lowering (cig-compile#31), first part: CIR to the register bytecode,
//! verified, loaded and run in the cig runtime's VM, and held to the
//! tree-walker.

#[path = "lower/burns.rs"]
mod burns;
#[path = "lower/gen.rs"]
#[allow(dead_code)]
mod gen;
use burns::burn_corpus;

use cigscript::burn::{Kernel, Mode};
use cigscript::cig_bytecode::{verify_module, Module};
use cigscript::diagnostics::Diagnostic;
use cigscript::interp::Interp;
use cigscript::lower::bc::{self, CompileError, Error};
use cigscript::lower::cir::ir::{
    Block, BurnNode, Const, FuncId, Kind, MapKey, OpKind, Piece, RegionId, ShortOp, StepSite,
    ValueId, VarId,
};
use cigscript::lower::cir::pass::{self, Pass};
use cigscript::syntax::ast::{AssignOp, BinOp, BurnClass, UnOp};
use cigscript::syntax::span::Span;
use cigscript::value::Effect;
use std::cell::RefCell;
use std::rc::Rc;

// ----- running both ways ---------------------------------------------------------

/// What a run left: the bytes it printed (`exhale`) and wrote to the
/// error stream (`log`), and how it ended: cleanly, with an `exit` code,
/// or with the error's code, message, hint, line and column.
#[derive(Debug, PartialEq)]
struct Outcome {
    printed: String,
    logged: String,
    exit: Option<i32>,
    end: Option<Stop>,
}

/// An error's code, message, hint, line and column.
type Stop = (String, String, Option<String>, Option<u32>, Option<u32>);

/// One of the interpreter's streams, kept for the comparison.
#[derive(Clone, Default)]
struct Stream(Rc<RefCell<Vec<u8>>>);

impl std::io::Write for Stream {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Run {
    interp: Interp,
    out: Stream,
    err: Stream,
}

fn interp(max_steps: u64) -> Run {
    let (out, err) = (Stream::default(), Stream::default());
    let mut i = Interp::new(Kernel::ephemeral(Mode::Run));
    i.out = Box::new(out.clone());
    i.err = Box::new(err.clone());
    i.set_max_steps(max_steps);
    // `args`, read as a value by the prelude programs.
    i.set_args(&["one".to_string(), "two".to_string()]);
    Run {
        interp: i,
        out,
        err,
    }
}

fn outcome(r: Result<(), Diagnostic>, run: &Run) -> (Outcome, u64) {
    let exit = run.interp.exit_requested;
    let text = |s: &Stream| String::from_utf8_lossy(&s.0.borrow()).into_owned();
    (
        Outcome {
            printed: text(&run.out),
            logged: text(&run.err),
            exit,
            // `exit()` stops the tree-walker cleanly and the VM with the
            // request recorded: both are the exit, not an error.
            end: r
                .err()
                .filter(|_| exit.is_none())
                .map(|d| (d.code.to_string(), d.message, d.hint, d.line, d.col)),
        },
        run.interp.steps(),
    )
}

/// The tree-walker, as `cig run` runs a script, and the steps it counted.
fn tree_walker(src: &str, max_steps: u64) -> (Outcome, u64) {
    let program = cigscript::syntax::parse(src).expect("parses");
    let mut run = interp(max_steps);
    let r = run.interp.run_program(&program);
    outcome(r, &run)
}

/// The VM, through its public entry points: `verify_module`, `vm::load`,
/// `vm::run`. A script has no top-level `snuff`, so the top level leaves
/// null, or the map of the names it declares; what it computed comes out
/// through `exhale`, which both write through the interpreter's one
/// writer.
fn vm(m: &Module, max_steps: u64) -> (Outcome, u64) {
    let verified = verify_module(m).unwrap_or_else(|e| panic!("does not verify: {e}"));
    let program =
        cigscript::vm::load(verified).unwrap_or_else(|e| panic!("does not load: {}", e.message));
    let mut run = interp(max_steps);
    let r = cigscript::vm::run(&mut run.interp, &program).map(|v| {
        assert!(
            matches!(v.type_name(), "null" | "map"),
            "the top level leaves null or its names: {}",
            v.repr()
        );
    });
    outcome(r, &run)
}

/// The tree-walker recurses on the native stack: deep recursion needs
/// more than a test thread's default.
fn big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(512 << 20)
        .spawn(f)
        .expect("a thread")
        .join()
        .expect("the run does not panic")
}

fn compile(src: &str, level: u8) -> Module {
    match bc::compile(src, "t.cig", &pass::level(level)) {
        Ok(m) => m,
        Err(CompileError::Bytecode(e)) => panic!("-O{level}: {e}\n{src}"),
        Err(e) => panic!("-O{level}: {e:?}\n{src}"),
    }
}

// ----- the corpus -------------------------------------------------------------------

/// Small programs inside what the lowering covers. Each prints what it
/// computed with `exhale`, or stops on an error first.
fn corpus() -> Vec<(String, String)> {
    let mut c: Vec<(String, String)> = Vec::new();
    let mut add = |name: &str, src: &str| c.push((name.to_string(), src.to_string()));

    // Arithmetic at the i64 edges, and division by zero.
    add(
        "max-plus-one",
        "roll a = 9223372036854775807\nroll b = 1\nexhale a + b\n",
    );
    add(
        "min-minus-one",
        "roll a = -9223372036854775807 - 1\nroll b = 1\nexhale a - b\n",
    );
    add(
        "min-negated",
        "roll a = -9223372036854775807 - 1\nexhale -a\n",
    );
    add("mul-overflow", "roll a = 3037000500\nexhale a * a\n");
    add("mul-edge", "roll a = 3037000499\nexhale a * a\n");
    add(
        "min-div-minus-one",
        "roll a = -9223372036854775807 - 1\nroll b = -1\nexhale a / b\n",
    );
    add(
        "min-mod-minus-one",
        "roll a = -9223372036854775807 - 1\nroll b = -1\nexhale a % b\n",
    );
    add("div-zero", "roll a = 7\nroll b = 0\nexhale a / b\n");
    add("mod-zero", "roll a = 7\nroll b = 0\nexhale a % b\n");
    add(
        "float-div-zero",
        "roll a = 7.0\nroll b = 0.0\nexhale a / b\n",
    );
    add("int-div", "roll a = -7\nroll b = 2\nexhale a / b\n");
    add("int-mod", "roll a = -7\nroll b = 2\nexhale a % b\n");
    add("mixed", "roll a = 7\nroll b = 2.5\nexhale a * b - a / b\n");
    add("strings", "roll a = \"ci\"\nroll b = \"g\"\nexhale a + b\n");
    add("str-plus-int", "roll a = \"n\"\nroll b = 1\nexhale a + b\n");
    add(
        "big-constant",
        "roll a = 123456789012\nroll b = 40000\nexhale a + b - 70000\n",
    );
    add(
        "compound",
        "roll a = 10\na += 5\na -= 3\na *= 4\na /= 6\nexhale a\n",
    );

    // Every comparison across types.
    let operands = [
        "1", "2", "-3", "2.5", "0.0", "\"a\"", "\"b\"", "true", "false", "null",
    ];
    for (i, x) in operands.iter().enumerate() {
        for (j, y) in operands.iter().enumerate() {
            for op in ["==", "!=", "<", "<=", ">", ">="] {
                add(
                    &format!("cmp-{i}-{op}-{j}"),
                    &format!("roll x = {x}\nroll y = {y}\nexhale x {op} y\n"),
                );
            }
        }
    }
    add(
        "nan",
        "roll z = 0.0\nroll n = z / z\nroll o = 1.0\nexhale n < o\n",
    );
    add("not", "roll t = 0\nexhale not t\n");

    // Short-circuit logic and `??`.
    add(
        "short",
        "roll a = false and int(\"x\")\nroll b = true or int(\"x\")\nroll c = null ?? 3\nroll d = 4 ?? int(\"x\")\nroll e = 0 and 1\nroll f = \"\" or \"s\"\nroll g = 1 and 2\nroll h = false or null\nexhale str(a) + str(b) + str(c) + str(d) + str(e) + f + str(g) + str(h)\n",
    );

    // Nested `if`.
    add(
        "nested-if",
        "roll n = 0\nroll i = 0\nwhile i < 12 {\n  i += 1\n  if i % 2 == 0 {\n    if i % 3 == 0 {\n      n += 100\n    } else if i % 4 == 0 {\n      n += 10\n    } else {\n      n += 1\n    }\n  } else if i > 9 {\n    n -= 1\n  }\n}\nexhale n\n",
    );

    // `while` with `break` and `continue`, nested.
    add(
        "while-break-continue",
        "roll total = 0\nroll i = 0\nwhile i < 20 {\n  i += 1\n  if i == 3 {\n    continue\n  }\n  roll j = 0\n  while true {\n    j += 1\n    if j > i {\n      break\n    }\n    if j % 2 == 0 {\n      continue\n    }\n    total += j\n  }\n  if i == 15 {\n    break\n  }\n}\nexhale total\n",
    );
    add(
        "while-false",
        "roll n = 0\nwhile n > 0 {\n  n -= 1\n}\nexhale n\n",
    );

    // Pulls, lambdas, `snuff`, recursion (a pull that calls itself
    // through a parameter: one that names itself captures itself).
    add(
        "pulls",
        "pull add(a, b) {\n  snuff a + b\n}\npull none() {\n  roll x = 1\n}\nstick sq = pack(x) => x * x\nexhale str(add(sq(3), 4)) + str(none())\n",
    );
    add(
        "fact",
        "pull fact(self, n) {\n  if n <= 1 {\n    snuff 1\n  }\n  snuff n * self(self, n - 1)\n}\nexhale fact(fact, 20)\n",
    );
    add(
        "fact-overflow",
        "pull fact(self, n) {\n  if n <= 1 {\n    snuff 1\n  }\n  snuff n * self(self, n - 1)\n}\nexhale fact(fact, 25)\n",
    );
    add(
        "fib",
        "pull fib(self, n) {\n  if n < 2 {\n    snuff n\n  }\n  snuff self(self, n - 1) + self(self, n - 2)\n}\nexhale fib(fib, 10)\n",
    );
    add(
        "deep",
        "pull down(self, n) {\n  if n == 0 {\n    snuff 0\n  }\n  snuff 1 + self(self, n - 1)\n}\nexhale down(down, 3000)\n",
    );
    add(
        "too-deep",
        "pull down(self, n) {\n  if n == 0 {\n    snuff 0\n  }\n  snuff 1 + self(self, n - 1)\n}\nexhale down(down, 5000)\n",
    );
    add(
        "callee-type-error",
        "pull f(a) {\n  roll b = a * 2\n  snuff b + \"x\"\n}\nroll i = 0\nwhile i < 3 {\n  i += 1\n}\nexhale f(i)\n",
    );
    add(
        "arity",
        "pull f(a, b) {\n  snuff a\n}\nroll g = f\nexhale g(1)\n",
    );
    add("call-a-number", "roll g = 3\nexhale g(1)\n");
    add(
        "loop-in-callee",
        "pull spin(n) {\n  roll i = 0\n  while i < n {\n    i += 1\n  }\n  snuff i\n}\nroll t = 0\nroll k = 0\nwhile k < 4 {\n  k += 1\n  t += spin(k)\n}\nexhale t\n",
    );
    add(
        "early-snuff-in-loop",
        "pull find(n) {\n  roll i = 0\n  while true {\n    i += 1\n    if i * i > n {\n      snuff i\n    }\n  }\n}\nexhale find(50) + find(1000)\n",
    );
    add(
        "top-level-break-in-loop",
        "roll i = 0\nwhile true {\n  i += 1\n  if i == 7 {\n    exhale i\n    break\n  }\n}\n",
    );
    add(
        "nested-pull",
        "pull outer(a) {\n  pull inner(b) {\n    snuff b + 1\n  }\n  snuff inner(a) * 2\n}\nexhale outer(20)\n",
    );

    // Builtin calls, and their errors.
    add(
        "builtins",
        "roll a = str(12)\nroll b = len(a)\nroll c = int(\"41\")\nroll d = type_of(2.5)\nexhale a + str(b) + str(c + 1) + d\n",
    );
    add("builtin-arity", "roll a = len()\nexhale a\n");
    add("builtin-error", "roll a = int(\"x\")\nexhale a\n");
    add(
        "assert",
        "roll n = 2\nassert(n == 3, \"n is not three\")\nexhale n\n",
    );

    // Nothing to say.
    add("silent", "roll n = 1\nn += 1\n");
    // ----- closures that capture, and named recursion -----
    add("fib-by-name", "pull fib(n) {\n  if n < 2 {\n    snuff n\n  }\n  snuff fib(n - 1) + fib(n - 2)\n}\nexhale fib(15)\n");
    add("mutual", "pull even(n) {\n  if n == 0 {\n    snuff true\n  }\n  snuff odd(n - 1)\n}\npull odd(n) {\n  if n == 0 {\n    snuff false\n  }\n  snuff even(n - 1)\n}\nexhale even(10), odd(7), even(3)\n");
    add("deep-by-name", "pull down(n) {\n  if n == 0 {\n    snuff 0\n  }\n  snuff 1 + down(n - 1)\n}\nexhale down(5000)\n");
    add("counter", "pull make() {\n  roll n = 0\n  snuff pack() {\n    n += 1\n    snuff n\n  }\n}\nstick a = make()\nstick b = make()\nexhale a(), a(), b(), a()\n");
    add("per-turn-for", "roll fs = []\nfor i in 0..3 {\n  fs.push(pack() => i)\n}\nfor f in fs {\n  exhale f()\n}\n");
    add("per-turn-for-list", "roll fs = []\nfor s in [\"a\", \"b\"] {\n  fs.push(pack() => s + s)\n}\nexhale fs[0](), fs[1]()\n");
    add("per-turn-while", "roll fs = []\nroll k = 0\nwhile k < 3 {\n  roll j = k * 10\n  fs.push(pack() => j)\n  k += 1\n}\nexhale fs[0](), fs[1](), fs[2]()\n");
    add("shared-before-loop", "roll fs = []\nroll n = 0\nwhile n < 3 {\n  fs.push(pack() => n)\n  n += 1\n}\nexhale fs[0](), fs[2]()\n");
    add("three-levels", "pull outer(a) {\n  roll b = a * 2\n  pull mid(c) {\n    snuff pack(d) => a + b + c + d\n  }\n  snuff mid(100)\n}\nstick f = outer(1)\nexhale f(1000)\n");
    add("captured-param", "pull adder(x) {\n  snuff pack(y) => x + y\n}\nstick add5 = adder(5)\nexhale add5(1), adder(10)(2)\n");
    add("closure-writes", "roll total = 0\nstick bump = pack(x) {\n  total += x\n  snuff total\n}\nbump(3)\nbump(4)\nexhale total\n");
    add("nested-pulls-call-each-other", "pull run(n) {\n  pull a(x) {\n    if x <= 0 {\n      snuff 0\n    }\n    snuff b(x - 1) + 1\n  }\n  pull b(x) {\n    snuff a(x - 1) + n\n  }\n  snuff a(6)\n}\nexhale run(10)\n");
    add("callback", "roll seen = 0\nroll ys = [1, 2, 3].map(pack(x) {\n  seen += 1\n  snuff x * seen\n})\nexhale ys, seen\n");
    add(
        "callback-error",
        "roll ys = [1, \"a\"].map(pack(x) => x * 2)\nexhale ys\n",
    );
    add(
        "pull-as-value",
        "pull twice(x) {\n  snuff x * 2\n}\nexhale [1, 2].map(twice)\n",
    );
    // ----- lists and maps -----
    add("literals", "roll xs = [1, \"two\", [3, 4], {k: null}]\nroll m = {a: 1, \"b c\": [2], d: {e: true}}\nexhale xs, m\nexhale xs[2][1], m.d.e, m[\"b c\"][0]\n");
    add(
        "negative-index",
        "roll xs = [1, 2, 3]\nroll s = \"héllo\"\nexhale xs[-1], s[1], s[-1]\n",
    );
    add("index-past-end", "roll xs = [1, 2]\nexhale xs[5]\n");
    add("string-index-past-end", "roll s = \"ab\"\nexhale s[9]\n");
    add(
        "index-type",
        "roll xs = [1, 2]\nroll k = \"a\"\nexhale xs[k]\n",
    );
    add("map-key-type", "roll m = {a: 1}\nroll k = 1\nexhale m[k]\n");
    add("index-into-int", "roll n = 5\nexhale n[0]\n");
    add("missing-key-index", "roll m = {a: 1}\nexhale m[\"b\"]\n");
    add("missing-member", "roll m = {a: 1}\nexhale m.b\n");
    add("member-of-null", "roll m = null\nexhale m.b\n");
    add("member-of-int", "roll m = 3\nexhale m.b\n");
    add("safe-member", "roll m = {a: 1, n: null}\nroll z = null\nroll s = \"x\"\nexhale m?.a, m?.b, m?.n, z?.a\nexhale s?.a\n");
    add(
        "safe-member-shadowed",
        "roll m = {keys: 1, contains: 2, a: 3}\nexhale m?.a, m?.keys, m?.zz\n",
    );
    add("stores", "roll xs = [1, 2, 3]\nroll m = {a: 1}\nxs[0] = 9\nxs[-1] += 10\nm[\"b\"] = 2\nm.c = 3\nm.a += 5\nm[\"b\"] *= 7\nexhale xs, m\n");
    add("store-past-end", "roll xs = [1]\nxs[3] = 1\n");
    add(
        "store-index-type",
        "roll xs = [1]\nroll k = \"a\"\nxs[k] = 1\n",
    );
    add("store-map-key-type", "roll m = {}\nroll k = 2\nm[k] = 1\n");
    add("store-into-int", "roll n = 4\nroll k = 0\nn[k] = 1\n");
    add(
        "store-into-string",
        "roll s = \"ab\"\nroll k = 0\ns[k] = \"x\"\n",
    );
    add(
        "compound-into-string",
        "roll s = \"ab\"\nroll k = 0\ns[k] += \"x\"\n",
    );
    add("compound-missing-key", "roll m = {a: 1}\nm[\"b\"] += 1\n");
    add("compound-missing-member", "roll m = {a: 1}\nm.b += 1\n");
    add("compound-past-end", "roll xs = [1]\nxs[2] -= 1\n");
    add("compound-type", "roll xs = [\"a\"]\nxs[0] -= 1\n");
    add("set-member-on-int", "roll n = 1\nn.k = 2\n");
    add("compound-member-on-int", "roll n = 1\nn.k += 2\n");
    add(
        "member-compound-type",
        "roll m = {a: \"s\"}\nm.a *= \"t\"\n",
    );
    add(
        "dynamic-keys",
        "roll k = \"x\"\nroll n = 7\nroll m = {[k]: 1, [n]: 2, plain: 3}\nexhale m\n",
    );
    add(
        "dynamic-key-type",
        "roll f = 1.5\nroll m = {[f]: 1}\nexhale m\n",
    );
    add(
        "aliasing",
        "roll a = [1]\nroll b = a\nb.push(2)\nroll m = {l: a}\nm.l.push(3)\nexhale a, b, m\n",
    );
    // ----- for -----
    add("for-range", "roll t = 0\nfor i in 0..10 {\n  if i == 3 {\n    continue\n  }\n  if i == 8 {\n    break\n  }\n  t += i\n}\nexhale t\n");
    add("for-range-empty-and-back", "roll n = 0\nfor i in 5..5 {\n  n += 1\n}\nfor i in 5..2 {\n  n += 10\n}\nfor i in -3..-1 {\n  n += i\n}\nexhale n\n");
    add(
        "for-range-bounds",
        "roll a = 2\nroll b = a * 3\nfor i in a..b {\n  i = 100\n  exhale i\n}\n",
    );
    add(
        "for-range-float",
        "roll t = 0\nfor i in 1..2.5 {\n  t += i\n}\n",
    );
    add(
        "for-range-too-big",
        "roll t = 0\nfor i in 0..100000000 {\n  t += i\n}\n",
    );
    add("for-nested", "roll t = 0\nfor i in 0..4 {\n  for j in 0..i {\n    if j == 2 {\n      break\n    }\n    t += i * j\n  }\n}\nexhale t\n");
    add("for-list-map-text", "roll out = []\nfor x in [1, [2], null] {\n  out.push(x)\n}\nfor k in {b: 1, a: 2} {\n  out.push(k)\n}\nfor c in \"hé!\" {\n  out.push(c)\n}\nfor e in [] {\n  out.push(e)\n}\nexhale out\n");
    add("for-mutating", "roll xs = [1, 2]\nfor x in xs {\n  xs.push(x * 10)\n}\nroll m = {a: 1}\nfor k in m {\n  m[k + k] = 2\n}\nexhale xs, m\n");
    add(
        "for-not-iterable",
        "roll n = 5\nfor x in n {\n  exhale x\n}\n",
    );
    add("for-continue-list", "roll t = []\nfor x in [1, 2, 3, 4] {\n  if x % 2 == 0 {\n    continue\n  }\n  t.push(x)\n}\nexhale t\n");
    add("for-in-pull", "pull total(xs) {\n  roll t = 0\n  for x in xs {\n    t += x\n  }\n  snuff t\n}\nroll s = 0\nfor i in 0..3 {\n  s += total([i, i])\n}\nexhale s\n");
    // ----- interpolation, exhale, method calls -----
    add("interp", "roll n = 3\nroll xs = [1, \"a\"]\nroll m = {k: 2.5}\nexhale \"n=${n} xs=${xs} m=${m} f=${1.0} ${null} ${true}\"\n");
    add(
        "interp-sees-before",
        "roll xs = [1]\nexhale \"${xs} then ${xs.push(2)} ${xs}\"\n",
    );
    add(
        "exhale-sees-before",
        "roll xs = [1]\nexhale xs, xs.push(2), xs\n",
    );
    add(
        "exhale-many",
        "exhale 1, \"two\", 3.5, null, [true]\nexhale \"\"\n",
    );
    add("methods", "roll s = \"Hello\"\nroll xs = [3, 1, 2]\nexhale s.upper(), s.len(), xs.len(), xs.contains(2)\nroll ys = xs.filter(pack(x) => x > 1)\nexhale ys, xs.map(pack(x) => x * x)\n");
    add("method-unknown", "roll xs = [1]\nexhale xs.lenn()\n");
    add("method-arity", "roll xs = [1]\nxs.push()\n");
    add(
        "safe-method",
        "roll z = null\nroll s = \"a\"\nexhale z?.upper(), s?.upper()\n",
    );
    add(
        "map-holds-function",
        "roll m = {f: pack(x) => x + 1}\nexhale m.f(1)\n",
    );
    add("exit", "exhale 1\nexit(3)\nexhale 2\n");
    add(
        "interp-lines",
        "pull f(x) {\n  roll a = 1\n  snuff \"${a} ${x / 0}\"\n}\nexhale f(1)\n",
    );
    add(
        "interp-index",
        "roll xs = [1]\nroll k = \"a\"\nexhale \"v=${xs[k]}\"\n",
    );
    add(
        "range-values",
        "roll a = 3\nroll b = 0\nexhale 0..4, a..b, -2..1, (0..3).map(pack(x) => x * x)\n",
    );
    add(
        "range-value-float",
        "roll a = 1\nroll b = 2.5\nroll r = a..b\n",
    );
    add("range-value-too-big", "roll a = 0\nroll r = a..100000000\n");
    add("range-value-extremes", "roll a = 9223372036854775807\nroll b = -9223372036854775807 - 1\nexhale a..b\nexhale b..a\n");
    // A computed key is checked before the entry's value runs; a pass
    // that replaces an operand leaves its position where it was.
    add(
        "key-before-failing-value",
        "roll k = true\nexhale {[k]: 1, b: 1 / 0}\n",
    );
    add(
        "key-before-a-call",
        "pull f() {\n  snuff \"v\"\n}\nroll k = \"j\"\nroll m = {[k]: f(), [3]: f()}\nexhale m\n",
    );
    add(
        "key-effect-read-by-value",
        "roll xs = [1]\nroll m = {[str(xs.push(2))]: xs.len(), n: xs}\nexhale m\n",
    );
    add(
        "key-list-after-copy-prop",
        "stick k = [1]\nexhale {[k]: 1}\n",
    );
    add(
        "key-int-after-copy-prop",
        "stick n = 7\nstick m = {[n]: 1}\nexhale m[\"7\"]\n",
    );
    add(
        "index-type-after-copy-prop",
        "stick xs = [1, 2]\nstick k = \"a\"\nexhale xs[k]\n",
    );
    add(
        "cannot-index-after-copy-prop",
        "stick n = 5\nstick k = 0\nexhale n[k]\n",
    );
    add(
        "store-index-type-after-copy-prop",
        "stick xs = [1]\nstick k = \"a\"\nxs[k] = 2\n",
    );
    add("store-into-int-after-copy-prop", "stick n = 5\nn[0] = 1\n");
    add("member-store-after-copy-prop", "stick n = 5\nn.k = 1\n");
    add(
        "member-compound-after-copy-prop",
        "stick n = \"s\"\nn.k += 1\n",
    );
    add(
        "module-call",
        "exhale math.floor(2.5), path.join(\"a\", \"b\"), text.split(\"a,b\", \",\")\n",
    );

    // ----- try, ashtray, finally, cough -----
    add("try-catch", "try {\n  cough \"boom\"\n} ashtray e {\n  exhale e.message, e.kind, e.line\n}\nexhale \"after\"\n");
    add("try-catch-map", "try {\n  cough {message: \"m\", code: 7}\n} ashtray e {\n  exhale e.message, e.code, e\n}\n");
    add(
        "try-catch-value",
        "try {\n  cough [1, 2]\n} ashtray e {\n  exhale e\n}\n",
    );
    add(
        "try-no-binding",
        "try {\n  roll x = 1 / 0\n} ashtray {\n  exhale \"caught\"\n}\nexhale \"after\"\n",
    );
    add(
        "try-builtin-error",
        "try {\n  exhale int(\"x\")\n} ashtray e {\n  exhale e.kind, e.message, e.line\n}\n",
    );
    add(
        "try-no-error",
        "try {\n  exhale 1\n} ashtray e {\n  exhale 2\n} finally {\n  exhale 3\n}\nexhale 4\n",
    );
    add(
        "try-empty-body",
        "try {\n} finally {\n  exhale 1\n}\ntry {\n} ashtray e {\n  exhale 2\n}\n",
    );
    add(
        "finally-fall-through",
        "try {\n  exhale \"body\"\n} finally {\n  exhale \"fin\"\n}\nexhale \"after\"\n",
    );
    add(
        "finally-error",
        "try {\n  roll x = 1 / 0\n} finally {\n  exhale \"fin\"\n}\nexhale \"never\"\n",
    );
    add("finally-after-catch", "try {\n  cough \"a\"\n} ashtray e {\n  exhale \"caught\", e.message\n} finally {\n  exhale \"fin\"\n}\n");
    add("finally-break", "roll i = 0\nwhile true {\n  try {\n    i += 1\n    if i == 3 {\n      break\n    }\n  } finally {\n    exhale \"fin\", i\n  }\n}\nexhale \"out\", i\n");
    add("finally-continue", "roll t = []\nfor i in 0..4 {\n  try {\n    if i % 2 == 0 {\n      continue\n    }\n    t.push(i)\n  } finally {\n    t.push(i * 10)\n  }\n}\nexhale t\n");
    add("finally-snuff", "pull f(x) {\n  try {\n    snuff x * 2\n  } finally {\n    exhale \"fin\"\n    roll y = 100\n    roll z = y + 1\n    exhale z\n  }\n}\nexhale f(5)\n");
    add("finally-snuff-keeps-value", "pull f() {\n  roll x = 1\n  try {\n    snuff x\n  } finally {\n    x = 2\n  }\n}\nexhale f()\n");
    add("snuff-in-catch", "pull f() {\n  try {\n    cough \"x\"\n  } ashtray e {\n    snuff \"from catch \" + e.message\n  } finally {\n    exhale \"fin\"\n  }\n  snuff \"never\"\n}\nexhale f()\n");
    add("snuff-in-try-in-loop", "pull find(xs, t) {\n  for x in xs {\n    try {\n      if x == t {\n        snuff x * 100\n      }\n    } finally {\n      exhale \"check\", x\n    }\n  }\n  snuff null\n}\nexhale find([1, 2, 3], 2), find([4], 9)\n");
    add("snuff-in-try-in-while", "pull f(n) {\n  roll i = 0\n  while true {\n    i += 1\n    try {\n      if i == n {\n        snuff i\n      }\n    } ashtray e {\n      exhale \"no\"\n    }\n  }\n}\nexhale f(4)\n");
    add("break-from-finally", "roll n = 0\nwhile n < 5 {\n  n += 1\n  try {\n    cough \"x\"\n  } finally {\n    break\n  }\n}\nexhale n\n");
    add("continue-from-finally", "roll n = 0\nfor i in 0..3 {\n  try {\n    cough \"x\"\n  } finally {\n    n += 1\n    continue\n  }\n}\nexhale n\n");
    add("snuff-from-finally", "pull f() {\n  try {\n    snuff 1\n  } finally {\n    snuff 2\n  }\n}\npull g() {\n  try {\n    cough \"lost\"\n  } finally {\n    snuff 3\n  }\n}\nexhale f(), g()\n");
    add("break-crosses-two", "for i in 0..3 {\n  try {\n    try {\n      if i == 1 {\n        break\n      }\n    } finally {\n      exhale \"a\", i\n    }\n  } finally {\n    exhale \"b\", i\n  }\n}\nexhale \"out\"\n");
    add("continue-in-catch", "for i in 0..3 {\n  try {\n    cough i\n  } ashtray e {\n    if e.value == 1 {\n      continue\n    }\n    exhale \"c\", i\n  } finally {\n    exhale \"f\", i\n  }\n}\n");
    add("nested-handlers", "try {\n  try {\n    cough \"inner\"\n  } ashtray e {\n    exhale \"in\", e.message\n    cough \"again\"\n  } finally {\n    exhale \"inner fin\"\n  }\n} ashtray e {\n  exhale \"out\", e.message\n} finally {\n  exhale \"outer fin\"\n}\n");
    add("try-in-catch", "try {\n  cough 1\n} ashtray e {\n  try {\n    cough 2\n  } ashtray f {\n    exhale e.value, f.value\n  }\n}\n");
    add("error-in-finally-replaces", "try {\n  try {\n    cough \"first\"\n  } finally {\n    cough \"second\"\n  }\n} ashtray e {\n  exhale e.message\n}\n");
    add(
        "error-in-finally-uncaught",
        "try {\n  exhale 1\n} finally {\n  roll x = [1][5]\n}\n",
    );
    add(
        "error-in-handler",
        "try {\n  cough 1\n} ashtray e {\n  roll x = e.nope\n}\nexhale \"never\"\n",
    );
    add(
        "error-in-handler-finally",
        "try {\n  cough 1\n} ashtray e {\n  roll x = 1 / 0\n} finally {\n  exhale \"fin\"\n}\n",
    );
    add("error-in-callee", "pull bad(x) {\n  snuff x / 0\n}\ntry {\n  exhale bad(1)\n} ashtray e {\n  exhale e.message, e.line\n}\n");
    add("error-through-callee-finally", "pull f() {\n  try {\n    cough \"deep\"\n  } finally {\n    exhale \"f fin\"\n  }\n}\npull g() {\n  f()\n}\ng()\n");
    add("error-in-callback", "try {\n  roll ys = [1, \"a\"].map(pack(x) => x - 1)\n} ashtray e {\n  exhale e.message, e.line\n}\n");
    add("cough-in-callback", "try {\n  [1, 2].map(pack(x) {\n    cough \"cb ${x}\"\n  })\n} ashtray e {\n  exhale e.message\n}\n");
    add("catch-in-callback", "exhale [1, 2].map(pack(x) {\n  try {\n    cough x\n  } ashtray e {\n    snuff e.value * 10\n  }\n})\n");
    add("try-in-loop", "roll t = 0\nfor i in 0..5 {\n  try {\n    if i % 2 == 0 {\n      cough i\n    }\n    t += 1\n  } ashtray e {\n    t += e.value * 10\n  }\n}\nexhale t\n");
    add("captured-binding", "roll fs = []\nfor i in 0..3 {\n  try {\n    cough i\n  } ashtray e {\n    fs.push(pack() => e.value)\n  }\n}\nexhale fs[0](), fs[1](), fs[2]()\n");
    add("catch-out-of-loops", "try {\n  for i in 0..3 {\n    roll n = 0\n    while true {\n      n += 1\n      if n > 2 {\n        cough \"x\"\n      }\n    }\n  }\n} ashtray e {\n  exhale e.message\n}\nroll k = 0\nwhile k < 2 {\n  k += 1\n}\nexhale \"done\", k\n");
    add("exit-in-try", "try {\n  exhale \"a\"\n  exit(3)\n} ashtray e {\n  exhale \"caught?\"\n} finally {\n  exhale \"fin\"\n}\n");
    add("cough-top", "roll a = 1\ncough \"top ${a}\"\n");
    add(
        "cough-map-top",
        "cough {message: \"structured\", code: 7}\n",
    );
    add("cough-int-top", "cough 5\n");
    add("recursion-caught", "pull r(n) {\n  snuff r(n + 1)\n}\ntry {\n  r(0)\n} ashtray e {\n  exhale e.kind, e.message\n}\n");
    add("try-in-pull-many", "pull safe(x) {\n  try {\n    snuff 10 / x\n  } ashtray e {\n    snuff -1\n  }\n}\nroll t = []\nfor x in [1, 0, 5, 0] {\n  t.push(safe(x))\n}\nexhale t\n");

    // ----- a prelude value used as a value -----
    add("prelude-builtin", "roll f = len\nstick p = path\nexhale f(\"abc\"), p.join(\"a\", \"b\"), type_of(f), type_of(p)\n");
    add("prelude-module", "roll m = math\nexhale m.floor(2.7), m\n");
    add(
        "prelude-args",
        "exhale args, len(args)\nroll a = args\na.push(\"x\")\nexhale args\n",
    );
    add("prelude-callback", "roll g = str\nexhale [1, 2].map(g)\n");
    add("prelude-wrong-arg", "roll f = len\nexhale f(5)\n");
    add(
        "prelude-shadowed",
        "roll len = 5\nexhale len\npull f(path) {\n  snuff path\n}\nexhale f(1)\n",
    );

    // ----- a pull named again by a later declaration -----
    // Once the read of the pull's name is propagated away, the alias is
    // declared with the closure's own value. A direct call still reads the
    // pull's slot: the alias's is empty until its statement runs, and may
    // be assigned again after it (#254).
    add(
        "pull-aliased-after-a-call",
        "pull o() {\n  pull f() {\n    snuff 1\n  }\n  exhale f()\n  roll g = f\n  exhale g()\n}\no()\no()\n",
    );
    add(
        "pull-aliased-then-the-alias-assigned",
        "pull o() {\n  pull f() {\n    snuff 1\n  }\n  roll g = f\n  g = pack() => 2\n  exhale [f(), g()]\n}\no()\no()\n",
    );
    add(
        "pull-aliased-at-the-top",
        "pull f() {\n  snuff 1\n}\nexhale f()\nstick g = f\nexhale [f(), g()]\n",
    );
    add(
        "pull-aliased-in-an-inner-block",
        "pull o() {\n  pull f() {\n    snuff 1\n  }\n  for n in [1, 2] {\n    stick g = f\n    exhale g() + f() + n\n  }\n  snuff f()\n}\nexhale o()\n",
    );
    add(
        "pull-aliased-twice-and-kept",
        "roll kept = []\npull o() {\n  pull f() {\n    snuff len(kept)\n  }\n  stick g = f\n  roll h = g\n  kept.push(h)\n  snuff [f(), g(), h()]\n}\nexhale o()\nexhale o()\nexhale kept.map(pack(k) => k())\n",
    );
    c
}

// ----- item 4: run it -----------------------------------------------------------

/// Every corpus program at -O0 and -O1: the value the top level leaves,
/// or the error's code, message, line and column, equal to the
/// tree-walker's; and the same number of steps.
#[test]
fn the_lowered_program_runs_like_the_interpreter() {
    big_stack(|| {
        let (mut runs, mut errors) = (0, 0);
        for (name, src) in corpus() {
            let (want, want_steps) = tree_walker(&src, 0);
            errors += usize::from(want.end.is_some());
            for level in [0, 1] {
                let m = compile(&src, level);
                let (got, got_steps) = vm(&m, 0);
                assert_eq!(got, want, "{name} -O{level}:\n{src}");
                assert_eq!(got_steps, want_steps, "{name} -O{level}: steps");
                runs += 1;
            }
        }
        println!(
            "{runs} runs agree ({} programs, {errors} of them stop on an error)",
            corpus().len()
        );
    });
}

/// A spent step budget (E515) is an error a `try` catches like any other:
/// the catch runs, and its first step mark raises it again; a catch with
/// no mark swallows it and the next mark after the `try` raises it. A
/// loop that never ends inside the body, so only the budget stops it.
#[test]
fn a_spent_budget_is_caught_and_raised_again_at_the_next_mark() {
    let cases = [
        (
            "try {\n  while true {\n    roll n = 1\n  }\n} ashtray e {\n  exhale e.message\n}\nexhale \"after\"\n",
            6,
        ),
        (
            "try {\n  while true {\n    roll n = 1\n  }\n} ashtray e {\n}\nexhale \"after\"\n",
            7,
        ),
        (
            "for i in 0..3 {\n  try {\n    while true {\n    }\n  } finally {\n    exhale i\n  }\n}\n",
            6,
        ),
    ];
    for (src, line) in cases {
        for level in [0, 1] {
            let m = compile(src, level);
            for max in [1, 2, 3, 10, 51, 200] {
                let (want, want_steps) = tree_walker(src, max);
                let (got, got_steps) = vm(&m, max);
                assert_eq!(got, want, "-O{level} --max-steps {max}:\n{src}");
                assert_eq!(got_steps, want_steps, "-O{level} --max-steps {max}: steps");
                if max >= 10 {
                    let end = got.end.as_ref().expect("the budget stops it");
                    assert_eq!((end.0.as_str(), end.3), ("E515", Some(line)), "{src}");
                }
            }
        }
    }
}

/// For each corpus program and every `--max-steps` from 1 to its total
/// (and one past it): the same stop, line, column and code, or the same
/// value when the budget is enough.
#[test]
fn the_step_budget_stops_on_the_same_line_through_bytecode() {
    big_stack(|| {
        let mut budgets = 0u64;
        for (name, src) in corpus() {
            let (_, total) = tree_walker(&src, 0);
            let m = compile(&src, 1);
            // Every budget up to 2,000 steps; past that (the deep
            // recursions), every 97th and the last fifty, which keeps the
            // run linear.
            let budgets_of =
                (1..=total + 1).filter(|b| total <= 2000 || b % 97 == 0 || *b + 50 > total);
            for max in budgets_of {
                let (want, _) = tree_walker(&src, max);
                let (got, _) = vm(&m, max);
                assert_eq!(got, want, "{name} with --max-steps {max}:\n{src}");
                budgets += 1;
            }
        }
        println!("{budgets} budgets agree");
    });
}

/// The repository's program generator: every program either lowers to a
/// module that verifies, or is refused with a construct this part does
/// not cover. Each generated program is then shrunk the way the
/// differential suite shrinks one, dropping the top-level statement a
/// refusal (or a check that a drop broke) names until it lowers; what
/// lowers verifies, and runs in the VM as it runs in the tree-walker.
#[test]
fn every_lowered_module_verifies() {
    big_stack(|| {
        const SEEDS: u64 = 3000;
        let (mut lowered, mut refused) = (0, 0);
        let mut constructs: std::collections::BTreeMap<String, u32> = Default::default();
        let (mut shrunk, mut kept_stmts, mut ran) = (0, 0, 0);
        // The generator's burns write under `out/`: here, a temporary
        // directory's, emptied before each side runs.
        let tmp = tempfile::tempdir().expect("a temporary directory");
        let out = tmp.path().join("out");
        let rooted = format!("\"{}/", out.to_string_lossy().replace('\\', "/"));
        let fresh = || {
            let _ = std::fs::remove_dir_all(&out);
        };
        let mut computed = 0;
        for seed in 0..SEEDS {
            let mut stmts: Vec<String> = gen::program(seed)
                .into_iter()
                .enumerate()
                .map(|(k, s)| computed_labels(&s.replace("\"out/", &rooted), seed + k as u64))
                .collect();
            let text = stmts.join("\n") + "\n";
            match bc::compile(&text, "gen.cig", &pass::default_passes()) {
                Ok(m) => {
                    verify_module(&m).unwrap_or_else(|e| panic!("seed {seed}: {e}\n{text}"));
                    lowered += 1;
                    if register_labeled(&m.main) {
                        computed += 1;
                    }
                }
                Err(CompileError::Bytecode(Error::Refused { construct, .. })) => {
                    *constructs.entry(construct).or_default() += 1;
                    refused += 1;
                }
                Err(e) => panic!("seed {seed}: {e:?}\n{text}"),
            }
            // Shrink to the part this lowering covers.
            loop {
                if stmts.is_empty() {
                    break;
                }
                let text = stmts.join("\n") + "\n";
                let line = match bc::compile(&text, "gen.cig", &pass::default_passes()) {
                    Ok(m) => {
                        verify_module(&m).unwrap_or_else(|e| panic!("seed {seed}: {e}\n{text}"));
                        shrunk += 1;
                        kept_stmts += stmts.len();
                        fresh();
                        let (got, _) = vm(&m, 0);
                        let got_files = files_under(&out);
                        fresh();
                        let (want, _) = tree_walker(&text, 0);
                        assert_eq!(got, want, "seed {seed}, shrunk:\n{text}");
                        assert_eq!(got_files, files_under(&out), "seed {seed}: files");
                        ran += 1;
                        break;
                    }
                    Err(CompileError::Bytecode(Error::Refused { span, .. })) => span.line,
                    Err(CompileError::Lower(cigscript::lower::cir::LowerError::Diagnostic(d))) => {
                        d.line.unwrap_or(1)
                    }
                    Err(e) => panic!("seed {seed}: {e:?}\n{text}"),
                };
                // The statement that holds that line.
                let mut first = 1;
                let mut drop = stmts.len() - 1;
                for (k, st) in stmts.iter().enumerate() {
                    let n = st.lines().count().max(1) as u32;
                    if line < first + n {
                        drop = k;
                        break;
                    }
                    first += n;
                }
                stmts.remove(drop);
            }
        }
        println!(
            "{lowered} of {SEEDS} generated programs lowered as they are, {refused} refused: {constructs:?}"
        );
        println!(
            "shrunk to this part: {shrunk} programs lowered ({kept_stmts} top-level statements), every one verified, {ran} ran like the tree-walker"
        );
        println!("{computed} of them burn under a computed label");
        assert_eq!(lowered + refused, SEEDS);
        assert!(shrunk > SEEDS / 2, "{shrunk}");
        assert!(computed > 0, "no generated burn has a computed label");
    });
}

/// The generator's labeled burns, `burn ("w") {`, some with the label
/// computed: `str("w")`, a state and `"w" + str(n)`, and now and then an
/// int (`len("w")`, the tree-walker's error at the label); the rest kept.
fn computed_labels(stmt: &str, seed: u64) -> String {
    let mut out = String::new();
    let mut rest = stmt;
    let mut k = seed;
    while let Some(at) = rest.find("burn (\"") {
        let (head, tail) = rest.split_at(at);
        out.push_str(head);
        let Some(end) = tail.find("\") {") else {
            out.push_str(tail);
            return out;
        };
        let word = &tail["burn (\"".len()..end];
        out.push_str(&match k % 7 {
            0 | 1 => format!("burn (str(\"{word}\")) {{"),
            2 | 3 => format!("burn (st_, \"{word}\" + str({})) {{", k % 5),
            4 if k % 21 == 4 => format!("burn (len(\"{word}\")) {{"),
            _ => format!("burn (\"{word}\") {{"),
        });
        rest = &tail[end + "\") {".len()..];
        k = k.wrapping_mul(31).wrapping_add(7);
    }
    out.push_str(rest);
    out
}

/// Some burn of `f` or a function under it takes its label from a
/// register.
fn register_labeled(f: &cigscript::cig_bytecode::Function) -> bool {
    f.regions
        .iter()
        .any(|r| matches!(r.label, cigscript::cig_bytecode::RegionLabel::Reg(_)))
        || f.children.iter().any(register_labeled)
}

/// What the bytecode cannot carry stops the lowering with an ordinary
/// error naming it and its position: a `pack` root that runs a pull or a
/// function (it could burn first, which the VM's `pack` refuses and the
/// tree-walker's does not).
#[test]
fn a_program_outside_this_part_is_refused_with_its_construct() {
    let cases = [
        (
            "pack { root() }\npull root() {\n  snuff \"./build\"\n}\n",
            "a `pack` root that runs a pull or a function",
            1,
            1,
        ),
        (
            "pack { [\"./b\"].map(pack(x) => x)[0] }\n",
            "a `pack` root that runs a pull or a function",
            1,
            1,
        ),
    ];
    for (src, construct, line, col) in cases {
        match bc::compile(src, "t.cig", &pass::level(0)) {
            Err(CompileError::Bytecode(Error::Refused { construct: c, span })) => {
                assert_eq!(c, construct, "{src}");
                assert_eq!((span.line, span.col), (line, col), "{src}: {c}");
            }
            other => panic!("{src}: {:?}", other.map(|_| ())),
        }
    }
}

/// Every CIR instruction kind, lowered or refused whatever its operands.
/// Every kind lowers; what is refused is refused for its operands (a
/// `pack` root that runs code). A new kind does not
/// compile here until it is added.
#[test]
fn every_cir_kind_is_lowered_or_refused() {
    fn name(k: &Kind) -> &'static str {
        match k {
            Kind::Step(_) => "step",
            Kind::Const(_) => "const",
            Kind::Prelude(_) => "prelude",
            Kind::Load(_) => "load",
            Kind::Decl(..) => "decl",
            Kind::Store(..) => "store",
            Kind::Unary(..) => "unary",
            Kind::Binary(..) => "binary",
            Kind::Short { .. } => "short",
            Kind::MakeList(_) => "list",
            Kind::MakeMap(_) => "map",
            Kind::Key(_) => "key",
            Kind::Interp(_) => "interp",
            Kind::Member { .. } => "member",
            Kind::Index { .. } => "index",
            Kind::SetIndex { .. } => "set.index",
            Kind::SetMember { .. } => "set.member",
            Kind::Closure(_) => "closure",
            Kind::BurnOp { .. } => "burn.op",
            Kind::CallBuiltin { .. } => "call.builtin",
            Kind::CallUser { .. } => "call.user",
            Kind::CallValue { .. } => "call.value",
            Kind::CallMethod { .. } => "call.method",
            Kind::MakeChain { .. } => "chain",
            Kind::PackDecl(_) => "pack",
            Kind::Exhale(_) => "exhale",
            Kind::Burn(_) => "burn",
            Kind::Try { .. } => "try",
            Kind::If { .. } => "if",
            Kind::Loop { .. } => "loop",
            Kind::ForEach { .. } => "foreach",
            Kind::Yield(_) => "yield",
            Kind::Ret(_) => "ret",
            Kind::Break => "break",
            Kind::Continue => "continue",
            Kind::Throw(_) => "throw",
        }
    }
    let v = ValueId(0);
    let kinds = vec![
        Kind::Step(StepSite::Stmt),
        Kind::Const(Const::Null),
        Kind::Prelude("args".into()),
        Kind::Load(VarId(0)),
        Kind::Decl(VarId(0), v),
        Kind::Store(VarId(0), v),
        Kind::Unary(UnOp::Neg, v),
        Kind::Binary(BinOp::Add, v, v),
        Kind::Short {
            op: ShortOp::And,
            subject: v,
            rhs: Block::default(),
        },
        Kind::MakeList(vec![]),
        Kind::MakeMap(vec![(MapKey::Static("k".into()), v)]),
        Kind::Key(v),
        Kind::Interp(vec![Piece::Lit("x".into())]),
        Kind::Member {
            obj: v,
            name: "k".into(),
            safe: false,
        },
        Kind::Index {
            obj: v,
            idx: v,
            idx_at: Span::default(),
        },
        Kind::SetIndex {
            obj: v,
            idx: v,
            op: AssignOp::Set,
            val: v,
            obj_at: Span::default(),
            idx_at: Span::default(),
        },
        Kind::SetMember {
            obj: v,
            name: "k".into(),
            op: AssignOp::Set,
            val: v,
            obj_at: Span::default(),
        },
        Kind::Closure(FuncId(1)),
        Kind::BurnOp {
            kind: OpKind::Write,
            callee: "fs.write_text".into(),
            effect: Effect::Write,
            args: vec![],
        },
        Kind::CallBuiltin {
            callee: "len".into(),
            args: vec![],
        },
        Kind::CallUser {
            func: FuncId(1),
            args: vec![],
        },
        Kind::CallValue { f: v, args: vec![] },
        Kind::CallMethod {
            recv: v,
            name: "len".into(),
            args: vec![],
        },
        Kind::MakeChain {
            name: VarId(0),
            steps: vec![],
        },
        Kind::PackDecl(vec![]),
        Kind::Exhale(vec![]),
        Kind::Burn(Box::new(BurnNode {
            region: RegionId(1),
            class: BurnClass::Normal,
            state: None,
            label: None,
            body: Block::default(),
            compensate: None,
            unburn_src: None,
            unburn_at: None,
            label_at: None,
        })),
        Kind::Try {
            body: Block::default(),
            catch: None,
            finally: None,
        },
        Kind::If {
            cond: v,
            then: Block::default(),
            otherwise: None,
        },
        Kind::Loop {
            cond: Block::default(),
            body: Block::default(),
        },
        Kind::ForEach {
            iter: v,
            var: VarId(0),
            body: Block::default(),
        },
        Kind::Yield(v),
        Kind::Ret(v),
        Kind::Break,
        Kind::Continue,
        Kind::Throw(v),
    ];
    let table: Vec<String> = kinds
        .iter()
        .map(|k| match bc::refused_kind(k) {
            None => format!("{} lowered", name(k)),
            Some(c) => format!("{} refused: {c}", name(k)),
        })
        .collect();
    let want = [
        "step lowered",
        "const lowered",
        "prelude lowered",
        "load lowered",
        "decl lowered",
        "store lowered",
        "unary lowered",
        "binary lowered",
        "short lowered",
        "list lowered",
        "map lowered",
        "key lowered",
        "interp lowered",
        "member lowered",
        "index lowered",
        "set.index lowered",
        "set.member lowered",
        "closure lowered",
        "burn.op lowered",
        "call.builtin lowered",
        "call.user lowered",
        "call.value lowered",
        "call.method lowered",
        "chain lowered",
        "pack lowered",
        "exhale lowered",
        "burn lowered",
        "try lowered",
        "if lowered",
        "loop lowered",
        "foreach lowered",
        "yield lowered",
        "ret lowered",
        "break lowered",
        "continue lowered",
        "throw lowered",
    ];
    assert_eq!(table, want);
}

// ----- operands past 8 and 16 bits ---------------------------------------------------

/// Three hundred slots: registers past 255 take the `WIDE` forms, and the
/// program still runs like the interpreter.
#[test]
fn a_frame_past_255_registers_uses_wide_operands() {
    let mut src = String::new();
    for k in 0..300 {
        src.push_str(&format!("roll v{k} = {k}\n"));
    }
    src.push_str("roll s = 0\n");
    for k in (0..300).step_by(7) {
        src.push_str(&format!("s += v{k} * 3 - v{} \n", 299 - k));
    }
    src.push_str("exhale s + len(str(v299))\n");
    big_stack(move || {
        let m = compile(&src, 0);
        assert!(m.main.reg_count > 256, "{}", m.main.reg_count);
        let text = bc::disassemble(&m);
        assert!(text.contains(" w "), "a wide instruction");
        assert_eq!(vm(&m, 0), tree_walker(&src, 0));
    });
}

/// A loop whose body is longer than a 16-bit jump: the function is
/// lowered again with wide jumps.
#[test]
fn a_jump_past_16_bits_is_wide() {
    let mut src = String::from("roll n = 0\nroll i = 0\nwhile i < 3 {\n  i += 1\n");
    for _ in 0..17000 {
        src.push_str("  n += i\n");
    }
    src.push_str("}\nexhale n\n");
    big_stack(move || {
        let m = compile(&src, 0);
        assert!(m.main.instrs.len() > 32768, "{}", m.main.instrs.len());
        assert_eq!(vm(&m, 0), tree_walker(&src, 0));
    });
}

// ----- item 3: see it ---------------------------------------------------------------

/// `--emit bc`'s text, byte for byte: tests/lower/bc/<name>.bc from
/// tests/lower/bc/<name>.cig at -O1. After a deliberate change,
/// BC_UPDATE_GOLDEN=1 writes them again.
#[test]
fn the_disassembly_matches_its_golden_text() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/lower/bc");
    let lf = |t: String| t.replace("\r\n", "\n");
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| {
            let p = e.unwrap().path();
            if p.extension()? != "cig" {
                return None;
            }
            Some(p.file_stem()?.to_string_lossy().into_owned())
        })
        .collect();
    names.sort();
    assert!(!names.is_empty());
    for name in names {
        let file = format!("{name}.cig");
        let src = lf(std::fs::read_to_string(dir.join(&file)).unwrap());
        let m = match bc::compile(&src, &file, &pass::default_passes()) {
            Ok(m) => m,
            Err(e) => panic!("{file}: {e:?}"),
        };
        let text = bc::disassemble(&m);
        let golden = dir.join(format!("{name}.bc"));
        if std::env::var_os("BC_UPDATE_GOLDEN").is_some() {
            std::fs::write(&golden, &text).unwrap();
        }
        assert_eq!(
            text,
            lf(std::fs::read_to_string(&golden).unwrap_or_default()),
            "{file}: BC_UPDATE_GOLDEN=1 writes it again"
        );
    }
}

/// The passes shape the bytecode: a constant expression -O1 folds is
/// one load, where -O0 computes it (`addk`, the constant an operand).
#[test]
fn the_level_shapes_the_bytecode() {
    let src = "stick a = 2 + 3\nexhale a * 4\n";
    let o0 = compile(src, 0);
    let o1 = compile(src, 1);
    let ops = |m: &Module| bc::disassemble(m).matches(" add").count();
    assert_eq!(ops(&o0), 1);
    assert_eq!(ops(&o1), 0);
    let _: &[&dyn Pass] = &pass::level(1);
}

/// More live slots than a frame can hold: an ordinary error at the
/// statement that crosses the limit, not a module the verifier refuses.
#[test]
fn a_frame_past_the_limit_is_an_error() {
    let mut src = String::new();
    for k in 0..65_540 {
        src.push_str(&format!("roll v{k} = 1\n"));
    }
    match bc::compile(&src, "t.cig", &pass::level(0)) {
        Err(CompileError::Bytecode(Error::Limit { what, span })) => {
            assert_eq!(
                what,
                "the function needs more than 65536 registers, the frame limit"
            );
            // Each slot is made in its own register, the first at R0.
            assert_eq!(span.line, 65_537);
        }
        other => panic!("{:?}", other.map(|_| ())),
    }
}

/// How far stage B is from real programs: every script of the CigScript
/// checkout `CIGSCRIPT_SRC` names (`examples`, `tests/scripts`,
/// `tests/corpus`, as the differential suite reads them) and
/// `orchestrate.cig` of the checkout `CIGO_SRC` names, each lowered at
/// -O1 or named by the construct that first refuses it. A count, not a
/// gate: what lowers verifies, and nothing may fail inside the lowering.
#[test]
fn real_programs_lower_or_name_what_refuses_them() {
    let mut scripts = Vec::new();
    // This checkout's scripts, or another's when `CIGSCRIPT_SRC` names one.
    {
        let src = std::env::var_os("CIGSCRIPT_SRC")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")));
        for dir in ["examples", "tests/scripts", "tests/corpus"] {
            if let Ok(rd) = std::fs::read_dir(src.join(dir)) {
                let mut found: Vec<_> = rd
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x == "cig"))
                    .collect();
                found.sort();
                scripts.extend(found.into_iter().map(|p| (format!("{dir}/"), p)));
            }
        }
    }
    if let Some(src) = std::env::var_os("CIGO_SRC").map(std::path::PathBuf::from) {
        scripts.push((String::new(), src.join("orchestrate.cig")));
    }
    if scripts.is_empty() {
        eprintln!("note: skipping; set CIGSCRIPT_SRC and CIGO_SRC to checkouts");
        return;
    }
    let (mut lowered, mut refused, mut unchecked, mut ran) = (0, 0, 0, 0);
    let mut firsts: std::collections::BTreeMap<String, u32> = Default::default();
    for (dir, path) in &scripts {
        let name = format!("{dir}{}", path.file_name().unwrap().to_string_lossy());
        let src = std::fs::read_to_string(path).unwrap();
        let what = match bc::compile(&src, &name, &pass::default_passes()) {
            Ok(m) => {
                verify_module(&m).unwrap_or_else(|e| panic!("{name}: {e}"));
                lowered += 1;
                // The language scripts that burn nothing need no files: run
                // them both ways here. The others run in an empty directory
                // of their own, for real and as a dry run, and leave the
                // same files, journal and plan (a script that burns would
                // otherwise write into the directory the tests run in); one
                // that starts a process runs only where CIG_BC_RUN_PROCS is
                // set (the test box).
                if dir == "tests/scripts/" && !src.contains("burn") {
                    let src = src.clone();
                    let (got, want) =
                        big_stack(move || (vm(&m, 1_000_000), tree_walker(&src, 1_000_000)));
                    assert_eq!(got, want, "{name}");
                    ran += 1;
                    "lowered, and runs like the tree-walker".to_string()
                } else if src.contains("proc.") && std::env::var_os("CIG_BC_RUN_PROCS").is_none() {
                    "lowered (it starts a process: run with CIG_BC_RUN_PROCS on the test box)"
                        .to_string()
                } else {
                    script_agrees(path, &name);
                    ran += 1;
                    "lowered, and runs like the tree-walker in a directory of its own".to_string()
                }
            }
            Err(CompileError::Bytecode(Error::Refused { construct, span })) => {
                refused += 1;
                *firsts.entry(construct.clone()).or_default() += 1;
                format!("refused at {}:{}: {construct}", span.line, span.col)
            }
            Err(CompileError::Lower(cigscript::lower::cir::LowerError::Diagnostic(d))) => {
                unchecked += 1;
                format!("does not check: {} {}", d.code, d.message)
            }
            Err(e) => panic!("{name}: {e:?}"),
        };
        println!("{name}: {what}");
    }
    println!(
        "{} scripts: {lowered} lowered ({ran} of them ran like the tree-walker), {refused} refused, {unchecked} do not check; first refusals: {firsts:?}",
        scripts.len()
    );
}

// ----- burns, `pack` and chains (the fourth part) -------------------------------------

/// Every file under `dir` with its text (`<dir>` for a directory), sorted.
fn files_under(dir: &std::path::Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let name = p
                .strip_prefix(dir)
                .unwrap_or(&p)
                .to_string_lossy()
                .into_owned();
            if p.is_dir() {
                out.push((name, "<dir>".to_string()));
                todo.push(p);
            } else {
                out.push((name, std::fs::read_to_string(&p).unwrap_or_default()));
            }
        }
    }
    out.sort();
    out
}

/// `path` run in an empty directory of its own as its working directory:
/// this test binary again, running only
/// `a_script_runs_like_the_interpreter_in_its_own_directory`, there, with
/// a scratch home. The working directory is the process's, so it is never
/// moved in this one.
fn script_agrees(path: &std::path::Path, name: &str) {
    script_agrees_with(path, name, &[], &[], false);
}

/// `script_agrees` with the script's `args`, `files` written into its
/// directory before each run, and, with `budget`, each real run again
/// with half the steps it took as its step budget.
fn script_agrees_with(
    path: &std::path::Path,
    name: &str,
    args: &[&str],
    files: &[(&str, &str)],
    budget: bool,
) {
    let seed = tempfile::tempdir().expect("a seed directory");
    for (f, text) in files {
        std::fs::write(seed.path().join(f), text).expect("a seed file");
    }
    let dir = tempfile::tempdir().expect("a temporary directory");
    let home = tempfile::tempdir().expect("a scratch home");
    let h = home.path();
    let path = std::fs::canonicalize(path).expect("the script");
    let o = std::process::Command::new(std::env::current_exe().expect("this test binary"))
        .args([
            "--exact",
            "a_script_runs_like_the_interpreter_in_its_own_directory",
            "--test-threads=1",
            "--nocapture",
        ])
        .current_dir(dir.path())
        .env("CIG_BC_SCRIPT", &path)
        .env("CIG_BC_SCRIPT_NAME", name)
        .env("CIG_BC_SCRIPT_ARGS", args.join("\n"))
        .env("CIG_BC_SCRIPT_SEED", seed.path())
        .env("CIG_BC_SCRIPT_BUDGET", if budget { "1" } else { "" })
        .env("CIGO_HOME", h.join(".cig-orchestrator"))
        .env("HOME", h)
        .env("CIGSCRIPT_HOME", h.join(".cigscript"))
        .env("XDG_CONFIG_HOME", h.join(".config"))
        .env("XDG_DATA_HOME", h.join(".local/share"))
        .env("XDG_CACHE_HOME", h.join(".cache"))
        .env("XDG_STATE_HOME", h.join(".local/state"))
        .env("CIG_SEAL_KEY", "file")
        .env("CIG_AUTO_LOWER", "off")
        .output()
        .expect("the test binary runs");
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        o.status.success() && out.contains("1 passed"),
        "{name}:\n{out}\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
    if budget {
        for l in out.lines().filter_map(|l| l.find(name).map(|at| &l[at..])) {
            println!("{l}");
        }
    }
}

/// cigo's own program, `orchestrate.cig` of the checkout `CIGO_SRC`
/// names, on the commands that start no process and need no box or
/// network: `--version`, `help`, `list` and `list --all` in a directory
/// with a small `cig-tasks.json`, `list` with none, an unknown command.
/// Each lowered at -O1 and run as a script, for real and as a dry run,
/// and the real run again with half its steps as the budget, held to
/// the tree-walker. Its whole suite against a compiled cigo runs on the
/// test box.
#[test]
fn cigos_program_runs_like_the_interpreter_without_a_box() {
    let Some(src) = std::env::var_os("CIGO_SRC").map(std::path::PathBuf::from) else {
        eprintln!("note: skipping; set CIGO_SRC to a cig-orchestrator checkout");
        return;
    };
    let path = src.join("orchestrate.cig");
    let tasks = "{\n  \"project\": \"demo\",\n  \"tasks\": {\n    \"lint\": {\"cmd\": [\"python3\", \"lint.py\"], \"desc\": \"style\"},\n    \"build\": {\"cmd\": [\"sh\", \"build.sh\"], \"needs\": [\"lint\"], \"desc\": \"the app\"},\n    \"test\": {\"cmd\": [\"sh\", \"test.sh\"], \"needs\": [\"build\"], \"timeout_ms\": 1000}\n  }\n}\n";
    let with = [("cig-tasks.json", tasks)];
    // Each command's arguments and the files its directory starts with.
    type Command<'a> = (&'a [&'a str], &'a [(&'a str, &'a str)]);
    let runs: [Command; 6] = [
        (&["--version"], &[]),
        (&["help"], &[]),
        (&["list"], &with),
        (&["list", "--all"], &with),
        (&["list"], &[]),
        (&["no-such-command"], &with),
    ];
    for (args, files) in runs {
        script_agrees_with(&path, "orchestrate.cig", args, files, true);
    }
}

/// The script `CIG_BC_SCRIPT` names on the tree-walker and, lowered at
/// -O1, on the VM as a script (`vm::run_script`), each in the emptied
/// working directory (the seed's files copied in) and an emptied scratch
/// home, with the arguments `CIG_BC_SCRIPT_ARGS` gives, for real and as a
/// dry run: the same output, ending, steps, journal, plan, rollback and
/// files; with `CIG_BC_SCRIPT_BUDGET`, the real run again with half its
/// steps as the budget. Started by `script_agrees`; without
/// `CIG_BC_SCRIPT` it does nothing.
#[test]
fn a_script_runs_like_the_interpreter_in_its_own_directory() {
    let Some(path) = std::env::var_os("CIG_BC_SCRIPT") else {
        return;
    };
    let name = std::env::var("CIG_BC_SCRIPT_NAME").unwrap_or_default();
    let args: Vec<String> = std::env::var("CIG_BC_SCRIPT_ARGS")
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect();
    let seed = std::env::var_os("CIG_BC_SCRIPT_SEED").map(std::path::PathBuf::from);
    let budget = std::env::var("CIG_BC_SCRIPT_BUDGET").is_ok_and(|b| b == "1");
    // The scratch home `script_agrees` made, emptied too: never another.
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .filter(|h| h.starts_with(std::env::temp_dir()));
    let src = std::fs::read_to_string(&path).expect("the script");
    big_stack(move || {
        let dir = std::env::current_dir().expect("a working directory");
        // What is emptied before each run is the temporary directory
        // `script_agrees` started this process in: never any other.
        let tmp = std::env::temp_dir()
            .canonicalize()
            .expect("the temporary directory");
        assert!(
            dir.canonicalize()
                .is_ok_and(|d| d != tmp && d.starts_with(&tmp)),
            "{}: `CIG_BC_SCRIPT` is for `script_agrees`, which starts this test in a temporary directory of its own",
            dir.display()
        );
        let clear = |d: &std::path::Path| {
            for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
                let p = e.path();
                let _ = std::fs::remove_dir_all(&p);
                let _ = std::fs::remove_file(&p);
            }
        };
        let empty = || {
            clear(&dir);
            if let Some(h) = &home {
                clear(h);
            }
            for e in seed
                .iter()
                .flat_map(|s| std::fs::read_dir(s).into_iter().flatten().flatten())
            {
                std::fs::copy(e.path(), dir.join(e.file_name())).expect("a seed file");
            }
        };
        let m = match bc::compile(&src, &name, &pass::default_passes()) {
            Ok(m) => m,
            Err(e) => panic!("{name}: {e:?}"),
        };
        let program = cigscript::syntax::parse(&src).expect("parses");
        let verified = verify_module(&m).unwrap_or_else(|e| panic!("{name}: {e}"));
        let p = cigscript::vm::load(verified).unwrap_or_else(|e| panic!("{name}: {}", e.message));
        let both = |mode: Mode, max_steps: u64| {
            empty();
            let want = burn_run(&[], Some(&dir), mode, max_steps, |i| {
                i.set_args(&args);
                i.run_program(&program)
            });
            empty();
            let got = burn_run(&[], Some(&dir), mode, max_steps, |i| {
                i.set_args(&args);
                cigscript::vm::run_script(i, &p)
            });
            assert_eq!(got, want, "{name} {args:?} {mode:?} budget {max_steps}");
            want
        };
        for mode in [Mode::Run, Mode::DryRun] {
            let left = both(mode, if budget { 1_000_000_000 } else { 1_000_000 });
            println!(
                "{name} {args:?} {mode:?}: {} steps, {} printed bytes, exit {:?}, ending {:?}, {} journaled, {} planned",
                left.steps,
                left.outcome.printed.len(),
                left.outcome.exit,
                left.outcome.end.as_ref().map(|e| e.0.as_str()),
                left.journal.len(),
                left.plan.len()
            );
            if budget && mode == Mode::Run {
                let half = both(mode, (left.steps / 2).max(1));
                println!(
                    "{name} {args:?} with a budget of {}: ending {:?}",
                    left.steps / 2,
                    half.outcome.end.as_ref().map(|e| (e.0.as_str(), e.3, e.4))
                );
            }
        }
        empty();
    });
}

/// What a run left, with its burns: the bytes printed and logged (the
/// chain runner's times taken out), the ending, the steps, the files, the
/// journal without its times, the plan, and what a rollback did and left.
#[derive(Debug, PartialEq)]
struct Left {
    outcome: Outcome,
    steps: u64,
    files: Vec<Option<String>>,
    journal: Vec<serde_json::Value>,
    plan: Vec<String>,
    rollback: Vec<(String, String)>,
    rolled_back: Vec<Option<String>>,
    /// Every file under the run's directory, before and after the
    /// rollback, when it has one.
    tree: Vec<(String, String)>,
    tree_rolled_back: Vec<(String, String)>,
    still_burning: bool,
}

fn files_now(paths: &[String]) -> Vec<Option<String>> {
    paths
        .iter()
        .map(|p| {
            let p = std::path::Path::new(p);
            if p.is_dir() {
                Some("<dir>".to_string())
            } else {
                std::fs::read_to_string(p).ok()
            }
        })
        .collect()
}

/// `text` without the times the chain runner prints, `(14 ms)`.
fn untimed(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(" (") {
        let (head, tail) = rest.split_at(at);
        out.push_str(head);
        let inner = &tail[2..];
        match inner.find(" ms)") {
            Some(end) if inner[..end].chars().all(|c| c.is_ascii_digit()) => {
                rest = &inner[end + 4..];
            }
            _ => {
                out.push_str(" (");
                rest = inner;
            }
        }
    }
    out.push_str(rest);
    out
}

/// One run in `mode` with the files `paths` gone first: `go` runs the
/// program on the interpreter it is given.
fn burn_run(
    paths: &[String],
    under: Option<&std::path::Path>,
    mode: Mode,
    max_steps: u64,
    go: impl FnOnce(&mut Interp) -> Result<(), Diagnostic>,
) -> Left {
    for p in paths {
        let p = std::path::Path::new(p);
        let _ = std::fs::remove_dir_all(p);
        let _ = std::fs::remove_file(p);
    }
    let mut run = interp(max_steps);
    run.interp.kernel = Kernel::ephemeral(mode);
    let r = go(&mut run.interp);
    let (mut outcome, steps) = outcome(r, &run);
    outcome.logged = untimed(&outcome.logged);
    let i = &mut run.interp;
    let journal = i
        .kernel
        .journal()
        .entries()
        .iter()
        .map(|e| {
            let mut v = serde_json::to_value(e).expect("an entry is json");
            if let Some(o) = v.as_object_mut() {
                o.remove("at");
            }
            v
        })
        .collect();
    let plan = i
        .kernel
        .planned
        .iter()
        .map(|p| serde_json::to_string(p).expect("a planned op is json"))
        .collect();
    let files = files_now(paths);
    let tree = under.map(files_under).unwrap_or_default();
    let report = i.kernel.rollback();
    Left {
        tree,
        tree_rolled_back: under.map(files_under).unwrap_or_default(),
        outcome,
        steps,
        files,
        journal,
        plan,
        rollback: report.actions,
        rolled_back: files_now(paths),
        still_burning: i.in_burn() || i.in_unlit(),
    }
}

/// `template` with `P0`, `P1`, .. replaced by the paths of `names` in a
/// fresh directory, run by the tree-walker and, lowered at -O0 and -O1, by
/// the VM, for real and as a dry run, with `max_steps`: everything they
/// leave is the same. Returns the tree-walker's real run and dry run.
fn burns_agree(template: &str, names: &[&str], max_steps: u64) -> (Left, Left) {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let paths: Vec<String> = names
        .iter()
        .map(|n| dir.path().join(n).to_string_lossy().into_owned())
        .collect();
    // One pass over the template: a temporary path may itself hold `P1`.
    let src = regex::Regex::new(r"P(\d+)")
        .expect("a literal pattern")
        .replace_all(template, |c: &regex::Captures| {
            format!("{:?}", paths[c[1].parse::<usize>().expect("digits")])
        })
        .into_owned();
    let program = cigscript::syntax::parse(&src).expect("parses");
    let modules: Vec<(u8, Module)> = [0, 1].into_iter().map(|l| (l, compile(&src, l))).collect();
    let mut runs = Vec::new();
    for mode in [Mode::Run, Mode::DryRun] {
        let want = burn_run(&paths, None, mode, max_steps, |i| i.run_program(&program));
        assert!(
            !want.still_burning,
            "the tree-walker left a burn open:\n{src}"
        );
        for (level, m) in &modules {
            let verified = verify_module(m).unwrap_or_else(|e| panic!("does not verify: {e}"));
            let p = cigscript::vm::load(verified)
                .unwrap_or_else(|e| panic!("does not load: {}", e.message));
            let got = burn_run(&paths, None, mode, max_steps, |i| {
                cigscript::vm::run(i, &p).map(|_| ())
            });
            assert_eq!(got, want, "-O{level} {mode:?}:\n{src}");
        }
        runs.push(want);
    }
    let dry = runs.pop().expect("a dry run");
    (runs.pop().expect("a real run"), dry)
}

/// The burn instructions (`burn.enter`, `comp.enter`, `comp.leave`,
/// `burn.leave`) of `f` and its children that no run reaches: from pc 0
/// and every handler's `handler_pc` and `finally_pc`, along every
/// fall-through (none after `ret`, `tailcall`, `throw`, `jmp`, `loop`) and
/// every jump operand. Each is `(function, pc, mnemonic)`.
fn unreached_burn_instrs(f: &cigscript::cig_bytecode::Function, at: &str) -> Vec<String> {
    use cigscript::cig_bytecode::{instr::walk, Op, OperandKind};
    let decoded: Vec<_> = walk(&f.instrs).map(|d| d.expect("decodes")).collect();
    let index: std::collections::HashMap<u32, usize> =
        decoded.iter().enumerate().map(|(k, d)| (d.pc, k)).collect();
    let mut seen = vec![false; decoded.len()];
    let mut todo = vec![0u32];
    for h in &f.handlers {
        todo.push(h.handler_pc);
        if h.finally_pc != cigscript::cig_bytecode::RESERVED_U32 {
            todo.push(h.finally_pc);
        }
    }
    while let Some(pc) = todo.pop() {
        let Some(&k) = index.get(&pc) else {
            continue;
        };
        if std::mem::replace(&mut seen[k], true) {
            continue;
        }
        let d = decoded[k];
        let info = d.op.info();
        let next = d.pc + d.width;
        if info.b == OperandKind::Jump {
            todo.push((next as i64 + d.b_s as i64) as u32);
        }
        if info.c == OperandKind::Jump {
            todo.push((next as i64 + d.c_s as i64) as u32);
        }
        if !matches!(
            d.op,
            Op::RET | Op::TAILCALL | Op::THROW | Op::JMP | Op::LOOP
        ) {
            todo.push(next);
        }
    }
    let mut out: Vec<String> = decoded
        .iter()
        .zip(&seen)
        .filter(|(d, seen)| {
            !**seen
                && matches!(
                    d.op,
                    Op::BURN_ENTER | Op::COMP_ENTER | Op::COMP_LEAVE | Op::BURN_LEAVE
                )
        })
        .map(|(d, _)| format!("{at}: {:04} {}", d.pc, d.op.mnemonic()))
        .collect();
    for (k, c) in f.children.iter().enumerate() {
        out.extend(unreached_burn_instrs(c, &format!("{at}/{k}")));
    }
    out
}

/// No way out of a burn leaves an instruction after it that no run
/// reaches: `break` and `continue` (out of one burn and out of two),
/// `snuff`, `cough`, a burn ending in another that ends so; nor a burn
/// in a loop, a `try`, a pull. Each program of the burn corpus and the
/// goldens, at -O0 and -O1.
#[test]
fn no_way_out_of_a_burn_leaves_an_instruction_no_run_reaches() {
    let golden = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/lower/bc");
    let mut programs: Vec<(String, String)> = burn_corpus()
        .into_iter()
        .map(|(name, src, files)| {
            // Compiled, not run: any path will do.
            let mut src = src.to_string();
            for k in (0..files.len()).rev() {
                src = src.replace(&format!("P{k}"), &format!("\"f{k}\""));
            }
            (name.to_string(), src)
        })
        .collect();
    for g in ["burns", "loops", "errors"] {
        let src = std::fs::read_to_string(golden.join(format!("{g}.cig"))).unwrap();
        programs.push((format!("{g}.cig"), src.replace("\r\n", "\n")));
    }
    let mut checked = 0;
    for (name, src) in &programs {
        for level in [0, 1] {
            let m = compile(src, level);
            let dead = unreached_burn_instrs(&m.main, "fn0");
            assert!(dead.is_empty(), "{name} -O{level}: {dead:?}");
            checked += 1;
        }
    }
    // The shapes the old lowering wrote dead code for are among them.
    for name in [
        "break",
        "ends-in-break",
        "ends-in-continue",
        "ends-in-snuff",
        "ends-in-cough",
    ] {
        assert!(programs.iter().any(|(n, _)| n == name), "{name}");
    }
    println!("{checked} modules, no burn instruction no run reaches");
}

#[test]
fn burns_run_like_the_interpreter() {
    big_stack(|| {
        let mut n = 0;
        for (name, src, files) in burn_corpus() {
            let (left, dry) = burns_agree(src, &files, 0);
            println!(
                "{name}: {} journaled, {} planned in the dry run, {} rolled back, ending {:?}",
                left.journal.len(),
                dry.plan.len(),
                left.rollback.len(),
                left.outcome.end.as_ref().map(|e| e.0.as_str())
            );
            n += 1;
        }
        println!("{n} burn programs agree, each -O0 and -O1, run and dry run");
    });
}

/// A compensation is recorded with its label, its state's name and the
/// `unburn` block's line, as the tree-walker records it; one whose block
/// failed is not.
#[test]
fn a_compensation_is_journaled_with_its_line_and_label() {
    big_stack(|| {
        let src = "burn (s, \"deploy\") {\n  s.path = P0\n  fs.write_text(P0, \"a\")\n}\n\n\nunburn {\n  fs.rm(s.path)\n}\n";
        let (left, _) = burns_agree(src, &["a.txt"], 0);
        let comp = left
            .journal
            .iter()
            .find_map(|e| e.get("compensation"))
            .expect("a compensation was journaled");
        assert_eq!(comp["label"], "deploy");
        assert_eq!(comp["state_name"], "s");
        assert_eq!(comp["line"], 7);
        assert_eq!(left.rolled_back, vec![None]);
        let (left, _) = burns_agree(
            "burn (s) {\n  fs.write_text(P0, \"a\")\n  cough \"no\"\n} unburn {\n  exhale s\n}\n",
            &["a.txt"],
            0,
        );
        assert!(left.journal.iter().all(|e| e.get("compensation").is_none()));
        assert!(left.outcome.end.is_some());
    });
}

/// Every step budget from 1 to the total (and one past it) stops on the
/// same step, line and column across burns, with the same files, journal
/// and plan.
#[test]
fn the_step_budget_stops_on_the_same_line_across_burns() {
    big_stack(|| {
        let mut budgets = 0;
        for (name, src, files) in burn_corpus() {
            let total = burns_agree(src, &files, 0).0.steps;
            for max in 1..=total + 1 {
                burns_agree(src, &files, max);
                budgets += 1;
            }
            let _ = name;
        }
        println!("{budgets} budgets agree across burns");
    });
}

/// The bytecode a burn is: the state map, `burn.enter` with the region,
/// `comp.enter` on the `unburn`'s line, `comp.leave`, `burn.leave`; the
/// region's label a constant; a pull with an effect outside its own burns
/// marked `requires_burn`, a pull whose effects are all in its own burns
/// not.
#[test]
fn a_burn_lowers_as_the_format_lays_it_out() {
    let src = "pull w(p) {\n  fs.write_text(p, \"a\")\n}\npull v(p) {\n  burn unlit {\n    fs.write_text(p, \"b\")\n  }\n}\nburn (s, \"deploy\") {\n  s.n = 1\n  w(\"x\")\n} unburn {\n  exhale s.n\n}\n";
    let m = compile(src, 0);
    let main = &m.main;
    assert_eq!(main.regions.len(), 1);
    let cigscript::cig_bytecode::RegionLabel::Const(label) = main.regions[0].label else {
        panic!("a literal label is a constant: {:?}", main.regions[0].label);
    };
    assert!(
        matches!(&main.constants[label as usize], cigscript::cig_bytecode::Constant::Str(s) if s == "deploy")
    );
    assert_eq!(main.regions[0].class, 0);
    assert_eq!(main.region_rows.len(), 1);
    let text = bc::disassemble(&m);
    for op in [
        "newmap",
        "burn.enter",
        "comp.enter",
        "comp.leave",
        "burn.leave",
    ] {
        assert!(text.contains(op), "{op}:\n{text}");
    }
    let w = main.children.iter().find(|c| c.name == "w").expect("w");
    let v = main.children.iter().find(|c| c.name == "v").expect("v");
    assert!(w.requires_burn);
    assert!(!v.requires_burn);
    assert_eq!(v.regions[0].class, 1);
    assert!(!main.requires_burn);
    // The `unburn`'s line is `comp.enter`'s row.
    let comp_pc = main
        .instrs
        .iter()
        .position(|i| i.op() == Some(cigscript::cig_bytecode::Op::COMP_ENTER))
        .expect("comp.enter") as u32;
    let row = main.lines.iter().find(|r| r.pc == comp_pc).expect("a row");
    assert_eq!(main.span_base.line + row.line_off, 12);
}

/// The compiler's op kinds and the VM's (`vm::burn_op_kind`) agree for
/// every callee the compiler knows: one kind on each side, the same kind.
#[test]
fn the_op_kinds_are_the_vms() {
    use cigscript::lower::cir::effects::kind_of;
    let mut checked = 0;
    for (_, rows) in cigscript::stdlib::catalogue() {
        for (name, effect, _, _) in rows {
            let ours = kind_of(name);
            let vms = cigscript::vm::burn_op_kind(name).map(|k| cigscript::vm::BURN_OP_KINDS[k]);
            assert_eq!(ours.is_some(), vms.is_some(), "{name}");
            assert_eq!(effect.needs_burn(), ours.is_some(), "{name}");
            let (Some(ours), Some(vms)) = (ours, vms) else {
                continue;
            };
            let want = match ours {
                OpKind::Write => "write",
                OpKind::Append => "append",
                OpKind::Rm => "delete",
                OpKind::Mv => "move",
                OpKind::Cp => "copy",
                OpKind::Mkdir => "mkdir",
                OpKind::Hop => "proc",
                OpKind::EnvSet => "env_set",
                OpKind::EnvUnset => "env_unset",
                OpKind::LayerApply => "layer_apply",
                OpKind::LayerDiscard => "layer_discard",
            };
            assert_eq!(vms, want, "{name}");
            checked += 1;
        }
    }
    assert!(checked >= 20, "{checked}");
    println!("{checked} callees: the compiler's kind is the VM's");
}

/// A `burn.op` past 8 bits (a callee constant past 255, a window past
/// register 255) takes `WIDE`, with the kind in C's low byte and the
/// count in its high byte, and runs.
#[test]
fn a_wide_burn_op_runs() {
    let mut tmpl = String::new();
    for k in 0..300 {
        tmpl.push_str(&format!("roll v{k} = \"k{k}\"\n"));
    }
    tmpl.push_str("burn {\n  fs.write_text(P0, v0 + v299)\n}\nexhale v1\n");
    let tmpl: &'static str = Box::leak(tmpl.into_boxed_str());
    big_stack(move || {
        let m = compile(&tmpl.replace("P0", "\"x\""), 0);
        let text = bc::disassemble(&m);
        assert!(
            text.lines()
                .any(|l| l.contains("burn.op") && l.contains(" w ")),
            "{text}"
        );
        burns_agree(tmpl, &["a.txt"], 0);
    });
}

/// `pack` and chains: each root and step is checked where the tree-walker
/// checks it, so a bad part before one whose evaluation fails, prints or
/// runs is the error, at its own position; then the chains the language
/// scripts build.
#[test]
fn pack_and_chains_run_like_the_interpreter() {
    big_stack(|| {
        let cases = [
            // Roots.
            "pack { 5, 1 / 0 }\nexhale \"no\"\n",
            "pack { 5 }\n",
            "pack { \"./a\", 5, 1 / 0 }\n",
            "pack { \" \" + \"\", 1 / 0 }\n",
            "pack { \"\" + \"\" }\n",
            "pack { \"./a\", null }\n",
            "pack { \"./a\", len(args) }\n",
            "pack { 5, [][1] }\n",
            "pack { \"./a\", \"./b\" }\nexhale \"ok\"\n",
            "pack { str(1) + \"\", \"\" + \"x\" }\nexhale \"ok\"\n",
            // Steps.
            "stick a = pack(x) => x + 1\nchain c { a, 5 }\n",
            "stick a = pack(x) => x + 1\nchain c { 5, 1 / 0 }\n",
            "stick a = pack(x) => x + 1\nchain c { a, 5, [][1] }\n",
            "pull a(x) {\n  snuff x + 1\n}\npull b(x) {\n  snuff x * 2\n}\nchain c { a, b }\nexhale c, c.len, c.steps\nexhale light(c, {quiet: true}).result\n",
            "pull a() {\n  snuff 1\n}\nchain inner { a }\nchain outer { inner, pack(x) => x + 1 }\nexhale outer.steps, light(outer, {quiet: true}).result\n",
            "stick m = {f: pack() => 3}\nchain c { m.f }\nexhale c.steps, light(c, {quiet: true}).result\n",
            "chain c { len }\nexhale c.steps\n",
            "pull f() {\n  chain c { f }\n  snuff c\n}\nexhale f().name\n",
            "for i in 0..2 {\n  chain c { pack() => i }\n  exhale light(c, {quiet: true}).result\n}\n",
            "stick a = pack() => 1\nchain c { a }\nstick g = pack() => c.name\nexhale g()\n",
            "chain c { pack() { cough \"boom\" } }\nexhale light(c).ok\n",
        ];
        let mut n = 0;
        for src in cases {
            let (want, want_steps) = tree_walker(src, 0);
            for level in [0, 1] {
                let m = compile(src, level);
                let (mut got, got_steps) = vm(&m, 0);
                got.logged = untimed(&got.logged);
                let mut want = Outcome {
                    printed: want.printed.clone(),
                    logged: untimed(&want.logged),
                    exit: want.exit,
                    end: want.end.clone(),
                };
                want.logged = untimed(&want.logged);
                assert_eq!(got, want, "-O{level}:\n{src}");
                assert_eq!(got_steps, want_steps, "-O{level} steps:\n{src}");
                n += 1;
            }
        }
        println!("{n} pack and chain runs agree");
    });
}

/// A second `pack`, a `pack` after a burn or inside a block: the checker's
/// E753 stops them before the bytecode, as `cig check` does, and an empty
/// literal root its E755 (a computed one is the run's, above).
#[test]
fn a_misplaced_pack_is_the_checkers_e753() {
    for (src, code) in [
        ("pack { \"./a\" }\npack { \"./b\" }\n", "E753"),
        ("burn {\n  exhale 1\n}\npack { \"./a\" }\n", "E753"),
        ("if true {\n  pack { \"./a\" }\n}\n", "E753"),
        ("pack { \"./a\", \" \" }\n", "E755"),
    ] {
        match bc::compile(src, "t.cig", &pass::level(1)) {
            Err(CompileError::Lower(cigscript::lower::cir::LowerError::Diagnostic(d))) => {
                assert_eq!(d.code, code, "{src}")
            }
            other => panic!("{src}: {:?}", other.map(|_| ())),
        }
    }
}

// ----- the top level's names (`--light`) ----------------------------------------------

/// `cig run --light <name>` after a run: the name looked up in the
/// interpreter's globals, as `cli::run`'s `light_named_chain` does it (the
/// hint that lists the chains left out): a chain is lit, another value is
/// `` `<name>` is a <type>, not a chain ``, a name not there is `E804`.
fn light_after(i: &mut Interp, name: &str) -> Result<(), Diagnostic> {
    use cigscript::diagnostics::Kind as DKind;
    use cigscript::value::Value;
    let value = match i.globals.get(name) {
        Some(v @ Value::Chain(_)) => v,
        Some(other) => {
            return Err(Diagnostic::new(
                DKind::Runtime,
                format!("`{name}` is a {}, not a chain", other.type_name()),
            ))
        }
        None => {
            return Err(Diagnostic::new(
                DKind::Usage,
                format!("no chain named `{name}` in this script"),
            )
            .code("E804"))
        }
    };
    let light = i.globals.get("light").expect("light is in the prelude");
    i.call(&light, vec![value], Default::default()).map(|_| ())
}

/// `src` run, then `--light name`, on the tree-walker (`run_program`) and,
/// lowered at -O0 and -O1, on the VM as a script (`vm::run_script`, which
/// declares the names the top level returns): the run's and the light's
/// output, logs (times out) and endings are the same. Returns the ending.
fn light_agrees(src: &str, name: &str) -> Option<Stop> {
    let program = cigscript::syntax::parse(src).expect("parses");
    let both = |run: &mut Run, r: Result<(), Diagnostic>| {
        let r = r.and_then(|()| light_after(&mut run.interp, name));
        let (mut o, steps) = outcome(r, run);
        o.logged = untimed(&o.logged);
        (o, steps)
    };
    let mut run = interp(0);
    let r = run.interp.run_program(&program);
    let (want, want_steps) = both(&mut run, r);
    for level in [0, 1] {
        let m = compile(src, level);
        let verified = verify_module(&m).unwrap_or_else(|e| panic!("does not verify: {e}"));
        let p = cigscript::vm::load(verified).unwrap_or_else(|e| panic!("{}", e.message));
        let mut run = interp(0);
        let r = cigscript::vm::run_script(&mut run.interp, &p);
        let (got, got_steps) = both(&mut run, r);
        assert_eq!(got, want, "-O{level} --light {name}:\n{src}");
        assert_eq!(
            got_steps, want_steps,
            "-O{level} --light {name} steps:\n{src}"
        );
    }
    want.end
}

/// `--light` answers as the tree-walker's: a chain of the top level (lit,
/// also when declared before `exit()`), a chain inside a block or a pull
/// (`E804`: not a top-level name), a top-level name that is not a chain
/// (a stick, a reassigned and captured roll, a pull, a lambda: "is a
/// <type>, not a chain", with its value at the end), a name not declared
/// (`E804`), a builtin.
#[test]
fn light_answers_like_the_tree_walker() {
    big_stack(|| {
        let src = "pull a() {\n  exhale \"a\"\n  snuff 1\n}\npull b(x) {\n  exhale \"b\", x\n  snuff x + 1\n}\nchain go { a, b }\nstick n = 5\nroll t = \"x\"\nstick f = pack() => t\nt = 2.5\nif true {\n  chain inner { a }\n}\npull mk() {\n  chain made { b }\n  snuff made\n}\nexhale \"ran\", f(), mk().name\n";
        let cases = [
            ("go", None),
            ("n", Some("E500")),
            ("t", Some("E500")),
            ("f", Some("E500")),
            ("a", Some("E500")),
            ("inner", Some("E804")),
            ("made", Some("E804")),
            ("nope", Some("E804")),
            ("len", Some("E500")),
        ];
        let mut n = 0;
        for (name, code) in cases {
            let end = light_agrees(src, name);
            assert_eq!(
                end.as_ref().map(|e| e.0.as_str()),
                code,
                "--light {name}: {end:?}"
            );
            n += 1;
        }
        // A chain declared before `exit()` is still lit.
        let exits = "pull a() {\n  exhale \"a\"\n  snuff 1\n}\nchain go { a }\nexit(2)\n";
        assert_eq!(light_agrees(exits, "go"), None);
        // A chain the top level declares in a burn or a loop is not a
        // top-level name; one it declares after a burn is.
        let blocks = "pull a() {\n  snuff 1\n}\nburn {\n  chain inb { a }\n}\nfor i in 0..1 {\n  chain inl { a }\n}\nchain after { a }\n";
        for (name, code) in [
            ("inb", Some("E804")),
            ("inl", Some("E804")),
            ("after", None),
        ] {
            let end = light_agrees(blocks, name);
            assert_eq!(end.as_ref().map(|e| e.0.as_str()), code, "--light {name}");
        }
        // No top-level name at all: the top level returns null, as before.
        let m = compile("exhale 1\n", 1);
        let p = cigscript::vm::load(verify_module(&m).unwrap()).unwrap();
        let mut run = interp(0);
        let v = cigscript::vm::run(&mut run.interp, &p).unwrap();
        assert_eq!(v.repr(), "null");
        println!("{} --light cases agree, each -O0 and -O1", n + 4);
    });
}

/// `chain.make`'s `C` is 1 for a `chain` statement directly in the top
/// level's block and 0 for every other: in an `if`, a loop, a burn, a
/// pull.
#[test]
fn only_a_top_level_chain_is_declared_by_its_chain_make() {
    use cigscript::cig_bytecode::{instr::walk, Function, Op};
    let src = "pull a() {\n  chain p { a }\n  snuff p\n}\nchain top { a }\nif true {\n  chain i { a }\n}\nfor k in 0..1 {\n  chain l { a }\n}\nburn {\n  chain b { a }\n}\nchain last { top }\n";
    fn cs(f: &Function, out: &mut Vec<u32>) {
        for d in walk(&f.instrs) {
            let d = d.unwrap();
            if d.op == Op::CHAIN_MAKE {
                out.push(d.c);
            }
        }
        for c in &f.children {
            cs(c, out);
        }
    }
    for level in [0, 1] {
        let m = compile(src, level);
        let mut top = Vec::new();
        cs(&m.main, &mut top);
        // fn0 in order: top, i, l, b, last; then the pull's p.
        assert_eq!(top, vec![1, 0, 0, 0, 1, 0], "-O{level}");
    }
}

/// The top level's map costs one `loadk` and one `insertmap` per name, at
/// its one normal end, and no register beyond two: on a script of 600
/// top-level pulls and 600 sticks, the end is `newmap`, 1,200 pairs and
/// `ret`, and the run still answers `--light` like the tree-walker.
#[test]
fn the_top_levels_names_cost_two_instructions_each() {
    big_stack(|| {
        let mut src = String::new();
        for k in 0..600 {
            src.push_str(&format!(
                "pull p{k}() {{\n  snuff {k}\n}}\nstick s{k} = {k}\n"
            ));
        }
        src.push_str("chain c { p0, p599 }\nexhale s599\n");
        let m = compile(&src, 1);
        let main = &m.main;
        let ops: Vec<_> = cigscript::cig_bytecode::instr::walk(&main.instrs)
            .map(|d| d.unwrap().op)
            .collect();
        let end = ops.len() - (2 + 2 * 1201);
        assert_eq!(ops[end], cigscript::cig_bytecode::Op::NEWMAP);
        for pair in ops[end + 1..ops.len() - 1].chunks(2) {
            assert_eq!(pair[0], cigscript::cig_bytecode::Op::LOADK);
            assert_eq!(pair[1], cigscript::cig_bytecode::Op::INSERTMAP);
        }
        assert_eq!(ops[ops.len() - 1], cigscript::cig_bytecode::Op::RET);
        let started = std::time::Instant::now();
        let p = cigscript::vm::load(verify_module(&m).unwrap()).unwrap();
        let mut run = interp(0);
        cigscript::vm::run_script(&mut run.interp, &p).unwrap();
        println!(
            "1,201 top-level names: the top level is {} instructions ({} words, {} constants, {} registers), {} of them the end's map; loaded and run in {:?}",
            ops.len(),
            main.instrs.len(),
            main.constants.len(),
            main.reg_count,
            ops.len() - end,
            started.elapsed()
        );
        for name in ["c", "s599", "p3", "nope"] {
            light_agrees(&src, name);
        }
    });
}

// ----- tight code: operands in place, constants, fused branches, steps ----------

/// Programs for the lowering's tight forms: a slot read with no copy, a
/// value made in its slot, a constant operand (`addk` ... `lek`, a
/// constant left operand swapped only where that is exact), a comparison
/// fused with its branch, and a loop's end folded into the next mark.
/// Each one's errors are the tree-walker's: code, message, line, column.
fn tight_corpus() -> Vec<(&'static str, String)> {
    let mut long_then = String::from("roll a = 1\nif a == 1 {\n");
    for _ in 0..80 {
        long_then.push_str("  exhale a\n");
    }
    long_then.push_str("} else {\n  exhale 0\n}\nif a != 1 {\n  exhale 2\n}\n");
    vec![
        ("addk-overflow", "roll a = 9223372036854775807\nexhale a + 1\n".into()),
        ("subk-type", "roll s = \"x\"\nexhale s - 1\n".into()),
        ("divk-zero", "roll a = 5\nexhale a / 0\n".into()),
        ("modk-zero", "roll a = 5\nexhale a % 0\n".into()),
        ("mulk-string", "roll n = 3\nexhale n * \"ab\"\n".into()),
        ("left-const-dynamic", "roll s = \"a\"\nexhale 1 + s\n".into()),
        ("left-const-eq", "roll s = \"a\"\nexhale 1 == s, \"a\" != s, 2 * 3.5\n".into()),
        ("ltk-type", "roll s = \"a\"\nexhale s < 1\n".into()),
        ("gt-nan", "roll n = math.sqrt(-1.0)\nexhale n > 1.0\n".into()),
        (
            "branch-type",
            "roll s = \"a\"\nif s > 1 {\n  exhale 1\n} else {\n  exhale 2\n}\n".into(),
        ),
        (
            "branch-nan",
            "roll n = math.sqrt(-1.0)\nroll m = 1.0\nif n <= m {\n  exhale 1\n}\n".into(),
        ),
        (
            "every-branch",
            "roll i = 0\nroll seen = []\nwhile i < 6 {\n  if i == 2 {\n    seen.push(\"eq\")\n  }\n  if i != 3 {\n    seen.push(\"ne\")\n  }\n  if i < 1 {\n    seen.push(\"lt\")\n  }\n  if i <= 1 {\n    seen.push(\"le\")\n  }\n  if i > 4 {\n    seen.push(\"gt\")\n  } else {\n    seen.push(\"ngt\")\n  }\n  if i >= 4 {\n    seen.push(\"ge\")\n  }\n  if not (i == 5) {\n    seen.push(\"not\")\n  }\n  i += 1\n}\nroll j = 9\nwhile j >= 7 {\n  j -= 1\n}\nroll k = 0\nwhile k != 3 {\n  k += 1\n}\nroll done = false\nwhile not done {\n  done = true\n}\nexhale seen, j, k\n".into(),
        ),
        (
            "failing-op-keeps-slot",
            "roll a = 9223372036854775807\ntry {\n  a = a + 1\n} ashtray e {\n  exhale e.message\n}\nroll b = 4\ntry {\n  b = b / 0\n} ashtray e {\n  exhale e.code\n}\nroll c = 2\nfor i in 0..3 {\n  try {\n    c *= 4611686018427387904\n  } ashtray e {\n    exhale i, e.message\n  }\n}\nexhale a, b, c\n".into(),
        ),
        (
            "read-then-written",
            "roll a = 1\nroll b = a\na = 5\nexhale a, b\nroll i = 0\nroll prev = 0\nwhile i < 3 {\n  prev = i\n  i += 1\n  exhale prev, i\n}\nroll x = 1\nx = x + x * x\nroll y = x\nx = y - x\nexhale x, y\nroll p = 1\nstick q = p\np = 7\nexhale p, q\nroll u = 2\nstick w = u * u\nu += 1\nexhale w + u, u\n".into(),
        ),
        (
            "loop-end-then-mark",
            "roll k = 0\nif k == 0 {\n  while k < 2 {\n    k += 1\n  }\n} else {\n  k = 9\n}\nroll n = 0\nfor i in 0..3 {\n  roll j = 0\n  while j < i {\n    j += 1\n    n += j\n  }\n  n += 1\n}\nexhale k, n\n".into(),
        ),
        ("long-then", long_then),
    ]
}

/// Every tight-corpus program at -O0 and -O1 runs as the tree-walker
/// does, with every step budget from 1 to one past its total.
#[test]
fn the_tight_forms_run_like_the_interpreter() {
    big_stack(|| {
        for (name, src) in tight_corpus() {
            let (_, total) = tree_walker(&src, 0);
            for level in [0, 1] {
                let m = compile(&src, level);
                for max in std::iter::once(0).chain(1..=total + 1) {
                    let (want, want_steps) = tree_walker(&src, max);
                    let (got, got_steps) = vm(&m, max);
                    assert_eq!(got, want, "{name} -O{level} --max-steps {max}:\n{src}");
                    assert_eq!(got_steps, want_steps, "{name} -O{level} --max-steps {max}");
                }
            }
        }
    });
}

/// The forms are there: a constant operand, a fused branch of each kind,
/// a slot updated in place, and a branch too far for the fused jump's
/// 8-bit offset in the long form.
#[test]
fn the_lowering_writes_the_tight_forms() {
    let text = |src: &str, level| bc::disassemble(&compile(src, level));
    for level in [0, 1] {
        let t = text(
            "roll i = 0\nwhile i < 9 {\n  i += 1\n}\nexhale i % 7\n",
            level,
        );
        assert!(t.contains("ltj       R0,"), "-O{level}:\n{t}");
        assert!(t.contains("addk      R0, R0,"), "-O{level}:\n{t}");
        assert!(t.contains("modk"), "-O{level}:\n{t}");
        let t = text(
            "roll a = 1\nroll b = 2\nif a == b {\n  exhale 1\n}\nif a != b {\n  exhale 2\n}\nif a > b {\n  exhale 3\n}\nif a >= b {\n  exhale 4\n}\n",
            level,
        );
        for op in ["nej", "eqj", "lej", "ltj"] {
            assert!(t.contains(op), "-O{level} {op}:\n{t}");
        }
        assert!(!t.contains("not "), "-O{level}:\n{t}");
        let far = tight_corpus()
            .into_iter()
            .find(|(n, _)| *n == "long-then")
            .map(|(_, s)| s)
            .expect("long-then");
        let t = text(&far, level);
        assert!(!t.contains("nej"), "-O{level}: too far for nej:\n{t}");
        assert!(t.contains("eqj"), "-O{level}: `a != 1` near:\n{t}");
    }
}

// ----- index operations: one instruction, its positions in trailing `nop`s -----

/// Index reads and stores of every kind of object with every kind of
/// index, as plain statements, in a `try`, in a loop, as an argument, as
/// an operand and nested, out of range and with a missing key; plain and
/// compound stores; the operands in `roll`s, so -O1 keeps them as values,
/// and in `stick`s, so -O1 forwards them as constants.
fn index_corpus() -> Vec<(String, String)> {
    let objects = [
        ("list", "[10, 20]"),
        ("string", "\"ab\""),
        ("map", "{a: 1, b: 2}"),
        ("null", "null"),
        ("int", "5"),
        ("bool", "true"),
        ("function", "pack(x) => x"),
    ];
    let indexes = [
        ("int", "0"),
        ("negative", "-1"),
        ("past-end", "5"),
        ("float", "1.5"),
        ("string", "\"a\""),
        ("missing-key", "\"zz\""),
        ("bool", "false"),
        ("null", "null"),
        ("list", "[0]"),
    ];
    let shapes = [
        ("read", "exhale o[k]\n"),
        ("read-spaced", "exhale 1 ==   o[ k ]\n"),
        ("read-arg", "exhale str(o[k]), 1\n"),
        ("read-operand", "roll t = o[k] == 2\nexhale t\n"),
        (
            "read-try",
            "try {\n  exhale o[k]\n} ashtray e {\n  exhale e.message, e.line\n}\nexhale \"after\"\n",
        ),
        (
            "read-loop",
            "for i in 0..2 {\n  exhale i, o[k]\n}\n",
        ),
        ("read-nested", "roll ks = [k]\nexhale o[ks[0]]\n"),
        ("read-nested-outer", "roll w = [o]\nexhale w[0][k]\n"),
        ("store", "o[k] = 7\nexhale o\n"),
        ("store-spaced", "o  [  k ] = 7\nexhale o\n"),
        (
            "store-try",
            "try {\n  o[k] = 7\n} ashtray e {\n  exhale e.message, e.line\n}\nexhale o\n",
        ),
        ("store-loop", "for i in 0..2 {\n  o[k] = i\n}\nexhale o\n"),
        ("store-nested", "roll w = [o]\nw[0][k] = 7\nexhale w\n"),
        ("store-index-of-index", "roll ks = [k]\no[ks[0]] = 7\nexhale o\n"),
        ("compound", "o[k] += 1\nexhale o\n"),
        ("compound-nested", "roll w = [o]\nw[0][k] *= 2\nexhale w\n"),
    ];
    let mut c = Vec::new();
    for (on, o) in objects {
        for (kn, k) in indexes {
            for (sn, s) in shapes {
                for decl in ["roll", "stick"] {
                    c.push((
                        format!("{on}[{kn}] {sn} {decl}"),
                        format!("{decl} o = {o}\n{decl} k = {k}\n{s}"),
                    ));
                }
            }
        }
    }
    c
}

/// Every index program at -O0 and -O1 runs as the tree-walker does, with
/// no budget and with every step budget from 1 to one past its total:
/// the value, or the error's code, message, hint, line and column.
#[test]
fn an_index_error_is_raised_where_the_tree_walker_raises_it() {
    big_stack(|| {
        let (mut runs, mut errors) = (0, 0);
        for (name, src) in index_corpus() {
            let (want, total) = tree_walker(&src, 0);
            errors += usize::from(want.end.is_some());
            for level in [0, 1] {
                let m = compile(&src, level);
                for max in std::iter::once(0).chain(1..=total + 1) {
                    let (want, want_steps) = tree_walker(&src, max);
                    let (got, got_steps) = vm(&m, max);
                    assert_eq!(got, want, "{name} -O{level} --max-steps {max}:\n{src}");
                    assert_eq!(got_steps, want_steps, "{name} -O{level} --max-steps {max}");
                    runs += 1;
                }
            }
        }
        println!(
            "{runs} runs agree ({} programs, {errors} of them stop on an error)",
            index_corpus().len()
        );
    });
}

/// An index read is one `getindex` and a `nop`, a plain store one
/// `setindex` and two `nop`s, with no `type_of`; the `nop`s carry the
/// index's and the object's positions. A compound store keeps its checks.
#[test]
fn an_index_is_one_instruction_and_its_positions() {
    for level in [0, 1] {
        let m = compile(
            "roll xs = [1, 2]\nroll i = 1\nroll v = xs[i]\nxs [ i] = v\nexhale xs\n",
            level,
        );
        let t = bc::disassemble(&m);
        assert!(!t.contains("callb"), "-O{level}:\n{t}");
        assert!(!t.contains("eqk"), "-O{level}:\n{t}");
        let ops: Vec<&str> = t
            .lines()
            .filter_map(|l| l.split_whitespace().nth(1))
            .collect();
        let at = |op: &str| ops.iter().position(|o| *o == op).expect(op);
        let (g, s) = (at("getindex"), at("setindex"));
        assert_eq!(ops[g + 1], "nop", "-O{level}:\n{t}");
        assert_ne!(ops[g + 2], "nop", "-O{level}:\n{t}");
        assert_eq!(&ops[s + 1..s + 3], ["nop", "nop"], "-O{level}:\n{t}");
        // The rows: the read at 3:10, its index at 3:13; the store at
        // 4:1, its index at 4:6, its object at 4:1.
        let lines = t.lines().find(|l| l.starts_with("; fn 0 lines:")).unwrap();
        for row in ["@3:10", "@3:13", "@4:6"] {
            assert!(lines.contains(row), "-O{level} {row}: {lines}");
        }
        let t = bc::disassemble(&compile("roll xs = [1]\nxs[0] += 1\n", level));
        assert!(
            t.contains("callb"),
            "-O{level}: a compound keeps its checks:\n{t}"
        );
    }
}

// ----- call arguments made in their window --------------------------------------

/// Calls whose arguments are made in their place in the window: by name,
/// by value, to builtins and to methods; nested calls in a later
/// argument, a value block between an argument and its call, arguments
/// that fail (in a `try` and not), in loops and in recursion, a closure,
/// a list, a map, an index, a member and an interpolation as arguments,
/// a captured slot and a slot read with no copy beside them.
fn window_corpus() -> Vec<(&'static str, &'static str)> {
    vec![
        ("by-name", "pull f(a, b) {\n  snuff a * 10 + b\n}\nroll x = 3\nexhale f(x + 1, x * 2), f(1, 2)\n"),
        ("nested", "pull f(a, b) {\n  snuff a - b\n}\npull g(a, b, c) {\n  snuff a + b * c\n}\nroll x = 2\nexhale f(x + 1, g(x * 2, f(x - 1, 7), x + 5)), g(f(1, x), x, x + x)\n"),
        ("value-block-between", "pull f(a, b, c) {\n  snuff [a, b, c]\n}\nroll n = null\nroll y = 4\nexhale f(y + 1, n ?? 9, y > 3 and y < 9), f(y - 1, y or 0, n?.k)\n"),
        ("fails-in-try", "pull f(a, b) {\n  snuff a + b\n}\nroll z = 0\nroll s = \"x\"\ntry {\n  exhale f(1 / z, 2)\n} ashtray e {\n  exhale e.message, e.line\n}\ntry {\n  exhale f(2, s - 1)\n} ashtray e {\n  exhale e.message, e.line\n}\nexhale f(3, 4)\n"),
        ("fails-top", "pull f(a, b) {\n  snuff a + b\n}\nroll xs = [1]\nexhale f(5, xs[3])\n"),
        ("fails-second", "roll m = {a: 1}\nexhale str(m.a + 1), str(m.zz)\n"),
        ("loop", "roll t = []\nfor i in 0..5 {\n  t.push(i * 2)\n  t.push(str(i + 1))\n}\nroll j = 0\nwhile j < 3 {\n  t.push(len(\"${j}x\"))\n  j += 1\n}\nexhale t\n"),
        ("recursion", "pull fib(n) {\n  if n < 2 {\n    snuff n\n  }\n  snuff fib(n - 1) + fib(n - 2)\n}\npull fact(n, acc) {\n  if n <= 1 {\n    snuff acc\n  }\n  snuff fact(n - 1, acc * n)\n}\nexhale fib(12), fact(10, 1)\n"),
        ("by-value", "roll f = pack(a, b) => a + b\nroll g = f\nroll x = 1\nexhale g(x + 1, x * 3), [f][0](x, x + 9)\n"),
        ("methods", "roll xs = [3, 1, 2]\nroll s = \"abc\"\nexhale xs.map(pack(x) => x * 2), s.contains(\"b\"), xs.filter(pack(x) => x > 1).len(), s.replace(\"a\", str(xs[0] + 1))\n"),
        ("made-values", "pull f(a, b, c, d) {\n  snuff \"${a} ${b} ${c} ${d}\"\n}\nroll m = {k: 5}\nroll xs = [7, 8]\nexhale f([1, xs[1]], {q: m.k}, \"v${m.k}\", xs[0]), len([m.k, 2]), keys({z: 1})\n"),
        ("captured", "roll c = 1\nroll inc = pack() {\n  c += 1\n  snuff c\n}\npull f(a, b) {\n  snuff a * 100 + b\n}\nexhale f(c + 1, inc()), f(inc(), c * 2), c\n"),
        ("slot-beside", "pull f(a, b, c) {\n  snuff a - b - c\n}\nroll a = 10\nroll b = 3\nexhale f(a, b + 1, a), f(b * 2, a, b)\nb = f(b + 1, b, b - 1)\nexhale b\n"),
        ("arg-then-write", "pull f(a, b) {\n  snuff [a, b]\n}\nroll x = 1\nroll y = x + 1\nx = 5\nexhale f(y + 0, x), f(x + 1, x)\nx = f(x * 2, x)[0]\nexhale x\n"),
        ("builtins", "roll n = 16\nexhale math.sqrt(n * 1.0), str(n % 5), type_of(n / 2), range(n - 14, n - 10)\n"),
        ("error-positions", "pull f(a, b) {\n  snuff a\n}\nroll s = \"x\"\nroll n = 1\nexhale f(n, s * s)\n"),
    ]
}

/// Every window program at -O0 and -O1 runs as the tree-walker does,
/// with no budget and with every step budget from 1 to one past its
/// total.
#[test]
fn arguments_made_in_their_window_run_like_the_interpreter() {
    big_stack(|| {
        let mut runs = 0;
        for (name, src) in window_corpus() {
            let (_, total) = tree_walker(src, 0);
            for level in [0, 1] {
                let m = compile(src, level);
                for max in std::iter::once(0).chain(1..=total + 1) {
                    let (want, want_steps) = tree_walker(src, max);
                    let (got, got_steps) = vm(&m, max);
                    assert_eq!(got, want, "{name} -O{level} --max-steps {max}:\n{src}");
                    assert_eq!(got_steps, want_steps, "{name} -O{level} --max-steps {max}");
                    runs += 1;
                }
            }
        }
        println!("{runs} runs agree");
    });
}

/// An argument read only by its call is made in the window: `fib`'s two
/// calls move nothing, and a slot read with no copy is still moved.
#[test]
fn an_argument_is_made_in_its_window() {
    for level in [0, 1] {
        let t = bc::disassemble(&compile(
            "pull fib(n) {\n  if n < 2 {\n    snuff n\n  }\n  snuff fib(n - 1) + fib(n - 2)\n}\nexhale fib(5)\n",
            level,
        ));
        let pull = t.split("fn 1 fib").nth(1).expect("the pull");
        let pull = pull.split("\n\n").next().unwrap_or(pull);
        assert!(!pull.contains("mov "), "-O{level}:\n{t}");
        assert_eq!(pull.matches("call ").count(), 2, "-O{level}:\n{t}");
        let t = bc::disassemble(&compile("roll a = 1\nexhale str(a)\n", level));
        assert!(
            t.contains("mov "),
            "-O{level}: a slot read with no copy is moved:\n{t}"
        );
    }
}

// ----- a forwarded value made in its slot ---------------------------------------

/// Values -O1 forwards past their `stick` (or that a store also reads)
/// with readers after the store: in the same block, in a branch, in a
/// loop's condition and body, as a `for`'s bounds, in a `try` and its
/// catch, beside a slot written in a nested block, redeclared each turn
/// of a loop, and captured by a closure.
fn slot_corpus() -> Vec<(&'static str, &'static str)> {
    vec![
        ("same-block", "roll x = 3\nstick d = x * 2 + 1\nstick e = d * d\nexhale d, e, d + e\n"),
        ("nested-loops", "stick w = 4\nstick h = 3\nroll n = 0\nfor y in 0..h {\n  for x in 0..w {\n    stick c = 2.0 * x / w - 1.5\n    roll k = 0\n    while k < 3 and c < 1.0 {\n      n += 1\n      k += 1\n    }\n    if c > 0.0 {\n      n += 10\n    }\n  }\n}\nexhale n\n"),
        ("branches", "roll a = 5\nstick b = a - 2\nif b > 1 {\n  exhale b\n} else {\n  exhale -b\n}\nwhile b < 0 {\n  exhale \"never\"\n}\nexhale b\n"),
        ("for-bounds", "roll s = 2\nstick n = 3 * s\nroll t = 0\nfor i in 0..n {\n  t += i + n\n}\nexhale t, n\n"),
        ("try-catch", "roll z = 0\nstick q = z + 7\ntry {\n  exhale q / z\n} ashtray e {\n  exhale e.message, q\n}\nexhale q\n"),
        ("slot-written-nested", "roll a = 1\nroll b = a + 1\nexhale b\nfor i in 0..3 {\n  b = b + i\n}\nexhale b\nroll c = a * 4\nif a == 1 {\n  c = 0\n}\nexhale c\n"),
        ("redeclared-per-turn", "roll out = []\nfor i in 0..4 {\n  stick sq = i * i\n  if sq % 2 == 0 {\n    out.push(sq)\n  }\n  roll j = 0\n  while j < sq {\n    j += 3\n  }\n  out.push(j - sq)\n}\nexhale out\n"),
        ("captured", "roll base = 2\nstick k = base * 5\nroll f = pack(x) => x + k\nexhale f(1), k\n"),
        ("stored-again", "roll a = 2\nroll v = a * 3\nroll w = v\nv = 1\nexhale v, w\nroll p = a + 1\nroll q = p\nfor i in 0..2 {\n  q += p\n}\nexhale p, q\n"),
        ("index-member-into-slot", "roll xs = [[1, 2], [3]]\nroll m = {inner: {k: 5}, n: 1}\nroll x = 0\nfor i in 0..3 {\n  try {\n    x = xs[i]\n  } ashtray e {\n    exhale e.message, e.line, x\n  }\n}\nroll y = 9\ntry {\n  y = m.zz\n} ashtray e {\n  exhale e.message, y\n}\nroll ys = xs\nys = ys[0]\nys = ys[1]\nroll mm = m\nmm = mm.inner\nmm = mm.k\nstick c = xs[1]\nstick d = m.n\nfor j in 0..2 {\n  exhale c, d + j\n}\nexhale x, y, ys, mm\n"),
        ("fails-into-slot", "roll a = 9223372036854775807\nroll b = 0\nfor i in 0..2 {\n  try {\n    b = a + i\n  } ashtray e {\n    exhale e.message, b\n  }\n}\nexhale b\n"),
    ]
}

/// Every slot program at -O0 and -O1 runs as the tree-walker does, with
/// no budget and every step budget from 1 to one past its total.
#[test]
fn a_forwarded_value_made_in_its_slot_runs_like_the_interpreter() {
    big_stack(|| {
        let mut runs = 0;
        for (name, src) in slot_corpus() {
            let (_, total) = tree_walker(src, 0);
            for level in [0, 1] {
                let m = compile(src, level);
                for max in std::iter::once(0).chain(1..=total + 1) {
                    let (want, want_steps) = tree_walker(src, max);
                    let (got, got_steps) = vm(&m, max);
                    assert_eq!(got, want, "{name} -O{level} --max-steps {max}:\n{src}");
                    assert_eq!(got_steps, want_steps, "{name} -O{level} --max-steps {max}");
                    runs += 1;
                }
            }
        }
        println!("{runs} runs agree");
    });
}

/// -O1 writes no more instructions than -O0 where it forwards a `stick`
/// into a nested loop: the value is made in its slot, with no move.
#[test]
fn a_forwarded_stick_is_made_in_its_slot() {
    let src = "stick w = 4\nroll n = 0\nfor x in 0..w {\n  stick c = 2.0 * x / w - 1.5\n  roll k = 0\n  while k < 3 {\n    n += c\n    k += 1\n  }\n}\nexhale n\n";
    let count = |level| {
        let t = bc::disassemble(&compile(src, level));
        t.lines()
            .filter(|l| l.starts_with("  0") || l.starts_with("    0"))
            .count()
    };
    assert!(count(1) <= count(0), "-O1 {} > -O0 {}", count(1), count(0));
    let t = bc::disassemble(&compile(src, 1));
    assert!(
        !t.contains("subk") || {
            let after = t.split("subk").nth(1).unwrap_or("");
            let next = after.lines().nth(1).unwrap_or("");
            !next.contains("mov ")
        },
        "-O1: the value is moved into its slot:\n{t}"
    );
}

/// The slot a value is made in must hold it at every reader. A pass that
/// forwards a value to a reader past a store of its slot (none of ours
/// does: they forward `stick`s) leaves the value read where the slot was
/// written over: then the value keeps its own register. The CIR here is
/// the lowered source with the reader pointed at the first value by hand,
/// so the expected output is the CIR's, not the source's.
#[test]
fn a_value_is_not_made_in_a_slot_written_before_its_reader() {
    /// Point the `exhale`s numbered in `which` (in lowering order) at
    /// `to`.
    fn point(b: &mut Block, which: &[usize], seen: &mut usize, to: ValueId) {
        for i in &mut b.instrs {
            if let Kind::Exhale(vs) = &mut i.kind {
                if which.contains(seen) {
                    vs[0] = to;
                }
                *seen += 1;
            }
            for nested in i.kind.blocks_mut() {
                point(nested, which, seen, to);
            }
        }
    }
    let cases = [
        // Written in a nested block of the reader's own instruction.
        (
            "roll x = 1\nroll a = x + 1\nfor i in 0..3 {\n  exhale a\n  a = i * 10\n}\n",
            0,
            "2\n2\n2\n",
        ),
        // Written between the store and the reader.
        (
            "roll x = 1\nroll a = x + 1\nexhale a\na = 7\nexhale a\n",
            1,
            "2\n2\n",
        ),
    ];
    for (src, which, want) in cases {
        let mut m = cigscript::lower::cir::lower_source(src, "t.cig").expect("lowers");
        let main = &mut m.funcs[0];
        let first = main
            .body
            .instrs
            .iter()
            .filter_map(|i| match i.kind {
                Kind::Decl(_, v) => Some(v),
                _ => None,
            })
            .nth(1)
            .expect("the decl of `a`");
        // That `exhale a` reads the first value.
        point(&mut main.body, &[which], &mut 0, first);
        let module = bc::lower(&m).expect("lowers to bytecode");
        let (got, _) = vm(&module, 0);
        assert_eq!(got.printed, want, "{src}\n{}", bc::disassemble(&module));
    }
}

// ----- constants made once, before their loop -----------------------------------

/// Loops whose constants are made once before the outermost loop around
/// them: a loop entered no times and then twice, left by `break` and
/// entered again, `continue`, a `try` around a loop that fails mid-turn
/// and is entered again, a failing turn caught inside the loop, a call
/// and a recursive call inside the loop, a call inside an argument,
/// nested loops sharing a constant, closures made in the loop, every
/// kind of constant (null, a bool, a small and a large int, a float, a
/// string) as an operand, a comparison, an index and an item, and a
/// function with more than 255 registers live around its loop.
fn hoist_corpus() -> Vec<(&'static str, String)> {
    let mut wide = String::new();
    for k in 0..260 {
        wide.push_str(&format!("roll v{k} = {k}\n"));
    }
    wide.push_str("roll t = []\nroll i = 0\nwhile i < 3 {\n  t.push([i, 2.5, \"w\", null, i == 1])\n  i += 1\n}\nexhale t, v0 + v259\n");
    vec![
        ("zero-then-twice", "pull f(n) {\n  roll out = []\n  roll i = 0\n  while i < n {\n    out.push([i * 2.5, \"c\", null, i == 3, 70000])\n    i += 1\n  }\n  snuff out\n}\nexhale f(0), f(2), f(0), f(4)\n".into()),
        ("break-and-again", "roll t = []\nfor r in 0..3 {\n  roll k = 0\n  while true {\n    if k >= 2 {\n      break\n    }\n    t.push([k, 100000, -70000])\n    k += 1\n  }\n  for j in 0..r {\n    if j == 1 {\n      continue\n    }\n    t.push([j, -7, false])\n  }\n}\nexhale t\n".into()),
        ("fails-mid-turn", "roll xs = [1, 0, 2, \"q\"]\nroll s = 0\nfor x in xs {\n  try {\n    s += 10 / x\n    s += [5, 6][1]\n  } ashtray e {\n    exhale e.message, e.line, 7.5\n  }\n}\nexhale s\n".into()),
        ("loop-in-try", "pull g(n) {\n  roll acc = []\n  try {\n    for i in 0..n {\n      acc.push([i, \"x\" + str(i), 3 - i])\n      roll q = 6 / (2 - i)\n      acc.push(q)\n    }\n  } ashtray e {\n    acc.push(e.message)\n  } finally {\n    acc.push(\"done\")\n  }\n  snuff acc\n}\nfor r in 0..4 {\n  exhale g(r + 1)\n}\n".into()),
        ("recursion-in-loop", "pull walk(d) {\n  roll out = 0\n  roll i = 0\n  while i < 3 {\n    if d > 0 {\n      out += walk(d - 1) * 2\n    }\n    out += [10, 20, 30][i]\n    i += 1\n  }\n  snuff out\n}\nexhale walk(3)\n".into()),
        ("call-in-argument", "pull f(a, b) {\n  snuff a * 10 + b\n}\nroll t = 0\nfor i in 0..4 {\n  t += f(f(i, 2), f(3, i)) - [1, 2][0]\n  t = t % 1000\n  exhale t, 1000\n}\n".into()),
        ("every-kind", "roll n = null\nroll out = []\nroll i = 0\nwhile i < 3 {\n  if n == null {\n    out.push(\"null\")\n  }\n  if (i == 1) == true {\n    out.push(\"one\")\n  }\n  if i * 1.5 > 2.0 {\n    out.push(1.5 * i)\n  }\n  if \"a${i}\" == \"a2\" {\n    out.push(\"two\")\n  }\n  out.push(-40000 + i)\n  out.push([null, true, false, 0.0, -0.0, \"\"])\n  i += 1\n}\nexhale out\n".into()),
        ("nested-shared", "roll t = []\nfor a in 0..3 {\n  t.push([a, 9])\n  for b in 0..2 {\n    t.push([b, 9, \"s\"])\n    roll c = 0\n    while c < 2 {\n      t.push([\"s\", 9])\n      c += 1\n    }\n  }\n}\nexhale t.len(), t[5], t[17]\n".into()),
        ("closures-in-loop", "roll fs = []\nfor i in 0..3 {\n  stick k = i * 2\n  fs.push(pack(x) => x + k + 100)\n}\nroll out = []\nfor f in fs {\n  out.push([f(1), 5])\n}\nexhale out\n".into()),
        ("store-of-constant", "roll a = 0\nroll b = 0\nroll i = 0\nwhile i < 3 {\n  a = 4\n  exhale a, b\n  b = \"z\"\n  a = a + i\n  i += 1\n}\nexhale a, b\n".into()),
        ("past-255", wide),
    ]
}

/// Every program whose loops have constants made before them runs as
/// the tree-walker does, at -O0 and -O1, with no budget and with every
/// step budget from 1 to one past its total.
#[test]
fn constants_made_before_their_loop_run_like_the_interpreter() {
    big_stack(|| {
        let mut runs = 0;
        for (name, src) in hoist_corpus() {
            let (_, total) = tree_walker(&src, 0);
            for level in [0, 1] {
                let m = compile(&src, level);
                for max in std::iter::once(0).chain(1..=total + 1) {
                    let (want, want_steps) = tree_walker(&src, max);
                    let (got, got_steps) = vm(&m, max);
                    assert_eq!(got, want, "{name} -O{level} --max-steps {max}:\n{src}");
                    assert_eq!(got_steps, want_steps, "{name} -O{level} --max-steps {max}");
                    runs += 1;
                }
            }
        }
        println!("{runs} runs agree");
    });
}

/// The pcs from a loop's first `steps k, 0` to its last back-edge.
fn loop_text(t: &str) -> String {
    let lines: Vec<&str> = t.lines().collect();
    let first = lines
        .iter()
        .position(|l| l.contains("steps     1, 0"))
        .expect("a loop");
    let last = lines
        .iter()
        .rposition(|l| l.contains(" loop ") || l.contains(" forloop "))
        .expect("a back-edge");
    lines[first..=last].join("\n")
}

/// A constant an instruction in a loop reads from a register is made
/// once, before the loop, in a register below the loop's call windows;
/// one stored into a slot is made in the slot; past 255 registers it is
/// made where it is read.
#[test]
fn a_loops_constants_are_made_before_it() {
    for level in [0, 1] {
        let src = "roll is = [true, true, true, true]\nroll m = 0\nroll n = 3\nwhile m <= n {\n  is[m] = false\n  exhale str(m), 2.5, \"x\"\n  m += 1\n}\nexhale is\n";
        let t = bc::disassemble(&compile(src, level));
        let body = loop_text(&t);
        assert!(!body.contains("load"), "-O{level}:\n{t}");
        let before = t.split("steps     1, 0").next().unwrap_or("");
        assert!(before.contains("loadbool"), "-O{level}:\n{t}");
        // Below the call's window.
        let reg = |line: &str| -> u32 {
            line.split_whitespace()
                .nth(2)
                .and_then(|r| r.trim_start_matches('R').trim_end_matches(',').parse().ok())
                .expect("a register")
        };
        let made = before
            .lines()
            .filter(|l| l.contains("loadbool") || l.contains("loadk"))
            .map(reg)
            .max()
            .expect("the constants");
        let window = body
            .lines()
            .find(|l| l.contains("callb"))
            .map(reg)
            .expect("a call");
        assert!(made < window, "-O{level}: R{made} >= R{window}:\n{t}");
        // A constant stored into a slot is made there.
        let t = bc::disassemble(&compile(
            "roll a = 0\nroll i = 0\nwhile i < 2 {\n  a = 4\n  i += 1\n}\nexhale a\n",
            level,
        ));
        assert!(loop_text(&t).contains("loadint"), "-O{level}:\n{t}");
    }
    let wide = hoist_corpus()
        .into_iter()
        .find(|(n, _)| *n == "past-255")
        .map(|(_, s)| s)
        .expect("the wide program");
    big_stack(move || {
        let t = bc::disassemble(&compile(&wide, 0));
        assert!(loop_text(&t).contains("loadk"), "{t}");
    });
}

// ----- a bool constant as an operand ----------------------------------------------

/// A bool constant on either side of `==` and `!=`, and on the right of
/// every other operator, including the ones that fail (`+`, `<`) in a
/// `try` and not.
fn bool_k_corpus() -> Vec<(&'static str, &'static str)> {
    vec![
        ("compare", "roll xs = [true, false, null, 1, \"true\"]\nroll out = []\nfor x in xs {\n  roll a = x == true\n  roll b = false != x\n  roll c = true == x\n  out.push([a, b, c, x != false])\n}\nexhale out\n"),
        ("fails", "roll n = 1\ntry {\n  exhale n + true\n} ashtray e {\n  exhale e.message, e.line\n}\ntry {\n  exhale n < false\n} ashtray e {\n  exhale e.message, e.line\n}\nroll s = \"a\"\ntry {\n  exhale s * true\n} ashtray e {\n  exhale e.message, e.line\n}\nexhale n - false\n"),
    ]
}

/// The bool programs run as the tree-walker does at -O0 and -O1 and
/// every step budget; `== true` is one `eqk` with no `loadbool`.
#[test]
fn a_bool_constant_is_an_operand() {
    big_stack(|| {
        for (name, src) in bool_k_corpus() {
            let (_, total) = tree_walker(src, 0);
            for level in [0, 1] {
                let m = compile(src, level);
                for max in std::iter::once(0).chain(1..=total + 1) {
                    let (want, want_steps) = tree_walker(src, max);
                    let (got, got_steps) = vm(&m, max);
                    assert_eq!(got, want, "{name} -O{level} --max-steps {max}:\n{src}");
                    assert_eq!(got_steps, want_steps, "{name} -O{level} --max-steps {max}");
                }
            }
        }
    });
    for level in [0, 1] {
        let t = bc::disassemble(&compile(
            "roll x = len(\"ab\") > 1\nroll a = x == true\nexhale a\n",
            level,
        ));
        assert!(
            t.contains("eqk") && !t.contains("loadbool"),
            "-O{level}:\n{t}"
        );
    }
}

// ----- value blocks and callees made where they are read -------------------------

/// Value blocks (`and`, `or`, `??`, `?.`) whose subject or right side is
/// made in the block's register: each kind with subjects and right sides
/// of every kind, nested on either side, as a call's argument, as a
/// receiver, in a loop's condition, with a right side that fails (in a
/// `try` and not); callees and receivers made in their window: a list,
/// a map, a member, an index, a closure, a value block, a prelude name,
/// one that fails, a call inside an argument, and recursion.
fn short_corpus() -> Vec<(&'static str, &'static str)> {
    vec![
        ("each-kind", "roll n = null\nroll m = {a: {b: 2}, z: null}\nroll xs = [0, 1, \"\", \"s\", null, false, true, 2.5]\nroll out = []\nfor x in xs {\n  out.push([x and [x], x or 7, x ?? \"dflt\", m.a?.b, m.z?.b, n?.q, x == 1 or x == 2.5, not x and 3])\n}\nexhale out\n"),
        ("made-subjects", "roll m = {k: [1, 2], s: \"t\"}\nroll i = 1\nexhale m.k[i] and m.s, -i or 5, [i] ?? 0, \"${i}\" and {q: i}, (i < 2) or i, m.k ?? 1, len(m.s) and i * 3\n"),
        ("nested", "roll a = 0\nroll b = null\nroll c = 3\nexhale a and (b or c), (a or b) ?? c, b ?? (a and c) ?? 9, (c > 1 and (b ?? c) > 2) or a, a or b or c, a and b and c\n"),
        ("loop-condition", "roll k = 0\nroll done = false\nroll t = 0\nwhile k < 50 and not done {\n  t += k\n  if t > 100 {\n    done = true\n  }\n  k += 1\n}\nroll j = 0\nwhile j < 3 or (t > 1000 and j < 9) {\n  j += 1\n}\nexhale k, t, j\n"),
        ("rhs-fails", "roll s = \"x\"\nroll v = 1\nfor i in 0..3 {\n  try {\n    v = (i > 0) and s - i\n  } ashtray e {\n    exhale e.message, e.line, v\n  }\n}\ntry {\n  exhale null ?? s * s\n} ashtray e {\n  exhale e.message\n}\nroll w = 2\ntry {\n  w = w or s - 1\n  w = null ?? [1][4]\n} ashtray e {\n  exhale e.message, e.line, w\n}\nexhale v, w\n"),
        ("as-argument", "pull f(a, b) {\n  snuff [a, b]\n}\nroll n = null\nroll x = 4\nexhale f(n ?? x, x and x + 1), str(x > 3 or n), f(f(n?.k, x or 0), n ?? (x ?? 2))\n"),
        ("callees", "roll fs = [pack(a) => a + 1, pack(a) => a * 2]\nroll m = {f: pack(a) => a - 1, g: {h: pack() => \"h\"}}\nroll n = null\nexhale fs[1](5), m.f(3), m.g.h(), [3, 1, 2].len(), {q: 1}.keys(), (pack(x) => x * 10)(4), (n ?? fs[0])(1), \"abc\".len(), len(\"ab\")\n"),
        ("callee-fails", "roll fs = [pack(a) => a]\nroll m = {}\ntry {\n  exhale fs[3](1)\n} ashtray e {\n  exhale e.message, e.line\n}\ntry {\n  exhale m.nope(1)\n} ashtray e {\n  exhale e.message, e.line\n}\ntry {\n  exhale (null ?? 5)(1)\n} ashtray e {\n  exhale e.message\n}\nexhale fs[0](fs[0](2))\n"),
        ("recursion", "pull fib(n) {\n  snuff (n < 2 and n) or fib(n - 1) + fib(n - 2)\n}\npull walk(t) {\n  snuff t?.l and (walk(t.l) + walk(t.r)) or 1\n}\nexhale fib(10), walk({l: {l: null, r: null}, r: {l: {l: null, r: null}, r: null}})\n"),
        ("receiver-in-loop", "roll out = []\nfor i in 0..4 {\n  out.push([i, i + 1].map(pack(x) => x * i).len() + {a: i}.keys().len())\n  roll t = (i > 1 and [i]) or []\n  out.push(t.len())\n}\nexhale out\n"),
    ]
}

/// Every value-block and callee program at -O0 and -O1 runs as the
/// tree-walker does, with no budget and every step budget from 1 to
/// one past its total.
#[test]
fn values_made_where_their_block_or_call_reads_them_run_like_the_interpreter() {
    big_stack(|| {
        let mut runs = 0;
        for (name, src) in short_corpus() {
            let (_, total) = tree_walker(src, 0);
            for level in [0, 1] {
                let m = compile(src, level);
                for max in std::iter::once(0).chain(1..=total + 1) {
                    let (want, want_steps) = tree_walker(src, max);
                    let (got, got_steps) = vm(&m, max);
                    assert_eq!(got, want, "{name} -O{level} --max-steps {max}:\n{src}");
                    assert_eq!(got_steps, want_steps, "{name} -O{level} --max-steps {max}");
                    runs += 1;
                }
            }
        }
        println!("{runs} runs agree");
    });
}

/// `k < 50 and not done` as a loop's condition makes both sides in the
/// block's register: no `mov`. A callee made by an index is made in its
/// window, with no `mov`.
#[test]
fn a_value_block_makes_its_sides_in_its_register() {
    for level in [0, 1] {
        let t = bc::disassemble(&compile(
            "roll k = 0\nroll done = false\nwhile k < 50 and not done {\n  k += 1\n}\nexhale k\n",
            level,
        ));
        assert!(!loop_text(&t).contains("mov "), "-O{level}:\n{t}");
        let t = bc::disassemble(&compile(
            "roll fs = [pack(a) => a + 1]\nexhale fs[0](2)\n",
            level,
        ));
        let call = t
            .lines()
            .position(|l| l.contains(" call "))
            .expect("a call");
        let before: Vec<&str> = t.lines().take(call).collect();
        assert!(
            before.iter().rev().take(3).all(|l| !l.contains("mov ")),
            "-O{level}:\n{t}"
        );
    }
}

// ----- a call's window sits above every live register (cig-script#230 step 3) ------

/// A value made after a call's window was taken (its first argument is
/// made in it) and read after the call: the window moves to the top, so
/// the callee's frame lies over no live register, and the module keeps
/// `read-before-write`.
#[test]
fn a_value_made_above_a_taken_window_moves_the_call_to_the_top() {
    let src = "pull f(a, b) {\n  snuff len(a) + b\n}\nroll x = 1\nexhale f([x], x * 2)\nexhale 0\n";
    let mut m = cigscript::lower::cir::lower_source(src, "t.cig").expect("lowers");
    let main = &mut m.funcs[0];
    let twice = main
        .body
        .instrs
        .iter()
        .find_map(|i| match i.kind {
            Kind::Binary(BinOp::Mul, ..) => i.result,
            _ => None,
        })
        .expect("x * 2");
    // The second `exhale` reads `x * 2` too: it lives past the call.
    let mut seen = 0;
    for i in &mut main.body.instrs {
        if let Kind::Exhale(vs) = &mut i.kind {
            if seen == 1 {
                vs[0] = twice;
            }
            seen += 1;
        }
    }
    let module = bc::lower(&m).expect("lowers to bytecode");
    verify_module(&module).expect("verifies");
    let dis = bc::disassemble(&module);
    let (got, _) = vm(&module, 0);
    assert_eq!(got.printed, "3\n2\n", "{dis}");
    // The call's window is above the register `x * 2` is in.
    let reg = |op: &str| -> u32 {
        dis.lines()
            .map(|l| l.split_whitespace().collect::<Vec<_>>())
            .find(|t| t.get(1) == Some(&op))
            .and_then(|t| {
                t.get(2)?
                    .trim_start_matches('R')
                    .trim_end_matches(',')
                    .parse()
                    .ok()
            })
            .unwrap_or_else(|| panic!("no {op}: {dis}"))
    };
    assert!(reg("call") > reg("mulk"), "{dis}");
}
