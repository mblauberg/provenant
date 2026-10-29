use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value as YamlValue};

type Mapping = JsonMap<String, YamlValue>;
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

/// A changed field, with its values in the requested order.
#[derive(Debug, Serialize)]
pub struct SummaryChange<T> {
    pub from: T,
    pub to: T,
}

/// Comparison of retained summary revisions. Missing revisions are listed so a
/// pruned baseline cannot be mistaken for an unchanged summary.
#[derive(Debug, Serialize)]
pub struct SummaryDiff {
    pub from: u64,
    pub to: u64,
    pub unavailable: Vec<u64>,
    pub now: Option<SummaryChange<String>>,
    pub next: Option<SummaryChange<String>>,
    pub needs_you: Option<SummaryIdChanges>,
    pub risk: Option<SummaryChange<String>>,
}

/// Request IDs added to and removed from the summary, in sorted order.
#[derive(Debug, Serialize)]
pub struct SummaryIdChanges {
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

fn changed<T: PartialEq + Clone>(from: &T, to: &T) -> Option<SummaryChange<T>> {
    (from != to).then(|| SummaryChange {
        from: from.clone(),
        to: to.clone(),
    })
}

impl super::SessionStore {
    /// Compare against a viewer's retained baseline, if one has been rendered.
    pub fn viewer_summary_diff(
        &self,
        baseline: Option<u64>,
        to: Option<u64>,
    ) -> Result<Option<SummaryDiff>, super::SessionError> {
        baseline.map(|from| self.summary_diff(from, to)).transpose()
    }

    /// Compare summary revision IDs found in the bounded retained checkpoint
    /// history. Repeated identical summaries from ordinary saves are allowed.
    pub fn summary_diff(
        &self,
        from: u64,
        to: Option<u64>,
    ) -> Result<SummaryDiff, super::SessionError> {
        let (current, history) = self.summary_snapshot()?;
        let to = match to {
            Some(to) => to,
            None => parse_summary(&current)?.ok_or(SummaryError)?.revision,
        };
        let mut left: Option<Summary> = None;
        let mut right: Option<Summary> = None;
        let mut any_summary = false;
        for text in history.iter().chain(std::iter::once(&current)) {
            // Invalid summaries may be saved with a warning. They do not
            // prevent comparison of other retained revisions.
            let Ok(Some(summary)) = parse_summary(text) else {
                continue;
            };
            any_summary = true;
            for (wanted, slot) in [(from, &mut left), (to, &mut right)] {
                if summary.revision == wanted {
                    if slot.as_ref().is_some_and(|previous| previous != &summary) {
                        return Err(super::SessionError::History);
                    }
                    *slot = Some(summary.clone());
                }
            }
        }
        if !any_summary {
            return Err(SummaryError.into());
        }
        let mut unavailable = Vec::new();
        if left.is_none() {
            unavailable.push(from);
        }
        if right.is_none() && to != from {
            unavailable.push(to);
        }
        let mut result = SummaryDiff {
            from,
            to,
            unavailable,
            now: None,
            next: None,
            needs_you: None,
            risk: None,
        };
        if let (Some(left), Some(right)) = (left, right) {
            result.now = changed(&left.now, &right.now);
            result.next = changed(&left.next, &right.next);
            let old: std::collections::BTreeSet<_> = left.needs_you.iter().cloned().collect();
            let new: std::collections::BTreeSet<_> = right.needs_you.iter().cloned().collect();
            let added: Vec<_> = new.difference(&old).cloned().collect();
            let removed: Vec<_> = old.difference(&new).cloned().collect();
            if !added.is_empty() || !removed.is_empty() {
                result.needs_you = Some(SummaryIdChanges { added, removed });
            }
            result.risk = changed(&left.risk, &right.risk);
        }
        Ok(result)
    }
    /// Update only as the explicitly bound chair; discovery identity is checked
    /// again under the checkpoint lock before the summary is read or changed.
    pub fn update_summary(
        &self,
        patch: SummaryPatch,
        expected: u64,
        budget: provenant_core::WaitBudget,
        cancel: &provenant_core::CancelToken,
        clock: &dyn provenant_core::Clock,
    ) -> Result<(Summary, super::Saved), super::SessionError> {
        if self.chair.is_none() {
            return Err(super::SessionError::ChairIdentityRequired);
        }
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
                    let text = current.ok_or_else(|| super::SessionError::MissingCheckpoint {
                        session: self.session.as_str().into(),
                    })?;
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
            .ok_or_else(|| super::SessionError::MissingCheckpoint {
                session: self.session.as_str().into(),
            })?;
        Ok((result.ok_or(SummaryError)?, saved))
    }
}

const HEADER_LIMIT: usize = 8192;

// Return the byte offset through the closing delimiter, including its newline.
fn header_end(text: &str) -> Result<Option<usize>, SummaryError> {
    if !text.starts_with("---\n") && !text.starts_with("---\r\n") {
        return Ok(None);
    }
    let mut offset = 0;
    for (index, line) in text.split_inclusive('\n').enumerate() {
        offset += line.len();
        if index > 0 && line.trim_end_matches('\n').trim_end_matches('\r') == "---" {
            return Ok(Some(offset));
        }
    }
    Err(SummaryError)
}

fn empty_header(text: &str, end: usize) -> bool {
    text[..end]
        .lines()
        .skip(1)
        .take_while(|line| line.trim_end_matches('\r') != "---")
        .all(|line| line.trim().is_empty() || line.trim_start().starts_with('#'))
}

fn yaml_options() -> serde_saphyr::Options {
    let mut budget = serde_saphyr::Budget::default();
    budget.max_reader_input_bytes = Some(256 * 1024);
    budget.max_depth = 32;
    budget.max_nodes = 10_000;
    let mut options = serde_saphyr::Options::default();
    options.budget = Some(budget);
    options
}

fn header_content(text: &str, end: usize) -> &str {
    let start = text.find('\n').map_or(0, |i| i + 1);
    let closing = text[..end].trim_end_matches(['\r', '\n']);
    let stop = closing.rfind('\n').map_or(start, |i| i + 1);
    &text[start..stop]
}

fn parse_header(text: &str, end: usize) -> Result<Mapping, SummaryError> {
    let header = header_content(text, end);
    if header.len() > 256 * 1024 {
        return Err(SummaryError);
    }
    serde_saphyr::from_str_with_options(header, yaml_options()).map_err(|_| SummaryError)
}

// Capture root key locations from the YAML parser. Values may contain
// multiline keys, flow collections or block scalars resembling root keys.
struct RootKeys(Vec<(String, usize)>);

impl<'de> Deserialize<'de> for RootKeys {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = RootKeys;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a YAML mapping")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<RootKeys, A::Error> {
                let mut keys = Vec::new();
                while let Some(key) = map.next_key::<serde_saphyr::Spanned<String>>()? {
                    let line =
                        usize::try_from(key.referenced.line()).map_err(serde::de::Error::custom)?;
                    keys.push((key.value, line));
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
                Ok(RootKeys(keys))
            }
        }
        deserializer.deserialize_map(Visitor)
    }
}

