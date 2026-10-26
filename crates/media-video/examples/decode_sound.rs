//! Decode an MP4/MOV's sound as it is to f32le on stdout (rate and
//! channels on stderr), for comparing with another decoder; `--null`
//! decodes without writing (timing).
use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let null = args.iter().any(|a| a == "--null");
    let path = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .expect("usage: decode_sound [--null] FILE");
    let mut out = std::io::BufWriter::with_capacity(1 << 20, std::io::stdout().lock());
    let mut bytes = Vec::new();
    let (rate, names) = media_video::decode_sound_blocks(path.as_ref(), &mut |b| {
        if !null {
            bytes.clear();
            for v in b {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            out.write_all(&bytes).unwrap();
        }
    })
    .unwrap();
    out.flush().unwrap();
    eprintln!("rate {rate} channels {}", names.len());
}
