//! The shared usage bucket and cost types. `UsageRecord`, the provider stream
//! and the attempt fact all project these, so one report means one shape.
use serde::{Deserialize, Serialize};

/// Token counts as a provider reports them. Every bucket stays absent until a
/// report states it, never a guessed zero.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    /// Future buckets survive a decode/encode round trip, like every record
    /// payload. Spec: docs/specs/state.md#strict-bounded-records
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    /// Input tokens as the provider states them; cache buckets say whether
    /// cache reads are included.
    pub input: Option<u64>,
    /// Output tokens as the provider states them.
    pub output: Option<u64>,
    /// Input tokens read from the prompt cache.
    pub cache_read: Option<u64>,
    /// Input tokens written to the prompt cache.
    pub cache_write: Option<u64>,
    /// The long-TTL part of `cache_write`, where the provider splits writes.
    pub cache_write_1h: Option<u64>,
    /// Reasoning tokens, where the provider reports them separately.
    pub reasoning: Option<u64>,
}

/// What one usage report means: the step that just finished or the run's
/// running total.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageScope {
    /// One step's tokens.
    Step,
    /// The provider's cumulative total for the run.
    Cumulative,
}

/// A cost, in integer micros, that the provider stated or the catalogue priced.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cost {
    /// Cost in millionths of `currency`.
    pub micros: u64,
    /// ISO 4217 code or the provider's own currency name.
    pub currency: String,
    /// Who stated the cost.
    pub stated_by: CostOrigin,
}

/// Who stated a cost.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostOrigin {
    /// The provider's own report.
    Provider,
    /// The bundled or configured model catalogue.
    Catalogue,
}

/// The declared meaning of one adapter's usage reports. The accumulator reads
/// this, so no reader guesses scope or cache inclusion from a report in
/// isolation, nor from a provider's name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct UsageSemantics {
    /// What one report means: a step that finished, or the run's total.
    pub scope: UsageScope,
    /// Whether the reported `input` already includes cache reads and writes.
    /// Fresh input is derived by removing every stated cache bucket.
    pub input_includes_cache_read: bool,
    /// Reports arrive during the run, so a token budget can act on them.
    pub live: bool,
}

impl UsageSemantics {
    /// Declare the meaning of an adapter's usage reports.
    pub const fn new(scope: UsageScope, input_includes_cache_read: bool, live: bool) -> Self {
        Self {
            scope,
            input_includes_cache_read,
            live,
        }
    }
}

/// Where a usage record was observed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageSource {
    /// The provider's live event stream.
    Stream,
    /// The provider's session store, read after the run.
    SessionStore,
    /// A record ingested from another host or lane.
    Ingest,
}

#[cfg(test)]
mod tests {
    use super::{Cost, CostOrigin, Tokens, UsageScope, UsageSource};

    #[test]
    fn an_absent_bucket_stays_absent() {
        let tokens = Tokens::default();
        assert_eq!(
            (
                tokens.input,
                tokens.output,
                tokens.cache_read,
                tokens.cache_write,
                tokens.cache_write_1h,
                tokens.reasoning,
            ),
            (None, None, None, None, None, None)
        );
    }

    #[test]
    fn usage_names_keep_their_wire_spelling() {
        assert_eq!(
            serde_json::to_value(UsageScope::Cumulative).expect("serialise"),
            "cumulative"
        );
        assert_eq!(
            serde_json::to_value(UsageSource::SessionStore).expect("serialise"),
            "session_store"
        );
        assert_eq!(
            serde_json::to_value(CostOrigin::Catalogue).expect("serialise"),
            "catalogue"
        );
    }

    #[test]
    fn a_cost_round_trips() {
        let cost = Cost {
            micros: 42,
            currency: "USD".into(),
            stated_by: CostOrigin::Provider,
        };
        let wire = serde_json::to_value(&cost).expect("serialise");
        assert_eq!(
            wire,
            serde_json::json!({"micros": 42, "currency": "USD", "stated_by": "provider"})
        );
        assert_eq!(
            serde_json::from_value::<Cost>(wire).expect("deserialise"),
            cost
        );
    }
}
