## Fixes

- The launch policy a run resolved is dispatch state, not part of a request or
  an attempt record. `HarnessProfile` no longer has a `launch_policy` field, so
  a base-12 sealed `request.json`, which is digest-bound and never rewritten,
  still reads with the old subtree as an unknown extra field. Current writers
  no longer copy the policy, and `resume` and settlement resolve it from
  current host, project and dispatch settings; dispatch overrides remain sealed
  in the profile.
- A writable open migrates the run store from base 12 to 13. A `lane_cache`
  write grant is filtered out; another grant, an absent `write_grants` field
  and unrelated fields are left alone. An attempt row's retired
  `profile.launch_policy` snapshot is left in place because the current
  `HarnessProfile` reads it as an unknown extra field and the sealed request
  keeps the authoritative copy. The grant walk is total for any valid JSON: a
  non-array `write_grants` or a non-object element is skipped rather than
  aborting the migration. A row whose bytes are not valid JSON is skipped so it
  fails only its own decode instead of aborting every writable open. The two
  retired names `git_isolation` and `data_integrity` are no longer accepted by
  the current types, and the prior aliases and discard-on-decode path are gone.
  `gc` no longer reports a decode error for those rows and the tick agent no
  longer exits 3 every minute. A retention error now names the attempt row
  rather than its `provider-state` child path. (#3801)
