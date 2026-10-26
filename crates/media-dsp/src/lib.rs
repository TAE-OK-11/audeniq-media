//! Signal-processing kernels shared by the AUDENIQ media codecs: a split
//! complex FFT and the DCT-IV on it (AAC's MDCT, SBR's QMF banks, MP3's
//! synthesis), and a dot product (resampling filters).
//!
//! Every kernel is plain safe Rust. On x86-64 each also has a build with
//! AVX2 enabled, chosen at runtime ([`simd`]); AArch64 always has NEON, so
//! its one build is already vectorized. The builds give identical results.
#![deny(unsafe_code)]

pub mod fft;
#[allow(unsafe_code)]
pub mod simd;

pub use fft::{Dct4, Fft};
use simd::multiversion;

multiversion! {
    /// Σ a[i]·b[i] over the shorter length, in eight independent lanes (the
    /// same order on every machine).
    pub fn dot(a: &[f32], b: &[f32]) -> f32 = dot_body;
}

#[inline(always)]
fn dot_body(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let (a, b) = (&a[..n], &b[..n]);
    let ((a8, at), (b8, bt)) = (a.as_chunks::<8>(), b.as_chunks::<8>());
    let mut lanes = [0f32; 8];
    for (x, y) in a8.iter().zip(b8) {
        for i in 0..8 {
            lanes[i] += x[i] * y[i];
        }
    }
    let tail: f32 = at.iter().zip(bt).map(|(x, y)| x * y).sum();
    ((lanes[0] + lanes[4]) + (lanes[1] + lanes[5]))
        + ((lanes[2] + lanes[6]) + (lanes[3] + lanes[7]))
        + tail
}
