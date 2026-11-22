## Fixes

### Accounts, quota and routing

- Quota status, readiness and route admission now use one effective-status derivation, one configured freshness policy and typed readiness scopes; `quota --json` reports each account's servable models and the reason for every exclusion.
