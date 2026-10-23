//! Versioned records, canonical encoders and digest contracts.
#![forbid(unsafe_code)]

pub mod attempt_evidence;
pub mod attempt_storage;
pub mod checked_json;
pub mod delivery_run_shape;
#[cfg(test)]
#[path = "delivery_run_shape/tests.rs"]
mod delivery_run_shape_tests;
pub mod numeric;
pub mod ordered_json;
pub mod python_json;
pub use checked_json::{DecodeError, NumberOutOfRange};
pub mod fabric_records;
pub use fabric_records::Field;
pub mod layout;
mod pending;

/// Lower-case hexadecimal bytes, without a digest prefix.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// SHA-256 hex over exact bytes, for source identity contracts without a prefix.
pub fn sha256_hex(value: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex(&Sha256::digest(value))
}

/// SHA-256 over the exact retained bytes, including any final newline.
pub fn digest_bytes(value: &[u8]) -> String {
    format!("sha256:{}", sha256_hex(value))
}

#[cfg(test)]
mod oracle;
