// SPDX-License-Identifier: LGPL-2.1-or-later
//! AAC-LC decoder (ISO/IEC 14496-3 4.4–4.6) for music-video sound checks:
//! single / pair / LFE elements, all window sequences and shapes, M/S and
//! intensity stereo, perceptual noise substitution, temporal noise shaping
//! and pulse data. Output is f32 PCM, full scale ±1.0, 1024 frames per
//! access unit, channels in element order.
//!
//! Not decoded: SBR/PS (HE-AAC decodes as its AAC-LC core at half rate),
//! main prediction, LTP, SSR and coupling channels; such streams are
//! refused or reported by the caller. Tables come from FFmpeg (LGPL), the
//! code is AUDENIQ's.
mod bits;
pub mod encoder;
mod huffman;
mod mdct;
mod tables;

use bits::Bits;
use huffman::Tree;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Truncated,
    Invalid(&'static str),
    Unsupported(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::Truncated => f.write_str("AAC access unit ends early"),
            Error::Invalid(s) => write!(f, "invalid AAC {s}"),
            Error::Unsupported(s) => write!(f, "unsupported AAC {s}"),
        }
    }
}

pub(crate) const RATES: [u32; 13] = [
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
];

const ONLY_LONG: u8 = 0;
const LONG_START: u8 = 1;
const EIGHT_SHORT: u8 = 2;
const LONG_STOP: u8 = 3;

const ZERO_HCB: u8 = 0;
const NOISE_HCB: u8 = 13;
const INTENSITY_HCB2: u8 = 14;
const INTENSITY_HCB: u8 = 15;

struct Trees {
    sf: Tree,
    spectral: Vec<Tree>,
}

fn trees() -> &'static Trees {
    static T: OnceLock<Trees> = OnceLock::new();
    T.get_or_init(|| Trees {
        sf: Tree::new(&tables::SF_CODES, &tables::SF_BITS),
        spectral: tables::SPECTRAL
            .iter()
            .map(|(c, l)| Tree::new(c, l))
            .collect(),
    })
}

/// `|q|^(4/3)` for every quantized magnitude (escape values reach 8191).
pub(crate) fn pow43() -> &'static [f32] {
    static T: OnceLock<Vec<f32>> = OnceLock::new();
    T.get_or_init(|| {
        (0..8192)
            .map(|i| (i as f64).powf(4.0 / 3.0) as f32)
            .collect()
    })
}

/// What `AudioSpecificConfig` says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub object_type: u32,
    pub sample_rate: u32,
    pub channels: u32,
    /// SBR or PS signalled: only the core is decoded (at `sample_rate`).
    pub extension: bool,
}

/// Parse an `AudioSpecificConfig` (ISO/IEC 14496-3 1.6.2.1).
pub fn parse_config(asc: &[u8]) -> Result<(Config, usize), Error> {
    let mut b = Bits::new(asc);
    let object = |b: &mut Bits| -> Result<u32, Error> {
        let o = b.u(5)?;
        Ok(if o == 31 { 32 + b.u(6)? } else { o })
    };
    let rate = |b: &mut Bits| -> Result<(usize, u32), Error> {
        let i = b.u(4)? as usize;
        if i == 15 {
            let r = b.u(24)?;
            let idx = RATES
                .iter()
                .position(|&x| x <= r * 92 / 100 + r * 8 / 100 && x <= r)
                .unwrap_or(12);
            Ok((idx, r))
        } else {
            Ok((
                i,
                *RATES
                    .get(i)
                    .ok_or(Error::Invalid("sampling frequency index"))?,
            ))
        }
    };
    let mut ot = object(&mut b)?;
    let (mut idx, mut sample_rate) = rate(&mut b)?;
    let channels = b.u(4)?;
    let mut extension = false;
    if ot == 5 || ot == 29 {
        extension = true;
        // The extension rate first, then the core object type; the core
        // rate stays the one above.
        rate(&mut b)?;
        ot = object(&mut b)?;
    }
    if ot != 2 {
        return Err(Error::Unsupported("object type (only AAC-LC)"));
    }
    if idx > 12 {
        return Err(Error::Invalid("sampling frequency index"));
    }
    // GASpecificConfig: frameLengthFlag must select 1024.
    if b.u(1)? != 0 {
        return Err(Error::Unsupported("960-sample frames"));
    }
    let channels = match channels {
        1..=6 => channels,
        7 => 8,
        _ => return Err(Error::Unsupported("channel configuration")),
    };
    if sample_rate == 0 {
        idx = 4;
        sample_rate = RATES[idx];
    }
    Ok((
        Config {
            object_type: ot,
            sample_rate,
            channels,
            extension,
        },
        idx,
    ))
}

