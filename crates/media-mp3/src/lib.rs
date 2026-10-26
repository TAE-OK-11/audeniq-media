// SPDX-License-Identifier: LGPL-2.1-or-later
//! MPEG-1/2/2.5 Layer III decoder (ISO/IEC 11172-3, 13818-3) for the sound
//! of music videos: mono and stereo, M/S and intensity stereo, long, short
//! and mixed blocks, the bit reservoir, CRC-protected frames. Output is f32
//! PCM, full scale ±1.0, 1152 (MPEG-1) or 576 frames per frame.
//!
//! Huffman, band and window tables come from FFmpeg (LGPL, `tables.rs`);
//! the code is AUDENIQ's. Not decoded: free-format bit rate, Layers I/II.
mod tables;

use media_bits::{Bits, Huffman};
use std::f64::consts::PI;
use std::sync::OnceLock;
use tables::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Truncated,
    Invalid(&'static str),
    Unsupported(&'static str),
}

impl From<media_bits::Error> for Error {
    fn from(e: media_bits::Error) -> Self {
        match e {
            media_bits::Error::Eof => Error::Truncated,
            media_bits::Error::Invalid(what) => Error::Invalid(what),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::Truncated => f.write_str("MP3 frame ends early"),
            Error::Invalid(s) => write!(f, "invalid MP3 {s}"),
            Error::Unsupported(s) => write!(f, "unsupported MP3 {s}"),
        }
    }
}

/// What a decode call gave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    pub sample_rate: u32,
    pub channels: usize,
    /// Frames (samples per channel) appended.
    pub frames: usize,
}

/// Interleaved `pcm` of `from` channels appended to `out` as `to` channels
/// (a stream may switch between mono and stereo frames): mono is copied to
/// both sides, stereo averaged to mono.
pub fn to_channels(pcm: &[f32], from: usize, to: usize, out: &mut Vec<f32>) {
    match (from, to) {
        (1, 2) => out.extend(pcm.iter().flat_map(|&v| [v, v])),
        (2, 1) => out.extend(pcm.chunks_exact(2).map(|p| 0.5 * (p[0] + p[1]))),
        _ => out.extend_from_slice(pcm),
    }
}

/// Tables built once: Huffman trees, |x|^(4/3), IMDCT and synthesis
/// matrices and windows.
struct Static {
    big: Vec<Huffman>,
    quad: [Huffman; 2],
    pow43: Vec<f32>,
    /// cos for the 36- and 12-point IMDCT: [i][k].
    cos36: Vec<[f32; 18]>,
    cos12: Vec<[f32; 6]>,
    /// Long windows by block type (0 normal, 1 start, 3 stop) and short.
    win: [[f32; 36]; 4],
    /// Antialias butterflies (cs, ca).
    alias: [(f32, f32); 8],
    /// Synthesis matrixing N[i][k] and window D.
    matrix: Vec<[f32; 32]>,
    window: Vec<f32>,
}

