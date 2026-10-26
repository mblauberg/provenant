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
