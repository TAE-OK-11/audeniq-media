//! Complex FFT of power-of-two sizes on split real and imaginary arrays
//! (so the butterflies of a stage are contiguous and vectorize), and the
//! DCT-IV built on it.
//!
//! Decimation in time: a bit-reversal permutation, a radix-4 first pass
//! (the two twiddle-free stages), then radix-2 stages fused in pairs and a
//! last single one when their count is odd; twiddles are stored per stage
//! in order. The inverse transform is the forward one
//! with real and imaginary parts swapped.
use crate::simd::multiversion;
use std::f64::consts::PI;

pub struct Fft {
    n: usize,
    /// Bit-reversal pairs (i, j), i < j.
    swaps: Vec<(u32, u32)>,
    /// Twiddles exp(−2πik/2h) of the stage of half-size h at [h + k].
    wr: Vec<f32>,
    wi: Vec<f32>,
}

impl Fft {
    pub fn new(n: usize) -> Fft {
        assert!(n.is_power_of_two() && n >= 4, "FFT size {n}");
        let bits = n.trailing_zeros();
        let swaps = (0..n)
            .filter_map(|i| {
                let j = i.reverse_bits() >> (usize::BITS - bits);
                (i < j).then_some((i as u32, j as u32))
            })
            .collect();
        let mut wr = vec![0f32; n];
        let mut wi = vec![0f32; n];
        let mut h = 1;
        while h < n {
            for k in 0..h {
                let a = -PI * k as f64 / h as f64;
                wr[h + k] = a.cos() as f32;
                wi[h + k] = a.sin() as f32;
            }
            h *= 2;
        }
        Fft { n, swaps, wr, wi }
    }

    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    /// In-place forward transform: X[k] = Σ x[j] exp(−2πijk/n).
    pub fn forward(&self, re: &mut [f32], im: &mut [f32]) {
        assert!(re.len() == self.n && im.len() == self.n);
        fft(self, re, im);
    }

    /// In-place inverse transform without scaling: x[j] = Σ X[k] exp(2πijk/n).
    pub fn inverse(&self, re: &mut [f32], im: &mut [f32]) {
        self.forward(im, re);
    }

    /// `lanes` forward transforms at once, stored point-major (point j of
    /// transform l at `j * lanes + l`), so every butterfly runs over
    /// `lanes` contiguous values: the fast way to do many small ones.
    /// `lanes` is a multiple of 8.
    pub fn forward_lanes(&self, re: &mut [f32], im: &mut [f32], lanes: usize) {
        assert!(
            lanes.is_multiple_of(8) && re.len() == self.n * lanes && im.len() == self.n * lanes
        );
        let (re, im) = (re.as_chunks_mut().0, im.as_chunks_mut().0);
        fft_lanes(self, re, im, lanes / 8);
    }
}

/// Eight lanes.
type V = [f32; 8];

/// Rows `a < b` of `g` vectors each.
#[inline(always)]
fn rows<T>(x: &mut [T], a: usize, b: usize, g: usize) -> (&mut [T], &mut [T]) {
    let (lo, hi) = x.split_at_mut(b * g);
    (&mut lo[a * g..(a + 1) * g], &mut hi[..g])
}

multiversion! {
    fn fft_lanes(f: &Fft, re: &mut [V], im: &mut [V], g: usize) = fft_lanes_body;
}

