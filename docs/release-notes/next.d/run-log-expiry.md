## Behaviour changes

Intermediate event log entries from the run-store database now expire under
`retention.logs_days` (default 30) once all sibling attempts in a run are
terminal. The terminal attempt records, result artifacts, exit evidence,
digests, bounded capture tails and child-store databases survive. Each attempt's
newest event also survives so unacknowledged settlements remain visible to lane
cursors. GC reports the number of expired event rows in previews and applied
sweeps, including child stores, as `provider_state.expired_events` in JSON.
