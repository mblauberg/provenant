# Telemetry and quota

`provenant telemetry stats` reduces retained terminal attempt facts with prices
from `config/telemetry-prices.toml`. Optional `--since DATE`, `--until DATE`,
`--run-id ID` and `--detail` bound or extend the report. Use `--json` for
structured output. The contract is in the [telemetry spec](../specs/telemetry.md).

## Offline timeline

`provenant telemetry trace [--out FILE] [--since 7d] [--run ID]` writes retained
attempts as Chrome Trace Event JSON (default `provenant-trace.json`), with one
process per task and one thread per attempt. Open the file in ui.perfetto.dev
or chrome://tracing; no collector is needed.

## Local usage counters

`provenant telemetry stats --usage --since 30d` counts CLI verbs and Fabric MCP
tools, including installed units with zero calls. Omit `--since` for all retained
calls; dates and RFC3339 timestamps also work. Counts cover this host's
`usage.jsonl` and `usage.jsonl.1` under the selected Provenant state directory.
A fresh host counts nothing until the state root exists. The report includes its
own call on the next invocation.

Each CLI invocation records `cli.verb`, its canonical verb path, exit code,
duration in milliseconds and UTC timestamp. Parse failures use `parse`, help
uses `help`, and version uses `version`. Each MCP tool call records `mcp.tool`,
the installed tool name, outcome and timing; unrecognised names use `unknown`.
Arguments, prompts and results are never logged. Skill loads and scripts are
not counted. These counters are separate from token and cost reports, and do
not enable OTel prompt or tool logging.

Set `usage.project_attribution` to `true` to include the existing `ProjectId`
hash, rather than a path or name, in usage events and group counts by project.
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

`provenant quota show` reads cached observations. `provenant quota tick`
refreshes due accounts; use `--trigger` and `--power` to record why the
refresh ran and the current power source. `provenant quota display [--headline
auto|5h|weekly] [--icon most_constrained|ACCOUNT]` sets the menu bar choices,
then shows quota. `provenant telemetry stats --usage` reports local invocation
counts. Quota is admission advice; a reset or bookkeeping failure never grants
permission to stop a healthy provider.

## Routing

`provenant route list` shows the catalogue and pools. Select a route or exact
model on dispatch and retain the resolved route from the attempt record.
