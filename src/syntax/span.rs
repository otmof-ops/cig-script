// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Source positions. Every token, AST node and diagnostic carries one.

/// A half-open byte range `[start, end)` in a source file, plus the
/// 1-based line and column of `start` so diagnostics never have to
/// recompute them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub line: u32,
    pub col: u32,
}

impl Span {
    pub fn new(start: usize, end: usize, line: u32, col: u32) -> Self {
        Self {
            start,
            end,
            line,
            col,
        }
    }

    /// The smallest span covering both `self` and `other`.
    pub fn to(self, other: Span) -> Span {
        let (first, last) = if self.start <= other.start {
            (self, other)
        } else {
            (other, self)
        };
        Span {
            start: first.start,
            end: first.end.max(last.end),
            line: first.line,
            col: first.col,
        }
    }
}
