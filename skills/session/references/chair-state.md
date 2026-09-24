# Chair state file

A chair that runs lanes across compactions and wakes keeps one run-local state
file. It is the recovery anchor after compaction, not project truth: tracked
owners (issues, board, specs, ADRs) still win.

## Location and shape

`.agent-run/sessions/<session>/STATE.md`, untracked. Project instructions may
name the session. Start from [the template](../templates/STATE.template.md).
Keep it at or under 6 KB, with these level-2 sections:

- **Goal and authority:** the goal in one or two lines; the authority source,
  its limits and expiry; links to the owner directions in force; the line
  `Chair session: <host session id>` so checks can tell chairs apart.
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
and before a handoff. It supplements the canonical handoff rather than
replacing it: a handoff is still written when the session ends or passes the
work to another owner. Then check it:

```sh
python3 "<installed-session-skill>/scripts/state_check.py" .agent-run/sessions/<session>/STATE.md
```

It reports size, missing sections, the top-level next-action count and
staleness (over 30 minutes), exiting non-zero on a problem. Several chairs can
share one project, so without a path it checks only a state file this session
identifies: the `PROVENANT_SESSION_STATE` environment variable, or, in hook
mode, recent state files that name the host session id. Otherwise it prints
nothing.

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

Claude Code can run the check automatically before every compaction. Print the
settings fragment with the installed script path, then merge it into
`~/.claude/settings.json`; the hook always exits 0, so it warns without
blocking compaction:

```sh
python3 "<installed-session-skill>/scripts/state_check.py" --hook-config
```

Hosts without a pre-compaction hook run the check as part of the checkpoint
above.
