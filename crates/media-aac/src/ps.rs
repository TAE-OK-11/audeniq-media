//! Parametric stereo (ISO/IEC 14496-3 subclause 8.6.4), HE-AAC v2's tool
//! that makes a stereo pair from the mono SBR signal and a few parameters
//! per frequency band, written from the standard's description:
//!
//! - `ps_data`: inter-channel intensity differences (IID) and coherence
//!   (ICC), optional phase differences (IPD, OPD), in 10, 20 or 34
//!   parameter bands, delta coded in frequency or time, per envelope;
//! - a hybrid filter bank that splits the lowest QMF subbands further
//!   (20-band configuration: 10 hybrid bands from QMF 0–2; 34-band: 32
//!   from QMF 0–4), centered on each slot with 6 slots of look-ahead;
//! - a decorrelated signal: all-pass filters with fractional delays in
//!   the lower bands, plain delays above, attenuated at transients;
//! - per band and slot a 2×2 mixing of the mono and decorrelated signals,
//!   interpolated between envelopes, then hybrid synthesis.
//!
//! Tables (`ps_tables.rs`) are the standard's data.
#![allow(clippy::excessive_precision)]
use crate::ps_tables as tab;
use crate::sbr::Qmf;
use media_bits::{Bits, Huffman as Tree};
use std::f32::consts::PI;
use std::sync::OnceLock;

const SLOTS: usize = 32;
/// Parameter envelopes per frame, with the one appended up to the end.
const MAX_ENV: usize = 5;
const MAX_PAR: usize = 34;

/// Huffman tables (by [`tab::HUFF`] index).
const IID_DF_FINE: usize = 0;
const IID_DT_FINE: usize = 1;
const IID_DF: usize = 2;
const IID_DT: usize = 3;
const ICC_DF: usize = 4;
const ICC_DT: usize = 5;
const IPD_DF: usize = 6;
const IPD_DT: usize = 7;
const OPD_DF: usize = 8;
const OPD_DT: usize = 9;

/// IID steps in dB: coarse (index −7..=7) and fine (−15..=15).
const IID_COARSE: [f32; 15] = [
    -25.0, -18.0, -14.0, -10.0, -7.0, -4.0, -2.0, 0.0, 2.0, 4.0, 7.0, 10.0, 14.0, 18.0, 25.0,
];
const IID_FINE: [f32; 31] = [
    -50.0, -45.0, -40.0, -35.0, -30.0, -25.0, -22.0, -19.0, -16.0, -13.0, -10.0, -8.0, -6.0, -4.0,
    -2.0, 0.0, 2.0, 4.0, 6.0, 8.0, 10.0, 13.0, 16.0, 19.0, 22.0, 25.0, 30.0, 35.0, 40.0, 45.0,
    50.0,
];
/// ICC index → correlation ρ.
const ICC_RHO: [f32; 8] = [1.0, 0.937, 0.84118, 0.60092, 0.36764, 0.0, -0.589, -1.0];

/// Hybrid prototype filters (first 7 of 13 symmetric taps).
const G20_2: [f32; 7] = [
    0.0,
    0.018_994_875,
    0.0,
    -0.072_931_39,
    0.0,
    0.305_966_3,
    0.5,
];
const G20_8: [f32; 7] = [
    0.007_460_829_5,
    0.022_704_21,
    0.045_468_66,
    0.072_661_14,
    0.098_851_09,
    0.117_937_1,
    0.125,
];
const G34_12: [f32; 7] = [
    0.040_811_8,
    0.038_128_11,
    0.051_449_08,
    0.063_998_31,
    0.074_283_14,
    0.081_003_48,
    0.083_333_33,
];
const G34_8: [f32; 7] = [
    0.015_656_756,
    0.037_527_164,
    0.054_178_914,
    0.084_170_44,
    0.103_073_44,
    0.122_224_52,
    0.125,
];
const G34_4: [f32; 7] = [
    -0.059_082_11,
    -0.048_714_98,
    0.0,
    0.077_787_24,
    0.164_863_03,
    0.232_798_57,
    0.25,
];

/// All-pass decorrelator: link delays, fractional-delay factors and the
/// filter coefficients; the overall fractional delay; the decay of the
/// filter gain above the cutoff band.
const LINK_DELAY: [usize; 3] = [3, 4, 5];
const LINK_FRACTION: [f32; 3] = [0.43, 0.75, 0.347];
const ALLPASS_A: [f32; 3] = [0.651_439_06, 0.564_718_1, 0.489_541_66];
const PHI_FRACTION: f32 = 0.39;
const DECAY_SLOPE: f32 = 0.05;
/// Transient detection: peak decay, smoothing and threshold.
const PEAK_DECAY: f32 = 0.765_928_34;
const SMOOTHING: f32 = 0.25;
const TRANSIENT_IMPACT: f32 = 1.5;

