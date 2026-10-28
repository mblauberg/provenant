# Session checkpoints

Owner: workflow
Anchor: tools/front-door/crates/workflow/src/session.rs

Save UTF-8 Markdown with `provenant workflow session save SESSION --file FILE`
and read it with `provenant workflow session show SESSION`. Each checkpoint is
limited to 256 KiB. Session hooks and hand-off prompts can include a shorter
excerpt.

Rewrite each checkpoint as the current state: retain authority references, open
items and next steps. Do not append a running log; history belongs in PRs and
issues. Saves warn about oversized checkpoints and references known to be
closed, but still save them.

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

Explicit CLI saves also publish a direct lookup under
`workflow/session-locations/<SHA-256 of canonical cwd, NUL, session ID>.json`,
containing the discovery record. Hook readers validate it against the current
checkpoint and discovery record. Missing, stale or rebound entries deliver no
answers; repeat the explicit save from the hook's working directory to repair
them. Publication is advisory and uses the existing save warning path. Reads
take an explicit deadline and never enumerate projects or invoke Git.

## Structured summary metadata

A checkpoint can include a `provenant_summary` YAML frontmatter header with
`schema_version`, `revision`, `updated_at`, `now`, `next`, `needs_you` and
`risk`. A save records valid summary metadata in the discovery sidecar along
with the checkpoint revision. The frontmatter is limited to 8 KiB; an invalid
header produces a save warning and leaves the summary unavailable. Checkpoints
without a header remain readable. Discovery is advisory: check its
consistency before using the indexed summary, and repeat a save to repair an
interrupted publication. Hooks do not generate summary prose.

The session owner's guarded field edits use the same lock and publication
sequence as a full save. Their result includes discovery warnings, so callers
can report an interrupted index publication and retry it.

Read a summary with `provenant chair summary show SESSION --json`. To change
selected fields, write a JSON object containing any of `now`, `next`, `risk`
(strings) and `needs_you` (an array of request IDs), then run
`provenant chair summary update SESSION --file PATCH.json --expect-revision N --json`.
Use revision 0 to add a summary to an existing checkpoint; omitted fields start
empty. Later patches retain omitted fields and advance the summary revision.
The update keeps the Markdown body and other frontmatter, and publishes a
checkpoint revision and discovery metadata under the save lock. Comments and
blank lines inside a rewritten summary block move before that block. The full
frontmatter, including escaped values and other keys, is limited to 8192 bytes.
A stale
revision is refused with `SUMMARY_STALE` and `data.current_revision`; reread
and reapply the intended change. Check the returned checkpoint warnings: if
discovery publication failed, reread the current checkpoint and save that
Markdown to repair it. Retrying the old summary revision is stale.

`provenant chair summary diff SESSION --from N [--to N] --json` compares fields
in two summary revisions found in retained checkpoint history. These are
summary revision numbers, not checkpoint revision numbers; `--to` defaults to
the current summary revision. The result lists
unavailable revisions when history has been pruned; it does not treat them as
unchanged. Request IDs are reported as added and removed sets. Repeated
identical summaries across checkpoint saves are allowed.

Viewer state is local and keyed by a caller-chosen stable viewer name. After
rendering a summary, record its actual revision with `provenant chair viewer
rendered VIEWER SESSION --revision N`. `chair viewer summary VIEWER SESSION
[--to N] --json` compares that baseline with the selected or current revision;
the baseline and diff are null before the first acknowledgement. The response
returns the current summary and its `to` revision by default; for an explicit
historical `--to`, `summary` is null. Acknowledge the revision only after
displaying it, so an intervening update is not marked as seen. A pruned
baseline appears in the diff's `unavailable` list; missing summaries report
unavailable coverage. Baselines cannot move backwards. `chair viewer forget
VIEWER SESSION` clears a baseline.

`chair viewer request VIEWER ID --json` reads the request and that viewer's
presentation state. `chair viewer set-request VIEWER ID [--seen-revision N]
[--snoozed-until UTC] [--dismissed]` replaces the whole presentation state;
omitted values clear their fields. These operations do not answer, close or
change the blocking flag. Each viewer has a limit of 10,000 request rows and
10,000 summary rows per project, and the project has a combined limit of
100,000 viewer rows. Writes reclaim closed or expired requests; if all retained
requests remain open, resolve or close one before retrying. At the summary
limit a new baseline is refused with `VIEWER_LIMIT` and existing baselines are
kept; run `chair viewer forget VIEWER SESSION` to free one.

Summary show also compares `needs_you` with the bounded local inbox. It reports
stale references and open requests missing from the summary for the same host,
project and chair. Coverage is `partial` when the scan is truncated or a
matching request has unknown host identity; in that case stale references
cannot be established from absence. References observed under another host,
project or chair, or not accounted for by an incomplete scan, appear in
`unknown` and make coverage partial. A complete scan can still establish other
absent references as stale when coverage is partial only because a reference
was observed elsewhere. Missing local host identity reports `host_unavailable`.
Unavailable or incompatible inbox data is reported in coverage. The comparison
does not contact peers or change inbox state.

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
