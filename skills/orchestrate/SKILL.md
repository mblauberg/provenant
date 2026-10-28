---
name: orchestrate
description: "Use to coordinate bounded work across agents, research lanes or independent reviews. Use diagnose for coupled defects and autopilot for missions that continue until STOP. Not for a small, single-owner task."
---

# Multi-agent orchestration

## Start here

1. Preflight authority, independent tasks, writer isolation and return checks.
   Use [native first](references/routing-and-tiers.md#native-first) for your own
   provider when its native tool supports the work; use Fabric for other
   providers, detached work and sandboxed worktree writers.
2. Before dispatch or recovery, read
   [dispatch-procedure.md](references/dispatch-procedure.md) for tool surfaces,
   submission, claims, waiting, resume, cancellation and landing. Use its
   Fabric brief template, including provenance for agent-filed issues.
3. Review briefs carry the target and rubric, never the chair's hypothesis or
   verdict ([review outcome](../code-review/references/finding-contract.md#review-outcome)).
   Use [verification.md](references/verification.md) for independent checks.

## Always

For a CLI submission, use `provenant run --prompt-file BRIEF --route senior`: it
waits up to 240 seconds and returns the state and result tail. On timeout,
follow its printed wait command; do not submit again. Run longer waits in the
background; never sleep or poll. Use `provenant status` after a restart. Full
worker output stays in run files; return a digest and path. Record `adapter/model@effort` from the
receipt.

## Rules

- Use parallel agents only when the tasks are independent, have stable
  interfaces and checkable outputs, and save more attention than coordination
  costs. Keep coupled work serial.
- Partition writers into separate registered worktrees, and name each writer
  lane's write scope (files or directories) before fan-out. Lanes with
  overlapping write scopes run serially, and dependent lanes merge in dependency
  order. The chair owns authority, decisions, user communication and the final
  synthesis. A worker's report is a claim: verify commits, counts and tests
  against the live tree and transcript.
- Use the [HARNESS.md](../../HARNESS.md) risk ladder for review. It also governs
  provider choice and claim/ack; do not duplicate those policies here.
- Send answer-bearing external work through Fabric request/reply when available.
  If unavailable, mark `FABRIC-ROUNDTRIP-UNAVAILABLE` and keep the direct result
  in a named artifact. Herdr observes and sends steering only.
- Prefer per-dispatch flags. Editing global provider configuration requires
  explicit authority.

## Adaptive loop

Dispatch independent waves, reduce their claims against objective evidence, and add only informative
repair, verification or review waves. Parallel lanes stop ready to merge; the
chair merges serially and reruns checks after the tree changes. Record missing
review legs and substitutions. Objective checks and tier-required review meet
delivery acceptance; the chair reports the outcome and the user may reject it.
Explicit user acceptance is required only for `terminal` work or when the
approved scope asks for it; a project may name its landing gate as acceptance.
External actions retain their separate authority gates. For accepted ready
issues, use
[autonomous-implementation.md](references/autonomous-implementation.md);
run-until-STOP missions use `autopilot`.

## Depth on demand

Use [orchestration-contract.md](references/orchestration-contract.md) for worker
contracts, [routing-and-tiers.md](references/routing-and-tiers.md) for routes,
[tiered-council.md](references/tiered-council.md) for councils, and
[lane-safety.md](references/lane-safety.md) for safeguards. For non-code work,
read [domain adaptation](references/domain-adaptation.md); for scratch handling,
read [memory and scratch](references/memory-scratchpad.md). Other references
cover provider pairing, Herdr, verification and evaluation.

Keep the criteria, evidence and outcome in the existing project document or PR.

Reference eval cases (not run by CI): [lifecycle](evals/lifecycle_cases.yaml),
[contracts](evals/contract_cases.yaml), [discipline](evals/discipline_cases.yaml),
and [topology value](evals/topology_value_cases.yaml).
