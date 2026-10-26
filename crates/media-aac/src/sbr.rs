// SPDX-License-Identifier: LGPL-2.1-or-later
//! Spectral Band Replication (ISO/IEC 14496-3 4.6.18), the HE-AAC tool that
//! rebuilds the upper half of the spectrum from the AAC core and a few
//! envelope and noise parameters: QMF analysis of the core (32 bands),
//! high-band generation by patching with inverse filtering, envelope
//! adjustment with noise and sinusoids, QMF synthesis (64 bands) at twice
//! the core rate.
//!
//! The buffers and their timing follow FFmpeg's decoder so the output can
//! be compared with it sample by sample; the QMF banks are the standard's
//! complex-exponential matrices. Tables come from FFmpeg (`sbr_tables.rs`).
use crate::Error;
use crate::bits::Bits;
use crate::huffman::Tree;
use crate::ps::Ps;
use crate::sbr_tables as tab;
use std::f64::consts::PI;
use std::sync::OnceLock;

/// QMF slots between the HF generator's input and the envelope grid.
const ENV_OFF: usize = 2;
const NOISE_FLOOR_OFFSET: i32 = 6;
/// Core samples are scaled to ±32768 inside SBR (the envelope scale).
const SCALE: f32 = 32768.0;

const FIXFIX: u32 = 0;
const VARFIX: u32 = 2;

/// Huffman tables (by [`tab::HUFF`] index).
const T_ENV_1_5: usize = 0;
const F_ENV_1_5: usize = 1;
const T_ENV_BAL_1_5: usize = 2;
const F_ENV_BAL_1_5: usize = 3;
const T_ENV_3_0: usize = 4;
const F_ENV_3_0: usize = 5;
const T_ENV_BAL_3_0: usize = 6;
const F_ENV_BAL_3_0: usize = 7;
const T_NOISE_3_0: usize = 8;
const T_NOISE_BAL_3_0: usize = 9;

struct Static {
    trees: Vec<Tree>,
    /// Analysis matrix 2·exp(iπ(k+½)(2n−½)/64): [k][n] (re, im).
    ana: Vec<[[f32; 2]; 64]>,
    /// Synthesis matrix exp(iπ(k+½)(2n−255)/128)/64: [n][k] (re, im).
    syn: Vec<[[f32; 2]; 64]>,
    /// Downsampled (32-band) synthesis exp(iπ(k+½)(2n−127.5)/64)/64: unity
    /// gain with the analysis bank, timed as the reference decoder's output
    /// (conformance stream `al_sbr_sr_48_2_fsaac48`).
    syn_ds: Vec<[[f32; 2]; 32]>,
}

fn statics() -> &'static Static {
    static S: OnceLock<Static> = OnceLock::new();
    S.get_or_init(|| Static {
        trees: tab::HUFF.iter().map(|(c, l)| Tree::new(c, l)).collect(),
        ana: (0..32)
            .map(|k| {
                std::array::from_fn(|n| {
                    let a = PI * (k as f64 + 0.5) * (2.0 * n as f64 - 0.5) / 64.0;
                    [(2.0 * a.cos()) as f32, (2.0 * a.sin()) as f32]
                })
            })
            .collect(),
        syn: (0..128)
            .map(|n| {
                std::array::from_fn(|k| {
                    let a = PI * (k as f64 + 0.5) * (2.0 * n as f64 - 255.0) / 128.0;
                    [(a.cos() / 64.0) as f32, (a.sin() / 64.0) as f32]
                })
            })
            .collect(),
        syn_ds: (0..64)
            .map(|n| {
                std::array::from_fn(|k| {
                    let a = PI * (k as f64 + 0.5) * (2.0 * n as f64 - 127.5) / 64.0;
                    [(a.cos() / 64.0) as f32, (a.sin() / 64.0) as f32]
                })
            })
            .collect(),
    })
}

fn huff(b: &mut Bits, table: usize) -> Result<i32, Error> {
    Ok(statics().trees[table].decode(b)? as i32 - tab::LAV[table])
}

/// One channel's SBR data and filter-bank state.
struct Channel {
    frame_class: u32,
    add_harmonic_flag: bool,
    num_env: usize,
    freq_res: [u8; 7],
    num_noise: usize,
    df_env: [u8; 5],
    df_noise: [u8; 2],
    invf_mode: [[u8; 5]; 2],
    add_harmonic: [u8; 48],
    amp_res: u8,
    /// Synthesis v (newest first) and analysis input (oldest first).
    v: Vec<f32>,
    x: Vec<f32>,
    e_a: [i32; 2],
    bw: [f32; 5],
    /// Analysis output of the last two frames: [buffer][slot][band].
    w: Box<[[[[f32; 2]; 32]; 32]; 2]>,
    ypos: usize,
    /// HF adjuster output of the last two frames: [buffer][slot][band].
    y: Box<[[[[f32; 2]; 64]; 38]; 2]>,
    g_temp: Box<[[f32; 48]; 42]>,
    q_temp: Box<[[f32; 48]; 42]>,
    s_indexmapped: [[u8; 48]; 8],
    env_facs_q: [[i32; 48]; 6],
    env_facs: [[f32; 48]; 6],
    noise_facs_q: [[i32; 5]; 3],
    noise_facs: [[f32; 5]; 3],
    t_env: [usize; 8],
    t_env_num_env_old: usize,
    t_q: [usize; 3],
    f_indexnoise: usize,
    f_indexsine: usize,
}

impl Channel {
    fn new() -> Channel {
        Channel {
            frame_class: 0,
            add_harmonic_flag: false,
            num_env: 0,
            freq_res: [0; 7],
            num_noise: 0,
            df_env: [0; 5],
            df_noise: [0; 2],
            invf_mode: [[0; 5]; 2],
            add_harmonic: [0; 48],
            amp_res: 0,
            v: vec![0.0; 1280],
            x: vec![0.0; 1312],
            e_a: [0, -1],
            bw: [0.0; 5],
            w: Box::new([[[[0.0; 2]; 32]; 32]; 2]),
            ypos: 0,
            y: Box::new([[[[0.0; 2]; 64]; 38]; 2]),
            g_temp: Box::new([[0.0; 48]; 42]),
            q_temp: Box::new([[0.0; 48]; 42]),
            s_indexmapped: [[0; 48]; 8],
            env_facs_q: [[0; 48]; 6],
            env_facs: [[0.0; 48]; 6],
            noise_facs_q: [[0; 5]; 3],
            noise_facs: [[0.0; 5]; 3],
            t_env: [0; 8],
            t_env_num_env_old: 0,
            t_q: [0; 3],
            f_indexnoise: 0,
            f_indexsine: 0,
        }
    }
}

/// Frequency-band parameters of the SBR header: start, stop, crossover,
/// scale, alter scale, noise bands (−1 before the first header).
type Spectrum = [i32; 6];

/// SBR of one channel element.
pub(crate) struct Sbr {
    /// SBR (output) sample rate: twice the core's.
    sample_rate: u32,
    start: bool,
    ready: bool,
    /// Element type of the data read (0 SCE, 1 CPE).
    id: u32,
    reset: bool,
    spectrum: Spectrum,
    amp_res_header: u8,
    limiter_bands: usize,
    limiter_gains: usize,
    interpol_freq: bool,
    smoothing_mode: bool,
    coupling: bool,
    k: [usize; 3],
    kx: [usize; 2],
    m: [usize; 2],
    kx_and_m_pushed: bool,
    n_master: usize,
    ch: [Channel; 2],
    n: [usize; 2],
    n_q: usize,
    n_lim: usize,
    f_master: [usize; 49],
    f_tablelow: [usize; 25],
    f_tablehigh: [usize; 49],
    f_tablenoise: [usize; 6],
    f_tablelim: [usize; 30],
    num_patches: usize,
    patch_num_subbands: [usize; 6],
    patch_start_subband: [usize; 6],
    x_low: Box<[[[f32; 2]; 40]; 32]>,
    x_high: Box<[[[f32; 2]; 40]; 64]>,
    /// Reconstructed QMF signal per channel: [re/im][slot][band].
    x: Box<[[[[f32; 64]; 38]; 2]; 2]>,
    alpha0: [[f32; 2]; 64],
    alpha1: [[f32; 2]; 64],
    e_origmapped: [[f32; 48]; 7],
    q_mapped: [[f32; 48]; 7],
    s_mapped: [[u8; 48]; 7],
    e_curr: [[f32; 48]; 7],
    q_m: [[f32; 48]; 7],
    s_m: [[f32; 48]; 7],
    gain: [[f32; 48]; 7],
    pub(crate) ps: Ps,
}

