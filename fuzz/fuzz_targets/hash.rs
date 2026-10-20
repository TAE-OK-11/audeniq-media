#![no_main]
use libfuzzer_sys::fuzz_target;
use md5::Digest as _;

fuzz_target!(|data: &[u8]| {
    let Some((&ctl, body)) = data.split_first() else { return };
    // Feed in pieces so every buffering path runs.
    let step = usize::from(ctl).max(1);
    let mut s = media_hash::Sha256::new();
    let mut m = media_hash::Md5::new();
    let mut c = media_hash::Crc32::new();
    for piece in body.chunks(step) {
        s.update(piece);
        m.update(piece);
        c.update(piece);
    }
    assert_eq!(s.finalize()[..], sha2::Sha256::digest(body)[..]);
    assert_eq!(m.finalize()[..], md5::Md5::digest(body)[..]);
    assert_eq!(c.finalize(), crc32fast::hash(body));
    assert_eq!(media_hash::crc32(body), crc32fast::hash(body));
    // Fused path: SHA over one buffer, MD5 over another.
    let half = body.len() / 2;
    let (a, b) = body.split_at(half);
    let mut s = media_hash::Sha256::new();
    let mut m = media_hash::Md5::new();
    media_hash::update_both(&mut s, a, &mut m, b, if ctl & 1 == 0 { 16 } else { 24 });
    assert_eq!(s.finalize()[..], sha2::Sha256::digest(a)[..]);
    assert_eq!(m.finalize()[..], md5::Md5::digest(b)[..]);
});
