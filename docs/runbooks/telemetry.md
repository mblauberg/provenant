# Telemetry and quota
Owner: provenant-telemetry
Anchor: tools/front-door/crates/telemetry/src

`provenant telemetry stats` reduces retained terminal attempt facts with prices
from `config/telemetry-prices.toml`. Optional `--since`, `--until DATE`,
`--run-id ID` and `--detail` bound or extend the report. `--since` takes a
number of seconds ago, a date or an RFC3339 timestamp. Use `--json` for
structured output. The contract is in the [telemetry spec](../specs/telemetry.md).

Cost is an API-equivalent estimate, not a bill. The report separately counts
attempts whose price-table entry is marked as a subscription, including those
whose token usage or API-equivalent price is unavailable. This classification
does not verify account billing. An unknown cost is not zero.

Group supervised attempts with `--by issue|graph|preset|role`. `issue` reads
the P7 `work_item` label; missing labels form an `unknown` group. Graph groups
also include the latest checkpoint's verdicts, findings and repair rounds,
plus selected attempts' partial-signal counts when available. `--by pr` joins the retained local
contribution store to PRs GitHub reports as merged, and apportions usage and
cost by each contribution's recorded weight. Missing weights are reported as
unknown allocations and counted at full value. PR lookup requires `gh` access
to the current repository. Add `--since` and `--until` to bound both the
attempt window and the merged-PR snapshot.

When a quota-history reading matches an account with configured plan prices,
JSON includes `plan_value`: the configured monthly price and API-equivalent
usage for the selected attempt window. These periods may differ; this is a
comparison, not a realised monthly rate or a bill.

For a wave digest, review `provenant inbox --state open` and then run
`provenant telemetry stats --by pr --since <wave-start>`. The inbox command
lists open requests across kinds; select the decision requests for the chair
summary. Both results are local reads, and PR rows include only merged PRs.

To inspect native Claude Code sub-agents for a session, run
`provenant telemetry stats --claude-subagents PATH`, where `PATH` is that
session's `subagents` directory under `~/.claude*/projects/<project>/<session>`.
The directory must contain only regular, non-symlink `agent-*.jsonl` files. This is an
explicit import and produces a separate usage and API-equivalent cost report
grouped by model; it does not add those calls to supervised attempt totals.
The import uses usage metadata, not message bodies. It cannot be combined with
`--run-id`, `--since`, `--until` or `--usage`.
With `--json`, the `native_claude_subagents` section reports agent and message
counts, usage, price-table billing and cost by model, with `cost_basis` set to
`api_equivalent_estimate`. A model's `cost_usd` is
null when its usage or price is incomplete. Model prices are matched exactly;
dated aliases are not inferred.

## Native session ingest

`provenant telemetry ingest [--provider claude] [--since T]` records native
Claude usage from every registered account directory and from every other
`~/.claude*` directory under home. Registered accounts use their label; any
other directory, including the default `~/.claude`, is `unattributed`. Each
assistant message becomes one row keyed by session and message ID, so running
the command again adds nothing. `--since` takes seconds ago, a date or an
RFC3339 timestamp and skips older files and messages. The chair's SessionStart
hook runs the last day of ingest automatically, so an interactive session is
captured without a manual command.

The store is `ingest.jsonl` under the state root. It holds counts, model,
effort, account, timestamp, session and message IDs and a `source` of `chair`,
`subagent` or `lane`; it never holds prompt, reply, tool or path text. A row
whose message streamed repeatedly keeps the largest count of each bucket.
Reads never follow links, and a message without a timestamp is counted as
skipped rather than guessed.

## Mileage

`provenant telemetry mileage [--account LABEL] [--window POOL] [--model MODEL]
[--since T] [--json]` reports what a quota percentage point costs. It joins the
quota history with ingested usage, priced by `config/telemetry-prices.toml`, so
run ingest first and let the quota refresh build history.

Mileage is the API-equivalent cost of one point between two distinct readings
in a reset period; capacity is the cost of a full window. A reset, an advanced
reset time, a used-percentage drop or a plan change starts a new run, and a run
that started from a drop with no reset time is flagged `reset: inferred`. The
headline is the median over the last seven days with `n` and the priced share.

`step_change` compares the last seven days with the prior 28 at the same plan
and tier. It is `flagged` for a fall of at least 20% with at least ten priced
segments on each side, and `inconclusive` otherwise; change the judgement with
`--min-runs`, `--fall-percent` and `--recent-days`. A plan change also reports
the capacity change around the upgrade.

`effective` reports, per bucket, the potential rate at full use (the API rate
times the plan's `price / capacity` discount) and the realised rate (the plan
price over tokens actually used), with the window's utilisation, `n` and token
coverage. The monthly plan price is prorated to the report window, and one
window's capacity is expanded to the full windows in it. Until ingest covers
the account, token use is a lower bound and the realised rate is an upper
bound; the report says so. Fitted bucket weights appear only with their sample
size and R-squared, and only when they explain at least half the variance. A
missing price table leaves rates unknown rather than guessing.

