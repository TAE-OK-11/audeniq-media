//! One `trak`: header, media timing, sample description and sample tables.
use crate::audio::{self, AudioConfig};
use crate::boxes::{child, children, fourcc, path, u16_at, u32_at, u64_at};
use crate::codec::{self, Color, Config};

/// Sample entries beyond this many runs or table entries are a hostile index,
/// not a video (a 4-hour 60 fps video has under a million samples).
pub(crate) const MAX_SAMPLES: u64 = 50_000_000;

#[derive(Debug, Default, Clone)]
pub(crate) struct Visual {
    pub fourcc: [u8; 4],
    pub entry_size: (u32, u32),
    pub config: Config,
    /// `colr` box (container) colour, used where the bitstream says nothing.
    pub colr: Color,
    pub pasp: Option<(u32, u32)>,
    /// QuickTime `fiel`: 1 progressive, 2 interlaced.
    pub fields: Option<u8>,
    pub dolby_vision: bool,
}

#[derive(Debug, Clone)]
pub(crate) enum Kind {
    Video(Visual),
    Audio([u8; 4], AudioConfig),
    Other(Option<[u8; 4]>),
}

/// Durations as runs of (count, delta), merged.
#[derive(Debug, Default, Clone)]
pub(crate) struct Timing {
    pub runs: Vec<(u64, u32)>,
    pub samples: u64,
    pub duration: u64,
    pub bytes: u64,
    /// Sync sample numbers known; `None`: every sample is a sync sample.
    pub sync: Option<Vec<u64>>,
}

