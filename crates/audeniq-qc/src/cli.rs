//! The `audeniq-qc` command line, shared by the `audeniq-qc` binary and
//! `audeniq-music qc`.
use crate::{kernels::Backend, Error, Limits};
use std::{
    ffi::OsString,
    io::{BufWriter, Write},
    path::Path,
    time::{Duration, Instant},
};
fn write_json(value: &impl crate::json::ToJson) -> crate::Result<()> {
    // Serialize retained i16 windows directly. An intermediate JSON Value
    // expands each two-byte sample into a much larger boxed JSON number.
    let stdout = std::io::stdout();
    let mut writer = BufWriter::with_capacity(65536, stdout.lock());
    crate::json::to_writer(&mut writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}
fn run(program: &str, mut args: Vec<OsString>) -> crate::Result<()> {
    let mut limits = Limits::default();
    let mut backend = Backend::detect();
    let mut fingerprint = false;
    let mut analyze_conversion = false;
    let mut compression_level = None;
    let mut pcm_format = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--scalar" {
            backend = Backend::Scalar;
            args.remove(i);
        } else if args[i] == "--analyze" {
            if analyze_conversion {
                return Err(Error::Invalid("duplicate conversion analysis"));
            }
            analyze_conversion = true;
            args.remove(i);
        } else if args[i] == "--fingerprint" {
            fingerprint = true;
            args.remove(i);
        } else if args[i] == "--compression-level" {
            if compression_level.is_some() {
                return Err(Error::Invalid("duplicate compression level"));
            }
            compression_level = Some(
                args.get(i + 1)
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.parse::<u8>().ok())
                    .filter(|n| *n <= 8)
                    .ok_or(Error::Invalid("compression level range 0..8"))?,
            );
            args.drain(i..i + 2);
        } else if args[i] == "--format" {
            if pcm_format.is_some() {
                return Err(Error::Invalid("duplicate PCM format"));
            }
            pcm_format = Some(match args.get(i + 1).and_then(|s| s.to_str()) {
                Some("wav") => crate::pcm::Format::Wav,
                Some("s32le") => crate::pcm::Format::S32le,
                _ => return Err(Error::Invalid("PCM format wav or s32le")),
            });
            args.drain(i..i + 2);
        } else if args[i] == "--timeout-secs" {
            if i + 1 >= args.len() {
                return Err(Error::Invalid("missing timeout"));
            }
            let n = args[i + 1]
                .to_str()
                .and_then(|s| s.parse::<u64>().ok())
                .filter(|n| *n > 0 && *n <= 3600)
                .ok_or(Error::Invalid("timeout range 1..3600"))?;
            limits.deadline = Instant::now() + Duration::from_secs(n);
            args.drain(i..i + 2);
        } else {
            i += 1;
        }
    }
    let command = args.first().and_then(|s| s.to_str()).unwrap_or("help");
    if compression_level.is_some() && command != "convert" {
        return Err(Error::Invalid("compression level requires convert"));
    }
    if pcm_format.is_some() && command != "decode" {
        return Err(Error::Invalid("PCM format requires decode"));
    }
    if analyze_conversion && command != "convert" {
        return Err(Error::Invalid("--analyze requires convert"));
    }
    if fingerprint && command != "analyze" && !(command == "convert" && analyze_conversion) {
        return Err(Error::Invalid(
            "--fingerprint requires analyze or convert --analyze",
        ));
    }
    match (command, args.len()) {
        ("capabilities", 1) => write_json(
            &crate::json!({"engine":crate::ENGINE_VERSION,"backend":backend,"cpu_features":crate::kernels::cpu_features(),"conversion_qc":true,"audio":["WAV/RF64/BW64 PCM16/24","AIFF/AIFC integer PCM","FLAC","M4A ALAC","TTA1","WavPack integer lossless single-block"],"images":["JPEG","PNG"],"pcm_outputs":["wav","s32le"],"compression_levels":{"min":0,"max":8,"default":5},"metric_version":crate::METRIC_VERSION,"resampler_version":crate::RESAMPLER_VERSION,"true_peak_certified":false,"media_codecs":if cfg!(feature="reference-codecs") {"symphonia-reference"} else {"audeniq-native"},"ffmpeg_runtime":false}),
        ),
        ("probe", 2) => write_json(&crate::probe::media(Path::new(&args[1]), limits)?),
        ("image-probe", 2) => write_json(&crate::probe::cover(Path::new(&args[1]), limits)?),
        ("tags", 2) => write_json(
            &crate::json!({"format":{"tags":crate::probe::tags(Path::new(&args[1]),&limits)?}}),
        ),
        ("analyze", 2) => write_json(&crate::meter::analyze(
            Path::new(&args[1]),
            limits,
            backend,
            fingerprint,
        )?),
        ("fingerprint", 2) => write_json(&crate::resample::fingerprint(
            Path::new(&args[1]),
            limits,
            backend,
        )?),
        ("pcm-hash", 2) => {
            let (spec, hash, frames) =
                crate::audio::pcm_sha256(Path::new(&args[1]), limits, backend)?;
            write_json(&crate::json!({"spec":spec,"pcm_sha256":hash,"frames":frames}))
        }
        ("decode", 3) => write_json(&crate::pcm::decode(
            Path::new(&args[1]),
            Path::new(&args[2]),
            pcm_format.unwrap_or(crate::pcm::Format::Wav),
            limits,
            backend,
        )?),
        ("convert", 3) => write_json(&crate::flac::convert_with_options(
            Path::new(&args[1]),
            Path::new(&args[2]),
            limits,
            backend,
            crate::flac::ConvertOptions {
                compression_level,
                analyze: analyze_conversion,
                fingerprint,
            },
        )?),
        ("help" | "--help" | "-h", _) => write_json(
            &crate::json!({"usage":format!("{program} <probe|analyze|fingerprint|pcm-hash|image-probe|tags> FILE | convert INPUT OUTPUT.flac [--compression-level 0..8] [--analyze [--fingerprint]] | decode INPUT OUTPUT [--format wav|s32le] | capabilities; options: --scalar --fingerprint --timeout-secs N")}),
        ),
        _ => Err(Error::Invalid("command/arguments; use --help")),
    }
}
/// Run one command (`args` without the program name) and return the process
/// exit code; errors are reported on stderr as JSON.
pub fn main(program: &str, args: Vec<OsString>) -> i32 {
    let outcome = run(program, args);
    #[cfg(feature = "profile-native")]
    eprintln!(
        "{}",
        crate::json!({"native_profile":crate::native_profile(),"success":outcome.is_ok()})
    );
    match outcome {
        Ok(()) => 0,
        Err(e) => {
            let code = match e {
                Error::Io(_) => "IO",
                Error::Invalid(_) => "INVALID_INPUT",
                Error::Unsupported(_) => "UNSUPPORTED",
                Error::Limit(_) => "RESOURCE_LIMIT",
                Error::Deadline => "DEADLINE",
            };
            eprintln!("{}", crate::json!({"error":code,"detail":e.to_string()}));
            2
        }
    }
}
