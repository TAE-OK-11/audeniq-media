//! Is the music video's sound the released master? Both are decoded to mono
//! at 11.025 kHz (enough for the musical content, and the same for a lossy
//! MV track and a lossless master), aligned, and compared.
//!
//! - Alignment: the onsets (rises of 24 log-spaced band energies, 46 ms
//!   frames) are cross-correlated band by band over every lag and summed;
//!   the eight strongest lags are then scored by the waveforms' normalized
//!   correlation (sample-exact, ±0.5 s) at the start, middle and end of the
//!   overlap, which also settles beat-periodic ambiguity. Offsets at
//!   the start and end of the overlap give the drift (a speed change, as
//!   from a 24 → 23.976 fps conversion).
//! - Null test: in 3 s segments, the MV sound minus the gain-matched master
//!   (each segment re-aligned within ±4 ms of the drift line). Its SNR is
//!   about 30 dB or more for the same master through a lossy codec, and a
//!   few dB for a different mix of the same song.
//! - Level and tone: the gain between the two, and how far the per-band
//!   levels differ beyond it (a different EQ or mastering).
//!
//! Masters are WAV/AIFF/FLAC/ALAC/WavPack/TTA (`audeniq-qc`'s reader) or
//! MP4/MOV/M4A (this crate's sound decoder).
use crate::Error;
use media_dsp::Fft;
use std::path::Path;

/// Analysis rate.
pub const RATE: u32 = 11025;
const FRAME: usize = 2048;
const HOP: usize = 512;
const BANDS: usize = 24;
const SEGMENT: usize = 3 * RATE as usize;
/// Re-alignment search per segment, samples (±4.4 ms).
const LOCAL: usize = 48;

/// The comparison (`video match-master`, `probe --master`).
#[derive(Debug, Clone, PartialEq)]
pub struct MasterMatch {
    /// `identical`, `same_master_level_differs`, `same_master_edited` (some
    /// sections differ: `differing`), `same_master_speed_differs`,
    /// `same_song_different_master` or `different`.
    pub verdict: &'static str,
    /// MV time minus master time of the same moment, seconds (this and the
    /// other alignment fields are 0 when the verdict is `different`).
    pub offset: f64,
    /// Speed difference, parts per million (MV runs slow when negative).
    pub drift_ppm: f64,
    /// MV level relative to the master, dB.
    pub gain_db: f64,
    /// SNR of the gain-matched difference over the matched segments, dB.
    pub null_snr_db: Option<f64>,
    /// Largest per-band level difference beyond the gain, dB.
    pub eq_difference_db: Option<f64>,
    /// Normalized waveform correlation at the offset (median of three
    /// 8 s windows; 1 for the same recording).
    pub alignment: f64,
    /// Normalized band-onset correlation at the offset: the musical
    /// match, which survives a different mix or a speed change.
    pub onset_alignment: f64,
    pub mv_duration: f64,
    pub master_duration: f64,
    /// Seconds both cover.
    pub overlap: f64,
    /// MV sound before and after the master's span (intro, outro), seconds.
    pub mv_extra: [f64; 2],
    /// Master missing from the MV at its start and end, seconds.
    pub master_missing: [f64; 2],
    /// Share of the non-silent overlap whose segments match (SNR ≥ 15 dB).
    pub matched_ratio: f64,
    /// MV time ranges (seconds) that do not match the master.
    pub differing: Vec<[f64; 2]>,
}
media_json::json_struct!(MasterMatch {
    verdict,
    offset,
    drift_ppm,
    gain_db,
    null_snr_db,
    eq_difference_db,
    alignment,
    onset_alignment,
    mv_duration,
    master_duration,
    overlap,
    mv_extra,
    master_missing,
    matched_ratio,
    differing
});

/// Mono at [`RATE`] from interleaved blocks of `channels`.
struct Mono {
    resampler: media_resample::Resampler,
    channels: usize,
    out: Vec<f32>,
    block: Vec<f32>,
}