fn statics() -> &'static Static {
    static S: OnceLock<Static> = OnceLock::new();
    S.get_or_init(|| {
        let mut big = Vec::with_capacity(15);
        let mut at = 0;
        for &n in &HUFF_SIZES {
            let mut codes = Vec::with_capacity(n);
            let mut code = 0u64;
            for i in at..at + n {
                let len = u32::from(HUFF_LENS[i]);
                codes.push(((code >> (32 - len)) as u32, len, u32::from(HUFF_SYMS[i])));
                code += 1u64 << (32 - len);
            }
            big.push(Huffman::from_codes(codes));
            at += n;
        }
        let quad = std::array::from_fn(|q| {
            Huffman::from_codes((0..16).map(|v| {
                (
                    u32::from(QUAD_CODES[q][v]),
                    u32::from(QUAD_BITS[q][v]),
                    v as u32,
                )
            }))
        });
        let pow43 = (0..8207)
            .map(|i| (i as f64).powf(4.0 / 3.0) as f32)
            .collect();
        let cos36 = (0..36)
            .map(|i| {
                std::array::from_fn(|k| {
                    (PI / 72.0 * (2 * i + 1 + 18) as f64 * (2 * k + 1) as f64).cos() as f32
                })
            })
            .collect();
        let cos12 = (0..12)
            .map(|i| {
                std::array::from_fn(|k| {
                    (PI / 24.0 * (2 * i + 1 + 6) as f64 * (2 * k + 1) as f64).cos() as f32
                })
            })
            .collect();
        let sin36 = |i: usize| (PI / 36.0 * (i as f64 + 0.5)).sin() as f32;
        let sin12 = |i: usize| (PI / 12.0 * (i as f64 + 0.5)).sin() as f32;
        let mut win = [[0f32; 36]; 4];
        for i in 0..36 {
            win[0][i] = sin36(i);
            win[1][i] = match i {
                0..18 => sin36(i),
                18..24 => 1.0,
                24..30 => sin12(i - 18),
                _ => 0.0,
            };
            win[3][i] = match i {
                0..6 => 0.0,
                6..12 => sin12(i - 6),
                12..18 => 1.0,
                _ => sin36(i),
            };
        }
        for i in 0..12 {
            win[2][i] = sin12(i);
        }
        let ci = [
            -0.6, -0.535, -0.33, -0.185, -0.095, -0.041, -0.0142, -0.0037f64,
        ];
        let alias = ci.map(|c| {
            let n = (1.0 + c * c).sqrt();
            ((1.0 / n) as f32, (c / n) as f32)
        });
        let matrix = (0..64)
            .map(|i| {
                std::array::from_fn(|k| {
                    ((16 + i) as f64 * (2 * k + 1) as f64 * PI / 64.0).cos() as f32
                })
            })
            .collect();
        // D[512 - i] = -D[i] except at multiples of 64.
        let mut window = vec![0f32; 512];
        for (i, &w) in WINDOW.iter().enumerate() {
            let v = w as f32 / 65536.0;
            window[i] = v;
            if i != 0 {
                window[512 - i] = if i % 64 != 0 { -v } else { v };
            }
        }
        Static {
            big,
            quad,
            pow43,
            cos36,
            cos12,
            win,
            alias,
            matrix,
            window,
        }
    })
}

#[derive(Debug, Clone, Copy)]
struct Header {
    lsf: bool,
    crc: bool,
    sample_rate: u32,
    /// Band table row: 0–2 MPEG-1, 3–5 MPEG-2, 6–8 MPEG-2.5.
    sr_index: usize,
    /// 0 stereo, 1 joint stereo, 2 dual channel, 3 mono.
    mode: u32,
    mode_ext: u32,
    len: usize,
    channels: usize,
}

