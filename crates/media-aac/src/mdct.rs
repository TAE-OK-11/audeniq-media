//! Inverse MDCT through a DCT-IV of half the size, itself a complex FFT of
//! a quarter the size, and the sine / Kaiser-Bessel-derived windows.
use std::f64::consts::PI;

#[derive(Clone, Copy)]
struct C {
    re: f64,
    im: f64,
}

impl C {
    fn mul(self, o: C) -> C {
        C {
            re: self.re * o.re - self.im * o.im,
            im: self.re * o.im + self.im * o.re,
        }
    }
}

/// IMDCT of `n/2` coefficients into `n` samples:
/// `y[i] = 2/n Σ X[k] cos(2π/n (i + n0)(k + ½))`, `n0 = (n/2 + 1)/2`.
pub(crate) struct Imdct {
    n: usize,
    /// Pre/post rotation `exp(-iπ(8j+1)/(8M))`, `M = n/2`.
    twiddle: Vec<C>,
    /// FFT roots of size `M/2` and its bit-reversal permutation.
    roots: Vec<C>,
    rev: Vec<usize>,
    buf: Vec<C>,
    dct: Vec<f64>,
}

impl Imdct {
    pub(crate) fn new(n: usize) -> Imdct {
        let m = n / 2;
        let q = m / 2;
        let twiddle = (0..q)
            .map(|j| {
                let a = -PI * (8 * j + 1) as f64 / (8 * m) as f64;
                C {
                    re: a.cos(),
                    im: a.sin(),
                }
            })
            .collect();
        let roots = (0..q / 2)
            .map(|k| {
                let a = -2.0 * PI * k as f64 / q as f64;
                C {
                    re: a.cos(),
                    im: a.sin(),
                }
            })
            .collect();
        let bits = q.trailing_zeros();
        let rev = (0..q)
            .map(|i| i.reverse_bits() >> (usize::BITS - bits))
            .collect();
        Imdct {
            n,
            twiddle,
            roots,
            rev,
            buf: vec![C { re: 0.0, im: 0.0 }; q],
            dct: vec![0.0; m],
        }
    }

    fn fft(&mut self) {
        let q = self.buf.len();
        for i in 0..q {
            let j = self.rev[i];
            if i < j {
                self.buf.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= q {
            let step = q / len;
            for start in (0..q).step_by(len) {
                for k in 0..len / 2 {
                    let w = self.roots[k * step];
                    let a = self.buf[start + k];
                    let b = self.buf[start + k + len / 2].mul(w);
                    self.buf[start + k] = C {
                        re: a.re + b.re,
                        im: a.im + b.im,
                    };
                    self.buf[start + k + len / 2] = C {
                        re: a.re - b.re,
                        im: a.im - b.im,
                    };
                }
            }
            len *= 2;
        }
    }

    /// `u[i] = Σ x[k] cos(π/M (i + ½)(k + ½))`.
    fn dct4(&mut self, x: &[f32]) {
        let m = self.n / 2;
        let q = m / 2;
        for j in 0..q {
            let v = C {
                re: f64::from(x[2 * j]),
                im: f64::from(x[m - 1 - 2 * j]),
            };
            self.buf[j] = v.mul(self.twiddle[j]);
        }
        self.fft();
        for k in 0..q {
            let c = self.buf[k].mul(self.twiddle[k]);
            self.dct[2 * k] = c.re;
            self.dct[m - 1 - 2 * k] = -c.im;
        }
    }

    pub(crate) fn run(&mut self, x: &[f32], out: &mut [f32]) {
        let (n, m) = (self.n, self.n / 2);
        self.dct4(x);
        let scale = 2.0 / n as f64;
        for (i, o) in out.iter_mut().enumerate().take(n) {
            let j = i + m / 2;
            let v = if j < m {
                self.dct[j]
            } else if j < 2 * m {
                -self.dct[2 * m - 1 - j]
            } else {
                -self.dct[j - 2 * m]
            };
            *o = (v * scale) as f32;
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
