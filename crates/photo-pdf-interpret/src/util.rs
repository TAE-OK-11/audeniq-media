//! A number of utility methods.

use kurbo::{BezPath, PathEl, Rect};
use std::hash::Hash;
use std::ops::Sub;

pub(crate) trait OptionLog {
    fn warn_none(self, f: &str) -> Self;
}

impl<T> OptionLog for Option<T> {
    #[inline]
    fn warn_none(self, _f: &str) -> Self {
        self.or_else(|| {
            warn!("{_f}");

            None
        })
    }
}

const SCALAR_NEARLY_ZERO: f32 = 1.0 / (1 << 8) as f32;

/// A number of useful methods for f32 numbers.
pub trait Float32Ext: Sized + Sub<f32, Output = f32> + Copy + PartialOrd<f32> {
    /// Whether the number is approximately 0.
    fn is_nearly_zero(&self) -> bool {
        self.is_nearly_zero_within_tolerance(SCALAR_NEARLY_ZERO)
    }

    /// Whether the number is nearly equal to another number.
    fn is_nearly_equal(&self, other: f32) -> bool {
        (*self - other).is_nearly_zero()
    }

    /// Whether the number is nearly equal to another number.
    fn is_nearly_less_or_equal(&self, other: f32) -> bool {
        (*self - other).is_nearly_zero() || *self < other
    }

    /// Whether the number is nearly equal to another number.
    fn is_nearly_greater_or_equal(&self, other: f32) -> bool {
        (*self - other).is_nearly_zero() || *self > other
    }

    /// Whether the number is approximately 0, with a given tolerance.
    fn is_nearly_zero_within_tolerance(&self, tolerance: f32) -> bool;
}

impl Float32Ext for f32 {
    fn is_nearly_zero_within_tolerance(&self, tolerance: f32) -> bool {
        debug_assert!(tolerance >= 0.0, "tolerance must be non-negative");

        self.abs() <= tolerance
    }
}

/// A number of useful methods for f64 numbers.
pub trait Float64Ext: Sized + Sub<f64, Output = f64> + Copy + PartialOrd<f64> {
    /// Whether the number is approximately 0.
    fn is_nearly_zero(&self) -> bool {
        self.is_nearly_zero_within_tolerance(SCALAR_NEARLY_ZERO as f64)
    }

    /// Whether the number is nearly equal to another number.
    fn is_nearly_equal(&self, other: f64) -> bool {
        (*self - other).is_nearly_zero()
    }

    /// Whether the number is nearly equal to another number.
    fn is_nearly_less_or_equal(&self, other: f64) -> bool {
        (*self - other).is_nearly_zero() || *self < other
    }

    /// Whether the number is nearly equal to another number.
    fn is_nearly_greater_or_equal(&self, other: f64) -> bool {
        (*self - other).is_nearly_zero() || *self > other
    }

    /// Whether the number is approximately 0, with a given tolerance.
    fn is_nearly_zero_within_tolerance(&self, tolerance: f64) -> bool;
}

impl Float64Ext for f64 {
    fn is_nearly_zero_within_tolerance(&self, tolerance: f64) -> bool {
        debug_assert!(tolerance >= 0.0, "tolerance must be non-negative");

        self.abs() <= tolerance
    }
}

pub(crate) trait PointExt: Sized {
    fn x(&self) -> f32;
    fn y(&self) -> f32;

    fn nearly_same(&self, other: Self) -> bool {
        self.x().is_nearly_equal(other.x()) && self.y().is_nearly_equal(other.y())
    }
}

impl PointExt for kurbo::Point {
    fn x(&self) -> f32 {
        self.x as f32
    }

    fn y(&self) -> f32 {
        self.y as f32
    }
}

/// Calculate a 128-bit siphash of a value (SipHash-1-3, 128-bit output, zero
/// key; the same values as `siphasher::sip128::SipHasher13`).
pub(crate) fn hash128<T: Hash + ?Sized>(value: &T) -> u128 {
    let mut state = SipHasher13::new();
    value.hash(&mut state);
    state.finish128()
}

/// SipHash-1-3 with the 128-bit finalization. Integers are hashed through
/// `write` as native-endian bytes, which matches the reference on the
/// little-endian targets this crate builds for.
struct SipHasher13 {
    v: [u64; 4],
    tail: u64,
    ntail: usize,
    length: usize,
}

impl SipHasher13 {
    fn new() -> Self {
        Self {
            v: [
                0x736f6d6570736575,
                0x646f72616e646f83,
                0x6c7967656e657261,
                0x7465646279746573,
            ],
            tail: 0,
            ntail: 0,
            length: 0,
        }
    }

    #[inline]
    fn round(v: &mut [u64; 4]) {
        v[0] = v[0].wrapping_add(v[1]);
        v[1] = v[1].rotate_left(13) ^ v[0];
        v[0] = v[0].rotate_left(32);
        v[2] = v[2].wrapping_add(v[3]);
        v[3] = v[3].rotate_left(16) ^ v[2];
        v[0] = v[0].wrapping_add(v[3]);
        v[3] = v[3].rotate_left(21) ^ v[0];
        v[2] = v[2].wrapping_add(v[1]);
        v[1] = v[1].rotate_left(17) ^ v[2];
        v[2] = v[2].rotate_left(32);
    }

    #[inline]
    fn compress(&mut self, m: u64) {
        self.v[3] ^= m;
        Self::round(&mut self.v);
        self.v[0] ^= m;
    }

