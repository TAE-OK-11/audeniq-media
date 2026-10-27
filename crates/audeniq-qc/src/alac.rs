//! Apple Lossless (ALAC) decoder, written from the format's description.
//!
//! The 24-byte magic cookie gives the frame length, bit depth and the
//! adaptive-Golomb parameters (`pb`, `mb`, `kb`), and the channel count. A
//! packet (MSB first) is a list of elements, each a 3-bit tag and 4-bit
//! instance: a single channel (SCE) or a channel pair (CPE) carries
//!
//! - a 4-bit header: partial frame (then a 32-bit sample count), the
//!   number of low bytes sent apart ("shifted"), and an escape flag for
//!   samples stored verbatim;
//! - when compressed: the stereo mix (`mix_bits`, `mix_res`), then per
//!   channel a prediction mode, a quantization shift, a Rice history
//!   factor and up to 31 16-bit coefficients; the shifted low bytes; then
//!   per channel the residuals in the adaptive Golomb code (a running mean
//!   picks the parameter, small means switch to coding runs of zeros).
//! - The residuals go through an adaptive FIR predictor whose coefficients
//!   move by the sign of each prediction error, and a pair is un-mixed:
//!   `l = u + v − (mix_res·v ≫ mix_bits)`, `r = l − v`.
//!
//! Output samples are interleaved and left-justified to 32 bits. Mono and
//! stereo streams are decoded.
use crate::{Error, Result};
use media_bits::Bits;

/// Decoder of one ALAC track.
pub struct Decoder {
    frame_length: u32,
    bit_depth: u32,
    pb: u32,
    mb: u32,
    kb: u32,
    channels: usize,
    max_run: u32,
    /// Residuals, then predicted samples, per channel (u, v).
    planes: [Vec<i32>; 2],
    /// Low bytes sent apart, interleaved by channel.
    low: Vec<u32>,
}

const ID_SCE: u32 = 0;
const ID_CPE: u32 = 1;
const ID_DSE: u32 = 4;
const ID_FIL: u32 = 6;
const ID_END: u32 = 7;

impl Decoder {
    /// From the `alac` box's 24-byte configuration (big-endian fields:
    /// frame length, version, bit depth, pb, mb, kb, channels, max run,
    /// max frame bytes, average bit rate, sample rate).
    pub fn new(config: [u8; 24]) -> Self {
        let u32_at =
            |i: usize| u32::from_be_bytes([config[i], config[i + 1], config[i + 2], config[i + 3]]);
        Self {
            frame_length: u32_at(0),
            bit_depth: u32::from(config[5]),
            pb: u32::from(config[6]),
            mb: u32::from(config[7]),
            kb: u32::from(config[8]),
            channels: usize::from(config[9]),
            max_run: u32::from(u16::from_be_bytes([config[10], config[11]])),
            planes: [Vec::new(), Vec::new()],
            low: Vec::new(),
        }
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Decode a packet of `frames` sample frames into `out` (replacing what
    /// it held), interleaved and left-justified to 32 bits.
    pub fn decode(&mut self, packet: &[u8], frames: u32, out: &mut Vec<i32>) -> Result<()> {
        let n = self.decode_packet(packet, out)?;
        if n != frames {
            out.clear();
            return Err(Error::Invalid("ALAC packet length differs from the index"));
        }
        Ok(())
    }

    /// [`Decoder::decode`] when the packet's length is not known up front
    /// (it is the cookie's frame length unless the packet says otherwise).
    pub fn decode_any(&mut self, packet: &[u8], out: &mut Vec<i32>) -> Result<()> {
        self.decode_packet(packet, out).map(|_| ())
    }

    /// Decode every element; the sample frames decoded.
    fn decode_packet(&mut self, packet: &[u8], out: &mut Vec<i32>) -> Result<u32> {
        #[cfg(target_arch = "x86_64")]
        if crate::kernels::bit_ops() {
            // SAFETY: the CPU has LZCNT, BMI1 and BMI2.
            return unsafe { self.decode_packet_bit_ops(packet, out) };
        }
        self.decode_packet_plain(packet, out)
    }

    /// [`Self::decode_packet_plain`] built with LZCNT/BMI for the bit
    /// reader and the Golomb prefixes.
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "lzcnt,bmi1,bmi2")]
    unsafe fn decode_packet_bit_ops(&mut self, packet: &[u8], out: &mut Vec<i32>) -> Result<u32> {
        self.decode_packet_plain(packet, out)
    }

