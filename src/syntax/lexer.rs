// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Hand-written lexer. Produces a flat token list; newlines are tokens
//! because statements are newline-terminated.

use super::span::Span;
use super::token::{StrPart, Token, TokenKind};
use crate::diagnostics::{lex, Diagnostic};

pub struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    line: u32,
    col: u32,
    tokens: Vec<Token>,
}

pub fn tokenize(src: &str) -> Result<Vec<Token>, Diagnostic> {
    Lexer::new(src).run()
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Self {
            src,
            bytes: src.as_bytes(),
            pos: 0,
            line: 1,
            col: 1,
            tokens: Vec::new(),
        }
    }

    /// Lex with a positional offset so interpolated expressions report
    /// their real location in the enclosing file.
    pub fn with_origin(src: &'a str, origin: Span) -> Self {
        let mut lx = Self::new(src);
        lx.line = origin.line.max(1);
        lx.col = origin.col.max(1);
        lx
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn peek_at(&self, off: usize) -> Option<u8> {
        self.bytes.get(self.pos + off).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        if b == b'\n' {
            self.line += 1;
            self.col = 1;
        } else if (b & 0xC0) != 0x80 {
            // Count characters, not continuation bytes.
            self.col += 1;
        }
        Some(b)
    }

    fn span_from(&self, start: usize, line: u32, col: u32) -> Span {
        Span::new(start, self.pos, line, col)
    }

    fn push(&mut self, kind: TokenKind, span: Span) {
        self.tokens.push(Token { kind, span });
    }

    pub fn run(mut self) -> Result<Vec<Token>, Diagnostic> {
        // A byte-order mark is not part of the script; some editors on
        // Windows put one at the front of every file.
        if self.pos == 0 && self.src.starts_with('\u{feff}') {
            self.pos = '\u{feff}'.len_utf8();
        }
        while let Some(b) = self.peek() {
            let start = self.pos;
            let (line, col) = (self.line, self.col);
            match b {
                b' ' | b'\t' | b'\r' => {
                    self.bump();
                }
                b'\n' => {
                    self.bump();
                    // Collapse runs of blank lines into one terminator.
                    if !matches!(
                        self.tokens.last(),
                        Some(Token {
                            kind: TokenKind::Newline,
                            ..
                        }) | None
                    ) {
                        self.push(TokenKind::Newline, self.span_from(start, line, col));
                    }
                }
                b'#' => {
                    while let Some(c) = self.peek() {
                        if c == b'\n' {
                            break;
                        }
                        self.bump();
                    }
                }
                b'0'..=b'9' => self.number()?,
                b'"' => self.string()?,
                b'\'' => self.raw_string()?,
                b'A'..=b'Z' | b'a'..=b'z' | b'_' => self.ident(),
                _ => self.punct()?,
            }
        }
        let end = self.span_from(self.pos, self.line, self.col);
        if !matches!(
            self.tokens.last(),
            Some(Token {
                kind: TokenKind::Newline,
                ..
            }) | None
        ) {
            self.push(TokenKind::Newline, end);
        }
        self.push(TokenKind::Eof, end);
        Ok(self.tokens)
    }

    fn ident(&mut self) {
        let start = self.pos;
        let (line, col) = (self.line, self.col);
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == b'_' {
                self.bump();
            } else {
                break;
            }
        }
        let word = &self.src[start..self.pos];
        let kind = TokenKind::keyword(word).unwrap_or_else(|| TokenKind::Ident(word.to_string()));
        self.push(kind, self.span_from(start, line, col));
    }

    fn number(&mut self) -> Result<(), Diagnostic> {
        let start = self.pos;
        let (line, col) = (self.line, self.col);
        let mut is_float = false;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || c == b'_' {
                self.bump();
            } else {
                break;
            }
        }
        // A `.` followed by a digit is a fraction; `..` is a range.
        if self.peek() == Some(b'.') && self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) {
            is_float = true;
            self.bump();
            while let Some(c) = self.peek() {
                if c.is_ascii_digit() || c == b'_' {
                    self.bump();
                } else {
                    break;
                }
            }
        }
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            let save = (self.pos, self.line, self.col);
            self.bump();
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.bump();
            }
            if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                is_float = true;
                while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    self.bump();
                }
            } else {
                (self.pos, self.line, self.col) = save;
            }
        }
        let text: String = self.src[start..self.pos]
            .chars()
            .filter(|c| *c != '_')
            .collect();
        let span = self.span_from(start, line, col);
        if is_float {
            let v: f64 = text
                .parse()
                .map_err(|_| lex(format!("malformed float literal `{text}`"), span).code("E104"))?;
            self.push(TokenKind::Float(v), span);
        } else {
            match text.parse::<i64>() {
                Ok(v) => self.push(TokenKind::Int(v), span),
                // Only meaningful as `-9223372036854775808`; the parser folds
                // the sign in, and rejects it anywhere else.
                Err(_) if text == "9223372036854775808" => {
                    self.push(TokenKind::IntMinMagnitude, span)
                }
                Err(_) => {
                    return Err(lex(
                        format!("integer literal `{text}` does not fit in 64 bits"),
                        span,
                    )
                    .code("E104")
                    .with_hint(
                        "the largest int is 9223372036854775807; use a float for bigger magnitudes",
                    ))
                }
            }
        }
        Ok(())
    }

    /// Single-quoted strings are raw: no escapes, no interpolation.
    fn raw_string(&mut self) -> Result<(), Diagnostic> {
        let start = self.pos;
        let (line, col) = (self.line, self.col);
        self.bump();
        let content_start = self.pos;
        loop {
            match self.peek() {
                None => {
                    return Err(lex(
                        "unterminated string literal",
                        self.span_from(start, line, col),
                    )
                    .code("E101"))
                }
                Some(b'\'') => break,
                Some(_) => {
                    self.bump();
                }
            }
        }
        let text = self.src[content_start..self.pos].to_string();
        self.bump();
        self.push(
            TokenKind::Str(vec![StrPart::Lit(text)]),
            self.span_from(start, line, col),
        );
        Ok(())
    }

    /// Double-quoted strings support escapes and `${expr}` interpolation.
    fn string(&mut self) -> Result<(), Diagnostic> {
        let start = self.pos;
        let (line, col) = (self.line, self.col);
        self.bump();
        let mut parts: Vec<StrPart> = Vec::new();
        let mut buf = String::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(lex(
                    "unterminated string literal",
                    self.span_from(start, line, col),
                )
                .code("E101"));
            };
            match c {
                b'"' => {
                    self.bump();
                    break;
                }
                b'\n' => {
                    // A string ends on the line it opened; the closing quote
                    // on a later line is the E101 the registry describes.
                    return Err(lex(
                        "unterminated string literal: the line ends before the closing quote",
                        self.span_from(start, line, col),
                    )
                    .code("E101")
                    .with_hint(
                        "close the string on this line; write \\n for a line break inside it",
                    ));
                }
                b'\\' => {
                    let esc_start = self.pos;
                    let (el, ec) = (self.line, self.col);
                    self.bump();
                    let Some(e) = self.bump() else {
                        return Err(lex(
                            "unterminated escape sequence",
                            self.span_from(esc_start, el, ec),
                        )
                        .code("E102"));
                    };
                    match e {
                        b'n' => buf.push('\n'),
                        b't' => buf.push('\t'),
                        b'r' => buf.push('\r'),
                        b'0' => buf.push('\0'),
                        b'\\' => buf.push('\\'),
                        b'"' => buf.push('"'),
                        b'$' => buf.push('$'),
                        b'u' => {
                            if self.bump() != Some(b'{') {
                                return Err(lex(
                                    "expected `{` after `\\u`",
                                    self.span_from(esc_start, el, ec),
                                )
                                .code("E102"));
                            }
                            let hex_start = self.pos;
                            while self.peek().is_some_and(|h| h.is_ascii_hexdigit()) {
                                self.bump();
                            }
                            let hex = &self.src[hex_start..self.pos];
                            if self.bump() != Some(b'}') || hex.is_empty() {
                                return Err(lex(
                                    "malformed `\\u{...}` escape",
                                    self.span_from(esc_start, el, ec),
                                )
                                .code("E102"));
                            }
                            let cp = u32::from_str_radix(hex, 16).ok().and_then(char::from_u32);
                            match cp {
                                Some(ch) => buf.push(ch),
                                None => {
                                    return Err(lex(
                                        format!("`\\u{{{hex}}}` is not a valid character"),
                                        self.span_from(esc_start, el, ec),
                                    )
                                    .code("E102"))
                                }
                            }
                        }
                        other => {
                            let hint = if b"dwsbDWSB.[](){}|*+?^".contains(&other) {
                                "regex escapes need a raw string: use 'single quotes', which take no escapes"
                            } else {
                                "valid escapes: \\n \\t \\r \\0 \\\\ \\\" \\$ \\u{hex}"
                            };
                            return Err(lex(
                                format!("unknown escape `\\{}`", other as char),
                                self.span_from(esc_start, el, ec),
                            )
                            .code("E102")
                            .with_hint(hint));
                        }
                    }
                }
                b'$' if self.peek_at(1) == Some(b'{') => {
                    if !buf.is_empty() {
                        parts.push(StrPart::Lit(std::mem::take(&mut buf)));
                    }
                    let interp_start = self.pos;
                    let (il, ic) = (self.line, self.col);
                    self.bump();
                    self.bump();
                    let expr_start = self.pos;
                    let (xl, xc) = (self.line, self.col);
                    let mut depth = 1usize;
                    let mut in_str: Option<u8> = None;
                    loop {
                        let Some(ch) = self.peek() else {
                            return Err(lex(
                                "unterminated `${` interpolation",
                                self.span_from(interp_start, il, ic),
                            )
                            .code("E101"));
                        };
                        match in_str {
                            Some(q) => {
                                if ch == b'\\' {
                                    self.bump();
                                } else if ch == q {
                                    in_str = None;
                                }
                                self.bump();
                            }
                            None => match ch {
                                b'"' | b'\'' => {
                                    in_str = Some(ch);
                                    self.bump();
                                }
                                b'{' => {
                                    depth += 1;
                                    self.bump();
                                }
                                b'}' => {
                                    depth -= 1;
                                    if depth == 0 {
                                        break;
                                    }
                                    self.bump();
                                }
                                _ => {
                                    self.bump();
                                }
                            },
                        }
                    }
                    let src = self.src[expr_start..self.pos].to_string();
                    let span = Span::new(expr_start, self.pos, xl, xc);
                    self.bump(); // closing brace
                    if src.trim().is_empty() {
                        return Err(lex("empty `${}` interpolation", span).code("E201"));
                    }
                    parts.push(StrPart::Interp { src, span });
                }
                _ => {
                    // Copy one full UTF-8 character.
                    let ch_start = self.pos;
                    self.bump();
                    while self.peek().is_some_and(|b| (b & 0xC0) == 0x80) {
                        self.bump();
                    }
                    buf.push_str(&self.src[ch_start..self.pos]);
                }
            }
        }
        if !buf.is_empty() || parts.is_empty() {
            parts.push(StrPart::Lit(buf));
        }
        self.push(TokenKind::Str(parts), self.span_from(start, line, col));
        Ok(())
    }

    fn punct(&mut self) -> Result<(), Diagnostic> {
        let start = self.pos;
        let (line, col) = (self.line, self.col);
        let two = self.src.get(start..start + 2).unwrap_or("");
        let kind2 = match two {
            ".." => Some(TokenKind::DotDot),
            "=>" => Some(TokenKind::FatArrow),
            "==" => Some(TokenKind::EqEq),
            "!=" => Some(TokenKind::NotEq),
            "<=" => Some(TokenKind::LtEq),
            ">=" => Some(TokenKind::GtEq),
            ">>" => Some(TokenKind::Pipe),
            "??" => Some(TokenKind::Coalesce),
            "?." => Some(TokenKind::SafeDot),
            "+=" => Some(TokenKind::PlusAssign),
            "-=" => Some(TokenKind::MinusAssign),
            "*=" => Some(TokenKind::StarAssign),
            "/=" => Some(TokenKind::SlashAssign),
            _ => None,
        };
        if let Some(kind) = kind2 {
            self.bump();
            self.bump();
            self.push(kind, self.span_from(start, line, col));
            return Ok(());
        }
        let c = self.bump().expect("punct called at end of input");
        let kind = match c {
            b'(' => TokenKind::LParen,
            b')' => TokenKind::RParen,
            b'{' => TokenKind::LBrace,
            b'}' => TokenKind::RBrace,
            b'[' => TokenKind::LBracket,
            b']' => TokenKind::RBracket,
            b',' => TokenKind::Comma,
            b':' => TokenKind::Colon,
            b'.' => TokenKind::Dot,
            b';' => TokenKind::Semicolon,
            b'=' => TokenKind::Assign,
            b'+' => TokenKind::Plus,
            b'-' => TokenKind::Minus,
            b'*' => TokenKind::Star,
            b'/' => TokenKind::Slash,
            b'%' => TokenKind::Percent,
            b'<' => TokenKind::Lt,
            b'>' => TokenKind::Gt,
            other => {
                // Consume the rest of a multi-byte character for a clean span.
                while self.peek().is_some_and(|b| (b & 0xC0) == 0x80) {
                    self.bump();
                }
                let text = &self.src[start..self.pos];
                let msg = if other == b'!' {
                    "unexpected `!`; CigScript spells negation `not`".to_string()
                } else if other == b'&' || other == b'|' {
                    format!("unexpected `{text}`; CigScript spells logic `and` / `or`")
                } else {
                    format!("unexpected character `{text}`")
                };
                return Err(lex(msg, self.span_from(start, line, col)).code("E103"));
            }
        };
        self.push(kind, self.span_from(start, line, col));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        tokenize(src).unwrap().into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn lexes_keywords_and_idents() {
        let k = kinds("roll x = 1");
        assert_eq!(
            k,
            vec![
                TokenKind::Roll,
                TokenKind::Ident("x".into()),
                TokenKind::Assign,
                TokenKind::Int(1),
                TokenKind::Newline,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn range_is_not_a_float() {
        let k = kinds("1..5");
        assert_eq!(k[0], TokenKind::Int(1));
        assert_eq!(k[1], TokenKind::DotDot);
        assert_eq!(k[2], TokenKind::Int(5));
        assert_eq!(kinds("1.5")[0], TokenKind::Float(1.5));
        assert_eq!(kinds("1_000")[0], TokenKind::Int(1000));
        assert_eq!(kinds("2e3")[0], TokenKind::Float(2000.0));
    }

    #[test]
    fn strings_escape_and_interpolate() {
        let k = kinds(r#""a\tb ${x + 1} c""#);
        match &k[0] {
            TokenKind::Str(parts) => {
                assert_eq!(parts.len(), 3);
                assert_eq!(parts[0], StrPart::Lit("a\tb ".into()));
                assert!(matches!(&parts[1], StrPart::Interp { src, .. } if src == "x + 1"));
                assert_eq!(parts[2], StrPart::Lit(" c".into()));
            }
            other => panic!("expected string, got {other:?}"),
        }
        assert_eq!(
            kinds(r"'raw \n ${x}'")[0],
            TokenKind::Str(vec![StrPart::Lit(r"raw \n ${x}".into())])
        );
    }

    #[test]
    fn comments_and_blank_lines_collapse() {
        let k = kinds("# shebang-ish\n\n\nroll a = 1 # trailing\n\n\nroll b = 2\n");
        let newlines = k.iter().filter(|t| **t == TokenKind::Newline).count();
        assert_eq!(newlines, 2);
    }

    #[test]
    fn rejects_c_style_operators() {
        let err = tokenize("a && b").unwrap_err();
        assert!(err.message.contains("`and`"));
        let err = tokenize("!a").unwrap_err();
        assert!(err.message.contains("`not`"));
    }

    #[test]
    fn tracks_line_and_column() {
        let toks = tokenize("roll a = 1\n  roll b = 2").unwrap();
        let b = toks
            .iter()
            .find(|t| t.kind == TokenKind::Ident("b".into()))
            .unwrap();
        assert_eq!((b.span.line, b.span.col), (2, 8));
    }
}
