//! Top-level walk of an MP4/MOV file: only box headers, `ftyp`, `moov` and
//! `moof` are read; media data (`mdat`) is skipped with a seek.
use crate::Error;
use crate::boxes::{child, children, fourcc, u32_at, u64_at};
use crate::track::{MAX_SAMPLES, Timing};
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};

/// Largest `moov` read into memory; hours of video index in a few MB.
pub const MAX_MOOV: u64 = 64 << 20;
/// Largest single `moof`.
const MAX_MOOF: u64 = 16 << 20;
/// Top-level boxes walked before the rest is treated as unreadable.
const MAX_BOXES: usize = 200_000;
/// Fragment sample positions kept per track.
pub const MAX_FRAGMENT_SAMPLES: usize = 2_000_000;

#[derive(Debug, Default)]
pub(crate) struct Layout {
    pub size: u64,
    pub ftyp: Option<Vec<u8>>,
    pub moov: Option<Vec<u8>>,
    pub moov_offset: u64,
    /// Payload ranges of the `mdat` boxes.
    pub mdat: Vec<(u64, u64)>,
    pub fragments: u64,
    /// Sample timing per track id gathered from `moof/traf/trun`.
    pub fragment_timing: BTreeMap<u32, Timing>,
    /// Per track id: each `trun`'s data offset and sample sizes (up to
    /// [`MAX_FRAGMENT_SAMPLES`] samples a track), for reading the sound.
    pub fragment_runs: BTreeMap<u32, Vec<(u64, Vec<u32>)>>,
    /// Data of a fragment points outside the file.
    pub fragment_outside: bool,
    /// The last top-level box runs past the end of the file.
    pub truncated: bool,
}

