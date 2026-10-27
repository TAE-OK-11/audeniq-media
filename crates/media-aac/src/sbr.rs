//! Spectral Band Replication (ISO/IEC 14496-3 subclause 4.6.18), the
//! HE-AAC tool that rebuilds the upper spectrum from the AAC core and a
//! few envelope and noise parameters, written from the standard's
//! description:
//!
//! - the SBR header and the per-frame data: time/frequency grid,
//!   delta-coded envelope and noise-floor scale factors (Huffman coded),
//!   inverse-filtering modes and added sinusoids, with stereo coupling;
//! - the frequency band tables derived from the header (master, high and
//!   low resolution, noise floor and limiter bands) and the patches that
//!   copy low QMF subbands up;
//! - the HF generator: QMF analysis of the core into 32 subbands, a
//!   second-order complex predictor per subband (covariance method) and
//!   chirp factors for inverse filtering;
//! - the HF adjuster: envelope estimation, gains with limiter and
//!   boost, time smoothing, noise floor and sinusoids;
//! - QMF synthesis at twice the core rate (or the core rate, downsampled).
//!
//! Timing: the QMF analysis keeps 8 slots of the previous frame before
//! the frame's 32, so buffer slot 8 is the frame's first analysis slot;
//! the envelope grid is offset by 2 slots into that buffer, and output
//! slot j is buffer slot j + 2. The grid may end up to 6 slots into the
//! next frame, whose output then takes those slots from this frame.
//!
//! The QMF banks are AUDENIQ's fast forms of the standard's matrices
//! (see [`Static`]). Tables (`sbr_tables.rs`) are the standard's data.
use crate::Error;
use crate::ps::Ps;
use crate::sbr_tables as tab;
use media_bits::{Bits, Huffman as Tree};
use media_dsp::{Dct4, Fft, History};
use std::f64::consts::PI;
use std::sync::OnceLock;

/// QMF slots per frame (16 time slots of 2).
const SLOTS: usize = 32;
/// Slots of the previous frame kept before the frame's analysis (tHFGen).
const KEPT: usize = 8;
/// Offset of the envelope grid into the analysis buffer (tHFAdj).
const ADJ: usize = 2;
/// Slots in the analysis and high-band buffers.
const SPAN: usize = SLOTS + KEPT;
/// Envelopes per frame at most.
const MAX_ENV: usize = 5;
/// Noise-floor bands at most.
const MAX_NOISE: usize = 5;
/// Core samples are scaled to ±32768 inside SBR (the envelope scale).
const SCALE: f32 = 32768.0;

/// Huffman tables (by [`tab::HUFF`] index): envelope 1.5 dB and 3 dB,
/// their balance forms, noise and noise balance; time (T) and frequency
/// (F) direction.
const ENV15_T: usize = 0;
const ENV15_F: usize = 1;
const BAL15_T: usize = 2;
const BAL15_F: usize = 3;
const ENV30_T: usize = 4;
const ENV30_F: usize = 5;
const BAL30_T: usize = 6;
const BAL30_F: usize = 7;
const NOISE_T: usize = 8;
const NOISE_BAL_T: usize = 9;

/// Frame classes of the time grid.
const FIXFIX: u32 = 0;
const FIXVAR: u32 = 1;
const VARFIX: u32 = 2;

/// One QMF frame: real and imaginary parts, 32 slots of 64 subbands.
pub(crate) type Qmf = [[[f32; 64]; SLOTS]; 2];

/// Complex factors as (re, im) arrays.
struct Rot {
    re: Vec<f32>,
    im: Vec<f32>,
}

fn rot(n: usize, scale: f64, angle: impl Fn(f64) -> f64) -> Rot {
    let (re, im) = (0..n)
        .map(|i| {
            let a = angle(i as f64);
            ((scale * a.cos()) as f32, (scale * a.sin()) as f32)
        })
        .unzip();
    Rot { re, im }
}

/// The QMF banks as fast transforms. With C₆₄ the DCT-IV of size 64,
/// C(j) = Σ a(m) cos(π/64 (j+½)(m+½)), extended to all j by C(−1−j) = C(j)
/// and C(j+128) = −C(j):
///
/// - analysis, W(k) = Σ u(n)·2e^{iπ(k+½)(2n−½)/64}, k < 32, is the kernel
///   at the odd inputs m = 2n − 1. Folding those into 0..64 (m = −1 → 0,
///   m > 63 → −(127 − m)) gives one C₆₄ whose outputs k and 63 − k are
///   Re W(k)/2 and −Im W(k)/2;
/// - synthesis, v(n) = Re Σ X(k)e^{iπ(k+½)(2n−255)/128}/64, is at the
///   DCT-IV index j = n − 128: v(n) = (S(n) − C(n))/64 for n < 64 and
///   (C(127−n) + S(127−n))/64 above, with C = C₆₄[Re X] and the DST-IV
///   S(j) = Σ Im X(k) sin(π/64 (k+½)(j+½)) = (−1)ʲ C₆₄[Im X(63−k)](j);
/// - downsampled synthesis (a rotation, an inverse DFT and a rotation), phase (k+½)(2n−127.5)/64 over 32 bands:
///   Re(e^{i(πn/64 − 127.5π/128)} · IDFT₆₄[X(k)e^{iπk/128}](n))/64 (the
///   offset 127.5 times the output as the reference decoder does,
///   conformance stream `al_sbr_sr_48_2_fsaac48`).
struct Static {
    trees: Vec<Tree>,
    fft64: Fft,
    dct64: Dct4,
    syn_pre: Rot,
    ds_post: Rot,
}

fn statics() -> &'static Static {
    static S: OnceLock<Static> = OnceLock::new();
    S.get_or_init(|| Static {
        trees: tab::HUFF.iter().map(|(c, l)| Tree::new(c, l)).collect(),
        fft64: Fft::new(64),
        dct64: Dct4::new(64),
        syn_pre: rot(64, 1.0, |k| PI * k / 128.0),
        ds_post: rot(64, 1.0 / 64.0, |n| PI * n / 64.0 - 127.5 * PI / 128.0),
    })
}

fn huff(b: &mut Bits, table: usize) -> Result<i32, Error> {
    Ok(statics().trees[table].decode(b)? as i32 - tab::LAV[table])
}

/// Scratch for a frame's QMF transforms run side by side, one lane per
/// slot (and per cosine/sine half in synthesis): `x`, `y` hold 64 points ×
/// 64 lanes, `re`, `im` 32 × 64.
struct Lanes {
    x: Vec<f32>,
    y: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
}

impl Default for Lanes {
    fn default() -> Lanes {
        Lanes {
            x: vec![0.0; 64 * 64],
            y: vec![0.0; 64 * 64],
            re: vec![0.0; 32 * 64],
            im: vec![0.0; 32 * 64],
        }
    }
}

/// Nearest integer, halves up (the standard's NINT for these positive
/// values).
fn nint(x: f64) -> i64 {
    (x + 0.5).floor() as i64
}

/// The SBR header fields that shape the decoding (4.5.2.8.2). Fields of
/// an absent extra part take their defaults.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Header {
    /// 3 dB envelope steps (else 1.5 dB).
    amp_res: bool,
    start: usize,
    stop: usize,
    xover: usize,
    freq_scale: usize,
    alter_scale: bool,
    noise_bands: usize,
    limiter_bands: usize,
    limiter_gains: usize,
    interpol_freq: bool,
    smoothing_mode: bool,
}