const BITRATES: [[u32; 15]; 2] = [
    [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
    ],
    [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
];
const RATES: [u32; 3] = [44100, 48000, 32000];

fn header(b: &[u8]) -> Result<Header, Error> {
    let h = u32::from_be_bytes(b.get(..4).ok_or(Error::Truncated)?.try_into().unwrap());
    if h >> 21 != 0x7ff {
        return Err(Error::Invalid("frame sync"));
    }
    let (lsf, mpeg25) = match h >> 19 & 3 {
        0 => (true, true),
        2 => (true, false),
        3 => (false, false),
        _ => return Err(Error::Invalid("version")),
    };
    if h >> 17 & 3 != 1 {
        return Err(Error::Unsupported("layer (only Layer III)"));
    }
    let br = (h >> 12 & 15) as usize;
    let sr = (h >> 10 & 3) as usize;
    if br == 0 {
        return Err(Error::Unsupported("free-format bit rate"));
    }
    if br == 15 || sr == 3 {
        return Err(Error::Invalid("bit rate or sample rate index"));
    }
    let sample_rate = RATES[sr] >> (u32::from(lsf) + u32::from(mpeg25));
    let bitrate = BITRATES[usize::from(lsf)][br] * 1000;
    let padding = (h >> 9 & 1) as usize;
    let mode = h >> 6 & 3;
    Ok(Header {
        lsf,
        crc: h >> 16 & 1 == 0,
        sample_rate,
        sr_index: sr
            + if mpeg25 {
                6
            } else if lsf {
                3
            } else {
                0
            },
        mode,
        mode_ext: h >> 4 & 3,
        len: (if lsf { 72 } else { 144 }) * bitrate as usize / sample_rate as usize + padding,
        channels: if mode == 3 { 1 } else { 2 },
    })
}

#[derive(Default, Clone, Copy)]
struct Granule {
    part23: usize,
    big_values: usize,
    global_gain: i32,
    scalefac_compress: u32,
    block_type: u8,
    mixed: bool,
    table_select: [u8; 3],
    subblock_gain: [u8; 3],
    /// Pairs in big-value regions 0, 1, 2.
    region: [usize; 3],
    preflag: bool,
    scalefac_scale: bool,
    count1_table: usize,
    scfsi: u32,
    /// Long bands end and short bands start (13: none).
    long_end: usize,
    short_start: usize,
}

/// Start of each long band in pairs of samples.
fn band_pairs(sr: usize, band: usize) -> usize {
    BAND_LONG[sr][..band.min(22)]
        .iter()
        .map(|&w| usize::from(w))
        .sum::<usize>()
        / 2
}

/// Bytes of main data kept for later frames (main_data_begin ≤ 511).
const RESERVOIR: usize = 1024;

pub struct Decoder {
    reservoir: Vec<u8>,
    /// IMDCT second halves per channel, subband-major (32 × 18).
    overlap: [[f32; 576]; 2],
    /// Synthesis V vectors (newest first) per channel.
    v: [Vec<f32>; 2],
}

impl Default for Decoder {
    fn default() -> Self {
        Decoder::new()
    }
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder {
            reservoir: Vec::new(),
            overlap: [[0.0; 576]; 2],
            v: [vec![0.0; 1024], vec![0.0; 1024]],
        }
    }

    /// Decode the frames in `packet` (an MP4 sample is one frame),
    /// appending interleaved samples to `out`.
    pub fn decode(&mut self, packet: &[u8], out: &mut Vec<f32>) -> Result<Frame, Error> {
        let mut at = 0;
        let mut info = None;
        let mut frames = 0;
        while at + 4 <= packet.len() {
            let h = match header(&packet[at..]) {
                Ok(h) => h,
                Err(e) if info.is_none() => return Err(e),
                Err(_) => break,
            };
            let Some(f) = packet.get(at..at + h.len) else {
                if info.is_none() {
                    return Err(Error::Truncated);
                }
                break;
            };
            frames += self.frame(&h, f, out)?;
            info = Some(h);
            at += h.len;
        }
        let h = info.ok_or(Error::Truncated)?;
        Ok(Frame {
            sample_rate: h.sample_rate,
            channels: h.channels,
            frames,
        })
    }

    fn frame(&mut self, h: &Header, f: &[u8], out: &mut Vec<f32>) -> Result<usize, Error> {
        let nch = h.channels;
        let ngr = if h.lsf { 1 } else { 2 };
        let side_len = match (h.lsf, nch) {
            (false, 1) => 17,
            (false, _) => 32,
            (true, 1) => 9,
            (true, _) => 17,
        };
        let side_at = if h.crc { 6 } else { 4 };
        let side = f.get(side_at..side_at + side_len).ok_or(Error::Truncated)?;
        let mut b = Bits::new(side);
        let main_begin = b.get(if h.lsf { 8 } else { 9 })? as usize;
        b.get(match (h.lsf, nch) {
            (true, n) => n as u32,
            (false, 1) => 5,
            (false, _) => 3,
        })?;
        let mut gr = [[Granule::default(); 2]; 2];
        if !h.lsf {
            for g in gr[1].iter_mut().take(nch) {
                g.scfsi = b.get(4)?;
            }
        }
        let ms_only = h.mode == 1 && h.mode_ext == 2;
        for grs in gr.iter_mut().take(ngr) {
            for g in grs.iter_mut().take(nch) {
                g.part23 = b.get(12)? as usize;
                g.big_values = b.get(9)? as usize;
                if g.big_values > 288 {
                    return Err(Error::Invalid("big_values"));
                }
                g.global_gain = b.get(8)? as i32;
                if ms_only {
                    // The 1/√2 of M/S folded into the gain.
                    g.global_gain -= 2;
                }
                g.scalefac_compress = b.get(if h.lsf { 9 } else { 4 })?;
                if b.bit()? == 1 {
                    g.block_type = b.get(2)? as u8;
                    if g.block_type == 0 {
                        return Err(Error::Invalid("block type"));
                    }
                    g.mixed = b.bit()? == 1;
                    for t in &mut g.table_select[..2] {
                        *t = b.get(5)? as u8;
                    }
                    for s in &mut g.subblock_gain {
                        *s = b.get(3)? as u8;
                    }
                    g.region[0] = match (g.block_type, h.sr_index) {
                        (2, 8) => 36,
                        (2, _) => 18,
                        (_, 0..=2) => 18,
                        (_, 8) => 54,
                        _ => 27,
                    };
                    g.region[1] = 288;
                } else {
                    for t in &mut g.table_select {
                        *t = b.get(5)? as u8;
                    }
                    let r0 = b.get(4)? as usize;
                    let r1 = b.get(3)? as usize;
                    g.region[0] = band_pairs(h.sr_index, r0 + 1);
                    g.region[1] = band_pairs(h.sr_index, r0 + r1 + 2);
                }
                // Region ends → sizes, cut at big_values.
                g.region[2] = 288;
                let mut prev = 0;
                for r in &mut g.region {
                    let end = (*r).min(g.big_values).max(prev);
                    *r = end - prev;
                    prev = end;
                }
                if !h.lsf {
                    g.preflag = b.bit()? == 1;
                }
                g.scalefac_scale = b.bit()? == 1;
                g.count1_table = b.bit()? as usize;
                (g.long_end, g.short_start) = match (g.block_type, g.mixed) {
                    (2, true) => (if h.sr_index <= 2 { 8 } else { 6 }, 3),
                    (2, false) => (0, 0),
                    _ => (22, 13),
                };
            }
        }

        // Main data starts `main_begin` bytes before this frame's own, in
        // the reservoir. Granules that start before the bytes still held
        // (after a cut or a damaged frame) stay silent; the rest decode.
        let main = f.get(side_at + side_len..).unwrap_or_default();
        let mut data = Vec::with_capacity(self.reservoir.len() + main.len());
        data.extend_from_slice(&self.reservoir);
        data.extend_from_slice(main);
        let mut pos = (self.reservoir.len() as i64 - main_begin as i64) * 8;
        self.reservoir.extend_from_slice(main);
        if self.reservoir.len() > RESERVOIR {
            self.reservoir.drain(..self.reservoir.len() - RESERVOIR);
        }

        let mut bits = Bits::new(&data);
        let mut xr = [[0f32; 576]; 2];
        let mut sf = [[0u8; 40]; 2];
        let mut pcm = vec![0f32; ngr * 576 * nch];
        for g_i in 0..ngr {
            for ch in 0..nch {
                xr[ch] = [0.0; 576];
                let g = gr[g_i][ch];
                let end = pos + g.part23 as i64;
                if pos >= 0 && end as usize <= data.len() * 8 {
                    bits.seek(pos as usize)?;
                    let sf_prev = sf[ch];
                    sf[ch] = if h.lsf {
                        lsf_scalefactors(&mut bits, &mut gr[g_i][ch], h, ch)?
                    } else {
                        scalefactors(&mut bits, &g, g_i, &sf_prev)?
                    };
                    let g = gr[g_i][ch];
                    huffman(
                        &mut bits,
                        &g,
                        end as usize,
                        h.sr_index,
                        &sf[ch],
                        &mut xr[ch],
                    )?;
                }
                pos = end;
            }
            if h.mode == 1 && nch == 2 {
                let [l, r] = &mut xr;
                stereo(h, &gr[g_i][1], &sf[1], l, r);
            }
            for ch in 0..nch {
                let g = gr[g_i][ch];
                reorder(&g, h.sr_index, &mut xr[ch]);
                antialias(&g, &mut xr[ch]);
                let mut sb = [[0f32; 32]; 18];
                imdct(&g, &xr[ch], &mut self.overlap[ch], &mut sb);
                for (t, s) in sb.iter().enumerate() {
                    let mut o = [0f32; 32];
                    synth(&mut self.v[ch], s, &mut o);
                    for (i, v) in o.iter().enumerate() {
                        pcm[(g_i * 576 + t * 32 + i) * nch + ch] = *v;
                    }
                }
            }
        }
        out.extend_from_slice(&pcm);
        Ok(ngr * 576)
    }
}

