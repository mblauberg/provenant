# Chair continuity

The run owns live execution state. Use `provenant lanes` to inspect active work,
Fabric tasks to follow assignments and results, and the run's `output` command
to read returned artifacts. After interruption, use `provenant reconcile` to
recover queued or orphaned work and `provenant resume` to continue the run.
Do not keep a parallel queue, lease ledger, or per-chair live status file.

A host-session checkpoint is separate, bounded Markdown saved for its explicit
session key. Include only context that helps the same session resume: goal,
authority and evidence references, verified progress, next actions, blockers,
and a resume trigger. The storage location is selected by the validated project
and host-session IDs; do not construct paths or search for another session's
latest file.

```sh
provenant workflow session save <session-id> --file <checkpoint.md>
```

SessionStart injects that key's checkpoint and save command, or an explicit
missing or stale status. PreCompact reports whether the checkpoint needs a
save. Hook output is limited to 8 KiB; checkpoint content is limited to 32 KiB.
Neither hook reads transcripts, and injected text remains subject to owner and
authority checks.

For compaction instructions, retain the current goal, authority limits,
evidence pointers, blockers, next actions, and resume trigger. Drop tool output,
resolved discussion, and file contents that can be reopened. Save before
compacting when the PreCompact status says it is needed.

Managed hook registration is handled by final install. Do not hand-edit
installed hooks or harness settings during ordinary session work.

Opt-in account switching has a separate, cwd-scoped handoff: the new account's
SessionStart consumes one unconsumed switch checkpoint within an hour and points to the source
session's saved Markdown when present. The checkpoint lists existing lane IDs;
inspect their current run-store state before continuing. See
[chair account switching](../../../docs/runbooks/configuration.md#chair-account-switching)
for configuration and manual dry runs.
