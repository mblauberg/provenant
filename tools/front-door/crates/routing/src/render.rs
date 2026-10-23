//! Human-readable weighted pool listing, preserving source dictionary order.
use crate::{
    ordered_json::{OrderedObject, OrderedValue},
    pending::ordered_string,
};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RenderError {
    #[error("missing route listing field: {0}")]
    Missing(&'static str),
    #[error("invalid route listing field: {0}")]
    Invalid(&'static str),
}
fn field<'a>(
    object: &'a OrderedObject,
    key: &'static str,
) -> Result<&'a OrderedValue, RenderError> {
    object.get(key).ok_or(RenderError::Missing(key))
}
fn text<'a>(object: &'a OrderedObject, key: &'static str) -> Result<&'a str, RenderError> {
    field(object, key)?
        .as_str()
        .ok_or(RenderError::Invalid(key))
}
fn object<'a>(
    value: &'a OrderedValue,
    field: &'static str,
) -> Result<&'a OrderedObject, RenderError> {
    value.as_object().ok_or(RenderError::Invalid(field))
}
fn array<'a>(
    value: &'a OrderedValue,
    field: &'static str,
) -> Result<&'a [OrderedValue], RenderError> {
    match value {
        OrderedValue::Array(items) => Ok(items),
        _ => Err(RenderError::Invalid(field)),
    }
}
fn pad(text: &str, width: usize) -> String {
    format!(
        "{text}{}",
        " ".repeat(width.saturating_sub(text.chars().count()))
    )
}

pub fn render(document: &OrderedValue, full: bool) -> Result<String, RenderError> {
    let document = object(document, "document")?;
    let mut lines = vec![];
    for (name, entries) in &object(field(document, "routes")?, "routes")?.0 {
        lines.push(name.clone());
        let mut hidden = 0;
        let mut cooling = vec![];
        for entry in array(entries, "entries")? {
            let entry = object(entry, "entry")?;
            let availability = text(entry, "availability")?;
            if !full && availability != "available" {
                hidden += 1;
                if let Some((_, until)) = availability.split_once("cooling until ") {
                    cooling.push((until.to_owned(), text(entry, "model")?.to_owned()));
                }
                continue;
            }
            let model = format!(
                "{}{}",
                text(entry, "model")?,
                entry
                    .get("effort")
                    .map(|e| format!("@{}", ordered_string(e)))
                    .unwrap_or_default()
            );
            let mut caveat = String::new();
            if let Some(traits) = entry.get("traits") {
                let traits = array(traits, "traits")?
                    .iter()
                    .map(|v| v.as_str().ok_or(RenderError::Invalid("trait")))
                    .collect::<Result<Vec<_>, _>>()?;
                caveat = format!("  [{}]", traits.join(", "));
            }
            if let Some(quota) = entry.get("quota") {
                let quota = object(quota, "quota")?;
                if quota
                    .get("state")
                    .and_then(OrderedValue::as_str)
                    .is_some_and(|s| ["ahead_of_pace", "deficit", "credit_only"].contains(&s))
                {
                    caveat.push_str(&format!(
                        "  [quota {}]",
                        ordered_string(field(quota, "summary")?)
                    ));
                }
            }
            lines.push(format!(
                "  {} {} {availability}{caveat}",
                pad(&model, 52),
                pad(&ordered_string(field(entry, "weight")?), 8)
            ));
        }
        if hidden > 0 {
            cooling.sort();
            lines.push(format!(
                "  {hidden} unavailable or cooling{}; use --all",
                cooling
                    .first()
                    .map(|(until, _)| format!("; soonest cooling until {until}"))
                    .unwrap_or_default()
            ));
            if !cooling.is_empty() {
                lines.push(format!(
                    "  cooling: {}",
                    cooling
                        .into_iter()
                        .map(|(_, model)| model)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
    }
    let mut synonyms: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for (alias, target) in &object(field(document, "synonyms")?, "synonyms")?.0 {
        synonyms
            .entry(
                target
                    .as_str()
                    .ok_or(RenderError::Invalid("synonym target"))?
                    .into(),
            )
            .or_default()
            .push(alias);
    }
    if !synonyms.is_empty() {
        lines.push(format!(
            "synonyms: {}",
            synonyms
                .into_iter()
                .map(|(target, names)| format!("{target} <- {}", names.join(", ")))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    #[test]
    fn listing_bytes_match_python_with_hidden_cooling_quota_unicode_and_presence() {
        let raw = r#"{"routes":{"senior":[{"model":"codex/gpt-6@high","weight":4,"availability":"available"},{"model":"日本語","weight":1.0,"availability":"available","effort":null,"traits":[],"quota":{"state":"deficit","summary":"low"}},{"model":"z","weight":"off","availability":"disabled"},{"model":"b","availability":"cooling until 2026-10-24"},{"model":"a","availability":"cooling until 2026-10-24"}],"empty":[]},"synonyms":{"z":"senior","a":"empty","q":"senior"}}"#;
        let document: OrderedValue = crate::pending::from_json(raw).unwrap();
        let Some(expected): Option<Value> = crate::oracle::python(
            r#"
import importlib.util,json,pathlib,sys
s=importlib.util.spec_from_file_location('pools',pathlib.Path('scripts/model_route_pools.py'));m=importlib.util.module_from_spec(s);s.loader.exec_module(m)
d=json.load(sys.stdin);print(json.dumps(m.render(json.loads(d))))
"#,
            &json!(raw),
        ) else {
            return;
        };
        assert_eq!(json!(render(&document, false).unwrap()), expected);
        assert!(render(&document, true).is_err()); // hidden rows lack weight, as in Python.
        let raw = r#"{"routes":{"x":[{"model":"m","weight":true,"availability":"disabled","traits":["a","b"]}]},"synonyms":{}}"#;
        let doc = crate::pending::from_json(raw).unwrap();
        let Some(expected): Option<String> = crate::oracle::python(
            r#"
import importlib.util,json,pathlib,sys
s=importlib.util.spec_from_file_location('pools',pathlib.Path('scripts/model_route_pools.py'));m=importlib.util.module_from_spec(s);s.loader.exec_module(m)
print(json.dumps(m.render(json.loads(json.load(sys.stdin)),True)))
"#,
            &json!(raw),
        ) else {
            return;
        };
        assert_eq!(render(&doc, true).unwrap(), expected);
        assert!(render(&OrderedValue::Null, false).is_err());
    }
}