/// MPEG-1 scale factors; granule 1 may reuse granule 0's (`scfsi`).
fn scalefactors(b: &mut Bits, g: &Granule, gr: usize, prev: &[u8; 40]) -> Result<[u8; 40], Error> {
    let (s1, s2) = (
        u32::from(SLEN[0][g.scalefac_compress as usize]),
        u32::from(SLEN[1][g.scalefac_compress as usize]),
    );
    let mut sf = [0u8; 40];
    if g.block_type == 2 {
        let n1 = if g.mixed { 17 } else { 18 };
        for v in &mut sf[..n1] {
            *v = b.get(s1)? as u8;
        }
        for v in &mut sf[n1..n1 + 18] {
            *v = b.get(s2)? as u8;
        }
    } else {
        for (k, (from, to)) in [(0, 6), (6, 11), (11, 16), (16, 21)]
            .into_iter()
            .enumerate()
        {
            if gr == 1 && g.scfsi & (8 >> k) != 0 {
                sf[from..to].copy_from_slice(&prev[from..to]);
            } else {
                let s = if k < 2 { s1 } else { s2 };
                for v in &mut sf[from..to] {
                    *v = b.get(s)? as u8;
                }
            }
        }
    }
    Ok(sf)
}

/// MPEG-2 scale factors; the right channel of intensity stereo codes them
/// differently.
fn lsf_scalefactors(
    b: &mut Bits,
    g: &mut Granule,
    h: &Header,
    ch: usize,
) -> Result<[u8; 40], Error> {
    let kind = match (g.block_type, g.mixed) {
        (2, true) => 2,
        (2, false) => 1,
        _ => 0,
    };
    let mut c = g.scalefac_compress;
    // slen[3], slen[2], slen[1] split off by n3, n2, n1; slen[0] is the rest.
    let split = |c: &mut u32, n: [u32; 3]| -> [u32; 4] {
        let mut s = [0u32; 4];
        for (i, d) in [(3usize, n[2]), (2, n[1]), (1, n[0])] {
            if d != 0 {
                s[i] = *c % d;
                *c /= d;
            }
        }
        s[0] = *c;
        s
    };
    let (table, slen) = if h.mode_ext & 1 != 0 && ch == 1 {
        c >>= 1;
        if c < 180 {
            (3, split(&mut c, [6, 6, 0]))
        } else if c < 244 {
            c -= 180;
            (4, split(&mut c, [4, 4, 0]))
        } else {
            c -= 244;
            (5, split(&mut c, [3, 0, 0]))
        }
    } else if c < 400 {
        (0, split(&mut c, [5, 4, 4]))
    } else if c < 500 {
        c -= 400;
        (1, split(&mut c, [5, 4, 0]))
    } else {
        c -= 500;
        g.preflag = true;
        (2, split(&mut c, [3, 0, 0]))
    };
    let mut sf = [0u8; 40];
    let mut j = 0;
    for k in 0..4 {
        for _ in 0..LSF_NSF[table][kind][k] {
            if j < 40 {
                sf[j] = b.get(slen[k])? as u8;
                j += 1;
            }
        }
    }
    Ok(sf)
}