#[inline(always)]
fn fft_lanes_body(f: &Fft, re: &mut [V], im: &mut [V], g: usize) {
    let n = f.n;
    for &(i, j) in &f.swaps {
        let (a, b) = rows(re, i as usize, j as usize, g);
        a.swap_with_slice(b);
        let (a, b) = rows(im, i as usize, j as usize, g);
        a.swap_with_slice(b);
    }
    // Radix 4 over rows s..s+4 (twiddles 1 and −i), as in `fft_body`.
    for (rb, ib) in re.chunks_exact_mut(4 * g).zip(im.chunks_exact_mut(4 * g)) {
        for v in 0..g {
            let (r0, r1, r2, r3) = (rb[v], rb[g + v], rb[2 * g + v], rb[3 * g + v]);
            let (i0, i1, i2, i3) = (ib[v], ib[g + v], ib[2 * g + v], ib[3 * g + v]);
            let mut o: [V; 8] = [[0.0; 8]; 8];
            for e in 0..8 {
                let (a0r, a0i) = (r0[e] + r1[e], i0[e] + i1[e]);
                let (a1r, a1i) = (r0[e] - r1[e], i0[e] - i1[e]);
                let (b0r, b0i) = (r2[e] + r3[e], i2[e] + i3[e]);
                let (b1r, b1i) = (r2[e] - r3[e], i2[e] - i3[e]);
                o[0][e] = a0r + b0r;
                o[1][e] = a0i + b0i;
                o[2][e] = a0r - b0r;
                o[3][e] = a0i - b0i;
                o[4][e] = a1r + b1i;
                o[5][e] = a1i - b1r;
                o[6][e] = a1r - b1i;
                o[7][e] = a1i + b1r;
            }
            (rb[v], ib[v], rb[2 * g + v], ib[2 * g + v]) = (o[0], o[1], o[2], o[3]);
            (rb[g + v], ib[g + v], rb[3 * g + v], ib[3 * g + v]) = (o[4], o[5], o[6], o[7]);
        }
    }
    let mut h = 4;
    while h < n {
        for blk in (0..n).step_by(2 * h) {
            for k in 0..h {
                let (wr, wi) = (f.wr[h + k], f.wi[h + k]);
                let (ar, br) = rows(re, blk + k, blk + k + h, g);
                let (ai, bi) = rows(im, blk + k, blk + k + h, g);
                for (((ar, br), ai), bi) in ar.iter_mut().zip(br).zip(ai).zip(bi) {
                    for e in 0..8 {
                        let tr = br[e] * wr - bi[e] * wi;
                        let ti = br[e] * wi + bi[e] * wr;
                        br[e] = ar[e] - tr;
                        bi[e] = ai[e] - ti;
                        ar[e] += tr;
                        ai[e] += ti;
                    }
                }
            }
        }
        h *= 2;
    }
}

multiversion! {
    fn fft(f: &Fft, re: &mut [f32], im: &mut [f32]) = fft_body;
}

