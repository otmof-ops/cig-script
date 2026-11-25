// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! What native code and its caller share for the length of one call.

use crate::loader::Helpers;

/// The 80 bytes native code and its caller share for one call from the
/// caller: the two counts native code spends, where it writes why it
/// left, and (for a body) the frame, the helper table and the host the
/// helpers find their host by.
///
/// The layout is part of the interface ([`INTERFACE`](crate::INTERFACE)):
/// native code reads and writes these fields by their offsets, which are
/// asserted below.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Context {
    /// The steps left before the step budget is spent. At each step mark
    /// native code takes one, and leaves with
    /// [`Exit::Budget`](crate::Exit) (an entry) or stops `At` the mark
    /// (a body) when that took it below zero. However it comes back,
    /// what it took from this is the steps it ran. `i64::MAX` when there
    /// is no budget.
    pub steps_left: i64,
    /// How deep native code's own calls may nest from here. An entry
    /// leaves with [`Exit::Depth`](crate::Exit) at a call that finds it
    /// at zero.
    pub depth_left: i64,
    /// Entries: on an exit, the index of the site it left from. Bodies
    /// do not write this field.
    pub site: u32,
    /// Entries: on an exit, bit 0 is set when a function on the way out
    /// was called from inside one of its caller's loops. The other bits
    /// are zero. Bodies do not write this field.
    pub flags: u32,
    /// Entries: on an exit that has one, the left operand's slot.
    /// Bodies: after [`Helpers::list`] returns 1, the address of the
    /// list's first element.
    pub left: u64,
    /// Entries: on an exit that has one, the right operand's slot.
    /// Bodies: after [`Helpers::list`] returns 1, how many elements the
    /// list has.
    pub right: u64,
    /// Bodies: the address of the frame's `R(0)`. Register `r` is at
    /// `frame + r * value::SIZE`. It is valid until the body's next
    /// helper call returns; after every helper call the body reads this
    /// field again. Null for an entry.
    pub frame: *mut u8,
    /// Bodies: the runtime's functions. Native code calls each through
    /// this table ([`Helpers`]). Null for an entry.
    pub helpers: *const Helpers,
    /// Bodies: what the helpers find their host by; native code never
    /// reads it. Null for an entry.
    pub host: *mut core::ffi::c_void,
    /// Bodies: the frame's loop nesting, as the last `steps` instruction
    /// set it. Zero for an entry.
    pub nest: u32,
    /// Zero.
    pub zero: u32,
    /// Bodies: the address of the function's site cells, one of eight
    /// bytes for each pc of its code: what a field instruction at that
    /// pc last found in a map that has a shape, the shape in the high
    /// half and the field's place among the map's values in the low.
    /// Zero until the runtime has found one there. Native code reads
    /// the cells; only the runtime writes them. Null for an entry.
    pub sites: *const u64,
}

impl Context {
    /// A context for one call: `steps_left` steps and calls nested
    /// `depth_left` deep, every other field null or zero.
    pub const fn new(steps_left: i64, depth_left: i64) -> Context {
        Context {
            steps_left,
            depth_left,
            site: 0,
            flags: 0,
            left: 0,
            right: 0,
            frame: core::ptr::null_mut(),
            helpers: core::ptr::null(),
            host: core::ptr::null_mut(),
            nest: 0,
            zero: 0,
            sites: core::ptr::null(),
        }
    }
}

// The layout native code is compiled against.
const _: () = {
    assert!(core::mem::size_of::<Context>() == 80);
    assert!(core::mem::align_of::<Context>() == 8);
    assert!(core::mem::offset_of!(Context, steps_left) == 0);
    assert!(core::mem::offset_of!(Context, depth_left) == 8);
    assert!(core::mem::offset_of!(Context, site) == 16);
    assert!(core::mem::offset_of!(Context, flags) == 20);
    assert!(core::mem::offset_of!(Context, left) == 24);
    assert!(core::mem::offset_of!(Context, right) == 32);
    assert!(core::mem::offset_of!(Context, frame) == 40);
    assert!(core::mem::offset_of!(Context, helpers) == 48);
    assert!(core::mem::offset_of!(Context, host) == 56);
    assert!(core::mem::offset_of!(Context, nest) == 64);
    assert!(core::mem::offset_of!(Context, zero) == 68);
    assert!(core::mem::offset_of!(Context, sites) == 72);
};

#[cfg(test)]
mod tests {
    use super::Context;

    #[test]
    fn a_new_context_holds_its_two_counts_and_nothing_else() {
        let c = Context::new(7, 3);
        assert_eq!((c.steps_left, c.depth_left), (7, 3));
        assert_eq!((c.site, c.flags, c.left, c.right), (0, 0, 0, 0));
        assert!(c.frame.is_null());
        assert!(c.helpers.is_null());
        assert!(c.host.is_null());
        assert_eq!((c.nest, c.zero), (0, 0));
        assert!(c.sites.is_null());
    }
}
