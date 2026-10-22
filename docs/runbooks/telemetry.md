# Telemetry and provider quota

The ledger implements the first ingestion and reporting slice of
[R4](../specs/harness/operating-model.md#r4-local-telemetry-ledger). It stores
usage and routing metadata locally, with no message text, prompts, tool
output or file contents. Ingestion reads provider stores without modifying
them. Ledger ingestion does not contact providers or fetch prices. The separate quota
command contacts provider quota services on demand.

1. Run `provenant telemetry ingest` to refresh the ledger. Repeated runs are
   incremental and idempotent. A missing source is `absent`; a parsing or
   discovery failure is `error` and makes ingest exit 1. Other sources still
   ingest. Error output excludes source contents.
2. Run `provenant telemetry sources` (or add `--json`) to inspect each source's
   path pattern, parser version, last ingest attempt, stored row count and
   status. Row counts refer to usage rows, except route health and cooldowns,
   where they refer to observations. Codex quota observations are reported
   separately by stats.
3. Run `provenant stats --since 2026-10-21` (or add `--json` or `--detail`) for provider,
   adapter and model token totals, cache-read share, estimated cost, latest
   lane status, attempt outcomes, daily provider/account usage, daily limit
   events and observed Codex quota windows. The default output is the summary
   tables only; per-account rows (last limit events, quota observations) and
   uncapped tables are behind `--detail`, and `--json` prints the full report.
   `--since` accepts an ISO date or
   timestamp, inclusive; dates and daily buckets use UTC. Undated records are
   included only when no date filter is supplied. Stats reads the last ingest;
   it does not refresh sources.
4. Run `provenant telemetry changes --sync` to record merged PRs as change
   markers, then `provenant stats --compare <PR#|rev|date>` for before/after
   metrics or `provenant retro` for a digest. See
   [Events, change markers and comparison](#events-change-markers-and-comparison).

## Storage and inputs

The versioned SQLite database is
`$AGENT_FABRIC_STATE_ROOT/telemetry.sqlite3`, defaulting to
`~/.local/state/agent-harness/fabric/telemetry.sqlite3`. Schema version 4
migrates versions 1–3 metadata; unknown newer schemas are refused. The
`events`, `changes`, `quota_series` and `harness_stamps` tables are additive
and keep version 4, so peers on this version still read the ledger.

| Source | Discovery and cursor |
|---|---|
| Fabric | Current workspace plus projects registered in the Fabric `agents` table; shared Git run roots; receipt size and mtime |
| Route health | `AGENT_FABRIC_ROUTE_HEALTH_PATH`, otherwise the state root's `route-health.json`; size and mtime |
| Cooldowns | `FABRIC_COOLDOWNS_PATH`, otherwise the state root's `cooldowns.json`; size and mtime |
| Claude Code | `<identity>/projects/**/*.jsonl` for every Claude identity directory, including subagents; byte offset, file identity and consumed-prefix digest |
| Codex | `<identity>/sessions/**/rollout-*.jsonl` for every Codex identity directory; byte offset, file identity, consumed-prefix digest and cumulative token counters |
| OpenCode | `<identity>/opencode.db` (default `~/.local/share/opencode`), opened with SQLite `mode=ro`; assistant turns from `message` and, since OpenCode 2.0.30, `session_message`; an inclusive `time_updated` watermark per table and file identity |
| OTLP collector | `$AGENT_FABRIC_STATE_ROOT/otel/*.ndjson` when the optional collector ran; byte offset as for JSONL. Feeds events only |

Identity directories are the provider default (`~/.claude`, `~/.codex`) plus
every `identity_dir` and `aliases` entry under `accounts.<provider>` in the
telemetry config (see [Provider quota](#provider-quota)). Each native usage row's
`account` is the label of the directory its file lives in, so usage joins
quota, cooldowns and receipts on one label. The default directory is `default`
unless an account claims it. Codex rows use that label, never the provider's
`creator_account_id`. A file nested in several identity directories belongs to
the deepest one. A message found in several files, such as a resumed Claude
session, takes the owner of its first file by path, in ingest and relabelling
alike. When the configured directories change, the next ingest relabels this
host's usage rows, Codex observations and `session.start` events together from
their recorded file paths.

Discovery skips any transcript name that is a symlink or resolves outside its
identity directory. Ingest then opens each file with `O_NOFOLLOW` and reads it
only if it is still the regular file, by device and inode, that discovery saw.

The Fabric registry follows its existing `AGENT_FABRIC_STATE_DIRECTORY`
setting, defaulting to the same state directory. This differs from the
`AGENT_FABRIC_STATE_ROOT` setting used by route records and telemetry.

Unchanged JSONL files are skipped. Changed files verify the consumed prefix
before parsing new complete lines; an incomplete final line waits for the next
run. A detected rewrite reconciles the file's previous usage rows. Claude
messages deduplicate by `message.id`, retaining the more complete output when
a later duplicate contains partial usage. Codex cumulative counters become
per-event deltas; repeated totals contribute no new tokens. OpenCode messages
upsert by ID, so completion updates do not add another message. A turn that
OpenCode's 2.0.30 migration copied into `session_message` is read only from
there, so it keeps one row. A replaced
OpenCode database resets its cursor and reconciles its previous rows.

An older ledger rebuilds file ownership from its source paths on the next
ingest. Unassociated historical rows remain until every previous path is
available and parses successfully; `pending_source_rebuilds` in stats names
incomplete rebuilds.

## Interpretation and cost

`usage.model` keeps the folded model (`COALESCE(observed, resolved, requested)`).
`requested_model`, `resolved_model` and `observed_model` preserve the receipt's
raw provider strings separately. Native Claude and Codex usage records set
only `observed_model`. Existing ledgers re-read surviving Fabric receipts once;
pruned receipts keep NULL components and their existing folded model.

Token buckets are disjoint: input excludes known cache reads and writes;
output excludes known reasoning tokens. Missing categories remain unknown.
For legacy receipts that omit categories, residual input/output can still
include those unreported categories; `missing_token_fields` identifies the
incomplete breakdown. Aggregates sum known values and mark fields null when
all values are unknown. Cache-read share uses read / (input + read + write)
over rows with complete input categories. `cache_share_rows` reports that coverage;
rows with unknown categories do not erase the known share.

For an adapter/session with native usage in the requested period, stats uses
that usage instead of the Fabric receipt's aggregate. Receipt statuses remain
visible. `receipt_usage_rows_excluded` reports this choice. Limit outcomes in
route health and cooldowns are deduplicated against matching receipt outcomes,
then against each other by route, run, status and day. These snapshots retain
only the events still present at ingest time; the ledger cannot recover older
observations that vanished before its first ingest.

Nested Fabric attempts retain their `parent` identity in the ledger's
`parent_run_id`, `parent_task_id` and `parent_attempt` columns. Lane events also
carry parent and child identities, without copying child error text. Local
`provenant stats --json` includes `usage_by_parent`, grouped by the exact parent
attempt. This view uses child receipt usage, including retries, and is separate
from the native-session deduplication used for overall usage totals.

[telemetry-prices.json](../../config/telemetry-prices.json) owns USD rates per
million input, cached-input, cache-write and output tokens. Its dated entries cite the local `claude-api` skill references for Anthropic and
official provider pages for other prices. Token Meter's reviewed catalogue is a
secondary cross-check. Unverified model prices remain null. Free and subscription
routes are marked explicitly. Prices are static, standard API-equivalent rates;
context-dependent, peak-hour and cache-TTL adjustments are not inferred. OpenCode
route keys retain the upstream namespace, for example
`opencode/opencode-go/<model>`.

Estimated cost charges each bucket once, using the output rate for reasoning.
It is labelled `api_equivalent_estimate`, including subscription routes, and
remains null if a required price or token category is unknown. An incomplete
aggregate's estimated cost stays null; `missing_cost_rows` gives its coverage.
OpenCode's reported cost is retained separately from the estimate.

Stats normalises model names against the routing catalogue and keeps raw names
in `model_raw`. Test and fixture rows are excluded; unattributed rows are listed
separately. Outcomes group by model, effort, task class and route alias, with
success rate, retries, error classes, wall-time percentiles and output tokens per
provider second where recorded. The ingested sources do not currently supply a
first-token timestamp, so time to first token is unreported. `last_limit_events` reports the latest
recorded limit and its age per provider, account and host; accounts missing in a
source remain unattributed. Fabric `ok` records
execution success, not user acceptance or a passed delivery gate.

`provenant telemetry accepted [--since ISO-DATE] [--json]` reports deliveries
by risk tier and one harness digest per run (most linked usage rows, ties by
name). `--since` filters on the run's earliest recorded timestamp, falling back
to its run-directory stamp, never ingest time. A run is accepted when its
`RUN.json` has `delivery_acceptance` with basis `checks-and-review`,
`project-landing` or `user`. A run is merged only when the commit on its
`software_delivery.canonical_artifact_id` artifact matches a
`changes.merge_commit` row populated by `telemetry changes --sync`; this only
matches PRs in the product repository because sync covers that checkout only.
Lanes link by realpath workspace equality or containment under an allowed
source path (excluding `.`), with attempt starts bounded by the delivery's
first and last recorded lifecycle timestamps. Cost includes every linked
attempt and reviewer lane, including failed attempts and unaccepted runs.
Runs without acceptance stay in the numerator and run count, and never enter
the accepted denominator. Unpriced rows are reported; known costs and cost per
accepted run are lower bounds. No linked lanes yields `usage_rows: 0` and null
cost. The ledger stores receipt facts in `deliveries`; `accepted` and `merged`
are report labels.


## Events, change markers and comparison

Ingest appends metadata events to the `events` table: `(event_id, host, kind,
ts, session, run_id, lane, task_id, pr, provider, account, model, harness_rev,
attrs, source_ref)`. `kind` is a free string, so a new kind needs no migration;
`EVENT_KINDS` in `scripts/telemetry.py` names the known ones. `source_ref`
points back to the receipt, snapshot, transcript or collector line
(`path@offset`). An event's ID derives from fields stored in its own row,
never the host name: source and session for `session.start`; the receipt's
resolved path for `lane.attempt`, so projects sharing run ids stay apart;
provider, account and normalised plan pair for `plan.mismatch`; otherwise `source_ref`, time, linked fields and attributes. Collector refs are
`path@offset#record`, so identical records on one line keep their count.
Re-ingest therefore adds no duplicate rows, even after a `local_host` rename,
which leaves earlier rows under their old id for readers to map. Opening a
ledger from an earlier build re-keys its events once under a write lock,
keeping the earliest of any duplicates, so concurrent openers cannot race.

| Kind | From |
|---|---|
| `lane.attempt` | A terminal Fabric receipt: adapter, effort, attempt, status, error class, task class, route alias, wall time, retry/escalation lineage, `empty_patch`, and profile stamp metadata when known (absent profile means balanced) |
| `limit.rate_limited`, `limit.usage_limited`, `cooldown` | Route-health and cooldown snapshots |
| `session.start` | The first ingested usage row of a Claude, Codex or OpenCode session; one event per session, not per message |
| `plan.mismatch` | Configured `plan` and `plan_detected` disagree; one warning per provider, account and normalised pair, with both values in attrs |
| `plan.change` | A quota reading whose `plan`, `plan_detected` or `plan_tier` differs from the account's previous reading; attrs hold each `<field>_from` and `<field>_to`. Spelling-only differences and unknown values are not changes |
| `otel.<event.name>`, `otel.metric` | Collector log records and metric data points |

Re-reading a lane receipt merges its attrs into the existing event, preserving
other history. `empty_patch` comes from the writer's Git snapshot: true only
when a clean starting worktree keeps the same HEAD and porcelain after the
provider returns. Read-only lanes omit it; a dirty start or unreadable Git
state stays unknown.
Success with no diff selects successful lane events with
`json_extract(attrs,'$.empty_patch')=1`; missing values do not count as false.

`retry_of` names the predecessor attempt. A task may also set
`escalation_reason` to `gate_failed`, `review_blocked`, `capability` or
`quota_or_limit`, requiring `retry_of`. Only these escalations get
`escalated_from` in `<run_id>/<task_id>/<n>` form; a plain retry stays a retry.
Automatic provider fallback retains its separate `fallback_from` provenance.

Lane attrs retain the profile's preset, digest, `risk_tier` and `floored`.
`harness_stamps` stores full resolved knobs once per digest, with preset and
first-seen time. A digest without a knobs object creates no stamp row.

Events never hold prompt, output or error text. OTLP attributes keep numeric
values plus `model`, `effort`, `type`, `tool_name`, `decision`, `source`,
`success` and `status_code`; every other string, including identity and email
attributes, is dropped. Resource attributes `provenant.run_id`,
`provenant.lane` and `provenant.harness_rev` link a record to its lane when the
exporter sets them.

`harness_rev` is the installed Provenant revision. Fabric lanes stamp it into
attempt provenance (`provenance.harness_rev`); older receipts, sessions and
unlinked OTLP records leave it null. An existing ledger re-reads its receipts
and snapshots once to backfill events; transcripts are not re-read, because
session events derive from usage rows already stored.

The future OTel exporter mapping is pinned here while `gen_ai.*` remains a
Development convention. Keep it in one constant in the exporter module when
that module exists; the map is documentation only until then.

| Ledger field | OTel export attribute |
|---|---|
| `requested_model` | `gen_ai.request.model` |
| `resolved_model`, `observed_model` | `gen_ai.response.model` (observed preferred); resolved also as `provenant.model.resolved` |
| `provider` | `gen_ai.provider.name` |
| `session` | `gen_ai.conversation.id` |
| input and output tokens | `gen_ai.usage.input_tokens`, `gen_ai.usage.output_tokens` |
| `review.finding` disposition | `gen_ai.evaluation.result` event (score label = disposition); otherwise `provenant.*` |
| `empty_patch` | `provenant.empty_patch` |
| `escalation_reason`, `escalated_from` | `provenant.escalation.reason`, `provenant.escalation.from` |
| `repair_cycle` | `provenant.repair.cycle` |
| compaction count | `provenant.session.compactions` |
| profile preset and digest | `provenant.profile.preset`, `provenant.profile.digest` |
| `harness_rev` | `provenant.harness_rev` (also used on inbound records) |

```sh
provenant telemetry accepted [--since ISO-DATE] [--json]
provenant telemetry changes --sync           # merged PRs via gh, incremental
provenant telemetry changes --add "switched default reviewer" --at 2026-10-20
provenant telemetry changes [--since DATE] [--json]
provenant stats --compare 1050 [--window 7d] [--json]
provenant retro [--since DATE] [--window 7d] [--json]
```

`changes --sync` runs `gh pr list --state merged` in the product checkout over
a merge-time range: the first sweep from 2008, later sweeps from a day before
the last completed sweep. Search results are not ordered by merge time, so a
page that reaches the 200-row limit splits its range at the median merge time
and reads both halves, down to single seconds. A full single second is re-read up to GitHub
search's 1,000-result cap; if that is full too, the range stays listed as
incomplete and every `--sync` warns about it. Pending ranges persist in the ledger, so a sweep capped
at 25 queries, or stopped by a `gh` failure, resumes on the next `--sync`. It
stores PR number, merge commit, merge time, title and labels; a re-sync
updates a renamed PR in place. Manual markers record a label and time.

`--compare` pivots on a PR's merge time, a merge commit or other revision's
commit time, or an ISO date, and compares the window before with the window
after (default 7 days each; the after side stops at now). Metrics come from
the same row selection as stats: lane success rate, retries per lane, p50
wall time, cache-read share, known API-equivalent cost per day and limit
events per day. Each metric's n counts the observations that produce it:
lanes for success and retries, attempts with a start time for latency, rows
with every input field for cache share, priced rows for cost and usage rows
for limits. A metric with n below 10 on either side (`MIN_SAMPLE` in
`scripts/telemetry_changes.py`) is `inconclusive`, never a win. Cost is also
`inconclusive` whenever either side has unpriced rows, because those would
read as free; `cost_coverage` gives the priced share. Otherwise success rate is `within_noise` when the
difference is within two pooled standard errors, and other metrics when it is
within 10%; the rest are `improved` or `regressed`.

`provenant retro` defaults to the last 7 days: overall lanes and cost, each
change marker in the period with its compare summary, top error classes by
attempt, and attention proxies already in the ledger (sessions started, lanes
awaiting input, and OTLP user prompts, tool decisions and active time when the
collector ran). Text output of `changes`, `--compare` and `retro` stays under
4 KB of UTF-8 and says how many lines it omitted; `--json` is the full report.

## Provider quota

```sh
provenant quota                 # fetches only the accounts that are due
provenant quota --json
provenant quota --refresh       # explicitly bypass the schedule
provenant quota tick --json     # fetch what is due, write report.json, print it
provenant quota schedule        # each account's next due time and reason; never fetches
provenant quota trace --since 24h
provenant quota path [--json]   # the quota state directory
provenant quota forget agy/google-<hash>   # drop a signed-out account's remembered reading
provenant routes --json         # cached quota only; never fetches
```

Codex prefers the credential-free `codex app-server` JSON-RPC rate-limit read,
then falls back to the latest local rollout observations already ingested for
the same account. Claude reads its own OAuth usage endpoint using the
identity's credentials in process. On macOS it falls back to the identity's
Keychain item, which Claude Code names `Claude Code-credentials` for `~/.claude`
and adds an 8-character path hash for any other config directory. The first
read of each item may ask you to allow access. Cursor reads its usage-summary
endpoint using its local session.
Authenticated HTTP requests refuse redirects, so credentials stay at the
provider endpoint. Tokens never enter the ledger, cache, logs, receipts or
stdout and are never shown to agents. The owner authorised this credential use
for this feature on 2026-10-21. Development and tests use fixtures only.

agy answers `agy -p /usage --output-format json`, which reads its quota summary
without an agent turn or quota spend; the CLI keeps its credential. Each model
pool reports a weekly and a five-hour bucket, shown as `gemini.weekly`,
`claude.5h` and so on; a bucket agy marks disabled is unreported. One Keychain
item serves every Google account agy signs in to, so a discovered agy account
is labelled `google-<hash>`: a hash of the signed-in address from agy's newest
log under `~/.gemini/antigravity-cli/log`. A reading whose own run reports
another address is refused rather than filed under the wrong account. agy quota
is report-only: its pools differ per model family, so it sets no provider-wide
cooldown (#1062). Each window carries its `pool`, such as `gemini` or `claude`;
see Display below.

Switching agy to another Google account keeps the previous one as
`status: signed_out`, with its last reading, observed time and resets. A
signed-out account is never fetched, has no local activity and never enters
routing placement. Signing in again resumes the same label and history. The
label is a hash of the address, so it is the same on every host. `provenant
quota forget <provider>/<label>` deletes a signed-out account's remembered
snapshot; it refuses an account that is signed in. `quota.remembered.forget_after_days`
(default unset) stops listing a remembered account unseen that long, without
deleting it. Listing configured agy fingerprints in `accounts.agy` names them
and still shows the signed-in and remembered ones not listed. An alias such
as `accounts.agy.work` binds to the account it first reads, decided under the
refresh lock, and keeps that account's history when another signs in: a
reading for another account, or one that does not say whose it is, fails as
`auth` and leaves the last reading `stale`. To rebind an alias deliberately,
set its `fingerprint` to the new account's label, such as
`"work": {"fingerprint": "google-4e7d2a91c05b"}`. The mechanism is
generic: a provider module with `fingerprinted(label)` gets it. Codex and
Cursor are labelled `default` and do not use it yet: Codex's rate-limit read
carries no account identity, so it would need a further `account/read` call,
and Cursor's user id is only in the session token it reads at fetch time.

`PROVENANT_QUOTA=0` or `quota.enabled=false` in the telemetry config disables
fetching and quota routing input. Cached quota can still be displayed. Errors
retain the last successful observation with `status: stale`; failed attempts
back off by error kind, as below. The cache lives beside the ledger under
`quota/<provider>--<account>--<host>.json`.

### Refresh schedule

Each account is due on its own schedule. It is fetched sooner while its identity
directory shows recent use (`active`), less often when idle, sooner still when a
window nears exhaustion or will run out before its reset, and not at all while
a window is exhausted until just after its reset. Every report row carries a
`schedule` block with `next_due_at`, `reason`, `ttl_seconds`, `trigger`,
`failures`, `backoff_until`, `error_kind`, `last_duration_ms` and
`source_kind`; the report adds the earliest `next_due_at` and `refreshing`, the
`provider/account` ids being fetched now. Activity is read from file times only.

`provenant quota tick` takes `--trigger timer|panel|manual|event|wake|power` and
`--power ac|battery|low`, supplied by the caller. It holds
`quota/refresh.lock` without waiting: a second tick prints the current report
and fetches nothing. Up to `max_parallel` accounts fetch at once; agy runs alone.
`quota/report.json` is rewritten atomically after each fetch. When nothing is
due, tick fetches nothing and rewrites the report only if `next_due_at` moved.
A `manual` trigger fetches an account at least 30 seconds old and overrides a
backoff other than `rate_limited`; `--ignore-backoff` overrides any backoff.
A `power` trigger never fetches.

`quota.schedule.enabled=false` restores the fixed period: an account is fetched
when its last attempt is older than `quota.cache_seconds` (at least 300). With
the schedule on, `cache_seconds` is unused.

| `quota.schedule` key | Default | Meaning |
|---|---|---|
| `enabled` | `true` | Use the adaptive schedule |
| `min_interval_seconds` | 60 | Shortest wait between fetches of one account |
| `max_interval_seconds` | 21600 | Longest wait |
| `active_window_seconds` | 900 | Identity use this recent makes an account active |
| `jitter_fraction` | 0.1 | Spread of each wait, stable per account |
| `coalesce_seconds` | 120 | Fetch an account due this soon with the others (at most a quarter of its TTL) |
| `near_exhaustion_percent` | 85 | Use at which the wait shortens |
| `near_exhaustion_factor` | 0.5 | Multiplier then, and when run-out falls within the horizon |
| `runs_out_horizon_factor` | 2 | Horizon as a multiple of the TTL |
| `reset_grace_seconds` | 60 | Wait after a reset before an exhausted account is checked |
| `exhausted_recheck_seconds` | 21600 | Longest wait while exhausted |
| `battery_factor` | 2 | Wait multiplier on battery |
| `low_power_factor` | 4 | Wait multiplier in low power |
| `low_power_skip_costs` | `["expensive"]` | Costs that wait the maximum in low power |
| `manual_min_age_seconds` | 30 | Youngest reading a manual trigger refetches |
| `event_debounce_seconds` | 30 | Youngest reading an event trigger refetches |
| `max_parallel` | 3 | Concurrent fetches |

Per provider, `quota.providers.<name>` or an account's own `quota` object sets
`ttl_active_seconds`, `ttl_idle_seconds`, `ttl_panel_seconds` (the youngest
reading a panel open refetches), `cost` (`cheap`, `moderate` or `expensive`)
and `solo`, and may override any key above. The defaults:

| Provider | Cost | Active | Idle | Panel |
|---|---|---|---|---|
| claude | moderate | 300 | 1800 | 120 |
| codex | moderate | 900 | 3600 | 300 |
| cursor | cheap | 1800 | 3600 | 600 |
| opencode | cheap | 300 | 3600 | 60 |
| agy | expensive, solo | 1200 | 7200 | 900 |

An invalid value falls back to the next layer, and the account's `schedule`
block lists it under `notes`.

A failed fetch sets `error_kind` and waits before the next attempt:

| `quota.backoff` key | Default | Applies to |
|---|---|---|
| `base_seconds` | 120 | `rate_limited`, `network` and `other`: doubles per consecutive failure, with jitter |
| `max_seconds` | 3600 | Cap on that doubling |
| `auth_seconds` | 3600 | `auth`; cleared early by identity use, such as signing in again |
| `missing_seconds` | 86400 | `missing`, such as a CLI that is not installed |

Each wait runs from when the attempt finished. A `Retry-After` header lengthens
a `rate_limited` wait, up to a day; a date in it is kept as that deadline. Trace
and state files are created 0600 and never written through a symlink. If every
fetch in a tick fails to connect, the report sets `offline: true`.

Each attempt appends one line to `quota/fetches.jsonl`: `at`, `provider`,
`account`, `trigger`, `outcome`, `error_kind`, `duration_ms`, `retry_after_s`
and a scrubbed `error`. `provenant quota trace --since 24h [--json]` totals
fetches, failures by kind and duration percentiles per account. Lines older than
`quota.trace.retention_days` (default 30) are pruned at most once a day.

Claude's `claude auth status` result is cached per identity under
`quota/memo/` for a day, or until the identity is used. OpenCode reads its v1
message table through its session and time index, one time range per session,
filtered by message time.

Configure accounts in `<instance-root>/config/telemetry.json`. These are the
`telemetry.accounts.<provider>.<label>` settings; omit the outer `telemetry`
object in this dedicated file. Labels are short non-secret names, never email
addresses or tokens. Plan labels are optional display text; priority is an
optional integer, with smaller numbers preferred.
An entry without `identity_dir` means the provider's default directory.
`aliases` lists other directories signed in to the same account, such as the
CLI default when it holds the same login as a named directory. They affect
only ledger attribution, never quota fetching or routing.

```json
{
  "quota": {"enabled": true, "schedule": {"max_parallel": 3},
            "providers": {"agy": {"ttl_idle_seconds": 14400}},
            "window_labels": {"iguana_necktie": "Cloud session credits"},
            "credits_policy": "last_resort", "credit_floor": 0},
  "accounts": {
    "claude": {
      "max-a": {"identity_dir": "~/.claude-a", "aliases": ["~/.claude"], "plan": "Max", "priority": 1},
      "max-b": {"identity_dir": "~/.claude-b", "plan": "Max", "priority": 2}
    },
    "agy": {
      "work": {"plan": "work"}
    },
    "codex": {"work": {"identity_dir": "~/.codex"}}
  }
}
```

The agy example is this Mac's account. Its sign-in lives in the shared macOS
login Keychain: another `identity_dir`, `AGY_CONFIG_DIR` or HOME on this Mac does
not isolate a second account. Configure the personal agy account on the workshop
Mac, for example `"agy": {"personal": {}}` in that host's telemetry config.
Give the Macs distinct `local_host` ids in `.agent-fabric/hosts.json`; host scope
keeps their cooldowns apart. No agy HOME switching is performed.

A provider absent from config gets one discovered default account when its
identity directory exists. An explicitly empty provider object disables that
auto-discovery. Disabled or removed accounts disappear from the report and
routing input; their history remains. There are no built-in plan quotas or
window schedules. A missing window is unreported, never 0%.

Each window shows provider-reported use, length and reset time. Pace compares
actual use with even use since the reported window start, projects run-out and
states whether use lasts until reset. Pace is recomputed at report time from
the latest observation. `use_it_or_lose_it` reports the current unused fraction and projected headroom
expiring at the longest reported reset. Each window also reports headroom.
Usage of 100% or more is exhausted even when timing is missing or inconsistent.
Only zero use can mean a window has not started. Positive use with a missing or
passed reset has unknown pace pending a fresh reading; it retains its reported
headroom. A reset up to five minutes beyond the window length is clamped for
pace calculations; larger discrepancies have unknown pace.

Some providers report budget windows in money rather than a percentage. Claude
names them by codename, such as `iguana_necktie` with `limit_dollars` and
`used_dollars`. Such a window keeps its percentage for pace and adds
`used_amount`, `limit_amount` and `currency`; the table shows `$136 of $250`.
`quota.window_labels` maps a window name to a display label and adds to or
overrides the built-in map, which labels `iguana_necktie` as
`Cloud session credits`. Every report window carries `label`; see Display.

An account can also report `usage_credits`, a list of credit balances drawn on
once the plan allowance runs out, and `resets`, the banked resets available with
their known expiries. Each credit has `kind`, `balance` (null when `unlimited`),
`currency` or `unit`, `expires_at` and `source`. `resets` has `available`, a
sorted `expires_at` list that may be shorter than `available`, and `source`.
Both are omitted when the provider reports none, zero or nothing; a missing
balance is unknown, not zero. The table adds one line per account that has
either, for example `codex/default: 27,406 credits available; 1 banked reset
(expires 7 Nov)`. Codex reports both through `account/rateLimits/read`. Claude
reports a `spend.balance`, shown when positive, and no banked resets. A reset
recorded with `provenant quota credit` appears with `source: manual` when the
provider reports no count.

Successive observations record `natural`, `granted_date_kept`,
`granted_window_restarted`, `window_moved`, `not_started` or `window_started`
events in `quota_events`, per provider, account, host and window. These describe
observed changes, not a presumed billing calendar. Natural rollover compares
the new provider window start against the previous provider reset. Restarted
windows must start within five minutes of the observation.

### Display

The report owns the display vocabulary, so the table and the menu bar agree.
All of it is additive to `provenant.quota.report.v1`.

- Each account has `provider_name` ("Claude", "Codex", "OpenCode",
  "Antigravity"; an unknown provider is title-cased), `display_name` and
  `plan_label` ("Pro Lite", "Pro (Student)").
- `display_name` comes from an optional `display_name` key on
  `accounts.<provider>.<label>` in `telemetry.json`. Without one, a label is
  title-cased (`anese` reads "Anese"), `default` has none, and a fingerprint
  label such as `google-<hash>` has none, so the provider name stands alone.
  Labels themselves never change.
- Each window's `label` comes from `quota.window_labels`, else from its length:
  `5-hour`, `Daily`, `Weekly`, `Monthly` (28 to 31 days, so a billing cycle
  reads Monthly), or `3-day` for another length, with a qualifier when the
  name adds one, such as `Weekly (Opus)`. An untimed window is its name in
  sentence case.
- A `limits` entry that restates a named window, with the same pool, length,
  use and reset to the minute, such as Claude's `weekly_all:2` beside
  `seven_day`, is dropped; the named one stays. An entry that differs in any
  of those stays, and two named windows are never merged.
- A pooled window has `pool` and `pool_label` ("Claude models"). Pools are
  independent: the account is usable while any pool is, the most used usable
  pool binds, and when every pool is exhausted the account is `back_at` the
  first pool's return.
- `display_window` names the window to show: an exhausted one; else one at or
  above `quota.display.short_window_alert_percent` (default 90, echoed in the
  report's `display`); else the longest. A window whose reset has passed
  carries `reset_passed: true` and never shows as current use; the table shows
  `-` and "reset since".
- `headline` names the account the menu bar shows, set by
  `quota.menubar.headline`: `recent` (default), the usable account with the
  latest local activity, or the highest priority without any; `worst`, the
  highest binding use; `<provider>/<account>` to pin one; or `none`. Its
  `account` is null when nothing usable remains.

```json
{"quota": {"display": {"short_window_alert_percent": 90},
           "menubar": {"headline": "recent"},
           "remembered": {"forget_after_days": 30}},
 "accounts": {"claude": {"anese": {"identity_dir": "~/.claude"}},
              "agy": {"google-<hash>": {"display_name": "Michael"}}}}
```

### Quota history and plan prices

Every fetch (not a cached or failed reading) also lands in `quota_series`, one
row per provider, account, host and window: `observed_at`, `last_seen_at`,
`source`, `used_percent`, `window_minutes`, `reset_at`, `plan` (configured label,
else detected), `plan_detected` (Claude `subscriptionType`, Codex `planType`,
Cursor `membershipType`), `plan_tier` (Claude `rateLimitTier`, which separates
Max 5x from 20x), `credit_balance` (Codex usage credits drawn after the plan
allowance) and `reset_credits`. A reading identical to the window's previous
row only advances that row's `last_seen_at`, so the menu bar's two-minute polling
adds a row only when something changed. A changed plan field emits
`plan.change`. A configured plan that disagrees with the detected plan emits
`plan.mismatch` once per account and normalised pair. No new provider call is
made for this history.

Plan prices are user configuration, read at report time and never fetched.
List them under `plans.<provider>.<plan>` in the same file. `<plan>` matches a
reading's `plan_tier`, then `plan_detected`, then `plan`, ignoring case and
punctuation; `*` covers any reading of that provider with no other match. Each
entry is a list of monthly prices with the date each took effect; the latest
`effective` date at or before a reading applies. `list_usd` is the advertised
price and `paid_usd` what you pay, so a free student plan has a list price and
`paid_usd: 0`. Value for money divides by `paid_usd`; upgrade comparisons can
use `list_usd`. A price applies per account, so two accounts on one plan count
twice:

```json
{
  "plans": {
    "claude": {
      "default_claude_max_20x": [{"list_usd": 200, "paid_usd": 200, "effective": "2026-01-01"}],
      "pro": [{"list_usd": 20, "paid_usd": 20, "effective": "2026-01-01"}]
    },
    "cursor": {"pro_student": [{"list_usd": 20, "paid_usd": 0, "effective": "2026-01-01"}]},
    "opencode": {"*": [{"list_usd": 10, "paid_usd": 10, "effective": "2026-09-01"}]}
  }
}
```

The values above illustrate the shape only. Enter the amounts you actually pay.
Mileage (API-equivalent USD per 1% of a window), upgrade gain and value per plan
read these tables; see #1071.

```sh
provenant quota credit codex --account work add
provenant quota credit list --json
provenant quota credit codex --account work use
```

This example banks one Codex reset credit. `use` records a credit the user has
already consumed; it never redeems anything with a provider. Credit records are
local advice, keyed by provider, account and host. Existing two-part
provider/account keys remain host-agnostic when updated; an exact host key takes
precedence when present. Agents never redeem credits.
Advice is to hold until near exhaustion, wait if the natural reset is within
six hours, and prefer a reset that keeps the date when that date is days away.

Only the currently selected CLI identity affects routing (`CLAUDE_CONFIG_DIR`,
`CODEX_HOME`, or the provider's default directory). Changing that identity is
reflected on the next routing read, including when snapshots are cached. Cooldowns use the same account key and host as quota. See
[model routing](model-routing.md) for identity sources and scoped reset. Cache
and lock paths include the resolved identity directory; older unbound caches
require a fresh reading before they can affect routing. The report orders other
configured accounts by priority and reset proximity as
placement advice; it never switches credentials. An exhausted selected account
blocks that adapter until the latest exhausted window resets. A newer provider
reading with a reset date updates that account and host's adapter cooldown. Exhaustion without a
reset date preserves existing usage cooldowns. Reserve, on-pace, early and
not-started readings clear older contradictory usage cooldowns in the same
account and host; unknown readings
preserve them. Short-term `rate_limited` cooldowns remain.
Deficit is shown in `provenant routes` and JSON only; it does not lower weights.

An account whose allowance is spent can stay usable on credits. The allowance is
spent when a window is exhausted or the provider reports `ordinary_usage_allowed:
false`. Credits keep the account usable when the latest reading has a balance
above `quota.credit_floor` (default 0), or unlimited credits, and the provider
reports no spend control reached and no depleted credits. Such an account has
state `credit_only`, `drawing_credits: true`, a summary with its balance, and
`credit_burn_per_hour` once successive readings show the balance falling. It adds
no quota cooldown and clears older cooldowns that quota set. A cooldown from a
real refusal stays until it expires or a reading shows allowance again, since
credits may be switched off. A spent allowance without usable credits is
`exhausted`, even below 100%, and cools until the governing window resets. With
no future reset time known, it cools for `quota.unknown_reset_cooldown_seconds`
(default 3600) from the reading, and the next reading decides.
`quota.credits_policy` sets how routing ranks it:

- `last_resort` (default): placement ranks it after every account with
  allowance, and pools skip its adapter while another entry with allowance
  qualifies. A request that names the adapter or the model still uses it.
- `normal`: it ranks like an account with allowance.
- `never`: it stays exhausted and cools until reset.

A balance that the latest reading does not report is unknown, never usable.

### Adding a provider

1. Add one small module under `scripts/quota_providers/`, then one entry in
   `provider_quota.REGISTRY`. No provider-name branches belong in the core.
2. Implement `ADAPTER`, `default_identity()` and `fetch(account, io, now)`.
   Fetch returns `windows`, optionally `plan`, `source`, `unreported`, `credits`,
   `usage_credits`, `resets` and `observed_at`. Provider-specific account config keys are passed through
   in `account.options`; `identity_dir`, `enabled`, `plan` and `priority` are
   handled by the core. Options stay out of snapshots and reports. Each window
   has `name`, `used_percent`, `window_minutes`, `reset_at` and `raw_kind`; names
   are free strings. Report unknown lengths or
   resets as null. Core code normalises, caches and computes pace. When one
   credential store serves several accounts, `account_label()` may name the
   signed-in one from local, non-secret state; it must never fetch.
3. Use the bounded `io` HTTP/process functions and fixed, sanitised failures.
   Return only quota metadata. Keep tokens inside the provider call chain.
4. Register a fake module and mock all process/network boundaries in tests.
   Add the account to config and verify it appears in `provenant quota`.

OpenCode has no usage or limits endpoint and documents no reset semantics.
Go limits are plan-wide dollar amounts (5 hour 20%, weekly 50%, monthly 100% of
the monthly amount). The opencode provider is a local estimate: it sums paid
`opencode-go` cost from both `opencode.db` message stores (v1 `message`, v2
`session_message`; read-only, rolling windows by message timestamp) against a
monthly cap calibrated from the Go dashboard, not documented: $32 for Go, which is
assumed when no `plan` is set (the source says so). Set `limits.monthly_usd` to
override it or to cover another plan, which is otherwise unreported. The estimate
is partial: other hosts using the same account are not counted, so it
undercounts. A recent `GoUsageLimitError` in the store is authoritative: that
window reads 100% until its estimated reset (the error time plus the window).
Set `limits.cycle_anchor` (ISO datetime of the subscription cycle start) to use fixed
weekly (every 7 days) and monthly (same day each month, clamped to short months)
cycles instead of rolling ones: spend is summed within the current cycle, resets
are the next boundary, and an error expires at it. Set `limits.weekly_anchor` when
the weekly cycle starts at a different time from the monthly one; it defaults to
`cycle_anchor`. The 5 hour window always rolls.
Free zen models are ignored. Only message tables are read, never credentials.

```json
"opencode": {"go": {"plan": "Go", "limits": {"monthly_usd": 32}}}
```

agy usage remains receipt-only; its quota comes from `/usage` above. A
configured agy label names whichever account is signed in, so leave agy
unconfigured to keep its accounts apart. The inspected Cursor reference uses
its SQLite store for authentication, then obtains allowance from the endpoint;
it does not supply a local token-usage ledger. Cursor usage tokens therefore
remain receipt-only. Neither missing coverage is represented as zero use.

## Multiple hosts

```sh
provenant stats --hosts all --since 2026-10-21
provenant stats --hosts studio peer --json
```

Host IDs come from the existing `hosts.json` `local_host`, defaulting to the
hostname's slug as for fabric (see [fabric hosts](fabric-hosts.md)). With no selectors, stats remains local. With no peers,
`--hosts all` also remains local. SSH uses the existing peer transport and its
configured deadlines; no daemon, HTTP server or sync service is required.

The versioned read-only `telemetry.stats` peer operation returns aggregates,
not raw event rows, paths, prompts or credentials. Session aggregates are
identified by `(host, source, session)` and grouped further by day and model;
repeated host selectors do not duplicate them. Usage totals merge across hosts;
route outcomes retain host labels so percentiles are never averaged. Offline,
old, malformed or unknown peers yield typed `partial` host rows, and the command
still returns local results successfully. Narrow `--since` if a peer summary
exceeds the transport's 1 MiB bound. Ingest on a peer before requesting stats;
the peer read never creates or migrates its ledger.

Quota is account-scoped: a provider fetch on either host includes the account's
use on both hosts, so quotas need no usage sync. Peer stats includes cached
quota metadata and selects the freshest observation for each provider/account,
without adding percentages or importing peer state into local routing. Use the
same non-secret account label for the same account on each host.

## Storage check

On 2026-10-22, separate disposable SQLite databases with 10,000 synthetic Codex
usage rows (100 sessions, one transcript ownership path, unique event IDs,
known token buckets), committed and vacuumed, measured:

| Measure | Before W12 (`5afeeb25`, schema 3) | W12 (schema 4) |
| --- | ---: | ---: |
| Whole database bytes | 3,862,528 | 3,932,160 |
| Bytes per usage row, including indexes and ownership | 386.25 | 393.22 |
| Usage record payload bytes per row (`dbstat`) | 138 | 142 |
| `usage_time` index bytes | 286,720 | 286,720 |
| Usage primary-key index bytes | 520,192 | 520,192 |
| Ownership primary-key index bytes | 782,336 | 782,336 |

The four added receipt columns cost four payload bytes on rows where they are
null; empty metadata/event tables also add pages. No usage indexes or rollups
changed. Ownership and indexes account for much of the footprint. Daily rollups
could reduce older detail, but would remove per-session evidence needed for
reconciliation and outcomes. This slice preserves that evidence; it makes no
size-reduction claim. No live database was opened for this measurement.

## Optional OTLP collector

Transcript and receipt ingestion above remains the baseline. When the
collector is absent nothing changes. The collector is an opt-in, on-demand
macOS receiver for OTLP/HTTP JSON that Claude Code and Codex can export to.
The collector stores what it receives; `provenant telemetry ingest` turns its
lines into metadata events (see
[Events, change markers and comparison](#events-change-markers-and-comparison)).

Source: [tools/otlp-collector](../../tools/otlp-collector/otlp-collector.swift),
Swift with no packages. It listens on `127.0.0.1` only, on port 4318 unless
`PROVENANT_OTEL_PORT` names another, through launchd socket activation, so it
costs nothing while idle. It exits after
`PROVENANT_OTEL_IDLE_MINUTES` (default 10) without a connection and launchd
starts it again on the next one. Foreground mode for hosts without launchd and
for tests: `otlp-collector --listen 127.0.0.1:PORT [--idle-seconds N]`.

Enable and disable:

```sh
scripts/install-harness --platform claude --otel-collector   # builds with swiftc, installs
scripts/install-harness --uninstall-otel-collector           # removes agent and binary
```

The installer writes `~/Library/LaunchAgents/com.provenant.otlp-collector.plist`,
installs `provenant-otlp-collector` beside the `provenant` command and loads the
agent with `launchctl bootstrap`. `PROVENANT_OTEL_PORT` (an integer from 1 to
65535, default 4318) sets the listening port in the plist; change it by
reinstalling. `PROVENANT_OTEL_NO_LAUNCHCTL=1` writes the files without touching
launchd. Install exits non-zero, with the reason, on a host other than macOS,
when `swiftc` is missing (install the Xcode command line tools) or when the
port is invalid. Uninstall works on any host and keeps stored data. The installer never
edits `~/.claude/settings.json` or `~/.codex/config.toml`; add the exports
yourself.

Stored data: `POST /v1/{logs,metrics,traces}` with `Content-Type:
application/json` is validated as JSON and appended, untransformed, as one line
`{"received_at","signal","body"}` to
`$AGENT_FABRIC_STATE_ROOT/otel/<signal>-YYYYMMDD.ndjson` (UTC day, mode 0600,
directory 0700; state root as in `scripts/telemetry.py`). Replies are `200 {}`,
`415` for protobuf or compressed bodies, `400` for invalid JSON, `413` above
`PROVENANT_OTEL_MAX_BODY_BYTES` (default 4 MiB) and `411` for chunked bodies.
Files older than `PROVENANT_OTEL_RETENTION_DAYS` (default 30) are pruned at
start and when the day changes.

The examples below use the default port; with `PROVENANT_OTEL_PORT` set, use
that port instead.

Claude Code (environment, for example in a shell profile):

```sh
export CLAUDE_CODE_ENABLE_TELEMETRY=1
export OTEL_METRICS_EXPORTER=otlp
export OTEL_LOGS_EXPORTER=otlp
export OTEL_EXPORTER_OTLP_PROTOCOL=http/json
export OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318
```

Codex (`~/.codex/config.toml`):

```toml
[otel]
log_user_prompt = false
exporter = { otlp-http = { endpoint = "http://127.0.0.1:4318/v1/logs", protocol = "json" } }
trace_exporter = "none"
metrics_exporter = "none"
```

Privacy defaults: leave `OTEL_LOG_USER_PROMPTS` and `OTEL_LOG_TOOL_DETAILS`
unset or `0`, and keep Codex `log_user_prompt = false`. Do not enable either.

Verification status: codex-cli 0.159.0 binary strings confirm the `[otel]` keys
`environment`, `exporter`, `trace_exporter`, `metrics_exporter` and
`log_user_prompt`, the exporter kinds `none`, `statsig`, `otlp-http` and
`otlp-grpc`, and the `endpoint`, `protocol` (`binary` or `json`), `headers` and
`tls` fields. The exact TOML shape above (inline table keyed by exporter kind)
and whether Codex appends signal paths to `endpoint` were not verified against
a running Codex; the Codex endpoint is therefore given as the full `/v1/logs`
URL, and Codex metrics and traces stay off.

Verified on 2026-10-22 (laptop, Claude Code 2.1.306): a throwaway agent under
the real launchd had no process before the first request, started on it (0.56 s
for the first load, 72 ms on a later wake, 15 ms warm), exited when idle and
woke again on the next request, at about 7 MB RSS. The source also compiles with
Swift 6.1 (workshop). A `claude -p` run with the Claude Code exports above wrote
logs (`user_prompt`, `api_request`, `assistant_response`, hook and MCP events)
and metrics (`active_time.total`, `cost.usage`, `session.count`,
`token.usage`). `api_request` carries model, effort, input, output and cache
tokens (5-minute and 1-hour writes separately), cost, duration, time to first
token and request id. Prompt and response text arrived as `<REDACTED>`, with
lengths only. Every record carries account and user identity attributes,
including the account email, so the files stay local and private.

Lane linkage (follow-up, not implemented): the dispatcher should set
`OTEL_RESOURCE_ATTRIBUTES=provenant.run_id=<run>,provenant.lane=<lane>,provenant.account=<account>`
in each lane's environment so exported events join to Fabric runs. It touches
the lane process environment in `scripts/fabric_lanes.py` and the adapters, so
it is left for its own change.
