//! TTA (True Audio, format 1) lossless decoder, written from the format's
//! description:
//!
//! - A 22-byte header (`TTA1`, format 1 = integer PCM, channels, bits per
//!   sample, sample rate, samples per channel, CRC-32), then a table with
//!   each frame's size and its own CRC-32.
//! - Frames of `rate · 256 / 245` samples per channel (the last one
//!   shorter), each an LSB-first bit stream followed by the CRC-32 of its
//!   bytes. Every frame starts every channel's state afresh.
//! - Per sample, one residual per channel: an adaptive Rice code with two
//!   parameters (one for small values, one for the excess of large ones);
//!   then an eight-tap sign–sign adaptive filter; then a first-order fixed
//!   predictor `x += prev·(2ᵏ − 1) / 2ᵏ`; then, with more than one channel,
//!   the inter-channel differences are undone.
//!
//! Output samples are left-justified to 32 bits, as the other decoders'.
use crate::{
    bits::{crc32, Lsb},
    AudioSpec, Error, Limits, Result,
};
use std::fs::File;
use std::io::Read;

const HEADER: usize = 22;

pub struct Tta {
    file: File,
    pub spec: AudioSpec,
    /// Byte size of every frame (CRC included), in order.
    frame_sizes: Vec<u32>,
    next_frame: usize,
    /// Samples per channel in a full frame, and still to come.
    frame_samples: u64,
    left: u64,
    bytes: Vec<u8>,
    states: Vec<Channel>,
    /// One sample's values across the channels.
    row: Vec<i32>,
}

fn u16_le(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}

fn u32_le(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

impl Tta {
    pub fn open(mut file: File, limits: &Limits) -> Result<Self> {
        let mut h = [0u8; HEADER];
        file.read_exact(&mut h)?;
        if &h[..4] != b"TTA1" {
            return Err(Error::Invalid("not a TTA1 file"));
        }
        if crc32(&h[..18]) != u32_le(&h[18..]) {
            return Err(Error::Invalid("TTA header CRC"));
        }
        let format = u16_le(&h[4..]);
        let channels = u16_le(&h[6..]);
        let bits = u16_le(&h[8..]);
        let rate = u32_le(&h[10..]);
        let samples = u64::from(u32_le(&h[14..]));
        if format != 1 {
            return Err(Error::Unsupported("TTA format other than integer PCM"));
        }
        if channels == 0 || !(8..=24).contains(&bits) || rate == 0 {
            return Err(Error::Invalid("TTA stream parameters"));
        }
        if samples > limits.max_frames {
            return Err(Error::Limit("decoded frames"));
        }
        let frame_samples = u64::from(rate) * 256 / 245;
        if frame_samples == 0 {
            return Err(Error::Invalid("TTA sample rate"));
        }
        let frames = samples.div_ceil(frame_samples);
        // The table must fit the file: 4 bytes per frame.
        let file_len = file.metadata()?.len();
        if frames.saturating_mul(4) > file_len {
            return Err(Error::Invalid("TTA seek table larger than the file"));
        }
        let mut table = vec![0u8; frames as usize * 4 + 4];
        file.read_exact(&mut table)?;
        let (sizes, crc) = table.split_at(frames as usize * 4);
        if crc32(sizes) != u32_le(crc) {
            return Err(Error::Invalid("TTA seek table CRC"));
        }
        let frame_sizes: Vec<u32> = sizes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes(*c))
            .collect();
        if frame_sizes
            .iter()
            .any(|&s| s < 4 || s as usize > limits.max_packet_bytes)
        {
            return Err(Error::Invalid("TTA frame size"));
        }
        Ok(Self {
            file,
            spec: AudioSpec {
                container: "tta".into(),
                codec: "tta".into(),
                sample_rate: rate,
                channels,
                bits_per_sample: bits,
                frames: Some(samples),
            },
            frame_sizes,
            next_frame: 0,
            frame_samples,
            left: samples,
            bytes: Vec::new(),
            states: (0..channels).map(|_| Channel::new(bits)).collect(),
            row: vec![0; usize::from(channels)],
        })
    }

    /// The next frame's samples, interleaved, into `out` (replacing what it
    /// held); `out` stays empty at the end of the stream.
    pub fn next(&mut self, out: &mut Vec<i32>, limits: &Limits) -> Result<()> {
        out.clear();
        if self.left == 0 {
            return Ok(());
        }
        limits.check()?;
        let size = *self
            .frame_sizes
            .get(self.next_frame)
            .ok_or(Error::Invalid("TTA frame missing"))? as usize;
        self.next_frame += 1;
        self.bytes.resize(size, 0);
        self.file.read_exact(&mut self.bytes)?;
        let (data, crc) = self.bytes.split_at(size - 4);
        if crc32(data) != u32_le(crc) {
            return Err(Error::Invalid("TTA frame CRC"));
        }
        let n = self.left.min(self.frame_samples) as usize;
        self.left -= n as u64;
        for s in &mut self.states {
            s.reset();
        }
        out.resize(n * self.row.len(), 0);
        let bits = u32::from(self.spec.bits_per_sample);
        let job = Frame {
            data,
            states: &mut self.states,
            row: &mut self.row,
            out,
            bits,
        };
        #[cfg(target_arch = "x86_64")]
        if crate::kernels::bit_ops() {
            // SAFETY: LZCNT/BMI1/BMI2 were detected at runtime; the body is
            // the same safe code.
            return unsafe { job.run_bit_ops() };
        }
        job.run()
    }
}

