//! LSB-first bit reader (TTA, WavPack).
use crate::Error;

/// LSB-first bit reader (TTA, WavPack): a 64-bit cache refilled with one unaligned 8-byte
/// load while eight bytes remain, byte by byte near the end.
pub struct Lsb<'a> {
    data: &'a [u8],
    next: usize,
    pub cache: u64,
    pub available: u32,
}
impl<'a> Lsb<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            next: 0,
            cache: 0,
            available: 0,
        }
    }
    #[inline(always)]
    pub fn refill(&mut self) {
        if let Some(word) = self.data.get(self.next..self.next + 8) {
            self.cache |= u64::from_le_bytes(word.try_into().unwrap()) << self.available;
            let bytes = (63 - self.available) >> 3;
            self.next += bytes as usize;
            self.available += bytes * 8;
        } else {
            while self.available <= 56 && self.next < self.data.len() {
                self.cache |= (self.data[self.next] as u64) << self.available;
                self.next += 1;
                self.available += 8;
            }
        }
    }
    /// `n` (at most 32) bits.
    #[inline(always)]
    pub fn read(&mut self, n: u32) -> Result<u32, Error> {
        if self.available < n {
            self.refill();
            if self.available < n {
                return Err(Error::Invalid("truncated bitstream"));
            }
        }
        let value = (self.cache & ((1u64 << n) - 1)) as u32;
        self.cache >>= n;
        self.available -= n;
        Ok(value)
    }
    /// A run of one bits ended by a zero (consumed); fewer than `max` ones.
    #[inline(always)]
    pub fn ones(&mut self, max: u32) -> Result<u32, Error> {
        let mut n = 0;
        loop {
            if self.available < 57 {
                self.refill();
                if self.available == 0 {
                    return Err(Error::Invalid("truncated bitstream"));
                }
            }
            let ones = self.cache.trailing_ones().min(self.available);
            if ones != 0 && ones >= max.saturating_sub(n) {
                return Err(Error::Invalid("unbounded unary code"));
            }
            n += ones;
            if ones < self.available {
                // The byte-wise refill can fill all 64 bits.
                self.cache = self.cache.checked_shr(ones + 1).unwrap_or(0);
                self.available -= ones + 1;
                return Ok(n);
            }
            self.cache = 0;
            self.available = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cached_reader_matches_bit_reference_and_bounds() {
        for length in 0..=97 {
            let data: Vec<u8> = (0..length).map(|i| (i * 131 + 197) as u8).collect();
            let mut reader = Lsb::new(&data);
            let mut position = 0;
            for n in (0..=32).cycle().take(150) {
                let expected = if position + n as usize <= length * 8 {
                    let mut value = 0u32;
                    for bit in 0..n {
                        value |= (((data[(position + bit as usize) / 8]
                            >> ((position + bit as usize) % 8))
                            & 1) as u32)
                            << bit;
                    }
                    Some(value)
                } else {
                    None
                };
                let actual = reader.read(n).ok();
                assert_eq!(actual, expected);
                if actual.is_none() {
                    break;
                }
                position += n as usize;
            }
        }
        for ones in 0..=129 {
            let mut data = vec![0u8; (ones + 1usize).div_ceil(8)];
            for i in 0..ones {
                data[i / 8] |= 1 << (i % 8);
            }
            assert_eq!(Lsb::new(&data).ones(ones as u32 + 1).unwrap(), ones as u32);
            if ones > 0 {
                assert!(Lsb::new(&data).ones(ones as u32).is_err());
            }
        }
        assert!(Lsb::new(&[255; 8]).ones(256).is_err());
    }
}
