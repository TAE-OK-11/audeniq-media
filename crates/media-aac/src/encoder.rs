// SPDX-License-Identifier: LGPL-2.1-or-later
//! AAC-LC encoder for deliveries (music-video sound at 256–384 kbit/s).
//!
//! Sine windows with block switching: an attack (high-passed energy of a
//! 128-sample block ten times its recent average) turns the frame into
//! eight short windows, framed by `LONG_START` / `LONG_STOP` (the encoder
//! reads two frames ahead to place them). Per scalefactor band and window
//! group it picks the coarsest scalefactor whose measured quantization
//! noise stays under a threshold — band energy minus a per-frame SNR,
//! leaned towards the frame's even noise level and floored by the absolute
//! threshold of hearing (capped, so the highs stay) — and searches that
//! SNR so each frame fits its bit budget (the target rate plus a bit
//! reservoir). Codebooks and sections are chosen by dynamic programming;
//! M/S stereo per band where the channels are alike. The output decodes
//! with this crate's decoder and FFmpeg's (tests compare both with the
//! source).
use crate::mdct::{Imdct, sine};
use crate::tables::{
    NUM_SWB_LONG, NUM_SWB_SHORT, SF_BITS, SF_CODES, SPECTRAL, SWB_LONG, SWB_SHORT,
};
use crate::{Error, RATES};

/// Encoder delay: the first access unit decodes to this many samples of
/// silence (MP4 edit lists skip it).
pub const DELAY: usize = 1024;

/// How much less noise a quiet short-window group gets: allowed noise
/// scales with (group energy / loudest group)^QUIET_WEIGHT.
const QUIET_WEIGHT: f32 = 0.6;
/// How far each band's allowed noise leans from its own energy × ratio
/// towards the frame's even noise level (0 = constant SNR per band, 1 = the
/// same noise everywhere). 0.3 keeps the highs coded and beats FFmpeg's
/// SNR in every range at 320k.
const EVEN_WEIGHT: f32 = 0.3;

const ONLY_LONG: u8 = 0;
const LONG_START: u8 = 1;
const EIGHT_SHORT: u8 = 2;
const LONG_STOP: u8 = 3;

struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitWriter {
    fn new() -> Self {
        BitWriter {
            out: Vec::with_capacity(2048),
            acc: 0,
            n: 0,
        }
    }
    fn put(&mut self, bits: u32, v: u32) {
        if bits == 0 {
            return;
        }
        self.acc = (self.acc << bits) | (u64::from(v) & ((1u64 << bits) - 1));
        self.n += bits;
        while self.n >= 8 {
            self.n -= 8;
            self.out.push((self.acc >> self.n) as u8);
        }
    }
    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            let pad = 8 - self.n;
            self.put(pad, 0);
        }
        self.out
    }
}

/// Codebook shape: (dimensions, signed, modulo, offset, largest |value|).
fn shape(cb: usize) -> (usize, bool, i32, i32, i32) {
    match cb {
        1 | 2 => (4, true, 3, 1, 1),
        3 | 4 => (4, false, 3, 0, 2),
        5 | 6 => (2, true, 9, 4, 4),
        7 | 8 => (2, false, 8, 0, 7),
        9 | 10 => (2, false, 13, 0, 12),
        _ => (2, false, 17, 0, 8191),
    }
}

fn escape_bits(v: i32) -> u32 {
    let v = v.unsigned_abs();
    if v < 16 {
        return 0;
    }
    let n = 31 - v.leading_zeros() - 4;
    n + 1 + n + 4
}

/// Bits to code `q` with codebook `cb`; `None` when a value does not fit.
fn band_bits(q: &[i32], cb: usize) -> Option<u32> {
    let (dims, signed, modulo, off, lav) = shape(cb);
    let lens = SPECTRAL[cb - 1].1;
    let mut bits = 0u32;
    for t in q.chunks(dims) {
        let mut idx = 0i32;
        for &v in t {
            if v.abs() > lav {
                return None;
            }
            let a = if cb == 11 { v.abs().min(16) } else { v.abs() };
            idx = idx * modulo + if signed { v + off } else { a };
            if !signed && v != 0 {
                bits += 1;
            }
            if cb == 11 {
                bits += escape_bits(v);
            }
        }
        bits += u32::from(lens[idx as usize]);
    }
    Some(bits)
}

