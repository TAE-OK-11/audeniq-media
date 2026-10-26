//! Music-video delivery checks (`mv-delivery-v1`).
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
use crate::track::Track;

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

pub(crate) fn evaluate(r: &Report, l: &Layout, tracks: &[Track]) -> Vec<Issue> {
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
    out
}