/// Huffman-coded values up to bit `end`, requantized into `xr` (short
/// blocks still window by window within each band).
fn huffman(
    b: &mut Bits,
    g: &Granule,
    end: usize,
    sr: usize,
    sf: &[u8; 40],
    xr: &mut [f32; 576],
) -> Result<(), Error> {
    let st = statics();
    // 2^(gain/4) per sample.
    let mut gain = [0f32; 576];
    let base = g.global_gain - 210;
    let shift = if g.scalefac_scale { 2 } else { 1 };
    let mut at = 0;
    for i in 0..g.long_end {
        let pre = if g.preflag { i32::from(PRETAB[i]) } else { 0 };
        let e = base - ((i32::from(sf[i]) + pre) << shift);
        let v = (f64::from(e) / 4.0).exp2() as f32;
        let w = usize::from(BAND_LONG[sr][i]);
        gain[at..(at + w).min(576)].fill(v);
        at += w;
    }
    let mut k = g.long_end;
    for i in g.short_start..13 {
        let w = usize::from(BAND_SHORT[sr][i]);
        for win in 0..3 {
            let e =
                base - 8 * i32::from(g.subblock_gain[win]) - (i32::from(sf[k.min(39)]) << shift);
            k += 1;
            let v = (f64::from(e) / 4.0).exp2() as f32;
            let to = (at + w).min(576);
            if at < to {
                gain[at..to].fill(v);
            }
            at += w;
        }
    }
    let value = |x: u32, sign: bool, i: usize| -> f32 {
        let m = st.pow43[x as usize] * gain[i];
        if sign { -m } else { m }
    };

    let mut s = 0usize;
    for r in 0..3 {
        let n = g.region[r];
        let (tree, linbits) = HUFF_DATA[usize::from(g.table_select[r])];
        if tree == 0 {
            s += 2 * n;
            continue;
        }
        let t = &st.big[usize::from(tree) - 1];
        for _ in 0..n {
            if b.pos() >= end || s + 1 >= 576 {
                break;
            }
            let sym = t.decode(b)?;
            let mut pair = [sym >> 4, sym & 15];
            for (j, v) in pair.iter_mut().enumerate() {
                if *v == 15 && linbits > 0 {
                    *v += b.get(u32::from(linbits))?;
                }
                if *v != 0 {
                    let neg = b.bit()? == 1;
                    xr[s + j] = value(*v, neg, s + j);
                }
            }
            s += 2;
        }
    }
    s = s.min(576);
    let q = &st.quad[g.count1_table];
    while s + 4 <= 576 && b.pos() < end {
        let code = q.decode(b)?;
        let mut vals = [0f32; 4];
        for (j, v) in vals.iter_mut().enumerate() {
            if code & (8 >> j) != 0 {
                let neg = b.bit()? == 1;
                *v = value(1, neg, s + j);
            }
        }
        // An encoder's part2_3_length can end inside the last quadruple;
        // that one is left out.
        if b.pos() > end {
            break;
        }
        xr[s..s + 4].copy_from_slice(&vals);
        s += 4;
    }
    Ok(())
}

