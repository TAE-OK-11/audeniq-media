//! The probe report (FFprobe field names where FFprobe has one).
use crate::boxes::{child, fourcc_str, u32_at, u64_at};
use crate::check::Issue;
use crate::codec::Color;
use crate::file::Layout;
use crate::track::{self, Kind, Timing, Track};
use crate::{ENGINE_VERSION, Error, RULE_VERSION};

#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub engine: &'static str,
    pub rule: &'static str,
    pub container: Container,
    pub video: Vec<VideoStream>,
    pub audio: Vec<AudioStream>,
    pub other: Vec<OtherStream>,
    pub issues: Vec<Issue>,
    /// No issue of severity `error`.
    pub mv_ready: bool,
}
media_json::json_struct!(Report {
    engine,
    rule,
    container,
    video,
    audio,
    other,
    issues,
    mv_ready
});

#[derive(Debug, Clone, PartialEq)]
pub struct Container {
    /// `mov` for QuickTime (`qt  ` brand), else `mp4`.
    pub format: &'static str,
    pub major_brand: Option<String>,
    pub compatible_brands: Vec<String>,
    pub size: u64,
    pub duration: Option<f64>,
    pub bit_rate: Option<u64>,
    /// The index precedes the media data (playback can start while
    /// downloading).
    pub fast_start: bool,
    pub fragments: u64,
}
media_json::json_struct!(Container {
    format,
    major_brand,
    compatible_brands,
    size,
    duration,
    bit_rate,
    fast_start,
    fragments
});

#[derive(Debug, Clone, PartialEq)]
pub struct VideoStream {
    pub track_id: u32,
    pub enabled: bool,
    pub codec_name: String,
    pub codec_tag: String,
    pub profile: Option<String>,
    pub level: Option<u32>,
    pub width: u32,
    pub height: u32,
    pub sample_aspect_ratio: Option<String>,
    pub display_aspect_ratio: Option<String>,
    /// Quarter turns of the track matrix, degrees counter-clockwise as
    /// FFprobe's display matrix `rotation`.
    pub rotation: i32,
    pub pix_fmt: Option<String>,
    pub bits_per_raw_sample: Option<u32>,
    /// `progressive`, `interlaced`, or `null` when nothing says.
    pub field_order: Option<&'static str>,
    pub r_frame_rate: Option<String>,
    pub avg_frame_rate: Option<String>,
    pub fps: Option<f64>,
    pub variable_frame_rate: bool,
    pub nb_frames: u64,
    pub duration: Option<f64>,
    pub start_time: f64,
    pub bit_rate: Option<u64>,
    pub keyframes: u64,
    /// Longest distance between key frames, in seconds.
    pub max_keyframe_interval: Option<f64>,
    pub color_range: Option<&'static str>,
    pub color_space: Option<&'static str>,
    pub color_transfer: Option<&'static str>,
    pub color_primaries: Option<&'static str>,
    /// `PQ`, `HLG` or `Dolby Vision`.
    pub hdr: Option<&'static str>,
}
media_json::json_struct!(VideoStream {
    track_id,
    enabled,
    codec_name,
    codec_tag,
    profile,
    level,
    width,
    height,
    sample_aspect_ratio,
    display_aspect_ratio,
    rotation,
    pix_fmt,
    bits_per_raw_sample,
    field_order,
    r_frame_rate,
    avg_frame_rate,
    fps,
    variable_frame_rate,
    nb_frames,
    duration,
    start_time,
    bit_rate,
    keyframes,
    max_keyframe_interval,
    color_range,
    color_space,
    color_transfer,
    color_primaries,
    hdr
});

#[derive(Debug, Clone, PartialEq)]
pub struct AudioStream {
    pub track_id: u32,
    pub enabled: bool,
    pub codec_name: String,
    pub codec_tag: String,
    pub profile: Option<String>,
    pub sample_rate: u32,
    pub channels: u32,
    pub bits_per_sample: Option<u32>,
    pub nb_frames: u64,
    pub duration: Option<f64>,
    pub start_time: f64,
    pub bit_rate: Option<u64>,
    pub language: Option<String>,
}
media_json::json_struct!(AudioStream {
    track_id,
    enabled,
    codec_name,
    codec_tag,
    profile,
    sample_rate,
    channels,
    bits_per_sample,
    nb_frames,
    duration,
    start_time,
    bit_rate,
    language
});

