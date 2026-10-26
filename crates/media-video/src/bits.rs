//! Bit reading for codec parameter sets (H.264/HEVC SPS).

/// RBSP of a NAL unit: the payload with emulation-prevention bytes
/// (`00 00 03`) removed.
pub(crate) fn unescape(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &b in nal {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

/// The shared MSB reader (media-bits) with every read `None` past the end
/// or on a malformed code, as the parameter-set parsers here use it.
pub(crate) struct Bits<'a>(media_bits::Bits<'a>);

impl<'a> Bits<'a> {
    pub(crate) fn new(b: &'a [u8]) -> Self {
        Self(media_bits::Bits::new(b))
    }

    pub(crate) fn flag(&mut self) -> Option<bool> {
        self.0.flag().ok()
    }

    /// `n` ≤ 32 bits.
    pub(crate) fn u(&mut self, n: u32) -> Option<u32> {
        self.0.get(n).ok()
    }

    pub(crate) fn skip(&mut self, n: usize) -> Option<()> {
        self.0.skip(n).ok()
    }

    /// Exp-Golomb `ue(v)`; longer than 32 bits is malformed.
    pub(crate) fn ue(&mut self) -> Option<u32> {
        self.0.ue().ok()
    }

    /// Exp-Golomb `se(v)`.
    pub(crate) fn se(&mut self) -> Option<i32> {
        self.0.se().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exp_golomb() {
        // 1 | 010 | 011 | 00100 | 00101
        let mut b = Bits::new(&[0b1010_0110, 0b0100_0010, 0b1000_0000]);
        assert_eq!(b.ue(), Some(0));
        assert_eq!(b.ue(), Some(1));
        assert_eq!(b.se(), Some(-1));
        assert_eq!(b.ue(), Some(3));
        assert_eq!(b.se(), Some(-2));
        assert_eq!(b.u(8), None);
    }

    #[test]
    fn removes_emulation_prevention() {
        assert_eq!(
            unescape(&[0, 0, 3, 1, 0, 0, 3, 0, 3]),
            [0, 0, 1, 0, 0, 0, 3]
        );
    }
}
