---
name: orchestrate
description: "Use to delegate or parallelise work: lanes, graphs, templates, review panels, waiting and landing."
---

# Orchestrate

Use `provenant templates show NAME --json` to inspect bundled, user, repository
and inline templates. Dotted names select flavours such as `review.security`.

## Commands

Use `provenant help --all` for commands and options; pin a PR with `provenant run review-panel --pr 123 --brief-file brief.md`. Explain a graph before
submitting it; check its authority, write scope and gates first.

## Divide the work

Delegate decoupled, context-heavy or cross-family work, or to use another
subscription; keep small, coupled and user-facing steps with the chair. Split
by context and dependency, settle shared interfaces first, and budget roughly
1.4–3.3 times the tokens for concurrent work. Integrate each return as it
finishes and land ready work independently. Any lane may launch child lanes.
`implement` starts a junior writer by default; `--route` picks another tier and
`--plan` sizes the brief first.

Sources: [dynamic concurrency](https://arxiv.org/abs/2610.10263),
[agent loop](https://claude.com/resources/articles/building-agents-when-to-fold-subagents-into-your-main-agent-loop),
[coordination](https://arxiv.org/abs/2608.16801) and
[coordination study](https://arxiv.org/abs/2607.27877).

`run implement --branch NAME --base REF` creates a single writer's worktree. Register one
by hand (`provenant worktree create BRANCH`) only to share it: `batch` gives every
slice the same worktree, so its writers run in sequence. Parallel writers need
a graph with a worktree and write scope per node.

Inside a lane, `provenant run TEMPLATE` adds a child node to the calling graph, and
`--name` names it. Attached children are cancelled when their parent settles;
`wait` collects them first.

`cloud launch` hands a brief to a provider's cloud session and prints its link
and key; Provenant keeps no lifecycle for it. `cloud collect KEY` adds a
worktree holding the branch it pushed, which then goes through review like any
other.

## Wait and recover

Block on one `wait` call, with `--any` for the first settlement; never poll.
Expiry leaves targets running. Reconcile the returned event before resuming.
Recover an existing graph with `status`, `output` and `retry` before launching
replacement work. Read a settled lane with `result NAME`, which carries its
handoff, head and verification; do not parse provider logs. An idle run or empty queue means no eligible work now, not
done.

For a Claude Code chair, arm a Monitor on `provenant events --follow --json
--consumer chair-<session>`. Each JSON line is a notification while the chair
continues working. Monitors expire after 30 to 60 minutes; re-arm with the same
consumer to resume from its host cursor. For one outcome, run `provenant wait
… --json` in the background and receive the notification on exit. Delivery is
at least once: reconcile the result and deduplicate by event identity.

## Collect

Check commits, paths, acceptance evidence, review subject and the resolved
`adapter/model@effort` against the output, not the requested tier. A failed
child blocks dependent work, and a synthesis cannot waive a gate. Integrate each
ready head in dependency order; run affected checks and use proportionate review.
Read CI through `ci failures pr N M` and `pr status N M`: one call per question, not one `gh` read per job.

[Lanes and panels](references/lanes.md) covers briefs, confinement and review
independence. [Herdr](references/herdr.md) covers watching panes and peers.
