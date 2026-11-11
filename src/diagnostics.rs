// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Structured errors with stable codes and source locations, rendered
//! rustc-style. The codes are catalogued in [`crate::errors`].

use crate::syntax::span::Span;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Which layer produced the error. Stable identifiers: scripts and tools
/// can match on them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Bad characters or malformed literals.
    Lex,
    /// The source does not follow the grammar.
    Syntax,
    /// Static check failure (`cig check`): unknown name, assignment to a stick, ...
    Check,
    /// A value of the wrong type reached an operation.
    Type,
    /// Any other failure while running (division by zero, missing key, IO error, ...).
    Runtime,
    /// A script raised an error with `cough`.
    Cough,
    /// A side effect was attempted outside a `burn` block, or refused by policy.
    Burn,
    /// The command line or the environment is wrong.
    Usage,
    /// A bug in CigScript itself.
    Internal,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Lex => "lex",
            Kind::Syntax => "syntax",
            Kind::Check => "check",
            Kind::Type => "type",
            Kind::Runtime => "runtime",
            Kind::Cough => "cough",
            Kind::Burn => "burn",
            Kind::Usage => "usage",
            Kind::Internal => "internal",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Diagnostic {
    /// Stable code such as `E502`; see `cig explain`.
    pub code: &'static str,
    pub kind: Kind,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub col: Option<u32>,
    /// The path, program, run id or name the message is about, for doctor's
    /// probes and for tools; the message already shows it to people.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(skip)]
    pub span: Option<Span>,
}

impl Diagnostic {
    pub fn new(kind: Kind, message: impl Into<String>) -> Self {
        Self {
            code: crate::errors::default_code(kind),
            kind,
            message: message.into(),
            hint: None,
            line: None,
            col: None,
            subject: None,
            span: None,
        }
    }

    /// Name the thing the message is about (a path, a program, a run id).
    pub fn with_subject(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }

    /// Rebuild a diagnostic from its `--json` form (a stored run record).
    pub fn from_json(v: &serde_json::Value) -> Option<Self> {
        let kind: Kind = serde_json::from_value(v.get("kind")?.clone()).ok()?;
        let code = v
            .get("code")
            .and_then(|c| c.as_str())
            .and_then(crate::errors::lookup)
            .map(|c| c.code.as_str())
            .unwrap_or(crate::errors::default_code(kind));
        let mut d = Diagnostic::new(kind, v.get("message")?.as_str()?.to_string());
        d.code = code;
        d.hint = v.get("hint").and_then(|h| h.as_str()).map(str::to_string);
        d.line = v.get("line").and_then(|l| l.as_u64()).map(|l| l as u32);
        d.col = v.get("col").and_then(|l| l.as_u64()).map(|l| l as u32);
        d.subject = v
            .get("subject")
            .and_then(|s| s.as_str())
            .map(str::to_string);
        Some(d)
    }

    /// Give the diagnostic a specific catalogue code.
    pub fn code(mut self, code: &'static str) -> Self {
        debug_assert!(
            crate::errors::lookup(code).is_some(),
            "uncatalogued code {code}"
        );
        self.code = code;
        self
    }

    pub fn at(mut self, span: Span) -> Self {
        self.span = Some(span);
        self.line = Some(span.line);
        self.col = Some(span.col);
        self
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Attach a span only if the diagnostic does not already have one.
    pub fn or_at(self, span: Span) -> Self {
        if self.span.is_some() {
            self
        } else {
            self.at(span)
        }
    }

    /// Render with the offending source line and a caret underline.
    pub fn render(&self, file: Option<&str>, source: Option<&str>) -> String {
        let mut out = format!(
            "error[{} {}]: {}\n",
            self.code,
            self.kind.as_str(),
            self.message
        );
        if let (Some(span), Some(src)) = (self.span, source) {
            let name = file.unwrap_or("<script>");
            out.push_str(&format!("  --> {}:{}:{}\n", name, span.line, span.col));
            if let Some(line_text) = src.lines().nth(span.line.saturating_sub(1) as usize) {
                let width = span.line.to_string().len();
                let col0 = (span.col as usize).saturating_sub(1);
                let (line_text, col0) = window(line_text, col0);
                let room = line_text.chars().count().saturating_sub(col0).max(1);
                let underline_len = span.end.saturating_sub(span.start).clamp(1, room);
                out.push_str(&format!("{:width$} |\n", "", width = width));
                out.push_str(&format!("{} | {}\n", span.line, line_text));
                out.push_str(&format!(
                    "{:width$} | {}{}\n",
                    "",
                    " ".repeat(col0),
                    "^".repeat(underline_len),
                    width = width
                ));
            }
        } else if let Some(line) = self.line {
            let name = file.unwrap_or("<script>");
            out.push_str(&format!(
                "  --> {}:{}:{}\n",
                name,
                line,
                self.col.unwrap_or(1)
            ));
        }
        if let Some(hint) = &self.hint {
            out.push_str(&format!("  = hint: {hint}\n"));
        }
        out.push_str(&format!("  = explain: cig explain {}\n", self.code));
        out
    }
}

/// A source line longer than this is shown as a window around the caret,
/// with `…` where it was cut: a generated one-liner runs to hundreds of
/// kilobytes and the whole line says nothing the window does not.
const SNIPPET_WIDTH: usize = 120;

fn window(line: &str, col0: usize) -> (String, usize) {
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= SNIPPET_WIDTH {
        return (line.to_string(), col0);
    }
    let start = col0
        .saturating_sub(SNIPPET_WIDTH / 3)
        .min(chars.len() - SNIPPET_WIDTH);
    let end = (start + SNIPPET_WIDTH).min(chars.len());
    let mut shown = String::new();
    if start > 0 {
        shown.push('…');
    }
    shown.extend(&chars[start..end]);
    if end < chars.len() {
        shown.push('…');
    }
    (shown, col0 - start + usize::from(start > 0))
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for Diagnostic {}

/// Convenience constructors.
pub fn syntax(message: impl Into<String>, span: Span) -> Diagnostic {
    Diagnostic::new(Kind::Syntax, message).at(span)
}

pub fn lex(message: impl Into<String>, span: Span) -> Diagnostic {
    Diagnostic::new(Kind::Lex, message).at(span)
}

/// A check-family diagnostic raised at run time (`--no-check`, or a name
/// the checker could not see).
pub fn check(message: impl Into<String>) -> Diagnostic {
    Diagnostic::new(Kind::Check, message)
}

pub fn runtime(message: impl Into<String>) -> Diagnostic {
    Diagnostic::new(Kind::Runtime, message)
}

pub fn type_error(message: impl Into<String>) -> Diagnostic {
    Diagnostic::new(Kind::Type, message)
}

pub fn burn(message: impl Into<String>) -> Diagnostic {
    Diagnostic::new(Kind::Burn, message)
}

pub fn usage(message: impl Into<String>) -> Diagnostic {
    Diagnostic::new(Kind::Usage, message)
}

pub fn internal(message: impl Into<String>) -> Diagnostic {
    Diagnostic::new(Kind::Internal, message)
}
