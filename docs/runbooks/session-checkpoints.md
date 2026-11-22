# Session checkpoints

Owner: workflow
Anchor: tools/front-door/crates/workflow/src/session.rs

Save UTF-8 Markdown with
`provenant workflow session save SESSION --file FILE` and read it with
`provenant workflow session show SESSION`. The `session` skill owns checkpoint
content and goal continuation; this page owns identity and recovery. The
invariants are in the [workflow spec](../specs/workflow.md#sessions).

## Identity

Saves, hooks and hand-off share one inbox actor: host plus
`PROVENANT_AGENT_LABEL`, then `domains.fabric.agent_label`, the harness client,
then `domains.fabric.client_label` (default `agent`). The harness is inferred
from `CODEX_THREAD_ID` (Codex) or `CLAUDECODE=1`, `CLAUDE_CODE_SESSION_ID` or
`CLAUDE_SESSION_ID` (Claude); with markers for both, or none, pass
`--client claude|codex`. Lanes cannot save (`IN_LANE`). Fabric custody uses the
same harness-aware actor; with `PROVENANT_SESSION_ID` unset it uses the saved
session for that actor if one exists.

A save under a changed label or an old binding returns `IDENTITY`. Recover with

```sh
provenant workflow session save SESSION --file FILE --client HARNESS --rebind
```

The record keeps the last 8 bindings. Corrupt discovery JSON returns `HISTORY`
and an unreadable file `IO`: restore the discovery record from backup. Discovery
(the per-cwd lookup hooks read) is advisory: if publishing fails after the
checkpoint commits, the save succeeds with a warning, and repeating it from the
same working directory repairs it. A missing, stale or rebound entry delivers no
inbox answers.

## Revisions

Each save keeps the checkpoint it replaces as a numbered revision with its
parent and SHA-256:

```sh
provenant workflow session show SESSION --revisions   # number, parent, hash, size, time
provenant workflow session show SESSION --rev 3       # that revision as written
provenant workflow session diff SESSION 3             # revision 3 against current
```

To restore, `show --rev 3 > restore.md`, then save that file: it adds a new
revision rather than rewriting history. `retention.session_revisions` sets how
many are kept (default 8). Saving identical text adds no revision but refreshes
the modification time hooks use for staleness. A revision whose text no longer
matches its hash is refused with `HISTORY`; if the history file is unreadable
the next save starts a new chain.

## Summary and chair log

A checkpoint may open with a `provenant_summary` frontmatter (at most 8 KiB); an
invalid header warns and leaves the summary unavailable. Read it with
`provenant chair summary show SESSION --json`, change fields with
`chair summary update SESSION --file PATCH.json --expect-revision N` (revision 0
adds one) and compare revisions with `chair summary diff SESSION --from N`.
`SUMMARY_STALE` returns `data.current_revision`: reread and reapply. `show` also
reports stale `needs_you` references against the local inbox; coverage is
`partial` when the scan was truncated.

Append a substantive update once with
`provenant chair log append --session SESSION --file update.json --operation-id ID --issued-at UNIX_SECONDS`
(fields in `chair log append --help`; `-` reads stdin). Keep the ID, issued time
and identical input for retries: within the replay horizon a retry returns the
original entry, a different input under the same ID is refused, and an older
append returns an expired result. List with `chair log list --session SESSION`.
Age, count and size limits are `retention.chair_log_days`, `retention.chair_log_entries` and `retention.chair_log_bytes`.
