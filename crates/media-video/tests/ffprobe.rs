//! Parity with FFprobe on clips FFmpeg encodes here. Skipped (passes) when
//! ffmpeg/ffprobe are not installed; CI installs them.
use media_json::Value;
use media_video::{Report, Severity};
use std::path::{Path, PathBuf};
use std::process::Command;

fn tools() -> bool {
    ["ffmpeg", "ffprobe"].iter().all(|t| {
        Command::new(t)
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success())
    })
}

fn dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("media-video-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Encode `name` with FFmpeg arguments; `None` when this FFmpeg lacks an
/// encoder used (the case is skipped).
fn encode(name: &str, args: &[&str]) -> Option<PathBuf> {
    let out = dir().join(name);
    let st = Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args(args)
        .arg(&out)
        .output()
        .unwrap();
    if !st.status.success() {
        eprintln!("skip {name}: {}", String::from_utf8_lossy(&st.stderr));
        return None;
    }
    Some(out)
}

fn ffprobe(path: &Path) -> Value {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(out.status.success());
    media_json::from_slice(&out.stdout).unwrap()
}

fn stream<'a>(p: &'a Value, kind: &str) -> &'a Value {
    p["streams"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["codec_type"] == kind)
        .unwrap()
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str()?.parse().ok())
}

fn near(ours: Option<f64>, theirs: &Value, tol: f64, what: &str) {
    if let Some(t) = num(theirs) {
        let o = ours.unwrap_or_else(|| panic!("{what}: ffprobe {t}, ours none"));
        assert!(
            (o - t).abs() <= tol * t.abs().max(1.0),
            "{what}: ours {o}, ffprobe {t}"
        );
    }
}

fn same_video(r: &Report, p: &Value) {
    let v = &r.video[0];
    let s = stream(p, "video");
    assert_eq!(s["codec_name"], v.codec_name.as_str());
    assert_eq!(s["width"].as_u64(), Some(u64::from(v.width)), "width");
    assert_eq!(s["height"].as_u64(), Some(u64::from(v.height)), "height");
    let parsed = matches!(
        v.codec_name.as_str(),
        "h264" | "hevc" | "prores" | "av1" | "vp9"
    );
    if let Some(profile) = s["profile"].as_str().filter(|_| parsed) {
        assert_eq!(Some(profile), v.profile.as_deref(), "profile");
    }
    if let Some(level) = s["level"].as_i64().filter(|&l| l > 0 && parsed) {
        assert_eq!(Some(level as u32), v.level, "level");
    }
    if let Some(pf) = s["pix_fmt"].as_str().filter(|_| parsed) {
        assert_eq!(Some(pf), v.pix_fmt.as_deref(), "pix_fmt");
    }
    if let Some(sar) = s["sample_aspect_ratio"].as_str().filter(|s| *s != "0:1") {
        assert_eq!(Some(sar), v.sample_aspect_ratio.as_deref(), "sar");
        assert_eq!(
            s["display_aspect_ratio"].as_str(),
            v.display_aspect_ratio.as_deref(),
            "dar"
        );
    }
    assert_eq!(
        s["avg_frame_rate"].as_str(),
        v.avg_frame_rate.as_deref(),
        "avg_frame_rate"
    );
    if !v.variable_frame_rate {
        assert_eq!(
            s["r_frame_rate"].as_str(),
            v.r_frame_rate.as_deref(),
            "r_frame_rate"
        );
    }
    if let Some(n) = num(&s["nb_frames"]) {
        assert_eq!(n as u64, v.nb_frames, "nb_frames");
    }
    // FFprobe takes the edit list's length or the media's depending on the
    // stream; they differ by a frame or two.
    near(v.duration, &s["duration"], 0.02, "video duration");
    near(
        v.bit_rate.map(|b| b as f64),
        &s["bit_rate"],
        0.005,
        "video bit_rate",
    );
    // Colour is read from H.264/HEVC/VP9 headers and `colr`; AV1's lives in
    // its sequence header OBU, which is not parsed.
    let colour = v.codec_name != "av1";
    for (key, ours) in [
        ("color_range", v.color_range),
        ("color_space", v.color_space),
        ("color_transfer", v.color_transfer),
        ("color_primaries", v.color_primaries),
    ] {
        if let Some(theirs) = s[key].as_str().filter(|t| colour && *t != "unknown") {
            assert_eq!(Some(theirs), ours, "{key}");
        }
    }
    if let Some(fo) = s["field_order"].as_str() {
        let expect = if fo == "progressive" {
            "progressive"
        } else {
            "interlaced"
        };
        assert_eq!(v.field_order, Some(expect), "field_order");
    }
}

