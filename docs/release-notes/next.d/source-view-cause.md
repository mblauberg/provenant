## Fixes

### Lanes and settlement

- `run --use review` now builds its pinned source view with one batched object read instead of one Git process per file, so large repositories and linked worktrees whose HEAD differs from the subject no longer fail with a generic preparation refusal. A refusal now says which step failed (tree listing, object read, temporary directory or index build) and whether it timed out, hit an output limit or exited from Git; `status` and `output` show it, and the text no longer points at route and account configuration. The time, file-count and size limits are `domains.run.source_view_seconds`, `source_view_entries` and `source_view_bytes`.
