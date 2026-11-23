//! One typed path for usage: an adapter builds a report from the buckets it
//! actually observed, and one accumulator merges those reports using the
//! semantics the adapter declared. No reader infers scope, cache inclusion or
//! liveness from a provider's name or from a report in isolation.
use provenant_records::{Cost, Tokens, UsageScope};
use std::collections::BTreeMap;

pub use provenant_records::UsageSemantics;

/// One usage observation from a provider adapter. Only the buckets the adapter
/// actually observed are `Some`; an unstated bucket stays absent, never zero.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct UsageReport {
    /// What this report means. It matches the adapter's declared semantics.
    pub scope: UsageScope,
    /// Identity of the call or step, where the provider supplies one. A repeat
    /// of a key replaces that key's earlier report instead of adding to it.
    pub key: Option<String>,
    /// The model this report was charged against, when the adapter observed it.
    pub model: Option<String>,
    /// Token buckets the provider stated.
    pub tokens: Tokens,
    /// A cost the provider stated or the catalogue priced, when observed.
    pub cost: Option<Cost>,
}

impl UsageReport {
    /// A report of one step. Adapters add a key, model or cost as observed.
    pub fn step(tokens: Tokens) -> Self {
        Self {
            scope: UsageScope::Step,
            key: None,
            model: None,
            tokens,
            cost: None,
        }
    }

    /// A cumulative report replaces the running total.
    pub fn cumulative(tokens: Tokens) -> Self {
        Self {
            scope: UsageScope::Cumulative,
            ..Self::step(tokens)
        }
    }

    /// Key this report to one call or step.
    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    /// Record the model this report was observed against.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Record a cost stated by the provider or priced by the catalogue.
    pub fn cost(mut self, cost: Cost) -> Self {
        self.cost = Some(cost);
        self
    }
}

/// Add one report's buckets into a running total. A bucket absent on either
/// side stays absent rather than becoming a guessed zero.
fn add(into: &mut Tokens, from: Tokens) {
    macro_rules! bucket {
        ($field:ident) => {
            if let Some(count) = from.$field {
                into.$field = Some(into.$field.unwrap_or(0).saturating_add(count));
            }
        };
    }
    bucket!(input);
    bucket!(output);
    bucket!(cache_read);
    bucket!(cache_write);
    bucket!(cache_write_1h);
    bucket!(reasoning);
}

/// Remove the cache buckets from `input` where the adapter declared that its
/// input figure already includes them. A provider that reports fresh input
/// leaves its figure untouched.
fn fresh_input(tokens: &mut Tokens, semantics: UsageSemantics) {
    if !semantics.input_includes_cache_read {
        return;
    }
    let Some(input) = tokens.input else { return };
    let cache = tokens
        .cache_read
        .unwrap_or(0)
        .saturating_add(tokens.cache_write.unwrap_or(0))
        .saturating_add(tokens.cache_write_1h.unwrap_or(0));
    tokens.input = Some(input.saturating_sub(cache));
}

/// The one merge. A keyed step report replaces that key's earlier report;
/// keyless step reports add; a cumulative report replaces the running total.
#[derive(Clone, Debug)]
pub struct UsageAccumulator {
    semantics: UsageSemantics,
    keyed: BTreeMap<String, UsageReport>,
    keyless: Vec<UsageReport>,
    cumulative: Option<UsageReport>,
}

impl UsageAccumulator {
    /// Merge reports using `semantics`, declared once by the adapter.
    pub fn new(semantics: UsageSemantics) -> Self {
        Self {
            semantics,
            keyed: BTreeMap::new(),
            keyless: Vec::new(),
            cumulative: None,
        }
    }

    /// Absorb one report. The declared scope, not the report's own label,
    /// decides how it merges; `UsageReport.scope` is the adapter's evidence.
    pub fn push(&mut self, mut report: UsageReport) {
        // Normalise per report: a provider that states a cache count larger
        // than one call's input must not borrow from another call's input.
        fresh_input(&mut report.tokens, self.semantics);
        match self.semantics.scope {
            UsageScope::Step => {
                if let Some(key) = report.key.as_ref() {
                    self.keyed.insert(key.clone(), report);
                } else {
                    self.keyless.push(report);
                }
            }
            UsageScope::Cumulative => self.cumulative = Some(report),
        }
    }

