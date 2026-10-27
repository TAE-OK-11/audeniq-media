//! MP3 frames (`media-mp3`): the input as consecutive packets, each
//! starting with a valid Layer III header so the decoder gets past it.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut d = media_mp3::Decoder::new();
    let mut out = Vec::new();
    for unit in data.chunks(420) {
        let mut p = vec![0xff, 0xfb, 0x90, 0x64];
        p.extend_from_slice(unit);
        let _ = d.decode(&p, &mut out);
        let _ = d.decode(unit, &mut out);
        out.clear();
    }
});
