## Behaviour changes

- Pacing profiles, reset references and account overrides accept registered account labels and entry IDs, and reject unknown or ambiguous accounts. Use `adapter/key` when a key is shared across providers (#1992).
- Invalid or stale pacing account references are reported as configuration errors before route resolution or dispatch. Update or remove references when you unregister an account (#1992).
- In `route list` JSON, the `configured_accounts` keys and `reset_of` references now use `adapter/key`. Update scripts that read them (#1992).
