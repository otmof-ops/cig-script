// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The format's tests: the layout pinned byte for byte, every refusal on
//! the writer's side and on the reader's, and the two properties the
//! format promises (what is written is read back; what is read is
//! written back the same), on sections and bodies made at random.

use super::*;

fn module() -> [u8; 32] {
    core::array::from_fn(|i| 0xA0 + i as u8)
}

fn site(kind: Exit, op: Op, left: Ty, right: Ty) -> Site {
    Site {
        start: 10,
        end: 20,
        line: 3,
        col: 5,
        kind,
        op,
        left,
        right,
        in_loop: false,
    }
}

/// Four entries (two rows of one function among them) and a site of
/// every kind, over 17 bytes of code: 7 bytes of padding.
fn sample() -> Section {
    Section {
        arch: Arch::X86_64,
        module: module(),
        code: (0..17u8).collect(),
        entries: vec![
            Entry {
                func: 1,
                offset: 0,
                args: vec![Ty::Int, Ty::Int],
                ret: Ty::Int,
            },
            Entry {
                func: 1,
                offset: 5,
                args: vec![Ty::Float, Ty::Float],
                ret: Ty::Float,
            },
            Entry {
                func: 2,
                offset: 11,
                args: vec![Ty::Bool],
                ret: Ty::Bool,
            },
            Entry {
                func: 7,
                offset: 16,
                args: vec![],
                ret: Ty::Null,
            },
        ],
        sites: vec![
            site(Exit::Operator, op::ADD, Ty::Int, Ty::Int),
            Site {
                in_loop: true,
                ..site(Exit::Operator, op::LT, Ty::Float, Ty::Int)
            },
            site(Exit::Negate, 0, Ty::Int, Ty::Null),
            site(Exit::Range, 0, Ty::Int, Ty::Int),
            Site {
                in_loop: true,
                ..site(Exit::Budget, 0, Ty::Null, Ty::Null)
            },
            site(Exit::Depth, 0, Ty::Null, Ty::Null),
        ],
    }
}

// Where the sample's parts lie.
const ENTRIES_AT: usize = 24;
const SITES_AT: usize = ENTRIES_AT + 4 * ENTRY_LEN;
const FOOTER_AT: usize = SITES_AT + 6 * SITE_LEN;

fn entry_at(i: usize) -> usize {
    ENTRIES_AT + i * ENTRY_LEN
}

fn site_at(i: usize) -> usize {
    SITES_AT + i * SITE_LEN
}

fn body() -> Vec<u8> {
    sample().to_bytes().expect("the sample is a section")
}

/// The reader refuses the sample's body after `change`, with `want`.
fn reader_refuses(change: impl FnOnce(&mut Vec<u8>), want: FormatError) {
    let mut b = body();
    change(&mut b);
    assert_eq!(Tables::parse(&b), Err(want.clone()));
    assert_eq!(Section::parse(&b), Err(want));
}

/// The writer refuses the sample after `change`, with `want`.
fn writer_refuses(change: impl FnOnce(&mut Section), want: FormatError) {
    let mut s = sample();
    change(&mut s);
    assert_eq!(s.to_bytes(), Err(want));
}

fn put_u32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

// ----- the layout ------------------------------------------------------

