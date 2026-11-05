//! Advisory checks over issue text. Issue bodies are data, never commands.
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path, sync::OnceLock};

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Off,
    Info,
    #[default]
    Warn,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub citation: Severity,
    pub closed_owner: Severity,
    pub acceptance: Severity,
    pub verify: Severity,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Finding {
    pub check: &'static str,
    pub severity: Severity,
    pub message: String,
}

fn pattern(cell: &'static OnceLock<Regex>, expression: &str) -> &'static Regex {
    cell.get_or_init(|| {
        Regex::new(expression).unwrap_or_else(|error| unreachable!("static regex: {error}"))
    })
}

fn add(findings: &mut Vec<Finding>, check: &'static str, severity: Severity, message: String) {
    if severity != Severity::Off {
        findings.push(Finding {
            check,
            severity,
            message,
        });
    }
}

/// Extract references whose surrounding sentence describes a live dependency or owner.
pub fn live_references(body: &str) -> Vec<u64> {
    static RE: OnceLock<Regex> = OnceLock::new();
    static LIVE: OnceLock<Regex> = OnceLock::new();
    static HISTORY: OnceLock<Regex> = OnceLock::new();
    let refs = pattern(&RE, r"#([1-9][0-9]*)\b");
    let live = pattern(
        &LIVE,
        r"(?i)\b(owner|owns|owned by|blocked by|blocker|depends on|dependency)\b",
    );
    let history = pattern(
        &HISTORY,
        r"(?i)\b(absorbed|copied|migrated|derived|superseded|closed|completed|resolved)\s+from\s+(?:#[1-9][0-9]*\s*,\s*)*$",
    );
    let mut result = Vec::new();
    for line in body.lines() {
        for cap in refs.captures_iter(line) {
            let Some(m) = cap.get(0) else { continue };
            let before = &line[..m.start()];
            if history.is_match(before) {
                continue;
            }
            // The sentence, rather than the whole body, supplies the relationship.
            let sentence = line
                .split(['.', ';'])
                .find(|part| part.contains(m.as_str()))
                .unwrap_or(line);
            if live.is_match(sentence)
                && let Ok(number) = cap[1].parse()
            {
                result.push(number);
            }
        }
    }
    result.sort_unstable();
    result.dedup();
    result
}

/// Check the body against the current tree and known issue states. Unknown states
/// are deliberately not interpreted as open or closed.
pub fn lint(
    root: &Path,
    body: &str,
    kind: &str,
    config: &Config,
    states: &BTreeMap<u64, bool>,
) -> Vec<Finding> {
    static CITATION: OnceLock<Regex> = OnceLock::new();
    static ACCEPTANCE: OnceLock<Regex> = OnceLock::new();
    static VERIFY: OnceLock<Regex> = OnceLock::new();
    let citation = pattern(
        &CITATION,
        r"(?:[A-Za-z0-9_.-]+/)+(?:[A-Za-z0-9_.-]+)#[A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*|[A-Za-z0-9_.-]+\.[A-Za-z0-9_-]+#[A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*",
    );
    let acceptance = pattern(
        &ACCEPTANCE,
        r"(?i)\b(acceptance|accept|done when|criteria|criterion|expected)\b",
    );
    let verify = pattern(
        &VERIFY,
        r"(?i)\b(verify|verification|verified|test|tests|check|run|command|capture|review)\b",
    );
    let mut findings = Vec::new();
    if config.citation != Severity::Off {
        for hit in citation.find_iter(body) {
            // Ignore matches within URL tokens, including the path and fragment.
            let token_start = body[..hit.start()]
                .rfind(char::is_whitespace)
                .map_or(0, |index| index + 1);
            if body[token_start..hit.start()].contains("://") {
                continue;
            }
            let Some((path, symbol)) = hit.as_str().split_once('#') else {
                continue;
            };
            let relative = Path::new(path);
            let safe = relative
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_)));
            let resolved = safe
                && root
                    .canonicalize()
                    .ok()
                    .zip(root.join(relative).canonicalize().ok())
                    .is_some_and(|(base, target)| {
                        target.starts_with(base)
                            && std::fs::read_to_string(target).is_ok_and(|source| {
                                let token = symbol.rsplit("::").next().unwrap_or(symbol);
                                source
                                    .split(|c: char| !c.is_alphanumeric() && c != '_')
                                    .any(|word| word == token)
                            })
                    });
            if !resolved {
                add(
                    &mut findings,
                    "citation",
                    config.citation,
                    format!("unresolved citation: {}", hit.as_str()),
                );
            }
        }
    }
    for number in live_references(body) {
        if states.get(&number) == Some(&true) {
            add(
                &mut findings,
                "closed_owner",
                config.closed_owner,
                format!("closed issue #{number} is named as a live owner or blocker"),
            );
        }
    }
    if matches!(kind.to_ascii_lowercase().as_str(), "task" | "feature") {
        if !acceptance.is_match(body) {
            add(
                &mut findings,
                "acceptance",
                config.acceptance,
                "no acceptance found".into(),
            );
        }
        if !verify.is_match(body) {
            add(
                &mut findings,
                "verify",
                config.verify,
                "no verification route found".into(),
            );
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn tracker_citation_warns_when_symbol_disappears() -> Result<(), Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("model.rs"), "pub struct Present;\n")?;
        let body =
            "See model.rs#Present and model.rs#Missing.\nAcceptance: works.\nVerify: test it.";
        let findings = lint(
            dir.path(),
            body,
            "task",
            &Config::default(),
            &BTreeMap::new(),
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].check, "citation");
        assert!(findings[0].message.contains("model.rs#Missing"));
        Ok(())
    }

    #[test]
    fn tracker_closed_owner_distinguishes_history() {
        let body = "Owner: #12\nBlocked by #13\nAbsorbed from #14\n";
        let states = BTreeMap::from([(12, true), (13, true), (14, true)]);
        let findings = lint(Path::new("."), body, "bug", &Config::default(), &states);
        assert_eq!(
            findings
                .iter()
                .filter(|f| f.check == "closed_owner")
                .count(),
            2
        );
        assert!(findings.iter().all(|f| !f.message.contains("#14")));
    }

    #[test]
    fn tracker_ignores_urls_history_and_unknown_states() {
        let body = "Owner: #12 (absorbed from #14, #15)\nBlocks #16\nSee https://example.com/docs/file.rs#Symbol";
        assert_eq!(live_references(body), vec![12]);
        assert!(
            lint(
                Path::new("."),
                body,
                "bug",
                &Config::default(),
                &BTreeMap::new()
            )
            .is_empty()
        );
    }

    #[test]
    fn tracker_shape_keywords_are_specific() {
        let findings = lint(
            Path::new("."),
            "When ready, check output",
            "task",
            &Config::default(),
            &BTreeMap::new(),
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].check, "acceptance");
        assert!(
            lint(
                Path::new("."),
                "Acceptance: works; run it",
                "task",
                &Config::default(),
                &BTreeMap::new()
            )
            .is_empty()
        );
    }

    #[test]
    fn tracker_compact_task_has_no_shape_findings() {
        let body = "Outcome: exports data.\nAcceptance: export contains the selected records.\nVerify: test the export.";
        assert!(
            lint(
                Path::new("."),
                body,
                "task",
                &Config::default(),
                &BTreeMap::new()
            )
            .is_empty()
        );
    }

    #[test]
    fn tracker_config_disables_or_changes_severity() -> Result<(), Box<dyn Error>> {
        let config: Config = toml_edit::de::from_str("acceptance = 'off'\nverify = 'info'\n")?;
        let findings = lint(
            Path::new("."),
            "Build it",
            "feature",
            &config,
            &BTreeMap::new(),
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].check, "verify");
        assert_eq!(findings[0].severity, Severity::Info);
        assert_eq!(Config::default().acceptance, Severity::Warn);
        Ok(())
    }
}
