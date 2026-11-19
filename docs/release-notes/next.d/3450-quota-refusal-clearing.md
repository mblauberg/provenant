## Fixes

### Accounts, quota and routing

- A fresh usage reading below the headroom threshold clears recorded refusals,
  including model-scoped refusals without a pool; a successful run on a route
  clears that model's refusal. Each clearing records its source. `provenant
  quota clear ACCOUNT [MODEL]` removes recorded refusals and reports exactly
  what it removed. (#3450)
