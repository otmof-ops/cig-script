// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! A minimal read-eval-print loop. Lines that open a block keep reading
//! until the braces balance.

use super::{exit, Ctx};
use crate::burn::{Kernel, Mode};
use crate::interp::Interp;
use crate::syntax::parse;
use std::io::{self, BufRead, Write};

pub fn repl(ctx: &Ctx) -> i32 {
    let mut interp = Interp::new(Kernel::ephemeral(Mode::Run));
    interp.set_args(&[]);
    eprintln!(
        "cig {} repl. Nothing real happens unless you burn. Ctrl-D to leave.",
        crate::VERSION
    );
    let stdin = io::stdin();
    let mut buffer = String::new();
    let mut depth: i32 = 0;
    loop {
        let prompt = if depth > 0 { "...  " } else { "cig> " };
        eprint!("{prompt}");
        let _ = io::stderr().flush();
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => {
                eprintln!();
                return exit::OK;
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("{} {e}", ctx.red("error:"));
                return exit::USAGE;
            }
        }
        depth += brace_delta(&line);
        buffer.push_str(&line);
        if depth > 0 {
            continue;
        }
        depth = 0;
        let src = std::mem::take(&mut buffer);
        if src.trim().is_empty() {
            continue;
        }
        match parse(&src) {
            Ok(program) => match interp.eval_program(&program) {
                Ok(Some(v)) => outln!("{}", v.repr()),
                Ok(None) => {}
                Err(d) => eprint!("{}", d.render(None, Some(&src))),
            },
            Err(d) => eprint!("{}", d.render(None, Some(&src))),
        }
        if let Some(code) = interp.exit_requested {
            return code;
        }
        let _ = interp.out.flush();
    }
}

/// Net brace count of a line, ignoring braces inside string literals and
/// comments.
fn brace_delta(line: &str) -> i32 {
    let mut delta = 0;
    let mut in_str: Option<char> = None;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match in_str {
            Some(q) => {
                if c == '\\' {
                    chars.next();
                } else if c == q {
                    in_str = None;
                }
            }
            None => match c {
                '"' | '\'' => in_str = Some(c),
                '#' => break,
                '{' => delta += 1,
                '}' => delta -= 1,
                _ => {}
            },
        }
    }
    delta
}
