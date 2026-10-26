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

    /// The next `n` ≤ 32 bits without consuming them; bits past the data
    /// read as zero, and those past `end` are whatever the data holds.
    #[inline]
    pub(crate) fn peek(&self, n: u32) -> u32 {
        debug_assert!((1..=32).contains(&n));
        let i = self.pos / 8;
        let w = match self.b.get(i..i + 8) {
            Some(w) => u64::from_be_bytes(w.try_into().unwrap()),
            None => {
                let mut w = [0u8; 8];
                let rest = self.b.get(i..).unwrap_or(&[]);
                w[..rest.len()].copy_from_slice(rest);
                u64::from_be_bytes(w)
            }
        };
        ((w << (self.pos % 8)) >> (64 - n)) as u32
    }

    /// `n` ≤ 32 bits.
    #[inline]
    pub(crate) fn u(&mut self, n: u32) -> Result<u32, Error> {
        if n as usize > self.left() {
            return Err(Error::Truncated);
        }
        if n == 0 {
            return Ok(0);
        }
        let v = self.peek(n);
        self.pos += n as usize;
        Ok(v)
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
