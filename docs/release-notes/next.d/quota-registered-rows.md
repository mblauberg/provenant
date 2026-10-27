## Fixes

- Quota reports and coverage readings count registered accounts only, omitting
  unregistered cache records and unsupported provider source rows from the
  total. Non-retaining adapters with no registered accounts drop stale records on
  tick, legacy-keyed duplicate agy records are dropped on tick when their
  fingerprint matches a registered account, and empty orphan quota directories
  are removed on drop (#1810).