impl Timing {
    pub(crate) fn push(&mut self, count: u64, delta: u32) {
        if count == 0 {
            return;
        }
        self.samples = self.samples.saturating_add(count);
        self.duration = self
            .duration
            .saturating_add(count.saturating_mul(u64::from(delta)));
        if let Some((n, d)) = self.runs.last_mut()
            && *d == delta
        {
            *n = n.saturating_add(count);
        } else if self.runs.len() < MAX_SAMPLES as usize {
            self.runs.push((count, delta));
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Track {
    pub id: u32,
    pub enabled: bool,
    pub handler: [u8; 4],
    pub kind: Kind,
    /// Rotation of the track matrix, degrees counter-clockwise.
    pub rotation: i32,
    pub timescale: u32,
    pub media_duration: u64,
    pub language: Option<String>,
    /// Presentation start and presented length from the edit list, seconds.
    pub start: f64,
    pub edited: Option<f64>,
    pub timing: Timing,
    /// (offset, length) of every chunk.
    pub chunks: Vec<(u64, u64)>,
    /// The sample tables disagree with each other.
    pub inconsistent: bool,
    /// `dinf/dref` points at media in another file.
    pub external: bool,
}

fn fixed16(b: &[u8], i: usize) -> Option<i64> {
    Some(i64::from(u32_at(b, i)? as i32))
}

/// Rotation of a tkhd matrix rounded to a quarter turn, in degrees
/// counter-clockwise (FFprobe's display matrix `rotation`).
fn rotation(m: &[u8]) -> Option<i32> {
    let (a, b) = (fixed16(m, 0)?, fixed16(m, 4)?);
    let (c, d) = (fixed16(m, 12)?, fixed16(m, 16)?);
    Some(match (a.signum(), b.signum(), c.signum(), d.signum()) {
        (0, 1, -1, 0) => 270,
        (-1, 0, 0, -1) => 180,
        (0, -1, 1, 0) => 90,
        _ => 0,
    })
}

/// ISO-639-2/T packed into 15 bits; QuickTime Macintosh codes (< 0x400) and
/// `und` say nothing.
fn language(code: u16) -> Option<String> {
    if code < 0x400 || code == 0x55c4 {
        return None;
    }
    let s: String = [10, 5, 0]
        .iter()
        .map(|s| char::from(((code >> s) & 0x1f) as u8 + 0x60))
        .collect();
    s.chars().all(|c| c.is_ascii_lowercase()).then_some(s)
}

fn visual_entry(fourcc: [u8; 4], e: &[u8]) -> Option<Visual> {
    let mut v = Visual {
        fourcc,
        entry_size: (u32::from(u16_at(e, 24)?), u32::from(u16_at(e, 26)?)),
        ..Visual::default()
    };
    let kids = e.get(78..).unwrap_or_default();
    for (t, p) in children(kids) {
        let parsed = match &t {
            b"avcC" => codec::avcc(p),
            b"hvcC" => codec::hvcc(p),
            b"av1C" => codec::av1c(p),
            b"vpcC" => codec::vpcc(p),
            b"colr" => {
                v.colr = colr(p).unwrap_or_default();
                None
            }
            b"pasp" => {
                v.pasp = Some((u32_at(p, 0)?, u32_at(p, 4)?)).filter(|&(h, w)| h > 0 && w > 0);
                None
            }
            b"fiel" => {
                v.fields = p.first().copied();
                None
            }
            b"dvcC" | b"dvvC" | b"dvwC" => {
                v.dolby_vision = true;
                None
            }
            _ => None,
        };
        if let Some(cfg) = parsed {
            v.config = cfg;
        }
    }
    Some(v)
}

fn colr(p: &[u8]) -> Option<Color> {
    let typ = p.get(0..4)?;
    if typ != b"nclx" && typ != b"nclc" {
        return None;
    }
    let known = |v: u16| (v != 2).then_some(v);
    Some(Color {
        primaries: known(u16_at(p, 4)?),
        transfer: known(u16_at(p, 6)?),
        matrix: known(u16_at(p, 8)?),
        full_range: if typ == b"nclx" {
            Some(*p.get(10)? & 0x80 != 0)
        } else {
            None
        },
    })
}

/// Number of entries a full-box table may hold given its payload size.
pub(crate) fn entries(b: &[u8], at: usize, width: usize) -> Option<usize> {
    let n = u32_at(b, at)? as usize;
    let room = b.len().saturating_sub(at + 4) / width;
    (n <= room).then_some(n)
}

/// Sizes from `stsz` or `stz2`: (uniform size, per-sample sizes).
pub(crate) fn sample_sizes(stbl: &[u8]) -> Option<(u32, u64, Vec<u32>)> {
    if let Some(b) = child(stbl, b"stsz") {
        let uniform = u32_at(b, 4)?;
        let count = u64::from(u32_at(b, 8)?);
        if uniform != 0 {
            return Some((uniform, count, Vec::new()));
        }
        let n = entries(b, 8, 4)?;
        let sizes = (0..n)
            .map(|i| u32_at(b, 12 + 4 * i))
            .collect::<Option<_>>()?;
        return Some((0, count, sizes));
    }
    let b = child(stbl, b"stz2")?;
    let field = *b.get(7)?;
    let n = u32_at(b, 8)? as usize;
    let bits = match field {
        4 | 8 | 16 => usize::from(field),
        _ => return None,
    };
    if n > (b.len().saturating_sub(12) * 8) / bits {
        return None;
    }
    let sizes = (0..n)
        .map(|i| {
            let bit = i * bits;
            Some(match bits {
                4 => u32::from(*b.get(12 + bit / 8)? >> (4 - bit % 8) & 0xf),
                8 => u32::from(*b.get(12 + i)?),
                _ => u32::from(u16_at(b, 12 + 2 * i)?),
            })
        })
        .collect::<Option<_>>()?;
    Some((0, n as u64, sizes))
}

pub(crate) fn chunk_offsets(stbl: &[u8]) -> Option<Vec<u64>> {
    if let Some(b) = child(stbl, b"stco") {
        let n = entries(b, 4, 4)?;
        return (0..n)
            .map(|i| u32_at(b, 8 + 4 * i).map(u64::from))
            .collect();
    }
    let b = child(stbl, b"co64")?;
    let n = entries(b, 4, 8)?;
    (0..n).map(|i| u64_at(b, 8 + 8 * i)).collect()
}

/// Sample tables → timing and chunk extents. `None` when a required table is
/// missing or does not fit its box.
fn sample_tables(stbl: &[u8], track: &mut Track) -> Option<()> {
    let stts = child(stbl, b"stts")?;
    for i in 0..entries(stts, 4, 8)? {
        let at = 8 + 8 * i;
        track
            .timing
            .push(u64::from(u32_at(stts, at)?), u32_at(stts, at + 4)?);
    }
    if track.timing.samples > MAX_SAMPLES {
        return None;
    }
    if let Some(stss) = child(stbl, b"stss") {
        let n = entries(stss, 4, 4)?;
        let sync = (0..n)
            .map(|i| u32_at(stss, 8 + 4 * i).map(u64::from))
            .collect::<Option<Vec<_>>>()?;
        track.timing.sync = Some(sync);
    }
    let (uniform, count, sizes) = sample_sizes(stbl)?;
    if count != track.timing.samples || (uniform == 0 && sizes.len() as u64 != count) {
        track.inconsistent = true;
    }
    let size_of = |i: u64| -> u64 {
        if uniform != 0 {
            u64::from(uniform)
        } else {
            sizes.get(i as usize).map_or(0, |&s| u64::from(s))
        }
    };
    let offsets = chunk_offsets(stbl)?;
    let stsc = child(stbl, b"stsc")?;
    let runs = entries(stsc, 4, 12)?;
    let mut sample = 0u64;
    let mut bytes = 0u64;
    for r in 0..runs {
        let at = 8 + 12 * r;
        let first = u64::from(u32_at(stsc, at)?);
        let per = u64::from(u32_at(stsc, at + 4)?);
        let next = if r + 1 < runs {
            u64::from(u32_at(stsc, at + 12)?)
        } else {
            offsets.len() as u64 + 1
        };
        if first == 0 || next < first {
            track.inconsistent = true;
            break;
        }
        for c in first..next.min(offsets.len() as u64 + 1) {
            let mut len = 0u64;
            for _ in 0..per {
                if sample >= count {
                    track.inconsistent = true;
                    break;
                }
                len = len.saturating_add(size_of(sample));
                sample += 1;
            }
            bytes = bytes.saturating_add(len);
            track.chunks.push((offsets[c as usize - 1], len));
            if sample >= count {
                break;
            }
        }
    }
    if sample != count {
        track.inconsistent = true;
    }
    track.timing.bytes = bytes;
    Some(())
}

/// Presentation timing from `edts/elst`: leading empty edits delay the
/// track (start, seconds); the media edits give its presented length
/// (seconds). A first edit past media time 0 only skips encoder priming or
/// reordering delay; presentation still starts at the delay.
fn edits(trak: &[u8], movie_timescale: u32) -> Option<(f64, Option<f64>)> {
    let elst = path(trak, &[b"edts", b"elst"])?;
    let v1 = *elst.first()? == 1;
    let n = entries(elst, 4, if v1 { 20 } else { 12 })?;
    let (mut delay, mut shown) = (0u64, 0u64);
    for i in 0..n {
        let (dur, media) = if v1 {
            let at = 8 + 20 * i;
            (u64_at(elst, at)?, u64_at(elst, at + 8)? as i64)
        } else {
            let at = 8 + 12 * i;
            (
                u64::from(u32_at(elst, at)?),
                i64::from(u32_at(elst, at + 4)? as i32),
            )
        };
        if media == -1 {
            if shown == 0 {
                delay = delay.saturating_add(dur);
            }
        } else {
            shown = shown.saturating_add(dur);
        }
    }
    let ts = f64::from(movie_timescale.max(1));
    Some((delay as f64 / ts, (shown > 0).then(|| shown as f64 / ts)))
}

/// A `dref` entry without the self-contained flag (1) names another file.
fn external(dref: &[u8]) -> bool {
    children(dref.get(8..).unwrap_or_default()).any(|(_, e)| e.get(3).is_some_and(|f| f & 1 == 0))
}

pub(crate) fn parse(trak: &[u8], movie_timescale: u32) -> Option<Track> {
    let tkhd = child(trak, b"tkhd")?;
    let v1 = *tkhd.first()? == 1;
    let (id_at, matrix_at) = if v1 { (20, 52) } else { (12, 40) };
    let mdia = child(trak, b"mdia")?;
    let mdhd = child(mdia, b"mdhd")?;
    let (timescale, duration, lang_at) = if *mdhd.first()? == 1 {
        (u32_at(mdhd, 20)?, u64_at(mdhd, 24)?, 32)
    } else {
        (u32_at(mdhd, 12)?, u64::from(u32_at(mdhd, 16)?), 20)
    };
    let handler = fourcc(child(mdia, b"hdlr")?.get(8..12)?);
    let mut track = Track {
        id: u32_at(tkhd, id_at)?,
        enabled: tkhd.get(3).is_some_and(|f| f & 1 == 1),
        handler,
        kind: Kind::Other(None),
        rotation: tkhd.get(matrix_at..).and_then(rotation).unwrap_or(0),
        timescale,
        media_duration: if duration == u64::from(u32::MAX) || duration == u64::MAX {
            0
        } else {
            duration
        },
        language: u16_at(mdhd, lang_at).and_then(language),
        start: 0.0,
        edited: None,
        timing: Timing::default(),
        chunks: Vec::new(),
        inconsistent: false,
        external: path(mdia, &[b"minf", b"dinf", b"dref"]).is_some_and(external),
    };
    if let Some((start, edited)) = edits(trak, movie_timescale) {
        (track.start, track.edited) = (start, edited);
    }
    let stbl = path(mdia, &[b"minf", b"stbl"])?;
    let entry = child(stbl, b"stsd").and_then(|s| children(s.get(8..)?).next());
    track.kind = match (&handler, entry) {
        (b"vide", Some((t, e))) => Kind::Video(visual_entry(t, e)?),
        (b"soun", Some((t, e))) => Kind::Audio(t, audio::sample_entry(&t, e)?),
        (_, e) => Kind::Other(e.map(|(t, _)| t)),
    };
    if sample_tables(stbl, &mut track).is_none() {
        track.inconsistent = true;
    }
    Some(track)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages() {
        assert_eq!(language(0x15c7).as_deref(), Some("eng"));
        assert_eq!(language(0x55c4), None);
        assert_eq!(language(0), None);
    }

    #[test]
    fn quarter_turns() {
        let m = |a: i32, b: i32, c: i32, d: i32| {
            let mut v = vec![0u8; 36];
            for (i, x) in [(0, a), (4, b), (12, c), (16, d)] {
                v[i..i + 4].copy_from_slice(&(x << 16).to_be_bytes());
            }
            v
        };
        assert_eq!(rotation(&m(1, 0, 0, 1)), Some(0));
        assert_eq!(rotation(&m(0, 1, -1, 0)), Some(270));
        assert_eq!(rotation(&m(-1, 0, 0, -1)), Some(180));
        assert_eq!(rotation(&m(0, -1, 1, 0)), Some(90));
    }
}
