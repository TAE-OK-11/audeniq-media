//! ADM BWF reader (`media-adm`, `audeniq-music atmos probe --peaks`).
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = media_adm::probe(&mut std::io::Cursor::new(data), true);
});