fn write_band(w: &mut BitWriter, q: &[i32], cb: usize) {
    let (dims, signed, modulo, off, _) = shape(cb);
    let (codes, lens) = SPECTRAL[cb - 1];
    for t in q.chunks(dims) {
        let mut idx = 0i32;
        for &v in t {
            let a = if cb == 11 { v.abs().min(16) } else { v.abs() };
            idx = idx * modulo + if signed { v + off } else { a };
        }
        w.put(
            u32::from(lens[idx as usize]),
            u32::from(codes[idx as usize]),
        );
        if !signed {
            for &v in t {
                if v != 0 {
                    w.put(1, u32::from(v < 0));
                }
            }
        }
        if cb == 11 {
            for &v in t {
                let a = v.unsigned_abs();
                if a >= 16 {
                    let n = 31 - a.leading_zeros() - 4;
                    w.put(n, (1 << n) - 1);
                    w.put(1, 0);
                    w.put(n + 4, a - (1 << (n + 4)));
                }
            }
        }
    }
}

/// How a frame is laid out: window sequence, window groups, bands.
#[derive(Clone)]
struct Layout {
    seq: u8,
    /// (first window, windows) per group; one group for long windows.
    groups: Vec<(usize, usize)>,
    offsets: &'static [u16],
    bands: usize,
}

impl Layout {
    fn short(&self) -> bool {
        self.seq == EIGHT_SHORT
    }
    /// Coefficient ranges of band `b` in group `g`, in coding order.
    fn ranges(&self, g: usize, b: usize) -> impl Iterator<Item = std::ops::Range<usize>> + '_ {
        let (w0, n) = self.groups[g];
        let (lo, hi) = (
            usize::from(self.offsets[b]),
            usize::from(self.offsets[b + 1]),
        );
        (w0..w0 + n).map(move |w| w * 128 + lo..w * 128 + hi)
    }
    fn info_bits(&self) -> u32 {
        if self.short() { 15 } else { 11 }
    }
}

/// One channel's coded frame: per (group, band) scalefactor and codebook,
/// quantized values window-major.
#[derive(Clone, Default)]
struct Coded {
    sf: Vec<i32>,
    cb: Vec<u8>,
    q: Vec<i32>,
    bits: u32,
}

pub struct Encoder {
    pub sample_rate: u32,
    pub channels: usize,
    pub bit_rate: u32,
    sr: usize,
    long: Imdct,
    short: Imdct,
    long_rise: Vec<f32>,
    short_rise: Vec<f32>,
    /// Pending input per channel in 16-bit units; `queue[c][0]` is the
    /// first sample of the next block to code (`block * 1024 - 1024`).
    queue: Vec<Vec<f32>>,
    /// Samples pushed, and whether the end has been seen.
    pushed: u64,
    ended: bool,
    /// Next block to code, and the access units still owed at the end.
    block: u64,
    prev_seq: u8,
    /// Short-block decision of the next block (decided one block ahead).
    next_short: Option<bool>,
    ath_long: Vec<f32>,
    ath_short: Vec<f32>,
    snr: f32,
    reservoir: i64,
}