    #[inline(always)]
    fn decode_packet_plain(&mut self, packet: &[u8], out: &mut Vec<i32>) -> Result<u32> {
        out.clear();
        if !(1..=2).contains(&self.channels) {
            return Err(Error::Unsupported("ALAC with more than two channels"));
        }
        if !matches!(self.bit_depth, 16 | 20 | 24 | 32) || self.frame_length == 0 {
            return Err(Error::Invalid("ALAC configuration"));
        }
        if self.frame_length > 1 << 20 {
            return Err(Error::Invalid("ALAC frame length"));
        }
        let mut b = Bits::new(packet);
        let mut frames = None;
        let mut channel = 0usize;
        loop {
            let tag = b.get(3)?;
            match tag {
                ID_END => break,
                ID_SCE | ID_CPE => {
                    let pair = tag == ID_CPE;
                    let width = if pair { 2 } else { 1 };
                    if channel + width > self.channels {
                        return Err(Error::Invalid("ALAC element beyond the channel count"));
                    }
                    b.skip(4)?; // instance tag
                    let n = self.element(&mut b, pair)?;
                    if frames.is_some_and(|f| f != n) {
                        return Err(Error::Invalid("ALAC elements of different lengths"));
                    }
                    frames = Some(n);
                    out.resize(n as usize * self.channels, 0);
                    let justify = 32 - self.bit_depth;
                    let n = n as usize;
                    if self.channels == 1 {
                        for (o, &v) in out.iter_mut().zip(&self.planes[0][..n]) {
                            *o = v.wrapping_shl(justify);
                        }
                    } else if pair {
                        let [l, r] = &self.planes;
                        for ((o, &l), &r) in out
                            .as_chunks_mut::<2>()
                            .0
                            .iter_mut()
                            .zip(&l[..n])
                            .zip(&r[..n])
                        {
                            o[0] = l.wrapping_shl(justify);
                            o[1] = r.wrapping_shl(justify);
                        }
                    } else {
                        for (o, &v) in out
                            .as_chunks_mut::<2>()
                            .0
                            .iter_mut()
                            .zip(&self.planes[0][..n])
                        {
                            o[channel] = v.wrapping_shl(justify);
                        }
                    }
                    channel += width;
                }
                ID_DSE => {
                    b.skip(4)?;
                    let aligned = b.flag()?;
                    let mut count = b.get(8)? as usize;
                    if count == 255 {
                        count += b.get(8)? as usize;
                    }
                    if aligned {
                        b.align()?;
                    }
                    b.skip(count * 8)?;
                }
                ID_FIL => {
                    let mut count = b.get(4)? as usize;
                    if count == 15 {
                        count += b.get(8)? as usize - 1;
                    }
                    b.skip(count * 8)?;
                }
                _ => return Err(Error::Unsupported("ALAC element type")),
            }
        }
        match frames {
            Some(n) if channel == self.channels => Ok(n),
            _ => {
                out.clear();
                Err(Error::Invalid("ALAC packet without every channel"))
            }
        }
    }