/// One frame's decoding, apart from the reader so it can be compiled twice.
struct Frame<'a> {
    data: &'a [u8],
    states: &'a mut [Channel],
    row: &'a mut [i32],
    out: &'a mut [i32],
    bits: u32,
}

impl Frame<'_> {
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "lzcnt,bmi1,bmi2")]
    unsafe fn run_bit_ops(self) -> Result<()> {
        self.run()
    }

    #[inline(always)]
    fn run(self) -> Result<()> {
        match self.states.len() {
            1 => self.samples::<1>(),
            2 => self.samples::<2>(),
            _ => self.samples::<0>(),
        }
    }

    /// The samples, with the channel count `C` fixed (0: any).
    #[inline(always)]
    fn samples<const C: usize>(self) -> Result<()> {
        let ch = if C == 0 { self.states.len() } else { C };
        let (states, row) = (&mut self.states[..ch], &mut self.row[..ch]);
        let justify = 32 - self.bits;
        let (lo, hi) = (-(1i32 << (self.bits - 1)), (1i32 << (self.bits - 1)) - 1);
        let mut reader = Lsb::new(self.data);
        let mut wide = false;
        for frame in self.out.chunks_exact_mut(ch) {
            for (v, s) in row.iter_mut().zip(states.iter_mut()) {
                let residual = s.rice.decode(&mut reader)?;
                let filtered = s.filter.decode(residual);
                *v = s.predict(filtered);
            }
            undo_differences(row);
            for (o, &v) in frame.iter_mut().zip(row.iter()) {
                wide |= v < lo || v > hi;
                *o = v << justify;
            }
        }
        if wide {
            return Err(Error::Invalid("TTA sample out of range"));
        }
        Ok(())
    }
}

/// The encoder sends, for channels x₀..xₙ₋₁, dᵢ = xᵢ₊₁ − xᵢ (i < n − 1) and
/// dₙ₋₁ = xₙ₋₁ − dₙ₋₂/2 (C division, toward zero); this undoes it in place.
fn undo_differences(row: &mut [i32]) {
    let n = row.len();
    if n < 2 {
        return;
    }
    row[n - 1] = row[n - 1].wrapping_add(row[n - 2] / 2);
    for i in (0..n - 1).rev() {
        row[i] = row[i + 1].wrapping_sub(row[i]);
    }
}

/// One channel's decoding state, reset at every frame.
struct Channel {
    rice: Rice,
    filter: Filter,
    /// Previous output of the fixed predictor, and its order shift.
    prev: i32,
    shift: u32,
}

impl Channel {
    fn new(bits: u16) -> Self {
        Self {
            rice: Rice::new(),
            filter: Filter::new(bits),
            prev: 0,
            // 8-bit streams use x·15/16, wider ones x·31/32.
            shift: if bits <= 8 { 4 } else { 5 },
        }
    }

    fn reset(&mut self) {
        self.rice = Rice::new();
        self.filter.reset();
        self.prev = 0;
    }

    #[inline(always)]
    fn predict(&mut self, x: i32) -> i32 {
        let k = self.shift;
        let v = x.wrapping_add(self.prev.wrapping_mul((1 << k) - 1) >> k);
        self.prev = v;
        v
    }
}

/// The two-parameter adaptive Rice code: a run of ones ended by a zero
/// chooses between `k0` (no ones) and `k1` plus a `2^k0` offset (each
/// further one adds `2^k1`); each parameter follows a running sum of what
/// it coded, moving down below `16·2^k` and up above `16·2^(k+1)`.
#[derive(Clone, Copy)]
struct Rice {
    k: [u32; 2],
    sum: [u32; 2],
}

