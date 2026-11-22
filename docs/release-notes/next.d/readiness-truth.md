## Fixes

### Accounts, quota and routing

- Quota status, readiness and route admission now use one effective-status derivation, one configured freshness policy and typed readiness scopes; `quota --json` reports each account's servable models and the reason for every exclusion.
- State writers with only the state directory (provider settlement, launch reservation) read `domains.quota.freshness` through the state-keyed reader, so a `config.toml` that sets just the freshness keys expires the readiness they publish under those keys instead of the compiled defaults.