impl Header {
    fn read(b: &mut Bits) -> Result<Header, Error> {
        let amp_res = b.get(1)? == 1;
        let start = b.get(4)? as usize;
        let stop = b.get(4)? as usize;
        let xover = b.get(3)? as usize;
        b.skip(2)?; // reserved
        let extra_1 = b.get(1)? == 1;
        let extra_2 = b.get(1)? == 1;
        let (freq_scale, alter_scale, noise_bands) = if extra_1 {
            (b.get(2)? as usize, b.get(1)? == 1, b.get(2)? as usize)
        } else {
            (2, true, 2)
        };
        let (limiter_bands, limiter_gains, interpol_freq, smoothing_mode) = if extra_2 {
            (
                b.get(2)? as usize,
                b.get(2)? as usize,
                b.get(1)? == 1,
                b.get(1)? == 1,
            )
        } else {
            (2, 2, true, true)
        };
        Ok(Header {
            amp_res,
            start,
            stop,
            xover,
            freq_scale,
            alter_scale,
            noise_bands,
            limiter_bands,
            limiter_gains,
            interpol_freq,
            smoothing_mode,
        })
    }
    /// Whether a change to `other` changes the band tables (a reset).
    fn same_bands(&self, other: &Header) -> bool {
        (self.start, self.stop, self.xover, self.freq_scale)
            == (other.start, other.stop, other.xover, other.freq_scale)
            && (self.alter_scale, self.noise_bands) == (other.alter_scale, other.noise_bands)
    }
}

/// The frequency band tables of a header at an SBR rate (4.6.18.3.2), in
/// QMF subband indices, and the HF generator's patches (4.6.18.6.3).
struct Bands {
    /// First and last-plus-one SBR subband.
    kx: usize,
    m: usize,
    high: Vec<usize>,
    low: Vec<usize>,
    noise: Vec<usize>,
    limiter: Vec<usize>,
    /// (first source subband, subbands) of each patch; the targets follow
    /// each other from `kx`.
    patches: Vec<(usize, usize)>,
    /// Noise band of each subband from `kx`.
    noise_of: [u8; 64],
}

/// Ascending differences between the rounded points of a geometric series
/// from `a` to `b` in `n` steps.
fn geometric_steps(a: usize, b: usize, n: usize) -> Vec<usize> {
    let ratio = b as f64 / a as f64;
    let point = |k: usize| nint(a as f64 * ratio.powf(k as f64 / n as f64));
    let mut d: Vec<usize> = (0..n)
        .map(|k| (point(k + 1) - point(k)).max(0) as usize)
        .collect();
    d.sort_unstable();
    d
}

fn accumulate(start: usize, steps: &[usize]) -> Vec<usize> {
    let mut v = Vec::with_capacity(steps.len() + 1);
    v.push(start);
    for &d in steps {
        v.push(v.last().unwrap() + d);
    }
    v
}

impl Bands {
    fn new(h: &Header, rate: u32) -> Option<Bands> {
        let fs = f64::from(rate);
        // k0 from the start frequency: a minimum by rate and an offset.
        let start_min = if rate < 32000 {
            3000.0
        } else if rate < 64000 {
            4000.0
        } else {
            5000.0
        };
        let group = match rate {
            0..=16000 => 0,
            16001..=22050 => 1,
            22051..=24000 => 2,
            24001..=32000 => 3,
            32001..=64000 => 4,
            _ => 5,
        };
        let k0 = nint(start_min * 128.0 / fs) + i64::from(tab::OFFSET[group][h.start]);
        // k2 from the stop frequency.
        let k2 = match h.stop {
            14 => 2 * k0,
            15 => 3 * k0,
            stop => {
                let stop_min = if rate < 32000 {
                    6000.0
                } else if rate < 64000 {
                    8000.0
                } else {
                    10000.0
                };
                let base = nint(stop_min * 128.0 / fs);
                let point =
                    |i: usize| nint(base as f64 * (64.0 / base as f64).powf(i as f64 / 13.0));
                let mut d: Vec<i64> = (0..13).map(|i| point(i + 1) - point(i)).collect();
                d.sort_unstable();
                base + d[..stop].iter().sum::<i64>()
            }
        }
        .min(64);
        let limit = if rate <= 32000 {
            48
        } else if rate == 44100 {
            35
        } else {
            32
        };
        if k0 <= 0 || k2 <= k0 || k2 - k0 > limit {
            return None;
        }
        let (k0, k2) = (k0 as usize, k2 as usize);
        let master = Self::master(h, k0, k2)?;
        if h.xover >= master.len() - 1 {
            return None;
        }
        let high = master[h.xover..].to_vec();
        let n_high = high.len() - 1;
        let n_low = n_high.div_ceil(2);
        let odd = n_high % 2;
        let low: Vec<usize> = (0..=n_low)
            .map(|i| if i == 0 { high[0] } else { high[2 * i - odd] })
            .collect();
        let kx = high[0];
        let m = high[n_high] - kx;
        if kx > 32 || kx + m > 64 || m == 0 {
            return None;
        }
        let n_q = if h.noise_bands == 0 {
            1
        } else {
            nint(h.noise_bands as f64 * (k2 as f64 / kx as f64).log2()).max(1) as usize
        };
        if n_q > MAX_NOISE {
            return None;
        }
        let mut noise = vec![low[0]];
        let mut i = 0;
        for k in 1..=n_q {
            i += (n_low - i) / (n_q + 1 - k);
            noise.push(low[i]);
        }
        let patches = Self::patches(&master, k0, kx, m, rate)?;
        let limiter = Self::limiter(h, &low, kx, &patches);
        let mut noise_of = [0u8; 64];
        for (g, w) in noise.windows(2).enumerate() {
            for k in w[0]..w[1] {
                noise_of[k - kx] = g as u8;
            }
        }
        Some(Bands {
            kx,
            m,
            high,
            low,
            noise,
            limiter,
            patches,
            noise_of,
        })
    }

    /// The master table: linear steps of 1 or 2 subbands, or 12, 10 or 8
    /// bands per octave (one region, or two above k1 = 2·k0 when k2/k0 >
    /// 2.2449, the upper one optionally warped by 1.3).
    fn master(h: &Header, k0: usize, k2: usize) -> Option<Vec<usize>> {
        if h.freq_scale == 0 {
            let dk = if h.alter_scale { 2 } else { 1 };
            // 2·NINT((k2 − k0) / 2dk): an even count, rounded.
            let n = 2 * nint((k2 - k0) as f64 / (2 * dk) as f64) as usize;
            if n == 0 {
                return None;
            }
            let mut steps = vec![dk as i64; n];
            // Spread the difference to k2 over the steps, from the top
            // when it is positive and from the bottom otherwise.
            let mut diff = k2 as i64 - (k0 + n * dk) as i64;
            let (dir, mut k) = if diff > 0 {
                (-1i64, n as i64 - 1)
            } else {
                (1, 0)
            };
            while diff != 0 && (0..n as i64).contains(&k) {
                steps[k as usize] -= dir;
                k += dir;
                diff += dir;
            }
            if steps.iter().any(|&s| s <= 0) {
                return None;
            }
            let steps: Vec<usize> = steps.iter().map(|&s| s as usize).collect();
            return Some(accumulate(k0, &steps));
        }
        let per_octave = [12.0, 10.0, 8.0][h.freq_scale - 1];
        let warp = if h.alter_scale { 1.3 } else { 1.0 };
        let two_regions = k2 as f64 / k0 as f64 > 2.2449;
        let k1 = if two_regions { 2 * k0 } else { k2 };
        let n0 = 2 * nint(per_octave * (k1 as f64 / k0 as f64).log2() / 2.0) as usize;
        if n0 == 0 {
            return None;
        }
        let low = geometric_steps(k0, k1, n0);
        if low[0] == 0 {
            return None;
        }
        let mut master = accumulate(k0, &low);
        if two_regions {
            let n1 = 2 * nint(per_octave * (k2 as f64 / k1 as f64).log2() / (2.0 * warp)) as usize;
            if n1 == 0 {
                return None;
            }
            let mut high = geometric_steps(k1, k2, n1);
            // The upper steps may not be narrower than the lower ones.
            let widest = *low.last().unwrap();
            if high[0] < widest {
                let change = widest - high[0];
                high[0] += change;
                high[n1 - 1] = high[n1 - 1].checked_sub(change)?;
                high.sort_unstable();
            }
            if high[0] == 0 {
                return None;
            }
            master.extend(accumulate(k1, &high).into_iter().skip(1));
        }
        Some(master)
    }

