//! Prefix-code decoding (AAC, SBR, MP3): one table lookup for codewords of
//! up to [`FAST`] bits, a binary tree for longer ones.
use crate::{Bits, Error};

/// Bits looked up at once.
const FAST: u32 = 9;

/// A `fast` entry that continues in the tree at node `entry & !CONTINUE`.
const CONTINUE: u32 = 1 << 31;

/// Nodes are `[zero, one]`: `>= 0` another node, `< 0` the symbol `-v - 1`.
/// `fast[p]` for the next `FAST` bits `p` is a codeword of up to `FAST`
/// bits as `len << 24 | symbol`, the node `FAST` bits down as
/// `CONTINUE | node`, or 0 for no codeword.
pub struct Huffman {
    nodes: Vec<[i32; 2]>,
    fast: Vec<u32>,
}

impl Huffman {
    /// Symbol `i` has the `lens[i]`-bit codeword `codes[i]`.
    pub fn new<C: Copy + Into<u32>>(codes: &[C], lens: &[u8]) -> Huffman {
        Huffman::from_codes(
            codes
                .iter()
                .zip(lens)
                .enumerate()
                .map(|(sym, (&code, &len))| (code.into(), u32::from(len), sym as u32)),
        )
    }

    /// From `(codeword, length, symbol)`; symbols are below 2²⁴.
    pub fn from_codes(codes: impl IntoIterator<Item = (u32, u32, u32)>) -> Huffman {
        let mut nodes = vec![[0i32; 2]];
        for (code, len, sym) in codes {
            assert!(sym < 1 << 24 && (1..=32).contains(&len));
            let mut at = 0usize;
            for i in (0..len).rev() {
                let bit = (code >> i & 1) as usize;
                if i == 0 {
                    nodes[at][bit] = -(sym as i32) - 1;
                } else {
                    if nodes[at][bit] == 0 {
                        nodes.push([0, 0]);
                        nodes[at][bit] = (nodes.len() - 1) as i32;
                    }
                    at = nodes[at][bit] as usize;
                }
            }
        }
        let fast = (0..1u32 << FAST)
            .map(|p| {
                let mut at = 0usize;
                for i in (0..FAST).rev() {
                    let next = nodes[at][(p >> i & 1) as usize];
                    if next < 0 {
                        return (FAST - i) << 24 | (-next - 1) as u32;
                    }
                    if next == 0 {
                        return 0;
                    }
                    at = next as usize;
                }
                CONTINUE | at as u32
            })
            .collect();
        Huffman { nodes, fast }
    }

    /// The next symbol.
    #[inline(always)]
    pub fn decode(&self, b: &mut Bits) -> Result<u32, Error> {
        if b.left() < FAST as usize {
            return self.walk(b, 0);
        }
        let e = self.fast[b.peek(FAST)? as usize];
        if e & CONTINUE != 0 {
            b.advance(FAST);
            return self.walk(b, (e & !CONTINUE) as usize);
        }
        if e == 0 {
            return Err(Error::Invalid("Huffman codeword"));
        }
        b.advance(e >> 24);
        Ok(e & 0xff_ffff)
    }

    #[cold]
    #[inline(never)]
    fn walk(&self, b: &mut Bits, mut at: usize) -> Result<u32, Error> {
        loop {
            let next = self.nodes[at][b.bit()? as usize];
            if next < 0 {
                return Ok((-next - 1) as u32);
            }
            if next == 0 {
                return Err(Error::Invalid("Huffman codeword"));
            }
            at = next as usize;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_a_prefix_code() {
        // a=0, b=10, c=11
        let t = Huffman::new(&[0u16, 2, 3], &[1, 2, 2]);
        let mut b = Bits::new(&[0b0101_1000]);
        let got: Vec<_> = (0..4).map(|_| t.decode(&mut b).unwrap()).collect();
        assert_eq!(got, [0, 1, 2, 0]);
    }

    #[test]
    fn long_codewords_continue_in_the_tree() {
        // a=0, b=10, c=1111111111 (10 bits), d=1111111110, e=111111110
        let t = Huffman::new(&[0u16, 2, 0x3ff, 0x3fe, 0x1fe], &[1, 2, 10, 10, 9]);
        // c d e b a b, then padding.
        let bits = "11111111111111111110111111110100101";
        let mut bytes = vec![0u8; bits.len().div_ceil(8) + 2];
        for (i, c) in bits.bytes().enumerate() {
            bytes[i / 8] |= (c - b'0') << (7 - i % 8);
        }
        let mut b = Bits::new(&bytes);
        let got: Vec<_> = (0..6).map(|_| t.decode(&mut b).unwrap()).collect();
        assert_eq!(got, [2, 3, 4, 1, 0, 1]);
    }

    #[test]
    fn codewords_past_the_limit_are_eof() {
        let t = Huffman::new(&[0u16, 2, 3], &[1, 2, 2]);
        let mut b = Bits::new(&[0b1100_0000]).take(1);
        assert_eq!(t.decode(&mut b), Err(Error::Eof));
        let mut b = Bits::new(&[0b0110_0000, 0, 0]).take(2);
        assert_eq!(t.decode(&mut b), Ok(0));
        assert_eq!(t.decode(&mut b), Err(Error::Eof));
    }
}
