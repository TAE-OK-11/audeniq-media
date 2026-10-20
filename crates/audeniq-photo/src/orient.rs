//! EXIF orientation (Pillow `ImageOps.exif_transpose` semantics).

use photo_core::Image;

/// Apply orientation 2..=8; other values leave the image unchanged.
pub(crate) fn apply(img: Image, orientation: i64) -> Image {
    if !(2..=8).contains(&orientation) {
        return img;
    }
    let (w, h) = (img.width as usize, img.height as usize);
    let ch = img.format.channels();
    let swap = orientation >= 5;
    let (ow, oh) = if swap { (h, w) } else { (w, h) };
    // Source pixel of output (x, y) is base + x * dx + y * dy, per Pillow
    // transposes: 2 FLIP_LEFT_RIGHT, 3 ROTATE_180, 4 FLIP_TOP_BOTTOM,
    // 5 TRANSPOSE, 6 ROTATE_270, 7 TRANSVERSE, 8 ROTATE_90.
    let (wi, hi) = (w as isize, h as isize);
    let (base, dx, dy) = match orientation {
        2 => (wi - 1, -1, wi),
        3 => (hi * wi - 1, -1, -wi),
        4 => ((hi - 1) * wi, 1, -wi),
        5 => (0, wi, 1),
        6 => ((hi - 1) * wi, -wi, 1),
        7 => (hi * wi - 1, -wi, -1),
        _ => (wi - 1, wi, -1),
    };
    let mut out = vec![0u8; img.data.len()];
    let walk = Walk { ow, base, dx, dy };
    // Constant channel counts for the common formats (inlined copies).
    match ch {
        1 => walk.copy(1, &img.data, &mut out),
        3 => walk.copy(3, &img.data, &mut out),
        4 => walk.copy(4, &img.data, &mut out),
        _ => walk.copy(ch, &img.data, &mut out),
    }
    Image {
        width: ow as u32,
        height: oh as u32,
        format: img.format,
        data: out,
    }
}

struct Walk {
    ow: usize,
    base: isize,
    dx: isize,
    dy: isize,
}

impl Walk {
    #[inline(always)]
    fn copy(&self, ch: usize, src: &[u8], out: &mut [u8]) {
        if self.ow == 0 {
            return;
        }
        for (y, row) in out.chunks_exact_mut(self.ow * ch).enumerate() {
            let mut s = self.base + y as isize * self.dy;
            for px in row.chunks_exact_mut(ch) {
                let at = s as usize * ch;
                px.copy_from_slice(&src[at..at + ch]);
                s += self.dx;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::apply;
    use photo_core::{Image, PixelFormat};

    /// The per-pixel definition this module used to compute directly.
    fn reference(img: &Image, orientation: i64) -> Vec<u8> {
        let (w, h) = (img.width as usize, img.height as usize);
        let ch = img.format.channels();
        let ow = if orientation >= 5 { h } else { w };
        let oh = if orientation >= 5 { w } else { h };
        let mut out = vec![0u8; img.data.len()];
        for y in 0..oh {
            for x in 0..ow {
                let (sx, sy) = match orientation {
                    2 => (w - 1 - x, y),
                    3 => (w - 1 - x, h - 1 - y),
                    4 => (x, h - 1 - y),
                    5 => (y, x),
                    6 => (y, h - 1 - x),
                    7 => (w - 1 - y, h - 1 - x),
                    _ => (w - 1 - y, x),
                };
                let s = (sy * w + sx) * ch;
                let d = (y * ow + x) * ch;
                out[d..d + ch].copy_from_slice(&img.data[s..s + ch]);
            }
        }
        out
    }

    #[test]
    fn matches_per_pixel_definition() {
        for format in [
            PixelFormat::Gray8,
            PixelFormat::GrayAlpha8,
            PixelFormat::Rgb8,
            PixelFormat::Rgba8,
        ] {
            for (w, h) in [(1, 1), (1, 5), (5, 1), (3, 7), (8, 8), (13, 4)] {
                let n = w * h * format.channels();
                let data: Vec<u8> = (0..n).map(|i| (i * 31 % 251) as u8).collect();
                let img = Image {
                    width: w as u32,
                    height: h as u32,
                    format,
                    data,
                };
                for o in 2..=8 {
                    let got = apply(img.clone(), o);
                    assert_eq!(got.data, reference(&img, o), "{format:?} {w}x{h} o{o}");
                    let swap = o >= 5;
                    assert_eq!(got.width as usize, if swap { h } else { w });
                }
            }
        }
    }
}
