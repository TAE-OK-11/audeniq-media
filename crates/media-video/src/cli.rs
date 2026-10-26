//! `audeniq-music video …` command line.
//!
//! ```text
//! video probe FILE      report JSON on stdout, exit 0
//! video capabilities    engine and rule versions
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
    let usage = format!("usage: {prog} probe FILE | {prog} capabilities");
    let mut args = args.into_iter();
    match (
        args.next().as_ref().and_then(|a| a.to_str()),
        args.next(),
        args.next(),
    ) {
        (Some("probe"), Some(path), None) => match crate::probe_path(path.as_ref()) {
            Ok(report) => emit(&report),
            Err(e) => fail(e.code(), &e.to_string()),
        },
        (Some("capabilities"), None, None) => emit(&media_json::json!({
            "engine": crate::ENGINE_VERSION,
            "rule": crate::RULE_VERSION,
            "containers": ["mp4", "mov"],
            "max_moov_bytes": crate::file::MAX_MOOV,
        })),
        _ => fail("USAGE", &usage),
    }
}
