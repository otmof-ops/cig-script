// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The layout of a `native` section's body: the code, the entry table,
//! the site table, the body table, the resume table and the footer.
//!
//! ```text
//! code           the machine code, from the body's first byte
//! zero padding   to a multiple of 8
//! entry table    count x 24 bytes
//! site table     count x 24 bytes
//! body table     count x 16 bytes: func u32, offset u32, first u32, count u32
//! resume table   count x 4 bytes:  pc u32
//! zero padding   to a multiple of 8
//! footer         80 bytes, at the body's end
//! ```
//!
//! Every integer is little-endian. [`Section::to_bytes`] writes a body
//! and [`Tables::parse`] and [`Section::parse`] read one. A body is
//! hostile data to the reader: it never panics, never reads out of
//! bounds, and allocates nothing a body's own length does not bound.
//!
//! **A section has exactly one encoding.** The writer and the reader
//! apply the same rules (`check`, below), every reserved byte is
//! zero, and every field that a site's kind does not use is zero. So
//! `parse` of what `to_bytes` wrote gives the section back, and
//! `to_bytes` of what `parse` read gives the same bytes back.

#![forbid(unsafe_code)]

use core::cmp::Ordering;
use core::fmt;

/// The footer's first eight bytes.
pub const MAGIC: [u8; 8] = *b"CIGNATV1";

/// The format version this crate writes and reads.
pub const VERSION: u32 = 2;

/// The interface version this crate writes and reads: the context's
/// layout, the entry convention, [`MAX_FRAME`], and the body's
/// signature and the helpers' table ([`MAX_BODY_FRAME`]).
pub const INTERFACE: u32 = 2;

/// A `native` body starts at a file offset that is a multiple of this:
/// the largest unit any target maps a file in (4 KiB pages on x86-64
/// Linux, up to 64 KiB on aarch64 Linux, 16 KiB on Apple's systems, a
/// 64 KiB granularity on Windows), so the code can be mapped where it
/// lies.
pub const ALIGN: u64 = 65_536;

/// The most arguments an entry takes.
pub const MAX_ARGS: usize = 8;

/// The most entries one function has: the rows of argument types it is
/// compiled for. A caller looks through them at a call.
pub const MAX_ROWS: usize = 16;

/// The most stack one call of any function in a section uses through
/// its entry, in bytes: its frame, with the return address and the
/// saved frame pointer. It is part of interface 2. The code generator
/// compiles no function whose entry needs more, and a caller that lets
/// native code go `depth_left` calls deep counts on `depth_left *
/// MAX_FRAME` bytes of stack being enough.
///
/// It is no larger than the smallest guard page, so a chain of calls
/// that did run off a stack would touch the guard page and not jump it.
pub const MAX_FRAME: usize = 4096;

/// The most stack one call of any body in a section uses, in bytes.
/// The code generator compiles no body whose frame is larger, and the
/// VM enters a body only where the stack has room for it.
pub const MAX_BODY_FRAME: usize = 65_536;

/// The footer's length.
pub const FOOTER_LEN: usize = 80;

/// An entry's length in the table.
pub const ENTRY_LEN: usize = 24;

/// A site's length in the table.
pub const SITE_LEN: usize = 24;

/// A body's length in the table.
pub const BODY_LEN: usize = 16;

/// A resume point's length in the table.
pub const RESUME_LEN: usize = 4;

/// The type of a value native code holds.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Ty {
    /// `null`. Its slot is zero.
    Null = 0,
    /// A bool: its slot is 0 or 1.
    Bool = 1,
    /// A 64-bit signed integer.
    Int = 2,
    /// A 64-bit IEEE 754 float: its slot is the float's bits.
    Float = 3,
}

impl Ty {
    /// The type's code in a table.
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// The type a code names.
    pub const fn from_u8(v: u8) -> Option<Ty> {
        match v {
            0 => Some(Ty::Null),
            1 => Some(Ty::Bool),
            2 => Some(Ty::Int),
            3 => Some(Ty::Float),
            _ => None,
        }
    }

    const fn is_number(self) -> bool {
        matches!(self, Ty::Int | Ty::Float)
    }
}

/// The machine a section's code is for.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Arch {
    /// x86-64.
    X86_64 = 1,
    /// 64-bit ARM.
    Aarch64 = 2,
}

impl Arch {
    /// The architecture's code in the footer.
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// The architecture a code names.
    pub const fn from_u32(v: u32) -> Option<Arch> {
        match v {
            1 => Some(Arch::X86_64),
            2 => Some(Arch::Aarch64),
            _ => None,
        }
    }

    /// The architecture this crate was built for, when it has a code.
    pub const fn host() -> Option<Arch> {
        if cfg!(target_arch = "x86_64") {
            Some(Arch::X86_64)
        } else if cfg!(target_arch = "aarch64") {
            Some(Arch::Aarch64)
        } else {
            None
        }
    }

