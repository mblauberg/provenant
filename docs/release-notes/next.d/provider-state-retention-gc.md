## Behaviour changes

- `provenant gc`, including the six-hour macOS schedule, collects terminal
  provider state as well as lane build caches. Set
  `[retention] provider_state_days = 7` in `config.toml` (range 1 to 3650).
  The key is reread at each sweep. `--dry-run` previews provider-state
  removals; active attempts, retained reservations and pending
  continuations are protected (#1570).
