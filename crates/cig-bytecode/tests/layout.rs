// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Layout 2 (`docs/BYTECODE.md`, "File layout"): the head, the directory,
//! the bodies, the string pool and the line region. Each part damaged is
//! refused by the hash; with the hash recomputed, by the parser under a
//! named rule, or by the verifier. Random and mutated bytes never panic.

use cig_bytecode::{
    parse_module, serialize_module, verify_function, verify_module, BurnRegion, Constant, FuncKind,
    Function, HandlerEntry, Instr, LineEntry, Module, Op, ParentCounts, ParseError, Place, Reader,
    RegionLabel, RegionRow, SpanBase, UpvalDesc, UpvalKind, VerifyError, FORMAT_VERSION,
    RESERVED_U32,
};

/// A module with something in every table: three functions under main
/// (one nested), strings shared between names and constants, line rows,
/// a burn region, a handler and upvalues of both kinds.
fn sample() -> Module {
    let mut m = Module::empty("layout.cig");
    let f = &mut m.main;
    f.reg_count = 4;
    f.constants = vec![
        Constant::Str("write".to_string()),
        Constant::Int(-7),
        Constant::Float(2.5),
        Constant::Bool(true),
        Constant::Str("inner".to_string()),
        Constant::Str("é ü".to_string()),
    ];
    f.regions.push(BurnRegion {
        class: 0,
        label: RegionLabel::Const(0),
    });
    f.region_rows.push(RegionRow {
        pc_start: 0,
        pc_end: 3,
        region: 0,
    });
    f.instrs = vec![
        Instr::abx(Op::BURN_ENTER, 0, 0),
        Instr::abc(Op::BURN_OP, 1, 0, 0x10),
        Instr::abc(Op::BURN_LEAVE, 0, 0, 0),
        Instr::abx(Op::CLOSURE, 2, 0),
        Instr::abx(Op::CLOSURE, 3, 1),
        Instr::abc(Op::LOADNULL, 0, 0, 0),
        Instr::abc(Op::RET, 0, 0, 0),
    ];
    f.span_base = SpanBase {
        byte_offset: 10,
        line: 3,
        col: 1,
    };
    f.lines = vec![
        LineEntry {
            pc: 0,
            span_off: 0,
            span_len: 6,
            line_off: 0,
            col: 1,
        },
        LineEntry {
            pc: 1,
            span_off: 300,
            span_len: 70_000,
            line_off: 9,
            col: 5,
        },
        LineEntry {
            pc: 5,
            span_off: 12,
            span_len: 1,
            line_off: 2,
            col: 3,
        },
    ];

    let mut leaf = Function::empty_main();
    leaf.kind = FuncKind::Lambda;
    leaf.name = "inner".to_string();
    leaf.upvalues.push(UpvalDesc {
        kind: UpvalKind::ParentUpvalue,
        index: 0,
    });

    let mut a = Function::empty_main();
    a.kind = FuncKind::Lambda;
    a.name = "write".to_string();
    a.reg_count = 3;
    a.arity = 1;
    a.upvalues.push(UpvalDesc {
        kind: UpvalKind::ParentLocal,
        index: 1,
    });
    a.instrs = vec![Instr::abc(Op::MOV, 1, 0, 0), Instr::abc(Op::RET, 1, 0, 0)];
    a.handlers.push(HandlerEntry {
        pc_start: 0,
        pc_end: 1,
        handler_pc: 1,
        catch_reg: RESERVED_U32,
        finally_pc: RESERVED_U32,
    });
    a.children.push(leaf);

    let mut b = Function::empty_main();
    b.kind = FuncKind::Pull;
    b.name = "b".to_string();
    b.reg_count = 2;
    b.requires_burn = true;

    f.children = vec![a, b];
    verify_module(&m).expect("the sample verifies");
    m
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn set_u32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn add_u32(b: &mut [u8], at: usize, d: u32) {
    let v = u32_at(b, at).wrapping_add(d);
    set_u32(b, at, v);
}

fn rehash(b: &mut [u8]) {
    let h = blake3::hash(&b[40..]);
    b[8..40].copy_from_slice(h.as_bytes());
}

/// Where directory entry `i` starts.
fn entry(i: usize) -> usize {
    72 + 80 * i
}

/// The head's fields: function count, bodies, pool, pool length, line
/// region, line region length.
struct Head {
    n: usize,
    bodies: usize,
    pool: usize,
    pool_len: usize,
    lines: usize,
    lines_len: usize,
}

fn head(b: &[u8]) -> Head {
    Head {
        n: u32_at(b, 40) as usize,
        bodies: u32_at(b, 52) as usize,
        pool: u32_at(b, 56) as usize,
        pool_len: u32_at(b, 60) as usize,
        lines: u32_at(b, 64) as usize,
        lines_len: u32_at(b, 68) as usize,
    }
}

/// `edit` applied to the sample's bytes: refused by the hash as it is,
/// then with the hash recomputed, the parser's (or the verifier's) answer.
fn damaged(edit: impl Fn(&mut Vec<u8>)) -> Result<Module, ParseError> {
    let good = serialize_module(&sample());
    let mut b = good.clone();
    edit(&mut b);
    assert_ne!(b, good, "the edit changed nothing");
    assert_eq!(parse_module(&b).unwrap_err(), ParseError::BadIntegrity);
    rehash(&mut b);
    parse_module(&b)
}

#[test]
fn the_sample_round_trips_and_the_layout_is_as_documented() {
    let m = sample();
    let b = serialize_module(&m);
    assert_eq!(parse_module(&b).unwrap(), m);
    assert_eq!(u32_at(&b, 4) & 0xffff, u32::from(FORMAT_VERSION));
    let h = head(&b);
    assert_eq!(h.n, 4);
    assert_eq!(h.bodies, entry(4));
    assert_eq!(h.lines, h.pool + h.pool_len);
    assert_eq!(h.lines + h.lines_len, b.len());
    // Pre-order: main, write, inner, b; parents and bodies in order.
    let parents: Vec<u32> = (0..4).map(|i| u32_at(&b, entry(i) + 8)).collect();
    assert_eq!(parents, [RESERVED_U32, 0, 1, 0]);
    let mut at = h.bodies;
    for i in 0..4 {
        assert_eq!(u32_at(&b, entry(i) + 20) as usize, at);
        assert_eq!(at % 4, 0, "fn{i}'s body is 4-aligned");
        at += u32_at(&b, entry(i) + 24) as usize;
    }
    assert_eq!(at, h.pool);
    // Code words read where they lie.
    let body = h.bodies;
    let words: Vec<Instr> = (0..7)
        .map(|k| Instr::from_le_bytes(b[body + 4 * k..body + 4 * k + 4].try_into().unwrap()))
        .collect();
    assert_eq!(words, m.main.instrs);
    // One string pool: "write" and "inner" are stored once each.
    let pool = std::str::from_utf8(&b[h.pool..h.pool + h.pool_len]).unwrap();
    assert_eq!(pool.matches("write").count(), 1);
    assert_eq!(pool.matches("inner").count(), 1);
    assert!(pool.starts_with("layout.cig"));
}

#[test]
fn a_version_1_module_is_refused() {
    let mut b = serialize_module(&sample());
    b[4..6].copy_from_slice(&1u16.to_le_bytes());
    assert_eq!(
        parse_module(&b).unwrap_err(),
        ParseError::BadVersion {
            found: 1,
            supported: 2
        }
    );
}

#[test]
fn a_directory_entrys_offsets_and_lengths_are_where_the_layout_puts_them() {
    let e1 = entry(1);
    let err = damaged(|b| add_u32(b, e1 + 20, 4)).unwrap_err();
    assert!(
        matches!(
            err,
            ParseError::BadOffset {
                at: "body offset",
                function: 1,
                ..
            }
        ),
        "{err:?}"
    );
    let err = damaged(|b| add_u32(b, e1 + 24, 4)).unwrap_err();
    assert!(
        matches!(
            err,
            ParseError::BadOffset {
                at: "body length",
                function: 1,
                ..
            }
        ),
        "{err:?}"
    );
    // A count that does not match the body's length.
    let err = damaged(|b| add_u32(b, e1 + 28, 1)).unwrap_err();
    assert!(
        matches!(
            err,
            ParseError::BadOffset {
                at: "body length",
                function: 1,
                ..
            }
        ),
        "{err:?}"
    );
    // A count past every byte of the module.
    let err = damaged(|b| set_u32(b, e1 + 28, u32::MAX)).unwrap_err();
    assert!(
        matches!(
            err,
            ParseError::BadOffset {
                at: "body length",
                function: 1,
                ..
            }
        ),
        "{err:?}"
    );
    let err = damaged(|b| set_u32(b, e1 + 56, 1)).unwrap_err();
    assert!(
        matches!(
            err,
            ParseError::BadOffset {
                at: "line rows offset",
                function: 1,
                ..
            }
        ),
        "{err:?}"
    );
    let e0 = entry(0);
    let err = damaged(|b| add_u32(b, e0 + 60, 1)).unwrap_err();
    assert!(
        matches!(err, ParseError::BadOffset { .. }),
        "the line rows' length: {err:?}"
    );
    // The head's regions.
    for (at, name) in [
        (52, "bodies offset"),
        (56, "pool offset"),
        (64, "line region offset"),
    ] {
        // Three numbers with one rule between them: the refusal names the
        // first that breaks it.
        let err = damaged(|b| add_u32(b, at, 4)).unwrap_err();
        assert!(
            matches!(err, ParseError::BadOffset { .. }),
            "{name}: {err:?}"
        );
    }
    let err = damaged(|b| set_u32(b, 40, 3)).unwrap_err();
    assert!(
        matches!(
            err,
            ParseError::BadOffset {
                at: "bodies offset",
                ..
            }
        ),
        "{err:?}"
    );
    let err = damaged(|b| set_u32(b, 40, 0)).unwrap_err();
    assert!(
        matches!(
            err,
            ParseError::BadCount {
                at: "functions",
                ..
            }
        ),
        "{err:?}"
    );
    let err = damaged(|b| set_u32(b, 40, u32::MAX)).unwrap_err();
    assert!(
        matches!(
            err,
            ParseError::BadCount {
                at: "functions",
                ..
            }
        ),
        "{err:?}"
    );
}

/// The child table of fn0: its offset in the bytes.
fn children_of_main(b: &[u8]) -> usize {
    let e = entry(0);
    let body = u32_at(b, e + 20) as usize;
    let (instrs, consts, handlers, rows) = (
        u32_at(b, e + 28) as usize,
        u32_at(b, e + 32) as usize,
        u32_at(b, e + 36) as usize,
        u32_at(b, e + 40) as usize,
    );
    body + 4 * instrs + 8 * consts + 20 * handlers + 12 * rows
}

#[test]
fn the_directory_is_a_tree_in_pre_order() {
    let c = children_of_main(&serialize_module(&sample()));
    // fn0's children are fn1 and fn3.
    let err = damaged(|b| set_u32(b, c, 9)).unwrap_err();
    assert!(
        matches!(
            err,
            ParseError::BadIndex {
                at: "child",
                index: 9,
                ..
            }
        ),
        "{err:?}"
    );
    let err = damaged(|b| set_u32(b, c, 2)).unwrap_err();
    assert!(
        matches!(
            err,
            ParseError::BadTree {
                function: 0,
                at: "child"
            }
        ),
        "{err:?}"
    );
    let err = damaged(|b| {
        set_u32(b, c, 3);
        set_u32(b, c + 4, 1);
    })
    .unwrap_err();
    assert!(
        matches!(
            err,
            ParseError::BadTree {
                function: 0,
                at: "child order"
            }
        ),
        "{err:?}"
    );
    let err = damaged(|b| set_u32(b, entry(3) + 8, 1)).unwrap_err();
    assert!(
        matches!(err, ParseError::BadTree { at: "child", .. }),
        "fn3 names fn1 as its parent: {err:?}"
    );
    let err = damaged(|b| set_u32(b, entry(2) + 8, 2)).unwrap_err();
    assert!(
        matches!(
            err,
            ParseError::BadTree {
                function: 2,
                at: "parent"
            }
        ),
        "{err:?}"
    );
    let err = damaged(|b| set_u32(b, entry(0) + 8, 0)).unwrap_err();
    assert!(
        matches!(
            err,
            ParseError::BadTree {
                function: 0,
                at: "parent"
            }
        ),
        "{err:?}"
    );
}

#[test]
fn a_tree_out_of_pre_order_is_refused() {
    // main -> [x -> [y], z]; z naming y as its parent is not pre-order of
    // any tree with these child lists.
    let mut m = Module::empty("");
    let mut x = Function::empty_main();
    x.kind = FuncKind::Lambda;
    let mut y = x.clone();
    y.name = "y".to_string();
    x.children.push(y);
    let z = Function::empty_main();
    m.main.children = vec![x, z];
    let mut b = serialize_module(&m);
    // fn3 (z): parent 0 -> 2 (y), and y lists z, main lists only x.
    set_u32(&mut b, entry(3) + 8, 2);
    rehash(&mut b);
    let err = parse_module(&b).unwrap_err();
    assert!(
        matches!(err, ParseError::BadTree { function: 0, .. }),
        "{err:?}"
    );
    // A parent that is not on the path to the function before:
    // main -> [x -> [y], z -> [w]] is fn0 -> [fn1 -> [fn2], fn3 -> [fn4]];
    // fn4 naming fn1 is not pre-order.
    let mut m = Module::empty("");
    let mut x = Function::empty_main();
    x.kind = FuncKind::Lambda;
    x.children.push(Function::empty_main());
    m.main.children = vec![x.clone(), x];
    let mut b = serialize_module(&m);
    set_u32(&mut b, entry(4) + 8, 1);
    rehash(&mut b);
    assert_eq!(
        parse_module(&b).unwrap_err(),
        ParseError::BadTree {
            function: 4,
            at: "pre-order"
        }
    );
}

#[test]
fn the_string_pool_is_checked_by_offset_and_by_its_bytes() {
    let good = serialize_module(&sample());
    let h = head(&good);
    // The source name's offset past the pool.
    let err = damaged(|b| set_u32(b, 44, h.pool_len as u32)).unwrap_err();
    assert!(
        matches!(
            err,
            ParseError::BadIndex {
                at: "source name",
                ..
            }
        ),
        "{err:?}"
    );
    // A function name's length past the pool.
    let err = damaged(|b| set_u32(b, entry(1) + 4, h.pool_len as u32 + 1)).unwrap_err();
    assert!(
        matches!(
            err,
            ParseError::BadIndex {
                at: "function name",
                ..
            }
        ),
        "{err:?}"
    );
    // A string constant's offset past the pool: fn0's constant 0.
    let k0 = u32_at(&good, entry(0) + 20) as usize + 4 * 7;
    let err = damaged(|b| set_u32(b, k0, h.pool_len as u32)).unwrap_err();
    assert!(
        matches!(
            err,
            ParseError::BadIndex {
                at: "string constant",
                ..
            }
        ),
        "{err:?}"
    );
    // A string constant that starts inside a character: "é ü" is
    // constant 5, and its first character is two bytes.
    let k5 = k0 + 8 * 5;
    let err = damaged(|b| add_u32(b, k5, 1)).unwrap_err();
    assert!(
        matches!(
            err,
            ParseError::BadString {
                at: "string constant",
                ..
            }
        ),
        "{err:?}"
    );
    // A pool byte that is not UTF-8.
    let err = damaged(|b| b[h.pool + 2] = 0xFF).unwrap_err();
    assert_eq!(
        err,
        ParseError::BadString {
            at: "string pool",
            index: 2
        }
    );
    // A pool byte changed to another letter is a different string: the
    // module reads, and says so.
    let m = damaged(|b| b[h.pool] = b'L').unwrap();
    assert_eq!(m.source_name, "Layout.cig");
}

#[test]
fn the_line_region_is_checked_row_by_row() {
    let good = serialize_module(&sample());
    let h = head(&good);
    let last = h.lines + h.lines_len - 1;
    // The last byte made to continue past the region.
    let err = damaged(|b| b[last] |= 0x80).unwrap_err();
    assert!(
        matches!(
            err,
            ParseError::Truncated {
                at: "line rows",
                ..
            }
        ),
        "{err:?}"
    );
    // A varint not in its shortest form: the first row's pc (0) as two
    // bytes would shift the row; written over the first two bytes it is
    // refused where it stands.
    let err = damaged(|b| {
        b[h.lines] = 0x80;
        b[h.lines + 1] = 0x00;
    })
    .unwrap_err();
    assert!(
        matches!(err, ParseError::BadVarint { function: 0, .. }),
        "{err:?}"
    );
    // Six bytes of continuation.
    let err = damaged(|b| {
        for k in 0..6 {
            b[h.lines + k] = 0xFF;
        }
    })
    .unwrap_err();
    assert!(
        matches!(err, ParseError::BadVarint { function: 0, .. }),
        "{err:?}"
    );
    // A row count above what the rows' bytes allow.
    let err = damaged(|b| set_u32(b, entry(0) + 64, 1000)).unwrap_err();
    assert!(
        matches!(err, ParseError::BadCount { at: "lines", .. }),
        "{err:?}"
    );
    // A delta that takes a value below zero.
    let err = damaged(|b| b[h.lines] = 0x01).unwrap_err();
    assert!(
        matches!(err, ParseError::BadVarint { function: 0, .. }),
        "{err:?}"
    );
    // A row changed to a pc that is not in order: the parser reads it, the
    // verifier refuses it.
    let m = damaged(|b| b[h.lines] = 0x04).unwrap();
    assert_eq!(m.main.lines[0].pc, 2);
    assert!(verify_module(&m).is_err());
}

#[test]
fn padding_and_reserved_bytes_are_zero() {
    let good = serialize_module(&sample());
    let e0 = entry(0);
    let body = u32_at(&good, e0 + 20) as usize;
    let len = u32_at(&good, e0 + 24) as usize;
    // fn0 has 6 constants: 6 tag bytes, then two bytes of padding.
    let err = damaged(|b| b[body + len - 1] = 1).unwrap_err();
    assert_eq!(
        err,
        ParseError::BadPadding {
            at: "body padding",
            offset: body + len - 1
        }
    );
    // An upvalue's reserved byte: fn1's.
    let e1 = entry(1);
    let b1 = u32_at(&good, e1 + 20) as usize;
    let up = b1
        + 4 * u32_at(&good, e1 + 28) as usize
        + 9 * u32_at(&good, e1 + 32) as usize
        + 20 * u32_at(&good, e1 + 36) as usize
        + 12 * u32_at(&good, e1 + 40) as usize
        + 4 * u32_at(&good, e1 + 52) as usize
        + 8 * u32_at(&good, e1 + 44) as usize
        - u32_at(&good, e1 + 32) as usize;
    let err = damaged(|b| b[up + 1] = 1).unwrap_err();
    assert_eq!(
        err,
        ParseError::BadPadding {
            at: "upvalue padding",
            offset: up + 1
        }
    );
    // A Bool constant other than 0 or 1, and a tag that is not one.
    let k3 = body + 4 * 7 + 8 * 3;
    let err = damaged(|b| b[k3] = 2).unwrap_err();
    assert!(
        matches!(err, ParseError::BadTag { at: "bool", .. }),
        "{err:?}"
    );
    let tags = body + 4 * 7 + 8 * 6 + 12 + 4 * 2 + 8;
    let err = damaged(|b| b[tags] = 9).unwrap_err();
    assert_eq!(
        err,
        ParseError::BadTag {
            at: "constant",
            value: 9
        }
    );
    let err = damaged(|b| b[e0 + 13] = 2).unwrap_err();
    assert!(
        matches!(
            err,
            ParseError::BadTag {
                at: "requires_burn",
                ..
            }
        ),
        "{err:?}"
    );
    let err = damaged(|b| b[e0 + 12] = 7).unwrap_err();
    assert!(
        matches!(
            err,
            ParseError::BadTag {
                at: "function kind",
                ..
            }
        ),
        "{err:?}"
    );
}

#[test]
fn trailing_and_missing_bytes_are_refused() {
    let mut b = serialize_module(&sample());
    b.push(0);
    rehash(&mut b);
    assert!(matches!(
        parse_module(&b).unwrap_err(),
        ParseError::TrailingBytes { .. }
    ));
    let mut b = serialize_module(&sample());
    b.pop();
    rehash(&mut b);
    assert!(matches!(
        parse_module(&b).unwrap_err(),
        ParseError::Truncated { .. }
    ));
}

/// A small generator of numbers: the same sequence from the same seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Random bytes, and the sample with random changes, never make the
/// parser panic; a changed module fails the hash, and with the hash
/// recomputed either fails to parse or parses to a module the verifier
/// judges (without panicking).
#[test]
fn random_and_mutated_bytes_never_panic() {
    let good = serialize_module(&sample());
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..2_000 {
        let n = rng.below(400);
        let mut b: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
        let _ = parse_module(&b);
        if b.len() >= 40 {
            b[..8].copy_from_slice(&good[..8]);
            rehash(&mut b);
            let _ = parse_module(&b);
        }
    }
    let (mut read, mut judged) = (0, 0);
    for round in 0..20_000 {
        let mut b = good.clone();
        let edits = 1 + rng.below(4);
        for _ in 0..edits {
            let at = 40 + rng.below(b.len() - 40);
            match rng.below(4) {
                0 => b[at] ^= 1 << rng.below(8),
                1 => b[at] = rng.next() as u8,
                // A plausible number where a u32 field may be.
                2 if at + 4 <= b.len() => {
                    let v = u32_at(&b, at & !3);
                    let w = [0, 1, v.wrapping_add(1), v.wrapping_sub(1), u32::MAX][rng.below(5)];
                    set_u32(&mut b, at & !3, w);
                }
                _ => {
                    if rng.below(2) == 0 {
                        b.insert(at, rng.next() as u8);
                    } else {
                        b.remove(at);
                    }
                }
            }
        }
        if b == good {
            continue;
        }
        assert!(parse_module(&b).is_err(), "round {round}: read unhashed");
        rehash(&mut b);
        if let Ok(m) = parse_module(&b) {
            read += 1;
            if verify_module(&m).is_ok() {
                judged += 1;
            }
        }
    }
    println!("mutated: {read} of 20000 read after rehashing, {judged} of those verified");
}

/// A parsed module's functions in pre-order, each without its children
/// (as [`Reader::function`] gives them).
fn preorder(m: &Module) -> Vec<Function> {
    let mut out = Vec::new();
    let mut stack = vec![&m.main];
    while let Some(f) = stack.pop() {
        let mut g = f.clone();
        g.children.clear();
        out.push(g);
        stack.extend(f.children.iter().rev());
    }
    out
}

/// The module read one function at a time: opened, then each function's
/// head and body, the last first (the order `parse_module` builds them).
fn lazily(b: &[u8]) -> Result<Vec<Function>, ParseError> {
    let r = Reader::open(b.to_vec())?;
    let n = r.len() as u32;
    let mut out = vec![None; n as usize];
    for i in (0..n).rev() {
        r.head(i)?;
        out[i as usize] = Some(r.function(i)?);
    }
    Ok(out.into_iter().flatten().collect())
}

/// Every function verified at its own place, in pre-order: the first error.
fn verified_lazily(b: &[u8]) -> Result<(), VerifyError> {
    let r = Reader::open(b.to_vec()).expect("opens");
    for i in 0..r.len() as u32 {
        let f = r.function(i).expect("reads");
        verify_function(&f, r.place(i).expect("a place"))?;
    }
    Ok(())
}

#[test]
fn a_reader_reads_each_function_as_the_parser_does() {
    let m = sample();
    let b = serialize_module(&m);
    let r = Reader::open(b.clone()).unwrap();
    assert_eq!(r.len(), 4);
    assert_eq!(r.source_name(), "layout.cig");
    assert_eq!(lazily(&b).unwrap(), preorder(&m));
    // main, write (under main), inner (under write), b (under main).
    assert_eq!(r.children(0), [1, 3]);
    assert_eq!(r.children(1), [2]);
    assert!(r.children(2).is_empty() && r.children(3).is_empty() && r.children(9).is_empty());
    assert_eq!(
        r.place(2),
        Some(Place {
            index: 2,
            depth: 2,
            children: 0,
            parent: Some(ParentCounts {
                reg_count: 3,
                upvalues: 1
            }),
        })
    );
    assert_eq!(r.place(0).unwrap().parent, None);
    assert_eq!(r.place(4), None);
    let h = r.head(1).unwrap();
    assert_eq!(
        (
            h.name.as_str(),
            h.kind,
            h.arity,
            h.reg_count,
            h.requires_burn
        ),
        ("write", FuncKind::Lambda, 1, 3, false)
    );
    assert_eq!(h.upvalues, m.main.children[0].upvalues);
    assert!(r.head(3).unwrap().requires_burn);
    assert!(matches!(r.function(4), Err(ParseError::BadIndex { .. })));
    // Each function verifies at its place, and hands back its decoded code.
    for i in 0..4 {
        let f = r.function(i).unwrap();
        let v = verify_function(&f, r.place(i).unwrap()).unwrap();
        assert_eq!(v.index(), i);
        assert_eq!(v.code().len(), f.instrs.len());
        assert!(v.code().iter().flatten().count() > 0);
    }
}

#[test]
fn a_function_refused_at_its_place_is_refused_as_the_module_verifier_refuses_it() {
    // One rule broken in one function at a time: fn2 (inner) moves a register
    // past its frame; fn1's upvalue names a register past main's; fn3 has
    // no terminator; fn2 makes a closure of a child it does not have.
    let edits: [fn(&mut Module); 4] = [
        |m| m.main.children[0].children[0].instrs[0] = Instr::abc(Op::MOV, 9, 0, 0),
        |m| m.main.children[0].upvalues[0].index = 4,
        |m| m.main.children[1].instrs = vec![Instr::abc(Op::LOADNULL, 0, 0, 0)],
        |m| m.main.children[0].children[0].instrs[0] = Instr::abx(Op::CLOSURE, 0, 0),
    ];
    for (k, edit) in edits.iter().enumerate() {
        let mut m = sample();
        edit(&mut m);
        let want = verify_module(&m).expect_err("refused");
        let b = serialize_module(&m);
        assert_eq!(verified_lazily(&b), Err(want.clone()), "edit {k}");
        // The other functions verify on their own.
        let r = Reader::open(b).unwrap();
        let bad = (0..4)
            .filter(|&i| verify_function(&r.function(i).unwrap(), r.place(i).unwrap()).is_err())
            .count();
        assert_eq!(bad, 1, "edit {k}: {want}");
    }
}

/// Mutated modules, the hash recomputed: reading one function at a time
/// gives exactly what `parse_module` gives (the same functions, or the same
/// first error), and verifying each function at its place gives exactly
/// what `verify_module` gives. So every check the eager reader and the
/// eager verifier make is made by the lazy ones, each before the bytes it
/// guards are used.
#[test]
fn reading_and_verifying_one_function_at_a_time_refuses_what_the_whole_module_refuses() {
    let good = serialize_module(&sample());
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let (mut read, mut verified) = (0, 0);
    for _ in 0..20_000 {
        let mut b = good.clone();
        for _ in 0..1 + rng.below(3) {
            let at = 40 + rng.below(b.len() - 40);
            match rng.below(3) {
                0 => b[at] ^= 1 << rng.below(8),
                1 => b[at] = rng.next() as u8,
                _ if at + 4 <= b.len() => {
                    let v = u32_at(&b, at & !3);
                    let w = [0, 1, v.wrapping_add(1), v.wrapping_sub(1), u32::MAX][rng.below(5)];
                    set_u32(&mut b, at & !3, w);
                }
                _ => b[at] ^= 0x80,
            }
        }
        rehash(&mut b);
        let eager = parse_module(&b);
        let lazy = lazily(&b);
        match (&eager, &lazy) {
            (Ok(m), Ok(fs)) => {
                read += 1;
                // Debug text, so a NaN constant compares equal to itself.
                assert_eq!(format!("{:?}", preorder(m)), format!("{fs:?}"));
                let want = verify_module(m).map(|_| ());
                if want.is_ok() {
                    verified += 1;
                }
                assert_eq!(verified_lazily(&b), want);
            }
            (Err(a), Err(b)) => assert_eq!(a, b),
            _ => panic!("eager {:?}, lazy {:?}", eager.map(|_| ()), lazy.map(|_| ())),
        }
    }
    println!("mutated: {read} of 20000 read, {verified} verified");
    assert!(read > 0 && verified > 0);
}

#[test]
fn the_disassembler_describes_the_layout() {
    let b = serialize_module(&sample());
    let text = cig_bytecode::disasm::describe_layout(&b).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 5, "{text}");
    assert!(lines[0].starts_with("; layout 2: "), "{text}");
    assert!(lines[0].contains("(4 functions)"), "{text}");
    assert!(lines[1].starts_with("fn0: parent -, body "), "{text}");
    assert!(lines[3].starts_with("fn2: parent fn1, "), "{text}");
    assert!(lines[4].starts_with("fn3: parent fn0, "), "{text}");
    let mut bad = b.clone();
    bad[60] ^= 1;
    assert!(cig_bytecode::disasm::describe_layout(&bad).is_err());
}