    fn finish128(&self) -> u128 {
        let mut v = self.v;
        let b = ((self.length as u64 & 0xff) << 56) | self.tail;
        v[3] ^= b;
        Self::round(&mut v);
        v[0] ^= b;
        v[2] ^= 0xee;
        for _ in 0..3 {
            Self::round(&mut v);
        }
        let h1 = v[0] ^ v[1] ^ v[2] ^ v[3];
        v[1] ^= 0xdd;
        for _ in 0..3 {
            Self::round(&mut v);
        }
        let h2 = v[0] ^ v[1] ^ v[2] ^ v[3];
        u128::from(h1) | (u128::from(h2) << 64)
    }
}

impl std::hash::Hasher for SipHasher13 {
    fn write(&mut self, mut msg: &[u8]) {
        self.length += msg.len();
        if self.ntail != 0 {
            let take = (8 - self.ntail).min(msg.len());
            for (i, b) in msg[..take].iter().enumerate() {
                self.tail |= u64::from(*b) << (8 * (self.ntail + i));
            }
            self.ntail += take;
            msg = &msg[take..];
            if self.ntail < 8 {
                return;
            }
            self.compress(self.tail);
            self.tail = 0;
            self.ntail = 0;
        }
        let mut words = msg.chunks_exact(8);
        for w in &mut words {
            self.compress(u64::from_le_bytes(w.try_into().unwrap()));
        }
        for (i, b) in words.remainder().iter().enumerate() {
            self.tail |= u64::from(*b) << (8 * i);
        }
        self.ntail = words.remainder().len();
    }

    fn finish(&self) -> u64 {
        (self.finish128() >> 64) as u64
    }
}

pub(crate) trait BezPathExt {
    fn fast_bounding_box(&self) -> Rect;
}

impl BezPathExt for BezPath {
    fn fast_bounding_box(&self) -> Rect {
        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut max_y = f64::NEG_INFINITY;

        let mut include = |x: f64, y: f64| {
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        };

        for el in self.elements() {
            match *el {
                PathEl::MoveTo(p) | PathEl::LineTo(p) => include(p.x, p.y),
                PathEl::QuadTo(p1, p2) => {
                    include(p1.x, p1.y);
                    include(p2.x, p2.y);
                }
                PathEl::CurveTo(p1, p2, p3) => {
                    include(p1.x, p1.y);
                    include(p2.x, p2.y);
                    include(p3.x, p3.y);
                }
                PathEl::ClosePath => {}
            }
        }

        if min_x > max_x {
            Rect::ZERO
        } else {
            Rect::new(min_x, min_y, max_x, max_y)
        }
    }
}

/// Extension methods for converting a [`photo_pdf_syntax::transform::Transform`] to a [`kurbo::Affine`].
pub trait TransformExt {
    /// Convert to a `kurbo::Affine`.
    fn to_kurbo(&self) -> kurbo::Affine;
}

impl TransformExt for photo_pdf_syntax::transform::Transform {
    fn to_kurbo(&self) -> kurbo::Affine {
        kurbo::Affine::new(self.as_coeffs())
    }
}

/// Extension methods for rectangles.
pub trait RectExt {
    /// Convert the rectangle to a `kurbo` rectangle.
    fn to_kurbo(&self) -> Rect;
}

impl RectExt for photo_pdf_syntax::object::Rect {
    fn to_kurbo(&self) -> Rect {
        Rect::new(self.x0, self.y0, self.x1, self.y1)
    }
}

/// Return the vectors by which a point in the transformed space moves when
/// advancing by one unit in the x or y direction, ignoring translation.
pub fn x_y_advances(transform: &kurbo::Affine) -> (kurbo::Vec2, kurbo::Vec2) {
    let scale_skew_transform = {
        let c = transform.as_coeffs();
        kurbo::Affine::new([c[0], c[1], c[2], c[3], 0.0, 0.0])
    };

    let x_advance = scale_skew_transform * kurbo::Point::new(1.0, 0.0);
    let y_advance = scale_skew_transform * kurbo::Point::new(0.0, 1.0);

    (
        kurbo::Vec2::new(x_advance.x, x_advance.y),
        kurbo::Vec2::new(y_advance.x, y_advance.y),
    )
}

#[cfg(test)]
mod tests {
    use super::hash128;

    /// Reference values from `siphasher` 1.0.4 `sip128::SipHasher13`.
    #[test]
    fn hash128_matches_siphasher() {
        let bytes: [(&[u8], u128); 4] = [
            (b"", 0x9ddcabcca2183c5fec4827ee54e8eed6),
            (b"a", 0x97f2a473ba79840c3dcb2dc54f1a5652),
            (b"abcdefg", 0xa79b74dac3df258dfdc8992f71b776cf),
            (b"abcdefgh", 0x1b081bbd879867e84fd7859d2f47bfff),
        ];
        for (input, expected) in bytes {
            assert_eq!(hash128(input), expected, "{input:?}");
        }
        let long: Vec<u8> = (0u8..=200).collect();
        assert_eq!(hash128(long.as_slice()), 0x50fd5251d022b61af7482f33aa08f9fc);
        let tuple = (
            1u8,
            2u16,
            3u32,
            4u64,
            5usize,
            "str",
            (-1.5f64).to_bits(),
            7u128,
            [9u8; 13],
            -3i32,
        );
        assert_eq!(hash128(&tuple), 0xad5c6060e169e89c195abb599254eb05);
        let nested = (hash128(&42u32), vec!["x".to_string(), "yz".to_string()]);
        assert_eq!(hash128(&nested), 0x7246631afdada83671ab34f867862888);
    }
}
