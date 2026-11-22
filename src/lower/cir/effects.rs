// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
//! What the prelude's names are, and which stdlib calls are effects.

use super::ir::OpKind;
use crate::value::{Effect, Value};
use std::collections::HashMap;

/// The op an effectful stdlib call is. Every builtin whose effect needs a
/// burn has one (a test walks the catalogue), so a new effect cannot lower
/// to a plain call unseen.
pub fn kind_of(callee: &str) -> Option<OpKind> {
    Some(match callee {
        "fs.write_text" | "json.save" | "csv.write" => OpKind::Write,
        "fs.append_text" => OpKind::Append,
        "fs.mkdir" => OpKind::Mkdir,
        "fs.rm" => OpKind::Rm,
        "fs.cp" => OpKind::Cp,
        "fs.mv" => OpKind::Mv,
        "proc.run" | "proc.text" | "proc.lines" | "proc.json" | "proc.csv" | "proc.kv"
        | "proc.pipe" | "proc.shell" => OpKind::Hop,
        "proc.apply_layer" => OpKind::LayerApply,
        "proc.discard_layer" => OpKind::LayerDiscard,
        "env.set" => OpKind::EnvSet,
        "env.unset" => OpKind::EnvUnset,
        _ => return None,
    })
}

/// A name in the prelude.
pub enum Resolved {
    Builtin {
        name: &'static str,
        effect: Effect,
    },
    /// A module and its members' effects (a member that is not a builtin
    /// is listed with `None`).
    Module(HashMap<&'static str, Option<(&'static str, Effect)>>),
    /// `args` and anything else that is a plain value.
    Value,
}

pub struct Resolver {
    names: HashMap<String, Resolved>,
}

impl Resolver {
    pub fn new() -> Resolver {
        let mut names = HashMap::new();
        for (name, v) in crate::stdlib::prelude() {
            let r = match &v {
                Value::Builtin(b) => Resolved::Builtin {
                    name: b.name,
                    effect: b.effect,
                },
                Value::Module(m) => Resolved::Module(
                    m.items
                        .iter()
                        .map(|(k, v)| {
                            let b = match v {
                                Value::Builtin(b) => Some((b.name, b.effect)),
                                _ => None,
                            };
                            (*k, b)
                        })
                        .collect(),
                ),
                _ => Resolved::Value,
            };
            names.insert(name.to_string(), r);
        }
        names.insert("args".to_string(), Resolved::Value);
        Resolver { names }
    }

    pub fn get(&self, name: &str) -> Option<&Resolved> {
        self.names.get(name)
    }
}

impl Default for Resolver {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_effect_in_the_stdlib_has_an_op() {
        let mut missing = Vec::new();
        for (_, rows) in crate::stdlib::catalogue() {
            for (name, effect, _, _) in rows {
                if effect.needs_burn() && kind_of(name).is_none() {
                    missing.push(name);
                }
            }
        }
        assert!(
            missing.is_empty(),
            "effects with no burn.op kind: {missing:?}"
        );
    }

    #[test]
    fn the_prelude_resolves_modules_and_builtins() {
        let r = Resolver::new();
        assert!(matches!(r.get("fs"), Some(Resolved::Module(_))));
        assert!(matches!(r.get("args"), Some(Resolved::Value)));
        let Some(Resolved::Module(fs)) = r.get("fs") else {
            panic!("fs")
        };
        assert!(matches!(
            fs.get("write_text"),
            Some(Some((_, Effect::Write)))
        ));
    }
}
