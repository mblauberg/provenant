//! Opt-in metadata import from a Claude Code session's subagents directory.
use crate::{Categories, PriceTable, TokenTotal};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{BufRead, BufReader},
    path::Path,
};

#[derive(Deserialize)]
struct Record {
    #[serde(rename = "type")]
    kind: Option<String>,
    message: Option<Message>,
}
#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
}
#[derive(Clone, Deserialize)]
struct Usage {
    input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}
impl Usage {
    fn counts(&self) -> [Option<u64>; 4] {
        [
            self.input_tokens,
            self.cache_read_input_tokens,
            self.cache_creation_input_tokens,
            self.output_tokens,
        ]
    }
    fn merge(&mut self, other: Self) {
        self.input_tokens = self.input_tokens.max(other.input_tokens);
        self.cache_read_input_tokens = self
            .cache_read_input_tokens
            .max(other.cache_read_input_tokens);
        self.cache_creation_input_tokens = self
            .cache_creation_input_tokens
            .max(other.cache_creation_input_tokens);
        self.output_tokens = self.output_tokens.max(other.output_tokens);
    }
}
#[derive(Serialize)]
pub struct NativeModel {
    pub model: String,
    /// Billing classification in the price table, not account billing evidence.
    pub price_table_billing: Option<crate::BillingKind>,
    pub agents: u64,
    pub messages: u64,
    pub usage: Categories<TokenTotal>,
    /// API-equivalent USD estimate, absent if usage or prices are incomplete.
    pub cost_usd: Option<String>,
}
#[derive(Serialize)]
pub struct NativeReport {
    pub scope: &'static str,
    pub agents: u64,
    pub models: Vec<NativeModel>,
}

