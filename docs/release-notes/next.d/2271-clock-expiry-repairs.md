## Fixes
### Accounts, quota and routing

- RFC 3339 configuration instants accept fractional seconds, including nanoseconds; finer precision truncates towards zero to stored microseconds. Policy schedules honour fractional starts and expiry, matching pacing. Settlement retries use the injected clock, and `route pacing` prints every active override (#2271).
