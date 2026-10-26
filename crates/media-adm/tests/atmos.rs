//! ADM BWF masters built here the way a Dolby Atmos renderer lays them out
//! (BW64 + ds64, 7.1.2 bed pack, object packs, chna, axml, dbmd), plus the
//! ways they go wrong.
use media_adm::{Report, probe};
use std::io::Cursor;

const BED: [&str; 10] = [
    "RC_L", "RC_R", "RC_C", "RC_LFE", "RC_Lss", "RC_Rss", "RC_Lrs", "RC_Rrs", "RC_Lts", "RC_Rts",
];

struct Master {
    kind: &'static [u8; 4],
    rate: u32,
    bits: u16,
    objects: usize,
    frames: usize,
    integrated: Option<f64>,
    true_peak: Option<f64>,
    chna: bool,
    bad_uid: bool,
    clip: bool,
}

impl Default for Master {
    fn default() -> Self {
        Master {
            kind: b"BW64",
            rate: 48000,
            bits: 24,
            objects: 2,
            frames: 4800,
            integrated: Some(-20.0),
            true_peak: Some(-2.0),
            chna: true,
            bad_uid: false,
            clip: false,
        }
    }
}

fn axml(m: &Master) -> String {
    let tracks = BED.len() + m.objects;
    let dur = m.frames as f64 / f64::from(m.rate);
    let mut x = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?><ebuCoreMain xmlns="urn:ebu:metadata-schema:ebuCore_2016"><coreMetadata><format><audioFormatExtended>"#,
    );
    x += &format!(
        r#"<audioProgramme audioProgrammeID="APR_1001" audioProgrammeName="Song" start="00:00:00.00000" end="00:00:{dur:08.5}"><audioContentIDRef>ACO_1001</audioContentIDRef>"#
    );
    if m.integrated.is_some() || m.true_peak.is_some() {
        x += "<loudnessMetadata>";
        if let Some(i) = m.integrated {
            x += &format!("<integratedLoudness>{i}</integratedLoudness>");
        }
        if let Some(t) = m.true_peak {
            x += &format!("<maxTruePeak>{t}</maxTruePeak>");
        }
        x += "</loudnessMetadata>";
    }
    x += "</audioProgramme><audioContent audioContentID=\"ACO_1001\" audioContentName=\"Song\">";
    x += "<audioObjectIDRef>AO_1001</audioObjectIDRef>";
    for o in 0..m.objects {
        x += &format!("<audioObjectIDRef>AO_{:04X}</audioObjectIDRef>", 0x1002 + o);
    }
    x += "</audioContent><audioObject audioObjectID=\"AO_1001\" audioObjectName=\"Bed\"><audioPackFormatIDRef>AP_00011001</audioPackFormatIDRef>";
    for t in 0..BED.len() {
        x += &format!("<audioTrackUIDRef>ATU_{:08X}</audioTrackUIDRef>", t + 1);
    }
    x += "</audioObject>";
    for o in 0..m.objects {
        x += &format!(
            "<audioObject audioObjectID=\"AO_{:04X}\" audioObjectName=\"Obj {o}\"><audioPackFormatIDRef>AP_{:08X}</audioPackFormatIDRef><audioTrackUIDRef>ATU_{:08X}</audioTrackUIDRef></audioObject>",
            0x1002 + o,
            0x0003_1001 + o,
            BED.len() + o + 1
        );
    }
    x += "<audioPackFormat audioPackFormatID=\"AP_00011001\" audioPackFormatName=\"Bed 7.1.2\" typeLabel=\"0001\" typeDefinition=\"DirectSpeakers\">";
    for c in 0..BED.len() {
        x += &format!(
            "<audioChannelFormatIDRef>AC_{:08X}</audioChannelFormatIDRef>",
            0x0001_1001 + c
        );
    }
    x += "</audioPackFormat>";
    for o in 0..m.objects {
        x += &format!(
            "<audioPackFormat audioPackFormatID=\"AP_{:08X}\" audioPackFormatName=\"Obj\" typeLabel=\"0003\" typeDefinition=\"Objects\"><audioChannelFormatIDRef>AC_{:08X}</audioChannelFormatIDRef></audioPackFormat>",
            0x0003_1001 + o,
            0x0003_1001 + o
        );
    }
    for (c, label) in BED.iter().enumerate() {
        x += &format!(
            "<audioChannelFormat audioChannelFormatID=\"AC_{:08X}\" audioChannelFormatName=\"{label}\" typeLabel=\"0001\" typeDefinition=\"DirectSpeakers\"><audioBlockFormat audioBlockFormatID=\"AB_{:08X}_00000001\"><speakerLabel>{label}</speakerLabel><position coordinate=\"azimuth\">0</position></audioBlockFormat></audioChannelFormat>",
            0x0001_1001 + c,
            0x0001_1001 + c
        );
    }
    for o in 0..m.objects {
        x += &format!(
            "<audioChannelFormat audioChannelFormatID=\"AC_{:08X}\" audioChannelFormatName=\"Obj\" typeLabel=\"0003\" typeDefinition=\"Objects\">",
            0x0003_1001 + o
        );
        for b in 0..3 {
            x += &format!(
                "<audioBlockFormat audioBlockFormatID=\"AB_{:08X}_{:08X}\" rtime=\"00:00:0{b}.00000\"><position coordinate=\"X\">0.5</position></audioBlockFormat>",
                0x0003_1001 + o,
                b + 1
            );
        }
        x += "</audioChannelFormat>";
    }
    for t in 0..tracks {
        x += &format!(
            "<audioTrackUID UID=\"ATU_{:08X}\" sampleRate=\"{}\" bitDepth=\"{}\"/>",
            t + 1,
            m.rate,
            m.bits
        );
    }
    x += "</audioFormatExtended></format></coreMetadata></ebuCoreMain>";
    x
}

