# Next release

Every install's source digest changes once with this release. `check install`
reports existing installs as stale until they are reinstalled.

The source digest now covers shipped workspace source (each member's `src/`,
`assets/` and `build.rs`), manifests, the lockfile, the toolchain and Cargo
configuration, the release packager (`examples/package-release.rs`) and the
bundled assets. Edits under a member's `tests/`, `benches/` and other `examples/`
no longer change it.

## Stability

The v1.0.0 stability promise is withdrawn. Commands, flags, `--json` output,
`config.toml` keys, the state layout and Fabric MCP tool names and schemas can
change in any release, with no deprecation period or compatibility shims. Each
release's notes list its breaking changes. Upgrade every host together, since
peers on different versions are not supported.

## Breaking changes

- Terminal attempts now lose regenerable Codex caches at settlement, known
  credential entries after 24 hours, and resumable provider state after
  `retention.provider_state_days` (default 7). Existing attempts are eligible
  on the first GC sweep. Expired resumes return `PROVIDER_STATE_EXPIRED`.
  Transcripts move to `provider-history/`; results, captures, exit evidence
  and digests remain. Unclassified provider payloads are archived
  conservatively (#1570).
- `disk report` separates provider-state bytes into the `provider_state`
  class instead of `attempt_retained`. GC JSON adds `provider_state` with
  `removed`, `kept` and `errors`; `config show` adds the root `retention`
  section. New candidate state records its adapter in `provider.txt` (#1570).

- Installer cut-over support is removed: the `--legacy-checkout` install flag,
  the `LEGACY_CHECKOUT` error code, pre-1.0 cleanup and Git launcher ownership
  are removed. `check install` no longer reports the finding rules
  `legacy_entry`, `legacy_hook_conflict`, `legacy_instruction_variable` or
  `legacy_checkout_unavailable`. Receipts with a non-null `legacy` object need
  a reinstall; see [receipt recovery](../runbooks/install.md#receipt-recovery).
  Current receipts with `"legacy":null` upgrade normally; newly written
  receipts omit the key (#1571).

## Behaviour changes

- `provenant gc`, including the six-hour macOS schedule, collects terminal
  provider state as well as lane build caches. Set
  `[retention] provider_state_days = 7` in `config.toml` (range 1 to 3650).
  The key is reread at each sweep. `--dry-run` previews provider-state
  removals; active attempts, retained reservations and pending
  continuations are protected (#1570).

- `provenant output` on a terminal attempt that has no requested part now
  reports `NOT_FOUND` (exit 2). It used to report `NONTERMINAL` (exit 1)
  locally and `CONFLICT` over a peer. Over MCP the facade code is
  `not_found`. Active attempts still report `NONTERMINAL` (#1673).