/// The two hybrid configurations (20 and 34 parameter bands).
struct Config {
    /// Hybrid bands in all (the split low ones, then the other QMF ones).
    bands: usize,
    /// QMF subbands split by the hybrid filters.
    split: usize,
    /// Hybrid bands from the split subbands.
    split_bands: usize,
    parameters: usize,
    ipd_parameters: usize,
    /// Bands with all-pass decorrelation; below `short` a 14-slot delay,
    /// above a 1-slot one.
    allpass: usize,
    short: usize,
    /// Parameter band from which the all-pass gain decays.
    decay_cutoff: usize,
    k_to_i: &'static [usize],
}

const CONFIG_20: Config = Config {
    bands: 71,
    split: 3,
    split_bands: 10,
    parameters: 20,
    ipd_parameters: 11,
    allpass: 30,
    short: 42,
    decay_cutoff: 10,
    k_to_i: &tab::K_TO_I_20,
};
const CONFIG_34: Config = Config {
    bands: 91,
    split: 5,
    split_bands: 32,
    parameters: 34,
    ipd_parameters: 17,
    allpass: 50,
    short: 62,
    decay_cutoff: 32,
    k_to_i: &tab::K_TO_I_34,
};

fn trees() -> &'static [Tree] {
    static T: OnceLock<Vec<Tree>> = OnceLock::new();
    T.get_or_init(|| tab::HUFF.iter().map(|(c, l)| Tree::new(c, l)).collect())
}

fn huff(b: &mut Bits, table: usize) -> Result<i32, media_bits::Error> {
    Ok(trees()[table].decode(b)? as i32 - tab::HUFF_OFFSET[table])
}

type Pars = [[i8; MAX_PAR]; MAX_ENV];

/// One frame's parameters as coded.
#[derive(Clone, Copy)]
struct Params {
    envelopes: usize,
    /// Last slot of each envelope.
    borders: [usize; MAX_ENV],
    iid: Pars,
    icc: Pars,
    ipd: Pars,
    opd: Pars,
}

impl Default for Params {
    fn default() -> Params {
        Params {
            envelopes: 0,
            borders: [0; MAX_ENV],
            iid: [[0; MAX_PAR]; MAX_ENV],
            icc: [[0; MAX_PAR]; MAX_ENV],
            ipd: [[0; MAX_PAR]; MAX_ENV],
            opd: [[0; MAX_PAR]; MAX_ENV],
        }
    }
}

/// The PS header state (kept until a frame brings a new one).
#[derive(Clone, Copy, Default)]
struct Header {
    iid: Option<usize>,
    icc: Option<usize>,
    ext: bool,
}

/// Parameter bands of an IID/ICC mode (0..=5).
fn par_count(mode: usize) -> usize {
    [10, 20, 34][mode % 3]
}

/// Complex numbers as (re, im).
type C = [f32; 2];

#[inline(always)]
fn mul(a: C, b: C) -> C {
    [a[0] * b[0] - a[1] * b[1], a[0] * b[1] + a[1] * b[0]]
}

#[inline(always)]
fn expj(phase: f32) -> C {
    [phase.cos(), phase.sin()]
}

/// The mixing matrix of one parameter band: H11, H12, H21, H22.
type Mix = [C; 4];

pub(crate) struct Ps {
    header: Option<Header>,
    params: Option<Params>,
    /// Parameters of the last envelope (time-delta reference), as coded.
    last_iid: [i8; MAX_PAR],
    last_icc: [i8; MAX_PAR],
    last_ipd: [i8; MAX_PAR],
    last_opd: [i8; MAX_PAR],
    /// The configuration in use (34 bands) and its state.
    wide: bool,
    /// Split QMF subbands' last 12 slots, [band][slot].
    split_history: [[C; 12]; 5],
    /// Decorrelator input history per hybrid band (14 slots).
    delay: Box<[[C; 14]; 91]>,
    /// All-pass link states per band: up to 5 past values per link.
    links: Box<[[[C; 5]; 3]; 50]>,
    peak: [f32; MAX_PAR],
    power: [f32; MAX_PAR],
    peak_diff: [f32; MAX_PAR],
    /// Mixing at the end of the last frame, per parameter band.
    mix_prev: [Mix; MAX_PAR],
    /// IPD/OPD of the last two envelopes per band (as indices).
    ipd_hist: [[i8; 2]; MAX_PAR],
    opd_hist: [[i8; 2]; MAX_PAR],
}

