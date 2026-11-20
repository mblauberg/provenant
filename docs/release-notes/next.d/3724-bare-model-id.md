## Fixes

### Accounts, quota and routing

- A model ID written without its provider segment, such as `opencode/hy4-flash-free` for the catalogued `opencode/opencode/hy4-flash-free`, now warns with the full catalogued ID (or every matching candidate) instead of claiming the model is not in the catalogue. When no registered account can serve such a model, the refusal names the unresolved account rather than reporting unverified readiness. (#3724)
