## Fixes

Fix disk exhaustion safeguards (#1780). Scheduled GC runs at load and hourly,
with a default free-space floor of 10% of the state volume, at least 50 GiB.
GC reclaims terminal attempts' temporary directories after one day, reports
paths and best-effort byte estimates, and honours dry runs. Temporary cleanup
failures leave credential expiry and provider-cache pruning eligible to run.
Writer dispatch and resume check the minimum available space across the
worktree, state and lane-cache volumes against the floor, while read-only lanes
remain available. Concurrent writers can each pass this check; it reserves no
space. GC alerts when space stays below the floor after collection.
`gc.alert_below` can only raise the alert threshold above that floor.
Rerun `provenant install` to update the agent.
