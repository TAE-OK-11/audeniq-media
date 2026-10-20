//! Exercise QC CRC through PNG encoding and validation, independently of its tables.
use photo_core::{Deadline, Image, Limits, PixelFormat};
use photo_deflate::Level;
use photo_png::{ChunkIter, decode, encode};

fn bitwise_crc(kind: &[u8], data: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in kind.iter().chain(data) {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0_u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

fn image(width: u32, height: u32, format: PixelFormat) -> Image {
    let mut seed = 0x9e37_79b9_u32;
    let data = (0..width as usize * height as usize * format.channels())
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as u8
        })
        .collect();
    Image {
        width,
        height,
        format,
        data,
    }
}

#[test]
fn encoded_chunks_match_independent_crc_and_pixels_roundtrip() {
    for format in [
        PixelFormat::Gray8,
        PixelFormat::GrayAlpha8,
        PixelFormat::Rgb8,
        PixelFormat::Rgba8,
    ] {
        for level in [Level::STORE, Level::DEFAULT] {
            let original = image(257, 129, format);
            let png = encode(&original, level).unwrap();
            let mut idats = 0;
            for chunk in ChunkIter::new(&png).unwrap() {
                let chunk = chunk.unwrap();
                assert!(matches!(&chunk.kind, b"IHDR" | b"IDAT" | b"IEND"));
                idats += usize::from(chunk.kind == *b"IDAT");
                let offset = chunk.offset + 8 + chunk.data.len();
                let stored = u32::from_be_bytes(png[offset..offset + 4].try_into().unwrap());
                assert_eq!(stored, bitwise_crc(&chunk.kind, chunk.data));
            }
            if format == PixelFormat::Rgba8 {
                assert!(idats > 1);
            }
            let (_, decoded) = decode(&png, &Limits::default(), &Deadline::NONE).unwrap();
            assert_eq!(decoded, original);
        }
    }
}

#[test]
fn invalid_idat_crc_is_rejected_before_decode() {
    let mut png = encode(&image(17, 13, PixelFormat::Rgb8), Level::DEFAULT).unwrap();
    let offset = ChunkIter::new(&png)
        .unwrap()
        .map(Result::unwrap)
        .find(|chunk| chunk.kind == *b"IDAT")
        .map(|chunk| chunk.offset + 8 + chunk.data.len())
        .unwrap();
    png[offset] ^= 1;
    assert!(decode(&png, &Limits::default(), &Deadline::NONE).is_err());
    assert!(ChunkIter::new(&png).unwrap().any(|chunk| chunk.is_err()));
}
