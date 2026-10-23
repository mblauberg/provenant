//! Shared, risk-scaled review coverage checks. Independence remains the caller's job.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

pub const PRIMARY_FAMILIES: &[&str] = &["anthropic", "openai"];
pub const TERMINAL_PRESSURE_MARKERS: &[&str] = &["adversarial", "challenge"];
pub const REVIEW_PLAN_COMPLETE_STATUS: &str = "complete";
pub const REVIEW_PLAN_STATUSES: &[&str] = &["complete", "failed", "unavailable", "omitted"];
pub const SKIPPED_STATUSES: &[&str] = &["failed", "unavailable", "omitted"];
pub const BLOCKER_STATUSES: &[&str] = &["omitted", "unavailable"];

/// Fields stay JSON values at this permissive boundary so malformed legs retain
/// the source's decisions. Receipt shape validation belongs to each caller.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReviewLeg {
    #[serde(default)] pub family: Value,
    #[serde(default)] pub provider_family: Value,
    #[serde(default)] pub status: Value,
    #[serde(default)] pub lenses: Value,
    #[serde(default)] pub reason: Value,
}
impl ReviewLeg {
    fn family(&self) -> Option<&str> {
        self.provider_family.as_str().filter(|s| !s.is_empty()).or_else(|| self.family.as_str()).map(str::trim)
    }
    fn lenses(&self) -> BTreeSet<String> {
        let values: Vec<&str> = match &self.lenses {
            Value::String(s) => vec![s],
            Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
            Value::Object(o) => o.keys().map(String::as_str).collect(),
            _ => vec![],
        };
        values.into_iter().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect()
    }
}

pub fn check_review_ladder(risk_tier: &str, legs: &[ReviewLeg], chair_family: Option<&str>) -> Vec<String> {
    if !["substantial", "crucial", "terminal"].contains(&risk_tier) { return vec![]; }
    let Some(chair) = chair_family.filter(|s| !s.trim().is_empty()) else { return vec!["chair_family is required".into()]; };
    // Deliberately do not trim the chair after its non-blank check: the oracle
    // compares the caller's original chair value against trimmed leg families.
    let passing: Vec<_> = legs.iter().filter(|leg| leg.status == "pass" && !leg.lenses().is_empty()).collect();
    let blocked: BTreeSet<_> = legs.iter().filter(|leg| leg.status.as_str().is_some_and(|s| BLOCKER_STATUSES.contains(&s))
        && leg.reason.as_str().is_some_and(|s| !s.trim().is_empty()))
        .filter_map(ReviewLeg::family).filter(|f| !f.is_empty() && *f != chair).collect();
    let non_chair: BTreeSet<_> = passing.iter().filter_map(|leg| leg.family()).filter(|f| !f.is_empty() && *f != chair).collect();
    let mut errors = Vec::new();
    if risk_tier == "substantial" {
        if passing.is_empty() { errors.push("substantial review requires at least one passing independent leg".into()); }
        if non_chair.is_empty() {
            if blocked.is_empty() { errors.push("substantial review requires cross-family coverage or a recorded blocker with a reason".into()); }
            if !passing.iter().any(|leg| leg.family() == Some(chair)) {
                errors.push("substantial fallback requires a passing same-family review with a lens".into());
            }
        }
        return errors;
    }
    if passing.len() < 2 { errors.push(format!("{risk_tier} review requires at least two passing independent legs")); }
    let lens_sets: Vec<_> = passing.iter().map(|leg| leg.lenses()).collect();
    if !lens_sets.iter().enumerate().any(|(i, left)| lens_sets[i+1..].iter().any(|right| left != right && left.union(right).count() >= 2)) {
        errors.push(format!("{risk_tier} review requires distinct lens sets covering at least two lenses"));
    }
    if non_chair.union(&blocked).count() < 2 {
        errors.push(format!("{risk_tier} review requires two non-chair families or a recorded blocker with a reason for each missing family"));
    }
    let missing = 2_usize.saturating_sub(non_chair.len());
    if passing.iter().filter(|leg| leg.family() == Some(chair)).count() < missing {
        errors.push(format!("{risk_tier} fallback requires a passing same-family review for each missing non-chair family"));
    }
    if risk_tier == "terminal" && !lens_sets.iter().flatten().any(|lens| TERMINAL_PRESSURE_MARKERS.iter().any(|marker| lens.to_lowercase().contains(marker))) {
        errors.push("terminal review requires an adversarial or challenge lens".into());
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn translated_policy_fixtures_and_live_python_agree() {
        let fixtures: Value = serde_json::from_str(include_str!("../tests/fixtures/review_ladder.json")).unwrap();
        let mut cases = Vec::new();
        for fixture in fixtures.as_array().unwrap() {
            let input = &fixture["input"];
            let legs: Vec<ReviewLeg> = serde_json::from_value(input["legs"].clone()).unwrap();
            let actual = check_review_ladder(input["risk"].as_str().unwrap(), &legs, input["chair"].as_str());
            assert_eq!(json!(actual), fixture["expected"], "{input}");
            cases.push(input.clone());
        }
        if let Some(expected) = crate::test_support::oracle("review_ladder", &json!(cases)) {
            assert_eq!(expected, Value::Array(fixtures.as_array().unwrap().iter().map(|f| f["expected"].clone()).collect()));
        }
    }
    #[test]
    fn malformed_lenses_and_provider_lineage_keep_source_semantics() {
        let cases = json!([
            {"risk":"substantial","chair":"openai","legs":[{"status":"pass","family":"anthropic","lenses":{"security":false}}]},
            {"risk":"substantial","chair":"openai","legs":[{"status":"pass","family":"anthropic","provider_family":"   ","lenses":"security"}]},
            {"risk":"crucial","chair":" openai ","legs":[{"status":"pass","family":"openai","lenses":["a",5]},{"status":"pass","family":"google","lenses":"b"}]},
            {"risk":"terminal","chair":"openai","legs":[{"status":"pass","family":null,"lenses":5}]}
        ]);
        let actual: Vec<_> = cases.as_array().unwrap().iter().map(|c| check_review_ladder(c["risk"].as_str().unwrap(), &serde_json::from_value::<Vec<ReviewLeg>>(c["legs"].clone()).unwrap(), c["chair"].as_str())).collect();
        if let Some(expected) = crate::test_support::oracle("review_ladder", &cases) { assert_eq!(json!(actual), expected); }
    }
}
