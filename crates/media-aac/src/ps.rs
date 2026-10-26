// SPDX-License-Identifier: LGPL-2.1-or-later
//! Parametric Stereo (ISO/IEC 14496-3 8.6.4), the HE-AACv2 tool that makes
//! stereo from the mono SBR signal and a few inter-channel parameters:
//! hybrid analysis of the low QMF bands, a decorrelated copy by all-pass
//! filters, and mixing by intensity difference, coherence and phase.
//!
//! Follows FFmpeg's float decoder, step and buffer for step, so the output
//! can be compared with it; tables from FFmpeg (`ps_tables.rs`).
use crate::Error;
use crate::bits::Bits;
use crate::huffman::Tree;
use crate::ps_tables as tab;
use std::f64::consts::PI;
use std::sync::OnceLock;

const MAX_ENV: usize = 5;
const MAX_PAR: usize = 34;
const SLOTS: usize = 32;
const MAX_DELAY: usize = 14;
const AP_LINKS: usize = 3;
const MAX_AP_DELAY: usize = 5;

const NR_PAR_BANDS: [usize; 2] = [20, 34];
const NR_IPDOPD_BANDS: [usize; 2] = [11, 17];
const NR_BANDS: [usize; 2] = [71, 91];
const DECAY_CUTOFF: [i32; 2] = [10, 32];
const NR_ALLPASS_BANDS: [usize; 2] = [30, 50];
const SHORT_DELAY_BAND: [usize; 2] = [42, 62];

const IID_DF1: usize = 0;
const IID_DT1: usize = 1;
const IID_DF0: usize = 2;
const IID_DT0: usize = 3;
const ICC_DF: usize = 4;
const ICC_DT: usize = 5;
const IPD_DF: usize = 6;
const IPD_DT: usize = 7;
const OPD_DF: usize = 8;
const OPD_DT: usize = 9;

type Filter = Vec<[[f32; 2]; 8]>;

struct Static {
    trees: Vec<Tree>,
    pd_re: Vec<f32>,
    pd_im: Vec<f32>,
    ha: Vec<[[f32; 4]; 8]>,
    hb: Vec<[[f32; 4]; 8]>,
    f20_0_8: Filter,
    f34_0_12: Filter,
    f34_1_8: Filter,
    f34_2_4: Filter,
    /// [is34][band][link] and [is34][band].
    q_fract: [Vec<[[f32; 2]; 3]>; 2],
    phi_fract: [Vec<[f32; 2]>; 2],
}

fn filters(proto: &[f32; 7], bands: usize) -> Filter {
    (0..bands)
        .map(|q| {
            std::array::from_fn(|n| {
                if n < 7 {
                    let theta = 2.0 * PI * (q as f64 + 0.5) * (n as f64 - 6.0) / bands as f64;
                    [
                        (f64::from(proto[n]) * theta.cos()) as f32,
                        (f64::from(proto[n]) * -theta.sin()) as f32,
                    ]
                } else {
                    [0.0; 2]
                }
            })
        })
        .collect()
}

