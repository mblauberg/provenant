//! Validate and normalise Delivery Authority V2 scopes.
//!
//! This is the Rust port of `skills/deliver/scripts/authority_mapping.py`.

use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::ser::SerializeMap;
use serde::Deserializer;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::fmt;

const MAX_SAFE_INTEGER: u64 = (1_u64 << 53) - 1;
const MAX_BUDGET_UNITS: usize = 128;
const MAX_ARRAY_ITEMS: usize = 256;
const MAX_PATH_BYTES: usize = 4096;

const AUTHORITY_FIELDS: &[&str] = &[
    "schema_version",
    "workspace_roots",
    "allowed_source_paths",
    "allowed_artifact_paths",
    "denied_paths",
    "prohibited_actions",
    "disclosure",
    "secrets_access",
    "secret_refs",
    "deployment",
    "deployment_targets",
    "irreversible_actions",
    "irreversible_action_ids",
    "network",
    "expires_at",
    "budget",
    "approved_by",
    "evidence",
    "evidence_digest",
    "delegations",
];
const DELEGATION_FIELDS: &[&str] = &[
    "schema_version",
    "workspace_roots",
    "allowed_source_paths",
    "allowed_artifact_paths",
    "denied_paths",
    "prohibited_actions",
    "disclosure",
    "secrets_access",
    "secret_refs",
    "deployment",
    "deployment_targets",
    "irreversible_actions",
    "irreversible_action_ids",
    "network",
    "expires_at",
    "budget",
    "actor",
];
const GENERIC_BUDGET_UNITS: &[&str] = &[
    "turns",
    "provider_calls",
    "concurrent_turns",
    "descendants",
    "message_bytes",
    "artifact_bytes",
    "wall_clock_milliseconds",
];