    /// The patches: from the goal subband 2.048 MHz / rate down, runs of
    /// low subbands (ending at an even distance below k0) copied up until
    /// kx + m is covered; a last patch narrower than 3 subbands is dropped.
    /// Up to six patches (the conformance streams at 44.1 kHz need six).
    fn patches(
        master: &[usize],
        k0: usize,
        kx: usize,
        m: usize,
        rate: u32,
    ) -> Option<Vec<(usize, usize)>> {
        let goal = nint(2.048e6 / f64::from(rate)) as usize;
        let top = master.len() - 1;
        let mut k = if goal < kx + m {
            master.iter().position(|&f| f >= goal).unwrap_or(top)
        } else {
            top
        };
        let (mut msb, mut usb) = (k0, kx);
        let mut patches: Vec<(usize, usize)> = Vec::new();
        for _ in 0..64 {
            let mut j = k + 1;
            let (sb, odd) = loop {
                j -= 1;
                let sb = master[j];
                let odd = (sb + k0) % 2; // (sb − 2 + k0) mod 2
                if sb + odd < k0 + msb || j == 0 {
                    break (sb, odd);
                }
            };
            let width = sb.saturating_sub(usb);
            if width > 0 {
                let first = (k0 - odd).checked_sub(width)?;
                patches.push((first, width));
                usb = sb;
                msb = sb;
            } else {
                msb = kx;
            }
            if master[k] - sb < 3 {
                k = top;
            }
            if sb == kx + m {
                if patches.len() > 1 && patches.last()?.1 < 3 {
                    patches.pop();
                }
                return (1..=6).contains(&patches.len()).then_some(patches);
            }
        }
        None
    }

    /// The limiter bands: none (one band over the SBR range) or 1.2, 2 or
    /// 3 per octave, from the low-resolution table and the patch borders,
    /// merging bands narrower than 0.49 of a limiter band (patch borders
    /// are kept where possible).
    fn limiter(h: &Header, low: &[usize], kx: usize, patches: &[(usize, usize)]) -> Vec<usize> {
        if h.limiter_bands == 0 {
            return vec![low[0], *low.last().unwrap()];
        }
        let per_octave = [1.2, 2.0, 3.0][h.limiter_bands - 1];
        let mut borders = vec![kx];
        for &(_, w) in patches {
            borders.push(borders.last().unwrap() + w);
        }
        let mut table: Vec<usize> = low.to_vec();
        table.extend_from_slice(&borders[1..borders.len() - 1]);
        table.sort_unstable();
        let is_border = |k: usize| borders.contains(&k);
        let mut k = 1;
        while k < table.len() {
            let octaves = (table[k] as f64 / table[k - 1] as f64).log2();
            if octaves * per_octave < 0.49 {
                if table[k] == table[k - 1] || !is_border(table[k]) {
                    table.remove(k);
                    continue;
                }
                if !is_border(table[k - 1]) {
                    table.remove(k - 1);
                    continue;
                }
            }
            k += 1;
        }
        table
    }
}

/// A frame's time grid (4.6.18.3.3): envelope borders in time slots (2 QMF
/// slots each), each envelope's frequency resolution, the noise-floor
/// borders and the transient envelope.
#[derive(Clone, Copy, Default)]
struct Grid {
    class: u32,
    envelopes: usize,
    borders: [usize; MAX_ENV + 1],
    /// High frequency resolution per envelope.
    high: [bool; MAX_ENV],
    noises: usize,
    noise_borders: [usize; 3],
    /// The envelope a transient starts (`lA`), none when negative; the
    /// envelope count when it starts at the frame's end.
    transient: i32,
}

impl Grid {
    fn read(b: &mut Bits) -> Result<Grid, Error> {
        let class = b.get(2)?;
        let mut g = Grid {
            class,
            ..Grid::default()
        };
        let rel = |b: &mut Bits, n: u32| -> Result<Vec<usize>, Error> {
            (0..n).map(|_| Ok(2 * b.get(2)? as usize + 2)).collect()
        };
        let pointer_bits = |n: usize| usize::BITS - n.leading_zeros(); // ⌈log2(n + 1)⌉
        let (lead, trail, rel_lead, rel_trail, pointer);
        match class {
            FIXFIX => {
                let n = 1usize << b.get(2)?;
                if n > 4 {
                    return Err(Error::Invalid("SBR envelopes"));
                }
                let high = b.get(1)? == 1;
                g.envelopes = n;
                g.high = [high; MAX_ENV];
                for l in 0..=n {
                    g.borders[l] = 16 * l / n;
                }
                g.transient = -1;
                g.set_noise(n / 2);
                return Ok(g);
            }
            FIXVAR => {
                lead = 0;
                trail = b.get(2)? as usize + 16;
                let n1 = b.get(2)?;
                rel_lead = Vec::new();
                rel_trail = rel(b, n1)?;
                g.envelopes = n1 as usize + 1;
                pointer = b.get(pointer_bits(g.envelopes))? as usize;
                for l in (0..g.envelopes).rev() {
                    g.high[l] = b.get(1)? == 1;
                }
            }
            VARFIX => {
                lead = b.get(2)? as usize;
                trail = 16;
                let n0 = b.get(2)?;
                rel_lead = rel(b, n0)?;
                rel_trail = Vec::new();
                g.envelopes = n0 as usize + 1;
                pointer = b.get(pointer_bits(g.envelopes))? as usize;
                for l in 0..g.envelopes {
                    g.high[l] = b.get(1)? == 1;
                }
            }
            _ => {
                lead = b.get(2)? as usize;
                trail = b.get(2)? as usize + 16;
                let n0 = b.get(2)?;
                let n1 = b.get(2)?;
                rel_lead = rel(b, n0)?;
                rel_trail = rel(b, n1)?;
                g.envelopes = (n0 + n1) as usize + 1;
                if g.envelopes > MAX_ENV {
                    return Err(Error::Invalid("SBR envelopes"));
                }
                pointer = b.get(pointer_bits(g.envelopes))? as usize;
                for l in 0..g.envelopes {
                    g.high[l] = b.get(1)? == 1;
                }
            }
        }
        let n = g.envelopes;
        if pointer > n + 1 {
            return Err(Error::Invalid("SBR pointer"));
        }
        g.borders[0] = lead;
        g.borders[n] = trail;
        for (l, &r) in rel_lead.iter().enumerate() {
            g.borders[l + 1] = g.borders[l] + r;
        }
        for (i, &r) in rel_trail.iter().enumerate() {
            g.borders[n - 1 - i] = g.borders[n - i]
                .checked_sub(r)
                .ok_or(Error::Invalid("SBR borders"))?;
        }
        if g.borders[..=n].windows(2).any(|w| w[0] >= w[1]) {
            return Err(Error::Invalid("SBR borders"));
        }
        let (transient, middle) = if class == VARFIX {
            (
                if pointer > 1 { pointer as i32 - 1 } else { -1 },
                match pointer {
                    0 => 1,
                    1 => n - 1,
                    p => p - 1,
                },
            )
        } else {
            (
                if pointer > 0 {
                    (n + 1 - pointer) as i32
                } else {
                    -1
                },
                if pointer > 1 { n + 1 - pointer } else { n - 1 },
            )
        };
        g.transient = transient;
        g.set_noise(middle);
        Ok(g)
    }

    /// One noise floor for one envelope, else two split at envelope border
    /// `middle`.
    fn set_noise(&mut self, middle: usize) {
        let n = self.envelopes;
        self.noises = if n > 1 { 2 } else { 1 };
        self.noise_borders = [
            self.borders[0],
            self.borders[middle.min(n)],
            self.borders[n],
        ];
        if self.noises == 1 {
            self.noise_borders[1] = self.borders[n];
        }
    }

    /// The noise floor that covers envelope `l`.
    fn noise_of(&self, l: usize) -> usize {
        usize::from(self.noises == 2 && self.borders[l] >= self.noise_borders[1])
    }
}

/// One channel's data of one frame, as coded (scale factors as indices).
#[derive(Clone, Copy)]
struct Frame {
    grid: Grid,
    /// 3 dB envelope steps in this frame.
    amp_res: bool,
    invf: [u8; MAX_NOISE],
    envelope: [[i32; 64]; MAX_ENV],
    noise: [[i32; MAX_NOISE]; 2],
    /// Added sinusoids per high-resolution band.
    sines: [bool; 64],
}