    /// One SCE or CPE into `planes` (final samples, not yet justified);
    /// its sample count.
    #[inline(always)]
    fn element(&mut self, b: &mut Bits, pair: bool) -> Result<u32> {
        if b.get(12)? != 0 {
            return Err(Error::Invalid("ALAC element header"));
        }
        let head = b.get(4)?;
        let (partial, shifted, escape) = (head >> 3 & 1 == 1, head >> 1 & 3, head & 1 == 1);
        let n = if partial {
            b.get(32)?
        } else {
            self.frame_length
        };
        if n == 0 || n > self.frame_length {
            return Err(Error::Invalid("ALAC sample count"));
        }
        let width = if pair { 2 } else { 1 };
        // Every sample is written below (or the element fails), so the
        // planes only need the length.
        for p in &mut self.planes[..width] {
            p.resize(n as usize, 0);
        }
        let shift = shifted * 8;
        if shift >= self.bit_depth {
            return Err(Error::Invalid("ALAC shifted bytes"));
        }
        let (mut mix_bits, mut mix_res) = (0u32, 0i32);
        let mut shift = shift;
        if escape {
            // Verbatim samples, interleaved by channel, full width.
            shift = 0;
            let bits = self.bit_depth;
            for i in 0..n as usize {
                for c in 0..width {
                    let raw = b.get(bits)?;
                    self.planes[c][i] = sign_extend(raw, bits);
                }
            }
        } else {
            let chan_bits = self.bit_depth - shift + u32::from(pair);
            mix_bits = b.get(8)?;
            mix_res = b.get(8)? as u8 as i8 as i32;
            let mut params = [Predictor::default(), Predictor::default()];
            for p in &mut params[..width] {
                let h = b.get(8)?;
                p.mode = h >> 4;
                p.den_shift = h & 15;
                let h = b.get(8)?;
                p.pb_factor = h >> 5;
                p.order = (h & 31) as usize;
                for c in &mut p.coefs[..p.order] {
                    *c = b.get(16)? as u16 as i16;
                }
            }
            // The low bytes come first in the stream; read them after.
            let lows = b.clone();
            if shift != 0 {
                b.skip(shift as usize * width * n as usize)?;
            }
            for (c, p) in params[..width].iter_mut().enumerate() {
                let pb = self.pb * p.pb_factor / 4;
                let g = Golomb {
                    pb,
                    mb0: self.mb,
                    kb: self.kb,
                    wb: (1u32 << self.kb.min(31)) - 1,
                    max_run: self.max_run,
                };
                let plane = &mut self.planes[c];
                g.decode(b, plane, chan_bits)?;
                if p.mode != 0 {
                    // Mode 15 ("order 31"): one pass of first-order
                    // integration before the predictor.
                    integrate(plane, chan_bits);
                }
                predict(plane, &mut p.coefs[..p.order], chan_bits, p.den_shift);
            }
            if shift != 0 {
                let mut lb = lows;
                lb.skip(0)?;
                self.low.clear();
                for _ in 0..n as usize * width {
                    self.low.push(lb.get(shift)?);
                }
            }
        }
        if pair && mix_res != 0 {
            let [u, v] = &mut self.planes;
            for (u, v) in u.iter_mut().zip(v.iter_mut()) {
                let l = u
                    .wrapping_add(*v)
                    .wrapping_sub(mix_res.wrapping_mul(*v) >> mix_bits.min(31));
                *u = l;
                *v = l.wrapping_sub(*v);
            }
        }
        if shift != 0 {
            for (c, plane) in self.planes[..width].iter_mut().enumerate() {
                for (i, s) in plane.iter_mut().enumerate() {
                    *s = s.wrapping_shl(shift) | self.low[i * width + c] as i32;
                }
            }
        }
        Ok(n)
    }
}

#[inline]
fn sign_extend(v: u32, bits: u32) -> i32 {
    (v.wrapping_shl(32 - bits) as i32) >> (32 - bits)
}

#[derive(Default)]
struct Predictor {
    mode: u32,
    den_shift: u32,
    pb_factor: u32,
    order: usize,
    coefs: [i16; 32],
}

/// The adaptive Golomb code of the residuals.
struct Golomb {
    pb: u32,
    mb0: u32,
    kb: u32,
    wb: u32,
    max_run: u32,
}

/// Leading ones of the next (up to) 32 bits, and those bits left-aligned.
#[inline(always)]
fn window(b: &mut Bits) -> Result<u32> {
    // A full cache keeps the peek on its fast path.
    b.refill();
    let n = b.left().min(32) as u32;
    if n == 0 {
        return Err(Error::Invalid("ALAC residuals end early"));
    }
    Ok(b.peek(n)? << (32 - n))
}

impl Golomb {
    /// `out.len()` residuals; `max_bits` is the escape width.
    #[inline(always)]
    fn decode(&self, b: &mut Bits, out: &mut [i32], max_bits: u32) -> Result<()> {
        const QB_SHIFT: u32 = 9;
        let n = out.len();
        let mut mb = self.mb0;
        let mut zero_mode = 0u32;
        let mut c = 0usize;
        let _ = self.max_run;
        while c < n {
            let m_mean = mb >> QB_SHIFT;
            let k = (31 - (m_mean + 3).leading_zeros()).min(self.kb);
            let value = Self::value(b, k, max_bits)?;
            let coded = value.wrapping_add(zero_mode);
            // The low bit is the sign.
            out[c] = if coded & 1 == 1 {
                -(((coded >> 1) + 1) as i32)
            } else {
                (coded >> 1) as i32
            };
            c += 1;
            mb = self
                .pb
                .wrapping_mul(coded)
                .wrapping_add(mb)
                .wrapping_sub(self.pb.wrapping_mul(mb) >> QB_SHIFT);
            if value > 0xffff {
                mb = 0xffff;
            }
            zero_mode = 0;
            if (mb << 2) < (1 << QB_SHIFT) && c < n {
                // A low mean: a run of zeros follows.
                zero_mode = 1;
                let k = (mb.leading_zeros() + ((mb + 16) >> 6)).wrapping_sub(24);
                let run = Self::run(b, k, ((1u32 << k.min(31)) - 1) & self.wb)? as usize;
                if c + run > n {
                    return Err(Error::Invalid("ALAC zero run past the frame"));
                }
                out[c..c + run].fill(0);
                c += run;
                if run >= 65535 {
                    zero_mode = 0;
                }
                mb = 0;
            }
        }
        Ok(())
    }