#[derive(Debug, thiserror::Error)]
pub enum AuthorityMappingError {
    #[error("{0}")]
    Invalid(String),
    #[error("Python integer is outside the i64/u64 range")]
    NumberOutOfRange,
    #[error("unhashable type: '{kind}'")]
    Unhashable { kind: &'static str },
    #[error("invalid JSON: {0}")]
    Json(#[source] serde_json::Error),
}

impl AuthorityMappingError {
    pub fn error_type(&self) -> &'static str {
        match self {
            Self::Unhashable { .. } => "TypeError",
            Self::Json(_) => "JSONDecodeError",
            _ => "AuthorityMappingError",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Approval {
    pub approved_by: String,
    pub evidence_id: String,
    pub evidence_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Disclosure {
    pub level: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Secrets {
    pub access: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub references: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Deployment {
    pub allowed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub targets: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IrreversibleActions {
    pub allowed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Network {
    pub tool_egress: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_hosts: Option<Vec<String>>,
}

/// A JSON object that serialises validated budget units in sorted key order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrderedBudget(Vec<(String, u64)>);

impl OrderedBudget {
    fn from_entries(entries: Vec<(String, u64)>) -> Self {
        let mut entries = entries;
        entries.sort_by(|(left, _), (right, _)| left.cmp(right));
        Self(entries)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &u64)> {
        self.0.iter().map(|(key, amount)| (key.as_str(), amount))
    }

    pub fn get(&self, key: &str) -> Option<&u64> {
        self.0
            .iter()
            .find_map(|(candidate, amount)| (candidate == key).then_some(amount))
    }
}

impl Serialize for OrderedBudget {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, amount) in &self.0 {
            map.serialize_entry(key, amount)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for OrderedBudget {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct OrderedBudgetVisitor;

        impl<'de> Visitor<'de> for OrderedBudgetVisitor {
            type Value = OrderedBudget;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON object of budget units")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut entries: Vec<(String, u64)> = Vec::new();
                while let Some((key, amount)) = map.next_entry::<String, u64>()? {
                    if let Some(index) = entries.iter().position(|(candidate, _)| candidate == &key)
                    {
                        entries[index].1 = amount;
                    } else {
                        entries.push((key, amount));
                    }
                }
                Ok(OrderedBudget::from_entries(entries))
            }
        }

        deserializer.deserialize_map(OrderedBudgetVisitor)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MappedAuthority {
    pub schema_version: u8,
    pub approval: Approval,
    pub workspace_roots: Vec<String>,
    pub source_paths: Vec<String>,
    pub artifact_paths: Vec<String>,
    pub denied_paths: Vec<String>,
    pub prohibited_actions: Vec<String>,
    pub disclosure: Disclosure,
    pub secrets: Secrets,
    pub deployment: Deployment,
    pub irreversible_actions: IrreversibleActions,
    pub network: Network,
    pub expires_at: String,
    pub budget: OrderedBudget,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MappedDelegation {
    pub actor: String,
    pub authority: MappedAuthority,
}

fn invalid(message: impl Into<String>) -> AuthorityMappingError {
    AuthorityMappingError::Invalid(message.into())
}

fn object<'a>(
    value: &'a Value,
    field: &str,
) -> Result<&'a Map<String, Value>, AuthorityMappingError> {
    value
        .as_object()
        .ok_or_else(|| invalid(format!("{field} must be an object")))
}

fn py_list(values: &[String]) -> String {
    let rendered = values
        .iter()
        .map(|value| provenant_core::py::python_repr(value))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{rendered}]")
}

fn closed(
    value: &Map<String, Value>,
    fields: &[&str],
    field: &str,
) -> Result<(), AuthorityMappingError> {
    let mut missing: Vec<String> = fields
        .iter()
        .filter(|name| !value.contains_key(**name))
        .map(|name| (*name).to_owned())
        .collect();
    let mut unknown: Vec<String> = value
        .keys()
        .filter(|name| !fields.contains(&name.as_str()))
        .cloned()
        .collect();
    missing.sort();
    unknown.sort();
    if !missing.is_empty() || !unknown.is_empty() {
        return Err(invalid(format!(
            "{field} fields are invalid; missing={}, unknown={}",
            py_list(&missing),
            py_list(&unknown)
        )));
    }
    Ok(())
}

fn token(value: &Value, field: &str) -> Result<String, AuthorityMappingError> {
    let text = value
        .as_str()
        .filter(|text| {
            let bytes = text.as_bytes();
            !bytes.is_empty()
                && bytes.len() <= 256
                && bytes[0].is_ascii_alphanumeric()
                && bytes
                    .iter()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(byte))
        })
        .ok_or_else(|| invalid(format!("{field} must be a bounded token")))?;
    Ok(text.to_owned())
}

fn path_value(value: &Value, field: &str) -> Result<String, AuthorityMappingError> {
    let text = value
        .as_str()
        .filter(|text| !text.is_empty())
        .ok_or_else(|| invalid(format!("{field} must be a relative path")))?;
    let bytes = text.as_bytes();
    let drive_path = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    let forbidden = text.starts_with('/')
        || drive_path
        || text.contains('\\')
        || text.contains('\0')
        || text.chars().any(|character| "*?[]{}".contains(character));
    if bytes.len() > MAX_PATH_BYTES || forbidden {
        return Err(invalid(format!(
            "{field} must be a canonical relative path"
        )));
    }

    let mut parts = Vec::new();
    for part in text.split('/') {
        if part == ".." {
            return Err(invalid(format!(
                "{field} must be a canonical relative path"
            )));
        }
        if !part.is_empty() && part != "." {
            parts.push(part);
        }
    }
    let canonical = if parts.is_empty() {
        ".".to_owned()
    } else {
        parts.join("/")
    };
    if canonical != text {
        return Err(invalid(format!(
            "{field} must be a canonical relative path"
        )));
    }
    Ok(canonical)
}

fn strings<F>(
    value: &Value,
    field: &str,
    minimum: usize,
    maximum: usize,
    parser: F,
) -> Result<Vec<String>, AuthorityMappingError>
where
    F: Fn(&Value, &str) -> Result<String, AuthorityMappingError>,
{
    let values = value
        .as_array()
        .ok_or_else(|| invalid(format!("{field} must be a list")))?;
    let mut parsed = Vec::with_capacity(values.len());
    for (index, item) in values.iter().enumerate() {
        parsed.push(parser(item, &format!("{field}[{index}]"))?);
    }
    if parsed.len() < minimum {
        return Err(invalid(format!(
            "{field} must contain at least {minimum} item(s)"
        )));
    }
    if parsed.len() > maximum {
        return Err(invalid(format!(
            "{field} must contain at most {maximum} item(s)"
        )));
    }
    let mut seen = HashSet::with_capacity(parsed.len());
    if parsed.iter().any(|value| !seen.insert(value.as_str())) {
        return Err(invalid(format!("{field} must contain unique values")));
    }
    parsed.sort();
    Ok(parsed)
}

fn token_parser(value: &Value, field: &str) -> Result<String, AuthorityMappingError> {
    token(value, field)
}

fn path_parser(value: &Value, field: &str) -> Result<String, AuthorityMappingError> {
    path_value(value, field)
}

fn unhashable(value: &Value) -> Option<&'static str> {
    match value {
        Value::Array(_) => Some("list"),
        Value::Object(_) => Some("dict"),
        _ => None,
    }
}

fn choice<'a>(
    value: &'a Value,
    field: &str,
    choices: &[&'a str],
) -> Result<&'a str, AuthorityMappingError> {
    if let Some(kind) = unhashable(value) {
        return Err(AuthorityMappingError::Unhashable { kind });
    }
    let value = value
        .as_str()
        .filter(|value| choices.contains(value))
        .ok_or_else(|| invalid(format!("{field} is invalid")))?;
    Ok(value)
}

fn disclosure(value: &Value) -> Result<Disclosure, AuthorityMappingError> {
    if let Some(kind) = unhashable(value) {
        return Err(AuthorityMappingError::Unhashable { kind });
    }
    match value.as_str() {
        Some("public") => Ok(Disclosure {
            level: "allowed".into(),
            scopes: None,
        }),
        Some("local-only") => Ok(Disclosure {
            level: "scoped".into(),
            scopes: Some(vec!["local".into()]),
        }),
        Some("approved-providers") => Ok(Disclosure {
            level: "scoped".into(),
            scopes: Some(vec!["approved-provider".into(), "local".into()]),
        }),
        _ => Err(invalid("authority.disclosure is invalid")),
    }
}

fn ascii_or_unicode_digit(value: char) -> Option<u32> {
    crate::pending::decimal_digit(value).map(u32::from)
}

fn decimal(chars: &[char]) -> Option<u32> {
    chars.iter().try_fold(0_u32, |value, character| {
        value
            .checked_mul(10)?
            .checked_add(ascii_or_unicode_digit(*character)?)
    })
}

#[derive(Debug, Clone)]
struct ParsedTimestamp {
    utc_second: i64,
    padded_fraction: String,
}

impl ParsedTimestamp {
    fn order_key(&self) -> (i64, &str) {
        (self.utc_second, &self.padded_fraction)
    }
}

fn parse_timestamp(value: &str) -> Option<ParsedTimestamp> {
    // datetime.fromisoformat rejects Unicode decimal digits even though re \d accepts them.
    if !value.is_ascii() {
        return None;
    }
    let chars: Vec<char> = value.chars().collect();
    if chars.len() < 20
        || chars.get(4) != Some(&'-')
        || chars.get(7) != Some(&'-')
        || chars.get(10) != Some(&'T')
        || chars.get(13) != Some(&':')
        || chars.get(16) != Some(&':')
    {
        return None;
    }
    let year = decimal(&chars[0..4])?;
    let month = decimal(&chars[5..7])?;
    let day = decimal(&chars[8..10])?;
    let hour = decimal(&chars[11..13])?;
    let minute = decimal(&chars[14..16])?;
    let second = decimal(&chars[17..19])?;
    let mut index = 19;
    let mut fraction = String::new();
    if chars.get(index) == Some(&'.') {
        index += 1;
        let start = index;
        while index < chars.len() && ascii_or_unicode_digit(chars[index]).is_some() {
            fraction.push(chars[index]);
            index += 1;
        }
        if index - start == 0 || index - start > 9 {
            return None;
        }
    }

    let (offset_sign, offset_hour, offset_minute) = match chars.get(index) {
        Some('Z') if index + 1 == chars.len() => (0_i64, 0_u32, 0_u32),
        Some(sign @ ('+' | '-'))
            if index + 6 == chars.len() && chars.get(index + 3) == Some(&':') =>
        {
            let hours = decimal(&chars[index + 1..index + 3])?;
            let minutes = decimal(&chars[index + 4..index + 6])?;
            let sign = if *sign == '+' { 1_i64 } else { -1_i64 };
            (sign, hours, minutes)
        }
        _ => return None,
    };

    if year == 0
        || month == 0
        || month > 12
        || hour > 23
        || minute > 59
        || second > 59
        || offset_hour > 23
        || offset_minute > 59
    {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if day == 0 || day > month_days {
        return None;
    }

    let year_before = i64::from(year) - 1;
    let mut days = year_before * 365 + year_before / 4 - year_before / 100 + year_before / 400;
    for earlier_month in 1..month {
        days += match earlier_month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if leap => 29,
            2 => 28,
            _ => 0,
        };
    }
    days += i64::from(day - 1);
    let local_second =
        days * 86_400 + i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second);
    let offset_second =
        offset_sign * (i64::from(offset_hour) * 3_600 + i64::from(offset_minute) * 60);
    let mut padded_fraction = fraction;
    padded_fraction.extend(std::iter::repeat_n(
        '0',
        9 - padded_fraction.chars().count(),
    ));
    Some(ParsedTimestamp {
        utc_second: local_second - offset_second,
        padded_fraction,
    })
}

fn timestamp(value: &Value, field: &str) -> Result<String, AuthorityMappingError> {
    let text = value
        .as_str()
        .filter(|text| parse_timestamp(text).is_some())
        .ok_or_else(|| invalid(format!("{field} must be a strict RFC3339 timestamp")))?;
    Ok(text.to_owned())
}

fn budget(
    value: &Value,
    field: &str,
    ordered_keys: Option<&[String]>,
) -> Result<OrderedBudget, AuthorityMappingError> {
    let input = object(value, field)?;
    if input.len() > MAX_BUDGET_UNITS {
        return Err(invalid(format!(
            "{field} must contain at most {MAX_BUDGET_UNITS} units"
        )));
    }
    let mut result = Vec::with_capacity(input.len());
    let ordered_entries: Vec<(&str, &Value)> = ordered_keys
        .map(|keys| {
            keys.iter()
                .filter_map(|key| input.get(key).map(|value| (key.as_str(), value)))
                .collect()
        })
        .unwrap_or_else(|| {
            input
                .iter()
                .map(|(key, value)| (key.as_str(), value))
                .collect()
        });
    for (key, amount) in ordered_entries {
        let allowed_provider_key = provider_token_budget(key);
        if !GENERIC_BUDGET_UNITS.contains(&key) && !allowed_provider_key {
            return Err(invalid(format!("{field} contains an invalid budget unit")));
        }
        let integer = match amount {
            Value::Number(number) if number.is_u64() => number.as_u64(),
            Value::Number(number) if number.is_i64() => {
                number.as_i64().and_then(|value| u64::try_from(value).ok())
            }
            _ => None,
        }
        .filter(|amount| *amount <= MAX_SAFE_INTEGER)
        .ok_or_else(|| invalid(format!("{field}.{key} must be a non-negative safe integer")))?;
        result.push((key.to_owned(), integer));
    }
    Ok(OrderedBudget::from_entries(result))
}

fn provider_token_budget(value: &str) -> bool {
    let Some(suffix) = value
        .strip_prefix("input_tokens:")
        .or_else(|| value.strip_prefix("output_tokens:"))
    else {
        return false;
    };
    !suffix.is_empty()
        && suffix.split(['.', '-']).all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

fn map_scope(
    scope: &Map<String, Value>,
    approval: Approval,
    field: &str,
    budget_order: Option<&[String]>,
) -> Result<MappedAuthority, AuthorityMappingError> {
    let version = scope.get("schema_version").unwrap_or(&Value::Null);
    let version_is_two = match version {
        Value::Bool(_) => false,
        Value::Number(number) => {
            number.as_i64() == Some(2)
                || number.as_u64() == Some(2)
                || (number.is_f64() && number.as_f64() == Some(2.0))
        }
        _ => false,
    };
    if !version_is_two {
        return Err(invalid(format!("{field}.schema_version must be 2")));
    }

    let workspace_roots = strings(
        scope.get("workspace_roots").unwrap_or(&Value::Null),
        &format!("{field}.workspace_roots"),
        1,
        64,
        path_parser,
    )?;
    let source_paths = strings(
        scope.get("allowed_source_paths").unwrap_or(&Value::Null),
        &format!("{field}.allowed_source_paths"),
        0,
        MAX_ARRAY_ITEMS,
        path_parser,
    )?;
    let artifact_paths = strings(
        scope.get("allowed_artifact_paths").unwrap_or(&Value::Null),
        &format!("{field}.allowed_artifact_paths"),
        0,
        MAX_ARRAY_ITEMS,
        path_parser,
    )?;
    if !allowed_paths_contained(&source_paths, &workspace_roots) {
        return Err(invalid(format!(
            "{field}.allowed_source_paths must be contained by {field}.workspace_roots"
        )));
    }
    if !allowed_paths_contained(&artifact_paths, &workspace_roots) {
        return Err(invalid(format!(
            "{field}.allowed_artifact_paths must be contained by {field}.workspace_roots"
        )));
    }
    let denied_paths = strings(
        scope.get("denied_paths").unwrap_or(&Value::Null),
        &format!("{field}.denied_paths"),
        0,
        MAX_ARRAY_ITEMS,
        path_parser,
    )?;
    let prohibited_actions = strings(
        scope.get("prohibited_actions").unwrap_or(&Value::Null),
        &format!("{field}.prohibited_actions"),
        0,
        MAX_ARRAY_ITEMS,
        token_parser,
    )?;
    let secret_refs = strings(
        scope.get("secret_refs").unwrap_or(&Value::Null),
        &format!("{field}.secret_refs"),
        0,
        MAX_ARRAY_ITEMS,
        token_parser,
    )?;
    let secrets_access = choice(
        scope.get("secrets_access").unwrap_or(&Value::Null),
        &format!("{field}.secrets_access"),
        &["none", "use-without-disclosure"],
    )?;
    if secrets_access == "none" && !secret_refs.is_empty() {
        return Err(invalid(format!(
            "{field}.secret_refs must be empty when secrets access is none"
        )));
    }
    if secrets_access == "use-without-disclosure" && secret_refs.is_empty() {
        return Err(invalid(format!(
            "{field}.secret_refs must be non-empty when secrets access is enabled"
        )));
    }
    let secrets = if secrets_access == "none" {
        Secrets {
            access: "none".into(),
            references: None,
        }
    } else {
        Secrets {
            access: "use-without-disclosure".into(),
            references: Some(secret_refs),
        }
    };

    let deployment_value = scope.get("deployment").unwrap_or(&Value::Null);
    let deployment_allowed = deployment_value
        .as_bool()
        .ok_or_else(|| invalid(format!("{field}.deployment must be boolean")))?;
    let deployment_targets = strings(
        scope.get("deployment_targets").unwrap_or(&Value::Null),
        &format!("{field}.deployment_targets"),
        0,
        MAX_ARRAY_ITEMS,
        token_parser,
    )?;
    if !deployment_allowed && !deployment_targets.is_empty() {
        return Err(invalid(format!(
            "{field}.deployment_targets must be empty when deployment is denied"
        )));
    }
    if deployment_allowed && deployment_targets.is_empty() {
        return Err(invalid(format!(
            "{field}.deployment_targets must be non-empty when deployment is allowed"
        )));
    }
    let deployment = Deployment {
        allowed: deployment_allowed,
        targets: deployment_allowed.then_some(deployment_targets),
    };

    let irreversible_value = scope.get("irreversible_actions").unwrap_or(&Value::Null);
    let irreversible_allowed = irreversible_value
        .as_bool()
        .ok_or_else(|| invalid(format!("{field}.irreversible_actions must be boolean")))?;
    let action_ids = strings(
        scope.get("irreversible_action_ids").unwrap_or(&Value::Null),
        &format!("{field}.irreversible_action_ids"),
        0,
        MAX_ARRAY_ITEMS,
        token_parser,
    )?;
    if !irreversible_allowed && !action_ids.is_empty() {
        return Err(invalid(format!(
            "{field}.irreversible_action_ids must be empty when irreversible actions are denied"
        )));
    }
    if irreversible_allowed && action_ids.is_empty() {
        return Err(invalid(format!(
            "{field}.irreversible_action_ids must be non-empty when irreversible actions are allowed"
        )));
    }
    let irreversible_actions = IrreversibleActions {
        allowed: irreversible_allowed,
        action_ids: irreversible_allowed.then_some(action_ids),
    };

    let network_field = format!("{field}.network");
    let network_value = scope.get("network").unwrap_or(&Value::Null);
    let network_object = object(network_value, &network_field)?;
    closed(
        network_object,
        &["tool_egress", "allowed_hosts"],
        &network_field,
    )?;
    let tool_egress = choice(
        network_object.get("tool_egress").unwrap_or(&Value::Null),
        &format!("{network_field}.tool_egress"),
        &["none", "allowlist"],
    )?;
    let allowed_hosts = strings(
        network_object.get("allowed_hosts").unwrap_or(&Value::Null),
        &format!("{network_field}.allowed_hosts"),
        0,
        MAX_ARRAY_ITEMS,
        host_parser,
    )?;
    if tool_egress == "none" && !allowed_hosts.is_empty() {
        return Err(invalid(format!(
            "{network_field}.allowed_hosts must be empty when tool egress is none"
        )));
    }
    if tool_egress == "allowlist" && allowed_hosts.is_empty() {
        return Err(invalid(format!(
            "{network_field}.allowed_hosts must be non-empty for an allowlist"
        )));
    }
    let network = Network {
        tool_egress: tool_egress.into(),
        allowed_hosts: (tool_egress == "allowlist").then_some(allowed_hosts),
    };

    let disclosure = disclosure(scope.get("disclosure").unwrap_or(&Value::Null))?;
    let expires_at = timestamp(
        scope.get("expires_at").unwrap_or(&Value::Null),
        &format!("{field}.expires_at"),
    )?;
    let budget = budget(
        scope.get("budget").unwrap_or(&Value::Null),
        &format!("{field}.budget"),
        budget_order,
    )?;

    Ok(MappedAuthority {
        schema_version: 2,
        approval,
        workspace_roots,
        source_paths,
        artifact_paths,
        denied_paths,
        prohibited_actions,
        disclosure,
        secrets,
        deployment,
        irreversible_actions,
        network,
        expires_at,
        budget,
    })
}

fn host(value: &Value, field: &str) -> Result<String, AuthorityMappingError> {
    let text = value
        .as_str()
        .filter(|text| {
            let bytes = text.as_bytes();
            !bytes.is_empty()
                && bytes.len() <= 253
                && bytes[0].is_ascii_alphanumeric()
                && bytes
                    .iter()
                    .all(|byte| byte.is_ascii_alphanumeric() || b".:-".contains(byte))
        })
        .ok_or_else(|| invalid(format!("{field} must be a bounded host")))?;
    Ok(text.to_owned())
}

fn host_parser(value: &Value, field: &str) -> Result<String, AuthorityMappingError> {
    host(value, field)
}

/// Normalise one closed Delivery Authority V2 object.
pub fn map_delivery_authority(authority: &Value) -> Result<MappedAuthority, AuthorityMappingError> {
    map_delivery_authority_with_budget_order(authority, None)
}

fn map_delivery_authority_with_budget_order(
    authority: &Value,
    budget_order: Option<&[String]>,
) -> Result<MappedAuthority, AuthorityMappingError> {
    let authority_object = object(authority, "authority")?;
    closed(authority_object, AUTHORITY_FIELDS, "authority")?;
    let approved_by = token(
        authority_object.get("approved_by").unwrap_or(&Value::Null),
        "authority.approved_by",
    )?;
    let evidence = token(
        authority_object.get("evidence").unwrap_or(&Value::Null),
        "authority.evidence",
    )?;
    let evidence_digest = authority_object
        .get("evidence_digest")
        .and_then(Value::as_str)
        .filter(|value| digest_valid(value))
        .ok_or_else(|| invalid("authority.evidence_digest must be a sha256 digest"))?
        .to_owned();
    let delegations = authority_object
        .get("delegations")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("authority.delegations must be a list"))?;
    let _ = delegations;
    let approval = Approval {
        approved_by,
        evidence_id: evidence,
        evidence_digest,
    };
    map_scope(authority_object, approval, "authority", budget_order)
}

/// Decode raw JSON with the shared Python integer range guard, retaining the
/// insertion order needed for source-compatible first-invalid budget errors.
/// Callers that start from an already-decoded `Value` cannot recover that
/// original order and should use this boundary when exact diagnostics matter.
pub fn map_delivery_authority_json(bytes: &[u8]) -> Result<MappedAuthority, AuthorityMappingError> {
    let value =
        provenant_records::checked_json::from_slice(bytes).map_err(|error| match error {
            provenant_records::checked_json::DecodeError::NumberOutOfRange(_) => {
                AuthorityMappingError::NumberOutOfRange
            }
            provenant_records::checked_json::DecodeError::Json(error) => {
                AuthorityMappingError::Json(error)
            }
        })?;
    let orders =
        serde_json::from_slice::<BudgetOrderSet>(bytes).map_err(AuthorityMappingError::Json)?;
    map_delivery_authority_with_budget_order(&value, orders.budget.as_deref())
}

/// Decode and map a raw authority while retaining budget insertion order for
/// Python-compatible first-invalid diagnostics.
pub fn map_delivery_delegations_json(
    bytes: &[u8],
) -> Result<Vec<MappedDelegation>, AuthorityMappingError> {
    let value =
        provenant_records::checked_json::from_slice(bytes).map_err(|error| match error {
            provenant_records::checked_json::DecodeError::NumberOutOfRange(_) => {
                AuthorityMappingError::NumberOutOfRange
            }
            provenant_records::checked_json::DecodeError::Json(error) => {
                AuthorityMappingError::Json(error)
            }
        })?;
    let orders =
        serde_json::from_slice::<BudgetOrderSet>(bytes).map_err(AuthorityMappingError::Json)?;
    map_delivery_delegations_with_orders(&value, &orders)
}

#[derive(Default)]
struct BudgetOrderSet {
    budget: Option<Vec<String>>,
    delegations: Vec<Option<Vec<String>>>,
}

impl<'de> Deserialize<'de> for BudgetOrderSet {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct OrdersVisitor;

        impl<'de> Visitor<'de> for OrdersVisitor {
            type Value = BudgetOrderSet;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON authority object")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut result = BudgetOrderSet::default();
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "budget" => result.budget = map.next_value::<BudgetKeys>()?.0,
                        "delegations" => {
                            result.delegations = map.next_value::<DelegationBudgetOrders>()?.0
                        }
                        _ => {
                            let _: IgnoredAny = map.next_value()?;
                        }
                    }
                }
                Ok(result)
            }

            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(BudgetOrderSet::default())
            }

            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(BudgetOrderSet::default())
            }

            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(BudgetOrderSet::default())
            }

            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(BudgetOrderSet::default())
            }

            fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(BudgetOrderSet::default())
            }

            fn visit_string<E: serde::de::Error>(self, _: String) -> Result<Self::Value, E> {
                Ok(BudgetOrderSet::default())
            }

            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(BudgetOrderSet::default())
            }

            fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(BudgetOrderSet::default())
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                while sequence.next_element::<IgnoredAny>()?.is_some() {}
                Ok(BudgetOrderSet::default())
            }
        }

        deserializer.deserialize_any(OrdersVisitor)
    }
}