fn ath_table(sample_rate: u32, offsets: &[u16], bands: usize, n: usize) -> Vec<f32> {
    let hz = |bin: u16| f64::from(bin) * f64::from(sample_rate) / n as f64;
    (0..bands)
        .map(|b| {
            // Terhardt's threshold (dB SPL), its lowest point across the
            // band; 0 dB SPL ≈ a quarter LSB of 16-bit noise, which a 2·Σ
            // MDCT of `n` points carries as 2n/16 per coefficient. Capped at
            // 30 dB (≈ −72 dBFS) so quiet lows and highs are kept for a
            // master's delivery; the rate loop drops them when bits are short.
            let (lo, hi) = (hz(offsets[b]), hz(offsets[b + 1]));
            let db = (0..=8)
                .map(|i| {
                    let f = ((lo + (hi - lo) * f64::from(i) / 8.0) / 1000.0).max(0.02);
                    3.64 * f.powf(-0.8) - 6.5 * (-0.6 * (f - 3.3).powi(2)).exp() + 1e-3 * f.powi(4)
                })
                .fold(f64::INFINITY, f64::min);
            (n as f64 / 8.0 * 10f64.powf(db.min(30.0) / 10.0)) as f32
        })
        .collect()
}

impl Encoder {
    pub fn new(sample_rate: u32, channels: usize, bit_rate: u32) -> Result<Encoder, Error> {
        let sr = RATES
            .iter()
            .position(|&r| r == sample_rate)
            .ok_or(Error::Unsupported("sample rate"))?;
        if !(1..=2).contains(&channels) {
            return Err(Error::Unsupported("channel count (1 or 2)"));
        }
        let per_frame = u64::from(bit_rate) * 1024 / u64::from(sample_rate);
        if per_frame > 6000 * channels as u64 || bit_rate < 16_000 {
            return Err(Error::Unsupported("bit rate"));
        }
        Ok(Encoder {
            sample_rate,
            channels,
            bit_rate,
            sr,
            long: Imdct::new(2048),
            short: Imdct::new(256),
            long_rise: sine(2048),
            short_rise: sine(256),
            queue: vec![vec![0.0; 1024]; channels],
            pushed: 0,
            ended: false,
            block: 0,
            prev_seq: ONLY_LONG,
            next_short: None,
            ath_long: ath_table(
                sample_rate,
                SWB_LONG[sr],
                usize::from(NUM_SWB_LONG[sr]),
                2048,
            ),
            ath_short: ath_table(
                sample_rate,
                SWB_SHORT[sr],
                usize::from(NUM_SWB_SHORT[sr]),
                256,
            ),
            snr: 20.0,
            reservoir: 0,
        })
    }

    /// `AudioSpecificConfig`: AAC-LC, this rate and channel count.
    pub fn asc(&self) -> [u8; 2] {
        let v = (2u16 << 11) | ((self.sr as u16) << 7) | ((self.channels as u16) << 3);
        v.to_be_bytes()
    }

    /// Access units in the whole stream for `frames` input frames.
    pub fn units_for(frames: u64) -> u64 {
        (frames + DELAY as u64).div_ceil(1024)
    }

    /// Feed interleaved samples (any length); returns the access units now
    /// complete, in order.
    pub fn push(&mut self, pcm: &[f32]) -> Vec<Vec<u8>> {
        let ch = self.channels;
        for f in pcm.chunks_exact(ch) {
            for (c, v) in f.iter().enumerate() {
                self.queue[c].push(v * 32768.0);
            }
        }
        self.pushed += (pcm.len() / ch) as u64;
        self.drain()
    }

    /// End of input: the remaining access units.
    pub fn finish(&mut self) -> Vec<Vec<u8>> {
        self.ended = true;
        self.drain()
    }

    fn drain(&mut self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let total = Self::units_for(self.pushed);
        loop {
            if self.ended && self.block >= total {
                break;
            }
            // Block t spans samples (t-1)·1024 .. (t+1)·1024; deciding block
            // t+1 looks up to (t+1)·1024 + 576, so wait for the queue to hold
            // through (t+2)·1024. At the end zeros follow.
            if self.queue[0].len() < 3072 {
                if !self.ended {
                    break;
                }
                for q in &mut self.queue {
                    q.resize(3072, 0.0);
                }
            }
            out.push(self.code_block());
            for q in &mut self.queue {
                q.drain(..1024);
            }
            self.block += 1;
        }
        out
    }