fn same_audio(r: &Report, p: &Value) {
    let a = &r.audio[0];
    let s = stream(p, "audio");
    assert_eq!(s["codec_name"], a.codec_name.as_str());
    assert_eq!(
        num(&s["sample_rate"]),
        Some(f64::from(a.sample_rate)),
        "sample_rate"
    );
    assert_eq!(
        s["channels"].as_u64(),
        Some(u64::from(a.channels)),
        "channels"
    );
    if let Some(profile) = s["profile"].as_str() {
        assert_eq!(Some(profile), a.profile.as_deref(), "audio profile");
    }
    if let Some(n) = num(&s["nb_frames"]) {
        assert_eq!(n as u64, a.nb_frames, "audio nb_frames");
    }
    near(a.duration, &s["duration"], 0.001, "audio duration");
    near(
        a.bit_rate.map(|b| b as f64),
        &s["bit_rate"],
        0.005,
        "audio bit_rate",
    );
}

fn codes(r: &Report) -> Vec<&'static str> {
    r.issues.iter().map(|i| i.code).collect()
}

fn clip(name: &str, video: &str, audio: &str, extra: &[&str]) -> Option<PathBuf> {
    let mut args = vec![
        "-f", "lavfi", "-i", video, "-f", "lavfi", "-i", audio, "-t", "2",
    ];
    args.extend_from_slice(extra);
    encode(name, &args)
}

const TONE: &str = "sine=frequency=440:sample_rate=48000";

#[test]
fn h264_aac_fast_start_is_ready() {
    if !tools() {
        return;
    }
    let Some(f) = clip(
        "h264.mp4",
        "testsrc2=size=1920x1080:rate=30000/1001",
        TONE,
        &[
            "-ac",
            "2",
            "-c:v",
            "libx264",
            "-b:v",
            "10M",
            "-pix_fmt",
            "yuv420p",
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "-colorspace",
            "bt709",
            "-c:a",
            "aac",
            "-b:a",
            "256k",
            "-movflags",
            "+faststart",
        ],
    ) else {
        return;
    };
    let r = media_video::probe_path(&f).unwrap();
    let p = ffprobe(&f);
    same_video(&r, &p);
    same_audio(&r, &p);
    near(
        r.container.duration,
        &p["format"]["duration"],
        0.001,
        "format duration",
    );
    assert!(r.container.fast_start);
    assert!(r.mv_ready, "{:?}", r.issues);
    assert!(r.issues.iter().all(|i| i.severity != Severity::Error));
    assert!(!codes(&r).contains(&"RESOLUTION_BELOW_1080P"));
}

#[test]
fn hevc_main10_hdr_in_mov() {
    if !tools() {
        return;
    }
    let Some(f) = clip(
        "hevc.mov",
        "testsrc2=size=1280x720:rate=25",
        TONE,
        &[
            "-ac",
            "2",
            "-c:v",
            "libx265",
            "-x265-params",
            "log-level=error",
            "-pix_fmt",
            "yuv420p10le",
            "-tag:v",
            "hvc1",
            "-color_primaries",
            "bt2020",
            "-color_trc",
            "smpte2084",
            "-colorspace",
            "bt2020nc",
            "-c:a",
            "pcm_s24le",
        ],
    ) else {
        return;
    };
    let r = media_video::probe_path(&f).unwrap();
    let p = ffprobe(&f);
    same_video(&r, &p);
    same_audio(&r, &p);
    assert_eq!(r.container.format, "mov");
    assert_eq!(r.video[0].hdr, Some("PQ"));
    assert_eq!(r.audio[0].bits_per_sample, Some(24));
    let c = codes(&r);
    assert!(
        c.contains(&"RESOLUTION_BELOW_1080P") && c.contains(&"HDR"),
        "{c:?}"
    );
}

#[test]
fn prores_hq_with_pcm() {
    if !tools() {
        return;
    }
    let Some(f) = clip(
        "prores.mov",
        "testsrc2=size=1920x1080:rate=24000/1001",
        TONE,
        &[
            "-ac",
            "2",
            "-c:v",
            "prores_ks",
            "-profile:v",
            "3",
            "-c:a",
            "pcm_s16le",
        ],
    ) else {
        return;
    };
    let r = media_video::probe_path(&f).unwrap();
    let p = ffprobe(&f);
    same_video(&r, &p);
    same_audio(&r, &p);
    assert_eq!(r.video[0].profile.as_deref(), Some("HQ"));
    assert!(r.mv_ready);
}

