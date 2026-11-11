// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

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
    /// When the scope was made, in order: a pass's recording refuses a read
    /// of any scope made before its work began (cig-script#186).
    serial: u64,
}

#[cfg(debug_assertions)]
thread_local! {
    static RESOLVED_CHECKED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(debug_assertions)]
thread_local! {
    static RESOLVED_MISSED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(debug_assertions)]
fn note_missed() {
    RESOLVED_MISSED.with(|c| c.set(c.get() + 1));
}

#[cfg(not(debug_assertions))]
#[inline(always)]
fn note_missed() {}

/// Debug builds: how many resolved reads or writes on this thread found
/// the wrong slot (or none) and went back to the lookup by name. A script
/// the checker passes should never make one.
#[cfg(debug_assertions)]
pub fn resolved_missed() -> u64 {
    RESOLVED_MISSED.with(|c| c.get())
}

/// Debug builds: how many resolved reads and writes on this thread have
/// been checked against the lookup by name so far.
#[cfg(debug_assertions)]
pub fn resolved_checked() -> u64 {
    RESOLVED_CHECKED.with(|c| c.get())
}

thread_local! {
    static NEXT_SERIAL: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
}

fn take_serial() -> u64 {
    NEXT_SERIAL.with(|c| {
        let v = c.get();
        c.set(v + 1);
        v
    })
}

/// The serial the next scope will be given: every scope made from now on
/// has a serial at least this.
pub fn next_serial() -> u64 {
    NEXT_SERIAL.with(|c| c.get())
}

struct Slot {
    name: String,
    value: Value,
    mutable: bool,
}

/// How many names a scope holds before it keeps a hash index beside them.
const SMALL: usize = 8;

type Index = HashMap<String, usize, BuildHasherDefault<NameHasher>>;

/// A scope's names, in the order they were declared. A name resolved by
/// `cig check` is read by its position (`Scope::get_at`), with no hashing
/// and no comparing past one name; the rest are found by name. Most scopes
/// hold a handful (a loop's variable, a call's parameters, a block's
/// locals) and are searched in place; one that grows past `SMALL` (the
/// globals, the prelude) also keeps a hash index, with a fast hasher. A
/// name is never removed, so a position, once given, stays.
struct Vars {
    /// Only the first `len` are live; the rest keep their name buffers for
    /// the next scope that takes this one from the pool (cig-script#146).
    slots: Vec<Slot>,
    len: usize,
    index: Option<Index>,
}

thread_local! {
    /// Slot arrays of scopes that ended, ready for the next call, block or
    /// loop turn: a scope in steady state allocates no array and no names.
    static POOL: RefCell<Vec<Vars>> = const { RefCell::new(Vec::new()) };
}

/// The most arrays the pool keeps, and the longest one it takes back.
const POOL_ARRAYS: usize = 256;
const POOL_SLOTS: usize = 16;

impl Drop for Scope {
    fn drop(&mut self) {
        let vars = self.vars.get_mut();
        if vars.index.is_some() || vars.slots.len() > POOL_SLOTS {
            return;
        }
        let mut taken = std::mem::replace(vars, Vars::new());
        // Values go first: one may be the last hold on another scope.
        for slot in &mut taken.slots[..taken.len] {
            slot.value = Value::Null;
        }
        taken.len = 0;
        let _ = POOL.try_with(|p| {
            if let Ok(mut p) = p.try_borrow_mut() {
                if p.len() < POOL_ARRAYS {
                    p.push(taken);
                }
            }
        });
    }
}

impl Vars {
    fn new() -> Self {
        Vars {
            slots: Vec::new(),
            len: 0,
            index: None,
        }
    }

    fn pooled() -> Self {
        POOL.try_with(|p| p.try_borrow_mut().ok().and_then(|mut p| p.pop()))
            .ok()
            .flatten()
            .unwrap_or_else(Vars::new)
    }

    #[inline]
    fn live(&self) -> &[Slot] {
        &self.slots[..self.len]
    }

    fn find(&self, name: &str) -> Option<usize> {
        match &self.index {
            Some(m) => m.get(name).copied(),
            None => self.live().iter().position(|s| s.name == name),
        }
    }

    fn get(&self, name: &str) -> Option<&Slot> {
        self.find(name).map(|i| &self.slots[i])
    }

    fn get_mut(&mut self, name: &str) -> Option<&mut Slot> {
        let i = self.find(name)?;
        Some(&mut self.slots[i])
    }

    /// Insert or overwrite.
    fn put(&mut self, name: &str, value: Value, mutable: bool) {
        if let Some(s) = self.get_mut(name) {
            s.value = value;
            s.mutable = mutable;
            return;
        }
        self.push_new(name, value, mutable);
    }

    /// Add a name that is not here yet.
    fn push_new(&mut self, name: &str, value: Value, mutable: bool) {
        let i = self.len;
        if let Some(slot) = self.slots.get_mut(i) {
            slot.name.clear();
            slot.name.push_str(name);
            slot.value = value;
            slot.mutable = mutable;
        } else {
            self.slots.push(Slot {
                name: name.to_string(),
                value,
                mutable,
            });
        }
        self.len += 1;
        match &mut self.index {
            Some(m) => {
                m.insert(name.to_string(), i);
            }
            None if self.len > SMALL => {
                let mut m: Index = HashMap::with_capacity_and_hasher(SMALL * 2, Default::default());
                for (i, s) in self.live().iter().enumerate() {
                    m.insert(s.name.clone(), i);
                }
                self.index = Some(m);
            }
            None => {}
        }
    }

