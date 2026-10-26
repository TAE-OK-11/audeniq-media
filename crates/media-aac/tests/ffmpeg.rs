//! Decoded PCM against FFmpeg's AAC decoder on streams FFmpeg's encoder
//! makes here (ADTS, so both decoders start at the same frame). Skipped when
//! ffmpeg is not installed; CI installs it.
use std::path::PathBuf;
use std::process::Command;

fn ffmpeg_ok() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("media-aac-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Music-like test signal: tones, noise and sharp transients (short
/// windows), `seconds` long.
fn source(rate: u32, channels: u32, seconds: f32) -> Vec<String> {
    let expr = "0.4*sin(2*PI*440*t)+0.2*sin(2*PI*3150*t+1)+0.1*sin(2*PI*9000*t)\
                +0.6*lt(mod(t\\,0.37)\\,0.004)*sin(2*PI*1800*t)+0.05*(random(0)-0.5)";
    let right = "0.3*sin(2*PI*660*t)+0.25*sin(2*PI*5200*t)+0.6*lt(mod(t+0.1\\,0.29)\\,0.003)+0.08*(random(1)-0.5)";
    let e = if channels == 6 {
        format!(
            "aevalsrc={expr}|{right}|0.3*sin(2*PI*330*t)|0.2*sin(2*PI*55*t)|{right}|{expr}:s={rate}:d={seconds}:c=5.1"
        )
    } else if channels == 2 {
        format!("aevalsrc={expr}|{right}:s={rate}:d={seconds}")
    } else {
        format!("aevalsrc={expr}:s={rate}:d={seconds}")
    };
    vec!["-f".into(), "lavfi".into(), "-i".into(), e]
}

/// (ADTS stream, FFmpeg's decode as interleaved f32).
fn encode(name: &str, rate: u32, channels: u32, opts: &[&str]) -> (Vec<u8>, Vec<f32>) {
    let d = dir();
    let aac = d.join(format!("{name}.aac"));
    let st = Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args(source(rate, channels, 3.0))
        .args(["-c:a", "aac"])
        .args(opts)
        .arg(&aac)
        .status()
        .unwrap();
    assert!(st.success(), "{name}");
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&aac)
        .args(["-f", "f32le", "-c:a", "pcm_f32le", "-"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let pcm = out
        .stdout
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    (std::fs::read(&aac).unwrap(), pcm)
}

/// ADTS frames → (AudioSpecificConfig, raw access units).
fn adts(stream: &[u8]) -> (Vec<u8>, Vec<&[u8]>) {
    let mut at = 0;
    let mut units = Vec::new();
    let mut asc = Vec::new();
    while at + 7 <= stream.len() {
        let h = &stream[at..];
        assert_eq!(h[0], 0xff);
        let protection_absent = h[1] & 1 == 1;
        let profile = u32::from(h[2] >> 6) + 1;
        let sf = u32::from(h[2] >> 2 & 0xf);
        let ch = u32::from(h[2] & 1) << 2 | u32::from(h[3] >> 6);
        let len = (usize::from(h[3] & 3) << 11) | (usize::from(h[4]) << 3) | usize::from(h[5] >> 5);
        let hdr = if protection_absent { 7 } else { 9 };
        if asc.is_empty() {
            let v = profile << 11 | sf << 7 | ch << 3;
            asc = vec![(v >> 8) as u8, v as u8];
        }
        units.push(&stream[at + hdr..at + len]);
        at += len;
    }
    (asc, units)
}

/// Decoded PCM in FFmpeg's channel order (5.1: L R C LFE Ls Rs).
fn decode(asc: &[u8], units: &[&[u8]]) -> (Vec<f32>, usize) {
    let mut d = media_aac::Decoder::new(asc).unwrap();
    let mut out = Vec::new();
    for u in units {
        d.decode(u, &mut out).unwrap();
    }
    let ch = d.channels();
    if ch == 6 {
        let names = d.layout();
        let order: Vec<usize> = ["L", "R", "C", "LFE", "Ls", "Rs"]
            .iter()
            .map(|n| names.iter().position(|m| m == n).unwrap())
            .collect();
        out = out
            .chunks_exact(6)
            .flat_map(|f| order.iter().map(move |&i| f[i]))
            .collect();
    }
    (out, ch)
}

fn compare(name: &str, rate: u32, channels: u32, opts: &[&str]) -> f32 {
    let (stream, reference) = encode(name, rate, channels, opts);
    let (asc, units) = adts(&stream);
    let (ours, ch) = decode(&asc, &units);
    assert_eq!(ch as u32, channels);
    assert_eq!(ours.len(), reference.len(), "{name}: sample count");
    let max = ours
        .iter()
        .zip(&reference)
        .map(|(a, b)| (a - b).abs())
        .fold(0f32, f32::max);
    eprintln!("{name}: max |diff| {max:e}");
    max
}

const PLAIN: [&str; 8] = [
    "-aac_pns", "0", "-aac_is", "0", "-aac_ms", "0", "-aac_tns", "0",
];

#[test]
fn matches_ffmpeg_sample_for_sample() {
    if !ffmpeg_ok() {
        return;
    }
    for (name, rate, ch, opts) in [
        ("plain48", 48000, 2, &PLAIN[..]),
        ("plain44mono", 44100, 1, &PLAIN[..]),
        (
            "ms",
            48000,
            2,
            &[
                "-aac_pns", "0", "-aac_is", "0", "-aac_tns", "0", "-aac_ms", "1",
            ][..],
        ),
        (
            "is",
            44100,
            2,
            &[
                "-aac_pns", "0", "-aac_ms", "0", "-aac_tns", "0", "-aac_is", "1", "-b:a", "64k",
            ][..],
        ),
        (
            "tns",
            48000,
            2,
            &[
                "-aac_pns", "0", "-aac_is", "0", "-aac_ms", "0", "-aac_tns", "1",
            ][..],
        ),
        (
            "high-rate",
            48000,
            2,
            &["-aac_pns", "0", "-b:a", "320k"][..],
        ),
        ("low-rate", 22050, 2, &["-aac_pns", "0", "-b:a", "48k"][..]),
        (
            "r22-plain",
            22050,
            2,
            &[&PLAIN[..], &["-b:a", "48k"]].concat()[..],
        ),
        (
            "r22-is",
            22050,
            2,
            &[
                "-aac_pns", "0", "-aac_ms", "0", "-aac_tns", "0", "-b:a", "48k",
            ][..],
        ),
        (
            "r22-ms",
            22050,
            2,
            &[
                "-aac_pns", "0", "-aac_is", "0", "-aac_tns", "0", "-b:a", "48k",
            ][..],
        ),
        (
            "r22-tns",
            22050,
            2,
            &[
                "-aac_pns", "0", "-aac_is", "0", "-aac_ms", "0", "-b:a", "48k",
            ][..],
        ),
        ("rate32", 32000, 1, &["-aac_pns", "0"][..]),
        ("surround", 48000, 6, &["-aac_pns", "0", "-b:a", "384k"][..]),
    ] {
        let max = compare(name, rate, ch, opts);
        if std::env::var_os("AAC_DEBUG").is_none() {
            assert!(max < 2e-4, "{name}: {max}");
        }
    }
}

#[test]
fn noise_substitution_has_the_same_energy() {
    if !ffmpeg_ok() {
        return;
    }
    // Low rate with PNS on: noise bands are random, so compare loudness of
    // each 1024-frame block (within 1 dB) rather than samples.
    let (stream, reference) = encode("pns", 44100, 2, &["-aac_pns", "1", "-b:a", "48k"]);
    let (asc, units) = adts(&stream);
    let (ours, _) = decode(&asc, &units);
    assert_eq!(ours.len(), reference.len());
    for (a, b) in ours.chunks(2048).zip(reference.chunks(2048)) {
        let e = |x: &[f32]| x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len() as f64;
        let (ea, eb) = (e(a), e(b));
        if eb > 1e-8 {
            let db = 10.0 * (ea / eb).log10();
            assert!(db.abs() < 1.0, "block energy differs by {db} dB");
        }
    }
}

#[test]
fn damaged_units_are_errors_not_panics() {
    if !ffmpeg_ok() {
        return;
    }
    let (stream, _) = encode("damage", 48000, 2, &[]);
    let (asc, units) = adts(&stream);
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut d = media_aac::Decoder::new(&asc).unwrap();
    let mut out = Vec::new();
    for _ in 0..3000 {
        let mut u = units[(next() % units.len() as u64) as usize].to_vec();
        for _ in 0..1 + next() % 3 {
            let i = (next() % u.len() as u64) as usize;
            u[i] ^= 1 << (next() % 8);
        }
        if next() % 5 == 0 {
            u.truncate((next() % u.len() as u64) as usize);
        }
        let _ = d.decode(&u, &mut out);
        out.clear();
    }
}

/// Our encoder's streams, decoded by FFmpeg, match our own decode: the
/// bitstream is read the same way by both.
#[test]
fn ffmpeg_reads_our_encoder() {
    if !ffmpeg_ok() {
        eprintln!("ffmpeg not installed; skipped");
        return;
    }
    let d = dir();
    let raw = d.join("enc-src.f32");
    let st = Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args(source(48000, 2, 3.0))
        .args(["-f", "f32le"])
        .arg(&raw)
        .status()
        .unwrap();
    assert!(st.success());
    let pcm: Vec<f32> = std::fs::read(&raw)
        .unwrap()
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    let mut e = media_aac::encoder::Encoder::new(48000, 2, 256_000).unwrap();
    let mut units = e.push(&pcm);
    units.extend(e.finish());
    let mut stream = Vec::new();
    for au in &units {
        let len = au.len() + 7;
        stream.extend([
            0xff,
            0xf1,
            (1 << 6) | (3 << 2),
            (2 << 6) | (len >> 11) as u8,
            (len >> 3) as u8,
            ((len & 7) << 5) as u8 | 0x1f,
            0xfc,
        ]);
        stream.extend(au);
    }
    let aac = d.join("ours.aac");
    std::fs::write(&aac, &stream).unwrap();
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&aac)
        .args(["-f", "f32le", "-c:a", "pcm_f32le", "-"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let theirs: Vec<f32> = out
        .stdout
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    let refs: Vec<&[u8]> = units.iter().map(Vec::as_slice).collect();
    let (ours, _) = decode(&e.asc(), &refs);
    assert_eq!(theirs.len(), ours.len());
    let worst = theirs
        .iter()
        .zip(&ours)
        .map(|(a, b)| (a - b).abs())
        .fold(0f32, f32::max);
    assert!(worst < 1e-4, "largest difference {worst}");
    // And against the source, after the encoder delay.
    let (mut s, mut n) = (0f64, 0f64);
    for (a, b) in pcm.iter().zip(&theirs[media_aac::encoder::DELAY * 2..]) {
        s += f64::from(*a).powi(2);
        n += f64::from(a - b).powi(2);
    }
    let snr = 10.0 * (s / n).log10();
    assert!(snr > 20.0, "SNR {snr:.1} dB");
}
