# Telemetry

Owner: `crates/telemetry` (reducer, prices, trace, usage log, usage ledger)
Anchor: `crates/telemetry/src/lib.rs`, `crates/telemetry/src/ingest.rs`,
`crates/telemetry/src/ledger.rs`

Telemetry reports outcomes, usage and API-equivalent cost (the same tokens at
published API prices) from terminal attempts and native sessions. Cost is an
estimate, never a bill. The [telemetry runbook](../runbooks/telemetry.md) owns
use. Paths are relative to `tools/front-door/`.
Trace exports identify attempts by graph and node when the fact has that
context; missing context stays unknown.

## A report cannot touch a lane

Reporting starts no provider operation or background collection, and run does
not depend on telemetry. An absent run store gives an empty report and creates
nothing.
- Why: measurement must never perturb what it measures.

## The usage ledger outlives the run store

The append-only ledger retains one priced row per terminal attempt. Retried
settlement adds nothing, an unreadable price table leaves settlement pending,
and unrecognised lines are reported. Cost reports read the ledger.
- Why: lane usage cannot be regenerated once run history is dropped, and one
  reader of one ledger cannot disagree with itself.
- Owner: `crates/telemetry/src/ledger.rs`, `src/settlement.rs`

## Bounded, honest totals

The reducer refuses over-limit input, never partial totals.
Known values and missing coverage are reported separately: partial coverage is
a lower bound, all-unknown is unknown, and only a measured zero is zero. An
unpriced subscription is not free; an attempt with no account is
`unattributed`, never folded into another. A provider that reports no cache
split is `missing` in the cache-read share, never a miss.
- Why: a partial or zero-filled total reads as a real one.

## Exact money and prices

Cost accumulates in checked integers and rounds once; no floating point.
Prices are keyed by exact provider and model, an omitted rate is unknown, a zero
rate must be verified, and there is no runtime fetch or alias. A price refresh
proposes a diff and never applies it.
- Why: rounding drift or a guessed price corrupts route comparison.

## Attribution is detected, never guessed

Usage rows retain per-session account identity, epoch and evidence source from the provider identity port; unavailable identity is typed unknown, never inferred from a config-directory path.
Ingest preserves that attribution; unknown usage remains `unattributed`, never dropped.
Lane and chair quota totals share that verified identity; route labels cannot
turn unknown usage into an attributed account.
Conflicting known identities persist as a per-session override that masks dated rows
before account filtering. Ingest stores token counts
without prompt, reply, tool or path text, and rerunning it adds nothing. The
store is one segment per UTC day; a day that reaches its cap stops additions to
that day visibly, and no reader loads the whole store.
Historical context is never filled from the current release or instructions.
- Why: chair sessions are most of the spend, and a guessed row hides who spent it.

## Offline post-merge outcomes and suggestions

Outcome reports read local Git history and never fetch or change the checkout.
Unmatched rows stay `unattributed`, and revert or fix signals are escaped-defect
signals, not causality. A suggestion needs separated intervals with no worse
false-positive or escape rate in the same stratum, carries an expiry, and is
never applied by the report.
- Why: changing routing policy needs comparable cohorts and an explicit decision.
- Owner: `crates/telemetry/src/outcome.rs`, `crates/telemetry/src/suggest.rs`

## Demand counters are lossy and private

`usage.jsonl` records never carry argument, prompt or result text, are
best-effort single appends, and never create a state root. An I/O error drops
the event.
- Why: counting demand must not leak work content or slow a command.

Quota freshness belongs to [routing](routing.md#unknown-never-means-ready).
