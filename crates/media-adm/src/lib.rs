//! AUDENIQ Dolby Atmos master check: an ADM BWF (ITU-R BS.2088 BW64 or
//! RF64/RIFF with `chna` + `axml`, ITU-R BS.2076 ADM) is read for its PCM
//! format, ADM programme, bed and object tracks, loudness metadata and the
//! consistency between `chna` and the ADM document, then checked against
//! what stores take for spatial audio (`atmos-delivery-v1`). With `peaks`
//! the PCM is scanned once for per-track peaks, silent tracks and clipping.
//!
//! The ADM metadata is read, not rendered: loudness is what the master
//! states (`loudnessMetadata` or `bext` v2), not a measurement.
mod adm;
pub mod cli;
mod wave;
mod xml;

use std::io::{Read, Seek, SeekFrom};

pub const ENGINE_VERSION: &str = "audeniq-atmos/0.1.0";
pub const RULE_VERSION: &str = "atmos-delivery-v1";

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Invalid(String),
    Limit(String),
}

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Error::Io(_) => "IO",
            Error::Invalid(_) => "INVALID",
            Error::Limit(_) => "LIMIT",
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Invalid(s) | Error::Limit(s) => f.write_str(s),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            Error::Invalid("unexpected end of file".into())
        } else {
            Error::Io(e)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
}
media_json::json_enum!(Severity {
    Error => "error",
    Warning => "warning",
    Info => "info",
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub code: &'static str,
    pub severity: Severity,
    pub detail: String,
}
media_json::json_struct!(Issue {
    code,
    severity,
    detail
});

#[derive(Debug, Clone, PartialEq)]
pub struct Container {
    /// `RIFF`, `RF64` or `BW64`.
    pub format: String,
    pub size: u64,
    pub chunks: Vec<String>,
    /// A Dolby `dbmd` metadata chunk is present.
    pub dolby_metadata: bool,
}
media_json::json_struct!(Container {
    format,
    size,
    chunks,
    dolby_metadata
});

#[derive(Debug, Clone, PartialEq)]
pub struct Audio {
    pub codec_name: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub bits_per_sample: u32,
    pub frames: u64,
    pub duration: f64,
}
media_json::json_struct!(Audio {
    codec_name,
    sample_rate,
    channels,
    bits_per_sample,
    frames,
    duration
});

#[derive(Debug, Clone, PartialEq)]
pub struct Bed {
    pub pack_id: String,
    pub name: String,
    /// e.g. `7.1.2`.
    pub layout: String,
    pub speakers: Vec<String>,
}
media_json::json_struct!(Bed {
    pack_id,
    name,
    layout,
    speakers
});

#[derive(Debug, Clone, PartialEq)]
pub struct Loudness {
    pub integrated: Option<f64>,
    pub true_peak: Option<f64>,
    pub dialogue: Option<f64>,
    /// `axml` (`loudnessMetadata`) or `bext`.
    pub source: &'static str,
}
media_json::json_struct!(Loudness {
    integrated,
    true_peak,
    dialogue,
    source
});

#[derive(Debug, Clone, PartialEq)]
pub struct Adm {
    pub programme: Option<String>,
    pub programmes: u32,
    pub contents: u32,
    pub objects: u32,
    pub beds: Vec<Bed>,
    /// Tracks of DirectSpeakers packs (beds), Objects packs, and others.
    pub bed_tracks: u32,
    pub object_tracks: u32,
    pub other_tracks: u32,
    pub chna_tracks: u32,
    pub programme_duration: Option<f64>,
    pub loudness: Option<Loudness>,
}
media_json::json_struct!(Adm {
    programme,
    programmes,
    contents,
    objects,
    beds,
    bed_tracks,
    object_tracks,
    other_tracks,
    chna_tracks,
    programme_duration,
    loudness
});

#[derive(Debug, Clone, PartialEq)]
pub struct Peaks {
    /// dBFS per track (1-based order), `null` for a silent track.
    pub tracks: Vec<Option<f64>>,
    pub silent_tracks: Vec<u32>,
    pub clipped_samples: u64,
}
media_json::json_struct!(Peaks {
    tracks,
    silent_tracks,
    clipped_samples
});

#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub engine: &'static str,
    pub rule: &'static str,
    pub container: Container,
    pub audio: Option<Audio>,
    pub adm: Option<Adm>,
    pub peaks: Option<Peaks>,
    pub issues: Vec<Issue>,
    /// No issue of severity `error`.
    pub deliverable: bool,
}
media_json::json_struct!(Report {
    engine,
    rule,
    container,
    audio,
    adm,
    peaks,
    issues,
    deliverable
});

