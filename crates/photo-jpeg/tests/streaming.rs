//! Row streaming gives the same pixels and bytes as the whole-image calls.
use photo_core::{Deadline, Image, Limits, PixelFormat};
use photo_jpeg::{Encoder, RowEncoder, Subsampling, decode, decode_rows, encode};

fn image(w: u32, h: u32, format: PixelFormat) -> Image {
    let ch = format.channels();
    let data = (0..w as usize * h as usize * ch)
        .map(|i| ((i * 31 + i / 7 * 3) % 256) as u8)
        .collect();
    Image {
        width: w,
        height: h,
        format,
        data,
    }
}

#[test]
fn row_encoder_matches_encode_for_any_split() {
    for (w, h) in [(1, 1), (9, 17), (16, 16), (33, 47), (100, 31)] {
        for format in [PixelFormat::Gray8, PixelFormat::Rgb8] {
            let img = image(w, h, format);
            let row = w as usize * format.channels();
            for sub in [Subsampling::S444, Subsampling::S420] {
                let whole = encode(&img, 90, sub).unwrap();
                for step in [1, 3, 8, 16, 17] {
                    let mut e = RowEncoder::new(Encoder::new(90, sub), w, h, format).unwrap();
                    for rows in img.data.chunks(step * row) {
                        e.push_rows(rows).unwrap();
                    }
                    assert_eq!(
                        e.finish().unwrap(),
                        whole,
                        "{w}x{h} {format:?} {sub:?} step {step}"
                    );
                }
            }
        }
    }
    // Rows must add up to the height.
    let e = RowEncoder::new(
        Encoder::new(90, Subsampling::S444),
        4,
        4,
        PixelFormat::Gray8,
    )
    .unwrap();
    assert!(e.finish().is_err());
}

#[test]
fn decode_rows_matches_decode() {
    for (w, h) in [(1, 1), (9, 17), (33, 47), (100, 31)] {
        for (format, sub) in [
            (PixelFormat::Gray8, Subsampling::S444),
            (PixelFormat::Rgb8, Subsampling::S444),
            (PixelFormat::Rgb8, Subsampling::S420),
        ] {
            let jpeg = encode(&image(w, h, format), 85, sub).unwrap();
            for cut in [jpeg.len(), jpeg.len() - 2, jpeg.len() * 2 / 3] {
                let data = &jpeg[..cut];
                let whole = decode(data, &Limits::default(), &Deadline::NONE);
                let mut rows = Vec::new();
                let mut next = 0;
                let streamed = decode_rows(data, &Limits::default(), &Deadline::NONE, &mut |r| {
                    assert_eq!(r.y, next);
                    next += (r.data.len() / (r.width as usize * r.format.channels())) as u32;
                    rows.extend_from_slice(r.data);
                });
                match (whole, streamed) {
                    (Ok((a, img)), Ok((b, None))) => {
                        assert_eq!(format!("{a:?}"), format!("{b:?}"));
                        assert_eq!(img.data, rows);
                    }
                    (Err(a), Err(b)) => assert_eq!(format!("{a:?}"), format!("{b:?}")),
                    (a, b) => panic!("{w}x{h} cut {cut}: {:?} vs {:?}", a.err(), b.err()),
                }
            }
        }
    }
}
