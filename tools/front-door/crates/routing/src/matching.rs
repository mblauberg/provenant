//! Model identity resolution. Catalogue and fresh listing observations are explicit inputs.
use crate::{casefold::fold, pending::python_strip};
use regex::Regex;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MatchError {
    #[error("malformed model catalogue: {0}")]
    Catalog(&'static str),
    #[error("model version integer is outside i64/u64 range")]
    NumberOutOfRange,
    #[error(transparent)]
    Regex(#[from] regex::Error),
}

pub type ModelMatch = (Option<Value>, Vec<String>);

pub struct ModelCatalog<'a> {
    pub catalog: &'a Value,
    /// Fresh model listings, already filtered by the cache owner.
    pub cached: &'a BTreeMap<String, Vec<String>>,
}

fn key(text: &str) -> String {
    fold(text)
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect()
}
fn family_parts(model: &str) -> Result<Vec<String>, MatchError> {
    let version = Regex::new(r"^v?\d+(?:\.\d+)*$")?;
    let folded = fold(model);
    let parts: Vec<_> = folded
        .split(|c: char| c == '-' || c == '_' || python_strip(&c.to_string()).is_empty())
        .filter(|part| !part.is_empty() && !version.is_match(part))
        .map(str::to_owned)
        .collect();
    Ok(if parts.is_empty() {
        vec![String::new()]
    } else {
        parts
    })
}
fn family_key(model: &str) -> Result<String, MatchError> {
    Ok(key(&family_parts(model)?.concat()))
}
fn version(model: &str) -> Result<Vec<u64>, MatchError> {
    Regex::new(r"\d+")?
        .find_iter(model)
        .map(|m| {
            let digits: String = m
                .as_str()
                .chars()
                .map(|c| {
                    provenant_core::py::decimal_digit(c)
                        .and_then(|d| char::from_digit(u32::from(d), 10))
                })
                .collect::<Option<String>>()
                .ok_or(MatchError::NumberOutOfRange)?;
            digits.parse().map_err(|_| MatchError::NumberOutOfRange)
        })
        .collect()
}
fn id(entry: &Value) -> Result<&str, MatchError> {
    entry
        .get("id")
        .and_then(Value::as_str)
        .ok_or(MatchError::Catalog("model id"))
}
fn strings<'a>(entry: &'a Value, field: &str) -> Result<Vec<&'a str>, MatchError> {
    match entry.get(field) {
        None => Ok(vec![]),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| v.as_str().ok_or(MatchError::Catalog("model string list")))
            .collect(),
        _ => Err(MatchError::Catalog("model string list")),
    }
}
fn live_entry(base: &Value, model: &str) -> Result<Value, MatchError> {
    let mut result = base
        .as_object()
        .cloned()
        .ok_or(MatchError::Catalog("model entry"))?;
    let mut inherits = vec![id(base)?.to_owned()];
    inherits.extend(strings(base, "names")?.into_iter().map(str::to_owned));
    result.insert("id".into(), json!(model));
    result.insert("names".into(), json!([]));
    result.insert("default".into(), json!(false));
    result.insert("inherits".into(), json!(inherits));
    Ok(Value::Object(result))
}
fn newer(model: &str) -> String {
    format!("{model} is newer than the catalogue; run provenant refresh-routing or add it to config/model-routing.json")
}
fn newest<'a>(
    entries: impl IntoIterator<Item = &'a Value>,
) -> Result<Option<&'a Value>, MatchError> {
    let mut best: Option<(&Value, Vec<u64>)> = None;
    for entry in entries {
        let rank = version(id(entry)?)?;
        if best.as_ref().is_none_or(|(_, r)| rank > *r) {
            best = Some((entry, rank));
        }
    }
    Ok(best.map(|(entry, _)| entry))
}
fn suffixes(entry: &Value) -> Result<Vec<(&str, &str)>, MatchError> {
    if entry.get("effort_transport").and_then(Value::as_str) != Some("model-suffix") {
        return Ok(vec![]);
    }
    match entry.get("suffix") {
        None => Ok(vec![]),
        Some(Value::Object(map)) => map
            .iter()
            .map(|(k, v)| {
                Ok((
                    k.as_str(),
                    v.as_str().ok_or(MatchError::Catalog("effort suffix"))?,
                ))
            })
            .collect(),
        _ => Err(MatchError::Catalog("effort suffix map")),
    }
}

