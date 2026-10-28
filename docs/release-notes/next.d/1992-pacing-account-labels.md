## Fixes

- Resolve registered account labels and entry IDs in pacing profiles, reset references and account overrides, and reject unknown or ambiguous accounts. Use `adapter/key` when a key is shared across providers.
- Report invalid or stale pacing account references as configuration errors before route resolution or dispatch, including routes for other providers. Update or remove references when unregistering accounts.
- The `route list` JSON `configured_accounts` keys and `reset_of` references now use `adapter/key`.
