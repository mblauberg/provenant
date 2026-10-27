## Behaviour changes

Intermediate event log entries from the run-store database now expire under
`retention.logs_days` (default 30) once all sibling attempts in a run are
terminal. The terminal attempt records, result artifacts, exit evidence,
digests, bounded capture tails and child-store databases survive. The same
retention policy visits child stores.
