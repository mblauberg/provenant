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
3. Run `provenant stats --since 2026-10-21` (or add `--json`) for provider,
   adapter and model token totals, cache-read share, estimated cost, latest
   lane status, attempt outcomes, daily provider/account usage, daily limit
   events and observed Codex quota windows. `--since` accepts an ISO date or
   timestamp, inclusive; dates and daily buckets use UTC. Undated records are
   included only when no date filter is supplied. Stats reads the last ingest;
   it does not refresh sources.

## Storage and inputs

The versioned SQLite database is
`$AGENT_FABRIC_STATE_ROOT/telemetry.sqlite3`, defaulting to
`~/.local/state/agent-harness/fabric/telemetry.sqlite3`. Schema version 4
migrates versions 1–3 metadata; unknown newer schemas are refused.

| Source | Discovery and cursor |
|---|---|
| Fabric | Current workspace plus projects registered in the Fabric `agents` table; shared Git run roots; receipt size and mtime |
| Route health | `AGENT_FABRIC_ROUTE_HEALTH_PATH`, otherwise the state root's `route-health.json`; size and mtime |
| Cooldowns | `FABRIC_COOLDOWNS_PATH`, otherwise the state root's `cooldowns.json`; size and mtime |
| Claude Code | `~/.claude/projects/**/*.jsonl`, including subagents; byte offset, file identity and consumed-prefix digest |
| Codex | `~/.codex/sessions/**/rollout-*.jsonl`; byte offset, file identity, consumed-prefix digest and cumulative token counters |
| OpenCode | `~/.local/share/opencode/opencode.db`, opened with SQLite `mode=ro`; inclusive `time_updated` watermark and file identity |

The Fabric registry follows its existing `AGENT_FABRIC_STATE_DIRECTORY`
setting, defaulting to the same state directory. This differs from the
`AGENT_FABRIC_STATE_ROOT` setting used by route records and telemetry.

Unchanged JSONL files are skipped. Changed files verify the consumed prefix
before parsing new complete lines; an incomplete final line waits for the next
run. A detected rewrite reconciles the file's previous usage rows. Claude
messages deduplicate by `message.id`, retaining the more complete output when
a later duplicate contains partial usage. Codex cumulative counters become
per-event deltas; repeated totals contribute no new tokens. OpenCode messages
upsert by ID, so completion updates do not add another message. A replaced
OpenCode database resets its cursor and reconciles its previous rows.

An older ledger rebuilds file ownership from its source paths on the next
ingest. Unassociated historical rows remain until every previous path is
available and parses successfully; `pending_source_rebuilds` in stats names
incomplete rebuilds.

## Interpretation and cost

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


## Provider quota

```sh
provenant quota                 # cached for at least five minutes
provenant quota --json
provenant quota --refresh       # explicitly bypass the cache
provenant routes --json         # cached quota only; never fetches
```

Codex prefers the credential-free `codex app-server` JSON-RPC rate-limit read,
then falls back to the latest local rollout observations already ingested for
its default identity. Claude reads its own OAuth usage endpoint using the
identity's credentials in process, with Keychain fallback for the default macOS
identity. Cursor reads its usage-summary endpoint using its local session.
Authenticated HTTP requests refuse redirects, so credentials stay at the
provider endpoint. Tokens never enter the ledger, cache, logs, receipts or
stdout and are never shown to agents. The owner authorised this credential use
for this feature on 2026-10-21. Development and tests use fixtures only.

`PROVENANT_QUOTA=0` or `quota.enabled=false` in the telemetry config disables
fetching and quota routing input. Cached quota can still be displayed. Errors
retain the last successful observation with `status: stale`; failed attempts
also respect the cache TTL. The cache lives beside the ledger under
`quota/<provider>--<account>--<host>.json`.

Configure accounts in `<instance-root>/config/telemetry.json`. These are the
`telemetry.accounts.<provider>.<label>` settings; omit the outer `telemetry`
object in this dedicated file. Labels are short non-secret names, never email
addresses or tokens. Plan labels are optional display text; priority is an
optional integer, with smaller numbers preferred.

```json
{
  "quota": {"enabled": true, "cache_seconds": 300},
  "accounts": {
    "claude": {
      "max-a": {"identity_dir": "~/.claude", "plan": "Max", "priority": 1},
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

Successive observations record `natural`, `granted_date_kept`,
`granted_window_restarted`, `window_moved`, `not_started` or `window_started`
events in `quota_events`, per provider, account, host and window. These describe
observed changes, not a presumed billing calendar. Natural rollover compares
the new provider window start against the previous provider reset. Restarted
windows must start within five minutes of the observation.

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

### Adding a provider

1. Add one small module under `scripts/quota_providers/`, then one entry in
   `provider_quota.REGISTRY`. No provider-name branches belong in the core.
2. Implement `ADAPTER`, `default_identity()` and `fetch(account, io, now)`.
   Fetch returns `windows`, optionally `plan`, `source`, `unreported`, `credits`
   and `observed_at`. Provider-specific account config keys are passed through
   in `account.options`; `identity_dir`, `enabled`, `plan` and `priority` are
   handled by the core. Options stay out of snapshots and reports. Each window
   has `name`, `used_percent`, `window_minutes`, `reset_at` and `raw_kind`; names
   are free strings. Report unknown lengths or
   resets as null. Core code normalises, caches and computes pace.
3. Use the bounded `io` HTTP/process functions and fixed, sanitised failures.
   Return only quota metadata. Keep tokens inside the provider call chain.
4. Register a fake module and mock all process/network boundaries in tests.
   Add the account to config and verify it appears in `provenant quota`.

OpenCode's native SQLite source supplies usage tokens and reported cost, not a
verified account quota endpoint. Its Go subscription documentation describes
limits, but Provenant does not turn those descriptions into assumed windows.
agy has receipt-only usage and no registered quota reader. Configured agy
accounts therefore appear as unreported. The inspected Cursor reference uses
its SQLite store for authentication, then obtains allowance from the endpoint;
it does not supply a local token-usage ledger. Cursor usage tokens therefore
remain receipt-only. Neither missing coverage is represented as zero use.

## Multiple hosts

```sh
provenant stats --hosts all --since 2026-10-21
provenant stats --hosts studio peer --json
```

Host IDs come from the existing `hosts.json` `local_host`, defaulting to the
hostname for telemetry. With no selectors, stats remains local. With no peers,
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
Its first slice only stores what it receives; ledger ingestion is a separate
change.

Source: [tools/otlp-collector](../../tools/otlp-collector/otlp-collector.swift),
Swift with no packages. It listens on `127.0.0.1:4318` only, through launchd
socket activation, so it costs nothing while idle. It exits after
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
agent with `launchctl bootstrap`. `PROVENANT_OTEL_NO_LAUNCHCTL=1` writes the
files without touching launchd. Uninstall keeps stored data. The installer never
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