    /// What every entry's offset and the code's length are a multiple
    /// of: an instruction's size where it is fixed, 1 where it is not. A
    /// jump to an address that is not a multiple of it faults.
    pub const fn code_align(self) -> u32 {
        match self {
            Arch::X86_64 => 1,
            Arch::Aarch64 => 4,
        }
    }
}

/// Why native code left: the status an entry returns. 0 is not an exit:
/// the entry returned its result.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Exit {
    /// A binary operator cannot give a result. The site names the
    /// operator and the operands' types; the context holds the operands.
    Operator = 1,
    /// Unary minus overflowed. The context's `left` is the operand.
    Negate = 2,
    /// A range wider than the limit. The context's `left` and `right`
    /// are its bounds.
    Range = 3,
    /// The step budget is spent.
    Budget = 4,
    /// The call depth is reached.
    Depth = 5,
}

impl Exit {
    /// The exit's code: a site's kind byte and an entry's status.
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// The exit a code names. `None` for 0 and for any other value.
    pub const fn from_u8(v: u8) -> Option<Exit> {
        match v {
            1 => Some(Exit::Operator),
            2 => Some(Exit::Negate),
            3 => Some(Exit::Range),
            4 => Some(Exit::Budget),
            5 => Some(Exit::Depth),
            _ => None,
        }
    }
}

/// A binary operator, numbered as cig's own `BinOp` is.
pub type Op = u8;

/// The operators a site can name.
pub mod op {
    use super::Op;
    /// `+`
    pub const ADD: Op = 0;
    /// `-`
    pub const SUB: Op = 1;
    /// `*`
    pub const MUL: Op = 2;
    /// `/`
    pub const DIV: Op = 3;
    /// `%`
    pub const REM: Op = 4;
    /// `<`
    pub const LT: Op = 7;
    /// `<=`
    pub const LE: Op = 8;
    /// `>`
    pub const GT: Op = 9;
    /// `>=`
    pub const GE: Op = 10;
}

/// Which functions of the module the build gave a body.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rule {
    /// No function has a body.
    None = 0,
    /// A function has a body when one of its loops is faster as machine
    /// code, by the compiler's own reckoning, and the body is entered
    /// at those loops.
    Loops = 1,
    /// Every function has a body, entered at every resume point the
    /// bytecode has: a build for tests, where every body should run. A
    /// runtime enters such a body whatever its guesses do.
    Every = 2,
}

impl Rule {
    /// The rule's code in the footer.
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// The rule a code names.
    pub const fn from_u32(v: u32) -> Option<Rule> {
        match v {
            0 => Some(Rule::None),
            1 => Some(Rule::Loops),
            2 => Some(Rule::Every),
            _ => None,
        }
    }
}

/// One function's body: the function's index in the module (0 is the
/// top level), where its code starts, and the pcs it can be entered at.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Body {
    /// The function's index in the bytecode module. 0 is the top level.
    pub func: u32,
    /// The offset of the body's first instruction in the code.
    pub offset: u32,
    /// The pcs the body can be entered at, strictly ascending. The
    /// first is `0`. Its index here is the `at` the body is entered
    /// with.
    pub resumes: Vec<u32>,
}

/// One specialization: a function, a row of argument types, and where
/// its entry is in the code.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Entry {
    /// The function's index in the bytecode module.
    pub func: u32,
    /// The offset of the entry's first instruction in the code.
    pub offset: u32,
    /// The arguments' types, at most [`MAX_ARGS`]. Every entry of one
    /// function has as many as the function has parameters.
    pub args: Vec<Ty>,
    /// The result's type.
    pub ret: Ty,
}

/// A place native code can leave from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Site {
    /// Where the instruction starts in the source, in bytes.
    pub start: u32,
    /// Where it ends.
    pub end: u32,
    /// Its line.
    pub line: u32,
    /// Its column.
    pub col: u32,
    /// What happens there.
    pub kind: Exit,
    /// For [`Exit::Operator`], the operator ([`op`]); otherwise 0.
    pub op: Op,
    /// The left operand's type. [`Ty::Null`] where the kind has none.
    pub left: Ty,
    /// The right operand's type. [`Ty::Null`] where the kind has none.
    pub right: Ty,
    /// The place is inside a loop of its function.
    pub in_loop: bool,
}

/// A section, whole: what the code generator makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// The machine the code is for.
    pub arch: Arch,
    /// The BLAKE3 in the header of the bytecode module the code was
    /// compiled from.
    pub module: [u8; 32],
    /// The machine code.
    pub code: Vec<u8>,
    /// The entries, sorted by function and then by argument types.
    pub entries: Vec<Entry>,
    /// The sites. Native code names one by its index.
    pub sites: Vec<Site>,
    /// The bodies, strictly ascending by function.
    pub bodies: Vec<Body>,
    /// Which functions of the module the build gave a body.
    pub rule: Rule,
}