impl Ps {
    pub(crate) fn new() -> Ps {
        Ps {
            header: None,
            params: None,
            last_iid: [0; MAX_PAR],
            last_icc: [0; MAX_PAR],
            last_ipd: [0; MAX_PAR],
            last_opd: [0; MAX_PAR],
            wide: false,
            split_history: [[[0.0; 2]; 12]; 5],
            delay: Box::new([[[0.0; 2]; 14]; 91]),
            links: Box::new([[[[0.0; 2]; 5]; 3]; 50]),
            peak: [0.0; MAX_PAR],
            power: [0.0; MAX_PAR],
            peak_diff: [0.0; MAX_PAR],
            mix_prev: [identity(); MAX_PAR],
            ipd_hist: [[0; 2]; MAX_PAR],
            opd_hist: [[0; 2]; MAX_PAR],
        }
    }

    /// Whether PS data has arrived (the output is stereo from then on).
    pub(crate) fn started(&self) -> bool {
        self.header.is_some()
    }

    /// `ps_data` within `bits_left` bits at `b`, which it passes; returns
    /// the bits used (all of them when the data is bad, which leaves the
    /// frame without new parameters).
    pub(crate) fn read(&mut self, b: &mut Bits, bits_left: usize) -> usize {
        let start = b.pos();
        let mut sub = b.take(bits_left);
        let used = match self.parse(&mut sub) {
            Ok(()) if sub.pos() - start <= bits_left => sub.pos() - start,
            _ => {
                self.params = None;
                bits_left
            }
        };
        let _ = b.skip(used);
        used
    }

    fn parse(&mut self, b: &mut Bits) -> Result<(), media_bits::Error> {
        let mut h = self.header.unwrap_or_default();
        if b.get(1)? == 1 {
            let mode = |b: &mut Bits| -> Result<Option<usize>, media_bits::Error> {
                if b.get(1)? == 0 {
                    return Ok(None);
                }
                let m = b.get(3)? as usize;
                if m > 5 {
                    return Err(media_bits::Error::Invalid("PS mode"));
                }
                Ok(Some(m))
            };
            h.iid = mode(b)?;
            h.icc = mode(b)?;
            h.ext = b.get(1)? == 1;
        } else if self.header.is_none() {
            // Parameters without a header to read them by.
            return Err(media_bits::Error::Invalid("PS header"));
        }
        let variable = b.get(1)? == 1;
        let index = b.get(2)? as usize;
        let envelopes = if variable {
            index + 1
        } else {
            [0, 1, 2, 4][index]
        };
        let mut p = Params {
            envelopes,
            ..Params::default()
        };
        if variable {
            for e in 0..envelopes {
                p.borders[e] = b.get(5)? as usize;
            }
        }
        if let Some(mode) = h.iid {
            let fine = mode >= 3;
            let (df, dt, limit) = if fine {
                (IID_DF_FINE, IID_DT_FINE, 15)
            } else {
                (IID_DF, IID_DT, 7)
            };
            let n = par_count(mode);
            for e in 0..envelopes {
                let prev = if e == 0 { self.last_iid } else { p.iid[e - 1] };
                read_par(b, &mut p.iid[e], &prev, n, df, dt, |v| v.abs() <= limit)?;
            }
        }
        if let Some(mode) = h.icc {
            let n = par_count(mode);
            for e in 0..envelopes {
                let prev = if e == 0 { self.last_icc } else { p.icc[e - 1] };
                read_par(b, &mut p.icc[e], &prev, n, ICC_DF, ICC_DT, |v| {
                    (0..=7).contains(&v)
                })?;
            }
        }
        if h.ext {
            let mut size = b.get(4)? as usize;
            if size == 15 {
                size += b.get(8)? as usize;
            }
            let mut left = 8 * size;
            while left > 7 {
                let id = b.get(2)?;
                left -= 2;
                let start = b.pos();
                if id == 0 && b.get(1)? == 1 {
                    // IPD/OPD in the first parameter bands.
                    let n = [5, 11, 17][h.iid.unwrap_or(1) % 3];
                    for e in 0..envelopes {
                        let prev = if e == 0 { self.last_ipd } else { p.ipd[e - 1] };
                        read_par(b, &mut p.ipd[e], &prev, n, IPD_DF, IPD_DT, |_| true)?;
                        let prev = if e == 0 { self.last_opd } else { p.opd[e - 1] };
                        read_par(b, &mut p.opd[e], &prev, n, OPD_DF, OPD_DT, |_| true)?;
                    }
                    for e in 0..envelopes {
                        for v in p.ipd[e].iter_mut().chain(p.opd[e].iter_mut()) {
                            *v &= 7;
                        }
                    }
                    b.skip(1)?; // reserved
                }
                let used = b.pos() - start;
                if used > left {
                    return Err(media_bits::Error::Invalid("PS extension"));
                }
                b.skip(left - used)?;
                left = 0;
            }
            b.skip(left)?;
        }
        // Envelope ends: even, or as coded up to slot 31 (an envelope with
        // the last parameters is added up to it); none keeps the last.
        if envelopes == 0 {
            p.envelopes = 1;
            p.borders[0] = SLOTS - 1;
            p.iid[0] = self.last_iid;
            p.icc[0] = self.last_icc;
            p.ipd[0] = self.last_ipd;
            p.opd[0] = self.last_opd;
        } else if !variable {
            for e in 0..envelopes {
                p.borders[e] = (e + 1) * SLOTS / envelopes - 1;
            }
        } else {
            for e in 1..envelopes {
                if p.borders[e] <= p.borders[e - 1] {
                    p.borders[e] = (p.borders[e - 1] + 1).min(SLOTS - 1);
                }
            }
            if p.borders[envelopes - 1] < SLOTS - 1 {
                let e = envelopes;
                p.envelopes = e + 1;
                p.borders[e] = SLOTS - 1;
                p.iid[e] = p.iid[e - 1];
                p.icc[e] = p.icc[e - 1];
                p.ipd[e] = p.ipd[e - 1];
                p.opd[e] = p.opd[e - 1];
            }
        }
        let last = p.envelopes - 1;
        self.last_iid = p.iid[last];
        self.last_icc = p.icc[last];
        self.last_ipd = p.ipd[last];
        self.last_opd = p.opd[last];
        self.header = Some(h);
        self.params = Some(p);
        Ok(())
    }
}

