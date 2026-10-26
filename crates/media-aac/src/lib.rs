// SPDX-License-Identifier: LGPL-2.1-or-later
//! AAC-LC decoder (ISO/IEC 14496-3 4.4–4.6) for music-video sound checks:
//! single / pair / LFE elements, all window sequences and shapes, M/S and
//! intensity stereo, perceptual noise substitution, temporal noise shaping
//! and pulse data; HE-AAC's SBR ([`sbr`], twice the core rate) and
//! HE-AACv2's parametric stereo ([`ps`]). Output is f32 PCM, full scale
//! ±1.0, 1024 frames per access unit (2048 with SBR), channels in element
//! order.
//!
//! Not decoded: main prediction, LTP, SSR, coupling channels and
//! downsampled SBR; such streams are refused or reported by the caller.
//! Tables come from FFmpeg (LGPL), the code is AUDENIQ's.
mod bits;
pub mod encoder;
mod huffman;
mod mdct;
mod ps;
mod ps_tables;
mod sbr;
mod sbr_tables;
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
    /// Each spectral codebook's symbols as their (up to four) values.
    values: Vec<Vec<[i8; 4]>>,
}

fn trees() -> &'static Trees {
    static T: OnceLock<Trees> = OnceLock::new();
    T.get_or_init(|| Trees {
        sf: Tree::new(&tables::SF_CODES, &tables::SF_BITS),
        spectral: tables::SPECTRAL
            .iter()
            .map(|(c, l)| Tree::new(c, l))
            .collect(),
        values: (1..=11u8)
            .map(|cb| {
                let (dims, modulo, off) = unpacking(cb);
                let n = (modulo as usize).pow(dims as u32);
                (0..n as i32)
                    .map(|mut idx| {
                        let mut v = [0i8; 4];
                        for d in (0..dims).rev() {
                            v[d] = (idx % modulo - off) as i8;
                            idx /= modulo;
                        }
                        v
                    })
                    .collect()
            })
            .collect(),
    })
}

/// Values per symbol, their modulus and offset for codebook `cb`.
fn unpacking(cb: u8) -> (usize, i32, i32) {
    match cb {
        1 | 2 => (4, 3, 1),
        3 | 4 => (4, 3, 0),
        5 | 6 => (2, 9, 4),
        7 | 8 => (2, 8, 0),
        9 | 10 => (2, 13, 0),
        _ => (2, 17, 0),
    }
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
    /// Core object type (2, AAC-LC).
    pub object_type: u32,
    /// Core sample rate and channels.
    pub sample_rate: u32,
    pub channels: u32,
    /// SBR signalled (HE-AAC): explicitly, or by a sync extension.
    pub extension: bool,
    /// SBR: 1 present, 0 absent, −1 unknown (found in the first access
    /// unit or never: implicit signalling). PS likewise (HE-AACv2).
    pub sbr: i8,
    pub ps: i8,
    /// Output rate SBR signals (0 when not said).
    pub ext_rate: u32,
}

