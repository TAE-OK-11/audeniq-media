//! AUDENIQ video probe: what FFprobe told about an uploaded music video,
//! read from the MP4/QuickTime index alone, plus the checks a music-video
//! delivery needs (`mv-delivery-v2`), and with `--sound` the sound track
//! decoded and measured ([`sound`]). [`convert`] rewrites a video with its
//! picture copied and its sound converted to a distributor's spec.
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
pub mod convert;
mod file;
mod report;
pub mod sound;
mod source;
mod track;

pub use check::{Issue, Severity};
pub use report::{AudioStream, Container, OtherStream, Report, VideoStream};
pub use source::Pick;

use std::io::{Read, Seek, SeekFrom};

/// Read buffer for index and sound reads. Sound chunks sit between video
/// chunks, so each is a seek and a read: a buffer much larger than a chunk
/// would read video bytes only to throw them away. Reads at least this large
/// go straight to the file.
pub(crate) const READ_BUFFER: usize = 16 << 10;

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
            Ok(s) => {
                // HE-AAC signalled only in the stream (implicit SBR): the
                // decoder knows its real profile and rate.
                if let Some(p) = &s.profile
                    && p != "LC"
                    && let Some(a) = report.audio.iter_mut().find(|a| a.track_id == s.track_id)
                {
                    a.profile = Some(p.clone());
                    a.sample_rate = s.sample_rate;
                    if s.mapping != "dual-mono" {
                        a.channels = s.channels;
                    }
                }
                report.sound = Some(s);
            }
            Err(note) => report.sound_note = Some(note),
        }
    }
    report.issues = check::evaluate(&report, &layout, &tracks);
    report.mv_ready = report.issues.iter().all(|i| i.severity != Severity::Error);
    Ok(report)
}

/// [`probe_with`] of a file on disk.
pub fn probe_path(path: &std::path::Path, sound: bool) -> Result<Report, Error> {
    let mut f = std::io::BufReader::with_capacity(READ_BUFFER, std::fs::File::open(path)?);
    probe_with(&mut f, sound)
}

/// Byte ranges of the sound track's data ([`sound::ranges`]).
pub fn sound_ranges(path: &std::path::Path) -> Result<Vec<(u64, u64)>, Error> {
    let mut f = std::io::BufReader::with_capacity(READ_BUFFER, std::fs::File::open(path)?);
    let size = f.seek(SeekFrom::End(0))?;
    let layout = file::walk(&mut f, size)?;
    if layout.moov.is_none() {
        return Err(Error::Invalid("no moov (movie index)".into()));
    }
    sound::ranges(&layout)
}

/// The delivery sound of an MP4/MOV decoded as it is: edit lists applied,
/// no resampling or folding, handed to `sink` block by block (interleaved
/// f32). Returns the sample rate and channel names.
pub fn decode_sound_blocks(
    path: &std::path::Path,
    sink: &mut dyn FnMut(&[f32]),
) -> Result<(u32, Vec<&'static str>), Error> {
    let mut f = std::io::BufReader::with_capacity(READ_BUFFER, std::fs::File::open(path)?);
    let size = f.seek(SeekFrom::End(0))?;
    let layout = file::walk(&mut f, size)?;
    let moov = layout
        .moov
        .clone()
        .ok_or_else(|| Error::Invalid("no moov (movie index)".into()))?;
    let plan = source::plan(&moov, None)?.ok_or_else(|| Error::Invalid("no sound track".into()))?;
    let ts = source::movie_timescale(&moov).unwrap_or(1);
    let mut mix = source::Mix::open(&mut f, &layout, &plan, ts, source::Use::Convert)?;
    let mut pcm = Vec::new();
    while mix.pull(&mut f, &mut pcm)? {
        sink(&pcm);
        pcm.clear();
    }
    Ok((mix.rate(), mix.names()))
}

/// [`decode_sound_blocks`] gathered: (sample rate, channel names,
/// interleaved f32).
pub fn decode_sound(path: &std::path::Path) -> Result<(u32, Vec<&'static str>, Vec<f32>), Error> {
    let mut all = Vec::new();
    let (rate, names) = decode_sound_blocks(path, &mut |b| all.extend_from_slice(b))?;
    Ok((rate, names, all))
}
