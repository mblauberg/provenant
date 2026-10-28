## Fixes
- Report model capacity failures as `model_unavailable` with their reason in lanes and apply the configured route cooldown. Read-only graph routes can fall back, while manual resume can continue a recorded provider session where supported. Writers are not replayed automatically.