    /// One value: a unary prefix of ones (9 or more: escape to `max_bits`
    /// raw bits), then, for k > 1, k − 1 bits and one more when they read
    /// as 2 or more (a truncated binary code of m = 2ᵏ − 1).
    #[inline(always)]
    fn value(b: &mut Bits, k: u32, max_bits: u32) -> Result<u32> {
        let w = window(b)?;
        let prefix = (!w).leading_zeros();
        if prefix >= 9 {
            b.skip(9)?;
            return b.get(max_bits).map_err(Into::into);
        }
        if k <= 1 {
            b.skip(prefix as usize + 1)?;
            return Ok(prefix);
        }
        let v = (w << (prefix + 1)) >> (32 - k);
        let m = (1u32 << k) - 1;
        if v >= 2 {
            b.skip((prefix + 1 + k) as usize)?;
            Ok(prefix * m + v - 1)
        } else {
            b.skip((prefix + k) as usize)?;
            Ok(prefix * m)
        }
    }

    /// A zero-run length: the same code with divisor `m`, escaping to 16
    /// raw bits.
    #[inline(always)]
    fn run(b: &mut Bits, k: u32, m: u32) -> Result<u32> {
        let w = window(b)?;
        let prefix = (!w).leading_zeros();
        if prefix >= 9 {
            b.skip(9)?;
            return b.get(16).map_err(Into::into);
        }
        let v = if k == 0 {
            0
        } else {
            (w << (prefix + 1)) >> (32 - k)
        };
        if v >= 2 {
            b.skip((prefix + 1 + k) as usize)?;
            Ok(prefix * m + v - 1)
        } else {
            b.skip((prefix + k) as usize)?;
            Ok(prefix * m)
        }
    }
}

/// `x[i] += x[i − 1]`, wrapped to `bits` (the residuals of a first-order
/// difference).
fn integrate(x: &mut [i32], bits: u32) {
    let s = 32 - bits;
    for i in 1..x.len() {
        x[i] = x[i].wrapping_add(x[i - 1]).wrapping_shl(s) >> s;
    }
}

#[inline]
fn sign(x: i32) -> i32 {
    x.signum()
}

/// Undo the adaptive predictor in place: the first `order` samples are a
/// running sum of the residuals; each later sample is its residual plus
/// the prediction from the previous `order` samples relative to the
/// oldest one, rounded by `den_shift`. After each sample, the coefficients
/// step by ±1 toward reducing the residual, from the oldest tap on, until
/// the residual's share is used up.
fn predict(x: &mut [i32], coefs: &mut [i16], bits: u32, den_shift: u32) {
    crate::profile::alac_order(coefs.len(), x.len());
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("avx2") {
        // SAFETY: the CPU has AVX2.
        return unsafe { predict_avx2(x, coefs, bits, den_shift) };
    }
    predict_plain(x, coefs, bits, den_shift);
}

/// [`predict_plain`] with AVX2, whose 32-bit multiplies keep the dot
/// product in one or two vectors.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn predict_avx2(x: &mut [i32], coefs: &mut [i16], bits: u32, den_shift: u32) {
    predict_plain(x, coefs, bits, den_shift);
}

