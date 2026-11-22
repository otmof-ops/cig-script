// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The burn programs two suites hold the VM to the tree-walker on: every
//! way into, through and out of a burn. `tests/lower_bc.rs` runs each in
//! one process, at each level and under every step budget;
//! `tests/engines.rs` runs each through `cig run` on either engine and
//! compares what the runs leave, journals included.

/// The burn programs: a name, a template (`P0`, `P1`, .. stand for the
/// files) and the files.
pub fn burn_corpus() -> Vec<(&'static str, &'static str, Vec<&'static str>)> {
    let ab = vec!["a.txt", "b.txt"];
    vec![
        // One that completes, one that errors.
        (
            "completes",
            "burn {\n  fs.write_text(P0, \"a\")\n}\nexhale \"done\"\n",
            ab.clone(),
        ),
        (
            "errors",
            "burn {\n  fs.write_text(P0, \"a\")\n  roll x = 1 / 0\n}\nexhale \"not here\"\n",
            ab.clone(),
        ),
        // Left by break, continue, snuff and exit(), each with an
        // `unburn` that is recorded (or not, for the error).
        (
            "break",
            "for i in 0..4 {\n  burn (s, \"turn\") {\n    s.p = P0\n    fs.append_text(P0, str(i))\n    if i == 1 {\n      break\n    }\n  } unburn {\n    fs.rm(s.p)\n  }\n  exhale i\n}\nexhale \"after\"\n",
            ab.clone(),
        ),
        (
            "continue",
            "roll i = 0\nwhile i < 3 {\n  i += 1\n  burn (s) {\n    s.i = i\n    fs.append_text(P0, str(i))\n    if i == 2 {\n      continue\n    }\n    exhale i\n  } unburn {\n    exhale s.i\n  }\n}\nexhale fs.read_text(P0)\n",
            ab.clone(),
        ),
        (
            "snuff",
            "pull w(p) {\n  burn (s, \"w\") {\n    s.p = p\n    fs.write_text(p, \"a\")\n    snuff 7\n  } unburn {\n    fs.rm(s.p)\n  }\n  snuff 0\n}\nexhale w(P0)\n",
            ab.clone(),
        ),
        (
            "exit",
            "burn (s) {\n  s.p = P0\n  fs.write_text(P0, \"a\")\n  exit(3)\n} unburn {\n  fs.rm(s.p)\n}\nexhale \"not here\"\n",
            ab.clone(),
        ),
        // Nested burns, with an error in the inner one.
        (
            "nested",
            "burn (s, \"outer\") {\n  s.p = P0\n  fs.write_text(P0, \"a\")\n  burn (t, \"inner\") {\n    t.p = P1\n    fs.write_text(P1, \"b\")\n  } unburn {\n    fs.rm(t.p)\n  }\n  fs.append_text(P0, \"c\")\n} unburn {\n  fs.rm(s.p)\n}\n",
            ab.clone(),
        ),
        (
            "nested-error",
            "burn (s, \"outer\") {\n  fs.write_text(P0, \"a\")\n  burn (t) {\n    fs.write_text(P1, \"b\")\n    cough {message: \"inner\", code: 3}\n  } unburn {\n    exhale t\n  }\n} unburn {\n  exhale s\n}\n",
            ab.clone(),
        ),
        // A burn in a try, and a try in a burn, each with an error.
        (
            "burn-in-try",
            "try {\n  burn (s, \"deploy\") {\n    s.p = P0\n    fs.write_text(P0, \"a\")\n    cough \"no\"\n  } unburn {\n    fs.rm(s.p)\n  }\n} ashtray e {\n  exhale \"caught\", e.message\n} finally {\n  exhale \"finally\"\n}\nexhale fs.exists(P0)\n",
            ab.clone(),
        ),
        (
            "try-in-burn",
            "burn (s) {\n  try {\n    fs.write_text(P0, \"a\")\n    roll x = [][1]\n  } ashtray e {\n    exhale \"caught\", e.message\n    fs.append_text(P0, \"b\")\n  } finally {\n    fs.append_text(P0, \"f\")\n  }\n  s.done = true\n} unburn {\n  exhale s.done\n}\n",
            ab.clone(),
        ),
        (
            "try-in-burn-uncaught",
            "burn {\n  try {\n    fs.write_text(P0, \"a\")\n    roll x = 1 / 0\n  } finally {\n    fs.append_text(P0, \"f\")\n  }\n}\n",
            ab.clone(),
        ),
        // Every way out across tries and burns, in each nesting.
        (
            "break-burn-try-finally",
            "for i in 0..3 {\n  try {\n    burn (s) {\n      fs.append_text(P0, str(i))\n      if i == 1 {\n        break\n      }\n    } unburn {\n      exhale \"comp\"\n    }\n  } finally {\n    exhale \"finally\", i\n  }\n}\nexhale fs.read_text(P0)\n",
            ab.clone(),
        ),
        (
            "continue-try-burn",
            "for i in 0..3 {\n  burn (s, \"b\") {\n    try {\n      fs.append_text(P0, str(i))\n      if i == 1 {\n        continue\n      }\n    } finally {\n      exhale \"finally\", i\n    }\n    exhale \"end\", i\n  } unburn {\n    exhale \"comp\"\n  }\n}\nexhale fs.read_text(P0)\n",
            ab.clone(),
        ),
        (
            "snuff-burn-try",
            "pull w(p) {\n  try {\n    burn (s) {\n      fs.write_text(p, \"a\")\n      snuff \"from the burn\"\n    } unburn {\n      exhale s\n    }\n  } finally {\n    fs.append_text(p, \"f\")\n  }\n  snuff \"no\"\n}\nburn {\n  exhale w(P0)\n}\n",
            ab.clone(),
        ),
        (
            "snuff-try-burn",
            "pull w(p) {\n  burn (s) {\n    try {\n      fs.write_text(p, \"a\")\n      snuff \"from the try\"\n    } finally {\n      fs.append_text(p, \"f\")\n    }\n  } unburn {\n    exhale s\n  }\n  snuff \"no\"\n}\nexhale w(P0)\n",
            ab.clone(),
        ),
        (
            "out-of-a-finally",
            "for i in 0..3 {\n  burn (s) {\n    try {\n      fs.append_text(P0, str(i))\n      roll x = 1 / 0\n    } finally {\n      burn {\n        fs.append_text(P1, str(i))\n        if i == 1 {\n          break\n        }\n      }\n    }\n  } unburn {\n    exhale \"comp\"\n  }\n}\nexhale fs.read_text(P0), fs.read_text(P1)\n",
            ab.clone(),
        ),
        (
            "snuff-out-of-a-finally",
            "pull w(p) {\n  try {\n    cough \"x\"\n  } finally {\n    burn (s) {\n      fs.write_text(p, \"a\")\n      snuff 5\n    } unburn {\n      exhale \"comp\"\n    }\n  }\n}\nexhale w(P0)\n",
            ab.clone(),
        ),
        (
            "break-two-loops",
            "for i in 0..2 {\n  burn (s) {\n    for j in 0..3 {\n      burn {\n        fs.append_text(P0, \"${i}${j}\")\n        if j == 1 {\n          break\n        }\n      }\n    }\n    if i == 1 {\n      continue\n    }\n    exhale \"turn\", i\n  } unburn {\n    exhale \"comp\"\n  }\n}\nexhale fs.read_text(P0)\n",
            ab.clone(),
        ),
        (
            "exit-in-try-in-burn",
            "burn (s) {\n  try {\n    fs.write_text(P0, \"a\")\n    exit(2)\n  } finally {\n    exhale \"finally\"\n  }\n} unburn {\n  exhale \"comp\"\n}\n",
            ab.clone(),
        ),
        // An unlit burn, alone and with an `unburn` (which it never
        // records), inside a burn.
        (
            "unlit",
            "burn unlit {\n  fs.write_text(P0, \"a\")\n}\nexhale fs.exists(P0)\n",
            ab.clone(),
        ),
        (
            "unlit-nested",
            "burn {\n  fs.write_text(P0, \"a\")\n  burn unlit (s) {\n    s.n = 1\n    fs.write_text(P1, \"b\")\n  } unburn {\n    exhale s\n  }\n  fs.append_text(P0, \"c\")\n}\n",
            ab.clone(),
        ),
        // A compensation whose state the body fills, captured by a lambda,
        // fresh each turn of a loop.
        (
            "state-filled",
            "burn (s, \"deploy\") {\n  s.path = P0\n  s.n = 2\n  s.list = [1, 2]\n  fs.write_text(P0, \"a\")\n  s.n += 1\n} unburn {\n  fs.rm(s.path)\n}\n",
            ab.clone(),
        ),
        (
            "state-captured",
            "roll fs_ = []\nfor i in 0..2 {\n  burn (s) {\n    s.i = i\n    fs_.push(pack() => s.i)\n    fs.append_text(P0, str(i))\n  } unburn {\n    exhale s.i\n  }\n}\nexhale fs_[0](), fs_[1]()\n",
            ab.clone(),
        ),
        // An effect outside every burn: a pull called inside a burn and
        // outside one (E701 at the op).
        (
            "e701-pull",
            "pull w(p) {\n  exhale \"in\"\n  fs.write_text(p, \"a\")\n  snuff 1\n}\nburn {\n  exhale w(P0)\n}\nexhale w(P1)\n",
            ab.clone(),
        ),
        (
            "e701-not-taken",
            "pull w(p, go) {\n  if go {\n    fs.write_text(p, \"a\")\n  }\n  snuff go\n}\nexhale w(P0, false)\nburn {\n  exhale w(P0, true)\n}\n",
            ab.clone(),
        ),
        (
            "e701-lambda",
            "stick w = pack(p) => fs.write_text(p, \"a\")\nburn {\n  w(P0)\n}\nw(P1)\n",
            ab.clone(),
        ),
        // Ops of every kind the files can show.
        (
            "ops",
            "burn {\n  fs.mkdir(P0)\n  fs.write_text(P1, \"a\")\n  fs.cp(P1, P2)\n  fs.mv(P2, P3)\n  fs.append_text(P3, \"b\")\n  json.save(P4, {k: [1]})\n  fs.rm(P1)\n}\n",
            vec!["d", "a.txt", "b.txt", "c.txt", "j.json"],
        ),
        // A burn that errors where its compensation is recorded, and a
        // burn in a loop with labels.
        (
            "loop-of-burns",
            "for i in 0..3 {\n  burn (s, \"step\") {\n    s.i = i\n    fs.append_text(P0, str(i))\n  } unburn {\n    exhale s.i\n  }\n}\nexhale fs.read_text(P0)\n",
            ab.clone(),
        ),
        // A computed label: from a variable, from a call, from a loop's
        // variable (each turn its own label, one region), read before the
        // state map is made (`burn (s, s)` takes the outer `s`); a label
        // that is not a string, and one that fails to evaluate, stop the
        // burn before it is entered.
        (
            "label-var",
            "stick task = \"deploy\"\nburn (s, task) {\n  s.p = P0\n  fs.write_text(P0, task)\n} unburn {\n  fs.rm(s.p)\n}\nexhale \"done\"\n",
            ab.clone(),
        ),
        (
            "label-call",
            "pull name_of(k) {\n  snuff \"step-\" + str(k)\n}\nburn (s, name_of(2)) {\n  s.k = 2\n  fs.write_text(P0, \"a\")\n} unburn {\n  exhale s.k\n}\n",
            ab.clone(),
        ),
        (
            "label-loop",
            "for t in [\"a\", \"b\", \"c\"] {\n  burn (s, t) {\n    s.t = t\n    fs.append_text(P0, t)\n  } unburn {\n    exhale s.t\n  }\n}\nexhale fs.read_text(P0)\n",
            ab.clone(),
        ),
        (
            "label-while",
            "stick names = [\"x\", \"y\"]\nroll i = 0\nwhile i < names.len() {\n  burn (names[i]) {\n    fs.append_text(P0, names[i])\n  }\n  i += 1\n}\n",
            ab.clone(),
        ),
        (
            "label-captured",
            "roll task = \"one\"\nstick get = pack() => task\ntask = \"two\"\nburn (s, task) {\n  s.v = get()\n  fs.write_text(P0, s.v)\n} unburn {\n  exhale s.v\n}\n",
            ab.clone(),
        ),
        (
            "label-before-state",
            "stick s = \"outer\"\nburn (s, s) {\n  s.n = 1\n  fs.write_text(P0, \"a\")\n} unburn {\n  exhale s.n\n}\nexhale s\n",
            ab.clone(),
        ),
        (
            "label-not-a-string",
            "burn {\n  fs.write_text(P1, \"before\")\n}\nburn (s, 40 + 2) {\n  fs.write_text(P0, \"a\")\n} unburn {\n  exhale \"comp\"\n}\nexhale \"not here\"\n",
            ab.clone(),
        ),
        (
            "label-not-a-string-in-burn",
            "burn (s, \"outer\") {\n  s.p = P0\n  fs.write_text(P0, \"a\")\n  roll n = null\n  burn (t, n) {\n    fs.write_text(P1, \"b\")\n  }\n} unburn {\n  fs.rm(s.p)\n}\n",
            ab.clone(),
        ),
        (
            "label-fails",
            "burn (s, [\"a\"][3]) {\n  fs.write_text(P0, \"a\")\n}\nexhale \"not here\"\n",
            ab.clone(),
        ),
        (
            "label-caught",
            "for v in [\"ok\", 5, \"fine\"] {\n  try {\n    burn (s, v) {\n      fs.append_text(P0, str(v))\n    } unburn {\n      exhale \"comp\"\n    }\n  } ashtray e {\n    exhale \"caught\", e.message\n  }\n}\n",
            ab.clone(),
        ),
        (
            "label-unlit",
            "stick l = \"u\" + \"nlit\"\nburn unlit (u, l) {\n  fs.write_text(P0, \"a\")\n}\n",
            ab.clone(),
        ),
        // Every way out of a burn ends its row with nothing after it.
        (
            "ends-in-break",
            "for i in 0..3 {\n  burn (s, \"t\" + str(i)) {\n    s.i = i\n    fs.append_text(P0, str(i))\n    break\n  } unburn {\n    exhale s.i\n  }\n}\nexhale fs.read_text(P0)\n",
            ab.clone(),
        ),
        (
            "ends-in-continue",
            "for i in 0..3 {\n  burn (s) {\n    s.i = i\n    burn {\n      fs.append_text(P0, str(i))\n      continue\n    }\n  } unburn {\n    exhale s.i\n  }\n}\nexhale fs.read_text(P0)\n",
            ab.clone(),
        ),
        (
            "ends-in-snuff",
            "pull w(p) {\n  burn (s, \"w\") {\n    s.p = p\n    fs.write_text(p, \"a\")\n    snuff 7\n  } unburn {\n    fs.rm(s.p)\n  }\n}\nexhale w(P0)\n",
            ab.clone(),
        ),
        (
            "ends-in-cough",
            "burn (s, \"c\") {\n  s.p = P0\n  fs.write_text(P0, \"a\")\n  cough \"stop\"\n} unburn {\n  fs.rm(s.p)\n}\n",
            ab.clone(),
        ),
        (
            "ends-in-break-two-burns",
            "while true {\n  burn (s) {\n    burn (t) {\n      fs.write_text(P0, \"a\")\n      break\n    } unburn {\n      exhale \"inner\"\n    }\n  } unburn {\n    exhale \"outer\"\n  }\n}\nexhale \"out\"\n",
            ab.clone(),
        ),
        // `pack`: roots, then a write inside and outside them.
        ("pack", "pack { P0 }\nburn {\n  fs.write_text(P0, \"a\")\n}\nexhale \"in\"\n", ab.clone()),
        (
            "pack-outside",
            "pack { P0, P1 }\nburn {\n  fs.write_text(P1, \"b\")\n  fs.write_text(\"/nonexistent-cig-root\" + \"/x\", \"c\")\n}\n",
            ab.clone(),
        ),
        ("pack-built", "pack { str(P0) + \"\" }\nburn {\n  fs.write_text(P0, \"a\")\n}\n", ab.clone()),
    ]
}
