## Fixes
### Lanes and settlement

- Short `run --wait` values no longer cut short the writer registry lock wait.
  Top-level and child writer bindings have a separate launch allowance, configured
  by `domains.fabric.graph.writer_launch_seconds` (default 55 seconds). A launch
  can block beyond a short settlement wait; expiry leaves created attempts running.
