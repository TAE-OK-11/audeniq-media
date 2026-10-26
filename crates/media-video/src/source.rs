//! The delivery sound, decoded: which tracks and channels make it
//! ([`plan`]), each track's packets decoded to float PCM ([`Source`]) and
//! the picked channels put side by side ([`Mix`]). Measuring and converting
//! read the sound the same way.
//!
//! Decoders: AAC with `media-aac`, ALAC with `audeniq-qc`'s, MP3 with
//! `media-mp3`, integer and float PCM directly.
use crate::Error;
use crate::boxes::{child, path, u32_at};
use crate::file::Layout;
use crate::sound::{self, Chosen};
use std::io::{Read, Seek, SeekFrom};

/// How the delivery's channels come from the plan's tracks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Mapping {
    /// The one track as it is: mono, stereo, or more channels folded to
    /// stereo by their speaker layout.
    Whole,
    /// (track, channel) pairs, zero-based into the plan: two make left and
    /// right, one is mono.
    Pick(Vec<(usize, usize)>),
}

/// The tracks read and how their channels are used.
pub(crate) struct Plan<'a> {
    pub tracks: Vec<Chosen<'a>>,
    pub mapping: Mapping,
    /// `mono`, `stereo`, `downmix`, `dual-mono` or `channels` (picked).
    pub kind: &'static str,
}

impl Plan<'_> {
    /// (track id, one-based channel) of every source channel used.
    pub fn source_channels(&self) -> Vec<[u32; 2]> {
        match &self.mapping {
            Mapping::Whole => {
                let t = &self.tracks[0];
                (1..=t.cfg.channels).map(|c| [t.id, c]).collect()
            }
            Mapping::Pick(p) => p
                .iter()
                .map(|&(t, c)| [self.tracks[t].id, c as u32 + 1])
                .collect(),
        }
    }
}

/// Channel picks as the command line gives them: one-based positions in
/// the channels of all sound tracks, in file order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    Mono(u16),
    Stereo(u16, u16),
}

/// Whether a track's channels have known speakers (so more than two can be
/// folded to stereo).
fn speakers_known(c: &Chosen) -> bool {
    c.cfg.layout.is_some() || c.cfg.codec == "aac" || (c.cfg.channels == 6 && is_pcm(c))
}

fn is_pcm(c: &Chosen) -> bool {
    sound::pcm_frame(&c.cfg).is_some()
}

/// Which tracks and channels make the delivery sound. Without `pick`: the
/// first enabled sound track (or the only one); a mono track followed by another mono
/// track at the same rate is a left/right pair (editors export stereo that
/// way); more than two channels without a speaker layout (discrete stems)
/// give their first two. `None` when there is no sound track.
pub(crate) fn plan(moov: &[u8], pick: Option<Pick>) -> Result<Option<Plan<'_>>, Error> {
    let all = sound::audio_tracks(moov);
    if all.is_empty() {
        return Ok(None);
    }
    if let Some(p) = pick {
        let wanted: Vec<u16> = match p {
            Pick::Mono(a) => vec![a],
            Pick::Stereo(a, b) => vec![a, b],
        };
        let total: u32 = all.iter().map(|(_, c)| c.cfg.channels).sum();
        let mut at: Vec<(usize, usize)> = Vec::new();
        for &w in &wanted {
            let mut left = u32::from(w);
            let found = all.iter().enumerate().find_map(|(i, (_, c))| {
                if left >= 1 && left <= c.cfg.channels {
                    Some((i, left as usize - 1))
                } else {
                    left = left.saturating_sub(c.cfg.channels);
                    None
                }
            });
            at.push(found.ok_or_else(|| {
                Error::Invalid(format!(
                    "채널 {w}이(가) 없습니다(소리 트랙 채널은 모두 {total}개)"
                ))
            })?);
        }
        let mut order: Vec<usize> = Vec::new();
        for &(t, _) in &at {
            if !order.contains(&t) {
                order.push(t);
            }
        }
        let mapping = Mapping::Pick(
            at.iter()
                .map(|&(t, c)| (order.iter().position(|&o| o == t).unwrap_or(0), c))
                .collect(),
        );
        let mut tracks: Vec<Option<Chosen>> = all.into_iter().map(|(_, c)| Some(c)).collect();
        return Ok(Some(Plan {
            tracks: order.iter().filter_map(|&i| tracks[i].take()).collect(),
            mapping,
            kind: "channels",
        }));
    }
    let only = all.len() == 1;
    let Some(first) = all.iter().position(|(e, _)| *e || only) else {
        return Ok(None);
    };
    let mut all: Vec<Option<Chosen>> = all.into_iter().map(|(_, c)| Some(c)).collect();
    let c = all[first]
        .take()
        .ok_or_else(|| Error::Invalid("sound track".into()))?;
    let channels = c.cfg.channels;
    if channels == 1 {
        let partner = all
            .get(first + 1)
            .and_then(Option::as_ref)
            .is_some_and(|n| n.cfg.channels == 1 && n.cfg.sample_rate == c.cfg.sample_rate);
        if partner && let Some(n) = all[first + 1].take() {
            return Ok(Some(Plan {
                tracks: vec![c, n],
                mapping: Mapping::Pick(vec![(0, 0), (1, 0)]),
                kind: "dual-mono",
            }));
        }
    }
    let (mapping, kind) = match channels {
        1 => (Mapping::Whole, "mono"),
        2 => (Mapping::Whole, "stereo"),
        _ if speakers_known(&c) => (Mapping::Whole, "downmix"),
        _ => (Mapping::Pick(vec![(0, 0), (0, 1)]), "channels"),
    };
    Ok(Some(Plan {
        tracks: vec![c],
        mapping,
        kind,
    }))
}

