//! Platform profiles (`probe --profile`) on clips FFmpeg makes here.
//! Skipped (passes) when ffmpeg is not installed; CI installs it.
use media_video::check::profile;
use media_video::probe_path_for;
use std::path::{Path, PathBuf};
use std::process::Command;

fn encode(name: &str, args: &[&str]) -> Option<PathBuf> {
    let d = std::env::temp_dir().join(format!("media-video-profile-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let out = d.join(name);
    let st = Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args(args)
        .arg(&out)
        .output()
        .ok()?;
    if !st.status.success() {
        eprintln!("skip {name}: {}", String::from_utf8_lossy(&st.stderr));
        return None;
    }
    Some(out)
}

/// The `PROFILE_*` codes `name` raises on `file`, measured with sound.
fn codes(file: &Path, name: &str) -> Vec<&'static str> {
    let r = probe_path_for(file, true, profile(name)).unwrap();
    assert_eq!(r.profile, profile(name).map(|p| p.version));
    let mut c: Vec<_> = r
        .issues
        .iter()
        .map(|i| i.code)
        .filter(|c| c.starts_with("PROFILE_"))
        .collect();
    c.sort_unstable();
    c
}

const VIDEO_1080: &[&str] = &[
    "-f",
    "lavfi",
    "-i",
    "testsrc2=size=1920x1080:rate=24:duration=2",
];
/// A quiet tone (about −20 LUFS), so loudness rules stay silent.
const TONE_48K: &[&str] = &[
    "-f",
    "lavfi",
    "-i",
    "sine=frequency=440:sample_rate=48000:duration=2,volume=0.1,aformat=channel_layouts=stereo",
];

#[test]
fn youtube_ready_mp4_raises_nothing() {
    let Some(f) = encode(
        "yt.mp4",
        &[
            VIDEO_1080,
            TONE_48K,
            &[
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-color_primaries",
                "bt709",
                "-c:a",
                "aac",
                "-b:a",
                "320k",
                "-movflags",
                "+faststart",
            ],
        ]
        .concat(),
    ) else {
        return;
    };
    assert_eq!(codes(&f, "youtube"), Vec::<&str>::new());
    assert_eq!(codes(&f, "spotify"), Vec::<&str>::new());
    // Apple wants ProRes and PCM in MOV.
    assert_eq!(
        codes(&f, "apple"),
        [
            "PROFILE_AUDIO_CODEC",
            "PROFILE_CONTAINER",
            "PROFILE_VIDEO_CODEC"
        ]
    );
}

#[test]
fn off_spec_mp4_is_flagged() {
    // 4:3, 4:2:2, 44.1 kHz, index at the end, sound delayed by an edit list.
    let Some(f) = encode(
        "off.mp4",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=1440x1080:rate=24:duration=2",
            "-itsoffset",
            "0.5",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=44100:duration=2,volume=0.1",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv422p",
            "-c:a",
            "aac",
            "-ac",
            "2",
        ],
    ) else {
        return;
    };
    assert_eq!(
        codes(&f, "youtube"),
        [
            "PROFILE_ASPECT_RATIO",
            "PROFILE_AUDIO_SAMPLE_RATE",
            "PROFILE_CHROMA",
            "PROFILE_EDIT_LIST",
            "PROFILE_INDEX_AT_END",
        ]
    );
}

#[test]
fn apple_prores_mov_raises_nothing() {
    let Some(f) = encode(
        "apple.mov",
        &[
            VIDEO_1080,
            TONE_48K,
            &[
                "-c:v",
                "prores_ks",
                "-profile:v",
                "3",
                "-color_primaries",
                "bt709",
                "-color_trc",
                "bt709",
                "-colorspace",
                "bt709",
                "-c:a",
                "pcm_s24le",
            ],
        ]
        .concat(),
    ) else {
        return;
    };
    assert_eq!(codes(&f, "apple"), Vec::<&str>::new());
    assert_eq!(codes(&f, "spotify"), Vec::<&str>::new());
}

#[test]
fn loud_master_over_the_ceiling() {
    // A near-full-scale tone: far louder than −14 LUFS, peaks near 0 dBTP.
    let Some(f) = encode(
        "loud.mp4",
        &[
            VIDEO_1080,
            &[
                "-f",
                "lavfi",
                "-i",
                "aevalsrc=0.95*sin(2*PI*997*t)|0.95*sin(2*PI*997*t):s=48000:d=2",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-b:a",
                "320k",
                "-movflags",
                "+faststart",
            ],
        ]
        .concat(),
    ) else {
        return;
    };
    assert!(codes(&f, "spotify").contains(&"PROFILE_TRUE_PEAK"));
}

#[test]
fn unknown_profile_is_none() {
    assert!(profile("tiktok").is_none());
    assert!(profile("youtube").is_some());
}
