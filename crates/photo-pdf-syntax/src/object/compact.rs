//! Owned bytes of names and strings: up to 22 bytes inline, longer ones on
//! the heap, in 24 bytes whatever features other crates enable.
//!
//! `SmallVec<[u8; 23]>` did this until a dependency turned on smallvec's
//! `union` feature for the whole build, which removes the niche that kept
//! `Name` and `String` at 32 bytes (and `Object` at 40): they grew to 40 and
//! 48. Hex strings are always owned and frequent in text-heavy content, so
//! short values stay off the heap.
use core::ops::Deref;

const INLINE: usize = 22;

#[derive(Clone)]
pub(crate) enum Compact {
    Inline(u8, [u8; INLINE]),
    Heap(Box<[u8]>),
}

impl Compact {
    pub(crate) fn new(bytes: &[u8]) -> Self {
        if bytes.len() <= INLINE {
            let mut a = [0; INLINE];
            a[..bytes.len()].copy_from_slice(bytes);
            Self::Inline(bytes.len() as u8, a)
        } else {
            Self::Heap(bytes.into())
        }
    }
}

impl From<Vec<u8>> for Compact {
    fn from(v: Vec<u8>) -> Self {
        if v.len() <= INLINE {
            Self::new(&v)
        } else {
            Self::Heap(v.into_boxed_slice())
        }
    }
}

impl Deref for Compact {
    type Target = [u8];

    #[inline]
    fn deref(&self) -> &[u8] {
        match self {
            Self::Inline(n, a) => &a[..usize::from(*n)],
            Self::Heap(b) => b,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_and_heap_round_trip() {
        for n in [0, 1, 21, 22, 23, 100] {
            let v: Vec<u8> = (0..n as u8).collect();
            assert_eq!(&*Compact::new(&v), &v[..]);
            assert_eq!(&*Compact::from(v.clone()), &v[..]);
        }
        assert_eq!(size_of::<Compact>(), 24);
    }
}