/// One envelope's parameters: frequency deltas from 0, or time deltas
/// from `prev`; each value checked by `valid`.
fn read_par(
    b: &mut Bits,
    out: &mut [i8; MAX_PAR],
    prev: &[i8; MAX_PAR],
    n: usize,
    df: usize,
    dt: usize,
    valid: impl Fn(i32) -> bool,
) -> Result<(), media_bits::Error> {
    let time = b.get(1)? == 1;
    let mut v = 0i32;
    for k in 0..n {
        v = if time {
            i32::from(prev[k]) + huff(b, dt)?
        } else {
            v + huff(b, df)?
        };
        // IPD/OPD wrap (the caller masks them); others must be in range.
        if !valid(v) {
            return Err(media_bits::Error::Invalid("PS parameter"));
        }
        out[k] = v as i8;
    }
    Ok(())
}

fn identity() -> Mix {
    [[1.0, 0.0], [1.0, 0.0], [0.0, 0.0], [0.0, 0.0]]
}

/// The hybrid band split of the split QMF subbands: per subband the
/// filter, its band count and whether it is real (2 bands), and the
/// order of its outputs among the hybrid bands, which sums some.
struct Split {
    taps: &'static [f32; 7],
    bands: usize,
    real: bool,
    /// Hybrid band of each output (outputs of one band are summed).
    to: &'static [usize],
}

const SPLIT_20: [Split; 3] = [
    // QMF 0: 8 complex bands; the ±5/16 and ±7/16 pairs are summed.
    Split {
        taps: &G20_8,
        bands: 8,
        real: false,
        to: &[2, 3, 4, 5, 5, 4, 0, 1],
    },
    Split {
        taps: &G20_2,
        bands: 2,
        real: true,
        to: &[7, 6],
    },
    Split {
        taps: &G20_2,
        bands: 2,
        real: true,
        to: &[8, 9],
    },
];

const SPLIT_34: [Split; 5] = [
    Split {
        taps: &G34_12,
        bands: 12,
        real: false,
        to: &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
    },
    Split {
        taps: &G34_8,
        bands: 8,
        real: false,
        to: &[12, 13, 14, 15, 16, 17, 18, 19],
    },
    Split {
        taps: &G34_4,
        bands: 4,
        real: false,
        to: &[20, 21, 22, 23],
    },
    Split {
        taps: &G34_4,
        bands: 4,
        real: false,
        to: &[24, 25, 26, 27],
    },
    Split {
        taps: &G34_4,
        bands: 4,
        real: false,
        to: &[28, 29, 30, 31],
    },
];

/// Hybrid bands per QMF subband among the split ones.
fn split_of(wide: bool) -> &'static [Split] {
    if wide { &SPLIT_34 } else { &SPLIT_20 }
}

