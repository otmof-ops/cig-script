// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Tokens produced by the lexer.

use super::span::Span;

/// A piece of a double-quoted string literal. Interpolations (`${expr}`)
/// carry their source text; the parser turns them into expressions.
#[derive(Clone, Debug, PartialEq)]
pub enum StrPart {
    Lit(String),
    Interp { src: String, span: Span },
}

#[derive(Clone, Debug, PartialEq)]
pub enum TokenKind {
    // Literals and names
    Int(i64),
    /// The magnitude of `i64::MIN`, valid only directly after a unary minus.
    IntMinMagnitude,
    Float(f64),
    Str(Vec<StrPart>),
    Ident(String),

    // Themed keywords
    Roll,
    Stick,
    Pull,
    Pack,
    Snuff,
    Exhale,
    Cough,
    Try,
    Finally,
    Ashtray,
    Burn,
    Unlit,
    Chain,

    // Plain keywords
    If,
    Else,
    While,
    For,
    In,
    Break,
    Continue,
    And,
    Or,
    Not,
    True,
    False,
    Null,

    // Delimiters
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Colon,
    Dot,
    DotDot,
    Semicolon,
    Newline,

    // Assignment
    Assign,
    PlusAssign,
    MinusAssign,
    StarAssign,
    SlashAssign,

    // Operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    EqEq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    Coalesce,
    SafeDot,
    Pipe,
    FatArrow,

    Eof,
}

impl TokenKind {
    /// Human-readable name used in "expected X, found Y" messages.
    pub fn describe(&self) -> String {
        match self {
            TokenKind::Int(v) => format!("integer `{v}`"),
            TokenKind::IntMinMagnitude => "integer `9223372036854775808`".to_string(),
            TokenKind::Float(v) => format!("float `{v}`"),
            TokenKind::Str(_) => "string".to_string(),
            TokenKind::Ident(s) => format!("`{s}`"),
            TokenKind::Newline => "end of line".to_string(),
            TokenKind::Eof => "end of file".to_string(),
            other => format!("`{}`", other.symbol()),
        }
    }

    fn symbol(&self) -> &'static str {
        match self {
            TokenKind::Roll => "roll",
            TokenKind::Stick => "stick",
            TokenKind::Pull => "pull",
            TokenKind::Pack => "pack",
            TokenKind::Snuff => "snuff",
            TokenKind::Exhale => "exhale",
            TokenKind::Cough => "cough",
            TokenKind::Try => "try",
            TokenKind::Ashtray => "ashtray",
            TokenKind::Finally => "finally",
            TokenKind::Burn => "burn",
            TokenKind::Unlit => "unlit",
            TokenKind::Chain => "chain",
            TokenKind::If => "if",
            TokenKind::Else => "else",
            TokenKind::While => "while",
            TokenKind::For => "for",
            TokenKind::In => "in",
            TokenKind::Break => "break",
            TokenKind::Continue => "continue",
            TokenKind::And => "and",
            TokenKind::Or => "or",
            TokenKind::Not => "not",
            TokenKind::True => "true",
            TokenKind::False => "false",
            TokenKind::Null => "null",
            TokenKind::LParen => "(",
            TokenKind::RParen => ")",
            TokenKind::LBrace => "{",
            TokenKind::RBrace => "}",
            TokenKind::LBracket => "[",
            TokenKind::RBracket => "]",
            TokenKind::Comma => ",",
            TokenKind::Colon => ":",
            TokenKind::Dot => ".",
            TokenKind::DotDot => "..",
            TokenKind::Semicolon => ";",
            TokenKind::Assign => "=",
            TokenKind::PlusAssign => "+=",
            TokenKind::MinusAssign => "-=",
            TokenKind::StarAssign => "*=",
            TokenKind::SlashAssign => "/=",
            TokenKind::Plus => "+",
            TokenKind::Minus => "-",
            TokenKind::Star => "*",
            TokenKind::Slash => "/",
            TokenKind::Percent => "%",
            TokenKind::EqEq => "==",
            TokenKind::NotEq => "!=",
            TokenKind::Lt => "<",
            TokenKind::LtEq => "<=",
            TokenKind::Gt => ">",
            TokenKind::GtEq => ">=",
            TokenKind::Coalesce => "??",
            TokenKind::SafeDot => "?.",
            TokenKind::Pipe => ">>",
            TokenKind::FatArrow => "=>",
            _ => "?",
        }
    }

    /// The source text of a keyword token, so keywords can serve as member
    /// names (`e.chain`) and map keys (`{chain: 1}`).
    pub fn keyword_text(&self) -> Option<&'static str> {
        match self {
            TokenKind::Roll
            | TokenKind::Stick
            | TokenKind::Pull
            | TokenKind::Pack
            | TokenKind::Snuff
            | TokenKind::Exhale
            | TokenKind::Cough
            | TokenKind::Try
            | TokenKind::Ashtray
            | TokenKind::Finally
            | TokenKind::Burn
            | TokenKind::Unlit
            | TokenKind::Chain
            | TokenKind::If
            | TokenKind::Else
            | TokenKind::While
            | TokenKind::For
            | TokenKind::In
            | TokenKind::Break
            | TokenKind::Continue
            | TokenKind::And
            | TokenKind::Or
            | TokenKind::Not
            | TokenKind::True
            | TokenKind::False
            | TokenKind::Null => Some(self.symbol()),
            _ => None,
        }
    }

    pub fn keyword(word: &str) -> Option<TokenKind> {
        Some(match word {
            "roll" => TokenKind::Roll,
            "stick" => TokenKind::Stick,
            "pull" => TokenKind::Pull,
            "pack" => TokenKind::Pack,
            "snuff" => TokenKind::Snuff,
            "exhale" => TokenKind::Exhale,
            "cough" => TokenKind::Cough,
            "try" => TokenKind::Try,
            "ashtray" => TokenKind::Ashtray,
            "finally" => TokenKind::Finally,
            "burn" => TokenKind::Burn,
            "unlit" => TokenKind::Unlit,
            "chain" => TokenKind::Chain,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "while" => TokenKind::While,
            "for" => TokenKind::For,
            "in" => TokenKind::In,
            "break" => TokenKind::Break,
            "continue" => TokenKind::Continue,
            "and" => TokenKind::And,
            "or" => TokenKind::Or,
            "not" => TokenKind::Not,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "null" => TokenKind::Null,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}
