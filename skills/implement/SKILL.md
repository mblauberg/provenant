---
name: implement
description: "Use to build an approved software change through the deliver loop: write, CI, repair, independent review, land. Not for unsettled scope (scope), root cause (diagnose), read-only review (code-review) or promotion (release)."
---

# Implement

The `deliver` preset is the default loop. This skill covers what the preset
does not: what to hand it, how to map work onto its nodes, and when to stop.

## Entry

Start from an approved spec: outcome, acceptance criteria with how each is
verified, non-goals, risk tier and write authority (the tracker issue or the
scope digest). Missing inputs go back to `scope`. Do not invent owner decisions;
record an engineering call as `Decided (<source>): <reason>` and proceed.

Repository specifics (verify commands, commit and PR rules, review ladder) come
from the repo's own `AGENTS.md`, `CONTRIBUTING` and runbooks. Look there first.

## How it runs

```sh
provenant run --preset deliver --prompt-file ISSUE.md \
  --set 'parameters.worktree="WT"' --set 'parameters.risk_tier="substantial"' \
  --set 'parameters.spec="#N"' --explain
```

Check the explanation, then submit the same command without `--explain` (add
`--wait 2h` when no one is watching). Nodes run in order: `write`, `ci` (a
repair loop bounded by `max_iterations`), `review` (`review-council`), `accept`
and `land`. `land_tier` lands routine and substantial work automatically;
crucial and terminal work asks the `decision_session`. Follow a run with
`provenant status` and `provenant fabric graph status ID`; take over a stalled
landing with `provenant land N --head SHA --wait`.

Other shapes: `generator-evaluator` for a slice that stays in your worktree,
`big-batch` for several lanes, `single` when the change is routine. Read
`docs/runbooks/presets.md` for parameters.

## Writing the brief

The writer sees only the brief. Put in it the issue path, the acceptance
criteria, the files and owners already found, the checks to run, and the
instruction to stop and report instead of widening scope. One writer per
worktree. Name the method skill: `test-design` for observable behaviour,
`refactor` for restructuring, `diagnose` for an unknown cause.

## Rules the preset cannot enforce

- The implementer may not edit a failing acceptance test or weaken an
  assertion to pass. A changed test is reported with the reason.
- Update the owned docs in the same PR: behaviour, architecture, operations. A
  settled decision moves to its durable home (see `engineering-docs`).
- Keep compatibility only for published or persisted contracts. Otherwise
  change the callers and delete the old path.
- Report checks that did not run as unverified, never as passing.
- A repair that leaves failures unchanged or larger twice in a row stops the
  run. Return the evidence to `scope` when the spec turns out wrong.

## Done

Exact-head CI and the review the tier requires meet acceptance. Finish with
the outcome report owned by `deliver`. Promotion needs separate authority
(`release`).
