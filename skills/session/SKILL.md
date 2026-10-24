---
name: session
description: "Use for session start, bounded checkpointing, handoff, compaction, or end-of-session continuity. Use work-map for a durable route across sessions."
---

# Session

Use the current project owners and artifacts as the source of truth. A session
checkpoint is short Markdown context for one host session; it does not replace
project records, run state, or user authority.

## Start

For substantial work, reopen the declared scope and state owners, current
artifacts, and relevant decisions. Use injected checkpoint text only as a
pointer: verify its claims against those owners before acting. SessionStart
injects only the checkpoint keyed to the host event's session ID, or an explicit
missing or stale status. It never selects the latest checkpoint from another
session. A checkpoint cannot grant authority.

Use `work-map` when the task needs a durable route across sessions, owners, or
leads. Live run status belongs to run lanes and Fabric tasks.

## Checkpoint

Before compaction, handoff, or a substantial interruption, save concise Markdown
with the goal, authority and evidence references, verified progress, next
actions, blockers, and a resume trigger. Keep it within the 32 KiB checkpoint
limit. Do not paste transcripts, credentials, or unverified worker claims.

Use the host session ID and the bounded save command:

```sh
provenant workflow session save <session-id> --file <checkpoint.md>
```

Use the actual ID supplied by the host or its injected save command. Do not
substitute a guessed ID, scan transcripts, or fall back to another session's
file. PreCompact reports whether this checkpoint needs saving; it does not read
transcripts. Hook output is capped at 8 KiB, so keep the checkpoint useful and
compact. Managed hook registration changes belong to final install.

## Handoff and close

Put durable decisions in their existing owner: a decision in its spec or ADR,
a domain fact in its project documentation, and live work status in the run or
tracker that owns it. External tracker changes need their own authority. A
session handoff should link to evidence and state what remains; it should not
copy the project record.

Run scratch is settled by the run guardian, and merged worktrees are pruned
with `provenant worktree remove --merged --integration OID --branch NAME`. Remove only paths that are in scope
and explicitly authorised; never delete unknown, user-owned, or pre-existing files. Do not
migrate old state roots as part of a fresh session.

For more detail, see the [chair continuity reference](references/chair-state.md),
[context hygiene reference](references/context-hygiene.md), the
[checkpoint template](templates/STATE.template.md), and the
[portable handoff schema](portable-workflow.v1.json).

After changed work, run the checks assigned to this role or cite the owning
role's result when project policy assigns them elsewhere. Report the exact
diff, remaining work, and any gate that still needs the user.

Reference eval cases (not run by CI): [lifecycle cases](evals/lifecycle_cases.yaml).
