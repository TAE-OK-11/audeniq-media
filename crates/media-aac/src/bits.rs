//! MSB-first bit reader over one access unit.
use crate::Error;

#[derive(Clone)]
pub(crate) struct Bits<'a> {
    b: &'a [u8],
    pos: usize,
    /// Bit position reading stops at.
    end: usize,
}

impl<'a> Bits<'a> {
    pub(crate) fn new(b: &'a [u8]) -> Self {
        Self {
            b,
            pos: 0,
            end: b.len() * 8,
        }
    }

    /// A reader of the next `n` bits only (an extension payload).
    pub(crate) fn take(&self, n: usize) -> Self {
        Self {
            b: self.b,
            pos: self.pos,
            end: (self.pos + n).min(self.end),
        }
    }

    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    pub(crate) fn left(&self) -> usize {
        self.end.saturating_sub(self.pos)
    }

    pub(crate) fn bit(&mut self) -> Result<u32, Error> {
        if self.pos >= self.end {
            return Err(Error::Truncated);
        }
        let byte = *self.b.get(self.pos / 8).ok_or(Error::Truncated)?;
        let v = byte >> (7 - self.pos % 8) & 1;
        self.pos += 1;
        Ok(u32::from(v))
    }

    /// `n` ≤ 32 bits.
    pub(crate) fn u(&mut self, n: u32) -> Result<u32, Error> {
        if n as usize > self.left() {
            return Err(Error::Truncated);
        }
        let mut v = 0u64;
        for _ in 0..n {
            let byte = self.b[self.pos / 8];
            v = v << 1 | u64::from(byte >> (7 - self.pos % 8) & 1);
            self.pos += 1;
        }
        Ok(v as u32)
    }

    pub(crate) fn skip(&mut self, n: usize) -> Result<(), Error> {
        if n > self.left() {
            return Err(Error::Truncated);
        }
        self.pos += n;
        Ok(())
    }

    pub(crate) fn align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }
}