/// A section written out by hand from the layout's description: each
/// field at its offset, in its width, little-endian. A writer and a
/// reader that shared a wrong offset would pass every round trip and
/// fail here.
#[test]
fn the_layout_is_the_documents_byte_for_byte() {
    let section = Section {
        arch: Arch::X86_64,
        module: module(),
        code: vec![0x90, 0xC3, 0xC3],
        entries: vec![Entry {
            func: 0x0102,
            offset: 2,
            args: vec![Ty::Int, Ty::Float],
            ret: Ty::Bool,
        }],
        sites: vec![Site {
            start: 0x11,
            end: 0x22,
            line: 0x33,
            col: 0x44,
            kind: Exit::Operator,
            op: op::DIV,
            left: Ty::Float,
            right: Ty::Int,
            in_loop: true,
        }],
    };
    #[rustfmt::skip]
    let golden: Vec<u8> = [
        // The code, then zero padding to a multiple of 8.
        &[0x90, 0xC3, 0xC3][..],
        &[0, 0, 0, 0, 0],
        // The entry: function index, offset, argument count, result
        // type, two zero bytes, eight argument types, four zero bytes.
        &[0x02, 0x01, 0, 0],
        &[2, 0, 0, 0],
        &[2],
        &[1],
        &[0, 0],
        &[2, 3, 0, 0, 0, 0, 0, 0],
        &[0, 0, 0, 0],
        // The site: start, end, line, column, kind, operator, the two
        // operand types, flags, three zero bytes.
        &[0x11, 0, 0, 0],
        &[0x22, 0, 0, 0],
        &[0x33, 0, 0, 0],
        &[0x44, 0, 0, 0],
        &[1],
        &[3],
        &[3],
        &[2],
        &[1],
        &[0, 0, 0],
        // The footer: magic, version, architecture, interface version,
        // code length, entry count, site count, module hash, 16 zeros.
        b"CIGNATV1",
        &[1, 0, 0, 0],
        &[1, 0, 0, 0],
        &[1, 0, 0, 0],
        &[3, 0, 0, 0],
        &[1, 0, 0, 0],
        &[1, 0, 0, 0],
        &module()[..],
        &[0; 16],
    ]
    .concat();
    assert_eq!(golden.len(), 3 + 5 + ENTRY_LEN + SITE_LEN + FOOTER_LEN);
    assert_eq!(section.to_bytes().as_deref(), Ok(&golden[..]));
    assert_eq!(Section::parse(&golden), Ok(section));
}

#[test]
fn the_constants_are_the_layouts() {
    assert_eq!(&MAGIC, b"CIGNATV1");
    assert_eq!((VERSION, INTERFACE), (1, 1));
    assert_eq!((ENTRY_LEN, SITE_LEN, FOOTER_LEN), (24, 24, 80));
    assert_eq!(
        (ALIGN, MAX_ARGS, MAX_ROWS, MAX_FRAME),
        (65_536, 8, 16, 4096)
    );
    assert_eq!(
        [Ty::Null, Ty::Bool, Ty::Int, Ty::Float].map(Ty::as_u8),
        [0, 1, 2, 3]
    );
    assert_eq!([Arch::X86_64, Arch::Aarch64].map(Arch::as_u32), [1, 2]);
    assert_eq!(
        [
            Exit::Operator,
            Exit::Negate,
            Exit::Range,
            Exit::Budget,
            Exit::Depth
        ]
        .map(Exit::as_u8),
        [1, 2, 3, 4, 5]
    );
    assert_eq!(
        [
            op::ADD,
            op::SUB,
            op::MUL,
            op::DIV,
            op::REM,
            op::LT,
            op::LE,
            op::GT,
            op::GE
        ],
        [0, 1, 2, 3, 4, 7, 8, 9, 10]
    );
    for v in 0..=255u8 {
        assert_eq!(Ty::from_u8(v).map(Ty::as_u8), (v < 4).then_some(v));
        assert_eq!(
            Exit::from_u8(v).map(Exit::as_u8),
            (1..=5).contains(&v).then_some(v)
        );
    }
    assert_eq!(Arch::from_u32(0), None);
    assert_eq!(Arch::from_u32(3), None);
}

// ----- what is written is read back ------------------------------------

#[test]
fn what_is_written_is_read_back() {
    let s = sample();
    let b = body();
    assert_eq!(b.len(), FOOTER_AT + FOOTER_LEN);
    assert_eq!(Section::parse(&b).as_ref(), Ok(&s));
    let t = Tables::parse(&b).expect("tables");
    assert_eq!(
        (t.arch, t.module, t.code_len, &t.entries, &t.sites),
        (s.arch, s.module, 17, &s.entries, &s.sites)
    );
}