/// Speaker labels → `main.lfe[.top]` (`7.1.2`).
fn layout(speakers: &[String]) -> String {
    let (mut main, mut lfe, mut top) = (0, 0, 0);
    for s in speakers {
        let l = s.trim_start_matches("RC_").to_ascii_lowercase();
        if l.contains("lfe") {
            lfe += 1;
        } else if l.starts_with('u')
            || l.starts_with("t+")
            || l.ends_with("ts")
            || l.contains("top")
            || matches!(l.as_str(), "ltf" | "rtf" | "ltr" | "rtr" | "ltm" | "rtm")
        {
            top += 1;
        } else {
            main += 1;
        }
    }
    if top > 0 {
        format!("{main}.{lfe}.{top}")
    } else {
        format!("{main}.{lfe}")
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// Scan the PCM for per-track peaks, silent tracks and full-scale samples.
fn scan<R: Read + Seek>(
    r: &mut R,
    data: (u64, u64),
    channels: usize,
    bytes: usize,
) -> Result<Peaks, Error> {
    let mut max = vec![0u32; channels];
    let mut clipped = 0u64;
    let frame = channels * bytes;
    let full = (1u32 << (bytes * 8 - 1)) - 1;
    r.seek(SeekFrom::Start(data.0))?;
    let mut left = data.1 - data.1 % frame as u64;
    let mut buf = vec![0u8; (4 << 20) / frame * frame];
    while left > 0 {
        let n = (left as usize).min(buf.len());
        r.read_exact(&mut buf[..n])?;
        for f in buf[..n].chunks_exact(frame) {
            for (c, s) in f.chunks_exact(bytes).enumerate() {
                let mut v = [0u8; 4];
                v[4 - bytes..].copy_from_slice(s);
                let x = (i32::from_le_bytes(v) >> ((4 - bytes) * 8)).unsigned_abs();
                if x > max[c] {
                    max[c] = x;
                }
                if x >= full {
                    clipped += 1;
                }
            }
        }
        left -= n as u64;
    }
    let tracks: Vec<Option<f64>> = max
        .iter()
        .map(|&m| (m > 0).then(|| round2(20.0 * (f64::from(m) / f64::from(full)).log10())))
        .collect();
    let silent_tracks = tracks
        .iter()
        .enumerate()
        .filter(|(_, p)| p.is_none_or(|db| db < -90.0))
        .map(|(i, _)| i as u32 + 1)
        .collect();
    Ok(Peaks {
        tracks,
        silent_tracks,
        clipped_samples: clipped,
    })
}

/// Read and check an ADM BWF; `peaks` also scans the PCM.
pub fn probe<R: Read + Seek>(r: &mut R, peaks: bool) -> Result<Report, Error> {
    let w = wave::read(r)?;
    let mut issues = Vec::new();
    let mut add = |code: &'static str, severity: Severity, detail: String| {
        issues.push(Issue {
            code,
            severity,
            detail,
        })
    };
    use Severity::{Error as E, Info as I, Warning as W};
    let fmt = w.format.clone();
    let audio = match (&fmt, w.data) {
        (Some(f), Some((_, len))) if f.block_align > 0 => {
            let frames = len / u64::from(f.block_align);
            let codec = match (f.code, f.bits) {
                (1, 16) => "pcm_s16le",
                (1, 24) => "pcm_s24le",
                (1, 32) => "pcm_s32le",
                (3, 32) => "pcm_f32le",
                (3, 64) => "pcm_f64le",
                _ => "unknown",
            };
            Some(Audio {
                codec_name: codec.into(),
                sample_rate: f.sample_rate,
                channels: u32::from(f.channels),
                bits_per_sample: u32::from(f.bits),
                frames,
                duration: if f.sample_rate > 0 {
                    (frames as f64 / f64::from(f.sample_rate) * 1000.0).round() / 1000.0
                } else {
                    0.0
                },
            })
        }
        _ => None,
    };
    match (&fmt, &audio) {
        (None, _) => add("NO_FORMAT", E, "fmt 청크가 없습니다.".into()),
        (_, None) => add("NO_AUDIO_DATA", E, "data 청크가 없습니다.".into()),
        (Some(f), Some(a)) => {
            if f.code != 1 {
                add(
                    "NOT_INTEGER_PCM",
                    E,
                    format!(
                        "정수 PCM이 아닙니다({}). Dolby Atmos 마스터는 24비트 정수 PCM입니다.",
                        a.codec_name
                    ),
                );
            } else if f.bits != 24 {
                add(
                    "BIT_DEPTH",
                    E,
                    format!("{}비트입니다. 24비트 PCM으로 내보내 주세요.", f.bits),
                );
            }
            match f.sample_rate {
                48000 => {}
                96000 => add(
                    "SAMPLE_RATE_96K",
                    W,
                    "96kHz입니다. 애플 뮤직 등은 48kHz Atmos 마스터를 받습니다.".into(),
                ),
                r => add(
                    "SAMPLE_RATE",
                    E,
                    format!("샘플레이트 {r}Hz: Dolby Atmos 마스터는 48kHz입니다."),
                ),
            }
            if f.channels > 128 {
                add(
                    "TOO_MANY_TRACKS",
                    E,
                    format!("트랙이 {}개입니다. ADM BWF는 128개까지입니다.", f.channels),
                );
            }
            if w.data.is_some_and(|d| d.1 % u64::from(f.block_align) != 0) {
                add(
                    "DATA_ALIGNMENT",
                    W,
                    "오디오 데이터 길이가 프레임 크기로 나누어떨어지지 않습니다.".into(),
                );
            }
        }
    }
    if w.truncated {
        add(
            "TRUNCATED",
            E,
            "파일이 중간에 끊겨 있습니다(data 청크가 파일 끝을 넘음). 다시 올려 주세요.".into(),
        );
    }
    // ADM.
    let mut adm_out = None;
    let channels = fmt.as_ref().map_or(0, |f| u32::from(f.channels));
    match (&w.axml, &w.chna) {
        (None, _) | (_, None) => add(
            "NO_ADM",
            E,
            format!(
                "ADM 메타데이터가 없습니다({} 없음). Dolby Atmos 렌더러에서 ADM BWF로 내보내 주세요.",
                [("axml", w.axml.is_none()), ("chna", w.chna.is_none())]
                    .iter()
                    .filter(|x| x.1)
                    .map(|x| x.0)
                    .collect::<Vec<_>>()
                    .join("·")
            ),
        ),
        (Some(axml), Some(chna)) => {
            let text = String::from_utf8_lossy(axml);
            let chna = wave::chna(chna);
            match (adm::parse(text.trim_end_matches('\0')), chna) {
                (Err(e), _) => add("ADM_INVALID", E, format!("ADM XML을 읽을 수 없습니다: {e}")),
                (_, Err(e)) => add(
                    "CHNA_INVALID",
                    E,
                    format!("chna 청크를 읽을 수 없습니다: {e}"),
                ),
                (Ok(d), Ok(chna)) => {
                    let (mut bed_t, mut obj_t, mut other_t) = (0u32, 0u32, 0u32);
                    let mut bad_index = 0;
                    let mut unknown_uid = Vec::new();
                    let mut used = std::collections::BTreeSet::new();
                    for (index, uid, _, pack) in &chna {
                        if *index == 0 || u32::from(*index) > channels {
                            bad_index += 1;
                        }
                        used.insert(*index);
                        match adm::id_type(pack) {
                            Some("0001") => bed_t += 1,
                            Some("0003") => obj_t += 1,
                            _ => other_t += 1,
                        }
                        if !d.track_uids.is_empty() && !d.track_uids.contains(uid) {
                            unknown_uid.push(uid.clone());
                        }
                    }
                    let chna_uids: std::collections::BTreeSet<&str> =
                        chna.iter().map(|c| c.1.as_str()).collect();
                    let dangling: Vec<&str> = d
                        .objects
                        .values()
                        .flat_map(|o| o.tracks.iter())
                        .map(String::as_str)
                        .filter(|u| !chna_uids.contains(u) && *u != "ATU_00000000")
                        .collect();
                    if bad_index > 0 {
                        add(
                            "CHNA_TRACK_RANGE",
                            E,
                            format!(
                                "chna의 트랙 번호 {bad_index}개가 오디오 채널 범위(1~{channels}) 밖입니다."
                            ),
                        );
                    }
                    if !unknown_uid.is_empty() {
                        add(
                            "CHNA_UID_UNKNOWN",
                            E,
                            format!(
                                "chna의 트랙 UID {}개가 ADM 문서에 없습니다(예: {}).",
                                unknown_uid.len(),
                                unknown_uid[0]
                            ),
                        );
                    }
                    if !dangling.is_empty() {
                        add(
                            "ADM_TRACK_MISSING",
                            E,
                            format!(
                                "ADM 오브젝트가 가리키는 트랙 {}개가 chna에 없습니다(예: {}).",
                                dangling.len(),
                                dangling[0]
                            ),
                        );
                    }
                    let unused = (1..=channels)
                        .filter(|i| !used.contains(&(*i as u16)))
                        .count();
                    if unused > 0 {
                        add(
                            "UNUSED_TRACKS",
                            W,
                            format!("ADM에 연결되지 않은 오디오 트랙이 {unused}개 있습니다."),
                        );
                    }
                    if d.programmes.is_empty() {
                        add("NO_PROGRAMME", E, "ADM audioProgramme이 없습니다.".into());
                    } else if d.programmes.len() > 1 {
                        add(
                            "MULTIPLE_PROGRAMMES",
                            W,
                            format!(
                                "audioProgramme이 {}개입니다. 배급할 하나만 남겨 주세요.",
                                d.programmes.len()
                            ),
                        );
                    }
                    if obj_t > 118 {
                        add(
                            "TOO_MANY_OBJECTS",
                            E,
                            format!(
                                "오브젝트 트랙이 {obj_t}개입니다. Dolby Atmos는 118개까지입니다."
                            ),
                        );
                    }
                    // Beds: DirectSpeakers packs the chna uses.
                    let mut beds = Vec::new();
                    let bed_packs: std::collections::BTreeSet<&str> = chna
                        .iter()
                        .filter(|c| adm::id_type(&c.3) == Some("0001"))
                        .map(|c| c.3.as_str())
                        .collect();
                    for id in bed_packs {
                        let (name, speakers) = match d.packs.get(id) {
                            Some(p) => (
                                p.name.clone(),
                                p.channels
                                    .iter()
                                    .map(|c| {
                                        d.channels
                                            .get(c)
                                            .and_then(|ch| {
                                                ch.speaker.clone().or_else(|| Some(ch.name.clone()))
                                            })
                                            .unwrap_or_else(|| c.clone())
                                    })
                                    .collect::<Vec<_>>(),
                            ),
                            None => (String::new(), Vec::new()),
                        };
                        beds.push(Bed {
                            pack_id: id.to_owned(),
                            name,
                            layout: layout(&speakers),
                            speakers,
                        });
                    }
                    if bed_t == 0 {
                        add("NO_BED", W, "베드(DirectSpeakers) 트랙이 없습니다. Dolby Atmos 음악 마스터는 보통 7.1.2 베드를 씁니다.".into());
                    } else if !beds.iter().any(|b| b.layout == "7.1.2") {
                        let l: Vec<&str> = beds.iter().map(|b| b.layout.as_str()).collect();
                        add(
                            "BED_LAYOUT",
                            W,
                            format!("베드 구성이 {}입니다(보통 7.1.2).", l.join(", ")),
                        );
                    }
                    let prog = d.programmes.first();
                    let programme_duration =
                        prog.and_then(|p| Some(p.end? - p.start.unwrap_or(0.0)));
                    if let (Some(pd), Some(a)) = (programme_duration, &audio)
                        && (pd - a.duration).abs() > 1.0
                    {
                        add(
                            "DURATION_MISMATCH",
                            W,
                            format!(
                                "ADM 프로그램 길이 {pd:.1}초와 오디오 길이 {:.1}초가 다릅니다.",
                                a.duration
                            ),
                        );
                    }
                    let loudness = prog
                        .and_then(|p| p.loudness.clone())
                        .map(|l| Loudness {
                            integrated: l.integrated,
                            true_peak: l.true_peak,
                            dialogue: l.dialogue,
                            source: "axml",
                        })
                        .or_else(|| {
                            let (i, t) = wave::bext_loudness(w.bext.as_deref()?);
                            (i.is_some() || t.is_some()).then_some(Loudness {
                                integrated: i,
                                true_peak: t,
                                dialogue: None,
                                source: "bext",
                            })
                        });
                    match &loudness {
                        None => add("LOUDNESS_MISSING", I, "마스터에 음량 정보(loudnessMetadata)가 없습니다. 렌더러의 음량 측정값을 함께 보내 주세요.".into()),
                        Some(l) => {
                            if l.integrated.is_some_and(|v| v > -18.0) {
                                add("LOUDNESS_HIGH", W, format!("통합 음량이 {:.1} LKFS입니다. 애플 뮤직 공간 음향은 −18 LKFS 이하를 요구합니다.", l.integrated.unwrap()));
                            }
                            if l.true_peak.is_some_and(|v| v > -1.0) {
                                add("TRUE_PEAK_HIGH", W, format!("True Peak가 {:+.1} dBTP입니다. −1 dBTP 이하로 맞춰 주세요.", l.true_peak.unwrap()));
                            }
                        }
                    }
                    adm_out = Some(Adm {
                        programme: prog.map(|p| p.name.clone()),
                        programmes: d.programmes.len() as u32,
                        contents: d.contents.len() as u32,
                        objects: d.objects.len() as u32,
                        beds,
                        bed_tracks: bed_t,
                        object_tracks: obj_t,
                        other_tracks: other_t,
                        chna_tracks: chna.len() as u32,
                        programme_duration: programme_duration
                            .map(|v| (v * 1000.0).round() / 1000.0),
                        loudness,
                    });
                }
            }
        }
    }
    if w.dbmd.is_none() && adm_out.is_some() {
        add("NO_DOLBY_METADATA", I, "Dolby 메타데이터(dbmd)가 없습니다. Dolby Atmos 렌더러가 만든 마스터인지 확인해 주세요.".into());
    }
    // PCM scan.
    let mut peaks_out = None;
    if peaks
        && let (Some(f), Some(data)) = (&fmt, w.data)
        && f.code == 1
        && matches!(f.bits, 16 | 24 | 32)
        && f.channels > 0
        && usize::from(f.block_align) == usize::from(f.channels) * usize::from(f.bits / 8)
    {
        let p = scan(r, data, usize::from(f.channels), usize::from(f.bits / 8))?;
        if p.silent_tracks.len() == p.tracks.len() {
            add("ALL_SILENT", E, "모든 트랙이 무음입니다.".into());
        } else if !p.silent_tracks.is_empty() {
            add(
                "SILENT_TRACKS",
                I,
                format!(
                    "무음 트랙 {}개: {:?}",
                    p.silent_tracks.len(),
                    p.silent_tracks
                ),
            );
        }
        if p.clipped_samples > 0 {
            add(
                "CLIPPING",
                W,
                format!(
                    "최대치(0 dBFS)에 닿은 표본이 {}개 있습니다.",
                    p.clipped_samples
                ),
            );
        }
        peaks_out = Some(p);
    }
    let deliverable = issues.iter().all(|i| i.severity != Severity::Error);
    Ok(Report {
        engine: ENGINE_VERSION,
        rule: RULE_VERSION,
        container: Container {
            format: w.kind.clone(),
            size: w.size,
            chunks: w.chunks.clone(),
            dolby_metadata: w.dbmd.is_some(),
        },
        audio,
        adm: adm_out,
        peaks: peaks_out,
        issues,
        deliverable,
    })
}

pub fn probe_path(path: &std::path::Path, peaks: bool) -> Result<Report, Error> {
    let mut f = std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(path)?);
    probe(&mut f, peaks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(
            layout(&s(&[
                "RC_L", "RC_R", "RC_C", "RC_LFE", "RC_Lss", "RC_Rss", "RC_Lrs", "RC_Rrs", "RC_Lts",
                "RC_Rts"
            ])),
            "7.1.2"
        );
        assert_eq!(
            layout(&s(&["M+030", "M-030", "M+000", "LFE1", "M+110", "M-110"])),
            "5.1"
        );
        assert_eq!(
            layout(&s(&[
                "M+030", "M-030", "M+000", "LFE", "M+090", "M-090", "M+135", "M-135", "U+045",
                "U-045", "U+135", "U-135"
            ])),
            "7.1.4"
        );
    }
}
