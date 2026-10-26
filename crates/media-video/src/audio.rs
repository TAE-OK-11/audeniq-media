//! Audio sample entries: QuickTime/ISO sound descriptions and the codec
//! boxes inside them (esds, dfLa, dOps, alac, enda).
use crate::boxes::{child, u16_at, u32_at};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AudioConfig {
    /// FFprobe codec name (`aac`, `pcm_s24le`, `alac`, …).
    pub codec: String,
    pub profile: Option<String>,
    pub sample_rate: u32,
    pub channels: u32,
    pub bits_per_sample: Option<u32>,
    /// From an `esds` decoder config, when the encoder filled it in.
    pub avg_bit_rate: Option<u64>,
}

/// MPEG-4 descriptor: tag, then a 1–4 byte length (7 bits per byte).
fn descriptor(b: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let tag = *b.first()?;
    let mut len = 0usize;
    let mut at = 1;
    loop {
        let byte = *b.get(at)?;
        at += 1;
        len = len << 7 | usize::from(byte & 0x7f);
        if byte & 0x80 == 0 {
            break;
        }
        if at > 4 {
            return None;
        }
    }
    let body = b.get(at..at.checked_add(len)?)?;
    Some((tag, body, &b[at + len..]))
}

/// ISO/IEC 14496-3 sampling frequency index.
const AAC_RATES: [u32; 13] = [
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
];

struct Asc {
    object: u32,
    rate: Option<u32>,
    channels: Option<u32>,
}

fn audio_specific_config(b: &[u8]) -> Option<Asc> {
    let mut r = crate::bits::Bits::new(b);
    let object_type = |r: &mut crate::bits::Bits| -> Option<u32> {
        let o = r.u(5)?;
        Some(if o == 31 { 32 + r.u(6)? } else { o })
    };
    let rate = |r: &mut crate::bits::Bits| -> Option<Option<u32>> {
        let i = r.u(4)?;
        Some(if i == 15 {
            Some(r.u(24)?)
        } else {
            AAC_RATES.get(i as usize).copied()
        })
    };
    let mut object = object_type(&mut r)?;
    let mut sample_rate = rate(&mut r)?;
    let channels = r.u(4)?;
    // Explicit SBR (HE-AAC) and PS (HE-AACv2) signal the extension rate.
    if object == 5 || object == 29 {
        sample_rate = rate(&mut r)?;
        let core = object_type(&mut r)?;
        if core == 2 {
            object = if object == 29 { 29 } else { 5 };
        }
    }
    let channels = match channels {
        1..=6 => Some(channels),
        7 => Some(8),
        _ => None,
    };
    Some(Asc {
        object,
        rate: sample_rate,
        channels,
    })
}

fn esds(entry: &mut AudioConfig, b: &[u8]) -> Option<()> {
    let (tag, es, _) = descriptor(b.get(4..)?)?;
    if tag != 3 {
        return None;
    }
    let flags = *es.get(2)?;
    let mut at = 3;
    if flags & 0x80 != 0 {
        at += 2;
    }
    if flags & 0x40 != 0 {
        at += 1 + usize::from(*es.get(at)?);
    }
    if flags & 0x20 != 0 {
        at += 2;
    }
    let mut rest = es.get(at..)?;
    while let Some((tag, body, next)) = descriptor(rest) {
        rest = next;
        if tag != 4 {
            continue;
        }
        let oti = *body.first()?;
        let avg = u64::from(u32::from_be_bytes(body.get(9..13)?.try_into().ok()?));
        entry.avg_bit_rate = (avg > 0).then_some(avg);
        entry.codec = match oti {
            0x40 | 0x66..=0x68 => "aac",
            0x69 | 0x6b => "mp3",
            0xa5 => "ac3",
            0xa6 => "eac3",
            0xa9 | 0xac => "dts",
            0xad => "opus",
            _ => "unknown",
        }
        .to_owned();
        if entry.codec != "aac" {
            return Some(());
        }
        let Some((5, asc, _)) = descriptor(body.get(13..)?) else {
            return Some(());
        };
        let asc = audio_specific_config(asc)?;
        entry.profile = match asc.object {
            1 => Some("Main"),
            2 => Some("LC"),
            3 => Some("SSR"),
            4 => Some("LTP"),
            5 => Some("HE-AAC"),
            29 => Some("HE-AACv2"),
            23 => Some("LD"),
            39 => Some("ELD"),
            _ => None,
        }
        .map(str::to_owned);
        if let Some(rate) = asc.rate {
            entry.sample_rate = rate;
        }
        // HE-AACv2 is coded mono; the decoder outputs stereo.
        if asc.object == 29 {
            entry.channels = 2;
        } else if let Some(ch) = asc.channels {
            entry.channels = ch;
        }
        return Some(());
    }
    None
}

/// `pcm_<s|u|f><bits><le|be>` as FFprobe names it.
fn pcm(float: bool, signed: bool, bits: u32, little: bool) -> String {
    let kind = if float {
        'f'
    } else if signed || bits > 8 {
        's'
    } else {
        'u'
    };
    if bits == 8 {
        return format!("pcm_{kind}8");
    }
    format!("pcm_{kind}{bits}{}", if little { "le" } else { "be" })
}