#[inline(always)]
fn fft_body(f: &Fft, re: &mut [f32], im: &mut [f32]) {
    let n = f.n;
    for &(i, j) in &f.swaps {
        re.swap(i as usize, j as usize);
        im.swap(i as usize, j as usize);
    }
    // Radix 4: stages of half-size 1 and 2 at once (twiddles 1 and −i).
    for s in (0..n).step_by(4) {
        let (a0r, a0i) = (re[s] + re[s + 1], im[s] + im[s + 1]);
        let (a1r, a1i) = (re[s] - re[s + 1], im[s] - im[s + 1]);
        let (b0r, b0i) = (re[s + 2] + re[s + 3], im[s + 2] + im[s + 3]);
        let (b1r, b1i) = (re[s + 2] - re[s + 3], im[s + 2] - im[s + 3]);
        re[s] = a0r + b0r;
        im[s] = a0i + b0i;
        re[s + 2] = a0r - b0r;
        im[s + 2] = a0i - b0i;
        // b1 · (−i) = (b1i, −b1r)
        re[s + 1] = a1r + b1i;
        im[s + 1] = a1i - b1r;
        re[s + 3] = a1r - b1i;
        im[s + 3] = a1i + b1r;
    }
    // Radix 4 after that: stages of half-size h and 2h at once, then one
    // radix-2 stage if the count is odd.
    let mut h = 4;
    while 4 * h <= n {
        let (w1r, w1i) = (&f.wr[h..2 * h], &f.wi[h..2 * h]);
        let (w2r, w2i) = (&f.wr[2 * h..3 * h], &f.wi[2 * h..3 * h]);
        for (rb, ib) in re.chunks_exact_mut(4 * h).zip(im.chunks_exact_mut(4 * h)) {
            let (r01, r23) = rb.split_at_mut(2 * h);
            let (i01, i23) = ib.split_at_mut(2 * h);
            let (r0, r1) = r01.split_at_mut(h);
            let (r2, r3) = r23.split_at_mut(h);
            let (i0, i1) = i01.split_at_mut(h);
            let (i2, i3) = i23.split_at_mut(h);
            for k in 0..h {
                // Stage h: (0, 1) and (2, 3) with w1.
                let (ar, ai) = (
                    r1[k] * w1r[k] - i1[k] * w1i[k],
                    r1[k] * w1i[k] + i1[k] * w1r[k],
                );
                let (br, bi) = (
                    r3[k] * w1r[k] - i3[k] * w1i[k],
                    r3[k] * w1i[k] + i3[k] * w1r[k],
                );
                let (b0r, b0i) = (r0[k] + ar, i0[k] + ai);
                let (b1r, b1i) = (r0[k] - ar, i0[k] - ai);
                let (b2r, b2i) = (r2[k] + br, i2[k] + bi);
                let (b3r, b3i) = (r2[k] - br, i2[k] - bi);
                // Stage 2h: (0, 2) with w2, (1, 3) with w2·(−i).
                let (cr, ci) = (b2r * w2r[k] - b2i * w2i[k], b2r * w2i[k] + b2i * w2r[k]);
                let (dr, di) = (b3r * w2r[k] - b3i * w2i[k], b3r * w2i[k] + b3i * w2r[k]);
                r0[k] = b0r + cr;
                i0[k] = b0i + ci;
                r2[k] = b0r - cr;
                i2[k] = b0i - ci;
                // d·(−i) = (di, −dr)
                r1[k] = b1r + di;
                i1[k] = b1i - dr;
                r3[k] = b1r - di;
                i3[k] = b1i + dr;
            }
        }
        h *= 4;
    }
    if h < n {
        let (wr, wi) = (&f.wr[h..2 * h], &f.wi[h..2 * h]);
        let (ar, br) = re.split_at_mut(h);
        let (ai, bi) = im.split_at_mut(h);
        for k in 0..h {
            let tr = br[k] * wr[k] - bi[k] * wi[k];
            let ti = br[k] * wi[k] + bi[k] * wr[k];
            br[k] = ar[k] - tr;
            bi[k] = ai[k] - ti;
            ar[k] += tr;
            ai[k] += ti;
        }
    }
}

/// DCT-IV of size `m`: `y[i] = Σ x[k] cos(π/m (i + ½)(k + ½))`, through a
/// complex FFT of size `m/2` with pre- and post-rotation.
pub struct Dct4 {
    m: usize,
    fft: Fft,
    /// Rotation exp(−iπ(8j+1)/(8m)).
    tr: Vec<f32>,
    ti: Vec<f32>,
}

impl Dct4 {
    pub fn new(m: usize) -> Dct4 {
        assert!(m.is_power_of_two() && m >= 16, "DCT-IV size {m}");
        let q = m / 2;
        let (tr, ti) = (0..q)
            .map(|j| {
                let a = -PI * (8 * j + 1) as f64 / (8 * m) as f64;
                (a.cos() as f32, a.sin() as f32)
            })
            .unzip();
        Dct4 {
            m,
            fft: Fft::new(q),
            tr,
            ti,
        }
    }

