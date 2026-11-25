// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! A set of value tags, as decision 0005 says
//! (`docs/decisions/0005-one-lattice-for-proven-and-observed-types.md`):
//! a proven type and an observed type are both a set of tags over the
//! same eleven bits. One set holds one register's own observation or
//! proof: a two-operand instruction carries two sets, one each.
//!
//! The set has no member that means "generic" or "anything else": every
//! bit set names a tag the register's value may carry. The "I do not
//! know" element of the lattice is [`TagSet::ALL`], the set of all
//! eleven tags.

use core::fmt;

use crate::value::{self, name, TAG_COUNT};

/// Every bit the set may have on: the eleven tags, from
/// [`crate::value::NULL`] at bit 0 to [`crate::value::CHAIN`] at bit 10.
/// Bits 11 to 15 are reserved.
const VALID_BITS: u16 = (1u16 << TAG_COUNT) - 1;

/// A set of value tags, stored as one bit a tag in a 16-bit word. The
/// first eleven bits are the tags, in the order [`crate::value`] names
/// them; the five above are reserved and always zero.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TagSet(u16);

impl TagSet {
    /// The empty set: no value gets here, which is what is proven of
    /// code that is never reached. The lattice's bottom.
    pub const NONE: TagSet = TagSet(0);

    /// Every tag: nothing is known. The lattice's top.
    pub const ALL: TagSet = TagSet(VALID_BITS);

    /// The five bits no set has. A reader of stored bits refuses a word
    /// with any of them on ([`TagSet::from_bits`]).
    pub const RESERVED: u16 = !VALID_BITS;

    /// The set of [`crate::value::NULL`] alone.
    pub const NULL: TagSet = TagSet(1 << value::NULL);
    /// The set of [`crate::value::BOOL`] alone.
    pub const BOOL: TagSet = TagSet(1 << value::BOOL);
    /// The set of [`crate::value::INT`] alone.
    pub const INT: TagSet = TagSet(1 << value::INT);
    /// The set of [`crate::value::FLOAT`] alone.
    pub const FLOAT: TagSet = TagSet(1 << value::FLOAT);
    /// The set of [`crate::value::STR`] alone.
    pub const STR: TagSet = TagSet(1 << value::STR);
    /// The set of [`crate::value::LIST`] alone.
    pub const LIST: TagSet = TagSet(1 << value::LIST);
    /// The set of [`crate::value::MAP`] alone.
    pub const MAP: TagSet = TagSet(1 << value::MAP);
    /// The set of [`crate::value::FUNC`] alone.
    pub const FUNC: TagSet = TagSet(1 << value::FUNC);
    /// The set of [`crate::value::BUILTIN`] alone.
    pub const BUILTIN: TagSet = TagSet(1 << value::BUILTIN);
    /// The set of [`crate::value::MODULE`] alone.
    pub const MODULE: TagSet = TagSet(1 << value::MODULE);
    /// The set of [`crate::value::CHAIN`] alone.
    pub const CHAIN: TagSet = TagSet(1 << value::CHAIN);

    /// The set of one tag; `None` for a number that is not a tag.
    #[inline]
    pub const fn of(tag: u8) -> Option<TagSet> {
        if tag < TAG_COUNT {
            Some(TagSet(1u16 << tag))
        } else {
            None
        }
    }

    /// The set of every tag in either operand: bit-OR, which is
    /// commutative, associative and idempotent. The lattice's join.
    #[inline]
    pub const fn join(self, other: TagSet) -> TagSet {
        TagSet(self.0 | other.0)
    }

    /// Whether this set has a given tag. `false` for a number that is
    /// not a tag.
    #[inline]
    pub const fn has(self, tag: u8) -> bool {
        tag < TAG_COUNT && (self.0 & (1u16 << tag)) != 0
    }

    /// Whether this set has no tag.
    #[inline]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// How many tags are in this set.
    #[inline]
    pub const fn len(self) -> u32 {
        self.0.count_ones()
    }

    /// Every tag of this set is in `other`: the lattice's order.
    #[inline]
    pub const fn within(self, other: TagSet) -> bool {
        (self.0 & !other.0) == 0
    }

