//! The encoder against our own decoder: quality, bit rate, timing and
//! transients. `tests/ffmpeg.rs` covers decoding FFmpeg's files; here every
//! stream we write must decode back to the source.
use media_aac::Decoder;
use media_aac::encoder::{DELAY, Encoder};

/// Deterministic noise in −1..1.
fn noise(seed: &mut u64) -> f32 {
    *seed = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    ((*seed >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
}

/// Music-like stereo: a chord, a bass line and a little noise.
fn music(rate: u32, secs: f32) -> Vec<f32> {
    let n = (rate as f32 * secs) as usize;
    let mut seed = 7;
    let mut out = Vec::with_capacity(n * 2);
    for i in 0..n {
        let t = i as f32 / rate as f32;
        let chord: f32 = [220.0, 277.2, 329.6, 440.0, 1760.0]
            .iter()
            .enumerate()
            .map(|(k, f)| (std::f32::consts::TAU * f * t).sin() * 0.08 / (k as f32 + 1.0).sqrt())
            .sum();
        let bass =
            (std::f32::consts::TAU * 55.0 * (1.0 + (t * 2.0).floor() % 3.0 / 4.0) * t).sin() * 0.2;
        let n1 = noise(&mut seed) * 0.01;
        let n2 = noise(&mut seed) * 0.01;
        out.push(chord + bass + n1);
        out.push(chord * 0.8 + bass + n2);
    }
    out
}

/// Encode then decode; returns (units, decoded with the delay removed).
fn roundtrip(pcm: &[f32], rate: u32, ch: usize, bit_rate: u32) -> (Vec<Vec<u8>>, Vec<f32>) {
    let mut e = Encoder::new(rate, ch, bit_rate).unwrap();
    let mut units = Vec::new();
    for block in pcm.chunks(1000 * ch) {
        units.extend(e.push(block));
    }
    units.extend(e.finish());
    assert_eq!(
        units.len() as u64,
        Encoder::units_for((pcm.len() / ch) as u64)
    );
    let mut d = Decoder::new(&e.asc()).unwrap();
    assert_eq!(d.channels(), ch);
    let mut out = Vec::new();
    for au in &units {
        assert_eq!(d.decode(au, &mut out).unwrap(), 1024);
    }
    (units, out.split_off(DELAY * ch))
}

fn snr(src: &[f32], dec: &[f32], ch: usize, c: usize) -> f64 {
    let (mut s, mut e) = (0f64, 0f64);
    for (a, b) in src.chunks(ch).zip(dec.chunks(ch)) {
        s += f64::from(a[c]).powi(2);
        e += f64::from(a[c] - b[c]).powi(2);
    }
    10.0 * (s / e).log10()
}

fn kbps(units: &[Vec<u8>], rate: u32) -> f64 {
    let bits: usize = units.iter().map(|u| u.len() * 8).sum();
    bits as f64 / (units.len() as f64 * 1024.0 / f64::from(rate)) / 1000.0
}

#[test]
fn stereo_music_decodes_back_at_the_asked_rate() {
    let pcm = music(48000, 4.0);
    for (rate, want) in [(256_000, 24.0), (320_000, 27.0)] {
        let (units, dec) = roundtrip(&pcm, 48000, 2, rate);
        let got = kbps(&units, 48000);
        assert!(
            (got - f64::from(rate) / 1000.0).abs() < f64::from(rate) / 1000.0 * 0.05,
            "{rate}: {got:.1} kbit/s"
        );
        for c in 0..2 {
            let s = snr(&pcm, &dec, 2, c);
            assert!(s > want, "{rate} ch{c}: SNR {s:.1} dB");
        }
        // No access unit beyond the 6144 bits per channel a decoder buffers.
        assert!(units.iter().all(|u| u.len() * 8 <= 6144 * 2));
    }
}

#[test]
fn mono_and_44100() {
    let pcm: Vec<f32> = music(44100, 2.0).chunks(2).map(|f| f[0]).collect();
    let (units, dec) = roundtrip(&pcm, 44100, 1, 128_000);
    assert!((kbps(&units, 44100) - 128.0).abs() < 7.0);
    let s = snr(&pcm, &dec, 1, 0);
    assert!(s > 20.0, "SNR {s:.1} dB");
}

#[test]
fn highs_are_kept() {
    // White noise to 24 kHz: a coder that drops the top band caps near 5 dB.
    let mut seed = 3;
    let pcm: Vec<f32> = (0..48000 * 2 * 2).map(|_| noise(&mut seed) * 0.2).collect();
    let (_, dec) = roundtrip(&pcm, 48000, 2, 512_000);
    let s = snr(&pcm, &dec, 2, 0);
    assert!(s > 20.0, "SNR {s:.1} dB");
}

#[test]
fn attacks_do_not_smear_backwards() {
    // A quiet tone and a loud noise burst every half second: the 20 ms
    // before each burst must stay clean (short windows, no pre-echo).
    let rate = 48000;
    let mut seed = 11;
    let pcm: Vec<f32> = (0..rate * 3)
        .flat_map(|i| {
            let tone = (std::f32::consts::TAU * 440.0 * i as f32 / rate as f32).sin() * 0.02;
            let k = i % 24000;
            let burst = if (i as usize) >= 24000 && k < 4800 {
                noise(&mut seed) * 0.5 * (-(k as f32) / 1200.0).exp()
            } else {
                0.0
            };
            [tone + burst, tone + burst]
        })
        .collect();
    let (_, dec) = roundtrip(&pcm, rate as u32, 2, 256_000);
    for onset in [24000usize, 48000, 72000, 96000, 120000] {
        let (mut s, mut e) = (0f64, 0f64);
        for i in onset - 960..onset {
            s += f64::from(pcm[i * 2]).powi(2);
            e += f64::from(pcm[i * 2] - dec[i * 2]).powi(2);
        }
        let pre = 10.0 * (e / s).log10();
        assert!(pre < -25.0, "onset {onset}: pre-echo {pre:.1} dB");
    }
}

#[test]
fn silence_and_short_input() {
    for frames in [0usize, 1, 1023, 1024, 1025, 5000] {
        let pcm = vec![0f32; frames * 2];
        let (units, dec) = roundtrip(&pcm, 48000, 2, 192_000);
        assert!(dec.len() >= frames * 2);
        assert!(dec.iter().all(|v| v.abs() < 1e-6));
        assert!(units.iter().all(|u| u.len() < 64));
    }
}

#[test]
fn rejects_what_it_cannot_code() {
    assert!(Encoder::new(47000, 2, 256_000).is_err());
    assert!(Encoder::new(48000, 6, 256_000).is_err());
    assert!(Encoder::new(48000, 2, 8_000).is_err());
    assert!(Encoder::new(48000, 2, 1_000_000).is_err());
}