/// A section without its code: what a runtime keeps while the code
/// stays in the file it is mapped from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tables {
    /// The machine the code is for.
    pub arch: Arch,
    /// The BLAKE3 in the header of the bytecode module the code was
    /// compiled from.
    pub module: [u8; 32],
    /// The code's length: the body's first `code_len` bytes.
    pub code_len: u32,
    /// The entries, sorted by function and then by argument types.
    pub entries: Vec<Entry>,
    /// The sites.
    pub sites: Vec<Site>,
    /// The bodies, strictly ascending by function.
    pub bodies: Vec<Body>,
    /// Which functions of the module the build gave a body.
    pub rule: Rule,
}

/// Why bytes are not a section's body, or a [`Section`] is not one
/// [`Section::to_bytes`] writes. `at` is an entry's or a site's index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatError {
    /// The body is shorter than a footer.
    TooShort {
        /// The body's length.
        len: usize,
    },
    /// The footer does not start with [`MAGIC`].
    BadMagic {
        /// What it starts with.
        found: [u8; 8],
    },
    /// The format version is not [`VERSION`].
    BadVersion {
        /// The version found.
        found: u32,
    },
    /// The architecture's code names none.
    BadArch {
        /// The code found.
        found: u32,
    },
    /// The interface version is not [`INTERFACE`].
    BadInterface {
        /// The version found.
        found: u32,
    },
    /// The code, its padding, the two tables and the footer do not add
    /// up to the body.
    BadLength {
        /// The body's length.
        body: u64,
        /// What the footer's counts add up to.
        declared: u64,
    },
    /// More code, entries or sites than a 32-bit count holds.
    TooMany {
        /// `"bytes of code"`, `"entries"` or `"sites"`.
        what: &'static str,
        /// How many.
        count: usize,
    },
    /// A byte of the padding after the code is not zero.
    NonzeroPadding {
        /// The byte's offset in the body.
        offset: usize,
    },
    /// A reserved byte is not zero.
    NonzeroReserved {
        /// `"entry"`, `"site"` or `"footer"`.
        region: &'static str,
        /// The entry's or site's index; 0 for the footer.
        at: usize,
        /// The byte's offset in the entry, the site or the footer.
        offset: usize,
    },
    /// An entry has more than [`MAX_ARGS`] arguments.
    TooManyArgs {
        /// The entry.
        at: usize,
        /// How many it has.
        count: usize,
    },
    /// A byte that should name a [`Ty`] names none.
    BadType {
        /// `"an entry's result"`, `"an entry's argument"`, `"a site's
        /// left operand"` or `"a site's right operand"`.
        what: &'static str,
        /// The entry or site.
        at: usize,
        /// The byte.
        value: u8,
    },
    /// An argument slot past an entry's argument count is not zero.
    NonzeroUnusedArg {
        /// The entry.
        at: usize,
        /// The slot.
        slot: usize,
    },
    /// An entry's offset is not inside the code.
    EntryOutsideCode {
        /// The entry.
        at: usize,
        /// Its offset.
        offset: u32,
        /// The code's length.
        code_len: u32,
    },
    /// The code's length or an entry's offset is not a multiple of the
    /// architecture's instruction size ([`Arch::code_align`]).
    Misaligned {
        /// `"the code's length"` or `"an entry's offset"`.
        what: &'static str,
        /// The entry; 0 for the code's length.
        at: usize,
        /// The value.
        value: u32,
        /// What it must be a multiple of.
        align: u32,
    },
    /// Two entries of one function have different argument counts.
    ArgCountDiffers {
        /// The later entry.
        at: usize,
    },
    /// A function has more than [`MAX_ROWS`] entries.
    TooManyRows {
        /// The first entry past the limit.
        at: usize,
    },
    /// An entry sorts before the one it follows (by function, then by
    /// argument types).
    EntriesOutOfOrder {
        /// The later entry.
        at: usize,
    },
    /// An entry has the function and the argument types of the one it
    /// follows.
    DuplicateEntry {
        /// The later entry.
        at: usize,
    },
    /// A site's kind byte names no [`Exit`].
    BadExit {
        /// The site.
        at: usize,
        /// The byte.
        value: u8,
    },
    /// A site's operator is not one its kind allows: an operator that
    /// can leave for [`Exit::Operator`], 0 for every other kind.
    BadOp {
        /// The site.
        at: usize,
        /// The operator.
        value: u8,
    },
    /// A site's operand types are not ones its kind and operator leave
    /// with.
    BadOperands {
        /// The site.
        at: usize,
    },
    /// A site's flags set a bit other than bit 0.
    BadFlags {
        /// The site.
        at: usize,
        /// The flags.
        value: u8,
    },
    /// A site ends before it starts.
    BadSpan {
        /// The site.
        at: usize,
    },
    /// A body's offset is not inside the code.
    BodyOutsideCode {
        /// The body.
        at: usize,
        /// Its offset.
        offset: u32,
        /// The code's length.
        code_len: u32,
    },
    /// A body sorts the same as, or before, the one it follows (by
    /// function).
    BodiesOutOfOrder {
        /// The later body.
        at: usize,
    },
    /// A body has no resume point.
    BodyWithoutResume {
        /// The body.
        at: usize,
    },
    /// A body's resume points are not strictly ascending.
    ResumesOutOfOrder {
        /// The body.
        at: usize,
        /// The pc that sorts out of order.
        pc: u32,
    },
    /// A body's `first` or `count` does not place its resume points as
    /// the run of pcs following the previous body's.
    ResumeRunMismatch {
        /// The body.
        at: usize,
        /// The `first` and `count` found.
        found: (u32, u32),
        /// Where the run should start, and how many are left.
        expected: (u32, u32),
    },
    /// The resume table has pcs no body's `first`/`count` reaches.
    ResumeTableHasExtra {
        /// How many pcs are past the last body's run.
        extra: u32,
    },
    /// The footer's `rule` is not 0, 1 or 2.
    BadRule {
        /// The code found.
        found: u32,
    },
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use FormatError::*;
        match self {
            TooShort { len } => write!(f, "the body is {len} bytes, shorter than its footer"),
            BadMagic { found } => write!(f, "the footer starts with {found:02x?}, not `CIGNATV1`"),
            BadVersion { found } => write!(f, "format version {found}; this reads {VERSION}"),
            BadArch { found } => write!(f, "architecture code {found} names none"),
            BadInterface { found } => {
                write!(f, "interface version {found}; this reads {INTERFACE}")
            }
            BadLength { body, declared } => write!(
                f,
                "the body is {body} bytes, and its footer's counts add up to {declared}"
            ),
            TooMany { what, count } => write!(f, "{count} {what}, more than a section holds"),
            NonzeroPadding { offset } => {
                write!(f, "the padding byte at offset {offset} is not zero")
            }
            NonzeroReserved { region, at, offset } => write!(
                f,
                "reserved byte {offset} of {region} {at} is not zero"
            ),
            TooManyArgs { at, count } => write!(
                f,
                "entry {at} has {count} arguments, more than {MAX_ARGS}"
            ),
            BadType { what, at, value } => write!(f, "{what} ({at}) has type code {value}"),
            NonzeroUnusedArg { at, slot } => write!(
                f,
                "entry {at}'s argument slot {slot} is past its argument count and not zero"
            ),
            EntryOutsideCode {
                at,
                offset,
                code_len,
            } => write!(
                f,
                "entry {at} is at offset {offset}, outside the {code_len} bytes of code"
            ),
            Misaligned {
                what,
                at,
                value,
                align,
            } => write!(f, "{what} ({at}) is {value}, not a multiple of {align}"),
            TooManyRows { at } => write!(
                f,
                "entry {at} is its function's entry number {}, and a function has at most {MAX_ROWS}",
                MAX_ROWS + 1
            ),
            ArgCountDiffers { at } => write!(
                f,
                "entry {at} has another argument count than the entry before it, of the same function"
            ),
            EntriesOutOfOrder { at } => write!(
                f,
                "entry {at} sorts before the entry it follows (by function, then by argument types)"
            ),
            DuplicateEntry { at } => write!(
                f,
                "entry {at} repeats the function and the argument types of the entry before it"
            ),
            BadExit { at, value } => write!(f, "site {at} has kind {value}, which names no exit"),
            BadOp { at, value } => write!(
                f,
                "site {at} has operator {value}, which its kind does not allow"
            ),
            BadOperands { at } => write!(
                f,
                "site {at} has operand types its kind and operator do not leave with"
            ),
            BadFlags { at, value } => {
                write!(f, "site {at} has flags {value:#04x}; only bit 0 is defined")
            }
            BadSpan { at } => write!(f, "site {at} ends before it starts"),
            BodyOutsideCode {
                at,
                offset,
                code_len,
            } => write!(
                f,
                "body {at} is at offset {offset}, outside the {code_len} bytes of code"
            ),
            BodiesOutOfOrder { at } => write!(
                f,
                "body {at} sorts before the body it follows, or shares its function"
            ),
            BodyWithoutResume { at } => write!(f, "body {at} has no resume point"),
            ResumesOutOfOrder { at, pc } => write!(
                f,
                "body {at}'s resume point {pc} sorts before the pc it follows"
            ),
            ResumeRunMismatch {
                at,
                found,
                expected,
            } => write!(
                f,
                "body {at}'s resume run (first {}, count {}) does not follow the previous body's (expected first {}, up to {} left)",
                found.0, found.1, expected.0, expected.1
            ),
            ResumeTableHasExtra { extra } => write!(
                f,
                "the resume table has {extra} pcs past the last body's run"
            ),
            BadRule { found } => write!(f, "rule code {found} names none"),
        }
    }
}