#[derive(Debug, Clone, PartialEq)]
pub struct OtherStream {
    pub track_id: u32,
    /// `hdlr` type: `tmcd` timecode, `text`/`sbtl`/`subt` subtitles, …
    pub handler: String,
    pub codec_tag: Option<String>,
    pub duration: Option<f64>,
}
media_json::json_struct!(OtherStream {
    track_id,
    handler,
    codec_tag,
    duration
});

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// `n/d` in lowest terms; `None` for a zero denominator.
pub(crate) fn ratio(n: u64, d: u64) -> Option<(u64, u64)> {
    if d == 0 {
        return None;
    }
    let g = gcd(n, d).max(1);
    Some((n / g, d / g))
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn seconds(ticks: u64, timescale: u32) -> Option<f64> {
    (timescale > 0 && ticks > 0).then(|| round3(ticks as f64 / f64::from(timescale)))
}

fn primaries(v: u16) -> Option<&'static str> {
    Some(match v {
        1 => "bt709",
        4 => "bt470m",
        5 => "bt470bg",
        6 => "smpte170m",
        7 => "smpte240m",
        8 => "film",
        9 => "bt2020",
        10 => "smpte428",
        11 => "smpte431",
        12 => "smpte432",
        22 => "jedec-p22",
        _ => return None,
    })
}

fn transfer(v: u16) -> Option<&'static str> {
    Some(match v {
        1 => "bt709",
        4 => "gamma22",
        5 => "gamma28",
        6 => "smpte170m",
        7 => "smpte240m",
        8 => "linear",
        9 => "log100",
        10 => "log316",
        11 => "iec61966-2-4",
        12 => "bt1361e",
        13 => "iec61966-2-1",
        14 => "bt2020-10",
        15 => "bt2020-12",
        16 => "smpte2084",
        17 => "smpte428",
        18 => "arib-std-b67",
        _ => return None,
    })
}

fn matrix(v: u16) -> Option<&'static str> {
    Some(match v {
        0 => "gbr",
        1 => "bt709",
        4 => "fcc",
        5 => "bt470bg",
        6 => "smpte170m",
        7 => "smpte240m",
        8 => "ycgco",
        9 => "bt2020nc",
        10 => "bt2020c",
        11 => "smpte2085",
        12 => "chroma-derived-nc",
        13 => "chroma-derived-c",
        14 => "ictcp",
        _ => return None,
    })
}

/// FFprobe codec name of a visual sample entry.
fn video_codec(f: &[u8; 4]) -> &'static str {
    match f {
        b"avc1" | b"avc3" => "h264",
        b"hvc1" | b"hev1" | b"dvh1" | b"dvhe" => "hevc",
        b"av01" => "av1",
        b"vp09" => "vp9",
        b"vp08" => "vp8",
        b"apch" | b"apcn" | b"apcs" | b"apco" | b"ap4h" | b"ap4x" => "prores",
        b"mp4v" => "mpeg4",
        b"jpeg" | b"mjpa" | b"mjpb" => "mjpeg",
        b"dvc " | b"dvcp" | b"dv5n" | b"dv5p" | b"dvh5" | b"dvh6" => "dvvideo",
        b"AVdh" | b"AVdn" => "dnxhd",
        b"xd5c" | b"xd5d" | b"xd5e" | b"xd5f" | b"xdvc" | b"mx5n" | b"mx5p" => "mpeg2video",
        b"png " => "png",
        b"raw " | b"2vuy" | b"v210" => "rawvideo",
        _ => "unknown",
    }
}

/// ProRes flavour by sample entry (FFprobe's profile names), 4:2:2 or 4:4:4.
fn prores(f: &[u8; 4]) -> Option<(&'static str, u32)> {
    Some(match f {
        b"apco" => ("Proxy", 2),
        b"apcs" => ("LT", 2),
        b"apcn" => ("Standard", 2),
        b"apch" => ("HQ", 2),
        b"ap4h" => ("4444", 3),
        b"ap4x" => ("4444 XQ", 3),
        _ => return None,
    })
}

