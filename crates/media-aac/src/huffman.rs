//! Binary decoding trees for the AAC codebooks.
use crate::{Error, bits::Bits};

/// Bits looked up at once; longer codewords continue in the tree.
const FAST: u32 = 9;

/// Nodes: `[zero, one]`; `>= 0` another node, `< 0` the symbol `-v - 1`.
/// `fast[p]` for the next `FAST` bits `p` is a codeword of up to `FAST`
/// bits as `len << 24 | symbol`, the node `FAST` bits down as
/// `CONTINUE | node`, or 0 for no codeword.
pub(crate) struct Tree {
    nodes: Vec<[i32; 2]>,
    fast: Vec<u32>,
}

const CONTINUE: u32 = 1 << 31;

impl Tree {
    pub(crate) fn new<C: Copy + Into<u32>>(codes: &[C], lens: &[u8]) -> Tree {
        let mut nodes = vec![[0i32; 2]];
        for (sym, (&code, &len)) in codes.iter().zip(lens).enumerate() {
            let code: u32 = code.into();
            let mut at = 0usize;
            for i in (0..u32::from(len)).rev() {
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
        Tree { nodes, fast }
    }

    #[inline]
    pub(crate) fn decode(&self, b: &mut Bits) -> Result<usize, Error> {
        if b.left() < FAST as usize {
            return self.walk(b, 0);
        }
        let e = self.fast[b.peek(FAST) as usize];
        if e & CONTINUE != 0 {
            b.skip(FAST as usize)?;
            return self.walk(b, (e & !CONTINUE) as usize);
        }
        if e == 0 {
            return Err(Error::Invalid("Huffman codeword"));
        }
        b.skip((e >> 24) as usize)?;
        Ok((e & 0xff_ffff) as usize)
    }

    fn walk(&self, b: &mut Bits, mut at: usize) -> Result<usize, Error> {
        loop {
            let next = self.nodes[at][b.bit()? as usize];
            if next < 0 {
                return Ok((-next - 1) as usize);
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
        let t = Tree::new(&[0u16, 2, 3], &[1, 2, 2]);
        let mut b = Bits::new(&[0b0101_1000]);
        let got: Vec<_> = (0..4).map(|_| t.decode(&mut b).unwrap()).collect();
        assert_eq!(got, [0, 1, 2, 0]);
    }

    #[test]
    fn long_codewords_continue_in_the_tree() {
        // a=0, b=10, c=1111111111 (10 bits), d=1111111110, e=111111110
        let t = Tree::new(&[0u16, 2, 0x3ff, 0x3fe, 0x1fe], &[1, 2, 10, 10, 9]);
        // c d e b a b, then padding: 1111111111 1111111110 111111110 10 0 10
        let bits = "11111111111111111110111111110100101";
        let mut bytes = vec![0u8; bits.len().div_ceil(8) + 2];
        for (i, c) in bits.bytes().enumerate() {
            bytes[i / 8] |= (c - b'0') << (7 - i % 8);
        }
        let mut b = Bits::new(&bytes);
        let got: Vec<_> = (0..6).map(|_| t.decode(&mut b).unwrap()).collect();
        assert_eq!(got, [2, 3, 4, 1, 0, 1]);
    }
}
