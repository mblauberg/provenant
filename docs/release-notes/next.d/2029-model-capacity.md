## Fixes
### Accounts, quota and routing

- `lanes` shows a model capacity failure as `model_unavailable`, with the reason. Read-only graph routes fall back to another model, and a manual resume can continue the recorded provider session where the provider supports it. Writers are never replayed automatically (#2029).
