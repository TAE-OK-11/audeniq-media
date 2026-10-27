//! `audeniq-music video …` command line.
//!
//! ```text
//! video probe [--sound] [--profile NAME] [--master FILE] FILE
//!                              report JSON on stdout, exit 0; `--sound`
//!                              also decodes and measures the sound track,
//!                              `--profile` adds one platform's spec checks
//!                              (youtube, spotify, apple), `--master` compares
//!                              the sound with the released master
//! video sound-ranges FILE      byte ranges of the sound track's data
//! video convert-audio [--preset NAME] [--codec aac|pcm24] [--bitrate N]
//!       [--rate 44100|48000] [--channels L,R|M] IN OUT
//!                              copy the picture, convert the sound to a
//!                              distributor's spec (default preset from
//!                              OUT's extension: .mov → pcm24, else aac-320);
//!                              `--channels` picks source channels, counted
//!                              from 1 across all sound tracks in file order
//! video profiles               the platform specs `--profile` checks
//! video match-master MV MASTER is the MV's sound the released master?
//!                              (offset, drift, level, null-test SNR, verdict)
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

/// `convert-audio` options and paths.
fn convert_args(
    args: &[std::ffi::OsString],
) -> Result<
    (
        crate::convert::Options,
        std::path::PathBuf,
        std::path::PathBuf,
    ),
    String,
> {
    use crate::convert::{Codec, Container, preset};
    let mut paths = Vec::new();
    let (mut name, mut codec, mut bitrate, mut rate) = (None, None, None, None);
    let mut pick = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || {
            it.next()
                .and_then(|v| v.to_str())
                .ok_or_else(|| format!("{} needs a value", a.to_string_lossy()))
        };
        match a.to_str() {
            Some("--preset") => name = Some(value()?.to_string()),
            Some("--codec") => codec = Some(value()?.to_string()),
            Some("--bitrate") => {
                bitrate = Some(value()?.parse::<u32>().map_err(|e| e.to_string())?)
            }
            Some("--rate") => rate = Some(value()?.parse::<u32>().map_err(|e| e.to_string())?),
            Some("--channels") => {
                let v = value()?;
                let n: Vec<u16> = v
                    .split(',')
                    .map(|c| c.trim().parse::<u16>().ok().filter(|&c| c > 0))
                    .collect::<Option<_>>()
                    .ok_or_else(|| format!("--channels {v}: channel numbers from 1"))?;
                pick = Some(match n.as_slice() {
                    [m] => crate::Pick::Mono(*m),
                    [l, r] => crate::Pick::Stereo(*l, *r),
                    _ => return Err(format!("--channels {v}: one (mono) or two (L,R) channels")),
                });
            }
            Some(s) if s.starts_with("--") => return Err(format!("unknown option {s}")),
            _ => paths.push(std::path::PathBuf::from(a)),
        }
    }
    let [input, output] =
        <[_; 2]>::try_from(paths).map_err(|_| "IN and OUT are required".to_string())?;
    let mov = output
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("mov"));
    let name = name.unwrap_or_else(|| if mov { "pcm24" } else { "aac-320" }.into());
    let mut o = preset(&name).ok_or_else(|| format!("unknown preset {name}"))?;
    o.container = if mov { Container::Mov } else { Container::Mp4 };
    match codec.as_deref() {
        Some("aac") => o.codec = Codec::Aac(bitrate.unwrap_or(320_000)),
        Some("pcm24") => o.codec = Codec::Pcm24,
        Some(c) => return Err(format!("unknown codec {c}")),
        None => {
            if let (Some(b), Codec::Aac(_)) = (bitrate, o.codec) {
                o.codec = Codec::Aac(b);
            }
        }
    }
    if let Some(r) = rate {
        o.sample_rate = r;
    }
    o.pick = pick;
    Ok((o, input, output))
}

pub fn main(prog: &str, args: Vec<std::ffi::OsString>) -> ExitCode {
    let usage = format!(
        "usage: {prog} probe [--sound] [--profile NAME] [--master FILE] FILE | {prog} profiles | {prog} match-master MV MASTER | {prog} sound-ranges FILE | {prog} convert-audio [--preset NAME] [--codec aac|pcm24] [--bitrate N] [--rate HZ] [--channels L,R|M] IN OUT | {prog} capabilities"
    );
    let words: Vec<Option<&str>> = args.iter().map(|a| a.to_str()).collect();
    match words.as_slice() {
        [Some("probe"), .., _] => {
            let (mut sound, mut profile, mut master) = (false, None, None);
            let mut opts = words[1..words.len() - 1].iter();
            while let Some(o) = opts.next() {
                match o {
                    Some("--sound") => sound = true,
                    Some("--profile") => {
                        let name = opts.next().copied().flatten().unwrap_or("");
                        match crate::check::profile(name) {
                            Some(p) => profile = Some(p),
                            None => {
                                let names: Vec<_> =
                                    crate::check::PROFILES.iter().map(|p| p.name).collect();
                                return fail(
                                    "USAGE",
                                    &format!("unknown profile {name:?} ({})", names.join(", ")),
                                );
                            }
                        }
                    }
                    Some("--master") => match opts.next().copied().flatten() {
                        Some(m) => master = Some(std::path::PathBuf::from(m)),
                        None => return fail("USAGE", &usage),
                    },
                    _ => return fail("USAGE", &usage),
                }
            }
            let path: &std::path::Path = args[args.len() - 1].as_ref();
            match crate::probe_path_master(path, sound, profile, master.as_deref()) {
                Ok(report) => emit(&report),
                Err(e) => fail(e.code(), &e.to_string()),
            }
        }
        [Some("profiles")] => emit(&crate::check::PROFILES.to_vec()),
        [Some("match-master"), _, _] => {
            match crate::master::match_master(args[1].as_ref(), args[2].as_ref()) {
                Ok(m) => emit(&m),
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
        [Some("convert-audio"), ..] => match convert_args(&args[1..]) {
            Ok((opts, input, output)) => match crate::convert::convert(&input, &output, &opts) {
                Ok(c) => emit(&c),
                Err(e) => fail(e.code(), &e.to_string()),
            },
            Err(e) => fail("USAGE", &format!("{e}; {usage}")),
        },
        [Some("capabilities")] => emit(&media_json::json!({
            "engine": crate::ENGINE_VERSION,
            "rule": crate::RULE_VERSION,
            "containers": ["mp4", "mov"],
            "max_moov_bytes": crate::file::MAX_MOOV,
            "sound_codecs": ["aac", "alac", "mp3", "pcm"],
            "max_sound_bytes": crate::sound::MAX_SOUND_BYTES,
            "convert_presets": crate::convert::PRESETS.iter().map(|p| p.0).collect::<Vec<_>>(),
            "profiles": crate::check::PROFILES.iter().map(|p| p.version).collect::<Vec<_>>(),
        })),
        _ => fail("USAGE", &usage),
    }
}
