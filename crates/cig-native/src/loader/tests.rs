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
    use crate::format::{op, Body, Entry, Exit, Rule, Section, Site, Ty};
    use crate::{Host, Stop};

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
            bodies: vec![],
            rule: Rule::None,
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

    // ----- bodies, entered on both architectures ----------------------

    // Two bodies of the body signature,
    // `extern "C" fn(ctx: *mut Context, at: u32) -> u64`:
    //
    //   run_through: calls ctx.helpers->run(ctx, 10, 20) and returns
    //                its result unchanged. Context offsets: helpers at
    //                48, Helpers::run at offset 0 of Helpers.
    //   const_stop:  returns the bits of Stop::At(42) = 42.

    /// System V: ctx in rdi, at in esi, result in rax.
    #[cfg(target_arch = "x86_64")]
    #[rustfmt::skip]
    mod body_code {
        pub const RUN_THROUGH: &[u8] = &[
            0x55,                               // push rbp
            0x48, 0x89, 0xE5,                   // mov rbp, rsp
            0x48, 0x8B, 0x47, 0x30,             // mov rax, [rdi+48]      ; helpers
            0x48, 0x8B, 0x00,                   // mov rax, [rax]         ; helpers->run
            0xBE, 0x0A, 0x00, 0x00, 0x00,       // mov esi, 10
            0xBA, 0x14, 0x00, 0x00, 0x00,       // mov edx, 20
            0xFF, 0xD0,                         // call rax
            0x5D,                               // pop rbp
            0xC3,                               // ret
        ];
        pub const CONST_STOP: &[u8] = &[
            0xB8, 0x2A, 0x00, 0x00, 0x00,       // mov eax, 42
            0xC3,                               // ret
        ];
        pub const AGAIN_THROUGH: &[u8] = &[
            0x55,                               // push rbp
            0x48, 0x89, 0xE5,                   // mov rbp, rsp
            0x48, 0x8B, 0x47, 0x30,             // mov rax, [rdi+48]      ; helpers
            0x48, 0x8B, 0x40, 0x10,             // mov rax, [rax+16]      ; helpers->again
            0xBE, 0x07, 0x00, 0x00, 0x00,       // mov esi, 7
            0xFF, 0xD0,                         // call rax
            0x5D,                               // pop rbp
            0xC3,                               // ret
        ];
    }

    /// AAPCS64: ctx in x0, at in w1, result in x0. Each instruction is
    /// one 32-bit word, little-endian.
    #[cfg(target_arch = "aarch64")]
    #[rustfmt::skip]
    mod body_code {
        pub const RUN_THROUGH: &[u8] = &[
            0xFD, 0x7B, 0xBF, 0xA9, // stp x29, x30, [sp, #-16]!
            0xFD, 0x03, 0x00, 0x91, // mov x29, sp
            0x08, 0x18, 0x40, 0xF9, // ldr x8, [x0, #48]
            0x08, 0x01, 0x40, 0xF9, // ldr x8, [x8]
            0x41, 0x01, 0x80, 0x52, // mov w1, #10
            0x82, 0x02, 0x80, 0x52, // mov w2, #20
            0x00, 0x01, 0x3F, 0xD6, // blr x8
            0xFD, 0x7B, 0xC1, 0xA8, // ldp x29, x30, [sp], #16
            0xC0, 0x03, 0x5F, 0xD6, // ret
        ];
        pub const CONST_STOP: &[u8] = &[
            0x40, 0x05, 0x80, 0xD2, // mov x0, #42
            0xC0, 0x03, 0x5F, 0xD6, // ret
        ];
        pub const AGAIN_THROUGH: &[u8] = &[
            0xFD, 0x7B, 0xBF, 0xA9, // stp x29, x30, [sp, #-16]!
            0xFD, 0x03, 0x00, 0x91, // mov x29, sp
            0x08, 0x18, 0x40, 0xF9, // ldr x8, [x0, #48]
            0x08, 0x09, 0x40, 0xF9, // ldr x8, [x8, #16]
            0xE1, 0x00, 0x80, 0x52, // mov w1, #7
            0x00, 0x01, 0x3F, 0xD6, // blr x8
            0xFD, 0x7B, 0xC1, 0xA8, // ldp x29, x30, [sp], #16
            0xC0, 0x03, 0x5F, 0xD6, // ret
        ];
    }

    /// `section()` plus two bodies after its entries: body 0 is
    /// `run_through`, body 1 is `const_stop`.
    fn section_with_bodies() -> Section {
        let mut s = section();
        let entries_len = code::ADD.len() + code::MARK.len() + code::TICK.len();
        let run_through_offset = entries_len as u32;
        let const_stop_offset = run_through_offset + body_code::RUN_THROUGH.len() as u32;
        s.code.extend_from_slice(body_code::RUN_THROUGH);
        s.code.extend_from_slice(body_code::CONST_STOP);
        s.bodies = vec![
            Body {
                func: 1,
                offset: run_through_offset,
                resumes: vec![0, 1, 5],
            },
            Body {
                func: 2,
                offset: const_stop_offset,
                resumes: vec![0],
            },
        ];
        s.rule = Rule::Every;
        s
    }

    fn armed_with_bodies() -> Code {
        let body = section_with_bodies().to_bytes().expect("a section");
        let (_keep, file) = file_with(65_536, &body);
        let tables = Tables::parse(&body).expect("its tables");
        let mapped = Mapped::map(&file, 65_536, tables.code_len as usize).expect("mapped");
        let code = &body[..tables.code_len as usize];
        // SAFETY: as in `armed`.
        let armed = unsafe { mapped.arm(code, &tables) }.expect("armed");
        // Hold the file alive for the length of the test by leaking
        // the handle: the mapping needs the file only while it reads,
        // and once `arm` has returned it is independent.
        std::mem::forget(_keep);
        armed
    }

    /// A host that records how it was called and returns a scripted
    /// list of stops from `run`.
    struct Recording {
        /// Stops `run` returns, one per call, in order. Panics past
        /// the end.
        run_returns: Vec<Stop>,
        /// Arguments `run` was given, in order.
        run_calls: Vec<(u32, u32)>,
        /// `(reg, list)` `list` returns.
        list_returns: Vec<(u32, bool)>,
        /// The pcs `again` was given, in order: it answers each with
        /// `At(pc + 1)`.
        again_calls: Vec<u32>,
    }

    impl Host for Recording {
        fn run(&mut self, _ctx: &mut Context, from: u32, to: u32) -> Stop {
            self.run_calls.push((from, to));
            assert!(
                !self.run_returns.is_empty(),
                "host.run called more times than scripted",
            );
            self.run_returns.remove(0)
        }
        fn list(&mut self, _ctx: &mut Context, reg: u32) -> bool {
            let (want, giving) = *self
                .list_returns
                .first()
                .expect("host.list called more times than scripted");
            assert_eq!(want, reg);
            self.list_returns.remove(0);
            giving
        }
        fn again(&mut self, _ctx: &mut Context, pc: u32) -> Stop {
            self.again_calls.push(pc);
            Stop::At(pc + 1)
        }
    }

    #[test]
    fn enter_calls_run_through_the_context_and_returns_its_bits() {
        let code = armed_with_bodies();
        assert_eq!(code.bodies(), 2);
        let mut host = Recording {
            run_returns: vec![Stop::At(17)],
            run_calls: vec![],
            list_returns: vec![],
            again_calls: vec![],
        };
        let mut ctx = Context::new(100, 10);
        let stop = code.enter(0, 0, &mut ctx, &mut host);
        assert_eq!(stop, Some(Stop::At(17)));
        assert_eq!(host.run_calls, vec![(10, 20)]);
        // `enter` clears the two pointers after the body returns.
        assert!(ctx.helpers.is_null() && ctx.host.is_null());
    }

    #[test]
    fn enter_calls_again_through_the_context_and_returns_its_bits() {
        // One body after the entries: it calls
        // `ctx.helpers->again(ctx, 7)` and returns what that returns.
        let mut section = section();
        let offset = (code::ADD.len() + code::MARK.len() + code::TICK.len()) as u32;
        section.code.extend_from_slice(body_code::AGAIN_THROUGH);
        section.bodies = vec![Body {
            func: 1,
            offset,
            resumes: vec![0],
        }];
        section.rule = Rule::Loops;
        let body = section.to_bytes().expect("a section");
        let (keep, file) = file_with(65_536, &body);
        let tables = Tables::parse(&body).expect("its tables");
        let mapped = Mapped::map(&file, 65_536, tables.code_len as usize).expect("mapped");
        let bytes = &body[..tables.code_len as usize];
        // SAFETY: as in `armed`.
        let code = unsafe { mapped.arm(bytes, &tables) }.expect("armed");
        drop(keep);
        let mut host = Recording {
            run_returns: vec![],
            run_calls: vec![],
            list_returns: vec![],
            again_calls: vec![],
        };
        let mut ctx = Context::new(100, 10);
        assert_eq!(code.enter(0, 0, &mut ctx, &mut host), Some(Stop::At(8)));
        assert_eq!(host.again_calls, vec![7]);
        assert!(host.run_calls.is_empty());
    }

    #[test]
    fn enter_a_body_that_returns_a_constant_stop() {
        let code = armed_with_bodies();
        let mut host = Recording {
            run_returns: vec![],
            run_calls: vec![],
            list_returns: vec![],
            again_calls: vec![],
        };
        let mut ctx = Context::new(50, 5);
        let stop = code.enter(1, 0, &mut ctx, &mut host);
        assert_eq!(stop, Some(Stop::At(42)));
        assert!(host.run_calls.is_empty());
    }

    #[test]
    fn enter_passes_raised_through_unchanged() {
        let code = armed_with_bodies();
        let mut host = Recording {
            run_returns: vec![Stop::Raised(13)],
            run_calls: vec![],
            list_returns: vec![],
            again_calls: vec![],
        };
        let mut ctx = Context::new(100, 10);
        let stop = code.enter(0, 0, &mut ctx, &mut host);
        assert_eq!(stop, Some(Stop::Raised(13)));
    }

    #[test]
    fn enter_returns_none_for_a_body_or_a_resume_point_it_does_not_have() {
        let code = armed_with_bodies();
        let mut host = Recording {
            run_returns: vec![],
            run_calls: vec![],
            list_returns: vec![],
            again_calls: vec![],
        };
        let mut ctx = Context::new(100, 10);
        for body in [2usize, 99, usize::MAX] {
            assert_eq!(code.enter(body, 0, &mut ctx, &mut host), None);
        }
        // Body 1 has only resume point 0.
        assert_eq!(code.enter(1, 1, &mut ctx, &mut host), None);
        assert_eq!(code.enter(1, 99, &mut ctx, &mut host), None);
        // Body 0 has three resume points (pcs 0, 1 and 5): a fourth is
        // not one, and a pc is not an index.
        for at in [3u32, 4, 5, 6, u32::MAX] {
            assert_eq!(code.enter(0, at, &mut ctx, &mut host), None);
        }
        // Nothing was run.
        assert!(host.run_calls.is_empty());
        // Each of its three is entered.
        for at in 0..3u32 {
            host.run_returns = vec![Stop::At(17)];
            assert_eq!(
                code.enter(0, at, &mut ctx, &mut host),
                Some(Stop::At(17)),
                "{at}"
            );
        }
        assert_eq!(host.run_calls.len(), 3);
    }

    #[test]
    fn a_panic_in_host_run_is_caught_and_resumed_after_the_body_returns() {
        /// A host that panics from `run`, and counts how many times it
        /// was asked. The body is expected to see `Stop::Raised(from)`
        /// and return it; `enter` then resumes the panic.
        struct Panicky {
            calls: std::cell::Cell<u32>,
        }
        impl Host for Panicky {
            fn run(&mut self, _ctx: &mut Context, _from: u32, _to: u32) -> Stop {
                self.calls.set(self.calls.get() + 1);
                panic!("host run blew up");
            }
            fn list(&mut self, _ctx: &mut Context, _reg: u32) -> bool {
                unreachable!("list is not called in this test");
            }
            fn again(&mut self, _ctx: &mut Context, _pc: u32) -> Stop {
                unreachable!("again is not called in this test");
            }
        }
        let code = armed_with_bodies();
        let mut host = Panicky {
            calls: std::cell::Cell::new(0),
        };
        let mut ctx = Context::new(100, 10);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            code.enter(0, 0, &mut ctx, &mut host)
        }));
        let payload = result.expect_err("the panic was resumed");
        let text = payload
            .downcast_ref::<&'static str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(|s| s.as_str()))
            .unwrap_or("<not a string>");
        assert_eq!(text, "host run blew up");
        assert_eq!(host.calls.get(), 1, "host.run was reached once");
        // The pointers are cleared even on the panic path.
        assert!(ctx.helpers.is_null() && ctx.host.is_null());
    }

    #[test]
    fn arm_refuses_a_body_outside_the_code() {
        // `arm` checks each body's offset too.
        let body = section_with_bodies().to_bytes().expect("written");
        let (_keep, file) = file_with(65_536, &body);
        let mut tables = Tables::parse(&body).expect("its tables");
        let mapped = Mapped::map(&file, 65_536, tables.code_len as usize).expect("mapped");
        let code_bytes = body[..tables.code_len as usize].to_vec();
        tables.bodies[0].offset = tables.code_len;
        match unsafe { mapped.arm(&code_bytes, &tables) } {
            Err(MapError::Mismatch(what)) => assert_eq!(what, "a body lies outside the code"),
            Err(e) => panic!("another error: {e}"),
            Ok(_) => panic!("armed"),
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn arm_refuses_a_body_inside_an_instruction() {
        let body = section_with_bodies().to_bytes().expect("written");
        let (_keep, file) = file_with(65_536, &body);
        let mut tables = Tables::parse(&body).expect("its tables");
        let mapped = Mapped::map(&file, 65_536, tables.code_len as usize).expect("mapped");
        let code_bytes = body[..tables.code_len as usize].to_vec();
        tables.bodies[0].offset += 1;
        match unsafe { mapped.arm(&code_bytes, &tables) } {
            Err(MapError::Mismatch(what)) => {
                assert_eq!(what, "a body is not on an instruction boundary")
            }
            Err(e) => panic!("another error: {e}"),
            Ok(_) => panic!("armed"),
        }
    }

    // ----- end bodies tests -------------------------------------------

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
