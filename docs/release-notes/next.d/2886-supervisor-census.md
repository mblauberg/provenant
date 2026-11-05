## Fixes
### Supervision

- Supervisors follow lane process events and use a bounded tree walk between full censuses, reducing process-table work and unnecessary wakeups while lanes run or wait (#2886).