#[test]
fn a_section_with_nothing_in_it_is_a_footer() {
    let s = Section {
        arch: Arch::Aarch64,
        module: module(),
        code: vec![],
        entries: vec![],
        sites: vec![],
    };
    let b = s.to_bytes().expect("written");
    assert_eq!(b.len(), FOOTER_LEN);
    assert_eq!(Section::parse(&b), Ok(s));
}

#[test]
fn code_of_every_length_gets_its_padding() {
    for n in 0..=16usize {
        let s = Section {
            code: vec![0xCC; n],
            entries: vec![],
            sites: vec![],
            ..sample()
        };
        let b = s.to_bytes().expect("written");
        assert_eq!((b.len() - FOOTER_LEN) % 8, 0, "{n} bytes of code");
        assert!(b.len() - FOOTER_LEN - n < 8, "{n} bytes of code");
        assert_eq!(Section::parse(&b), Ok(s));
    }
}

// ----- the reader's refusals -------------------------------------------

#[test]
fn the_reader_refuses_a_body_shorter_than_a_footer() {
    for len in [0, 1, 79] {
        let b = vec![0u8; len];
        assert_eq!(Tables::parse(&b), Err(FormatError::TooShort { len }));
    }
}

#[test]
fn the_reader_refuses_a_wrong_footer() {
    reader_refuses(
        |b| b[FOOTER_AT] = b'c',
        FormatError::BadMagic {
            found: *b"cIGNATV1",
        },
    );
    reader_refuses(
        |b| put_u32(b, FOOTER_AT + 8, 2),
        FormatError::BadVersion { found: 2 },
    );
    for found in [0, 3, u32::MAX] {
        reader_refuses(
            |b| put_u32(b, FOOTER_AT + 12, found),
            FormatError::BadArch { found },
        );
    }
    reader_refuses(
        |b| put_u32(b, FOOTER_AT + 16, 2),
        FormatError::BadInterface { found: 2 },
    );
    for offset in 64..FOOTER_LEN {
        reader_refuses(
            |b| b[FOOTER_AT + offset] = 1,
            FormatError::NonzeroReserved {
                region: "footer",
                at: 0,
                offset,
            },
        );
    }
}

#[test]
fn the_reader_refuses_counts_that_do_not_add_up_to_the_body() {
    let len = (FOOTER_AT + FOOTER_LEN) as u64;
    // Eight more bytes of code than there are: the same padding.
    reader_refuses(
        |b| put_u32(b, FOOTER_AT + 20, 25),
        FormatError::BadLength {
            body: len,
            declared: len + 8,
        },
    );
    reader_refuses(
        |b| put_u32(b, FOOTER_AT + 24, 5),
        FormatError::BadLength {
            body: len,
            declared: len + 24,
        },
    );
    reader_refuses(
        |b| put_u32(b, FOOTER_AT + 28, 0),
        FormatError::BadLength {
            body: len,
            declared: len - 6 * 24,
        },
    );
    // The largest counts there are: no overflow, no allocation.
    reader_refuses(
        |b| {
            put_u32(b, FOOTER_AT + 20, u32::MAX);
            put_u32(b, FOOTER_AT + 24, u32::MAX);
            put_u32(b, FOOTER_AT + 28, u32::MAX);
        },
        FormatError::BadLength {
            body: len,
            declared: u64::from(u32::MAX) + 1 + 2 * 24 * u64::from(u32::MAX) + 80,
        },
    );
    // A byte after the footer: what ends the body is no footer.
    let mut b = body();
    b.push(0);
    assert!(matches!(
        Tables::parse(&b),
        Err(FormatError::BadMagic { .. })
    ));
}

#[test]
fn the_reader_refuses_padding_that_is_not_zero() {
    for offset in 17..24 {
        reader_refuses(|b| b[offset] = 0xFF, FormatError::NonzeroPadding { offset });
    }
}