    /// The merged tokens, with input normalised to exclude cache reads where
    /// the adapter declared it includes them, and the last stated cost.
    pub fn total(&self) -> (Tokens, Option<Cost>) {
        if let Some(report) = self.cumulative.as_ref() {
            return (report.tokens, report.cost.clone());
        }
        let mut tokens = Tokens::default();
        let mut cost = None;
        for report in self.keyed.values().chain(self.keyless.iter()) {
            add(&mut tokens, report.tokens);
            if report.cost.is_some() {
                // Costs are stated once per report in a single currency; the
                // last one is the adapter's authoritative figure.
                cost = report.cost.clone();
            }
        }
        (tokens, cost)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_records::{CostOrigin, UsageScope};

    fn step(input: Option<u64>, output: Option<u64>) -> UsageReport {
        UsageReport::step(Tokens {
            input,
            output,
            ..Tokens::default()
        })
    }

    fn semantics(scope: UsageScope, includes_cache: bool) -> UsageSemantics {
        UsageSemantics::new(scope, includes_cache, false)
    }

    // Spec: docs/specs/telemetry.md#usage-merges-by-declared-semantics
    #[test]
    fn a_repeated_key_replaces_its_earlier_report() {
        let mut accumulator = UsageAccumulator::new(semantics(UsageScope::Step, false));
        accumulator.push(step(Some(10), Some(2)).key("call-1"));
        accumulator.push(step(Some(30), Some(4)).key("call-1"));
        accumulator.push(step(Some(7), Some(1)).key("call-2"));
        let (tokens, _) = accumulator.total();
        assert_eq!(tokens.input, Some(37));
        assert_eq!(tokens.output, Some(5));
    }

    // Spec: docs/specs/telemetry.md#usage-merges-by-declared-semantics
    #[test]
    fn keyless_step_reports_add_and_unstated_buckets_stay_absent() {
        let mut accumulator = UsageAccumulator::new(semantics(UsageScope::Step, false));
        accumulator.push(step(Some(10), None));
        accumulator.push(step(Some(5), Some(3)));
        let (tokens, _) = accumulator.total();
        assert_eq!(tokens.input, Some(15));
        assert_eq!(tokens.output, Some(3));
        assert_eq!(tokens.cache_read, None);
        assert_eq!(tokens.reasoning, None);
    }

    // Spec: docs/specs/telemetry.md#usage-merges-by-declared-semantics
    #[test]
    fn a_cumulative_report_replaces_the_running_total() {
        let mut accumulator = UsageAccumulator::new(semantics(UsageScope::Cumulative, false));
        accumulator.push(step(Some(10), Some(2)).key("old"));
        accumulator.push(step(Some(99), Some(40)));
        let (tokens, _) = accumulator.total();
        assert_eq!(tokens.input, Some(99));
        assert_eq!(tokens.output, Some(40));
        // A later cumulative report replaces the earlier one, too.
        accumulator.push(step(Some(120), Some(50)));
        let (tokens, _) = accumulator.total();
        assert_eq!(tokens.input, Some(120));
        assert_eq!(tokens.output, Some(50));
    }

    // Spec: docs/specs/telemetry.md#usage-merges-by-declared-semantics
    #[test]
    fn input_is_normalised_only_when_declared_to_include_cache() {
        let report = UsageReport::step(Tokens {
            input: Some(100),
            cache_read: Some(60),
            cache_write: Some(10),
            ..Tokens::default()
        });
        let mut fresh = UsageAccumulator::new(semantics(UsageScope::Step, false));
        fresh.push(report.clone());
        assert_eq!(fresh.total().0.input, Some(100));

        let mut inclusive = UsageAccumulator::new(semantics(UsageScope::Step, true));
        inclusive.push(report);
        let (tokens, _) = inclusive.total();
        assert_eq!(tokens.input, Some(30));
        // The cache buckets themselves are carried over, not consumed.
        assert_eq!(tokens.cache_read, Some(60));
        assert_eq!(tokens.cache_write, Some(10));
    }

    // Spec: docs/specs/telemetry.md#usage-merges-by-declared-semantics
    #[test]
    fn a_cost_is_returned_when_the_adapter_observed_one() {
        let cost = Cost {
            micros: 42,
            currency: "USD".into(),
            stated_by: CostOrigin::Provider,
        };
        let mut accumulator = UsageAccumulator::new(semantics(UsageScope::Step, false));
        assert_eq!(accumulator.total().1, None);
        accumulator.push(step(Some(1), None).cost(cost.clone()));
        assert_eq!(accumulator.total().1, Some(cost));
    }
}