impl ModelCatalog<'_> {
    pub fn adapter(&self, adapter: &str) -> Result<&Value, MatchError> {
        self.catalog
            .get("adapters")
            .and_then(|v| v.get(adapter))
            .filter(|v| v.is_object())
            .ok_or(MatchError::Catalog("adapter"))
    }
    fn entries(&self, adapter: &str) -> Result<Vec<&Value>, MatchError> {
        match self.adapter(adapter)?.get("models") {
            None => Ok(vec![]),
            Some(Value::Array(items)) => Ok(items.iter().collect()),
            _ => Err(MatchError::Catalog("model list")),
        }
    }
    pub fn live_base(&self, adapter: &str, model: &str) -> Result<String, MatchError> {
        let mut all = BTreeSet::new();
        for entry in self.entries(adapter)? {
            for (_, suffix) in suffixes(entry)? {
                all.insert(suffix);
            }
        }
        let mut all: Vec<_> = all.into_iter().collect();
        all.sort_by_key(|suffix| std::cmp::Reverse(suffix.chars().count()));
        for suffix in all {
            if !suffix.is_empty() && fold(model).ends_with(&fold(suffix)) {
                // Python slices by code points, including case-fold expansions.
                return Ok(model
                    .chars()
                    .take(model.chars().count().saturating_sub(suffix.chars().count()))
                    .collect());
            }
        }
        Ok(model.into())
    }
    pub fn suffix_effort(&self, adapter: &str, requested: &str) -> Result<String, MatchError> {
        let base = self.live_base(adapter, requested)?;
        let tail = fold(
            &requested
                .chars()
                .skip(base.chars().count())
                .collect::<String>(),
        );
        let mut levels: Vec<(String, String)> = vec![];
        for entry in self.entries(adapter)? {
            for (level, value) in suffixes(entry)? {
                if let Some((_, old)) = levels.iter_mut().find(|(l, _)| l == level) {
                    *old = fold(value);
                } else {
                    levels.push((level.into(), fold(value)));
                }
            }
        }
        Ok(levels
            .into_iter()
            .find(|(_, v)| !tail.is_empty() && v == &tail)
            .map(|(l, _)| l)
            .unwrap_or_default())
    }
    /// Ordered ingress for model-suffix transports. First level position is
    /// retained while later occurrences replace its suffix, like a Python dict.
    pub fn suffix_effort_ordered(
        &self,
        adapter: &str,
        requested: &str,
        catalog: &crate::ordered_json::OrderedValue,
    ) -> Result<String, MatchError> {
        use crate::ordered_json::OrderedValue;
        let base = self.live_base(adapter, requested)?;
        let tail = fold(
            &requested
                .chars()
                .skip(base.chars().count())
                .collect::<String>(),
        );
        let entries = catalog
            .get("adapters")
            .and_then(|a| a.get(adapter))
            .and_then(|a| a.get("models"));
        let mut levels: Vec<(String, String)> = Vec::new();
        if let Some(OrderedValue::Array(entries)) = entries {
            for entry in entries.iter().filter(|e| {
                e.get("effort_transport").and_then(OrderedValue::as_str) == Some("model-suffix")
            }) {
                if let Some(suffixes) = entry.get("suffix").and_then(OrderedValue::as_object) {
                    for (level, suffix) in &suffixes.0 {
                        let suffix = suffix
                            .as_str()
                            .ok_or(MatchError::Catalog("effort suffix"))?;
                        if let Some((_, old)) = levels.iter_mut().find(|(key, _)| key == level) {
                            *old = fold(suffix);
                        } else {
                            levels.push((level.clone(), fold(suffix)));
                        }
                    }
                }
            }
        }
        Ok(levels
            .into_iter()
            .find(|(_, suffix)| !tail.is_empty() && suffix == &tail)
            .map(|(level, _)| level)
            .unwrap_or_default())
    }
    fn latest(&self, adapter: &str, requested: &str) -> Result<ModelMatch, MatchError> {
        let entries = self.entries(adapter)?;
        let token = key(requested);
        if token.is_empty() || Regex::new(r"\d")?.is_match(requested) {
            return Ok((None, vec![]));
        }
        let mut named = vec![];
        for entry in &entries {
            if strings(entry, "names")?.iter().any(|n| key(n) == token) {
                named.push(*entry);
            }
        }
        let pool = if named.is_empty() {
            let mut pool = vec![];
            for entry in &entries {
                let name = id(entry)?;
                if family_key(name)? == token
                    || key(family_parts(name)?
                        .last()
                        .map(String::as_str)
                        .unwrap_or_default())
                        == token
                {
                    pool.push(*entry);
                }
            }
            pool
        } else {
            named
        };
        let keys: BTreeSet<_> = pool
            .iter()
            .map(|e| family_key(id(e)?))
            .collect::<Result<_, MatchError>>()?;
        if keys.len() != 1 {
            return Ok((None, vec![]));
        }
        let family = keys.into_iter().next().unwrap_or_default();
        let mut related = vec![];
        for entry in entries {
            if family_key(id(entry)?)? == family {
                related.push(entry);
            }
        }
        let Some(best) = newest(related)? else {
            return Ok((None, vec![]));
        };
        let mut live: Option<(String, Vec<u64>)> = None;
        for model in self.cached.get(adapter).into_iter().flatten() {
            let base = self.live_base(adapter, model)?;
            let rank = version(&base)?;
            if family_key(&base)? == family
                && rank > version(id(best)?)?
                && live.as_ref().is_none_or(|(_, r)| rank > *r)
            {
                live = Some((base, rank));
            }
        }
        if let Some((model, _)) = live {
            Ok((Some(live_entry(best, &model)?), vec![newer(&model)]))
        } else {
            Ok((Some(best.clone()), vec![]))
        }
    }
    fn live_id(&self, adapter: &str, requested: &str) -> Result<ModelMatch, MatchError> {
        let base = self.live_base(adapter, requested)?;
        let entries = self.entries(adapter)?;
        for entry in &entries {
            if fold(id(entry)?) == fold(&base) {
                return Ok((None, vec![]));
            }
        }
        let mut found = false;
        for model in self.cached.get(adapter).into_iter().flatten() {
            if fold(&self.live_base(adapter, model)?) == fold(&base) {
                found = true;
                break;
            }
        }
        if !found {
            return Ok((None, vec![]));
        }
        let mut family = vec![];
        for entry in entries {
            if family_key(id(entry)?)? == family_key(&base)? {
                family.push(entry);
            }
        }
        Ok(match newest(family)? {
            Some(best) => (Some(live_entry(best, &base)?), vec![newer(&base)]),
            None => (None, vec![]),
        })
    }
    pub fn registered_match(
        &self,
        adapter: &str,
        requested: &str,
    ) -> Result<ModelMatch, MatchError> {
        let entries = self.entries(adapter)?;
        let token = fold(requested);
        let (mut exact, mut named, mut variant, mut retired) = (vec![], vec![], vec![], vec![]);
        for entry in &entries {
            let name = id(entry)?;
            if fold(name) == token {
                exact.push(*entry);
            }
            if strings(entry, "names")?.iter().any(|n| fold(n) == token) {
                named.push(*entry);
            }
            if suffixes(entry)?
                .iter()
                .any(|(_, s)| fold(&format!("{name}{s}")) == token)
            {
                variant.push(*entry);
            }
        }
        if exact.is_empty() && named.is_empty() && variant.is_empty() && adapter == "codex" {
            let pattern = Regex::new(r"^gpt-(\d+(?:\.\d+)*)-(.+)$")?;
            if let Some(request) = pattern.captures(&token) {
                for entry in &entries {
                    if let Some(candidate) = pattern.captures(&fold(id(entry)?)) {
                        if candidate.get(2).map(|m| m.as_str())
                            == request.get(2).map(|m| m.as_str())
                            && version(request.get(1).map(|m| m.as_str()).unwrap_or_default())?
                                < version(candidate.get(1).map(|m| m.as_str()).unwrap_or_default())?
                        {
                            retired.push(*entry);
                        }
                    }
                }
            }
        }
        let latest = self.adapter(adapter)?.get("latest_aliases") == Some(&Value::Bool(true));
        if exact.is_empty() && variant.is_empty() && latest {
            let (entry, mut notes) = self.latest(adapter, requested)?;
            if let Some(entry) = entry {
                if named.len() > 1 {
                    notes.insert(0, ambiguity(requested, &entry, &named)?);
                }
                return Ok((Some(entry), notes));
            }
        }
        let was_retired = !retired.is_empty();
        let matches = if !exact.is_empty() {
            exact
        } else if !named.is_empty() {
            named
        } else if !variant.is_empty() {
            variant
        } else {
            retired
        };
        if matches.is_empty() {
            return if latest {
                self.live_id(adapter, requested)
            } else {
                Ok((None, vec![]))
            };
        }
        let chosen = matches
            .iter()
            .copied()
            .find(|e| crate::pending::value_truthy(e.get("default").unwrap_or(&Value::Null)));
        let chosen = match chosen {
            Some(chosen) => Some(chosen),
            None => newest(matches.iter().copied())?,
        };
        let Some(chosen) = chosen else {
            return Ok((None, vec![]));
        };
        let mut notes = vec![];
        if matches.len() > 1 {
            notes.push(ambiguity(requested, chosen, &matches)?);
        }
        if was_retired {
            notes.push(format!("{requested} is retired; routed to {}", id(chosen)?));
        }
        Ok((Some(chosen.clone()), notes))
    }
    pub fn owner_adapter(&self, requested: &str) -> Result<String, MatchError> {
        let token = fold(requested);
        if ["opencode/", "opencode-go/", "openrouter/"]
            .iter()
            .any(|p| token.starts_with(p))
        {
            return Ok("opencode".into());
        }
        for adapter in ["codex", "claude", "agy", "cursor", "opencode"] {
            if self
                .catalog
                .get("adapters")
                .and_then(|v| v.get(adapter))
                .is_some()
                && self.registered_match(adapter, requested)?.0.is_some()
            {
                return Ok(adapter.into());
            }
        }
        for (adapter, prefixes) in [
            ("codex", vec!["gpt-", "o1", "o3", "o4"]),
            ("claude", vec!["claude-"]),
            ("agy", vec!["gemini-"]),
            ("cursor", vec!["grok-", "cursor-", "composer-"]),
        ] {
            if prefixes.iter().any(|p| token.starts_with(p)) {
                return Ok(adapter.into());
            }
        }
        Ok("opencode".into())
    }
    pub fn canonical_model(&self, adapter: &str, model: &str) -> Result<String, MatchError> {
        if model == "*"
            || self
                .catalog
                .get("adapters")
                .and_then(|v| v.get(adapter))
                .is_none()
        {
            return Ok(model.into());
        }
        Ok(match self.registered_match(adapter, model)?.0 {
            Some(entry) => id(&entry)?.into(),
            None => model.into(),
        })
    }
}
fn ambiguity(requested: &str, chosen: &Value, matches: &[&Value]) -> Result<String, MatchError> {
    Ok(format!(
        "{requested} is ambiguous; used {} (alternatives: {})",
        id(chosen)?,
        matches
            .iter()
            .map(|e| id(e))
            .collect::<Result<Vec<_>, _>>()?
            .join(", ")
    ))
}

