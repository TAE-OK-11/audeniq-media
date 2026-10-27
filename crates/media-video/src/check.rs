//! Music-video delivery checks (`mv-delivery-v2`: v1 plus the sound checks).
//!
//! `error`: the file cannot be delivered as is (no picture or sound, media
//! outside the file, a broken index). `warning`: a DSP is likely to reject it
//! or staff should look (below 1080p, variable frame rate, interlacing, a
//! codec outside H.264/HEVC/ProRes, odd audio). `info`: worth knowing (HDR,
//! index after the media).
//!
//! The bar follows what the Korean and global stores ask of a music video
//! master: progressive H.264/HEVC (MP4) or ProRes (MOV), at least 1080 lines
//! on the short side (1920×1080), a standard frame rate, stereo AAC/PCM at 44.1 kHz
//! or more, and sound as long as picture.
use crate::file::Layout;
use crate::report::Report;
use crate::track::{Kind, Track};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
}
media_json::json_enum!(Severity {
    Error => "error",
    Warning => "warning",
    Info => "info",
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// Stable code (`NO_AUDIO`, `RESOLUTION_BELOW_1080P`, …).
    pub code: &'static str,
    pub severity: Severity,
    /// Korean text for staff and the artist.
    pub detail: String,
}
media_json::json_struct!(Issue {
    code,
    severity,
    detail
});

/// Frame rates the stores take, with a tolerance for rounding.
const RATES: [f64; 8] = [
    24000.0 / 1001.0,
    24.0,
    25.0,
    30000.0 / 1001.0,
    30.0,
    50.0,
    60000.0 / 1001.0,
    60.0,
];

/// Minimum video bit rate (bit/s) for H.264 by short side and frame rate
/// (YouTube's SDR upload recommendations); HEVC needs about half.
fn min_video_bit_rate(short_side: u32, fps: f64) -> Option<u64> {
    let high = fps > 31.0;
    Some(match short_side {
        0..1080 => return None,
        1080..1440 => {
            if high {
                12_000_000
            } else {
                8_000_000
            }
        }
        1440..2160 => {
            if high {
                24_000_000
            } else {
                16_000_000
            }
        }
        _ => {
            if high {
                53_000_000
            } else {
                35_000_000
            }
        }
    })
}

/// Every chunk inside an `mdat` payload of a file of `size` bytes.
fn chunks_inside(track: &Track, l: &Layout) -> (bool, bool) {
    let mut mdat = l.mdat.clone();
    mdat.sort_unstable();
    let (mut past_end, mut outside) = (false, false);
    for &(off, len) in &track.chunks {
        if len == 0 {
            continue;
        }
        let end = off.saturating_add(len);
        if end > l.size {
            past_end = true;
            continue;
        }
        let i = mdat.partition_point(|&(at, _)| at <= off);
        let inside = i > 0 && {
            let (at, n) = mdat[i - 1];
            end <= at.saturating_add(n)
        };
        outside |= !inside;
    }
    (past_end, outside)
}

/// One platform's delivery spec, checked on top of the general rules by
/// `probe --profile NAME`. Its findings are `PROFILE_*` warnings: what that
/// platform asks for, which the general bar does not.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Profile {
    /// `--profile` name, and the platform's name in the details.
    pub name: &'static str,
    pub label: &'static str,
    /// Spec version: changes whenever a rule here does.
    pub version: &'static str,
    /// `mp4`, `mov`.
    pub containers: &'static [&'static str],
    pub video_codecs: &'static [&'static str],
    /// `pcm` for any PCM; `aac` means AAC-LC.
    pub audio_codecs: &'static [&'static str],
    pub sample_rates: &'static [u32],
    /// PCM bit depths, when the spec names them.
    pub pcm_bits: &'static [u32],
    pub sixteen_by_nine: bool,
    /// 4:2:0 chroma for the long-GOP codecs (not ProRes, which is 4:2:2+).
    pub chroma_420: bool,
    /// `color_primaries` values; empty takes any.
    pub primaries: &'static [&'static str],
    /// Index before the media, and no edit lists that move or cut tracks.
    pub fast_start: bool,
    pub no_edits: bool,
    /// Nothing but one picture and one sound track.
    pub only_av: bool,
    /// Playback normalization target (LUFS), and the True Peak ceiling for
    /// masters louder than it (dBTP).
    pub loudness_target: Option<f64>,
    pub true_peak_max: Option<f64>,
}

