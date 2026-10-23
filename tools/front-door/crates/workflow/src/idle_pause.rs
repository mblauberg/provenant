//! Idle-frontier pause validation. Markdown policy is independent of file access.
use provenant_core::py::python_strip as strip;
use regex::Regex;
use std::{collections::BTreeSet, path::Path};

fn re(pattern: &str) -> Result<Regex, regex::Error> {
    Regex::new(
        &pattern
            .replace(r"\s", r"[\s\x1c-\x1f]")
            .replace(r"\S", r"[^\s\x1c-\x1f]"),
    )
}
fn field(text: &str, name: &str, prefix: &str) -> Result<String, regex::Error> {
    let pattern = re(&format!(
        r"(?m)^- \*\*{}:\*\*\s*(.+?)\s*$",
        regex::escape(name)
    ))?;
    if let Some(c) = pattern.captures(text) {
        return Ok(strip(&c[1]).into());
    }
    let heading = re(r"(?m)^## Heartbeat\s*$\n")?;
    let Some(start) = heading.find(text) else {
        return Ok(String::new());
    };
    let tail = &text[start.end()..];
    let end = re(r"(?m)^## ")?
        .find(tail)
        .map_or(tail.len(), |m| m.start());
    let section = strip(&tail[..end]);
    for line in crate::pending::splitlines(section) {
        let value = strip(line);
        if value.is_empty() || value.starts_with("<!--") || value.starts_with("-->") {
            continue;
        }
        if prefix.is_empty() {
            return Ok(value.into());
        }
        if let Some(c) = re(&format!(r"(?i)\A{}:\s*(.+)\z", regex::escape(prefix)))?.captures(value)
        {
            return Ok(strip(&c[1]).into());
        }
    }
    Ok(String::new())
}

pub fn queue_has_active_rows(text: &str) -> Result<(bool, Option<String>), regex::Error> {
    if !re(r"(?im)^##\s+Tier\s+\S")?.is_match(text) {
        return Ok((
            false,
            Some("canonical queue lacks a '## Tier ...' section".into()),
        ));
    }
    let separator = re(r"\A:?-{3,}:?\z")?;
    for line in crate::pending::splitlines(text) {
        let value = strip(line);
        if !value.starts_with('|') || !value.ends_with('|') {
            continue;
        }
        // Python splits only pipes whose immediately preceding character is not '\'.
        let inner = &value[1..value.len() - 1];
        let mut cells = Vec::new();
        let mut start = 0;
        for (i, c) in inner.char_indices() {
            if c == '|' && (i == 0 || inner.as_bytes().get(i - 1) != Some(&b'\\')) {
                cells.push(strip(&inner[start..i]));
                start = i + 1;
            }
        }
        cells.push(strip(&inner[start..]));
        let normalized: Vec<_> = cells.iter().map(|c| c.to_lowercase()).collect();
        if normalized
            == [
                "id",
                "status",
                "depends-on",
                "lease-owner",
                "lease-expiry",
                "notes",
            ]
            || cells.iter().all(|c| separator.is_match(c))
        {
            continue;
        }
        if cells.len() != 6 {
            return Ok((
                true,
                Some(format!("malformed queue row (expected 6 cells): {value}")),
            ));
        }
        let status = cells[1].trim_matches('`').to_uppercase();
        if ["PENDING", "LEASED"].contains(&status.as_str()) {
            return Ok((true, None));
        }
        if !["DONE", "BLOCKED", "DEFERRED"].contains(&status.as_str()) {
            return Ok((true,Some(format!("unrecognized queue status '{status}' — controlled vocabulary is PENDING, LEASED, DONE, BLOCKED, DEFERRED"))));
        }
    }
    Ok((false, None))
}

