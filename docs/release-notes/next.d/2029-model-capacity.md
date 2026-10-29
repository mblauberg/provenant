## Fixes
### Accounts, quota and routing

- `lanes` shows a model capacity failure as `model_unavailable`, with the reason. Read-only graph routes fall back to another model, and a manual resume can continue the provider session where supported (#2029).
