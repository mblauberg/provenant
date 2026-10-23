//! Output policy. Clocks, catalogue access and provider observation are explicit seams.
use crate::{
    adapters,
    ordered_json::{self, Node},
};
use provenant_core::py::python_strip;
use regex::{Regex, RegexBuilder};
use serde_json::Value;

mod rules {
    include!("failure_rules.rs");
}

#[derive(Debug, thiserror::Error)]
pub enum OutputError {
    #[error("unknown adapter: {0}")]
    UnknownAdapter(String),
    #[error(transparent)]
    Decode(#[from] crate::DecodeError),
    #[error(transparent)]
    Regex(#[from] regex::Error),
    #[error(transparent)]
    Event(#[from] adapters::opencode::OutputError),
    #[error("reset time is outside datetime range or has an invalid local minute")]
    InvalidResetTime,
    #[error("provider events must be a JSON array")]
    InvalidEventArray,
    #[error("provider event type is not hashable")]
    InvalidEventKind,
}

pub struct Classifier {
    rules: Vec<(&'static str, Regex, &'static [&'static str])>,
}
impl Classifier {
    pub fn new() -> Result<Self, OutputError> {
        Ok(Self {
            rules: rules::FAILURE_PATTERNS
                .iter()
                .map(|&(s, p, a)| Ok((s, insensitive(p)?, a)))
                .collect::<Result<_, regex::Error>>()?,
        })
    }
    pub fn classify<'a>(
        &'a self,
        adapter: &str,
        text: &str,
    ) -> Result<Option<&'a str>, OutputError> {
        for (signature, pattern, adapters) in &self.rules {
            if (adapters.is_empty() || adapters.contains(&adapter)) && pattern.is_match(text) {
                return Ok(Some(if *signature == "quota_window" {
                    if quota_window_is_short(text)? {
                        "rate_limited"
                    } else {
                        "usage_limited"
                    }
                } else {
                    signature
                }));
            }
        }
        Ok(None)
    }
}
pub(crate) fn insensitive(pattern: &str) -> Result<Regex, regex::Error> {
    RegexBuilder::new(pattern).case_insensitive(true).build()
}
pub(crate) fn is_match(pattern: &str, text: &str) -> Result<bool, regex::Error> {
    Ok(insensitive(pattern)?.is_match(text))
}
pub(crate) fn truncate(text: &str, count: usize) -> String {
    text.chars().take(count).collect()
}

pub fn error_class(status: &Value, signature: Option<&Value>) -> &'static str {
    let status = status.as_str();
    let signature = signature.and_then(Value::as_str);
    if let Some((_, class)) = rules::STATUS_ERROR_CLASSES
        .iter()
        .find(|(s, _)| Some(*s) == status)
    {
        return class;
    }
    if let Some((_, class)) = rules::ERROR_CLASS_ALIASES
        .iter()
        .find(|(s, _)| Some(*s) == signature)
    {
        return class;
    }
    if matches!(
        status,
        Some(
            "failed"
                | "rejected"
                | "tool_missing"
                | "output_identity_invalid"
                | "output_write_error"
        )
    ) {
        "provider_error"
    } else {
        "unknown"
    }
}

pub fn variant_fix(text: &str) -> Result<String, OutputError> {
    if let Some(c) =
        Regex::new(r#"Variant unavailable for ([^\s:\"]+): ([^\s\"}]+)"#)?.captures(text)
    {
        if let (Some(model), Some(variant)) = (c.get(1), c.get(2)) {
            return Ok(format!("{} has no `{}` variant; drop --effort or `#{}`, or pass a variant that model supports",model.as_str(),variant.as_str(),variant.as_str()));
        }
    }
    Ok(truncate(python_strip(text), 400))
}

/// Routing owns catalogue loading. Errors are deliberately equivalent to no registered ids.
#[derive(Debug, thiserror::Error)]
#[error("model catalogue unavailable: {0}")]
pub struct CatalogueError(pub String);

pub trait ModelCatalogue {
    fn registered_ids(&self, adapter: &str) -> Result<Vec<String>, CatalogueError>;
    fn registered_model(&self, adapter: &str, model: &str) -> Result<String, CatalogueError>;
}
pub fn model_unavailable_fix(
    adapter: &str,
    identity_source: Option<&str>,
    catalogue: &dyn ModelCatalogue,
) -> String {
    if identity_source != Some("passed-through") {
        return "choose another model".into();
    }
    let registered = catalogue.registered_ids(adapter).unwrap_or_default();
    let listing = if adapter == "opencode" {
        "; list every id with `opencode models`"
    } else {
        ""
    };
    if registered.is_empty() {
        format!("choose another model{listing}")
    } else {
        format!(
            "choose a registered model: {}{}{listing}",
            registered
                .iter()
                .take(6)
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
            if registered.len() > 6 { ", …" } else { "" }
        )
    }
}

pub fn quota_window_is_short(text: &str) -> Result<bool, OutputError> {
    let start = insensitive("quota exceeded")?
        .find(text)
        .or_else(|| Regex::new("RESOURCE_EXHAUSTED").ok()?.find(text));
    let suffix = start
        .map(|s| truncate(&text[s.start()..], 240))
        .unwrap_or_default();
    let phrase = Regex::new(r#"\.\s|\n|"\s*[,}\]]"#)?
        .split(&suffix)
        .next()
        .unwrap_or("");
    // Rust regex has no negative lookahead: inspect the character after the metric.
    fn metric(text: &str, pattern: &str) -> Result<bool, regex::Error> {
        let re = Regex::new(pattern)?;
        let found = re.find_iter(text).any(|m| {
            !text[m.end()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_lowercase())
        });
        Ok(found)
    }
    let long = is_match(r"per[ -](?:day|hour)\b|/\s*(?:day|hr|hour)\b", phrase)?
        || metric(text, r"Per(?:Day|Hour)")?;
    let short = is_match(
        r"per[ -](?:minute|second)\b|/\s*(?:min(?:ute)?|sec(?:ond)?)\b",
        phrase,
    )? || metric(text, r"Per(?:Minute|Second)")?;
    Ok(short && !long)
}

/// `at` is the caller's aware local clock, including its timezone for AM/PM resets.
/// Ordered provider event array. Decode from source bytes before map order is lost.
#[derive(Clone, Debug)]
pub struct OutputEvents(pub(crate) Vec<Node>);
impl OutputEvents {
    pub fn from_json(json: &str) -> Result<Self, OutputError> {
        match ordered_json::parse(json)? {
            Node::Array(events) => Ok(Self(events)),
            _ => Err(OutputError::InvalidEventArray),
        }
    }
}

pub fn reset_time(
    text: &str,
    events: &OutputEvents,
    at: &jiff::Zoned,
) -> Result<Option<String>, OutputError> {
    reset_time_nodes(text, &events.0, at)
}

fn iso(timestamp: jiff::Timestamp) -> String {
    let text = timestamp.to_string();
    // Python isoformat uses six places whenever microseconds are non-zero.
    if let Some((prefix, fraction)) = text.strip_suffix('Z').and_then(|s| s.split_once('.')) {
        format!("{prefix}.{fraction:0<6}Z")
    } else {
        text
    }
}
pub(crate) fn reset_time_nodes(
    text: &str,
    events: &[Node],
    at: &jiff::Zoned,
) -> Result<Option<String>, OutputError> {
    for item in events.iter().flat_map(Node::objects) {
        if let Some(Node::Atom(Value::Number(n))) = item.get("resetsAt") {
            let timestamp = if let Some(n) = n.as_i64() {
                n.checked_mul(1_000_000)
                    .and_then(|n| jiff::Timestamp::from_microsecond(n).ok())
            } else if let Some(n) = n.as_u64() {
                i64::try_from(n)
                    .ok()
                    .and_then(|n| n.checked_mul(1_000_000))
                    .and_then(|n| jiff::Timestamp::from_microsecond(n).ok())
            } else {
                n.as_f64()
                    .and_then(|n| provenant_core::py::from_timestamp(n).ok())
                    .and_then(|t| t.to_jiff().ok())
            };
            if let Some(t) = timestamp {
                if (-62135596800..253402300800).contains(&t.as_second()) {
                    return Ok(Some(iso(t)));
                }
            }
        }
    }
    let number = |s: &str| -> Result<i64, OutputError> {
        s.chars()
            .map(crate::pending::decimal_digit)
            .collect::<Option<Vec<_>>>()
            .and_then(|d| {
                d.into_iter()
                    .try_fold(0i64, |n, d| n.checked_mul(10)?.checked_add(i64::from(d)))
            })
            .ok_or(OutputError::InvalidResetTime)
    };
    for (pattern, unit) in [
        (r"Resets in\s+(\d+)\s*d(?:ays?)?\b", 86400i64),
        (r"Resets in\s+(\d+)h(?:\s*(\d+)m)?", 3600),
    ] {
        if let Some(c) = insensitive(pattern)?.captures(text) {
            let count = number(c.get(1).map(|m| m.as_str()).unwrap_or(""))?;
            let minutes = c
                .get(2)
                .map(|m| number(m.as_str()))
                .transpose()?
                .unwrap_or(0);
            let seconds = count
                .checked_mul(unit)
                .and_then(|n| minutes.checked_mul(60).and_then(|m| n.checked_add(m)))
                .ok_or(OutputError::InvalidResetTime)?;
            // Python timedelta arithmetic changes wall time before converting back to UTC.
            let dt = at
                .datetime()
                .checked_add(jiff::SignedDuration::from_secs(seconds))
                .map_err(|_| OutputError::InvalidResetTime)?;
            let t = dt
                .to_zoned(at.time_zone().clone())
                .map_err(|_| OutputError::InvalidResetTime)?
                .timestamp();
            return Ok(Some(iso(t)));
        }
    }
    if let Some(c) = insensitive(r"try again at\s+(\d{1,2}):(\d{2})\s*(AM|PM)")?.captures(text) {
        let get = |i| c.get(i).map(|m| m.as_str()).unwrap_or("");
        let hour = number(get(1))? % 12
            + if get(3).eq_ignore_ascii_case("PM") {
                12
            } else {
                0
            };
        let minute = number(get(2))?;
        let time = jiff::civil::Time::new(
            hour as i8,
            i8::try_from(minute).map_err(|_| OutputError::InvalidResetTime)?,
            0,
            0,
        )
        .map_err(|_| OutputError::InvalidResetTime)?;
        let mut candidate = at
            .date()
            .to_datetime(time)
            .to_zoned(at.time_zone().clone())
            .map_err(|_| OutputError::InvalidResetTime)?;
        if candidate <= *at {
            candidate = candidate
                .checked_add(jiff::Span::new().days(1))
                .map_err(|_| OutputError::InvalidResetTime)?;
        }
        return Ok(Some(iso(candidate.timestamp())));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn telemetry_classes_and_variant_fix_match_source() {
        type Oracle = (Vec<(Value, Value)>, Vec<String>, Vec<String>);
        let Some((cases, expected, fixes)): Option<Oracle> = crate::oracle::python(
            r#"
import json,sys
from skills.orchestrate.scripts import provider_output as m
values=[None,False,True,0,1,[],{},'unknown','failed','rejected','tool_missing','output_identity_invalid','output_write_error',*m.STATUS_ERROR_CLASSES,*m.ERROR_CLASS_ALIASES]
cases=[(s,e) for s in values for e in values]
print(json.dumps([cases,[m.error_class(s,e) for s,e in cases],[m._variant_fix(s) for s in ['Variant unavailable for p/m: high','plain input error','\u001c error \u001f']]]))
"#,
            &Value::Null,
            None,
        ) else {
            return;
        };
        assert_eq!(
            cases
                .iter()
                .map(|(s, e)| error_class(s, Some(e)))
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(
            [
                "Variant unavailable for p/m: high",
                "plain input error",
                "\u{1c} error \u{1f}"
            ]
            .map(|s| variant_fix(s).unwrap())
            .as_slice(),
            fixes
        );
    }
}
