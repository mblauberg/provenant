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

/// Fields omitted from a patch retain their existing values. Revision zero
/// creates a summary on an existing checkpoint.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SummaryPatch {
    pub now: Option<String>,
    pub next: Option<String>,
    pub needs_you: Option<Vec<String>>,
    pub risk: Option<String>,
}

impl super::SessionStore {
    pub fn update_summary(
        &self,
        patch: SummaryPatch,
        expected: u64,
        budget: provenant_core::WaitBudget,
        cancel: &provenant_core::CancelToken,
        clock: &dyn provenant_core::Clock,
    ) -> Result<(Summary, super::Saved), super::SessionError> {
        if patch.now.is_none()
            && patch.next.is_none()
            && patch.needs_you.is_none()
            && patch.risk.is_none()
        {
            return Err(SummaryError.into());
        }
        let mut result = None;
        let saved = self
            .update(
                false,
                |current| {
                    let text = current.ok_or(super::SessionError::MissingCheckpoint)?;
                    let mut summary = parse_summary(text)?.unwrap_or(Summary {
                        schema_version: 1,
                        revision: 0,
                        updated_at: String::new(),
                        now: String::new(),
                        next: String::new(),
                        needs_you: Vec::new(),
                        risk: String::new(),
                    });
                    if summary.revision != expected {
                        return Err(super::SessionError::SummaryStale {
                            current_revision: summary.revision,
                        });
                    }
                    summary.revision = summary.revision.checked_add(1).ok_or(SummaryError)?;
                    summary.updated_at = clock.wall_now()?.to_string();
                    if let Some(value) = patch.now {
                        summary.now = value;
                    }
                    if let Some(value) = patch.next {
                        summary.next = value;
                    }
                    if let Some(value) = patch.needs_you {
                        summary.needs_you = value;
                    }
                    if let Some(value) = patch.risk {
                        summary.risk = value;
                    }
                    let next = replace_summary(text, &summary)?;
                    result = Some(summary);
                    Ok(Some(next))
                },
                budget,
                cancel,
                clock,
            )?
            .ok_or(super::SessionError::MissingCheckpoint)?;
        Ok((result.ok_or(SummaryError)?, saved))
    }
}

fn replace_summary(text: &str, summary: &Summary) -> Result<String, SummaryError> {
    let newline = if text.starts_with("---\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let quote = |value: &str| serde_json::to_string(value).map_err(|_| SummaryError);
    let block = format!(
        "provenant_summary:{newline}  schema_version: 1{newline}  revision: {}{newline}  updated_at: {}{newline}  now: {}{newline}  next: {}{newline}  needs_you: {}{newline}  risk: {}{newline}",
        summary.revision,
        quote(&summary.updated_at)?,
        quote(&summary.now)?,
        quote(&summary.next)?,
        serde_json::to_string(&summary.needs_you).map_err(|_| SummaryError)?,
        quote(&summary.risk)?
    );
    let output = if !text.starts_with("---\n") && !text.starts_with("---\r\n") {
        format!("---\n{block}---\n{text}")
    } else {
        let parsed = parse_frontmatter_integers(text).map_err(|_| SummaryError)?;
        let YamlValue::Mapping(root) = parsed.value else {
            return Err(SummaryError);
        };
        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        let end = lines
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, line)| line.trim_end_matches(['\r', '\n']) == "---")
            .map(|(i, _)| i)
            .ok_or(SummaryError)?;
        let (start, stop) = if let Some(node) = root.get("provenant_summary") {
            let child = node.span.line.checked_sub(1).ok_or(SummaryError)?;
            let start = (1..=child)
                .rev()
                .find(|&i| {
                    let line = lines[i].trim_end_matches(['\r', '\n']);
                    !line.is_empty()
                        && !line.starts_with(char::is_whitespace)
                        && !line.starts_with('#')
                })
                .ok_or(SummaryError)?;
            let mut stop = (start + 1..end)
                .find(|&i| {
                    let line = lines[i].trim_end_matches(['\r', '\n']);
                    !line.is_empty()
                        && !line.starts_with(char::is_whitespace)
                        && !line.starts_with('#')
                })
                .unwrap_or(end);
            while stop > start + 1 {
                let trimmed = lines[stop - 1].trim();
                if !trimmed.is_empty() && !trimmed.starts_with('#') {
                    break;
                }
                stop -= 1;
            }
            (start, stop)
        } else {
            (end, end)
        };
        let trivia: String = lines[start..stop]
            .iter()
            .skip(1)
            .filter(|line| {
                let trimmed = line.trim();
                trimmed.is_empty() || trimmed.starts_with('#')
            })
            .copied()
            .collect();
        format!(
            "{}{}{}{}",
            lines[..start].concat(),
            trivia,
            block,
            lines[stop..].concat()
        )
    };
    if parse_summary(&output)?.as_ref() != Some(summary) {
        return Err(SummaryError);
    }
    Ok(output)
}

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
