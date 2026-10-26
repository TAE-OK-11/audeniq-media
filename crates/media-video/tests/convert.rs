//! `convert`: the picture comes out bit for bit, the sound in the asked
//! format, read back by FFprobe/FFmpeg. Skipped (passes) when ffmpeg is not
//! installed; CI installs it.
use media_json::Value;
use media_video::convert::{Codec, Container, Options, convert, preset};
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
    let d = std::env::temp_dir().join(format!("media-video-convert-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

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

/// Hashes of every video packet, as FFmpeg reads them.
fn video_md5(path: &Path) -> Vec<String> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:v", "-c", "copy", "-f", "framemd5", "-"])
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(String::from)
        .collect()
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

fn audio(p: &Value) -> &Value {
    p["streams"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["codec_type"].as_str() == Some("audio"))
        .unwrap()
}

fn secs(v: &Value) -> f64 {
    v.as_str().unwrap().parse().unwrap()
}

/// FFmpeg's decode of the sound, interleaved.
fn decoded(path: &Path) -> Vec<f32> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:a", "-f", "f32le", "-c:a", "pcm_f32le", "-"])
        .output()
        .unwrap();
    assert!(out.status.success());
    out.stdout
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

const TONES: &str =
    "aevalsrc=0.3*sin(2*PI*440*t)+0.2*sin(2*PI*3000*t)|0.3*sin(2*PI*660*t)+0.1*sin(2*PI*9000*t)";

#[test]
fn h264_aac_44100_to_aac_48000() {
    if !tools() {
        return;
    }
    let Some(src) = encode(
        "h264-aac.mp4",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=320x240:r=30000/1001:d=4",
            "-f",
            "lavfi",
            "-i",
            &format!("{TONES}:s=44100:d=4"),
            "-c:v",
            "libx264",
            "-bf",
            "2",
            "-c:a",
            "aac",
            "-b:a",
            "128k",
            "-shortest",
        ],
    ) else {
        return;
    };
    let out = dir().join("h264-aac-out.mp4");
    let c = convert(&src, &out, &preset("aac-320").unwrap()).unwrap();
    assert_eq!(
        (c.source_sound.sample_rate, c.sample_rate, c.channels),
        (44100, 48000, 2)
    );
    assert_eq!(c.copied_tracks, [1]);
    assert_eq!(video_md5(&src), video_md5(&out));
    let p = ffprobe(&out);
    let a = audio(&p);
    assert_eq!(a["codec_name"].as_str(), Some("aac"));
    assert_eq!(a["sample_rate"].as_str(), Some("48000"));
    assert_eq!(a["channels"].as_i64(), Some(2));
    assert!(
        (secs(&a["duration"]) - 4.0).abs() < 0.03,
        "{}",
        a["duration"]
    );
    assert!(secs(&a["start_time"]).abs() < 1e-6);
    // `moov` before `mdat`: the file streams.
    let head = std::fs::read(&out).unwrap();
    let moov = head.windows(4).position(|w| w == b"moov").unwrap();
    let mdat = head.windows(4).position(|w| w == b"mdat").unwrap();
    assert!(moov < mdat);
    // The tones come through at the same time and level.
    let pcm = decoded(&out);
    let t = 2.0f64;
    let at = (t * 48000.0) as usize;
    let expect = 0.3 * (std::f64::consts::TAU * 440.0 * t).sin()
        + 0.2 * (std::f64::consts::TAU * 3000.0 * t).sin();
    let rms_err = (0..480)
        .map(|i| {
            let t = (at + i) as f64 / 48000.0;
            let e = 0.3 * (std::f64::consts::TAU * 440.0 * t).sin()
                + 0.2 * (std::f64::consts::TAU * 3000.0 * t).sin();
            (e - f64::from(pcm[(at + i) * 2])).powi(2)
        })
        .sum::<f64>()
        / 480.0;
    assert!(
        rms_err.sqrt() < 0.01,
        "left channel off by {:.4} near {expect:.3}",
        rms_err.sqrt()
    );
}

#[test]
fn prores_pcm_51_to_pcm24_mov_keeps_timecode() {
    if !tools() {
        return;
    }
    let Some(src) = encode(
        "prores.mov",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=320x240:r=24:d=2",
            "-f",
            "lavfi",
            "-i",
            "aevalsrc=0.3*sin(2*PI*440*t)|0.3*sin(2*PI*550*t)|0.3*sin(2*PI*660*t)|0.3*sin(2*PI*50*t)|0.2*sin(2*PI*880*t)|0.2*sin(2*PI*990*t):s=48000:d=2:c=5.1",
            "-c:v",
            "prores_ks",
            "-c:a",
            "pcm_s16le",
            "-timecode",
            "01:00:00:00",
        ],
    ) else {
        return;
    };
    let mp4 = dir().join("prores-out.mp4");
    let mp4_opts = Options {
        container: Container::Mp4,
        codec: Codec::Aac(320_000),
        sample_rate: 48000,
    };
    assert!(
        convert(&src, &mp4, &mp4_opts).is_err(),
        "ProRes does not go in .mp4"
    );
    let out = dir().join("prores-out.mov");
    let c = convert(&src, &out, &preset("apple-music").unwrap()).unwrap();
    assert_eq!(c.copied_tracks.len(), 2, "video and timecode");
    assert_eq!(video_md5(&src), video_md5(&out));
    let p = ffprobe(&out);
    let a = audio(&p);
    assert_eq!(a["codec_name"].as_str(), Some("pcm_s24le"));
    assert_eq!(a["channels"].as_i64(), Some(2));
    let tc = p["streams"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["tags"]["timecode"].as_str() == Some("01:00:00:00"));
    assert!(tc);
    // L = FL + C/√2 + SL/√2 exactly (PCM in, PCM out, same rate).
    let src_pcm = decoded(&src);
    let out_pcm = decoded(&out);
    assert_eq!(src_pcm.len() / 6, out_pcm.len() / 2);
    let k = std::f32::consts::FRAC_1_SQRT_2;
    for (s, o) in src_pcm.chunks(6).zip(out_pcm.chunks(2)).step_by(97) {
        assert!((s[0] + k * s[2] + k * s[4] - o[0]).abs() < 1e-5);
        assert!((s[1] + k * s[2] + k * s[5] - o[1]).abs() < 1e-5);
    }
}

#[test]
fn no_sound_is_an_error() {
    if !tools() {
        return;
    }
    let Some(src) = encode(
        "silent.mp4",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=160x120:r=25:d=1",
            "-c:v",
            "libx264",
        ],
    ) else {
        return;
    };
    assert!(
        convert(
            &src,
            &dir().join("silent-out.mp4"),
            &preset("aac-320").unwrap()
        )
        .is_err()
    );
}