    /// The raw bits: the eleven tags at bits 0 to 10, the five reserved
    /// bits above at zero.
    #[inline]
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// Build a set from raw bits. `None` when any reserved bit is set.
    #[inline]
    pub const fn from_bits(bits: u16) -> Option<TagSet> {
        if (bits & !VALID_BITS) == 0 {
            Some(TagSet(bits))
        } else {
            None
        }
    }

    /// The two bytes a profile holds, little-endian: the byte format
    /// decision 0005 section 7 pins.
    #[inline]
    pub const fn to_le_bytes(self) -> [u8; 2] {
        self.0.to_le_bytes()
    }

    /// Read a set from two little-endian bytes. `None` when any
    /// reserved bit is set.
    #[inline]
    pub const fn from_le_bytes(bytes: [u8; 2]) -> Option<TagSet> {
        Self::from_bits(u16::from_le_bytes(bytes))
    }
}

impl fmt::Display for TagSet {
    /// `{int, float}`: the tag names in [`crate::value::name`]'s order,
    /// separated by `", "`, between braces. The empty set prints `{}`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("{")?;
        let mut first = true;
        let mut tag: u8 = 0;
        while tag < TAG_COUNT {
            if self.has(tag) {
                if !first {
                    f.write_str(", ")?;
                }
                first = false;
                if let Some(n) = name(tag) {
                    f.write_str(n)?;
                }
            }
            tag += 1;
        }
        f.write_str("}")
    }
}

