#![no_main]
use audeniq_qc::{kernels::Backend, Limits};
use libfuzzer_sys::fuzz_target;
use std::time::{Duration, Instant};

fuzz_target!(|data: &[u8]| {
    let path = std::path::PathBuf::from(format!("/dev/shm/fz-audio-{}", std::process::id()));
    std::fs::write(&path, data).unwrap();
    let limits = || Limits {
        max_frames: 4_000_000,
        deadline: Instant::now() + Duration::from_secs(5),
        ..Limits::default()
    };
    let _ = audeniq_qc::probe::media(&path, limits());
    let _ = audeniq_qc::probe::tags(&path, &limits());
    for backend in [Backend::detect(), Backend::Scalar] {
        let _ = audeniq_qc::audio::pcm_sha256(&path, limits(), backend);
    }
});
