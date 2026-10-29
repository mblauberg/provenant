## Fixes
### Accounts, quota and routing

- agy Claude models are no longer paced, reserved or refused because the Gemini pool is spent, and the reverse. The routing catalogue gains an optional `quota_pool` per model; for adapters with grouped quota windows only that group's windows apply, and a model without one keeps every window. `route resolve` shows the pool it used (#2224).
