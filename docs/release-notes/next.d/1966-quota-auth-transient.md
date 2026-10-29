## Fixes
### Accounts, quota and routing

- Transient Claude credential and identity read failures are treated as unknown and retried with a delay that grows from 15 to 300 seconds. Only an explicit rejection or missing credentials reports an authentication failure, and a manual quota check can retry an account in backoff (#1966).
