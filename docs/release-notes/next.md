# Next release

Every install's source digest changes once with this release. `check install`
reports existing installs as stale until they are reinstalled.

The source digest now covers shipped workspace source (each member's `src/`,
`assets/` and `build.rs`), manifests, the lockfile, the toolchain and Cargo
configuration, the release packager (`examples/package-release.rs`) and the
bundled assets. Edits under a member's `tests/`, `benches/` and other
`examples/` no longer change it.

## Stability

The v1.0.0 stability promise is withdrawn. Commands, flags, `--json` output,
`config.toml` keys, the state layout and Fabric MCP tool names and schemas can
change in any release, with no deprecation period or compatibility shims. Each
release's notes list its breaking changes. Upgrade every host together, since
peers on different versions are not supported.

## Breaking changes

- `worktree remove NAME` now defaults to the fetched upstream default branch,
  falling back to the primary branch with a warning if fetching fails. An
  explicit `--integration` is honoured. Upstream Git calls time out after
  five seconds each.
- `gc` adds `--apply` for merged worktree and declared output deletion. Bare
  `gc` still deletes orphaned and idle caches while previewing these new
  classes; `--dry-run` previews all classes. Reinstall the scheduled agent to
  add `--apply` to its arguments.
- GC JSON adds `worktree_cleanup` and `alerts`. Each worktree cleanup includes
  its own `dry_run` flag, independent of cache cleanup. Preview output entries
  have `removed: false` and reason `would_remove`.
- `[gc]` adds `alert_below` and per-project `regenerable` and `keep` settings.
  Merged worktree deletion requires known-terminal lanes and merged commits
  of their own; fresh tips and first-parent ancestors remain.
- GC reports locked or prunable entries as `locked_or_prunable` and omits
  the primary checkout. Pruning or branch-settlement failures retain that
  entry and allow the sweep to continue.
- Every peer request and reply envelope now carries integer `protocol: 2`.
  The host listing reports the protocol observed in the reply envelope. Calls
  use one SSH connection and reject any different version, naming both
  versions; upgrade peers together. Missing reply protocols and released
  peers' decode refusals identify protocol 1. There is no capability
  negotiation or support for older peers. Facade errors use distinct
  `not_terminal` and `operation_conflict` codes locally and on the peer wire,
  instead of `conflict` (#1631, #1571).
- The CLI reports an unavailable run owner as `OWNER_UNAVAILABLE`, replacing
  `UNSUPPORTED` so the refusal round-trips without changing its meaning.
- Installer cut-over support is removed: the `--legacy-checkout` install flag,
  the `LEGACY_CHECKOUT` error code, pre-1.0 cleanup and Git launcher ownership
  are removed. `check install` no longer reports the finding rules
  `legacy_entry`, `legacy_hook_conflict`, `legacy_instruction_variable` or
  `legacy_checkout_unavailable`. Receipts with a non-null `legacy` object need
  a reinstall; see [receipt recovery](../runbooks/install.md#receipt-recovery).
  Current receipts with `"legacy":null` upgrade normally; newly written
  receipts omit the key (#1571).

## Behaviour changes

- Host entries report `protocol` only when known. Mismatched peers remain in
  the list with `available: false`, their protocol and `reason: "peer_version"`.
- `provenant output` on a terminal attempt that has no requested part now
  reports `NOT_FOUND` (exit 2). It used to report `NONTERMINAL` (exit 1)
  locally and `CONFLICT` over a peer. Over MCP the facade code is
  `not_found`. Active attempts still report `NONTERMINAL` (#1673).
