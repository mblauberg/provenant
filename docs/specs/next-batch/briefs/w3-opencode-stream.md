# Slice w3-opencode-stream: OpenCode stream parity: step usage and cost, retry, 5xx

Issues: #3706 #3708 #3709. Wave 3 of the next batch; integration branch `batch/next`. Expected size about 550 changed lines.

## Outcome

OpenCode stream parity in `crates/providers/src/opencode.rs`: (#3709) parse per-step token usage and provider-stated cost from step-finish events into `UsageReport` (`UsageScope::Step` with the step id as key; `CostOrigin::Provider`); (#3706) add a retry event variant: a provider 429 retry loop is a typed rate-limit signal and, past a sourced retry bound (default from the adapter spec), ends the attempt as usage-limited; (#3708) upstream 5xx (`provider.internal` and similar) classifies as capacity, not a generic error.

## Interface to honour

These types exist on the integration branch (or, for wave 1 w1-interfaces, are yours to create exactly). Use them as written; do not add parallel types.

```rust
// crates/records/src/usage.rs  (owner: records; shared by UsageRecord and the stream)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    pub input: Option<u64>, pub output: Option<u64>,
    pub cache_read: Option<u64>, pub cache_write: Option<u64>,
    pub cache_write_1h: Option<u64>, pub reasoning: Option<u64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageScope { Step, Cumulative }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cost { pub micros: u64, pub currency: String, pub stated_by: CostOrigin }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostOrigin { Provider, Catalogue }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageSource { Stream, SessionStore, Ingest }

// crates/providers/src/usage.rs  (owner: providers)
pub struct UsageReport { pub scope: UsageScope, pub key: Option<String>, pub model: Option<String>, pub tokens: Tokens, pub cost: Option<Cost> }
/// Declared per adapter in AdapterSpec; the accumulator reads it, nobody else guesses.
pub struct UsageSemantics {
    pub scope: UsageScope,              // what one report means
    pub input_includes_cache_read: bool,
    pub live: bool,                     // reports arrive during the run (token budget can act)
}
/// The one merge: Step reports with a key replace that key's earlier report;
/// keyless Step reports add; a Cumulative report replaces the running total.
/// Buckets stay None until some report states them.
pub struct UsageAccumulator { /* .. */ }
impl UsageAccumulator {
    pub fn new(semantics: UsageSemantics) -> Self;
    pub fn push(&mut self, report: UsageReport);
    pub fn total(&self) -> (Tokens, Option<Cost>); // input normalised to exclude cache reads
}
// ProviderEvent::Usage { .. } becomes ProviderEvent::Usage(UsageReport).
```

## Files (write scope)

- `tools/front-door/crates/providers/src/opencode.rs`

## Tests

Recorded stream fixtures: multi-step usage totals and cost; a retry loop ends at the bound with the right outcome; a 5xx is capacity.

## Docs

none beyond telemetry.

## Release note

Add `docs/release-notes/next.d/next-batch-w3-opencode-stream.md` in the style of the existing fragments (a `## Fixes`, `## Changes` or `## Added` heading, an area subheading, one bullet ending with the issue numbers). Content: Fixed: OpenCode lanes record per-step token usage and cost, end on endless provider retries, and treat upstream 5xx as capacity.

## Rules for this lane

- Work only in your worktree. Treat the write scope as advisory; list every out-of-scope edit in the handoff.
- Run `cargo fmt` only. Never run `cargo build`, `check`, `clippy` or `test`: the chair runs one integrated clippy check and CI runs the tests. Write code that you expect to compile, and say in the handoff which parts you could not check.
- No backwards compatibility: no shims, aliases, legacy readers or fallbacks. Move every caller and delete the old shape. Persisted formats change only through a migration step (`records::migrate`), never through a tolerant decoder.
- Warn and proceed except at the three boundaries: credentials never reach a lane, provider or output not granted them; nothing moves, rewrites or deletes another lane's refs, commits or uncommitted work; merge, release and ruleset changes need the owner. Every other limit is an overridable default with its source shown.
- No host- or repository-specific special cases. Fix root causes.
- Tests pin invariants at public boundaries, never prose, counts or snapshots, unless the brief says the value is a persisted contract. Each test cites the spec section it pins with `// Spec: docs/specs/NAME.md#anchor`.
- Names in *italics* below are new: create them. Paths are relative to the repository root unless they start with `crates/` or `src/`, which are under `tools/front-door/`.
- Commit with a conventional message (no issue-closing keywords) and end with the handoff block.