/// Joint stereo of one granule: intensity stereo where the right channel's
/// upper bands are empty, M/S elsewhere (or everywhere).
fn stereo(h: &Header, g1: &Granule, sf1: &[u8; 40], l: &mut [f32; 576], r: &mut [f32; 576]) {
    let ms = h.mode_ext & 2 != 0;
    if h.mode_ext & 1 == 0 {
        if ms {
            for (a, b) in l.iter_mut().zip(r.iter_mut()) {
                (*a, *b) = (*a + *b, *a - *b);
            }
        }
        return;
    }
    let k2 = std::f32::consts::FRAC_1_SQRT_2;
    let ratio = |pos: u8| -> Option<(f32, f32)> {
        if h.lsf {
            let p = i32::from(pos);
            let e = -(i32::from((g1.scalefac_compress & 1) as u8) + 1) * ((p + 1) >> 1);
            let f = (f64::from(e) / 4.0).exp2() as f32;
            Some(if p & 1 == 1 { (f, 1.0) } else { (1.0, f) })
        } else if pos >= 7 {
            None
        } else {
            let t = (f64::from(pos) * PI / 12.0).tan();
            if pos == 6 {
                Some((1.0, 0.0))
            } else {
                Some(((t / (1.0 + t)) as f32, (1.0 / (1.0 + t)) as f32))
            }
        }
    };
    let band = |l: &mut [f32], r: &mut [f32], is: Option<(f32, f32)>| match is {
        Some((kl, kr)) => {
            for (a, b) in l.iter_mut().zip(r.iter_mut()) {
                let m = *a;
                *a = m * kl;
                *b = m * kr;
            }
        }
        None if ms => {
            for (a, b) in l.iter_mut().zip(r.iter_mut()) {
                (*a, *b) = ((*a + *b) * k2, (*a - *b) * k2);
            }
        }
        None => {}
    };
    let sr = h.sr_index;
    let mut pos = 576usize;
    let mut found = [false; 3];
    if g1.short_start < 13 {
        // Short bands from the top; the last band uses the one before's
        // scale factor.
        let mut k = (13 - g1.short_start) * 3 + g1.long_end;
        for i in (g1.short_start..13).rev() {
            if i != 11 {
                k -= 3;
            }
            let w = usize::from(BAND_SHORT[sr][i]);
            for win in (0..3).rev() {
                pos -= w;
                let is = if found[win] {
                    None
                } else if r[pos..pos + w].iter().any(|&v| v != 0.0) {
                    found[win] = true;
                    None
                } else {
                    ratio(sf1[(k + win).min(39)])
                };
                band(&mut l[pos..pos + w], &mut r[pos..pos + w], is);
            }
        }
    }
    let mut nz = found.iter().any(|&f| f);
    for i in (0..g1.long_end).rev() {
        let w = usize::from(BAND_LONG[sr][i]);
        pos -= w;
        let is = if nz {
            None
        } else if r[pos..pos + w].iter().any(|&v| v != 0.0) {
            nz = true;
            None
        } else {
            ratio(sf1[if i == 21 { 20 } else { i }])
        };
        band(&mut l[pos..pos + w], &mut r[pos..pos + w], is);
    }
}

