## Behaviour changes
### Accounts, quota and routing

- Pacing profiles, reset references and account overrides accept registered account labels and entry IDs, and reject unknown or ambiguous accounts. Use `adapter/key` when a key is shared across providers (#1992).
- Invalid or stale pacing account references are configuration errors, reported before route resolution or dispatch. Update or remove references when you unregister an account (#1992).