#[inline(always)]
fn predict_plain(x: &mut [i32], coefs: &mut [i16], bits: u32, den_shift: u32) {
    // Encoders pick orders up to 8 (FFmpeg) or 16; each gets its own
    // fixed-size loop.
    match coefs.len() {
        1 => predict_fixed::<1>(x, coefs.try_into().unwrap(), bits, den_shift),
        2 => predict_fixed::<2>(x, coefs.try_into().unwrap(), bits, den_shift),
        3 => predict_fixed::<3>(x, coefs.try_into().unwrap(), bits, den_shift),
        4 => predict_fixed::<4>(x, coefs.try_into().unwrap(), bits, den_shift),
        5 => predict_fixed::<5>(x, coefs.try_into().unwrap(), bits, den_shift),
        6 => predict_fixed::<6>(x, coefs.try_into().unwrap(), bits, den_shift),
        7 => predict_fixed::<7>(x, coefs.try_into().unwrap(), bits, den_shift),
        8 => predict_fixed::<8>(x, coefs.try_into().unwrap(), bits, den_shift),
        9 => predict_fixed::<9>(x, coefs.try_into().unwrap(), bits, den_shift),
        10 => predict_fixed::<10>(x, coefs.try_into().unwrap(), bits, den_shift),
        11 => predict_fixed::<11>(x, coefs.try_into().unwrap(), bits, den_shift),
        12 => predict_fixed::<12>(x, coefs.try_into().unwrap(), bits, den_shift),
        13 => predict_fixed::<13>(x, coefs.try_into().unwrap(), bits, den_shift),
        14 => predict_fixed::<14>(x, coefs.try_into().unwrap(), bits, den_shift),
        15 => predict_fixed::<15>(x, coefs.try_into().unwrap(), bits, den_shift),
        16 => predict_fixed::<16>(x, coefs.try_into().unwrap(), bits, den_shift),
        _ => predict_any(x, coefs, bits, den_shift),
    }
}

/// [`predict_any`] for a fixed order. The coefficients live in a local
/// array in history order (`c[t]` weighs `x[j − N + t]`), widened to i32
/// and re-narrowed after each step, so the dot product and the update walk
/// the same direction with no bounds checks.
#[inline(always)]
fn predict_fixed<const N: usize>(x: &mut [i32], coefs: &mut [i16; N], bits: u32, den_shift: u32) {
    let n = x.len();
    let s = 32 - bits;
    if n < N + 2 {
        return predict_any(x, coefs, bits, den_shift);
    }
    for j in 1..=N {
        x[j] = x[j].wrapping_add(x[j - 1]).wrapping_shl(s) >> s;
    }
    let half = if den_shift == 0 {
        0
    } else {
        1i32 << (den_shift - 1)
    };
    let ds = den_shift.min(31);
    let mut c: [i32; N] = core::array::from_fn(|t| i32::from(coefs[N - 1 - t]));
    for j in N + 1..n {
        let top = x[j - N - 1];
        let h: [i32; N] = x[j - N..j].try_into().unwrap();
        let e: [i32; N] = core::array::from_fn(|t| h[t].wrapping_sub(top));
        let mut sum = 0i32;
        for t in 0..N {
            sum = sum.wrapping_add(c[t].wrapping_mul(e[t]));
        }
        let residual = x[j];
        let predicted = residual
            .wrapping_add(top)
            .wrapping_add(sum.wrapping_add(half) >> ds);
        x[j] = predicted.wrapping_shl(s) >> s;
        // Step the coefficients toward the error, from the oldest tap,
        // until the residual's share is used up.
        let mut left = residual;
        if residual > 0 {
            for t in 0..N {
                let sg = sign(e[t]);
                c[t] = i32::from(c[t].wrapping_add(sg) as i16);
                left = left.wrapping_sub((t as i32 + 1).wrapping_mul(e[t].wrapping_abs() >> ds));
                if left <= 0 {
                    break;
                }
            }
        } else if residual < 0 {
            for t in 0..N {
                let sg = sign(e[t]);
                c[t] = i32::from(c[t].wrapping_sub(sg) as i16);
                left = left.wrapping_sub(
                    (t as i32 + 1).wrapping_mul(e[t].wrapping_abs().wrapping_neg() >> ds),
                );
                if left >= 0 {
                    break;
                }
            }
        }
    }
    for t in 0..N {
        coefs[N - 1 - t] = c[t] as i16;
    }
}

