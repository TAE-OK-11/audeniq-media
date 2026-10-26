//! `audeniq-music` — the one engine binary the AUDENIQ backend runs.
//!
//! ```text
//! audeniq-music qc <command> ...      audio engine, same commands and JSON as `audeniq-qc`
//! audeniq-music photo <command> ...   image/PDF tools, same commands and output as `audeniq-photo`
//! audeniq-music video <command> ...   music-video probe and delivery checks (`media-video`)
//! audeniq-music --version
//! ```
//!
//! Both halves share one build (`--profile release-music`), which has to
//! unwind because the photo decoders isolate panics with `catch_unwind`.
//! The audio half keeps its abort-on-panic behaviour through a panic hook.
#![forbid(unsafe_code)]

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::ffi::OsString;
use std::process::ExitCode;

const USAGE: &str = "usage: audeniq-music qc <command> ... | audeniq-music photo <command> ... | audeniq-music video <command> ... | audeniq-music --version";

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let group = args.next();
    match group.as_ref().and_then(|g| g.to_str()) {
        Some("qc") => {
            // audeniq-qc is built with panic = "abort": a bug ends the
            // process with SIGABRT instead of exit code 101.
            let report = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                report(info);
                std::process::abort();
            }));
            let code = audeniq_qc::cli::main("audeniq-music qc", args.collect::<Vec<OsString>>());
            ExitCode::from(code as u8)
        }
        Some("photo") => {
            let args = match args
                .map(OsString::into_string)
                .collect::<Result<Vec<_>, _>>()
            {
                Ok(args) => args,
                Err(_) => {
                    eprintln!("error: arguments must be UTF-8");
                    return ExitCode::FAILURE;
                }
            };
            photo_cli::main("audeniq-music photo", args)
        }
        Some("video") => media_video::cli::main("audeniq-music video", args.collect()),
        Some("--version" | "-V") => {
            println!(
                "audeniq-music {} ({})",
                env!("CARGO_PKG_VERSION"),
                audeniq_qc::ENGINE_VERSION
            );
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
