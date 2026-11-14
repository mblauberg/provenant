# Telemetry

Owner: `crates/telemetry` (reducer, prices, trace, usage log, usage ledger)
Anchor: `crates/telemetry/src/tests.rs`, `crates/telemetry/src/ingest/tests.rs`,
`crates/telemetry/src/ledger/tests.rs`, `crates/telemetry/tests/telemetry_pipeline.rs`

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
- Test: `tests/workspace_policy.rs::internal_edges_obey_the_layering_table`;
  `crates/telemetry/src/tests.rs::empty_reader_has_complete_zero_cost`

## The usage ledger outlives the run store

One append-only format-2 ledger row per terminal attempt, priced at settlement
and kept with no age limit. The settlement reaction appends it, so a retried
settlement adds nothing; an unreadable price table leaves the reaction pending
rather than writing an unpriced row. Unrecognised lines are reported, never
skipped. `telemetry cost` and `telemetry value` are the only producers of cost
and read only this ledger. Each row carries its required work graph and node.
The format-1 reset must discard `usage-ledger/`, the prior schema-1 `ingest/`
segments and `ingest-cursors.json`. Format 2 reads only the format-2 ledger and
schema-2 ingest rows. Removing the old ingest cursors lets later requests replay
transcripts they select instead of skipping them by a matching old cursor digest;
any `since` bound on that request still applies. The reset may also remove the
inert single-file `ingest.jsonl` and `ingest.jsonl.unmigrated` paths.
- Why: lane usage cannot be regenerated once run history is dropped, and one
  reader of one ledger cannot disagree with itself.
- Test: `crates/telemetry/src/ledger/tests.rs::a_settled_attempt_appends_one_row_even_when_settlement_is_retried`;
  `::an_unrecognised_line_is_reported_by_segment_and_line`;
  `src/settlement.rs::tests::settlement_writes_one_priced_ledger_row_and_a_failed_append_stays_pending`;
  `crates/telemetry/src/ledger/tests.rs::settled_cost_for_a_past_window_equals_the_telemetry_report`;
  `tests/telemetry_value_cost.rs::cost_reports_unpriced_and_unattributed_attempts_without_zero_filling`
- Owner: `crates/telemetry/src/ledger.rs`, `src/settlement.rs`

## Bounded, honest totals

The reducer refuses over-limit input with `TooLarge`, never partial totals.
Known values and missing coverage are reported separately: partial coverage is
a lower bound, all-unknown is unknown, and only a measured zero is zero. An
unpriced subscription is not free; an attempt with no account is
`unattributed`, never folded into another. A provider that reports no cache
split is `missing` in the cache-read share, never a miss.
- Why: a partial or zero-filled total reads as a real one.
- Test: `crates/telemetry/src/tests.rs::attempt_and_group_limits_are_enforced_before_insertion`;
  `::missing_usage_and_prices_preserve_known_lower_bounds`;
  `::cache_read_share_is_reported_per_model_account_provider_and_source`;
  `::route_outcomes_zero_success_and_unknown_time_remain_unknown`

## Exact money and prices

Cost accumulates in checked integers and rounds once; no floating point.
Prices are keyed by exact provider and model, an omitted rate is unknown, a zero
rate must be verified, and there is no runtime fetch or alias. A price refresh
proposes a diff and never applies it.
- Why: rounding drift or a guessed price corrupts route comparison.
- Test: `crates/telemetry/src/tests.rs::rounding_occurs_once_across_categories_attempts_and_groups`;
  `::price_validation_refuses_unsupported_units_duplicates_and_unverified_zero`;
  `tests/telemetry_native.rs::prices_refresh_proposes_a_deterministic_diff_and_never_edits_the_sheet`

## Attribution is detected, never guessed

Chair and native usage is attributed only by a registered config directory;
anything else is `unattributed`, never dropped. Ingest stores token counts
without prompt, reply, tool or path text, and rerunning it adds nothing. The
store is one segment per UTC day; a day that reaches its cap stops additions to
that day visibly, and no reader loads the whole store.
Historical context is never filled from the current release or instructions.
- Why: chair sessions are most of the spend, and a guessed row hides who spent it.
- Test: `tests/chair.rs::chair_on_an_unregistered_config_directory_is_recorded_as_unattributed`;
  `crates/telemetry/src/ingest/tests.rs::rerun_adds_nothing_and_accounts_stay_distinct`;
  `::enrichment_preserves_existing_token_buckets_and_excludes_text`;
  `crates/telemetry/src/tests.rs::lane_context_fixture_joins_existing_chair_and_graph`

## Offline post-merge outcomes and suggestions

Outcome reports read local Git history and never fetch or change the checkout.
Unmatched rows stay `unattributed`, and revert or fix signals are escaped-defect
signals, not causality. A suggestion needs separated intervals with no worse
false-positive or escape rate in the same stratum, carries an expiry, and is
never applied by the report.
- Why: changing routing policy needs comparable cohorts and an explicit decision.
- Test: `crates/telemetry/tests/telemetry_pipeline.rs`; `tests/telemetry_pipeline_cli.rs`
- Owner: `crates/telemetry/src/outcome.rs`, `crates/telemetry/src/suggest.rs`

## Demand counters are lossy and private

`usage.jsonl` records never carry argument, prompt or result text, are
best-effort single appends, and never create a state root. An I/O error drops
the event.
- Why: counting demand must not leak work content or slow a command.
- Test: `tests/usage.rs::cli_usage_records_exclude_arguments_prompts_and_results`;
  `crates/telemetry/src/usage/tests.rs::recording_on_a_missing_root_creates_nothing`

## Quota display stays honest

A failed refresh keeps the last snapshot marked stale; stale is never shown as
zero usage.
- Test: `tests/quota.rs::failed_auth_refresh_shows_last_known_usage_age_and_auth_marker`
