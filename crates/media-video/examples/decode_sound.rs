//! Decode an MP4/MOV's sound as it is to f32le on stdout (rate and
//! channels on stderr), for comparing with another decoder.
use std::io::Write;

fn main() {
    let path = std::env::args().nth(1).expect("usage: decode_sound FILE");
    let (rate, names, pcm) = media_video::decode_sound(path.as_ref()).unwrap();
    eprintln!("rate {rate} channels {}", names.len());
    let mut w = std::io::stdout().lock();
    for v in pcm {
        w.write_all(&v.to_le_bytes()).unwrap();
    }
}
