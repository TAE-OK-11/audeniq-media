//! A music video's sound, decoded and measured with the audio engine's
//! meters (EBU R128 loudness, true peak, clipping, silence), as the audio
//! QC does for masters. AAC-LC is decoded by `media-aac`; QuickTime/ISO
//! integer and float PCM directly. Only the sound track's chunks are read,
//! so a caller holding the file in object storage fetches just those
//! ([`ranges`]).
use crate::Error;
use crate::audio::AudioConfig;
use crate::boxes::{child, children, path, u32_at};
use crate::file::Layout;
use crate::track::{self, Kind};
use std::io::{Read, Seek, SeekFrom};

/// Bytes of sound data read at most (24-bit 48 kHz stereo PCM: 15 minutes).
pub const MAX_SOUND_BYTES: u64 = 256 << 20;

#[derive(Debug, Clone, PartialEq)]
pub struct Sound {
    pub track_id: u32,
    pub codec_name: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub duration: f64,
    /// Packets decoded and packets the decoder refused.
    pub packets: u64,
    pub decode_errors: u64,
    pub integrated_lufs: Option<f64>,
    pub true_peak_dbtp: Option<f64>,
    pub peak_dbfs: Option<f64>,
    pub channel_peaks_dbfs: Vec<Option<f64>>,
    /// Clip events: 3+ consecutive samples at ≥ 0.999 full scale.
    pub clip_events: u64,
    pub clipped_samples: u64,
    /// Share of 50 ms blocks below −60 dBFS and the longest such stretch.
    pub silent_ratio: f64,
    pub longest_silence: f64,
}
media_json::json_struct!(Sound {
    track_id,
    codec_name,
    sample_rate,
    channels,
    duration,
    packets,
    decode_errors,
    integrated_lufs,
    true_peak_dbtp,
    peak_dbfs,
    channel_peaks_dbfs,
    clip_events,
    clipped_samples,
    silent_ratio,
    longest_silence
});

/// The sound track the delivery uses: the first enabled one (or the only).
struct Chosen<'a> {
    id: u32,
    cfg: AudioConfig,
    stbl: &'a [u8],
}

fn choose(moov: &[u8]) -> Option<Chosen<'_>> {
    let ts = child(moov, b"mvhd")
        .and_then(|m| u32_at(m, if m.first() == Some(&1) { 20 } else { 12 }))?;
    let mut found = Vec::new();
    for (t, trak) in children(moov) {
        if &t != b"trak" {
            continue;
        }
        let Some(tr) = track::parse(trak, ts) else {
            continue;
        };
        if let Kind::Audio(_, cfg) = tr.kind {
            let stbl = path(trak, &[b"mdia", b"minf", b"stbl"])?;
            found.push((
                tr.enabled,
                Chosen {
                    id: tr.id,
                    cfg,
                    stbl,
                },
            ));
        }
    }
    let only = found.len() == 1;
    found.into_iter().find(|(e, _)| *e || only).map(|(_, c)| c)
}

/// Bytes of one PCM frame, `None` for compressed codecs.
fn pcm_frame(c: &AudioConfig) -> Option<usize> {
    let bits: usize = c.codec.strip_prefix("pcm_")?[1..]
        .trim_end_matches(['l', 'b', 'e'])
        .parse()
        .ok()?;
    Some(bits / 8 * c.channels as usize)
}

/// (chunk offset, first sample, samples) of every chunk.
fn chunks(stbl: &[u8]) -> Option<Vec<(u64, usize, usize)>> {
    let offsets = track::chunk_offsets(stbl)?;
    let stsc = child(stbl, b"stsc")?;
    let runs = track::entries(stsc, 4, 12)?;
    let mut out = Vec::with_capacity(offsets.len());
    let mut sample = 0usize;
    for r in 0..runs {
        let at = 8 + 12 * r;
        let first = u32_at(stsc, at)? as usize;
        let per = u32_at(stsc, at + 4)? as usize;
        let next = if r + 1 < runs {
            u32_at(stsc, at + 12)? as usize
        } else {
            offsets.len() + 1
        };
        if first == 0 || next < first {
            return None;
        }
        for c in first..next.min(offsets.len() + 1) {
            out.push((offsets[c - 1], sample, per));
            sample = sample.checked_add(per)?;
        }
    }
    Some(out)
}