impl Default for Frame {
    fn default() -> Frame {
        Frame {
            grid: Grid::default(),
            amp_res: false,
            invf: [0; MAX_NOISE],
            envelope: [[0; 64]; MAX_ENV],
            noise: [[0; MAX_NOISE]; 2],
            sines: [false; 64],
        }
    }
}

/// The scale factors a channel's next frame is delta-coded against: its
/// last envelope (with its resolution) and last noise floor.
#[derive(Clone, Copy)]
struct Last {
    envelope: [i32; 64],
    high: bool,
    noise: [i32; MAX_NOISE],
}

impl Default for Last {
    fn default() -> Last {
        Last {
            envelope: [0; 64],
            high: true,
            noise: [0; MAX_NOISE],
        }
    }
}

/// The delta directions of a frame (bs_df_env, bs_df_noise; true = time).
#[derive(Clone, Copy, Default)]
struct Directions {
    envelope: [bool; MAX_ENV],
    noise: [bool; 2],
}

impl Directions {
    fn read(b: &mut Bits, g: &Grid) -> Result<Directions, Error> {
        let mut d = Directions::default();
        for v in &mut d.envelope[..g.envelopes] {
            *v = b.get(1)? == 1;
        }
        for v in &mut d.noise[..g.noises] {
            *v = b.get(1)? == 1;
        }
        Ok(d)
    }
}

/// The band count of a resolution.
fn band_count(bands: &Bands, high: bool) -> usize {
    if high {
        bands.high.len() - 1
    } else {
        bands.low.len() - 1
    }
}

/// Envelope scale factors (4.6.18.3.4): per envelope a start value and
/// frequency deltas, or time deltas against the previous envelope (mapped
/// between resolutions). The balance channel of a coupled pair has its
/// own tables and steps of 2.
fn read_envelope(
    b: &mut Bits,
    bands: &Bands,
    f: &mut Frame,
    dir: &Directions,
    last: &mut Last,
    balance: bool,
) -> Result<(), Error> {
    let g = &f.grid;
    let fine = !f.amp_res;
    let (t_table, f_table, start_bits) = match (balance, fine) {
        (false, true) => (ENV15_T, ENV15_F, 7),
        (false, false) => (ENV30_T, ENV30_F, 6),
        (true, true) => (BAL15_T, BAL15_F, 6),
        (true, false) => (BAL30_T, BAL30_F, 5),
    };
    let step = if balance { 2 } else { 1 };
    for l in 0..g.envelopes {
        let high = g.high[l];
        let n = band_count(bands, high);
        let env = &mut f.envelope[l];
        if dir.envelope[l] {
            for k in 0..n {
                // The band of the previous envelope at the same place.
                let p = match (high, last.high) {
                    (a, b) if a == b => k,
                    (true, false) => {
                        let f0 = bands.high[k];
                        bands
                            .low
                            .windows(2)
                            .position(|w| w[0] <= f0 && f0 < w[1])
                            .unwrap_or(0)
                    }
                    _ => {
                        let f0 = bands.low[k];
                        bands.high.iter().position(|&h| h == f0).unwrap_or(0)
                    }
                };
                env[k] = last.envelope[p] + step * huff(b, t_table)?;
            }
        } else {
            env[0] = step * b.get(start_bits)? as i32;
            for k in 1..n {
                env[k] = env[k - 1] + step * huff(b, f_table)?;
            }
        }
        last.envelope = *env;
        last.high = high;
    }
    Ok(())
}

/// Noise-floor scale factors, coded like the envelopes (5-bit start, the
/// 3 dB frequency tables, own time tables).
fn read_noise(
    b: &mut Bits,
    bands: &Bands,
    f: &mut Frame,
    dir: &Directions,
    last: &mut Last,
    balance: bool,
) -> Result<(), Error> {
    let n = bands.noise.len() - 1;
    let (t_table, f_table) = if balance {
        (NOISE_BAL_T, BAL30_F)
    } else {
        (NOISE_T, ENV30_F)
    };
    let step = if balance { 2 } else { 1 };
    for q in 0..f.grid.noises {
        let noise = &mut f.noise[q];
        if dir.noise[q] {
            for k in 0..n {
                noise[k] = last.noise[k] + step * huff(b, t_table)?;
            }
        } else {
            noise[0] = step * b.get(5)? as i32;
            for k in 1..n {
                noise[k] = noise[k - 1] + step * huff(b, f_table)?;
            }
        }
        last.noise = *noise;
    }
    Ok(())
}

/// One channel's coded state between frames and its filter-bank buffers.
struct Channel {
    /// This frame's data, if any arrived.
    frame: Option<Frame>,
    /// The last frame's data (repeated when a frame brings none).
    previous: Option<Frame>,
    last: Last,
    /// Inverse-filtering modes and chirp factors of the last frame.
    invf_prev: [u8; MAX_NOISE],
    chirp_prev: [f32; MAX_NOISE],
    /// Subbands (from `kx`) with a sinusoid in the last envelope.
    sines_prev: [bool; 64],
    /// The last frame's transient was at its end (`lA = L_E`).
    transient_at_end: bool,
    /// The last frame's final border (time slots) and band range.
    end_prev: usize,
    kx_prev: usize,
    m_prev: usize,
    /// Gain and noise histories for smoothing (newest at `hist`), and
    /// whether they still need filling.
    gains: [[f32; 64]; 5],
    noises: [[f32; 64]; 5],
    hist: usize,
    hist_fresh: bool,
    noise_index: usize,
    sine_index: usize,
    /// QMF analysis input (the core, newest last).
    x: Vec<f32>,
    /// Analysis output: [re/im][slot][subband], slot 8 the frame's first.
    low: [[[f32; 32]; SPAN]; 2],
    /// Adjusted high band, same slots; the part past the frame carries on.
    high: [[[f32; 64]; SPAN]; 2],
    v: History,
}

impl Channel {
    fn new() -> Channel {
        Channel {
            frame: None,
            previous: None,
            last: Last::default(),
            invf_prev: [0; MAX_NOISE],
            chirp_prev: [0.0; MAX_NOISE],
            sines_prev: [false; 64],
            transient_at_end: false,
            end_prev: 16,
            kx_prev: 32,
            m_prev: 0,
            gains: [[0.0; 64]; 5],
            noises: [[0.0; 64]; 5],
            hist: 0,
            hist_fresh: true,
            noise_index: 0,
            sine_index: 0,
            x: vec![0.0; 1312],
            low: [[[0.0; 32]; SPAN]; 2],
            high: [[[0.0; 64]; SPAN]; 2],
            v: History::new(1280),
        }
    }

    /// Forget everything that depends on the band tables.
    fn reset(&mut self) {
        self.frame = None;
        self.previous = None;
        self.last = Last::default();
        self.invf_prev = [0; MAX_NOISE];
        self.chirp_prev = [0.0; MAX_NOISE];
        self.sines_prev = [false; 64];
        self.transient_at_end = false;
        self.hist_fresh = true;
    }
}

/// The SBR decoder of one AAC element (one or two channels).
pub(crate) struct Sbr {
    /// SBR sampling rate (twice the core's).
    rate: u32,
    header: Option<Header>,
    bands: Option<Bands>,
    coupling: bool,
    ch: [Channel; 2],
    pub(crate) ps: Ps,
    lanes: Lanes,
    work: Box<Work>,
}

/// Per-frame buffers, kept between frames to avoid reallocating them.
struct Work {
    /// The frame's QMF signal and, for parametric stereo, its two outputs.
    x: Qmf,
    l: Qmf,
    r: Qmf,
    /// Generated high band before adjustment, [re/im][subband][slot].
    xh: [[[f32; SPAN]; 64]; 2],
}

impl Sbr {
    pub(crate) fn new(core_rate: u32) -> Sbr {
        Sbr {
            rate: 2 * core_rate,
            header: None,
            bands: None,
            coupling: false,
            ch: [Channel::new(), Channel::new()],
            ps: Ps::new(),
            lanes: Lanes::default(),
            work: Box::new(Work {
                x: [[[0.0; 64]; SLOTS]; 2],
                l: [[[0.0; 64]; SLOTS]; 2],
                r: [[[0.0; 64]; SLOTS]; 2],
                xh: [[[0.0; SPAN]; 64]; 2],
            }),
        }
    }