    fn names(&self) -> Vec<String> {
        self.live().iter().map(|s| s.name.clone()).collect()
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
            vars: RefCell::new(Vars::new()),
            parent: None,
            serial: take_serial(),
        })
    }

    pub fn child(parent: &Env) -> Env {
        Rc::new(Scope {
            vars: RefCell::new(Vars::pooled()),
            parent: Some(parent.clone()),
            serial: take_serial(),
        })
    }

    /// When this scope was made (see `next_serial`).
    pub fn serial(&self) -> u64 {
        self.serial
    }

    /// Declare a name in this scope. Returns `false` if it already exists here.
    pub fn declare(&self, name: &str, value: Value, mutable: bool) -> bool {
        let mut vars = self.vars.borrow_mut();
        if vars.find(name).is_some() {
            return false;
        }
        vars.push_new(name, value, mutable);
        true
    }

    /// Declare or overwrite, used by the prelude and the REPL.
    pub fn force_declare(&self, name: &str, value: Value, mutable: bool) {
        self.vars.borrow_mut().put(name, value, mutable);
    }

    /// A name's value, the serial of the scope that holds it, and whether it
    /// is mutable (`roll`).
    pub fn get_traced(&self, name: &str) -> Option<(Value, u64, bool)> {
        let mut scope = self;
        loop {
            if let Some(slot) = scope.vars.borrow().get(name) {
                return Some((slot.value.clone(), scope.serial, slot.mutable));
            }
            scope = scope.parent.as_deref()?;
        }
    }

    /// The serial of the scope that holds a name.
    pub fn owner_serial(&self, name: &str) -> Option<u64> {
        let mut scope = self;
        loop {
            if scope.vars.borrow().get(name).is_some() {
                return Some(scope.serial);
            }
            scope = scope.parent.as_deref()?;
        }
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

    /// The scope `depth` parents up.
    #[inline]
    fn ancestor(&self, depth: u16) -> Option<&Scope> {
        let mut scope = self;
        for _ in 0..depth {
            scope = scope.parent.as_deref()?;
        }
        Some(scope)
    }

    /// A name `cig check` resolved: the `idx`th name declared `depth` scopes
    /// up. `None` when that is not where the name is (the declaration has
    /// not run yet, or the tree was not checked against this chain), and the
    /// caller looks it up by name. A debug build also checks every hit
    /// against the lookup by name (cig-script#146).
    #[inline]
    pub fn get_at(&self, depth: u16, idx: u16, name: &str) -> Option<Value> {
        let Some(scope) = self.ancestor(depth) else {
            note_missed();
            return None;
        };
        let value = {
            let vars = scope.vars.borrow();
            match vars.live().get(usize::from(idx)) {
                Some(slot) if slot.name == name => slot.value.clone(),
                _ => {
                    note_missed();
                    return None;
                }
            }
        };
        #[cfg(debug_assertions)]
        self.cross_check(scope, idx, name);
        Some(value)
    }

    /// `assign` for a resolved name. `Err(value)` hands the value back when
    /// the slot is not the name's, for the lookup by name.
    #[inline]
    pub fn assign_at(
        &self,
        depth: u16,
        idx: u16,
        name: &str,
        value: Value,
    ) -> Result<Result<(), AssignError>, Value> {
        let Some(scope) = self.ancestor(depth) else {
            note_missed();
            return Err(value);
        };
        {
            let mut vars = scope.vars.borrow_mut();
            let len = vars.len;
            match vars.slots[..len].get_mut(usize::from(idx)) {
                Some(slot) if slot.name == name => {
                    if !slot.mutable {
                        return Ok(Err(AssignError::Immutable));
                    }
                    slot.value = value;
                }
                _ => {
                    note_missed();
                    return Err(value);
                }
            }
        }
        #[cfg(debug_assertions)]
        self.cross_check(scope, idx, name);
        Ok(Ok(()))
    }

    /// Debug builds: the lookup by name from here must land on the same
    /// scope and the same slot as the resolved position did.
    #[cfg(debug_assertions)]
    fn cross_check(&self, scope: &Scope, idx: u16, name: &str) {
        let (_, serial, mutable) = self
            .get_traced(name)
            .unwrap_or_else(|| panic!("resolved `{name}` but the named lookup finds nothing"));
        assert_eq!(
            serial, scope.serial,
            "resolved `{name}` to another scope than the named lookup"
        );
        let vars = scope.vars.borrow();
        assert_eq!(
            vars.find(name),
            Some(usize::from(idx)),
            "resolved `{name}` to another slot than the named lookup"
        );
        assert_eq!(vars.slots[usize::from(idx)].mutable, mutable);
        RESOLVED_CHECKED.with(|c| c.set(c.get() + 1));
    }

    /// Debug builds: a name read straight from the prelude must be the one
    /// the lookup by name finds from here.
    #[cfg(debug_assertions)]
    pub fn cross_check_owner(&self, name: &str, owner: u64) {
        assert_eq!(
            self.owner_serial(name),
            Some(owner),
            "resolved `{name}` to the prelude but the named lookup finds it elsewhere"
        );
        RESOLVED_CHECKED.with(|c| c.set(c.get() + 1));
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
