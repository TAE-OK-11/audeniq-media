//! Streaming MD5, SHA-256, IEEE CRC-32 and the FLAC CRC-16/CRC-8 shared by
//! the audio engine (audeniq-qc), the media codecs and the image/PDF crates
//! (audeniq-photo).
//!
//! Each hash selects a hardware kernel at runtime (SHA-NI / AArch64 SHA2,
//! PCLMULQDQ / VPCLMULQDQ / PMULL) and falls back to portable code. Without
//! the `std` feature there is no runtime CPU detection and the portable
//! kernels are used.
#![cfg_attr(not(feature = "std"), no_std)]
#![deny(unsafe_code)]

#[cfg(test)]
extern crate std;

#[allow(unsafe_code)]
mod crc16;
#[allow(unsafe_code)]
mod crc32;
mod crc8;
#[cfg(target_arch = "x86_64")]
#[allow(unsafe_code)]
mod fused;
mod md5;
#[allow(unsafe_code)]
mod sha256;

pub use crc16::crc16;
pub use crc32::{crc32, Crc32};
pub use crc8::crc8;
pub use md5::Md5;
pub use sha256::Sha256;

/// Hash `sha_bytes` into `sha` and `md5_bytes` into `md5`; the digests are
/// identical to updating each hash separately.
///
/// Both are serial per-block chains. When the SHA extensions are in use,
/// runs of whole blocks are compressed by kernels that interleave one MD5
/// block with two SHA blocks (`bits` 16) or three MD5 blocks with four SHA
/// blocks (otherwise), so the core overlaps the two chains. This is the
/// ratio of compact 16/24-bit PCM (MD5, as in FLAC STREAMINFO) to
/// left-aligned 32-bit PCM (SHA-256).
pub fn update_both(sha: &mut Sha256, sha_bytes: &[u8], md5: &mut Md5, md5_bytes: &[u8], bits: u16) {
    let (sha_blocks, sha_tail) = sha.absorb_head(sha_bytes).as_chunks::<64>();
    let (md5_blocks, md5_tail) = md5.absorb_head(md5_bytes).as_chunks::<64>();
    #[cfg(target_arch = "x86_64")]
    let (sha_blocks, md5_blocks) = interleave(sha, sha_blocks, md5, md5_blocks, bits);
    #[cfg(not(target_arch = "x86_64"))]
    let _ = bits;
    sha.absorb_blocks(sha_blocks);
    sha.absorb_tail(sha_tail);
    md5.absorb_blocks(md5_blocks);
    md5.absorb_tail(md5_tail);
}

/// Compress the longest run of whole interleaved units with the fused
/// kernels (SHA extensions only) and return the blocks left for each hash.
#[cfg(target_arch = "x86_64")]
fn interleave<'a, 'b>(
    sha: &mut Sha256,
    sha_blocks: &'a [[u8; 64]],
    md5: &mut Md5,
    md5_blocks: &'b [[u8; 64]],
    bits: u16,
) -> (&'a [[u8; 64]], &'b [[u8; 64]]) {
    // (MD5 blocks, SHA blocks) per interleaved unit.
    let (m, s) = if bits == 16 { (1, 2) } else { (3, 4) };
    let units = (md5_blocks.len() / m).min(sha_blocks.len() / s);
    let Some(state) = sha.sha_ni_state().filter(|_| units > 0) else {
        return (sha_blocks, md5_blocks);
    };
    let (md5_run, md5_rest) = md5_blocks.split_at(units * m);
    let (sha_run, sha_rest) = sha_blocks.split_at(units * s);
    // SAFETY: `sha_ni_state` is Some only when the SHA extensions (and
    // SSSE3/SSE4.1) were detected at runtime.
    #[allow(unsafe_code)]
    unsafe {
        if bits == 16 {
            fused::md5_1_sha_2(md5.state_mut(), md5_run, state, sha_run);
        } else {
            fused::md5_3_sha_4(md5.state_mut(), md5_run, state, sha_run);
        }
    }
    (sha_rest, md5_rest)
}

#[cfg(test)]
fn hex(bytes: &[u8]) -> std::string::String {
    use core::fmt::Write;
    let mut result = std::string::String::new();
    for byte in bytes {
        write!(&mut result, "{byte:02x}").unwrap();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    /// Any split of the two streams gives the separately computed digests,
    /// for both interleaving ratios.
    #[test]
    fn update_both_equals_separate_hashes() {
        let mut seed = 0x2545f491u32;
        let data: Vec<u8> = (0..20_000)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as u8
            })
            .collect();
        for (bits, md5_len) in [(16u16, 10_000usize), (24, 15_000)] {
            let (sha_bytes, md5_bytes) = (&data[..], &data[..md5_len]);
            let mut sha = Sha256::new();
            sha.update(sha_bytes);
            let mut md5 = Md5::new();
            md5.update(md5_bytes);
            let expected = (sha.finalize(), md5.finalize());
            for parts in [1usize, 2, 3, 7, 64, 500] {
                let (mut sha, mut md5) = (Sha256::new(), Md5::new());
                let (sha_step, md5_step) =
                    (sha_bytes.len().div_ceil(parts), md5_len.div_ceil(parts));
                for i in 0..parts {
                    let sha_part = &sha_bytes[(i * sha_step).min(sha_bytes.len())
                        ..((i + 1) * sha_step).min(sha_bytes.len())];
                    let md5_part =
                        &md5_bytes[(i * md5_step).min(md5_len)..((i + 1) * md5_step).min(md5_len)];
                    update_both(&mut sha, sha_part, &mut md5, md5_part, bits);
                }
                assert_eq!(
                    (sha.finalize(), md5.finalize()),
                    expected,
                    "bits {bits} parts {parts}"
                );
            }
        }
    }
}
