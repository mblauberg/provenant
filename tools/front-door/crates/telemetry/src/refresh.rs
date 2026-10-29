//! Price sheet refresh: parse market price sources into the sheet's units and
//! propose a diff. Nothing here fetches or writes; callers own both edges.
use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::{ConfiguredPrices, Rates};

pub const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/models";
pub const LITELLM_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
/// Used when the sheet sets no `discrepancy_threshold_bps`.
pub const DEFAULT_THRESHOLD_BPS: u32 = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Openrouter,
    Litellm,
}
impl Source {
    pub fn url(self) -> &'static str {
        match self {
            Self::Openrouter => OPENROUTER_URL,
            Self::Litellm => LITELLM_URL,
        }
    }
    pub fn other(self) -> Self {
        match self {
            Self::Openrouter => Self::Litellm,
            Self::Litellm => Self::Openrouter,
        }
    }
    pub fn parse(self, document: &Value) -> Result<BTreeMap<String, Rates>, &'static str> {
        match self {
            Self::Openrouter => parse_openrouter(document),
            Self::Litellm => parse_litellm(document),
        }
    }
    fn id(self, model: &crate::ConfiguredModel) -> Option<&str> {
        match self {
            Self::Openrouter => model.openrouter_id.as_deref(),
            Self::Litellm => model.litellm_id.as_deref(),
        }
    }
}

/// Exact conversion of a USD-per-token decimal ("0.000003", "3e-06") into
/// microdollars per million tokens, rounding half up. Negative or malformed
/// values (OpenRouter uses -1 for variable pricing) have no rate.
pub fn microdollars_per_million(per_token: &str) -> Option<u64> {
    let text = per_token.trim().to_ascii_lowercase();
    let (mantissa, exponent) = match text.split_once('e') {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<i32>().ok()?),
        None => (text.as_str(), 0),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if whole.is_empty() && fraction.is_empty()
        || !whole
            .chars()
            .chain(fraction.chars())
            .all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let digits: u128 = format!("{whole}{fraction}").parse().ok()?;
    // USD per token to microdollars per million tokens scales by 10^12.
    let scale = exponent.checked_sub(i32::try_from(fraction.len()).ok()?)? + 12;
    let value = if scale >= 0 {
        digits.checked_mul(10u128.checked_pow(u32::try_from(scale).ok()?)?)?
    } else {
        let divisor = 10u128.checked_pow(u32::try_from(-scale).ok()?);
        match divisor {
            Some(divisor) => digits / divisor + u128::from(digits % divisor * 2 >= divisor),
            None => 0,
        }
    };
    u64::try_from(value).ok()
}

fn number_rate(value: &Value) -> Option<u64> {
    match value {
        Value::String(text) => microdollars_per_million(text),
        Value::Number(number) => microdollars_per_million(&number.to_string()),
        _ => None,
    }
}

/// OpenRouter `/api/v1/models`: `data[].pricing` in USD per token. Conditional
/// `overrides` are ignored: the sheet records standard API-equivalent rates.
pub fn parse_openrouter(document: &Value) -> Result<BTreeMap<String, Rates>, &'static str> {
    let mut out = BTreeMap::new();
    let entries = document["data"]
        .as_array()
        .ok_or("OpenRouter document has no `data` array")?;
    for entry in entries {
        let (Some(id), pricing) = (entry["id"].as_str(), &entry["pricing"]) else {
            continue;
        };
        if !pricing.is_object() {
            continue;
        }
        out.insert(
            id.to_owned(),
            Rates {
                uncached_input: number_rate(&pricing["prompt"]),
                cache_read: number_rate(&pricing["input_cache_read"]),
                cache_write: number_rate(&pricing["input_cache_write"]),
                output_excluding_reasoning: number_rate(&pricing["completion"]),
            },
        );
    }
    if out.is_empty() {
        return Err("OpenRouter document lists no priced models");
    }
    Ok(out)
}

