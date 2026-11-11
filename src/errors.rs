// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The error-code registry, read from `errors/registry.toml`.
//!
//! A code is a node, not a number: it carries its family and kind (from the
//! range it sits in), which of the four kinds of no it is, where it can
//! arise, ranked causes each with a read-only probe and a remedy, and the
//! codes it relates to. Everything that talks about codes reads this one
//! table: `cig explain`, the `--json` output, the JSON schema, doctor, and
//! the generated `docs/ERRORS.md`.
//!
//! The registry file documents the ranges. `tests/registry.rs` keeps the
//! source and the registry in step in both directions.

use crate::diagnostics::Kind;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// The registry source, embedded so the binary needs no file at run time.
pub const REGISTRY_TOML: &str = include_str!("../errors/registry.toml");

#[derive(Debug, Deserialize)]
pub struct Registry {
    pub version: u32,
    #[serde(rename = "range")]
    pub ranges: Vec<Range>,
    #[serde(rename = "no")]
    pub nos: Vec<No>,
    #[serde(rename = "probe")]
    pub probes: Vec<Probe>,
    #[serde(rename = "code")]
    pub codes: Vec<Code>,
}

/// A contiguous block of codes with one family and one rendering kind.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Range {
    pub from: String,
    pub to: String,
    pub family: String,
    pub kind: Kind,
    pub meaning: String,
}

/// One of the four kinds of no (plus `none` and `depends`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct No {
    pub id: String,
    /// What you hear, in the hallway.
    pub hear: String,
    /// Whose problem it is.
    pub whose: String,
    /// The manual's name for it.
    pub manual: String,
}

/// A read-only check doctor may run to confirm a cause.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Probe {
    pub name: String,
    pub reads: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cause {
    pub why: String,
    /// Which kind of no this cause is, when it is one (overrides the code's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no: Option<String>,
    #[serde(default = "none")]
    pub probe: String,
    pub remedy: String,
    /// Doctor can apply the remedy itself, with consent.
    #[serde(default)]
    pub fixable: bool,
    #[serde(default)]
    pub needs_burn: bool,
    #[serde(default = "yes")]
    pub reversible: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Code {
    pub code: String,
    pub title: String,
    pub meaning: String,
    /// What to type next.
    pub fix: String,
    #[serde(default = "none")]
    pub no: String,
    #[serde(default)]
    pub arises: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub since: String,
    #[serde(default)]
    pub retired: bool,
    #[serde(default)]
    pub related: Vec<String>,
    #[serde(default, rename = "cause")]
    pub causes: Vec<Cause>,
}

fn none() -> String {
    "none".to_string()
}

fn yes() -> bool {
    true
}

/// A code's number: `E502` is 502, `E1050` is 1050. Codes are compared
/// by number, never as text (as text, `E1050` would sort among `E100`s).
pub fn number(code: &str) -> Option<u32> {
    let digits = code.strip_prefix('E')?;
    if !(3..=4).contains(&digits.len()) || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

impl Range {
    /// Does this range hold the code numbered `n`?
    pub fn holds(&self, n: u32) -> bool {
        number(&self.from).is_some_and(|f| f <= n) && number(&self.to).is_some_and(|t| n <= t)
    }
}

impl Code {
    /// The range this code sits in.
    pub fn range(&self) -> &'static Range {
        let n = number(&self.code)
            .expect("every registered code is E and 3 or 4 digits (tests/registry.rs)");
        registry()
            .ranges
            .iter()
            .find(|r| r.holds(n))
            .expect("every registered code lies in a declared range (tests/registry.rs)")
    }

    pub fn kind(&self) -> Kind {
        self.range().kind
    }

    pub fn family(&self) -> &'static str {
        &self.range().family
    }

    /// The kind-of-no entry for this code.
    pub fn no_entry(&self) -> &'static No {
        lookup_no(&self.no).expect("every code's `no` is declared (tests/registry.rs)")
    }

    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t == tag)
    }

    /// The full node as JSON, with the derived fields filled in.
    pub fn to_json(&self) -> serde_json::Value {
        let no = self.no_entry();
        serde_json::json!({
            "code": self.code,
            "kind": self.kind().as_str(),
            "family": self.family(),
            "title": self.title,
            "meaning": self.meaning,
            "fix": self.fix,
            "no": {"id": no.id, "hear": no.hear, "whose": no.whose, "manual": no.manual},
            "arises": self.arises,
            "tags": self.tags,
            "since": self.since,
            "retired": self.retired,
            "related": self.related,
            "causes": self.causes,
        })
    }
}

/// The parsed registry. Parsing happens once; a malformed registry is a
/// build defect, caught by the unit test below before it can ship.
pub fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        toml::from_str(REGISTRY_TOML).expect("errors/registry.toml parses (see tests/registry.rs)")
    })
}

/// Every code, in registry order.
pub fn all() -> &'static [Code] {
    &registry().codes
}

pub fn lookup(code: &str) -> Option<&'static Code> {
    let wanted = code.trim().to_ascii_uppercase();
    all().iter().find(|e| e.code == wanted)
}

pub fn lookup_no(id: &str) -> Option<&'static No> {
    registry().nos.iter().find(|n| n.id == id)
}

