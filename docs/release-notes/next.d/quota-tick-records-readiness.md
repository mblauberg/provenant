## Fixes

### Accounts, quota and routing

- `provenant quota tick` now records a positive account readiness observation when a probe succeeds with figures. `provenant quota show` no longer reports "unknown (not probed)" and advises another tick that would repeat the same result. A deferred refresh still reports no probe, and a successful probe with no figures remains informational