/// LiteLLM `model_prices_and_context_window.json`: an object keyed by model.
pub fn parse_litellm(document: &Value) -> Result<BTreeMap<String, Rates>, &'static str> {
    let priced = |entry: &Value| {
        entry.get("input_cost_per_token").is_some() || entry.get("output_cost_per_token").is_some()
    };
    let map = document
        .as_object()
        .ok_or("LiteLLM document is not an object")?;
    if !map.values().any(priced) {
        return Err("LiteLLM document lists no priced models");
    }
    Ok(map
        .iter()
        .map(|(id, entry)| {
            (
                id.clone(),
                Rates {
                    uncached_input: number_rate(&entry["input_cost_per_token"]),
                    cache_read: number_rate(&entry["cache_read_input_token_cost"]),
                    cache_write: number_rate(&entry["cache_creation_input_token_cost"]),
                    output_excluding_reasoning: number_rate(&entry["output_cost_per_token"]),
                },
            )
        })
        .collect())
}

const CATEGORIES: [&str; 4] = [
    "uncached_input",
    "cache_read",
    "cache_write",
    "output_excluding_reasoning",
];

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Change {
    pub provider: String,
    pub model: String,
    pub category: &'static str,
    pub current: Option<u64>,
    pub proposed: u64,
    /// The current rate's `effective_from`, to be closed by this change.
    pub current_effective_from: Option<String>,
}

/// Two sources disagree beyond the threshold. Reported, never resolved.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Discrepancy {
    pub provider: String,
    pub model: String,
    pub category: &'static str,
    pub primary: u64,
    pub cross_check: u64,
    pub difference_bps: u64,
}

#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub struct Proposal {
    pub changes: Vec<Change>,
    pub discrepancies: Vec<Discrepancy>,
    /// Sheet models with a source id that the primary source did not list.
    pub missing: Vec<String>,
}
impl Proposal {
    pub fn is_clean(&self) -> bool {
        self.changes.is_empty() && self.discrepancies.is_empty() && self.missing.is_empty()
    }
}

fn rates_array(rates: &Rates) -> [Option<u64>; 4] {
    rates.values()
}

