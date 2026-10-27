//! Inverse MDCT through a DCT-IV of half the size (`media-dsp`: a complex
//! FFT of a quarter the size), and the sine / Kaiser-Bessel-derived windows.
use media_dsp::Dct4;
use std::f64::consts::PI;

/// IMDCT of `n/2` coefficients into `n` samples:
/// `y[i] = 2/n Σ X[k] cos(2π/n (i + n0)(k + ½))`, `n0 = (n/2 + 1)/2`.
pub(crate) struct Imdct {
    n: usize,
    dct4: Dct4,
    dct: Vec<f32>,
    fold: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
}

impl Imdct {
    pub(crate) fn new(n: usize) -> Imdct {
        let m = n / 2;
        Imdct {
            n,
            dct4: Dct4::new(m),
            dct: vec![0.0; m],
            fold: vec![0.0; m],
            re: vec![0.0; m / 2],
            im: vec![0.0; m / 2],
        }
    }

    /// Forward MDCT of `n` windowed samples into `n/2` coefficients,
    /// `X[k] = Σ z[i] cos(2π/n (i + n0)(k + ½))` (no scale), through the
    /// same DCT-IV: fold the four quarter-blocks into `n/2` values.
    pub(crate) fn forward(&mut self, z: &[f32], out: &mut [f32]) {
        let m = self.n / 2;
        let h = m / 2;
        let u = &mut self.fold;
        for i in 0..h {
            u[i] = -z[3 * h - 1 - i] - z[3 * h + i];
            u[h + i] = z[i] - z[m - 1 - i];
        }
        self.dct4.run(&self.fold, out, &mut self.re, &mut self.im);
    }

    pub(crate) fn run(&mut self, x: &[f32], out: &mut [f32]) {
        let (n, m) = (self.n, self.n / 2);
        self.dct4.run(x, &mut self.dct, &mut self.re, &mut self.im);
        let scale = 2.0 / n as f32;
        // y[i] = d[i + m/2] for the first half of d, then the mirrored and
        // negated halves.
        let q = m / 2;
        let d = &self.dct;
        let out = &mut out[..n];
        for i in 0..q {
            out[i] = d[q + i] * scale;
        }
        for i in q..q + m {
            out[i] = -d[2 * m - 1 - (i + q)] * scale;
        }
        for i in q + m..n {
            out[i] = -d[i + q - 2 * m] * scale;
        }
    }
}

/// Rising half of a sine window of length `n`.
pub(crate) fn sine(n: usize) -> Vec<f32> {
    (0..n / 2)
        .map(|i| (PI / n as f64 * (i as f64 + 0.5)).sin() as f32)
        .collect()
}

fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term) = (1.0, 1.0);
    for k in 1..60 {
        term *= (x / 2.0 / k as f64).powi(2);
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

/// Rising half of a Kaiser-Bessel-derived window of length `n`.
pub(crate) fn kbd(n: usize, alpha: f64) -> Vec<f32> {
    let half = n / 2;
    let w: Vec<f64> = (0..=half)
        .map(|j| {
            let r = (2.0 * j as f64 / half as f64) - 1.0;
            bessel_i0(PI * alpha * (1.0 - r * r).max(0.0).sqrt())
        })
        .collect();
    let total: f64 = w.iter().sum();
    let mut acc = 0.0;
    (0..half)
        .map(|i| {
            acc += w[i];
            (acc / total).sqrt() as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn direct(x: &[f32]) -> Vec<f64> {
        let n = x.len() * 2;
        let n0 = (n as f64 / 2.0 + 1.0) / 2.0;
        (0..n)
            .map(|i| {
                x.iter()
                    .enumerate()
                    .map(|(k, &v)| {
                        f64::from(v)
                            * (2.0 * PI / n as f64 * (i as f64 + n0) * (k as f64 + 0.5)).cos()
                    })
                    .sum::<f64>()
                    * 2.0
                    / n as f64
            })
            .collect()
    }

    #[test]
    fn fast_imdct_matches_the_definition() {
        for n in [256, 2048] {
            let x: Vec<f32> = (0..n / 2)
                .map(|k| ((k * 37 % 101) as f32 - 50.0) / 7.0)
                .collect();
            let mut out = vec![0.0; n];
            Imdct::new(n).run(&x, &mut out);
            for (a, b) in out.iter().zip(direct(&x)) {
                assert!((f64::from(*a) - b).abs() < 1e-4, "{a} {b}");
            }
        }
    }

    #[test]
    fn forward_mdct_matches_the_definition() {
        for n in [256usize, 2048] {
            let z: Vec<f32> = (0..n)
                .map(|i| ((i * 53 % 97) as f32 - 48.0) / 5.0)
                .collect();
            let mut fast = vec![0.0; n / 2];
            Imdct::new(n).forward(&z, &mut fast);
            let n0 = (n as f64 / 2.0 + 1.0) / 2.0;
            for k in 0..n / 2 {
                let d: f64 = z
                    .iter()
                    .enumerate()
                    .map(|(i, &v)| {
                        f64::from(v)
                            * (2.0 * PI / n as f64 * (i as f64 + n0) * (k as f64 + 0.5)).cos()
                    })
                    .sum();
                assert!(
                    (f64::from(fast[k]) - d).abs() < 1e-2,
                    "{n} {k}: {} {d}",
                    fast[k]
                );
            }
        }
    }

    #[test]
    fn windows_satisfy_princen_bradley() {
        for w in [sine(2048), sine(256), kbd(2048, 4.0), kbd(256, 6.0)] {
            let h = w.len();
            for i in 0..h {
                let s = w[i] * w[i] + w[h - 1 - i] * w[h - 1 - i];
                assert!((s - 1.0).abs() < 1e-5, "{s}");
            }
        }
    }
}