    /// Whether a usable header has arrived.
    pub(crate) fn started(&self) -> bool {
        self.bands.is_some()
    }

    /// The SBR extension payload of a fill element (4.5.2.8.1) for the
    /// element `id` before it: CRC bits, an optional header, the frame
    /// data. A damaged payload leaves the frame without data.
    pub(crate) fn read(&mut self, b: &mut Bits, crc: bool, id: u32, ps_allowed: bool) {
        for ch in &mut self.ch {
            ch.frame = None;
        }
        if self.read_payload(b, crc, id, ps_allowed).is_err() {
            for ch in &mut self.ch {
                ch.frame = None;
            }
        }
    }

    fn read_payload(
        &mut self,
        b: &mut Bits,
        crc: bool,
        id: u32,
        ps_allowed: bool,
    ) -> Result<(), Error> {
        if crc {
            b.skip(10)?;
        }
        if b.get(1)? == 1 {
            let h = Header::read(b)?;
            let reset = self.header.is_none_or(|old| !old.same_bands(&h));
            self.header = Some(h);
            if reset {
                self.bands = Bands::new(&h, self.rate);
                for ch in &mut self.ch {
                    ch.reset();
                }
            }
        }
        let (Some(h), Some(bands)) = (self.header, self.bands.as_ref()) else {
            return Ok(());
        };
        let mut frames = [Frame::default(); 2];
        let mut lasts = [self.ch[0].last, self.ch[1].last];
        if id == 1 {
            if b.get(1)? == 1 {
                b.skip(8)?; // reserved
            }
            let coupling = b.get(1)? == 1;
            frames[0].grid = Grid::read(b)?;
            frames[1].grid = if coupling {
                frames[0].grid
            } else {
                Grid::read(b)?
            };
            let d0 = Directions::read(b, &frames[0].grid)?;
            let d1 = Directions::read(b, &frames[1].grid)?;
            for f in &mut frames {
                f.amp_res = amp_res(&h, &f.grid);
            }
            let n_q = bands.noise.len() - 1;
            for i in 0..n_q {
                frames[0].invf[i] = b.get(2)? as u8;
            }
            if coupling {
                frames[1].invf = frames[0].invf;
                let [f0, f1] = &mut frames;
                let [l0, l1] = &mut lasts;
                read_envelope(b, bands, f0, &d0, l0, false)?;
                read_noise(b, bands, f0, &d0, l0, false)?;
                read_envelope(b, bands, f1, &d1, l1, true)?;
                read_noise(b, bands, f1, &d1, l1, true)?;
            } else {
                for i in 0..n_q {
                    frames[1].invf[i] = b.get(2)? as u8;
                }
                let [f0, f1] = &mut frames;
                let [l0, l1] = &mut lasts;
                read_envelope(b, bands, f0, &d0, l0, false)?;
                read_envelope(b, bands, f1, &d1, l1, false)?;
                read_noise(b, bands, f0, &d0, l0, false)?;
                read_noise(b, bands, f1, &d1, l1, false)?;
            }
            for f in &mut frames {
                read_sines(b, bands, f)?;
            }
            self.coupling = coupling;
        } else {
            if b.get(1)? == 1 {
                b.skip(4)?; // reserved
            }
            let f = &mut frames[0];
            f.grid = Grid::read(b)?;
            let d = Directions::read(b, &f.grid)?;
            f.amp_res = amp_res(&h, &f.grid);
            for i in 0..bands.noise.len() - 1 {
                f.invf[i] = b.get(2)? as u8;
            }
            read_envelope(b, bands, f, &d, &mut lasts[0], false)?;
            read_noise(b, bands, f, &d, &mut lasts[0], false)?;
            read_sines(b, bands, f)?;
            self.coupling = false;
        }
        let channels = if id == 1 { 2 } else { 1 };
        for c in 0..channels {
            self.ch[c].frame = Some(frames[c]);
            self.ch[c].last = lasts[c];
        }
        // Extensions: parametric stereo (id 2) for a single channel.
        if b.get(1)? == 1 {
            let mut size = b.get(4)? as usize;
            if size == 15 {
                size += b.get(8)? as usize;
            }
            let mut left = 8 * size;
            while left > 7 {
                let ext = b.get(2)?;
                left -= 2;
                if ext == 2 && id == 0 && ps_allowed {
                    let used = self.ps.read(b, left);
                    left = left.saturating_sub(used);
                } else {
                    b.skip(left)?;
                    left = 0;
                }
            }
            b.skip(left)?;
        }
        Ok(())
    }
}

/// 1.5 dB steps are used for a single FIXFIX envelope whatever the header
/// says.
fn amp_res(h: &Header, g: &Grid) -> bool {
    h.amp_res && !(g.class == FIXFIX && g.envelopes == 1)
}

fn read_sines(b: &mut Bits, bands: &Bands, f: &mut Frame) -> Result<(), Error> {
    if b.get(1)? == 1 {
        for s in &mut f.sines[..bands.high.len() - 1] {
            *s = b.get(1)? == 1;
        }
    }
    Ok(())
}

/// Smoothing filter of the gains and noise levels over 5 slots (newest
/// first), 4.6.18.7.5.
const SMOOTH: [f32; 5] = [
    0.333_333_33,
    0.301_502_83,
    0.218_169_5,
    0.115_163_83,
    0.031_830_5,
];

/// Limiter gains (amplitude): −3, 0, +3 dB and none.
const LIMITER_GAIN: [f32; 4] = [0.707_945_8, 1.0, 1.412_537_5, 1e10];

/// Dequantized scale factors of one channel's frame (4.6.18.3.5):
/// envelope energies per band and noise-floor ratios.
struct Levels {
    envelope: [[f32; 64]; MAX_ENV],
    noise: [[f32; MAX_NOISE]; 2],
}

fn dequantize(f: &Frame, bands: &Bands) -> Levels {
    let a = if f.amp_res { 1.0 } else { 0.5 };
    let mut out = Levels {
        envelope: [[0.0; 64]; MAX_ENV],
        noise: [[0.0; MAX_NOISE]; 2],
    };
    for l in 0..f.grid.envelopes {
        for k in 0..band_count(bands, f.grid.high[l]) {
            out.envelope[l][k] = (f.envelope[l][k] as f32 * a + 6.0).exp2();
        }
    }
    for q in 0..f.grid.noises {
        for k in 0..bands.noise.len() - 1 {
            out.noise[q][k] = (6.0 - f.noise[q][k] as f32).exp2();
        }
    }
    out
}

/// A coupled pair: level from the first channel, balance from the second.
fn dequantize_coupled(f0: &Frame, f1: &Frame, bands: &Bands) -> [Levels; 2] {
    let a = if f0.amp_res { 1.0 } else { 0.5 };
    let pan = if f0.amp_res { 12.0 } else { 24.0 };
    let mut out = [dequantize(f0, bands), dequantize(f0, bands)];
    for l in 0..f0.grid.envelopes {
        for k in 0..band_count(bands, f0.grid.high[l]) {
            let level = (f0.envelope[l][k] as f32 * a + 7.0).exp2();
            let balance = f1.envelope[l][k] as f32;
            out[0].envelope[l][k] = level / (1.0 + ((pan - balance) * a).exp2());
            out[1].envelope[l][k] = level / (1.0 + ((balance - pan) * a).exp2());
        }
    }
    for q in 0..f0.grid.noises {
        for k in 0..bands.noise.len() - 1 {
            let level = (7.0 - f0.noise[q][k] as f32).exp2();
            let balance = f1.noise[q][k] as f32;
            out[0].noise[q][k] = level / (1.0 + (12.0 - balance).exp2());
            out[1].noise[q][k] = level / (1.0 + (balance - 12.0).exp2());
        }
    }
    out
}

/// The chirp factor of a noise band from its inverse-filtering mode, the
/// last frame's mode and factor (4.6.18.6.2).
fn chirp(mode: u8, prev_mode: u8, prev: f32) -> f32 {
    let target = match (mode, prev_mode) {
        (0, 1) => 0.6,
        (0, _) => 0.0,
        (1, 0) => 0.6,
        (1, _) => 0.75,
        (2, _) => 0.9,
        _ => 0.98,
    };
    let c = if target < prev {
        0.75 * target + 0.25 * prev
    } else {
        0.90625 * target + 0.09375 * prev
    };
    if c < 0.015625 { 0.0 } else { c }
}

