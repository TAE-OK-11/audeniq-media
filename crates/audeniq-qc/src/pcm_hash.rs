//! Canonical PCM SHA-256 and, optionally, the FLAC STREAMINFO MD5 of the
//! same samples in one traversal.
//!
//! SHA-256 covers left-aligned little-endian i32 samples (4 bytes each); the
//! MD5 covers compact little-endian samples (2 or 3 bytes each). Both go
//! through `media_hash::update_both`, which interleaves the two block chains
//! on SHA-NI; digests are identical to hashing the two byte streams
//! separately.
use media_hash::{Md5, Sha256};

pub(crate) struct PcmHash {
    sha: Sha256,
    md5: Option<Md5>,
    /// The MD5 will be the source's verified STREAMINFO MD5 (same PCM).
    adopted: bool,
    bits: u16,
    compact: Vec<u8>,
}

impl PcmHash {
    /// `bits` is the source depth (16 or 24); `md5` requests the FLAC MD5.
    pub(crate) fn new(bits: u16, md5: bool) -> Self {
        Self {
            sha: Sha256::new(),
            md5: md5.then(Md5::new),
            adopted: false,
            bits,
            compact: Vec::new(),
        }
    }

    pub(crate) fn update(&mut self, samples: &[i32]) {
        let canonical = crate::audio::pcm_bytes(samples);
        let Some(md5) = &mut self.md5 else {
            self.sha.update(&canonical);
            return;
        };
        crate::audio::compact_pcm(samples, self.bits, &mut self.compact);
        media_hash::update_both(&mut self.sha, &canonical, md5, &self.compact, self.bits);
    }

    pub(crate) fn sha_mut(&mut self) -> &mut Sha256 {
        &mut self.sha
    }
    pub(crate) fn wants_md5(&self) -> bool {
        self.md5.is_some() || self.adopted
    }
    /// The MD5 alone, when the SHA-256 was computed elsewhere.
    pub(crate) fn update_md5_only(&mut self, samples: &[i32]) {
        if let Some(md5) = &mut self.md5 {
            crate::audio::compact_pcm(samples, self.bits, &mut self.compact);
            md5.update(&self.compact);
        }
    }
    /// Use the source's STREAMINFO MD5 (of the same compact PCM, verified by
    /// its decoder) instead of computing it again. Only valid from the first
    /// update on, so nothing has been hashed into this MD5 yet.
    pub(crate) fn adopt_source_md5(&mut self) {
        if !self.adopted {
            debug_assert!(self.md5.as_ref().is_some_and(Md5::is_empty));
            self.md5 = None;
            self.adopted = true;
        }
    }
    pub(crate) fn finish(self) -> ([u8; 32], Option<[u8; 16]>) {
        debug_assert!(!self.adopted);
        (self.sha.finalize(), self.md5.map(Md5::finalize))
    }
    /// [`Self::finish`] where an adopted MD5 is the source's verified one.
    pub(crate) fn finish_with(
        self,
        verified_source_md5: Option<[u8; 16]>,
    ) -> crate::Result<([u8; 32], Option<[u8; 16]>)> {
        let md5 = if self.adopted {
            Some(verified_source_md5.ok_or(crate::Error::Invalid("unverified source MD5"))?)
        } else {
            self.md5.map(Md5::finalize)
        };
        Ok((self.sha.finalize(), md5))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Any chunking, both depths, with and without MD5: digests equal the
    /// separately computed SHA-256 of canonical bytes and MD5 of compact
    /// bytes (which the md5/sha256 tests check against independent code).
    #[test]
    fn fused_digests_equal_separate_hashes_for_any_chunking() {
        let mut seed = 0x2545f491u32;
        for bits in [16u16, 24] {
            let shift = 32 - bits as u32;
            let samples: Vec<i32> = (0..40_000)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    ((seed as i32) >> shift) << shift
                })
                .collect();
            let mut sha = Sha256::new();
            sha.update(crate::audio::pcm_bytes(&samples));
            let mut compact = Vec::new();
            crate::audio::compact_pcm(&samples, bits, &mut compact);
            let mut md5 = Md5::new();
            md5.update(&compact);
            let expected = (sha.finalize(), md5.finalize());
            for chunk in [1usize, 2, 3, 7, 31, 64, 96, 1000, 4608, 40_000] {
                for with_md5 in [true, false] {
                    let mut hash = PcmHash::new(bits, with_md5);
                    for part in samples.chunks(chunk) {
                        hash.update(part);
                    }
                    let (sha, md5) = hash.finish();
                    assert_eq!(sha, expected.0, "bits {bits} chunk {chunk}");
                    assert_eq!(
                        md5,
                        with_md5.then_some(expected.1),
                        "bits {bits} chunk {chunk}"
                    );
                }
            }
        }
    }
}
