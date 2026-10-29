# Telemetry

Owner: `crates/telemetry` (reducer, prices, trace, usage log)
Anchor: `crates/telemetry/src/tests.rs`, `crates/telemetry/src/usage/tests.rs`,
`crates/telemetry/src/native.rs`, `crates/telemetry/src/ingest/tests.rs`,
`crates/telemetry/src/mileage.rs`, `crates/telemetry/src/chair/tests.rs`,
`tests/usage.rs`, `tests/telemetry_native.rs`, `tests/telemetry_ingest.rs`,
`tests/telemetry_mileage.rs`, `tests/chair.rs`,
`crates/telemetry/tests/telemetry_pipeline.rs`, `tests/telemetry_pipeline_cli.rs`

Telemetry summarises run's terminal attempt records into per-route outcomes,
usage and API-equivalent cost (the cost of the same tokens at published API
prices) using the bundled price table, and exports a trace.
There is no ledger, store or collector. Grammar is in the generated [CLI
reference](../reference/cli.md); quota is specified in [routing](routing.md).
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

The reducer accepts at most 100,000 attempts, 10,000 groups and 1 MiB of keys.
It checks each limit before inserting and returns `TooLarge`, never partial
totals.
Sums add known values and count the rest as missing; `cost.complete` is null
when any cost is unknown. Each `totals.cost` object (including route and detail
groups) contains `complete` and `known_lower_bound`, each a money object
(`microdollars`, integer, and `usd`, decimal string) or null, plus
`priced_attempts` and `missing_attempts` counts. Their sum equals `totals.attempts`.
`priced_attempts` counts attempts with complete cost data. If no attempt is
priced and no partial cost contribution is known, `known_lower_bound` is null
and the CLI prints `cost unknown (N attempts missing cost data)`. With partial
coverage it prints `at least $X (N of M attempts priced)`. Known contributions
from partially priced attempts remain in the lower bound. Measured zero and
an empty report retain a numeric zero. An unpriced subscription is not free.
The latest task outcome is the highest-numbered attempt.
`subscription_attempts` counts attempts whose exact provider and model have a
subscription entry in the price table, even when usage or rates are missing.
Unknown identities are not classified as subscriptions, and this classification
does not verify account billing.
- Why: a partial or zero-filled total reads as a real one.
- Test: `crates/telemetry/src/tests.rs::attempt_and_group_limits_are_enforced_before_insertion`;
  `::missing_usage_and_prices_preserve_known_lower_bounds`
- Owner: `crates/telemetry/src/`

## Exact money and prices

Telemetry never subtracts again from counts the provider has already
normalised. Cost accumulates in checked integers, is divided by 10^6 once and
rounded half-up once, and floating point is never used. Prices are keyed by exact provider and model; an omitted
rate is unknown. A zero rate must be verified. There is no runtime fetch,
regex or alias. Cost is an estimate, not a bill.
- Why: rounding drift or a guessed price corrupts route comparison.
- Test: `crates/telemetry/src/tests.rs::rounding_occurs_once_across_categories_attempts_and_groups`;
  `::price_validation_refuses_unsupported_units_duplicates_and_unverified_zero`
- Owner: `crates/telemetry/src/`, `../../config/telemetry-prices.toml`

## Native Claude Code sub-agents

An explicit `--claude-subagents PATH` report reads usage metadata from one
Claude session's `subagents` directory and groups usage and estimated cost by
model. Message bodies are not used. Message IDs are deduplicated. These records
are a standalone report, independent of the project and run store, and the
option cannot be combined with attempt filters or `--usage`. The report must
count and skip an incomplete, unterminated final JSON record while rejecting
malformed interior records. Native model costs use the same money object as
other telemetry costs and identify the reviewed price table. Pricing uses
exact model identities, including dated snapshots present in the table;
unknown or incomplete prices remain unknown.
- Price sheet: `telemetry prices refresh` proposes a diff from OpenRouter and
  LiteLLM and never applies it; a cross-source discrepancy above
  `discrepancy_threshold_bps` is reported, not resolved. Test:
  `crates/telemetry/src/refresh.rs::tests::converts_per_token_to_exact_microdollars_per_million`;
  `tests/telemetry_native.rs::prices_refresh_proposes_a_deterministic_diff_and_never_edits_the_sheet`;
  `crates/telemetry/src/tests.rs::review_older_than_the_configured_age_is_stale`
- Why: native sub-agent usage should be visible without counting it as a
  Provenant attempt or collecting conversation content.