pub fn lookup_probe(name: &str) -> Option<&'static Probe> {
    registry().probes.iter().find(|p| p.name == name)
}

/// Generic code for a kind, used when nothing more specific applies.
pub const fn default_code(kind: Kind) -> &'static str {
    match kind {
        Kind::Lex => "E100",
        Kind::Syntax => "E200",
        Kind::Check => "E300",
        Kind::Type => "E400",
        Kind::Runtime => "E500",
        Kind::Cough => "E600",
        Kind::Burn => "E700",
        Kind::Usage => "E800",
        Kind::Internal => "E900",
    }
}

/// JSON Schema (draft 2020-12) for one diagnostic as `--json` prints it,
/// generated from the registry so the `code` enum can never drift.
pub fn diagnostic_schema() -> serde_json::Value {
    let codes: Vec<&str> = all().iter().map(|c| c.code.as_str()).collect();
    let kinds: Vec<&str> = [
        Kind::Lex,
        Kind::Syntax,
        Kind::Check,
        Kind::Type,
        Kind::Runtime,
        Kind::Cough,
        Kind::Burn,
        Kind::Usage,
        Kind::Internal,
    ]
    .iter()
    .map(|k| k.as_str())
    .collect();
    let families: Vec<&str> = registry()
        .ranges
        .iter()
        .map(|r| r.family.as_str())
        .collect();
    let nos: Vec<&str> = registry().nos.iter().map(|n| n.id.as_str()).collect();
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://github.com/otmof-ops/cig-script/blob/main/docs/diagnostic.schema.json",
        "title": "CigScript diagnostic",
        "description": format!("One diagnostic as `cig --json` prints it. Generated from errors/registry.toml (registry version {}) for cigscript {}.", registry().version, crate::VERSION),
        "type": "object",
        "required": ["code", "kind", "message"],
        "properties": {
            "code": {"type": "string", "enum": codes},
            "kind": {"type": "string", "enum": kinds},
            "message": {"type": "string"},
            "hint": {"type": "string", "description": "what to type next"},
            "line": {"type": "integer", "minimum": 1},
            "col": {"type": "integer", "minimum": 1},
            "subject": {"type": "string", "description": "the path, program, run id or name the message is about"},
            "file": {"type": "string"},
            "diagnosis": {"$ref": "#/$defs/diagnosis"}
        },
        "additionalProperties": false,
        "$defs": {
            "family": {"type": "string", "enum": families},
            "no": {"type": "string", "enum": nos},
            "diagnosis": {
                "type": "object",
                "description": "doctor's read-only diagnosis, when it fired: verdict, why, fix, and what to check if that is not it",
                "required": ["code", "confirmed", "verdict", "why", "fix", "if_not", "probes", "ms"],
                "properties": {
                    "code": {"type": "string"},
                    "confirmed": {"type": "boolean"},
                    "verdict": {"type": "string"},
                    "no": {"$ref": "#/$defs/no"},
                    "ms": {"type": "integer", "minimum": 0},
                    "why": {"type": "array", "items": {"type": "string"}},
                    "fix": {"type": "array", "items": {"type": "string"}},
                    "if_not": {"type": "array", "items": {"type": "string"}},
                    "probes": {"type": "array", "items": {"type": "object"}}
                },
                "additionalProperties": true
            },
            "check_report": {
                "type": "object",
                "description": "what `cig --json check <file>` prints",
                "required": ["file", "ok", "errors", "warnings"],
                "properties": {
                    "file": {"type": "string"},
                    "ok": {"type": "boolean"},
                    "errors": {"type": "array", "items": {"$ref": "#"}},
                    "warnings": {"type": "array", "items": {"$ref": "#"}}
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_parses_and_codes_are_unique_and_in_range() {
        let reg = registry();
        assert!(reg.version >= 1);
        let mut seen = std::collections::HashSet::new();
        for c in all() {
            assert!(number(&c.code).is_some(), "{}", c.code);
            assert!(seen.insert(c.code.as_str()), "duplicate {}", c.code);
            // range() panics when a code sits outside every range.
            let _ = c.range();
            // A three-digit code's first digit is its kind's; the
            // four-digit families (E1000-E1999, cigcompile and CigarScript)
            // say their kind in the range alone.
            if c.code.len() == 4 {
                assert_eq!(
                    &c.code[1..2],
                    &default_code(c.kind())[1..2],
                    "{} kind",
                    c.code
                );
            }
        }
        for kind in [
            Kind::Lex,
            Kind::Syntax,
            Kind::Check,
            Kind::Type,
            Kind::Runtime,
            Kind::Cough,
            Kind::Burn,
            Kind::Usage,
            Kind::Internal,
        ] {
            let d = lookup(default_code(kind)).unwrap_or_else(|| panic!("{kind:?} default"));
            assert_eq!(d.kind(), kind);
        }
        assert!(lookup("e502").is_some(), "lookup is case-insensitive");
        assert!(lookup("E0000").is_none());
    }

    #[test]
    fn schema_lists_every_code() {
        let schema = diagnostic_schema();
        let listed = schema["properties"]["code"]["enum"].as_array().unwrap();
        assert_eq!(listed.len(), all().len());
    }
}
