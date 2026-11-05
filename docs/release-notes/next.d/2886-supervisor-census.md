## Fixes
### Supervision

- Supervisors follow lane process events and use a bounded tree walk between full censuses, reducing process-table work and unnecessary wakeups while lanes run or wait (#2886).
- When writer evidence is removed, a supervisor can retire after proving the provider tree and writer lease are quiescent, instead of retrying an impossible reservation write indefinitely.