struct BudgetKeys(Option<Vec<String>>);

impl<'de> Deserialize<'de> for BudgetKeys {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct KeysVisitor;

        impl<'de> Visitor<'de> for KeysVisitor {
            type Value = BudgetKeys;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON object of budget units")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut keys = Vec::new();
                let mut seen = HashSet::new();
                while let Some(key) = map.next_key::<String>()? {
                    let _: IgnoredAny = map.next_value()?;
                    if seen.insert(key.clone()) {
                        keys.push(key);
                    }
                }
                Ok(BudgetKeys(Some(keys)))
            }

            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(BudgetKeys(None))
            }

            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(BudgetKeys(None))
            }

            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(BudgetKeys(None))
            }

            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(BudgetKeys(None))
            }

            fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(BudgetKeys(None))
            }

            fn visit_string<E: serde::de::Error>(self, _: String) -> Result<Self::Value, E> {
                Ok(BudgetKeys(None))
            }

            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(BudgetKeys(None))
            }

            fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(BudgetKeys(None))
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                while sequence.next_element::<IgnoredAny>()?.is_some() {}
                Ok(BudgetKeys(None))
            }
        }

        deserializer.deserialize_any(KeysVisitor)
    }
}

struct DelegationBudgetOrders(Vec<Option<Vec<String>>>);