/// An audio sample entry (`stsd` child) of type `fourcc`, payload after the
/// box header.
pub(crate) fn sample_entry(fourcc: &[u8; 4], e: &[u8]) -> Option<AudioConfig> {
    let version = u16_at(e, 8)?;
    let mut cfg = AudioConfig {
        channels: u32::from(u16_at(e, 16)?),
        bits_per_sample: Some(u32::from(u16_at(e, 18)?)),
        sample_rate: u32_at(e, 24)? >> 16,
        ..AudioConfig::default()
    };
    let mut lpcm_flags = None;
    let children_at = match version {
        0 => 28,
        1 => 44,
        2 => {
            let rate = f64::from_bits(u64::from_be_bytes(e.get(32..40)?.try_into().ok()?));
            cfg.sample_rate = if rate.is_finite() && rate > 0.0 && rate < 1e7 {
                rate.round() as u32
            } else {
                0
            };
            cfg.channels = u32_at(e, 40)?;
            cfg.bits_per_sample = Some(u32_at(e, 48)?);
            lpcm_flags = Some(u32_at(e, 52)?);
            64
        }
        _ => return None,
    };
    let kids = e.get(children_at..).unwrap_or_default();
    // QuickTime nests codec boxes in `wave`.
    let wave = child(kids, b"wave");
    let find = |name: &[u8; 4]| child(kids, name).or_else(|| wave.and_then(|w| child(w, name)));
    let little = find(b"enda")
        .and_then(|b| b.get(1).copied())
        .map(|v| v & 1 == 1);
    let bits = cfg.bits_per_sample.unwrap_or(16);
    cfg.codec = match fourcc {
        b"mp4a" => {
            cfg.codec = "aac".into();
            if let Some(b) = find(b"esds") {
                esds(&mut cfg, b);
            }
            cfg.bits_per_sample = None;
            return Some(cfg);
        }
        b"sowt" => pcm(false, true, 16, true),
        b"twos" => pcm(false, true, if bits == 8 { 8 } else { 16 }, false),
        b"raw " | b"NONE" => pcm(false, false, 8, false),
        b"in24" => pcm(false, true, 24, little.unwrap_or(false)),
        b"in32" => pcm(false, true, 32, little.unwrap_or(false)),
        b"fl32" => pcm(true, true, 32, little.unwrap_or(false)),
        b"fl64" => pcm(true, true, 64, little.unwrap_or(false)),
        b"lpcm" => {
            let f = lpcm_flags.unwrap_or(0);
            pcm(f & 1 != 0, f & 4 != 0, bits, f & 2 == 0)
        }
        b"ipcm" | b"fpcm" => {
            // ISO/IEC 23003-5: pcmC carries endianness and sample size.
            let c = find(b"pcmC");
            let little = c.and_then(|c| c.get(4)).is_some_and(|f| f & 1 == 1);
            let size = c.and_then(|c| c.get(5)).map_or(bits, |&s| u32::from(s));
            cfg.bits_per_sample = Some(size);
            pcm(fourcc == b"fpcm", true, size, little)
        }
        b"alac" => {
            // Magic cookie: a full box whose config holds depth, channels, rate.
            if let Some(c) = find(b"alac").and_then(|c| c.get(4..28)) {
                cfg.bits_per_sample = Some(u32::from(c[5]));
                cfg.channels = u32::from(c[9]);
                cfg.sample_rate = u32_at(c, 20)?;
            }
            "alac".into()
        }
        b"fLaC" => {
            // dfLa: full box, then metadata blocks; STREAMINFO first.
            if let Some(si) = find(b"dfLa").and_then(|d| d.get(8..26)) {
                let packed = u64::from_be_bytes(si[10..18].try_into().ok()?);
                cfg.sample_rate = (packed >> 44) as u32;
                cfg.channels = (packed >> 41 & 7) as u32 + 1;
                cfg.bits_per_sample = Some((packed >> 36 & 31) as u32 + 1);
            }
            "flac".into()
        }
        b"Opus" => {
            if let Some(d) = find(b"dOps") {
                cfg.channels = u32::from(*d.get(1)?);
            }
            cfg.sample_rate = 48000;
            cfg.bits_per_sample = None;
            "opus".into()
        }
        b"ac-3" => "ac3".into(),
        b"ec-3" => "eac3".into(),
        b".mp3" => "mp3".into(),
        _ => "unknown".into(),
    };
    if matches!(cfg.codec.as_str(), "ac3" | "eac3" | "mp3") {
        cfg.bits_per_sample = None;
    }
    if cfg.codec.starts_with("pcm_") && fourcc != b"lpcm" {
        cfg.bits_per_sample = cfg.codec[5..]
            .trim_end_matches(['l', 'b', 'e'])
            .parse()
            .ok();
    }
    Some(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asc_lc_and_he_aac() {
        // AAC LC, 48 kHz, stereo.
        let lc = audio_specific_config(&[0x11, 0x90]).unwrap();
        assert_eq!((lc.object, lc.rate, lc.channels), (2, Some(48000), Some(2)));
        // Explicit HE-AAC: SBR at 24 kHz core → 48 kHz output, stereo.
        let he = audio_specific_config(&[0x2b, 0x11, 0x88, 0x00]).unwrap();
        assert_eq!((he.object, he.rate, he.channels), (5, Some(48000), Some(2)));
    }

    #[test]
    fn pcm_names() {
        assert_eq!(pcm(false, true, 24, true), "pcm_s24le");
        assert_eq!(pcm(true, true, 32, false), "pcm_f32be");
        assert_eq!(pcm(false, false, 8, false), "pcm_u8");
    }

    #[test]
    fn descriptor_lengths() {
        assert_eq!(
            descriptor(&[3, 0x80, 0x80, 0x01, 9]),
            Some((3, &[9][..], &[][..]))
        );
        assert!(descriptor(&[3, 0x80, 0x80, 0x80, 0x80, 1]).is_none());
        assert!(descriptor(&[3, 5, 1]).is_none());
    }
}
