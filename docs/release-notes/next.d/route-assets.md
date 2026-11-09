## Behaviour changes
### Accounts, quota and routing

- Crucial review floors now cover only credential and secret paths, release, publish, deploy and auto-merge workflows, `CODEOWNERS`, and this repository's own boundary modules, each with its reason in `review-defaults.toml`. Paths that matched only a word such as `build`, `object` or `chair` fall back to their other classes, and a large change no longer selects the `deep` review shape by size alone; integration heads and boundary paths still do (#3081).
- The bundled `senior` pool offers `codex/gpt-6.1-terra` only at low or medium effort, with the weight moved to `codex/gpt-6.1-sol`, because terra at high effort stopped writers early more often than the other routes. An exact `codex/gpt-6.1-terra@high` route and pool overrides in configuration still work (#3074).