- Test: `crates/telemetry/src/native.rs::tests::imports_metadata_once_per_message_and_prices_exact_model`;
  `tests/telemetry_native.rs::native_stats_runs_without_a_project_or_store_and_sanitises_output`
- Owner: `crates/telemetry/src/`

## Cache-read share is a first-class figure

Every `totals` object carries `cache`: the records that reported cache reads
and those that did not, their input tokens (uncached input, cache reads and
cache writes), `read_tokens`, `write_tokens`, `write_1h_tokens` and
`read_share_basis_points`, cache reads as a share of all input. Only records
whose provider reported cache reads enter the share; a record that reports no
cache split is counted as `missing`, never as a miss. The report's `cache`
section rolls the same figures up `by_model`, `by_account`, `by_provider` and
`by_source` (`lane` or `chair`). The share is the figure to raise: cache reads
are the cheapest input. Provider decoders keep the categories disjoint. Codex
counts cached and newly cached tokens inside `input_tokens`, so the decoder
subtracts both; Claude and Cursor report them beside the uncached count. Agy's
`cache_read_tokens` is not recorded, because nothing shows whether its
`input_tokens` already contains it. `cache_write_1h` is the long-TTL part of
`cache_write` where the provider splits writes (Claude); prices stay at the
table's single write rate.
An absent `reasoning` count never makes a cost a lower bound: reasoning is
priced at the output rate and providers count it inside output.
- Why: a share that counts providers which never report a split reads as a
  cache miss, and a token total that includes cached input is not input.
- Test: `crates/telemetry/src/tests.rs::cache_read_share_is_reported_per_model_account_provider_and_source`;
  `crates/providers/src/tests.rs::codex_cached_input_is_cache_read_and_leaves_uncached_input`
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
- Why: chair sessions are most of the spend, and an unattributed or zero row
  hides who spent it.
- Test: `crates/telemetry/src/chair/tests.rs`;
  `tests/chair.rs::chair_start_records_serving_account_without_a_pending_handoff`;
  `::chair_on_an_unregistered_config_directory_is_recorded_as_unattributed`
- Owner: `crates/telemetry/src/chair.rs`, `src/chair.rs`, `crates/cli/src/quota.rs`

## Native session ingest

`provenant telemetry ingest [--provider P] [--since T]` reads each Claude
session transcript under a config directory's `projects/<project>/<session>.jsonl`
and its `subagents/agent-*.jsonl`, and stores one row per assistant message in
`ingest.jsonl` under the state root. A row carries the account label, provider,
model, effort when the transcript names one, the five disjoint token buckets,
the message timestamp, session ID, message ID and a `source` of `chair`,
`subagent` or `lane`. Prompt, reply, tool and path text is never stored, and a
row is keyed by session and message ID, so a re-run adds nothing. A message
that streams repeatedly is kept once at its largest counts, mixing the
short-TTL and one-hour cache-write parts into separate buckets.
`--since` skips files last written before it and messages stamped before it;
`--provider` other than `claude` ingests nothing.

Attribution uses directories only, never credentials. Each registered account's
`directory` and its extra `directories` attribute to its label; a directory
claimed twice is visited once. Every other `~/.claude*` directory with a
`projects` child, including the default `~/.claude`, is stored under
`unattributed` and never dropped. A session recorded by the chair's
SessionStart hook is a `chair`, a file under a session's `subagents` directory
is a `subagent`, and every other session is a `lane`. The chair's SessionStart
hook also runs the last day of ingest, best effort, so the interactive session
is captured without a manual command.
Reads are bounded (1 GiB a transcript, 128 MiB a store, 8 MiB a line, 100,000
sessions, 2,000 subagent files a session) and never follow links. A message
without a timestamp is counted as skipped, not guessed. When the store reaches
its cap, ingest stops adding rows and the report says the store is full.
- Why: most Claude spend is the interactive chair and its native sub-agents,
  which no supervised attempt record sees.
- Test: `crates/telemetry/src/ingest/tests.rs::rerun_adds_nothing_and_accounts_stay_distinct`;
  `::registered_directories_get_their_label_and_the_rest_are_unattributed`;
  `tests/telemetry_ingest.rs::ingest_is_idempotent_and_attributes_by_directory`;
  `::chair_session_start_ingests_the_session_as_chair`
- Owner: `crates/telemetry/src/ingest.rs`, `src/usage.rs`, `src/hooks.rs`

## Mileage and effective rates

