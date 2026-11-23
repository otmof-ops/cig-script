// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The loader's tests. On Linux x86-64 and aarch64 they run real machine
//! code: three small functions written out instruction by instruction,
//! put in a section, written to a file where a program's `native`
//! section would lie, mapped, armed and called. On every other target
//! they hold `map` to `Unsupported`.

use super::*;
use std::io::Write;

/// A file of `before` zero bytes and then `bytes`.
fn file_with(before: usize, bytes: &[u8]) -> (tempfile::NamedTempFile, std::fs::File) {
    let mut f = tempfile::NamedTempFile::new().expect("a temp file");
    f.write_all(&vec![0u8; before]).expect("written");
    f.write_all(bytes).expect("written");
    f.flush().expect("flushed");
    let read = std::fs::File::open(f.path()).expect("opened");
    (f, read)
}

#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
#[test]
fn there_is_no_loader_on_this_target() {
    let (_keep, file) = file_with(65_536, &[0xC3; 64]);
    assert!(matches!(
        Mapped::map(&file, 65_536, 64),
        Err(MapError::Unsupported)
    ));
    assert!(matches!(
        Mapped::map(&file, 1, 0),
        Err(MapError::Unsupported)
    ));
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod on_linux {
    use super::*;
    use crate::format::{op, Entry, Exit, Section, Site, Ty};

    // Three entries of the entry convention,
    // `extern "C" fn(ctx: *mut Context, args: *const u64, ret: *mut u64) -> u32`:
    //
    //   add:  *ret = args[0] + args[1]; return 0
    //   mark: ctx.site = 7; ctx.left = args[0]; return 1
    //   tick: ctx.steps_left -= 1; *ret = ctx.steps_left; return 0
    //
    // `Context`: steps_left at 0, site at 16, left at 24.

    /// System V: ctx in rdi, args in rsi, ret in rdx, the status in eax.
    #[cfg(target_arch = "x86_64")]
    #[rustfmt::skip]
    mod code {
        pub const ADD: &[u8] = &[
            0x48, 0x8B, 0x06,                         // mov rax, [rsi]
            0x48, 0x03, 0x46, 0x08,                   // add rax, [rsi+8]
            0x48, 0x89, 0x02,                         // mov [rdx], rax
            0x31, 0xC0,                               // xor eax, eax
            0xC3,                                     // ret
        ];
        pub const MARK: &[u8] = &[
            0xC7, 0x47, 0x10, 0x07, 0x00, 0x00, 0x00, // mov dword [rdi+16], 7
            0x48, 0x8B, 0x06,                         // mov rax, [rsi]
            0x48, 0x89, 0x47, 0x18,                   // mov [rdi+24], rax
            0xB8, 0x01, 0x00, 0x00, 0x00,             // mov eax, 1
            0xC3,                                     // ret
        ];
        pub const TICK: &[u8] = &[
            0x48, 0x8B, 0x07,                         // mov rax, [rdi]
            0x48, 0x83, 0xE8, 0x01,                   // sub rax, 1
            0x48, 0x89, 0x07,                         // mov [rdi], rax
            0x48, 0x89, 0x02,                         // mov [rdx], rax
            0x31, 0xC0,                               // xor eax, eax
            0xC3,                                     // ret
        ];
    }

    /// AAPCS64: ctx in x0, args in x1, ret in x2, the status in w0. Each
    /// instruction is one 32-bit word, little-endian.
    #[cfg(target_arch = "aarch64")]
    #[rustfmt::skip]
    mod code {
        pub const ADD: &[u8] = &[
            0x28, 0x00, 0x40, 0xF9, // ldr x8, [x1]
            0x29, 0x04, 0x40, 0xF9, // ldr x9, [x1, #8]
            0x08, 0x01, 0x09, 0x8B, // add x8, x8, x9
            0x48, 0x00, 0x00, 0xF9, // str x8, [x2]
            0x00, 0x00, 0x80, 0x52, // mov w0, #0
            0xC0, 0x03, 0x5F, 0xD6, // ret
        ];
        pub const MARK: &[u8] = &[
            0xE8, 0x00, 0x80, 0x52, // mov w8, #7
            0x08, 0x10, 0x00, 0xB9, // str w8, [x0, #16]
            0x29, 0x00, 0x40, 0xF9, // ldr x9, [x1]
            0x09, 0x0C, 0x00, 0xF9, // str x9, [x0, #24]
            0x20, 0x00, 0x80, 0x52, // mov w0, #1
            0xC0, 0x03, 0x5F, 0xD6, // ret
        ];
        pub const TICK: &[u8] = &[
            0x08, 0x00, 0x40, 0xF9, // ldr x8, [x0]
            0x08, 0x05, 0x00, 0xD1, // sub x8, x8, #1
            0x08, 0x00, 0x00, 0xF9, // str x8, [x0]
            0x48, 0x00, 0x00, 0xF9, // str x8, [x2]
            0x00, 0x00, 0x80, 0x52, // mov w0, #0
            0xC0, 0x03, 0x5F, 0xD6, // ret
        ];
    }

    /// The three entries as a section: entry 0 is `add`, 1 `mark`, 2
    /// `tick`.
    fn section() -> Section {
        let code = [code::ADD, code::MARK, code::TICK].concat();
        let mark = code::ADD.len() as u32;
        let tick = mark + code::MARK.len() as u32;
        Section {
            arch: Arch::host().expect("a target with a loader"),
            module: [9; 32],
            code,
            entries: vec![
                Entry {
                    func: 1,
                    offset: 0,
                    args: vec![Ty::Int, Ty::Int],
                    ret: Ty::Int,
                },
                Entry {
                    func: 2,
                    offset: mark,
                    args: vec![Ty::Int],
                    ret: Ty::Null,
                },
                Entry {
                    func: 3,
                    offset: tick,
                    args: vec![],
                    ret: Ty::Int,
                },
            ],
            sites: vec![Site {
                start: 0,
                end: 1,
                line: 1,
                col: 1,
                kind: Exit::Operator,
                op: op::ADD,
                left: Ty::Int,
                right: Ty::Int,
                in_loop: false,
            }],
        }
    }

    /// The section in a file, at the offset a program keeps one, with
    /// its body as read back and its code mapped.
    struct Loaded {
        _keep: tempfile::NamedTempFile,
        body: Vec<u8>,
        tables: Tables,
        mapped: Mapped,
    }

    fn loaded() -> Loaded {
        let body = section().to_bytes().expect("a section");
        let (keep, file) = file_with(65_536, &body);
        let tables = Tables::parse(&body).expect("its tables");
        let mapped = Mapped::map(&file, 65_536, tables.code_len as usize).expect("mapped");
        Loaded {
            _keep: keep,
            body,
            tables,
            mapped,
        }
    }

    fn armed() -> Code {
        let l = loaded();
        let code = &l.body[..l.tables.code_len as usize];
        // SAFETY: the file is this test's own and nothing writes it
        // after `file_with`; the code is the three functions above, each
        // of the entry convention and reading as many slots as its
        // entry says.
        unsafe { l.mapped.arm(code, &l.tables) }.expect("armed")
    }

    #[test]
    fn mapped_armed_and_called() {
        let code = armed();
        assert_eq!((code.len(), code.is_empty()), (3, false));

        let mut ctx = Context::new(100, 10);
        assert_eq!(code.call(0, &mut ctx, &[40, 2]), Some((0, 42)));
        assert_eq!(
            code.call(0, &mut ctx, &[u64::MAX, 2]),
            Some((0, 1)),
            "the sum wraps: these are raw slots"
        );
        assert_eq!(ctx, Context::new(100, 10), "`add` leaves the context alone");

        assert_eq!(code.call(1, &mut ctx, &[0xDEAD_BEEF]), Some((1, 0)));
        assert_eq!((ctx.site, ctx.left), (7, 0xDEAD_BEEF));

        let mut ctx = Context::new(100, 10);
        assert_eq!(code.call(2, &mut ctx, &[]), Some((0, 99)));
        assert_eq!(code.call(2, &mut ctx, &[]), Some((0, 98)));
        assert_eq!(ctx.steps_left, 98);
    }

    #[test]
    fn a_call_with_the_wrong_number_of_slots_runs_nothing() {
        let code = armed();
        let mut ctx = Context::new(100, 10);
        for (entry, args) in [
            (0, &[][..]),
            (0, &[1][..]),
            (0, &[1, 2, 3][..]),
            (1, &[][..]),
            (1, &[1, 2][..]),
            (2, &[1][..]),
        ] {
            assert_eq!(code.call(entry, &mut ctx, args), None, "entry {entry}");
        }
        assert_eq!(ctx, Context::new(100, 10));
    }

    #[test]
    fn a_call_of_no_entry_runs_nothing() {
        let code = armed();
        let mut ctx = Context::new(100, 10);
        for entry in [3, 4, 99, usize::MAX] {
            assert_eq!(code.call(entry, &mut ctx, &[]), None);
            assert_eq!(code.call(entry, &mut ctx, &[1, 2]), None);
        }
        assert_eq!(ctx, Context::new(100, 10));
    }

    /// `arm` refuses, with `said`, what `change` made of the code and
    /// the tables.
    fn arm_refuses(change: impl FnOnce(&mut Vec<u8>, &mut Tables), said: &str) {
        let l = loaded();
        let mut code = l.body[..l.tables.code_len as usize].to_vec();
        let mut tables = l.tables.clone();
        change(&mut code, &mut tables);
        // SAFETY: as in `armed`; and every case here is refused before
        // anything could be called.
        match unsafe { l.mapped.arm(&code, &tables) } {
            Err(MapError::Mismatch(what)) => assert_eq!(what, said),
            Err(e) => panic!("refused with `{e}`, not `{said}`"),
            Ok(_) => panic!("armed, not refused with `{said}`"),
        }
    }

    #[test]
    fn arm_refuses_code_for_another_machine() {
        arm_refuses(
            |_, t| {
                t.arch = match t.arch {
                    Arch::X86_64 => Arch::Aarch64,
                    Arch::Aarch64 => Arch::X86_64,
                }
            },
            "the code is for another machine",
        );
    }

    #[test]
    fn arm_refuses_code_that_is_not_what_the_tables_and_the_mapping_hold() {
        arm_refuses(
            |_, t| t.code_len += 4,
            "the code is not as long as its tables say",
        );
        arm_refuses(
            |c, _| c.truncate(c.len() - 4),
            "the code is not as long as its tables say",
        );
        // As long as the tables say, and longer than what was mapped.
        arm_refuses(
            |c, t| {
                c.extend_from_slice(&[0; 4]);
                t.code_len += 4;
            },
            "the mapping is not as long as the code",
        );
        // The right length, and one byte that is not the file's.
        for at in [0, 7, 30] {
            arm_refuses(
                |c, _| c[at] ^= 0x40,
                "the mapping does not hold the code that was read",
            );
        }
    }

    #[test]
    fn arm_refuses_an_entry_that_is_not_in_the_code() {
        arm_refuses(
            |_, t| t.entries[2].offset = t.code_len,
            "an entry lies outside the code",
        );
        arm_refuses(
            |_, t| t.entries[0].offset = u32::MAX,
            "an entry lies outside the code",
        );
        arm_refuses(
            |_, t| t.entries[1].args = vec![Ty::Int; 9],
            "an entry has too many arguments",
        );
        arm_refuses(
            |_, t| t.entries[1].args = vec![Ty::Int; 300],
            "an entry has too many arguments",
        );
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn arm_refuses_an_entry_inside_an_instruction() {
        for offset in [1, 2, 3, 6] {
            arm_refuses(
                |_, t| t.entries[0].offset = offset,
                "an entry is not on an instruction boundary",
            );
        }
    }

    #[test]
    fn map_refuses_what_is_not_a_page_inside_the_file() {
        let (_keep, file) = file_with(65_536, &[0xC3; 100]);
        assert!(matches!(
            Mapped::map(&file, 65_536, 0),
            Err(MapError::Empty)
        ));
        for offset in [1, 8, 4095, 65_537] {
            match Mapped::map(&file, offset, 16) {
                Err(MapError::NotPageAligned { offset: o, page }) => {
                    assert_eq!(o, offset);
                    assert!(page >= 4096 && page.is_power_of_two());
                }
                other => panic!("offset {offset}: {:?}", other.map(|m| m.len())),
            }
        }
        // One byte past the end, a range that starts past the end, and
        // a length that overflows the offset.
        for (offset, len) in [
            (65_536, 101),
            (131_072, 1),
            (1 << 40, 16),
            (65_536, usize::MAX),
        ] {
            match Mapped::map(&file, offset, len) {
                Err(MapError::PastEnd {
                    offset: o,
                    len: l,
                    file_len,
                }) => assert_eq!((o, l, file_len), (offset, len as u64, 65_636)),
                other => panic!("{offset}+{len}: {:?}", other.map(|m| m.len())),
            }
        }
        // To the file's last byte is inside it.
        let m = Mapped::map(&file, 65_536, 100).expect("mapped");
        assert_eq!((m.len(), m.is_empty()), (100, false));
    }

    #[test]
    fn a_refusal_of_the_system_is_reported_with_its_error() {
        // A file opened for writing only cannot be mapped for reading.
        let keep = tempfile::NamedTempFile::new().expect("a temp file");
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(keep.path())
            .expect("opened");
        file.set_len(65_536).expect("sized");
        match Mapped::map(&file, 0, 4096) {
            Err(e @ MapError::Refused(_)) => {
                assert!(std::error::Error::source(&e).is_some());
                assert!(e
                    .to_string()
                    .starts_with("the system refused the mapping: "));
            }
            other => panic!("{:?}", other.map(|m| m.len())),
        }
    }
}
