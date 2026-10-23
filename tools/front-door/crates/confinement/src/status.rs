//! NUL-delimited Git porcelain v2 parsing. Paths remain native bytes.
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum StatusError {
    #[error("Git status output is not NUL terminated")]
    Unterminated,
    #[error("Git status output contains a malformed record")]
    Malformed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusKind {
    Changed,
    Untracked,
    Ignored,
    RenameOrCopy,
    Conflict,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusPath {
    pub path: Vec<u8>,
    pub kind: StatusKind,
    pub original_path: Option<Vec<u8>>,
}

/// Parse `git status --porcelain=v2 -z` without interpreting path bytes as text.
pub fn parse_porcelain_v2_z(raw: &[u8]) -> Result<Vec<StatusPath>, StatusError> {
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    if !raw.ends_with(&[0]) {
        return Err(StatusError::Unterminated);
    }
    let mut fields = raw.split(|byte| *byte == 0);
    let mut paths = Vec::new();
    while let Some(record) = fields.next() {
        if record.is_empty() {
            continue;
        }
        match record.first().copied() {
            Some(b'#') => continue,
            Some(b'?') | Some(b'!') => {
                if record.get(1) != Some(&b' ') || record.len() <= 2 {
                    return Err(StatusError::Malformed);
                }
                paths.push(StatusPath {
                    path: record.get(2..).ok_or(StatusError::Malformed)?.to_vec(),
                    kind: if record.first() == Some(&b'?') {
                        StatusKind::Untracked
                    } else {
                        StatusKind::Ignored
                    },
                    original_path: None,
                });
            }
            Some(b'1') => {
                let pieces: Vec<_> = record.splitn(9, |byte| *byte == b' ').collect();
                let path = pieces.get(8).ok_or(StatusError::Malformed)?;
                if pieces.len() != 9 || pieces.first() != Some(&&b"1"[..]) || path.is_empty() {
                    return Err(StatusError::Malformed);
                }
                paths.push(StatusPath {
                    path: path.to_vec(),
                    kind: StatusKind::Changed,
                    original_path: None,
                });
            }
            Some(b'2') => {
                let pieces: Vec<_> = record.splitn(10, |byte| *byte == b' ').collect();
                let path = pieces.get(9).ok_or(StatusError::Malformed)?;
                if pieces.len() != 10 || pieces.first() != Some(&&b"2"[..]) || path.is_empty() {
                    return Err(StatusError::Malformed);
                }
                let original = fields.next().ok_or(StatusError::Malformed)?;
                if original.is_empty() {
                    return Err(StatusError::Malformed);
                }
                paths.push(StatusPath {
                    path: path.to_vec(),
                    kind: StatusKind::RenameOrCopy,
                    original_path: Some(original.to_vec()),
                });
            }
            Some(b'u') => {
                let pieces: Vec<_> = record.splitn(11, |byte| *byte == b' ').collect();
                let path = pieces.get(10).ok_or(StatusError::Malformed)?;
                if pieces.len() != 11 || pieces.first() != Some(&&b"u"[..]) || path.is_empty() {
                    return Err(StatusError::Malformed);
                }
                paths.push(StatusPath {
                    path: path.to_vec(),
                    kind: StatusKind::Conflict,
                    original_path: None,
                });
            }
            _ => return Err(StatusError::Malformed),
        }
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::{StatusError, StatusKind, parse_porcelain_v2_z};

    #[test]
    fn porcelain_v2_keeps_non_utf8_and_space_paths_from_nul_records() {
        let raw = b"# branch.oid 0123456789012345678901234567890123456789\0? .agents/a b\xff.md\0";
        let parsed = parse_porcelain_v2_z(raw);
        assert_eq!(
            parsed,
            Ok(vec![super::StatusPath {
                path: b".agents/a b\xff.md".to_vec(),
                kind: StatusKind::Untracked,
                original_path: None,
            }])
        );
    }

    #[test]
    fn malformed_and_unterminated_status_are_refused() {
        assert_eq!(
            parse_porcelain_v2_z(b"1 bad\0"),
            Err(StatusError::Malformed)
        );
        assert_eq!(
            parse_porcelain_v2_z(b"? path"),
            Err(StatusError::Unterminated)
        );
        assert_eq!(
            parse_porcelain_v2_z(b"x path\0"),
            Err(StatusError::Malformed)
        );
    }

    #[test]
    fn rename_and_conflict_records_are_retained_as_review_required() {
        let rename = b"2 R. N... 100644 100644 100644 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb R100 .agents/new\0.agents/old\0";
        let conflict = b"u UU N... 100644 100644 100644 100644 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb cccccccccccccccccccccccccccccccccccccccc .agents/conflict\0";
        assert_eq!(
            parse_porcelain_v2_z(rename).map(|records| {
                records
                    .first()
                    .map(|row| (row.kind, row.original_path.clone()))
            }),
            Ok(Some((
                StatusKind::RenameOrCopy,
                Some(b".agents/old".to_vec()),
            )))
        );
        assert_eq!(
            parse_porcelain_v2_z(conflict).map(|records| records.first().map(|row| row.kind)),
            Ok(Some(StatusKind::Conflict))
        );
    }
}
