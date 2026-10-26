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
- Run and lane responses expose `provider_seconds` instead of `provider_ms`;
  fractional seconds preserve the observed millisecond precision.
- CLI `lanes --since` accepts integer seconds or an RFC3339 timestamp;
  duration suffixes such as `24h` are removed. CLI output now uses the same
  `bytes: {encoding: utf8|base64, data: ...}` payload as MCP, replacing
  separate `text` and `encoding` fields. All four output parts also work
  on peers.
- Fencing tokens use 32 lower-case hex digits. Binary output uses padded
  base64 with `encoding: base64` instead of JSON byte arrays.
- Fabric peer protocol is now 3. Upgrade peers together. Run operations share
  one typed service, with transport-specific errors rendered at the edge.
- CLI continuation refusals use `CONTINUATION_UNSUPPORTED` to distinguish
  an unavailable provider session from other refused execution controls.