/// Where the chosen track's data is: (offset, sizes of the packets stored
/// back to back there). PCM chunks are one "packet" of whole frames.
fn reads(layout: &Layout, c: &Chosen) -> Result<Vec<(u64, Vec<u32>)>, Error> {
    let bad = || Error::Invalid("sound track sample tables".into());
    if layout.fragments > 0 {
        return Ok(layout.fragment_runs.get(&c.id).cloned().unwrap_or_default());
    }
    let (uniform, count, sizes) = track::sample_sizes(c.stbl).ok_or_else(bad)?;
    let frame = pcm_frame(&c.cfg);
    let mut out = Vec::new();
    for (off, first, n) in chunks(c.stbl).ok_or_else(bad)? {
        let n = n.min((count as usize).saturating_sub(first));
        if n == 0 {
            continue;
        }
        let packets = match frame {
            Some(f) => vec![u32::try_from(n * f).map_err(|_| bad())?],
            None => (first..first + n)
                .map(|i| {
                    if uniform != 0 {
                        uniform
                    } else {
                        sizes.get(i).copied().unwrap_or(0)
                    }
                })
                .collect(),
        };
        out.push((off, packets));
    }
    let total: u64 = out
        .iter()
        .flat_map(|r| r.1.iter())
        .map(|&s| u64::from(s))
        .sum();
    if total > MAX_SOUND_BYTES {
        return Err(Error::Limit(format!(
            "sound data over {MAX_SOUND_BYTES} bytes"
        )));
    }
    Ok(out)
}

/// Byte ranges (offset, length) holding the sound track's data, adjacent
/// ones merged. Empty when there is no sound track.
pub(crate) fn ranges(layout: &Layout) -> Result<Vec<(u64, u64)>, Error> {
    let Some(c) = layout.moov.as_deref().and_then(choose) else {
        return Ok(Vec::new());
    };
    let mut out: Vec<(u64, u64)> = Vec::new();
    let mut total = 0u64;
    for (off, sizes) in reads(layout, &c)? {
        let len: u64 = sizes.iter().map(|&s| u64::from(s)).sum();
        total = total.saturating_add(len);
        if total > MAX_SOUND_BYTES {
            return Err(Error::Limit(format!(
                "sound data over {MAX_SOUND_BYTES} bytes"
            )));
        }
        match out.last_mut() {
            Some((o, l)) if *o + *l == off => *l += len,
            _ if len > 0 => out.push((off, len)),
            _ => {}
        }
    }
    Ok(out)
}

/// Interleaved PCM bytes → left-aligned i32.
fn pcm_to_i32(codec: &str, bytes: &[u8], out: &mut Vec<i32>) {
    let little = codec.ends_with("le");
    let rd = |b: &[u8]| -> [u8; 8] {
        let mut a = [0u8; 8];
        if little {
            a[..b.len()].copy_from_slice(b);
        } else {
            for (i, v) in b.iter().rev().enumerate() {
                a[i] = *v;
            }
        }
        a
    };
    let float = |x: f64| (x.clamp(-1.0, 1.0) * 2147483647.0).round() as i32;
    match codec {
        "pcm_u8" => out.extend(bytes.iter().map(|&v| (i32::from(v) - 128) << 24)),
        "pcm_s8" => out.extend(bytes.iter().map(|&v| i32::from(v as i8) << 24)),
        c if c.starts_with("pcm_s16") => out.extend(
            bytes
                .chunks_exact(2)
                .map(|b| i32::from(i16::from_le_bytes(rd(b)[..2].try_into().unwrap())) << 16),
        ),
        c if c.starts_with("pcm_s24") => out.extend(bytes.chunks_exact(3).map(|b| {
            let a = rd(b);
            i32::from_le_bytes([0, a[0], a[1], a[2]])
        })),
        c if c.starts_with("pcm_s32") => out.extend(
            bytes
                .chunks_exact(4)
                .map(|b| i32::from_le_bytes(rd(b)[..4].try_into().unwrap())),
        ),
        c if c.starts_with("pcm_f32") => out.extend(bytes.chunks_exact(4).map(|b| {
            float(f64::from(f32::from_le_bytes(
                rd(b)[..4].try_into().unwrap(),
            )))
        })),
        c if c.starts_with("pcm_f64") => out.extend(
            bytes
                .chunks_exact(8)
                .map(|b| float(f64::from_le_bytes(rd(b)))),
        ),
        _ => {}
    }
}

fn db(v: f64) -> Option<f64> {
    (v > 0.0).then(|| (20.0 * v.log10() * 100.0).round() / 100.0)
}

