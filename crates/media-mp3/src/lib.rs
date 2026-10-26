// SPDX-License-Identifier: LGPL-2.1-or-later
//! MPEG-1/2/2.5 Layer III decoder.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Truncated,
    Invalid(&'static str),
    Unsupported(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::Truncated => f.write_str("MP3 frame ends early"),
            Error::Invalid(s) => write!(f, "invalid MP3 {s}"),
            Error::Unsupported(s) => write!(f, "unsupported MP3 {s}"),
        }
    }
}

/// What a decoded frame was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    pub sample_rate: u32,
    pub channels: usize,
    pub frames: usize,
}

#[derive(Default)]
pub struct Decoder {}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder::default()
    }

    pub fn decode(&mut self, _packet: &[u8], _out: &mut Vec<f32>) -> Result<Frame, Error> {
        Err(Error::Unsupported("decoding"))
    }
}