impl Mono {
    fn new(rate: u32, channels: usize) -> Mono {
        Mono {
            resampler: media_resample::Resampler::new(rate, RATE, 1),
            channels: channels.max(1),
            out: Vec::new(),
            block: Vec::new(),
        }
    }
    fn push(&mut self, interleaved: &[f32]) {
        let k = 1.0 / self.channels as f32;
        self.block.clear();
        self.block.extend(
            interleaved
                .chunks_exact(self.channels)
                .map(|f| f.iter().sum::<f32>() * k),
        );
        self.resampler.push(&self.block, &mut self.out);
    }
    fn finish(mut self) -> Vec<f32> {
        self.resampler.finish(&mut self.out);
        self.out
    }
}

/// The sound of `path` as mono at [`RATE`]: an MP4/MOV through this crate,
/// anything else through `audeniq-qc`'s reader.
pub fn load(path: &Path) -> Result<Vec<f32>, Error> {
    let mut mono: Option<Mono> = None;
    let mut pending = Vec::new();
    match crate::decode_sound_blocks(path, &mut |b| pending.extend_from_slice(b)) {
        Ok((rate, names)) => {
            let mut m = Mono::new(rate, names.len());
            m.push(&pending);
            return Ok(m.finish());
        }
        Err(Error::Io(e)) => return Err(Error::Io(e)),
        Err(_) => {}
    }
    let limits = audeniq_qc::Limits::default();
    let backend = audeniq_qc::kernels::Backend::detect();
    let mut reader = audeniq_qc::audio::AudioReader::open(path, limits)
        .map_err(|e| Error::Invalid(format!("master: {e}")))?;
    let (rate, channels) = (reader.spec.sample_rate, usize::from(reader.spec.channels));
    let mut samples = Vec::new();
    let mut block = Vec::new();
    while reader
        .next(&mut samples, backend)
        .map_err(|e| Error::Invalid(format!("master: {e}")))?
    {
        block.clear();
        block.extend(samples.iter().map(|&v| v as f32 / 2_147_483_648.0));
        mono.get_or_insert_with(|| Mono::new(rate, channels))
            .push(&block);
        samples.clear();
    }
    Ok(mono.map(Mono::finish).unwrap_or_default())
}

/// Log band energies (dB) per frame, 24 bands from 60 Hz to 5 kHz.
fn bands(x: &[f32]) -> Vec<[f32; BANDS]> {
    let fft = Fft::new(FRAME);
    let window: Vec<f32> = (0..FRAME)
        .map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / FRAME as f64).cos()) as f32)
        .collect();
    let bin = |hz: f64| (hz / f64::from(RATE) * FRAME as f64).round() as usize;
    let edges: Vec<usize> = (0..=BANDS)
        .map(|b| bin(60.0 * (5000.0f64 / 60.0).powf(b as f64 / BANDS as f64)))
        .collect();
    let (mut re, mut im) = (vec![0f32; FRAME], vec![0f32; FRAME]);
    let frames = x.len().saturating_sub(FRAME) / HOP + 1;
    (0..if x.len() >= FRAME { frames } else { 0 })
        .map(|f| {
            let s = &x[f * HOP..f * HOP + FRAME];
            for ((r, &v), &w) in re.iter_mut().zip(s).zip(&window) {
                *r = v * w;
            }
            im.fill(0.0);
            fft.forward(&mut re, &mut im);
            let mut out = [0f32; BANDS];
            for (b, o) in out.iter_mut().enumerate() {
                let (lo, hi) = (edges[b], edges[b + 1].max(edges[b] + 1));
                let e: f32 = (lo..hi).map(|k| re[k] * re[k] + im[k] * im[k]).sum();
                *o = 10.0 * (e + 1e-9).log10();
            }
            out
        })
        .collect()
}

/// Each band's rises per frame, zero mean and unit variance (silent bands
/// stay zero).
fn onsets(b: &[[f32; BANDS]]) -> Vec<Vec<f32>> {
    (0..BANDS)
        .map(|k| {
            let mut o: Vec<f32> = b.windows(2).map(|w| (w[1][k] - w[0][k]).max(0.0)).collect();
            let n = o.len().max(1) as f32;
            let mean = o.iter().sum::<f32>() / n;
            let sd = (o.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / n).sqrt();
            let k = if sd > 1e-6 { 1.0 / sd } else { 0.0 };
            o.iter_mut().for_each(|v| *v = (*v - mean) * k);
            o
        })
        .collect()
}

