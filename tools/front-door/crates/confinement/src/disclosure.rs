//! Secret-pattern scan of an exact in-memory snapshot.
//!
//! Callers use this before sealing a prompt. A match refuses without naming
//! the matching text.
use crate::redact::{Redactor, RedactorError, prose_spans};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DisclosureError {
    #[error("disclosure scan found content matching a secret pattern")]
    SecretMatch,
    #[error("disclosure secret set is invalid")]
    InvalidSecretSet(#[source] RedactorError),
}

/// Scan one exact in-memory snapshot, such as a prompt about to be sealed,
/// for credential tokens and declared secret values. Assignment-like prose is
/// not a credential declaration. A match refuses without naming the text.
pub fn scan_snapshot(bytes: &[u8], known_secrets: &[Vec<u8>]) -> Result<(), DisclosureError> {
    Redactor::new(known_secrets.iter().cloned()).map_err(DisclosureError::InvalidSecretSet)?;
    let spans = prose_spans(bytes, known_secrets).map_err(|_| DisclosureError::SecretMatch)?;
    if spans.is_empty() {
        Ok(())
    } else {
        Err(DisclosureError::SecretMatch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_refuses_known_secrets_without_returning_content() {
        let result = scan_snapshot(b"private token-abc-123", &[b"token-abc-123".to_vec()]);
        assert!(matches!(result, Err(DisclosureError::SecretMatch)));
    }

    // Spec: docs/specs/fabric.md#checkpoint-envelope
    #[test]
    fn prompt_sealing_preserves_prose_but_still_refuses_credential_tokens() {
        for text in ["the tests pass:", "pass: run", "password: run", "token=abc"] {
            assert!(scan_snapshot(text.as_bytes(), &[]).is_ok());
        }
        assert!(scan_snapshot(b"AKIAIOSFODNN7EXAMPLE", &[]).is_err());
    }

    #[test]
    fn snapshot_allows_ordinary_urls() {
        let text = b"bearer values; Bearer values; bare sk-; Akia1234567890123456; read ssh://git@host and https://example.invalid/docs";
        assert!(scan_snapshot(text, &[b"secret-123".to_vec()]).is_ok());
    }
}