#[test]
fn interlaced_and_anamorphic_h264() {
    if !tools() {
        return;
    }
    let Some(f) = clip(
        "interlaced.mp4",
        "testsrc2=size=1440x1080:rate=25",
        TONE,
        &[
            "-ac",
            "2",
            "-vf",
            "setsar=4/3",
            "-c:v",
            "libx264",
            "-flags",
            "+ildct+ilme",
            "-x264opts",
            "tff=1",
            "-c:a",
            "aac",
        ],
    ) else {
        return;
    };
    let r = media_video::probe_path(&f).unwrap();
    same_video(&r, &ffprobe(&f));
    assert_eq!(r.video[0].display_aspect_ratio.as_deref(), Some("16:9"));
    let c = codes(&r);
    assert!(
        c.contains(&"INTERLACED") && c.contains(&"NON_SQUARE_PIXELS"),
        "{c:?}"
    );
}

#[test]
fn full_range_h264() {
    if !tools() {
        return;
    }
    let Some(f) = clip(
        "fullrange.mp4",
        "testsrc2=size=1920x1080:rate=30",
        TONE,
        &[
            "-ac", "2", "-c:v", "libx264", "-pix_fmt", "yuvj420p", "-c:a", "aac",
        ],
    ) else {
        return;
    };
    let r = media_video::probe_path(&f).unwrap();
    same_video(&r, &ffprobe(&f));
    assert_eq!(r.video[0].color_range, Some("pc"));
}

#[test]
fn fragmented_mp4_counts_samples_from_fragments() {
    if !tools() {
        return;
    }
    let Some(f) = clip(
        "frag.mp4",
        "testsrc2=size=1920x1080:rate=25",
        TONE,
        &[
            "-ac",
            "2",
            "-c:v",
            "libx264",
            "-g",
            "25",
            "-c:a",
            "aac",
            "-movflags",
            "frag_keyframe+empty_moov+default_base_moof",
        ],
    ) else {
        return;
    };
    let r = media_video::probe_path(&f).unwrap();
    assert!(r.container.fragments > 1);
    assert_eq!(r.video[0].nb_frames, 50);
    assert_eq!(r.video[0].avg_frame_rate.as_deref(), Some("25/1"));
    // One key frame per second (-g 25), whatever x264 adds at the end.
    assert!(r.video[0].keyframes >= 2);
    assert!(r.video[0].max_keyframe_interval.unwrap() <= 1.0);
    assert!(r.audio[0].nb_frames > 90);
    assert!(r.mv_ready, "{:?}", r.issues);
}

#[test]
fn variable_frame_rate_is_flagged() {
    if !tools() {
        return;
    }
    // 60 fps source: every frame for half a second, then every other one.
    let Some(f) = clip(
        "vfr.mp4",
        "testsrc2=size=1920x1080:rate=60",
        TONE,
        &[
            "-ac",
            "2",
            "-vf",
            "select='lt(n\\,30)+not(mod(n\\,2))'",
            "-fps_mode",
            "vfr",
            "-enc_time_base",
            "1/60",
            "-c:v",
            "libx264",
            "-c:a",
            "aac",
        ],
    ) else {
        return;
    };
    let r = media_video::probe_path(&f).unwrap();
    same_video(&r, &ffprobe(&f));
    assert!(r.video[0].variable_frame_rate);
    assert!(codes(&r).contains(&"VARIABLE_FRAME_RATE"));
}

#[test]
fn rotation_and_mono_low_rate_audio() {
    if !tools() {
        return;
    }
    let Some(plain) = encode(
        "unrotated.mp4",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=1920x1080:rate=30",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=22050",
            "-t",
            "2",
            "-c:v",
            "libx264",
            "-c:a",
            "aac",
            "-ac",
            "1",
        ],
    ) else {
        return;
    };
    let plain = plain.to_str().unwrap().to_owned();
    let Some(f) = encode(
        "rotated.mp4",
        &["-display_rotation", "90", "-i", &plain, "-c", "copy"],
    ) else {
        return;
    };
    let r = media_video::probe_path(&f).unwrap();
    let p = ffprobe(&f);
    same_audio(&r, &p);
    let side = &stream(&p, "video")["side_data_list"][0]["rotation"];
    assert_eq!(side.as_i64(), Some(90));
    assert_eq!(r.video[0].rotation, 90);
    let c = codes(&r);
    for code in ["ROTATED", "AUDIO_CHANNELS", "AUDIO_SAMPLE_RATE"] {
        assert!(c.contains(&code), "{code} in {c:?}");
    }
}

