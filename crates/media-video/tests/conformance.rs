//! HE-AAC (SBR, PS) and AAC streams from the MPEG conformance suite, as
//! FFmpeg's FATE keeps them, decoded and compared with the reference
//! decoder's output. Needs the samples: `FATE_SAMPLES=/path/to/fate-suite`
//! (`rsync -vrltLW rsync://fate-suite.ffmpeg.org/fate-suite/aac/ …`);
//! skipped without them.
use std::path::{Path, PathBuf};

fn samples() -> Option<PathBuf> {
    let d = PathBuf::from(std::env::var_os("FATE_SAMPLES")?).join("aac");
    d.is_dir().then_some(d)
}

/// RMS difference from the 16-bit reference (raw little-endian samples)
/// in its LSBs, and frames.
fn compare(src: &Path, reference: &[u8], channels: usize) -> (f64, usize, usize) {
    let (_, names, pcm) = media_video::decode_sound(src).unwrap();
    assert_eq!(names.len(), channels, "{}", src.display());
    let bytes = reference;
    let reference: Vec<f64> = bytes
        .chunks_exact(2)
        .map(|b| f64::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0)
        .collect();
    let n = pcm.len().min(reference.len());
    let err: f64 = pcm[..n]
        .iter()
        .zip(&reference[..n])
        .map(|(&a, &b)| (f64::from(a) - b).powi(2))
        .sum::<f64>()
        / n as f64;
    (
        err.sqrt() * 32768.0,
        pcm.len() / channels,
        reference.len() / channels,
    )
}

fn check_pcm(stream: &str, reference: &[u8], channels: usize) {
    let Some(src) = samples().map(|d| d.join(stream)).filter(|p| p.exists()) else {
        eprintln!("skip {stream}: not in FATE_SAMPLES");
        return;
    };
    let (lsb, ours, theirs) = compare(&src, reference, channels);
    assert_eq!(ours, theirs, "{stream}: frames");
    // The reference is rounded to 16 bits: about half an LSB is exact.
    assert!(lsb < 1.0, "{stream}: {lsb} LSB from the reference");
}

fn check(stream: &str, reference: &str, channels: usize) {
    let Some(r) = samples().and_then(|d| std::fs::read(d.join(reference)).ok()) else {
        eprintln!("skip {stream}: not in FATE_SAMPLES");
        return;
    };
    check_pcm(stream, &r, channels);
}

#[test]
fn sbr_stereo() {
    check("al_sbr_cm_48_2.mp4", "al_sbr_hq_cm_48_2.s16", 2);
}

#[test]
fn sbr_downsampled() {
    check(
        "al_sbr_sr_48_2_fsaac48.mp4",
        "al_sbr_hq_sr_48_2_fsaac48.s16",
        2,
    );
}

#[test]
fn parametric_stereo() {
    check("al_sbr_ps_04_new.mp4", "al_sbr_ps_04_ur.s16", 2);
    check("al_sbr_ps_06_new.mp4", "al_sbr_ps_06_ur.s16", 2);
}

/// Implicit, backward-compatible and hierarchical SBR/PS signalling.
#[test]
fn signalling_variants() {
    let Some(bytes) =
        samples().and_then(|d| std::fs::read(d.join("CT_DecoderCheck/aacPlusv2.wav")).ok())
    else {
        return;
    };
    // 16-bit stereo WAV; the data chunk is the last one.
    let at = bytes.windows(4).rposition(|w| w == b"data").unwrap() + 8;
    for s in [
        "sbr_bc-ps_bc.mp4",
        "sbr_bc-ps_i.mp4",
        "sbr_i-ps_bic.mp4",
        "sbr_i-ps_i.mp4",
    ] {
        check_pcm(&format!("CT_DecoderCheck/{s}"), &bytes[at..], 2);
    }
}