#[test]
fn the_reader_refuses_an_entry_out_of_its_ranges() {
    for at in 0..4 {
        for offset in [10, 11, 20, 21, 22, 23] {
            reader_refuses(
                |b| b[entry_at(at) + offset] = 1,
                FormatError::NonzeroReserved {
                    region: "entry",
                    at,
                    offset,
                },
            );
        }
    }
    reader_refuses(
        |b| b[entry_at(2) + 8] = 9,
        FormatError::TooManyArgs { at: 2, count: 9 },
    );
    reader_refuses(
        |b| b[entry_at(2) + 8] = 255,
        FormatError::TooManyArgs { at: 2, count: 255 },
    );
    reader_refuses(
        |b| b[entry_at(0) + 9] = 4,
        FormatError::BadType {
            what: "an entry's result",
            at: 0,
            value: 4,
        },
    );
    reader_refuses(
        |b| b[entry_at(1) + 13] = 200,
        FormatError::BadType {
            what: "an entry's argument",
            at: 1,
            value: 200,
        },
    );
    // Entry 2 has one argument: slots 1 to 7 are unused. Entry 3 has
    // none: slot 0 is.
    for slot in 1..MAX_ARGS {
        reader_refuses(
            |b| b[entry_at(2) + 12 + slot] = 2,
            FormatError::NonzeroUnusedArg { at: 2, slot },
        );
    }
    reader_refuses(
        |b| b[entry_at(3) + 12] = 1,
        FormatError::NonzeroUnusedArg { at: 3, slot: 0 },
    );
}

#[test]
fn the_reader_refuses_a_site_out_of_its_ranges() {
    for at in 0..6 {
        for offset in [21, 22, 23] {
            reader_refuses(
                |b| b[site_at(at) + offset] = 1,
                FormatError::NonzeroReserved {
                    region: "site",
                    at,
                    offset,
                },
            );
        }
    }
    for value in [0, 6, 255] {
        reader_refuses(
            |b| b[site_at(3) + 16] = value,
            FormatError::BadExit { at: 3, value },
        );
    }
    reader_refuses(
        |b| b[site_at(0) + 18] = 4,
        FormatError::BadType {
            what: "a site's left operand",
            at: 0,
            value: 4,
        },
    );
    reader_refuses(
        |b| b[site_at(0) + 19] = 9,
        FormatError::BadType {
            what: "a site's right operand",
            at: 0,
            value: 9,
        },
    );
    for value in [2, 3, 0x80, 0xFF] {
        reader_refuses(
            |b| b[site_at(5) + 20] = value,
            FormatError::BadFlags { at: 5, value },
        );
    }
}

// ----- the rules both sides apply --------------------------------------

/// A rule of the tables: the writer refuses `change`d sample, and the
/// reader refuses the same section's bytes (made by `bytes`, a change to
/// the sample's body), both with `want`.
fn both_refuse(
    change: impl FnOnce(&mut Section),
    bytes: impl FnOnce(&mut Vec<u8>),
    want: FormatError,
) {
    writer_refuses(change, want.clone());
    reader_refuses(bytes, want);
}

#[test]
fn entries_lie_inside_the_code() {
    for offset in [17, 18, u32::MAX] {
        both_refuse(
            |s| s.entries[3].offset = offset,
            |b| put_u32(b, entry_at(3) + 4, offset),
            FormatError::EntryOutsideCode {
                at: 3,
                offset,
                code_len: 17,
            },
        );
    }
}

