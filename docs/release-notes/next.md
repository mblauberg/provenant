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

## Behaviour changes

- `provenant output` on a terminal attempt that has no requested part now
  reports `NOT_FOUND` (exit 2). It used to report `NONTERMINAL` (exit 1)
  locally and `CONFLICT` over a peer. Over MCP the facade code is
  `not_found`. Active attempts still report `NONTERMINAL` (#1673).