/// Read only assistant IDs, model and usage metadata; other JSON fields are ignored.
/// Fail on malformed transcripts rather than silently undercounting.
pub fn claude_subagents(path: &Path, prices: &PriceTable) -> Result<NativeReport, String> {
    let entries =
        std::fs::read_dir(path).map_err(|e| format!("cannot read subagents directory: {e}"))?;
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read subagents directory: {e}"))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("agent-")
            && name.ends_with(".jsonl")
            && entry.file_type().map_err(|e| e.to_string())?.is_file()
        {
            files.push(entry.path());
        }
    }
    files.sort();
    if files.len() > 10_000 {
        return Err("too many native subagent transcripts".into());
    }
    let mut by_model: BTreeMap<String, (BTreeSet<usize>, Vec<Usage>)> = BTreeMap::new();
    let mut bytes = 0u64;
    let mut message_count = 0usize;
    for (agent, path) in files.iter().enumerate() {
        let file = std::fs::File::open(path)
            .map_err(|e| format!("cannot open subagent transcript: {e}"))?;
        let size = file.metadata().map_err(|e| e.to_string())?.len();
        if size > 32 * 1024 * 1024 {
            return Err("subagent transcript exceeds 32 MiB".into());
        }
        bytes = bytes
            .checked_add(size)
            .ok_or("native transcript size overflow")?;
        if bytes > 64 * 1024 * 1024 {
            return Err("native transcripts exceed 64 MiB".into());
        }
        let mut messages: BTreeMap<String, (String, Usage)> = BTreeMap::new();
        for line in BufReader::new(file).lines() {
            let line = line.map_err(|e| format!("cannot read subagent transcript: {e}"))?;
            let record: Record =
                serde_json::from_str(&line).map_err(|_| "invalid subagent transcript JSON")?;
            if record.kind.as_deref() != Some("assistant") {
                continue;
            }
            let Some(message) = record.message else {
                continue;
            };
            let Some(usage) = message.usage else { continue };
            let id = message
                .id
                .filter(|s| !s.is_empty())
                .ok_or("subagent usage missing message ID")?;
            let model = message
                .model
                .filter(|s| !s.is_empty())
                .ok_or("subagent usage missing model")?;
            match messages.entry(id) {
                std::collections::btree_map::Entry::Vacant(e) => {
                    message_count += 1;
                    if message_count > 100_000 {
                        return Err("too many native messages".into());
                    }
                    e.insert((model, usage));
                }
                std::collections::btree_map::Entry::Occupied(mut e) => {
                    if e.get().0 != model {
                        return Err("subagent message model changed".into());
                    }
                    e.get_mut().1.merge(usage);
                }
            }
        }
        for (model, usage) in messages.into_values() {
            let row = by_model.entry(model).or_default();
            row.0.insert(agent);
            row.1.push(usage);
        }
    }
    let mut models = Vec::new();
    for (model, (agents, messages)) in by_model {
        let mut totals = Categories::<TokenTotal>::default();
        let mut numerator = 0u128;
        let mut complete = true;
        let canonical = if model.starts_with("claude/") {
            model.clone()
        } else {
            format!("claude/{model}")
        };
        let priced = prices.get("claude", &canonical);
        let billing = priced.map(|(_, billing)| *billing);
        let rates = priced
            .map(|(rates, _)| rates.clone())
            .unwrap_or_default()
            .usage_rates();
        for usage in &messages {
            for ((total, count), rate) in totals
                .values_mut()
                .into_iter()
                .take(4)
                .zip(usage.counts())
                .zip(rates)
            {
                match count {
                    Some(n) => {
                        total.known = total.known.checked_add(n).ok_or("native usage overflow")?;
                        if let Some(rate) = rate {
                            numerator = numerator
                                .checked_add(u128::from(n) * u128::from(rate))
                                .ok_or("native cost overflow")?;
                        } else if n > 0 {
                            complete = false;
                        }
                    }
                    None => {
                        total.missing += 1;
                        complete = false;
                    }
                }
            }
        }
        let micro = (numerator / 1_000_000)
            .checked_add(u128::from(numerator % 1_000_000 >= 500_000))
            .ok_or("native cost overflow")?;
        models.push(NativeModel {
            model,
            price_table_billing: billing,
            agents: agents.len() as u64,
            messages: messages.len() as u64,
            usage: totals,
            cost_usd: complete.then(|| format!("{}.{:06}", micro / 1_000_000, micro % 1_000_000)),
        });
    }
    Ok(NativeReport {
        scope: "claude_native_subagents",
        agents: files.len() as u64,
        models,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ConfiguredPrices, validate_prices};

    #[test]
    fn imports_metadata_once_per_message_and_prices_exact_model()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        std::fs::write(
            dir.path().join("agent-a.jsonl"),
            concat!(
                "{\"type\":\"user\",\"message\":{\"content\":\"private\"}}\n",
                "{\"type\":\"assistant\",\"message\":{\"id\":\"m\",\"model\":\"example\",\"content\":\"private\",\"usage\":{\"input_tokens\":10,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0,\"output_tokens\":2}}}\n",
                "{\"type\":\"assistant\",\"message\":{\"id\":\"m\",\"model\":\"example\",\"usage\":{\"input_tokens\":10,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0,\"output_tokens\":5}}}\n"
            ),
        )?;
        let config: ConfiguredPrices = toml_edit::de::from_str(
            r#"
schema_version = 1
id = "test"
reviewed_at = "2026-01-01"
units = "usd_microdollars_per_million_tokens"
[[models]]
provider = "claude"
model = "claude/example"
sources = ["test"]
billing = "api"
[models.rates]
uncached_input = 1000000
cache_read = 1000000
cache_write = 1000000
output_excluding_reasoning = 2000000
"#,
        )?;
        let report = claude_subagents(dir.path(), &validate_prices(&config)?)?;
        assert_eq!(report.agents, 1);
        assert_eq!(report.models[0].messages, 1);
        assert_eq!(report.models[0].usage.output_excluding_reasoning.known, 5);
        assert_eq!(report.models[0].cost_usd.as_deref(), Some("0.000020"));
        Ok(())
    }
}