#[derive(Clone, Copy, Default)]
struct Info {
    seq: u8,
    shape: u8,
    max_sfb: usize,
    groups: usize,
    group_len: [usize; 8],
}

impl Info {
    fn windows(&self) -> usize {
        if self.seq == EIGHT_SHORT { 8 } else { 1 }
    }
}

#[derive(Default, Clone)]
struct Tns {
    /// Per window: filters as (length, direction, lpc).
    filters: Vec<Vec<(usize, bool, Vec<f32>)>>,
}

/// One channel's decoded spectrum, window-major (`window * 128 + bin` for
/// short windows).
struct Spectrum {
    info: Info,
    coef: Vec<f32>,
    /// Band types and values per (group, band): gain exponent for normal
    /// bands, intensity position, noise energy.
    band: Vec<u8>,
    value: Vec<i32>,
    tns: Option<Tns>,
}

#[derive(Clone)]
struct ChannelState {
    saved: Vec<f32>,
    prev_shape: u8,
}

pub struct Decoder {
    pub config: Config,
    sr: usize,
    state: Vec<ChannelState>,
    long: mdct::Imdct,
    short: mdct::Imdct,
    windows: [[Vec<f32>; 2]; 2],
    random: u32,
    frame: Vec<f32>,
    tmp: Vec<f32>,
}

impl Decoder {
    pub fn new(asc: &[u8]) -> Result<Decoder, Error> {
        let (config, sr) = parse_config(asc)?;
        Ok(Decoder {
            config,
            sr,
            state: Vec::new(),
            long: mdct::Imdct::new(2048),
            short: mdct::Imdct::new(256),
            // [shape][long?]: 0 sine, 1 KBD.
            windows: [
                [mdct::sine(256), mdct::sine(2048)],
                [mdct::kbd(256, 6.0), mdct::kbd(2048, 4.0)],
            ],
            random: 0x1f2e3d4c,
            frame: vec![0.0; 2048],
            tmp: vec![0.0; 256],
        })
    }

    pub fn channels(&self) -> usize {
        self.config.channels as usize
    }