pub fn is_free_model(adapter: &Value, model: &str) -> Result<bool, MatchError> {
    Ok(match adapter.get("free_pattern").and_then(Value::as_str) {
        Some(p) => Regex::new(p)?.is_match(model),
        None => false,
    })
}
pub fn looks_free(adapter: &Value, model: &str) -> Result<bool, MatchError> {
    Ok(is_free_model(adapter, model)?
        || model.ends_with("-free")
        || model.ends_with(":free")
        || model.starts_with("opencode/"))
}
pub fn training_flag(
    adapter: &Value,
    registered: Option<&Value>,
    model: &str,
) -> Result<Option<bool>, MatchError> {
    if let Some(own) = registered
        .and_then(|v| v.get("trains_on_prompts"))
        .and_then(Value::as_bool)
    {
        return Ok(Some(own));
    }
    let value = adapter.get("trains_on_prompts").and_then(Value::as_bool);
    Ok(
        if value == Some(false) && !model.is_empty() && looks_free(adapter, model)? {
            None
        } else {
            value
        },
    )
}
pub fn no_effort_control(entry: Option<&Value>) -> bool {
    entry.filter(|e| e.is_object()).is_some_and(|e| {
        e.get("effort_transport").and_then(Value::as_str) == Some("none")
            || !crate::pending::value_truthy(e.get("efforts").unwrap_or(&Value::Null))
    })
}
pub fn effort_ignored(effort: &str, model: &str) -> String {
    format!("effort {effort} ignored: {model} has no effort control")
}

#[cfg(test)]
mod tests;
