//! RIFF / RF64 / BW64 WAVE chunks: `ds64` 64-bit sizes, `fmt `, `bext`,
//! `chna`, `axml`, `dbmd` and where `data` is. Sample data is not read here.
use crate::Error;
use std::io::{Read, Seek, SeekFrom};

/// Largest metadata chunk read into memory (`axml` of a long Atmos master
/// with every object's automation runs to tens of MB).
pub const MAX_META: u64 = 256 << 20;
const MAX_CHUNKS: usize = 10_000;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Format {
    /// 1 PCM, 3 IEEE float (from the extensible sub-format when present).
    pub code: u16,
    pub channels: u16,
    pub sample_rate: u32,
    pub block_align: u16,
    pub bits: u16,
    pub valid_bits: u16,
    pub channel_mask: u32,
}

#[derive(Debug, Default)]
pub struct Wave {
    /// `RIFF`, `RF64` or `BW64`.
    pub kind: String,
    pub size: u64,
    pub format: Option<Format>,
    pub data: Option<(u64, u64)>,
    pub chna: Option<Vec<u8>>,
    pub axml: Option<Vec<u8>>,
    pub bext: Option<Vec<u8>>,
    pub dbmd: Option<Vec<u8>>,
    /// The `data` chunk claims more bytes than the file holds.
    pub truncated: bool,
    pub chunks: Vec<String>,
}

fn u16le(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?))
}
fn u32le(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?))
}
fn u64le(b: &[u8], i: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(i..i + 8)?.try_into().ok()?))
}

fn read_at<R: Read + Seek>(r: &mut R, at: u64, len: usize) -> Result<Vec<u8>, Error> {
    r.seek(SeekFrom::Start(at))?;
    let mut b = vec![0; len];
    r.read_exact(&mut b)?;
    Ok(b)
}

fn format(b: &[u8]) -> Option<Format> {
    let mut f = Format {
        code: u16le(b, 0)?,
        channels: u16le(b, 2)?,
        sample_rate: u32le(b, 4)?,
        block_align: u16le(b, 12)?,
        bits: u16le(b, 14)?,
        ..Format::default()
    };
    f.valid_bits = f.bits;
    if f.code == 0xfffe && b.len() >= 40 {
        f.valid_bits = u16le(b, 18)?;
        f.channel_mask = u32le(b, 20)?;
        f.code = u16le(b, 24)?;
    }
    Some(f)
}

pub fn read<R: Read + Seek>(r: &mut R) -> Result<Wave, Error> {
    let size = r.seek(SeekFrom::End(0))?;
    if size < 12 {
        return Err(Error::Invalid("not a WAVE file".into()));
    }
    let head = read_at(r, 0, 12)?;
    let kind = &head[0..4];
    if !matches!(kind, b"RIFF" | b"RF64" | b"BW64") || &head[8..12] != b"WAVE" {
        return Err(Error::Invalid("not a RIFF/RF64/BW64 WAVE file".into()));
    }
    let mut w = Wave {
        kind: String::from_utf8_lossy(kind).into_owned(),
        size,
        ..Wave::default()
    };
    let wide = kind != b"RIFF";
    let mut ds64_data = None;
    let mut table: Vec<([u8; 4], u64)> = Vec::new();
    let mut at = 12u64;
    for n in 0..MAX_CHUNKS {
        if at + 8 > size {
            break;
        }
        let h = read_at(r, at, 8)?;
        let id: [u8; 4] = h[0..4].try_into().unwrap();
        let mut len = u64::from(u32le(&h, 4).unwrap());
        if n == 0 && wide {
            if &id != b"ds64" {
                return Err(Error::Invalid("RF64/BW64 without ds64".into()));
            }
            let body = read_at(r, at + 8, len.min(4096) as usize)?;
            ds64_data = u64le(&body, 8);
            let entries = u32le(&body, 24).unwrap_or(0) as usize;
            for i in 0..entries.min(256) {
                let e = 28 + 12 * i;
                if let (Some(cid), Some(sz)) = (body.get(e..e + 4), u64le(&body, e + 4)) {
                    table.push((cid.try_into().unwrap(), sz));
                }
            }
        }
        if wide && len == 0xffff_ffff {
            len = if &id == b"data" {
                ds64_data.ok_or_else(|| Error::Invalid("data size missing from ds64".into()))?
            } else {
                table
                    .iter()
                    .find(|(c, _)| *c == id)
                    .map(|t| t.1)
                    .ok_or_else(|| Error::Invalid("chunk size missing from ds64".into()))?
            };
        }
        w.chunks
            .push(String::from_utf8_lossy(&id).trim_end().to_owned());
        let body = at + 8;
        let avail = size.saturating_sub(body);
        let meta = |len: u64| -> Result<usize, Error> {
            if len > MAX_META {
                return Err(Error::Limit(format!("metadata chunk of {len} bytes")));
            }
            if len > avail {
                return Err(Error::Invalid("metadata chunk runs past the end".into()));
            }
            Ok(len as usize)
        };
        match &id {
            b"fmt " => {
                let b = read_at(r, body, meta(len)?)?;
                w.format =
                    Some(format(&b).ok_or_else(|| Error::Invalid("short fmt chunk".into()))?);
            }
            b"chna" => w.chna = Some(read_at(r, body, meta(len)?)?),
            b"axml" => w.axml = Some(read_at(r, body, meta(len)?)?),
            b"bext" => w.bext = Some(read_at(r, body, meta(len)?)?),
            b"dbmd" => w.dbmd = Some(read_at(r, body, meta(len)?)?),
            b"data" => {
                if len > avail {
                    w.truncated = true;
                }
                w.data = Some((body, len.min(avail)));
            }
            _ => {}
        }
        at = body.saturating_add(len).saturating_add(len & 1);
    }
    Ok(w)
}

/// `chna` entries: (track index, audioTrackUID, track/channel format ref,
/// pack format ref).
pub fn chna(b: &[u8]) -> Result<Vec<(u16, String, String, String)>, Error> {
    let bad = || Error::Invalid("chna chunk".into());
    let n = u16le(b, 2).ok_or_else(bad)? as usize;
    let text = |s: &[u8]| {
        String::from_utf8_lossy(s)
            .trim_end_matches(['\0', ' '])
            .to_owned()
    };
    let mut out = Vec::with_capacity(n.min(1024));
    for i in 0..n {
        let e = 4 + 40 * i;
        let entry = b.get(e..e + 40).ok_or_else(bad)?;
        let index = u16le(entry, 0).ok_or_else(bad)?;
        out.push((
            index,
            text(&entry[2..14]),
            text(&entry[14..28]),
            text(&entry[28..39]),
        ));
    }
    Ok(out)
}

/// `bext` version 2 loudness (EBU Tech 3285 v2): integrated LUFS and max
/// true peak dBTP, when filled in.
pub fn bext_loudness(b: &[u8]) -> (Option<f64>, Option<f64>) {
    if u16le(b, 346).unwrap_or(0) < 2 {
        return (None, None);
    }
    let v = |i| {
        u16le(b, i)
            .map(|x| x as i16)
            .filter(|&x| x != 0x7fff && x != i16::MIN && x != 0)
            .map(|x| f64::from(x) / 100.0)
    };
    (v(412), v(416))
}