/// `num_bands` band widths from `start` to `stop` on a log scale.
fn make_bands(start: usize, stop: usize, num_bands: usize) -> Vec<i32> {
    let base = (stop as f32 / start as f32).powf(1.0 / num_bands as f32);
    let mut prod = start as f32;
    let mut previous = start as i32;
    let mut bands = Vec::with_capacity(num_bands);
    for _ in 0..num_bands.saturating_sub(1) {
        prod *= base;
        let present = prod.round_ties_even() as i32;
        bands.push(present - previous);
        previous = present;
    }
    bands.push(stop as i32 - previous);
    bands
}

impl Sbr {
    pub(crate) fn new(core_rate: u32) -> Sbr {
        let mut s = Sbr {
            sample_rate: 2 * core_rate,
            start: false,
            ready: false,
            id: 0,
            reset: false,
            spectrum: [-1; 6],
            amp_res_header: 0,
            limiter_bands: 0,
            limiter_gains: 0,
            interpol_freq: false,
            smoothing_mode: false,
            coupling: false,
            k: [0; 3],
            kx: [32, 32],
            m: [0; 2],
            kx_and_m_pushed: false,
            n_master: 0,
            ch: [Channel::new(), Channel::new()],
            n: [0; 2],
            n_q: 0,
            n_lim: 0,
            f_master: [0; 49],
            f_tablelow: [0; 25],
            f_tablehigh: [0; 49],
            f_tablenoise: [0; 6],
            f_tablelim: [0; 30],
            num_patches: 0,
            patch_num_subbands: [0; 6],
            patch_start_subband: [0; 6],
            x_low: Box::new([[[0.0; 2]; 40]; 32]),
            x_high: Box::new([[[0.0; 2]; 40]; 64]),
            x: Box::new([[[[0.0; 64]; 38]; 2]; 2]),
            alpha0: [[0.0; 2]; 64],
            alpha1: [[0.0; 2]; 64],
            e_origmapped: [[0.0; 48]; 7],
            q_mapped: [[0.0; 48]; 7],
            s_mapped: [[0; 48]; 7],
            e_curr: [[0.0; 48]; 7],
            q_m: [[0.0; 48]; 7],
            s_m: [[0.0; 48]; 7],
            gain: [[0.0; 48]; 7],
            ps: Ps::new(),
        };
        s.turn_off();
        s
    }

    #[cfg(test)]
    pub(crate) fn started(&self) -> bool {
        self.start
    }

    /// Pure upsampling until the next good header.
    fn turn_off(&mut self) {
        self.start = false;
        self.ready = false;
        self.kx[1] = 32;
        self.m[1] = 0;
        self.ch[0].e_a[1] = -1;
        self.ch[1].e_a[1] = -1;
        self.spectrum = [-1; 6];
    }

    /// Limiter band table (4.6.18.3.2.3).
    fn make_f_tablelim(&mut self) {
        if self.limiter_bands > 0 {
            const WARPED: [f32; 3] = [1.327_151_8, 1.185_092_8, 1.119_871_6];
            let warp = WARPED[self.limiter_bands - 1];
            let mut patch_borders = [0usize; 7];
            patch_borders[0] = self.kx[1];
            for k in 1..=self.num_patches {
                patch_borders[k] = patch_borders[k - 1] + self.patch_num_subbands[k - 1];
            }
            let n0 = self.n[0];
            let mut lim: Vec<usize> = self.f_tablelow[..=n0].to_vec();
            if self.num_patches > 1 {
                lim.extend_from_slice(&patch_borders[1..self.num_patches]);
            }
            lim.sort_unstable();
            let is_border = |v: usize| patch_borders[..=self.num_patches].contains(&v);
            let mut n_lim = n0 + self.num_patches - 1;
            let (mut out, mut inp) = (0usize, 1usize);
            while out < n_lim {
                if lim[inp] as f32 >= lim[out] as f32 * warp {
                    out += 1;
                    lim[out] = lim[inp];
                    inp += 1;
                } else if lim[inp] == lim[out] || !is_border(lim[inp]) {
                    inp += 1;
                    n_lim -= 1;
                } else if !is_border(lim[out]) {
                    lim[out] = lim[inp];
                    inp += 1;
                    n_lim -= 1;
                } else {
                    out += 1;
                    lim[out] = lim[inp];
                    inp += 1;
                }
            }
            self.n_lim = n_lim;
            for (d, &v) in self.f_tablelim.iter_mut().zip(&lim[..=n_lim]) {
                *d = v;
            }
        } else {
            self.f_tablelim[0] = self.f_tablelow[0];
            self.f_tablelim[1] = self.f_tablelow[self.n[0]];
            self.n_lim = 1;
        }
    }

    fn read_header(&mut self, b: &mut Bits) -> Result<(), Error> {
        let old_limiter = self.limiter_bands;
        let old = self.spectrum;
        self.start = true;
        self.ready = false;
        self.amp_res_header = b.u(1)? as u8;
        self.spectrum[0] = b.u(4)? as i32;
        self.spectrum[1] = b.u(4)? as i32;
        self.spectrum[2] = b.u(3)? as i32;
        b.u(2)?;
        let extra1 = b.u(1)? == 1;
        let extra2 = b.u(1)? == 1;
        if extra1 {
            self.spectrum[3] = b.u(2)? as i32;
            self.spectrum[4] = b.u(1)? as i32;
            self.spectrum[5] = b.u(2)? as i32;
        } else {
            self.spectrum[3] = 2;
            self.spectrum[4] = 1;
            self.spectrum[5] = 2;
        }
        if self.spectrum != old {
            self.reset = true;
        }
        if extra2 {
            self.limiter_bands = b.u(2)? as usize;
            self.limiter_gains = b.u(2)? as usize;
            self.interpol_freq = b.u(1)? == 1;
            self.smoothing_mode = b.u(1)? == 1;
        } else {
            self.limiter_bands = 2;
            self.limiter_gains = 2;
            self.interpol_freq = true;
            self.smoothing_mode = true;
        }
        if self.limiter_bands != old_limiter && !self.reset {
            self.make_f_tablelim();
        }
        Ok(())
    }

