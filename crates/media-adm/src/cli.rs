//! `audeniq-music atmos …` command line.
//!
//! ```text
//! atmos probe [--peaks] FILE   report JSON on stdout, exit 0
//! atmos capabilities           engine and rule versions
//! ```
//!
//! Errors go to stderr as `{"detail":…,"error":"IO|INVALID|LIMIT|USAGE"}`
//! with exit code 2, like `audeniq-music qc` and `video`.
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
    let usage = format!("usage: {prog} probe [--peaks] FILE | {prog} capabilities");
    let words: Vec<Option<&str>> = args.iter().map(|a| a.to_str()).collect();
    match words.as_slice() {
        [Some("probe"), Some("--peaks"), _] | [Some("probe"), _] => {
            let peaks = words[1] == Some("--peaks");
            let path: &std::path::Path = args[args.len() - 1].as_ref();
            match crate::probe_path(path, peaks) {
                Ok(r) => emit(&r),
                Err(e) => fail(e.code(), &e.to_string()),
            }
        }
        [Some("capabilities")] => emit(&media_json::json!({
            "engine": crate::ENGINE_VERSION,
            "rule": crate::RULE_VERSION,
            "containers": ["RIFF", "RF64", "BW64"],
            "max_metadata_bytes": crate::wave::MAX_META,
        })),
        _ => fail("USAGE", &usage),
    }
}
