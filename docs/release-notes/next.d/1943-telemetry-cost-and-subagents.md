## Fixes

- `telemetry stats` matches recorded model identities to the bundled price
  table, estimates available usage at API-equivalent prices and counts
  subscription-marked entries. An explicit Claude Code sub-agent report groups
  native usage by model without including message bodies or requiring a
  project or run store. It counts incomplete final transcript records and
  skips them. Native JSON costs use money objects and identify the reviewed
  price table; dated model snapshots are priced when listed in that table.
