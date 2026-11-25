// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Shapes: which keys a map has, and in which order, as one number.
//!
//! A map is its entries in insertion order ([`Entries`]). Reading
//! `b.vx` finds `vx` among them: a hash of the name, a probe, a compare
//! of the key. Most maps a script reads fields of are records: every
//! `{x: .., y: .., vx: ..}` a loop makes has the same keys in the same
//! order, and `vx` is the third of each. A **shape** says that in one
//! word. Two maps have the same shape when they got the same keys in
//! the same order, by a literal or by adding fields one at a time, and
//! a site that last read `.vx` from a map of shape 17 at place 2 reads
//! the next map of shape 17 at place 2 with one compare: no hash, no
//! key looked at.
//!
//! Shapes are a second layer over the entries, not another way of
//! keeping them. A map's entries are what they were, in the order they
//! were; its shape is a number beside them that is kept when a key is
//! added where the tree of shapes can follow, and dropped ([`NONE`])
//! whenever the entries change any other way. A map without a shape is
//! read as every map was before there were shapes.
//!
//! **The tree.** A shape is a node: its parent (the shape before its
//! last key was added) and that key. The root ([`EMPTY`]) is the shape
//! of a map with no key. Adding key `k` to a map of shape `s` gives the
//! child of `s` under `k`. A node is never changed or removed, so a
//! shape, once given, means the same keys for the whole run.
//!
//! **Who makes a shape.** A child is made only where a script names the
//! key itself: a map literal's key and a field assigned by name
//! ([`Entries::add_field`]). The keys a script computes (`m[k] = v`,
//! what `json.parse` reads) follow the tree where it already leads and
//! make nothing ([`Entries::insert`]), so the tree's size is bounded by
//! the keys written in the script, not by its data.
//!
//! **What has no shape.** A map with more than [`MAX_KEYS`] keys is a
//! dictionary, not a record. A shape that [`MAX_KIDS`] different keys
//! were already added to gets no more children, and the tree holds at
//! most [`MAX_SHAPES`] shapes. In each case the map goes on without a
//! shape, which costs it what a map cost before there were shapes.

use crate::value::Value;
use indexmap::IndexMap;
use std::cell::RefCell;
use std::rc::Rc;

/// No shape: the map's keys are found by looking them up.
pub const NONE: u32 = 0;

/// The shape of a map with no key.
pub const EMPTY: u32 = 1;

/// A map with more keys than this has no shape.
pub const MAX_KEYS: usize = 32;

/// How many different keys may be added to one shape before the next
/// new one gives no shape.
pub const MAX_KIDS: usize = 64;

/// How many shapes a run may make.
pub const MAX_SHAPES: usize = 1 << 16;

/// One shape: the shape it was made from, and the key added to it.
struct Node {
    parent: u32,
    /// The key that makes this shape of its parent; empty for the root.
    key: Rc<str>,
    /// The shapes made of this one, one for each key added to it so far.
    kids: Vec<u32>,
}

/// Every shape of this thread's run, by its number. Number 0 is no
/// shape and number 1 the root; both are there from the start.
struct Tree {
    nodes: Vec<Node>,
}

impl Tree {
    fn new() -> Tree {
        let root = || Node {
            parent: NONE,
            key: Rc::from(""),
            kids: Vec::new(),
        };
        Tree {
            nodes: vec![root(), root()],
        }
    }
}

thread_local! {
    static SHAPES: RefCell<Tree> = RefCell::new(Tree::new());
}

/// The shape of a map of shape `from`, holding `len` keys, once `key`,
/// which it does not hold, is added at its end: the child of `from`
/// under `key`. With `make`, a child that is not there yet is made;
/// without, there is no shape for it. [`NONE`] as well when `from` is
/// none, or the map is past what a shape describes (see the module).
fn child(from: u32, key: &str, len: usize, make: bool) -> u32 {
    if from == NONE || len >= MAX_KEYS {
        return NONE;
    }
    SHAPES.with(|tree| {
        let Ok(mut tree) = tree.try_borrow_mut() else {
            return NONE;
        };
        let Some(node) = tree.nodes.get(from as usize) else {
            return NONE;
        };
        for &kid in &node.kids {
            if tree.nodes.get(kid as usize).is_some_and(|k| &*k.key == key) {
                return kid;
            }
        }
        if !make || node.kids.len() >= MAX_KIDS || tree.nodes.len() >= MAX_SHAPES {
            return NONE;
        }
        let Ok(made) = u32::try_from(tree.nodes.len()) else {
            return NONE;
        };
        tree.nodes.push(Node {
            parent: from,
            key: Rc::from(key),
            kids: Vec::new(),
        });
        if let Some(node) = tree.nodes.get_mut(from as usize) {
            node.kids.push(made);
        }
        made
    })
}