`provenant telemetry mileage [--account] [--window] [--model] [--since] [--json]`
joins the quota history to ingested usage priced by the reviewed table. Mileage
is the API-equivalent cost of one percentage point of a window between two
distinct readings in one reset period; capacity is the cost of a full window
(mileage times 100). A reset, an advance of the reset time, a fall in the used
percentage or a plan change ends a run, so no segment is negative. A drop with
no reset time is segmented the same way and flagged `reset: inferred`.
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
the window's utilisation, `n` and token coverage; while coverage is incomplete
the report says token use is a lower bound and realised rates are upper bounds.
Fitted non-negative weights for `delta used = sum w_b x tokens_b` are reported
per account, window and model only with `n` and the fit's R-squared, and only
when the fit explains at least half the variance. All money is integer
microdollars, rounded once.
- Why: a subscription's value is what a percentage point costs, not what the
  list price says a token costs.
- Test: `crates/telemetry/src/mileage.rs::tests::three_readings_give_exact_mileage_and_capacity`;
  `::a_thirty_percent_fall_at_a_constant_plan_is_flagged`;
  `::a_ten_percent_fall_or_too_few_runs_is_inconclusive`;
  `tests/telemetry_mileage.rs::three_readings_give_exact_mileage_capacity_and_effective_rates`
- Owner: `crates/telemetry/src/mileage.rs`, `crates/cli/src/mileage.rs`

## Trace export

`telemetry trace` writes Chrome Trace Event JSON, with one process per host and
one thread per attempt, and flow arrows from a child to its `Trace.parent`
across hosts. An
unreadable peer is listed with its error and the rest of the export is still written.
- Why: one peer outage must not hide the local picture.
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
Reverts identify the merge by the canonical revert trailer or its quoted
subject. Follow-up commits count once when their subject starts with `fix`,
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
counts are null. A supplied empty file measures zero runs; it does not prove
that the export covers every workflow or date.

Ranking compares pattern, lens (explicit context lens, otherwise role) and
model separately within risk-tier and task-kind strata. Retained context extras
supply these strata where present; absent values stay `unknown`. This change
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
never apply changes. Pattern and lens choices have no scheduled write path.
The current model-policy CLI also cannot schedule an arbitrary model choice:
`route override set` schedules pacing only. Suggestions therefore have
`accept: null` and explain that they are advisory. Graph node asks and the
#2179 whole-patch-read measurement remain outstanding under #2011; there is no
read-event source for that measurement.

- Why: post-merge quality and verified value need evidence coverage, comparable
  cohorts and an explicit decision before changing policy.
- Test: `crates/telemetry/tests/telemetry_pipeline.rs`;
  `tests/telemetry_pipeline_cli.rs`
- Owner: `crates/telemetry/src/outcome.rs`, `crates/telemetry/src/suggest.rs`,
  `crates/cli/src/run.rs`

Experiment assignments add `experiment_arm` to retained work context. Their
pattern label is `experiment:ID:ARM`, so `telemetry experiments show ID` uses
the same mature outcomes, cost coverage, CI and risk-tier/task-kind strata as
the pipeline ranking report. Shadow graphs carry the selected arm label while
executing the control shape. Comparisons are advisory and do not change preset
defaults. Assignment stops on expiry or when observed experiment spend reaches
`budget_usd`; absent spend evidence is not treated as zero by the report.

## Demand counters are lossy and private

`usage.jsonl` records are appended best-effort under an existing root, never
creating one. They carry the command or tool name, time, duration and outcome, never
argument, prompt or result text. Each line is one bounded `O_APPEND` write.
Only rotation takes a non-blocking lock, and an I/O error discards the event.
Recording the project is opt-in and stores only the `ProjectId` hash.
- Why: counting demand must not leak work content or slow a command.
- Test: `tests/usage.rs::cli_and_mcp_usage_records_exclude_arguments_prompts_and_results`;
  `crates/telemetry/src/usage/tests.rs::recording_on_a_missing_root_creates_nothing`
- Owner: `crates/telemetry/src/usage.rs`, `src/usage.rs`

## Quota display stays honest

A failed refresh keeps the last snapshot, marked stale. The menu bar
never shows a stale value as zero usage. An unregistered icon account falls back
to `most_constrained` with a warning and leaves config unchanged.
- Why: stale data must not look like free headroom.
- Test: `tests/quota.rs::failed_auth_refresh_shows_last_known_usage_age_and_stale_marker`;
  `::show_warns_when_display_icon_account_is_not_registered`
- Owner: `crates/cli/src/quota.rs`

## Not provided

- A ledger, OpenTelemetry ingest or collector; prompt and tool logging stays off.
- Delivery cost columns, a telemetry MCP tool or an `--account` selector on `telemetry stats`.