#[test]
fn entries_are_sorted_by_function_then_by_argument_types() {
    // Function 2 before function 1's second row.
    both_refuse(
        |s| s.entries.swap(1, 2),
        |b| {
            let (one, two) = (entry_at(1), entry_at(2));
            let kept: Vec<u8> = b[one..one + ENTRY_LEN].to_vec();
            b.copy_within(two..two + ENTRY_LEN, one);
            b[two..two + ENTRY_LEN].copy_from_slice(&kept);
        },
        FormatError::EntriesOutOfOrder { at: 2 },
    );
    // Function 1's two rows the other way round: (float, float) before
    // (int, int).
    both_refuse(
        |s| s.entries.swap(0, 1),
        |b| {
            let (zero, one) = (entry_at(0), entry_at(1));
            let kept: Vec<u8> = b[zero..zero + ENTRY_LEN].to_vec();
            b.copy_within(one..one + ENTRY_LEN, zero);
            b[one..one + ENTRY_LEN].copy_from_slice(&kept);
        },
        FormatError::EntriesOutOfOrder { at: 1 },
    );
    // The second type decides when the first is the same.
    both_refuse(
        |s| {
            s.entries[0].args = vec![Ty::Int, Ty::Float];
            s.entries[1].args = vec![Ty::Int, Ty::Int];
        },
        |b| {
            b[entry_at(0) + 13] = 3;
            b[entry_at(1) + 12] = 2;
            b[entry_at(1) + 13] = 2;
        },
        FormatError::EntriesOutOfOrder { at: 1 },
    );
}

#[test]
fn no_two_entries_are_one_function_and_row() {
    both_refuse(
        |s| s.entries[1].args = vec![Ty::Int, Ty::Int],
        |b| {
            b[entry_at(1) + 12] = 2;
            b[entry_at(1) + 13] = 2;
        },
        FormatError::DuplicateEntry { at: 1 },
    );
}

#[test]
fn a_function_has_one_argument_count() {
    both_refuse(
        |s| s.entries[1].args = vec![Ty::Float],
        |b| {
            b[entry_at(1) + 8] = 1;
            b[entry_at(1) + 13] = 0;
        },
        FormatError::ArgCountDiffers { at: 1 },
    );
    writer_refuses(
        |s| s.entries[0].args = vec![Ty::Int; 9],
        FormatError::TooManyArgs { at: 0, count: 9 },
    );
    writer_refuses(
        |s| s.entries[0].args = vec![Ty::Int; 300],
        FormatError::TooManyArgs { at: 0, count: 300 },
    );
}

#[test]
fn a_function_has_at_most_sixteen_entries() {
    let all = [Ty::Null, Ty::Bool, Ty::Int, Ty::Float];
    // The first `n` rows of three types, in their order: each distinct.
    let rows = |func: u32, n: usize| -> Vec<Entry> {
        let mut out = Vec::new();
        for a in all {
            for b in all {
                for c in all {
                    out.push(Entry {
                        func,
                        offset: 0,
                        args: vec![a, b, c],
                        ret: Ty::Null,
                    });
                }
            }
        }
        out.truncate(n);
        out
    };
    let with = |entries: Vec<Entry>| Section {
        entries,
        sites: vec![],
        ..sample()
    };
    assert_eq!(MAX_ROWS, 16);
    // Sixteen each of two functions is a section.
    let full = with([rows(3, 16), rows(4, 16)].concat());
    let body = full.to_bytes().expect("written");
    assert_eq!(Section::parse(&body).as_ref(), Ok(&full));
    // A seventeenth of the second is not, to the writer.
    let want = FormatError::TooManyRows { at: 32 };
    assert_eq!(
        with([rows(3, 16), rows(4, 17)].concat()).to_bytes(),
        Err(want.clone())
    );
    // Nor to the reader: the same entry put after the table by hand
    // (the next row in order: bool, null, null), and counted.
    let last = ENTRIES_AT + 31 * ENTRY_LEN;
    let mut entry = body[last..last + ENTRY_LEN].to_vec();
    entry[12..15].copy_from_slice(&[1, 0, 0]);
    let mut b = body.clone();
    b.splice(last + ENTRY_LEN..last + ENTRY_LEN, entry);
    let footer = b.len() - FOOTER_LEN;
    put_u32(&mut b, footer + 24, 33);
    assert_eq!(Tables::parse(&b), Err(want.clone()));
    assert_eq!(
        want.to_string(),
        "entry 32 is its function's entry number 17, and a function has at most 16"
    );
}