/// Center frequency of hybrid band `k` in QMF subband units (QMF band q
/// spans q..q + 1; the split bands' centers are the standard's).
fn center(wide: bool, k: usize) -> f32 {
    if wide {
        const F34: [f32; 32] = [
            2.0, 6.0, 10.0, 14.0, 18.0, 22.0, 26.0, 30.0, 34.0, -10.0, -6.0, -2.0, 51.0, 57.0,
            15.0, 21.0, 27.0, 33.0, 39.0, 45.0, 54.0, 66.0, 78.0, 42.0, 102.0, 66.0, 78.0, 90.0,
            102.0, 114.0, 126.0, 90.0,
        ];
        if k < 32 {
            F34[k] / 24.0
        } else {
            k as f32 - 26.5
        }
    } else {
        const F20: [f32; 10] = [-3.0, -1.0, 1.0, 3.0, 5.0, 7.0, 10.0, 14.0, 18.0, 22.0];
        if k < 10 { F20[k] / 8.0 } else { k as f32 - 6.5 }
    }
}

/// Whether hybrid band `k` holds negative frequencies (its phases are
/// mirrored).
fn negative(wide: bool, k: usize) -> bool {
    if wide { (9..12).contains(&k) } else { k < 2 }
}

/// 10 → 20 parameter bands (each doubled).
fn map_10_to_20(p: &[i8; MAX_PAR]) -> [i8; MAX_PAR] {
    let mut out = [0; MAX_PAR];
    for b in 0..10 {
        out[2 * b] = p[b];
        out[2 * b + 1] = p[b];
    }
    out
}

/// 20 → 34 parameter bands; `average` for the two bands between two
/// coarse ones (IID, ICC), else the lower one (phases).
fn map_20_to_34(p: &[i8; MAX_PAR], average: bool) -> [i8; MAX_PAR] {
    const FROM: [usize; 34] = [
        0, 0, 1, 2, 2, 3, 4, 4, 5, 5, 6, 7, 8, 8, 9, 9, 10, 11, 12, 13, 14, 14, 15, 15, 16, 16, 17,
        17, 18, 18, 18, 18, 19, 19,
    ];
    let mut out = [0; MAX_PAR];
    for (j, &i) in FROM.iter().enumerate() {
        out[j] = p[i];
    }
    if average {
        out[1] = ((i32::from(p[0]) + i32::from(p[1])) / 2) as i8;
        out[4] = ((i32::from(p[2]) + i32::from(p[3])) / 2) as i8;
    }
    out
}

/// Parameters coded in `n` bands at the configuration's resolution.
fn to_config(p: &[i8; MAX_PAR], n: usize, wide: bool, average: bool) -> [i8; MAX_PAR] {
    let p20 = if n <= 10 { map_10_to_20(p) } else { *p };
    if wide && n != 34 {
        map_20_to_34(&p20, average)
    } else {
        p20
    }
}

impl Ps {
    fn reset_state(&mut self) {
        self.split_history = [[[0.0; 2]; 12]; 5];
        *self.delay = [[[0.0; 2]; 14]; 91];
        *self.links = [[[[0.0; 2]; 5]; 3]; 50];
        self.peak = [0.0; MAX_PAR];
        self.power = [0.0; MAX_PAR];
        self.peak_diff = [0.0; MAX_PAR];
        self.mix_prev = [identity(); MAX_PAR];
        self.ipd_hist = [[0; 2]; MAX_PAR];
        self.opd_hist = [[0; 2]; MAX_PAR];
    }

    /// Stereo QMF frames `l`, `r` from the mono frame `x`; `ahead` holds
    /// the lowest five subbands 6 slots later (the hybrid filters' look-
    /// ahead, from the SBR analysis buffer), so nothing is delayed.
    pub(crate) fn apply(&mut self, x: &Qmf, ahead: &[[C; SLOTS]; 5], l: &mut Qmf, r: &mut Qmf) {
        let Some(h) = self.header else {
            *l = *x;
            *r = *x;
            return;
        };
        // A frame without data keeps the last parameters.
        let p = self.params.take().unwrap_or_else(|| {
            let mut p = Params {
                envelopes: 1,
                ..Params::default()
            };
            p.borders[0] = SLOTS - 1;
            p.iid[0] = self.last_iid;
            p.icc[0] = self.last_icc;
            p.ipd[0] = self.last_ipd;
            p.opd[0] = self.last_opd;
            p
        });
        let wide =
            h.iid.is_some_and(|m| par_count(m) == 34) || h.icc.is_some_and(|m| par_count(m) == 34);
        if wide != self.wide {
            // A new configuration starts its filters afresh; the mixing
            // interpolates on from the last matrices at the new resolution.
            let prev = self.mix_prev;
            self.reset_state();
            self.mix_prev = remap_mix(&prev, wide);
            self.wide = wide;
        }
        let cfg = if wide { &CONFIG_34 } else { &CONFIG_20 };
        let mut s = Box::new([[[0f32; 2]; SLOTS]; 91]);
        self.analysis(x, ahead, cfg, &mut s);
        let mut d = Box::new([[[0f32; 2]; SLOTS]; 91]);
        self.decorrelate(&s, cfg, &mut d);
        let mut hl = Box::new([[[0f32; 2]; SLOTS]; 91]);
        let mut hr = Box::new([[[0f32; 2]; SLOTS]; 91]);
        self.mix(&h, &p, cfg, &s, &d, &mut hl, &mut hr);
        synthesis(&hl, cfg, wide, l);
        synthesis(&hr, cfg, wide, r);
    }