impl std::error::Error for FormatError {}

/// The rules a section's tables obey beyond each field's own range, for
/// the writer and the reader alike.
///
/// Entries: at most [`MAX_ARGS`] arguments; an offset inside the code and
/// a multiple of the architecture's instruction size; sorted by function
/// and then by argument types, no two alike; one argument count a
/// function, and at most [`MAX_ROWS`] entries of it.
///
/// Sites: a span that does not end before it starts, and only what its
/// kind uses, with every other field zero:
///
/// | kind | operator | left | right |
/// |---|---|---|---|
/// | `Operator` | `+ - *` | int | int |
/// | `Operator` | `%` | int | int |
/// | `Operator` | `/`, `< <= > >=` | int or float | int or float, not both ints |
/// | `Negate` | 0 | int | null |
/// | `Range` | 0 | int | int |
/// | `Budget`, `Depth` | 0 | null | null |
///
/// Bodies: strictly ascending by function; an offset inside the code and
/// a multiple of the architecture's instruction size; at least one
/// resume point, and strictly ascending; with their
/// `first` and `count` running back-to-back over the resume table and
/// covering it exactly.
fn check(
    arch: Arch,
    code_len: u32,
    entries: &[Entry],
    sites: &[Site],
    bodies: &[Body],
) -> Result<(), FormatError> {
    let align = arch.code_align();
    if code_len % align != 0 {
        return Err(FormatError::Misaligned {
            what: "the code's length",
            at: 0,
            value: code_len,
            align,
        });
    }
    let mut prev: Option<&Entry> = None;
    let mut rows = 0;
    for (at, e) in entries.iter().enumerate() {
        if e.args.len() > MAX_ARGS {
            return Err(FormatError::TooManyArgs {
                at,
                count: e.args.len(),
            });
        }
        if e.offset >= code_len {
            return Err(FormatError::EntryOutsideCode {
                at,
                offset: e.offset,
                code_len,
            });
        }
        if e.offset % align != 0 {
            return Err(FormatError::Misaligned {
                what: "an entry's offset",
                at,
                value: e.offset,
                align,
            });
        }
        if let Some(p) = prev {
            match p.func.cmp(&e.func) {
                Ordering::Less => rows = 0,
                Ordering::Greater => return Err(FormatError::EntriesOutOfOrder { at }),
                Ordering::Equal => {
                    if p.args.len() != e.args.len() {
                        return Err(FormatError::ArgCountDiffers { at });
                    }
                    match p.args.cmp(&e.args) {
                        Ordering::Less => {}
                        Ordering::Equal => return Err(FormatError::DuplicateEntry { at }),
                        Ordering::Greater => return Err(FormatError::EntriesOutOfOrder { at }),
                    }
                }
            }
        }
        rows += 1;
        if rows > MAX_ROWS {
            return Err(FormatError::TooManyRows { at });
        }
        prev = Some(e);
    }
    for (at, s) in sites.iter().enumerate() {
        if s.start > s.end {
            return Err(FormatError::BadSpan { at });
        }
        let (l, r) = (s.left, s.right);
        let both_ints = l == Ty::Int && r == Ty::Int;
        let mixed = l.is_number() && r.is_number() && !both_ints;
        let operands = match s.kind {
            Exit::Operator => match s.op {
                op::ADD | op::SUB | op::MUL | op::REM => both_ints,
                op::DIV | op::LT | op::LE | op::GT | op::GE => mixed,
                _ => return Err(FormatError::BadOp { at, value: s.op }),
            },
            _ if s.op != 0 => return Err(FormatError::BadOp { at, value: s.op }),
            Exit::Negate => l == Ty::Int && r == Ty::Null,
            Exit::Range => both_ints,
            Exit::Budget | Exit::Depth => l == Ty::Null && r == Ty::Null,
        };
        if !operands {
            return Err(FormatError::BadOperands { at });
        }
    }
    let mut prev_func: Option<u32> = None;
    for (at, b) in bodies.iter().enumerate() {
        if let Some(p) = prev_func {
            if b.func <= p {
                return Err(FormatError::BodiesOutOfOrder { at });
            }
        }
        prev_func = Some(b.func);
        if b.offset >= code_len {
            return Err(FormatError::BodyOutsideCode {
                at,
                offset: b.offset,
                code_len,
            });
        }
        if b.offset % align != 0 {
            return Err(FormatError::Misaligned {
                what: "a body's offset",
                at,
                value: b.offset,
                align,
            });
        }
        if b.resumes.is_empty() {
            return Err(FormatError::BodyWithoutResume { at });
        }
        for w in b.resumes.windows(2) {
            if w[1] <= w[0] {
                return Err(FormatError::ResumesOutOfOrder { at, pc: w[1] });
            }
        }
    }
    Ok(())
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    let mut v = [0u8; 4];
    for (d, s) in v.iter_mut().zip(b.iter().skip(at)) {
        *d = *s;
    }
    u32::from_le_bytes(v)
}