/// Cross-correlations `c[k] = Σ a[t + k]·b[t]` summed over the pairs, for
/// every lag `k` in `-(b.len() - 1)..a.len()`: `out[k + b.len() - 1]`.
fn xcorr_sum<'a>(pairs: impl Iterator<Item = (&'a [f32], &'a [f32])> + Clone) -> Vec<f64> {
    let (na, nb) = pairs
        .clone()
        .fold((0, 0), |(x, y), (a, b)| (x.max(a.len()), y.max(b.len())));
    if na == 0 || nb == 0 {
        return Vec::new();
    }
    let n = (na + nb).next_power_of_two().max(4);
    let fft = Fft::new(n);
    let (mut sr, mut si) = (vec![0f32; n], vec![0f32; n]);
    let (mut ar, mut ai) = (vec![0f32; n], vec![0f32; n]);
    let (mut br, mut bi) = (vec![0f32; n], vec![0f32; n]);
    for (a, b) in pairs {
        ar.fill(0.0);
        ai.fill(0.0);
        br.fill(0.0);
        bi.fill(0.0);
        ar[..a.len()].copy_from_slice(a);
        br[..b.len()].copy_from_slice(b);
        fft.forward(&mut ar, &mut ai);
        fft.forward(&mut br, &mut bi);
        for k in 0..n {
            sr[k] += ar[k] * br[k] + ai[k] * bi[k];
            si[k] += ai[k] * br[k] - ar[k] * bi[k];
        }
    }
    fft.inverse(&mut sr, &mut si);
    (-(nb as i64 - 1)..na as i64)
        .map(|k| {
            let at = if k >= 0 {
                k as usize
            } else {
                (n as i64 + k) as usize
            };
            f64::from(sr[at]) / n as f64
        })
        .collect()
}

fn energy(x: &[f32]) -> f64 {
    x.iter().map(|&v| f64::from(v) * f64::from(v)).sum()
}

/// Waveform search window around a guess, samples (±0.5 s: drift up to
/// about 2000 ppm over four minutes).
const MARGIN: i64 = (RATE / 2) as i64;

/// The sample offset (MV index − master index) within ±[`MARGIN`] of
/// `guess` that best matches master samples `at..at + len`, with its
/// normalized correlation; `None` when the window falls outside.
fn fine_offset(
    mv: &[f32],
    master: &[f32],
    at: usize,
    len: usize,
    guess: i64,
) -> Option<(i64, f64)> {
    let len = len.min(master.len().saturating_sub(at));
    let start = (at as i64 + guess - MARGIN).max(0);
    let end = ((at as i64 + guess + MARGIN) as usize + len).min(mv.len());
    if len < RATE as usize || end <= start as usize + len {
        return None;
    }
    let y = &master[at..at + len];
    let x = &mv[start as usize..end];
    let c = xcorr_sum(std::iter::once((x, y)));
    let nb = y.len() as i64;
    let (lag, _) = (0..=(x.len() - y.len()) as i64)
        .map(|k| (k, c[(k + nb - 1) as usize]))
        .max_by(|p, q| p.1.total_cmp(&q.1))?;
    let xs = &x[lag as usize..lag as usize + y.len()];
    let norm = (energy(xs) * energy(y)).sqrt();
    let r = if norm > 0.0 {
        f64::from(media_dsp::dot(xs, y)) / norm
    } else {
        0.0
    };
    Some((start - at as i64 + lag, r))
}

