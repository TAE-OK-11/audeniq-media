#![deny(unsafe_code)]
//! CRC-32 from audeniq-qc and Adler-32 from the merged zlib-rs kernels.

// Lazily initialize dispatch to preserve the const constructor and Copy API.
#[derive(Debug, Clone, Copy, Default)]
pub struct Crc32(Option<media_hash::Crc32>);

impl Crc32 {
    pub const fn new() -> Self {
        Self(None)
    }
    pub fn update(&mut self, data: &[u8]) {
        self.0
            .get_or_insert_with(media_hash::Crc32::new)
            .update(data);
    }
    pub fn finish(&self) -> u32 {
        self.0.as_ref().map_or(0, media_hash::Crc32::finalize)
    }
}

pub fn crc32(data: &[u8]) -> u32 {
    media_hash::crc32(data)
}

#[derive(Debug, Clone, Copy)]
pub struct Adler32(u32);

impl Default for Adler32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Adler32 {
    pub const fn new() -> Self {
        Adler32(1)
    }
    pub fn update(&mut self, data: &[u8]) {
        self.0 = crate::engine::adler32::adler32(self.0, data);
    }
    pub fn finish(&self) -> u32 {
        self.0
    }
}

pub fn adler32(data: &[u8]) -> u32 {
    crate::engine::adler32::adler32(1, data)
}

/// Adler-32 of `A ++ B` from the checksums of `A` and `B` (`len_b` bytes).
pub fn adler32_combine(adler_a: u32, adler_b: u32, len_b: u64) -> u32 {
    crate::engine::adler32::adler32_combine(adler_a, adler_b, len_b)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn crc_const_copy_and_fragmented_png_chunks() {
        const EMPTY: Crc32 = Crc32::new();
        assert_eq!(EMPTY.finish(), 0);
        let data: Vec<u8> = (0..65_536).map(|i| (i * 37) as u8).collect();
        let mut crc = EMPTY;
        crc.update(b"IDAT");
        let prefix = crc;
        for chunk in data.chunks(7) {
            crc.update(chunk);
        }
        let mut reference = crc32fast::Hasher::new();
        reference.update(b"IDAT");
        reference.update(&data);
        assert_eq!(crc.finish(), reference.finalize());
        let mut copy = prefix;
        copy.update(&data);
        assert_eq!(copy.finish(), crc.finish());
        assert_eq!(prefix.finish(), crc32(b"IDAT"));
        assert_eq!(Crc32::default().finish(), 0);
    }

    #[test]
    fn known_vectors() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(
            crc32(b"The quick brown fox jumps over the lazy dog"),
            0x414F_A339
        );
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        let big = vec![0xFFu8; 100_000];
        let mut split = Adler32::new();
        split.update(&big[..33_333]);
        split.update(&big[33_333..]);
        assert_eq!(split.finish(), adler32(&big));
        let mut c = Crc32::new();
        c.update(&big[..7]);
        c.update(&big[7..]);
        assert_eq!(c.finish(), crc32(&big));
    }
}
