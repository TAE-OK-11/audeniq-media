//! Binary decoding trees for the AAC codebooks.
use crate::{Error, bits::Bits};

/// Nodes: `[zero, one]`; `>= 0` another node, `< 0` the symbol `-v - 1`.
pub(crate) struct Tree(Vec<[i32; 2]>);

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
        Tree(nodes)
    }

    pub(crate) fn decode(&self, b: &mut Bits) -> Result<usize, Error> {
        let mut at = 0usize;
        loop {
            let next = self.0[at][b.bit()? as usize];
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
}