/// Byte ranges (offset, length) of the plan's sound data, sorted and with
/// adjacent ones merged.
pub(crate) fn ranges(layout: &Layout, plan: &Plan) -> Result<Vec<(u64, u64)>, Error> {
    let mut all: Vec<(u64, u64)> = Vec::new();
    let mut total = 0u64;
    for c in &plan.tracks {
        for (off, sizes) in sound::reads(layout, c)? {
            let len: u64 = sizes.iter().map(|&s| u64::from(s)).sum();
            total = total.saturating_add(len);
            if len > 0 {
                all.push((off, len));
            }
        }
    }
    if total > sound::MAX_SOUND_BYTES * plan.tracks.len() as u64 {
        return Err(Error::Limit(format!(
            "sound data over {} bytes",
            sound::MAX_SOUND_BYTES
        )));
    }
    all.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::new();
    for (off, len) in all {
        match out.last_mut() {
            Some((o, l)) if *o + *l >= off => *l = (*l).max(off + len - *o),
            _ => out.push((off, len)),
        }
    }
    Ok(out)
}

/// Discrete channel names for PCM without a layout.
const DISCRETE: [&str; 16] = [
    "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15", "16",
];

enum Dec {
    Aac(Box<media_aac::Decoder>),
    Alac(Box<audeniq_qc::alac::Decoder>),
    Mp3(Box<media_mp3::Decoder>),
    Pcm(String),
}

/// How a source treats packets and the edit list.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Use {
    /// Measuring: every decoded sample, damaged packets left out (counted).
    Measure,
    /// Converting: the edit list applied, damaged packets become silence.
    Convert,
}

/// One track's sound, decoded chunk by chunk.
pub(crate) struct Source {
    reads: Vec<(u64, Vec<u32>)>,
    next: usize,
    dec: Dec,
    use_: Use,
    /// Sample rate and channel names of the decoded output.
    pub rate: u32,
    pub names: Vec<&'static str>,
    /// Edit list in output frames: leading silence, frames to drop, frames
    /// to keep after that (`None`: the rest).
    lead: u64,
    skip: u64,
    keep: Option<u64>,
    /// Frames of the last decoded packet (silence for a damaged one).
    last_frames: usize,
    /// Output decoded while opening.
    primed: Vec<f32>,
    pub packets: u64,
    pub errors: u64,
    buf: Vec<u8>,
    pcm_i: Vec<i32>,
    pcm_f: Vec<f32>,
}

fn invalid(s: impl Into<String>) -> Error {
    Error::Invalid(s.into())
}

/// The track's edit list: leading empty edits (movie timescale) and the
/// first media edit (media time, duration in movie timescale; `None` for
/// the rest of the media).
fn edit(trak: &[u8]) -> (u64, u64, Option<u64>) {
    use crate::boxes::u64_at;
    let Some(elst) = path(trak, &[b"edts", b"elst"]) else {
        return (0, 0, None);
    };
    let v1 = elst.first() == Some(&1);
    let n = u32_at(elst, 4).unwrap_or(0) as usize;
    let w = if v1 { 20 } else { 12 };
    let mut empty = 0u64;
    for i in 0..n.min(64) {
        let at = 8 + i * w;
        let (dur, time) = if v1 {
            (u64_at(elst, at), u64_at(elst, at + 8).map(|t| t as i64))
        } else {
            (
                u32_at(elst, at).map(u64::from),
                u32_at(elst, at + 4).map(|t| i64::from(t as i32)),
            )
        };
        let (Some(dur), Some(time)) = (dur, time) else {
            break;
        };
        if time < 0 {
            empty = empty.saturating_add(dur);
        } else {
            return (empty, time as u64, (dur > 0).then_some(dur));
        }
    }
    (empty, 0, None)
}

