//! `audeniq-music video …` command line.
//!
//! ```text
//! video probe [--sound] FILE   report JSON on stdout, exit 0; `--sound`
//!                              also decodes and measures the sound track
//! video sound-ranges FILE      byte ranges of the sound track's data
//! video capabilities           engine and rule versions
//! ```
//!
//! Errors go to stderr as `{"detail":…,"error":"IO|INVALID|LIMIT|USAGE"}`
//! with exit code 2, the contract of `audeniq-music qc`.
use std::io::Write;
use std::process::ExitCode;

fn fail(code: &str, detail: &str) -> ExitCode {
    let v = media_json::json!({"error": code, "detail": detail});
    let _ = writeln!(std::io::stderr(), "{}", media_json::to_string(&v));
    ExitCode::from(2)
}

fn emit(v: &impl media_json::ToJson) -> ExitCode {
    let mut out = media_json::to_vec(v);
    out.push(b'\n');
    let mut stdout = std::io::stdout().lock();
    match stdout.write_all(&out).and_then(|()| stdout.flush()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail("IO", &e.to_string()),
    }
}

pub fn main(prog: &str, args: Vec<std::ffi::OsString>) -> ExitCode {
    let usage = format!(
        "usage: {prog} probe [--sound] FILE | {prog} sound-ranges FILE | {prog} capabilities"
    );
    let words: Vec<Option<&str>> = args.iter().map(|a| a.to_str()).collect();
    match words.as_slice() {
        [Some("probe"), Some("--sound"), _] | [Some("probe"), _] => {
            let sound = words[1] == Some("--sound");
            let path: &std::path::Path = args[args.len() - 1].as_ref();
            match crate::probe_path(path, sound) {
                Ok(report) => emit(&report),
                Err(e) => fail(e.code(), &e.to_string()),
            }
        }
        [Some("sound-ranges"), _] => match crate::sound_ranges(args[1].as_ref()) {
            Ok(ranges) => {
                let total: u64 = ranges.iter().map(|r| r.1).sum();
                let list: Vec<[u64; 2]> = ranges.iter().map(|&(o, l)| [o, l]).collect();
                emit(&media_json::json!({"ranges": list, "bytes": total}))
            }
            Err(e) => fail(e.code(), &e.to_string()),
        },
        [Some("capabilities")] => emit(&media_json::json!({
            "engine": crate::ENGINE_VERSION,
            "rule": crate::RULE_VERSION,
            "containers": ["mp4", "mov"],
            "max_moov_bytes": crate::file::MAX_MOOV,
            "sound_codecs": ["aac", "pcm"],
            "max_sound_bytes": crate::sound::MAX_SOUND_BYTES,
        })),
        _ => fail("USAGE", &usage),
    }
}
