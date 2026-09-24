# Chair state file

A chair that runs lanes across compactions and wakes keeps one run-local state
file. It is the recovery anchor after compaction, not project truth: tracked
owners (issues, board, specs, ADRs) still win.

## Location and shape

`.agent-run/sessions/<session>/STATE.md`, untracked. Project instructions may
name the session. Start from [the template](../templates/STATE.template.md).
Keep it at or under 6 KB, with these level-2 sections in order:

- **Goal and authority:** the goal in one or two lines; the authority source,
  its limits and expiry; links to the owner directions in force.
- **Stage and blockers:** current stage; each blocker with its owner.
- **Active lanes:** a table `id | route | worktree | issue | state | since`,
  one row per live lane or native subagent. Remove a row when its work lands or
  is abandoned.
- **Queue:** a pointer to the frontier (board query, queue file), not a copy.
- **Last checkpoint:** time, base revision and what changed since the previous
  checkpoint.
- **Next actions:** one to three concrete actions.
- **Links:** run directories, assumption ledger, briefs, open pull requests.

Rewrite the file in place; do not append history. Anything durable graduates to
its owner (see **End after changed state** in the skill). When the file
approaches the cap, move detail into linked files rather than trimming live
lanes.

## Checkpoint

Rewrite the state file at the end of every wake, before any manual compaction,
and before a handoff. Then check it:

```sh
python3 "<installed-session-skill>/scripts/state_check.py"
```

With no arguments it finds `.agent-run/sessions/*/STATE.md` under the project
root and reports size, missing sections, the next-action count and staleness
(over 30 minutes), exiting non-zero on a problem. Outside such projects it
prints nothing.

## Compaction

After compaction, re-read the state file and reconcile it against live
evidence before dispatching anything.

Where the host accepts compaction instructions (Claude Code reads a
`Compact instructions` section from project instructions and accepts text after
`/compact`), keep them short:

```markdown
## Compact instructions

Keep: the state file path, active lane ids with routes and worktrees, owner
directions in force, open decisions and the next actions. Drop tool output,
file contents, diffs, images and resolved threads; they are on disk.
```

Claude Code can run the check automatically before every compaction. The
owner registers it in `~/.claude/settings.json`; it always exits 0, so it
warns without blocking compaction:

```json
{
  "hooks": {
    "PreCompact": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "python3 \"$HOME/.claude/skills/session/scripts/state_check.py\" --hook",
            "timeout": 5
          }
        ]
      }
    ]
  }
}
```

Hosts without a pre-compaction hook run the check as part of the checkpoint
above.