/// An entry's 24 bytes.
fn put_entry(out: &mut Vec<u8>, e: &Entry) {
    out.extend_from_slice(&e.func.to_le_bytes());
    out.extend_from_slice(&e.offset.to_le_bytes());
    // At most `MAX_ARGS`: `check` ran.
    out.push(e.args.len() as u8);
    out.push(e.ret.as_u8());
    out.extend_from_slice(&[0; 2]);
    let mut row = [0u8; MAX_ARGS];
    for (slot, t) in row.iter_mut().zip(&e.args) {
        *slot = t.as_u8();
    }
    out.extend_from_slice(&row);
    out.extend_from_slice(&[0; 4]);
}

/// An entry from its 24 bytes: each field in its own range, each
/// reserved byte and unused slot zero.
fn get_entry(b: &[u8; ENTRY_LEN], at: usize) -> Result<Entry, FormatError> {
    for offset in [10, 11, 20, 21, 22, 23] {
        if b[offset] != 0 {
            return Err(FormatError::NonzeroReserved {
                region: "entry",
                at,
                offset,
            });
        }
    }
    let count = usize::from(b[8]);
    if count > MAX_ARGS {
        return Err(FormatError::TooManyArgs { at, count });
    }
    let ret = Ty::from_u8(b[9]).ok_or(FormatError::BadType {
        what: "an entry's result",
        at,
        value: b[9],
    })?;
    let mut args = Vec::with_capacity(count);
    for (slot, value) in b[12..12 + MAX_ARGS].iter().copied().enumerate() {
        if slot < count {
            args.push(Ty::from_u8(value).ok_or(FormatError::BadType {
                what: "an entry's argument",
                at,
                value,
            })?);
        } else if value != 0 {
            return Err(FormatError::NonzeroUnusedArg { at, slot });
        }
    }
    Ok(Entry {
        func: u32_at(b, 0),
        offset: u32_at(b, 4),
        args,
        ret,
    })
}