media_json::json_struct!(Profile {
    name,
    label,
    version,
    containers,
    video_codecs,
    audio_codecs,
    sample_rates,
    pcm_bits,
    sixteen_by_nine,
    chroma_420,
    primaries,
    fast_start,
    no_edits,
    only_av,
    loudness_target,
    true_peak_max
});

/// The platforms' published or distributor-relayed specs (see
/// docs/video/README.md for sources; distributors differ, so these follow
/// what the platform itself or most distributors state).
pub const PROFILES: &[Profile] = &[
    // YouTube recommended upload encoding settings: MP4, moov at the front,
    // no edit lists, AAC-LC (or Opus) at 48 kHz, 4:2:0, 16:9 player;
    // playback normalized to about −14 LUFS.
    Profile {
        name: "youtube",
        label: "YouTube",
        version: "youtube-2026-10",
        containers: &["mp4", "mov"],
        video_codecs: &["h264", "hevc", "prores", "av1", "vp9"],
        audio_codecs: &["aac", "opus", "pcm"],
        sample_rates: &[48000],
        pcm_bits: &[],
        sixteen_by_nine: true,
        chroma_420: true,
        primaries: &[],
        fast_start: true,
        no_edits: true,
        only_av: false,
        loudness_target: Some(-14.0),
        true_peak_max: Some(-1.0),
    },
    // Spotify music videos through distributors: MOV/MP4, H.264, HEVC or
    // ProRes 422 HQ, 1080p+, 16:9, Rec.709 or Rec.2020, 4:2:0 for long GOP,
    // AAC-LC or PCM at 48/96 kHz, one video and one sound asset; Spotify
    // normalizes to −14 LUFS and asks −2 dBTP for louder masters.
    Profile {
        name: "spotify",
        label: "Spotify",
        version: "spotify-2026-10",
        containers: &["mov", "mp4"],
        video_codecs: &["h264", "hevc", "prores"],
        audio_codecs: &["aac", "pcm"],
        sample_rates: &[48000, 96000],
        pcm_bits: &[16, 24],
        sixteen_by_nine: true,
        chroma_420: true,
        primaries: &["bt709", "bt2020"],
        fast_start: false,
        no_edits: false,
        only_av: true,
        loudness_target: Some(-14.0),
        true_peak_max: Some(-2.0),
    },
    // Apple Music video (iTunes-era HD spec as relayed by distributors):
    // ProRes 422 HQ in MOV, 1920×1080, Rec.709, 16/24-bit LPCM at 48 kHz.
    Profile {
        name: "apple",
        label: "Apple Music",
        version: "apple-2026-10",
        containers: &["mov"],
        video_codecs: &["prores"],
        audio_codecs: &["pcm"],
        sample_rates: &[48000],
        pcm_bits: &[16, 24],
        sixteen_by_nine: true,
        chroma_420: false,
        primaries: &["bt709"],
        fast_start: false,
        no_edits: false,
        only_av: true,
        loudness_target: None,
        true_peak_max: None,
    },
];

/// The profile called `name`.
pub fn profile(name: &str) -> Option<&'static Profile> {
    PROFILES.iter().find(|p| p.name == name)
}

/// `a:b` as a ratio.
fn ratio(s: &str) -> Option<f64> {
    let (a, b) = s.split_once(':')?;
    let (a, b): (f64, f64) = (a.parse().ok()?, b.parse().ok()?);
    (a > 0.0 && b > 0.0).then(|| a / b)
}

