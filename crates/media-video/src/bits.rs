//! Bit reader for codec parameter sets (H.264/HEVC SPS).

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

/// MSB-first reader; every read is `None` past the end.
pub(crate) struct Bits<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    pub(crate) fn new(b: &'a [u8]) -> Self {
        Self { b, pos: 0 }
    }

    pub(crate) fn flag(&mut self) -> Option<bool> {
        let byte = *self.b.get(self.pos / 8)?;
        let bit = byte >> (7 - self.pos % 8) & 1;
        self.pos += 1;
        Some(bit == 1)
    }

    /// `n` ≤ 32 bits.
    pub(crate) fn u(&mut self, n: u32) -> Option<u32> {
        let mut v = 0u64;
        for _ in 0..n {
            v = v << 1 | u64::from(self.flag()?);
        }
        u32::try_from(v).ok()
    }

    pub(crate) fn skip(&mut self, n: usize) -> Option<()> {
        let end = self.pos.checked_add(n)?;
        (end <= self.b.len() * 8).then(|| self.pos = end)
    }

    /// Exp-Golomb `ue(v)`; longer than 32 bits is malformed.
    pub(crate) fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while !self.flag()? {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        let rest = self.u(zeros)?;
        ((1u64 << zeros) - 1 + u64::from(rest)).try_into().ok()
    }

    /// Exp-Golomb `se(v)`.
    pub(crate) fn se(&mut self) -> Option<i32> {
        let k = i64::from(self.ue()?);
        let v = if k % 2 == 1 { (k + 1) / 2 } else { -(k / 2) };
        i32::try_from(v).ok()
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