fn pix_fmt(chroma: u32, depth: u32, full_range: bool, h264: bool) -> Option<String> {
    let base = match chroma {
        0 => "gray",
        1 => "yuv420p",
        2 => "yuv422p",
        3 => "yuv444p",
        _ => return None,
    };
    // FFmpeg's H.264 decoder reports full-range 8-bit as the `yuvj` formats.
    if h264 && full_range && depth == 8 && chroma > 0 {
        return Some(base.replacen("yuv", "yuvj", 1));
    }
    Some(match depth {
        8 => base.to_owned(),
        _ if chroma == 0 => format!("gray{depth}le"),
        _ => format!("{base}{depth}le"),
    })
}

/// Sync interval and count over a timing (sample numbers are 1-based).
fn keyframes(t: &Timing, avg: Option<f64>) -> (u64, Option<f64>) {
    let Some(sync) = &t.sync else {
        return (t.samples, avg);
    };
    let mut prev = 1u64;
    let mut longest = 0u64;
    let mut count = 0u64;
    for &s in sync.iter().filter(|&&s| s >= 1 && s <= t.samples) {
        longest = longest.max(s - prev);
        prev = s;
        count += 1;
    }
    if count == 0 {
        return (0, None);
    }
    longest = longest.max(t.samples + 1 - prev);
    (count, avg.map(|d| round3(longest as f64 * d)))
}

/// A rate as a reduced fraction.
type Rate = Option<(u64, u64)>;

/// Nominal and average rate of a video timing: (r_frame_rate,
/// avg_frame_rate, variable).
fn frame_rates(t: &Timing, timescale: u32) -> (Rate, Rate, bool) {
    let ts = u64::from(timescale);
    let avg = ratio(t.samples.saturating_mul(ts), t.duration);
    // A final sample of its own length is common (the muxer's guess); it does
    // not make the stream variable.
    let mut runs: &[(u64, u32)] = &t.runs;
    if runs.len() > 1 && runs[runs.len() - 1].0 == 1 {
        runs = &runs[..runs.len() - 1];
    }
    let min = runs.iter().map(|r| r.1).min();
    let max = runs.iter().map(|r| r.1).max();
    let (Some(min), Some(max)) = (min, max) else {
        return (None, avg, false);
    };
    if min == max {
        return (ratio(ts, u64::from(min)), avg, false);
    }
    // Deltas one tick apart are rounding of a constant rate (29.97 fps in a
    // 1000 Hz timescale is 33/34/33…).
    let variable = max - min > 1;
    let common = runs.iter().max_by_key(|r| r.0).map(|r| r.1).unwrap_or(min);
    let nominal = if variable {
        ratio(ts, u64::from(common))
    } else {
        avg
    };
    (nominal, avg, variable)
}

fn text(r: Option<(u64, u64)>) -> Option<String> {
    r.map(|(n, d)| format!("{n}/{d}"))
}

fn bit_rate(bytes: u64, seconds: Option<f64>) -> Option<u64> {
    let s = seconds.filter(|&s| s > 0.0)?;
    (bytes > 0).then(|| (bytes as f64 * 8.0 / s).round() as u64)
}

/// Track length as presented: the edit list's, else the media's. (FFprobe
/// mixes the two; they differ by a frame or two at most.)
fn presented(t: &Track, timing: &Timing) -> Option<f64> {
    if let Some(s) = t.edited.filter(|&s| s > 0.0) {
        return Some(round3(s));
    }
    let ticks = if t.media_duration > 0 {
        t.media_duration
    } else {
        timing.duration
    };
    seconds(ticks, t.timescale)
}