    pub fn len(&self) -> usize {
        self.m
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    /// `out = DCT-IV(x)`; `re` and `im` are scratch of `m/2` each.
    pub fn run(&self, x: &[f32], out: &mut [f32], re: &mut [f32], im: &mut [f32]) {
        let (m, q) = (self.m, self.m / 2);
        assert!(x.len() == m && out.len() == m && re.len() == q && im.len() == q);
        pre(x, &self.tr, &self.ti, re, im);
        self.fft.forward(re, im);
        post(re, im, &self.tr, &self.ti, out);
    }
}

impl Dct4 {
    /// `lanes` DCT-IVs at once, point-major as in [`Fft::forward_lanes`]
    /// (`lanes` a multiple of 8); `re` and `im` are scratch of `m/2 · lanes`
    /// each.
    pub fn run_lanes(
        &self,
        x: &[f32],
        out: &mut [f32],
        re: &mut [f32],
        im: &mut [f32],
        lanes: usize,
    ) {
        let (m, q) = (self.m, self.m / 2);
        assert!(lanes.is_multiple_of(8) && x.len() == m * lanes && out.len() == m * lanes);
        assert!(re.len() == q * lanes && im.len() == q * lanes);
        let g = lanes / 8;
        let (x, out) = (x.as_chunks().0, out.as_chunks_mut().0);
        let (re, im) = (re.as_chunks_mut().0, im.as_chunks_mut().0);
        pre_lanes(x, &self.tr, &self.ti, re, im, g);
        fft_lanes(&self.fft, re, im, g);
        post_lanes(re, im, &self.tr, &self.ti, out, g);
    }
}

multiversion! {
    fn pre_lanes(x: &[V], tr: &[f32], ti: &[f32], re: &mut [V], im: &mut [V], g: usize) = pre_lanes_body;
}

#[inline(always)]
fn pre_lanes_body(x: &[V], tr: &[f32], ti: &[f32], re: &mut [V], im: &mut [V], g: usize) {
    let m = x.len() / g;
    for (j, (rr, ir)) in re
        .chunks_exact_mut(g)
        .zip(im.chunks_exact_mut(g))
        .enumerate()
    {
        let (a, b) = (&x[2 * j * g..][..g], &x[(m - 1 - 2 * j) * g..][..g]);
        let (cr, ci) = (tr[j], ti[j]);
        for (((r, i), a), b) in rr.iter_mut().zip(ir).zip(a).zip(b) {
            for e in 0..8 {
                r[e] = a[e] * cr - b[e] * ci;
                i[e] = a[e] * ci + b[e] * cr;
            }
        }
    }
}

multiversion! {
    fn post_lanes(re: &[V], im: &[V], tr: &[f32], ti: &[f32], out: &mut [V], g: usize) = post_lanes_body;
}

#[inline(always)]
fn post_lanes_body(re: &[V], im: &[V], tr: &[f32], ti: &[f32], out: &mut [V], g: usize) {
    let m = out.len() / g;
    for (k, (rr, ir)) in re.chunks_exact(g).zip(im.chunks_exact(g)).enumerate() {
        let (cr, ci) = (tr[k], ti[k]);
        let (e, o) = (2 * k, m - 1 - 2 * k);
        let (even, odd) = if e < o {
            rows(out, e, o, g)
        } else {
            let (odd, even) = rows(out, o, e, g);
            (even, odd)
        };
        for (((ev, od), r), i) in even.iter_mut().zip(odd).zip(rr).zip(ir) {
            for l in 0..8 {
                ev[l] = r[l] * cr - i[l] * ci;
                od[l] = -(r[l] * ci + i[l] * cr);
            }
        }
    }
}

multiversion! {
    fn pre(x: &[f32], tr: &[f32], ti: &[f32], re: &mut [f32], im: &mut [f32]) = pre_body;
}

#[inline(always)]
fn pre_body(x: &[f32], tr: &[f32], ti: &[f32], re: &mut [f32], im: &mut [f32]) {
    // (x[2j], x[m−1−2j]) are pair j's even and pair (q−1−j)'s odd value;
    // gather them, then rotate in contiguous runs.
    let q = re.len();
    for (j, c) in x.as_chunks::<2>().0.iter().enumerate() {
        re[j] = c[0];
        im[q - 1 - j] = c[1];
    }
    for (((r, i), &cr), &ci) in re.iter_mut().zip(im.iter_mut()).zip(tr).zip(ti) {
        let (a, b) = (*r, *i);
        *r = a * cr - b * ci;
        *i = a * ci + b * cr;
    }
}

multiversion! {
    fn post(re: &mut [f32], im: &mut [f32], tr: &[f32], ti: &[f32], out: &mut [f32]) = post_body;
}

#[inline(always)]
fn post_body(re: &mut [f32], im: &mut [f32], tr: &[f32], ti: &[f32], out: &mut [f32]) {
    // Rotate, then out[2k] = Re c(k) and out[m−1−2k] = −Im c(k): pair k
    // takes Re c(k) and −Im c(q−1−k).
    let q = re.len();
    for (((r, i), &cr), &ci) in re.iter_mut().zip(im.iter_mut()).zip(tr).zip(ti) {
        let (a, b) = (*r, *i);
        *r = a * cr - b * ci;
        *i = a * ci + b * cr;
    }
    for (k, c) in out.as_chunks_mut::<2>().0.iter_mut().enumerate() {
        c[0] = re[k];
        c[1] = -im[q - 1 - k];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dft(re: &[f32], im: &[f32]) -> (Vec<f64>, Vec<f64>) {
        let n = re.len();
        (0..n)
            .map(|k| {
                (0..n).fold((0.0, 0.0), |(sr, si), j| {
                    let a = -2.0 * PI * (j * k) as f64 / n as f64;
                    let (c, s) = (a.cos(), a.sin());
                    let (xr, xi) = (f64::from(re[j]), f64::from(im[j]));
                    (sr + xr * c - xi * s, si + xr * s + xi * c)
                })
            })
            .unzip()
    }

    fn signal(n: usize, seed: u32) -> Vec<f32> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (s >> 8) as f32 / (1 << 24) as f32 - 0.5
            })
            .collect()
    }

    #[test]
    fn fft_matches_dft_and_inverts() {
        for n in [4, 8, 16, 32, 64, 128, 512, 1024] {
            let (r0, i0) = (signal(n, 1), signal(n, 2));
            let (mut re, mut im) = (r0.clone(), i0.clone());
            let f = Fft::new(n);
            f.forward(&mut re, &mut im);
            let (er, ei) = dft(&r0, &i0);
            for k in 0..n {
                assert!(
                    (f64::from(re[k]) - er[k]).abs() < 1e-4 * n as f64,
                    "n {n} k {k}"
                );
                assert!((f64::from(im[k]) - ei[k]).abs() < 1e-4 * n as f64);
            }
            f.inverse(&mut re, &mut im);
            for k in 0..n {
                assert!((re[k] / n as f32 - r0[k]).abs() < 1e-5);
                assert!((im[k] / n as f32 - i0[k]).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn lanes_match_single_transforms() {
        let (m, c) = (64, 16);
        let d = Dct4::new(m);
        let xs: Vec<Vec<f32>> = (0..c).map(|l| signal(m, 10 + l as u32)).collect();
        let x: Vec<f32> = (0..m * c).map(|i| xs[i % c][i / c]).collect();
        let mut out = vec![0f32; m * c];
        let (mut re, mut im) = (vec![0f32; m / 2 * c], vec![0f32; m / 2 * c]);
        d.run_lanes(&x, &mut out, &mut re, &mut im, c);
        let (mut one, mut r1, mut i1) = (vec![0f32; m], vec![0f32; m / 2], vec![0f32; m / 2]);
        for (l, xl) in xs.iter().enumerate() {
            d.run(xl, &mut one, &mut r1, &mut i1);
            for i in 0..m {
                assert!((out[i * c + l] - one[i]).abs() < 1e-5, "lane {l} i {i}");
            }
        }
    }

    #[test]
    fn dct4_matches_definition() {
        for m in [16, 64, 256, 1024] {
            let x = signal(m, 3);
            let d = Dct4::new(m);
            let (mut out, mut re, mut im) = (vec![0f32; m], vec![0f32; m / 2], vec![0f32; m / 2]);
            d.run(&x, &mut out, &mut re, &mut im);
            for i in 0..m {
                let want: f64 = (0..m)
                    .map(|k| {
                        f64::from(x[k])
                            * (PI / m as f64 * (i as f64 + 0.5) * (k as f64 + 0.5)).cos()
                    })
                    .sum();
                assert!(
                    (f64::from(out[i]) - want).abs() < 2e-5 * m as f64,
                    "m {m} i {i}"
                );
            }
        }
    }
}