/// Decode and measure the sound track. The inner `Err` says why it cannot
/// be measured (no track, a codec or layout the meters do not take).
pub(crate) fn measure<R: Read + Seek>(
    r: &mut R,
    layout: &Layout,
) -> Result<Result<Sound, String>, Error> {
    let Some(c) = layout.moov.as_deref().and_then(choose) else {
        return Ok(Err("소리 트랙이 없습니다".into()));
    };
    if !(1..=2).contains(&c.cfg.channels) {
        return Ok(Err(format!(
            "{}채널 소리는 측정하지 않습니다(모노·스테레오만)",
            c.cfg.channels
        )));
    }
    let frame = pcm_frame(&c.cfg);
    let mut aac = match (c.cfg.codec.as_str(), frame) {
        ("aac", _) => match media_aac::Decoder::new(&c.cfg.asc) {
            Ok(d) => Some(d),
            Err(e) => return Ok(Err(format!("AAC를 디코딩할 수 없습니다: {e}"))),
        },
        (_, Some(_)) => None,
        (codec, None) => return Ok(Err(format!("{codec} 소리는 아직 측정하지 않습니다"))),
    };
    let rate = aac
        .as_ref()
        .map_or(c.cfg.sample_rate, |d| d.config.sample_rate);
    let channels = aac.as_ref().map_or(c.cfg.channels, |d| d.channels() as u32);
    let spec = audeniq_qc::AudioSpec {
        container: "mp4".into(),
        codec: c.cfg.codec.clone(),
        sample_rate: rate,
        channels: channels as u16,
        bits_per_sample: 24,
        frames: None,
    };
    let Ok(mut meter) = audeniq_qc::meter::PcmMeter::new(spec) else {
        return Ok(Err("측정기를 열 수 없습니다".into()));
    };
    let (mut packets, mut errors, mut frames) = (0u64, 0u64, 0u64);
    let (mut buf, mut pcm_f, mut pcm_i) = (Vec::new(), Vec::new(), Vec::new());
    for (off, sizes) in reads(layout, &c)? {
        let len: usize = sizes.iter().map(|&s| s as usize).sum();
        buf.resize(len, 0);
        r.seek(SeekFrom::Start(off))?;
        r.read_exact(&mut buf)?;
        pcm_i.clear();
        let mut at = 0;
        for &s in &sizes {
            let packet = &buf[at..at + s as usize];
            at += s as usize;
            packets += 1;
            match &mut aac {
                Some(d) => {
                    pcm_f.clear();
                    match d.decode(packet, &mut pcm_f) {
                        Ok(_) => pcm_i.extend(pcm_f.iter().map(|&x| {
                            (f64::from(x).clamp(-1.0, 1.0) * 2147483647.0).round() as i32
                        })),
                        Err(_) => errors += 1,
                    }
                }
                None => pcm_to_i32(&c.cfg.codec, packet, &mut pcm_i),
            }
        }
        frames += (pcm_i.len() / channels as usize) as u64;
        meter.push(&pcm_i);
    }
    let a = meter.finish(frames);
    let block = 0.05;
    Ok(Ok(Sound {
        track_id: c.id,
        codec_name: c.cfg.codec.clone(),
        sample_rate: rate,
        channels,
        duration: (a.duration_secs * 1000.0).round() / 1000.0,
        packets,
        decode_errors: errors,
        integrated_lufs: a.integrated_lufs.map(|v| (v * 100.0).round() / 100.0),
        true_peak_dbtp: a.true_peak_dbtp.map(|v| (v * 100.0).round() / 100.0),
        peak_dbfs: db(a.peak),
        channel_peaks_dbfs: a.channel_peaks.iter().map(|&p| db(p)).collect(),
        clip_events: a.clip_events,
        clipped_samples: a.clipped_samples,
        silent_ratio: if a.blocks > 0 {
            (a.silent_blocks as f64 / a.blocks as f64 * 1000.0).round() / 1000.0
        } else {
            0.0
        },
        longest_silence: (a.longest_silent_run as f64 * block * 100.0).round() / 100.0,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_conversion() {
        let mut v = Vec::new();
        pcm_to_i32("pcm_s16le", &[0x00, 0x80, 0xff, 0x7f], &mut v);
        assert_eq!(v, [i32::MIN, 0x7fff_0000]);
        v.clear();
        pcm_to_i32("pcm_s24be", &[0x80, 0, 0, 0x00, 0x00, 0x01], &mut v);
        assert_eq!(v, [i32::MIN, 0x100]);
        v.clear();
        pcm_to_i32("pcm_f32le", &2.0f32.to_le_bytes(), &mut v);
        assert_eq!(v, [i32::MAX]);
    }
}
