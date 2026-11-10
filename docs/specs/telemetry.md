# Telemetry

Owner: `crates/telemetry` (reducer, prices, trace, usage log)
Anchor: `crates/telemetry/src/tests.rs`, `crates/telemetry/src/ingest/tests.rs`,
`crates/telemetry/tests/telemetry_pipeline.rs`

Telemetry summarises run's terminal attempt records into per-route outcomes,
usage and API-equivalent cost (the cost of the same tokens at published API
prices) using the bundled price table, and exports a trace.
There is no billing ledger or collector. Grammar is in the generated [CLI
reference](../reference/cli-all.md); quota is specified in [routing](routing.md).
Paths are relative to `tools/front-door/`.

`telemetry stats --by route` groups attempts by their resolved `model@effort`,
combining accounts for the same route. When effort is unknown the group is the
model alone; when the model is unknown the group is unattributed. Requested
routes do not fill missing resolved values.
- Test: `crates/telemetry/src/tests.rs::groups_by_resolved_model_and_effort`
- Owner: `crates/telemetry/src/`, `crates/cli/src/run/telemetry.rs`

Every `telemetry stats` report also includes `route_outcomes`, grouped by
resolved route, access mode, effort and pool assignment. Each row gives sample
size, complete successes, success rate with a Wilson interval, API-equivalent
cost per success and provider milliseconds per success. The last two divide
all measured attempt cost or time, including failures, by complete successes.
A successful settlement with a partial-completion signal is not a complete
success. Missing measurements and ratios with no successes remain unknown.
Only an original weighted pool draw is marked `hash_assigned_pool`; exact
routes, preference overrides and conditional redraws are observational. A draw
whose receipt lists a graph or flag exclusion in `considered_exclusions`, such
as a graph restart excluding its predecessors' models, depends on the earlier
attempt and is observational too; standing configuration exclusions are not.
- Test: `crates/telemetry/src/tests.rs::route_outcomes_report_success_cost_time_access_effort_and_assignment`;
  `crates/telemetry/src/tests.rs::route_outcomes_zero_success_and_unknown_cost_remain_unknown`;
  `crates/telemetry/src/tests.rs::pool_preferences_and_fallbacks_are_not_hash_comparisons`;
  `tests/telemetry_pipeline_cli.rs::stats_reports_route_access_effort_outcomes_and_nonrandom_comparison`
- Owner: `crates/telemetry/src/route_outcomes.rs`, `crates/cli/src/run/telemetry.rs`

## A report cannot touch a lane

Reporting starts no provider operation or background collection, and run does
not depend on telemetry. Attempt reports read through run's `AttemptReader`;
native-session ingest owns separate usage records. An absent run store gives
an empty attempt report and creates nothing; a corrupt one is an error.
- Why (default): measurement must never perturb what it measures.
- Test: `tests/workspace_policy.rs::internal_edges_obey_the_layering_table`;
  `crates/telemetry/src/tests.rs::empty_reader_has_complete_zero_cost`
- Owner: `crates/telemetry/src/`

## Bounded, honest totals

The reducer accepts at most 100,000 attempts, 10,000 groups and 1 MiB of keys.
It checks each limit before inserting and returns `TooLarge`, never partial
totals.
Sums add known values and count the rest as missing; `cost.complete` is null
when any cost is unknown. Known totals and missing coverage must be reported
separately. A report with no known contribution says cost is unknown; partial
coverage is labelled as a lower bound. Known contributions from partially priced
attempts remain in the lower bound. Measured zero and
an empty report retain a numeric zero. An unpriced subscription is not free.
The latest task outcome is the highest-numbered attempt.
`subscription_attempts` counts attempts whose exact provider and model have a
subscription entry in the price table, even when usage or rates are missing.
Unknown identities are not classified as subscriptions, and this classification
does not verify account billing.
- Why (default): a partial or zero-filled total reads as a real one.
- Test: `crates/telemetry/src/tests.rs::attempt_and_group_limits_are_enforced_before_insertion`;
  `crates/telemetry/src/tests.rs::missing_usage_and_prices_preserve_known_lower_bounds`
