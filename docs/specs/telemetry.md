# Telemetry

Owner: `crates/telemetry` (reducer, prices, trace, usage log)
Anchor: `crates/telemetry/src/tests.rs`, `crates/telemetry/src/usage/tests.rs`,
`tests/usage.rs`

Telemetry reduces run's terminal attempt records with the bundled price table
into per-route outcomes, usage and API-equivalent cost, and exports a trace.
There is no ledger, store or collector. Grammar is in the generated [CLI
reference](../reference/cli.md); quota belongs to [routing](routing.md).
Paths are relative to `tools/front-door/`.

## A report cannot touch a lane

Telemetry owns no store, lock, background collection or provider operation,
and run does not depend on it. It reads only through run's `AttemptReader`.
An absent run store gives an empty report and creates nothing; a corrupt one
is an error.
- Why: measurement must never perturb what it measures.
- Test: `tests/workspace_policy.rs::internal_edges_obey_the_layering_table`;
  `crates/telemetry/src/tests.rs::empty_reader_has_complete_zero_cost`
- Owner: `crates/telemetry/src/`

## Bounded, honest totals

Reduction is bounded to 100,000 attempts, 10,000 groups and 1 MiB of keys,
checked before insertion; a bound returns `TooLarge`, never partial totals.
Sums add known values and count the rest as missing; `cost.complete` is null
when any cost is unknown, with the known lower bound reported. An unpriced
subscription is not free, and the latest task outcome is the
highest-numbered attempt.
- Why: a partial or zero-filled total reads as a real one.
- Test: `crates/telemetry/src/tests.rs::attempt_and_group_limits_are_enforced_before_insertion`;
  `::missing_usage_and_prices_preserve_known_lower_bounds`
- Owner: `crates/telemetry/src/`

## Exact money and prices

Telemetry never re-subtracts provider-normalised counts. Cost accumulates in
checked integers, divides by 10^6 once and rounds half-up once; floating
point is never used. Prices are keyed by exact provider and model; an omitted
rate is unknown, zero needs a verified zero, and there is no runtime fetch,
regex or alias. Cost is an estimate, not a bill.
- Why: rounding drift or a guessed price corrupts route comparison.
- Test: `crates/telemetry/src/tests.rs::rounding_occurs_once_across_categories_attempts_and_groups`;
  `::price_validation_refuses_unsupported_units_duplicates_and_unverified_zero`
- Owner: `crates/telemetry/src/`, `../../config/telemetry-prices.toml`

## Trace export

`telemetry trace` writes Chrome Trace Event JSON, one process per host and
one thread per attempt, with flow arrows to `Trace.parent` across hosts. An
unreadable peer is listed with its error and the rest of the export is still written.
- Why: one peer outage must not hide the local picture.
- Test: `crates/telemetry/src/tests.rs::trace_groups_hosts_and_links_a_child_to_its_parent_across_them`;
  `crates/cli/src/run/remote.rs::peer_export_asks_every_configured_peer_for_its_lanes`
- Owner: `crates/telemetry/src/`

## Demand counters are lossy and private

`usage.jsonl` records are appended best-effort under an existing root, never
creating one. They carry the verb or tool, time, duration and outcome, never
argument, prompt or result text. Each line is one bounded `O_APPEND` write;
only rotation takes a non-blocking lock, and I/O errors drop the event.
Project attribution is opt-in and records only the `ProjectId` hash.
- Why: counting demand must not leak work content or slow a command.
- Test: `tests/usage.rs::cli_and_mcp_usage_records_exclude_arguments_prompts_and_results`;
  `crates/telemetry/src/usage/tests.rs::recording_on_a_missing_root_creates_nothing`
- Owner: `crates/telemetry/src/usage.rs`, `src/usage.rs`

## Quota display stays honest

A failed refresh keeps the last snapshot, marked stale, and the menu bar
never reads staleness as zero usage. An unregistered icon account falls back
to `most_constrained` with a warning and leaves config unchanged.
- Why: stale data must not look like free headroom.
- Test: `tests/quota.rs::failed_auth_refresh_shows_last_known_usage_age_and_stale_marker`;
  `::show_warns_when_display_icon_account_is_not_registered`
- Owner: `crates/cli/src/quota.rs`

## Not provided

- A ledger, OpenTelemetry ingest or collector; prompt and tool logging stays off.
- Delivery cost columns, a telemetry MCP tool or an `--account` selector.
