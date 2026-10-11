//! Hash/checksum primitives reused from audeniq-qc; see SOURCE.md.
//! `std` enables runtime CPU dispatch. Without it, portable kernels are used.
#![cfg_attr(not(feature = "std"), no_std)]
#![deny(unsafe_code)]

#[cfg(test)]
extern crate std;

#[allow(unsafe_code)]
mod crc32;
mod md5;
#[allow(unsafe_code)]
mod sha256;

pub use crc32::{crc32, Crc32};
pub use md5::Md5;
pub use sha256::Sha256;

#[cfg(test)]
fn hex(bytes: &[u8]) -> std::string::String {
    use core::fmt::Write;
    let mut result = std::string::String::new();
    for byte in bytes {
        write!(&mut result, "{byte:02x}").unwrap();
    }
    result
}
