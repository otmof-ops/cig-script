// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! What native code and its caller share for the length of one call.

/// The 64 bytes native code and its caller share for one call from the
/// caller: the two counts native code spends, and where it writes why it
/// left.
///
/// The layout is part of the interface ([`INTERFACE`](crate::INTERFACE)):
/// native code reads and writes these fields by their offsets, which are
/// asserted below.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Context {
    /// The steps left before the step budget is spent. Native code takes
    /// one at each step mark and leaves with [`Exit::Budget`](crate::Exit)
    /// when it would go below zero. `i64::MAX` when there is no budget.
    pub steps_left: i64,
    /// How deep native code's own calls may nest from here. Native code
    /// leaves with [`Exit::Depth`](crate::Exit) at a call that finds it
    /// at zero. The caller sets it to no more than its stack has room
    /// for, at [`MAX_FRAME`](crate::MAX_FRAME) bytes a call.
    pub depth_left: i64,
    /// On an exit: the index of the site it left from.
    pub site: u32,
    /// On an exit: bit 0 is set when a function on the way out was
    /// called from inside one of its caller's loops. The other bits are
    /// zero.
    pub flags: u32,
    /// On an exit that has one: the left operand's slot.
    pub left: u64,
    /// On an exit that has one: the right operand's slot.
    pub right: u64,
    /// Zero.
    pub reserved: [u64; 3],
}

impl Context {
    /// A context for one call: `steps_left` steps and calls nested
    /// `depth_left` deep, and nothing written yet.
    pub const fn new(steps_left: i64, depth_left: i64) -> Context {
        Context {
            steps_left,
            depth_left,
            site: 0,
            flags: 0,
            left: 0,
            right: 0,
            reserved: [0; 3],
        }
    }
}

// The layout native code is compiled against.
const _: () = {
    assert!(core::mem::size_of::<Context>() == 64);
    assert!(core::mem::align_of::<Context>() == 8);
    assert!(core::mem::offset_of!(Context, steps_left) == 0);
    assert!(core::mem::offset_of!(Context, depth_left) == 8);
    assert!(core::mem::offset_of!(Context, site) == 16);
    assert!(core::mem::offset_of!(Context, flags) == 20);
    assert!(core::mem::offset_of!(Context, left) == 24);
    assert!(core::mem::offset_of!(Context, right) == 32);
    assert!(core::mem::offset_of!(Context, reserved) == 40);
};

#[cfg(test)]
mod tests {
    use super::Context;

    #[test]
    fn a_new_context_holds_its_two_counts_and_nothing_else() {
        let c = Context::new(7, 3);
        assert_eq!((c.steps_left, c.depth_left), (7, 3));
        assert_eq!((c.site, c.flags, c.left, c.right), (0, 0, 0, 0));
        assert_eq!(c.reserved, [0; 3]);
    }
}
