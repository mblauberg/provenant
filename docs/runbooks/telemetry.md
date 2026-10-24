# Telemetry and quota

`provenant telemetry stats` reduces retained terminal attempt facts with bundled
prices. Optional `--since DATE`, `--until DATE` and `--detail` bound the report.
Use `--json` for structured output. The old ledger, transcript readers, timer
wrappers, manual credit store and Python quota plugins are retired.

## Quota

`provenant quota show` reads cached observations. `provenant quota tick`
refreshes due accounts through the Rust providers/quota owners. Use
`provenant help quota` for current flags. Quota is admission advice; a reset
or bookkeeping failure never grants permission to stop a healthy provider.

## Routing

`provenant route list` shows cached availability. Select a route or exact model
on dispatch and retain the resolved route from the attempt record.