/// A track whose edit list moves its start or cuts its media (a plain
/// one-edit list that only skips encoder priming does neither).
fn edited(t: &Track) -> bool {
    if t.start > 0.0005 {
        return true;
    }
    let media = t.media_duration as f64 / f64::from(t.timescale.max(1));
    t.edited
        .is_some_and(|e| matches!(t.kind, Kind::Video(..)) && (media - e).abs() > 0.05)
}

fn evaluate_profile(
    p: &Profile,
    r: &Report,
    tracks: &[Track],
    add: &mut dyn FnMut(&'static str, Severity, String),
) {
    use Severity::{Info, Warning};
    let who = p.label;
    if !p.containers.contains(&r.container.format) {
        add(
            "PROFILE_CONTAINER",
            Warning,
            format!(
                "{who} 사양: {} 파일 ({} 파일입니다).",
                p.containers.join("·").to_uppercase(),
                r.container.format.to_uppercase()
            ),
        );
    }
    if p.fast_start && !r.container.fast_start && r.container.fragments == 0 {
        add(
            "PROFILE_INDEX_AT_END",
            Warning,
            format!(
                "{who} 사양: 색인(moov)이 파일 앞에 있는 파일(Fast Start). 다시 내보내거나 Fast Start로 저장해 주세요."
            ),
        );
    }
    if p.no_edits && tracks.iter().any(edited) {
        add(
            "PROFILE_EDIT_LIST",
            Warning,
            format!(
                "{who} 사양: 편집 목록(시작 지연·잘라내기)이 없는 파일. 편집을 적용해 내보내 주세요."
            ),
        );
    }
    if p.only_av && (r.video.len() > 1 || r.audio.len() > 1 || !r.other.is_empty()) {
        add(
            "PROFILE_EXTRA_TRACKS",
            Warning,
            format!(
                "{who} 사양: 영상 1개·소리 1개만 있는 파일 (영상 {}·소리 {}·기타 {}개).",
                r.video.len(),
                r.audio.len(),
                r.other.len()
            ),
        );
    }
    let video = r.video.iter().find(|v| v.enabled || r.video.len() == 1);
    if let Some(v) = video {
        if !p.video_codecs.contains(&v.codec_name.as_str()) {
            add(
                "PROFILE_VIDEO_CODEC",
                Warning,
                format!(
                    "{who} 사양: 영상 코덱 {} ({}입니다).",
                    p.video_codecs.join("·"),
                    v.codec_name
                ),
            );
        }
        let (w, h) = if v.rotation % 180 == 0 {
            (v.width, v.height)
        } else {
            (v.height, v.width)
        };
        let sar = v
            .sample_aspect_ratio
            .as_deref()
            .and_then(ratio)
            .unwrap_or(1.0);
        let dar = f64::from(w) * sar / f64::from(h.max(1));
        if p.sixteen_by_nine && (dar - 16.0 / 9.0).abs() > 0.01 {
            add(
                "PROFILE_ASPECT_RATIO",
                Warning,
                format!(
                    "{who} 사양: 16:9 화면 (화면비 {dar:.3}:1, {w}×{h}). 레터박스 없이 16:9로 내보내 주세요."
                ),
            );
        }
        if p.chroma_420
            && v.codec_name != "prores"
            && let Some(f) = v.pix_fmt.as_deref()
            && !f.starts_with("yuv420p")
            && !f.starts_with("yuvj420p")
        {
            add(
                "PROFILE_CHROMA",
                Warning,
                format!("{who} 사양: 4:2:0 크로마 (픽셀 형식 {f}입니다)."),
            );
        }
        if !p.primaries.is_empty()
            && let Some(c) = v.color_primaries
            && !p.primaries.contains(&c)
        {
            add(
                "PROFILE_COLOR",
                Warning,
                format!("{who} 사양: 색 공간 {} ({c}입니다).", p.primaries.join("·")),
            );
        }
    }
    let audio = r.audio.iter().find(|a| a.enabled || r.audio.len() == 1);
    if let Some(a) = audio {
        let pcm = a.codec_name.starts_with("pcm_");
        let kind = if pcm { "pcm" } else { a.codec_name.as_str() };
        let lc = a.codec_name != "aac" || a.profile.as_deref().is_none_or(|p| p == "LC");
        if !p.audio_codecs.contains(&kind) || !lc {
            add(
                "PROFILE_AUDIO_CODEC",
                Warning,
                format!(
                    "{who} 사양: 소리 코덱 {} ({}{}입니다).",
                    p.audio_codecs
                        .iter()
                        .map(|c| match *c {
                            "aac" => "AAC-LC",
                            "pcm" => "PCM",
                            "opus" => "Opus",
                            other => other,
                        })
                        .collect::<Vec<_>>()
                        .join("·"),
                    a.codec_name,
                    a.profile
                        .as_deref()
                        .map(|p| format!(" {p}"))
                        .unwrap_or_default()
                ),
            );
        }
        if !p.sample_rates.contains(&a.sample_rate) {
            add(
                "PROFILE_AUDIO_SAMPLE_RATE",
                Warning,
                format!(
                    "{who} 사양: 샘플레이트 {}Hz ({}Hz입니다). convert-audio로 맞출 수 있습니다.",
                    p.sample_rates
                        .iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join("·"),
                    a.sample_rate
                ),
            );
        }
        if pcm
            && !p.pcm_bits.is_empty()
            && let Some(b) = a.bits_per_sample
            && !p.pcm_bits.contains(&b)
        {
            add(
                "PROFILE_AUDIO_BIT_DEPTH",
                Warning,
                format!(
                    "{who} 사양: PCM {}비트 ({b}비트입니다).",
                    p.pcm_bits
                        .iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join("·")
                ),
            );
        }
    }
    if let (Some(target), Some(ceiling)) = (p.loudness_target, p.true_peak_max) {
        match &r.sound {
            Some(s) => {
                if let (Some(l), Some(tp)) = (s.integrated_lufs, s.true_peak_dbtp)
                    && l > target
                    && tp > ceiling
                {
                    add(
                        "PROFILE_TRUE_PEAK",
                        Warning,
                        format!(
                            "{who} 사양: {target:.0} LUFS보다 큰 마스터({l:.1} LUFS)는 True Peak {ceiling:.0}dBTP 이하 ({tp:+.1}dBTP입니다). 재생할 때 소리를 줄이면서 찌그러질 수 있습니다."
                        ),
                    );
                }
                if let Some(l) = s.integrated_lufs
                    && l > target + 0.5
                {
                    add(
                        "PROFILE_LOUDNESS",
                        Info,
                        format!(
                            "{who} 재생 음량 {target:.0} LUFS: 이 소리({l:.1} LUFS)는 {:.1}dB 작게 재생됩니다.",
                            l - target
                        ),
                    );
                }
            }
            None if r.sound_note.is_none() => add(
                "PROFILE_LOUDNESS_NOT_MEASURED",
                Info,
                format!("{who} 음량 기준은 소리를 측정해야 확인할 수 있습니다(probe --sound)."),
            ),
            None => {}
        }
    }
}

pub(crate) fn evaluate(
    r: &Report,
    l: &Layout,
    tracks: &[Track],
    profile: Option<&Profile>,
) -> Vec<Issue> {
    let mut out = Vec::new();
    let mut add = |code: &'static str, severity: Severity, detail: String| {
        out.push(Issue {
            code,
            severity,
            detail,
        })
    };
    use Severity::{Error, Info, Warning};

    // The file itself.
    let (mut past_end, mut outside) = (l.truncated || l.fragment_outside, false);
    for t in tracks {
        let (p, o) = chunks_inside(t, l);
        past_end |= p;
        outside |= o;
    }
    if past_end {
        add("TRUNCATED", Error, "파일이 중간에 끊겨 있습니다 (영상·소리 데이터가 파일 끝을 넘어감). 다시 내보내 올려 주세요.".into());
    } else if outside {
        add(
            "MEDIA_OUTSIDE_MDAT",
            Error,
            "색인이 가리키는 영상·소리 데이터가 파일의 미디어 영역 밖에 있습니다.".into(),
        );
    }
    if tracks
        .iter()
        .any(|t| t.inconsistent && t.timing.samples > 0)
    {
        add(
            "INDEX_INCONSISTENT",
            Error,
            "영상 색인(샘플 표)이 서로 맞지 않습니다. 다시 내보내 올려 주세요.".into(),
        );
    }
    if tracks.iter().any(|t| t.external) {
        add(
            "EXTERNAL_MEDIA",
            Error,
            "영상이 다른 파일의 미디어를 참조합니다(참조 MOV). 미디어를 포함해 내보내 주세요."
                .into(),
        );
    }

    // Picture.
    let video: Vec<_> = r
        .video
        .iter()
        .filter(|v| v.enabled || r.video.len() == 1)
        .collect();
    match video.len() {
        0 => add("NO_VIDEO", Error, "영상 트랙이 없습니다.".into()),
        1 => {}
        n => add(
            "MULTIPLE_VIDEO",
            Warning,
            format!("영상 트랙이 {n}개입니다. 배급에는 첫 번째만 쓰입니다."),
        ),
    }
    if let Some(v) = video.first() {
        if v.nb_frames == 0 {
            add(
                "NO_VIDEO_FRAMES",
                Error,
                "영상 트랙에 프레임이 없습니다.".into(),
            );
        }
        if !matches!(v.codec_name.as_str(), "h264" | "hevc" | "prores") {
            add(
                "VIDEO_CODEC",
                Warning,
                format!(
                    "영상 코덱 {}({})은 음원 사이트가 받지 않을 수 있습니다. H.264, HEVC 또는 ProRes로 내보내 주세요.",
                    v.codec_name, v.codec_tag
                ),
            );
        }
        let (w, h) = if v.rotation % 180 == 0 {
            (v.width, v.height)
        } else {
            (v.height, v.width)
        };
        let short = w.min(h);
        if short < 1080 {
            add(
                "RESOLUTION_BELOW_1080P",
                Warning,
                format!("해상도 {w}×{h}: 짧은 변이 1080 미만입니다. 1920×1080 이상을 권장합니다."),
            );
        }
        if v.rotation != 0 {
            add(
                "ROTATED",
                Warning,
                format!(
                    "화면 회전 정보({}°)가 들어 있습니다. 일부 플랫폼은 회전을 무시하니 회전을 적용해 내보내 주세요.",
                    v.rotation
                ),
            );
        }
        if let Some(sar) = &v.sample_aspect_ratio
            && sar != "1:1"
        {
            add(
                "NON_SQUARE_PIXELS",
                Warning,
                format!("픽셀 비율이 {sar}입니다. 정사각형 픽셀(1:1)로 내보내 주세요."),
            );
        }
        if v.field_order == Some("interlaced") {
            add(
                "INTERLACED",
                Warning,
                "인터레이스 영상입니다. 프로그레시브로 내보내 주세요.".into(),
            );
        }
        if v.variable_frame_rate {
            add(
                "VARIABLE_FRAME_RATE",
                Warning,
                "프레임레이트가 일정하지 않습니다(VFR). 고정 프레임레이트로 내보내 주세요.".into(),
            );
        } else if let Some(fps) = v.fps
            && !RATES.iter().any(|r| (r - fps).abs() < 0.01)
        {
            add(
                "FRAME_RATE_UNUSUAL",
                Warning,
                format!(
                    "프레임레이트 {fps}fps는 표준(23.976·24·25·29.97·30·50·59.94·60)이 아닙니다."
                ),
            );
        }
        if v.codec_name == "h264" && v.bits_per_raw_sample.is_some_and(|d| d > 8) {
            add(
                "H264_HIGH_BIT_DEPTH",
                Warning,
                "10비트 이상 H.264는 재생되지 않는 기기가 많습니다. 8비트로 내보내 주세요.".into(),
            );
        }
        if let (Some(rate), Some(min)) = (
            v.bit_rate,
            min_video_bit_rate(short, v.fps.unwrap_or(30.0)).filter(|_| v.codec_name != "prores"),
        ) {
            let min = if v.codec_name == "h264" { min } else { min / 2 };
            if rate < min {
                add(
                    "VIDEO_BIT_RATE_LOW",
                    Warning,
                    format!(
                        "영상 비트레이트 {:.1}Mbps는 이 해상도에 낮습니다(권장 {:.0}Mbps 이상). 화질이 떨어질 수 있습니다.",
                        rate as f64 / 1e6,
                        min as f64 / 1e6
                    ),
                );
            }
        }
        if let Some(hdr) = v.hdr {
            add(
                "HDR",
                Info,
                format!(
                    "HDR 영상({hdr})입니다. HDR을 받지 않는 플랫폼에는 SDR 버전이 필요할 수 있습니다."
                ),
            );
        }
    }

    // Sound.
    let audio: Vec<_> = r
        .audio
        .iter()
        .filter(|a| a.enabled || r.audio.len() == 1)
        .collect();
    match audio.len() {
        0 => add("NO_AUDIO", Error, "소리(오디오 트랙)가 없습니다.".into()),
        1 => {}
        n => add(
            "MULTIPLE_AUDIO",
            Warning,
            format!("오디오 트랙이 {n}개입니다. 배급에는 첫 번째만 쓰입니다."),
        ),
    }
    if let Some(a) = audio.first() {
        if a.nb_frames == 0 {
            add(
                "NO_AUDIO_FRAMES",
                Error,
                "오디오 트랙에 데이터가 없습니다.".into(),
            );
        }
        let pcm = a.codec_name.starts_with("pcm_");
        if !(pcm || matches!(a.codec_name.as_str(), "aac" | "alac")) {
            add(
                "AUDIO_CODEC",
                Warning,
                format!(
                    "오디오 코덱 {}은 받지 않는 플랫폼이 있습니다. AAC 또는 PCM으로 내보내 주세요.",
                    a.codec_name
                ),
            );
        } else if a.codec_name == "aac" && a.profile.as_deref().is_some_and(|p| p != "LC") {
            add(
                "AUDIO_CODEC",
                Warning,
                format!(
                    "AAC {} 프로파일입니다. AAC-LC로 내보내 주세요.",
                    a.profile.as_deref().unwrap_or("")
                ),
            );
        }
        if a.sample_rate < 44100 {
            add(
                "AUDIO_SAMPLE_RATE",
                Warning,
                format!(
                    "샘플레이트 {}Hz: 44.1kHz 이상(48kHz 권장)으로 내보내 주세요.",
                    a.sample_rate
                ),
            );
        }
        if a.channels != 2 {
            add(
                "AUDIO_CHANNELS",
                Warning,
                format!(
                    "오디오 채널 {}개: 스테레오(2채널)로 내보내 주세요.",
                    a.channels
                ),
            );
        }
        if a.codec_name == "aac"
            && let Some(rate) = a.bit_rate
            && rate < 192_000
        {
            add(
                "AUDIO_BIT_RATE_LOW",
                Warning,
                format!(
                    "AAC 비트레이트 {}kbps: 256kbps 이상을 권장합니다.",
                    rate / 1000
                ),
            );
        }
    }

    // The decoded sound (`probe --sound`): what the audio QC checks on masters.
    if let Some(s) = &r.sound {
        if s.decode_errors > 0 {
            let broken = s.decode_errors * 100 >= s.packets.max(1);
            add(
                "AUDIO_DECODE_ERRORS",
                if broken { Error } else { Warning },
                format!(
                    "소리 데이터 {}곳이 깨져 재생되지 않습니다(전체 {}곳 중).",
                    s.decode_errors, s.packets
                ),
            );
        }
        match s.peak_dbfs {
            None => add(
                "AUDIO_SILENT",
                Error,
                "소리 트랙이 완전히 무음입니다.".into(),
            ),
            Some(p) if p < -80.0 => add(
                "AUDIO_SILENT",
                Error,
                format!("소리 트랙이 사실상 무음입니다(최대 {p:.1}dBFS)."),
            ),
            _ => {}
        }
        let samples = (s.duration * f64::from(s.sample_rate) * f64::from(s.channels)).max(1.0);
        if s.clipped_samples as f64 / samples >= 0.001 {
            add(
                "AUDIO_CLIPPING",
                Warning,
                format!(
                    "소리가 찌그러져 있습니다(클리핑 {}회). 마스터를 확인해 주세요.",
                    s.clip_events
                ),
            );
        } else if s.clip_events > 0 || s.true_peak_dbtp.is_some_and(|t| t > 0.0) {
            add(
                "AUDIO_CLIPPING",
                Warning,
                format!(
                    "소리 최고점이 0dBFS를 넘습니다(True Peak {:+.1}dBTP, 클리핑 {}회). 재생 기기에서 찌그러질 수 있습니다.",
                    s.true_peak_dbtp.unwrap_or(0.0),
                    s.clip_events
                ),
            );
        }
        if s.silent_ratio >= 0.5 || s.longest_silence >= 30.0 {
            add(
                "AUDIO_LONG_SILENCE",
                Warning,
                format!(
                    "소리가 없는 구간이 깁니다(가장 긴 무음 {:.0}초, 전체의 {:.0}%).",
                    s.longest_silence,
                    s.silent_ratio * 100.0
                ),
            );
        }
        if let [Some(a), Some(b)] | [Some(a), Some(b), ..] = s.channel_peaks_dbfs.as_slice()
            && ((*a < -60.0) != (*b < -60.0))
        {
            add(
                "AUDIO_ONE_CHANNEL_SILENT",
                Warning,
                "스테레오의 한쪽 채널에 소리가 없습니다.".into(),
            );
        } else if s.channels == 2
            && s.channel_peaks_dbfs.iter().filter(|p| p.is_none()).count() == 1
        {
            add(
                "AUDIO_ONE_CHANNEL_SILENT",
                Warning,
                "스테레오의 한쪽 채널에 소리가 없습니다.".into(),
            );
        }
        if let Some(l) = s.integrated_lufs
            && l < -30.0
        {
            add(
                "AUDIO_TOO_QUIET",
                Warning,
                format!(
                    "소리가 매우 작습니다({l:.1} LUFS). 음원과 같은 마스터를 썼는지 확인해 주세요."
                ),
            );
        }
    }
    if let Some(note) = &r.sound_note {
        add(
            "SOUND_NOT_MEASURED",
            Info,
            format!("소리는 측정하지 않았습니다: {note}."),
        );
    }

    // Picture and sound together.
    if let (Some(v), Some(a)) = (video.first(), audio.first()) {
        if let (Some(vd), Some(ad)) = (v.duration, a.duration)
            && (vd - ad).abs() > 1.0
        {
            add(
                "DURATION_MISMATCH",
                Warning,
                format!("영상 길이 {vd:.1}초와 소리 길이 {ad:.1}초가 1초 넘게 다릅니다."),
            );
        }
        let offset = (v.start_time - a.start_time).abs();
        if offset > 0.1 {
            add(
                "AV_OFFSET",
                Warning,
                format!(
                    "영상과 소리의 시작이 {:.0}ms 어긋나 있습니다(편집 목록). 싱크를 확인해 주세요.",
                    offset * 1000.0
                ),
            );
        }
    }
    if !r.container.fast_start && r.container.fragments == 0 {
        add(
            "INDEX_AT_END",
            Info,
            "색인(moov)이 파일 끝에 있습니다. 배급에는 문제없습니다.".into(),
        );
    }
    if r.container.duration.is_none() {
        add(
            "DURATION_UNKNOWN",
            Warning,
            "파일에 길이가 적혀 있지 않습니다.".into(),
        );
    }
    if let Some(p) = profile {
        evaluate_profile(p, r, tracks, &mut add);
    }
    out
}
