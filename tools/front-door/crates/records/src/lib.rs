//! Versioned records, canonical encoders and digest contracts.
#![forbid(unsafe_code)]

pub mod attempt_evidence;
pub mod attempt_storage;
mod pending;

/// SHA-256 over the exact retained bytes, including any final newline.
pub fn digest_bytes(value: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!(
        "sha256:{}",
        Sha256::digest(value)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}
