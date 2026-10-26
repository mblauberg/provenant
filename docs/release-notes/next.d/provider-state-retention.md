## Breaking changes

- Terminal attempts now lose regenerable Codex caches at settlement, known
  credential entries after 24 hours, and resumable provider state after
  `retention.provider_state_days` (default 7). Existing attempts are eligible
  on the first GC sweep. Expired resumes return `PROVIDER_STATE_EXPIRED`
  locally and `provider_state_expired` over MCP and peer calls.
  Transcripts move to `provider-history/`; results, captures, exit evidence
  and digests remain. Unclassified provider payloads are archived
  conservatively (#1570).
- Manual `gc` reads `config.toml`; an invalid config now fails the command.
  GC returns `GC` when provider-state retention reports errors (#1570).
- `disk report` separates provider-state bytes into the `provider_state`
  class instead of `attempt_retained`. GC JSON adds `provider_state` with
  `removed`, `kept` and `errors`; `config show` adds the root `retention`
  section. New candidate state records its adapter in `provider.txt` (#1570).

## Behaviour changes

- `provenant gc`, including the six-hour macOS schedule, collects terminal
  provider state as well as lane build caches. Set
  `[retention] provider_state_days = 7` in `config.toml` (range 1 to 3650).
  The key is reread at each sweep. `--dry-run` previews provider-state
  removals; active attempts, retained reservations and pending
  continuations are protected (#1570).

## Fixes

- Resuming skips provider GC tombstones, including interrupted cache removals.
  Legacy candidates infer their provider only when its marker is missing and
  use the same inference for cache pruning and expiry (#1570).
