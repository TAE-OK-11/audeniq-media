//! AAC access units (`media-aac`, with HE-AAC's SBR and PS). The first byte
//! picks the config length (2–5 bytes, so HE-AAC configs and sync
//! extensions are reachable), the config follows, then the units.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&first, rest)) = data.split_first() else {
        return;
    };
    let n = 2 + usize::from(first & 3);
    if rest.len() < n {
        return;
    }
    let Ok(mut d) = media_aac::Decoder::new(&rest[..n]) else {
        return;
    };
    let mut out = Vec::new();
    for unit in rest[n..].chunks(512) {
        let _ = d.decode(unit, &mut out);
        out.clear();
    }
});
