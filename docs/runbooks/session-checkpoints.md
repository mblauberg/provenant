# Session checkpoints

Owner: workflow
Anchor: tools/front-door/crates/workflow/src/session.rs

Save UTF-8 Markdown with `provenant workflow session save SESSION --file FILE`
and read it with `provenant workflow session show SESSION`. Each checkpoint is
limited to 256 KiB. Session hooks and hand-off prompts can include a shorter
excerpt.

Chair session start and a successful session save show one disk warning when
free space on the state, checkout or lane Cargo cache volume falls below twice
the dispatch floor, `gc.free_floor`, capped at half that volume's capacity.
The warning identifies the volume, gives its free space and threshold, and
points to `provenant disk report` and `provenant gc`. It samples free space
without scanning files; an unavailable observation is silently skipped. See
the [disk runbook](disk.md) to inspect and reclaim space.

Session saves, MCP callers, hooks and handoff use the same Fabric inbox actor:
host plus `PROVENANT_AGENT_LABEL`, configured `fabric.agent_label`, the harness client,
then configured `fabric.client_label` (default `agent`), in that order.
Pass `--client claude` or `--client codex` to select the harness explicitly.
Hook-generated save commands carry their installing harness's `--client`.
Without it, `CODEX_THREAD_ID` identifies Codex; `CLAUDECODE=1`,
`CLAUDE_CODE_SESSION_ID` or `CLAUDE_SESSION_ID` identifies Claude. If both
families have markers, pass `--client`; no harness is inferred.
Configuration directory variables are not active harness markers.
Unlabelled Claude and Codex chairs on one host have different inbox actors.
Lanes cannot save or register chairs (`IN_LANE`).

Ordinary CLI Fabric and lane commands retain their configured actor and
`identity.client` (default `agent`) regardless of harness markers.

An old binding under `agent` or a changed label can return `IDENTITY`.
Before retrying handoff, or to recover a save, run
`provenant workflow session save SESSION --file FILE --client HARNESS --rebind`;
the record keeps the last 8 previous host/actor bindings and change times.
Longer histories are truncated to the newest 8 on read. Handoff recovery
commands include `--client HARNESS` when no explicit client was supplied.
Legacy single-hop audit records upgrade to a list on the next save.
Corrupt discovery JSON returns `HISTORY`; unreadable discovery files return
`IO`. Restore a readable
discovery record from backup before retrying those errors.

Discovery is advisory and scoped to the host's state directory. If publication
fails after the checkpoint and revision manifest commit, the save succeeds with
a warning. Repeat the save to repair discovery (repeat `--rebind` if used).
Readers report stale discovery explicitly; reads never repair it.

## Chair log

Append a substantive update once using `provenant chair log append --session
SESSION --file update.json --operation-id ID --issued-at ISSUED_AT --json`.
Set `ISSUED_AT` to UTC Unix seconds when the operation is first created. The
JSON has required `kind`, `markdown` and `channel` strings and optional `request_ids`,
`attempt_ids`, `issue_ids`, `pr_ids` arrays and `correction_of` sequence.
Use `-` as the file for standard input. A correction is a new entry referencing
an earlier sequence. Keep the operation ID, issued time and identical input
for retries. Within the replay horizon, a retry returns the original entry even
after its payload is pruned; a different input with the same ID is refused.
An append older than the replay horizon returns an explicit expired result;
an issued time more than five minutes in the future is refused. Increasing the
replay horizon does not restore operations whose receipts have been pruned.
When the operation ID is omitted the CLI generates both an ID and an issued time for that invocation.
Supply and persist both values for planned retries.

Read with `provenant chair log list --session SESSION --json`. Pages default
to 50 entries and accept `--limit` from 1 to 100. Continue with `--after` and
the returned `next_cursor`; a byte limit may shorten a page. Entries are
limited to 16 KiB. Log contents are not copied into demand telemetry.

Each append applies the per-session age, count and size limits documented in
[configuration](configuration.md#lane-cache-collection). Listing applies all
three limits without changing storage, and full `gc` removes excess payloads
even for idle sessions. Operation receipts are kept for the configured replay horizon,
then pruned; aggregate appended/pruned counts survive. There is no lifetime
operation limit.

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