## Cache-read share

`telemetry stats` prints each model's, account's and source's cache-read share:
cache reads as a share of all input tokens. It is the figure to raise over
time, since cache reads are the cheapest input. `--json` carries the same under
`cache` (`by_model`, `by_account`, `by_provider`, `by_source`) and in every
`totals.cache`, with `read_share_basis_points`, cache writes and the one-hour
write part where the provider splits it. Records from a provider that reports
no cache split are counted as not reported, not as a miss. Compact at task
boundaries while the cache is warm, so the next task reads rather than rewrites.

## Chair usage

The chair's SessionStart hook records which account serves each session. Launch
the chair with `CLAUDE_CONFIG_DIR` set to the registered account's directory,
so attribution is unambiguous:

```sh
CLAUDE_CONFIG_DIR=~/.claude-work claude
```

A session on a config directory that is not a registered account, including the
default `~/.claude`, is reported under the account `unattributed`. Tokens are
read from the session's transcript when a report runs. `provenant quota usage`
lists chair and lane tokens per account and project, and `telemetry stats`
adds a `chair` section for the host's sessions, priced like lanes. Sessions
whose transcript cannot be read are counted as unread. Chair rows need no
extra setting; `usage.project_attribution` only adds the project.

## Offline timeline

For offline pipeline comparisons, export the chair's main-branch CI history
and pass the file to the local report:

```sh
gh run list --branch main --limit 1000 --json headSha,conclusion,createdAt,workflowName > main-ci.json
provenant telemetry stats --suggest --ci-file main-ci.json --main-ref main --json
```

The export must cover the merges and following observation windows being
compared. Increase the limit or export additional pages for a busy repository.
The telemetry command itself uses no network. Omit `--ci-file` to inspect local
reverts and line-overlapping follow-up fixes with CI marked `not_measured`.
Use `--outcomes` instead of `--suggest` to request the same outcome and ranking
evidence. `--since` and `--until` select attempt facts, while `--outcome-days`
sets the post-merge observation window (default 14 days).

`pipeline.outcomes.rows` retains originating graph, pattern, lens and model
alongside counts; unmatched evidence stays `unattributed`.
`pipeline.ranking.arms` shows sample counts, cost coverage, verifier counts,
rates and 95% intervals by risk tier and task kind. Missing strata are
`unknown`. Ranking waits for the full observation window and at least 20
graphs and mature merges per arm. Set `--min-samples`, `--outcome-days` and
`--expiry-days` to change these defaults; expiry is 14 days.

