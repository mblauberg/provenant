//! Fallback routes and live OpenCode listing decisions. Provider execution is an injected edge.
use crate::{
    casefold::fold,
    matching::{MatchError, ModelCatalog, training_flag},
    ordered_json::OrderedValue,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FallbackRoute {
    pub adapter: String,
    pub model: String,
    pub plan_cap_usd: Value,
    pub trains_on_prompts: Option<bool>,
}
pub struct FallbackRequest<'a> {
    pub adapter: &'a str,
    pub tier: &'a str,
    pub model: &'a str,
    pub fallback: Option<&'a str>,
    pub requested_routes: &'a [String],
    pub explicit: bool,
}
fn list(value: Option<&Value>) -> Result<Vec<String>, MatchError> {
    match value {
        None => Ok(vec![]),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or(MatchError::Catalog("alias candidate"))
            })
            .collect(),
        _ => Err(MatchError::Catalog("alias candidates")),
    }
}

pub fn fallback_candidates(
    request: &FallbackRequest<'_>,
    resolver: &ModelCatalog<'_>,
    ordered_catalog: &OrderedValue,
    cooldowns: &crate::ordered_json::OrderedObject,
    now: jiff::Timestamp,
) -> Result<Vec<FallbackRoute>, MatchError> {
    let adapter = resolver.adapter(request.adapter)?;
    let mut tiers = adapter.get("aliases");
    if tiers.is_none_or(|v| !crate::pending::value_truthy(v)) {
        if let Some(family) = adapter
            .get("fixed_model_family")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            tiers = resolver
                .catalog
                .get("families")
                .and_then(|f| f.get(family))
                .and_then(|f| f.get("aliases"));
        }
    }
    if request.fallback == Some("false")
        || (request.explicit && request.fallback.is_none() && request.requested_routes.is_empty())
    {
        return Ok(vec![]);
    }
    let own = list(tiers.and_then(|t| t.get(request.tier)))?;
    let current = resolver.canonical_model(request.adapter, request.model)?;
    let mut candidates: Vec<_> = own
        .into_iter()
        .map(|m| (request.adapter.to_owned(), m))
        .collect();
    let opencode = resolver
        .catalog
        .get("adapters")
        .and_then(|a| a.get("opencode"));
    if request.adapter != "opencode" {
        let tiers = opencode.and_then(|a| a.get("aliases"));
        let own = tiers
            .and_then(|t| t.get(request.tier))
            .filter(|v| crate::pending::value_truthy(v))
            .or_else(|| tiers.and_then(|t| t.get("senior")));
        candidates.extend(list(own)?.into_iter().map(|m| ("opencode".into(), m)));
    }
    if request.fallback == Some("any") {
        if let Some(models) = opencode.and_then(|a| a.get("models")) {
            let models = models
                .as_array()
                .ok_or(MatchError::Catalog("opencode models"))?;
            for entry in models {
                let id = entry
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or(MatchError::Catalog("model id"))?;
                candidates.push(("opencode".into(), id.into()));
            }
        }
    }
    for route in request.requested_routes {
        let adapter = ordered_catalog
            .get("adapters")
            .and_then(OrderedValue::as_object)
            .and_then(|map| {
                map.0
                    .iter()
                    .find(|(name, _)| route.starts_with(&format!("{name}/")))
            })
            .map(|(name, _)| name.as_str())
            .unwrap_or(request.adapter);
        let candidate = route.strip_prefix(&format!("{adapter}/")).unwrap_or(route);
        candidates.push((adapter.into(), candidate.into()));
    }
    let mut seen = BTreeSet::from([(request.adapter.to_owned(), current)]);
    let mut results = vec![];
    for (adapter, model) in candidates {
        let canonical = resolver.canonical_model(&adapter, &model)?;
        if !seen.insert((adapter.clone(), canonical.clone())) {
            continue;
        }
        let entry = resolver.registered_match(&adapter, &model)?.0;
        let opt_in = request.fallback == Some("any")
            || request
                .requested_routes
                .contains(&format!("{adapter}/{model}"))
            || request.requested_routes.contains(&model);
        let config = resolver
            .catalog
            .get("adapters")
            .and_then(|a| a.get(&adapter))
            .unwrap_or(&Value::Null);
        let flag = training_flag(config, entry.as_ref(), "")?;
        let Some(entry) = entry else {
            continue;
        };
        let allowed = if opt_in || flag == Some(false) {
            true
        } else {
            let positive = match entry.get("plan_cap_usd") {
                None => true,
                Some(Value::Bool(b)) => *b,
                Some(Value::Number(n)) if n.is_f64() => n.as_f64().is_some_and(|v| v > 0.0),
                Some(Value::Number(n)) => n.as_u64().is_some_and(|v| v > 0),
                _ => return Err(MatchError::Catalog("plan cap")),
            };
            positive && flag != Some(true)
        };
        if allowed
            && crate::live::cooling(&adapter, &canonical, cooldowns, resolver, now)?.is_empty()
        {
            results.push(FallbackRoute {
                adapter,
                model: canonical,
                plan_cap_usd: entry.get("plan_cap_usd").cloned().unwrap_or(Value::Null),
                trains_on_prompts: flag,
            });
        }
    }
    Ok(results)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpencodeRefusal {
    pub status: String,
    pub candidates: Vec<String>,
    pub message: String,
}
pub type OpencodeResolution = (Option<String>, Vec<String>, Option<OpencodeRefusal>);
pub fn resolve_opencode_name(requested: &str, live: &[String]) -> OpencodeResolution {
    let token = fold(requested);
    if live.is_empty() || requested.contains(['/', '#']) || token.is_empty() {
        return (None, vec![], None);
    }
    let tails: Vec<_> = live
        .iter()
        .map(|item| {
            (
                item,
                fold(item.split_once('/').map(|(_, tail)| tail).unwrap_or(item)),
            )
        })
        .collect();
    let exact: Vec<_> = tails
        .iter()
        .filter(|(_, tail)| tail == &token)
        .map(|(item, _)| *item)
        .collect();
    let near = if exact.is_empty() {
        tails
            .iter()
            .filter(|(_, tail)| tail.starts_with(&token))
            .map(|(item, _)| *item)
            .collect::<Vec<_>>()
    } else {
        exact.clone()
    };
    if let [matched] = near.as_slice() {
        let model = (*matched).clone();
        let how = if exact.is_empty() {
            " (unique prefix match)"
        } else {
            ""
        };
        return (
            Some(model.clone()),
            vec![format!(
                "{requested} resolved to {model} from the live opencode listing{how}"
            )],
            None,
        );
    }
    if !near.is_empty() {
        let names: Vec<_> = near.into_iter().take(12).cloned().collect();
        return (
            None,
            vec![],
            Some(OpencodeRefusal {
                status: "model_ambiguous".into(),
                message: format!(
                    "{requested} matches several opencode models: {}; fix: pass the full provider/model id",
                    names.join(", ")
                ),
                candidates: names,
            }),
        );
    }
    let mut closest: Vec<_> = tails
        .iter()
        .map(|(_, tail)| (similarity(tail, &token), tail))
        .filter(|(ratio, _)| *ratio >= 0.6)
        .collect();
    closest.sort_by(|(ar, at), (br, bt)| br.total_cmp(ar).then_with(|| bt.cmp(at)));
    closest.truncate(3);
    let names: Vec<_> = tails
        .iter()
        .filter(|(_, tail)| closest.iter().any(|(_, name)| *name == tail))
        .map(|(item, _)| (*item).clone())
        .collect();
    let hint = if names.is_empty() {
        String::new()
    } else {
        format!(" Closest: {}.", names.join(", "))
    };
    (
        None,
        vec![],
        Some(OpencodeRefusal {
            status: "model_not_found".into(),
            candidates: names,
            message: format!(
                "{requested} is not an opencode model.{hint} fix: run `opencode models` and pass a full provider/model id"
            ),
        }),
    )
}