/// Second-order complex predictor of each low subband from the analysis
/// buffer (covariance method, 4.6.18.6.2): (α0, α1) as (re, im) pairs,
/// zero when unstable or degenerate.
fn predictors(low: &[[[f32; 32]; SPAN]; 2], count: usize) -> [[[f32; 2]; 2]; 32] {
    // φ(i, j) = Σ x(n − i) x*(n − j) over n = 0..38 of slots offset by
    // ADJ: entries (0,1), (0,2), (1,2) complex; (1,1), (2,2) real. The
    // shifted sums share all but their end terms: from the lag-1 sum L1,
    // the lag-2 sum L2 and the energy E over the buffer, φ01 and φ12 are
    // L1 less one end product, φ11 and φ22 are E less one end energy.
    // All subbands are summed side by side, slot by slot.
    const LAST: usize = SLOTS + 6 + 1; // buffer slots 0..=LAST are used
    let (re, im) = (&low[0], &low[1]);
    let mut l1 = [[0f64; 32]; 2];
    let mut l2 = [[0f64; 32]; 2];
    let mut energy = [0f64; 32];
    for m in 1..=LAST {
        for k in 0..count {
            let (cr, ci) = (f64::from(re[m][k]), f64::from(im[m][k]));
            let (ar, ai) = (f64::from(re[m - 1][k]), f64::from(im[m - 1][k]));
            energy[k] += ar * ar + ai * ai;
            l1[0][k] += cr * ar + ci * ai;
            l1[1][k] += ci * ar - cr * ai;
        }
    }
    for m in 2..=LAST {
        for k in 0..count {
            let (cr, ci) = (f64::from(re[m][k]), f64::from(im[m][k]));
            let (br, bi) = (f64::from(re[m - 2][k]), f64::from(im[m - 2][k]));
            l2[0][k] += cr * br + ci * bi;
            l2[1][k] += ci * br - cr * bi;
        }
    }
    let mut out = [[[0.0; 2]; 2]; 32];
    for (k, o) in out.iter_mut().enumerate().take(count) {
        let x = |n: usize| (f64::from(re[n][k]), f64::from(im[n][k]));
        // L1 = Σ_{m<LAST} x(m+1)x*(m), E = Σ_{m<LAST} |x(m)|².
        let (x0, x1) = (x(0), x(1));
        let (xa, xb) = (x(LAST), x(LAST - 1));
        let p01 = (
            l1[0][k] - (x1.0 * x0.0 + x1.1 * x0.1),
            l1[1][k] - (x1.1 * x0.0 - x1.0 * x0.1),
        );
        let p12 = (
            l1[0][k] - (xa.0 * xb.0 + xa.1 * xb.1),
            l1[1][k] - (xa.1 * xb.0 - xa.0 * xb.1),
        );
        let p02 = (l2[0][k], l2[1][k]);
        let p11 = energy[k] - (x0.0 * x0.0 + x0.1 * x0.1);
        let p22 = energy[k] - (xb.0 * xb.0 + xb.1 * xb.1);
        let d = p22 * p11 - (p12.0 * p12.0 + p12.1 * p12.1) / (1.0 + 1e-6);
        let mut a1 = (0.0, 0.0);
        if d != 0.0 {
            // (φ01 φ12 − φ02 φ11) / d
            a1.0 = (p01.0 * p12.0 - p01.1 * p12.1 - p02.0 * p11) / d;
            a1.1 = (p01.0 * p12.1 + p01.1 * p12.0 - p02.1 * p11) / d;
        }
        let mut a0 = (0.0, 0.0);
        if p11 != 0.0 {
            // −(φ01 + α1 φ12*) / φ11
            a0.0 = -(p01.0 + a1.0 * p12.0 + a1.1 * p12.1) / p11;
            a0.1 = -(p01.1 + a1.1 * p12.0 - a1.0 * p12.1) / p11;
        }
        let big = |v: (f64, f64)| v.0 * v.0 + v.1 * v.1 >= 16.0;
        if big(a0) || big(a1) {
            continue;
        }
        *o = [[a0.0 as f32, a0.1 as f32], [a1.0 as f32, a1.1 as f32]];
    }
    out
}