impl Source {
    /// Open the track's decoder; compressed sound is decoded up to the
    /// first good packet, which settles the output rate and channels
    /// (HE-AAC doubles the core rate).
    pub fn open<R: Read + Seek>(
        r: &mut R,
        layout: &Layout,
        c: &Chosen,
        movie_timescale: u32,
        use_: Use,
    ) -> Result<Source, Error> {
        let dec = match c.cfg.codec.as_str() {
            "aac" => Dec::Aac(Box::new(
                media_aac::Decoder::new(&c.cfg.asc)
                    .map_err(|e| invalid(format!("AAC를 디코딩할 수 없습니다: {e}")))?,
            )),
            "alac" => {
                let cookie: [u8; 24] = c
                    .cfg
                    .cookie
                    .as_slice()
                    .try_into()
                    .map_err(|_| invalid("ALAC 설정(magic cookie)이 없습니다"))?;
                let d = audeniq_qc::alac::Decoder::new(cookie);
                if !(1..=2).contains(&d.channels()) {
                    return Err(invalid(format!(
                        "{}채널 ALAC은 디코딩하지 않습니다(모노·스테레오만)",
                        d.channels()
                    )));
                }
                Dec::Alac(Box::new(d))
            }
            "mp3" => Dec::Mp3(Box::new(media_mp3::Decoder::new())),
            codec if sound::pcm_frame(&c.cfg).is_some() => Dec::Pcm(codec.to_string()),
            codec => return Err(invalid(format!("{codec} 소리는 아직 디코딩하지 않습니다"))),
        };
        let mut s = Source {
            reads: sound::reads(layout, c)?,
            next: 0,
            dec,
            use_,
            rate: c.cfg.sample_rate,
            names: Vec::new(),
            lead: 0,
            skip: 0,
            keep: None,
            last_frames: 1024,
            primed: Vec::new(),
            packets: 0,
            errors: 0,
            buf: Vec::new(),
            pcm_i: Vec::new(),
            pcm_f: Vec::new(),
        };
        match &s.dec {
            Dec::Pcm(_) => {
                let ch = c.cfg.channels as usize;
                s.names = match (&c.cfg.layout, ch) {
                    (Some(l), _) => l.clone(),
                    (None, 1) => vec!["C"],
                    (None, 2) => vec!["L", "R"],
                    (None, 6) => vec!["L", "R", "C", "LFE", "Ls", "Rs"],
                    (None, n) if n <= DISCRETE.len() => DISCRETE[..n].to_vec(),
                    (None, n) => return Err(invalid(format!("{n}채널 PCM은 디코딩하지 않습니다"))),
                };
            }
            _ => {
                // Decode until a packet gives sound; that output is kept.
                let mut primed = Vec::new();
                while s.names.is_empty() {
                    if !s.decode_chunk(r, &mut primed)? {
                        break;
                    }
                }
                s.primed = primed;
            }
        }
        if s.rate == 0 || s.names.is_empty() {
            return Err(invalid("소리 트랙의 샘플레이트·채널을 알 수 없습니다"));
        }
        if use_ == Use::Convert {
            let mdhd = path(c.trak, &[b"mdia", b"mdhd"]).ok_or_else(|| invalid("mdhd"))?;
            let media_ts = if mdhd.first() == Some(&1) {
                u32_at(mdhd, 20)
            } else {
                u32_at(mdhd, 12)
            }
            .filter(|&t| t > 0)
            .ok_or_else(|| invalid("mdhd timescale"))?;
            let (empty, media_time, dur) = edit(c.trak);
            let mt = u64::from(movie_timescale.max(1));
            let rate = u64::from(s.rate);
            let rescale =
                |v: u64, from: u64| (u128::from(v) * u128::from(rate) / u128::from(from)) as u64;
            s.lead = rescale(empty, mt).min(rate * 60);
            s.skip = rescale(media_time, u64::from(media_ts));
            s.keep = dur.map(|d| rescale(d, mt));
        }
        Ok(s)
    }