pub fn validate_text(text: &str, queue: &str) -> Result<Vec<String>, regex::Error> {
    let status = field(text, "Run status", "")?;
    let lease = field(text, "Conductor lease", "CONDUCTOR LEASE")?;
    let in_flight = field(text, "In flight", "")?;
    let next_up = field(text, "Next up", "")?;
    let resume = field(text, "Resume protocol", "RESUME PROTOCOL")?;
    let mut errors = vec![];
    if !re(r"(?i)\APAUSED\s+(?:—|-)\s+reason:\s*idle-frontier\z")?.is_match(&status) {
        errors.push("run status must be exactly PAUSED — reason: idle-frontier".into());
    }
    if !["released", "release-on-driver-exit"].contains(&lease.to_lowercase().as_str()) {
        errors.push("idle pause must record lease release-on-driver-exit or released".into());
    }
    if !["none", "(none)"].contains(&in_flight.to_lowercase().as_str()) {
        errors.push("idle pause requires an empty in-flight ledger".into());
    }
    if !re(r"(?i)\A\(?none\s+(?:—|-)\s+dry(?:\s+after\s+bounded\s+re-enumeration)?\)?\z")?
        .is_match(&next_up)
    {
        errors.push("idle pause requires an empty dry next-up frontier sentinel".into());
    }
    let resume = resume
        .split(provenant_core::py::python_whitespace)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let resume_re = re(r"\Arestart-on:\s*([a-z-]+(?:\s*,\s*[a-z-]+)*)\z")?;
    let triggers = resume_re
        .captures(&resume)
        .map(|c| {
            c[1].split(',')
                .map(|s| strip(s).to_owned())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if triggers.is_empty()
        || triggers.iter().collect::<BTreeSet<_>>().len() != triggers.len()
        || triggers.iter().any(|s| {
            ![
                "human-directive",
                "gate-answer",
                "external-completion",
                "material-change",
                "explicit-restart",
            ]
            .contains(&s.as_str())
        })
    {
        errors.push("idle pause requires a structured external resume trigger".into());
    }
    let (active, error) = queue_has_active_rows(queue)?;
    if let Some(error) = error {
        errors.push(error);
    } else if active {
        errors.push("canonical queue still has PENDING or LEASED rows".into());
    }
    Ok(errors)
}

#[derive(Debug, thiserror::Error)]
pub enum IdlePauseError {
    #[error(transparent)]
    Pattern(#[from] regex::Error),
    #[error("{operation}: {source}")]
    Read {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
}
/// Read failures are explicit. The CLI can render them in the source's error list.
pub fn validate(path: &Path, queue: Option<&Path>) -> Result<Vec<String>, IdlePauseError> {
    let text = std::fs::read_to_string(path).map_err(|source| IdlePauseError::Read {
        operation: "cannot read state",
        source,
    })?;
    let default = path.parent().unwrap_or(Path::new(".")).join("QUEUE.md");
    let queue = std::fs::read_to_string(queue.unwrap_or(&default)).map_err(|source| {
        IdlePauseError::Read {
            operation: "cannot read canonical queue",
            source,
        }
    })?;
    Ok(validate_text(
        &text.replace("\r\n", "\n").replace('\r', "\n"),
        &queue.replace("\r\n", "\n").replace('\r', "\n"),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn state() -> String {
        "# STATE\n- **Run status:** PAUSED — reason: idle-frontier\n- **Conductor lease:** released\n- **In flight:** (none)\n- **Next up:** (none — dry after bounded re-enumeration)\n- **Resume protocol:** restart-on: material-change, explicit-restart\n".into()
    }
    #[test]
    fn idle_pause_rejects_active_unknown_and_malformed_queue_rows() {
        assert!(validate_text(&state(), "## Tier 0\n").unwrap().is_empty());
        for status in ["PENDING", "LEASED", "ACTIVE"] {
            assert!(!validate_text(
                &state(),
                &format!("## Tier 0\n| A | {status} | - | - | - | note |\n")
            )
            .unwrap()
            .is_empty());
        }
        assert!(validate_text(
            &state(),
            "## Tier 0\n| A | DEFERRED | - | - | - | A \\| B |\n"
        )
        .unwrap()
        .is_empty());
        assert!(!validate_text(&state(), "## Tier 0\n| A | DONE |\n")
            .unwrap()
            .is_empty());
    }
    #[test]
    fn pause_sentinels_and_fallback_fields_match_python() {
        let mut cases = vec![];
        for text in [
            state(),
            state().replace("released", "not released"),
            state().replace("(none)", "run-17"),
            state().replace("PAUSED", "NOT PAUSED"),
            state().replace("dry after bounded re-enumeration", "not dry"),
            state().replace(
                "material-change, explicit-restart",
                "human-directive, human-directive",
            ),
            state().replace("material-change, explicit-restart", "human-directive."),
            state().replace("material-change, explicit-restart", "timer-expiry"),
            state().replace("material-change, explicit-restart", "wait and see"),
            state()
                .replace(
                    "- **Conductor lease:** released",
                    "CONDUCTOR LEASE: released",
                )
                .replace("# STATE", "## Heartbeat"),
            state().replace(' ', "\u{1c}"),
            String::new(),
        ] {
            for queue in [
                "## Tier 0\n",
                "# Queue\n",
                "## Tier 0\n| A | DONE | - | - | - | A \\| B |\n",
                "## Tier 0\n| A | ACTIVE | - | - | - | note |\n",
            ] {
                cases.push(json!({"text":text,"queue":queue}));
            }
        }
        let actual: Vec<_> = cases
            .iter()
            .map(|c| {
                json!(
                    validate_text(c["text"].as_str().unwrap(), c["queue"].as_str().unwrap())
                        .unwrap()
                )
            })
            .collect();
        if let Some(expected) = crate::test_support::oracle("idle_pause", &json!(cases)) {
            assert_eq!(json!(actual), expected);
        }
    }
}