    /// Attack in the part of block `start` (queue index) that only its
    /// short windows cover: positions 576..1600 (before 576 the previous
    /// LONG_START window still reaches). Eight 128-sample blocks of
    /// high-passed energy, each against the average of the three before it.
    fn attack(&self, start: usize) -> bool {
        let mut e = [0f64; 11];
        for (i, slot) in e.iter_mut().enumerate() {
            let s = start + 576 - 384 + i * 128;
            let mut acc = 0f64;
            for q in &self.queue {
                for k in s.max(1)..s + 128 {
                    let d = f64::from(q[k] - q[k - 1]);
                    acc += d * d;
                }
            }
            *slot = acc;
        }
        (3..11).any(|i| {
            let before = (e[i - 1] + e[i - 2] + e[i - 3]) / 3.0;
            e[i] > 10.0 * before && e[i] > 1e6 * 128.0
        })
    }

    fn code_block(&mut self) -> Vec<u8> {
        let short_now = match self.next_short {
            Some(s) => s,
            None => self.attack(0),
        };
        let short_next = self.attack(1024);
        self.next_short = Some(short_next);
        let seq = match (
            short_now,
            self.prev_seq == EIGHT_SHORT || self.prev_seq == LONG_START,
            short_next,
        ) {
            (true, _, _) => EIGHT_SHORT,
            // A long block between two short ones cannot both stop and
            // start: keep it short.
            (false, true, true) => EIGHT_SHORT,
            (false, true, false) => LONG_STOP,
            (false, false, true) => LONG_START,
            (false, false, false) => ONLY_LONG,
        };
        // The previous block was a start only if this one is short.
        let seq = if self.prev_seq == LONG_START {
            EIGHT_SHORT
        } else {
            seq
        };
        let ch = self.channels;
        let mut spec = vec![vec![0f32; 1024]; ch];
        let (lr, sr) = (&self.long_rise, &self.short_rise);
        for c in 0..ch {
            let z = &self.queue[c][..2048];
            if seq == EIGHT_SHORT {
                let mut buf = vec![0f32; 256];
                let mut out = vec![0f32; 128];
                for w in 0..8 {
                    let at = 448 + w * 128;
                    for i in 0..128 {
                        buf[i] = 2.0 * z[at + i] * sr[i];
                        buf[128 + i] = 2.0 * z[at + 128 + i] * sr[127 - i];
                    }
                    self.short.forward(&buf, &mut out);
                    spec[c][w * 128..w * 128 + 128].copy_from_slice(&out);
                }
            } else {
                let mut buf = vec![0f32; 2048];
                for i in 0..1024 {
                    let left = if seq == LONG_STOP {
                        if i < 448 {
                            0.0
                        } else if i < 576 {
                            sr[i - 448]
                        } else {
                            1.0
                        }
                    } else {
                        lr[i]
                    };
                    let right = if seq == LONG_START {
                        if i < 448 {
                            1.0
                        } else if i < 576 {
                            sr[127 - (i - 448)]
                        } else {
                            0.0
                        }
                    } else {
                        lr[1023 - i]
                    };
                    buf[i] = 2.0 * z[i] * left;
                    buf[1024 + i] = 2.0 * z[1024 + i] * right;
                }
                self.long.forward(&buf, &mut spec[c]);
            }
        }
        self.prev_seq = seq;
        let layout = self.layout(seq, &spec);
        self.code_frame(&layout, spec)
    }

    fn layout(&self, seq: u8, spec: &[Vec<f32>]) -> Layout {
        if seq != EIGHT_SHORT {
            return Layout {
                seq,
                groups: vec![(0, 1)],
                offsets: SWB_LONG[self.sr],
                bands: usize::from(NUM_SWB_LONG[self.sr]),
            };
        }
        // Group windows of similar energy (a new group where it jumps 4×).
        let energy: Vec<f64> = (0..8)
            .map(|w| {
                spec.iter()
                    .map(|s| {
                        s[w * 128..w * 128 + 128]
                            .iter()
                            .map(|v| f64::from(*v).powi(2))
                            .sum::<f64>()
                    })
                    .sum()
            })
            .collect();
        let mut groups = vec![(0usize, 1usize)];
        for w in 1..8 {
            let first = energy[groups.last().unwrap().0];
            let ratio = (energy[w] + 1.0) / (first + 1.0);
            if (0.25..=4.0).contains(&ratio) {
                groups.last_mut().unwrap().1 += 1;
            } else {
                groups.push((w, 1));
            }
        }
        Layout {
            seq,
            groups,
            offsets: SWB_SHORT[self.sr],
            bands: usize::from(NUM_SWB_SHORT[self.sr]),
        }
    }

