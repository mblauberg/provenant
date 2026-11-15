## Fixes

### Lanes and settlement

- Same-session continuations for native Codex, Claude, Cursor and Agy launches now stay bound to the provider, model, account and launch-selected session root; missing or changed state refuses continuation instead of starting a fresh session. (#3318)