    /// Master frequency band table (4.6.18.3.2.1).
    fn make_f_master(&mut self) -> Option<()> {
        let sp = self.spectrum;
        let offsets = match self.sample_rate {
            16000 => &tab::OFFSET[0],
            22050 => &tab::OFFSET[1],
            24000 => &tab::OFFSET[2],
            32000 => &tab::OFFSET[3],
            44100 | 48000 | 64000 => &tab::OFFSET[4],
            88200 | 96000 | 128000 | 176400 | 192000 => &tab::OFFSET[5],
            _ => return None,
        };
        let sr = self.sample_rate;
        let temp: u32 = if sr < 32000 {
            3000
        } else if sr < 64000 {
            4000
        } else {
            5000
        };
        let start_min = ((temp << 7) + (sr >> 1)) / sr;
        let stop_min = ((temp << 8) + (sr >> 1)) / sr;
        let k0 = start_min as i32 + i32::from(offsets[usize::try_from(sp[0]).ok()?]);
        let mut k2 = match sp[1] {
            0..=13 => {
                let mut dk = make_bands(stop_min as usize, 64, 13);
                dk.sort_unstable();
                stop_min as i32 + dk[..sp[1] as usize].iter().sum::<i32>()
            }
            14 => 2 * k0,
            15 => 3 * k0,
            _ => return None,
        };
        k2 = k2.min(64);
        let max_qmf = if sr <= 32000 {
            48
        } else if sr == 44100 {
            35
        } else {
            32
        };
        if k0 <= 0 || k2 - k0 > max_qmf || k2 <= k0 {
            return None;
        }
        let (k0, k2) = (k0 as usize, k2 as usize);
        self.k[0] = k0;
        self.k[2] = k2;
        let xover = sp[2] as usize;
        if sp[3] == 0 {
            let dk = sp[4] as usize + 1;
            self.n_master = ((k2 - k0 + (dk & 2)) >> dk) << 1;
            if self.n_master == 0 || xover >= self.n_master {
                return None;
            }
            let mut f = [0i32; 49];
            for v in f.iter_mut().take(self.n_master + 1).skip(1) {
                *v = dk as i32;
            }
            let k2diff = k2 as i32 - k0 as i32 - (self.n_master * dk) as i32;
            if k2diff < 0 {
                f[1] -= 1;
                f[2] -= i32::from(k2diff < -1);
            } else if k2diff != 0 {
                f[self.n_master] += 1;
            }
            f[0] = k0 as i32;
            for k in 1..=self.n_master {
                f[k] += f[k - 1];
            }
            for (d, &v) in self.f_master.iter_mut().zip(&f) {
                *d = usize::try_from(v).ok()?;
            }
        } else {
            let half_bands = 7 - sp[3] as usize;
            let two_regions = 49 * k2 > 110 * k0;
            let k1 = if two_regions { 2 * k0 } else { k2 };
            self.k[1] = k1;
            let num0 =
                (half_bands as f32 * (k1 as f32 / k0 as f32).log2()).round_ties_even() as i32 * 2;
            if num0 <= 0 {
                return None;
            }
            let num0 = num0 as usize;
            let mut vk0 = make_bands(k0, k1, num0);
            vk0.sort_unstable();
            let vdk0_max = vk0[num0 - 1];
            let mut f0 = vec![k0 as i32];
            for &d in &vk0 {
                if d <= 0 {
                    return None;
                }
                f0.push(f0.last()? + d);
            }
            let mut all = f0;
            if two_regions {
                let invwarp = if sp[4] != 0 { 0.769_230_8 } else { 1.0f32 };
                let num1 = (half_bands as f32 * invwarp * (k2 as f32 / k1 as f32).log2())
                    .round_ties_even() as i32
                    * 2;
                if num1 <= 0 {
                    return None;
                }
                let num1 = num1 as usize;
                let mut vk1 = make_bands(k1, k2, num1);
                let vdk1_min = *vk1.iter().min()?;
                if vdk1_min < vdk0_max {
                    vk1.sort_unstable();
                    let change = (vdk0_max - vk1[0]).min((vk1[num1 - 1] - vk1[0]) >> 1);
                    vk1[0] += change;
                    vk1[num1 - 1] -= change;
                }
                vk1.sort_unstable();
                let mut at = k1 as i32;
                for &d in &vk1 {
                    if d <= 0 {
                        return None;
                    }
                    at += d;
                    all.push(at);
                }
                self.n_master = num0 + num1;
            } else {
                self.n_master = num0;
            }
            if self.n_master == 0 || xover >= self.n_master || all.len() > 49 {
                return None;
            }
            for (d, &v) in self.f_master.iter_mut().zip(&all) {
                *d = usize::try_from(v).ok()?;
            }
        }
        Some(())
    }

    /// Patch construction (4.6.18.6.3).
    fn calc_patches(&mut self) -> Option<()> {
        let mut last_k: i32 = -1;
        let mut last_msb: i32 = -1;
        let mut sb = 0usize;
        let mut msb = self.k[0];
        let mut usb = self.kx[1];
        let goal_sb = (((1000u32 << 11) + (self.sample_rate >> 1)) / self.sample_rate) as usize;
        self.num_patches = 0;
        let mut k = if goal_sb < self.kx[1] + self.m[1] {
            let mut k = 0;
            while self.f_master[k] < goal_sb {
                k += 1;
            }
            k
        } else {
            self.n_master
        };
        loop {
            let mut odd = 0;
            if k as i32 == last_k && msb as i32 == last_msb {
                return None;
            }
            last_k = k as i32;
            last_msb = msb as i32;
            let mut i = k as i32;
            loop {
                if !(i == k as i32 || sb > (self.k[0] - 1 + msb).saturating_sub(odd)) {
                    break;
                }
                if i < 0 {
                    return None;
                }
                sb = self.f_master[i as usize];
                odd = (sb + self.k[0]) & 1;
                i -= 1;
            }
            if self.num_patches > 5 {
                return None;
            }
            let n = sb.saturating_sub(usb);
            self.patch_num_subbands[self.num_patches] = n;
            self.patch_start_subband[self.num_patches] = (self.k[0] - odd).checked_sub(n)?;
            if n > 0 {
                usb = sb;
                msb = sb;
                self.num_patches += 1;
            } else {
                msb = self.kx[1];
            }
            if self.f_master[k] < sb + 3 {
                k = self.n_master;
            }
            if sb == self.kx[1] + self.m[1] {
                break;
            }
        }
        if self.num_patches > 1 && self.patch_num_subbands[self.num_patches - 1] < 3 {
            self.num_patches -= 1;
        }
        Some(())
    }

    /// Derived frequency tables (4.6.18.3.2.2).
    fn make_f_derived(&mut self) -> Option<()> {
        let xover = self.spectrum[2] as usize;
        self.n[1] = self.n_master - xover;
        self.n[0] = (self.n[1] + 1) >> 1;
        self.f_tablehigh[..=self.n[1]].copy_from_slice(&self.f_master[xover..=self.n_master]);
        self.m[1] = self.f_tablehigh[self.n[1]] - self.f_tablehigh[0];
        self.kx[1] = self.f_tablehigh[0];
        if self.kx[1] + self.m[1] > 64 || self.kx[1] > 32 {
            return None;
        }
        self.f_tablelow[0] = self.f_tablehigh[0];
        let odd = self.n[1] & 1;
        for k in 1..=self.n[0] {
            self.f_tablelow[k] = self.f_tablehigh[2 * k - odd];
        }
        let nq = (self.spectrum[5] as f32 * (self.k[2] as f32 / self.kx[1] as f32).log2())
            .round_ties_even() as i32;
        self.n_q = nq.max(1) as usize;
        if self.n_q > 5 {
            return None;
        }
        self.f_tablenoise[0] = self.f_tablelow[0];
        let mut temp = 0usize;
        for k in 1..=self.n_q {
            temp += (self.n[0] - temp) / (self.n_q + 1 - k);
            self.f_tablenoise[k] = self.f_tablelow[temp];
        }
        self.calc_patches()?;
        self.make_f_tablelim();
        self.ch[0].f_indexnoise = 0;
        self.ch[1].f_indexnoise = 0;
        Some(())
    }

    fn do_reset(&mut self) {
        if self
            .make_f_master()
            .and_then(|()| self.make_f_derived())
            .is_none()
        {
            self.turn_off();
        }
    }

