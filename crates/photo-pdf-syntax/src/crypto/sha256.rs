//! PDF-compatible Sha256 using the implementation developed in audeniq-qc.

pub(crate) fn calculate(data: &[u8]) -> [u8; 32] {
    let mut hash = media_hash::Sha256::new();
    hash.update(data);
    hash.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn correctness() {
        let test_cases = [
            b"" as &[u8],
            b"a",
            b"abc",
            b"Hello, World!",
            &[0xde, 0xad, 0xbe, 0xef],
        ];

        for test_case in test_cases {
            let our_result = calculate(test_case);
            let expected = Sha256::digest(test_case);
            assert_eq!(
                our_result,
                expected.as_slice(),
                "Failed for input: {test_case:?}"
            );
        }
    }
}
