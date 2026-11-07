## Fixes
### Lanes and settlement

- A waited graph submission stops retrying unaccepted admission when the caller's wait expires, retaining the lock cause and recovery guidance. A held writer lock refuses immediately; registry contention remains retryable (#2823).