/// Compare the MV sound with the master, both mono at [`RATE`].
pub fn compare(mv: &[f32], master: &[f32]) -> MasterMatch {
    let secs = |n: f64| n / f64::from(RATE);
    let (mv_duration, master_duration) = (secs(mv.len() as f64), secs(master.len() as f64));
    let mut m = MasterMatch {
        verdict: "different",
        offset: 0.0,
        drift_ppm: 0.0,
        gain_db: 0.0,
        null_snr_db: None,
        eq_difference_db: None,
        alignment: 0.0,
        onset_alignment: 0.0,
        mv_duration,
        master_duration,
        overlap: 0.0,
        mv_extra: [0.0; 2],
        master_missing: [0.0; 2],
        matched_ratio: 0.0,
        differing: Vec::new(),
    };
    let (bm, bs) = (bands(mv), bands(master));
    let (om, os) = (onsets(&bm), onsets(&bs));
    let c = xcorr_sum(
        om.iter()
            .zip(&os)
            .map(|(a, b)| (a.as_slice(), b.as_slice())),
    );
    if c.is_empty() {
        return m;
    }
    let nb = os[0].len() as i64;
    // The strongest local maxima, then the waveforms decide among them.
    let mut peaks: Vec<(i64, f64)> = (1..c.len().saturating_sub(1))
        .filter(|&i| c[i] >= c[i - 1] && c[i] > c[i + 1])
        .map(|i| (i as i64 - (nb - 1), c[i]))
        .collect();
    peaks.sort_by(|p, q| q.1.total_cmp(&p.1));
    peaks.truncate(8);
    let window = 8 * RATE as usize;
    let score = |lag: i64| -> Option<(i64, f64)> {
        let guess = lag * HOP as i64;
        let first = (-guess).max(0) as usize;
        let last = (mv.len() as i64 - guess).clamp(0, master.len() as i64) as usize;
        // Windows a search margin inside the overlap, so the search is
        // not cut off at either end.
        let edge = 2 * MARGIN as usize;
        if last < first + window + 2 * edge {
            return None;
        }
        let spots = [
            first + edge,
            (first + last - window) / 2,
            last - window - edge,
        ];
        let found: Vec<(i64, f64)> = spots
            .iter()
            .filter_map(|&at| fine_offset(mv, master, at, window, guess))
            .collect();
        // The median window: two agreeing windows outvote one that falls
        // in a section the MV changed.
        let mut by_corr = found.clone();
        by_corr.sort_by(|p, q| p.1.total_cmp(&q.1));
        let median = *by_corr.get(by_corr.len() / 2)?;
        Some((median.0, median.1))
    };
    let onset_norm = om.iter().map(|o| energy(o)).sum::<f64>().sqrt()
        * os.iter().map(|o| energy(o)).sum::<f64>().sqrt();
    let best = peaks
        .iter()
        .filter_map(|&(lag, _)| score(lag))
        .max_by(|p, q| p.1.total_cmp(&q.1));
    // Waveforms that do not line up (a speed change smears them over a
    // window): keep the strongest onset lag; the drift comes from onsets.
    let (mid, corr) = match best {
        Some(b) if b.1 >= 0.3 => b,
        _ => match peaks.first() {
            Some(&(lag, _)) => (lag * HOP as i64, best.map_or(0.0, |b| b.1)),
            None => return m,
        },
    };
    m.alignment = corr.max(0.0);
    let lag = (mid as f64 / HOP as f64).round() as i64;
    m.onset_alignment =
        c[(lag + nb - 1).clamp(0, c.len() as i64 - 1) as usize] / onset_norm.max(1e-12);
    let first = (-mid).max(0) as usize;
    let last = (mv.len() as i64 - mid).clamp(0, master.len() as i64) as usize;
    if last <= first + SEGMENT {
        return m;
    }
    // Drift: the onsets of up to a minute at each end of the overlap give
    // the offset line to a fraction of a frame; the waveforms there refine
    // it to a sample when they line up and agree with it.
    let edge = 2 * MARGIN as usize;
    let (mut drift, mut base, mut at0) = (0.0, mid as f64, first as f64);
    let span = (60 * RATE as usize / HOP).min((last - first) / HOP / 2);
    if last - first >= 40 * RATE as usize {
        let lag_at = |from: usize| -> Option<f64> {
            let f0 = from / HOP;
            let n = os[0].len();
            let (lo, hi) = (f0.min(n), (f0 + span).min(n));
            if hi <= lo {
                return None;
            }
            let pairs = om.iter().zip(&os).map(|(a, b)| (a.as_slice(), &b[lo..hi]));
            let c = xcorr_sum(pairs);
            let nb = (hi - lo) as i64;
            // Within ±2 s of the overall lag.
            let near = 2 * RATE as i64 / HOP as i64;
            let at = |k: i64| {
                let i = k + nb - 1;
                (i >= 0 && (i as usize) < c.len()).then(|| c[i as usize])
            };
            let k = ((lag + f0 as i64 - near)..=(lag + f0 as i64 + near))
                .filter(|&k| at(k).is_some())
                .max_by(|&x, &y| at(x).unwrap().total_cmp(&at(y).unwrap()))?;
            // Parabolic peak: a fraction of a frame.
            let frac = match (at(k - 1), at(k), at(k + 1)) {
                (Some(a), Some(b), Some(c)) if a - 2.0 * b + c < 0.0 => {
                    (0.5 * (a - c) / (a - 2.0 * b + c)).clamp(-0.5, 0.5)
                }
                _ => 0.0,
            };
            Some(((k - f0 as i64) as f64 + frac) * HOP as f64)
        };
        // The last onset frame that a whole frame of master covers.
        let tail_on = (last - span * HOP).min(os[0].len().saturating_sub(span) * HOP);
        if let (Some(a), Some(b)) = (lag_at(first), lag_at(tail_on)) {
            // Each onset window's offset stands for its middle.
            let (ta, tb) = (first + span * HOP / 2, tail_on + span * HOP / 2);
            drift = (b - a) / (tb - ta) as f64;
            base = a;
            at0 = ta as f64;
            let line = |t: usize| base + drift * (t as f64 - at0);
            // Short windows: a speed change smears the waveforms less.
            let short = 3 * RATE as usize;
            let (head, tail) = (first + edge, last - short - edge);
            let (mh, mt) = (head + short / 2, tail + short / 2);
            if let (Some(x), Some(y)) = (
                fine_offset(mv, master, head, short, line(mh).round() as i64),
                fine_offset(mv, master, tail, short, line(mt).round() as i64),
            ) && x.1 > 0.4
                && y.1 > 0.4
                && (x.0 as f64 - line(mh)).abs() < HOP as f64
                && (y.0 as f64 - line(mt)).abs() < HOP as f64
            {
                drift = (y.0 - x.0) as f64 / (mt - mh) as f64;
                base = x.0 as f64;
                at0 = mh as f64;
            }
            if drift.abs() < 20e-6 {
                drift = 0.0;
            }
        }
    }
    m.offset = secs(mid as f64);
    m.drift_ppm = drift * 1e6;
    let first = (-(mid.min(0))) as usize;
    let last = ((mv.len() as i64 - mid).max(0) as usize).min(master.len());
    m.overlap = secs(last.saturating_sub(first) as f64);
    m.master_missing = [secs(first as f64), secs((master.len() - last) as f64)];
    m.mv_extra = [
        secs(mid.max(0) as f64),
        secs((mv.len() as i64 - (last as i64 + mid)).max(0) as f64),
    ];
    // Null test per segment.
    let (mut sxy, mut sxx, mut syy) = (0.0f64, 0.0f64, 0.0f64);
    let (mut voiced, mut matched) = (0usize, 0usize);
    let mut bad: Vec<[f64; 2]> = Vec::new();
    // Master sample ranges of the matching segments.
    let mut good: Vec<(usize, usize)> = Vec::new();
    let mut t = first;
    while t + SEGMENT <= last {
        let off = (base + drift * (t as f64 - at0)).round() as i64;
        let y = &master[t..t + SEGMENT];
        let yy = energy(y);
        let x_at = |l: i64| -> Option<&[f32]> {
            let s = t as i64 + off + l;
            (s >= 0 && s as usize + SEGMENT <= mv.len())
                .then(|| &mv[s as usize..s as usize + SEGMENT])
        };
        let mut best: Option<(f64, f64, f64)> = None; // (corr², xy, xx)
        for l in -(LOCAL as i64)..=LOCAL as i64 {
            let Some(x) = x_at(l) else { continue };
            let xy = f64::from(media_dsp::dot(x, y));
            let xx = energy(x);
            let c = if xx * yy > 0.0 {
                xy * xy / (xx * yy)
            } else {
                0.0
            };
            if best.is_none_or(|b| c > b.0) {
                best = Some((c, xy, xx));
            }
        }
        let quiet = SEGMENT as f64 * 1e-8;
        if let Some((_, xy, xx)) = best
            && (xx > quiet || yy > quiet)
        {
            voiced += 1;
            let res = if yy > 0.0 { xx - xy * xy / yy } else { xx };
            let snr = 10.0 * (xx.max(1e-20) / res.max(xx * 1e-9).max(1e-20)).log10();
            if snr >= 15.0 && xy > 0.0 {
                matched += 1;
                good.push((t, t + SEGMENT));
                sxy += xy;
                sxx += xx;
                syy += yy;
            } else {
                let s = secs((t as i64 + off) as f64);
                match bad.last_mut() {
                    Some(r) if (s - r[1]).abs() < 0.5 => r[1] = s + secs(SEGMENT as f64),
                    _ => bad.push([s, s + secs(SEGMENT as f64)]),
                }
            }
        }
        t += SEGMENT;
    }
    m.matched_ratio = if voiced > 0 {
        matched as f64 / voiced as f64
    } else {
        0.0
    };
    m.differing = bad;
    if syy > 0.0 && sxy > 0.0 {
        let g = sxy / syy;
        m.gain_db = 20.0 * g.log10();
        let res = sxx - sxy * sxy / syy;
        m.null_snr_db = Some(10.0 * (sxx / res.max(sxx * 1e-9)).log10());
    } else {
        // No segment matches: the level from the overlap's energy.
        let x =
            &mv[(first as i64 + mid).max(0) as usize..((last as i64 + mid) as usize).min(mv.len())];
        let (xx, yy) = (energy(x), energy(&master[first..last]));
        if xx > 0.0 && yy > 0.0 {
            m.gain_db = 10.0 * (xx / yy).log10();
        }
    }
    // Per-band level difference beyond the gain, over frames both have
    // (only the matching segments' when some match, so an edited section
    // does not read as a different EQ).
    let mut diff = [0f64; BANDS];
    let mut n = 0usize;
    let in_good = |f: usize| {
        let at = f * HOP;
        good.is_empty() || good.iter().any(|&(a, b)| at >= a && at + FRAME <= b)
    };
    for (f, s) in bs.iter().enumerate() {
        let g = f as i64 + lag;
        if g < 0
            || g as usize >= bm.len()
            || !in_good(f)
            || s.iter().sum::<f32>() / (BANDS as f32) < -60.0
        {
            continue;
        }
        for k in 0..BANDS {
            diff[k] += f64::from(bm[g as usize][k] - s[k]);
        }
        n += 1;
    }
    if n > 0 {
        let mean = diff.iter().sum::<f64>() / (BANDS * n) as f64;
        m.eq_difference_db = Some(
            diff.iter()
                .map(|d| (d / n as f64 - mean).abs())
                .fold(0.0, f64::max),
        );
    }
    let same = m.matched_ratio >= 0.5
        && m.null_snr_db.is_some_and(|s| s >= 25.0)
        && m.eq_difference_db.is_some_and(|d| d <= 1.5);
    m.verdict = if same && m.matched_ratio < 0.98 {
        "same_master_edited"
    } else if same {
        if m.gain_db.abs() < 0.5 {
            "identical"
        } else {
            "same_master_level_differs"
        }
    } else if m.drift_ppm.abs() >= 200.0
        && m.onset_alignment >= 0.1
        && m.eq_difference_db.is_some_and(|d| d <= 1.5)
    {
        "same_master_speed_differs"
    } else if m.alignment >= 0.3 || m.onset_alignment >= 0.3 {
        "same_song_different_master"
    } else {
        "different"
    };
    if m.verdict == "different" {
        // No alignment to speak of: leave its numbers out.
        m.offset = 0.0;
        m.drift_ppm = 0.0;
        m.overlap = 0.0;
        m.mv_extra = [0.0; 2];
        m.master_missing = [0.0; 2];
        m.differing.clear();
    }
    m
}

