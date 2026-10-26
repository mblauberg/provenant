## Fixes

Fix disk exhaustion safeguards (#1780). Scheduled GC runs at load and hourly,
with a default free-space floor of 10% of the state volume, at least 50 GiB.
GC reclaims terminal attempts' temporary directories after one day, reports
paths and bytes, and honours dry runs. Writer dispatch refuses to launch below
the floor while read-only lanes remain available. GC alerts when space stays
below the floor after collection. Rerun `provenant install` to update the agent.
