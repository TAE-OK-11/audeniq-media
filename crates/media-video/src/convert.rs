//! A music video's sound converted to what a distributor asks for, the
//! picture left untouched: what `ffmpeg -c:v copy -c:a aac|pcm_s24le -ar
//! 48000 -ac 2 -movflags +faststart` did.
//!
//! Every track but the sound (video, timecode, captions) is copied sample
//! for sample: the bytes and the sample tables that describe them (`stsd`,
//! `stts`, `ctts`, `stss`, `sdtp`, …) are kept, only `stsc` and the chunk
//! offsets are rewritten. The sound ([`crate::source`]: the sound track, a
//! mono left/right track pair, or picked channels) is decoded, cut to its
//! edit list, folded to stereo, resampled (`media-resample`) and coded
//! again: AAC-LC (`media_aac::encoder`) or 24-bit PCM. Other sound tracks
//! are dropped. The output has `moov` first and the tracks interleaved in
//! half-second chunks.
use crate::Error;
use crate::boxes::{child, children, path, u16_at, u32_at, u64_at};
use crate::sound;
use crate::source::{Mix, Pick, Plan, Use};
use crate::track;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Mp4,
    Mov,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    /// AAC-LC at this bit rate.
    Aac(u32),
    /// Signed 24-bit little-endian PCM (`lpcm`).
    Pcm24,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub container: Container,
    pub codec: Codec,
    pub sample_rate: u32,
    /// Source channels to use (one-based across all sound tracks in file
    /// order); `None` lets [`crate::source::plan`] decide.
    pub pick: Option<Pick>,
}

/// Named delivery targets. Stereo 48 kHz throughout.
pub const PRESETS: &[(&str, &str)] = &[
    ("aac-320", "MP4, AAC-LC 320 kbit/s"),
    (
        "youtube",
        "MP4, AAC-LC 384 kbit/s (YouTube's recommended upload)",
    ),
    (
        "apple-music",
        "MOV, 24-bit PCM (Apple Music video: ProRes with LPCM)",
    ),
    ("pcm24", "MOV, 24-bit PCM"),
];

