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
    pub fn parse(self, document: &Value) -> BTreeMap<String, Rates> {
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
pub fn parse_openrouter(document: &Value) -> BTreeMap<String, Rates> {
    let mut out = BTreeMap::new();
    for entry in document["data"].as_array().into_iter().flatten() {
        let (Some(id), pricing) = (entry["id"].as_str(), &entry["pricing"]) else {
            continue;
        };
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
    out
}

/// LiteLLM `model_prices_and_context_window.json`: an object keyed by model.
pub fn parse_litellm(document: &Value) -> BTreeMap<String, Rates> {
    document
        .as_object()
        .into_iter()
        .flatten()
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
        .collect()
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
        self.changes.is_empty() && self.discrepancies.is_empty()
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
                let high = new.max(second);
                let difference = u128::from(new.abs_diff(second)) * 10_000;
                let bps = if high == 0 {
                    0
                } else {
                    u64::try_from(difference / u128::from(high)).unwrap_or(u64::MAX)
                };
                if bps > threshold {
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
