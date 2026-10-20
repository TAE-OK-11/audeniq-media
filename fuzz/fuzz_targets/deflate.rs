#![no_main]
use libfuzzer_sys::fuzz_target;
use photo_deflate::{inflate_zlib, inflate_raw, Compressor, Level, Tuning};

fuzz_target!(|data: &[u8]| {
    // Untrusted input to both inflaters.
    let mut out = Vec::new();
    let _ = inflate_zlib(data, &mut out, 1 << 22, false);
    out.clear();
    let _ = inflate_zlib(data, &mut out, 1 << 12, true);
    out.clear();
    let _ = inflate_raw(data, &mut out, 1 << 22);
    // Round trip through the deflater.
    if let Some((&ctl, body)) = data.split_first() {
        let level = Level::new(ctl % 10);
        let tuning = if ctl & 0x80 != 0 { Tuning::Image } else { Tuning::Default };
        let mut z = Compressor::zlib_tuned(level, tuning);
        let mut packed = Vec::new();
        let split = body.len() * usize::from(ctl >> 4) / 16;
        z.write(&body[..split], &mut packed);
        z.write(&body[split..], &mut packed);
        z.finish(&mut packed);
        let mut back = Vec::new();
        inflate_zlib(&packed, &mut back, body.len() + 1, false).expect("own stream inflates");
        assert_eq!(back, body);
    }
});
