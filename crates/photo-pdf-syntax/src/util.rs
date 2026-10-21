use crate::sync::OnceLock;
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ops::Sub;
use smallvec::{Array, SmallVec};

pub(crate) trait OptionLog {
    fn error_none(self, f: &str) -> Self;
}

impl<T> OptionLog for Option<T> {
    fn error_none(self, _f: &str) -> Self {
        self.or_else(|| {
            error!("{_f}");

            None
        })
    }
}

const SCALAR_NEARLY_ZERO: f32 = 1.0 / (1 << 12) as f32;

/// A number of useful methods for f32 numbers.
pub(crate) trait FloatExt: Sized + Sub<f32, Output = f32> + Copy {
    /// Whether the number is approximately 0.
    fn is_nearly_zero(&self) -> bool {
        self.is_nearly_zero_within_tolerance(SCALAR_NEARLY_ZERO)
    }

    /// Whether the number is approximately 0, with a given tolerance.
    fn is_nearly_zero_within_tolerance(&self, tolerance: f32) -> bool;
}

impl FloatExt for f32 {
    fn is_nearly_zero_within_tolerance(&self, tolerance: f32) -> bool {
        debug_assert!(tolerance >= 0.0, "tolerance must be non-negative");

        self.abs() <= tolerance
    }
}

pub(crate) trait ByteBuf: Default {
    fn with_capacity(capacity: usize) -> Self;
    fn push(&mut self, byte: u8);
}

impl ByteBuf for Vec<u8> {
    fn with_capacity(capacity: usize) -> Self {
        Self::with_capacity(capacity)
    }

    fn push(&mut self, byte: u8) {
        Self::push(self, byte);
    }
}

impl<A> ByteBuf for SmallVec<A>
where
    A: Array<Item = u8>,
{
    fn with_capacity(capacity: usize) -> Self {
        Self::with_capacity(capacity)
    }

    fn push(&mut self, byte: u8) {
        Self::push(self, byte);
    }
}

/// First position of `needle` in `haystack` (`Some(0)` for an empty needle).
///
/// Candidates are positions where the first two needle bytes both match,
/// tested sixteen positions at a time; only those are compared in full.
pub(crate) fn find_needle(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    let n = needle.len();
    let last_start = haystack.len().checked_sub(n)?;
    match n {
        0 => return Some(0),
        1 => return find_byte(haystack, needle[0]),
        _ => {}
    }
    let (p0, p1) = (LO128 * u128::from(needle[0]), LO128 * u128::from(needle[1]));
    let mut i = 0;
    // Each step reads haystack[i..i + 17] and tests starts i..i + 16.
    while i + 16 <= last_start && i + 17 <= haystack.len() {
        let w0 = u128::from_le_bytes(haystack[i..i + 16].try_into().unwrap());
        let w1 = u128::from_le_bytes(haystack[i + 1..i + 17].try_into().unwrap());
        let mut hits = zero_bytes128(w0 ^ p0) & zero_bytes128(w1 ^ p1);
        while hits != 0 {
            let at = i + (hits.trailing_zeros() / 8) as usize;
            if haystack[at..at + n] == *needle {
                return Some(at);
            }
            hits &= hits - 1;
        }
        i += 16;
    }
    (i..=last_start).find(|&at| haystack[at..at + n] == *needle)
}

/// Last position of `needle` in `haystack` (`Some(len)` for an empty needle).
pub(crate) fn findr_needle(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    let Some((&first, rest)) = needle.split_first() else {
        return Some(haystack.len());
    };
    let mut end = haystack.len().checked_sub(needle.len())? + 1;
    while end > 0 {
        let i = rfind_byte(&haystack[..end], first)?;
        if haystack[i + 1..i + needle.len()] == *rest {
            return Some(i);
        }
        end = i;
    }
    None
}

const LO: u64 = 0x0101_0101_0101_0101;
const HI: u64 = 0x8080_8080_8080_8080;

/// High bit set in every zero byte of `x` (bytes above a zero byte may be
/// flagged too; callers verify each candidate, and no zero byte is missed).
#[inline]
fn zero_bytes(x: u64) -> u64 {
    x.wrapping_sub(LO) & !x & HI
}

const LO128: u128 = u128::from_le_bytes([1; 16]);
const HI128: u128 = u128::from_le_bytes([0x80; 16]);

/// [`zero_bytes`] on sixteen bytes.
#[inline]
fn zero_bytes128(x: u128) -> u128 {
    x.wrapping_sub(LO128) & !x & HI128
}

/// Whether any byte of `word` equals the byte broadcast in `pattern`.
#[inline]
fn has_byte(word: u64, pattern: u64) -> bool {
    zero_bytes(word ^ pattern) != 0
}