// CPython difflib.SequenceMatcher without a junk predicate, including autojunk.
fn similarity(a: &str, b: &str) -> f64 {
    let a: Vec<_> = a.chars().collect();
    let b: Vec<_> = b.chars().collect();
    if a.len() + b.len() == 0 {
        return 1.0;
    }
    let mut positions: BTreeMap<char, Vec<usize>> = BTreeMap::new();
    for (index, c) in b.iter().enumerate() {
        positions.entry(*c).or_default().push(index);
    }
    if b.len() >= 200 {
        positions.retain(|_, p| p.len() <= b.len() / 100 + 1);
    }
    let mut queue = vec![(0, a.len(), 0, b.len())];
    let mut matched = 0usize;
    while let Some((alo, ahi, blo, bhi)) = queue.pop() {
        let (mut besti, mut bestj, mut size) = (alo, blo, 0usize);
        let mut lengths = BTreeMap::new();
        for (i, c) in a.iter().enumerate().take(ahi).skip(alo) {
            let mut next = BTreeMap::new();
            for &j in positions
                .get(c)
                .into_iter()
                .flatten()
                .filter(|j| **j >= blo && **j < bhi)
            {
                let n = if j == 0 {
                    1
                } else {
                    lengths.get(&(j - 1)).copied().unwrap_or(0) + 1
                };
                next.insert(j, n);
                if n > size {
                    besti = i + 1 - n;
                    bestj = j + 1 - n;
                    size = n;
                }
            }
            lengths = next;
        }
        while besti > alo && bestj > blo && a[besti - 1] == b[bestj - 1] {
            besti -= 1;
            bestj -= 1;
            size += 1;
        }
        while besti + size < ahi && bestj + size < bhi && a[besti + size] == b[bestj + size] {
            size += 1;
        }
        if size > 0 {
            matched += size;
            if alo < besti && blo < bestj {
                queue.push((alo, besti, blo, bestj));
            }
            if besti + size < ahi && bestj + size < bhi {
                queue.push((besti + size, ahi, bestj + size, bhi));
            }
        }
    }
    (2 * matched) as f64 / (a.len() + b.len()) as f64
}

#[cfg(test)]
mod tests;