    /// Hybrid analysis: the split subbands through their filters (centered
    /// on the slot, with the look-ahead), the others as they are.
    fn analysis(
        &mut self,
        x: &Qmf,
        ahead: &[[C; SLOTS]; 5],
        cfg: &Config,
        s: &mut [[C; SLOTS]; 91],
    ) {
        let wide = self.wide;
        for (q, sp) in split_of(wide).iter().enumerate() {
            let mut z = [[0f32; 2]; 12 + SLOTS];
            z[..12].copy_from_slice(&self.split_history[q]);
            z[12..].copy_from_slice(&ahead[q]);
            self.split_history[q].copy_from_slice(&z[SLOTS..]);
            let g = |t: usize| sp.taps[t.min(12 - t)];
            for b in 0..sp.bands {
                // Taps of band b: g(t)·e^{j2π(b+½)(t−6)/Q}, or the cosine.
                let mut c = [[0f32; 2]; 13];
                for (t, ct) in c.iter_mut().enumerate() {
                    let tt = t as f32 - 6.0;
                    *ct = if sp.real {
                        [g(t) * (PI * b as f32 * tt).cos(), 0.0]
                    } else {
                        let a = 2.0 * PI * (b as f32 + 0.5) * tt / sp.bands as f32;
                        [g(t) * a.cos(), g(t) * a.sin()]
                    };
                }
                let out = &mut s[sp.to[b]];
                for n in 0..SLOTS {
                    let mut acc = [0f32; 2];
                    for (t, ct) in c.iter().enumerate() {
                        let v = mul(*ct, z[n + 12 - t]);
                        acc[0] += v[0];
                        acc[1] += v[1];
                    }
                    out[n][0] += acc[0];
                    out[n][1] += acc[1];
                }
            }
        }
        for q in cfg.split..64 {
            let k = cfg.split_bands + q - cfg.split;
            for n in 0..SLOTS {
                s[k][n] = [x[0][n][q], x[1][n][q]];
            }
        }
    }

    /// The decorrelated signal: all-pass chains in the lower bands, delays
    /// of 14 and 1 slots above, attenuated at transients.
    fn decorrelate(&mut self, s: &[[C; SLOTS]; 91], cfg: &Config, d: &mut [[C; SLOTS]; 91]) {
        let wide = self.wide;
        // Transient attenuation per parameter band and slot: the smoothed
        // power against the smoothed drop below its decaying peak.
        let mut gain = [[1f32; SLOTS]; MAX_PAR];
        for n in 0..SLOTS {
            let mut power = [0f32; MAX_PAR];
            for k in 0..cfg.bands {
                let v = s[k][n];
                power[cfg.k_to_i[k]] += v[0] * v[0] + v[1] * v[1];
            }
            for i in 0..cfg.parameters {
                self.peak[i] = (self.peak[i] * PEAK_DECAY).max(power[i]);
                self.power[i] += SMOOTHING * (power[i] - self.power[i]);
                self.peak_diff[i] += SMOOTHING * (self.peak[i] - power[i] - self.peak_diff[i]);
                let limit = TRANSIENT_IMPACT * self.peak_diff[i];
                gain[i][n] = if limit > self.power[i] {
                    self.power[i] / limit
                } else {
                    1.0
                };
            }
        }
        for k in 0..cfg.bands {
            let mut ext = [[0f32; 2]; 14 + SLOTS];
            ext[..14].copy_from_slice(&self.delay[k]);
            ext[14..].copy_from_slice(&s[k]);
            self.delay[k].copy_from_slice(&ext[SLOTS..]);
            let out = &mut d[k];
            if k < cfg.allpass {
                // z⁻² with a fractional delay, then three all-pass links
                // (Q z^-d − a) / (1 − a Q z^-d) whose gains decay above the
                // cutoff band.
                let f = center(wide, k);
                let decay = if k < cfg.decay_cutoff {
                    1.0
                } else {
                    (1.0 - DECAY_SLOPE * (k - cfg.decay_cutoff) as f32).max(0.0)
                };
                let phi = expj(-PI * PHI_FRACTION * f);
                let q: [C; 3] = std::array::from_fn(|m| expj(-PI * LINK_FRACTION[m] * f));
                let mut w = [[[0f32; 2]; 5 + SLOTS]; 3];
                for m in 0..3 {
                    w[m][..5].copy_from_slice(&self.links[k][m]);
                }
                for n in 0..SLOTS {
                    let mut u = mul(phi, ext[14 + n - 2]);
                    for m in 0..3 {
                        let a = ALLPASS_A[m] * decay;
                        let past = mul(q[m], w[m][5 + n - LINK_DELAY[m]]);
                        let wn = [u[0] + a * past[0], u[1] + a * past[1]];
                        w[m][5 + n] = wn;
                        u = [past[0] - a * wn[0], past[1] - a * wn[1]];
                    }
                    out[n] = u;
                }
                for m in 0..3 {
                    self.links[k][m].copy_from_slice(&w[m][SLOTS..]);
                }
            } else {
                let delay = if k < cfg.short { 14 } else { 1 };
                for n in 0..SLOTS {
                    out[n] = ext[14 + n - delay];
                }
            }
            let g = &gain[cfg.k_to_i[k]];
            for n in 0..SLOTS {
                out[n][0] *= g[n];
                out[n][1] *= g[n];
            }
        }
    }

