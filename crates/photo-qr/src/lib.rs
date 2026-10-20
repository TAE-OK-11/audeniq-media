//! QR code detection for cover-art policy checks: port of quirc's
//! identification pipeline (adaptive threshold, region labelling, finder
//! detection, perspective fitting) with a standard Reed–Solomon decoder.
//!
//! Only *successfully decoded* symbols are counted, matching how the
//! backend used `zbarimg`. Payloads are validated but never returned.
// Only the calls into the AVX2 builds of the pixel loops are `unsafe`.
#![deny(unsafe_code)]

mod decode;
mod identify;
mod rs;
mod tables;

use photo_core::{Deadline, Result};

/// Result of scanning one image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Scan {
    /// Finder-pattern triples that looked like a QR grid.
    pub candidates: usize,
    /// Grids that decoded with valid error correction.
    pub decoded: usize,
}

/// Scan an 8-bit grayscale image (row-major, `width * height` bytes).
pub fn scan(gray: &[u8], width: usize, height: usize, deadline: &Deadline) -> Result<Scan> {
    scan_owned(gray.to_vec(), width, height, deadline)
}

/// [`scan`] that uses `gray` itself as the working buffer (no copy).
pub fn scan_owned(gray: Vec<u8>, width: usize, height: usize, deadline: &Deadline) -> Result<Scan> {
    use identify::Outcome;
    let mut q = identify::Quirc::new(gray, width, height);
    let (mut wide, row) = match q.identify(0, deadline)? {
        Outcome::Done => return decode_grids(&q, deadline),
        Outcome::RowOverflow(y) => (q.widen(), y),
        Outcome::GroupOverflow => (q.restart_wide(), 0),
    };
    wide.identify(row, deadline)?;
    decode_grids(&wide, deadline)
}

fn decode_grids<P: identify::Px>(q: &identify::Quirc<P>, deadline: &Deadline) -> Result<Scan> {
    let mut out = Scan {
        candidates: q.grids.len(),
        decoded: 0,
    };
    for i in 0..q.grids.len() {
        deadline.check()?;
        let code = q.extract(i);
        if decode::decode(&code).is_ok() || decode::decode(&code.flipped()).is_ok() {
            out.decoded += 1;
        }
    }
    Ok(out)
}

/// [`to_gray`] reusing `pixels`' buffer: pixel i's luma goes to byte i,
/// which no later pixel reads, so no second image is allocated.
pub fn to_gray_in_place(mut pixels: Vec<u8>, channels: usize) -> Vec<u8> {
    if channels > 1 {
        let n = pixels.len() / channels;
        for i in 0..n {
            let p = &pixels[i * channels..i * channels + channels];
            pixels[i] = if channels == 2 {
                p[0]
            } else {
                ((u32::from(p[0]) * 299 + u32::from(p[1]) * 587 + u32::from(p[2]) * 114 + 500)
                    / 1000) as u8
            };
        }
        pixels.truncate(n);
    }
    pixels
}

/// Rec.601 luma of interleaved 8-bit pixels with `channels` samples; the
/// first three are RGB, a fourth (alpha) is ignored.
pub fn to_gray(pixels: &[u8], channels: usize) -> Vec<u8> {
    match channels {
        1 => pixels.to_vec(),
        2 => pixels.as_chunks::<2>().0.iter().map(|p| p[0]).collect(),
        _ => pixels
            .chunks_exact(channels)
            .map(|p| {
                ((u32::from(p[0]) * 299 + u32::from(p[1]) * 587 + u32::from(p[2]) * 114 + 500)
                    / 1000) as u8
            })
            .collect(),
    }
}