/// Parse an `AudioSpecificConfig` (ISO/IEC 14496-3 1.6.2.1), with the SBR
/// and PS signalling as FFmpeg reads it: explicit (object type 5 or 29),
/// backward-compatible (sync extension 0x2b7 after the core config) or
/// implicit (left unknown).
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
    let chan_config = b.u(4)?;
    let (mut sbr, mut ps, mut ext_rate) = (-1i8, -1i8, 0u32);
    let explicit = ot == 5 || ot == 29;
    if explicit {
        if ot == 29 {
            ps = 1;
        }
        sbr = 1;
        // The extension rate first, then the core object type; the core
        // rate stays the one above.
        ext_rate = rate(&mut b)?.1;
        ot = object(&mut b)?;
    }
    if ot != 2 {
        return Err(Error::Unsupported("object type (only AAC-LC)"));
    }
    if idx > 12 {
        return Err(Error::Invalid("sampling frequency index"));
    }
    if !explicit {
        // Sync extension: searched bit by bit from here.
        let mut s = b.clone();
        while s.left() > 15 {
            if s.clone().u(11)? == 0x2b7 {
                s.u(11)?;
                if object(&mut s)? == 5 {
                    sbr = s.u(1)? as i8;
                    if sbr == 1 {
                        ext_rate = rate(&mut s)?.1;
                        if ext_rate == sample_rate {
                            sbr = -1;
                        }
                    }
                }
                if s.left() > 11 && s.u(11)? == 0x548 {
                    ps = s.u(1)? as i8;
                }
                break;
            }
            s.u(1)?;
        }
    }
    // GASpecificConfig: frameLengthFlag must select 1024.
    if b.u(1)? != 0 {
        return Err(Error::Unsupported("960-sample frames"));
    }
    let channels = match chan_config {
        0 => {
            if b.u(1)? == 1 {
                b.u(14)?; // dependsOnCoreCoder: coreCoderDelay
            }
            b.u(1)?; // extensionFlag
            match pce(&mut b)? {
                n @ 1..=8 => n,
                _ => return Err(Error::Unsupported("channel configuration")),
            }
        }
        1..=6 => chan_config,
        7 => 8,
        _ => return Err(Error::Unsupported("channel configuration")),
    };
    if sbr == 0 || channels > 1 {
        ps = 0;
    } else if sbr == 1 && ps == -1 {
        // Mono HE-AAC may carry PS: decoded as stereo, as FFmpeg does.
        ps = 1;
    }
    if sample_rate == 0 {
        idx = 4;
        sample_rate = RATES[idx];
    }
    Ok((
        Config {
            object_type: ot,
            sample_rate,
            channels,
            extension: sbr == 1,
            sbr,
            ps,
            ext_rate,
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
    /// SBR and PS in use: 1 yes, 0 no, −1 not known yet (implicit SBR is
    /// recognised in the first access unit only, as FFmpeg does).
    sbr_mode: i8,
    ps_mode: i8,
    /// Access units decoded.
    units: u64,
    /// SBR state per channel element (element order in the access unit).
    sbr: Vec<Option<Box<sbr::Sbr>>>,
    /// Core output per channel of the current access unit.
    core: Vec<Vec<f32>>,
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
            sbr_mode: config.sbr,
            ps_mode: config.ps,
            units: 0,
            sbr: Vec::new(),
            core: Vec::new(),
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

    /// Whether SBR is applied (HE-AAC).
    pub fn sbr(&self) -> bool {
        self.sbr_mode == 1
    }

    /// FFprobe's profile name for what was decoded so far: `LC`, `HE-AAC`
    /// (SBR) or `HE-AACv2` (SBR with parametric stereo data).
    pub fn profile(&self) -> &'static str {
        if !self.sbr() {
            "LC"
        } else if self.sbr.iter().flatten().any(|s| s.ps.started()) {
            "HE-AACv2"
        } else {
            "HE-AAC"
        }
    }

    /// Whether a mono core comes out as stereo (HE-AACv2 / PS).
    fn ps_stereo(&self) -> bool {
        self.sbr_mode == 1 && self.ps_mode == 1 && self.config.channels == 1
    }

    /// Channels of the decoded output.
    pub fn channels(&self) -> usize {
        self.config.channels as usize + usize::from(self.ps_stereo())
    }

    /// SBR signalled at less than twice the core rate: synthesized at the
    /// core rate (downsampled SBR).
    fn sbr_down(&self) -> bool {
        self.config.ext_rate != 0 && self.config.ext_rate < 2 * self.config.sample_rate
    }

    /// Sample rate of the decoded output (twice the core's with SBR).
    pub fn output_rate(&self) -> u32 {
        if self.sbr() && !self.sbr_down() {
            2 * self.config.sample_rate
        } else {
            self.config.sample_rate
        }
    }

    /// Output channel names in decode (AAC element) order for the
    /// configuration: C, L, R, Ls, Rs, LFE for 5.1.
    pub fn layout(&self) -> &'static [&'static str] {
        if self.ps_stereo() {
            return &["L", "R"];
        }
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
        // Channel elements: (type, first channel); a fill element's SBR
        // data belongs to the one before it.
        let mut elements: Vec<(u32, usize)> = Vec::new();
        loop {
            let id = b.u(3)?;
            match id {
                0 | 3 => {
                    b.u(4)?;
                    elements.push((id, spectra.len()));
                    spectra.push(self.ics(&mut b, None, id == 3)?);
                }
                1 => {
                    elements.push((1, spectra.len()));
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
                    if n > 0 {
                        self.fill(&b, n, &elements)?;
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
        self.core.resize(channels, Vec::new());
        for (c, mut s) in spectra.into_iter().enumerate() {
            if let Some(tns) = s.tns.take() {
                self.apply_tns(&mut s, &tns);
            }
            self.synthesize(c, &s);
            self.core[c].clear();
            self.core[c].extend_from_slice(&self.frame[..1024]);
        }
        self.units += 1;
        if self.sbr_mode == -1 {
            // No SBR in the first access unit: plain AAC-LC.
            self.sbr_mode = 0;
        }
        if !self.sbr() {
            let start = out.len();
            out.resize(start + 1024 * channels, 0.0);
            for (c, pcm) in self.core.iter().enumerate() {
                for (i, v) in pcm.iter().enumerate() {
                    out[start + i * channels + c] = *v;
                }
            }
            return Ok(1024);
        }
        // SBR on every element (without data of its own it only upsamples).
        let ps = self.ps_stereo();
        let down = self.sbr_down();
        let len = if down { 1024 } else { 2048 };
        let total = channels + usize::from(ps);
        let start = out.len();
        out.resize(start + len * total, 0.0);
        if self.sbr.len() < elements.len() {
            self.sbr.resize_with(elements.len(), || None);
        }
        let mut oc = 0;
        let (mut l, mut r) = (vec![0f32; 2048], vec![0f32; 2048]);
        for (e, &(id, first)) in elements.iter().enumerate() {
            let rate = self.config.sample_rate;
            let s = self.sbr[e].get_or_insert_with(|| Box::new(sbr::Sbr::new(rate)));
            let pair = id == 1;
            let core_r = if pair {
                &self.core[first + 1]
            } else {
                &self.core[first]
            };
            let stereo_out = pair || (ps && id == 0);
            s.apply(
                id,
                [&self.core[first], core_r],
                ps && id == 0,
                down,
                &mut l,
                &mut r,
            );
            for i in 0..len {
                out[start + i * total + oc] = l[i];
            }
            oc += 1;
            if stereo_out {
                for i in 0..len {
                    out[start + i * total + oc] = r[i];
                }
                oc += 1;
            }
        }
        Ok(len)
    }

    /// A fill element of `n` bytes at `b`: SBR data for the element before
    /// it (`extension_payload` 13/14), anything else skipped.
    fn fill(&mut self, b: &Bits, n: usize, elements: &[(u32, usize)]) -> Result<(), Error> {
        let mut p = b.take(n * 8);
        let kind = p.u(4)?;
        if !matches!(kind, 13 | 14) {
            return Ok(());
        }
        let Some(&(id, _)) = elements.last() else {
            return Ok(());
        };
        match self.sbr_mode {
            0 => return Ok(()),
            -1 if self.units > 0 => return Ok(()),
            -1 => {
                // Implicit signalling: SBR in the first access unit.
                self.sbr_mode = 1;
                if self.ps_mode == -1 && self.config.channels == 1 {
                    self.ps_mode = 1;
                }
            }
            _ => {}
        }
        let e = elements.len() - 1;
        if self.sbr.len() <= e {
            self.sbr.resize_with(e + 1, || None);
        }
        let rate = self.config.sample_rate;
        let ps_allowed = self.ps_mode == 1;
        self.sbr[e]
            .get_or_insert_with(|| Box::new(sbr::Sbr::new(rate)))
            .read(&mut p, kind == 14, id, ps_allowed);
        Ok(())
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
                let table = &t.values[usize::from(cb) - 1];
                let dims = unpacking(cb).0;
                let signed = matches!(cb, 1 | 2 | 5 | 6);
                for w in 0..glen {
                    let base = (win + w) * 128;
                    let mut k = lo;
                    while k < hi {
                        let sym = table
                            .get(tree.decode(b)?)
                            .ok_or(Error::Invalid("Huffman codeword"))?;
                        for (v, &s) in vals.iter_mut().zip(sym) {
                            *v = i32::from(s);
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
/// `program_config_element`: the channels it declares (pairs count two).
fn pce(b: &mut Bits) -> Result<u32, Error> {
    b.u(4)?; // element_instance_tag
    b.u(2)?; // object_type
    b.u(4)?; // sampling_frequency_index
    let front = b.u(4)?;
    let side = b.u(4)?;
    let back = b.u(4)?;
    let lfe = b.u(2)?;
    let assoc = b.u(3)?;
    let cc = b.u(4)?;
    // Mono and stereo mixdown element numbers, matrix mixdown index.
    for bits in [4, 4, 3] {
        if b.u(1)? == 1 {
            b.u(bits)?;
        }
    }
    let mut channels = lfe;
    for _ in 0..front + side + back {
        channels += 1 + b.u(1)?; // is_cpe
        b.u(4)?;
    }
    b.skip((lfe * 4 + assoc * 4 + cc * 5) as usize)?;
    b.align();
    let comment = b.u(8)? as usize;
    b.skip(comment * 8)?;
    Ok(channels)
}

fn skip_pce(b: &mut Bits) -> Result<(), Error> {
    pce(b).map(|_| ())
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

    /// MSB-first bit writer for building test configs and access units.
    struct Writer(Vec<u8>, usize);

    impl Writer {
        fn new() -> Self {
            Writer(Vec::new(), 0)
        }
        fn put(&mut self, v: u32, n: u32) {
            for i in (0..n).rev() {
                if self.1.is_multiple_of(8) {
                    self.0.push(0);
                }
                let last = self.0.len() - 1;
                self.0[last] |= ((v >> i & 1) as u8) << (7 - self.1 % 8);
                self.1 += 1;
            }
        }
    }

    /// `object` (5 HE-AAC, 29 HE-AACv2) over an AAC-LC core at 24 kHz.
    fn he_config(object: u32, channels: u32) -> Vec<u8> {
        let mut w = Writer::new();
        w.put(object, 5);
        w.put(6, 4); // 24 kHz core
        w.put(channels, 4);
        w.put(3, 4); // 48 kHz output
        w.put(2, 5); // AAC-LC core
        w.put(0, 3); // GASpecificConfig
        w.0
    }

    #[test]
    fn config_signalling() {
        let (c, _) = parse_config(&he_config(5, 2)).unwrap();
        assert_eq!(
            (c.sbr, c.ps, c.ext_rate, c.sample_rate),
            (1, 0, 48000, 24000)
        );
        let (c, _) = parse_config(&he_config(5, 1)).unwrap();
        assert_eq!((c.sbr, c.ps), (1, 1), "mono HE-AAC may carry PS");
        let (c, _) = parse_config(&he_config(29, 1)).unwrap();
        assert_eq!((c.sbr, c.ps), (1, 1));
        // AAC-LC 24 kHz stereo + sync extension 0x2b7 → SBR 48 kHz.
        let mut w = Writer::new();
        for (v, n) in [
            (2, 5),
            (6, 4),
            (2, 4),
            (0, 3),
            (0x2b7, 11),
            (5, 5),
            (1, 1),
            (3, 4),
        ] {
            w.put(v, n);
        }
        let (c, _) = parse_config(&w.0).unwrap();
        assert_eq!((c.sbr, c.ext_rate, c.sample_rate), (1, 48000, 24000));
        // Plain AAC-LC: implicit SBR stays possible.
        let (c, _) = parse_config(&[0x11, 0x90]).unwrap();
        assert_eq!((c.sbr, c.ps), (-1, 0));
        // Channel configuration 0 with a PCE of one pair and one single.
        let mut w = Writer::new();
        for (v, n) in [(2, 5), (3, 4), (0, 4), (0, 3)] {
            w.put(v, n);
        }
        for (v, n) in [
            (0, 4),
            (1, 2),
            (3, 4),
            (2, 4),
            (0, 4),
            (0, 4),
            (0, 2),
            (0, 3),
            (0, 4),
        ] {
            w.put(v, n);
        }
        w.put(0, 3); // no mixdowns
        for (v, n) in [(1, 1), (0, 4), (0, 1), (1, 4)] {
            w.put(v, n); // front: CPE, SCE
        }
        while !w.1.is_multiple_of(8) {
            w.put(0, 1);
        }
        w.put(0, 8); // comment bytes
        let (c, _) = parse_config(&w.0).unwrap();
        assert_eq!(c.channels, 3);
    }

    /// Random SBR (and PS) payloads after a silent element: no panics, and
    /// the output keeps its length.
    #[test]
    fn random_sbr_payloads_do_not_panic() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for (object, pair) in [(5, false), (5, true), (29, false)] {
            let mut d = Decoder::new(&he_config(object, if pair { 2 } else { 1 })).unwrap();
            let mut out = Vec::new();
            let (mut ok, mut started) = (0, 0);
            for unit in 0..4000 {
                let mut w = Writer::new();
                let elements = if pair { 1 } else { 0 };
                w.put(elements, 3);
                w.put(0, 4);
                if pair {
                    w.put(0, 1); // no common window
                }
                for _ in 0..if pair { 2 } else { 1 } {
                    w.put(100, 8); // global gain
                    w.put(0, 1);
                    w.put(0, 2); // only long
                    w.put(0, 1);
                    w.put(0, 6); // max_sfb 0
                    w.put(0, 1);
                    w.put(0, 3); // no pulse, TNS, gain control
                }
                let n = 1 + (next() % 60) as u32;
                w.put(6, 3); // FIL
                if n >= 15 {
                    w.put(15, 4);
                    w.put(n - 14, 8);
                } else {
                    w.put(n, 4);
                }
                w.put(if next() % 4 == 0 { 14 } else { 13 }, 4);
                // The payload's n bytes: usually a header, then noise.
                let mut left = n * 8 - 4;
                if unit % 3 != 0 && left > 13 {
                    w.put(1, 1);
                    w.put(next() as u32, 12);
                    left -= 13;
                }
                for _ in 0..left {
                    w.put(next() as u32 & 1, 1);
                }
                w.put(7, 3);
                let start = out.len();
                if d.decode(&w.0, &mut out).is_ok() {
                    assert_eq!(out.len() - start, 2048 * d.channels());
                    ok += 1;
                    if d.sbr.iter().flatten().any(|s| s.started()) {
                        started += 1;
                    }
                }
                assert!(out.iter().all(|v| v.is_finite()));
                out.clear();
            }
            eprintln!("object {object} pair {pair}: {ok} decoded, {started} with SBR running");
            assert!(started > 100, "SBR data rarely parsed: {started}");
        }
    }
}
