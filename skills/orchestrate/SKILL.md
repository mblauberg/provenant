---
name: orchestrate
description: "Use to delegate or parallelise work: lanes, graphs, templates, review panels, waiting and landing."
---

# Orchestrate

Templates: `task` for one bounded assignment, `plan` to shape open work,
`implement` for one writer slice, `batch` for disjoint slices into one
integration branch, `deliver` through CI, review and landing, `explore` over
areas, `research` for dated primary sources, `design` for options, `review`
for a pinned head, `verify` to falsify claims and `synthesise` to reconcile
returns. Dotted names select flavours (`review.security`, `implement.fix`).
Layers are bundled, user, repository (`.provenant/templates/`) and inline;
`provenant templates NAME --json` shows one.

## Commands

The one canonical set; other skills name templates and point here. Run with
`--explain` first, check authority, write scope and gates, then submit without
it.

```sh
provenant run --route junior --file brief.md --branch NAME --base REF
provenant run --use plan --file brief.md
provenant run --use explore --route junior --over a,b --file questions.md
provenant run --use review --file spec.md --set subject='"origin/BRANCH"' --set review_base='"origin/main"'
provenant status --wait --target graph:ID --target pr:N --until all
provenant output TARGET --trace
provenant retry TARGET    # also stop, steer, edit
provenant land N --head SHA --wait
provenant cloud launch --file brief.md --provider codex --environment ENV
provenant cloud collect KEY
provenant ask --file question.json --session SESSION
```

## Shape the work

Slice into lanes of about three acceptance items with disjoint write scopes,
merged into one integration branch. Settle shared contracts first; use waves
only for real dependencies. Default slices to junior, contained design to
senior and root constraints to principal; consult recorded cost per template.
A long writer is for coupled work that cannot split. Review each slice
lightly and the integrated head with the full panel.

`run --branch NAME --base REF` creates a single writer's worktree. Register one
by hand (`provenant worktree create`) only to share it: `batch` gives every
slice the same worktree, so its writers run in sequence. Parallel writers need
a graph with a worktree and write scope per node.

Inside a lane, `run --use TEMPLATE` adds a child to the same graph, sharing
budget, stopping and trace. Attached children are cancelled when their parent
settles; `--wait` collects them first.

`cloud launch` hands a brief to a provider's cloud session and prints its link
and key; Provenant keeps no lifecycle for it. `cloud collect KEY` adds a
worktree holding the branch it pushed, which then goes through review like any
other.

## Wait and recover

Block on one `status --wait` call; never poll. Expiry leaves targets running.
Reconcile the returned event before resuming. Recover an existing graph with
`status`, `output` and `retry` before launching replacement work. An idle run
or empty queue means no eligible work now, not done.

## Collect

Check commits, paths, acceptance evidence, review subject and the resolved
`adapter/model@effort` against the output, not the requested tier. A failed
child blocks dependent work, and a synthesis cannot waive a gate. Integrate
disjoint heads in dependency order, run the affected checks and review the
combined head.

To restart a long writer fresh from its handoff, set
`lanes.context_handoff_tokens` under `[knobs]` in host configuration; it is off
by default.

[Lanes and panels](references/lanes.md) covers briefs, confinement and review
independence. [Herdr](references/herdr.md) covers watching panes and peers.
