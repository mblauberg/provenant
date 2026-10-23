//! Receipt and sidecar metadata policy. Discovery and file cursors stay with the ledger owner.
use crate::{Event, HostIdentity, Result, Sink, Usage, costs, parse_time};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Review {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_family: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lenses: Option<Value>,
    pub verdict: Value,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Delivery {
    pub run_dir: String,
    pub host: String,
    pub run_id: Option<String>,
    pub risk_tier: Option<String>,
    pub chair_family: Option<String>,
    pub repair_cycles: Option<u64>,
    pub acceptance_basis: Option<String>,
    pub accepted: bool,
    pub merge_commit: Option<String>,
    pub ingested_at: String,
    pub started_at: Option<String>,
    pub writer_model: Option<String>,
    pub review_verdicts: Vec<Review>,
}
fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}
/// Earliest recorded time, excluding authority and risk overrides. No wall-clock access.
pub fn run_timestamp(receipt: &Value, mtime: jiff::Timestamp) -> String {
    fn visit(value: &Value, times: &mut Vec<jiff::Timestamp>) {
        match value {
            Value::Object(values) => {
                for (key, item) in values {
                    if matches!(key.as_str(), "authority" | "risk_override") {
                        continue;
                    }
                    if key == "at" || key == "timestamp" || key.ends_with("_at") {
                        if let Some(t) = item.as_str().and_then(|s| parse_time(s).ok()) {
                            times.push(t);
                        }
                    } else {
                        visit(item, times);
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    visit(item, times);
                }
            }
            _ => {}
        }
    }
    let mut times = Vec::new();
    visit(receipt, &mut times);
    crate::pending::iso(times.into_iter().min().unwrap_or(mtime)).replace('Z', "+00:00")
}
pub fn receipt(
    receipt: &Value,
    run_dir: String,
    host: String,
    now: String,
    mtime: jiff::Timestamp,
) -> Delivery {
    let basis = receipt["delivery_acceptance"]["basis"]
        .as_str()
        .filter(|s| matches!(*s, "checks-and-review" | "project-landing" | "user"));
    let canonical = &receipt["software_delivery"]["canonical_artifact_id"];
    let artifact = receipt["artifacts"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|v| v.is_object() && &v["id"] == canonical);
    let review_verdicts = receipt["reviews"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            if !r.is_object() {
                return None;
            }
            let verdict = r
                .get("verdict")
                .filter(|v| crate::pending::python_truthy(v))
                .or_else(|| r.get("status"))
                .filter(|v| !v.is_null())?;
            Some(Review {
                id: r.get("id").cloned(),
                role: r.get("role").cloned(),
                provider_family: r.get("provider_family").cloned(),
                model: r.get("model").cloned(),
                lenses: r.get("lenses").cloned(),
                verdict: verdict.clone(),
            })
        })
        .collect();
    Delivery {
        run_dir,
        host,
        run_id: text(receipt, "run_id"),
        risk_tier: text(receipt, "risk_tier"),
        chair_family: text(receipt, "chair_family"),
        repair_cycles: receipt["repair_cycles"].as_u64(),
        acceptance_basis: basis.map(str::to_owned),
        accepted: basis.is_some(),
        merge_commit: artifact.and_then(|a| text(&a["git_revision"], "commit")),
        ingested_at: now,
        started_at: Some(run_timestamp(receipt, mtime)),
        writer_model: text(receipt, "writer_model"),
        review_verdicts,
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SidecarState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous: Option<PreviousRepair>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreviousRepair {
    pub source_ref: String,
    pub attrs: Value,
}
fn failures(row: &Value) -> Vec<String> {
    row["failing_before"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
fn failure_hash(failing: &[String]) -> String {
    // json.dumps(..., separators=(',', ':')) still escapes non-ASCII.
    let compact = Value::Array(failing.iter().cloned().map(Value::String).collect());
    let encoded = match &compact {
        Value::Array(a) => format!(
            "[{}]",
            a.iter()
                .map(crate::pending::python_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        _ => String::new(),
    };
    crate::pending::hex(&Sha256::digest(encoded.as_bytes()))[..12].into()
}
fn after(mut attrs: Value, failing: &[String]) -> Value {
    attrs["failing_after_count"] = json!(failing.len());
    attrs["failing_after"] = json!(&failing[..failing.len().min(8)]);
    attrs["failing_after_hash"] = json!(failure_hash(failing));
    attrs
}
/// The caller supplies the owning receipt and resolved source ref, avoiding hidden filesystem reads.
pub fn record(
    sink: &mut impl Sink,
    filename: &str,
    row: &Value,
    state: &mut SidecarState,
    receipt: &Value,
    source_ref: String,
) -> Result<()> {
    if filename == "repairs.jsonl" {
        let failing = failures(row);
        if let Some(previous) = &state.previous {
            sink.update_event(
                "repair.cycle",
                &previous.source_ref,
                after(previous.attrs.clone(), &failing),
            )?;
        }
        let attrs = json!({"repair_cycle":row["cycle"],"failing_before_count":failing.len(),"failing_before":&failing[..failing.len().min(8)],"failing_before_hash":failure_hash(&failing)});
        sink.event(Event {
            kind: "repair.cycle".into(),
            ts: text(row, "at"),
            source_ref: source_ref.clone(),
            attrs: attrs.clone(),
            ..Event::default()
        })?;
        if row["cycle"] == receipt["repair_cycles"] && receipt.get("delivery_acceptance").is_some()
        {
            sink.update_event("repair.cycle", &source_ref, after(attrs.clone(), &[]))?;
        }
        state.previous = Some(PreviousRepair { source_ref, attrs });
    } else if filename == "findings.jsonl" {
        let review = receipt["reviews"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|r| r.is_object() && r["id"] == row["review_id"]);
        let mut attrs = serde_json::Map::new();
        for key in [
            "review_id",
            "finding",
            "disposition",
            "severity",
            "duplicate_of",
        ] {
            if let Some(v) = row.get(key).filter(|v| !v.is_null()) {
                attrs.insert(key.into(), v.clone());
            }
        }
        for (key, source) in [
            ("reviewer_family", "provider_family"),
            ("model", "model"),
            ("reviewer_id", "reviewer_id"),
        ] {
            if let Some(v) = review
                .and_then(|r| r.get(source))
                .filter(|v| crate::pending::python_truthy(v))
                .or_else(|| row.get(key))
                .filter(|v| !v.is_null())
            {
                attrs.insert(key.into(), v.clone());
            }
        }
        let attrs = Value::Object(attrs);
        sink.event(Event {
            kind: "review.finding".into(),
            ts: text(row, "at"),
            source_ref: source_ref.clone(),
            attrs: attrs.clone(),
            ..Event::default()
        })?;
        sink.update_event("review.finding", &source_ref, attrs)?;
    }
    Ok(())
}
pub fn compact(
    sink: &mut impl Sink,
    provider: &str,
    row: &Value,
    session: Option<&str>,
    path: &Path,
    offset: u64,
) -> Result<()> {
    let (attrs, session) = if provider == "claude"
        && row["type"] == "system"
        && row["subtype"] == "compact_boundary"
    {
        let meta = &row["compactMetadata"];
        (
            json!({"trigger":meta["trigger"],"pre_tokens":meta["preTokens"],"post_tokens":meta["postTokens"]}),
            text(row, "sessionId"),
        )
    } else if provider == "codex" && row["type"] == "compacted" {
        (json!({}), session.map(str::to_owned))
    } else {
        return Ok(());
    };
    sink.event(Event {
        kind: "session.compaction".into(),
        ts: text(row, "timestamp"),
        source_ref: format!("{}#{offset}", path.display()),
        session,
        attrs,
        ..Event::default()
    })
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Compactions {
    pub lane: String,
    pub compactions: u64,
}
/// A ledger owner joins compaction events to usage lanes before passing these rows.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct CompactionRow {
    pub lane: String,
    pub event_id: String,
    pub ts: Option<String>,
}
pub fn report(
    rows: &[CompactionRow],
    since: Option<&str>,
    until: Option<&str>,
) -> Result<Vec<Compactions>> {
    let low = since
        .filter(|s| !s.is_empty())
        .map(parse_time)
        .transpose()?;
    let high = until
        .filter(|s| !s.is_empty())
        .map(parse_time)
        .transpose()?;
    let mut counts = BTreeMap::<String, u64>::new();
    // Match SELECT DISTINCT lane,event_id,ts, including repeated joined usage rows.
    for row in rows.iter().collect::<BTreeSet<_>>() {
        let lane = row.lane.clone();
        let stamp = row.ts.as_deref();
        if low.is_some() || high.is_some() {
            let Some(stamp) = stamp else {
                continue;
            };
            let stamp = parse_time(stamp)?;
            if low.is_some_and(|l| stamp < l) || high.is_some_and(|h| stamp >= h) {
                continue;
            }
        }
        *counts.entry(lane).or_default() += 1;
    }
    Ok(counts
        .into_iter()
        .map(|(lane, compactions)| Compactions { lane, compactions })
        .collect())
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AcceptedCosts {
    pub risk_tier: Option<String>,
    pub harness_digest: Option<String>,
    pub runs: usize,
    pub accepted: usize,
    pub merged: usize,
    pub cost_usd_lower_bound: Option<f64>,
    pub usage_rows: u64,
    pub unpriced_rows: u64,
    pub cost_per_accepted_usd_lower_bound: Option<f64>,
    pub cost_basis: String,
}
/// Costs join only explicit delivery IDs. Matching run names do not imply lineage.
pub fn cost_per_accepted(
    deliveries: &[Delivery],
    lanes: &[Usage],
    merged_commits: &BTreeSet<String>,
    identity: &HostIdentity,
    since: Option<&str>,
    prices: &costs::Prices,
    adapters: &costs::Catalogue,
) -> Result<Vec<AcceptedCosts>> {
    #[derive(Default)]
    struct Group {
        runs: BTreeSet<String>,
        accepted: BTreeSet<String>,
        merged: BTreeSet<String>,
        cost: Option<f64>,
        rows: u64,
        unpriced: u64,
    }
    let boundary = since
        .filter(|s| !s.is_empty())
        .map(parse_time)
        .transpose()?;
    let mut groups = BTreeMap::<Option<String>, Group>::new();
    let mut run_groups = BTreeMap::new();
    let mut deliveries: Vec<_> = deliveries.iter().collect();
    deliveries.sort_by_key(|d| &d.run_dir);
    for run in deliveries {
        if let Some(boundary) = boundary {
            if run
                .started_at
                .as_deref()
                .map(parse_time)
                .transpose()?
                .is_none_or(|t| t < boundary)
            {
                continue;
            }
        }
        let g = groups.entry(run.risk_tier.clone()).or_default();
        g.runs.insert(run.run_dir.clone());
        if run.accepted {
            g.accepted.insert(run.run_dir.clone());
        }
        if run
            .merge_commit
            .as_ref()
            .is_some_and(|c| !c.is_empty() && merged_commits.contains(c))
        {
            g.merged.insert(run.run_dir.clone());
        }
        run_groups.insert(run.run_id.clone(), run.risk_tier.clone());
    }
    let mut dedup = Vec::<&Usage>::new();
    let mut positions = BTreeMap::new();
    for row in lanes
        .iter()
        .filter(|r| r.source == "fabric" && r.delivery_run_id.is_some())
    {
        let key = (&row.source, &row.event_id, identity.canonical(&row.host));
        if let Some(&index) = positions.get(&key) {
            if row.host == identity.current {
                dedup[index] = row;
            }
        } else {
            positions.insert(key, dedup.len());
            dedup.push(row);
        }
    }
    for row in dedup {
        let Some(tier) = run_groups.get(&row.delivery_run_id) else {
            continue;
        };
        let Some(group) = groups.get_mut(tier) else {
            continue;
        };
        let mut row = row.clone();
        row.model = costs::normalise_model(
            row.adapter.as_deref(),
            row.provider.as_deref(),
            row.model.as_deref(),
            adapters,
        );
        let cost = costs::estimated_cost(&row, &prices.entry(&row)?);
        group.rows += 1;
        group.unpriced += u64::from(cost.is_none());
        group.cost = Some(group.cost.unwrap_or(0.0) + cost.unwrap_or(0.0));
    }
    Ok(groups
        .into_iter()
        .map(|(risk_tier, g)| AcceptedCosts {
            risk_tier,
            harness_digest: None,
            runs: g.runs.len(),
            accepted: g.accepted.len(),
            merged: g.merged.len(),
            cost_usd_lower_bound: g.cost,
            usage_rows: g.rows,
            unpriced_rows: g.unpriced,
            cost_per_accepted_usd_lower_bound: g
                .cost
                .filter(|_| !g.accepted.is_empty())
                .map(|c| c / g.accepted.len() as f64),
            cost_basis: if g.cost.is_some() {
                "linked"
            } else {
                "unlinked"
            }
            .into(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receipt_uses_earliest_time_and_canonical_artifact() {
        let input = json!({"created_at":"2026-10-21T00:00:00Z","authority":{"at":"2000-01-01"},"risk_override":{"at":"1999-01-01"},"writer_model":"model","artifacts":[{"id":"other","git_revision":{"commit":"bad"}},{"id":"canonical","git_revision":{"commit":"good"}}],"software_delivery":{"canonical_artifact_id":"canonical"},"delivery_acceptance":{"basis":"checks-and-review"},"reviews":[{"id":"r","status":"pass","secret":"drop"}]});
        let run = receipt(
            &input,
            "run".into(),
            "host".into(),
            "now".into(),
            parse_time("2026-10-23").unwrap(),
        );
        assert_eq!(run.started_at.as_deref(), Some("2026-10-21T00:00:00+00:00"));
        assert_eq!(run.merge_commit.as_deref(), Some("good"));
        assert!(run.accepted);
        assert!(
            !serde_json::to_string(&run.review_verdicts)
                .unwrap()
                .contains("secret")
        );
    }
    #[test]
    fn unlinked_runs_never_appear_free() {
        let run = Delivery {
            run_dir: "run".into(),
            run_id: Some("r".into()),
            accepted: true,
            ..Delivery::default()
        };
        let report = cost_per_accepted(
            &[run],
            &[],
            &BTreeSet::new(),
            &HostIdentity::new("h".into(), BTreeSet::new(), &BTreeSet::new()),
            None,
            &costs::Prices::default(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(report[0].cost_usd_lower_bound, None);
        assert_eq!(report[0].cost_basis, "unlinked");
    }
    #[test]
    fn hashes_are_sorted_unique_and_ascii_escaped() {
        assert_eq!(
            failure_hash(&failures(&json!({"failing_before":["b","a","a",4]}))),
            failure_hash(&["a".into(), "b".into()])
        );
        assert_eq!(failure_hash(&[]), "4f53cda18c2b");
    }
    #[test]
    fn review_regression_linked_costs_follow_first_seen_order() {
        let run = Delivery {
            run_dir: "dir".into(),
            run_id: Some("run".into()),
            accepted: true,
            ..Delivery::default()
        };
        let rows: Vec<_> = [("c", 1), ("b", 2), ("a", 3)]
            .into_iter()
            .map(|(id, n)| Usage {
                source: "fabric".into(),
                event_id: id.into(),
                host: "h".into(),
                delivery_run_id: Some("run".into()),
                adapter: Some("a".into()),
                model: Some("m".into()),
                input_tokens: Some((n * 100_000).into()),
                cache_read_tokens: Some(0.into()),
                cache_write_tokens: Some(0.into()),
                output_tokens: Some(0.into()),
                ..Usage::default()
            })
            .collect();
        let prices = costs::Prices {
            models: BTreeMap::from([(
                "a/m".into(),
                costs::Price {
                    usd_per_million: BTreeMap::from([("input".into(), json!(1))]),
                    ..costs::Price::default()
                },
            )]),
            ..costs::Prices::default()
        };
        let report = cost_per_accepted(
            &[run],
            &rows,
            &BTreeSet::new(),
            &HostIdentity::new("h".into(), BTreeSet::new(), &BTreeSet::new()),
            None,
            &prices,
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&report[0]).unwrap()["cost_usd_lower_bound"].to_string(),
            "0.6000000000000001"
        );
        assert_eq!(
            report[0].cost_per_accepted_usd_lower_bound,
            Some(0.6000000000000001)
        );
    }

    #[test]
    fn review_regression_missing_and_null_ids_match() {
        let run = receipt(
            &json!({"artifacts":[{"git_revision":{"commit":"abc"}}]}),
            "dir".into(),
            "host".into(),
            "now".into(),
            parse_time("2026-01-01").unwrap(),
        );
        assert_eq!(run.merge_commit.as_deref(), Some("abc"));
    }
    #[test]
    fn review_regression_review_missing_and_null_ids_match() {
        for (review, finding) in [
            (
                json!({"provider_family":"family"}),
                json!({"review_id":null}),
            ),
            (json!({"id":null,"provider_family":"family"}), json!({})),
        ] {
            let mut sink = crate::ingest::tests::Capture::default();
            record(
                &mut sink,
                "findings.jsonl",
                &finding,
                &mut SidecarState::default(),
                &json!({"reviews":[review]}),
                "ref".into(),
            )
            .unwrap();
            assert_eq!(sink.events[0].attrs["reviewer_family"], "family");
        }
    }

    #[test]
    fn review_regression_empty_since_is_ignored() {
        let row = CompactionRow {
            lane: "lane".into(),
            event_id: "id".into(),
            ts: Some("2026-01-01".into()),
        };
        let rows = [row.clone(), row];
        assert_eq!(
            report(&rows, Some(""), Some("")).unwrap(),
            report(&rows, None, None).unwrap()
        );
        assert_eq!(report(&rows, None, None).unwrap()[0].compactions, 1);
        let run = Delivery {
            run_dir: "dir".into(),
            ..Delivery::default()
        };
        let identity = HostIdentity::new("h".into(), BTreeSet::new(), &BTreeSet::new());
        assert_eq!(
            cost_per_accepted(
                std::slice::from_ref(&run),
                &[],
                &BTreeSet::new(),
                &identity,
                Some(""),
                &costs::Prices::default(),
                &BTreeMap::new()
            )
            .unwrap(),
            cost_per_accepted(
                &[run],
                &[],
                &BTreeSet::new(),
                &identity,
                None,
                &costs::Prices::default(),
                &BTreeMap::new()
            )
            .unwrap()
        );
    }
}
