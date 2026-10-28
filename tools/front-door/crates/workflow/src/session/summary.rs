use provenant_records::{YamlValue, parse_frontmatter_integers};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Chair-authored summary in checkpoint frontmatter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Summary {
    pub schema_version: u64,
    pub revision: u64,
    pub updated_at: String,
    pub now: String,
    pub next: String,
    pub needs_you: Vec<String>,
    pub risk: String,
}

#[derive(Debug, Error)]
#[error("invalid or unsupported checkpoint summary header")]
pub struct SummaryError;

/// Read the summary from YAML frontmatter, without interpreting the Markdown body.
/// A missing header or absent summary is distinct from an invalid header.
pub fn parse_summary(markdown: &str) -> Result<Option<Summary>, SummaryError> {
    if !markdown.starts_with("---\n") && !markdown.starts_with("---\r\n") {
        return Ok(None);
    }
    let end = markdown
        .lines()
        .skip(1)
        .scan(4usize, |offset, line| {
            *offset += line.len() + 1;
            Some((*offset, line.trim_end_matches('\r')))
        })
        .find(|(_, line)| *line == "---")
        .map(|(offset, _)| offset)
        .ok_or(SummaryError)?;
    if end > 8192 {
        return Err(SummaryError);
    }
    let parsed = parse_frontmatter_integers(markdown).map_err(|_| SummaryError)?;
    let YamlValue::Mapping(mut root) = parsed.value else {
        return Err(SummaryError);
    };
    let Some(node) = root.remove("provenant_summary") else {
        return Ok(None);
    };
    let YamlValue::Mapping(mut fields) = node.value else {
        return Err(SummaryError);
    };
    fn string(
        fields: &mut std::collections::BTreeMap<String, provenant_records::SpannedYaml>,
        key: &str,
    ) -> Result<String, SummaryError> {
        match fields.remove(key).map(|n| n.value) {
            Some(YamlValue::String(s)) => Ok(s),
            _ => Err(SummaryError),
        }
    }
    let schema_version = string(&mut fields, "schema_version")?
        .parse()
        .map_err(|_| SummaryError)?;
    let revision = string(&mut fields, "revision")?
        .parse()
        .map_err(|_| SummaryError)?;
    let updated_at = string(&mut fields, "updated_at")?;
    let canonical_time = if updated_at.ends_with('Z') && !updated_at.contains('.') {
        format!("{}.000000Z", &updated_at[..updated_at.len() - 1])
    } else {
        updated_at.clone()
    };
    if !canonical_time.ends_with('Z') || provenant_core::UtcMicros::parse(&canonical_time).is_err()
    {
        return Err(SummaryError);
    }
    let now = string(&mut fields, "now")?;
    let next = string(&mut fields, "next")?;
    let risk = string(&mut fields, "risk")?;
    let needs_you = match fields.remove("needs_you").map(|n| n.value) {
        Some(YamlValue::Sequence(items)) => items
            .into_iter()
            .map(|item| match item.value {
                YamlValue::String(s) => Ok(s),
                _ => Err(SummaryError),
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(SummaryError),
    };
    if schema_version != 1 || revision == 0 || !fields.is_empty() {
        return Err(SummaryError);
    }
    Ok(Some(Summary {
        schema_version,
        revision,
        updated_at,
        now,
        next,
        needs_you,
        risk,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_header_parses_example_and_rejects_invalid_data() {
        let source = "---\nprovenant_summary:\n  schema_version: 1\n  revision: 12\n  updated_at: \"2026-10-28T03:00:00Z\"\n  now: \"Repairing CI.\"\n  next: \"Land.\"\n  needs_you: [\"request-id\"]\n  risk: \"Needs verification.\"\n---\nBody\n";
        let summary = parse_summary(source).unwrap().unwrap();
        assert_eq!(summary.revision, 12);
        assert_eq!(summary.needs_you, ["request-id"]);
        assert!(parse_summary(&source.replace("2026-10-28T03:00:00Z", "yesterday")).is_err());
        assert!(parse_summary(&source.replace("schema_version: 1", "schema_version: 2")).is_err());
        assert!(
            parse_summary(&source.replace("revision: 12", "revision: 12\n  revision: 13")).is_err()
        );
        assert!(parse_summary("plain checkpoint").unwrap().is_none());
    }
}
