//! AUDENIQ video probe: what FFprobe told about an uploaded music video,
//! read from the MP4/QuickTime index alone, plus the checks a music-video
//! delivery needs (`mv-delivery-v2`), and with `--sound` the sound track
//! decoded and measured ([`sound`]).
//!
//! Only box headers, `ftyp`, `moov` and `moof` are read; `mdat` is never
//! touched (with `--sound`, only the sound track's part of it), so an 8 GB
//! master costs a few ranged reads. That also means the picture report is
//! metadata: a stream whose video frames are corrupt still probes clean.
//! The bitstream parameter sets (H.264/HEVC SPS, av1C, vpcC) are parsed for
//! profile, level, chroma format, bit depth, cropped size, field coding and
//! colour description, the way a decoder sees them before the first frame.
//!
//! Report fields and names follow FFprobe (`codec_name`, `profile`,
//! `pix_fmt`, `avg_frame_rate`, colour names) so staff and the backend can
//! read it the same way; `tests/ffprobe.rs` compares them on encoded files.
pub mod audio;
mod bits;
mod boxes;
pub mod check;
pub mod cli;
pub mod codec;
mod file;
mod report;
pub mod sound;
mod track;

pub use check::{Issue, Severity};
pub use report::{AudioStream, Container, OtherStream, Report, VideoStream};

use std::io::{Read, Seek, SeekFrom};

/// Engine identity reported in every probe.
pub const ENGINE_VERSION: &str = "audeniq-video/0.1.0";
/// Delivery rules the `issues` were evaluated against (v2: sound checks).
pub const RULE_VERSION: &str = "mv-delivery-v2";

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// Not an MP4/QuickTime file, or its index is malformed.
    Invalid(String),
    /// Beyond the engine's index limits.
    Limit(String),
}

impl Error {
    /// Stable code for the JSON error report.
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

/// Probe a seekable MP4/MOV and evaluate the delivery checks.
pub fn probe<R: Read + Seek>(r: &mut R) -> Result<Report, Error> {
    probe_with(r, false)
}

/// [`probe`], and with `sound` also decode and measure the sound track
/// ([`sound::measure`]), which reads its media data.
pub fn probe_with<R: Read + Seek>(r: &mut R, sound: bool) -> Result<Report, Error> {
    let size = r.seek(SeekFrom::End(0))?;
    let layout = file::walk(r, size)?;
    let (mut report, tracks) = report::build(&layout)?;
    if sound {
        match sound::measure(r, &layout)? {
            Ok(s) => report.sound = Some(s),
            Err(note) => report.sound_note = Some(note),
        }
    }
    report.issues = check::evaluate(&report, &layout, &tracks);
    report.mv_ready = report.issues.iter().all(|i| i.severity != Severity::Error);
    Ok(report)
}

/// [`probe_with`] of a file on disk.
pub fn probe_path(path: &std::path::Path, sound: bool) -> Result<Report, Error> {
    let mut f = std::io::BufReader::with_capacity(64 * 1024, std::fs::File::open(path)?);
    probe_with(&mut f, sound)
}

/// Byte ranges of the sound track's data ([`sound::ranges`]).
pub fn sound_ranges(path: &std::path::Path) -> Result<Vec<(u64, u64)>, Error> {
    let mut f = std::io::BufReader::with_capacity(64 * 1024, std::fs::File::open(path)?);
    let size = f.seek(SeekFrom::End(0))?;
    let layout = file::walk(&mut f, size)?;
    if layout.moov.is_none() {
        return Err(Error::Invalid("no moov (movie index)".into()));
    }
    sound::ranges(&layout)
}
