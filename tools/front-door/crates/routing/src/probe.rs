//! Capability snapshot ingress and trust policy. Clocks are supplied by the caller.
use crate::{
    ordered_json::{OrderedObject, OrderedValue},
    pending::{self, JsonError},
};
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

struct Unique(OrderedValue);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Unique;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("JSON without duplicate members")
            }
            fn visit_unit<E: de::Error>(self) -> Result<Unique, E> {
                Ok(Unique(OrderedValue::Null))
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Unique, E> {
                Ok(Unique(OrderedValue::Bool(v)))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Unique, E> {
                Ok(Unique(OrderedValue::Number(v.into())))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Unique, E> {
                Ok(Unique(OrderedValue::Number(v.into())))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Unique, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| Unique(OrderedValue::Number(n)))
                    .ok_or_else(|| E::custom("nonfinite number"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Unique, E> {
                Ok(Unique(OrderedValue::String(v.into())))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Unique, A::Error> {
                let mut values = vec![];
                while let Some(Unique(v)) = seq.next_element()? {
                    values.push(v);
                }
                Ok(Unique(OrderedValue::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Unique, A::Error> {
                let mut values = OrderedObject::default();
                while let Some((k, Unique(v))) = map.next_entry::<String, Unique>()? {
                    if values.get(&k).is_some() {
                        return Err(de::Error::custom(format!("duplicate JSON member: {k}")));
                    }
                    values.insert(k, v);
                }
                Ok(Unique(OrderedValue::Object(values)))
            }
        }
        deserializer.deserialize_any(V)
    }
}
pub fn load_json(raw: &str) -> Result<OrderedValue, JsonError> {
    pending::from_json::<Unique>(raw).map(|v| v.0)
}

/// Accept provider model listings, never arbitrary help or notice text.
pub fn listed_models(raw: &str, adapter: Option<&str>) -> Result<Vec<String>, JsonError> {
    let parsed = match pending::from_json::<OrderedValue>(raw) {
        Ok(value) => Some(value),
        Err(error @ JsonError::NumberOutOfRange(_)) => return Err(error),
        Err(_) => None,
    };
    if parsed
        .as_ref()
        .is_some_and(|v| !matches!(v, OrderedValue::Null))
        && !matches!(adapter, None | Some("codex" | "kiro"))
    {
        return Ok(vec![]);
    }
    let mut data = parsed;
    if let Some(OrderedValue::Object(map)) = data.as_ref() {
        data = map.get("models").or_else(|| map.get("data")).cloned();
    }
    if let Some(OrderedValue::Object(map)) = data.as_ref() {
        if map
            .0
            .iter()
            .any(|(_, v)| !matches!(v, OrderedValue::Object(_)))
        {
            return Ok(vec![]);
        }
        data = Some(OrderedValue::Array(
            map.0
                .iter()
                .map(|(key, _)| OrderedValue::String(key.clone()))
                .collect(),
        ));
    }
    if let Some(OrderedValue::Array(items)) = data {
        let valid = regex::Regex::new(r"\A[A-Za-z0-9_./:~-]+\z")
            .map_err(|e| JsonError::Json(<serde_json::Error as de::Error>::custom(e)))?;
        let mut models = vec![];
        for item in items {
            let name = match &item {
                OrderedValue::String(s) => Some(s.as_str()),
                OrderedValue::Object(map) => ["id", "model_id", "slug", "name"]
                    .into_iter()
                    .find_map(|key| map.get(key))
                    .and_then(OrderedValue::as_str),
                _ => None,
            };
            let Some(name) = name.filter(|s| valid.is_match(s)) else {
                return Ok(vec![]);
            };
            if !models.iter().any(|m| m == name) {
                models.push(name.into());
            }
        }
        return Ok(models);
    }
    if matches!(adapter, Some("codex" | "kiro")) {
        return Ok(vec![]);
    }
    let pattern=regex::Regex::new(r"\A(?:[-*][\s\x1c-\x1f]+)?([A-Za-z](?:[A-Za-z0-9._/~-]*[A-Za-z0-9])?(?::[A-Za-z0-9._~-]+)*)(?:(?:\t| {2,}| - )[^\s\x1c-\x1f].*)?\z").map_err(|e|JsonError::Json(<serde_json::Error as de::Error>::custom(e)))?;
    let has_number = regex::Regex::new(r"[-/.\d]")
        .map_err(|e| JsonError::Json(<serde_json::Error as de::Error>::custom(e)))?;
    let mut listed = vec![];
    let mut gap = false;
    for line in raw.replace("\r\n", "\n").split([
        '\n', '\r', '\x0b', '\x0c', '\x1c', '\x1d', '\x1e', '\u{85}', '\u{2028}', '\u{2029}',
    ]) {
        let line = pending::python_strip(line);
        if line.is_empty() {
            gap = !listed.is_empty();
            continue;
        }
        if [
            "Fetching available models...",
            "Available models",
            "Available models:",
            "Models:",
        ]
        .contains(&line)
            || line.starts_with("Tip: use --model ")
        {
            continue;
        }
        let name = pattern
            .captures(line)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str())
            .filter(|m| *m == "auto" || has_number.is_match(m));
        let Some(name) = name else {
            if gap {
                break;
            }
            return Ok(vec![]);
        };
        if adapter == Some("opencode") && !name.contains('/') {
            return Ok(vec![]);
        }
        if !listed.iter().any(|m| m == name) {
            listed.push(name.into());
        }
        gap = false;
    }
    Ok(listed)
}

pub fn kiro_probe_enforced(evidence: &Value) -> bool {
    evidence.is_object()
        && evidence.get("attempted_write") == Some(&Value::Bool(true))
        && evidence.get("permission_denied") == Some(&Value::Bool(true))
        && evidence.get("file_created") == Some(&Value::Bool(false))
}

#[cfg(test)]
mod tests;
