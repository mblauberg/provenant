# Session checkpoints

Owner: workflow
Anchor: tools/front-door/crates/workflow/src/session.rs

Save UTF-8 Markdown with `provenant workflow session save SESSION --file FILE`
and read it with `provenant workflow session show SESSION`. Each checkpoint is
limited to 256 KiB. Session hooks and hand-off prompts can include a shorter
excerpt.

CLI, MCP callers, hooks and handoff derive chair identity from the host and the
explicit agent label, falling back to the configured Fabric agent/client label.
`--client` does not change that identity. Hooks inject the same save command
shown above. Lanes cannot save or register chairs (`IN_LANE`).

An old client-dependent binding or a changed label can return `IDENTITY`.
Recover with `provenant workflow session save SESSION --file FILE --rebind`;
the record keeps the previous host/actor and change time. Corrupt discovery JSON
returns `HISTORY`; unreadable discovery files return `IO`. Restore a readable
discovery record from backup before retrying those errors.

Discovery is advisory and scoped to the host's state directory. If publication
fails after the checkpoint and revision manifest commit, the save succeeds with
a warning. Repeat the save to repair discovery (repeat `--rebind` if used).
Readers report stale discovery explicitly; reads never repair it.

## Earlier saves

A save keeps the checkpoint it replaces as a numbered revision. Each revision
names its parent and carries a SHA-256 of its text, so a bad save or a
compaction can be inspected and restored by hand:

```sh
provenant workflow session show SESSION --revisions   # number, parent, hash, size, time
provenant workflow session show SESSION --rev 3       # that revision's text as written
provenant workflow session diff SESSION 3             # revision 3 against the current one
```

The diff compares whole lines including the final newline; text that differs only
there shows `-a` and `+a` with a `\ No newline at end of file` note on the side
that lacks it, rather than "no differences".

The save result reports the new `revision`, its `parent`, how many revisions
are `retained` and the `keep` limit. Saving text identical to the current
checkpoint adds no revision (`unchanged: true`) but still refreshes the
checkpoint's modification time, which hooks use to judge staleness.

To restore, show the revision and save it again: `show --rev 3 > restore.md`,
then `save SESSION --file restore.md`. That adds a new revision rather than
rewriting history.

The session keeps its newest 8 revisions, the current one included. Set
`[retention] session_revisions` (1 to 64) in `<state>/config.toml` to change
it; 1 keeps no history. The key is read on every save, including the one
`session handoff` makes, and older revisions are
deleted when a save writes the history that no longer names them. A revision
is at most 256 KiB, so a session holds at most 16 MiB of history. A revision
whose text no longer matches its recorded hash is refused with `HISTORY`.

A checkpoint written outside `session save`, or one that predates revisions, is
recorded as a revision at the next save, but the text it replaced was never
kept. If the history file is unreadable, `show --revisions` reports `HISTORY`
and the next save still succeeds, starting a new chain.
