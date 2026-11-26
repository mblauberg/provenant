# Telemetry

Owner: `crates/telemetry` (reducer, prices, trace, usage log, usage ledger)
Anchor: `crates/telemetry/src/lib.rs`, `crates/telemetry/src/ingest.rs`,
`crates/telemetry/src/ledger.rs`

Telemetry reports outcomes, usage and API-equivalent cost (the same tokens at
published API prices) from terminal attempts and native sessions. Cost is an
estimate, never a bill. The [telemetry runbook](../runbooks/telemetry.md) owns
use. Paths are relative to `tools/front-door/`. Trace exports identify attempts
by graph and node when the fact has that context; missing context stays unknown.

## A report cannot touch a lane

Reporting starts no provider operation or background collection, and run does not
depend on telemetry. An absent run store gives an empty report and creates
nothing.
- Why: measurement must never perturb what it measures.

## The usage ledger outlives the run store

The append-only ledger retains one priced row per terminal attempt. Retried
settlement adds nothing, an unreadable price table leaves settlement pending, and
unrecognised lines are reported. Cost reports read the ledger.
- Why: lane usage cannot be regenerated once run history is dropped, and one
  reader of one ledger cannot disagree with itself.
- Owner: `crates/telemetry/src/ledger.rs`, `src/settlement.rs`

## Usage merges by declared semantics

Provider usage travels as one typed report. An adapter builds it from the
buckets it actually observed and declares, once in `AdapterSpec`, what one
report means: a step or the run's total, whether the stated input includes
cache reads and writes, and whether reports arrive while the run can still be
stopped. One `UsageAccumulator` applies that declaration: a keyed step report
replaces its key's earlier report, keyless step reports add, and a cumulative
report replaces the running total. An unstated bucket stays absent, never a
guessed zero, and input is normalised to exclude cache reads only where the
adapter declared they were included. No reader infers any of this from a
provider's name.

The ledger moves to v4 on upgrade. Buckets carry over and v3 source spellings
map through one exhaustive table; a line whose source cannot be mapped is
reported and copied unchanged to a `.v3-unmapped` sidecar, never guessed.
- Why: a last-wins fold undercounts multi-turn usage, and a guessed source
  would attribute spend to an account or host that never reported it.
- Owner: `crates/providers/src/usage.rs`, `crates/telemetry/src/ledger.rs`

## Bounded, honest totals

The reducer refuses over-limit input, never partial totals. Known values and
missing coverage are reported separately: partial coverage is a lower bound,
all-unknown is unknown, and only a measured zero is zero. An unpriced
subscription is not free, an attempt with no account is `unattributed` and never
folded into another, and a provider that reports no cache split is `missing` in
the cache-read share, never a miss.
- Why: a partial or zero-filled total reads as a real one.

## Per-window value is measured, never filled in

`telemetry value` is the only producer of plan value, effective rate and token
totals by account. It joins the usage ledger with quota history: for each
requested window it reports API-equivalent cost per full window and its trend,
the value multiple against the plan price in force, the share unused at the last
reset, a sample count and priced coverage. Credit accounts add a draw-down rate
and days to exhaustion, or a named unavailable reason. The plan price and the
window set are stated with the source that supplied them.

An unused share is the last reading before the reset, labelled
`observed_before_reset` with its timestamp and sample count; a reset with no
reading before it is named unavailable. If the immediately preceding snapshot
omits the pool, `missing_reading_before_reset` is reported instead of treating
the missing quota as zero. A reset, an unpriced bucket and an attribution gap
are reported as such and never zero-filled: unpriced cost is a bound, and a
failed optional read names its cause and degrades that field. A plan price is
compared with USD API-equivalent cost only when its currency is USD or it has
an explicit `notional_usd`; a foreign-currency amount is not treated as USD.
`--since` limits the report summary, while bounded-window cost uses the complete
ledger cycle. If the window has no bounds, full-window cost is named
`incomplete_window` rather than scaled from a partial report range.
Plan suggestions are expiring recommendations with evidence counts; nothing
applies them.
- Why: a full window priced from a partial window, or a guessed zero for an
  unobserved reset, reads as a measurement that was never made.
- Owner: `crates/telemetry/src/value.rs`, `crates/cli/src/telemetry_value.rs`

## Exact money and prices

Cost accumulates in checked integers and rounds once; no floating point. Prices
are keyed by exact provider and model, an omitted rate is unknown, a zero rate
must be verified, and there is no alias. Reports never fetch prices;
`provenant telemetry prices refresh` writes a proposed diff and never edits the
sheet.
- Why: rounding drift or a guessed price corrupts route comparison.

## Attribution is detected, never guessed

Usage rows keep per-session account identity, epoch and evidence source from the
provider identity port. Unavailable identity is typed unknown, never inferred
from a config-directory path, and unknown usage stays `unattributed`, never
dropped. Lane and chair quota totals share that verified identity, and route
labels cannot turn unknown usage into an attributed account. Conflicting known
identities persist as a per-session override that masks dated rows before
account filtering. Historical context is never filled from the current release
or instructions.

Ingest stores token counts without prompt, reply, tool or path text, and
rerunning it adds nothing. The store is one segment per UTC day; a day that
reaches its cap stops additions to that day visibly, and no reader loads the
whole store.
- Why: a guessed row hides who spent the tokens.

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
best-effort single appends, and never create a state root. An I/O error drops the
event.
- Why: counting demand must not leak work content or slow a command.

Quota freshness belongs to [routing](routing.md#unknown-never-means-ready).