/// Diff the sheet against `primary`, and compare `primary` with `cross_check`
/// for the models both name. Output order follows the sheet, so a fixed input
/// gives a fixed proposal.
pub fn propose(
    sheet: &ConfiguredPrices,
    primary: (Source, &BTreeMap<String, Rates>),
    cross_check: (Source, &BTreeMap<String, Rates>),
) -> Proposal {
    let threshold = u64::from(
        sheet
            .discrepancy_threshold_bps
            .unwrap_or(DEFAULT_THRESHOLD_BPS),
    );
    let mut out = Proposal::default();
    for model in &sheet.models {
        let Some(id) = primary.0.id(model) else {
            continue;
        };
        let name = format!("{}:{}", model.provider, model.model);
        let Some(found) = primary.1.get(id) else {
            out.missing.push(name);
            continue;
        };
        let other = cross_check.0.id(model).and_then(|id| cross_check.1.get(id));
        let (current, proposed) = (rates_array(&model.rates), rates_array(found));
        let other = other.map(rates_array);
        for (index, category) in CATEGORIES.into_iter().enumerate() {
            let Some(new) = proposed[index] else {
                continue;
            };
            if current[index] != Some(new) {
                out.changes.push(Change {
                    provider: model.provider.clone(),
                    model: model.model.clone(),
                    category,
                    current: current[index],
                    proposed: new,
                    current_effective_from: model.effective_from.clone(),
                });
            }
            if let Some(second) = other.and_then(|o| o[index]) {
                let high = u128::from(new.max(second));
                let difference = u128::from(new.abs_diff(second)) * 10_000;
                // Exact: a fractional basis point over the threshold still counts.
                if difference > u128::from(threshold) * high {
                    let bps = u64::try_from(difference / high).unwrap_or(u64::MAX);
                    out.discrepancies.push(Discrepancy {
                        provider: model.provider.clone(),
                        model: model.model.clone(),
                        category,
                        primary: new,
                        cross_check: second,
                        difference_bps: bps,
                    });
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_per_token_to_exact_microdollars_per_million() {
        for (input, expected) in [
            ("0.000003", Some(3_000_000)),
            ("0.0000003", Some(300_000)),
            ("3e-06", Some(3_000_000)),
            ("1.25E-6", Some(1_250_000)),
            ("0.000000125", Some(125_000)),
            ("0.0000000000004", Some(0)),
            ("0.0000000000005", Some(1)),
            ("0", Some(0)),
            ("-1", None),
            ("", None),
            ("abc", None),
        ] {
            assert_eq!(microdollars_per_million(input), expected, "{input}");
        }
        assert_eq!(
            number_rate(&serde_json::json!(1.5e-6)),
            Some(1_500_000),
            "JSON numbers convert the same way"
        );
    }
}

#[cfg(test)]
mod proposal_tests {
    use super::*;
    use serde_json::json;

    fn sheet() -> ConfiguredPrices {
        toml_edit::de::from_str(
            r#"
schema_version = 1
id = "t"
reviewed_at = "2026-10-22"
units = "usd_microdollars_per_million_tokens"
discrepancy_threshold_bps = 1000
[[models]]
provider = "p"
model = "m"
billing = "api"
sources = ["s"]
openrouter_id = "v/m"
litellm_id = "m"
[models.rates]
uncached_input = 1000000
"#,
        )
        .unwrap()
    }
    fn run(openrouter: &str, litellm: &str) -> Proposal {
        let or = parse_openrouter(&json!({"data":[{"id":"v/m","pricing":{"prompt":openrouter}}]}))
            .unwrap();
        let ll = parse_litellm(&json!({"m":{"input_cost_per_token":litellm}})).unwrap();
        propose(&sheet(), (Source::Openrouter, &or), (Source::Litellm, &ll))
    }

    #[test]
    fn a_difference_just_over_the_threshold_is_reported() {
        // 1_000_000 vs 1_111_112: 1_111_112 - 1_000_000 = 111_112 over 1_111_112 is
        // 1000.0008 bps, which rounds down to the threshold but exceeds it.
        let over = run("0.000001", "0.000001111112");
        assert_eq!(over.discrepancies.len(), 1);
        assert_eq!(over.discrepancies[0].difference_bps, 1000);
        // 111_111 over 1_111_111 is just under 1000 bps.
        assert!(run("0.000001", "0.000001111111").discrepancies.is_empty());
    }

    #[test]
    fn a_model_the_source_no_longer_lists_is_not_clean() {
        let other =
            parse_openrouter(&json!({"data":[{"id":"v/other","pricing":{"prompt":"0.000001"}}]}))
                .unwrap();
        let ll = parse_litellm(&json!({"m":{"input_cost_per_token":0.000001}})).unwrap();
        let proposal = propose(
            &sheet(),
            (Source::Openrouter, &other),
            (Source::Litellm, &ll),
        );
        assert_eq!(proposal.missing, ["p:m"]);
        assert!(!proposal.is_clean());
    }

    #[test]
    fn unrelated_documents_are_rejected() {
        for document in [
            json!({"name":"icon"}),
            json!([]),
            json!({"data":[]}),
            json!({"data":[{"id":"x"}]}),
        ] {
            assert!(parse_openrouter(&document).is_err(), "{document}");
        }
        for document in [json!([]), json!({}), json!({"name":"icon","size":3})] {
            assert!(parse_litellm(&document).is_err(), "{document}");
        }
        // Optional fields stay optional.
        assert!(parse_openrouter(&json!({"data":[{"id":"x","pricing":{}}]})).is_ok());
    }
}