/// Short bands from window-by-window to sample-interleaved order.
fn reorder(g: &Granule, sr: usize, xr: &mut [f32; 576]) {
    if g.block_type != 2 {
        return;
    }
    let mut at = if !g.mixed {
        0
    } else if sr == 8 {
        72
    } else {
        36
    };
    let mut tmp = [0f32; 576];
    for i in g.short_start..13 {
        let w = usize::from(BAND_SHORT[sr][i]);
        if at + 3 * w > 576 {
            break;
        }
        for j in 0..w {
            for win in 0..3 {
                tmp[3 * j + win] = xr[at + win * w + j];
            }
        }
        xr[at..at + 3 * w].copy_from_slice(&tmp[..3 * w]);
        at += 3 * w;
    }
}

fn antialias(g: &Granule, xr: &mut [f32; 576]) {
    let n = match (g.block_type, g.mixed) {
        (2, false) => return,
        (2, true) => 1,
        _ => 31,
    };
    let st = statics();
    for sb in 1..=n {
        let at = 18 * sb;
        for (j, &(cs, ca)) in st.alias.iter().enumerate() {
            let (a, b) = (xr[at - 1 - j], xr[at + j]);
            xr[at - 1 - j] = a * cs - b * ca;
            xr[at + j] = b * cs + a * ca;
        }
    }
}