/// The keys of shape `shape`, first to last: for tests and for saying
/// what a shape is. Empty for [`NONE`] and for [`EMPTY`].
pub fn keys(shape: u32) -> Vec<Rc<str>> {
    SHAPES.with(|tree| {
        let Ok(tree) = tree.try_borrow() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut at = shape;
        while at > EMPTY {
            let Some(node) = tree.nodes.get(at as usize) else {
                break;
            };
            out.push(Rc::clone(&node.key));
            at = node.parent;
        }
        out.reverse();
        out
    })
}

/// A map's entries, in insertion order, and its shape.
///
/// It reads as the `IndexMap` it holds (every lookup, every walk: it
/// derefs to it). It is changed only through the functions here, each
/// of which keeps the shape true: one that adds a key at the end moves
/// the shape along the tree, one that puts a value where a key already
/// is leaves it, and anything else drops it.
#[derive(Default)]
pub struct Entries {
    map: IndexMap<String, Value>,
    shape: u32,
}

impl Entries {
    /// No entries: the root shape.
    pub fn new() -> Entries {
        Entries {
            map: IndexMap::new(),
            shape: EMPTY,
        }
    }

    /// No entries, with room for `n`.
    pub fn with_capacity(n: usize) -> Entries {
        Entries {
            map: IndexMap::with_capacity(n),
            shape: EMPTY,
        }
    }

    /// The map's shape, or [`NONE`].
    #[inline(always)]
    pub fn shape(&self) -> u32 {
        self.shape
    }

    /// `map[key] = value`: the value that was there, if the key was.
    /// A new key goes at the end, and the shape follows the tree where
    /// it already leads there; it makes no shape.
    pub fn insert(&mut self, key: String, value: Value) -> Option<Value> {
        self.put(key, value, false).1
    }

    /// [`Entries::insert`], with the entry's place.
    pub fn insert_full(&mut self, key: String, value: Value) -> (usize, Option<Value>) {
        self.put(key, value, false)
    }

    /// `map.name = value` for a key the script names itself (a literal's
    /// key, a field assigned by name): as [`Entries::insert`], and a new
    /// key makes its shape where the tree has none for it yet.
    pub fn add_field(&mut self, name: &str, value: Value) -> Option<Value> {
        if self.shape == NONE {
            return self.map.insert(name.to_string(), value);
        }
        if let Some(slot) = self.map.get_mut(name) {
            return Some(std::mem::replace(slot, value));
        }
        self.shape = child(self.shape, name, self.map.len(), true);
        self.map.insert(name.to_string(), value)
    }

    /// `map.name = value` for a key a map of this shape does not hold,
    /// by a caller that added it to a map of this shape before and kept
    /// the shape that map had after, `to`: the tree is not asked again.
    /// `to` must be what [`Entries::add_field`] gave for this shape and
    /// this key.
    pub fn add_known(&mut self, name: &str, value: Value, to: u32) {
        debug_assert!(!self.map.contains_key(name));
        self.map.insert(name.to_string(), value);
        self.shape = to;
    }

    fn put(&mut self, key: String, value: Value, make: bool) -> (usize, Option<Value>) {
        if self.shape == NONE {
            return self.map.insert_full(key, value);
        }
        let len = self.map.len();
        match self.map.entry(key) {
            indexmap::map::Entry::Occupied(mut e) => (e.index(), Some(e.insert(value))),
            indexmap::map::Entry::Vacant(e) => {
                self.shape = child(self.shape, e.key(), len, make);
                e.insert(value);
                (len, None)
            }
        }
    }