    fn read_grid(&mut self, b: &mut Bits, c: usize) -> Result<bool, Error> {
        let ch = &mut self.ch[c];
        let mut pointer = 0usize;
        let mut abs_bord_trail = 16usize;
        let num_env_old = ch.num_env;
        ch.freq_res[0] = ch.freq_res[ch.num_env];
        ch.amp_res = self.amp_res_header;
        ch.t_env_num_env_old = ch.t_env[num_env_old];
        const CEIL_LOG2: [u32; 6] = [0, 1, 2, 2, 3, 3];
        let class = b.u(2)?;
        match class {
            0 => {
                let n = 1usize << b.u(2)?;
                if n > 4 {
                    return Ok(false);
                }
                ch.num_env = n;
                if n == 1 {
                    ch.amp_res = 0;
                }
                ch.t_env[0] = 0;
                ch.t_env[n] = abs_bord_trail;
                abs_bord_trail = (abs_bord_trail + (n >> 1)) / n;
                for i in 0..n - 1 {
                    ch.t_env[i + 1] = ch.t_env[i] + abs_bord_trail;
                }
                ch.freq_res[1] = b.u(1)? as u8;
                for i in 1..n {
                    ch.freq_res[i + 1] = ch.freq_res[1];
                }
            }
            1 => {
                abs_bord_trail += b.u(2)? as usize;
                let rel = b.u(2)? as usize;
                let n = rel + 1;
                ch.num_env = n;
                ch.t_env[0] = 0;
                ch.t_env[n] = abs_bord_trail;
                for i in 0..rel {
                    let d = 2 * b.u(2)? as usize + 2;
                    ch.t_env[n - 1 - i] = ch.t_env[n - i].saturating_sub(d);
                }
                pointer = b.u(CEIL_LOG2[n])? as usize;
                for i in 0..n {
                    ch.freq_res[n - i] = b.u(1)? as u8;
                }
            }
            2 => {
                ch.t_env[0] = b.u(2)? as usize;
                let rel = b.u(2)? as usize;
                let n = rel + 1;
                ch.num_env = n;
                ch.t_env[n] = abs_bord_trail;
                for i in 0..rel {
                    ch.t_env[i + 1] = ch.t_env[i] + 2 * b.u(2)? as usize + 2;
                }
                pointer = b.u(CEIL_LOG2[n])? as usize;
                for i in 0..n {
                    ch.freq_res[i + 1] = b.u(1)? as u8;
                }
            }
            _ => {
                ch.t_env[0] = b.u(2)? as usize;
                abs_bord_trail += b.u(2)? as usize;
                let lead = b.u(2)? as usize;
                let trail = b.u(2)? as usize;
                let n = lead + trail + 1;
                if n > 5 {
                    return Ok(false);
                }
                ch.num_env = n;
                ch.t_env[n] = abs_bord_trail;
                for i in 0..lead {
                    ch.t_env[i + 1] = ch.t_env[i] + 2 * b.u(2)? as usize + 2;
                }
                for i in 0..trail {
                    let d = 2 * b.u(2)? as usize + 2;
                    ch.t_env[n - 1 - i] = ch.t_env[n - i].saturating_sub(d);
                }
                pointer = b.u(CEIL_LOG2[n])? as usize;
                for i in 0..n {
                    ch.freq_res[i + 1] = b.u(1)? as u8;
                }
            }
        }
        ch.frame_class = class;
        let n = ch.num_env;
        if pointer > n + 1 {
            return Ok(false);
        }
        for i in 1..=n {
            if ch.t_env[i - 1] >= ch.t_env[i] {
                return Ok(false);
            }
        }
        ch.num_noise = usize::from(n > 1) + 1;
        ch.t_q[0] = ch.t_env[0];
        ch.t_q[ch.num_noise] = ch.t_env[n];
        if ch.num_noise > 1 {
            let idx = if class == FIXFIX {
                n >> 1
            } else if class & 1 == 1 {
                n - (pointer.saturating_sub(1)).max(1)
            } else if pointer == 0 {
                1
            } else if pointer == 1 {
                n - 1
            } else {
                pointer - 1
            };
            ch.t_q[1] = ch.t_env[idx];
        }
        ch.e_a[0] = -i32::from(ch.e_a[1] != num_env_old as i32);
        ch.e_a[1] = -1;
        if class & 1 == 1 && pointer != 0 {
            ch.e_a[1] = (n + 1 - pointer) as i32;
        } else if class == VARFIX && pointer > 1 {
            ch.e_a[1] = pointer as i32 - 1;
        }
        Ok(true)
    }

    /// Channel 1's grid copied from channel 0 (coupled stereo).
    fn copy_grid(&mut self) {
        let [src, dst] = &mut self.ch;
        dst.freq_res[0] = dst.freq_res[dst.num_env];
        dst.t_env_num_env_old = dst.t_env[dst.num_env];
        dst.e_a[0] = -i32::from(dst.e_a[1] != dst.num_env as i32);
        dst.freq_res[1..].copy_from_slice(&src.freq_res[1..]);
        dst.t_env = src.t_env;
        dst.t_q = src.t_q;
        dst.num_env = src.num_env;
        dst.amp_res = src.amp_res;
        dst.num_noise = src.num_noise;
        dst.frame_class = src.frame_class;
        dst.e_a[1] = src.e_a[1];
    }

    fn read_dtdf(&mut self, b: &mut Bits, c: usize) -> Result<(), Error> {
        let ch = &mut self.ch[c];
        for i in 0..ch.num_env {
            ch.df_env[i] = b.u(1)? as u8;
        }
        for i in 0..ch.num_noise {
            ch.df_noise[i] = b.u(1)? as u8;
        }
        Ok(())
    }

    fn read_invf(&mut self, b: &mut Bits, c: usize) -> Result<(), Error> {
        let n_q = self.n_q;
        let ch = &mut self.ch[c];
        ch.invf_mode[1] = ch.invf_mode[0];
        for i in 0..n_q {
            ch.invf_mode[0][i] = b.u(2)? as u8;
        }
        Ok(())
    }

    fn read_envelope(&mut self, b: &mut Bits, c: usize) -> Result<bool, Error> {
        let coupled = self.coupling && c == 1;
        let delta = if coupled { 2 } else { 1 };
        let odd = self.n[1] & 1;
        let n = self.n;
        let ch = &mut self.ch[c];
        let (bits, t, f) = match (coupled, ch.amp_res) {
            (true, 1) => (5, T_ENV_BAL_3_0, F_ENV_BAL_3_0),
            (true, _) => (6, T_ENV_BAL_1_5, F_ENV_BAL_1_5),
            (false, 1) => (6, T_ENV_3_0, F_ENV_3_0),
            (false, _) => (7, T_ENV_1_5, F_ENV_1_5),
        };
        for i in 0..ch.num_env {
            let res = usize::from(ch.freq_res[i + 1]);
            if ch.df_env[i] != 0 {
                for j in 0..n[res] {
                    let k = if ch.freq_res[i + 1] == ch.freq_res[i] {
                        j
                    } else if res == 1 {
                        (j + odd) >> 1
                    } else if j == 0 {
                        0
                    } else {
                        2 * j - odd
                    };
                    let v = ch.env_facs_q[i][k] + delta * huff(b, t)?;
                    if !(0..=127).contains(&v) {
                        return Ok(false);
                    }
                    ch.env_facs_q[i + 1][j] = v;
                }
            } else {
                ch.env_facs_q[i + 1][0] = delta * b.u(bits)? as i32;
                for j in 1..n[res] {
                    let v = ch.env_facs_q[i + 1][j - 1] + delta * huff(b, f)?;
                    if !(0..=127).contains(&v) {
                        return Ok(false);
                    }
                    ch.env_facs_q[i + 1][j] = v;
                }
            }
        }
        ch.env_facs_q[0] = ch.env_facs_q[ch.num_env];
        Ok(true)
    }