- Owner: `crates/telemetry/src/`

## Exact money and prices

Telemetry never subtracts again from counts the provider has already
normalised. Cost accumulates in checked integers, is divided by 10^6 once and
rounded half-up once, and floating point is never used. Prices are keyed by exact provider and model; an omitted
rate is unknown. A zero rate must be verified. There is no runtime fetch,
regex or alias. Cost is an estimate, not a bill.
- Why (default): rounding drift or a guessed price corrupts route comparison.
- Test: `crates/telemetry/src/tests.rs::rounding_occurs_once_across_categories_attempts_and_groups`;
  `crates/telemetry/src/tests.rs::price_validation_refuses_unsupported_units_duplicates_and_unverified_zero`
- Owner: `crates/telemetry/src/`, `../../config/telemetry-prices.toml`

## Price sheet refresh

- Price sheet: `telemetry prices refresh` proposes a diff from OpenRouter and
  LiteLLM and never applies it; a cross-source discrepancy above
  `discrepancy_threshold_bps` is reported, not resolved. Test:
  `crates/telemetry/src/refresh.rs::tests::converts_per_token_to_exact_microdollars_per_million`;
  `tests/telemetry_native.rs::prices_refresh_proposes_a_deterministic_diff_and_never_edits_the_sheet`;
  `crates/telemetry/src/tests.rs::review_older_than_the_configured_age_is_stale`
- Owner: `crates/telemetry/src/refresh.rs`

## Cache-read share is a first-class figure

Reports show cache-read share alongside its coverage. Only records
whose provider reported cache reads enter the share; a record that reports no
cache split is counted as `missing`, never as a miss. Cache reads are the
cheapest input. Provider decoders keep the categories disjoint. Codex
counts cached and newly cached tokens inside `input_tokens`, so the decoder
subtracts both; Claude and Cursor report them beside the uncached count. Agy's
`cache_read_tokens` is not recorded, because nothing shows whether its
`input_tokens` already contains it. `cache_write_1h` is the long-TTL part of
`cache_write` where the provider splits writes (Claude); prices stay at the
table's single write rate.
An absent `reasoning` count never makes a cost a lower bound: reasoning is
priced at the output rate and providers count it inside output.
- Why (default): a share that counts providers which never report a split reads as a
  cache miss, and a token total that includes cached input is not input.
- Test: `crates/telemetry/src/tests.rs::cache_read_share_is_reported_per_model_account_provider_and_source`;
  `crates/providers/src/tests_stream_outcomes.rs::codex_cached_input_is_cache_read_and_leaves_uncached_input`
- Owner: `crates/telemetry/src/reducer.rs`, `crates/providers/src/`

## Chair usage is read from the session transcript

The chair's SessionStart hook records `chair.session` with the serving account
key resolved from `CLAUDE_CONFIG_DIR` (or `~/.claude`), the session ID, the
optional project and the config directory relative to home. A config directory
that is not a registered account, the default `~/.claude` included, is recorded
under the reserved account `unattributed` and reported as such; it is never
dropped and never guessed. Launch the chair with `CLAUDE_CONFIG_DIR` set to a
registered account's directory to attribute it. Tokens do not exist at session
start, so `quota usage` and `telemetry stats` read them when they run, from the
session's own `projects/*/<session>.jsonl` and its `subagents/agent-*.jsonl`:
only assistant message IDs, models and usage counts, each message once at its
largest counts. Reads are bounded (1 GiB a transcript, 2 GiB a report, 8 MiB a
line) and never follow links. A session whose transcript cannot be read is
counted as unread with its reason, not as zero. `telemetry stats` shows the
host's chair sessions in a `chair` section priced like lanes; `quota usage`
shows them per account and project beside lane tokens.
- Why (default): chair sessions are most of the spend, and an unattributed or zero row
  hides who spent it.
- Test: `crates/telemetry/src/chair/tests.rs`;
  `tests/chair.rs::chair_start_records_serving_account_without_a_pending_handoff`;
  `tests/chair.rs::chair_on_an_unregistered_config_directory_is_recorded_as_unattributed`