Review `pipeline.ranking.suggestions` with its referenced arm evidence before
making a routing decision. Each suggestion has counts, intervals and expiry.
`accept: null` means the choice is advisory: there is currently no scheduled
pattern, lens or arbitrary model-choice command. The report writes no override
and posts no graph ask. `route override set` is a pacing override and must not
be used as a model-choice command. See the
[telemetry spec](../specs/telemetry.md#offline-post-merge-outcomes-and-suggestions)
for the conservative line matcher and coverage limits.

Whole-patch read counts remain unavailable because provider telemetry supplies
no read-event source; token counts cannot stand in for that measurement.

## Attempt timeline

`provenant telemetry trace [--out FILE] [--since 604800] [--run ID] [--peers]`
writes retained attempts as Chrome Trace Event JSON (default
`provenant-trace.json`), with one process per host and one thread per attempt;
arrows join a child attempt to its parent, across hosts too. Open the file in
ui.perfetto.dev or chrome://tracing; no collector is needed. Add `--peers` to
read each configured peer's newest 100 lanes as well, whoever placed them; an
unreachable peer is reported and skipped.

## Price sheet freshness and refresh

`config/telemetry-prices.toml` records `reviewed_at`, optional per-model
`effective_from` and `effective_to` dates, and two settings:
`max_review_age_days` (when set, `telemetry stats` prints a warning and
`price_freshness.stale` is true once the review is older) and
`discrepancy_threshold_bps` (default 1000, i.e. 10%).

`provenant telemetry prices refresh [--source openrouter|litellm]` fetches
OpenRouter `/api/v1/models` and LiteLLM `model_prices_and_context_window.json`,
converts USD per token to microdollars per million tokens, and writes a proposal
(default `telemetry-prices.proposed.json`) with each source URL, the fetch time
and the rate changes. Only models with `openrouter_id` or `litellm_id` in the
sheet are compared. `--source` picks the primary; the other is a cross-check, and
a difference above the threshold is listed under `discrepancies`, never
resolved. `--openrouter-file` and `--litellm-file` read a recorded document
instead of fetching. The sheet is never edited: exit 0 means nothing to review,
exit 5 means a proposal awaits review. Vendor pages remain the record for
subscription-route equivalents.

## Local usage counters

`provenant telemetry stats --usage --since 2592000` counts CLI verbs and Fabric
MCP tools over the last 30 days, including installed units with zero calls.
Omit `--since` for all retained calls. Counts cover this host's `usage.jsonl`
and `usage.jsonl.1` under the selected Provenant state directory. A fresh host
counts nothing until the state root exists. The report includes its own call
on the next invocation.

Each CLI invocation records `cli.verb`, its canonical verb path, exit code,
duration in milliseconds and UTC timestamp. Parse failures use `parse`, help
uses `help`, and version uses `version`. Each MCP tool call records `mcp.tool`,
the installed tool name, outcome and timing; unrecognised names use `unknown`.
Arguments, prompts and results are never logged. Skill loads and scripts are
not counted. These counters are separate from token and cost reports, and do
not enable OTel prompt or tool logging.

Set `usage.project_attribution` to `true` to include the existing `ProjectId`
hash, rather than a path or name, in usage events (as `trace.project`) and group counts by project.
This reveals which projects use Provenant to anyone who can read the local
usage log, so it is off by default. Per-project rows are subsets of the
aggregate row for each kind and unit; do not sum them with the aggregate.
Text output labels these rows with `project=<id>`. JSON includes `project_id`
and `scope: "project"` on these rows; aggregate rows omit both fields.

Writes are best effort: one small `O_APPEND` write, no sync or background
collector. I/O failures leave command results unchanged. Only rotation takes
an exclusive nonblocking lock; a busy rotation lock still allows appends.
MCP rotation runs on existing workers, keeping it off the event loop. At
5 MB the current log replaces `.1`. Concurrent writes can briefly exceed the
cap. Reports read without locks, tolerate a missing `.1`, and skip malformed
or incomplete lines. A concurrent rotation can make a report approximate.
Only two logs are retained, so a 30-day query can cover less than 30 days on a
busy host. No config, provider or run-store state is created by these
counters.

## Quota

`provenant quota` reads cached observations. `provenant quota tick`
refreshes due accounts; use `--trigger` and `--power` to record why the
refresh ran and the current power source. `provenant quota display [--headline
auto|5h|weekly] [--icon most_constrained|ACCOUNT]` sets the menu bar choices,
then shows quota. `provenant telemetry stats --usage` reports local invocation
counts. Quota is admission advice; a reset or bookkeeping failure never grants
permission to stop a healthy provider.

### Quota history

The snapshot file for each account is overwritten on every refresh. To keep
the series, each refresh also appends one record to
`quota/history/<account_key>.<adapter>.jsonl` when the reading differs from the
last one. An identical reading appends nothing. A record holds the observed
time, account key and label, provider, the plan the account reports, every
window (pool, window seconds, used basis points, reset time), credit balance
and reset credits. It marks a `reset` when a window's use falls or its reset
time advances, and a `plan.change` when the plan label changes between two
known values. `tier` is null until a provider response carries one. Records
hold no credentials, provider payloads or prompt text. Nothing is backfilled,
and no provider call is added: readings are what the existing refresh already
fetches, so resolution is the refresh cadence.

Appends run under `quota.lock`, so lines never interleave. The file rotates to
`.jsonl.1` at half `domains.quota.history_max_bytes` (4 MiB by default, so the
pair stays under the cap) and the newest record always lands in the current
file. Readers skip torn lines.

`provenant quota history [--account LABEL] [--since T] [--until T] [--limit N]
[--json]` lists readings, oldest first, newest `--limit` kept. `--account`
takes a label or key. Times are seconds ago, a date or an RFC3339 timestamp.
`--json` returns `schema_version`, `count`, `matched`, `truncated` and
`readings`; each reading adds `plan_price`, the account's price in force at
that reading, or null.

Set prices in `config.toml` as an array of tables per account, one entry per
price change, so a rise or a plan switch keeps the old figures:

```toml
[[domains.accounts.entries.claude-work.plans]]
name = "max_5x"
price = 100.0
currency = "USD"
effective_from = "2026-01-01"
```

Prices are USD unless `currency` says otherwise, and nothing converts currencies. A free plan has
`price = 0` and may carry `notional_usd`, the list price it replaces; history
shows both. An account may also list extra `directories` signed into it; native
ingest attributes them to the same label, so one account signed in to two config
directories counts once. The keys are listed in
[config keys](../reference/config-keys.md). Mileage reads these prices; the
value multiple is not built yet.

## Routing

`provenant route list` shows the catalogue and pools. Select a route or exact
model on dispatch and retain the resolved route from the attempt record.