fn statics() -> &'static Static {
    static S: OnceLock<Static> = OnceLock::new();
    S.get_or_init(|| {
        let s2 = std::f32::consts::FRAC_1_SQRT_2;
        let pd_sin = [0.0, s2, 1.0, s2, 0.0, -s2, -1.0, -s2];
        let pd_cos = [1.0, s2, 0.0, -s2, -1.0, -s2, 0.0, s2];
        let mut pd_re = vec![0f32; 512];
        let mut pd_im = vec![0f32; 512];
        for p0 in 0..8 {
            for p1 in 0..8 {
                for p2 in 0..8 {
                    let re = 0.25f32 * pd_cos[p0] + 0.5 * pd_cos[p1] + pd_cos[p2];
                    let im = 0.25f32 * pd_sin[p0] + 0.5 * pd_sin[p1] + pd_sin[p2];
                    let mag = (1.0 / f64::from(im).hypot(f64::from(re))) as f32;
                    pd_re[p0 * 64 + p1 * 8 + p2] = re * mag;
                    pd_im[p0 * 64 + p1 * 8 + p2] = im * mag;
                }
            }
        }
        const IID_DEQUANT: [f64; 46] = [
            0.05623413251903,
            0.12589254117942,
            0.19952623149689,
            0.31622776601684,
            0.44668359215096,
            0.63095734448019,
            0.79432823472428,
            1.0,
            1.25892541179417,
            1.58489319246111,
            2.23872113856834,
            3.16227766016838,
            5.01187233627272,
            7.94328234724282,
            17.7827941003892,
            0.00316227766017,
            0.00562341325190,
            0.01,
            0.01778279410039,
            0.03162277660168,
            0.05623413251903,
            0.07943282347243,
            0.11220184543020,
            0.15848931924611,
            0.22387211385683,
            0.31622776601684,
            0.39810717055350,
            0.50118723362727,
            0.63095734448019,
            0.79432823472428,
            1.0,
            1.25892541179417,
            1.58489319246111,
            1.99526231496888,
            2.51188643150958,
            3.16227766016838,
            4.46683592150963,
            6.30957344480193,
            8.91250938133745,
            12.5892541179417,
            17.7827941003892,
            31.6227766016838,
            56.2341325190349,
            100.0,
            177.827941003892,
            316.227766016837,
        ];
        const ICC_INVQ: [f32; 8] = [1.0, 0.937, 0.84118, 0.60092, 0.36764, 0.0, -0.589, -1.0];
        const ACOS_ICC_INVQ: [f32; 8] = [
            0.0,
            0.356_855_27,
            0.571_334_66,
            0.926_144_7,
            1.194_326_3,
            std::f32::consts::FRAC_PI_2,
            2.200_617,
            std::f32::consts::PI,
        ];
        let mut ha = vec![[[0f32; 4]; 8]; 46];
        let mut hb = vec![[[0f32; 4]; 8]; 46];
        for iid in 0..46 {
            let c = IID_DEQUANT[iid] as f32;
            let c1 = std::f32::consts::SQRT_2 / (1.0 + c * c).sqrt();
            let c2 = c * c1;
            for icc in 0..8 {
                let alpha = 0.5 * ACOS_ICC_INVQ[icc];
                let beta = alpha * (c1 - c2) * std::f32::consts::FRAC_1_SQRT_2;
                ha[iid][icc] = [
                    c2 * (beta + alpha).cos(),
                    c1 * (beta - alpha).cos(),
                    c2 * (beta + alpha).sin(),
                    c1 * (beta - alpha).sin(),
                ];
                let rho = ICC_INVQ[icc].max(0.05);
                let mut alpha = 0.5 * (2.0 * c * rho).atan2(c * c - 1.0);
                let mu = c + 1.0 / c;
                let mu = (1.0 + (4.0 * rho * rho - 4.0) / (mu * mu)).sqrt();
                let gamma = ((1.0 - mu) / (1.0 + mu)).sqrt().atan();
                if alpha < 0.0 {
                    alpha = (f64::from(alpha) + PI / 2.0) as f32;
                }
                let (ac, as_, gc, gs) = (alpha.cos(), alpha.sin(), gamma.cos(), gamma.sin());
                let r2 = std::f64::consts::SQRT_2;
                hb[iid][icc] = [
                    (r2 * f64::from(ac) * f64::from(gc)) as f32,
                    (r2 * f64::from(as_) * f64::from(gc)) as f32,
                    (-r2 * f64::from(as_) * f64::from(gs)) as f32,
                    (r2 * f64::from(ac) * f64::from(gs)) as f32,
                ];
            }
        }
        const F_CENTER_20: [i32; 10] = [-3, -1, 1, 3, 5, 7, 10, 14, 18, 22];
        const F_CENTER_34: [i32; 32] = [
            2, 6, 10, 14, 18, 22, 26, 30, 34, -10, -6, -2, 51, 57, 15, 21, 27, 33, 39, 45, 54, 66,
            78, 42, 102, 66, 78, 90, 102, 114, 126, 90,
        ];
        const LINKS: [f32; 3] = [0.43, 0.75, 0.347];
        const GAIN: f32 = 0.39;
        let fract = |n: usize, centre: &dyn Fn(usize) -> f64| {
            let q: Vec<[[f32; 2]; 3]> = (0..n)
                .map(|k| {
                    std::array::from_fn(|m| {
                        let theta = -PI * f64::from(LINKS[m]) * centre(k);
                        [theta.cos() as f32, theta.sin() as f32]
                    })
                })
                .collect();
            let phi: Vec<[f32; 2]> = (0..n)
                .map(|k| {
                    let theta = -PI * f64::from(GAIN) * centre(k);
                    [theta.cos() as f32, theta.sin() as f32]
                })
                .collect();
            (q, phi)
        };
        let (q20, phi20) = fract(30, &|k| match F_CENTER_20.get(k) {
            Some(&c) => f64::from(c) * 0.125,
            None => f64::from(k as f32 - 6.5),
        });
        let (q34, phi34) = fract(50, &|k| match F_CENTER_34.get(k) {
            Some(&c) => f64::from(c) / 24.0,
            None => f64::from(k as f32 - 26.5),
        });
        Static {
            trees: tab::HUFF.iter().map(|(c, l)| Tree::new(c, l)).collect(),
            pd_re,
            pd_im,
            ha,
            hb,
            f20_0_8: filters(
                &[
                    0.007_460_829_5,
                    0.022_704_21,
                    0.045_468_66,
                    0.072_661_14,
                    0.098_851_09,
                    0.117_937_1,
                    0.125,
                ],
                8,
            ),
            f34_0_12: filters(
                &[
                    0.040_811_8,
                    0.038_128_11,
                    0.051_449_08,
                    0.063_998_31,
                    0.074_283_14,
                    0.081_003_48,
                    0.083_333_336,
                ],
                12,
            ),
            f34_1_8: filters(
                &[
                    0.015_656_756,
                    0.037_527_164,
                    0.054_178_914,
                    0.084_170_44,
                    0.103_073_44,
                    0.122_224_52,
                    0.125,
                ],
                8,
            ),
            f34_2_4: filters(
                &[
                    -0.059_082_11,
                    -0.048_714_984,
                    0.0,
                    0.077_787_24,
                    0.164_863_04,
                    0.232_798_57,
                    0.25,
                ],
                4,
            ),
            q_fract: [q20, q34],
            phi_fract: [phi20, phi34],
        }
    })
}

fn huff(b: &mut Bits, table: usize) -> Result<i32, Error> {
    Ok(statics().trees[table].decode(b)? as i32 - tab::HUFF_OFFSET[table])
}

type Pars = [[i8; MAX_PAR]; MAX_ENV];
/// H coefficients: [real/imag][envelope][parameter band].
type HMatrix = [[[f32; MAX_PAR]; MAX_ENV + 1]; 2];

pub(crate) struct Ps {
    start: bool,
    enable_iid: bool,
    iid_quant: bool,
    nr_iid_par: usize,
    nr_ipdopd_par: usize,
    enable_icc: bool,
    icc_mode: usize,
    nr_icc_par: usize,
    enable_ext: bool,
    num_env_old: usize,
    num_env: usize,
    enable_ipdopd: bool,
    border: [i32; MAX_ENV + 1],
    iid: Pars,
    icc: Pars,
    ipd: Pars,
    opd: Pars,
    is34: bool,
    is34_old: bool,
    in_buf: [[[f32; 2]; 44]; 5],
    delay: Box<[[[f32; 2]; SLOTS + MAX_DELAY]; 91]>,
    ap_delay: Box<[[[[f32; 2]; SLOTS + MAX_AP_DELAY]; AP_LINKS]; 50]>,
    peak_decay_nrg: [f32; 34],
    power_smooth: [f32; 34],
    peak_decay_diff_smooth: [f32; 34],
    h11: HMatrix,
    h12: HMatrix,
    h21: HMatrix,
    h22: HMatrix,
    lbuf: Box<[[[f32; 2]; SLOTS]; 91]>,
    rbuf: Box<[[[f32; 2]; SLOTS]; 91]>,
    opd_hist: [i8; MAX_PAR],
    ipd_hist: [i8; MAX_PAR],
}

