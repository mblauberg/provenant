## Fixes

- Attempt records written before the policy boundary rename decode again.
  Launch policy is dispatch state and is no longer copied into an attempt
  record, so the two retired names `git_isolation` and `data_integrity` in
  stored schema-2 rows no longer fail the attempt read. The retired names still
  decode as the `other_work` boundary wherever a policy snapshot is read, and
  the serialised boundary names are pinned by a test. `gc` no longer reports a
  decode error for those rows and the tick agent no longer exits 3 every
  minute. A retention error now names the attempt row rather than its
  `provider-state` child path. (#3801)
