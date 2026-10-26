## Behaviour changes

- `provenant telemetry trace` now writes one Perfetto process per host rather
  than per task, with the task in each thread name and the attempt's `Trace` in
  the slice args. A flow arrow joins an attempt to its parent when both are in
  the file. `--peers` also reads the terminal attempts in each configured
  peer's newest 100 lanes, as `lanes --host` does, so Fabric and MCP
  submissions to a peer appear as well as `dispatch --host` runs; an
  unreachable peer is listed under `peers` in `--json` and skipped (#1571).