impl fmt::Debug for TagSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::*;

    const EVERY_TAG: [u8; 11] = [
        value::NULL,
        value::BOOL,
        value::INT,
        value::FLOAT,
        value::STR,
        value::LIST,
        value::MAP,
        value::FUNC,
        value::BUILTIN,
        value::MODULE,
        value::CHAIN,
    ];

    #[test]
    fn each_tag_has_exactly_its_bit() {
        for &t in &EVERY_TAG {
            let s = TagSet::of(t).unwrap();
            assert_eq!(s.bits(), 1u16 << t);
            assert_eq!(s.len(), 1);
            assert!(s.has(t));
            for &other in &EVERY_TAG {
                if other != t {
                    assert!(!s.has(other));
                }
            }
        }
    }

    #[test]
    fn each_named_set_is_the_set_of_its_tag() {
        let named = [
            TagSet::NULL,
            TagSet::BOOL,
            TagSet::INT,
            TagSet::FLOAT,
            TagSet::STR,
            TagSet::LIST,
            TagSet::MAP,
            TagSet::FUNC,
            TagSet::BUILTIN,
            TagSet::MODULE,
            TagSet::CHAIN,
        ];
        for (set, &tag) in named.iter().zip(&EVERY_TAG) {
            assert_eq!(Some(*set), TagSet::of(tag));
        }
        // Together they are every tag, and no two share one.
        let mut all = TagSet::NONE;
        for set in named {
            assert!(all.join(set) != all, "a named set adds its tag");
            all = all.join(set);
        }
        assert_eq!(all, TagSet::ALL);
    }

    #[test]
    fn none_and_all_are_the_lattice_ends() {
        assert!(TagSet::NONE.is_empty());
        assert_eq!(TagSet::NONE.len(), 0);
        assert_eq!(TagSet::ALL.len(), TAG_COUNT as u32);
        // No reserved bit in ALL.
        assert_eq!(TagSet::ALL.bits() & !VALID_BITS, 0);
        for &t in &EVERY_TAG {
            assert!(TagSet::ALL.has(t));
            assert!(!TagSet::NONE.has(t));
        }
        // The default is the empty set.
        assert_eq!(TagSet::default(), TagSet::NONE);
    }

    #[test]
    fn join_is_bit_or_commutative_and_idempotent() {
        let a = TagSet::of(value::INT).unwrap();
        let b = TagSet::of(value::FLOAT).unwrap();
        let c = a.join(b);
        assert_eq!(c.bits(), a.bits() | b.bits());
        assert_eq!(a.join(b), b.join(a));
        assert_eq!(a.join(a), a);
        assert_eq!(TagSet::NONE.join(a), a);
        assert_eq!(TagSet::ALL.join(a), TagSet::ALL);
    }

    #[test]
    fn within_is_the_lattice_order() {
        let a = TagSet::of(value::INT).unwrap();
        let b = TagSet::of(value::FLOAT).unwrap();
        let ab = a.join(b);
        assert!(a.within(ab));
        assert!(b.within(ab));
        assert!(!ab.within(a));
        assert!(!ab.within(b));
        assert!(TagSet::NONE.within(a));
        assert!(a.within(TagSet::ALL));
        assert!(!TagSet::ALL.within(a));
        assert!(a.within(a));
    }

    #[test]
    fn from_bits_refuses_each_reserved_bit_and_takes_every_tag_set() {
        for bit in TAG_COUNT..16 {
            let bad = 1u16 << bit;
            assert_eq!(TagSet::from_bits(bad), None);
            assert_eq!(TagSet::from_bits(VALID_BITS | bad), None);
        }
        for bits in 0u16..=VALID_BITS {
            let s = TagSet::from_bits(bits).unwrap();
            assert_eq!(s.bits(), bits);
        }
    }

    #[test]
    fn bytes_round_trip() {
        for bits in 0u16..=VALID_BITS {
            let s = TagSet::from_bits(bits).unwrap();
            let bytes = s.to_le_bytes();
            assert_eq!(bytes, bits.to_le_bytes());
            assert_eq!(TagSet::from_le_bytes(bytes), Some(s));
        }
        // Reserved bit in bytes: refused.
        let bad = (1u16 << TAG_COUNT).to_le_bytes();
        assert_eq!(TagSet::from_le_bytes(bad), None);
    }

    #[test]
    fn display_of_known_sets() {
        assert_eq!(format!("{}", TagSet::NONE), "{}");
        let int = TagSet::of(value::INT).unwrap();
        assert_eq!(format!("{int}"), "{int}");
        let two = int.join(TagSet::of(value::FLOAT).unwrap());
        assert_eq!(format!("{two}"), "{int, float}");
        assert_eq!(
            format!("{}", TagSet::ALL),
            "{null, bool, int, float, string, list, map, function, builtin, module, chain}",
        );
        // Debug prints the same text.
        assert_eq!(format!("{int:?}"), "{int}");
    }

    #[test]
    fn of_refuses_numbers_that_are_not_tags() {
        assert_eq!(TagSet::of(TAG_COUNT), None);
        assert_eq!(TagSet::of(255), None);
    }

    #[test]
    fn a_number_that_is_not_a_tag_is_in_no_set() {
        for n in value::TAG_COUNT..=u8::MAX {
            assert!(TagSet::of(n).is_none(), "{n} is not a tag");
            assert!(!TagSet::ALL.has(n), "{n} is in no set");
            assert!(value::name(n).is_none(), "{n} has no name");
        }
    }

    #[test]
    fn the_reserved_bits_are_the_five_above_the_tags() {
        assert_eq!(TagSet::RESERVED.count_ones(), 5);
        assert_eq!(TagSet::RESERVED & TagSet::ALL.bits(), 0);
        assert_eq!(TagSet::RESERVED | TagSet::ALL.bits(), u16::MAX);
    }

    #[test]
    fn join_does_not_depend_on_how_three_sets_are_grouped() {
        let sets = [
            TagSet::NONE,
            TagSet::INT,
            TagSet::FLOAT.join(TagSet::STR),
            TagSet::MAP,
            TagSet::ALL,
        ];
        for a in sets {
            for b in sets {
                for c in sets {
                    assert_eq!(a.join(b).join(c), a.join(b.join(c)));
                }
            }
        }
    }

    #[test]
    fn every_set_prints_one_name_for_each_of_its_tags() {
        for bits in 0..=TagSet::ALL.bits() {
            let set = TagSet::from_bits(bits).unwrap();
            let text = set.to_string();
            assert!(text.starts_with('{') && text.ends_with('}'), "{text}");
            let inside = &text[1..text.len() - 1];
            let names: Vec<&str> = if inside.is_empty() {
                Vec::new()
            } else {
                inside.split(", ").collect()
            };
            assert_eq!(names.len() as u32, set.len(), "{text}");
            for (tag, name) in EVERY_TAG.iter().filter(|t| set.has(**t)).zip(&names) {
                assert_eq!(value::name(*tag), Some(*name), "{text}");
            }
        }
    }

    #[test]
    fn no_two_tags_share_a_name() {
        for &a in &EVERY_TAG {
            for &b in &EVERY_TAG {
                if a != b {
                    assert_ne!(value::name(a), value::name(b));
                }
            }
        }
    }
}
