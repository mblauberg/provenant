## Fixes
### Lanes and settlement

- Short `run --wait` values no longer cut short the writer registry lock wait.
  Top-level and child writer bindings have a separate launch allowance, configured
  by `domains.fabric.graph.writer_launch_seconds` (default 55 seconds). A launch
  can block beyond a short settlement wait. One deadline covers provisioning,
  existing worktrees, later custody and registry checks, and launch retries.
  Child relays honour configured allowances longer than 55 seconds and return
  the launch receipt or named refusal. Settlement expiry leaves accepted attempts running.
- Concurrent writer provisioning avoids probing sibling worktrees for cleanliness,
  preventing transient inspection handles from being mistaken for live writers.