- Owner: `crates/telemetry/src/chair.rs`, `src/chair.rs`, `crates/cli/src/quota.rs`

## Native session ingest

Native ingest reads supported session metadata and stores disjoint token
buckets without prompt, reply, tool or path text. Attribution uses registered
directories, never credentials. Unregistered sessions remain visible as
unattributed; identity must never be guessed. Message identity is scoped to its
session and serving directory. Repeated streamed messages merge at their largest
counts, and rerunning ingest adds no duplicate usage. Hook attribution may
upgrade an existing worker row to chair usage. Rows this build cannot decode
survive rewriting byte for byte.

Tracked native attempts join sub-agent usage only by the matching serving
directory, session and agent identity. A later settlement can link existing
rows without rereading transcripts; unmatched usage remains visible. Native and
chair breakdowns may overlap and must not be added twice.

Hooks request best-effort ingest within their wall deadline; manual ingest
scans the requested range independently. Incremental cursors retain only bounded
non-secret file identity and digest evidence. Replacements, truncation or store
changes invalidate a cursor. Incomplete lines are retried. Publish usage before
its cursor, so interruption can cause a reread but never skip committed input.

Reads and stores are bounded and never follow links. Missing message timestamps
are counted as skipped. Reaching the store cap stops additions visibly instead
of manufacturing complete coverage. The [CLI reference](../reference/cli-all.md)
owns invocation; `crates/telemetry/src/ingest.rs` owns limits and record layout.
- Why (default): most Claude spend is the interactive chair and its native sub-agents,
  which no supervised attempt record sees.
- Test: `crates/telemetry/src/ingest/tests.rs::rerun_adds_nothing_and_accounts_stay_distinct`;
  `crates/telemetry/src/ingest/tests.rs::lines_this_build_cannot_read_survive_a_rewrite`;
  `crates/telemetry/src/ingest/tests.rs::incremental_ingest_resumes_complete_lines_and_recovers_replacements`;
  `crates/telemetry/src/ingest/tests.rs::native_attempt_links_only_its_matching_subagent_and_updates_after_settlement`
- Owner: `crates/telemetry/src/ingest.rs`, `src/usage.rs`, `src/hooks.rs`

## Behaviour context and comparability

`telemetry stats` exposes `attempt_context`, `task_context`, `reply_context`,
`context_bands` and `cli_ergonomics`. Attempt figures cover selected finished project attempts;
native reply and demand-log figures cover the host's selected time window.
`context_bands` prices each native reply and groups it by source and by the
context it sent (fresh input, cached reads and cache writes), in bands starting
at 0, 50k, 100k, 200k and 400k tokens, so spend at large contexts can be
compared across windows.
These breakdowns do not add usage to the totals again. A run selection omits
the host-wide reply breakdown because it cannot be attributed to that run.

At attempt creation, the host stores observations in the existing trace,
with source, effective time and scope. It captures the compiled run-package
version. When an adjacent installed release manifest matches the binary's
compiled source, asset, target and toolchain identity and the executable's
size, it also records that manifest's install-verified executable SHA-256 and
size and its release source digest. Attempt creation is on the launch path, so
it never reads or hashes the executable; development builds leave both unknown.
The instruction observation hashes only the observed source-root `AGENTS.md`,
not the full effective session instructions. Cloud execution leaves that local
file observation absent. Harness version, harness experiment, system prompt
and tool schema remain unknown when the host does not expose them. A recorded
Provenant experiment arm is separate from a harness experiment.

Chair attribution reads the existing host-owned chair index by project and
run identity; request proposals and provider session identifiers cannot supply
that join. Graph and node labels come from existing work context. Each field
reports known and unknown coverage. Missing historical context is never filled
from the reporting host's current release or instructions.
Each attempt context row includes that exact attempt's usage and API-equivalent
cost from the existing reducer beside its chair and graph binding. These are
individually rounded breakdowns; use report totals for aggregate cost.