    fn read_noise(&mut self, b: &mut Bits, c: usize) -> Result<bool, Error> {
        let coupled = self.coupling && c == 1;
        let delta = if coupled { 2 } else { 1 };
        let (t, f) = if coupled {
            (T_NOISE_BAL_3_0, F_ENV_BAL_3_0)
        } else {
            (T_NOISE_3_0, F_ENV_3_0)
        };
        let n_q = self.n_q;
        let ch = &mut self.ch[c];
        for i in 0..ch.num_noise {
            if ch.df_noise[i] != 0 {
                for j in 0..n_q {
                    let v = ch.noise_facs_q[i][j] + delta * huff(b, t)?;
                    if !(0..=30).contains(&v) {
                        return Ok(false);
                    }
                    ch.noise_facs_q[i + 1][j] = v;
                }
            } else {
                ch.noise_facs_q[i + 1][0] = delta * b.u(5)? as i32;
                for j in 1..n_q {
                    let v = ch.noise_facs_q[i + 1][j - 1] + delta * huff(b, f)?;
                    if !(0..=30).contains(&v) {
                        return Ok(false);
                    }
                    ch.noise_facs_q[i + 1][j] = v;
                }
            }
        }
        ch.noise_facs_q[0] = ch.noise_facs_q[ch.num_noise];
        Ok(true)
    }

    fn read_harmonics(&mut self, b: &mut Bits, c: usize) -> Result<(), Error> {
        let n1 = self.n[1];
        let ch = &mut self.ch[c];
        ch.add_harmonic_flag = b.u(1)? == 1;
        if ch.add_harmonic_flag {
            for i in 0..n1 {
                ch.add_harmonic[i] = b.u(1)? as u8;
            }
        }
        Ok(())
    }

    /// `sbr_single_channel_element` / `sbr_channel_pair_element`; `false`
    /// for invalid data.
    fn read_element(&mut self, b: &mut Bits, id: u32) -> Result<bool, Error> {
        if id == 0 {
            if b.u(1)? == 1 {
                b.u(4)?;
            }
            if !self.read_grid(b, 0)? {
                return Ok(false);
            }
            self.read_dtdf(b, 0)?;
            self.read_invf(b, 0)?;
            if !self.read_envelope(b, 0)? || !self.read_noise(b, 0)? {
                return Ok(false);
            }
            self.read_harmonics(b, 0)?;
        } else {
            if b.u(1)? == 1 {
                b.u(8)?;
            }
            self.coupling = b.u(1)? == 1;
            if self.coupling {
                if !self.read_grid(b, 0)? {
                    return Ok(false);
                }
                self.copy_grid();
                self.read_dtdf(b, 0)?;
                self.read_dtdf(b, 1)?;
                self.read_invf(b, 0)?;
                self.ch[1].invf_mode[1] = self.ch[1].invf_mode[0];
                self.ch[1].invf_mode[0] = self.ch[0].invf_mode[0];
                if !self.read_envelope(b, 0)?
                    || !self.read_noise(b, 0)?
                    || !self.read_envelope(b, 1)?
                    || !self.read_noise(b, 1)?
                {
                    return Ok(false);
                }
            } else {
                if !self.read_grid(b, 0)? || !self.read_grid(b, 1)? {
                    return Ok(false);
                }
                self.read_dtdf(b, 0)?;
                self.read_dtdf(b, 1)?;
                self.read_invf(b, 0)?;
                self.read_invf(b, 1)?;
                if !self.read_envelope(b, 0)?
                    || !self.read_envelope(b, 1)?
                    || !self.read_noise(b, 0)?
                    || !self.read_noise(b, 1)?
                {
                    return Ok(false);
                }
            }
            self.read_harmonics(b, 0)?;
            self.read_harmonics(b, 1)?;
        }
        Ok(true)
    }

    fn read_data(&mut self, b: &mut Bits, id: u32, ps_allowed: bool) -> Result<(), Error> {
        self.id = id;
        self.ready = true;
        if id > 1 || !self.read_element(b, id)? {
            self.turn_off();
            return Ok(());
        }
        if b.u(1)? == 1 {
            let mut size = b.u(4)? as usize;
            if size == 15 {
                size += b.u(8)? as usize;
            }
            let mut left = size as i64 * 8;
            while left > 7 {
                left -= 2;
                let ext = b.u(2)?;
                if ext == 2 && ps_allowed {
                    let used = self.ps.read(b, left as usize);
                    b.skip(used)?;
                    left -= used as i64;
                } else {
                    b.skip(left as usize)?;
                    left = 0;
                }
            }
            if left > 0 {
                b.skip(left as usize)?;
            }
        }
        Ok(())
    }

    /// `sbr_extension_data` of a fill element (its `cnt` bytes after the
    /// 4-bit extension type) for element type `id` (0 SCE, 1 CPE).
    pub(crate) fn read(&mut self, b: &mut Bits, crc: bool, id: u32, ps_allowed: bool) {
        self.reset = false;
        self.kx[0] = self.kx[1];
        self.m[0] = self.m[1];
        self.kx_and_m_pushed = true;
        let r = (|| -> Result<(), Error> {
            if crc {
                b.u(10)?;
            }
            if b.u(1)? == 1 {
                self.read_header(b)?;
            }
            if self.reset {
                self.do_reset();
            }
            if self.start {
                self.read_data(b, id, ps_allowed)?;
            }
            Ok(())
        })();
        if r.is_err() {
            // Read past the element: the frame's SBR is unusable.
            self.turn_off();
        }
    }

    /// Dequantization and coupled-stereo decoding (4.6.18.3.5).
    fn dequant(&mut self, cpe: bool) {
        let exp2 = |q: i32| -> f32 { (q as f32).exp2() };
        let half = |q: i32| -> f32 {
            ((q >> 1) as f32).exp2()
                * if q & 1 == 1 {
                    std::f32::consts::SQRT_2
                } else {
                    1.0
                }
        };
        if cpe && self.coupling {
            let n = self.n;
            let n_q = self.n_q;
            let [c0, c1] = &mut self.ch;
            let pan = if c0.amp_res != 0 { 12 } else { 24 };
            for e in 1..=c0.num_env {
                for k in 0..n[usize::from(c0.freq_res[e])] {
                    let (mut t1, t2) = if c0.amp_res != 0 {
                        (
                            exp2(c0.env_facs_q[e][k] + 7),
                            exp2(pan - c1.env_facs_q[e][k]),
                        )
                    } else {
                        (
                            half(c0.env_facs_q[e][k] + 14),
                            half(pan - c1.env_facs_q[e][k]),
                        )
                    };
                    if t1 > 1e20 {
                        t1 = 1.0;
                    }
                    let fac = t1 / (1.0 + t2);
                    c0.env_facs[e][k] = fac;
                    c1.env_facs[e][k] = fac * t2;
                }
            }
            for e in 1..=c0.num_noise {
                for k in 0..n_q {
                    let t1 = exp2(NOISE_FLOOR_OFFSET - c0.noise_facs_q[e][k] + 1);
                    let t2 = exp2(12 - c1.noise_facs_q[e][k]);
                    let fac = t1 / (1.0 + t2);
                    c0.noise_facs[e][k] = fac;
                    c1.noise_facs[e][k] = fac * t2;
                }
            }
        } else {
            for c in 0..if cpe { 2 } else { 1 } {
                let n = self.n;
                let n_q = self.n_q;
                let ch = &mut self.ch[c];
                for e in 1..=ch.num_env {
                    for k in 0..n[usize::from(ch.freq_res[e])] {
                        let q = ch.env_facs_q[e][k];
                        let mut v = if ch.amp_res != 0 {
                            exp2(q + 6)
                        } else {
                            half(q + 12)
                        };
                        if v > 1e20 {
                            v = 1.0;
                        }
                        ch.env_facs[e][k] = v;
                    }
                }
                for e in 1..=ch.num_noise {
                    for k in 0..n_q {
                        ch.noise_facs[e][k] = exp2(NOISE_FLOOR_OFFSET - ch.noise_facs_q[e][k]);
                    }
                }
            }
        }
    }

