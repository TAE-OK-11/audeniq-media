//! Encode interleaved f32le PCM to ADTS AAC-LC:
//! `encode_raw IN.f32 OUT.aac RATE CHANNELS BITRATE`.
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let pcm: Vec<f32> = std::fs::read(&a[1])
        .unwrap()
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    let (rate, ch, br): (u32, usize, u32) = (
        a[3].parse().unwrap(),
        a[4].parse().unwrap(),
        a[5].parse().unwrap(),
    );
    let mut e = media_aac::encoder::Encoder::new(rate, ch, br).unwrap();
    let asc = e.asc();
    let sr = (asc[0] & 7) << 1 | asc[1] >> 7;
    let t = std::time::Instant::now();
    let mut units = Vec::new();
    for block in pcm.chunks(4096 * ch) {
        units.extend(e.push(block));
    }
    units.extend(e.finish());
    let mut out = Vec::new();
    let mut bytes = 0usize;
    for au in &units {
        bytes += au.len();
        let len = au.len() + 7;
        out.extend([
            0xff,
            0xf1,
            (1 << 6) | (sr << 2) | ((ch as u8) >> 2),
            (((ch as u8) & 3) << 6) | (len >> 11) as u8,
            (len >> 3) as u8,
            ((len & 7) << 5) as u8 | 0x1f,
            0xfc,
        ]);
        out.extend(au);
    }
    std::fs::write(&a[2], out).unwrap();
    let secs = units.len() as f64 * 1024.0 / f64::from(rate);
    eprintln!(
        "{} units, {:.0} kbit/s, {:.0} ms",
        units.len(),
        bytes as f64 * 8.0 / secs / 1000.0,
        t.elapsed().as_secs_f64() * 1000.0
    );
}