Native reply rows retain structural event enums separately from inferred
trigger labels. Narrow status questions and recognised notification/error
wrappers may inform inference; their text is discarded. Multiple causes are
mixed, unsupported evidence remains unknown or mixed, and no lane's aggregate
reasoning count is allocated to native replies. Thinking means provider-reported
reasoning tokens. Output counts remain `output_tokens_proxy`, with their own
unit. Explicit compaction boundaries attach to the next reply; absent historical
reply context has unknown compaction coverage.

CLI ergonomics counts help calls, failures followed by the same verb within
five minutes, first observed successes, waits and polls. Retry correlation
requires an existing attempt identity, or session plus project identity. Help calls
do not interrupt a sequence. The log is lossy; first observed success is not
proof of first-ever success. Parse errors have no known verb and cannot supply
a same-verb retry. Historical wait modes and missing correlation stay unknown.

Task difficulty joins risk tier and distinct graph/node pairs. Committed diff
size and file counts use captured task base/head object IDs, with at most 100
distinct comparisons; missing objects and binary line totals remain unknown.
Review findings and repair rounds reuse the latest graph checkpoint. Acceptance
item counts, first-pass CI and per-attempt reverts remain unknown without
retained attributable evidence; post-merge outcomes still use the separate
offline pipeline. Prompt text is not parsed into a guessed acceptance count.

An accepted outcome is one distinct delivery ID with a recorded accepted
workflow receipt among selected attempts. Provider success alone is not
acceptance. Cost per accepted outcome divides the complete API-equivalent cost
of selected attempts linked to those accepted deliveries by that count, as an
exact rational number of microdollars. Missing cost or a zero denominator stays
unknown. This excludes unattributed chair cost and attempts outside the selected
window; receipt decisions and graph checkpoints are labelled latest snapshots.

These are observational comparisons. Missing joint coverage or simultaneous
changes prevent separating a release effect from harness experiments, task
difficulty or user activity; every stats report states that limitation.

Storage adds optional trace, row and demand-log fields without changing
their existing versions. Serde defaults read pending and historical records;
unknown row versions continue to survive rewriting. Cursor state retains pending
structural events across a routine upgrade. The derived cursor cache advances to
version 2; version-1 cursors trigger a deduplicated transcript replay because
they did not retain pending events. No prompt or response text, remote
collection or parallel identity store is introduced.
- Test: `crates/telemetry/src/tests.rs::lane_context_fixture_joins_existing_chair_and_graph`;
  `crates/telemetry/src/ingest/tests.rs::ambiguous_native_trigger_and_unreported_reasoning_stay_distinct`;
  `crates/telemetry/src/ingest/tests.rs::enrichment_preserves_existing_token_buckets_and_excludes_text`;
  `crates/telemetry/src/usage/tests.rs::failed_verb_retry_help_and_wait_poll_are_counted`;
  `tests/telemetry_ingest.rs::telemetry_stats_reports_context_coverage_reasoning_and_acceptance_denominator`
- Owner: `crates/telemetry/src/context.rs`, `crates/telemetry/src/signals.rs`,
  `crates/run/src/telemetry_context.rs`, `crates/cli/src/run/telemetry_context.rs`

## Mileage and effective rates

`provenant telemetry mileage [--account] [--window] [--model] [--since] [--json]`
joins the quota history to ingested usage priced by the reviewed table. Mileage
is the API-equivalent cost of one percentage point of a window between two
distinct readings in one reset period; capacity is the cost of a full window
(mileage times 100). A reset, an advance of the reset time, a fall in the used
percentage or a plan change ends a run, so no segment is negative. A reset
time that advances by less than a minute is the same window reporting with
jitter, not a reset. A drop with no reset time is segmented the same way and
flagged `reset: inferred`.
Each figure is the median over the last seven days, with the segment count `n`
and the priced share.

The step change compares capacities in the last seven days with the prior 28 at
the same plan and tier. It is `flagged` only for a fall of at least 20% with at
least ten priced segments on both sides; otherwise it is `inconclusive`. A
`plan.change` run boundary reports the capacity change around the upgrade.