    /// Output channel names in decode (AAC element) order for the
    /// configuration: C, L, R, Ls, Rs, LFE for 5.1.
    pub fn layout(&self) -> &'static [&'static str] {
        match self.config.channels {
            1 => &["C"],
            2 => &["L", "R"],
            3 => &["C", "L", "R"],
            4 => &["C", "L", "R", "Cs"],
            5 => &["C", "L", "R", "Ls", "Rs"],
            6 => &["C", "L", "R", "Ls", "Rs", "LFE"],
            _ => &["C", "L", "R", "Lss", "Rss", "Ls", "Rs", "LFE"],
        }
    }

    /// Decode one access unit, appending interleaved samples to `out`.
    /// Returns the frames added (1024).
    pub fn decode(&mut self, au: &[u8], out: &mut Vec<f32>) -> Result<usize, Error> {
        let mut b = Bits::new(au);
        let mut spectra: Vec<Spectrum> = Vec::new();
        loop {
            let id = b.u(3)?;
            match id {
                0 | 3 => {
                    b.u(4)?;
                    spectra.push(self.ics(&mut b, None, id == 3)?);
                }
                1 => {
                    b.u(4)?;
                    let common = b.u(1)? == 1;
                    let mut info = None;
                    let mut ms = (0u32, Vec::new());
                    if common {
                        let i = self.ics_info(&mut b)?;
                        let mode = b.u(2)?;
                        let mut mask = Vec::new();
                        if mode == 1 {
                            for _ in 0..i.groups * i.max_sfb {
                                mask.push(b.u(1)? == 1);
                            }
                        }
                        if mode == 3 {
                            return Err(Error::Invalid("ms_mask_present"));
                        }
                        ms = (mode, mask);
                        info = Some(i);
                    }
                    let mut l = self.ics(&mut b, info, false)?;
                    let mut r = self.ics(&mut b, info, false)?;
                    if common {
                        stereo(
                            &mut l,
                            &mut r,
                            &ms,
                            tables::SWB_SHORT[self.sr],
                            tables::SWB_LONG[self.sr],
                        );
                    }
                    spectra.push(l);
                    spectra.push(r);
                }
                2 => return Err(Error::Unsupported("coupling channel element")),
                4 => {
                    b.u(4)?;
                    let align = b.u(1)? == 1;
                    let mut n = b.u(8)? as usize;
                    if n == 255 {
                        n += b.u(8)? as usize;
                    }
                    if align {
                        b.align();
                    }
                    b.skip(n * 8)?;
                }
                5 => skip_pce(&mut b)?,
                6 => {
                    let mut n = b.u(4)? as usize;
                    if n == 15 {
                        n += b.u(8)? as usize - 1;
                    }
                    b.skip(n * 8)?;
                }
                _ => break,
            }
        }
        if spectra.is_empty() {
            return Err(Error::Invalid("access unit without audio"));
        }
        let channels = spectra.len();
        if self.state.len() < channels {
            self.state.resize(
                channels,
                ChannelState {
                    saved: vec![0.0; 1024],
                    prev_shape: 0,
                },
            );
        }
        let start = out.len();
        out.resize(start + 1024 * channels, 0.0);
        for (c, mut s) in spectra.into_iter().enumerate() {
            if let Some(tns) = s.tns.take() {
                self.apply_tns(&mut s, &tns);
            }
            self.synthesize(c, &s);
            let pcm = &self.frame[..1024];
            for (i, v) in pcm.iter().enumerate() {
                out[start + i * channels + c] = *v;
            }
        }
        Ok(1024)
    }

    fn ics_info(&self, b: &mut Bits) -> Result<Info, Error> {
        b.u(1)?; // ics_reserved_bit
        let seq = b.u(2)? as u8;
        let shape = b.u(1)? as u8;
        let mut info = Info {
            seq,
            shape,
            ..Info::default()
        };
        if seq == EIGHT_SHORT {
            info.max_sfb = b.u(4)? as usize;
            let grouping = b.u(7)?;
            info.groups = 1;
            info.group_len[0] = 1;
            for i in 0..7 {
                if grouping >> (6 - i) & 1 == 1 {
                    info.group_len[info.groups - 1] += 1;
                } else {
                    info.groups += 1;
                    info.group_len[info.groups - 1] = 1;
                }
            }
            if info.max_sfb > usize::from(tables::NUM_SWB_SHORT[self.sr]) {
                return Err(Error::Invalid("max_sfb"));
            }
        } else {
            info.max_sfb = b.u(6)? as usize;
            info.groups = 1;
            info.group_len[0] = 1;
            if b.u(1)? == 1 {
                return Err(Error::Unsupported("prediction (main / LTP profile)"));
            }
            if info.max_sfb > usize::from(tables::NUM_SWB_LONG[self.sr]) {
                return Err(Error::Invalid("max_sfb"));
            }
        }
        Ok(info)
    }

    fn offsets(&self, info: &Info) -> &'static [u16] {
        if info.seq == EIGHT_SHORT {
            tables::SWB_SHORT[self.sr]
        } else {
            tables::SWB_LONG[self.sr]
        }
    }

    /// `individual_channel_stream`: side info, scalefactors, tools and the
    /// dequantized spectrum.
    fn ics(&mut self, b: &mut Bits, common: Option<Info>, lfe: bool) -> Result<Spectrum, Error> {
        let global = b.u(8)? as i32;
        let info = match common {
            Some(i) => i,
            None => self.ics_info(b)?,
        };
        if lfe && info.seq == EIGHT_SHORT {
            return Err(Error::Invalid("short LFE window"));
        }
        let offsets = self.offsets(&info);
        let bands = info.max_sfb;
        let (sect_bits, esc) = if info.seq == EIGHT_SHORT {
            (3, 7)
        } else {
            (5, 31)
        };
        // Section data.
        let mut band = vec![ZERO_HCB; info.groups * bands];
        for g in 0..info.groups {
            let mut k = 0;
            while k < bands {
                let cb = b.u(4)? as u8;
                if cb == 12 {
                    return Err(Error::Invalid("reserved codebook 12"));
                }
                let mut len = 0usize;
                loop {
                    let inc = b.u(sect_bits)? as usize;
                    len += inc;
                    if inc != esc {
                        break;
                    }
                }
                if k + len > bands {
                    return Err(Error::Invalid("section length"));
                }
                band[g * bands + k..g * bands + k + len].fill(cb);
                k += len;
            }
        }
        // Scalefactors (gain, intensity position, noise energy).
        let t = trees();
        let mut value = vec![0i32; info.groups * bands];
        let (mut gain, mut is, mut noise) = (global, 0i32, global - 90);
        let mut first_noise = true;
        for i in 0..info.groups * bands {
            match band[i] {
                ZERO_HCB => {}
                INTENSITY_HCB | INTENSITY_HCB2 => {
                    is += t.sf.decode(b)? as i32 - 60;
                    value[i] = is;
                }
                NOISE_HCB => {
                    if first_noise {
                        noise += b.u(9)? as i32 - 256;
                        first_noise = false;
                    } else {
                        noise += t.sf.decode(b)? as i32 - 60;
                    }
                    value[i] = noise.clamp(-100, 155);
                }
                _ => {
                    gain += t.sf.decode(b)? as i32 - 60;
                    if !(0..=255).contains(&gain) {
                        return Err(Error::Invalid("scalefactor"));
                    }
                    value[i] = gain;
                }
            }
        }
        // Pulse data (long windows only).
        let mut pulses = Vec::new();
        if b.u(1)? == 1 {
            if info.seq == EIGHT_SHORT {
                return Err(Error::Invalid("pulse data in short window"));
            }
            let n = b.u(2)? as usize + 1;
            let start = b.u(6)? as usize;
            let mut k = usize::from(*offsets.get(start).ok_or(Error::Invalid("pulse band"))?);
            for _ in 0..n {
                k += b.u(5)? as usize;
                let amp = b.u(4)? as i32;
                if k >= 1024 {
                    return Err(Error::Invalid("pulse offset"));
                }
                pulses.push((k, amp));
            }
        }
        // TNS.
        let tns = if b.u(1)? == 1 {
            let short = info.seq == EIGHT_SHORT;
            let (nf_bits, len_bits, order_bits) = if short { (1, 4, 3) } else { (2, 6, 5) };
            let mut tns = Tns::default();
            for _ in 0..info.windows() {
                let n = b.u(nf_bits)?;
                let mut filters = Vec::new();
                if n > 0 {
                    let res = b.u(1)? + 3;
                    for _ in 0..n {
                        let len = b.u(len_bits)? as usize;
                        let order = b.u(order_bits)? as usize;
                        if order == 0 {
                            filters.push((len, false, Vec::new()));
                            continue;
                        }
                        if order > if short { 7 } else { 12 } {
                            return Err(Error::Invalid("TNS order"));
                        }
                        let dir = b.u(1)? == 1;
                        let compress = b.u(1)?;
                        let bits = res - compress;
                        let iqfac =
                            (f64::from(1u32 << (res - 1)) - 0.5) / std::f64::consts::FRAC_PI_2;
                        let iqfac_m =
                            (f64::from(1u32 << (res - 1)) + 0.5) / std::f64::consts::FRAC_PI_2;
                        let mut parcor = Vec::with_capacity(order);
                        for _ in 0..order {
                            let raw = b.u(bits)? as i32;
                            let q = if raw >= 1 << (bits - 1) {
                                raw - (1 << bits)
                            } else {
                                raw
                            };
                            let q = f64::from(q);
                            parcor.push((q / if q >= 0.0 { iqfac } else { iqfac_m }).sin());
                        }
                        filters.push((len, dir, lpc(&parcor)));
                    }
                }
                tns.filters.push(filters);
            }
            Some(tns)
        } else {
            None
        };
        if b.u(1)? == 1 {
            return Err(Error::Unsupported("gain control (SSR)"));
        }
        // Spectral data: integers, window-major.
        let mut q = vec![0i32; 1024];
        let mut win = 0usize;
        let mut vals = [0i32; 4];
        for g in 0..info.groups {
            let glen = info.group_len[g];
            for sfb in 0..bands {
                let cb = band[g * bands + sfb];
                let (lo, hi) = (usize::from(offsets[sfb]), usize::from(offsets[sfb + 1]));
                if cb == ZERO_HCB || cb >= NOISE_HCB {
                    continue;
                }
                let tree = &t.spectral[usize::from(cb) - 1];
                let (dims, signed, modulo, off) = match cb {
                    1 | 2 => (4, true, 3, 1),
                    3 | 4 => (4, false, 3, 0),
                    5 | 6 => (2, true, 9, 4),
                    7 | 8 => (2, false, 8, 0),
                    9 | 10 => (2, false, 13, 0),
                    _ => (2, false, 17, 0),
                };
                for w in 0..glen {
                    let base = (win + w) * 128;
                    let mut k = lo;
                    while k < hi {
                        let mut idx = tree.decode(b)? as i32;
                        for d in (0..dims).rev() {
                            vals[d] = idx % modulo - off;
                            idx /= modulo;
                        }
                        if !signed {
                            for v in vals.iter_mut().take(dims) {
                                if *v != 0 && b.u(1)? == 1 {
                                    *v = -*v;
                                }
                            }
                        }
                        if cb == 11 {
                            for v in vals.iter_mut().take(2) {
                                if v.abs() == 16 {
                                    let mut n = 0;
                                    while b.u(1)? == 1 {
                                        n += 1;
                                        if n > 8 {
                                            return Err(Error::Invalid("escape"));
                                        }
                                    }
                                    let m = (1 << (n + 4)) + b.u(n + 4)? as i32;
                                    *v = v.signum() * m;
                                }
                            }
                        }
                        for d in 0..dims {
                            q[base + k + d] = vals[d];
                        }
                        k += dims;
                    }
                }
            }
            win += glen;
        }
        for (k, amp) in pulses {
            q[k] += if q[k] > 0 { amp } else { -amp };
        }
        // Dequantize.
        let p43 = pow43();
        let mut coef = vec![0f32; 1024];
        let mut win = 0usize;
        for g in 0..info.groups {
            let glen = info.group_len[g];
            for sfb in 0..bands {
                let i = g * bands + sfb;
                let (lo, hi) = (usize::from(offsets[sfb]), usize::from(offsets[sfb + 1]));
                match band[i] {
                    ZERO_HCB | INTENSITY_HCB | INTENSITY_HCB2 => {}
                    NOISE_HCB => {
                        let scale_db = (0.25 * f64::from(value[i])).exp2();
                        for w in 0..glen {
                            let base = (win + w) * 128;
                            let mut energy = 0.0f64;
                            for k in lo..hi {
                                self.random =
                                    self.random.wrapping_mul(1664525).wrapping_add(1013904223);
                                let v = self.random as i32 as f64;
                                coef[base + k] = v as f32;
                                energy += v * v;
                            }
                            let s = if energy > 0.0 {
                                scale_db / energy.sqrt()
                            } else {
                                0.0
                            };
                            for k in lo..hi {
                                coef[base + k] = (f64::from(coef[base + k]) * s) as f32;
                            }
                        }
                    }
                    _ => {
                        let gain = (0.25 * f64::from(value[i] - 100)).exp2() as f32;
                        for w in 0..glen {
                            let base = (win + w) * 128;
                            for k in lo..hi {
                                let v = q[base + k];
                                let m = p43[v.unsigned_abs().min(8191) as usize] * gain;
                                coef[base + k] = if v < 0 { -m } else { m };
                            }
                        }
                    }
                }
            }
            win += glen;
        }
        Ok(Spectrum {
            info,
            coef,
            band,
            value,
            tns,
        })
    }

    fn apply_tns(&self, s: &mut Spectrum, tns: &Tns) {
        let short = s.info.seq == EIGHT_SHORT;
        let (num_swb, max_bands) = if short {
            (
                tables::NUM_SWB_SHORT[self.sr],
                tables::TNS_MAX_BANDS_SHORT[self.sr],
            )
        } else {
            (
                tables::NUM_SWB_LONG[self.sr],
                tables::TNS_MAX_BANDS_LONG[self.sr],
            )
        };
        let mmm = usize::from(max_bands).min(s.info.max_sfb);
        if mmm == 0 {
            return;
        }
        let offsets = self.offsets(&s.info);
        for (w, filters) in tns.filters.iter().enumerate() {
            let mut bottom = usize::from(num_swb);
            for (len, dir, lpc) in filters {
                let top = bottom;
                bottom = top.saturating_sub(*len);
                let order = lpc.len();
                if order == 0 {
                    continue;
                }
                let start = usize::from(offsets[bottom.min(mmm)]);
                let end = usize::from(offsets[top.min(mmm)]);
                if end <= start {
                    continue;
                }
                let size = end - start;
                let c = &mut s.coef[w * 128..];
                for m in 0..size {
                    let at = if *dir { end - 1 - m } else { start + m };
                    let mut v = c[at];
                    for i in 1..=m.min(order) {
                        let prev = if *dir { at + i } else { at - i };
                        v -= c[prev] * lpc[i - 1];
                    }
                    c[at] = v;
                }
            }
        }
    }

    /// IMDCT, windowing and overlap-add of channel `c` into `self.frame`.
    fn synthesize(&mut self, c: usize, s: &Spectrum) {
        let st = &mut self.state[c];
        let prev = usize::from(st.prev_shape);
        let cur = usize::from(s.info.shape);
        let [ls_prev, ll_prev] = &self.windows[prev];
        let [ls_cur, ll_cur] = &self.windows[cur];
        let f = &mut self.frame;
        f.fill(0.0);
        match s.info.seq {
            EIGHT_SHORT => {
                for w in 0..8 {
                    self.short
                        .run(&s.coef[w * 128..w * 128 + 128], &mut self.tmp);
                    let left = if w == 0 { ls_prev } else { ls_cur };
                    let at = 448 + w * 128;
                    for i in 0..128 {
                        f[at + i] += self.tmp[i] * left[i];
                        f[at + 128 + i] += self.tmp[128 + i] * ls_cur[127 - i];
                    }
                }
            }
            seq => {
                self.long.run(&s.coef, f);
                // Left half.
                if seq == LONG_STOP {
                    f[..448].fill(0.0);
                    for i in 0..128 {
                        f[448 + i] *= ls_prev[i];
                    }
                } else {
                    for i in 0..1024 {
                        f[i] *= ll_prev[i];
                    }
                }
                // Right half.
                if seq == LONG_START {
                    for i in 0..128 {
                        f[1472 + i] *= ls_cur[127 - i];
                    }
                    f[1600..].fill(0.0);
                } else {
                    for i in 0..1024 {
                        f[1024 + i] *= ll_cur[1023 - i];
                    }
                }
                debug_assert!(seq == ONLY_LONG || seq == LONG_START || seq == LONG_STOP);
            }
        }
        for i in 0..1024 {
            let v = f[i] + st.saved[i];
            st.saved[i] = f[1024 + i];
            // Spec output is in 16-bit sample units.
            f[i] = v * (1.0 / 32768.0);
        }
        st.prev_shape = s.info.shape;
    }
}

