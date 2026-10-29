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

Before compaction, handoff, or a substantial interruption, rewrite the checkpoint
as concise current-state Markdown: goal, direction (user rulings and
boundaries), process, and plan with its resume trigger. Keep it near 2 KiB; the
hard limit is 256 KiB. Do not record live lanes, PR status, or progress history
in it; the digest derives lanes and PRs, and the chair log holds history. Do not
paste transcripts, credentials, or unverified worker claims.

## Scratch and owners

Harness scratch is disposable. Keep it in the harness scratch directory: for
Claude Code, the scratchpad under `CLAUDE_CODE_TMPDIR`, which sets where that
tree lives. Anything needed after the session moves to its owner before the
session ends: a project document, the issue or PR body, a run record, the chair
log, or the checkpoint's plan. Do not treat scratch as a resume source.

## Progress for the user

Append one chair-log entry per milestone: a merge, a release, a decision, or a
blocker. Use the host session ID and a fresh operation ID:

```sh
provenant chair log append --session <session-id> --file update.json --operation-id <id> --issued-at <unix-seconds> --json
```

`update.json` holds `kind`, `markdown`, and `channel`, with optional
`pr_ids` and `issue_ids`. Read the history with
`provenant chair log list --session <session-id> --json`. Keep each entry short:
the log is the record a person follows, and the checkpoint is not.

Use the host session ID and the bounded save command:

```sh
provenant workflow session save <session-id> --file <checkpoint.md>
```

Without an explicit Fabric agent label, pass `--client HARNESS` on saves and
handoffs to match your MCP inbox actor. Keep the same client for this session.

Use the actual ID supplied by the host or its injected save command. Do not
substitute a guessed ID, scan transcripts, or fall back to another session's
file. PreCompact reports whether this checkpoint needs saving and saves the
current lane, PR and worktree state to a separate `autosave.md`; it does
not read transcripts and never writes your checkpoint. SessionStart prints a fresh
digest of lanes and the PRs you named (write them as `PR #N`), so notes need not
copy that state. Hook output is capped at 8 KiB, so keep the checkpoint useful
and compact. Managed hook registration changes belong to final install; see
the Claude hooks runbook, `docs/runbooks/claude-hooks.md`.

## Handoff and close

Human inbox requests retain the explicitly declared originating host/session
and actor. Keep that identity with the checkpoint; do not retarget a pending
answer by reusing a label. Inbox target transfer is not implemented: consume and
acknowledge through the original chair identity, or leave delivery queued and
report it during handoff. The [human inbox runbook](../../docs/runbooks/human-inbox.md)
describes revision-safe chat answers and acknowledgement. Session saves do not
create structured chair summaries; they register explicit chair identities.

Put durable decisions in their existing owner: a decision in its spec or ADR,
a domain fact in its project documentation, and live work status in the run or
tracker that owns it. External tracker changes need their own authority. A
session handoff should link to evidence and state what remains; it should not
copy the project record.

Follow the [retention procedure](references/context-hygiene.md#retention)
for run scratch, merged worktrees and old state roots.

For more detail, see the [chair continuity reference](references/chair-state.md),
[context hygiene reference](references/context-hygiene.md), the
[checkpoint template](templates/checkpoint.template.md), and the
[portable handoff schema](portable-workflow.v1.json).

After changed work, run the checks assigned to this role or cite the owning
role's result when project policy assigns them elsewhere. Report the exact
diff, remaining work, and any gate that still needs the user.

Reference eval cases (not run by CI): [lifecycle cases](evals/lifecycle_cases.yaml).
