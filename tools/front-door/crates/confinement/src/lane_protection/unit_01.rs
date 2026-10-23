//! Lane release input validation and stable diagnostic rendering.
use super::*;
use provenant_core::py::python_repr;

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
        assert_eq!(RELEASE_DIRECTORY, "provenant-instruction-checks/releases");
        assert_eq!(RELEASE_EXTENSION, ".json");
        assert_eq!(DEFAULT_BASE, "HEAD");
        let err = ProtectionError::Changed("held".into());
        assert_eq!(err.code(), "protected_instructions_changed");
        assert_eq!(err.to_string(), "protected_instructions_changed: held");
        assert_eq!(
            validate_revision("-bad").unwrap_err().to_string(),
            "protected_instructions_unverifiable: invalid revision '-bad'"
        );
        assert!(validate_revision("HEAD").is_ok());
    }

    #[test]
    fn invalid_revision_repr_matches_python_nonprintable_unicode() {
        let values = [
            "-\u{00a0}",  // no-break space (Zs)
            "-\u{200b}",  // format (Cf)
            "-\u{2028}",  // line separator (Zl)
            "-\u{2029}",  // paragraph separator (Zp)
            "-\u{0378}",  // unassigned
            "-\u{e0001}", // format (Cf), non-printable astral character uses \\U
        ];
        let Some(expected) = crate::python::oracle(
            r#"
import lane_protection
print(json.dumps([str(lane_protection.ProtectionError(f"invalid revision {s!r}")) for s in fixture['values']]))
"#,
            &serde_json::json!({"values": values}),
        ) else {
            return;
        };
        let actual: Vec<_> = values
            .iter()
            .map(|value| validate_revision(value).unwrap_err().to_string())
            .collect();
        assert_eq!(serde_json::to_value(actual).unwrap(), expected);
    }
}
