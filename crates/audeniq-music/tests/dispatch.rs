//! The one binary routes `qc` and `photo` to the same command lines as the
//! separate `audeniq-qc` and `audeniq-photo` binaries.
use std::process::{Command, Output};

fn music(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_audeniq-music"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn version_names_the_audio_engine() {
    let out = music(&["--version"]);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!("audeniq-music 0.1.0 ({})\n", audeniq_qc::ENGINE_VERSION)
    );
}

#[test]
fn unknown_group_is_a_usage_error() {
    for args in [&[][..], &["probe", "x"][..], &["QC", "capabilities"][..]] {
        let out = music(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty());
    }
}

#[test]
fn qc_capabilities_match_the_engine() {
    let out = music(&["qc", "capabilities"]);
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains(&format!("\"engine\":\"{}\"", audeniq_qc::ENGINE_VERSION)));
    assert!(text.ends_with("}\n"));
}

#[test]
fn qc_errors_keep_the_json_contract() {
    let out = music(&["qc", "probe", "/nonexistent/audeniq-music-test.wav"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.starts_with("{\"detail\":"), "{err}");
    assert!(err.contains("\"error\":\"IO\""), "{err}");
}

#[test]
fn qc_usage_names_the_subcommand() {
    let out = music(&["qc", "--help"]);
    assert!(out.status.success());
    assert!(
        String::from_utf8(out.stdout)
            .unwrap()
            .contains("audeniq-music qc <probe|")
    );
}

#[test]
fn photo_commands_run() {
    let out = music(&["photo", "--version"]);
    assert!(out.status.success());
    assert_eq!(out.stdout, b"audeniq-music photo 0.1.0\n");

    let out = music(&["photo", "qr", "/nonexistent/audeniq-music-test.png"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8(out.stderr)
            .unwrap()
            .starts_with("error: ")
    );

    // Sanitize never echoes parser details.
    let out = music(&[
        "photo",
        "sanitize",
        "/nonexistent/a.png",
        "/nonexistent/b.png",
        "image/png",
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(out.stderr, b"upload sanitization failed\n");
}

#[test]
fn video_group_routes_to_the_video_probe() {
    let out = music(&["video", "capabilities"]);
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains(&format!("\"engine\":\"{}\"", media_video::ENGINE_VERSION)));
    assert!(text.ends_with("}\n"));

    let out = music(&["video", "probe", "/nonexistent/audeniq-music-test.mp4"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.starts_with("{\"detail\":"), "{err}");
    assert!(err.contains("\"error\":\"IO\""), "{err}");

    let out = music(&["video", "probe"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8(out.stderr)
            .unwrap()
            .contains("\"error\":\"USAGE\"")
    );
}
