## Fixes
### Lanes and settlement

- A waited graph submission stops retrying unaccepted admission when the caller's wait expires, retaining the lock cause and recovery guidance. A held writer lock refuses immediately; registry contention remains retryable (#2823).
- Dirty-writer refusals before launch are retained for status and operation replay, with one path-free notice to the bound chair when enabled (#2823).
- Recovery retains a slot-root custody fence until its carried attempt record is confirmed durable, and reports cutover storage errors while checking other work (#2823).