/// First index of `byte`, eight bytes per step.
fn find_byte(haystack: &[u8], byte: u8) -> Option<usize> {
    let pattern = LO * u64::from(byte);
    let mut chunks = haystack.chunks_exact(8);
    let mut offset = 0;
    for chunk in &mut chunks {
        if has_byte(u64::from_le_bytes(chunk.try_into().unwrap()), pattern) {
            return chunk.iter().position(|&b| b == byte).map(|i| offset + i);
        }
        offset += 8;
    }
    chunks
        .remainder()
        .iter()
        .position(|&b| b == byte)
        .map(|i| offset + i)
}

/// Last index of `byte`, eight bytes per step.
fn rfind_byte(haystack: &[u8], byte: u8) -> Option<usize> {
    let pattern = LO * u64::from(byte);
    let mut chunks = haystack.rchunks_exact(8);
    let mut end = haystack.len();
    for chunk in &mut chunks {
        if has_byte(u64::from_le_bytes(chunk.try_into().unwrap()), pattern) {
            return chunk.iter().rposition(|&b| b == byte).map(|i| end - 8 + i);
        }
        end -= 8;
    }
    chunks.remainder().iter().rposition(|&b| b == byte)
}

/// Allows to store elements at an index with an `&self` reference.
///
/// This is similar to a thread-safe arena data structure, but with less moving
/// parts and no unsafe code. It's based on the segment list presented in
/// <https://matklad.github.io/2023/04/23/data-oriented-parallel-value-interner.html>
///
/// Indices should be used in order. Usage of higher indices implies more memory
/// usage (even if lower indices are not in use).
///
/// The capacity is limited at 2^C - 1. Calling `get_or_init` with a higher
/// index will lead to a panic.
pub(crate) struct SegmentList<T, const C: usize>([OnceLock<Box<[OnceLock<T>]>>; C]);

impl<T, const C: usize> SegmentList<T, C> {
    pub(crate) fn new() -> Self {
        Self(core::array::from_fn(|_| OnceLock::new()))
    }

    pub(crate) fn get(&self, i: usize) -> Option<&T> {
        let (s, k) = self.locate(i);
        let segment = self.0[s as usize].get()?;
        segment.get(k)?.get()
    }

    #[track_caller]
    pub(crate) fn get_or_init(&self, i: usize, f: impl FnOnce() -> T) -> &T {
        let (s, k) = self.locate(i);
        let segment = self
            .0
            .get(s as usize)
            .expect("segment list is out of capacity")
            .get_or_init(|| {
                (0..2_usize.pow(s))
                    .map(|_| OnceLock::new())
                    .collect::<Vec<_>>()
                    .into_boxed_slice()
            });
        segment[k].get_or_init(f)
    }

    fn locate(&self, i: usize) -> (u32, usize) {
        let power = (i + 2).next_power_of_two() / 2;
        let s = power.trailing_zeros();
        let k = i - ((1 << s) - 1);
        (s, k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_segment_list() {
        let e = SegmentList::<String, 10>::new();

        // Ensure that it works.
        for i in 0..500 {
            e.get_or_init(i, || format!("{i}"));
        }
        for i in 0..500 {
            assert_eq!(e.get(i), Some(&format!("{i}")));
        }

        // Ensure that all slots in the first 8 segments are actually in use,
        // i.e. we didn't overallocate.
        for s in 0..8 {
            assert!(e.0[s].get().unwrap().iter().all(|s| s.get().is_some()));
        }
    }

    #[test]
    fn test_findr_needle() {
        assert_eq!(findr_needle(b"abc foo def foo xyz", b"foo"), Some(12));
        assert_eq!(findr_needle(b"abc", b""), Some(3));
        assert_eq!(findr_needle(b"abc", b"abcd"), None);
        assert_eq!(findr_needle(b"abc", b"z"), None);
    }
}

#[cfg(test)]
mod needle_tests {
    use super::{find_needle, findr_needle};

    fn naive(h: &[u8], n: &[u8]) -> Option<usize> {
        if n.is_empty() {
            return Some(0);
        }
        h.windows(n.len()).position(|w| w == n)
    }

    fn naive_r(h: &[u8], n: &[u8]) -> Option<usize> {
        if n.is_empty() {
            return Some(h.len());
        }
        h.windows(n.len()).rposition(|w| w == n)
    }

    /// Same answers as a plain window search (and `memchr::memmem`), at
    /// every alignment and with partial matches near both ends.
    #[test]
    fn matches_window_search() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let needles: [&[u8]; 7] = [b"", b"E", b"EI", b"startxref", b"EEI", b"\xff\x00", b"II"];
        for len in 0..80 {
            for _ in 0..40 {
                let h: Vec<u8> = (0..len)
                    .map(|_| [b'E', b'I', b's', b't', b'x', 0xff, 0x00][(next() % 7) as usize])
                    .collect();
                for n in needles {
                    assert_eq!(find_needle(&h, n), naive(&h, n), "{h:?} {n:?}");
                    assert_eq!(findr_needle(&h, n), naive_r(&h, n), "{h:?} {n:?}");
                }
            }
        }
    }
}