#[test]
fn code_for_a_machine_of_fixed_instructions_is_whole_instructions() {
    // The sample as aarch64 code: 17 bytes is not whole instructions.
    both_refuse(
        |s| s.arch = Arch::Aarch64,
        |b| put_u32(b, FOOTER_AT + 12, 2),
        FormatError::Misaligned {
            what: "the code's length",
            at: 0,
            value: 17,
            align: 4,
        },
    );
    // 16 bytes of code, and an entry in the middle of an instruction.
    let aarch64 = |offset: u32| Section {
        arch: Arch::Aarch64,
        code: vec![0; 16],
        entries: vec![Entry {
            func: 0,
            offset,
            args: vec![],
            ret: Ty::Null,
        }],
        sites: vec![],
        ..sample()
    };
    for offset in [1, 2, 3, 5, 14] {
        assert_eq!(
            aarch64(offset).to_bytes(),
            Err(FormatError::Misaligned {
                what: "an entry's offset",
                at: 0,
                value: offset,
                align: 4,
            })
        );
        let mut b = aarch64(4).to_bytes().expect("written");
        put_u32(&mut b, 16 + 4, offset);
        assert_eq!(
            Tables::parse(&b),
            Err(FormatError::Misaligned {
                what: "an entry's offset",
                at: 0,
                value: offset,
                align: 4,
            })
        );
    }
    for offset in [0, 4, 8, 12] {
        let s = aarch64(offset);
        assert_eq!(Section::parse(&s.to_bytes().expect("written")), Ok(s));
    }
    // On x86-64 any offset inside the code is an instruction's.
    for offset in 0..17 {
        let mut s = sample();
        s.entries[3].offset = offset;
        assert!(s.to_bytes().is_ok());
    }
}

#[test]
fn a_site_names_an_operator_only_when_an_operator_left() {
    // Site 0 is an `Operator`: `==`, `!=` and the operators that are
    // not binary operators at all never leave.
    for value in [5, 6, 11, 12, 13, 14, 200] {
        both_refuse(
            |s| s.sites[0].op = value,
            |b| b[site_at(0) + 17] = value,
            FormatError::BadOp { at: 0, value },
        );
    }
    // Sites 2 to 5 are the other kinds: their operator is zero.
    for at in 2..6 {
        for value in [1, 4, 7, 255] {
            both_refuse(
                |s| s.sites[at].op = value,
                |b| b[site_at(at) + 17] = value,
                FormatError::BadOp { at, value },
            );
        }
    }
}