impl<'de> Deserialize<'de> for DelegationBudgetOrders {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct DelegationsVisitor;

        impl<'de> Visitor<'de> for DelegationsVisitor {
            type Value = DelegationBudgetOrders;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON delegation list")
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut orders = Vec::new();
                while let Some(order) = sequence.next_element::<BudgetOrderSet>()? {
                    orders.push(order.budget);
                }
                Ok(DelegationBudgetOrders(orders))
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(DelegationBudgetOrders(Vec::new()))
            }

            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(DelegationBudgetOrders(Vec::new()))
            }

            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(DelegationBudgetOrders(Vec::new()))
            }

            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(DelegationBudgetOrders(Vec::new()))
            }

            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(DelegationBudgetOrders(Vec::new()))
            }

            fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(DelegationBudgetOrders(Vec::new()))
            }

            fn visit_string<E: serde::de::Error>(self, _: String) -> Result<Self::Value, E> {
                Ok(DelegationBudgetOrders(Vec::new()))
            }

            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(DelegationBudgetOrders(Vec::new()))
            }

            fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(DelegationBudgetOrders(Vec::new()))
            }
        }

        deserializer.deserialize_any(DelegationsVisitor)
    }
}

fn digest_valid(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Map complete Delivery delegation scopes and reject every widening.
pub fn map_delivery_delegations(
    authority: &Value,
) -> Result<Vec<MappedDelegation>, AuthorityMappingError> {
    map_delivery_delegations_with_orders(authority, &BudgetOrderSet::default())
}

fn map_delivery_delegations_with_orders(
    authority: &Value,
    orders: &BudgetOrderSet,
) -> Result<Vec<MappedDelegation>, AuthorityMappingError> {
    let authority_object = object(authority, "authority")?;
    let parent = map_delivery_authority_with_budget_order(authority, orders.budget.as_deref())?;
    let delegations = authority_object
        .get("delegations")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("authority.delegations must be a list"))?;
    let mut mapped = Vec::with_capacity(delegations.len());
    for (index, raw) in delegations.iter().enumerate() {
        let field = format!("authority.delegations[{index}]");
        let delegation = object(raw, &field)?;
        closed(delegation, DELEGATION_FIELDS, &field)?;
        let actor = token(
            delegation.get("actor").unwrap_or(&Value::Null),
            &format!("{field}.actor"),
        )?;
        let child = map_scope(
            delegation,
            parent.approval.clone(),
            &field,
            orders.delegations.get(index).and_then(Option::as_deref),
        )?;
        if !authority_contained(&child, &parent)? {
            return Err(invalid(format!(
                "delegation {index} broadens parent authority"
            )));
        }
        mapped.push(MappedDelegation {
            actor,
            authority: child,
        });
    }
    Ok(mapped)
}