    /// 32 slots of QMF analysis of 1024 core samples into `w[ypos]`.
    fn analysis(ch: &mut Channel, input: &[f32]) {
        let st = statics();
        let x = &mut ch.x;
        x.copy_within(1024..1312, 0);
        for (d, &v) in x[288..].iter_mut().zip(input) {
            *d = v * SCALE;
        }
        let w = &mut ch.w[ch.ypos];
        for (slot, ws) in w.iter_mut().enumerate() {
            let base = 32 * slot;
            let mut u = [0f32; 64];
            for (n, un) in u.iter_mut().enumerate() {
                let mut s = 0f32;
                for j in 0..5 {
                    let i = n + 64 * j;
                    s += tab::QMF_WINDOW[2 * i] * x[base + 319 - i];
                }
                *un = s;
            }
            for (k, wk) in ws.iter_mut().enumerate() {
                let m = &st.ana[k];
                let (mut re, mut im) = (0f32, 0f32);
                for (un, mk) in u.iter().zip(m) {
                    re += un * mk[0];
                    im += un * mk[1];
                }
                *wk = [re, im];
            }
        }
    }

    /// 32 slots of 64-band QMF synthesis of `x` into 2048 samples.
    fn synthesis(v: &mut [f32], x: &[[[f32; 64]; 38]; 2], out: &mut [f32]) {
        let st = statics();
        let c = &tab::QMF_WINDOW;
        for slot in 0..32 {
            v.copy_within(0..1152, 128);
            let (re, im) = (&x[0][slot], &x[1][slot]);
            for (n, vn) in v[..128].iter_mut().enumerate() {
                let m = &st.syn[n];
                let mut s = 0f32;
                for k in 0..64 {
                    s += re[k] * m[k][0] - im[k] * m[k][1];
                }
                *vn = s;
            }
            for j in 0..64 {
                let mut s = 0f32;
                for i in 0..5 {
                    s += v[256 * i + j] * c[128 * i + j];
                    s += v[256 * i + 192 + j] * c[128 * i + 64 + j];
                }
                out[64 * slot + j] = s / SCALE;
            }
        }
    }