    /// The value under `key`, to change it in place.
    #[inline]
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.map.get_mut(key)
    }

    /// The entry at `index`, its value to change in place.
    #[inline]
    pub fn get_index_mut(&mut self, index: usize) -> Option<(&String, &mut Value)> {
        self.map.get_index_mut(index)
    }

    /// Every value, to change in place.
    pub fn values_mut(&mut self) -> indexmap::map::ValuesMut<'_, String, Value> {
        self.map.values_mut()
    }

    /// Every entry, its value to change in place.
    pub fn iter_mut(&mut self) -> indexmap::map::IterMut<'_, String, Value> {
        self.map.iter_mut()
    }

    /// Take `key` out, the entries after it moving up: no shape after.
    pub fn shift_remove(&mut self, key: &str) -> Option<Value> {
        let gone = self.map.shift_remove(key);
        if gone.is_some() {
            self.shape = NONE;
        }
        gone
    }

    /// No entries: the root shape again.
    pub fn clear(&mut self) {
        self.map.clear();
        self.shape = EMPTY;
    }

    /// The entries themselves, for a change this type has no function
    /// for: the map has no shape after it.
    pub fn reshape(&mut self) -> &mut IndexMap<String, Value> {
        self.shape = NONE;
        &mut self.map
    }

    /// The entries, the map given up.
    pub fn into_inner(self) -> IndexMap<String, Value> {
        self.map
    }
}

impl std::ops::Deref for Entries {
    type Target = IndexMap<String, Value>;

    #[inline(always)]
    fn deref(&self) -> &IndexMap<String, Value> {
        &self.map
    }
}

impl From<IndexMap<String, Value>> for Entries {
    /// Entries made elsewhere: an empty map has the root shape, any
    /// other none (its keys did not come through the tree).
    fn from(map: IndexMap<String, Value>) -> Entries {
        let shape = if map.is_empty() { EMPTY } else { NONE };
        Entries { map, shape }
    }
}

impl Clone for Entries {
    /// The same entries have the same shape.
    fn clone(&self) -> Entries {
        Entries {
            map: self.map.clone(),
            shape: self.shape,
        }
    }
}

impl Extend<(String, Value)> for Entries {
    /// Each entry as [`Entries::insert`] puts it.
    fn extend<I: IntoIterator<Item = (String, Value)>>(&mut self, entries: I) {
        for (key, value) in entries {
            self.insert(key, value);
        }
    }
}

