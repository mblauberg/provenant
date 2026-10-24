# Telemetry and quota

`provenant telemetry stats` reduces retained terminal attempt facts with bundled
prices. Optional `--since DATE`, `--until DATE` and `--detail` bound the report.
Use `--json` for structured output. The old ledger, transcript readers, timer
wrappers, manual credit store and Python quota plugins are retired.

## Local usage counters

`provenant telemetry stats --usage --since 30d` counts CLI verbs and Fabric MCP
tools, including installed units with zero calls. Omit `--since` for all retained
calls; dates and RFC3339 timestamps also work. Counts cover this host's
`usage.jsonl` and `usage.jsonl.1` under the selected Provenant state directory.
The report includes its own call on the next invocation.

Each CLI invocation records `cli.verb`, its canonical verb path, exit code,
duration in milliseconds and UTC timestamp. Parse failures use `parse`, help
uses `help`, and version uses `version`. Each MCP tool call records `mcp.tool`,
the installed tool name, outcome and timing; unrecognised names use `unknown`.
Arguments, prompts and results are never logged. Skill loads and scripts are
not counted. These counters are separate from token and cost reports, and do
not enable OTel prompt or tool logging.

Writes are best effort: one small `O_APPEND` write, no sync or background
collector. I/O failures leave command results unchanged. Only rotation takes
an exclusive nonblocking lock; a busy rotation lock still allows appends.
MCP rotation runs on existing workers, keeping it off the event loop. At
5 MB the current log replaces `.1`. Concurrent writes can briefly exceed the
cap. Reports read without locks, tolerate a missing `.1`, and skip malformed
or incomplete lines. A concurrent rotation can make a report approximate.
Only two logs are retained, so a 30-day query can cover less than 30 days on a
busy host. Set `PROVENANT_USAGE_STATE_DIR` to isolate counters from the main
state directory, as integration tests do. No config, provider or run-store
state is created by these counters.

## Quota

`provenant quota show` reads cached observations. `provenant quota tick`
refreshes due accounts through the Rust providers/quota owners. Use
`provenant help quota` for current flags. Quota is admission advice; a reset
or bookkeeping failure never grants permission to stop a healthy provider.

## Routing

`provenant route list` shows cached availability. Select a route or exact model
on dispatch and retain the resolved route from the attempt record.