fn chunk(id: &[u8; 4], body: &[u8], size_field: Option<u32>) -> Vec<u8> {
    let mut v = id.to_vec();
    v.extend(size_field.unwrap_or(body.len() as u32).to_le_bytes());
    v.extend(body);
    if body.len() % 2 == 1 {
        v.push(0);
    }
    v
}

fn build(m: &Master) -> Vec<u8> {
    let tracks = BED.len() + m.objects;
    let bytes = usize::from(m.bits / 8);
    let mut fmt = Vec::new();
    fmt.extend(1u16.to_le_bytes());
    fmt.extend((tracks as u16).to_le_bytes());
    fmt.extend(m.rate.to_le_bytes());
    fmt.extend((m.rate * (tracks * bytes) as u32).to_le_bytes());
    fmt.extend(((tracks * bytes) as u16).to_le_bytes());
    fmt.extend(m.bits.to_le_bytes());
    let mut chna = Vec::new();
    chna.extend((tracks as u16).to_le_bytes());
    chna.extend((tracks as u16).to_le_bytes());
    for t in 0..tracks {
        chna.extend((t as u16 + 1).to_le_bytes());
        let uid = if m.bad_uid && t == 0 {
            "ATU_DEADBEEF".to_string()
        } else {
            format!("ATU_{:08X}", t + 1)
        };
        chna.extend(uid.as_bytes());
        let (track, pack) = if t < BED.len() {
            (
                format!("AT_{:08X}_01", 0x0001_1001 + t),
                "AP_00011001".to_string(),
            )
        } else {
            let o = t - BED.len();
            (
                format!("AT_{:08X}_01", 0x0003_1001 + o),
                format!("AP_{:08X}", 0x0003_1001 + o),
            )
        };
        chna.extend(track.as_bytes());
        chna.extend(pack.as_bytes());
        chna.push(0);
    }
    let mut data = Vec::with_capacity(m.frames * tracks * bytes);
    for f in 0..m.frames {
        for t in 0..tracks {
            // Track 4 (LFE) silent; the others a tone at −20 dBFS.
            let v = if t == 3 {
                0.0
            } else if m.clip && t == 0 && f == 100 {
                1.0
            } else {
                0.1 * (f as f64 * (t + 1) as f64 * 0.01).sin()
            };
            let full = f64::from((1u32 << (m.bits - 1)) - 1);
            let s = (v * full).round() as i32;
            data.extend(&s.to_le_bytes()[..bytes]);
        }
    }
    let wide = m.kind != b"RIFF";
    let mut body = b"WAVE".to_vec();
    if wide {
        let mut ds64 = vec![0u8; 28];
        ds64[8..16].copy_from_slice(&(data.len() as u64).to_le_bytes());
        ds64[16..24].copy_from_slice(&(m.frames as u64).to_le_bytes());
        body.extend(chunk(b"ds64", &ds64, None));
    }
    body.extend(chunk(b"fmt ", &fmt, None));
    if m.chna {
        body.extend(chunk(b"chna", &chna, None));
    }
    body.extend(chunk(b"axml", axml(m).as_bytes(), None));
    body.extend(chunk(b"dbmd", &[0, 0, 0, 7, 0], None));
    body.extend(chunk(b"data", &data, wide.then_some(u32::MAX)));
    let mut file = m.kind.to_vec();
    file.extend(if wide { u32::MAX } else { body.len() as u32 }.to_le_bytes());
    file.extend(body);
    file
}

fn run(m: &Master, peaks: bool) -> Report {
    probe(&mut Cursor::new(build(m)), peaks).unwrap()
}

fn codes(r: &Report) -> Vec<&'static str> {
    r.issues.iter().map(|i| i.code).collect()
}

