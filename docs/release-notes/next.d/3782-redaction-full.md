## Breaking changes
### Lanes and settlement
- `redaction = "full"` is no longer accepted. An unrecognised `redaction` value now warns and falls back to the restrictive `secrets`, not the registry default `off`; only an absent setting takes `off`. Use `off` or `secrets`. #3782
