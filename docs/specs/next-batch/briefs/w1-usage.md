# Slice w1-usage: UsageReport, declared semantics and one accumulator

Issues: #3709 (seam half). Wave 1 of the next batch; integration branch `batch/next`. Expected size about 750 changed lines. Starts after w1-interfaces have integrated.

## Outcome

Make usage one typed path.
- `ProviderEvent::Usage { .. }` (`crates/providers/src/stream.rs`) becomes `ProviderEvent::Usage(UsageReport)`; every adapter's parser (`claude.rs`, `codex.rs`, `codex_rollout.rs`, `agy.rs`, `cursor.rs`, `opencode.rs`) builds a `UsageReport` with only the buckets it actually observed.
- `AdapterSpec` (`crates/records/src/adapters.rs`) gains `usage: UsageSemantics`, declared per adapter from its documented behaviour (cumulative or per-step; whether input includes cache reads; live or end-only).
- New `crates/providers/src/usage.rs` holds `UsageAccumulator`; `crates/cli/src/run.rs` replaces its last-wins consumer with it; `crates/run/src/token_budget.rs` `enforces` reads `spec.usage.live` instead of matching provider names.
- `UsageRecord` holds `tokens: Tokens`, `cost: Option<Cost>` and `source: UsageSource`.
- Ledger v4: bump the JSONL schema id and add migration step *usage-ledger-v4* that rewrites each v3 line (buckets carry over; source strings map through an exhaustive table; an unmappable line is reported and copied unchanged to a `.v3-unmapped` sidecar, never guessed). The ingest store writes through the accumulator with `UsageSource::Ingest`.
OpenCode per-step usage and cost parsing is `w3-opencode-stream`; here only adapt its existing usage event to the new type.

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

- `tools/front-door/crates/providers/src/stream.rs`
- `tools/front-door/crates/providers/src/usage.rs`
- `tools/front-door/crates/providers/src/claude.rs`
- `tools/front-door/crates/providers/src/codex.rs`
- `tools/front-door/crates/providers/src/agy.rs`
- `tools/front-door/crates/providers/src/cursor.rs`
- `tools/front-door/crates/providers/src/opencode.rs`
- `tools/front-door/crates/records/src/adapters.rs`
- `tools/front-door/crates/records/src/run.rs`
- `tools/front-door/crates/cli/src/run.rs`
- `tools/front-door/crates/run/src/token_budget.rs`
- `tools/front-door/crates/telemetry/`
- `tools/front-door/src/install/migrations.rs`

## Tests

Accumulator invariants (keyed idempotence, keyless addition, cumulative replacement, unstated buckets stay `None`, cache normalisation per semantics); one recorded stream fixture per adapter totals as before; the ledger migration round-trips a v3 fixture and is idempotent.

## Docs

Add *Usage merges by declared semantics* to `docs/specs/telemetry.md`.

## Release note

Add `docs/release-notes/next.d/next-batch-w1-usage.md` in the style of the existing fragments (a `## Fixes`, `## Changes` or `## Added` heading, an area subheading, one bullet ending with the issue numbers). Content: Changed: token usage merges per adapter-declared semantics instead of the last report winning; the usage ledger moves to v4 on upgrade.

## Rules for this lane

- Work only in your worktree. Treat the write scope as advisory; list every out-of-scope edit in the handoff.
- Run `cargo fmt` only. Never run `cargo build`, `check`, `clippy` or `test`: the chair runs one integrated clippy check and CI runs the tests. Write code that you expect to compile, and say in the handoff which parts you could not check.
- No backwards compatibility: no shims, aliases, legacy readers or fallbacks. Move every caller and delete the old shape. Persisted formats change only through a migration step (`records::migrate`), never through a tolerant decoder.
- Warn and proceed except at the three boundaries: credentials never reach a lane, provider or output not granted them; nothing moves, rewrites or deletes another lane's refs, commits or uncommitted work; merge, release and ruleset changes need the owner. Every other limit is an overridable default with its source shown.
- No host- or repository-specific special cases. Fix root causes.
- Tests pin invariants at public boundaries, never prose, counts or snapshots, unless the brief says the value is a persisted contract. Each test cites the spec section it pins with `// Spec: docs/specs/NAME.md#anchor`.
- Names in *italics* below are new: create them. Paths are relative to the repository root unless they start with `crates/` or `src/`, which are under `tools/front-door/`.
- Commit with a conventional message (no issue-closing keywords) and end with the handoff block.
