---
name: orchestrate
description: "Use for Fabric graphs, presets, fan-out, councils, repair loops and agent batches. Not for single-owner work or coupled defects (diagnose). Session owns continuing goals and STOP missions."
---

# Orchestrate

Parallelise independent reads, reviews, hypotheses and disjoint writes. Keep
coupled writes in one lane. Start with 3 to 5 lanes unless the work is
mechanical; each lane needs a checkable return that earns its coordination cost.

## Choose the graph

| Job | Preset | Key parameters under `parameters` |
|---|---|---|
| Map unfamiliar ground | `scout-swarm` | `width`, `external`, `read_roots` |
| Reconcile independent views | `fan-out-synthesise` | `width`, `route` |
| Gather independent returns | `fan-out` | `width`, `route` |
| Scope, then build with authority | `scope-then-run` | `worktree`, `write_scope`, `spec` |
| Implement one approved slice | `generator-evaluator` | `worktree`, `scope`, `lenses` |
| Review a pinned subject | `review-council` | `subject.commit`, `risk_tier`, `risk_tags` |
| Integrate several writer lanes | `big-batch` | `lanes`, `batch_worktree`, `integration_branch` |
| Write through CI, repair and land | `deliver` | `worktree`, `max_iterations`, `land`, `decision_session` |
| One bounded task or two sequential passes | `single`, `chain` | `route` |

`scope-then-run` includes a writer: use it only when building is authorised.
`deliver` opens a PR and can land by default; check those actions against the
envelope. `single`, `chain` and the fan-out primitives default to read-only.

## How it runs

Inspect `provenant preset list`, `provenant preset show P` and `provenant preset explain P`.
Preview `provenant run --preset scout-swarm --prompt-file brief.md --set 'parameters.width=3' --explain`.
Submit the same command without `--explain`; add `--params-file lanes.toml`, `--concurrency N` or `--wait 30m` as needed.
Use `provenant status`, `provenant output TARGET` and the returned graph continuation command to collect evidence.

The CLI expands presets; MCP `fabric_graph` accepts graph documents, not preset
names. Keep custom compositions in `.provenant/presets/*.toml`. See the
[preset runbook](../../docs/runbooks/presets.md) for nodes and role overrides.

## Brief and collect

Each brief states outcome, acceptance evidence, source revision, read roots,
literal write scope, prohibited actions, authority, output location and stop
conditions. Give independent reviewers the base, subject and spec, leaving the
author's hypothesis out. `code-review` owns the rubric.

Register a separate worktree per writer. For `big-batch`, bind both lane
worktrees and the serial batch worktree; integrate into the named branch before
reviewing its combined head. Check returned commits, changed paths and tests
against retained output. A fluent report or idle pane proves no criterion.

Use deterministic gates for executable checks, bounded loops for repairs and
councils for questions without a decisive oracle. Inspect recorded family
separation before counting an independent review; HARNESS owns the ladder.
Record unavailable legs and substitutions. The chair reconciles evidence and
keeps authority, user communication and final acceptance.

Load [dispatch](references/dispatch.md) for briefs, continuation and ready queues,
[routing](references/routing-and-tiers.md) for councils and degradation, and
[lane safety](references/lane-safety.md) for writer boundaries. Interactive pane
operation belongs in the [Herdr runbook](../../docs/runbooks/herdr.md).
