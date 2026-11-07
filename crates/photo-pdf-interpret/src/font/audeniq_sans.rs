//! AUDENIQ Sans, the default face for PDF text whose font is not embedded.
//!
//! AUDENIQ's own typeface (a modified Pretendard, SIL Open Font License 1.1,
//! `assets/AudeniqSans-OFL.txt`) is what Studio, the apps and the brand use.
//! It covers Latin and every Hangul syllable, so Korean documents whose fonts
//! were not embedded (Batang, Malgun Gothic, ...) render as text instead of
//! nothing. Glyphs are stretched to the PDF's (or the standard font's)
//! widths, so the layout of a page does not change.
//!
//! The Regular and Bold TrueType files (from `audeniq/mobile/assets/fonts`,
//! SHA-256 in `assets/README.md`) are kept zlib-compressed and inflated once,
//! on the first page that needs them.
use super::FontData;
use std::sync::{Arc, OnceLock};

/// Upper bound of an inflated face (the files are about 3.1 MB).
const MAX_FACE: usize = 8 << 20;

fn inflate(compressed: &'static [u8]) -> Option<FontData> {
    let mut out = Vec::new();
    photo_deflate::inflate_zlib(compressed, &mut out, MAX_FACE, false).ok()?;
    Some(Arc::new(out))
}

/// The Regular or Bold face; `None` only if the bundled data is damaged.
pub fn audeniq_sans(bold: bool) -> Option<(FontData, u32)> {
    static REGULAR: OnceLock<Option<FontData>> = OnceLock::new();
    static BOLD: OnceLock<Option<FontData>> = OnceLock::new();
    let face = if bold {
        BOLD.get_or_init(|| inflate(include_bytes!("../../assets/AudeniqSans-Bold.ttf.zlib")))
    } else {
        REGULAR.get_or_init(|| inflate(include_bytes!("../../assets/AudeniqSans-Regular.ttf.zlib")))
    };
    face.clone().map(|data| (data, 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use skrifa::MetadataProvider;

    #[test]
    fn faces_inflate_and_cover_latin_and_hangul() {
        for bold in [false, true] {
            let (data, index) = audeniq_sans(bold).unwrap();
            let font = skrifa::FontRef::from_index(data.as_ref().as_ref(), index).unwrap();
            let cmap = font.charmap();
            for c in "Agreement 2026 계약서 동의 홍길동 ✓".chars() {
                assert!(c == ' ' || cmap.map(c).is_some(), "{c} in bold={bold}");
            }
        }
    }
}
