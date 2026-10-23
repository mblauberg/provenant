//! Catalogue records and live-list rendering, with an injected model lister.
//! Snapshot assembly, YAML compatibility and cooldown mutation await run/config.
use crate::{field::Field, pending::OrderedMap};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AdapterEntry {
    pub name: String,
    pub dispatch: String,
    pub endpoint_provider: Option<String>,
    pub fixed_model_family: Option<String>,
    pub effort_transport: String,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub latest_aliases: Field<bool>,
    pub aliases: OrderedMap<Vec<String>>,
    pub models: Vec<String>,
    pub model_details: Vec<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub read_only_guarantee: Field<String>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub write_modes: Field<Vec<String>>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub disabled_reason: Field<String>,
    pub endpoint_adapters: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Endpoint {
    pub base_url: String,
    pub token_env: String,
    pub model_family: String,
    pub adapters: Vec<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CatalogueSnapshot {
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub schema: Field<String>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub sha256: Field<String>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub sources: Field<Vec<String>>,
    pub drift: Vec<String>,
    pub adapters: Vec<AdapterEntry>,
    pub endpoints: OrderedMap<Endpoint>,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProbeRecord {
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub cache_hit: bool,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}
#[derive(Debug, Error)]
pub enum CatalogueError {
    #[error("catalogue source unavailable: {0}")]
    Unavailable(String),
    #[error("catalogue rendering requires a lossless UTF-16 string owner")]
    Utf16Boundary,
}
/// The source already varies by adapter. Executable selection has one table;
/// provider/process ownership stays outside the renderer.
pub const LISTED: &[(&str, &str)] = &[
    ("agy", "agy"),
    ("codex", "codex"),
    ("cursor", "cursor-agent"),
    ("kiro", "kiro-cli"),
    ("opencode", "opencode"),
];
pub trait ModelLister {
    fn list(&mut self, adapter: &str, executable: &str) -> Result<ProbeRecord, CatalogueError>;
}
const OUTPUT_BUDGET: usize = 4096;
fn length(text: &str) -> usize {
    text.encode_utf16().count()
}
fn prefix(text: &str, limit: usize) -> Result<String, CatalogueError> {
    let mut count = 0;
    let mut result = String::new();
    for ch in text.chars() {
        if count + ch.len_utf16() > limit {
            if count < limit {
                return Err(CatalogueError::Utf16Boundary);
            }
            break;
        }
        result.push(ch);
        count += ch.len_utf16();
    }
    Ok(result)
}
fn budget(text: String) -> Result<String, CatalogueError> {
    if length(&text) <= OUTPUT_BUDGET {
        Ok(text)
    } else {
        Ok(format!(
            "{}\n… truncated; pass match to narrow",
            prefix(&text, OUTPUT_BUDGET - 40)?
        ))
    }
}
fn catalogued_line(models: &[String], matched: Option<&str>, room: usize) -> String {
    let needle = matched.filter(|m| !m.is_empty()).map(str::to_lowercase);
    let chosen: Vec<_> = models
        .iter()
        .filter(|model| {
            needle
                .as_ref()
                .is_none_or(|needle| model.to_lowercase().contains(needle))
        })
        .collect();
    let mut line = if needle.is_some() {
        format!(
            "catalogued ({} of {} match {}):",
            chosen.len(),
            models.len(),
            matched.unwrap_or_default()
        )
    } else {
        "catalogued:".into()
    };
    if chosen.is_empty() {
        return format!("{line} -");
    }
    let mut kept = 0;
    for id in &chosen {
        if length(&line) + length(id) + 1 > room.saturating_sub(48) {
            break;
        }
        line.push(' ');
        line.push_str(id);
        kept += 1;
    }
    if kept < chosen.len() {
        line.push_str(&format!(
            " {} more omitted; pass match to narrow",
            chosen.len() - kept
        ));
    }
    line
}
pub fn live_models(
    adapter: &str,
    matched: Option<&str>,
    snapshot: &CatalogueSnapshot,
    lister: &mut impl ModelLister,
) -> Result<String, CatalogueError> {
    let Some(entry) = snapshot.adapters.iter().find(|entry| entry.name == adapter) else {
        let mut names = snapshot
            .adapters
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>();
        names.sort();
        return budget(format!(
            "{adapter}: unknown adapter; known: {}",
            names.join(", ")
        ));
    };
    let with_catalogue = |head: String| {
        let room = OUTPUT_BUDGET.saturating_sub(length(&head) + 1);
        budget(format!(
            "{head}\n{}",
            catalogued_line(&entry.models, matched, room)
        ))
    };
    if adapter == "claude" {
        return with_catalogue(
            "claude: no live list; run Claude models as native subagents (Agent tool)".into(),
        );
    }
    let Some((_, executable)) = LISTED.iter().find(|(name, _)| *name == adapter) else {
        return with_catalogue(format!("{adapter}: no live list"));
    };
    let record = match lister.list(adapter, executable) {
        Ok(record) => record,
        Err(_) => {
            return with_catalogue(format!(
                "{adapter}: live list unavailable (check the harness Python installation)"
            ))
        }
    };
    if record.models.is_empty() {
        return with_catalogue(format!(
            "{adapter}: live list unavailable ({})",
            prefix(
                record
                    .message
                    .as_deref()
                    .or(record.status.as_deref())
                    .unwrap_or("probe failed"),
                200
            )?
        ));
    }
    let needle = matched.filter(|m| !m.is_empty()).map(str::to_lowercase);
    let models: Vec<_> = record
        .models
        .iter()
        .filter(|model| {
            needle
                .as_ref()
                .is_none_or(|needle| model.to_lowercase().contains(needle))
        })
        .collect();
    let cached = if record.cache_hit { " (cached)" } else { "" };
    let head = if needle.is_some() {
        format!(
            "{adapter}: {} of {} live models match {}{cached}",
            models.len(),
            record.models.len(),
            matched.unwrap_or_default()
        )
    } else {
        format!("{adapter}: {} live models{cached}", models.len())
    };
    let mut groups: Vec<(String, Vec<String>)> = vec![];
    let lines: Vec<(String, usize)> = if models.len() <= 24 {
        if models.is_empty() {
            vec![]
        } else {
            vec![(
                models
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
                models.len(),
            )]
        }
    } else {
        for model in &models {
            let cut = model
                .find('/')
                .or_else(|| model.find('-'))
                .filter(|cut| *cut > 0);
            let (group, name) = cut.map_or(("", model.as_str()), |cut| model.split_at(cut + 1));
            if let Some((_, names)) = groups.iter_mut().find(|(key, _)| key == group) {
                names.push(name.into());
            } else {
                groups.push((group.into(), vec![name.into()]));
            }
        }
        groups
            .iter()
            .map(|(group, names)| {
                (
                    format!(
                        "{} ({}): {}",
                        if group.is_empty() { "other" } else { group },
                        names.len(),
                        if names.len() > 20 {
                            "pass match to list".into()
                        } else {
                            names.join(" ")
                        }
                    ),
                    names.len(),
                )
            })
            .collect()
    };
    let tail =
        format!("dispatch any as model \"{adapter}/<id>\"; an uncatalogued id runs with a note");
    let mut used = length(&head) + length(&tail) + 80;
    let mut kept = vec![];
    let mut omitted = 0;
    let mut omitted_models = 0;
    for (line, count) in lines {
        if used + length(&line) + 1 <= OUTPUT_BUDGET {
            used += length(&line) + 1;
            kept.push(line);
        } else {
            omitted += 1;
            omitted_models += count;
        }
    }
    let mut output = vec![head];
    output.extend(kept);
    if omitted > 0 {
        output.push(if models.len() <= 24 {
            format!("{omitted_models} models omitted; pass match to narrow")
        } else {
            format!("{omitted} more groups ({omitted_models} models) omitted; pass match to narrow")
        });
    }
    output.push(tail);
    budget(output.join("\n"))
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    struct Lister(ProbeRecord);
    impl ModelLister for Lister {
        fn list(&mut self, _: &str, _: &str) -> Result<ProbeRecord, CatalogueError> {
            Ok(self.0.clone())
        }
    }
    fn snapshot(name: &str, models: Vec<String>) -> CatalogueSnapshot {
        let entry:AdapterEntry=serde_json::from_value(json!({"name":name,"dispatch":"implemented","endpoint_provider":null,"fixed_model_family":null,"effort_transport":"flag","aliases":{},"models":models,"model_details":[],"endpoint_adapters":[]})).unwrap();
        CatalogueSnapshot {
            adapters: vec![entry],
            ..Default::default()
        }
    }
    #[test]
    fn native_catalogue_digest_matches_node_and_budgets_long_lists() {
        for (adapter, models, matched) in [
            ("claude", vec!["opus".into(), "sonnet".into()], None),
            ("copilot", vec!["model".repeat(1000)], None),
            ("claude", vec!["opus".into()], Some("NO")),
        ] {
            let snapshot = snapshot(adapter, models);
            let script = r#"import {liveModels} from './src/catalogue.ts'; const [adapter,snapshot,match]=process.argv.slice(1);console.log(JSON.stringify((await liveModels(adapter,{snapshot:JSON.parse(snapshot),...(match ? {match}: {})})).digest));"#;
            let Some(expected) = crate::test_support::node(
                script,
                &[
                    adapter.into(),
                    serde_json::to_string(&snapshot).unwrap(),
                    matched.unwrap_or_default().into(),
                ],
            ) else {
                return;
            };
            let actual = live_models(
                adapter,
                matched,
                &snapshot,
                &mut Lister(ProbeRecord::default()),
            )
            .unwrap();
            assert_eq!(json!(actual), expected);
            assert!(length(&actual) <= OUTPUT_BUDGET);
        }
    }
    #[test]
    fn live_lists_preserve_group_order_filter_cache_and_whole_line_budget() {
        let snapshot = snapshot("codex", vec![]);
        let models = (0..25).map(|n| format!("family/{n}")).collect();
        let mut lister = Lister(ProbeRecord {
            models,
            cache_hit: true,
            ..Default::default()
        });
        let digest = live_models("codex", None, &snapshot, &mut lister).unwrap();
        assert!(digest.contains("25 live models (cached)\nfamily/ (25): pass match to list"));
        let digest = live_models("codex", Some("/24"), &snapshot, &mut lister).unwrap();
        assert!(digest.contains("1 of 25 live models match /24 (cached)\nfamily/24"));
    }
}