#[test]
fn a_renderer_master_is_deliverable() {
    let r = run(&Master::default(), true);
    assert!(r.deliverable, "{:?}", r.issues);
    assert_eq!(r.container.format, "BW64");
    assert!(r.container.dolby_metadata);
    let a = r.audio.as_ref().unwrap();
    assert_eq!(
        (a.codec_name.as_str(), a.sample_rate, a.channels, a.frames),
        ("pcm_s24le", 48000, 12, 4800)
    );
    let adm = r.adm.as_ref().unwrap();
    assert_eq!(
        (adm.bed_tracks, adm.object_tracks, adm.chna_tracks),
        (10, 2, 12)
    );
    assert_eq!(adm.beds[0].layout, "7.1.2");
    assert_eq!(adm.programme.as_deref(), Some("Song"));
    assert_eq!(adm.programme_duration, Some(0.1));
    assert_eq!(adm.loudness.as_ref().unwrap().integrated, Some(-20.0));
    let p = r.peaks.as_ref().unwrap();
    assert_eq!(p.silent_tracks, [4]);
    assert!((p.tracks[0].unwrap() + 20.0).abs() < 0.1, "{:?}", p.tracks);
    assert_eq!(codes(&r), ["SILENT_TRACKS"]);
}

#[test]
fn riff_and_rf64_read_the_same() {
    for kind in [b"RIFF", b"RF64"] {
        let r = run(
            &Master {
                kind,
                ..Master::default()
            },
            false,
        );
        assert!(r.deliverable, "{:?}", r.issues);
        assert_eq!(r.audio.unwrap().frames, 4800);
    }
}

#[test]
fn delivery_problems() {
    let cases: Vec<(Master, &str)> = vec![
        (
            Master {
                chna: false,
                ..Master::default()
            },
            "NO_ADM",
        ),
        (
            Master {
                bits: 16,
                ..Master::default()
            },
            "BIT_DEPTH",
        ),
        (
            Master {
                rate: 44100,
                ..Master::default()
            },
            "SAMPLE_RATE",
        ),
        (
            Master {
                objects: 119,
                frames: 10,
                ..Master::default()
            },
            "TOO_MANY_OBJECTS",
        ),
        (
            Master {
                bad_uid: true,
                ..Master::default()
            },
            "CHNA_UID_UNKNOWN",
        ),
    ];
    for (m, code) in cases {
        let r = run(&m, false);
        assert!(!r.deliverable, "{code}");
        assert!(codes(&r).contains(&code), "{code}: {:?}", r.issues);
    }
    let r = run(
        &Master {
            integrated: Some(-14.0),
            true_peak: Some(0.3),
            ..Master::default()
        },
        false,
    );
    assert!(r.deliverable);
    assert!(codes(&r).contains(&"LOUDNESS_HIGH") && codes(&r).contains(&"TRUE_PEAK_HIGH"));
    let r = run(
        &Master {
            integrated: None,
            true_peak: None,
            ..Master::default()
        },
        false,
    );
    assert!(codes(&r).contains(&"LOUDNESS_MISSING"));
    let r = run(
        &Master {
            rate: 96000,
            ..Master::default()
        },
        false,
    );
    assert!(r.deliverable && codes(&r).contains(&"SAMPLE_RATE_96K"));
    let r = run(
        &Master {
            clip: true,
            ..Master::default()
        },
        true,
    );
    assert!(codes(&r).contains(&"CLIPPING"), "{:?}", r.issues);
}

#[test]
fn truncated_and_damaged_files() {
    let whole = build(&Master::default());
    let cut = whole[..whole.len() - 1000].to_vec();
    let r = probe(&mut Cursor::new(cut), true).unwrap();
    assert!(codes(&r).contains(&"TRUNCATED") && !r.deliverable);
    assert!(probe(&mut Cursor::new(b"RIFF\0\0\0\0AVI ".to_vec()), false).is_err());
    // Random damage: a report or an error, never a panic.
    let mut seed = 0x1234_5678_9abc_def0u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let head = whole.len() - 4800 * 12 * 3;
    for _ in 0..3000 {
        let mut b = whole.clone();
        for _ in 0..1 + next() % 4 {
            let i = (next() % head as u64) as usize;
            b[i] = next() as u8;
        }
        let _ = probe(&mut Cursor::new(b), next() % 2 == 0);
    }
}

#[test]
fn ffprobe_reads_the_same_format() {
    let ok = std::process::Command::new("ffprobe")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        return;
    }
    let path = std::env::temp_dir().join(format!("media-adm-{}.wav", std::process::id()));
    std::fs::write(&path, build(&Master::default())).unwrap();
    let out = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_name,sample_rate,channels",
            "-of",
            "csv=p=0",
        ])
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "pcm_s24le,48000,12"
    );
}
