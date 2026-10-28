## Fixes
- Report model capacity failures as `model_unavailable` with their reason in lanes, and apply a short model-scoped cooldown. Read-only graph routes can fall back, while manual resume can continue the provider session where supported. Writers are not replayed automatically.
