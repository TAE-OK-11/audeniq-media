//! Signal-processing kernels shared by the AUDENIQ media codecs: a split
//! complex FFT and the DCT-IV on it (AAC's MDCT, SBR's QMF banks, MP3's
//! synthesis), a dot product (resampling filters) and the sliding history
//! of synthesis filter banks.
//!
//! Every kernel is plain safe Rust. On x86-64 each also has a build with
//! AVX2 enabled, chosen at runtime ([`simd`]); AArch64 always has NEON, so
//! its one build is already vectorized. The builds give identical results.
#![deny(unsafe_code)]

pub mod fft;
mod history;
#[allow(unsafe_code)]
pub mod simd;

pub use fft::{Dct4, Fft};
pub use history::History;
use simd::multiversion;

multiversion! {
    /// Σ a[i]·b[i] over the shorter length, in eight independent lanes
    /// summed in order, then the tail (the same order on every machine, and
    /// as audeniq-qc's dot kernels).
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
    lanes.iter().sum::<f32>() + tail
}

multiversion! {
    /// [`dot`] of `h` with `a` and with `b` at once (one load of `h` for
    /// both, as stereo filters need); each result is exactly that `dot`.
    pub fn dot2(h: &[f32], a: &[f32], b: &[f32]) -> (f32, f32) = dot2_body;
}

#[inline(always)]
fn dot2_body(h: &[f32], a: &[f32], b: &[f32]) -> (f32, f32) {
    let n = h.len().min(a.len()).min(b.len());
    let (h, a, b) = (&h[..n], &a[..n], &b[..n]);
    let ((h8, ht), (a8, at), (b8, bt)) =
        (h.as_chunks::<8>(), a.as_chunks::<8>(), b.as_chunks::<8>());
    let (mut la, mut lb) = ([0f32; 8], [0f32; 8]);
    for ((h, x), y) in h8.iter().zip(a8).zip(b8) {
        for i in 0..8 {
            la[i] += h[i] * x[i];
            lb[i] += h[i] * y[i];
        }
    }
    let ta: f32 = ht.iter().zip(at).map(|(h, x)| h * x).sum();
    let tb: f32 = ht.iter().zip(bt).map(|(h, y)| h * y).sum();
    (la.iter().sum::<f32>() + ta, lb.iter().sum::<f32>() + tb)
}

multiversion! {
    /// [`dot2`] for four tap/input sets at once: eight independent sums in
    /// flight hide the add latency a single one waits on. Each result is
    /// exactly the [`dot`] of its pair; all slices have one length.
    pub fn dot2x4(h: [&[f32]; 4], a: [&[f32]; 4], b: [&[f32]; 4]) -> [(f32, f32); 4] = dot2x4_body;
}

#[inline(always)]
fn dot2x4_body(h: [&[f32]; 4], a: [&[f32]; 4], b: [&[f32]; 4]) -> [(f32, f32); 4] {
    let n = h[0].len();
    assert!(h.iter().chain(&a).chain(&b).all(|x| x.len() == n));
    let h8 = h.map(|x| x.as_chunks::<8>());
    let a8 = a.map(|x| x.as_chunks::<8>());
    let b8 = b.map(|x| x.as_chunks::<8>());
    let mut la = [[0f32; 8]; 4];
    let mut lb = [[0f32; 8]; 4];
    for c in 0..n / 8 {
        for k in 0..4 {
            let (hc, ac, bc) = (h8[k].0[c], a8[k].0[c], b8[k].0[c]);
            for i in 0..8 {
                la[k][i] += hc[i] * ac[i];
                lb[k][i] += hc[i] * bc[i];
            }
        }
    }
    std::array::from_fn(|k| {
        let ta: f32 = h8[k].1.iter().zip(a8[k].1).map(|(h, x)| h * x).sum();
        let tb: f32 = h8[k].1.iter().zip(b8[k].1).map(|(h, y)| h * y).sum();
        (
            la[k].iter().sum::<f32>() + ta,
            lb[k].iter().sum::<f32>() + tb,
        )
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn dot2_is_two_dots() {
        for n in [0, 1, 7, 8, 9, 63, 192, 197] {
            let h: Vec<f32> = (0..n).map(|i| (i as f32 * 0.31).sin()).collect();
            let a: Vec<f32> = (0..n).map(|i| (i as f32 * 0.17).cos()).collect();
            let b: Vec<f32> = (0..n).map(|i| (i as f32 * 0.07).sin() - 0.2).collect();
            assert_eq!(
                super::dot2(&h, &a, &b),
                (super::dot(&h, &a), super::dot(&h, &b))
            );
        }
    }
}