/// QMF signal of one channel: [re/im][slot][band].
pub(crate) type Qmf = [[[f32; 64]; 38]; 2];

impl Ps {
    pub(crate) fn new() -> Ps {
        Ps {
            start: false,
            enable_iid: false,
            iid_quant: false,
            nr_iid_par: 0,
            nr_ipdopd_par: 0,
            enable_icc: false,
            icc_mode: 0,
            nr_icc_par: 0,
            enable_ext: false,
            num_env_old: 0,
            num_env: 0,
            enable_ipdopd: false,
            border: [0; MAX_ENV + 1],
            iid: [[0; MAX_PAR]; MAX_ENV],
            icc: [[0; MAX_PAR]; MAX_ENV],
            ipd: [[0; MAX_PAR]; MAX_ENV],
            opd: [[0; MAX_PAR]; MAX_ENV],
            is34: false,
            is34_old: false,
            in_buf: [[[0.0; 2]; 44]; 5],
            delay: Box::new([[[0.0; 2]; SLOTS + MAX_DELAY]; 91]),
            ap_delay: Box::new([[[[0.0; 2]; SLOTS + MAX_AP_DELAY]; AP_LINKS]; 50]),
            peak_decay_nrg: [0.0; 34],
            power_smooth: [0.0; 34],
            peak_decay_diff_smooth: [0.0; 34],
            h11: [[[0.0; MAX_PAR]; MAX_ENV + 1]; 2],
            h12: [[[0.0; MAX_PAR]; MAX_ENV + 1]; 2],
            h21: [[[0.0; MAX_PAR]; MAX_ENV + 1]; 2],
            h22: [[[0.0; MAX_PAR]; MAX_ENV + 1]; 2],
            lbuf: Box::new([[[0.0; 2]; SLOTS]; 91]),
            rbuf: Box::new([[[0.0; 2]; SLOTS]; 91]),
            opd_hist: [0; MAX_PAR],
            ipd_hist: [0; MAX_PAR],
        }
    }

    pub(crate) fn started(&self) -> bool {
        self.start
    }

    /// After a bad PS payload: no parameters until the next header.
    fn clear(&mut self) {
        self.start = false;
        self.iid = [[0; MAX_PAR]; MAX_ENV];
        self.icc = [[0; MAX_PAR]; MAX_ENV];
        self.ipd = [[0; MAX_PAR]; MAX_ENV];
        self.opd = [[0; MAX_PAR]; MAX_ENV];
    }