/// [`load`] both files and [`compare`] them.
pub fn match_master(mv: &Path, master: &Path) -> Result<MasterMatch, Error> {
    let a = load(mv)?;
    let b = load(master)?;
    if a.len() < SEGMENT || b.len() < SEGMENT {
        return Err(Error::Invalid("sound shorter than 3 seconds".into()));
    }
    Ok(compare(&a, &b))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `secs` of music-like sound at [`RATE`]: irregular beats of decaying
    /// tones over shaped noise, from `seed`.
    fn music(secs: f64, seed: u32) -> Vec<f32> {
        let n = (secs * f64::from(RATE)) as usize;
        let mut s = seed;
        let mut rnd = move || {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (s >> 8) as f32 / (1 << 24) as f32
        };
        let mut x = vec![0f32; n];
        let mut t = 0usize;
        while t < n {
            let f = 110.0 * 2f32.powf((rnd() * 36.0).floor() / 12.0);
            let a = 0.1 + 0.2 * rnd();
            let len = (0.15 + 0.5 * rnd()) * RATE as f32;
            for i in 0..(len as usize).min(n - t) {
                let e = (-(i as f32) / (0.2 * len)).exp();
                x[t + i] += a * e * (2.0 * std::f32::consts::PI * f * i as f32 / RATE as f32).sin();
            }
            t += ((0.1 + 0.4 * rnd()) * RATE as f32) as usize;
        }
        let mut lp = 0f32;
        for v in &mut x {
            lp = 0.9 * lp + 0.1 * (rnd() - 0.5);
            *v += 0.2 * lp;
        }
        x
    }

    fn silence(secs: f64) -> Vec<f32> {
        vec![0.0; (secs * f64::from(RATE)) as usize]
    }

    #[test]
    fn the_same_master_quieter_and_late() {
        let master = music(60.0, 1);
        let mut mv = silence(2.5);
        mv.extend(master.iter().map(|v| v * 0.5));
        let m = compare(&mv, &master);
        assert_eq!(m.verdict, "same_master_level_differs");
        assert!((m.offset - 2.5).abs() < 1e-3, "{}", m.offset);
        assert!((m.gain_db + 6.02).abs() < 0.05, "{}", m.gain_db);
        assert!(m.null_snr_db.unwrap() > 60.0);
        assert!((m.mv_extra[0] - 2.5).abs() < 1e-3);
    }

    #[test]
    fn an_intro_and_an_identical_master() {
        let master = music(60.0, 2);
        let mut mv = music(4.0, 99);
        mv.extend(&master);
        let m = compare(&mv, &master);
        assert_eq!(m.verdict, "identical");
        assert!((m.offset - 4.0).abs() < 1e-3);
        assert!(m.differing.is_empty());
    }

    #[test]
    fn an_edited_section_is_located() {
        let master = music(60.0, 3);
        let mut mv = master.clone();
        let other = music(9.0, 77);
        let at = 30 * RATE as usize;
        mv[at..at + other.len()].copy_from_slice(&other);
        let m = compare(&mv, &master);
        assert_eq!(m.verdict, "same_master_edited");
        let [a, b] = m.differing[0];
        assert!(a <= 30.0 && b >= 39.0 && b - a <= 15.0, "{a}..{b}");
    }

    #[test]
    fn a_speed_change_is_measured() {
        let master = music(60.0, 4);
        // Played 0.1% fast: master read at 11036 Hz, written at 11025.
        let mut r = media_resample::Resampler::new(11036, RATE, 1);
        let mut mv = Vec::new();
        r.push(&master, &mut mv);
        r.finish(&mut mv);
        let m = compare(&mv, &master);
        assert_eq!(m.verdict, "same_master_speed_differs");
        assert!((m.drift_ppm + 997.0).abs() < 150.0, "{}", m.drift_ppm);
    }

    #[test]
    fn another_song_is_different() {
        let m = compare(&music(60.0, 5), &music(60.0, 6));
        assert_eq!(m.verdict, "different");
    }
}
