//! Bitstream reading and writing shared by the AUDENIQ media codecs (FLAC,
//! ALAC, TTA, WavPack, AAC, SBR, MP3, H.264/HEVC parameter sets): one MSB
//! reader with a cached word, an LSB reader, a word-at-a-time writer and a
//! table-driven Huffman decoder. Each codec keeps only its own syntax.
#![deny(unsafe_code)]

mod huffman;
mod lsb;
mod msb;
#[allow(unsafe_code)]
mod writer;

pub use huffman::Huffman;
pub use lsb::Lsb;
pub use msb::Bits;
pub use writer::BeWriter;

/// A read past the end of the data (or a reader's limit), or a malformed
/// code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Eof,
    Invalid(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::Eof => f.write_str("truncated bitstream"),
            Error::Invalid(what) => write!(f, "invalid {what}"),
        }
    }
}

impl std::error::Error for Error {}
