//! Secret-pattern scan of an exact in-memory snapshot.
//!
//! Callers use this before sealing a prompt. A match refuses without naming
//! the matching text.
use crate::redact::{Redactor, RedactorError};
use provenant_os::exec::CaptureFilter;
use thiserror::Error;

const BUFFER_SIZE: usize = 16 * 1024;

#[derive(Debug, Error)]
pub enum DisclosureError {
    #[error("disclosure scan found content matching a secret pattern")]
    SecretMatch,
    #[error("disclosure secret set is invalid")]
    InvalidSecretSet(#[source] RedactorError),
}

/// Scan one exact in-memory snapshot, such as a prompt about to be sealed,
/// with the same secret patterns. A match refuses without naming the text.
pub fn scan_snapshot(bytes: &[u8], known_secrets: &[Vec<u8>]) -> Result<(), DisclosureError> {
    let mut redactor =
        Redactor::new(known_secrets.iter().cloned()).map_err(DisclosureError::InvalidSecretSet)?;
    for chunk in bytes.chunks(BUFFER_SIZE) {
        redactor
            .filter(chunk)
            .map_err(|_| DisclosureError::SecretMatch)?;
        if redactor.has_redaction() {
            return Err(DisclosureError::SecretMatch);
        }
    }
    redactor
        .finish()
        .map_err(|_| DisclosureError::SecretMatch)?;
    if redactor.has_redaction() {
        return Err(DisclosureError::SecretMatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_refuses_known_secrets_without_returning_content() {
        let result = scan_snapshot(b"private token-abc-123", &[b"token-abc-123".to_vec()]);
        assert!(matches!(result, Err(DisclosureError::SecretMatch)));
    }

    #[test]
    fn snapshot_allows_ordinary_urls() {
        let text = b"bearer values; Bearer values; bare sk-; Akia1234567890123456; read ssh://git@host and https://example.invalid/docs";
        assert!(scan_snapshot(text, &[b"secret-123".to_vec()]).is_ok());
    }
}
