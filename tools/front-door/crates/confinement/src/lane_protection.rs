//! Lane gate requests and review records. Graph walking and release I/O are a later unit.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ProtectionError {
    #[error("protected_instructions_unverifiable: {0}")]
    Unverifiable(String),
    #[error("protected_instructions_changed: {0}")]
    Changed(String),
}

impl ProtectionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unverifiable(_) => "protected_instructions_unverifiable",
            Self::Changed(_) => "protected_instructions_changed",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckRequest {
    pub common: PathBuf,
    pub tip: String,
    pub base: String,
}

/// A release is an audit record. Only the owner's write boundary confers authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReleaseRecord {
    pub tip: String,
    pub actor: String,
    pub reason: String,
    pub worktree: PathBuf,
    /// Supplied by the clock owner, preserving UTC ISO spelling and fractional precision.
    pub released_at: String,
}

pub fn is_full_sha(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

pub fn validate_revision(rev: &str) -> Result<(), ProtectionError> {
    if rev.starts_with('-') {
        return Err(ProtectionError::Unverifiable(format!(
            "invalid revision {}",
            python_repr(rev)
        )));
    }
    Ok(())
}

fn python_repr(value: &str) -> String {
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut result = String::from(quote);
    for c in value.chars() {
        match c {
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            c if c == quote => {
                result.push('\\');
                result.push(c);
            }
            c if c.is_control() => {
                let n = u32::from(c);
                result.push_str(&if n <= 255 {
                    format!("\\x{n:02x}")
                } else {
                    format!("\\u{n:04x}")
                });
            }
            c => result.push(c),
        }
    }
    result.push(quote);
    result
}

/// Validate the explicit owner inputs before any release file is created.
/// The caller resolves both revisions through the trusted common Git directory.
pub fn validate_release(
    record: &ReleaseRecord,
    attempt_marker: Option<&str>,
    resolved_tip: &str,
    checkout_head: &str,
) -> Result<(), ProtectionError> {
    if attempt_marker.is_some_and(|value| !value.is_empty()) {
        return Err(ProtectionError::Unverifiable(
            "release runs only outside a writer attempt".into(),
        ));
    }
    if !is_full_sha(&record.tip) {
        return Err(ProtectionError::Unverifiable(
            "release requires the full reviewed commit SHA".into(),
        ));
    }
    if resolved_tip != record.tip || checkout_head != record.tip {
        return Err(ProtectionError::Unverifiable(format!(
            "{} HEAD is not the reviewed tip {}; review it again",
            record.worktree.display(),
            record.tip
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record() -> ReleaseRecord {
        ReleaseRecord {
            tip: "a".repeat(40),
            actor: "chair".into(),
            reason: "reviewed".into(),
            worktree: "/repo/lane".into(),
            released_at: "2026-10-23T00:00:00+00:00".into(),
        }
    }

    #[test]
    fn releases_require_full_lowercase_sha1_or_sha256() {
        assert!(is_full_sha(&"a".repeat(40)));
        assert!(is_full_sha(&"0".repeat(64)));
        for invalid in [
            "a".repeat(39),
            "a".repeat(41),
            "A".repeat(40),
            "g".repeat(64),
        ] {
            assert!(!is_full_sha(&invalid));
        }
    }

    #[test]
    fn writer_attempt_cannot_release_even_an_exact_tip() {
        let record = record();
        assert_eq!(
            validate_release(&record, Some("marker"), &record.tip, &record.tip)
                .unwrap_err()
                .to_string(),
            "protected_instructions_unverifiable: release runs only outside a writer attempt"
        );
        assert!(validate_release(&record, Some(""), &record.tip, &record.tip).is_ok());
    }

    #[test]
    fn release_covers_exact_reviewed_head_only() {
        let record = record();
        assert!(validate_release(&record, None, &record.tip, &record.tip).is_ok());
        assert!(validate_release(&record, None, &record.tip, &"b".repeat(40)).is_err());
        assert!(validate_release(&record, None, &"b".repeat(40), &record.tip).is_err());
    }

    #[test]
    fn release_record_keeps_all_wire_fields_and_explicit_clock() {
        let value = serde_json::to_value(record()).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 5);
        assert_eq!(value["released_at"], "2026-10-23T00:00:00+00:00");
        assert_eq!(
            serde_json::from_value::<ReleaseRecord>(value).unwrap(),
            record()
        );
    }

    #[test]
    fn errors_retain_stable_code_and_revision_refusal() {
        let err = ProtectionError::Changed("held".into());
        assert_eq!(err.code(), "protected_instructions_changed");
        assert_eq!(err.to_string(), "protected_instructions_changed: held");
        assert_eq!(
            validate_revision("-bad").unwrap_err().to_string(),
            "protected_instructions_unverifiable: invalid revision '-bad'"
        );
        assert!(validate_revision("HEAD").is_ok());
    }
}