    fn code_frame(&mut self, layout: &Layout, mut spec: Vec<Vec<f32>>) -> Vec<u8> {
        let ch = self.channels;
        let n = layout.groups.len() * layout.bands;
        // M/S per (group, band) where the channels are alike.
        let mut ms = vec![false; n];
        if ch == 2 {
            for g in 0..layout.groups.len() {
                for b in 0..layout.bands {
                    let (mut el, mut er, mut es) = (0f64, 0f64, 0f64);
                    for r in layout.ranges(g, b) {
                        for k in r {
                            let (l, rr) = (f64::from(spec[0][k]), f64::from(spec[1][k]));
                            el += l * l;
                            er += rr * rr;
                            es += (l - rr).powi(2) / 4.0;
                        }
                    }
                    let i = g * layout.bands + b;
                    ms[i] = es < 0.15 * el.min(er) && el > 0.0;
                    if ms[i] {
                        for r in layout.ranges(g, b) {
                            for k in r {
                                let (a, c) = (spec[0][k], spec[1][k]);
                                spec[0][k] = (a + c) / 2.0;
                                spec[1][k] = (a - c) / 2.0;
                            }
                        }
                    }
                }
            }
        }
        let ms_any = ms.iter().any(|&m| m);
        let target = i64::from(self.bit_rate) * 1024 / i64::from(self.sample_rate);
        let max_frame = 6144 * ch as i64 - 64;
        let budget = (target + self.reservoir.min(target)).min(max_frame);
        let header = 3
            + 4
            + 3
            + i64::from(layout.info_bits())
            + if ch == 2 {
                1 + 2 + if ms_any { n as i64 } else { 0 }
            } else {
                0
            };
        let pow34: Vec<Vec<f32>> = spec
            .iter()
            .map(|s| s.iter().map(|v| v.abs().powf(0.75)).collect())
            .collect();
        let (mut lo, mut hi) = ((self.snr - 8.0).max(-20.0), (self.snr + 8.0).min(80.0));
        let mut best: Option<(f32, Vec<Coded>)> = None;
        let fits = |coded: &[Coded]| {
            header + coded.iter().map(|c| i64::from(c.bits)).sum::<i64>() <= budget
        };
        // Widen the bracket until it holds the answer, then bisect.
        let coded_lo: Vec<Coded> = spec
            .iter()
            .zip(&pow34)
            .map(|(s, p)| self.code_channel(layout, s, p, lo))
            .collect();
        if fits(&coded_lo) {
            best = Some((lo, coded_lo));
            let coded_hi: Vec<Coded> = spec
                .iter()
                .zip(&pow34)
                .map(|(s, p)| self.code_channel(layout, s, p, hi))
                .collect();
            if fits(&coded_hi) {
                best = Some((hi, coded_hi));
                lo = hi;
                hi = 80.0;
            }
        } else {
            hi = lo;
            lo = -20.0;
        }
        for _ in 0..7 {
            if hi - lo < 0.25 {
                break;
            }
            let mid = (lo + hi) / 2.0;
            let coded: Vec<Coded> = spec
                .iter()
                .zip(&pow34)
                .map(|(s, p)| self.code_channel(layout, s, p, mid))
                .collect();
            if fits(&coded) {
                best = Some((mid, coded));
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let (snr, coded) = best.unwrap_or_else(|| {
            (
                -20.0,
                spec.iter()
                    .zip(&pow34)
                    .map(|(s, p)| self.code_channel(layout, s, p, -20.0))
                    .collect(),
            )
        });
        self.snr = snr;
        let mut w = BitWriter::new();
        if ch == 1 {
            w.put(3, 0);
            w.put(4, 0);
            write_ics(&mut w, &coded[0], layout, true);
        } else {
            w.put(3, 1);
            w.put(4, 0);
            w.put(1, 1); // common_window
            write_ics_info(&mut w, layout);
            w.put(2, u32::from(ms_any));
            if ms_any {
                for &m in &ms {
                    w.put(1, u32::from(m));
                }
            }
            write_ics(&mut w, &coded[0], layout, false);
            write_ics(&mut w, &coded[1], layout, false);
        }
        w.put(3, 7); // END
        let au = w.finish();
        let used = au.len() as i64 * 8;
        self.reservoir =
            (self.reservoir + target - used).clamp(0, (6144 * ch as i64 - target).max(0));
        au
    }

    /// Scalefactors, codebooks and quantized values of one channel at a
    /// given SNR, with its bit count.
    fn code_channel(&self, layout: &Layout, x: &[f32], pow34: &[f32], snr: f32) -> Coded {
        let bands = layout.bands;
        let n = layout.groups.len() * bands;
        let ath = if layout.short() {
            &self.ath_short
        } else {
            &self.ath_long
        };
        let ratio = 10f32.powf(-snr / 10.0);
        // Short windows: noise before an attack is masked only briefly, so a
        // quiet window group gets less noise than the loud one beside it.
        let group_energy: Vec<f32> = (0..layout.groups.len())
            .map(|g| {
                let (w0, n) = layout.groups[g];
                x[w0 * 128..(w0 + n) * 128]
                    .iter()
                    .map(|v| v * v)
                    .sum::<f32>()
                    / n as f32
            })
            .collect();
        let loudest = group_energy.iter().fold(0f32, |m, &e| m.max(e)).max(1e-12);
        // Mean energy per coefficient over the frame: the noise level that
        // would spread bits evenly. Each band's allowed noise leans part way
        // from its own ratio towards that level, so wide quiet bands do not
        // cost as much as loud ones.
        let mean = x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32;
        let mut sf = vec![-1i32; n];
        for g in 0..layout.groups.len() {
            let quiet = if layout.short() {
                (group_energy[g] / loudest).powf(QUIET_WEIGHT)
            } else {
                1.0
            };
            for b in 0..bands {
                let ranges: Vec<_> = layout.ranges(g, b).collect();
                let width: usize = ranges.iter().map(|r| r.len()).sum();
                let energy: f32 = ranges
                    .iter()
                    .flat_map(|r| x[r.clone()].iter())
                    .map(|v| v * v)
                    .sum();
                let own = energy * ratio * quiet;
                let even = mean * ratio * quiet * width as f32;
                let allowed = (own.max(1e-12).powf(1.0 - EVEN_WEIGHT)
                    * even.max(1e-12).powf(EVEN_WEIGHT))
                .max(ath[b] * width as f32);
                if energy <= allowed {
                    continue;
                }
                let peak = ranges
                    .iter()
                    .flat_map(|r| x[r.clone()].iter())
                    .fold(0f32, |m, v| m.max(v.abs()));
                let min_sf = (100.0 + (0.75 * peak.max(1e-9).log2() - 8191f32.log2()) / 0.1875)
                    .ceil()
                    .max(0.0) as i32;
                let (mut a, mut z) = (min_sf.min(255), 255);
                if noise(x, pow34, &ranges, a) > allowed {
                    sf[g * bands + b] = a;
                    continue;
                }
                while z - a > 1 {
                    let mid = (a + z) / 2;
                    if noise(x, pow34, &ranges, mid) <= allowed {
                        a = mid;
                    } else {
                        z = mid;
                    }
                }
                sf[g * bands + b] = a;
            }
        }
        // Coded neighbours differ by at most 60: lower (finer) the larger.
        loop {
            let mut changed = false;
            let mut last: Option<usize> = None;
            for i in 0..n {
                if sf[i] < 0 {
                    continue;
                }
                if let Some(p) = last {
                    if sf[i] > sf[p] + 60 {
                        sf[i] = sf[p] + 60;
                        changed = true;
                    } else if sf[p] > sf[i] + 60 {
                        sf[p] = sf[i] + 60;
                        changed = true;
                    }
                }
                last = Some(i);
            }
            if !changed {
                break;
            }
        }
        let mut q = vec![0i32; 1024];
        let quantize = |q: &mut Vec<i32>, g: usize, b: usize, s: i32| -> bool {
            let scale = (-0.1875 * (s - 100) as f32).exp2();
            let mut any = false;
            for r in layout.ranges(g, b) {
                for k in r {
                    let m = ((pow34[k] * scale + 0.4054) as i32).min(8191);
                    q[k] = if x[k] < 0.0 { -m } else { m };
                    any |= m != 0;
                }
            }
            any
        };
        for g in 0..layout.groups.len() {
            for b in 0..bands {
                let i = g * bands + b;
                if sf[i] >= 0 && !quantize(&mut q, g, b, sf[i]) {
                    sf[i] = -1;
                }
            }
        }
        // Zeroed bands can open gaps over 60 between the remaining ones.
        let mut last: Option<i32> = None;
        for g in 0..layout.groups.len() {
            for b in 0..bands {
                let i = g * bands + b;
                if sf[i] < 0 {
                    continue;
                }
                if let Some(p) = last
                    && (sf[i] - p).abs() > 60
                {
                    sf[i] = p + (sf[i] - p).signum() * 60;
                    if !quantize(&mut q, g, b, sf[i]) {
                        // Coarser made it vanish: keep it at the finer edge.
                        sf[i] = p - 60;
                        quantize(&mut q, g, b, sf[i]);
                    }
                }
                last = Some(sf[i]);
            }
        }
        // Codebooks and sections per group.
        let mut cb = vec![0u8; n];
        // global_gain and the pulse/TNS/gain-control flags.
        let mut bits = 8 + 3;
        let mut buf = Vec::with_capacity(1024);
        for g in 0..layout.groups.len() {
            let mut cost = vec![[u32::MAX; 12]; bands];
            for b in 0..bands {
                let i = g * bands + b;
                if sf[i] < 0 {
                    cost[b][0] = 0;
                    continue;
                }
                buf.clear();
                for r in layout.ranges(g, b) {
                    buf.extend_from_slice(&q[r]);
                }
                for c in 1..12 {
                    if let Some(v) = band_bits(&buf, c) {
                        cost[b][c] = v;
                    }
                }
            }
            let (cbs, spectral, section) = sections(&cost, if layout.short() { 3 } else { 5 });
            cb[g * bands..g * bands + bands].copy_from_slice(&cbs);
            bits += spectral + section;
        }
        let mut prev: Option<i32> = None;
        for i in 0..n {
            if cb[i] == 0 {
                continue;
            }
            if let Some(p) = prev {
                bits += u32::from(SF_BITS[(sf[i] - p + 60) as usize]);
            }
            prev = Some(sf[i]);
        }
        Coded { sf, cb, q, bits }
    }
}

/// Energy of the quantization error over `ranges` at scalefactor `sf`.
fn noise(x: &[f32], pow34: &[f32], ranges: &[std::ops::Range<usize>], sf: i32) -> f32 {
    let p43 = crate::pow43();
    let scale = (-0.1875 * (sf - 100) as f32).exp2();
    let step = (0.25 * (sf - 100) as f32).exp2();
    let mut e = 0f32;
    for r in ranges {
        for k in r.clone() {
            let m = ((pow34[k] * scale + 0.4054) as usize).min(8191);
            let d = x[k].abs() - p43[m] * step;
            e += d * d;
        }
    }
    e
}

/// Cheapest codebook per band including section overhead (4-bit codebook,
/// `len_bits`-bit length fields). Returns codebooks, spectral bits,
/// section bits.
fn sections(cost: &[[u32; 12]], len_field: u32) -> (Vec<u8>, u32, u32) {
    let n = cost.len();
    if n == 0 {
        return (Vec::new(), 0, 0);
    }
    let esc = (1u32 << len_field) - 1;
    let len_bits = |run: u32| len_field * (run / esc + 1);
    let mut best = vec![[u64::MAX; 12]; n];
    let mut start = vec![[0usize; 12]; n];
    for b in 0..n {
        let prev_min = if b == 0 {
            0
        } else {
            (0..12).map(|c| best[b - 1][c]).min().unwrap_or(u64::MAX)
        };
        for cb in 0..12 {
            if cost[b][cb] == u32::MAX {
                continue;
            }
            let here = u64::from(cost[b][cb]);
            let mut choice = (prev_min.saturating_add(4 + u64::from(len_field) + here), b);
            if b > 0 && best[b - 1][cb] != u64::MAX {
                let s = start[b - 1][cb];
                let run = (b - s) as u32;
                let ext = best[b - 1][cb] + here + u64::from(len_bits(run + 1) - len_bits(run));
                if ext < choice.0 {
                    choice = (ext, s);
                }
            }
            best[b][cb] = choice.0;
            start[b][cb] = choice.1;
        }
    }
    let mut cbs = vec![0u8; n];
    let mut cb = (0..12).min_by_key(|&c| best[n - 1][c]).unwrap();
    let (mut b, mut spectral, mut section) = (n, 0u32, 0u32);
    while b > 0 {
        let s = start[b - 1][cb];
        for i in s..b {
            cbs[i] = cb as u8;
            spectral += cost[i][cb];
        }
        section += 4 + len_bits((b - s) as u32);
        b = s;
        if b > 0 {
            cb = (0..12).min_by_key(|&c| best[b - 1][c]).unwrap();
        }
    }
    (cbs, spectral, section)
}

fn write_ics_info(w: &mut BitWriter, l: &Layout) {
    w.put(1, 0);
    w.put(2, u32::from(l.seq));
    w.put(1, 0); // sine
    if l.short() {
        w.put(4, l.bands as u32);
        // scale_factor_grouping: bit set = window joins the previous group.
        let mut grouping = 0u32;
        for &(w0, n) in &l.groups {
            for win in w0 + 1..w0 + n {
                grouping |= 1 << (7 - win);
            }
        }
        w.put(7, grouping);
    } else {
        w.put(6, l.bands as u32);
        w.put(1, 0); // predictor_data_present
    }
}

fn write_ics(w: &mut BitWriter, c: &Coded, l: &Layout, info: bool) {
    let n = l.groups.len() * l.bands;
    let global = (0..n).find(|&i| c.cb[i] != 0).map_or(100, |i| c.sf[i]);
    w.put(8, global as u32);
    if info {
        write_ics_info(w, l);
    }
    let (len_field, esc) = if l.short() { (3, 7) } else { (5, 31) };
    for g in 0..l.groups.len() {
        let mut b = 0;
        while b < l.bands {
            let cb = c.cb[g * l.bands + b];
            let mut e = b;
            while e < l.bands && c.cb[g * l.bands + e] == cb {
                e += 1;
            }
            w.put(4, u32::from(cb));
            let mut run = (e - b) as u32;
            while run >= esc {
                w.put(len_field, esc);
                run -= esc;
            }
            w.put(len_field, run);
            b = e;
        }
    }
    let mut prev = global;
    for i in 0..n {
        if c.cb[i] == 0 {
            continue;
        }
        let d = (c.sf[i] - prev + 60) as usize;
        w.put(u32::from(SF_BITS[d]), SF_CODES[d]);
        prev = c.sf[i];
    }
    w.put(1, 0); // pulse
    w.put(1, 0); // tns
    w.put(1, 0); // gain control
    let mut buf = Vec::with_capacity(1024);
    for g in 0..l.groups.len() {
        for b in 0..l.bands {
            let cb = usize::from(c.cb[g * l.bands + b]);
            if cb == 0 {
                continue;
            }
            buf.clear();
            for r in l.ranges(g, b) {
                buf.extend_from_slice(&c.q[r]);
            }
            write_band(w, &buf, cb);
        }
    }
}
