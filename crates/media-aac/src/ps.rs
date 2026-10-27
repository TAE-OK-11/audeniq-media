//! Parametric stereo (ISO/IEC 14496-3 subclause 8.6.4), HE-AAC v2's tool
//! that makes a stereo pair from the mono SBR signal and a few parameters
//! per frequency band, written from the standard's description:
//!
//! - `ps_data`: inter-channel intensity differences (IID) and coherence
//!   (ICC), optional phase differences (IPD, OPD), in 10, 20 or 34
//!   parameter bands, delta coded in frequency or time, per envelope;
//! - a hybrid filter bank that splits the lowest QMF subbands further
//!   (20-band configuration: 10 hybrid bands from QMF 0–2; 34-band: 32
//!   from QMF 0–4), the other subbands delayed to match;
//! - a decorrelated signal: all-pass filters with fractional delays in
//!   the lower bands, plain delays above, attenuated at transients;
//! - per band and slot a 2×2 mixing of the mono and decorrelated signals,
//!   interpolated between envelopes, then hybrid synthesis.
//!
//! Tables (`ps_tables.rs`) are the standard's data.
// The stereo processing that uses the rest arrives with the next change.
#![allow(dead_code, clippy::excessive_precision)]
use crate::ps_tables as tab;
use media_bits::{Bits, Huffman as Tree};
use std::sync::OnceLock;

const SLOTS: usize = 32;
/// Group delay of the hybrid filters, in QMF slots.
const HYBRID_DELAY: usize = 6;
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
    0.222_173_54,
    0.25,
];

/// All-pass decorrelator: link delays, fractional-delay factors and the
/// filter coefficient; the overall fractional delay; the decay of the
/// filter gain above the cutoff band.
const LINK_DELAY: [usize; 3] = [3, 4, 5];
const LINK_FRACTION: [f32; 3] = [0.43, 0.75, 0.347];
const ALLPASS_A: f32 = 0.651_439_06;
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
    /// Other QMF subbands' last 6 slots.
    qmf_delay: Box<[[C; HYBRID_DELAY]; 64]>,
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
            qmf_delay: Box::new([[[0.0; 2]; HYBRID_DELAY]; 64]),
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