pub fn preset(name: &str) -> Option<Options> {
    let (container, codec) = match name {
        "aac-320" => (Container::Mp4, Codec::Aac(320_000)),
        "youtube" => (Container::Mp4, Codec::Aac(384_000)),
        "apple-music" | "pcm24" => (Container::Mov, Codec::Pcm24),
        _ => return None,
    };
    Some(Options {
        container,
        codec,
        sample_rate: 48000,
        pick: None,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct SourceSound {
    pub track_id: u32,
    pub codec_name: String,
    pub sample_rate: u32,
    pub channels: u32,
}
media_json::json_struct!(SourceSound {
    track_id,
    codec_name,
    sample_rate,
    channels
});

#[derive(Debug, Clone, PartialEq)]
pub struct Converted {
    pub container: String,
    pub source_sound: SourceSound,
    /// How the source channels were used: `mono`, `stereo`, `downmix`,
    /// `dual-mono` (two mono tracks as left and right) or `channels`
    /// (picked, or the first two of discrete channels).
    pub mapping: String,
    /// (track id, one-based channel) of each source channel used.
    pub source_channels: Vec<[u32; 2]>,
    pub codec_name: String,
    pub sample_rate: u32,
    pub channels: u32,
    /// Target bit rate (AAC) or the PCM rate.
    pub bit_rate: u64,
    /// Sound frames presented and their length in seconds.
    pub frames: u64,
    pub duration: f64,
    /// Track ids copied untouched, and sound tracks not used.
    pub copied_tracks: Vec<u32>,
    pub dropped_tracks: Vec<u32>,
    /// Samples beyond full scale after downmix and resampling (PCM clips
    /// them; AAC carries them).
    pub over_full_scale: u64,
    pub bytes: u64,
}
media_json::json_struct!(Converted {
    container,
    source_sound,
    mapping,
    source_channels,
    codec_name,
    sample_rate,
    channels,
    bit_rate,
    frames,
    duration,
    copied_tracks,
    dropped_tracks,
    over_full_scale,
    bytes
});

/// Sample entries an `.mp4` may carry.
const MP4_VIDEO: &[&[u8; 4]] = &[
    b"avc1", b"avc3", b"hvc1", b"hev1", b"av01", b"vp09", b"mp4v",
];
/// Chunk length when interleaving, seconds.
const CHUNK_SECS: f64 = 0.5;
/// Frames per held block of PCM.
const PCM_BLOCK: u64 = 1024;

fn invalid(s: impl Into<String>) -> Error {
    Error::Invalid(s.into())
}

fn bx(name: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(payload.len() + 8);
    v.extend(
        u32::try_from(payload.len() + 8)
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    v.extend(name);
    v.extend(payload);
    v
}

fn full(name: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(body.len() + 4);
    p.extend((u32::from(version) << 24 | flags & 0xff_ffff).to_be_bytes());
    p.extend(body);
    bx(name, &p)
}

/// Where a track's samples go: (sample sizes, decode times in the track's
/// timescale, timescale), plus the bytes (copied tracks: source offsets).
struct Out {
    id: u32,
    timescale: u32,
    sizes: Vec<u32>,
    times: Vec<u64>,
    data: Data,
    /// Box payloads of `trak` built once offsets are known.
    trak: Trak,
}

enum Data {
    /// Source file offsets, one per sample.
    Copy(Vec<u64>),
    /// Coded here.
    Owned(Vec<u8>),
}

enum Trak {
    /// The source `trak` payload; `stsc` and `stco`/`co64` replaced.
    Copy(Vec<u8>),
    /// Everything but `stbl`'s chunk tables, which [`stbl_chunks`] adds.
    Sound(Box<SoundTrak>),
}

struct SoundTrak {
    tkhd: Vec<u8>,
    edts: Vec<u8>,
    mdhd: Vec<u8>,
    hdlr: Vec<u8>,
    minf_head: Vec<u8>,
    stbl_head: Vec<u8>,
}

/// One interleaved chunk: track index, first sample, samples.
type Chunk = (usize, usize, usize);

/// Decode the planned sound with its edit lists applied, fold it to stereo,
/// resample it to `out_rate` and hand it to `sink` in blocks.
fn decode_sound<R: Read + Seek>(
    r: &mut R,
    layout: &crate::file::Layout,
    plan: &Plan,
    movie_timescale: u32,
    out_rate: u32,
    sink: &mut dyn FnMut(&[f32]),
) -> Result<(), Error> {
    let mut mix = Mix::open(r, layout, plan, movie_timescale, Use::Convert)?;
    let names = mix.names();
    let mut resampler = media_resample::Resampler::new(mix.rate(), out_rate, 2);
    let (mut pcm, mut block) = (Vec::new(), Vec::new());
    while mix.pull(r, &mut pcm)? {
        if !pcm.is_empty() {
            let stereo = media_resample::downmix_stereo(&pcm, &names);
            block.clear();
            resampler.push(&stereo, &mut block);
            sink(&block);
            pcm.clear();
        }
    }
    block.clear();
    resampler.finish(&mut block);
    sink(&block);
    Ok(())
}

/// A copied track's samples: file offsets, sizes and decode times.
type Samples = (Vec<u64>, Vec<u32>, Vec<u64>);

fn copy_samples(trak: &[u8], tr: &track::Track) -> Result<Samples, Error> {
    let bad = || invalid(format!("track {}: sample tables", tr.id));
    let stbl = path(trak, &[b"mdia", b"minf", b"stbl"]).ok_or_else(bad)?;
    let (uniform, count, sizes) = track::sample_sizes(stbl).ok_or_else(bad)?;
    let count = count as usize;
    let mut offsets = Vec::with_capacity(count);
    let mut out_sizes = Vec::with_capacity(count);
    for (off, first, n) in sound::chunks(stbl).ok_or_else(bad)? {
        let mut at = off;
        for i in first..(first + n).min(count) {
            let s = if uniform != 0 {
                uniform
            } else {
                *sizes.get(i).ok_or_else(bad)?
            };
            offsets.push(at);
            out_sizes.push(s);
            at += u64::from(s);
        }
    }
    if offsets.len() != count {
        return Err(bad());
    }
    let mut times = Vec::with_capacity(count);
    let mut t = 0u64;
    for &(n, d) in &tr.timing.runs {
        for _ in 0..n {
            if times.len() == count {
                break;
            }
            times.push(t);
            t += u64::from(d);
        }
    }
    times.resize(count, t);
    Ok((offsets, out_sizes, times))
}

/// `stsc` and `co64` for chunks of `per` samples each, the first at
/// `offsets[0]`.
fn stbl_chunks(chunks: &[(usize, u64)]) -> Vec<u8> {
    let mut stsc = Vec::new();
    let mut runs = 0u32;
    let mut last = 0usize;
    for (i, &(n, _)) in chunks.iter().enumerate() {
        if n != last {
            stsc.extend((i as u32 + 1).to_be_bytes());
            stsc.extend((n as u32).to_be_bytes());
            stsc.extend(1u32.to_be_bytes());
            runs += 1;
            last = n;
        }
    }
    let mut body = runs.to_be_bytes().to_vec();
    body.extend(stsc);
    let mut out = full(b"stsc", 0, 0, &body);
    let mut co = (chunks.len() as u32).to_be_bytes().to_vec();
    for &(_, o) in chunks {
        co.extend(o.to_be_bytes());
    }
    out.extend(full(b"co64", 0, 0, &co));
    out
}

/// The source `trak` with its chunk tables replaced.
fn rebuilt_copy(trak: &[u8], chunks: &[(usize, u64)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (t, p) in children(trak) {
        if &t != b"mdia" {
            out.extend(bx(&t, p));
            continue;
        }
        let mut mdia = Vec::new();
        for (t, p) in children(p) {
            if &t != b"minf" {
                mdia.extend(bx(&t, p));
                continue;
            }
            let mut minf = Vec::new();
            for (t, p) in children(p) {
                if &t != b"stbl" {
                    minf.extend(bx(&t, p));
                    continue;
                }
                let mut stbl = Vec::new();
                for (t, p) in children(p) {
                    match &t {
                        b"stsc" => stbl.extend(stbl_chunks(chunks)),
                        b"stco" | b"co64" => {}
                        _ => stbl.extend(bx(&t, p)),
                    }
                }
                minf.extend(bx(b"stbl", &stbl));
            }
            mdia.extend(bx(b"minf", &minf));
        }
        out.extend(bx(b"mdia", &mdia));
    }
    bx(b"trak", &out)
}

fn esds(asc: &[u8], bit_rate: u32, max_unit: u32) -> Vec<u8> {
    let desc = |tag: u8, body: &[u8]| {
        let n = body.len() as u32;
        let mut v = vec![
            tag,
            0x80 | (n >> 21 & 0x7f) as u8,
            0x80 | (n >> 14 & 0x7f) as u8,
            0x80 | (n >> 7 & 0x7f) as u8,
            (n & 0x7f) as u8,
        ];
        v.extend(body);
        v
    };
    let mut dcd = vec![0x40, 0x15];
    dcd.extend(&max_unit.to_be_bytes()[1..]);
    dcd.extend(bit_rate.to_be_bytes());
    dcd.extend(bit_rate.to_be_bytes());
    dcd.extend(desc(5, asc));
    let mut es = vec![0, 0, 0];
    es.extend(desc(4, &dcd));
    es.extend(desc(6, &[2]));
    full(b"esds", 0, 0, &desc(3, &es))
}

fn sample_entry(opts: &Options, asc: &[u8], max_unit: u32) -> Vec<u8> {
    let mut e = vec![0u8; 6];
    e.extend(1u16.to_be_bytes());
    match opts.codec {
        Codec::Aac(bit_rate) => {
            e.extend([0u8; 8]);
            e.extend(2u16.to_be_bytes());
            e.extend(16u16.to_be_bytes());
            e.extend([0u8; 4]);
            e.extend((opts.sample_rate << 16).to_be_bytes());
            e.extend(esds(asc, bit_rate, max_unit));
            bx(b"mp4a", &e)
        }
        Codec::Pcm24 => {
            // SoundDescription v2, as QuickTime writes LPCM.
            e.extend(2u16.to_be_bytes());
            e.extend([0u8; 6]);
            e.extend(3u16.to_be_bytes());
            e.extend(16u16.to_be_bytes());
            e.extend(0xfffeu16.to_be_bytes());
            e.extend(0u16.to_be_bytes());
            e.extend(65536u32.to_be_bytes());
            e.extend(72u32.to_be_bytes());
            e.extend(f64::from(opts.sample_rate).to_bits().to_be_bytes());
            e.extend(2u32.to_be_bytes());
            e.extend(0x7f00_0000u32.to_be_bytes());
            e.extend(24u32.to_be_bytes());
            // Signed integer, packed, little-endian.
            e.extend(12u32.to_be_bytes());
            e.extend(6u32.to_be_bytes());
            e.extend(1u32.to_be_bytes());
            // Stereo channel layout.
            let mut chan = (101u32 << 16 | 2).to_be_bytes().to_vec();
            chan.extend([0u8; 8]);
            e.extend(full(b"chan", 0, 0, &chan));
            bx(b"lpcm", &e)
        }
    }
}

fn identity_matrix() -> Vec<u8> {
    let mut m = Vec::new();
    for v in [0x10000u32, 0, 0, 0, 0x10000, 0, 0, 0, 0x4000_0000] {
        m.extend(v.to_be_bytes());
    }
    m
}

#[allow(clippy::too_many_arguments)]
fn sound_trak(
    id: u32,
    opts: &Options,
    language: u16,
    movie_timescale: u32,
    media_frames: u64,
    presented: u64,
    priming: u64,
    stsd_entry: Vec<u8>,
) -> SoundTrak {
    let movie_dur =
        (u128::from(presented) * u128::from(movie_timescale) / u128::from(opts.sample_rate)) as u64;
    let mut tk = vec![0u8; 16];
    tk.extend(id.to_be_bytes());
    tk.extend([0u8; 4]);
    tk.extend(movie_dur.to_be_bytes());
    tk.extend([0u8; 8]);
    tk.extend(0u16.to_be_bytes());
    tk.extend(1u16.to_be_bytes());
    tk.extend(0x0100u16.to_be_bytes());
    tk.extend([0u8; 2]);
    tk.extend(identity_matrix());
    tk.extend([0u8; 8]);
    let tkhd = full(b"tkhd", 1, 3, &tk);
    let mut el = 1u32.to_be_bytes().to_vec();
    el.extend(movie_dur.to_be_bytes());
    el.extend(priming.to_be_bytes());
    el.extend(0x0001_0000u32.to_be_bytes());
    let edts = bx(b"edts", &full(b"elst", 1, 0, &el));
    let mut md = vec![0u8; 16];
    md.extend(opts.sample_rate.to_be_bytes());
    md.extend(media_frames.to_be_bytes());
    md.extend(language.to_be_bytes());
    md.extend([0u8; 2]);
    let mdhd = full(b"mdhd", 1, 0, &md);
    let mov = opts.container == Container::Mov;
    let mut hd = if mov { b"mhlr".to_vec() } else { vec![0u8; 4] };
    hd.extend(b"soun");
    hd.extend([0u8; 12]);
    hd.extend(b"SoundHandler\0");
    let hdlr = full(b"hdlr", 0, 0, &hd);
    let mut minf_head = full(b"smhd", 0, 0, &[0u8; 4]);
    let url = full(if mov { b"alis" } else { b"url " }, 0, 1, &[]);
    let mut dref = 1u32.to_be_bytes().to_vec();
    dref.extend(url);
    minf_head.extend(bx(b"dinf", &full(b"dref", 0, 0, &dref)));
    let mut sd = 1u32.to_be_bytes().to_vec();
    sd.extend(stsd_entry);
    SoundTrak {
        tkhd,
        edts,
        mdhd,
        hdlr,
        minf_head,
        stbl_head: full(b"stsd", 0, 0, &sd),
    }
}

/// `stts` and `stsz` for the new sound track. PCM is held in blocks of
/// [`PCM_BLOCK`] frames but described frame by frame, as QuickTime does.
fn sound_tables(o: &Out, pcm_frame: Option<u32>) -> Vec<u8> {
    let mut v = Vec::new();
    match pcm_frame {
        Some(f) => {
            let n = (o.sizes.iter().map(|&s| u64::from(s)).sum::<u64>() / u64::from(f)) as u32;
            let mut stts = 1u32.to_be_bytes().to_vec();
            stts.extend(n.to_be_bytes());
            stts.extend(1u32.to_be_bytes());
            v.extend(full(b"stts", 0, 0, &stts));
            let mut stsz = f.to_be_bytes().to_vec();
            stsz.extend(n.to_be_bytes());
            v.extend(full(b"stsz", 0, 0, &stsz));
        }
        None => {
            let n = o.sizes.len() as u32;
            let mut stts = 1u32.to_be_bytes().to_vec();
            stts.extend(n.to_be_bytes());
            stts.extend(1024u32.to_be_bytes());
            v.extend(full(b"stts", 0, 0, &stts));
            let mut stsz = 0u32.to_be_bytes().to_vec();
            stsz.extend(n.to_be_bytes());
            for s in &o.sizes {
                stsz.extend(s.to_be_bytes());
            }
            v.extend(full(b"stsz", 0, 0, &stsz));
        }
    }
    v
}

/// Convert the sound of the MP4/MOV at `input` per `opts`, writing `output`.
pub fn convert(input: &Path, output: &Path, opts: &Options) -> Result<Converted, Error> {
    if !matches!(opts.sample_rate, 44100 | 48000) {
        return Err(Error::Limit("sample rate must be 44100 or 48000".into()));
    }
    if opts.container == Container::Mp4 && opts.codec == Codec::Pcm24 {
        return Err(Error::Limit("24-bit PCM goes in .mov, not .mp4".into()));
    }
    let mut f = std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(input)?);
    let size = f.seek(SeekFrom::End(0))?;
    let layout = crate::file::walk(&mut f, size)?;
    let moov = layout
        .moov
        .clone()
        .ok_or_else(|| invalid("no moov (movie index)"))?;
    if layout.fragments > 0 {
        return Err(Error::Limit("fragmented MP4 is not converted yet".into()));
    }
    let mvhd = child(&moov, b"mvhd").ok_or_else(|| invalid("no mvhd"))?;
    let mvhd_v1 = mvhd.first() == Some(&1);
    let movie_ts = u32_at(mvhd, if mvhd_v1 { 20 } else { 12 })
        .filter(|&t| t > 0)
        .ok_or_else(|| invalid("mvhd timescale"))?;
    let plan =
        crate::source::plan(&moov, opts.pick)?.ok_or_else(|| invalid("소리 트랙이 없습니다"))?;
    let chosen = &plan.tracks[0];
    let merged: Vec<u32> = plan.tracks.iter().map(|c| c.id).collect();
    let source_sound = SourceSound {
        track_id: chosen.id,
        codec_name: chosen.cfg.codec.clone(),
        sample_rate: chosen.cfg.sample_rate,
        channels: chosen.cfg.channels,
    };

    // Tracks to copy, in file order; the new sound takes the chosen
    // track's place and id.
    let mut outs: Vec<Out> = Vec::new();
    let (mut copied, mut dropped) = (Vec::new(), Vec::new());
    let mut max_id = 0u32;
    let mut sound_at = None;
    let mut language = 0x55c4u16;
    for (t, trak) in children(&moov) {
        if &t != b"trak" {
            continue;
        }
        let tr = track::parse(trak, movie_ts).ok_or_else(|| invalid("unreadable trak"))?;
        max_id = max_id.max(tr.id);
        if tr.id == chosen.id {
            sound_at = Some(outs.len());
            let mdhd = path(trak, &[b"mdia", b"mdhd"]);
            language = mdhd
                .and_then(|m| u16_at(m, if m.first() == Some(&1) { 32 } else { 20 }))
                .unwrap_or(language);
            outs.push(Out {
                id: tr.id,
                timescale: opts.sample_rate,
                sizes: Vec::new(),
                times: Vec::new(),
                data: Data::Owned(Vec::new()),
                trak: Trak::Copy(Vec::new()),
            });
            continue;
        }
        if merged.contains(&tr.id) {
            continue;
        }
        if matches!(tr.kind, track::Kind::Audio(..)) {
            dropped.push(tr.id);
            continue;
        }
        if tr.external {
            return Err(Error::Limit(format!(
                "track {} keeps its media in another file",
                tr.id
            )));
        }
        if opts.container == Container::Mp4
            && let track::Kind::Video(v) = &tr.kind
            && !MP4_VIDEO.contains(&&v.fourcc)
        {
            return Err(Error::Limit(format!(
                "{} video cannot go in .mp4; convert to .mov",
                crate::boxes::fourcc_str(&v.fourcc)
            )));
        }
        let (offsets, sizes, times) = copy_samples(trak, &tr)?;
        copied.push(tr.id);
        outs.push(Out {
            id: tr.id,
            timescale: tr.timescale.max(1),
            sizes,
            times,
            data: Data::Copy(offsets),
            trak: Trak::Copy(trak.to_vec()),
        });
    }
    let sound_at = sound_at.ok_or_else(|| invalid("sound track"))?;

    // The new sound.
    let mut over = 0u64;
    let mut frames = 0u64;
    let (bytes_per_frame, units, pcm, asc) = match opts.codec {
        Codec::Aac(bit_rate) => {
            let mut enc = media_aac::encoder::Encoder::new(opts.sample_rate, 2, bit_rate)
                .map_err(|e| Error::Limit(e.to_string()))?;
            let mut units = Vec::new();
            decode_sound(
                &mut f,
                &layout,
                &plan,
                movie_ts,
                opts.sample_rate,
                &mut |b: &[f32]| {
                    over += b.iter().filter(|v| v.abs() > 1.0).count() as u64;
                    frames += (b.len() / 2) as u64;
                    units.extend(enc.push(b));
                },
            )?;
            units.extend(enc.finish());
            (None, units, Vec::new(), enc.asc().to_vec())
        }
        Codec::Pcm24 => {
            let mut pcm = Vec::new();
            decode_sound(
                &mut f,
                &layout,
                &plan,
                movie_ts,
                opts.sample_rate,
                &mut |b: &[f32]| {
                    frames += (b.len() / 2) as u64;
                    for &v in b {
                        if v.abs() > 1.0 {
                            over += 1;
                        }
                        let s = (f64::from(v) * 8_388_608.0)
                            .round()
                            .clamp(-8_388_608.0, 8_388_607.0)
                            as i32;
                        pcm.extend(&s.to_le_bytes()[..3]);
                    }
                },
            )?;
            (Some(6u32), Vec::new(), pcm, Vec::new())
        }
    };
    if frames == 0 {
        return Err(invalid("소리 트랙이 비어 있습니다"));
    }
    let (priming, media_frames) = match opts.codec {
        Codec::Aac(_) => (media_aac::encoder::DELAY as u64, units.len() as u64 * 1024),
        Codec::Pcm24 => (0, frames),
    };
    let max_unit = units.iter().map(Vec::len).max().unwrap_or(0) as u32;
    let entry = sample_entry(opts, &asc, max_unit);
    {
        let o = &mut outs[sound_at];
        match opts.codec {
            Codec::Aac(_) => {
                o.sizes = units.iter().map(|u| u.len() as u32).collect();
                o.times = (0..units.len() as u64).map(|i| i * 1024).collect();
                o.data = Data::Owned(units.concat());
            }
            Codec::Pcm24 => {
                let blocks = frames.div_ceil(PCM_BLOCK);
                o.sizes = (0..blocks)
                    .map(|b| ((frames - b * PCM_BLOCK).min(PCM_BLOCK) * 6) as u32)
                    .collect();
                o.times = (0..blocks).map(|b| b * PCM_BLOCK).collect();
                o.data = Data::Owned(pcm);
            }
        }
        o.trak = Trak::Sound(Box::new(sound_trak(
            o.id,
            opts,
            language,
            movie_ts,
            media_frames,
            frames,
            priming,
            entry,
        )));
    }

    // Interleave: chunks of up to half a second, ordered by start time.
    let mut chunks: Vec<(f64, Chunk)> = Vec::new();
    for (ti, o) in outs.iter().enumerate() {
        let span = (CHUNK_SECS * f64::from(o.timescale)) as u64;
        let mut i = 0;
        while i < o.sizes.len() {
            let start = o.times[i];
            let mut j = i + 1;
            while j < o.sizes.len() && o.times[j] < start + span.max(1) {
                j += 1;
            }
            chunks.push((start as f64 / f64::from(o.timescale), (ti, i, j - i)));
            i = j;
        }
    }
    chunks.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.0.cmp(&b.1.0)));

    // Offsets relative to the start of mdat's payload.
    let mut per_track: Vec<Vec<(usize, u64)>> = vec![Vec::new(); outs.len()];
    let mut at = 0u64;
    for &(_, (ti, first, n)) in &chunks {
        let len = outs[ti].sizes[first..first + n]
            .iter()
            .map(|&s| u64::from(s))
            .sum::<u64>();
        // stsc counts PCM in frames.
        let samples = match bytes_per_frame {
            Some(f) if ti == sound_at => (len / u64::from(f)) as usize,
            _ => n,
        };
        per_track[ti].push((samples, at));
        at += len;
    }
    let mdat_len = at;

    // moov, built twice: the second time with the real mdat position (co64
    // keeps its size the same).
    let ftyp = match opts.container {
        Container::Mp4 => {
            let mut p = b"isom".to_vec();
            p.extend(512u32.to_be_bytes());
            p.extend(b"isomiso2mp41");
            bx(b"ftyp", &p)
        }
        Container::Mov => {
            let mut p = b"qt  ".to_vec();
            p.extend(0x2005_0300u32.to_be_bytes());
            p.extend(b"qt  ");
            bx(b"ftyp", &p)
        }
    };
    let movie_dur = outs
        .iter()
        .filter_map(|o| match &o.trak {
            Trak::Copy(t) => {
                let tkhd = child(t, b"tkhd")?;
                if tkhd.first() == Some(&1) {
                    u64_at(tkhd, 28)
                } else {
                    u32_at(tkhd, 20).map(u64::from)
                }
            }
            Trak::Sound(_) => Some(
                (u128::from(frames) * u128::from(movie_ts) / u128::from(opts.sample_rate)) as u64,
            ),
        })
        .max()
        .unwrap_or(0);
    let build = |base: u64| -> Vec<u8> {
        let mut m = Vec::new();
        let mut mv = mvhd.to_vec();
        if mvhd_v1 {
            mv[24..32].copy_from_slice(&movie_dur.to_be_bytes());
        } else {
            let d = u32::try_from(movie_dur).unwrap_or(u32::MAX);
            mv[16..20].copy_from_slice(&d.to_be_bytes());
        }
        let n = mv.len();
        mv[n - 4..].copy_from_slice(&(max_id + 1).to_be_bytes());
        m.extend(bx(b"mvhd", &mv));
        for (ti, o) in outs.iter().enumerate() {
            let ch: Vec<(usize, u64)> = per_track[ti].iter().map(|&(n, a)| (n, a + base)).collect();
            match &o.trak {
                Trak::Copy(t) => m.extend(rebuilt_copy(t, &ch)),
                Trak::Sound(s) => {
                    let mut stbl = s.stbl_head.clone();
                    stbl.extend(sound_tables(o, bytes_per_frame));
                    stbl.extend(stbl_chunks(&ch));
                    let mut minf = s.minf_head.clone();
                    minf.extend(bx(b"stbl", &stbl));
                    let mut mdia = s.mdhd.clone();
                    mdia.extend(&s.hdlr);
                    mdia.extend(bx(b"minf", &minf));
                    let mut trak = s.tkhd.clone();
                    trak.extend(&s.edts);
                    trak.extend(bx(b"mdia", &mdia));
                    m.extend(bx(b"trak", &trak));
                }
            }
        }
        for (t, p) in children(&moov) {
            if !matches!(&t, b"mvhd" | b"trak" | b"iods") {
                m.extend(bx(&t, p));
            }
        }
        bx(b"moov", &m)
    };
    let probe = build(0);
    let base = (ftyp.len() + probe.len() + 16) as u64;
    let moov_out = build(base);
    debug_assert_eq!(moov_out.len(), probe.len());

    let file = std::fs::File::create(output)?;
    let mut w = BufWriter::with_capacity(1 << 20, file);
    w.write_all(&ftyp)?;
    w.write_all(&moov_out)?;
    w.write_all(&1u32.to_be_bytes())?;
    w.write_all(b"mdat")?;
    w.write_all(&(mdat_len + 16).to_be_bytes())?;
    let mut buf = Vec::new();
    for &(_, (ti, first, n)) in &chunks {
        let o = &outs[ti];
        match &o.data {
            Data::Owned(bytes) => {
                let start: u64 = o.sizes[..first].iter().map(|&s| u64::from(s)).sum();
                let len: u64 = o.sizes[first..first + n]
                    .iter()
                    .map(|&s| u64::from(s))
                    .sum();
                w.write_all(&bytes[start as usize..(start + len) as usize])?;
            }
            Data::Copy(offsets) => {
                // Runs of samples stored back to back read in one go.
                let mut i = first;
                while i < first + n {
                    let start = offsets[i];
                    let mut len = u64::from(o.sizes[i]);
                    let mut j = i + 1;
                    while j < first + n && offsets[j] == start + len && len < 16 << 20 {
                        len += u64::from(o.sizes[j]);
                        j += 1;
                    }
                    buf.resize(len as usize, 0);
                    f.seek(SeekFrom::Start(start))?;
                    f.read_exact(&mut buf)?;
                    w.write_all(&buf)?;
                    i = j;
                }
            }
        }
    }
    w.flush()?;
    let bytes = base + mdat_len;
    let (codec_name, bit_rate) = match opts.codec {
        Codec::Aac(b) => ("aac", u64::from(b)),
        Codec::Pcm24 => ("pcm_s24le", u64::from(opts.sample_rate) * 48),
    };
    Ok(Converted {
        container: match opts.container {
            Container::Mp4 => "mp4",
            Container::Mov => "mov",
        }
        .into(),
        source_sound,
        mapping: plan.kind.into(),
        source_channels: plan.source_channels(),
        codec_name: codec_name.into(),
        sample_rate: opts.sample_rate,
        channels: 2,
        bit_rate,
        frames,
        duration: (frames as f64 / f64::from(opts.sample_rate) * 1000.0).round() / 1000.0,
        copied_tracks: copied,
        dropped_tracks: dropped,
        over_full_scale: over,
        bytes,
    })
}
