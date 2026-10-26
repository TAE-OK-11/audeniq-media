//! Decode a raw MP3 stream (frames back to back) to f32le on stdout.
use std::io::Write;

fn main() {
    let path = std::env::args().nth(1).expect("usage: decode_mp3 FILE.mp3");
    let data = std::fs::read(path).unwrap();
    let mut d = media_mp3::Decoder::new();
    let mut out = Vec::new();
    let mut at = 0;
    // Skip an ID3v2 tag.
    if data.starts_with(b"ID3") && data.len() > 10 {
        let n = data[6..10]
            .iter()
            .fold(0usize, |a, &b| a << 7 | usize::from(b & 0x7f));
        at = 10 + n;
    }
    let mut errors = 0;
    let mut first = None;
    while at + 4 <= data.len() {
        let len = frame_len(&data[at..]);
        let Some(len) = len else {
            at += 1;
            continue;
        };
        let end = (at + len).min(data.len());
        let mut pcm = Vec::new();
        match d.decode(&data[at..end], &mut pcm) {
            Ok(f) => {
                // Mode changes mid-stream: keep the first frame's channels.
                let want = *first.get_or_insert(f.channels);
                media_mp3::to_channels(&pcm, f.channels, want, &mut out);
            }
            Err(e) => {
                errors += 1;
                eprintln!("frame at byte {at}: {e}");
            }
        }
        at = end;
    }
    eprintln!("errors {errors}");
    let mut w = std::io::stdout().lock();
    for v in out {
        w.write_all(&v.to_le_bytes()).unwrap();
    }
}

fn frame_len(h: &[u8]) -> Option<usize> {
    if h[0] != 0xff || h[1] & 0xe0 != 0xe0 {
        return None;
    }
    let lsf = h[1] >> 3 & 3 != 3;
    let mpeg25 = h[1] >> 3 & 3 == 0;
    let br = [
        [
            0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
        ],
        [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
    ][usize::from(lsf)]
    .get(usize::from(h[2] >> 4))
    .copied()?;
    let sr = [44100usize, 48000, 32000].get(usize::from(h[2] >> 2 & 3))?
        >> (usize::from(lsf) + usize::from(mpeg25));
    if br == 0 {
        return None;
    }
    Some((if lsf { 72 } else { 144 }) * br * 1000 / sr + usize::from(h[2] >> 1 & 1))
}