impl Sbr {
    /// The SBR part of one channel's frame: HF generation and adjustment
    /// into `ch.high` over the frame's envelope slots.
    fn generate(
        ch: &mut Channel,
        f: &Frame,
        levels: &Levels,
        bands: &Bands,
        h: &Header,
        xh: &mut [[[f32; SPAN]; 64]; 2],
    ) {
        let g = &f.grid;
        let (kx, m) = (bands.kx, bands.m);
        let first = 2 * g.borders[0] + ADJ;
        let end = 2 * g.borders[g.envelopes] + ADJ;
        // Chirp factors per noise band.
        let n_q = bands.noise.len() - 1;
        let mut chirps = [0f32; MAX_NOISE];
        for i in 0..n_q {
            chirps[i] = chirp(f.invf[i], ch.invf_prev[i], ch.chirp_prev[i]);
        }
        ch.chirp_prev = chirps;
        ch.invf_prev = f.invf;
        // HF generation: each patch copies its source subbands up,
        // inverse-filtered with the band's chirp factor.
        // Predictors of the source subbands the patches use.
        let sources = bands.patches.iter().map(|&(p, w)| p + w).max().unwrap_or(0);
        let alpha = predictors(&ch.low, sources.min(32));
        let mut k = kx;
        for &(src, width) in &bands.patches {
            for p in src..src + width {
                let bw = chirps[bands.noise_of[k - kx] as usize];
                let [a0, a1] = alpha[p];
                let (c0, c1) = ([bw * a0[0], bw * a0[1]], [bw * bw * a1[0], bw * bw * a1[1]]);
                let (lr, li) = (&ch.low[0], &ch.low[1]);
                for i in first..end {
                    let (r0, i0) = (lr[i][p], li[i][p]);
                    let (r1, i1) = (lr[i - 1][p], li[i - 1][p]);
                    let (r2, i2) = (lr[i - 2][p], li[i - 2][p]);
                    xh[0][k][i] = r0 + c0[0] * r1 - c0[1] * i1 + c1[0] * r2 - c1[1] * i2;
                    xh[1][k][i] = i0 + c0[0] * i1 + c0[1] * r1 + c1[0] * i2 + c1[1] * r2;
                }
                k += 1;
            }
        }
        // Sinusoid placement: the middle subband of each high-resolution
        // band, from the transient envelope on (or at once when the last
        // frame already had it).
        let mut sine_at = [false; 64];
        for i in 0..bands.high.len() - 1 {
            if f.sines[i] {
                sine_at[(bands.high[i] + bands.high[i + 1]) / 2 - kx] = true;
            }
        }
        let transient_prev = ch.transient_at_end;
        let smoothing = !h.smoothing_mode;
        let limiter_gain = LIMITER_GAIN[h.limiter_gains];
        let mut noise_index = ch.noise_index;
        let mut sine_index = ch.sine_index;
        let mut sines_now = [false; 64];
        for l in 0..g.envelopes {
            let table = if g.high[l] { &bands.high } else { &bands.low };
            let (s0, s1) = (2 * g.borders[l] + ADJ, 2 * g.borders[l + 1] + ADJ);
            let slots = (s1 - s0) as f32;
            let q_env = g.noise_of(l);
            let transient = l as i32 == g.transient || (l == 0 && transient_prev);
            // Per subband: original energy, noise ratio, sinusoid flags,
            // current energy.
            let mut e_orig = [0f32; 64];
            let mut q_orig = [0f32; 64];
            let mut sine = [false; 64];
            let mut sine_band = [false; 64];
            let mut e_curr = [0f32; 64];
            for (b, w) in table.windows(2).enumerate() {
                let mut any = false;
                for k in w[0]..w[1] {
                    let j = k - kx;
                    sine[j] = sine_at[j] && (l as i32 >= g.transient || ch.sines_prev[j]);
                    any |= sine[j];
                }
                let mut band_energy = 0f32;
                for k in w[0]..w[1] {
                    let j = k - kx;
                    e_orig[j] = levels.envelope[l][b];
                    q_orig[j] = levels.noise[q_env][bands.noise_of[j] as usize];
                    sine_band[j] = any;
                    let mut e = 0f32;
                    for i in s0..s1 {
                        e += xh[0][k][i] * xh[0][k][i] + xh[1][k][i] * xh[1][k][i];
                    }
                    e_curr[j] = e / slots;
                    band_energy += e;
                }
                if !h.interpol_freq {
                    let mean = band_energy / (slots * (w[1] - w[0]) as f32);
                    e_curr[w[0] - kx..w[1] - kx].fill(mean);
                }
            }
            if l == g.envelopes - 1 {
                sines_now = sine;
            }
            // Gains (energies), then limited and boosted per limiter band.
            let mut gain = [0f32; 64];
            let mut noise = [0f32; 64];
            let mut sinus = [0f32; 64];
            for j in 0..m {
                let (e, q) = (e_orig[j], q_orig[j]);
                noise[j] = e * q / (1.0 + q);
                sinus[j] = if sine[j] { e / (1.0 + q) } else { 0.0 };
                gain[j] = if !sine_band[j] {
                    let added = if transient { 0.0 } else { q };
                    e / ((1.0 + e_curr[j]) * (1.0 + added))
                } else {
                    e * q / ((1.0 + e_curr[j]) * (1.0 + q))
                };
            }
            const EPS: f32 = 1e-12;
            for w in bands.limiter.windows(2) {
                let (a, b) = (w[0] - kx, w[1] - kx);
                let orig: f32 = e_orig[a..b].iter().sum();
                let curr: f32 = e_curr[a..b].iter().sum();
                let max = (limiter_gain * limiter_gain * (EPS + orig) / (EPS + curr)).min(1e10);
                let mut total = 0f32;
                for j in a..b {
                    if gain[j] > max {
                        noise[j] *= max / gain[j];
                        gain[j] = max;
                    }
                    total += e_curr[j] * gain[j] + sinus[j];
                    if sinus[j] == 0.0 && !transient {
                        total += noise[j];
                    }
                }
                // At most +4 dB (1.584893192 in amplitude).
                let boost = ((EPS + orig) / (EPS + total)).min(1.584_893_2 * 1.584_893_2);
                for j in a..b {
                    gain[j] = (gain[j] * boost).sqrt();
                    noise[j] = (noise[j] * boost).sqrt();
                    sinus[j] = (sinus[j] * boost).sqrt();
                }
            }
            if ch.hist_fresh {
                for s in 0..5 {
                    ch.gains[s][..m].copy_from_slice(&gain[..m]);
                    ch.noises[s][..m].copy_from_slice(&noise[..m]);
                }
                ch.hist_fresh = false;
            }
            for i in s0..s1 {
                ch.hist = (ch.hist + 1) % 5;
                ch.gains[ch.hist][..m].copy_from_slice(&gain[..m]);
                ch.noises[ch.hist][..m].copy_from_slice(&noise[..m]);
                let smooth = smoothing && !transient;
                let (yr, yi) = {
                    let [hr, hi] = &mut ch.high;
                    (&mut hr[i], &mut hi[i])
                };
                for j in 0..m {
                    let k = kx + j;
                    let (gf, qf) = if smooth {
                        let mut gs = 0f32;
                        let mut qs = 0f32;
                        for (t, c) in SMOOTH.iter().enumerate() {
                            let s = (ch.hist + 5 - t) % 5;
                            gs += c * ch.gains[s][j];
                            qs += c * ch.noises[s][j];
                        }
                        (gs, qs)
                    } else {
                        (gain[j], noise[j])
                    };
                    let mut re = xh[0][k][i] * gf;
                    let mut im = xh[1][k][i] * gf;
                    let index = (noise_index + j + 1) & 511;
                    if sinus[j] != 0.0 {
                        let sign = if k % 2 == 1 { -1.0 } else { 1.0 };
                        match sine_index {
                            0 => re += sinus[j],
                            1 => im += sign * sinus[j],
                            2 => re -= sinus[j],
                            _ => im -= sign * sinus[j],
                        }
                    } else if !transient {
                        re += qf * tab::NOISE[index][0];
                        im += qf * tab::NOISE[index][1];
                    }
                    yr[k] = re;
                    yi[k] = im;
                }
                noise_index = (noise_index + m) & 511;
                sine_index = (sine_index + 1) & 3;
            }
        }
        ch.noise_index = noise_index;
        ch.sine_index = sine_index;
        ch.sines_prev = sines_now;
        ch.transient_at_end = g.transient == g.envelopes as i32;
    }
}

impl Sbr {
    /// 32 slots of QMF analysis of 1024 core samples into the analysis
    /// buffer after the kept slots.
    fn analysis(ch: &mut Channel, input: &[f32], sc: &mut Lanes) {
        const L: usize = 32;
        let st = statics();
        let x = &mut ch.x;
        x.copy_within(1024..1312, 0);
        for (d, &v) in x[288..].iter_mut().zip(input) {
            *d = v * SCALE;
        }
        let (a, d) = (&mut sc.x[..64 * L], &mut sc.y[..64 * L]);
        for slot in 0..32 {
            // u(n) = Σ_j c(2(n + 64j))·x(n + 64j), x(0) the newest sample.
            let newest = 32 * slot + 319;
            let mut u = [0f32; 64];
            for j in 0..5 {
                let c = &tab::QMF_WINDOW[128 * j..];
                let xs = &x[newest - 64 * j - 63..=newest - 64 * j];
                for n in 0..64 {
                    u[n] += c[2 * n] * xs[63 - n];
                }
            }
            a[slot] = u[0];
            for p in 1..32 {
                a[2 * p * L + slot] = -u[64 - p];
            }
            for p in 0..32 {
                a[(2 * p + 1) * L + slot] = u[p + 1];
            }
        }
        st.dct64
            .run_lanes(a, d, &mut sc.re[..32 * L], &mut sc.im[..32 * L], L);
        for part in &mut ch.low {
            part.copy_within(SLOTS..SPAN, 0);
        }
        for slot in 0..SLOTS {
            for k in 0..32 {
                ch.low[0][KEPT + slot][k] = 2.0 * d[k * L + slot];
                ch.low[1][KEPT + slot][k] = -2.0 * d[(63 - k) * L + slot];
            }
        }
    }

    /// 32 slots of 64-band QMF synthesis of `x` into 2048 samples.
    fn synthesis(ch: &mut Channel, x: &Qmf, out: &mut [f32], sc: &mut Lanes) {
        const L: usize = 64;
        let c = &tab::QMF_WINDOW;
        let (xin, y) = (&mut sc.x[..], &mut sc.y[..]);
        for slot in 0..32 {
            let (xr, xi) = (&x[0][slot], &x[1][slot]);
            for k in 0..64 {
                xin[k * L + slot] = xr[k];
                xin[(63 - k) * L + 32 + slot] = xi[k];
            }
        }
        statics().dct64.run_lanes(xin, y, &mut sc.re, &mut sc.im, L);
        for slot in 0..32 {
            let v = ch.v.push(1280, 128);
            for n in 0..64 {
                let c4 = y[n * L + slot];
                let s4 = if n % 2 == 0 { 1.0 } else { -1.0 } * y[n * L + 32 + slot];
                v[n] = (s4 - c4) * (1.0 / 64.0);
                v[127 - n] = (c4 + s4) * (1.0 / 64.0);
            }
            let o = &mut out[64 * slot..64 * slot + 64];
            o.fill(0.0);
            for i in 0..5 {
                let (a, b) = (&v[256 * i..256 * i + 64], &v[256 * i + 192..256 * i + 256]);
                let (ca, cb) = (&c[128 * i..128 * i + 64], &c[128 * i + 64..128 * i + 128]);
                for j in 0..64 {
                    o[j] += a[j] * ca[j] + b[j] * cb[j];
                }
            }
            for s in o.iter_mut() {
                *s /= SCALE;
            }
        }
    }

