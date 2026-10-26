//! Decode an ADTS `.aac` file and report frames and wall time:
//! `cargo run --release -p media-aac --example decode_adts FILE`.
fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: decode_adts FILE.aac");
    let data = std::fs::read(path).unwrap();
    let t = std::time::Instant::now();
    let (mut at, mut dec, mut out, mut frames) = (0, None, Vec::new(), 0usize);
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
        frames += d.decode(&data[at + hdr..at + len], &mut out).unwrap();
        out.clear();
        at += len;
    }
    println!(
        "{frames} frames in {:.1} ms",
        t.elapsed().as_secs_f64() * 1000.0
    );
}
