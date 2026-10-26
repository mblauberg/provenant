## Breaking changes

- Fabric MCP now exposes eight tools. Use `fabric_context` for identity and
  hosts, `fabric_lease` for `claim_*` and `landing_*` operations, and
  `fabric_run` with `op: submit` for submissions. The five superseded tool
  names are removed. Tool metadata advertises schema version 2.
- Request durations use seconds: `timeout_seconds`, `wait_seconds` and
  `lease_seconds`. CLI and MCP output parts are `prompt`, `result`, `stderr`
  and `events`; use bare attempt IDs or an unambiguous full
  `run/task/attempt` reference.
- Submissions and CLI manifest tasks use the same tagged route request:
  `kind: model` with `provider` and `model`, or `kind: named` with `name`.
  Both accept optional `effort` inside `route`. Manifest tasks require this
  object; flat `model`, string `route` and task-level `effort` are removed.
  An MCP submission names `task_class` on the task, not inside `route`.
- Run and lane responses expose `provider_seconds` instead of `provider_ms`;
  fractional seconds preserve the observed millisecond precision.
- CLI `lanes --since`, `telemetry stats --since` and `telemetry trace --since`
  accept integer seconds, a date or an RFC3339 timestamp; suffixes such as
  `24h` and `7d` are removed. The config key `limits.graph_timeout_ms` is now
  `limits.graph_timeout_seconds` (1 to 14,400, default 14,400).
- CLI output now uses the same `bytes: {encoding: utf8|base64, data: ...}`
  payload as MCP, replacing separate `text` and `encoding` fields. All four
  output parts also work on peers. A clipped CLI or MCP page stops before a
  split UTF-8 character, and an MCP `events` page holds as many whole events
  as fit `max_bytes`.
- Fencing tokens use 32 lower-case hex digits. Binary output uses padded
  base64 with `encoding: base64` instead of JSON byte arrays.
- Fabric peer protocol is now 3. Upgrade peers together. Run operations share
  one typed service, with transport-specific errors rendered at the edge. The
  peer wire uses the MCP op names (`submit`, `hosts`, `claim_*` and
  `landing_*`), and a peer error reply carries only its typed error; only
  `launch_unknown` names an operation ID.
- Coordination operation receipts now store the public request and result,
  with seconds and hex tokens. Retrying an operation ID recorded by an earlier
  build is refused as a reused ID; use a fresh operation ID.
- CLI continuation refusals use `CONTINUATION_UNSUPPORTED` to distinguish
  an unavailable provider session from other refused execution controls.
- CLI and MCP share one category per run error. A corrupt run record or
  artefact is `STATE_CORRUPT` on the CLI, not `IO`, and an invalid run
  request is `invalid_input` on MCP, not `state_corrupt`.