Effective rates apply the plan's discount factor `price / capacity` to the
average API rate per bucket for the potential rate at full use, and divide the
plan price by tokens actually used for the realised rate. The period is the
report window: the monthly plan price is prorated to it, and one window's
capacity is expanded to the number of full windows in the period. Both carry
the window's latest used percentage (`latest_used_basis_points`, the value at
its latest reading, not averaged over the period), `n` and priced token
coverage. Every effective report says token use is a lower bound and realised
rates are upper bounds, because ingest coverage of the account is not checked.
`--model` narrows only the tokens: quota deltas cover every model, so mileage
under a model filter is understated.
Fitted non-negative weights for `delta used = sum w_b x tokens_b` are reported
per account, window and model only with `n` and the fit's R-squared, and only
when the fit explains at least half the variance. All money is integer
microdollars, rounded once.
- Why (default): a subscription's value is what a percentage point costs, not what the
  list price says a token costs.
- Test: `crates/telemetry/src/mileage.rs::tests::three_readings_give_exact_mileage_and_capacity`;
  `crates/telemetry/src/mileage.rs::a_thirty_percent_fall_at_a_constant_plan_is_flagged`;
  `crates/telemetry/src/mileage.rs::a_ten_percent_fall_or_too_few_runs_is_inconclusive`;
  `crates/telemetry/src/mileage.rs::reset_time_jitter_under_a_minute_stays_in_one_window`;
  `crates/telemetry/src/mileage.rs::full_price_coverage_still_reports_the_bound_note`;
  `crates/cli/src/mileage.rs::tests::an_interval_takes_each_row_once_in_its_half_open_bounds`;
  `tests/telemetry_mileage.rs::three_readings_give_exact_mileage_capacity_and_effective_rates`
- Owner: `crates/telemetry/src/mileage.rs`, `crates/cli/src/mileage.rs`

## Trace export

`telemetry trace` writes Chrome Trace Event JSON, with one process per host and
one thread per attempt, and flow arrows from a child to its `Trace.parent`
across hosts. An
unreadable peer is listed with its error and the rest of the export is still written.
- Why (default): one peer outage must not hide the local picture.
- Test: `crates/telemetry/src/tests.rs::trace_groups_hosts_and_links_a_child_to_its_parent_across_them`;
  `crates/cli/src/run/remote.rs::peer_export_asks_every_configured_peer_for_its_lanes`
- Owner: `crates/telemetry/src/`

## Offline post-merge outcomes and suggestions

`telemetry stats --outcomes` adds an offline `pipeline` report. `--suggest`
also requests the same report with advisory comparisons. The reducer reads
first-parent history of `--main-ref` (default `main`) through the shared Git
executable resolver. It never fetches or changes the checkout. A merge subject
`Merge pull request #N` joins to a graph checkpoint's PR or a retained context
PR. Without a PR, an exact `Closes #N` body line joins the work item. Ambiguous
matches and rows without a graph or matching merge remain `unattributed`.

Origin merges come from first-parent history; reachable non-merge descendants
also include changes delivered through later pull requests.
The configurable `--outcome-days` window defaults to 14 days after merge.
Reverts identify the merge or a commit in its second-parent range by the
canonical revert trailer or its quoted subject. Follow-up commits count once when their subject begins with the word `fix`,
`bugfix` or `hotfix` (case insensitive), and a removed or replaced line blames
to a line the merge introduced. Blame identities handle shifted line numbers;
renames, insertion-only fixes, binary files and later edits that have already
replaced the original line are outside this conservative matcher. These are
escaped-defect signals, not proof of causality. Reverts are not counted again
as follow-up fixes.

An optional `--ci-file` accepts the recorded main-branch run-list JSON described
in the runbook. A failed run joins the nearest preceding merge in its head's
first-parent ancestry, within the window. Identical head, workflow and creation
time rows count once. Without this file, `ci` is `not_measured` and per-row CI
counts are null. The earliest `createdAt` is exposed as `ci_coverage_from`;
mature merges older than that date have null CI counts. A supplied empty file has no known coverage, so merge CI counts are null.
A coverage date does not prove that the export covers every workflow. Runs whose head SHA is unavailable locally are counted as unattributed
and skipped.

