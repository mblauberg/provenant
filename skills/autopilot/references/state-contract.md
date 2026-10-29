# Mission state and recovery

The mission goal belongs to the user. Live execution belongs to the run and
Fabric. A host-session checkpoint is a separate, bounded Markdown aid for
resuming context; it is not a second source of task status.

## State ownership

| Information | Owner | How to resume |
|---|---|---|
| Mission goal, constraints, authority, user gates, and `STATUS` | User-owned goal or project scope | Reopen that owner before acting |
| Active and queued work, task assignments, run outputs, and admission | Run and Fabric | `provenant lanes`, Fabric tasks, and `provenant output` |
| Interrupted or orphaned work | Run | `provenant reconcile`, then `fabric_run` `resume` |
| Short context for one host session | Workflow session checkpoint, keyed by host session ID | SessionStart injects only that key, or explicit missing/stale status |
| Progress for the user (merges, decisions, blockers) | Chair log (`provenant chair log append`) | `provenant chair log list --session` |
| Harness scratch (drafts, helper scripts) | Harness scratch directory (`CLAUDE_CODE_TMPDIR` for Claude Code) | Disposable; move anything needed to its owner first |

Do not create or maintain a manual queue, item lease, fixed set of state files,
or parallel run journal. Do not infer a mission state from an empty lane list.

## Checkpoint content

Before compaction or a substantial interruption, save ordinary Markdown with
the goal, authority and evidence references, verified progress, next actions,
blockers, and a resume trigger:

```sh
provenant workflow session save <session-id> --file <checkpoint.md>
```

Use the actual host session ID. Keep the checkpoint near 2 KiB; the hard limit
is 256 KiB, and hook output is limited to 8 KiB. The hook does not inspect transcripts or select another
session's latest file. Reopen owner records and verify checkpoint claims after
resuming.

## Mission lifetime

The mission stays open until the user sets `STATUS: STOP`. When no work is
eligible or active, the run owns admission and pause behaviour. Record the
condition that can resume work and stop the current invocation; do not call the
mission complete, manufacture work, or set STOP on the user's behalf.

Do not import old session Markdown, mission queues, or run records into a fresh
workflow state. Resume active work by reopening its current owner records and
creating a new authorised checkpoint when useful.