// Explicit keys may put the scalar on the line after the `?` indicator.
// Keep the indicator with the entry and reject flow mappings, whose entries
// do not occupy whole lines.
fn root_key_line(lines: &[&str], line: usize, end: usize) -> Result<usize, SummaryError> {
    if line == 0 || line >= end {
        return Err(SummaryError);
    }
    let mut start = line;
    if lines[start].starts_with(char::is_whitespace) {
        while start > 1 {
            start -= 1;
            let candidate = lines[start].trim();
            if candidate.is_empty() || candidate.starts_with('#') {
                continue;
            }
            if candidate != "?" {
                return Err(SummaryError);
            }
            break;
        }
    }
    if lines[start].starts_with(char::is_whitespace) || lines[start].starts_with('{') {
        return Err(SummaryError);
    }
    Ok(start)
}

fn replace_summary(text: &str, summary: &Summary) -> Result<String, super::SessionError> {
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
        let header_end = header_end(text)?.ok_or(SummaryError)?;
        let root = if empty_header(text, header_end) {
            Mapping::new()
        } else {
            parse_header(text, header_end)?
        };
        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        let end = lines
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, line)| line.trim_end_matches('\n').trim_end_matches('\r') == "---")
            .map(|(i, _)| i)
            .ok_or(SummaryError)?;
        let (start, stop) = if root.contains_key("provenant_summary") {
            let keys: RootKeys = serde_saphyr::from_str_with_options(
                header_content(text, header_end),
                yaml_options(),
            )
            .map_err(|_| SummaryError)?;
            let key_index = keys
                .0
                .iter()
                .position(|(key, _)| key == "provenant_summary")
                .ok_or(SummaryError)?;
            let start = root_key_line(&lines, keys.0[key_index].1, end)?;
            let mut stop = match keys.0.get(key_index + 1) {
                Some((_, line)) => root_key_line(&lines, *line, end)?,
                None => end,
            };
            if stop <= start || stop > end {
                return Err(SummaryError.into());
            }
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
    if header_end(&output)?.ok_or(SummaryError)? > HEADER_LIMIT {
        return Err(super::SessionError::SummaryTooLarge);
    }
    if parse_summary(&output)?.as_ref() != Some(summary) {
        return Err(SummaryError.into());
    }
    Ok(output)
}