fn inside(path: &str, root: &str) -> bool {
    root == "." || path == root || path.starts_with(&format!("{root}/"))
}

fn allowed_paths_contained(child: &[String], parent: &[String]) -> bool {
    child
        .iter()
        .all(|path| parent.iter().any(|root| inside(path, root)))
}

fn denied_paths_preserved(child: &[String], parent: &[String]) -> bool {
    parent
        .iter()
        .all(|denial| child.iter().any(|path| inside(denial, path)))
}

fn union_contained(
    child_allowed: bool,
    child_values: Option<&[String]>,
    parent_allowed: bool,
    parent_values: Option<&[String]>,
    field: &str,
) -> Result<bool, AuthorityMappingError> {
    if !parent_allowed {
        return Ok(!child_allowed);
    }
    if !child_allowed {
        return Ok(true);
    }
    let child = child_values.ok_or_else(|| invalid(format!("child {field} is missing")))?;
    let parent = parent_values.ok_or_else(|| invalid(format!("parent {field} is missing")))?;
    Ok(child.iter().all(|value| parent.contains(value)))
}

/// Mapped records remain fallible at this boundary: deserialising a struct is
/// not proof that an enabled union carries its required payload.
pub fn authority_contained(
    child: &MappedAuthority,
    parent: &MappedAuthority,
) -> Result<bool, AuthorityMappingError> {
    let rank = |level: &str| match level {
        "allowed" => Ok(0),
        "scoped" => Ok(1),
        "forbidden" => Ok(2),
        _ => Err(invalid("mapped disclosure level is invalid")),
    };
    let mut disclosure_ok = rank(&child.disclosure.level)? >= rank(&parent.disclosure.level)?;
    if child.disclosure.level == "scoped" && parent.disclosure.level == "scoped" {
        let c = child
            .disclosure
            .scopes
            .as_ref()
            .ok_or_else(|| invalid("child disclosure.scopes is missing"))?;
        let p = parent
            .disclosure
            .scopes
            .as_ref()
            .ok_or_else(|| invalid("parent disclosure.scopes is missing"))?;
        disclosure_ok = c.iter().all(|s| p.contains(s));
    }
    let mut secrets_ok = child.secrets.access == "none";
    if parent.secrets.access != "none" && child.secrets.access != "none" {
        let c = child
            .secrets
            .references
            .as_ref()
            .ok_or_else(|| invalid("child secrets.references is missing"))?;
        let p = parent
            .secrets
            .references
            .as_ref()
            .ok_or_else(|| invalid("parent secrets.references is missing"))?;
        secrets_ok = c.iter().all(|s| p.contains(s));
    }
    let mut network_ok = child.network.tool_egress == "none";
    if parent.network.tool_egress != "none" && child.network.tool_egress != "none" {
        let c = child
            .network
            .allowed_hosts
            .as_ref()
            .ok_or_else(|| invalid("child network.allowedHosts is missing"))?;
        let p = parent
            .network
            .allowed_hosts
            .as_ref()
            .ok_or_else(|| invalid("parent network.allowedHosts is missing"))?;
        network_ok = c.iter().all(|s| p.contains(s));
    }
    if !(child.approval == parent.approval
        && allowed_paths_contained(&child.workspace_roots, &parent.workspace_roots)
        && allowed_paths_contained(&child.source_paths, &parent.source_paths)
        && allowed_paths_contained(&child.artifact_paths, &parent.artifact_paths)
        && denied_paths_preserved(&child.denied_paths, &parent.denied_paths)
        && parent
            .prohibited_actions
            .iter()
            .all(|a| child.prohibited_actions.contains(a))
        && disclosure_ok
        && secrets_ok
        && union_contained(
            child.deployment.allowed,
            child.deployment.targets.as_deref(),
            parent.deployment.allowed,
            parent.deployment.targets.as_deref(),
            "deployment.targets",
        )?
        && union_contained(
            child.irreversible_actions.allowed,
            child.irreversible_actions.action_ids.as_deref(),
            parent.irreversible_actions.allowed,
            parent.irreversible_actions.action_ids.as_deref(),
            "irreversibleActions.actionIds",
        )?
        && network_ok)
    {
        return Ok(false);
    }
    let child_expiry = parse_timestamp(&child.expires_at)
        .ok_or_else(|| invalid("child expiresAt must be a strict RFC3339 timestamp"))?;
    let parent_expiry = parse_timestamp(&parent.expires_at)
        .ok_or_else(|| invalid("parent expiresAt must be a strict RFC3339 timestamp"))?;
    Ok(child_expiry.order_key() <= parent_expiry.order_key()
        && child.budget.iter().all(|(key, amount)| {
            parent
                .budget
                .get(key)
                .is_some_and(|parent| amount <= parent)
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::path::PathBuf;

    fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../..")
    }

    fn delivery_fixture() -> Value {
        let path = root().join("tests/fixtures/authority-envelope-v2/delivery-authority.json");
        let text = std::fs::read_to_string(path).expect("delivery authority fixture should exist");
        let mut value: Value = serde_json::from_str(&text).expect("fixture should be JSON");
        // The Python policy helper projects its fixture to the closed V2 input
        // accepted by authority_mapping; Fabric-only fields are separate.
        let fields: HashSet<&str> = AUTHORITY_FIELDS.iter().copied().collect();
        value
            .as_object_mut()
            .expect("fixture should be an object")
            .retain(|key, _| fields.contains(key.as_str()));
        value
    }

    fn mapped_fixture() -> MappedAuthority {
        map_delivery_authority(&delivery_fixture()).expect("fixture authority should map")
    }

    fn outcome<T: Serialize>(result: Result<T, AuthorityMappingError>) -> Value {
        match result {
            Ok(value) => json!({"ok": value}),
            Err(AuthorityMappingError::Unhashable { kind }) => {
                crate::test_support::unhashable(kind)
            }
            Err(AuthorityMappingError::Json(_)) => json!({
                "error_type": "JSONDecodeError",
            }),
            Err(error) => json!({
                "error_type": error.error_type(),
                "error": error.to_string(),
            }),
        }
    }

    fn compare_map_cases(cases: Vec<Value>) {
        let expected = crate::test_support::oracle_script(
            "oracle_style_authority.py",
            "authority_mapping",
            &json!(cases),
        );
        if let Some(expected) = expected {
            let actual: Vec<Value> = cases
                .iter()
                .map(|case| {
                    let authority = case.get("authority").unwrap_or(&Value::Null);
                    match case.get("op").and_then(Value::as_str) {
                        Some("map") => outcome(map_delivery_authority(authority)),
                        Some("delegations") => outcome(map_delivery_delegations(authority)),
                        _ => json!({"error_type":"ValueError", "error":"unknown operation"}),
                    }
                })
                .collect();
            assert_eq!(json!(actual), expected);
        }
    }

    fn raw_budget_case(raw: &str, replacement: &str) -> String {
        let start = raw.find("\"budget\":{").expect("fixture has budget");
        let end = raw[start..]
            .find('}')
            .map(|offset| start + offset + 1)
            .expect("budget object closes");
        format!(
            "{}\"budget\":{{{replacement}}}{}",
            &raw[..start],
            &raw[end..]
        )
    }

    fn raw_outcome<T: Serialize>(result: Result<T, AuthorityMappingError>) -> Value {
        match result {
            Ok(value) => {
                let wire = serde_json::to_string(&value).expect("typed authority serialises");
                json!({"ok": value, "wire": wire})
            }
            Err(error) => outcome::<Value>(Err(error)),
        }
    }

    #[test]
    fn raw_json_keeps_budget_first_invalid_order_and_exact_wire() {
        let authority = delivery_fixture();
        let raw = serde_json::to_string(&authority).expect("authority fixture serialises");
        let reversed_valid_budget = raw_budget_case(&raw, "\"turns\":3,\"provider_calls\":2");
        let invalid_budget = raw_budget_case(&raw, "\"turns\":-1,\"cost:ZZZ\":1");
        let cases = json!([
            {"op":"raw_map", "raw":"{"},
            {"op":"raw_map", "raw":raw},
            {"op":"raw_map", "raw":reversed_valid_budget},
            {"op":"raw_map", "raw":invalid_budget}
        ]);
        let expected = crate::test_support::oracle_script(
            "oracle_style_authority.py",
            "authority_mapping",
            &cases,
        );
        if let Some(expected) = expected {
            let actual: Vec<Value> = cases
                .as_array()
                .expect("case array")
                .iter()
                .map(|case| {
                    let raw = case["raw"].as_str().expect("raw JSON string");
                    raw_outcome(map_delivery_authority_json(raw.as_bytes()))
                })
                .collect();
            assert_eq!(json!(actual), expected);
        }
    }

    #[test]
    fn raw_delegation_json_keeps_child_budget_order() {
        let mut authority = delivery_fixture();
        authority["delegations"] = json!([{
            "actor":"worker-1", "schema_version":2, "workspace_roots":["."],
            "allowed_source_paths":["input"], "allowed_artifact_paths":["output/worker-1"],
            "denied_paths":[".git"], "prohibited_actions":["external-release"],
            "disclosure":"local-only", "secrets_access":"none", "secret_refs":[],
            "deployment":false, "deployment_targets":[], "irreversible_actions":false,
            "irreversible_action_ids":[], "network":{"tool_egress":"none","allowed_hosts":[]},
            "expires_at":"2026-07-19T00:00:00Z", "budget":{"turns":2}
        }]);
        let raw = serde_json::to_string(&authority).expect("authority fixture serialises");
        let ordered_child = raw.replace(
            "\"budget\":{\"turns\":2}",
            "\"budget\":{\"provider_calls\":3,\"turns\":2}",
        );
        let invalid_child = ordered_child.replace(
            "\"budget\":{\"provider_calls\":3,\"turns\":2}",
            "\"budget\":{\"provider_calls\":-1,\"turns\":-2}",
        );
        let cases = json!([
            {"op":"raw_delegations", "raw":ordered_child},
            {"op":"raw_delegations", "raw":invalid_child}
        ]);
        let expected = crate::test_support::oracle_script(
            "oracle_style_authority.py",
            "authority_mapping",
            &cases,
        );
        if let Some(expected) = expected {
            let actual: Vec<Value> = cases
                .as_array()
                .expect("case array")
                .iter()
                .map(|case| {
                    let raw = case["raw"].as_str().expect("raw JSON string");
                    raw_outcome(map_delivery_delegations_json(raw.as_bytes()))
                })
                .collect();
            assert_eq!(json!(actual), expected);
        }
    }

    #[test]
    fn raw_authority_decoder_refuses_integers_outside_python_range() {
        assert!(matches!(
            map_delivery_authority_json(b"18446744073709551616"),
            Err(AuthorityMappingError::NumberOutOfRange)
        ));
    }

    #[test]
    fn maps_golden_and_timestamp_vectors_against_python() {
        let fixture = delivery_fixture();
        let timestamp_vectors: Value = serde_json::from_str(
            &std::fs::read_to_string(
                root().join("tests/fixtures/authority-envelope-v2/timestamp-vectors.json"),
            )
            .expect("timestamp vectors should exist"),
        )
        .expect("timestamp vectors should be JSON");
        let mut cases = vec![json!({"op":"map", "authority":fixture})];
        for timestamp in timestamp_vectors["accepted"]
            .as_array()
            .expect("accepted timestamps")
        {
            let mut authority = delivery_fixture();
            authority["expires_at"] = timestamp.clone();
            cases.push(json!({"op":"map", "authority":authority}));
        }
        for timestamp in timestamp_vectors["rejected"]
            .as_array()
            .expect("rejected timestamps")
        {
            let mut authority = delivery_fixture();
            authority["expires_at"] = timestamp.clone();
            cases.push(json!({"op":"map", "authority":authority}));
        }
        compare_map_cases(cases);
    }

    #[test]
    fn rejects_closed_fields_bounds_and_widened_delegations_against_python() {
        let mut cases = Vec::new();
        let mut missing = delivery_fixture();
        missing
            .as_object_mut()
            .expect("fixture object")
            .remove("network");
        cases.push(json!({"op":"map", "authority":missing}));
        let mut unknown = delivery_fixture();
        unknown["implicit_default"] = json!(true);
        cases.push(json!({"op":"map", "authority":unknown}));
        let mut bad_path = delivery_fixture();
        bad_path["allowed_source_paths"] = json!(["workspace/../outside"]);
        cases.push(json!({"op":"map", "authority":bad_path}));
        let mut bad_budget = delivery_fixture();
        bad_budget["budget"] = json!({"turns":-1});
        cases.push(json!({"op":"map", "authority":bad_budget}));
        let mut delegation = json!({
            "actor":"worker-1", "schema_version":2, "workspace_roots":["."],
            "allowed_source_paths":["input"], "allowed_artifact_paths":["output/worker-1"],
            "denied_paths":[".git"], "prohibited_actions":["external-release"],
            "disclosure":"local-only", "secrets_access":"none", "secret_refs":[],
            "deployment":false, "deployment_targets":[], "irreversible_actions":false,
            "irreversible_action_ids":[], "network":{"tool_egress":"none","allowed_hosts":[]},
            "expires_at":"2026-07-19T00:00:00Z", "budget":{"turns":2}
        });
        let mut parent = delivery_fixture();
        parent["delegations"] = json!([delegation.clone()]);
        cases.push(json!({"op":"delegations", "authority":parent}));
        delegation["network"] = json!({"tool_egress":"allowlist","allowed_hosts":["api.example.test","outside.example.test"]});
        let mut widened = delivery_fixture();
        widened["delegations"] = json!([delegation]);
        cases.push(json!({"op":"delegations", "authority":widened}));
        compare_map_cases(cases);
    }

    #[test]
    fn containment_matches_timestamp_order_vectors() {
        let base = serde_json::to_value(mapped_fixture()).expect("authority serialises");
        let vectors: Value = serde_json::from_str(
            &std::fs::read_to_string(
                root().join("tests/fixtures/authority-envelope-v2/timestamp-vectors.json"),
            )
            .expect("timestamp vectors should exist"),
        )
        .expect("timestamp vectors should be JSON");
        let cases: Vec<Value> = vectors["ordering"]
            .as_array()
            .expect("ordering vectors")
            .iter()
            .map(|vector| {
                let mut child = base.clone();
                child["expiresAt"] = vector["child"].clone();
                let mut parent = base.clone();
                parent["expiresAt"] = vector["parent"].clone();
                json!({"op":"contained", "child":child, "parent":parent})
            })
            .collect();
        let expected = crate::test_support::oracle_script(
            "oracle_style_authority.py",
            "authority_mapping",
            &json!(cases),
        );
        if let Some(expected) = expected {
            let actual: Vec<Value> = cases
                .iter()
                .map(|case| {
                    let child: MappedAuthority = serde_json::from_value(case["child"].clone())
                        .expect("mapped child is typed");
                    let parent: MappedAuthority = serde_json::from_value(case["parent"].clone())
                        .expect("mapped parent is typed");
                    outcome(authority_contained(&child, &parent))
                })
                .collect();
            assert_eq!(json!(actual), expected);
        }
    }
    #[test]
    fn policy_boundaries_and_each_containment_dimension_match_python() {
        let mut cases = vec![];
        for (field, values) in [
            ("schema_version", vec![json!(true), json!(2.0), json!(null)]),
            (
                "disclosure",
                vec![json!([]), json!("public"), json!("other")],
            ),
            ("secrets_access", vec![json!({}), json!("none")]),
            ("deployment", vec![json!(1), json!(false)]),
            ("irreversible_actions", vec![json!(null), json!(false)]),
            ("delegations", vec![json!({}), json!(null)]),
            (
                "expires_at",
                vec![
                    json!("٢٠٢٦-01-01T00:00:00Z"),
                    json!("2026-01-01T00:00:00.١Z"),
                ],
            ),
            (
                "allowed_source_paths",
                vec![
                    json!(["input\0escape"]),
                    json!(["x".repeat(4097)]),
                    json!(["input", "input"]),
                    json!(["input/./x"]),
                    json!(["input//x"]),
                    json!(["./input"]),
                ],
            ),
            (
                "budget",
                vec![
                    json!({"turns":true}),
                    json!({"turns":2.0}),
                    json!({"turns":9007199254740992_u64}),
                    json!({"cost:ZZZ":1}),
                ],
            ),
        ] {
            for value in values {
                let mut input = delivery_fixture();
                input[field] = value;
                cases.push(json!({"op":"map","authority":input}));
            }
        }
        for field in [
            "workspace_roots",
            "allowed_source_paths",
            "allowed_artifact_paths",
            "denied_paths",
            "prohibited_actions",
            "secret_refs",
            "deployment_targets",
            "irreversible_action_ids",
        ] {
            let count = if field == "workspace_roots" { 65 } else { 257 };
            let mut input = delivery_fixture();
            input[field] = json!((0..count).map(|i| format!("item-{i}")).collect::<Vec<_>>());
            cases.push(json!({"op":"map","authority":input}));
        }
        for count in [128, 129] {
            let mut input = delivery_fixture();
            input["budget"] = json!((0..count)
                .map(|i| (format!("input_tokens:provider-{i}"), json!(1)))
                .collect::<Map<_, _>>());
            cases.push(json!({"op":"map","authority":input}));
        }
        compare_map_cases(cases);
        let base = serde_json::to_value(mapped_fixture()).unwrap();
        let mut cases = vec![json!({"op":"contained","child":base,"parent":base})];
        for (field, value) in [
            (
                "approval",
                json!({"approvedBy":"other","evidenceId":"e","evidenceDigest":"d"}),
            ),
            ("sourcePaths", json!(["outside"])),
            ("artifactPaths", json!(["output-sibling"])),
            ("deniedPaths", json!([])),
            ("prohibitedActions", json!([])),
            ("disclosure", json!({"level":"allowed"})),
            ("disclosure", json!({"level":"scoped","scopes":["public"]})),
            (
                "secrets",
                json!({"access":"use-without-disclosure","references":["outside"]}),
            ),
            ("deployment", json!({"allowed":true,"targets":["outside"]})),
            (
                "irreversibleActions",
                json!({"allowed":true,"actionIds":["outside"]}),
            ),
            (
                "network",
                json!({"toolEgress":"allowlist","allowedHosts":["outside.test"]}),
            ),
            ("expiresAt", json!("2026-07-21T00:00:00Z")),
            ("budget", json!({"turns":5})),
            ("budget", json!({"wall_clock_milliseconds":1})),
        ] {
            let mut child = base.clone();
            child[field] = value;
            cases.push(json!({"op":"contained","child":child,"parent":base}));
        }
        let mut child = base.clone();
        let mut parent = base.clone();
        child["workspaceRoots"] = json!(["workspace-sibling"]);
        parent["workspaceRoots"] = json!(["workspace"]);
        cases.push(json!({"op":"contained","child":child,"parent":parent}));
        let actual: Vec<_> = cases
            .iter()
            .map(|c| {
                let child: MappedAuthority = serde_json::from_value(c["child"].clone()).unwrap();
                let parent: MappedAuthority = serde_json::from_value(c["parent"].clone()).unwrap();
                outcome(authority_contained(&child, &parent))
            })
            .collect();
        if let Some(expected) = crate::test_support::oracle_script(
            "oracle_style_authority.py",
            "authority_mapping",
            &json!(cases),
        ) {
            assert_eq!(json!(actual), expected);
        }
    }
    #[test]
    fn malformed_mapped_unions_never_grant_containment() {
        let base = mapped_fixture();
        let mut child = base.clone();
        child.deployment.targets = None;
        assert!(authority_contained(&child, &base).is_err());
        child = base.clone();
        child.irreversible_actions.action_ids = None;
        assert!(authority_contained(&child, &base).is_err());
        child = base.clone();
        child.expires_at = "bad".into();
        assert!(authority_contained(&child, &base).is_err());
        child = base.clone();
        child.disclosure.level = "bad".into();
        assert!(authority_contained(&child, &base).is_err());
        child = base.clone();
        child.network.allowed_hosts = None;
        assert!(authority_contained(&child, &base).is_err());
        child = base.clone();
        child.secrets.references = None;
        assert!(authority_contained(&child, &base).is_err());
    }
}