/// Reflection coefficients `sin(q / iqfac)` → direct-form LPC (step-up
/// recursion, as FFmpeg's `compute_lpc_coefs` with its negated tables).
fn lpc(parcor: &[f64]) -> Vec<f32> {
    let mut a = vec![0f64; parcor.len()];
    for i in 0..parcor.len() {
        let r = parcor[i];
        a[i] = r;
        for j in 0..(i + 1) >> 1 {
            let (f, b) = (a[j], a[i - 1 - j]);
            a[j] = f + r * b;
            a[i - 1 - j] = b + r * f;
        }
    }
    a.into_iter().map(|v| v as f32).collect()
}

/// M/S and intensity stereo of a pair with a common window.
fn stereo(l: &mut Spectrum, r: &mut Spectrum, ms: &(u32, Vec<bool>), short: &[u16], long: &[u16]) {
    let info = l.info;
    let offsets = if info.seq == EIGHT_SHORT { short } else { long };
    let bands = info.max_sfb;
    let on = |i: usize| match ms.0 {
        1 => ms.1.get(i).copied().unwrap_or(false),
        2 => true,
        _ => false,
    };
    let mut win = 0;
    for g in 0..info.groups {
        let glen = info.group_len[g];
        for sfb in 0..bands {
            let i = g * bands + sfb;
            let (lo, hi) = (usize::from(offsets[sfb]), usize::from(offsets[sfb + 1]));
            let (bl, br) = (l.band[i], r.band[i]);
            if on(i) && bl < NOISE_HCB && br < NOISE_HCB {
                for w in 0..glen {
                    let base = (win + w) * 128;
                    for k in lo..hi {
                        let (a, b) = (l.coef[base + k], r.coef[base + k]);
                        l.coef[base + k] = a + b;
                        r.coef[base + k] = a - b;
                    }
                }
            }
        }
        win += glen;
    }
    let mut win = 0;
    for g in 0..info.groups {
        let glen = info.group_len[g];
        for sfb in 0..bands {
            let i = g * bands + sfb;
            let br = r.band[i];
            if br != INTENSITY_HCB && br != INTENSITY_HCB2 {
                continue;
            }
            let (lo, hi) = (usize::from(offsets[sfb]), usize::from(offsets[sfb + 1]));
            let mut c = if br == INTENSITY_HCB { 1.0f32 } else { -1.0 };
            if ms.0 != 0 && on(i) {
                c = -c;
            }
            let scale = c * (-0.25 * f64::from(r.value[i])).exp2() as f32;
            for w in 0..glen {
                let base = (win + w) * 128;
                for k in lo..hi {
                    r.coef[base + k] = l.coef[base + k] * scale;
                }
            }
        }
        win += glen;
    }
}

