## Breaking changes
### Lanes and settlement
- `redaction = "full"` is no longer accepted: config check reports an unknown value with its path and line and redaction falls back to its default. Use `off` or `secrets`. #3782