/// A site's 24 bytes.
fn put_site(out: &mut Vec<u8>, s: &Site) {
    for v in [s.start, s.end, s.line, s.col] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&[
        s.kind.as_u8(),
        s.op,
        s.left.as_u8(),
        s.right.as_u8(),
        u8::from(s.in_loop),
        0,
        0,
        0,
    ]);
}

/// A site from its 24 bytes: each field in its own range, each reserved
/// byte zero.
fn get_site(b: &[u8; SITE_LEN], at: usize) -> Result<Site, FormatError> {
    for offset in [21, 22, 23] {
        if b[offset] != 0 {
            return Err(FormatError::NonzeroReserved {
                region: "site",
                at,
                offset,
            });
        }
    }
    let kind = Exit::from_u8(b[16]).ok_or(FormatError::BadExit { at, value: b[16] })?;
    let left = Ty::from_u8(b[18]).ok_or(FormatError::BadType {
        what: "a site's left operand",
        at,
        value: b[18],
    })?;
    let right = Ty::from_u8(b[19]).ok_or(FormatError::BadType {
        what: "a site's right operand",
        at,
        value: b[19],
    })?;
    if b[20] & !1 != 0 {
        return Err(FormatError::BadFlags { at, value: b[20] });
    }
    Ok(Site {
        start: u32_at(b, 0),
        end: u32_at(b, 4),
        line: u32_at(b, 8),
        col: u32_at(b, 12),
        kind,
        op: b[17],
        left,
        right,
        in_loop: b[20] == 1,
    })
}

/// The zero bytes between the code and the entry table.
fn code_padding(code_len: u32) -> u32 {
    (8 - code_len % 8) % 8
}

/// The zero bytes between the resume table and the footer. The entry,
/// site and body tables are 8-byte aligned and have 8-byte-multiple
/// widths; only the resume table (4 bytes a row) can leave an odd
/// multiple of 4 behind.
fn resume_padding(resume_count: u32) -> u32 {
    // Zero or four bytes: an even number of resume points is already
    // an 8-byte multiple, an odd one needs four more. Taken `% 2`
    // first so the multiplication cannot overflow a u32.
    (resume_count % 2) * RESUME_LEN as u32
}

/// A body's 16 bytes.
fn put_body(out: &mut Vec<u8>, b: &Body, first: u32) {
    out.extend_from_slice(&b.func.to_le_bytes());
    out.extend_from_slice(&b.offset.to_le_bytes());
    out.extend_from_slice(&first.to_le_bytes());
    // At most `u32::MAX` resumes: the writer's count fits in a u32
    // because the resume table's own length fits.
    let count = b.resumes.len() as u32;
    out.extend_from_slice(&count.to_le_bytes());
}

/// A body from its 16 bytes' first 12: the resumes are filled in after
/// the resume table is read.
fn get_body_head(b: &[u8; BODY_LEN]) -> (u32, u32, u32, u32) {
    (u32_at(b, 0), u32_at(b, 4), u32_at(b, 8), u32_at(b, 12))
}