Ranking compares pattern, lens (explicit context lens, otherwise role) and
model separately within risk-tier and task-kind strata. Retained context extras
supply these strata where present; absent values stay `unknown`. Both strata
must be known for a comparison to be sufficient; unknown strata are reported
as insufficient with a reason. Escapes are attributed per PR, so every node
arm in that PR is charged with the PR outcome. This change
adds no context capture. Nodes combine selected attempts' costs, including
retries. A node attributes to a model only when every selected attempt names
the same model; mixed or missing identities stay `unknown` for model ranking.
Only leaf source findings enter the
confirmed and false-positive counts: graph-local IDs join `accepted` and
`rejected` verifier dispositions, deduplicated across verifier copies.
Conflicting, deferred or absent dispositions, ambiguous source IDs and missing
checkpoints remain unmeasured. Verifier copies contribute their costs and no
extra findings.

Arms show confirmed findings per API-equivalent dollar, false-positive rate,
and the fraction of distinct mature attributed merges with any escaped signal.
Only mature merges enter an attributed ranking cohort. The default minimum is
20 distinct graphs and mature merges per arm (`--min-samples`). Unknown costs,
missing dispositions or CI input and smaller arms are `insufficient`.
Without CI input, escaped-rate values and intervals are null; its retained
numerator counts only the observed Git signals.
Rates have 95% Wilson intervals. The confirmed-per-dollar interval scales the
Wilson confirmed share by observed dispositions per dollar, conditional on
observed costs; it does not model cost variability or finding correlation.
Raw finding volume never determines preference. Advice requires separated
confirmed-per-dollar intervals and no higher observed false-positive or
escaped-defect rates in the same stratum.

A suggestion includes both sample counts, intervals, references to its full
arm evidence and an RFC3339 expiry, default 14 days (`--expiry-days`). Reports
never apply changes. A model suggestion's `accept` is a configuration line under `[domains.routing]`,
carrying the same expiry; append its row if `model_choices` already exists. To accept it
through the CLI, use `provenant route override set --model ADAPTER/ID
--risk-tier TIER --task-kind KIND --until RFC3339`, using the suggestion's
values. Pattern and lens suggestions remain advisory with `accept: null`.
Whole-patch-read measurement has no read-event source and is unmeasured.

- Why (default): post-merge quality and verified value need evidence coverage, comparable
  cohorts and an explicit decision before changing policy.
- Test: `crates/telemetry/tests/telemetry_pipeline.rs`;
  `tests/telemetry_pipeline_cli.rs`
- Owner: `crates/telemetry/src/outcome.rs`, `crates/telemetry/src/suggest.rs`,
  `crates/cli/src/run.rs`

## Demand counters are lossy and private

`usage.jsonl` records are appended best-effort under an existing root, never
creating one. They carry the command or tool name, time, duration and outcome, never
argument, prompt or result text. Each line is one bounded `O_APPEND` write.
Only rotation takes a non-blocking lock, and an I/O error discards the event.
Recording the project is opt-in and stores only the `ProjectId` hash.
- Why (default): counting demand must not leak work content or slow a command.
- Test: `tests/usage.rs::cli_and_mcp_usage_records_exclude_arguments_prompts_and_results`;
  `crates/telemetry/src/usage/tests.rs::recording_on_a_missing_root_creates_nothing`
- Owner: `crates/telemetry/src/usage.rs`, `src/usage.rs`

## Quota display stays honest

A failed refresh keeps the last snapshot, marked stale. The menu bar
never shows a stale value as zero usage. An unregistered icon account falls back
to `most_constrained` with a warning and leaves config unchanged.
- Why (default): stale data must not look like free headroom.
- Test: `tests/quota.rs::failed_auth_refresh_shows_last_known_usage_age_and_auth_marker`;
  `tests/quota.rs::show_warns_when_display_icon_account_is_not_registered`
- Owner: `crates/cli/src/quota.rs`

`telemetry stats --detail --json` includes the recorded route decision and pool
selection receipt for each selected terminal attempt. Older attempts can omit
these fields.

## Not provided

- A ledger, OpenTelemetry ingest or collector; prompt and tool logging stays off.
- A telemetry MCP tool or an `--account` selector on `telemetry stats`.