    pub fn channels(&self) -> usize {
        self.names.len()
    }

    /// Read and decode the next chunk, appending its samples to `out`
    /// unedited. `false` once every chunk was read.
    fn decode_chunk<R: Read + Seek>(
        &mut self,
        r: &mut R,
        out: &mut Vec<f32>,
    ) -> Result<bool, Error> {
        let Some((off, sizes)) = self.reads.get(self.next) else {
            return Ok(false);
        };
        self.next += 1;
        let len: usize = sizes.iter().map(|&s| s as usize).sum();
        self.buf.resize(len, 0);
        r.seek(SeekFrom::Start(*off))?;
        r.read_exact(&mut self.buf)?;
        let mut at = 0;
        for &size in sizes {
            let packet = &self.buf[at..at + size as usize];
            at += size as usize;
            self.packets += 1;
            self.pcm_f.clear();
            let ok = match &mut self.dec {
                Dec::Pcm(codec) => {
                    self.pcm_i.clear();
                    sound::pcm_to_i32(codec, packet, &mut self.pcm_i);
                    self.pcm_f
                        .extend(self.pcm_i.iter().map(|&v| v as f32 / 2_147_483_648.0));
                    true
                }
                Dec::Aac(d) => match d.decode(packet, &mut self.pcm_f) {
                    Ok(_) => {
                        if self.names.is_empty() {
                            self.rate = d.output_rate();
                            self.names = d.layout().to_vec();
                        }
                        true
                    }
                    Err(_) => false,
                },
                Dec::Alac(d) => {
                    self.pcm_i.clear();
                    match d.decode_any(packet, &mut self.pcm_i) {
                        Ok(()) => {
                            if self.names.is_empty() {
                                self.names = if d.channels() == 1 {
                                    vec!["C"]
                                } else {
                                    vec!["L", "R"]
                                };
                            }
                            self.pcm_f
                                .extend(self.pcm_i.iter().map(|&v| v as f32 / 2_147_483_648.0));
                            true
                        }
                        Err(_) => false,
                    }
                }
                Dec::Mp3(d) => {
                    let mut pcm = std::mem::take(&mut self.pcm_f);
                    pcm.clear();
                    let r = d.decode(packet, &mut pcm);
                    let ok = match r {
                        Ok(f) => {
                            if self.names.is_empty() {
                                self.rate = f.sample_rate;
                                self.names = if f.channels == 1 {
                                    vec!["C"]
                                } else {
                                    vec!["L", "R"]
                                };
                            }
                            // A stream may switch between mono and stereo
                            // frames; keep the first frame's channels.
                            let mut fixed = Vec::new();
                            media_mp3::to_channels(&pcm, f.channels, self.names.len(), &mut fixed);
                            pcm = fixed;
                            true
                        }
                        Err(_) => false,
                    };
                    self.pcm_f = pcm;
                    ok
                }
            };
            let ch = self.names.len().max(1);
            if ok {
                if !self.pcm_f.is_empty() {
                    self.last_frames = self.pcm_f.len() / ch;
                }
                out.extend_from_slice(&self.pcm_f);
            } else {
                self.errors += 1;
                // A damaged packet becomes silence when converting, so the
                // timing holds; before the first good packet there is no
                // channel count yet and it is left out.
                if self.use_ == Use::Convert && !self.names.is_empty() {
                    out.resize(out.len() + self.last_frames * ch, 0.0);
                }
            }
        }
        Ok(true)
    }

    /// Append the next decoded, edited samples to `out` (possibly none).
    /// `false` once the track is finished.
    pub fn pull<R: Read + Seek>(&mut self, r: &mut R, out: &mut Vec<f32>) -> Result<bool, Error> {
        let ch = self.channels();
        if self.lead > 0 {
            out.resize(out.len() + self.lead as usize * ch, 0.0);
            self.lead = 0;
            return Ok(true);
        }
        if self.keep == Some(0) {
            return Ok(false);
        }
        let mut pcm = std::mem::take(&mut self.primed);
        let more = if pcm.is_empty() {
            self.decode_chunk(r, &mut pcm)?
        } else {
            true
        };
        if !more {
            return Ok(false);
        }
        let mut frames = &pcm[..pcm.len() / ch * ch];
        let skip = (self.skip as usize).min(frames.len() / ch);
        frames = &frames[skip * ch..];
        self.skip -= skip as u64;
        if let Some(k) = &mut self.keep {
            let take = (*k as usize).min(frames.len() / ch);
            frames = &frames[..take * ch];
            *k -= take as u64;
        }
        out.extend_from_slice(frames);
        Ok(true)
    }
}

