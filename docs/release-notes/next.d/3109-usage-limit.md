## Fixes
### Routing and quota

- A Codex lane that ends on the provider's usage-limit message now settles `quota_limited` instead of a generic partial failure, and its reason names the account and the reset time the provider gave (#3109).
- That usage limit holds every model on the account until the reset (or a fresh reading with headroom), so pools and graph nodes move to an account or member with capacity and `provenant quota` shows the account `exhausted` with the reset (#3109).
- `provenant quota` reports `credit_only` only when credits are spendable; a balance the provider marks `hasCredits: false` now reads `exhausted` (#3109).