    /// Downsampled synthesis (4.6.18.4.3): the lower 32 bands into 1024
    /// samples at the core rate, for SBR signalled at the core's rate.
    fn synthesis_ds(v: &mut [f32], x: &[[[f32; 64]; 38]; 2], out: &mut [f32]) {
        let st = statics();
        let c = &tab::QMF_WINDOW;
        for slot in 0..32 {
            v.copy_within(0..576, 64);
            let (re, im) = (&x[0][slot], &x[1][slot]);
            for (n, vn) in v[..64].iter_mut().enumerate() {
                let m = &st.syn_ds[n];
                let mut s = 0f32;
                for k in 0..32 {
                    s += re[k] * m[k][0] - im[k] * m[k][1];
                }
                *vn = s;
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

    /// Low band of the current and the previous frame (`X_low`).
    fn lf_gen(&mut self, c: usize) {
        let ch = &self.ch[c];
        let cur = &ch.w[ch.ypos];
        let prev = &ch.w[1 - ch.ypos];
        for row in self.x_low.iter_mut() {
            *row = [[0.0; 2]; 40];
        }
        for k in 0..self.kx[1] {
            for i in 8..40 {
                self.x_low[k][i] = cur[i - 8][k];
            }
        }
        for k in 0..self.kx[0] {
            for i in 0..8 {
                self.x_low[k][i] = prev[i + 24][k];
            }
        }
    }

    /// Inverse filtering coefficients (4.6.18.6.2).
    fn inverse_filter(&mut self) {
        for k in 0..self.k[0] {
            let x = &self.x_low[k];
            let mut r2 = x[0][0] * x[2][0] + x[0][1] * x[2][1];
            let mut i2 = x[0][0] * x[2][1] - x[0][1] * x[2][0];
            let (mut r1, mut i1, mut r0) = (0f32, 0f32, 0f32);
            for i in 1..38 {
                r0 += x[i][0] * x[i][0] + x[i][1] * x[i][1];
                r1 += x[i][0] * x[i + 1][0] + x[i][1] * x[i + 1][1];
                i1 += x[i][0] * x[i + 1][1] - x[i][1] * x[i + 1][0];
                r2 += x[i][0] * x[i + 2][0] + x[i][1] * x[i + 2][1];
                i2 += x[i][0] * x[i + 2][1] - x[i][1] * x[i + 2][0];
            }
            // phi[lag][...] as FFmpeg names them.
            let p0_1 = [r2, i2];
            let p2_1_0 = r0 + x[0][0] * x[0][0] + x[0][1] * x[0][1];
            let p1_0_0 = r0 + x[38][0] * x[38][0] + x[38][1] * x[38][1];
            let p1_1 = [
                r1 + x[0][0] * x[1][0] + x[0][1] * x[1][1],
                i1 + x[0][0] * x[1][1] - x[0][1] * x[1][0],
            ];
            let p0_0 = [
                r1 + x[38][0] * x[39][0] + x[38][1] * x[39][1],
                i1 + x[38][0] * x[39][1] - x[38][1] * x[39][0],
            ];
            let dk = p2_1_0 * p1_0_0 - (p1_1[0] * p1_1[0] + p1_1[1] * p1_1[1]) / 1.000001;
            let mut a1 = if dk == 0.0 {
                [0.0, 0.0]
            } else {
                [
                    (p0_0[0] * p1_1[0] - p0_0[1] * p1_1[1] - p0_1[0] * p1_0_0) / dk,
                    (p0_0[0] * p1_1[1] + p0_0[1] * p1_1[0] - p0_1[1] * p1_0_0) / dk,
                ]
            };
            let mut a0 = if p1_0_0 == 0.0 {
                [0.0, 0.0]
            } else {
                [
                    -(p0_0[0] + a1[0] * p1_1[0] + a1[1] * p1_1[1]) / p1_0_0,
                    -(p0_0[1] + a1[1] * p1_1[0] - a1[0] * p1_1[1]) / p1_0_0,
                ]
            };
            if a1[0] * a1[0] + a1[1] * a1[1] >= 16.0 || a0[0] * a0[0] + a0[1] * a0[1] >= 16.0 {
                a1 = [0.0, 0.0];
                a0 = [0.0, 0.0];
            }
            self.alpha0[k] = a0;
            self.alpha1[k] = a1;
        }
    }

    /// Chirp factors (4.6.18.6.2).
    fn chirp(&mut self, c: usize) {
        const BW: [f32; 4] = [0.0, 0.75, 0.9, 0.98];
        let ch = &mut self.ch[c];
        for i in 0..self.n_q {
            let mut bw = if ch.invf_mode[0][i] + ch.invf_mode[1][i] == 1 {
                0.6
            } else {
                BW[usize::from(ch.invf_mode[0][i])]
            };
            bw = if bw < ch.bw[i] {
                0.75 * bw + 0.25 * ch.bw[i]
            } else {
                0.90625 * bw + 0.09375 * ch.bw[i]
            };
            ch.bw[i] = if bw < 0.015625 { 0.0 } else { bw };
        }
    }

    /// High-band generation by patching (4.6.18.6.3).
    fn hf_gen(&mut self, c: usize) -> bool {
        let ch = &self.ch[c];
        let (start, end) = (2 * ch.t_env[0], 2 * ch.t_env[ch.num_env]);
        let mut g: i32 = 0;
        let mut k = self.kx[1];
        for j in 0..self.num_patches {
            for x in 0..self.patch_num_subbands[j] {
                let p = self.patch_start_subband[j] + x;
                while g <= self.n_q as i32 && k >= self.f_tablenoise[g as usize] {
                    g += 1;
                }
                g -= 1;
                if g < 0 || k >= 64 || p >= 32 {
                    return false;
                }
                let bw = ch.bw[g as usize];
                let a1 = self.alpha1[p];
                let a0 = self.alpha0[p];
                let a = [a1[0] * bw * bw, a1[1] * bw * bw, a0[0] * bw, a0[1] * bw];
                let lo = &self.x_low[p];
                let hi = &mut self.x_high[k];
                for i in start..end.min(38) {
                    let (i0, i1, i2) = (lo[i + ENV_OFF], lo[i + ENV_OFF - 1], lo[i + ENV_OFF - 2]);
                    hi[i + ENV_OFF] = [
                        i2[0] * a[0] - i2[1] * a[1] + i1[0] * a[2] - i1[1] * a[3] + i0[0],
                        i2[1] * a[0] + i2[0] * a[1] + i1[1] * a[2] + i1[0] * a[3] + i0[1],
                    ];
                }
                k += 1;
            }
        }
        for row in self.x_high.iter_mut().take(self.m[1] + self.kx[1]).skip(k) {
            *row = [[0.0; 2]; 40];
        }
        true
    }

    /// Envelope, noise and sinusoid parameters mapped to QMF bands
    /// (4.6.18.7.2).
    fn mapping(&mut self, c: usize) -> bool {
        let kx = self.kx[1];
        let ch = &mut self.ch[c];
        let e_a1 = ch.e_a[1];
        ch.s_indexmapped[1..8].fill([0; 48]);
        for e in 0..ch.num_env {
            let res = usize::from(ch.freq_res[e + 1]);
            let ilim = self.n[res];
            let table: &[usize] = if res == 1 {
                &self.f_tablehigh
            } else {
                &self.f_tablelow
            };
            if kx != table[0] {
                return false;
            }
            for i in 0..ilim {
                for m in table[i]..table[i + 1] {
                    self.e_origmapped[e][m - kx] = ch.env_facs[e + 1][i];
                }
            }
            let k = usize::from(ch.num_noise > 1 && ch.t_env[e] >= ch.t_q[1]);
            for i in 0..self.n_q {
                for m in self.f_tablenoise[i]..self.f_tablenoise[i + 1] {
                    self.q_mapped[e][m - kx] = ch.noise_facs[k + 1][i];
                }
            }
            for i in 0..self.n[1] {
                if ch.add_harmonic_flag {
                    let mid = (self.f_tablehigh[i] + self.f_tablehigh[i + 1]) >> 1;
                    ch.s_indexmapped[e + 1][mid - kx] = ch.add_harmonic[i]
                        * u8::from(e as i32 >= e_a1 || ch.s_indexmapped[0][mid - kx] == 1);
                }
            }
            for i in 0..ilim {
                let present =
                    (table[i]..table[i + 1]).any(|m| ch.s_indexmapped[e + 1][m - kx] != 0);
                for m in table[i]..table[i + 1] {
                    self.s_mapped[e][m - kx] = u8::from(present);
                }
            }
        }
        ch.s_indexmapped[0] = ch.s_indexmapped[ch.num_env];
        true
    }

    /// Energies of the generated high band per envelope (4.6.18.7.3).
    fn env_estimate(&mut self, c: usize) {
        let ch = &self.ch[c];
        let kx = self.kx[1];
        let sum_sq = |x: &[[f32; 2]]| -> f32 {
            let (mut s0, mut s1) = (0f32, 0f32);
            for p in x.chunks(2) {
                s0 += p[0][0] * p[0][0];
                s1 += p[0][1] * p[0][1];
                if let Some(q) = p.get(1) {
                    s0 += q[0] * q[0];
                    s1 += q[1] * q[1];
                }
            }
            s0 + s1
        };
        for e in 0..ch.num_env {
            let ilb = ch.t_env[e] * 2 + ENV_OFF;
            let iub = (ch.t_env[e + 1] * 2 + ENV_OFF).min(40);
            if self.interpol_freq {
                let recip = 0.5 / (ch.t_env[e + 1] - ch.t_env[e]) as f32;
                for m in 0..self.m[1] {
                    self.e_curr[e][m] = sum_sq(&self.x_high[m + kx][ilb..iub]) * recip;
                }
            } else {
                let env_size = 2 * (ch.t_env[e + 1] - ch.t_env[e]);
                let res = usize::from(ch.freq_res[e + 1]);
                let table: &[usize] = if res == 1 {
                    &self.f_tablehigh
                } else {
                    &self.f_tablelow
                };
                for p in 0..self.n[res] {
                    let den = (env_size * (table[p + 1] - table[p])) as f32;
                    let mut sum = 0f32;
                    for k in table[p]..table[p + 1] {
                        sum += sum_sq(&self.x_high[k][ilb..iub]);
                    }
                    sum /= den;
                    for k in table[p]..table[p + 1] {
                        self.e_curr[e][k - kx] = sum;
                    }
                }
            }
        }
    }

    /// Gains of the high band, noise and sinusoids with the limiter
    /// (4.6.18.7.4–5).
    fn gain_calc(&mut self, c: usize) {
        const LIMGAIN: [f32; 4] = [0.70795, 1.0, 1.41254, 10_000_000_000.0];
        let ch = &self.ch[c];
        let kx = self.kx[1];
        for e in 0..ch.num_env {
            let delta = f32::from(u8::from(!(e as i32 == ch.e_a[1] || e as i32 == ch.e_a[0])));
            for k in 0..self.n_lim {
                let (lo, hi) = (self.f_tablelim[k] - kx, self.f_tablelim[k + 1] - kx);
                for m in lo..hi {
                    let temp = self.e_origmapped[e][m] / (1.0 + self.q_mapped[e][m]);
                    self.q_m[e][m] = (temp * self.q_mapped[e][m]).sqrt();
                    self.s_m[e][m] = (temp * f32::from(ch.s_indexmapped[e + 1][m])).sqrt();
                    self.gain[e][m] = if self.s_mapped[e][m] == 0 {
                        (self.e_origmapped[e][m]
                            / ((1.0 + self.e_curr[e][m]) * (1.0 + self.q_mapped[e][m] * delta)))
                            .sqrt()
                    } else {
                        (self.e_origmapped[e][m] * self.q_mapped[e][m]
                            / ((1.0 + self.e_curr[e][m]) * (1.0 + self.q_mapped[e][m])))
                            .sqrt()
                    };
                    self.gain[e][m] += f32::MIN_POSITIVE;
                }
                let (mut s0, mut s1) = (0f32, 0f32);
                for m in lo..hi {
                    s0 += self.e_origmapped[e][m];
                    s1 += self.e_curr[e][m];
                }
                let gain_max = (LIMGAIN[self.limiter_gains]
                    * ((f32::EPSILON + s0) / (f32::EPSILON + s1)).sqrt())
                .min(100_000.0);
                for m in lo..hi {
                    let q_m_max = self.q_m[e][m] * gain_max / self.gain[e][m];
                    self.q_m[e][m] = self.q_m[e][m].min(q_m_max);
                    self.gain[e][m] = self.gain[e][m].min(gain_max);
                }
                let (mut s0, mut s1) = (0f32, 0f32);
                for m in lo..hi {
                    s0 += self.e_origmapped[e][m];
                    s1 += self.e_curr[e][m] * self.gain[e][m] * self.gain[e][m]
                        + self.s_m[e][m] * self.s_m[e][m]
                        + if delta != 0.0 && self.s_m[e][m] == 0.0 {
                            self.q_m[e][m] * self.q_m[e][m]
                        } else {
                            0.0
                        };
                }
                let boost = ((f32::EPSILON + s0) / (f32::EPSILON + s1))
                    .sqrt()
                    .min(1.584_893_2);
                for m in lo..hi {
                    self.gain[e][m] *= boost;
                    self.q_m[e][m] *= boost;
                    self.s_m[e][m] *= boost;
                }
            }
        }
    }

    /// The adjusted high band with noise and sinusoids into `y[ypos]`
    /// (4.6.18.7.5).
    fn hf_assemble(&mut self, c: usize) {
        const H_SMOOTH: [f32; 5] = [
            0.333_333_33,
            0.301_502_83,
            0.218_169_5,
            0.115_163_83,
            0.031_830_5,
        ];
        let h_sl = if self.smoothing_mode { 0 } else { 4 };
        let kx = self.kx[1];
        let m_max = self.m[1];
        let reset = self.reset;
        let ch = &mut self.ch[c];
        let mut indexnoise = ch.f_indexnoise;
        let mut indexsine = ch.f_indexsine;
        let t0 = 2 * ch.t_env[0];
        if reset {
            for i in 0..h_sl {
                ch.g_temp[i + t0][..m_max].copy_from_slice(&self.gain[0][..m_max]);
                ch.q_temp[i + t0][..m_max].copy_from_slice(&self.q_m[0][..m_max]);
            }
        } else if h_sl != 0 {
            let old = 2 * ch.t_env_num_env_old;
            for i in 0..4 {
                ch.g_temp[i + t0] = ch.g_temp[i + old];
                ch.q_temp[i + t0] = ch.q_temp[i + old];
            }
        }
        for e in 0..ch.num_env {
            for i in 2 * ch.t_env[e]..2 * ch.t_env[e + 1] {
                ch.g_temp[h_sl + i][..m_max].copy_from_slice(&self.gain[e][..m_max]);
                ch.q_temp[h_sl + i][..m_max].copy_from_slice(&self.q_m[e][..m_max]);
            }
        }
        let y1 = &mut ch.y[ch.ypos];
        for e in 0..ch.num_env {
            let edge = e as i32 == ch.e_a[0] || e as i32 == ch.e_a[1];
            for i in 2 * ch.t_env[e]..2 * ch.t_env[e + 1] {
                let mut g_filt = [0f32; 48];
                let mut q_filt = [0f32; 48];
                if h_sl != 0 && !edge {
                    let idx = i + h_sl;
                    for m in 0..m_max {
                        for (j, &h) in H_SMOOTH.iter().enumerate() {
                            g_filt[m] += ch.g_temp[idx - j][m] * h;
                            q_filt[m] += ch.q_temp[idx - j][m] * h;
                        }
                    }
                } else {
                    g_filt[..m_max].copy_from_slice(&ch.g_temp[i + h_sl][..m_max]);
                    // FFmpeg takes the unshifted row here.
                    q_filt[..m_max].copy_from_slice(&ch.q_temp[i][..m_max]);
                }
                let row = &mut y1[i];
                for m in 0..m_max {
                    let xh = self.x_high[kx + m][i + ENV_OFF];
                    row[kx + m] = [xh[0] * g_filt[m], xh[1] * g_filt[m]];
                }
                if !edge {
                    let (p0, p1) = match indexsine {
                        0 => (1.0, 0.0),
                        1 => (0.0, 1.0 - 2.0 * (kx & 1) as f32),
                        2 => (-1.0, 0.0),
                        _ => (0.0, -(1.0 - 2.0 * (kx & 1) as f32)),
                    };
                    let mut p1 = p1;
                    let mut noise = indexnoise;
                    for m in 0..m_max {
                        noise = (noise + 1) & 0x1ff;
                        let s = self.s_m[e][m];
                        let y = &mut row[kx + m];
                        if s != 0.0 {
                            y[0] += s * p0;
                            y[1] += s * p1;
                        } else {
                            y[0] += q_filt[m] * tab::NOISE[noise][0];
                            y[1] += q_filt[m] * tab::NOISE[noise][1];
                        }
                        p1 = -p1;
                    }
                } else {
                    let idx = indexsine & 1;
                    let a = 1.0 - (((indexsine + (kx & 1)) & 2) as f32);
                    let bsign = if idx == 1 { -a } else { a };
                    let mut m = 0;
                    while m + 1 < m_max {
                        row[kx + m][idx] += self.s_m[e][m] * a;
                        row[kx + m + 1][idx] += self.s_m[e][m + 1] * bsign;
                        m += 2;
                    }
                    if m_max & 1 == 1 {
                        row[kx + m][idx] += self.s_m[e][m] * a;
                    }
                }
                indexnoise = (indexnoise + m_max) & 0x1ff;
                indexsine = (indexsine + 1) & 3;
            }
        }
        ch.f_indexnoise = indexnoise;
        ch.f_indexsine = indexsine;
    }

    /// The QMF signal to synthesize: low band and the high band of the
    /// previous and current frame (4.6.18.8).
    fn x_gen(&mut self, c: usize) {
        let ch = &self.ch[c];
        let y0 = &ch.y[1 - ch.ypos];
        let y1 = &ch.y[ch.ypos];
        let i_temp = (2 * ch.t_env_num_env_old).saturating_sub(32).min(38);
        let x = &mut self.x[c];
        *x = [[[0.0; 64]; 38]; 2];
        let mut k = 0;
        while k < self.kx[0] {
            for i in 0..i_temp {
                let v = self.x_low[k][i + ENV_OFF];
                x[0][i][k] = v[0];
                x[1][i][k] = v[1];
            }
            k += 1;
        }
        while k < (self.kx[0] + self.m[0]).min(64) {
            for i in 0..i_temp {
                let v = y0[(i + 32).min(37)][k];
                x[0][i][k] = v[0];
                x[1][i][k] = v[1];
            }
            k += 1;
        }
        let mut k = 0;
        while k < self.kx[1] {
            for i in i_temp..38 {
                let v = self.x_low[k][i + ENV_OFF];
                x[0][i][k] = v[0];
                x[1][i][k] = v[1];
            }
            k += 1;
        }
        while k < (self.kx[1] + self.m[1]).min(64) {
            for i in i_temp..32 {
                let v = y1[i][k];
                x[0][i][k] = v[0];
                x[1][i][k] = v[1];
            }
            k += 1;
        }
    }

    /// Apply SBR to one element's 1024 core samples per channel (`left`,
    /// and `right` for a pair or with PS), giving 2048 per channel (1024
    /// with `down`: downsampled synthesis). `id`: 0 SCE, 1 CPE, 3 LFE.
    /// With `ps`, a mono element comes out as stereo in `out_r`.
    pub(crate) fn apply(
        &mut self,
        id: u32,
        core: [&[f32]; 2],
        ps: bool,
        down: bool,
        out_l: &mut [f32],
        out_r: &mut [f32],
    ) {
        let nch = if id == 1 { 2 } else { 1 };
        if id != self.id {
            self.turn_off();
        }
        if self.start && !self.ready {
            self.turn_off();
        }
        if !self.kx_and_m_pushed {
            self.kx[0] = self.kx[1];
            self.m[0] = self.m[1];
        } else {
            self.kx_and_m_pushed = false;
        }
        if self.start {
            self.dequant(id == 1);
            self.ready = false;
        }
        for c in 0..nch {
            Self::analysis(&mut self.ch[c], core[c]);
            self.lf_gen(c);
            self.ch[c].ypos ^= 1;
            if self.start {
                self.inverse_filter();
                self.chirp(c);
                if self.hf_gen(c) && self.mapping(c) {
                    self.env_estimate(c);
                    self.gain_calc(c);
                    self.hf_assemble(c);
                }
            }
            self.x_gen(c);
        }
        let mut stereo = nch == 2;
        if ps {
            let top = self.kx[1] + self.m[1];
            let [l, r] = &mut *self.x;
            if self.ps.started() {
                self.ps.apply(l, r, top);
            } else {
                *r = *l;
            }
            stereo = true;
        }
        let synth = if down {
            Self::synthesis_ds
        } else {
            Self::synthesis
        };
        synth(&mut self.ch[0].v, &self.x[0], out_l);
        if stereo {
            synth(&mut self.ch[1].v, &self.x[1], out_r);
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
                assert_eq!(st.trees[t].decode(&mut b).unwrap(), i, "table {t}");
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
