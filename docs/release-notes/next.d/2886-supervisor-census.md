## Fixes
### Supervision

- Supervisors follow lane process events and use a bounded tree walk between full censuses, reducing process-table work and unnecessary wakeups while lanes run or wait (#2886).
- When writer evidence or the original lock is removed, a supervisor can retire after proving the provider tree and original lease are quiescent, keeping any unresolved fence rather than retrying indefinitely.
