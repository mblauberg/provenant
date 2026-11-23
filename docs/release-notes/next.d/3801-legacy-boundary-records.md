## Fixes

- A writable open migrates the run store from base 12 to 13 and rewrites every
  affected attempt row: `profile.launch_policy` is removed, and `lane_cache`
  write grants are dropped. Attempt records no longer copy launch policy, so a
  change to the policy boundary vocabulary cannot invalidate stored attempts;
  the two retired names `git_isolation` and `data_integrity` are no longer
  accepted by the current types. The prior aliases and discard-on-decode path
  are gone. `gc` no longer reports a decode error for those rows and the tick
  agent no longer exits 3 every minute. A retention error now names the attempt
  row rather than its `provider-state` child path. (#3801)