    /// Parameters of one envelope, delta coded in time (`dt`) or frequency.
    /// `false` for a value out of range.
    fn read_par(
        b: &mut Bits,
        par: &mut Pars,
        num: usize,
        table: usize,
        e: usize,
        dt: bool,
        e_prev: usize,
        check: impl Fn(i32) -> bool,
        mask: bool,
    ) -> Result<bool, Error> {
        let mut val = 0i32;
        for k in 0..num.min(MAX_PAR) {
            let d = huff(b, table)?;
            val = if dt {
                i32::from(par[e_prev][k]) + d
            } else {
                val + d
            };
            if mask {
                val &= 7;
            }
            par[e][k] = val as i8;
            if !check(val) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// `ps_data` with `bits_left` bits available; returns the bits used
    /// (all of them when the data is bad).
    pub(crate) fn read(&mut self, b: &mut Bits, bits_left: usize) -> usize {
        let start = b.pos();
        let mut sub = b.take(bits_left);
        match self.parse(&mut sub) {
            Ok(true) if sub.pos() - start <= bits_left => sub.pos() - start,
            _ => {
                self.clear();
                bits_left
            }
        }
    }

    fn parse(&mut self, b: &mut Bits) -> Result<bool, Error> {
        const NUM_ENV: [[usize; 4]; 2] = [[0, 1, 2, 4], [1, 2, 3, 4]];
        const NR_IIDICC: [usize; 6] = [10, 20, 34, 10, 20, 34];
        const NR_IPDOPD: [usize; 6] = [5, 11, 17, 5, 11, 17];
        let header = b.u(1)? == 1;
        if header {
            self.enable_iid = b.u(1)? == 1;
            if self.enable_iid {
                let mode = b.u(3)? as usize;
                if mode > 5 {
                    return Ok(false);
                }
                self.nr_iid_par = NR_IIDICC[mode];
                self.iid_quant = mode > 2;
                self.nr_ipdopd_par = NR_IPDOPD[mode];
            }
            self.enable_icc = b.u(1)? == 1;
            if self.enable_icc {
                self.icc_mode = b.u(3)? as usize;
                if self.icc_mode > 5 {
                    return Ok(false);
                }
                self.nr_icc_par = NR_IIDICC[self.icc_mode];
            }
            self.enable_ext = b.u(1)? == 1;
        }
        let frame_class = b.u(1)? == 1;
        self.num_env_old = self.num_env;
        self.num_env = NUM_ENV[usize::from(frame_class)][b.u(2)? as usize];
        self.border[0] = -1;
        if frame_class {
            for e in 1..=self.num_env {
                self.border[e] = b.u(5)? as i32;
                if self.border[e] < self.border[e - 1] {
                    return Ok(false);
                }
            }
        } else {
            let log2 = [0, 0, 1, 2, 2][self.num_env];
            for e in 1..=self.num_env {
                self.border[e] = ((e * SLOTS) >> log2) as i32 - 1;
            }
        }
        let e_prev = |e: usize, old: usize| if e > 0 { e - 1 } else { old.saturating_sub(1) };
        let iid_max = 7 + 8 * i32::from(self.iid_quant);
        if self.enable_iid {
            for e in 0..self.num_env {
                let dt = b.u(1)? == 1;
                let table = match (dt, self.iid_quant) {
                    (false, false) => IID_DF0,
                    (false, true) => IID_DF1,
                    (true, false) => IID_DT0,
                    (true, true) => IID_DT1,
                };
                let ep = e_prev(e, self.num_env_old);
                let n = self.nr_iid_par;
                if !Self::read_par(
                    b,
                    &mut self.iid,
                    n,
                    table,
                    e,
                    dt,
                    ep,
                    |v| v.abs() <= iid_max,
                    false,
                )? {
                    return Ok(false);
                }
            }
        } else {
            self.iid = [[0; MAX_PAR]; MAX_ENV];
        }
        if self.enable_icc {
            for e in 0..self.num_env {
                let dt = b.u(1)? == 1;
                let ep = e_prev(e, self.num_env_old);
                let n = self.nr_icc_par;
                let table = if dt { ICC_DT } else { ICC_DF };
                if !Self::read_par(
                    b,
                    &mut self.icc,
                    n,
                    table,
                    e,
                    dt,
                    ep,
                    |v| (0..=7).contains(&v),
                    false,
                )? {
                    return Ok(false);
                }
            }
        } else {
            self.icc = [[0; MAX_PAR]; MAX_ENV];
        }
        if self.enable_ext {
            let mut cnt = b.u(4)? as i64;
            if cnt == 15 {
                cnt += i64::from(b.u(8)?);
            }
            cnt *= 8;
            while cnt > 7 {
                let id = b.u(2)?;
                let before = b.pos();
                if id == 0 {
                    self.enable_ipdopd = b.u(1)? == 1;
                    if self.enable_ipdopd {
                        for e in 0..self.num_env {
                            let ep = e_prev(e, self.num_env_old);
                            let n = self.nr_ipdopd_par;
                            let dt = b.u(1)? == 1;
                            let t = if dt { IPD_DT } else { IPD_DF };
                            Self::read_par(b, &mut self.ipd, n, t, e, dt, ep, |_| true, true)?;
                            let dt = b.u(1)? == 1;
                            let t = if dt { OPD_DT } else { OPD_DF };
                            Self::read_par(b, &mut self.opd, n, t, e, dt, ep, |_| true, true)?;
                        }
                    }
                    b.u(1)?;
                }
                cnt -= 2 + (b.pos() - before) as i64;
            }
            if cnt < 0 {
                return Ok(false);
            }
            b.skip(cnt as usize)?;
        }
        // A last envelope reaching the frame end, made up if missing.
        if self.num_env == 0 || self.border[self.num_env] < SLOTS as i32 - 1 {
            let source = if self.num_env > 0 {
                self.num_env as i32 - 1
            } else {
                self.num_env_old as i32 - 1
            };
            let n = self.num_env;
            if source >= 0 && source as usize != n {
                let s = source as usize;
                if self.enable_iid {
                    self.iid[n] = self.iid[s];
                }
                if self.enable_icc {
                    self.icc[n] = self.icc[s];
                }
                if self.enable_ipdopd {
                    self.ipd[n] = self.ipd[s];
                    self.opd[n] = self.opd[s];
                }
            }
            if self.enable_iid
                && self.iid[n][..self.nr_iid_par]
                    .iter()
                    .any(|&v| i32::from(v).abs() > iid_max)
            {
                return Ok(false);
            }
            if self.enable_icc
                && self.icc[n][..self.nr_iid_par.min(MAX_PAR)]
                    .iter()
                    .any(|&v| !(0..=7).contains(&v))
            {
                return Ok(false);
            }
            self.num_env += 1;
            self.border[self.num_env] = SLOTS as i32 - 1;
        }
        self.is34_old = self.is34;
        if self.enable_iid || self.enable_icc {
            self.is34 = (self.enable_iid && self.nr_iid_par == 34)
                || (self.enable_icc && self.nr_icc_par == 34);
        }
        if !self.enable_ipdopd {
            self.ipd = [[0; MAX_PAR]; MAX_ENV];
            self.opd = [[0; MAX_PAR]; MAX_ENV];
        }
        if header {
            self.start = true;
        }
        Ok(true)
    }

    /// One band split by a complex filter: 13 input slots → `n` outputs.
    fn hybrid_filter(input: &[[f32; 2]], filter: &[[[f32; 2]; 8]], out: &mut [[f32; 2]]) {
        let mut re0 = [0f32; 6];
        let mut re1 = [0f32; 6];
        let mut im0 = [0f32; 6];
        let mut im1 = [0f32; 6];
        for j in 0..6 {
            re0[j] = input[j][0] + input[12 - j][0];
            re1[j] = input[j][1] - input[12 - j][1];
            im0[j] = input[j][1] + input[12 - j][1];
            im1[j] = input[j][0] - input[12 - j][0];
        }
        for (o, f) in out.iter_mut().zip(filter) {
            let mut re = f[6][0] * input[6][0];
            let mut im = f[6][0] * input[6][1];
            for j in 0..6 {
                re += f[j][0] * re0[j] - f[j][1] * re1[j];
                im += f[j][0] * im0[j] + f[j][1] * im1[j];
            }
            *o = [re, im];
        }
    }

    fn hybrid_analysis(&mut self, l: &Qmf) {
        let st = statics();
        let is34 = self.is34;
        for i in 0..5 {
            for j in 0..38 {
                self.in_buf[i][j + 6] = [l[0][j][i], l[1][j][i]];
            }
        }
        let out = &mut self.lbuf;
        let mut tmp = [[0f32; 2]; 12];
        if is34 {
            let parts: [(usize, &Filter, usize, usize); 5] = [
                (0, &st.f34_0_12, 12, 0),
                (1, &st.f34_1_8, 8, 12),
                (2, &st.f34_2_4, 4, 20),
                (3, &st.f34_2_4, 4, 24),
                (4, &st.f34_2_4, 4, 28),
            ];
            for (band, filter, n, at) in parts {
                for i in 0..SLOTS {
                    Self::hybrid_filter(&self.in_buf[band][i..], &filter[..n], &mut tmp[..n]);
                    for q in 0..n {
                        out[at + q][i] = tmp[q];
                    }
                }
            }
            for k in 5..64 {
                for j in 0..SLOTS {
                    out[27 + k][j] = [l[0][j][k], l[1][j][k]];
                }
            }
        } else {
            for i in 0..SLOTS {
                Self::hybrid_filter(&self.in_buf[0][i..], &st.f20_0_8, &mut tmp[..8]);
                out[0][i] = tmp[6];
                out[1][i] = tmp[7];
                out[2][i] = tmp[0];
                out[3][i] = tmp[1];
                out[4][i] = [tmp[2][0] + tmp[5][0], tmp[2][1] + tmp[5][1]];
                out[5][i] = [tmp[3][0] + tmp[4][0], tmp[3][1] + tmp[4][1]];
            }
            const G1_Q2: [f32; 7] = [
                0.0,
                0.018_994_875,
                0.0,
                -0.072_931_39,
                0.0,
                0.305_966_3,
                0.5,
            ];
            for (band, at, reverse) in [(1usize, 6usize, true), (2, 8, false)] {
                for i in 0..SLOTS {
                    let x = &self.in_buf[band][i..];
                    let re_in = G1_Q2[6] * x[6][0];
                    let im_in = G1_Q2[6] * x[6][1];
                    let (mut re_op, mut im_op) = (0f32, 0f32);
                    for j in (0..6).step_by(2) {
                        re_op += G1_Q2[j + 1] * (x[j + 1][0] + x[12 - j - 1][0]);
                        im_op += G1_Q2[j + 1] * (x[j + 1][1] + x[12 - j - 1][1]);
                    }
                    let (a, b) = if reverse { (at + 1, at) } else { (at, at + 1) };
                    out[a][i] = [re_in + re_op, im_in + im_op];
                    out[b][i] = [re_in - re_op, im_in - im_op];
                }
            }
            for k in 3..64 {
                for j in 0..SLOTS {
                    out[7 + k][j] = [l[0][j][k], l[1][j][k]];
                }
            }
        }
        for row in &mut self.in_buf {
            row.copy_within(32..38, 0);
        }
    }

    fn hybrid_synthesis(out: &mut Qmf, input: &[[[f32; 2]; SLOTS]; 91], is34: bool) {
        let sum = |range: std::ops::Range<usize>, n: usize, c: usize| -> f32 {
            range.map(|i| input[i][n][c]).sum()
        };
        if is34 {
            for n in 0..SLOTS {
                for c in 0..2 {
                    out[c][n][0] = sum(0..12, n, c);
                    out[c][n][1] = sum(12..20, n, c);
                    out[c][n][2] = sum(20..24, n, c);
                    out[c][n][3] = sum(24..28, n, c);
                    out[c][n][4] = sum(28..32, n, c);
                }
            }
            for k in 5..64 {
                for n in 0..SLOTS {
                    out[0][n][k] = input[27 + k][n][0];
                    out[1][n][k] = input[27 + k][n][1];
                }
            }
        } else {
            for n in 0..SLOTS {
                for c in 0..2 {
                    out[c][n][0] = input[0][n][c]
                        + input[1][n][c]
                        + input[2][n][c]
                        + input[3][n][c]
                        + input[4][n][c]
                        + input[5][n][c];
                    out[c][n][1] = input[6][n][c] + input[7][n][c];
                    out[c][n][2] = input[8][n][c] + input[9][n][c];
                }
            }
            for k in 3..64 {
                for n in 0..SLOTS {
                    out[0][n][k] = input[7 + k][n][0];
                    out[1][n][k] = input[7 + k][n][1];
                }
            }
        }
    }

    /// The decorrelated signal into `rbuf` (8.6.4.5).
    fn decorrelation(&mut self) {
        let st = statics();
        let is34 = usize::from(self.is34);
        let k_to_i: &[usize] = if self.is34 {
            &tab::K_TO_I_34
        } else {
            &tab::K_TO_I_20
        };
        let mut power = [[0f32; SLOTS]; 34];
        let mut transient_gain = [[0f32; SLOTS]; 34];
        if self.is34 != self.is34_old {
            self.peak_decay_nrg = [0.0; 34];
            self.power_smooth = [0.0; 34];
            self.peak_decay_diff_smooth = [0.0; 34];
            *self.delay = [[[0.0; 2]; SLOTS + MAX_DELAY]; 91];
            *self.ap_delay = [[[[0.0; 2]; SLOTS + MAX_AP_DELAY]; AP_LINKS]; 50];
        }
        for k in 0..NR_BANDS[is34] {
            let i = k_to_i[k];
            for n in 0..SLOTS {
                let s = self.lbuf[k][n];
                power[i][n] += s[0] * s[0] + s[1] * s[1];
            }
        }
        const PEAK_DECAY: f32 = 0.765_928_3;
        for i in 0..NR_PAR_BANDS[is34] {
            for n in 0..SLOTS {
                let decayed = PEAK_DECAY * self.peak_decay_nrg[i];
                self.peak_decay_nrg[i] = decayed.max(power[i][n]);
                self.power_smooth[i] += 0.25 * (power[i][n] - self.power_smooth[i]);
                self.peak_decay_diff_smooth[i] +=
                    0.25 * (self.peak_decay_nrg[i] - power[i][n] - self.peak_decay_diff_smooth[i]);
                let denom = 1.5 * self.peak_decay_diff_smooth[i];
                transient_gain[i][n] = if denom > self.power_smooth[i] {
                    self.power_smooth[i] / denom
                } else {
                    1.0
                };
            }
        }
        const A: [f32; 3] = [0.651_439_1, 0.564_718_1, 0.489_541_66];
        let shift = |d: &mut [[f32; 2]; SLOTS + MAX_DELAY], s: &[[f32; 2]; SLOTS]| {
            d.copy_within(SLOTS..SLOTS + MAX_DELAY, 0);
            d[MAX_DELAY..].copy_from_slice(s);
        };
        let mut k = 0;
        while k < NR_ALLPASS_BANDS[is34] {
            let b = k_to_i[k];
            let g_decay = (1.0 - 0.05 * (k as i32 - DECAY_CUTOFF[is34]) as f32).clamp(0.0, 1.0);
            shift(&mut self.delay[k], &self.lbuf[k]);
            for m in 0..AP_LINKS {
                self.ap_delay[k][m].copy_within(SLOTS..SLOTS + 5, 0);
            }
            let phi = st.phi_fract[is34][k];
            let q = st.q_fract[is34][k];
            let ag = A.map(|a| a * g_decay);
            for n in 0..SLOTS {
                let d = self.delay[k][MAX_DELAY - 2 + n];
                let mut in_re = d[0] * phi[0] - d[1] * phi[1];
                let mut in_im = d[0] * phi[1] + d[1] * phi[0];
                for m in 0..AP_LINKS {
                    let a_re = ag[m] * in_re;
                    let a_im = ag[m] * in_im;
                    let link = self.ap_delay[k][m][n + 2 - m];
                    let (apd_re, apd_im) = (in_re, in_im);
                    in_re = link[0] * q[m][0] - link[1] * q[m][1];
                    in_re -= a_re;
                    in_im = link[0] * q[m][1] + link[1] * q[m][0];
                    in_im -= a_im;
                    self.ap_delay[k][m][n + 5] = [apd_re + ag[m] * in_re, apd_im + ag[m] * in_im];
                }
                let g = transient_gain[b][n];
                self.rbuf[k][n] = [g * in_re, g * in_im];
            }
            k += 1;
        }
        while k < NR_BANDS[is34] {
            let i = k_to_i[k];
            shift(&mut self.delay[k], &self.lbuf[k]);
            let lag = if k < SHORT_DELAY_BAND[is34] { 14 } else { 1 };
            for n in 0..SLOTS {
                let d = self.delay[k][MAX_DELAY - lag + n];
                let g = transient_gain[i][n];
                self.rbuf[k][n] = [d[0] * g, d[1] * g];
            }
            k += 1;
        }
    }

    /// Mixing of `lbuf` (s) and `rbuf` (d) into left and right (8.6.4.6).
    fn stereo_processing(&mut self) {
        let st = statics();
        let is34 = usize::from(self.is34);
        let k_to_i: &[usize] = if self.is34 {
            &tab::K_TO_I_34
        } else {
            &tab::K_TO_I_20
        };
        let lut = if self.icc_mode < 3 { &st.ha } else { &st.hb };
        let old = self.num_env_old;
        if old > 0 {
            for h in [&mut self.h11, &mut self.h12, &mut self.h21, &mut self.h22] {
                h[0][0] = h[0][old];
                h[1][0] = h[1][old];
            }
        }
        let n_env = self.num_env;
        let mut iid_m = self.iid;
        let mut icc_m = self.icc;
        let mut ipd_m = self.ipd;
        let mut opd_m = self.opd;
        if self.is34 {
            remap34(&mut iid_m, &self.iid, self.nr_iid_par, n_env, true);
            remap34(&mut icc_m, &self.icc, self.nr_icc_par, n_env, true);
            if self.enable_ipdopd {
                remap34(&mut ipd_m, &self.ipd, self.nr_ipdopd_par, n_env, false);
                remap34(&mut opd_m, &self.opd, self.nr_ipdopd_par, n_env, false);
            }
            if !self.is34_old {
                for h in [&mut self.h11, &mut self.h12, &mut self.h21, &mut self.h22] {
                    map_val_20_to_34(&mut h[0][0]);
                    map_val_20_to_34(&mut h[1][0]);
                }
                self.ipd_hist = [0; MAX_PAR];
                self.opd_hist = [0; MAX_PAR];
            }
        } else {
            remap20(&mut iid_m, &self.iid, self.nr_iid_par, n_env, true);
            remap20(&mut icc_m, &self.icc, self.nr_icc_par, n_env, true);
            if self.enable_ipdopd {
                remap20(&mut ipd_m, &self.ipd, self.nr_ipdopd_par, n_env, false);
                remap20(&mut opd_m, &self.opd, self.nr_ipdopd_par, n_env, false);
            }
            if self.is34_old {
                for h in [&mut self.h11, &mut self.h12, &mut self.h21, &mut self.h22] {
                    map_val_34_to_20(&mut h[0][0]);
                    map_val_34_to_20(&mut h[1][0]);
                }
                self.ipd_hist = [0; MAX_PAR];
                self.opd_hist = [0; MAX_PAR];
            }
        }
        let quant = 23 * usize::from(self.iid_quant);
        for e in 0..n_env {
            for b in 0..NR_PAR_BANDS[is34] {
                let row = (i32::from(iid_m[e][b]) + 7 + quant as i32).clamp(0, 45) as usize;
                let h = lut[row][usize::from(icc_m[e][b] as u8 & 7)];
                let (mut h11, mut h12, mut h21, mut h22) = (h[0], h[1], h[2], h[3]);
                if self.enable_ipdopd && b < NR_IPDOPD_BANDS[is34] {
                    let opd_idx =
                        (i32::from(self.opd_hist[b]) * 8 + i32::from(opd_m[e][b])) as usize & 511;
                    let ipd_idx =
                        (i32::from(self.ipd_hist[b]) * 8 + i32::from(ipd_m[e][b])) as usize & 511;
                    let (opd_re, opd_im) = (st.pd_re[opd_idx], st.pd_im[opd_idx]);
                    let (ipd_re, ipd_im) = (st.pd_re[ipd_idx], st.pd_im[ipd_idx]);
                    self.opd_hist[b] = (opd_idx & 0x3f) as i8;
                    self.ipd_hist[b] = (ipd_idx & 0x3f) as i8;
                    let adj_re = opd_re * ipd_re + opd_im * ipd_im;
                    let adj_im = opd_im * ipd_re - opd_re * ipd_im;
                    let h11i = h11 * opd_im;
                    h11 *= opd_re;
                    let h12i = h12 * adj_im;
                    h12 *= adj_re;
                    let h21i = h21 * opd_im;
                    h21 *= opd_re;
                    let h22i = h22 * adj_im;
                    h22 *= adj_re;
                    self.h11[1][e + 1][b] = h11i;
                    self.h12[1][e + 1][b] = h12i;
                    self.h21[1][e + 1][b] = h21i;
                    self.h22[1][e + 1][b] = h22i;
                }
                self.h11[0][e + 1][b] = h11;
                self.h12[0][e + 1][b] = h12;
                self.h21[0][e + 1][b] = h21;
                self.h22[0][e + 1][b] = h22;
            }
            for k in 0..NR_BANDS[is34] {
                let start = self.border[e];
                let stop = self.border[e + 1];
                let width = 1.0
                    / if stop - start != 0 {
                        (stop - start) as f32
                    } else {
                        1.0
                    };
                let b = k_to_i[k];
                let mut h = [[0f32; 4]; 2];
                let mut step = [[0f32; 4]; 2];
                h[0] = [
                    self.h11[0][e][b],
                    self.h12[0][e][b],
                    self.h21[0][e][b],
                    self.h22[0][e][b],
                ];
                let ipdopd = self.enable_ipdopd;
                if ipdopd {
                    let neg = (self.is34 && (9..=13).contains(&k)) || (!self.is34 && k <= 1);
                    let s = if neg { -1.0 } else { 1.0 };
                    h[1] = [
                        s * self.h11[1][e][b],
                        s * self.h12[1][e][b],
                        s * self.h21[1][e][b],
                        s * self.h22[1][e][b],
                    ];
                }
                let next = [
                    self.h11[0][e + 1][b],
                    self.h12[0][e + 1][b],
                    self.h21[0][e + 1][b],
                    self.h22[0][e + 1][b],
                ];
                for j in 0..4 {
                    step[0][j] = (next[j] - h[0][j]) * width;
                }
                if ipdopd {
                    let next = [
                        self.h11[1][e + 1][b],
                        self.h12[1][e + 1][b],
                        self.h21[1][e + 1][b],
                        self.h22[1][e + 1][b],
                    ];
                    for j in 0..4 {
                        step[1][j] = (next[j] - h[1][j]) * width;
                    }
                }
                if stop - start != 0 {
                    let from = (start + 1) as usize;
                    let len = (stop - start) as usize;
                    let (l, r) = (&mut self.lbuf[k], &mut self.rbuf[k]);
                    let mut hh = h;
                    for n in from..(from + len).min(SLOTS) {
                        let (l_re, l_im, r_re, r_im) = (l[n][0], l[n][1], r[n][0], r[n][1]);
                        for j in 0..4 {
                            hh[0][j] += step[0][j];
                        }
                        if ipdopd {
                            for j in 0..4 {
                                hh[1][j] += step[1][j];
                            }
                            let (h0, h1) = (hh[0], hh[1]);
                            l[n][0] = h0[0] * l_re + h0[2] * r_re - h1[0] * l_im - h1[2] * r_im;
                            l[n][1] = h0[0] * l_im + h0[2] * r_im + h1[0] * l_re + h1[2] * r_re;
                            r[n][0] = h0[1] * l_re + h0[3] * r_re - h1[1] * l_im - h1[3] * r_im;
                            r[n][1] = h0[1] * l_im + h0[3] * r_im + h1[1] * l_re + h1[3] * r_re;
                        } else {
                            let h0 = hh[0];
                            l[n][0] = h0[0] * l_re + h0[2] * r_re;
                            l[n][1] = h0[0] * l_im + h0[2] * r_im;
                            r[n][0] = h0[1] * l_re + h0[3] * r_re;
                            r[n][1] = h0[1] * l_im + h0[3] * r_im;
                        }
                    }
                }
            }
        }
    }

    /// Make `l` (the mono SBR signal) and `r` stereo; `top` is the last
    /// QMF band with signal.
    pub(crate) fn apply(&mut self, l: &mut Qmf, r: &mut Qmf, top: usize) {
        let is34 = usize::from(self.is34);
        let top = (top + NR_BANDS[is34]).saturating_sub(64);
        for row in self.delay.iter_mut().take(NR_BANDS[is34]).skip(top) {
            *row = [[0.0; 2]; SLOTS + MAX_DELAY];
        }
        if top < NR_ALLPASS_BANDS[is34] {
            for row in self
                .ap_delay
                .iter_mut()
                .take(NR_ALLPASS_BANDS[is34])
                .skip(top)
            {
                *row = [[[0.0; 2]; SLOTS + MAX_AP_DELAY]; AP_LINKS];
            }
        }
        self.hybrid_analysis(l);
        self.decorrelation();
        self.stereo_processing();
        Self::hybrid_synthesis(l, &self.lbuf, self.is34);
        Self::hybrid_synthesis(r, &self.rbuf, self.is34);
    }
}

fn map_idx_10_to_20(m: &mut [i8; MAX_PAR], p: &[i8; MAX_PAR], full: bool) {
    let top = if full {
        9
    } else {
        m[10] = 0;
        4
    };
    for b in (0..=top).rev() {
        m[2 * b + 1] = p[b];
        m[2 * b] = p[b];
    }
}

fn map_idx_34_to_20(m: &mut [i8; MAX_PAR], p: &[i8; MAX_PAR], full: bool) {
    let p = p.map(i32::from);
    let v = |x: i32| x as i8;
    m[0] = v((2 * p[0] + p[1]) / 3);
    m[1] = v((p[1] + 2 * p[2]) / 3);
    m[2] = v((2 * p[3] + p[4]) / 3);
    m[3] = v((p[4] + 2 * p[5]) / 3);
    m[4] = v((p[6] + p[7]) / 2);
    m[5] = v((p[8] + p[9]) / 2);
    m[6] = v(p[10]);
    m[7] = v(p[11]);
    m[8] = v((p[12] + p[13]) / 2);
    m[9] = v((p[14] + p[15]) / 2);
    m[10] = v(p[16]);
    if full {
        m[11] = v(p[17]);
        m[12] = v(p[18]);
        m[13] = v(p[19]);
        m[14] = v((p[20] + p[21]) / 2);
        m[15] = v((p[22] + p[23]) / 2);
        m[16] = v((p[24] + p[25]) / 2);
        m[17] = v((p[26] + p[27]) / 2);
        m[18] = v((p[28] + p[29] + p[30] + p[31]) / 4);
        m[19] = v((p[32] + p[33]) / 2);
    }
}

fn map_idx_10_to_34(m: &mut [i8; MAX_PAR], p: &[i8; MAX_PAR], full: bool) {
    if full {
        m[28..34].fill(p[9]);
        m[24..28].fill(p[8]);
        m[20..24].fill(p[7]);
        m[18..20].fill(p[6]);
        m[16..18].fill(p[5]);
    } else {
        m[16] = 0;
    }
    m[12..16].fill(p[4]);
    m[10..12].fill(p[3]);
    m[6..10].fill(p[2]);
    m[3..6].fill(p[1]);
    m[0..3].fill(p[0]);
}

fn map_idx_20_to_34(m: &mut [i8; MAX_PAR], p: &[i8; MAX_PAR], full: bool) {
    if full {
        m[33] = p[19];
        m[32] = p[19];
        m[28..32].fill(p[18]);
        m[26..28].fill(p[17]);
        m[24..26].fill(p[16]);
        m[22..24].fill(p[15]);
        m[20..22].fill(p[14]);
        m[19] = p[13];
        m[18] = p[12];
        m[17] = p[11];
    }
    m[16] = p[10];
    m[15] = p[9];
    m[14] = p[9];
    m[13] = p[8];
    m[12] = p[8];
    m[11] = p[7];
    m[10] = p[6];
    m[9] = p[5];
    m[8] = p[5];
    m[7] = p[4];
    m[6] = p[4];
    m[5] = p[3];
    m[4] = ((i32::from(p[2]) + i32::from(p[3])) / 2) as i8;
    m[3] = p[2];
    m[2] = p[1];
    m[1] = ((i32::from(p[0]) + i32::from(p[1])) / 2) as i8;
    m[0] = p[0];
}

fn map_val_34_to_20(p: &mut [f32; MAX_PAR]) {
    p[0] = (2.0 * p[0] + p[1]) * 0.333_333_34;
    p[1] = (p[1] + 2.0 * p[2]) * 0.333_333_34;
    p[2] = (2.0 * p[3] + p[4]) * 0.333_333_34;
    p[3] = (p[4] + 2.0 * p[5]) * 0.333_333_34;
    p[4] = (p[6] + p[7]) * 0.5;
    p[5] = (p[8] + p[9]) * 0.5;
    p[6] = p[10];
    p[7] = p[11];
    p[8] = (p[12] + p[13]) * 0.5;
    p[9] = (p[14] + p[15]) * 0.5;
    p[10] = p[16];
    p[11] = p[17];
    p[12] = p[18];
    p[13] = p[19];
    p[14] = (p[20] + p[21]) * 0.5;
    p[15] = (p[22] + p[23]) * 0.5;
    p[16] = (p[24] + p[25]) * 0.5;
    p[17] = (p[26] + p[27]) * 0.5;
    p[18] = (p[28] + p[29] + p[30] + p[31]) * 0.25;
    p[19] = (p[32] + p[33]) * 0.5;
}

fn map_val_20_to_34(p: &mut [f32; MAX_PAR]) {
    p[33] = p[19];
    p[32] = p[19];
    p[31] = p[18];
    p[30] = p[18];
    p[29] = p[18];
    p[28] = p[18];
    p[27] = p[17];
    p[26] = p[17];
    p[25] = p[16];
    p[24] = p[16];
    p[23] = p[15];
    p[22] = p[15];
    p[21] = p[14];
    p[20] = p[14];
    p[19] = p[13];
    p[18] = p[12];
    p[17] = p[11];
    p[16] = p[10];
    p[15] = p[9];
    p[14] = p[9];
    p[13] = p[8];
    p[12] = p[8];
    p[11] = p[7];
    p[10] = p[6];
    p[9] = p[5];
    p[8] = p[5];
    p[7] = p[4];
    p[6] = p[4];
    p[5] = p[3];
    p[4] = (p[2] + p[3]) * 0.5;
    p[3] = p[2];
    p[2] = p[1];
    p[1] = (p[0] + p[1]) * 0.5;
}

/// Parameters mapped to 34 bands (or kept when already 34/17).
fn remap34(mapped: &mut Pars, par: &Pars, num: usize, n_env: usize, full: bool) {
    match num {
        20 | 11 => {
            (0..n_env.min(MAX_ENV)).for_each(|e| map_idx_20_to_34(&mut mapped[e], &par[e], full))
        }
        10 | 5 => {
            (0..n_env.min(MAX_ENV)).for_each(|e| map_idx_10_to_34(&mut mapped[e], &par[e], full))
        }
        _ => *mapped = *par,
    }
}

/// Parameters mapped to 20 bands (or kept when already 20/11).
fn remap20(mapped: &mut Pars, par: &Pars, num: usize, n_env: usize, full: bool) {
    match num {
        34 | 17 => {
            (0..n_env.min(MAX_ENV)).for_each(|e| map_idx_34_to_20(&mut mapped[e], &par[e], full))
        }
        10 | 5 => {
            (0..n_env.min(MAX_ENV)).for_each(|e| map_idx_10_to_20(&mut mapped[e], &par[e], full))
        }
        _ => *mapped = *par,
    }
}