impl Section {
    /// The section's body. Refuses a section [`Section::parse`] would
    /// refuse, so what it writes is always read back.
    pub fn to_bytes(&self) -> Result<Vec<u8>, FormatError> {
        let count = |what: &'static str, n: usize| {
            u32::try_from(n).map_err(|_| FormatError::TooMany { what, count: n })
        };
        let code_len = count("bytes of code", self.code.len())?;
        let entries = count("entries", self.entries.len())?;
        let sites = count("sites", self.sites.len())?;
        let body_count = count("bodies", self.bodies.len())?;
        let resume_total: usize = self.bodies.iter().map(|b| b.resumes.len()).sum();
        let resume_count = count("resume points", resume_total)?;
        check(
            self.arch,
            code_len,
            &self.entries,
            &self.sites,
            &self.bodies,
        )?;

        let mut out = Vec::with_capacity(
            self.code.len()
                + 8
                + self.entries.len() * ENTRY_LEN
                + self.sites.len() * SITE_LEN
                + self.bodies.len() * BODY_LEN
                + resume_total * RESUME_LEN
                + 8
                + FOOTER_LEN,
        );
        out.extend_from_slice(&self.code);
        out.resize(out.len() + code_padding(code_len) as usize, 0);
        for e in &self.entries {
            put_entry(&mut out, e);
        }
        for s in &self.sites {
            put_site(&mut out, s);
        }
        let mut first: u32 = 0;
        for b in &self.bodies {
            put_body(&mut out, b, first);
            // `check` ran: `resume_total` fits a u32, so does `first`.
            first += b.resumes.len() as u32;
        }
        for b in &self.bodies {
            for pc in &b.resumes {
                out.extend_from_slice(&pc.to_le_bytes());
            }
        }
        out.resize(out.len() + resume_padding(resume_count) as usize, 0);
        out.extend_from_slice(&MAGIC);
        for v in [
            VERSION,
            self.arch.as_u32(),
            INTERFACE,
            code_len,
            entries,
            sites,
        ] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.module);
        for v in [body_count, resume_count, self.rule.as_u32()] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&[0; 4]);
        Ok(out)
    }

    /// A body as a section, with a copy of its code. Refuses what
    /// [`Tables::parse`] refuses.
    pub fn parse(body: &[u8]) -> Result<Section, FormatError> {
        let t = Tables::parse(body)?;
        let code = body
            .get(..t.code_len as usize)
            .ok_or(FormatError::TooShort { len: body.len() })?
            .to_vec();
        Ok(Section {
            arch: t.arch,
            module: t.module,
            code,
            entries: t.entries,
            sites: t.sites,
            bodies: t.bodies,
            rule: t.rule,
        })
    }
}

/// How many bodies a section's footer says the section has: `footer`
/// is the section's last [`FOOTER_LEN`] bytes. `None` when they are not
/// a footer of this version. Nothing else of the section is looked at:
/// a caller that goes on to use the section reads it whole
/// ([`Tables::parse`]).
pub fn bodies_in(footer: &[u8]) -> Option<u32> {
    let footer: &[u8; FOOTER_LEN] = footer.try_into().ok()?;
    if footer[..8] != MAGIC || u32_at(footer, 8) != VERSION {
        return None;
    }
    Some(u32_at(footer, 64))
}

