//! MP4/MOV index parser of `audeniq-music video probe`.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = media_video::probe_with(&mut std::io::Cursor::new(data), true);
});