#[test]
fn a_sites_operands_are_the_ones_its_kind_leaves_with() {
    use Ty::{Bool, Float, Int, Null};
    /// A site of the sample, an operator, and the operand types that
    /// site may then have.
    type Rule = (usize, Op, &'static [(Ty, Ty)]);
    let rules: [Rule; 13] = [
        (0, op::ADD, &[(Int, Int)]),
        (0, op::SUB, &[(Int, Int)]),
        (0, op::MUL, &[(Int, Int)]),
        (0, op::REM, &[(Int, Int)]),
        (0, op::DIV, &[(Int, Float), (Float, Int), (Float, Float)]),
        (0, op::LT, &[(Int, Float), (Float, Int), (Float, Float)]),
        (0, op::LE, &[(Int, Float), (Float, Int), (Float, Float)]),
        (0, op::GT, &[(Int, Float), (Float, Int), (Float, Float)]),
        (0, op::GE, &[(Int, Float), (Float, Int), (Float, Float)]),
        (2, 0, &[(Int, Null)]),
        (3, 0, &[(Int, Int)]),
        (4, 0, &[(Null, Null)]),
        (5, 0, &[(Null, Null)]),
    ];
    for (at, op, allowed) in rules {
        for left in [Null, Bool, Int, Float] {
            for right in [Null, Bool, Int, Float] {
                let mut s = sample();
                s.sites[at].op = op;
                s.sites[at].left = left;
                s.sites[at].right = right;
                let mut b = body();
                b[site_at(at) + 17] = op;
                b[site_at(at) + 18] = left.as_u8();
                b[site_at(at) + 19] = right.as_u8();
                if allowed.contains(&(left, right)) {
                    assert_eq!(s.to_bytes().as_ref(), Ok(&b), "site {at}, operator {op}");
                    assert_eq!(Section::parse(&b), Ok(s), "site {at}, operator {op}");
                } else {
                    let want = Err(FormatError::BadOperands { at });
                    assert_eq!(s.to_bytes(), want, "site {at}, operator {op}");
                    assert_eq!(Tables::parse(&b).map(|_| ()), want.map(|_: Vec<u8>| ()));
                }
            }
        }
    }
}

#[test]
fn a_site_does_not_end_before_it_starts() {
    both_refuse(
        |s| s.sites[4].end = 9,
        |b| put_u32(b, site_at(4) + 4, 9),
        FormatError::BadSpan { at: 4 },
    );
    // A site of no length, and one at the start of the source.
    let mut s = sample();
    s.sites[4].end = 10;
    s.sites[5].start = 0;
    s.sites[5].end = 0;
    assert_eq!(Section::parse(&s.to_bytes().expect("written")), Ok(s));
}

#[test]
fn every_error_says_what_and_where() {
    let said = |e: FormatError| e.to_string();
    assert_eq!(
        said(FormatError::EntryOutsideCode {
            at: 3,
            offset: 40,
            code_len: 17
        }),
        "entry 3 is at offset 40, outside the 17 bytes of code"
    );
    assert_eq!(
        said(FormatError::Misaligned {
            what: "an entry's offset",
            at: 2,
            value: 6,
            align: 4
        }),
        "an entry's offset (2) is 6, not a multiple of 4"
    );
    assert_eq!(
        said(FormatError::BadLength {
            body: 100,
            declared: 104
        }),
        "the body is 100 bytes, and its footer's counts add up to 104"
    );
    assert_eq!(
        said(FormatError::BadOp { at: 1, value: 5 }),
        "site 1 has operator 5, which its kind does not allow"
    );
}

// ----- hostile bodies ---------------------------------------------------

/// A body the reader takes is written back the same: there is one
/// encoding of a section.
fn read_then_written_is_the_same(b: &[u8]) {
    if let Ok(s) = Section::parse(b) {
        assert_eq!(s.to_bytes().as_deref(), Ok(b));
    }
}

#[test]
fn no_prefix_of_a_body_panics_the_reader() {
    let b = body();
    for n in 0..b.len() {
        assert!(Tables::parse(&b[..n]).is_err(), "a prefix of {n} bytes");
    }
}

#[test]
fn no_changed_bit_panics_the_reader_and_what_it_takes_is_canonical() {
    let good = body();
    let mut taken = 0;
    for i in 0..good.len() {
        for bit in 0..8 {
            let mut b = good.clone();
            b[i] ^= 1 << bit;
            taken += usize::from(Tables::parse(&b).is_ok());
            read_then_written_is_the_same(&b);
        }
    }
    // Some changes make another section (a byte of code, a line
    // number); most are refused.
    assert!(taken > 0 && taken < good.len() * 8);
}

/// A small generator, the same on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn ty(&mut self) -> Ty {
        [Ty::Null, Ty::Bool, Ty::Int, Ty::Float][self.below(4) as usize]
    }

    /// A section the writer takes: functions with one to three rows
    /// each, of 0 to 8 arguments, and a site of each shape.
    fn section(&mut self) -> Section {
        let arch = [Arch::X86_64, Arch::Aarch64][self.below(2) as usize];
        let align = arch.code_align();
        let code_len = (1 + self.below(40) as u32) * align;
        let code: Vec<u8> = (0..code_len).map(|_| self.next() as u8).collect();
        let mut entries = Vec::new();
        let mut func = 0u32;
        for _ in 0..self.below(6) {
            func += 1 + self.below(3) as u32;
            let argc = self.below(MAX_ARGS as u64 + 1) as usize;
            let mut rows: Vec<Vec<Ty>> = (0..1 + self.below(3))
                .map(|_| (0..argc).map(|_| self.ty()).collect())
                .collect();
            rows.sort();
            rows.dedup();
            for args in rows {
                entries.push(Entry {
                    func,
                    offset: self.below(u64::from(code_len / align)) as u32 * align,
                    args,
                    ret: self.ty(),
                });
            }
        }
        let numbers = [
            (Ty::Int, Ty::Float),
            (Ty::Float, Ty::Int),
            (Ty::Float, Ty::Float),
        ];
        let sites = (0..self.below(12))
            .map(|_| {
                let (kind, op, left, right) = match self.below(8) {
                    0 => {
                        let op = [op::ADD, op::SUB, op::MUL, op::REM][self.below(4) as usize];
                        (Exit::Operator, op, Ty::Int, Ty::Int)
                    }
                    1 | 2 => {
                        let op = [op::DIV, op::LT, op::LE, op::GT, op::GE][self.below(5) as usize];
                        let (l, r) = numbers[self.below(3) as usize];
                        (Exit::Operator, op, l, r)
                    }
                    3 => (Exit::Negate, 0, Ty::Int, Ty::Null),
                    4 => (Exit::Range, 0, Ty::Int, Ty::Int),
                    5 | 6 => (Exit::Budget, 0, Ty::Null, Ty::Null),
                    _ => (Exit::Depth, 0, Ty::Null, Ty::Null),
                };
                let start = self.next() as u32 / 2;
                Site {
                    start,
                    end: start + self.below(100) as u32,
                    line: self.next() as u32,
                    col: self.next() as u32,
                    kind,
                    op,
                    left,
                    right,
                    in_loop: self.below(2) == 1,
                }
            })
            .collect();
        Section {
            arch,
            module: core::array::from_fn(|_| self.next() as u8),
            code,
            entries,
            sites,
        }
    }
}

