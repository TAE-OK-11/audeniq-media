//! Shortest round-trip decimal digits of an f64: Ryu's `d2d` (Ulf Adams,
//! "Ryū: fast float-to-string conversion", PLDI 2018; reference code
//! Apache-2.0 OR BSL-1.0), the algorithm serde_json wrote floats with.
//! An exact tie between the two closest shortest candidates goes to even.
use crate::ryu_tables::{DOUBLE_POW5_INV_SPLIT, DOUBLE_POW5_SPLIT};

const MANTISSA_BITS: u32 = 52;
const BIAS: i32 = 1023;
const POW5_INV_BITCOUNT: i32 = 125;
const POW5_BITCOUNT: i32 = 125;

/// ceil(log2(5^e)) for 0 <= e <= 3528 (1 for e = 0).
fn pow5bits(e: i32) -> i32 {
    (((e as u32) * 1217359) >> 19) as i32 + 1
}
/// floor(log10(2^e)) for 0 <= e <= 1650.
fn log10_pow2(e: i32) -> u32 {
    ((e as u32) * 78913) >> 18
}
/// floor(log10(5^e)) for 0 <= e <= 2620.
fn log10_pow5(e: i32) -> u32 {
    ((e as u32) * 732923) >> 20
}
fn pow5_factor(mut value: u64) -> u32 {
    let mut count = 0;
    while value.is_multiple_of(5) {
        value /= 5;
        count += 1;
    }
    count
}
fn multiple_of_pow5(value: u64, p: u32) -> bool {
    pow5_factor(value) >= p
}
fn multiple_of_pow2(value: u64, p: u32) -> bool {
    value & ((1u64 << p) - 1) == 0
}
/// (m * mul) >> j for the 128-bit `mul` and 64 < j < 192.
fn mul_shift(m: u64, mul: &[u64; 2], j: i32) -> u64 {
    let b0 = m as u128 * mul[0] as u128;
    let b2 = m as u128 * mul[1] as u128;
    (((b0 >> 64) + b2) >> (j - 64)) as u64
}

/// Shortest decimal `(digits, exponent)` with value = digits * 10^exponent
/// for a finite, nonzero f64 given its raw mantissa and exponent fields.
pub(crate) fn d2d(ieee_mantissa: u64, ieee_exponent: u32) -> (u64, i32) {
    let (e2, m2) = if ieee_exponent == 0 {
        (1 - BIAS - MANTISSA_BITS as i32 - 2, ieee_mantissa)
    } else {
        (
            ieee_exponent as i32 - BIAS - MANTISSA_BITS as i32 - 2,
            (1u64 << MANTISSA_BITS) | ieee_mantissa,
        )
    };
    let accept_bounds = m2 & 1 == 0;

    // The interval of valid decimal representations: [mm, mp] around mv.
    let mv = 4 * m2;
    let mm_shift = (ieee_mantissa != 0 || ieee_exponent <= 1) as u64;

    // Convert to a decimal power base with 128-bit arithmetic.
    let (mut vr, mut vp, mut vm);
    let e10;
    let mut vm_is_trailing_zeros = false;
    let mut vr_is_trailing_zeros = false;
    if e2 >= 0 {
        let q = log10_pow2(e2) - (e2 > 3) as u32;
        e10 = q as i32;
        let k = POW5_INV_BITCOUNT + pow5bits(q as i32) - 1;
        let i = -e2 + q as i32 + k;
        let mul = &DOUBLE_POW5_INV_SPLIT[q as usize];
        vr = mul_shift(4 * m2, mul, i);
        vp = mul_shift(4 * m2 + 2, mul, i);
        vm = mul_shift(4 * m2 - 1 - mm_shift, mul, i);
        if q <= 21 {
            // Only one of mp, mv and mm can be a multiple of 5, if any.
            if mv % 5 == 0 {
                vr_is_trailing_zeros = multiple_of_pow5(mv, q);
            } else if accept_bounds {
                vm_is_trailing_zeros = multiple_of_pow5(mv - 1 - mm_shift, q);
            } else {
                vp -= multiple_of_pow5(mv + 2, q) as u64;
            }
        }
    } else {
        let q = log10_pow5(-e2) - (-e2 > 1) as u32;
        e10 = q as i32 + e2;
        let i = -e2 - q as i32;
        let k = pow5bits(i) - POW5_BITCOUNT;
        let j = q as i32 - k;
        let mul = &DOUBLE_POW5_SPLIT[i as usize];
        vr = mul_shift(4 * m2, mul, j);
        vp = mul_shift(4 * m2 + 2, mul, j);
        vm = mul_shift(4 * m2 - 1 - mm_shift, mul, j);
        if q <= 1 {
            // mv = 4 * m2 always has at least two trailing zero bits.
            vr_is_trailing_zeros = true;
            if accept_bounds {
                // mm = mv - 1 - mm_shift has one trailing zero bit iff
                // mm_shift is 1.
                vm_is_trailing_zeros = mm_shift == 1;
            } else {
                // mp = mv + 2 always has at least one trailing zero bit.
                vp -= 1;
            }
        } else if q < 63 {
            vr_is_trailing_zeros = multiple_of_pow2(mv, q);
        }
    }

    // The shortest representation in the interval.
    let mut removed = 0i32;
    let output = if vm_is_trailing_zeros || vr_is_trailing_zeros {
        // General case (~0.7%).
        let mut last_removed_digit = 0u64;
        while vp / 10 > vm / 10 {
            vm_is_trailing_zeros &= vm % 10 == 0;
            vr_is_trailing_zeros &= last_removed_digit == 0;
            last_removed_digit = vr % 10;
            vr /= 10;
            vp /= 10;
            vm /= 10;
            removed += 1;
        }
        if vm_is_trailing_zeros {
            while vm % 10 == 0 {
                vr_is_trailing_zeros &= last_removed_digit == 0;
                last_removed_digit = vr % 10;
                vr /= 10;
                vm /= 10;
                removed += 1;
            }
        }
        if vr_is_trailing_zeros && last_removed_digit == 5 && vr % 2 == 0 {
            // Exactly .....50..0: round to even.
            last_removed_digit = 4;
        }
        vr + ((vr == vm && (!accept_bounds || !vm_is_trailing_zeros)) || last_removed_digit >= 5)
            as u64
    } else {
        // Common case (~99.3%). Removing k digits is allowed while
        // vp / 10^k > vm / 10^k, which holds for every smaller k too, so
        // they go 8, 4 and 2 at a time first (Ryu takes two at a time); the
        // removed digits are never exactly half here, so rounding up is
        // whether they are at least 5 * 10^(k-1).
        let mut round_up = false;
        for (k, scale) in [(8, 100_000_000u64), (4, 10_000), (2, 100)] {
            if vp / scale > vm / scale {
                round_up = vr % scale >= scale / 2;
                vr /= scale;
                vp /= scale;
                vm /= scale;
                removed += k;
            }
        }
        while vp / 10 > vm / 10 {
            round_up = vr % 10 >= 5;
            vr /= 10;
            vp /= 10;
            vm /= 10;
            removed += 1;
        }
        vr + (vr == vm || round_up) as u64
    };
    (output, e10 + removed)
}
