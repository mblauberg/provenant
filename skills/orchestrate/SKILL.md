---
name: orchestrate
description: "Use to delegate or parallelise work: lanes, graphs, templates, review panels, waiting and landing."
---

# Orchestrate

`provenant templates` lists bundled, user, repository and inline templates and
`provenant templates show NAME --json` inspects one. Dotted names select
flavours such as `review.correctness`. `provenant help --all` lists commands
and options; `provenant run --explain` shows a compiled graph without starting
work. Check its authority, write scope and gates before submitting it.

## One accountable chair

One accountable chair owns coordination; native execution stays within its
assigned authority and never becomes a second chair or a parallel owner of the
same work. Only the chair holds the user, the boundaries, merge and release.
This rule names no lane shape: a lane that launches children coordinates only
its own subtree, with authority narrowed from its parent, and a native sub-agent
never re-plans or re-delegates the parent's whole assignment. The sections below
describe how the chair exercises that authority, not ways to divide it.

## Divide the work

The global rules own when and how to delegate. Any lane may launch child lanes.
`implement` starts a junior writer; `--route` picks another tier and `--plan`
sizes the brief first.

`run implement --branch NAME --base REF` creates a single writer's worktree;
`provenant worktree create BRANCH` and `--worktree` reuse one. `batch` gives
each slice its own worktree and integrates slices as they settle (see the
routing-and-templates runbook for `after` ordering and `max_parallel`). Name
disjoint write scopes for parallel writers.

Inside a lane, `provenant run TEMPLATE` adds a child node to the calling graph
and `--name` names it. Attached children are cancelled when their parent
settles; `wait` collects them first, and `--detached` lets one outlive its
parent.

`cloud launch` hands a brief to a provider's cloud session and prints its link
and key; Provenant keeps no lifecycle for it. `cloud collect KEY` adds a
worktree holding the branch it pushed, which then goes through review like any
other.

## Wait and recover

Block on one `wait` call, with `--any` for the first settlement; never poll.
Expiry leaves targets running. Reconcile the returned event before resuming.
Recover an existing graph with `status`, `output` and `retry` before launching
replacement work. Read a settled lane with `result NAME`, which carries its
handoff, head and verification; do not parse provider logs. An idle run or
empty queue means no eligible work now, not done.

For a Claude Code chair, arm a Monitor on `provenant events --follow --json
--consumer chair-<session>`; each JSON line is a notification while the chair
keeps working. A Monitor can expire; re-arm it with the same consumer to resume
from its cursor. For one outcome, run `provenant wait … --json` in the
background and receive the notification on exit. Delivery is at least once:
reconcile the result and deduplicate by event identity.

## Collect

Check commits, paths, acceptance evidence, review subject and the resolved
`adapter/model@effort` against the output, not the requested tier. A failed
child blocks dependent work, and a synthesis cannot waive a gate. Integrate each
ready head in dependency order, run affected checks and use proportionate review
(`code-review`). Read CI as `deliver` describes, not with one `gh` read per job.

[Lanes and panels](references/lanes.md) covers briefs, confinement and review
independence. [Herdr](references/herdr.md) covers watching panes and peers.
