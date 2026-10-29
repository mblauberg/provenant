## Behaviour changes
### Accounts, quota and routing

- Pacing profiles, reset references and overrides accept registered account labels and entry IDs, and reject unknown or ambiguous accounts; use `adapter/key` when a key is shared across providers. Invalid or stale references are configuration errors reported before dispatch (#1992).