fn video(t: &Track, v: &track::Visual, timing: &Timing) -> VideoStream {
    let cfg = &v.config;
    let codec = video_codec(&v.fourcc);
    let pr = prores(&v.fourcc);
    let (width, height) = cfg
        .size
        .filter(|&(w, h)| w > 0 && h > 0)
        .unwrap_or(v.entry_size);
    let chroma = cfg.chroma.or(pr.map(|p| p.1));
    let depth = cfg.bit_depth.or(pr.map(|_| 10));
    let color: Color = cfg.color.or(v.colr);
    let full_range = color.full_range.unwrap_or(false);
    let sar = cfg.sar.or(v.pasp).filter(|&(n, d)| n > 0 && d > 0);
    let dar = sar
        .unwrap_or((1, 1))
        .0
        .checked_mul(width)
        .zip(sar.unwrap_or((1, 1)).1.checked_mul(height))
        .and_then(|(n, d)| ratio(u64::from(n), u64::from(d)));
    let field_order = match (cfg.progressive, v.fields) {
        (_, Some(2)) | (Some(false), _) => Some("interlaced"),
        (Some(true), _) | (_, Some(1)) => Some("progressive"),
        _ => None,
    };
    let duration = presented(t, timing);
    let (r_rate, avg_rate, variable) = frame_rates(timing, t.timescale);
    let avg_frame = (timing.samples > 0 && timing.duration > 0 && t.timescale > 0)
        .then(|| timing.duration as f64 / timing.samples as f64 / f64::from(t.timescale));
    let (keyframes, max_kf) = keyframes(timing, avg_frame);
    let hdr = if v.dolby_vision {
        Some("Dolby Vision")
    } else {
        match color.transfer {
            Some(16) => Some("PQ"),
            Some(18) => Some("HLG"),
            _ => None,
        }
    };
    VideoStream {
        track_id: t.id,
        enabled: t.enabled,
        codec_name: codec.to_owned(),
        codec_tag: fourcc_str(&v.fourcc),
        profile: cfg.profile.clone().or(pr.map(|p| p.0.to_owned())),
        level: cfg.level,
        width,
        height,
        sample_aspect_ratio: sar.map(|(n, d)| format!("{n}:{d}")),
        display_aspect_ratio: dar.map(|(n, d)| format!("{n}:{d}")),
        rotation: t.rotation,
        pix_fmt: chroma
            .zip(depth)
            .and_then(|(c, d)| pix_fmt(c, d, full_range, codec == "h264")),
        bits_per_raw_sample: depth,
        field_order,
        r_frame_rate: text(r_rate),
        avg_frame_rate: text(avg_rate),
        fps: avg_rate.map(|(n, d)| round3(n as f64 / d as f64)),
        variable_frame_rate: variable,
        nb_frames: timing.samples,
        duration,
        start_time: round3(t.start),
        bit_rate: bit_rate(timing.bytes, seconds(timing.duration, t.timescale)),
        keyframes,
        max_keyframe_interval: max_kf,
        color_range: color.full_range.map(|f| if f { "pc" } else { "tv" }),
        color_space: color.matrix.and_then(matrix),
        color_transfer: color.transfer.and_then(transfer),
        color_primaries: color.primaries.and_then(primaries),
        hdr,
    }
}

