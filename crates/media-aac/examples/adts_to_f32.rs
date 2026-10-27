//! Decode an ADTS `.aac` file to interleaved f32le on stdout (channels and
//! rate on stderr), for comparing with another decoder.
use std::io::Write;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: adts_to_f32 FILE.aac");
    let data = std::fs::read(path).unwrap();
    let (mut at, mut dec, mut out, mut errors) = (0, None, Vec::new(), 0);
    while at + 7 <= data.len() {
        let h = &data[at..];
        let len = (usize::from(h[3] & 3) << 11) | (usize::from(h[4]) << 3) | usize::from(h[5] >> 5);
        let hdr = if h[1] & 1 == 1 { 7 } else { 9 };
        let d = dec.get_or_insert_with(|| {
            let v = (u32::from(h[2] >> 6) + 1) << 11
                | u32::from(h[2] >> 2 & 0xf) << 7
                | (u32::from(h[2] & 1) << 2 | u32::from(h[3] >> 6)) << 3;
            media_aac::Decoder::new(&[(v >> 8) as u8, v as u8]).unwrap()
        });
        if d.decode(&data[at + hdr..at + len], &mut out).is_err() {
            errors += 1;
        }
        at += len;
    }
    let d = dec.unwrap();
    eprintln!(
        "channels {} rate {} errors {errors}",
        d.channels(),
        d.output_rate()
    );
    let mut w = std::io::stdout().lock();
    for v in out {
        w.write_all(&v.to_le_bytes()).unwrap();
    }
}