    /// The mixing matrix of each parameter band per envelope, interpolated
    /// over the envelope's slots, applied to the mono and decorrelated
    /// signals.
    #[allow(clippy::too_many_arguments)]
    fn mix(
        &mut self,
        h: &Header,
        p: &Params,
        cfg: &Config,
        s: &[[C; SLOTS]; 91],
        d: &[[C; SLOTS]; 91],
        l: &mut [[C; SLOTS]; 91],
        r: &mut [[C; SLOTS]; 91],
    ) {
        let wide = self.wide;
        let iid_n = h.iid.map_or(10, par_count);
        let icc_n = h.icc.map_or(10, par_count);
        let ipd_n = [5, 11, 17][h.iid.unwrap_or(1) % 3];
        let fine = h.iid.is_some_and(|m| m >= 3);
        let rotate = h.icc.is_some_and(|m| m >= 3);
        let mut start = 0usize;
        for e in 0..p.envelopes {
            let iid = to_config(&p.iid[e], iid_n, wide, true);
            let icc = to_config(&p.icc[e], icc_n, wide, true);
            // 5, 11 or 17 phase bands are the first of 10, 20 or 34.
            let ipd_as = match ipd_n {
                5 => 10,
                11 => 20,
                _ => 34,
            };
            let ipd = to_config(&p.ipd[e], ipd_as, wide, false);
            let opd = to_config(&p.opd[e], ipd_as, wide, false);
            let mut target = [identity(); MAX_PAR];
            for b in 0..cfg.parameters {
                let db = if fine {
                    IID_FINE[(i32::from(iid[b]) + 15) as usize]
                } else {
                    IID_COARSE[(i32::from(iid[b]) + 7) as usize]
                };
                let c = 10f32.powf(db / 20.0);
                let rho = ICC_RHO[icc[b] as usize];
                let [h11, h12, h21, h22] = if rotate {
                    let mu = c + 1.0 / c;
                    let mu = (1.0 + (4.0 * rho * rho - 4.0) / (mu * mu)).max(0.0).sqrt();
                    let gamma = ((1.0 - mu) / (1.0 + mu)).max(0.0).sqrt().atan();
                    let mut alpha = 0.5 * (2.0 * c * rho).atan2(c * c - 1.0);
                    if alpha < 0.0 {
                        alpha += PI / 2.0;
                    }
                    let (sa, ca) = alpha.sin_cos();
                    let (sg, cg) = gamma.sin_cos();
                    let r2 = std::f32::consts::SQRT_2;
                    [r2 * ca * cg, r2 * sa * cg, -r2 * sa * sg, r2 * ca * sg]
                } else {
                    let c1 = (2.0 / (1.0 + c * c)).sqrt();
                    let c2 = c * c1;
                    let alpha = 0.5 * rho.clamp(-1.0, 1.0).acos();
                    let beta = alpha * (c1 - c2) / std::f32::consts::SQRT_2;
                    [
                        c2 * (beta + alpha).cos(),
                        c1 * (beta - alpha).cos(),
                        c2 * (beta + alpha).sin(),
                        c1 * (beta - alpha).sin(),
                    ]
                };
                let mut m = [[h11, 0.0], [h12, 0.0], [h21, 0.0], [h22, 0.0]];
                if h.ext && b < cfg.ipd_parameters {
                    // Phases smoothed over this and the last two envelopes.
                    let smooth = |now: i8, hist: [i8; 2]| {
                        let ph = |v: i8| f32::from(v) * PI / 4.0;
                        let (a, b0, b1) = (expj(ph(now)), expj(ph(hist[0])), expj(ph(hist[1])));
                        let re = a[0] + 0.5 * b0[0] + 0.25 * b1[0];
                        let im = a[1] + 0.5 * b0[1] + 0.25 * b1[1];
                        im.atan2(re)
                    };
                    let po = smooth(opd[b], self.opd_hist[b]);
                    let pi = smooth(ipd[b], self.ipd_hist[b]);
                    self.opd_hist[b] = [opd[b], self.opd_hist[b][0]];
                    self.ipd_hist[b] = [ipd[b], self.ipd_hist[b][0]];
                    let (p1, p2) = (expj(po), expj(po - pi));
                    m = [mul(m[0], p1), mul(m[1], p2), mul(m[2], p1), mul(m[3], p2)];
                }
                target[b] = m;
            }
            // Interpolate from the previous matrices over the envelope.
            let end = p.borders[e] + 1;
            let width = (end - start) as f32;
            for k in 0..cfg.bands {
                let b = cfg.k_to_i[k];
                let (from, to) = (self.mix_prev[b], target[b]);
                let neg = negative(wide, k);
                for n in start..end {
                    let t = (n + 1 - start) as f32 / width;
                    let mut mm = [[0f32; 2]; 4];
                    for (j, v) in mm.iter_mut().enumerate() {
                        *v = [
                            from[j][0] + t * (to[j][0] - from[j][0]),
                            from[j][1] + t * (to[j][1] - from[j][1]),
                        ];
                        if neg {
                            v[1] = -v[1];
                        }
                    }
                    let (sv, dv) = (s[k][n], d[k][n]);
                    let a = mul(mm[0], sv);
                    let b2 = mul(mm[2], dv);
                    l[k][n] = [a[0] + b2[0], a[1] + b2[1]];
                    let a = mul(mm[1], sv);
                    let b2 = mul(mm[3], dv);
                    r[k][n] = [a[0] + b2[0], a[1] + b2[1]];
                }
            }
            self.mix_prev = target;
            start = end;
        }
    }
}

