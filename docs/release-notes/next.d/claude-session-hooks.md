## Additions

- The Claude Code session hooks now report and keep lane and pull request
  state. A new `hook claude session-start` handler (SessionStart on startup,
  resume and clear), and the existing `compact` handler, print a bounded
  digest of this project's in-flight lanes, the lanes settled since the chair
  last saved the checkpoint, and `pr status` for the pull requests the chair
  named in the checkpoint (`PR #N`). The digest is read-only. A section that
  cannot be read is named as unavailable rather than shown empty (#1843).
- `pre-compact` now saves lane, PR and worktree state into the session
  checkpoint, and a new `hook claude stop` handler refreshes an existing
  checkpoint's state when it has been quiet for ten minutes. Both write one
  marked block after the chair's text, replace only that block on later
  saves, and refuse rather than trim the chair's text when it leaves no room.
  A checkpoint holding only the block is reported as such, so the chair still
  saves notes and authority references. Lane sessions only report (#1844).
- Rerun `provenant install --hooks on` to register the new handlers.