#[test]
fn sections_made_at_random_are_read_back() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let (mut rows, mut most_entries) = (0, 0);
    for _ in 0..2000 {
        let s = rng.section();
        let b = s.to_bytes().expect("the generator makes sections");
        rows += s
            .entries
            .windows(2)
            .filter(|w| w[0].func == w[1].func)
            .count();
        most_entries = most_entries.max(s.entries.len());
        assert_eq!(Section::parse(&b), Ok(s));
    }
    // The generator reaches several rows a function and long tables.
    assert!(rows > 500 && most_entries >= 8, "{rows} {most_entries}");
}

#[test]
fn bodies_changed_at_random_never_panic_and_what_is_taken_is_canonical() {
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    let mut taken = 0;
    for _ in 0..4000 {
        let mut b = rng.section().to_bytes().expect("written");
        // Up to four bytes changed, anywhere: the tables and the footer
        // are most of a small body.
        for _ in 0..1 + rng.below(4) {
            let at = rng.below(b.len() as u64) as usize;
            b[at] = rng.next() as u8;
        }
        // And, one time in four, cut short or made longer.
        match rng.below(8) {
            0 => b.truncate(rng.below(b.len() as u64) as usize),
            1 => b.extend((0..rng.below(40)).map(|_| 0)),
            _ => {}
        }
        taken += usize::from(Tables::parse(&b).is_ok());
        read_then_written_is_the_same(&b);
    }
    assert!(taken > 0 && taken < 4000, "{taken}");
}

#[test]
fn bytes_made_at_random_under_a_real_footer_never_panic() {
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    for _ in 0..2000 {
        // Tables of random bytes, with a footer whose counts fit them:
        // the reader gets past the lengths and into every field.
        let (entries, sites) = (rng.below(4) as u32, rng.below(4) as u32);
        let code_len = rng.below(5) as u32 * 8;
        let mut b: Vec<u8> = (0..code_len + (entries + sites) * 24)
            .map(|_| rng.next() as u8 % 12)
            .collect();
        b.extend_from_slice(&MAGIC);
        for v in [VERSION, 1, INTERFACE, code_len, entries, sites] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&[0; 48]);
        let _ = Tables::parse(&b);
        read_then_written_is_the_same(&b);
    }
}