/// IMDCT, windowing and overlap of each subband, frequency inversion of
/// the odd ones: `out[time][subband]`.
fn imdct(g: &Granule, xr: &[f32; 576], overlap: &mut [f32; 576], out: &mut [[f32; 32]; 18]) {
    let st = statics();
    for sb in 0..32 {
        let x = &xr[sb * 18..sb * 18 + 18];
        let bt = if g.mixed && sb < 2 {
            0
        } else {
            usize::from(g.block_type)
        };
        let mut y = [0f32; 36];
        if bt == 2 {
            for win in 0..3 {
                for i in 0..12 {
                    let c = &st.cos12[i];
                    let v: f32 = (0..6).map(|k| x[3 * k + win] * c[k]).sum();
                    y[6 + 6 * win + i] += v * st.win[2][i];
                }
            }
        } else {
            for (i, yi) in y.iter_mut().enumerate() {
                let c = &st.cos36[i];
                let v: f32 = x.iter().zip(c).map(|(a, b)| a * b).sum();
                *yi = v * st.win[bt][i];
            }
        }
        let o = &mut overlap[sb * 18..sb * 18 + 18];
        for i in 0..18 {
            let mut v = y[i] + o[i];
            if sb & 1 == 1 && i & 1 == 1 {
                v = -v;
            }
            out[i][sb] = v;
            o[i] = y[18 + i];
        }
    }
}

/// Polyphase synthesis of one time slot (ISO/IEC 11172-3 Annex A.2).
fn synth(v: &mut [f32], s: &[f32; 32], out: &mut [f32; 32]) {
    let st = statics();
    v.copy_within(0..960, 64);
    for (i, row) in st.matrix.iter().enumerate() {
        v[i] = row.iter().zip(s).map(|(a, b)| a * b).sum();
    }
    for (j, o) in out.iter_mut().enumerate() {
        let mut sum = 0f32;
        for i in 0..8 {
            sum += v[i * 128 + j] * st.window[i * 64 + j];
            sum += v[i * 128 + 96 + j] * st.window[i * 64 + 32 + j];
        }
        *o = sum;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers() {
        // MPEG-1 Layer III, 128 kbit/s, 44.1 kHz, joint stereo.
        let h = header(&[0xff, 0xfb, 0x90, 0x64]).unwrap();
        assert_eq!(
            (h.lsf, h.sample_rate, h.len, h.channels),
            (false, 44100, 417, 2)
        );
        assert_eq!(h.mode, 1);
        // MPEG-2, 64 kbit/s, 24 kHz, mono.
        let h = header(&[0xff, 0xf3, 0x84, 0xc4]).unwrap();
        assert_eq!(
            (h.lsf, h.sample_rate, h.len, h.channels),
            (true, 24000, 192, 1)
        );
        assert!(header(&[0xff, 0xfd, 0x90, 0x64]).is_err(), "layer II");
        assert!(header(&[0xff, 0xfb, 0x00, 0x64]).is_err(), "free format");
    }

    #[test]
    fn trees_are_complete() {
        let st = statics();
        assert_eq!(st.big.len(), 15);
        // Every symbol decodes from its own code.
        let mut at = 0;
        for (t, &n) in HUFF_SIZES.iter().enumerate() {
            let mut code = 0u64;
            for i in at..at + n {
                let len = u32::from(HUFF_LENS[i]);
                let c = (code >> (32 - len)) as u32;
                let bytes = (c << (32 - len)).to_be_bytes();
                let mut b = Bits::new(&bytes);
                assert_eq!(st.big[t].decode(&mut b).unwrap(), u32::from(HUFF_SYMS[i]));
                assert_eq!(b.pos(), len as usize);
                code += 1u64 << (32 - len);
            }
            assert_eq!(code, 1 << 32, "tree {t} fills the code space");
            at += n;
        }
    }

    #[test]
    fn damaged_frames_do_not_panic() {
        let mut d = Decoder::new();
        let mut out = Vec::new();
        let mut seed = 0x1234_5678u32;
        for len in [4usize, 10, 40, 417, 1000] {
            for _ in 0..200 {
                let mut f: Vec<u8> = (0..len)
                    .map(|_| {
                        seed ^= seed << 13;
                        seed ^= seed >> 17;
                        seed ^= seed << 5;
                        seed as u8
                    })
                    .collect();
                if len >= 4 {
                    f[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0x64]);
                }
                let _ = d.decode(&f, &mut out);
            }
        }
    }
}
