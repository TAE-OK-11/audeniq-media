//! Sample-rate conversion for deliveries (44.1 → 48 kHz and any other
//! rational ratio) and the channel downmixes stores ask for.
//!
//! [`Resampler`] is a polyphase windowed-sinc filter: the ratio is reduced
//! to `up/down`, one Kaiser-windowed sinc (β = 12, ≈ 120 dB stopband) is
//! cut into `up` phases of `2 * HALF_TAPS` taps each, and every output
//! sample is one phase's dot product with the input. The cutoff sits at
//! 0.95 of the lower Nyquist frequency with a ≈ 1.8 kHz transition band,
//! so 44.1 → 48 kHz is flat to 20 kHz and stops by 21.9 kHz. Equal rates
//! pass samples through untouched. Streaming: push blocks, pull what is ready; `finish`
//! flushes the filter tail. Output length is exactly
//! `ceil(input_frames * out_rate / in_rate)`.
//!
//! [`downmix_stereo`] folds mono and 5.1/7.1 (AAC / WAV channel orders) to
//! stereo with the ITU-R BS.775 coefficients.

/// Zero crossings of the sinc on each side: a ≈ 1.8 kHz transition band at
/// 44.1 kHz, so the passband is flat to 20 kHz.
const HALF_TAPS: usize = 96;
const BETA: f64 = 12.0;

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term) = (1.0, 1.0);
    for k in 1..80 {
        term *= (x / 2.0 / k as f64).powi(2);
        sum += term;
        if term < sum * 1e-18 {
            break;
        }
    }
    sum
}

pub struct Resampler {
    channels: usize,
    up: usize,
    down: usize,
    /// `up` phases × `2 * HALF_TAPS` taps.
    taps: Vec<f32>,
    /// Interleaved input not yet consumed, with `HALF_TAPS` frames of history.
    buf: Vec<f32>,
    /// Absolute index (in `up` steps) of the next output sample.
    pos: u64,
    /// Absolute input frame index of `buf[0]`.
    base: u64,
    in_frames: u64,
    out_frames: u64,
}

impl Resampler {
    pub fn new(in_rate: u32, out_rate: u32, channels: usize) -> Resampler {
        assert!(in_rate > 0 && out_rate > 0 && channels > 0);
        let g = gcd(u64::from(in_rate), u64::from(out_rate));
        let (up, down) = (
            (u64::from(out_rate) / g) as usize,
            (u64::from(in_rate) / g) as usize,
        );
        let width = 2 * HALF_TAPS;
        // Cutoff relative to the input rate: 0.95 × the lower Nyquist.
        let cutoff = 0.95 * (up as f64 / down as f64).min(1.0);
        let mut taps = vec![0f32; up * width];
        for p in 0..up {
            let frac = p as f64 / up as f64;
            let mut sum = 0.0;
            let mut phase = vec![0f64; width];
            for (j, t) in phase.iter_mut().enumerate() {
                // Distance from the output instant to input tap j.
                let x = j as f64 - (HALF_TAPS as f64 - 1.0) - frac;
                let r = x / HALF_TAPS as f64;
                let win = if r.abs() >= 1.0 {
                    0.0
                } else {
                    bessel_i0(BETA * (1.0 - r * r).sqrt()) / bessel_i0(BETA)
                };
                let s = if x == 0.0 {
                    1.0
                } else {
                    (std::f64::consts::PI * x * cutoff).sin() / (std::f64::consts::PI * x * cutoff)
                };
                *t = cutoff * s * win;
                sum += *t;
            }
            // Unity gain at DC for every phase.
            for (j, t) in phase.iter().enumerate() {
                taps[p * width + j] = (*t / sum) as f32;
            }
        }
        Resampler {
            channels,
            up,
            down,
            taps,
            buf: vec![0.0; (HALF_TAPS - 1) * channels],
            pos: 0,
            base: 0,
            in_frames: 0,
            out_frames: 0,
        }
    }

    /// Output frames for `in_frames` input frames.
    pub fn output_len(&self, in_frames: u64) -> u64 {
        (in_frames * self.up as u64).div_ceil(self.down as u64)
    }

    /// Feed interleaved frames; append the output that is ready.
    pub fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.up == self.down {
            out.extend_from_slice(input);
            return;
        }
        self.buf.extend_from_slice(input);
        self.in_frames += (input.len() / self.channels) as u64;
        self.drain(out, false);
    }

    /// Flush the tail (zeros after the end) and finish at the exact length.
    pub fn finish(&mut self, out: &mut Vec<f32>) {
        if self.up == self.down {
            return;
        }
        self.buf
            .extend(std::iter::repeat_n(0.0, (HALF_TAPS + 1) * self.channels));
        self.drain(out, true);
    }

    fn drain(&mut self, out: &mut Vec<f32>, last: bool) {
        let ch = self.channels;
        let width = 2 * HALF_TAPS;
        // Buffered frames start at input frame `base - (HALF_TAPS - 1)`.
        let frames = (self.buf.len() / ch) as u64;
        let target = self.output_len(self.in_frames);
        loop {
            if last && self.out_frames >= target {
                break;
            }
            let t = self.pos * self.down as u64; // in up-steps
            let centre = t / self.up as u64; // input frame at or before the instant
            let phase = (t % self.up as u64) as usize;
            // Taps cover input frames centre - (HALF_TAPS-1) ..= centre + HALF_TAPS;
            // buf[k] holds input frame base - (HALF_TAPS-1) + k.
            let first = (centre - self.base) as usize;
            if first as u64 + width as u64 > frames {
                break;
            }
            let h = &self.taps[phase * width..phase * width + width];
            for c in 0..ch {
                let mut acc = 0f32;
                for (j, &k) in h.iter().enumerate() {
                    acc += k * self.buf[(first + j) * ch + c];
                }
                out.push(acc);
            }
            self.pos += 1;
            self.out_frames += 1;
        }
        // Drop input no longer needed.
        let t = self.pos * self.down as u64;
        let centre = t / self.up as u64;
        let keep_from = centre.saturating_sub(self.base) as usize;
        let keep_from = keep_from.min(self.buf.len() / ch);
        if keep_from > 0 {
            self.buf.drain(..keep_from * ch);
            self.base += keep_from as u64;
        }
    }
}