/// The plan's sources with their channels put together.
pub(crate) struct Mix {
    pub sources: Vec<Source>,
    mapping: Mapping,
    queues: Vec<Vec<f32>>,
    done: Vec<bool>,
}

impl Mix {
    pub fn open<R: Read + Seek>(
        r: &mut R,
        layout: &Layout,
        plan: &Plan,
        movie_timescale: u32,
        use_: Use,
    ) -> Result<Mix, Error> {
        let mut sources = Vec::with_capacity(plan.tracks.len());
        for c in &plan.tracks {
            sources.push(Source::open(r, layout, c, movie_timescale, use_)?);
        }
        if sources.iter().any(|s| s.rate != sources[0].rate) {
            return Err(invalid("합칠 소리 트랙의 샘플레이트가 서로 다릅니다"));
        }
        if let Mapping::Pick(p) = &plan.mapping
            && let Some(&(t, c)) = p.iter().find(|&&(t, c)| c >= sources[t].channels())
        {
            return Err(invalid(format!(
                "트랙 {}에 채널 {}이(가) 없습니다",
                plan.tracks[t].id,
                c + 1
            )));
        }
        let n = sources.len();
        Ok(Mix {
            sources,
            mapping: plan.mapping.clone(),
            queues: vec![Vec::new(); n],
            done: vec![false; n],
        })
    }

    pub fn rate(&self) -> u32 {
        self.sources[0].rate
    }

    /// Channel names of what [`Mix::pull`] gives.
    pub fn names(&self) -> Vec<&'static str> {
        match &self.mapping {
            Mapping::Whole => self.sources[0].names.clone(),
            Mapping::Pick(p) if p.len() == 1 => vec!["C"],
            Mapping::Pick(_) => vec!["L", "R"],
        }
    }

    /// Append the next samples (interleaved per [`Mix::names`]) to `out`.
    /// `false` once all sound is out.
    pub fn pull<R: Read + Seek>(&mut self, r: &mut R, out: &mut Vec<f32>) -> Result<bool, Error> {
        let picks = match &self.mapping {
            Mapping::Whole => return self.sources[0].pull(r, out),
            Mapping::Pick(p) => p.clone(),
        };
        loop {
            let frames: Vec<usize> = self
                .queues
                .iter()
                .zip(&self.sources)
                .map(|(q, s)| q.len() / s.channels())
                .collect();
            let all_done = self.done.iter().all(|&d| d);
            // Frames every source can give; a finished one gives silence.
            let ready = if all_done {
                frames.iter().copied().max().unwrap_or(0)
            } else {
                frames
                    .iter()
                    .zip(&self.done)
                    .filter(|(_, d)| !**d)
                    .map(|(&f, _)| f)
                    .min()
                    .unwrap_or(0)
            };
            if ready > 0 {
                out.reserve(ready * picks.len());
                for f in 0..ready {
                    for &(t, c) in &picks {
                        let ch = self.sources[t].channels();
                        out.push(self.queues[t].get(f * ch + c).copied().unwrap_or(0.0));
                    }
                }
                for (q, s) in self.queues.iter_mut().zip(&self.sources) {
                    let n = (ready * s.channels()).min(q.len());
                    q.drain(..n);
                }
                return Ok(true);
            }
            if all_done {
                return Ok(false);
            }
            // Feed the source that has the least waiting.
            let i = (0..self.sources.len())
                .filter(|&i| !self.done[i])
                .min_by_key(|&i| frames[i])
                .unwrap_or(0);
            if !self.sources[i].pull(r, &mut self.queues[i])? {
                self.done[i] = true;
            }
        }
    }

    pub fn packets(&self) -> u64 {
        self.sources.iter().map(|s| s.packets).sum()
    }

    pub fn errors(&self) -> u64 {
        self.sources.iter().map(|s| s.errors).sum()
    }
}

/// The movie timescale (`mvhd`).
pub(crate) fn movie_timescale(moov: &[u8]) -> Option<u32> {
    let mvhd = child(moov, b"mvhd")?;
    u32_at(mvhd, if mvhd.first() == Some(&1) { 20 } else { 12 }).filter(|&t| t > 0)
}