fn read_exact_at<R: Read + Seek>(r: &mut R, at: u64, len: usize) -> Result<Vec<u8>, Error> {
    r.seek(SeekFrom::Start(at))?;
    let mut buf = vec![0; len];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

pub(crate) fn walk<R: Read + Seek>(r: &mut R, size: u64) -> Result<Layout, Error> {
    let mut l = Layout {
        size,
        ..Layout::default()
    };
    let mut at = 0u64;
    let mut trex: BTreeMap<u32, (u32, u32, u32)> = BTreeMap::new();
    for n in 0..MAX_BOXES {
        if at + 8 > size {
            break;
        }
        let head = read_exact_at(r, at, (size - at).min(16) as usize)?;
        let typ = fourcc(&head[4..8]);
        if n == 0
            && typ != *b"ftyp"
            && !matches!(&typ, b"moov" | b"wide" | b"free" | b"mdat" | b"skip")
        {
            return Err(Error::Invalid("not an MP4/QuickTime file".into()));
        }
        let (hdr, len) = match u32_at(&head, 0).unwrap_or(0) {
            0 => (8, size - at),
            1 => match u64_at(&head, 8) {
                Some(len) => (16, len),
                None => {
                    l.truncated = true;
                    break;
                }
            },
            len => (8, u64::from(len)),
        };
        if len < hdr {
            return Err(Error::Invalid(format!("box at {at} has size {len}")));
        }
        let end = at.saturating_add(len);
        if end > size {
            l.truncated = true;
        }
        let body = at + hdr;
        let body_len = end.min(size) - body;
        match &typ {
            b"ftyp" if l.ftyp.is_none() => {
                l.ftyp = Some(read_exact_at(r, body, body_len.min(4096) as usize)?);
            }
            b"moov" => {
                if l.moov.is_some() {
                    return Err(Error::Invalid("more than one moov".into()));
                }
                if body_len > MAX_MOOV {
                    return Err(Error::Limit(format!("moov of {body_len} bytes")));
                }
                l.moov = Some(read_exact_at(r, body, body_len as usize)?);
                l.moov_offset = at;
                if let Some(mvex) = l.moov.as_deref().and_then(|m| child(m, b"mvex")) {
                    for (t, p) in children(mvex) {
                        if &t == b"trex"
                            && let (Some(id), Some(d), Some(s), Some(f)) =
                                (u32_at(p, 4), u32_at(p, 12), u32_at(p, 16), u32_at(p, 20))
                        {
                            trex.insert(id, (d, s, f));
                        }
                    }
                }
            }
            b"mdat" => l.mdat.push((body, body_len)),
            b"moof" => {
                l.fragments += 1;
                if body_len > MAX_MOOF {
                    return Err(Error::Limit(format!("moof of {body_len} bytes")));
                }
                let moof = read_exact_at(r, body, body_len as usize)?;
                fragment(&mut l, &moof, at, &trex);
            }
            _ => {}
        }
        if end >= size {
            break;
        }
        at = end;
    }
    Ok(l)
}

/// `moof/traf` (ISO/IEC 14496-12 8.8): sample counts, durations, sizes and
/// sync flags from `tfhd`/`trun`, with `trex` defaults.
fn fragment(l: &mut Layout, moof: &[u8], moof_at: u64, trex: &BTreeMap<u32, (u32, u32, u32)>) {
    for (t, traf) in children(moof) {
        if &t != b"traf" {
            continue;
        }
        let Some(tfhd) = child(traf, b"tfhd") else {
            continue;
        };
        let (Some(flags), Some(id)) = (u32_at(tfhd, 0), u32_at(tfhd, 4)) else {
            continue;
        };
        let flags = flags & 0xff_ffff;
        let (mut dur, mut size, mut sflags) = trex.get(&id).copied().unwrap_or_default();
        let mut base = moof_at;
        let mut at = 8;
        let mut take = |bit: u32, width: usize| -> Option<u64> {
            if flags & bit == 0 {
                return None;
            }
            let v = if width == 8 {
                u64_at(tfhd, at)
            } else {
                u32_at(tfhd, at).map(u64::from)
            };
            at += width;
            v
        };
        if let Some(b) = take(0x1, 8) {
            base = b;
        }
        take(0x2, 4);
        if let Some(v) = take(0x8, 4) {
            dur = v as u32;
        }
        if let Some(v) = take(0x10, 4) {
            size = v as u32;
        }
        if let Some(v) = take(0x20, 4) {
            sflags = v as u32;
        }
        let timing = l.fragment_timing.entry(id).or_default();
        for (t, trun) in children(traf) {
            if &t != b"trun" {
                continue;
            }
            let (Some(tf), Some(count)) = (u32_at(trun, 0), u32_at(trun, 4)) else {
                continue;
            };
            let mut at = 8;
            let mut data = base;
            if tf & 0x1 != 0 {
                let Some(off) = u32_at(trun, at) else {
                    continue;
                };
                data = data.wrapping_add(i64::from(off as i32) as u64);
                at += 4;
            }
            let mut first_flags = None;
            if tf & 0x4 != 0 {
                first_flags = u32_at(trun, at);
                at += 4;
            }
            let per = [0x100, 0x200, 0x400, 0x800]
                .iter()
                .filter(|&&b| tf & b != 0)
                .count()
                * 4;
            if per > 0 && (count as usize) > trun.len().saturating_sub(at) / per {
                l.fragment_outside = true;
                continue;
            }
            if timing.samples.saturating_add(u64::from(count)) > MAX_SAMPLES {
                return;
            }
            let mut bytes = 0u64;
            let mut sizes = Vec::new();
            for i in 0..count {
                let mut field = |bit: u32, default: u32| -> u32 {
                    if tf & bit == 0 {
                        return default;
                    }
                    let v = u32_at(trun, at).unwrap_or(default);
                    at += 4;
                    v
                };
                let d = field(0x100, dur);
                let s = field(0x200, size);
                let f = field(
                    0x400,
                    if i == 0 {
                        first_flags.unwrap_or(sflags)
                    } else {
                        sflags
                    },
                );
                field(0x800, 0);
                let number = timing.samples + 1;
                timing.push(1, d);
                if f & 0x1_0000 == 0 {
                    timing.sync.get_or_insert_with(Vec::new).push(number);
                } else {
                    timing.sync.get_or_insert_with(Vec::new);
                }
                bytes = bytes.saturating_add(u64::from(s));
                sizes.push(s);
            }
            timing.bytes = timing.bytes.saturating_add(bytes);
            let runs = l.fragment_runs.entry(id).or_default();
            let kept: usize = runs.iter().map(|r| r.1.len()).sum();
            if kept + sizes.len() <= MAX_FRAGMENT_SAMPLES {
                runs.push((data, sizes));
            }
            if data.saturating_add(bytes) > l.size {
                l.fragment_outside = true;
            }
        }
    }
}
