## Fixes

- Model capacity failures now appear in `lanes` as `model_unavailable` with the reason. Read-only graph routes can fall back to another model, and a manual resume can continue a recorded provider session where the provider supports it. Writers are never replayed automatically (#2029).