impl Rice {
    fn new() -> Self {
        Self {
            k: [10; 2],
            // Each sum starts where its parameter sits: 16·2^k.
            sum: [16 << 10; 2],
        }
    }

    #[inline(always)]
    fn adapt(&mut self, i: usize, value: u32) {
        let s = &mut self.sum[i];
        *s = s.wrapping_add(value).wrapping_sub(*s >> 4);
        let k = &mut self.k[i];
        if *k > 0 && *s < 16 << *k {
            *k -= 1;
        } else if *k < 27 && *s > 16 << (*k + 1) {
            *k += 1;
        }
    }

    #[inline(always)]
    fn decode(&mut self, r: &mut Lsb<'_>) -> Result<i32> {
        let ones = r.ones(1 << 16)?;
        let (i, run) = if ones == 0 { (0, 0) } else { (1, ones - 1) };
        let k = self.k[i];
        if k > 31 || (run as u64) << k > u64::from(u32::MAX) {
            return Err(Error::Invalid("TTA residual"));
        }
        let mut value = (run << k) | if k > 0 { r.read(k)? } else { 0 };
        if i == 1 {
            self.adapt(1, value);
            value = value.wrapping_add(1 << self.k[0]);
        }
        self.adapt(0, value);
        // Zigzag: odd values are positive.
        Ok(if value & 1 == 1 {
            (value >> 1) as i32 + 1
        } else {
            -((value >> 1) as i32)
        })
    }
}

/// The eight-tap adaptive filter: the prediction is the dot product of the
/// last values' history with the weights, rounded and scaled down by
/// `2^shift`; before each step the weights move by their step sizes in the
/// direction of the last residual's sign. The step sizes come from the
/// signs of the newest history entries (±1, ±2, ±2, ±4), the history keeps
/// the last value and its first and second differences.
struct Filter {
    shift: u32,
    weights: [i32; 8],
    steps: [i32; 8],
    history: [i32; 8],
    last_residual: i32,
}

impl Filter {
    fn new(bits: u16) -> Self {
        Self {
            // 16-bit streams scale by 2⁹, 8- and 24-bit ones by 2¹⁰.
            shift: if bits > 8 && bits <= 16 { 9 } else { 10 },
            weights: [0; 8],
            steps: [0; 8],
            history: [0; 8],
            last_residual: 0,
        }
    }

    fn reset(&mut self) {
        self.weights = [0; 8];
        self.steps = [0; 8];
        self.history = [0; 8];
        self.last_residual = 0;
    }

    #[inline(always)]
    fn decode(&mut self, residual: i32) -> i32 {
        // Branch-free: the weights move by ±steps or not at all.
        let sign = self.last_residual.signum();
        let mut sum = 1i32 << (self.shift - 1);
        for i in 0..8 {
            self.weights[i] = self.weights[i].wrapping_add(self.steps[i].wrapping_mul(sign));
            sum = sum.wrapping_add(self.history[i].wrapping_mul(self.weights[i]));
        }
        let h = &mut self.history;
        let s = &mut self.steps;
        s.copy_within(1..5, 0);
        h.copy_within(1..5, 0);
        s[4] = (h[4] >> 30) | 1;
        s[5] = ((h[5] >> 30) | 2) & !1;
        s[6] = ((h[6] >> 30) | 2) & !1;
        s[7] = ((h[7] >> 30) | 4) & !3;
        self.last_residual = residual;
        let value = residual.wrapping_add(sum >> self.shift);
        h[4] = h[5].wrapping_neg();
        h[5] = h[6].wrapping_neg();
        h[6] = value.wrapping_sub(h[7]);
        h[7] = value;
        h[5] = h[5].wrapping_add(h[6]);
        h[4] = h[4].wrapping_add(h[5]);
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_differences_round_trip() {
        // The encoder's transform, then ours.
        for row in [[5, -3], [-7, 2], [100, 101], [-1, -1]] {
            let mut d = row;
            d[0] = row[1] - row[0];
            d[1] = row[1] - d[0] / 2;
            undo_differences(&mut d);
            assert_eq!(d, row);
        }
        let x = [3, -9, 12, 40];
        let mut d = [x[1] - x[0], x[2] - x[1], x[3] - x[2], 0];
        d[3] = x[3] - d[2] / 2;
        undo_differences(&mut d);
        assert_eq!(d, x);
    }

    #[test]
    fn rice_parameters_adapt_both_ways() {
        let mut r = Rice::new();
        for _ in 0..200 {
            r.adapt(0, 3);
        }
        assert!(r.k[0] < 10);
        for _ in 0..200 {
            r.adapt(0, 1 << 20);
        }
        assert!(r.k[0] > 10);
    }
}
