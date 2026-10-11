//! PDF-compatible Md5 using the implementation developed in audeniq-qc.

pub(crate) fn calculate(data: &[u8]) -> [u8; 16] {
    let mut hash = photo_hash::Md5::new();
    hash.update(data);
    hash.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn md5_test(data: &[u8]) {
        let our_result = calculate(data);

        let external_result = md5::compute(data);

        assert_eq!(
            our_result,
            external_result.as_slice(),
            "MD5 calculation should match external md5 crate for input: {data:?}"
        );
    }

    #[test]
    fn correctness() {
        md5_test(b"Hello, World!");
        md5_test(b"The quick brown fox jumps over the lazy dog");
        md5_test(b"abcdefghijklmnopqrstuvwxyz");
    }
}