/// `program_config_element` (skipped; the channel layout comes from the
/// elements themselves).
fn skip_pce(b: &mut Bits) -> Result<(), Error> {
    b.u(4)?; // element_instance_tag
    b.u(2)?; // object_type
    b.u(4)?; // sampling_frequency_index
    let front = b.u(4)?;
    let side = b.u(4)?;
    let back = b.u(4)?;
    let lfe = b.u(2)?;
    let assoc = b.u(3)?;
    let cc = b.u(4)?;
    for _ in 0..3 {
        if b.u(1)? == 1 {
            b.u(4)?; // mixdown element numbers
        }
    }
    if b.u(1)? == 1 {
        b.u(3)?; // matrix mixdown
    }
    b.skip(((front + side + back) * 5 + lfe * 4 + assoc * 4 + cc * 5) as usize)?;
    b.align();
    let comment = b.u(8)? as usize;
    b.skip(comment * 8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_lc_and_he() {
        let (c, idx) = parse_config(&[0x11, 0x90]).unwrap();
        assert_eq!(
            (c.object_type, c.sample_rate, c.channels, c.extension, idx),
            (2, 48000, 2, false, 3)
        );
        // HE-AAC: SBR over a 24 kHz LC core.
        let (c, _) = parse_config(&[0x2b, 0x11, 0x88, 0x00]).unwrap();
        assert_eq!(
            (c.object_type, c.sample_rate, c.extension),
            (2, 24000, true)
        );
        assert!(parse_config(&[0x0a, 0x10]).is_err()); // AAC Main
    }

    #[test]
    fn lpc_from_one_parcor() {
        assert_eq!(lpc(&[0.5]), vec![0.5]);
        let a = lpc(&[0.5, 0.25]);
        assert!((a[0] - (0.5 + 0.25 * 0.5)).abs() < 1e-6 && (a[1] - 0.25).abs() < 1e-6);
    }
}