impl Tables {
    /// A body's tables, without its code. Refuses a body that is not
    /// exactly what [`Section::to_bytes`] writes for some section: a
    /// wrong magic, version, architecture or interface version; counts
    /// that do not add up to the body; a byte of padding or a reserved
    /// byte that is not zero; a field out of its range; and whatever
    /// breaks the tables' rules (the entries' order, one argument count
    /// a function, offsets inside the code and on an instruction
    /// boundary, a site that says more than its kind uses).
    ///
    /// What it cannot check is what only the module knows: that an
    /// entry's function exists and takes that many arguments.
    pub fn parse(body: &[u8]) -> Result<Tables, FormatError> {
        let short = FormatError::TooShort { len: body.len() };
        let Some(footer) = body.last_chunk::<FOOTER_LEN>() else {
            return Err(short);
        };
        let mut magic = [0u8; 8];
        magic.copy_from_slice(&footer[..8]);
        if magic != MAGIC {
            return Err(FormatError::BadMagic { found: magic });
        }
        let version = u32_at(footer, 8);
        if version != VERSION {
            return Err(FormatError::BadVersion { found: version });
        }
        let arch = u32_at(footer, 12);
        let arch = Arch::from_u32(arch).ok_or(FormatError::BadArch { found: arch })?;
        let interface = u32_at(footer, 16);
        if interface != INTERFACE {
            return Err(FormatError::BadInterface { found: interface });
        }
        let (code_len, entries_n, sites_n) =
            (u32_at(footer, 20), u32_at(footer, 24), u32_at(footer, 28));
        let mut module = [0u8; 32];
        module.copy_from_slice(&footer[32..64]);
        let (bodies_n, resumes_n, rule_v) =
            (u32_at(footer, 64), u32_at(footer, 68), u32_at(footer, 72));
        if let Some(i) = footer[76..].iter().position(|b| *b != 0) {
            return Err(FormatError::NonzeroReserved {
                region: "footer",
                at: 0,
                offset: 76 + i,
            });
        }
        let rule = Rule::from_u32(rule_v).ok_or(FormatError::BadRule { found: rule_v })?;

        // The body is exactly its parts. In 64 bits the sum cannot
        // overflow: each term is under 2^37.
        let code_pad = code_padding(code_len);
        let resume_pad = resume_padding(resumes_n);
        let declared = u64::from(code_len)
            + u64::from(code_pad)
            + u64::from(entries_n) * ENTRY_LEN as u64
            + u64::from(sites_n) * SITE_LEN as u64
            + u64::from(bodies_n) * BODY_LEN as u64
            + u64::from(resumes_n) * RESUME_LEN as u64
            + u64::from(resume_pad)
            + FOOTER_LEN as u64;
        if declared != body.len() as u64 {
            return Err(FormatError::BadLength {
                body: body.len() as u64,
                declared,
            });
        }
        // From here every count is bounded by the body's own length.
        let entries_at = code_len as usize + code_pad as usize;
        let sites_at = entries_at + entries_n as usize * ENTRY_LEN;
        let bodies_at = sites_at + sites_n as usize * SITE_LEN;
        let resumes_at = bodies_at + bodies_n as usize * BODY_LEN;
        let resume_pad_at = resumes_at + resumes_n as usize * RESUME_LEN;
        let footer_at = resume_pad_at + resume_pad as usize;
        let (
            Some(code_pad_bytes),
            Some(entry_bytes),
            Some(site_bytes),
            Some(body_bytes),
            Some(resume_bytes),
            Some(resume_pad_bytes),
        ) = (
            body.get(code_len as usize..entries_at),
            body.get(entries_at..sites_at),
            body.get(sites_at..bodies_at),
            body.get(bodies_at..resumes_at),
            body.get(resumes_at..resume_pad_at),
            body.get(resume_pad_at..footer_at),
        )
        else {
            return Err(short);
        };
        if let Some(i) = code_pad_bytes.iter().position(|b| *b != 0) {
            return Err(FormatError::NonzeroPadding {
                offset: code_len as usize + i,
            });
        }
        if let Some(i) = resume_pad_bytes.iter().position(|b| *b != 0) {
            return Err(FormatError::NonzeroPadding {
                offset: resume_pad_at + i,
            });
        }
        let mut entries = Vec::with_capacity(entries_n as usize);
        for (at, chunk) in entry_bytes.chunks_exact(ENTRY_LEN).enumerate() {
            let Ok(b) = <&[u8; ENTRY_LEN]>::try_from(chunk) else {
                return Err(short);
            };
            entries.push(get_entry(b, at)?);
        }
        let mut sites = Vec::with_capacity(sites_n as usize);
        for (at, chunk) in site_bytes.chunks_exact(SITE_LEN).enumerate() {
            let Ok(b) = <&[u8; SITE_LEN]>::try_from(chunk) else {
                return Err(short);
            };
            sites.push(get_site(b, at)?);
        }
        // Read the body heads, check the resume runs cover the table,
        // then slice out each body's pcs from the resume bytes.
        let mut heads = Vec::with_capacity(bodies_n as usize);
        let mut expected_first: u32 = 0;
        let mut left: u32 = resumes_n;
        for (at, chunk) in body_bytes.chunks_exact(BODY_LEN).enumerate() {
            let Ok(b) = <&[u8; BODY_LEN]>::try_from(chunk) else {
                return Err(short);
            };
            let (func, offset, first, count) = get_body_head(b);
            if first != expected_first || count > left {
                return Err(FormatError::ResumeRunMismatch {
                    at,
                    found: (first, count),
                    expected: (expected_first, left),
                });
            }
            expected_first = expected_first.saturating_add(count);
            left -= count;
            heads.push((func, offset, first, count));
        }
        if left != 0 {
            return Err(FormatError::ResumeTableHasExtra { extra: left });
        }
        let mut bodies = Vec::with_capacity(bodies_n as usize);
        for (at, (func, offset, first, count)) in heads.into_iter().enumerate() {
            let from = first as usize * RESUME_LEN;
            let to = from + count as usize * RESUME_LEN;
            let Some(slice) = resume_bytes.get(from..to) else {
                return Err(short);
            };
            let mut resumes = Vec::with_capacity(count as usize);
            for chunk in slice.chunks_exact(RESUME_LEN) {
                let mut word = [0u8; 4];
                word.copy_from_slice(chunk);
                resumes.push(u32::from_le_bytes(word));
            }
            // `check` below catches an empty resume list and resume
            // points out of order; here we build the list and `check`
            // sees it like any other.
            let _ = at;
            bodies.push(Body {
                func,
                offset,
                resumes,
            });
        }
        check(arch, code_len, &entries, &sites, &bodies)?;
        Ok(Tables {
            arch,
            module,
            code_len,
            entries,
            sites,
            bodies,
            rule,
        })
    }
}

#[cfg(test)]
mod tests;
