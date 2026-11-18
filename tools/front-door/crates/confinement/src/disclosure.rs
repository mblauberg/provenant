//! Secret-pattern scan of an exact in-memory snapshot.
//!
//! Reports matches without exposing matched bytes. Shape matches are advisory;
//! callers decide how to warn or mask at their boundary.
use provenant_os::exec::CaptureFilter;
use provenant_redact::{Redactor, RedactorError};
use thiserror::Error;

const BUFFER_SIZE: usize = 16 * 1024;

#[derive(Debug, Error)]
pub enum DisclosureError {
    #[error("disclosure scan found content matching a secret pattern")]
    SecretMatch,
    #[error("disclosure secret set is invalid")]
    InvalidSecretSet(#[source] RedactorError),
}

/// Scan one exact snapshot for credential patterns and supplied literal values.
/// A match returns `SecretMatch` without including any matched bytes.
pub fn scan_snapshot(bytes: &[u8], known_secrets: &[Vec<u8>]) -> Result<(), DisclosureError> {
    let mut redactor =
        Redactor::new(known_secrets.iter().cloned()).map_err(DisclosureError::InvalidSecretSet)?;
    for chunk in bytes.chunks(BUFFER_SIZE) {
        redactor
            .filter(chunk, &mut |_| {})
            .map_err(|_| DisclosureError::SecretMatch)?;
        if redactor.has_redaction() {
            return Err(DisclosureError::SecretMatch);
        }
    }
    redactor
        .finish(&mut |_| {})
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
    fn snapshot_reports_known_secrets_without_returning_content() {
        let result = scan_snapshot(b"private token-abc-123", &[b"token-abc-123".to_vec()]);
        assert!(matches!(result, Err(DisclosureError::SecretMatch)));
    }

    #[test]
    fn snapshot_allows_ordinary_urls() {
        let text = b"bearer values; Bearer values; bare sk-; Akia1234567890123456; read ssh://git@host and https://example.invalid/docs";
        assert!(scan_snapshot(text, &[b"secret-123".to_vec()]).is_ok());
    }
}
