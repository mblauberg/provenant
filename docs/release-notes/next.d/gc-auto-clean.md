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