#[test]
fn silent_video_and_audio_only_are_not_ready() {
    if !tools() {
        return;
    }
    if let Some(f) = encode(
        "silent.mp4",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=1920x1080:rate=30",
            "-t",
            "1",
            "-c:v",
            "libx264",
        ],
    ) {
        let r = media_video::probe_path(&f).unwrap();
        assert!(!r.mv_ready);
        assert!(codes(&r).contains(&"NO_AUDIO"));
    }
    if let Some(f) = encode(
        "song.m4a",
        &["-f", "lavfi", "-i", TONE, "-t", "1", "-c:a", "aac"],
    ) {
        let r = media_video::probe_path(&f).unwrap();
        assert!(!r.mv_ready);
        assert!(codes(&r).contains(&"NO_VIDEO"));
    }
}

#[test]
fn truncated_upload_is_an_error() {
    if !tools() {
        return;
    }
    let Some(f) = clip(
        "whole.mp4",
        "testsrc2=size=1920x1080:rate=30",
        TONE,
        &[
            "-ac",
            "2",
            "-c:v",
            "libx264",
            "-c:a",
            "aac",
            "-movflags",
            "+faststart",
        ],
    ) else {
        return;
    };
    let bytes = std::fs::read(&f).unwrap();
    let cut = dir().join("cut.mp4");
    std::fs::write(&cut, &bytes[..bytes.len() * 2 / 3]).unwrap();
    let r = media_video::probe_path(&cut).unwrap();
    assert!(!r.mv_ready);
    assert!(codes(&r).contains(&"TRUNCATED"), "{:?}", r.issues);
}

#[test]
fn other_codecs_are_reported() {
    if !tools() {
        return;
    }
    for (name, vcodec, acodec) in [
        ("vp9.mp4", "libvpx-vp9", "libopus"),
        ("av1.mp4", "libaom-av1", "flac"),
        ("alac.mov", "mpeg4", "alac"),
    ] {
        let mut extra = vec!["-ac", "2", "-c:v", vcodec, "-c:a", acodec];
        if vcodec == "libaom-av1" {
            extra.extend_from_slice(&["-cpu-used", "8", "-b:v", "1M"]);
        }
        if vcodec == "libvpx-vp9" {
            extra.extend_from_slice(&["-deadline", "realtime", "-b:v", "1M"]);
        }
        let Some(f) = clip(name, "testsrc2=size=640x360:rate=24", TONE, &extra) else {
            continue;
        };
        let r = media_video::probe_path(&f).unwrap();
        let p = ffprobe(&f);
        same_video(&r, &p);
        same_audio(&r, &p);
        assert!(codes(&r).contains(&"VIDEO_CODEC"), "{name}");
    }
}

#[test]
fn not_a_video() {
    let d = dir();
    let png = d.join("x.png");
    std::fs::write(&png, b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR").unwrap();
    assert_eq!(media_video::probe_path(&png).unwrap_err().code(), "INVALID");
    let empty_moov = d.join("empty.mp4");
    std::fs::write(&empty_moov, b"\0\0\0\x10ftypisom\0\0\0\0\0\0\0\x08moov").unwrap();
    assert_eq!(
        media_video::probe_path(&empty_moov).unwrap_err().code(),
        "INVALID"
    );
}

/// Damaged indexes give a report or an error, never a panic (debug builds
/// check arithmetic overflow too). `fuzz/fuzz_targets/video.rs` goes further.
#[test]
fn corrupted_indexes_do_not_panic() {
    if !tools() {
        return;
    }
    let Some(f) = clip(
        "mutate.mp4",
        "testsrc2=size=320x240:rate=25",
        TONE,
        &[
            "-ac",
            "2",
            "-c:v",
            "libx264",
            "-g",
            "10",
            "-c:a",
            "aac",
            "-movflags",
            "+faststart",
        ],
    ) else {
        return;
    };
    let original = std::fs::read(&f).unwrap();
    let moov_end = original.windows(4).position(|w| w == b"mdat").unwrap();
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..4000 {
        let mut bytes = original.clone();
        for _ in 0..1 + next() % 4 {
            let at = (next() % moov_end as u64) as usize;
            bytes[at] = match next() % 3 {
                0 => next() as u8,
                1 => 0xff,
                _ => 0,
            };
        }
        let _ = media_video::probe(&mut std::io::Cursor::new(bytes));
    }
}
