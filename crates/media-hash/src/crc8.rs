//! FLAC frame-header CRC-8 (polynomial x^8 + x^2 + x + 1, MSB first,
//! initial value 0).

const fn table() -> [u8; 256] {
    let mut t = [0; 256];
    let mut i = 0;
    while i < 256 {
        let mut a = i as u8;
        let mut j = 0;
        while j < 8 {
            a = (a << 1) ^ if a & 0x80 != 0 { 7 } else { 0 };
            j += 1;
        }
        t[i] = a;
        i += 1;
    }
    t
}
static TABLE: [u8; 256] = table();

/// CRC-8 of `data`.
pub fn crc8(data: &[u8]) -> u8 {
    data.iter().fold(0, |crc, b| TABLE[(crc ^ b) as usize])
}

#[cfg(test)]
mod tests {
    #[test]
    fn check_value() {
        // CRC-8 (poly 0x07, init 0) of "123456789".
        assert_eq!(super::crc8(b"123456789"), 0xf4);
        assert_eq!(super::crc8(b""), 0);
    }
}
