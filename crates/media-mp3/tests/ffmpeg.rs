//! Decoded PCM against FFmpeg's MP3 decoder on streams LAME (FFmpeg's
//! `libmp3lame`) makes here, without a Xing/LAME tag so both decoders
//! start at the first frame. Skipped when ffmpeg or libmp3lame is missing;
//! CI installs them.
use std::path::PathBuf;
use std::process::Command;

fn dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("media-mp3-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Music-like test signal: tones, noise and sharp transients (short
/// blocks).
const SIGNAL: &str = "0.4*sin(2*PI*440*t)+0.2*sin(2*PI*3150*t+1)+0.1*sin(2*PI*9000*t)\
    +0.6*lt(mod(t\\,0.37)\\,0.004)*sin(2*PI*1800*t)+0.05*(random(0)-0.5)\
    |0.3*sin(2*PI*660*t)+0.25*sin(2*PI*5200*t)+0.6*lt(mod(t+0.1\\,0.29)\\,0.003)+0.08*(random(1)-0.5)";

/// (MP3 stream, FFmpeg's decode as interleaved f32); `None` without LAME.
fn encode(name: &str, rate: u32, opts: &[&str]) -> Option<(Vec<u8>, Vec<f32>)> {
    let mp3 = dir().join(format!("{name}.mp3"));
    let st = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("aevalsrc={SIGNAL}:s={rate}:d=3"))
        .args(opts)
        .args([
            "-c:a",
            "libmp3lame",
            "-write_xing",
            "0",
            "-id3v2_version",
            "0",
        ])
        .arg(&mp3)
        .output()
        .ok()?;
    if !st.status.success() {
        eprintln!("skip {name}: {}", String::from_utf8_lossy(&st.stderr));
        return None;
    }
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&mp3)
        .args(["-f", "f32le", "-c:a", "pcm_f32le", "-"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let pcm = out
        .stdout
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    Some((std::fs::read(&mp3).unwrap(), pcm))
}

/// Frames split by their headers' lengths (no free format here).
fn frames(stream: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 4 <= stream.len() {
        let h = &stream[at..];
        assert!(h[0] == 0xff && h[1] & 0xe0 == 0xe0, "sync at {at}");
        let lsf = h[1] >> 3 & 3 != 3;
        let mpeg25 = h[1] >> 3 & 3 == 0;
        let br = [
            [
                0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
            ],
            [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
        ][usize::from(lsf)][usize::from(h[2] >> 4)];
        let sr = [44100usize, 48000, 32000][usize::from(h[2] >> 2 & 3)]
            >> (usize::from(lsf) + usize::from(mpeg25));
        let len = (if lsf { 72 } else { 144 }) * br * 1000 / sr + usize::from(h[2] >> 1 & 1);
        out.push(&stream[at..(at + len).min(stream.len())]);
        at += len;
    }
    out
}

fn check(name: &str, rate: u32, channels: usize, opts: &[&str]) {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        return;
    }
    let Some((stream, reference)) = encode(name, rate, opts) else {
        return;
    };
    let mut d = media_mp3::Decoder::new();
    let mut ours = Vec::new();
    for f in frames(&stream) {
        let info = d.decode(f, &mut ours).unwrap();
        assert_eq!(
            (info.sample_rate, info.channels),
            (rate, channels),
            "{name}"
        );
    }
    assert_eq!(ours.len(), reference.len(), "{name}: length");
    let max = ours
        .iter()
        .zip(&reference)
        .map(|(a, b)| (a - b).abs())
        .fold(0f32, f32::max);
    assert!(max < 2e-5, "{name}: max difference {max}");
}

#[test]
fn mpeg1_joint_stereo_128k() {
    check("s44-128", 44100, 2, &["-b:a", "128k"]);
}

#[test]
fn mpeg1_stereo_320k_48k() {
    check("s48-320", 48000, 2, &["-b:a", "320k", "-joint_stereo", "0"]);
}

#[test]
fn mpeg1_vbr() {
    check("s44-vbr", 44100, 2, &["-q:a", "2"]);
}

#[test]
fn mpeg2_mono_22k() {
    check("m22-64", 22050, 1, &["-ac", "1", "-b:a", "64k"]);
}

#[test]
fn mpeg25_stereo_11k() {
    check("s11-32", 11025, 2, &["-b:a", "32k"]);
}
