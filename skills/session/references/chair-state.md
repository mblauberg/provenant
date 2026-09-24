# Chair state file

A chair that runs lanes across compactions and wakes keeps one run-local state
file. It is the recovery anchor after compaction, not project truth: tracked
owners (issues, board, specs, ADRs) still win.

## Run layout

One `.agent-run/` per Git common directory, shared by its linked worktrees,
unless project instructions name another root. Its top level holds only:

- `runs/`: Fabric-owned run directories; read them through Fabric, not by path;
- `sessions/<session-id>/`: one per chair session, holding its `STATE.md`,
  assumption ledger, lane briefs and any scripts the session resumes with;
- `locks/`: shared host or project locks.

Each chair keeps exactly one `STATE.md`; no `NOW.md`, `PICKUP.md` or parallel
status files. A host scratchpad or temporary directory is for disposable probes
only: anything a later wake or successor needs lives under the session
directory. Retention and cleanup go through `provenant clean`, never manual
deletion.

## Shape

Keep `STATE.md` untracked, at or under 6 KB, starting from
[the template](../templates/STATE.template.md), with these level-2 sections:

- **Goal and authority:** the goal in one or two lines; the authority source,
  its limits and expiry; links to the owner directions in force; the line
  `Chair session: <host session id>` so checks can tell chairs apart.
- **Stage and blockers:** current stage; each blocker with its owner.
- **Active lanes:** a table `id | route | worktree | issue | state | since`,
  one row per live lane or native subagent. Remove a row when its work lands or
  is abandoned. `worktree` names the directory a writer lane owns, per
  `setup-repo`'s branch naming doctrine (the branch with `/` replaced by `-`);
  `id` is this table's own lane label, not that branch name.
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
python3 "<installed-session-skill>/scripts/state_check.py" .agent-run/sessions/<session-id>/STATE.md
```

It is the single state validator; project checkpoint scripts call it rather
than re-implementing it. It reports size, missing sections, the top-level
next-action count and staleness (over 30 minutes), exiting non-zero on a
problem. Several chairs can share one project, so without a path it checks
only a state file this session identifies: the `PROVENANT_SESSION_STATE`
environment variable, or, in hook mode, a recent state file whose directory is
the host session id or whose `Chair session:` line names it. Otherwise it
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

Claude Code runs the check before every compaction once the hook is
installed, and installing it is part of setting up a Claude Code chair; hosts
without such a hook run the check at each checkpoint instead. The command adds one
`PreCompact` entry to `~/.claude/settings.json` (or the settings file given),
keeps everything else and is idempotent. The hook always exits 0, so it warns
without blocking compaction:

```sh
python3 "<installed-session-skill>/scripts/state_check.py" --install-hook
```

`--hook-config` prints the same fragment for manual merging.

Hosts without a pre-compaction hook run the check as part of the checkpoint
above.