/// Fold interleaved audio of `channels` (`layout` names in order: `L`, `R`,
/// `C`, `LFE`, `Ls`, `Rs`, …) to stereo. Mono is duplicated; LFE dropped;
/// centre and surrounds at −3 dB (ITU-R BS.775). Clipping is left to the
/// caller's meter.
pub fn downmix_stereo(input: &[f32], layout: &[&str]) -> Vec<f32> {
    let ch = layout.len();
    if ch == 2 {
        return input.to_vec();
    }
    let k = std::f32::consts::FRAC_1_SQRT_2;
    let gains: Vec<(f32, f32)> = layout
        .iter()
        .map(|n| match *n {
            "L" | "FL" => (1.0, 0.0),
            "R" | "FR" => (0.0, 1.0),
            "C" | "FC" | "M" if ch == 1 => (1.0, 1.0),
            "C" | "FC" => (k, k),
            "Ls" | "SL" | "BL" | "Lss" | "Lrs" => (k, 0.0),
            "Rs" | "SR" | "BR" | "Rss" | "Rrs" => (0.0, k),
            "Cs" | "BC" => (k * k, k * k),
            _ => (0.0, 0.0), // LFE and unknown
        })
        .collect();
    let mut out = Vec::with_capacity(input.len() / ch * 2);
    for f in input.chunks_exact(ch) {
        let (mut l, mut r) = (0f32, 0f32);
        for (v, (gl, gr)) in f.iter().zip(&gains) {
            l += v * gl;
            r += v * gr;
        }
        out.push(l);
        out.push(r);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, f: f64, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| {
                (2.0 * std::f64::consts::PI * f * i as f64 / f64::from(rate)).sin() as f32 * 0.5
            })
            .collect()
    }

    /// Error power of `y` against the exact sine, in dB below the signal.
    fn snr(y: &[f32], rate: u32, f: f64, skip: usize) -> f64 {
        let (mut s, mut e) = (0.0, 0.0);
        for (i, v) in y.iter().enumerate().skip(skip).take(y.len() - 2 * skip) {
            let r = (2.0 * std::f64::consts::PI * f * i as f64 / f64::from(rate)).sin() * 0.5;
            s += r * r;
            e += (f64::from(*v) - r).powi(2);
        }
        10.0 * (s / e).log10()
    }

    fn run(input: &[f32], from: u32, to: u32, block: usize) -> Vec<f32> {
        let mut r = Resampler::new(from, to, 1);
        let mut out = Vec::new();
        for b in input.chunks(block) {
            r.push(b, &mut out);
        }
        r.finish(&mut out);
        out
    }

    #[test]
    fn tones_survive_44_1_to_48k() {
        for f in [100.0, 1000.0, 10_000.0, 19_000.0] {
            let y = run(&sine(44100, f, 44100), 44100, 48000, 1000);
            assert_eq!(y.len(), 48000);
            let db = snr(&y, 48000, f, 200);
            assert!(db > 90.0, "{f} Hz: {db:.1} dB");
        }
    }

    #[test]
    fn downsampling_and_odd_blocks() {
        let x = sine(96000, 3000.0, 96000);
        let a = run(&x, 96000, 48000, 777);
        let b = run(&x, 96000, 48000, 96000);
        assert_eq!(a, b, "block size changes nothing");
        assert_eq!(a.len(), 48000);
        assert!(snr(&a, 48000, 3000.0, 200) > 90.0);
        // Above the new Nyquist is removed.
        let hi = run(&sine(96000, 30000.0, 96000), 96000, 48000, 4096);
        let rms = (hi[200..hi.len() - 200]
            .iter()
            .map(|v| f64::from(*v).powi(2))
            .sum::<f64>()
            / hi.len() as f64)
            .sqrt();
        assert!(20.0 * (rms / 0.354).log10() < -100.0, "{rms}");
    }

    #[test]
    fn same_rate_is_transparent_and_stereo_is_kept_apart() {
        let x = sine(48000, 1000.0, 4800);
        let y = run(&x, 48000, 48000, 100);
        assert_eq!(y.len(), x.len());
        assert!(x.iter().zip(&y).all(|(a, b)| (a - b).abs() < 1e-6));
        let st: Vec<f32> = x.iter().flat_map(|&v| [v, 0.0]).collect();
        let mut r = Resampler::new(44100, 48000, 2);
        let mut out = Vec::new();
        r.push(&st, &mut out);
        r.finish(&mut out);
        assert!(out.chunks(2).all(|f| f[1] == 0.0));
    }

    #[test]
    fn downmixes() {
        assert_eq!(downmix_stereo(&[0.5, -0.5], &["C"]), [0.5, 0.5, -0.5, -0.5]);
        let six = [1.0, 0.0, 0.0, 0.0, 0.0, 9.0]; // C L R Ls Rs LFE
        let s = downmix_stereo(&six, &["C", "L", "R", "Ls", "Rs", "LFE"]);
        assert!((s[0] - 0.70710677).abs() < 1e-6 && (s[1] - 0.70710677).abs() < 1e-6);
    }
}
