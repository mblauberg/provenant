# Continuity contract

## Host checkpoint

Rewrite about 2 KiB of Markdown for the explicit host session ID. Use the
[checkpoint template](../templates/checkpoint.template.md): goal and owner;
direction and authority; process and evidence pointers; next actions, blockers
and resume trigger. Keep PR references as `PR #N` for the derived digest.
Link live lanes and current criteria back to their owners instead of copying
their changing status. A checkpoint grants no new authority.

Save through `provenant workflow session save SESSION --file checkpoint.md`,
selecting the actual `--client` when needed to preserve the chair identity.
Use the host-supplied key, never another session's latest file. Lanes cannot
save chair checkpoints. Hooks derive a separate autosave; the chair alone
writes its checkpoint. The [runtime runbook](../../../docs/runbooks/session-checkpoints.md)
owns revision history, identity rebinding and summary metadata.

Before compaction, retain rulings, authority, evidence pointers, unresolved
questions and the next action. Drop transcripts, tool output and contents
that can be reopened. Move needed results from disposable scratch to their
declared owner before the invocation ends. Durable decisions belong in project
docs; only architectural decisions become ADRs. Apply existing cleanup
authority, including the merge workflow's own pruning, rather than treating
an old path as disposable.

## Lane handoff

When the harness supplies a progress path, update that JSON atomically
throughout work, outside the repository and commit set. Write a sibling
temporary file and rename it over the destination. Repeat its current value
as one JSON line in the final `handoff` fence, with nothing after it.

```json
{"schema_version":2,"status":"stopped_early","note":"Parser committed; verification remains.","remaining":["Run the parser contract check."],"decisions":["Preserve the published wire format."],"verify":["Compiler passed."]}
```

Statuses are `done` only when the brief is finished, `stopped_early` for
unfinished work that can continue, and `blocked` for work needing an external
change or decision. The object is at most 12,288 bytes. `note` and each
array entry are 1 to 4,096 UTF-8 bytes; `remaining`, `decisions` and
`verify` contain at most 64 strings each. Record verification actually
performed and its result, including unavailable checks.

For a successful writer, valid `stopped_early` can request a bounded
same-checkout continuation. It does not guarantee unlimited retries.
`done` and `blocked` request no automatic continuation. Preserve unfinished
items in `remaining` so downstream consumers retain them when excerpts clip.
This lane contract is separate from a host checkpoint and from a standing
mission's STOP rule.

## Progress history

Append milestones to the chair log. For retryable appends, retain
`--operation-id ID`, `--issued-at UNIX_SECONDS` and identical JSON input;
the [runbook](../../../docs/runbooks/session-checkpoints.md#chair-log)
owns replay and retention. Preserve the original host/project/session/actor
for pending inbox answers across handoff; changing a label does not transfer
their delivery target.