    /// Downsampled synthesis (4.6.18.4.3): the lower 32 bands into 1024
    /// samples at the core rate, for SBR signalled at the core's rate.
    fn synthesis_ds(ch: &mut Channel, x: &Qmf, out: &mut [f32], _: &mut Lanes) {
        let st = statics();
        let c = &tab::QMF_WINDOW;
        let (mut re, mut im) = ([0f32; 64], [0f32; 64]);
        for slot in 0..32 {
            let v = ch.v.push(640, 64);
            let (xr, xi) = (&x[0][slot], &x[1][slot]);
            let (pr, pi) = (&st.syn_pre.re, &st.syn_pre.im);
            for k in 0..32 {
                re[k] = xr[k] * pr[k] - xi[k] * pi[k];
                im[k] = xr[k] * pi[k] + xi[k] * pr[k];
            }
            re[32..].fill(0.0);
            im[32..].fill(0.0);
            st.fft64.inverse(&mut re, &mut im);
            let (qr, qi) = (&st.ds_post.re, &st.ds_post.im);
            for n in 0..64 {
                v[n] = re[n] * qr[n] - im[n] * qi[n];
            }
            for j in 0..32 {
                let mut s = 0f32;
                for i in 0..5 {
                    s += v[128 * i + j] * c[2 * (64 * i + j)];
                    s += v[128 * i + 96 + j] * c[2 * (64 * i + 32 + j)];
                }
                out[32 * slot + j] = s / SCALE;
            }
        }
    }
}

impl Sbr {
    /// One channel's frame for this call: its data, or the last frame's
    /// repeated over one envelope when none arrived (concealment).
    fn frame_for(ch: &Channel) -> Option<Frame> {
        if let Some(f) = ch.frame {
            return Some(f);
        }
        let mut f = ch.previous?;
        let mut g = Grid {
            class: FIXFIX,
            envelopes: 1,
            transient: -1,
            ..Grid::default()
        };
        g.borders[0] = ch.end_prev.saturating_sub(16);
        g.borders[1] = 16;
        g.high[0] = ch.last.high;
        g.set_noise(0);
        f.grid = g;
        f.envelope[0] = ch.last.envelope;
        f.noise[0] = ch.last.noise;
        f.sines = [false; 64];
        Some(f)
    }

    /// Decode one frame of element `id` from its core output: SBR on each
    /// channel, then QMF synthesis into `out_l` (and `out_r` for a pair, or
    /// for parametric stereo when `ps`). `down`: synthesis at the core rate.
    pub(crate) fn apply(
        &mut self,
        id: u32,
        core: [&[f32]; 2],
        ps: bool,
        down: bool,
        out_l: &mut [f32],
        out_r: &mut [f32],
    ) {
        let channels = if id == 1 { 2 } else { 1 };
        let frames: [Option<Frame>; 2] = [
            Self::frame_for(&self.ch[0]).filter(|_| self.started()),
            Self::frame_for(&self.ch[1]).filter(|_| self.started() && channels == 2),
        ];
        let levels: [Option<Levels>; 2] = match (&self.bands, frames) {
            (Some(bands), [Some(f0), Some(f1)])
                if channels == 2 && self.coupling && self.ch[0].frame.is_some() =>
            {
                let [a, b] = dequantize_coupled(&f0, &f1, bands);
                [Some(a), Some(b)]
            }
            (Some(bands), _) => frames.map(|f| f.map(|f| dequantize(&f, bands))),
            _ => [None, None],
        };
        for c in 0..channels {
            let ch = &mut self.ch[c];
            Self::analysis(ch, core[c], &mut self.lanes);
            // The high band past the last frame carries into this one.
            let [hr, hi] = &mut ch.high;
            for part in [hr, hi] {
                part.copy_within(SLOTS..SPAN, 0);
                for s in &mut part[KEPT..] {
                    s.fill(0.0);
                }
            }
            let carried = (2 * ch.end_prev).saturating_sub(SLOTS);
            let (kx, m) = match (&self.bands, &self.header, frames[c], &levels[c]) {
                (Some(bands), Some(h), Some(f), Some(lv)) => {
                    Self::generate(ch, &f, lv, bands, h, &mut self.work.xh);
                    ch.previous = Some(f);
                    ch.end_prev = f.grid.borders[f.grid.envelopes];
                    (bands.kx, bands.m)
                }
                _ => {
                    ch.end_prev = 16;
                    (32, 0)
                }
            };
            let Work { x, l, r, .. } = &mut *self.work;
            for j in 0..SLOTS {
                let (kx, m) = if j < carried {
                    (ch.kx_prev, ch.m_prev)
                } else {
                    (kx, m)
                };
                let s = j + ADJ;
                for part in 0..2 {
                    x[part][j][..kx].copy_from_slice(&ch.low[part][s][..kx]);
                    x[part][j][kx..kx + m].copy_from_slice(&ch.high[part][s][kx..kx + m]);
                    x[part][j][kx + m..].fill(0.0);
                }
            }
            ch.kx_prev = kx;
            ch.m_prev = m;
            ch.frame = None;
            if ps && channels == 1 {
                // Parametric stereo: two frames from the mono one, the
                // right one through the second channel's synthesis.
                // The hybrid filters' look-ahead: the lowest subbands 6
                // slots on, already in the analysis buffer.
                let mut ahead = [[[0f32; 2]; SLOTS]; 5];
                for (q, row) in ahead.iter_mut().enumerate() {
                    for (j, v) in row.iter_mut().enumerate() {
                        let s = j + ADJ + 6;
                        *v = [ch.low[0][s][q], ch.low[1][s][q]];
                    }
                }
                self.ps.apply(x, &ahead, kx + m, l, r);
                let [c0, c1] = &mut self.ch;
                if down {
                    Self::synthesis_ds(c0, l, out_l, &mut self.lanes);
                    Self::synthesis_ds(c1, r, out_r, &mut self.lanes);
                } else {
                    Self::synthesis(c0, l, out_l, &mut self.lanes);
                    Self::synthesis(c1, r, out_r, &mut self.lanes);
                }
                continue;
            }
            let out = if c == 0 { &mut *out_l } else { &mut *out_r };
            if down {
                Self::synthesis_ds(ch, x, out, &mut self.lanes);
            } else {
                Self::synthesis(ch, x, out, &mut self.lanes);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn huffman_tables_decode_their_codes() {
        let st = statics();
        for (t, (codes, lens)) in tab::HUFF.iter().enumerate() {
            for (i, (&c, &l)) in codes.iter().zip(lens.iter()).enumerate() {
                let v = (u64::from(c) << (64 - u32::from(l))).to_be_bytes();
                let mut b = Bits::new(&v);
                assert_eq!(st.trees[t].decode(&mut b).unwrap() as usize, i, "table {t}");
            }
        }
    }

    /// With no SBR data the banks only upsample: a low tone comes out at
    /// twice the rate, delayed but otherwise the same.
    #[test]
    fn pure_upsampling_reconstructs() {
        let mut s = Sbr::new(24000);
        let f = 1000.0f32;
        let mut out = Vec::new();
        for frame in 0..8 {
            let core: Vec<f32> = (0..1024)
                .map(|i| {
                    let t = (frame * 1024 + i) as f32 / 24000.0;
                    0.5 * (2.0 * std::f32::consts::PI * f * t).sin()
                })
                .collect();
            let mut l = vec![0f32; 2048];
            let mut r = vec![0f32; 2048];
            s.apply(0, [&core, &core], false, false, &mut l, &mut r);
            out.extend(l);
        }
        // Compare against the tone at 48 kHz, finding the delay.
        let tail = &out[4096..];
        let best = (0..2000)
            .map(|d| {
                let err: f32 = tail
                    .iter()
                    .enumerate()
                    .map(|(i, &v)| {
                        let t = (4096 + i) as f32 / 48000.0 - d as f32 / 48000.0;
                        (v - 0.5 * (2.0 * std::f32::consts::PI * f * t).sin()).powi(2)
                    })
                    .sum();
                (err / tail.len() as f32, d)
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .unwrap();
        assert!(
            best.0.sqrt() < 1e-3,
            "rms error {} at delay {}",
            best.0.sqrt(),
            best.1
        );
    }
}
