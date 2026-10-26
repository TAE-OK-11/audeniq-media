//! AAC-LC access units (`media-aac`), the first bytes choosing the config.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    let Ok(mut d) = media_aac::Decoder::new(&data[..2]) else {
        return;
    };
    let mut out = Vec::new();
    for unit in data[2..].chunks(512) {
        let _ = d.decode(unit, &mut out);
        out.clear();
    }
});
