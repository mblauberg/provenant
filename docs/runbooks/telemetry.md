# Local telemetry

The ledger implements the first ingestion and reporting slice of
[R4](../specs/harness/operating-model.md#r4-local-telemetry-ledger). It stores
usage and routing metadata locally, with no message text, prompts, tool
output or file contents. Ingestion reads provider stores without modifying
them. It does not contact providers or fetch prices.

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
`~/.local/state/agent-harness/fabric/telemetry.sqlite3`. Schema version 3
migrates version 1 and 2 metadata; unknown newer schemas are refused.

| Source | Discovery and cursor |
|---|---|
| Fabric | Current workspace plus projects registered in the Fabric `agents` table; shared Git run roots and legacy local `mcp-*` roots; receipt size and mtime |
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
and stays missing when any input category is incomplete.

For an adapter/session with native usage in the requested period, stats uses
that usage instead of the Fabric receipt's aggregate. Receipt statuses remain
visible. `receipt_usage_rows_excluded` reports this choice. Limit outcomes in
route health and cooldowns are deduplicated against matching receipt outcomes,
then against each other by route, run, status and day. These snapshots retain
only the events still present at ingest time; the ledger cannot recover older
observations that vanished before its first ingest.

[telemetry-prices.json](../../config/telemetry-prices.json) owns USD rates per
million input, cached-input, cache-write and output tokens. Its dated roster
entries cite repository model identities only; their numeric prices are null
because the repository does not verify usable prices. Populate a price only
with a dated source supporting that price category. OpenCode route keys retain
the upstream provider namespace, for example
`opencode/opencode-go/<model>`.

Estimated cost charges each bucket once, using the output rate for reasoning.
It is labelled `api_equivalent_estimate`, including subscription routes, and
remains null if a required price or token category is unknown. An incomplete
aggregate's estimated cost stays null; `missing_cost_rows` gives its coverage.
OpenCode's reported cost is retained separately from the estimate.

Fabric `ok` is an execution outcome. This first view does not infer acceptance,
gate results, graph efficiency, routing effectiveness or subscription allowance
usage from token counts. Accepted-task costs and the remaining R4.4 measures
need their owner evidence; Codex window observations seed the later R3 quota
ledger.

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
URL, and Codex metrics and traces stay off. Claude Code variable behaviour is
from its documented names and was not exercised here.

Lane linkage (follow-up, not implemented): the dispatcher should set
`OTEL_RESOURCE_ATTRIBUTES=provenant.run_id=<run>,provenant.lane=<lane>,provenant.account=<account>`
in each lane's environment so exported events join to Fabric runs. It touches
the lane process environment in `scripts/fabric_lanes.py` and the adapters, so
it is left for its own change.
