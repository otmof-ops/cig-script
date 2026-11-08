// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: Apache-2.0

//! Lexical scopes.

use crate::value::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::rc::Rc;

pub type Env = Rc<Scope>;

pub struct Scope {
    vars: RefCell<Vars>,
    parent: Option<Env>,
}

struct Slot {
    value: Value,
    mutable: bool,
}

/// How many names a scope holds in a plain list before it hashes them.
const SMALL: usize = 8;

/// A scope's names. Most scopes hold a handful (a loop's variable, a
/// call's parameters, a block's locals), where comparing a few short names
/// beats hashing one; a scope that grows past `SMALL` (the globals, the
/// prelude) is hashed, with a fast hasher. Nothing reads the order.
enum Vars {
    Small(Vec<(String, Slot)>),
    Large(HashMap<String, Slot, BuildHasherDefault<NameHasher>>),
}

impl Vars {
    fn get(&self, name: &str) -> Option<&Slot> {
        match self {
            Vars::Small(v) => v.iter().find(|(n, _)| n == name).map(|(_, s)| s),
            Vars::Large(m) => m.get(name),
        }
    }

    fn get_mut(&mut self, name: &str) -> Option<&mut Slot> {
        match self {
            Vars::Small(v) => v.iter_mut().find(|(n, _)| n == name).map(|(_, s)| s),
            Vars::Large(m) => m.get_mut(name),
        }
    }

    /// Insert or overwrite.
    fn put(&mut self, name: &str, slot: Slot) {
        if let Some(s) = self.get_mut(name) {
            *s = slot;
            return;
        }
        match self {
            Vars::Small(v) if v.len() < SMALL => v.push((name.to_string(), slot)),
            Vars::Small(v) => {
                let mut m: HashMap<String, Slot, BuildHasherDefault<NameHasher>> =
                    HashMap::with_capacity_and_hasher(SMALL * 2, Default::default());
                m.extend(v.drain(..));
                m.insert(name.to_string(), slot);
                *self = Vars::Large(m);
            }
            Vars::Large(m) => {
                m.insert(name.to_string(), slot);
            }
        }
    }

    fn names(&self) -> Vec<String> {
        match self {
            Vars::Small(v) => v.iter().map(|(n, _)| n.clone()).collect(),
            Vars::Large(m) => m.keys().cloned().collect(),
        }
    }
}

/// FxHash's mixing step: a multiply and a rotate per word. Names are short
/// and come from the script itself, so the default SipHash's resistance to
/// chosen collisions buys nothing here and costs every lookup.
#[derive(Default)]
struct NameHasher(u64);

impl NameHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
}

impl Hasher for NameHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for c in &mut chunks {
            self.add(u64::from_le_bytes(c.try_into().expect("eight bytes")));
        }
        let rest = chunks.remainder();
        if !rest.is_empty() {
            let mut word = [0u8; 8];
            word[..rest.len()].copy_from_slice(rest);
            self.add(u64::from_le_bytes(word));
        }
    }

    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add(u64::from(i));
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum AssignError {
    NotFound,
    Immutable,
}

impl Scope {
    pub fn root() -> Env {
        Rc::new(Scope {
            vars: RefCell::new(Vars::Small(Vec::new())),
            parent: None,
        })
    }

    pub fn child(parent: &Env) -> Env {
        Rc::new(Scope {
            vars: RefCell::new(Vars::Small(Vec::new())),
            parent: Some(parent.clone()),
        })
    }

    /// Declare a name in this scope. Returns `false` if it already exists here.
    pub fn declare(&self, name: &str, value: Value, mutable: bool) -> bool {
        let mut vars = self.vars.borrow_mut();
        if vars.get(name).is_some() {
            return false;
        }
        vars.put(name, Slot { value, mutable });
        true
    }

    /// Declare or overwrite, used by the prelude and the REPL.
    pub fn force_declare(&self, name: &str, value: Value, mutable: bool) {
        self.vars.borrow_mut().put(name, Slot { value, mutable });
    }

    pub fn get(&self, name: &str) -> Option<Value> {
        let mut scope = self;
        loop {
            if let Some(slot) = scope.vars.borrow().get(name) {
                return Some(slot.value.clone());
            }
            scope = scope.parent.as_deref()?;
        }
    }

    pub fn assign(&self, name: &str, value: Value) -> Result<(), AssignError> {
        let mut scope = self;
        loop {
            if let Some(slot) = scope.vars.borrow_mut().get_mut(name) {
                if !slot.mutable {
                    return Err(AssignError::Immutable);
                }
                slot.value = value;
                return Ok(());
            }
            match scope.parent.as_deref() {
                Some(p) => scope = p,
                None => return Err(AssignError::NotFound),
            }
        }
    }

    /// All names visible from this scope, for "did you mean" hints.
    pub fn visible_names(&self) -> Vec<String> {
        let mut names = self.vars.borrow().names();
        if let Some(p) = &self.parent {
            names.extend(p.visible_names());
        }
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scope that grows past the list keeps every name, its value and
    /// its mutability, and still refuses a second declaration.
    #[test]
    fn a_scope_that_grows_past_the_list_keeps_every_name() {
        let root = Scope::root();
        for i in 0..(SMALL * 3) {
            assert!(root.declare(&format!("v{i}"), Value::Int(i as i64), i % 2 == 0));
        }
        for i in 0..(SMALL * 3) {
            assert!(matches!(root.get(&format!("v{i}")), Some(Value::Int(n)) if n == i as i64));
            assert!(!root.declare(&format!("v{i}"), Value::Null, true));
            let r = root.assign(&format!("v{i}"), Value::Int(-1));
            if i % 2 == 0 {
                assert_eq!(r, Ok(()));
            } else {
                assert_eq!(r, Err(AssignError::Immutable));
            }
        }
        let child = Scope::child(&root);
        assert!(child.declare("v0", Value::Int(7), false));
        assert!(matches!(child.get("v0"), Some(Value::Int(7))));
        assert!(matches!(root.get("v0"), Some(Value::Int(-1))));
        assert_eq!(
            child.assign("nope", Value::Null),
            Err(AssignError::NotFound)
        );
        let mut names = child.visible_names();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), SMALL * 3);
        root.force_declare("v1", Value::Int(5), true);
        assert!(matches!(child.get("v1"), Some(Value::Int(5))));
    }
}