/// Hybrid synthesis: the split subbands' hybrid bands summed, the others
/// copied.
fn synthesis(hy: &[[C; SLOTS]; 91], cfg: &Config, wide: bool, out: &mut Qmf) {
    for part in out.iter_mut() {
        for row in part.iter_mut() {
            row.fill(0.0);
        }
    }
    for (q, sp) in split_of(wide).iter().enumerate() {
        let mut seen = [false; 91];
        for &k in sp.to {
            if std::mem::replace(&mut seen[k], true) {
                continue;
            }
            for n in 0..SLOTS {
                out[0][n][q] += hy[k][n][0];
                out[1][n][q] += hy[k][n][1];
            }
        }
    }
    for q in cfg.split..64 {
        let k = cfg.split_bands + q - cfg.split;
        for n in 0..SLOTS {
            out[0][n][q] = hy[k][n][0];
            out[1][n][q] = hy[k][n][1];
        }
    }
}

/// The last mixing matrices at the other resolution: 20 → 34 by the band
/// map, 34 → 20 as the mean over the 34 bands of each 20 band (bands
/// shared by two count half).
fn remap_mix(m: &[Mix; MAX_PAR], to_wide: bool) -> [Mix; MAX_PAR] {
    const FROM: [usize; 34] = [
        0, 0, 1, 2, 2, 3, 4, 4, 5, 5, 6, 7, 8, 8, 9, 9, 10, 11, 12, 13, 14, 14, 15, 15, 16, 16, 17,
        17, 18, 18, 18, 18, 19, 19,
    ];
    let mut out = [identity(); MAX_PAR];
    if to_wide {
        for (j, &i) in FROM.iter().enumerate() {
            out[j] = m[i];
        }
        for (j, (a, b)) in [(1, (0, 1)), (4, (2, 3))] {
            for e in 0..4 {
                for c in 0..2 {
                    out[j][e][c] = 0.5 * (m[a][e][c] + m[b][e][c]);
                }
            }
        }
    } else {
        // Weights: 1 per 34 band, ½ for the shared bands 1 and 4.
        let mut sum = [[[0f32; 2]; 4]; 20];
        let mut weight = [0f32; 20];
        for (j, &i) in FROM.iter().enumerate() {
            let shared = j == 1 || j == 4;
            let targets: &[usize] = if j == 1 {
                &[0, 1]
            } else if j == 4 {
                &[2, 3]
            } else {
                &[i]
            };
            let w = if shared { 0.5 } else { 1.0 };
            for &t in targets {
                weight[t] += w;
                for e in 0..4 {
                    for c in 0..2 {
                        sum[t][e][c] += w * m[j][e][c];
                    }
                }
            }
        }
        for t in 0..20 {
            for e in 0..4 {
                for c in 0..2 {
                    out[t][e][c] = sum[t][e][c] / weight[t];
                }
            }
        }
    }
    out
}