fn predict_any(x: &mut [i32], coefs: &mut [i16], bits: u32, den_shift: u32) {
    let n = x.len();
    let order = coefs.len();
    let s = 32 - bits;
    if order == 0 || n < 2 {
        // Nothing to predict (order 0: residuals are the samples).
        return;
    }
    let lim = (order + 1).min(n);
    for j in 1..lim {
        x[j] = x[j].wrapping_add(x[j - 1]).wrapping_shl(s) >> s;
    }
    let half = if den_shift == 0 {
        0
    } else {
        1i32 << (den_shift - 1)
    };
    let ds = den_shift.min(31);
    for j in lim..n {
        let top = x[j - lim];
        // coefs[k] weighs x[j − 1 − k].
        let mut sum = 0i32;
        for (k, &c) in coefs.iter().enumerate() {
            sum = sum.wrapping_add(i32::from(c).wrapping_mul(x[j - 1 - k].wrapping_sub(top)));
        }
        let residual = x[j];
        let predicted = residual
            .wrapping_add(top)
            .wrapping_add(sum.wrapping_add(half) >> ds);
        x[j] = predicted.wrapping_shl(s) >> s;
        let mut left = residual;
        match sign(residual) {
            1 => {
                for k in (0..order).rev() {
                    let d = top.wrapping_sub(x[j - 1 - k]);
                    let sg = sign(d);
                    coefs[k] = coefs[k].wrapping_sub(sg as i16);
                    left = left.wrapping_sub((order - k) as i32 * (sg.wrapping_mul(d) >> ds));
                    if left <= 0 {
                        break;
                    }
                }
            }
            -1 => {
                for k in (0..order).rev() {
                    let d = top.wrapping_sub(x[j - 1 - k]);
                    let sg = sign(d);
                    coefs[k] = coefs[k].wrapping_add(sg as i16);
                    left = left.wrapping_sub((order - k) as i32 * ((-sg).wrapping_mul(d) >> ds));
                    if left >= 0 {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cookie(frame_length: u32, bits: u8, channels: u8) -> [u8; 24] {
        let mut c = [0u8; 24];
        c[..4].copy_from_slice(&frame_length.to_be_bytes());
        c[5] = bits;
        c[6] = 40;
        c[7] = 10;
        c[8] = 14;
        c[9] = channels;
        c[10..12].copy_from_slice(&255u16.to_be_bytes());
        c
    }

    /// A packet of escaped (verbatim) samples, written bit by bit.
    fn escaped(samples: &[i32], channels: usize, bits: u32, partial: bool) -> Vec<u8> {
        let mut w = media_bits::BeWriter::new();
        let tag = if channels == 2 { ID_CPE } else { ID_SCE };
        w.put(3, u64::from(tag));
        w.put(4, 0);
        w.put(12, 0);
        w.put(4, (u64::from(partial) << 3) | 1);
        if partial {
            w.put(32, (samples.len() / channels) as u64);
        }
        for &s in samples {
            w.put(bits, u64::from(s as u32));
        }
        w.put(3, u64::from(ID_END));
        w.align();
        w.bytes
    }

    #[test]
    fn escaped_packets_decode_verbatim() {
        for (bits, channels) in [(16u32, 1usize), (16, 2), (24, 2), (32, 1)] {
            let n = 37;
            let samples: Vec<i32> = (0..n * channels)
                .map(|i| ((i as i64 * 7919 - 50_000) as i32).wrapping_shl(32 - bits) >> (32 - bits))
                .collect();
            let mut d = Decoder::new(cookie(4096, bits as u8, channels as u8));
            let mut out = Vec::new();
            d.decode(&escaped(&samples, channels, bits, true), n as u32, &mut out)
                .unwrap();
            let want: Vec<i32> = samples.iter().map(|&s| s.wrapping_shl(32 - bits)).collect();
            assert_eq!(out, want, "{bits} bits {channels} ch");
            // A wrong expected length and every truncation are errors.
            assert!(d
                .decode(
                    &escaped(&samples, channels, bits, true),
                    n as u32 + 1,
                    &mut out
                )
                .is_err());
            let p = escaped(&samples, channels, bits, true);
            for cut in 0..p.len() - 1 {
                assert!(
                    d.decode(&p[..cut], n as u32, &mut out).is_err(),
                    "cut {cut}"
                );
            }
        }
    }

    #[test]
    fn malformed_packets_and_configs_never_panic() {
        let mut seed = 0x1234_5678u32;
        for bits in [0u8, 16, 20, 24, 32, 255] {
            for channels in [0u8, 1, 2, 3] {
                let mut d = Decoder::new(cookie(4096, bits, channels));
                let mut out = Vec::new();
                for len in [0usize, 1, 3, 17, 300] {
                    let p: Vec<u8> = (0..len)
                        .map(|_| {
                            seed ^= seed << 13;
                            seed ^= seed >> 17;
                            seed ^= seed << 5;
                            seed as u8
                        })
                        .collect();
                    let _ = d.decode_any(&p, &mut out);
                }
            }
        }
    }

    #[test]
    fn predictor_order_zero_and_integration() {
        let mut x = vec![5, 1, 1, 1];
        integrate(&mut x, 16);
        assert_eq!(x, [5, 6, 7, 8]);
        let mut y = vec![3, 4, 5];
        predict(&mut y, &mut [], 16, 0);
        assert_eq!(y, [3, 4, 5]);
    }
}
