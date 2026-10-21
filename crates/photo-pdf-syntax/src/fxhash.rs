//! FxHash (`rustc-hash` 2.1.3's `FxHasher`, the same values on 64-bit
//! targets): a fast, non-cryptographic hasher for the parser's and renderer's
//! in-memory maps. Ported from rustc-hash (MIT OR Apache-2.0).

use core::hash::{BuildHasher, Hasher};
use std::collections::{HashMap, HashSet};

/// A `HashMap` using [`FxHasher`].
pub type FxHashMap<K, V> = HashMap<K, V, FxBuildHasher>;
/// A `HashSet` using [`FxHasher`].
pub type FxHashSet<V> = HashSet<V, FxBuildHasher>;

/// Builds [`FxHasher`]s.
#[derive(Copy, Clone, Default)]
pub struct FxBuildHasher;

impl BuildHasher for FxBuildHasher {
    type Hasher = FxHasher;
    fn build_hasher(&self) -> FxHasher {
        FxHasher::default()
    }
}

/// The FxHash hasher.
#[derive(Clone, Default)]
pub struct FxHasher {
    hash: usize,
}

#[cfg(target_pointer_width = "64")]
const K: usize = 0xf1357aea2e62a9c5;
#[cfg(target_pointer_width = "32")]
const K: usize = 0x93d765dd;

impl FxHasher {
    #[inline]
    fn add_to_hash(&mut self, i: usize) {
        self.hash = self.hash.wrapping_add(i).wrapping_mul(K);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        self.write_u64(hash_bytes(bytes));
    }

    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add_to_hash(i as usize);
    }

    #[inline]
    fn write_u16(&mut self, i: u16) {
        self.add_to_hash(i as usize);
    }

    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add_to_hash(i as usize);
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add_to_hash(i as usize);
        #[cfg(target_pointer_width = "32")]
        self.add_to_hash((i >> 32) as usize);
    }

    #[inline]
    fn write_u128(&mut self, i: u128) {
        self.add_to_hash(i as usize);
        #[cfg(target_pointer_width = "32")]
        self.add_to_hash((i >> 32) as usize);
        self.add_to_hash((i >> 64) as usize);
        #[cfg(target_pointer_width = "32")]
        self.add_to_hash((i >> 96) as usize);
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add_to_hash(i);
    }

    #[inline]
    fn finish(&self) -> u64 {
        #[cfg(target_pointer_width = "64")]
        const ROTATE: u32 = 26;
        #[cfg(target_pointer_width = "32")]
        const ROTATE: u32 = 15;
        self.hash.rotate_left(ROTATE) as u64
    }
}

const SEED1: u64 = 0x243f6a8885a308d3;
const SEED2: u64 = 0x13198a2e03707344;
const PREVENT_TRIVIAL_ZERO_COLLAPSE: u64 = 0xa4093822299f31d0;

#[inline]
fn multiply_mix(x: u64, y: u64) -> u64 {
    if cfg!(any(
        target_pointer_width = "64",
        target_arch = "aarch64",
        target_arch = "x86_64"
    )) {
        let full = (x as u128).wrapping_mul(y as u128);
        (full as u64) ^ ((full >> 64) as u64)
    } else {
        let (lx, ly) = (x as u32, y as u32);
        let (hx, hy) = ((x >> 32) as u32, (y >> 32) as u32);
        let afull = (lx as u64).wrapping_mul(hy as u64);
        let bfull = (hx as u64).wrapping_mul(ly as u64);
        afull ^ bfull.rotate_right(32)
    }
}

#[inline]
fn hash_bytes(bytes: &[u8]) -> u64 {
    let len = bytes.len();
    let mut s0 = SEED1;
    let mut s1 = SEED2;
    if len <= 16 {
        if len >= 8 {
            s0 ^= u64::from_le_bytes(bytes[0..8].try_into().unwrap());
            s1 ^= u64::from_le_bytes(bytes[len - 8..].try_into().unwrap());
        } else if len >= 4 {
            s0 ^= u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as u64;
            s1 ^= u32::from_le_bytes(bytes[len - 4..].try_into().unwrap()) as u64;
        } else if len > 0 {
            let (lo, mid, hi) = (bytes[0], bytes[len / 2], bytes[len - 1]);
            s0 ^= lo as u64;
            s1 ^= ((hi as u64) << 8) | mid as u64;
        }
    } else {
        let mut bulk = &bytes[..(len - 1)];
        while let Some((chunk, rest)) = bulk.split_first_chunk::<16>() {
            let x = u64::from_le_bytes(chunk[..8].try_into().unwrap());
            let y = u64::from_le_bytes(chunk[8..].try_into().unwrap());
            let t = multiply_mix(s0 ^ x, PREVENT_TRIVIAL_ZERO_COLLAPSE ^ y);
            s0 = s1;
            s1 = t;
            bulk = rest;
        }
        let suffix = &bytes[len - 16..];
        s0 ^= u64::from_le_bytes(suffix[0..8].try_into().unwrap());
        s1 ^= u64::from_le_bytes(suffix[8..16].try_into().unwrap());
    }
    multiply_mix(s0, s1) ^ (len as u64)
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::FxBuildHasher;
    use core::hash::BuildHasher;

    /// Reference values from `rustc-hash` 2.1.3 `FxBuildHasher`.
    #[test]
    fn matches_rustc_hash() {
        let b = FxBuildHasher;
        let long: Vec<u8> = (0u8..=200).collect();
        assert_eq!(b.hash_one(1u8), 0xa8b98aa717c4d5eb);
        assert_eq!(b.hash_one(0x1234_5678u32), 0x54e52f0961dcc295);
        assert_eq!(b.hash_one(u64::MAX), 0x57467558ec3b2a14);
        assert_eq!(b.hash_one(7u128 << 70), 0x44b2a46300765c67);
        assert_eq!(b.hash_one("ab"), 0x806562b06f9d42f3);
        assert_eq!(b.hash_one("abcdefghijklmnopq"), 0xda640f91de47c46f);
        assert_eq!(b.hash_one(long.as_slice()), 0x255f2a08920598f8);
        assert_eq!(b.hash_one(("abc", 5usize, -2i16)), 0x09a89077af08a8be);
        assert_eq!(
            b.hash_one(&[1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10][..]),
            0x14d1b341861ced13
        );
    }
}