impl<'a> IntoIterator for &'a Entries {
    type Item = (&'a String, &'a Value);
    type IntoIter = indexmap::map::Iter<'a, String, Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.map.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(keys: &[&str]) -> Entries {
        let mut e = Entries::new();
        for (n, k) in keys.iter().enumerate() {
            assert!(e.add_field(k, Value::Int(n as i64)).is_none());
        }
        e
    }

    fn names(shape: u32) -> Vec<String> {
        keys(shape).iter().map(|k| k.to_string()).collect()
    }

    #[test]
    fn maps_given_the_same_keys_in_the_same_order_have_one_shape() {
        assert_eq!(Entries::new().shape(), EMPTY);
        let a = record(&["x", "y", "vx"]);
        let b = record(&["x", "y", "vx"]);
        assert_ne!(a.shape(), NONE);
        assert_eq!(a.shape(), b.shape());
        assert_eq!(names(a.shape()), ["x", "y", "vx"]);
        // A prefix is the shape on the way there; another order and
        // another key are other shapes.
        let prefix = record(&["x", "y"]);
        assert_ne!(prefix.shape(), a.shape());
        assert_eq!(names(prefix.shape()), ["x", "y"]);
        let other_order = record(&["y", "x", "vx"]);
        assert_ne!(other_order.shape(), NONE);
        assert_ne!(other_order.shape(), a.shape());
        assert_ne!(record(&["x", "y", "vy"]).shape(), a.shape());
        // A key's place in a map is its place in the shape's keys.
        for (i, k) in keys(a.shape()).iter().enumerate() {
            assert_eq!(a.get_index_of(&**k), Some(i));
        }
    }

    #[test]
    fn a_value_put_where_a_key_is_leaves_the_shape() {
        let mut a = record(&["x", "y"]);
        let shape = a.shape();
        assert!(a.add_field("x", Value::Int(9)).is_some());
        assert!(a.insert("y".to_string(), Value::Int(8)).is_some());
        *a.get_mut("x").expect("there") = Value::Int(7);
        if let Some((_, v)) = a.get_index_mut(1) {
            *v = Value::Int(6);
        }
        for v in a.values_mut() {
            *v = Value::Null;
        }
        assert_eq!(a.shape(), shape);
        assert_eq!(a.len(), 2);
    }

    #[test]
    fn a_key_a_script_computes_follows_the_tree_and_makes_no_shape() {
        // Nothing leads from the root under this key yet: no shape.
        let mut d = Entries::new();
        d.insert("k1".to_string(), Value::Int(1));
        assert_eq!(d.shape(), NONE);
        assert_eq!(d.get("k1").map(Value::repr), Some("1".to_string()));
        // And none again, whatever is added to it after.
        d.add_field("x", Value::Int(2));
        assert_eq!(d.shape(), NONE);
        // Where a record went first, the computed key follows.
        let made = record(&["x", "y"]);
        let mut e = Entries::new();
        e.insert("x".to_string(), Value::Int(1));
        e.insert("y".to_string(), Value::Int(2));
        assert_eq!(e.shape(), made.shape());
        e.insert("z".to_string(), Value::Int(3));
        assert_eq!(e.shape(), NONE, "nothing was ever added to it by name");
    }

    #[test]
    fn any_other_change_drops_the_shape() {
        let mut a = record(&["x", "y", "z"]);
        assert!(a.shift_remove("y").is_some());
        assert_eq!(a.shape(), NONE);
        assert_eq!(a.keys().map(String::as_str).collect::<Vec<_>>(), ["x", "z"]);
        // A key that was not there takes nothing out and changes nothing.
        let mut b = record(&["x"]);
        let shape = b.shape();
        assert!(b.shift_remove("nope").is_none());
        assert_eq!(b.shape(), shape);
        // The entries themselves, changed by hand.
        b.reshape().insert("w".to_string(), Value::Null);
        assert_eq!(b.shape(), NONE);
        // Emptied, a map is a new map.
        b.clear();
        assert_eq!(b.shape(), EMPTY);
        // Entries made elsewhere came through no tree.
        let mut made = IndexMap::new();
        made.insert("x".to_string(), Value::Null);
        assert_eq!(Entries::from(made).shape(), NONE);
        assert_eq!(Entries::from(IndexMap::new()).shape(), EMPTY);
        // A copy has the entries, so it has the shape.
        let c = record(&["x", "y"]);
        assert_eq!(c.clone().shape(), c.shape());
    }

    #[test]
    fn a_map_with_more_keys_than_a_record_has_is_a_dictionary() {
        let keys: Vec<String> = (0..MAX_KEYS + 1).map(|n| format!("f{n}")).collect();
        let mut e = Entries::new();
        for (n, k) in keys.iter().enumerate() {
            e.add_field(k, Value::Null);
            assert_eq!(e.shape() != NONE, n < MAX_KEYS, "after {} keys", n + 1);
        }
        assert_eq!(e.len(), MAX_KEYS + 1);
        // Every key is still found.
        assert!(keys.iter().all(|k| e.contains_key(k)));
    }

    #[test]
    fn a_shape_too_many_keys_were_added_to_gets_no_more_children() {
        let base = record(&["id"]);
        let mut shapes = Vec::new();
        for n in 0..MAX_KIDS + 3 {
            let mut e = base.clone();
            e.add_field(&format!("k{n}"), Value::Null);
            shapes.push(e.shape());
        }
        assert!(shapes[..MAX_KIDS].iter().all(|s| *s != NONE));
        assert!(shapes[MAX_KIDS..].iter().all(|s| *s == NONE));
        // The children it has stay.
        let mut again = base.clone();
        again.add_field("k0", Value::Null);
        assert_eq!(again.shape(), shapes[0]);
    }
}
