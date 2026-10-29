## Fixes
### Accounts, quota and routing

- Transient Claude credential or identity read failures are now retried with a growing delay instead of reported as authentication failures. A manual quota check can retry an account in backoff (#1966).
