// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! What native code knows of a value in a frame (interface 2): a
//! register's size and alignment, where its tag is, where its number
//! or bool payload is, and the tags of the four values native code
//! writes inline.
//!
//! A runtime's value type agrees with these constants when the tag byte
//! of its `null`, its `true`, an `int(i)` and a `float(f)` are
//! [`NULL`], [`BOOL`], [`INT`] and [`FLOAT`], and the payload at
//! [`PAYLOAD`] is `1`, the int's `i64` and the float's `f64`
//! respectively. [`agrees`] checks that.

/// A register's size: the distance from one register of a frame to the
/// next.
pub const SIZE: usize = 24;

/// Where a value's tag is, in a register: one byte.
pub const TAG: usize = 0;

/// Where an int's `i64`, a float's `f64` and a bool's byte (0 or 1)
/// are, in a register.
pub const PAYLOAD: usize = 8;

/// The tag of `null`. Its payload is not read.
pub const NULL: u8 = 0;

/// The tag of a bool. Its payload is one byte, 0 or 1.
pub const BOOL: u8 = 1;

/// The tag of a 64-bit signed integer. Its payload is the int's `i64`.
pub const INT: u8 = 2;

/// The tag of a 64-bit IEEE 754 float. Its payload is the float's
/// `f64` bits.
pub const FLOAT: u8 = 3;

/// The first tag of a value that owns memory. Native code never writes
/// over a register whose tag is this or more, and never copies such a
/// value.
pub const OWNS: u8 = 4;

/// A value type agrees with the constants of this module. Given the
/// type's `null`, its `true`, its `int(i)` and its `float(f)`: it reads
/// the tag byte of each, the payload byte a bool initializes, and the
/// payload words an int and a float initialize, and nothing else (no
/// padding byte is read).
///
/// Call with a type of size [`SIZE`] and alignment `8`: the runtime's
/// own value type, pinned to those by compile-time assertions. The
/// function returns `false` for a smaller type ([`PAYLOAD`] + 8 bytes
/// at least are read).
pub fn agrees<T>(null: &T, yes: &T, int: (&T, i64), float: (&T, f64)) -> bool {
    if core::mem::size_of::<T>() < PAYLOAD + 8 {
        return false;
    }
    // SAFETY: each argument is a shared reference to a `T` of at least
    // PAYLOAD + 8 = 16 bytes (checked above). TAG = 0 is in range and
    // reads one byte through `*const u8`, which has no alignment
    // requirement. PAYLOAD = 8 is in range and reads one byte (the
    // bool) or eight bytes (the int and the float), byte by byte
    // through `*const u8`. Every byte read is initialized by the
    // caller's value: the tag byte of the discriminant for the four
    // values, the bool's payload byte, and the int's and the float's
    // eight payload bytes. No padding byte is read, so an
    // uninitialized byte of the type is never read. The reads borrow
    // each value for the length of the read, and the references
    // outlive the call.
    #[allow(unsafe_code)]
    unsafe {
        let tag_of = |v: &T| -> u8 { *(v as *const T as *const u8).add(TAG) };
        let byte_at = |v: &T, at: usize| -> u8 { *(v as *const T as *const u8).add(at) };
        if tag_of(null) != NULL {
            return false;
        }
        if tag_of(yes) != BOOL {
            return false;
        }
        if byte_at(yes, PAYLOAD) != 1 {
            return false;
        }
        let (int_val, int_bits) = int;
        if tag_of(int_val) != INT {
            return false;
        }
        let mut bytes = [0u8; 8];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = byte_at(int_val, PAYLOAD + i);
        }
        if i64::from_ne_bytes(bytes) != int_bits {
            return false;
        }
        let (float_val, float_bits) = float;
        if tag_of(float_val) != FLOAT {
            return false;
        }
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = byte_at(float_val, PAYLOAD + i);
        }
        if f64::from_ne_bytes(bytes).to_bits() != float_bits.to_bits() {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Pinned to size 24 by a dummy variant whose payload is 16 bytes,
    // so the `repr(C, u8)` layout matches cig's own `Value`.
    #[repr(C, u8)]
    #[derive(Clone)]
    #[allow(dead_code)]
    enum Good {
        Null = 0,
        Bool(bool) = 1,
        Int(i64) = 2,
        Float(f64) = 3,
        // Forces the enum's size to be (1 + padding + 16) = 24 bytes.
        Big([u64; 2]) = 4,
    }

    #[test]
    fn the_test_type_has_the_runtimes_shape() {
        assert_eq!(core::mem::size_of::<Good>(), SIZE);
        assert_eq!(core::mem::align_of::<Good>(), 8);
    }

    #[test]
    fn a_type_that_agrees() {
        assert!(agrees(
            &Good::Null,
            &Good::Bool(true),
            (&Good::Int(123_456_789), 123_456_789),
            (&Good::Float(2.5_f64), 2.5_f64),
        ));
        for &f in &[0.0_f64, -0.0, f64::INFINITY, -f64::INFINITY, f64::NAN] {
            assert!(agrees(
                &Good::Null,
                &Good::Bool(true),
                (&Good::Int(i64::MIN), i64::MIN),
                (&Good::Float(f), f),
            ));
        }
    }

    #[test]
    fn a_type_that_is_too_small_is_refused() {
        #[repr(C, u8)]
        #[derive(Clone)]
        #[allow(dead_code)]
        enum Tiny {
            Null = 0,
            Bool(bool) = 1,
            // Smallest type the discriminants still fit in: 2 bytes.
            Other(u8) = 2,
            Else(u8) = 3,
        }
        // size_of::<Tiny>() is well under PAYLOAD + 8.
        assert!(core::mem::size_of::<Tiny>() < PAYLOAD + 8);
        assert!(!agrees(
            &Tiny::Null,
            &Tiny::Bool(true),
            (&Tiny::Other(0), 0),
            (&Tiny::Else(0), 0.0),
        ));
    }

    #[test]
    fn a_wrong_integer_or_float_is_seen() {
        // The int value's bits say another number than the one passed.
        assert!(!agrees(
            &Good::Null,
            &Good::Bool(true),
            (&Good::Int(1), 2),
            (&Good::Float(0.0), 0.0),
        ));
        assert!(!agrees(
            &Good::Null,
            &Good::Bool(true),
            (&Good::Int(0), 0),
            (&Good::Float(1.0), 2.0),
        ));
    }

    #[test]
    fn a_type_whose_tags_differ_is_refused() {
        // Two orders where the tags do not line up. Padded to 24 bytes.
        #[repr(C, u8)]
        #[derive(Clone)]
        #[allow(dead_code)]
        enum Swapped {
            Null = 0,
            Bool(bool) = 1,
            // Int and Float swapped: `agrees` reads 2 for an Int and
            // finds a Float there.
            Float(f64) = 2,
            Int(i64) = 3,
            Big([u64; 2]) = 4,
        }
        assert_eq!(core::mem::size_of::<Swapped>(), SIZE);
        assert!(!agrees(
            &Swapped::Null,
            &Swapped::Bool(true),
            // The int's tag byte will be 3, not INT (2).
            (&Swapped::Int(1), 1),
            // The float's tag byte will be 2, not FLOAT (3).
            (&Swapped::Float(1.0), 1.0),
        ));
    }
}