/// Read the summary from YAML frontmatter, without interpreting the Markdown body.
/// A missing header or absent summary is distinct from an invalid header.
pub fn parse_summary(markdown: &str) -> Result<Option<Summary>, SummaryError> {
    let Some(end) = header_end(markdown)? else {
        return Ok(None);
    };
    if end > HEADER_LIMIT {
        return Err(SummaryError);
    }
    if empty_header(markdown, end) {
        return Ok(None);
    }
    let mut root = parse_header(markdown, end)?;
    let Some(node) = root.remove("provenant_summary") else {
        return Ok(None);
    };
    let YamlValue::Object(mut fields) = node else {
        return Err(SummaryError);
    };
    fn string(fields: &mut Mapping, key: &str) -> Result<String, SummaryError> {
        match fields.remove(key) {
            Some(YamlValue::String(s)) => Ok(s),
            Some(YamlValue::Number(n)) if key == "schema_version" || key == "revision" => {
                Ok(n.to_string())
            }
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
    let needs_you = match fields.remove("needs_you") {
        Some(YamlValue::Array(items)) => items
            .into_iter()
            .map(|item| match item {
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
        assert!(parse_summary("---\n---\n").unwrap().is_none());
        let updated = replace_summary("---\n---\n", &summary).unwrap();
        assert_eq!(parse_summary(&updated).unwrap(), Some(summary));
    }

    #[test]
    fn summary_header_counts_crlf_bytes_at_limit() {
        let prefix = "---\r\nprovenant_summary:\r\n  schema_version: 1\r\n  revision: 1\r\n  updated_at: \"2026-10-28T03:00:00Z\"\r\n  now: \"";
        let suffix = "\"\r\n  next: \"Next\"\r\n  needs_you: []\r\n  risk: \"None\"\r\n---\r\n";
        let header = format!(
            "{prefix}{}{suffix}",
            "x".repeat(8192 - prefix.len() - suffix.len())
        );
        assert_eq!(header.len(), 8192);
        assert!(parse_summary(&header).unwrap().is_some());
        let oversized = header.replacen("  now: \"", "  now: \"x", 1);
        assert_eq!(oversized.len(), 8193);
        assert!(parse_summary(&oversized).is_err());
    }

    #[test]
    fn replace_summary_uses_root_key_locations() {
        let summary = Summary {
            schema_version: 1,
            revision: 1,
            updated_at: "2026-10-28T03:00:00Z".into(),
            now: "Now".into(),
            next: "Next".into(),
            needs_you: vec![],
            risk: "None".into(),
        };
        let original = replace_summary("---\n---\nBody\n", &summary).unwrap();
        for prefix in [
            "? \"multiline\n  key\"\n: value\n",
            "'quoted key': value\n",
            "description: |\n  first line\n  provenant_summary: not a key\n",
        ] {
            let source = original.replacen(
                "provenant_summary:",
                &format!("{prefix}\"provenant_summary\":"),
                1,
            );
            let mut updated_summary = summary.clone();
            updated_summary.revision = 2;
            let updated = replace_summary(&source, &updated_summary).unwrap();
            assert!(updated.contains(prefix));
            assert!(updated.ends_with("---\nBody\n"));
            assert_eq!(parse_summary(&updated).unwrap(), Some(updated_summary));
        }
        let source = original.replace('\n', "\r\n");
        let updated = replace_summary(&source, &summary).unwrap();
        assert_eq!(updated, source);

        let source = original.replacen(
            "---\nBody",
            "# adjacent comment\n?\n  'following key'\n: value\n---\nBody",
            1,
        );
        let updated = replace_summary(&source, &summary).unwrap();
        assert!(updated.contains("# adjacent comment\n?\n  'following key'\n: value\n"));
        assert_eq!(parse_summary(&updated).unwrap(), Some(summary));
    }
}