/// The report (issues not yet evaluated) and the parsed tracks.
pub(crate) fn build(l: &Layout) -> Result<(Report, Vec<Track>), Error> {
    let moov = l
        .moov
        .as_deref()
        .ok_or_else(|| Error::Invalid("no moov (movie index)".into()))?;
    let mvhd = child(moov, b"mvhd").ok_or_else(|| Error::Invalid("no mvhd".into()))?;
    let bad = || Error::Invalid("malformed mvhd".into());
    let (timescale, mut duration) = match *mvhd.first().ok_or_else(bad)? {
        0 => (
            u32_at(mvhd, 12).ok_or_else(bad)?,
            u64::from(u32_at(mvhd, 16).ok_or_else(bad)?),
        ),
        1 => (
            u32_at(mvhd, 20).ok_or_else(bad)?,
            u64_at(mvhd, 24).ok_or_else(bad)?,
        ),
        _ => return Err(bad()),
    };
    if timescale == 0 {
        return Err(bad());
    }
    if duration == u64::from(u32::MAX) || duration == u64::MAX {
        duration = 0;
    }
    if duration == 0
        && let Some(mehd) = child(moov, b"mvex").and_then(|m| child(m, b"mehd"))
    {
        duration = match mehd.first() {
            Some(0) => u32_at(mehd, 4).map(u64::from).unwrap_or(0),
            _ => u64_at(mehd, 4).unwrap_or(0),
        };
    }
    let brands = |b: &[u8]| -> Vec<String> {
        b.chunks_exact(4)
            .map(|c| fourcc_str(&crate::boxes::fourcc(c)).trim_end().to_owned())
            .filter(|s| !s.is_empty())
            .collect()
    };
    let major = l.ftyp.as_deref().and_then(|f| f.get(0..4)).map(&brands);
    let major_brand = major.and_then(|m| m.into_iter().next());
    let compatible = l
        .ftyp
        .as_deref()
        .and_then(|f| f.get(8..))
        .map(&brands)
        .unwrap_or_default();
    let quicktime = major_brand.as_deref() == Some("qt") || l.ftyp.is_none();
    let mut report = Report {
        engine: ENGINE_VERSION,
        rule: RULE_VERSION,
        container: Container {
            format: if quicktime { "mov" } else { "mp4" },
            major_brand,
            compatible_brands: compatible,
            size: l.size,
            duration: None,
            bit_rate: None,
            fast_start: l.mdat.iter().all(|&(at, _)| at > l.moov_offset),
            fragments: l.fragments,
        },
        video: Vec::new(),
        audio: Vec::new(),
        other: Vec::new(),
        issues: Vec::new(),
        mv_ready: false,
    };
    let mut longest = seconds(duration, timescale);
    let mut tracks = Vec::new();
    for (t, trak) in crate::boxes::children(moov) {
        if &t != b"trak" {
            continue;
        }
        let Some(track) = track::parse(trak, timescale) else {
            return Err(Error::Invalid("malformed trak".into()));
        };
        let timing = match l.fragment_timing.get(&track.id) {
            Some(f) if track.timing.samples == 0 => f,
            _ => &track.timing,
        };
        let track_seconds = presented(&track, timing);
        if longest.is_none() {
            longest = track_seconds;
        }
        match &track.kind {
            Kind::Video(v) => report.video.push(video(&track, v, timing)),
            Kind::Audio(f, a) => {
                let real = seconds(timing.duration, track.timescale);
                report.audio.push(AudioStream {
                    track_id: track.id,
                    enabled: track.enabled,
                    codec_name: a.codec.clone(),
                    codec_tag: fourcc_str(f),
                    profile: a.profile.clone(),
                    sample_rate: a.sample_rate,
                    channels: a.channels,
                    bits_per_sample: a.bits_per_sample,
                    nb_frames: timing.samples,
                    duration: track_seconds,
                    start_time: round3(track.start),
                    bit_rate: bit_rate(timing.bytes, real).or(a.avg_bit_rate),
                    language: track.language.clone(),
                })
            }
            Kind::Other(f) => report.other.push(OtherStream {
                track_id: track.id,
                handler: fourcc_str(&track.handler),
                codec_tag: f.as_ref().map(fourcc_str),
                duration: track_seconds,
            }),
        }
        tracks.push(track);
    }
    report.container.duration = longest;
    report.container.bit_rate = bit_rate(l.size, longest);
    Ok((report, tracks))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_and_ratios() {
        let mut t = Timing::default();
        t.push(299, 1001);
        t.push(1, 1000);
        let (r, a, v) = frame_rates(&t, 30000);
        assert_eq!(r, Some((30000, 1001)));
        assert!(!v);
        assert_eq!(text(a).as_deref(), Some("9000000/300299"));

        let mut jitter = Timing::default();
        for _ in 0..50 {
            jitter.push(1, 33);
            jitter.push(2, 34);
        }
        assert!(!frame_rates(&jitter, 1000).2);

        let mut vfr = Timing::default();
        vfr.push(120, 3000);
        vfr.push(100, 1500);
        let (r, _, v) = frame_rates(&vfr, 90000);
        assert!(v);
        assert_eq!(r, Some((30, 1)));
    }

    #[test]
    fn pixel_formats() {
        assert_eq!(pix_fmt(1, 8, false, true).as_deref(), Some("yuv420p"));
        assert_eq!(pix_fmt(1, 8, true, true).as_deref(), Some("yuvj420p"));
        assert_eq!(pix_fmt(2, 10, false, false).as_deref(), Some("yuv422p10le"));
        assert_eq!(pix_fmt(0, 8, false, false).as_deref(), Some("gray"));
    }

    #[test]
    fn keyframe_interval() {
        let mut t = Timing::default();
        t.push(300, 1);
        t.sync = Some(vec![1, 121, 241]);
        let (n, longest) = keyframes(&t, Some(1.0 / 30.0));
        assert_eq!(n, 3);
        assert_eq!(longest, Some(4.0));
    }
}
