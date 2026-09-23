---
name: orchestrate
description: "Use when bounded fan-out, multi-agent research, cross-family review, parallel audits, Herdr control, or autonomous ready-issue implementation helps. Not for tiny work, coupled debugging, or run-until-STOP jobs; use diagnose or autopilot."
---

# Multi-agent orchestration

## Quickstart

1. Use a native subagent for the chair's own models; use Fabric for other providers, long runs, and worktree writers.
2. Call `fabric_dispatch` with `prompt` or `prompt_file`, `adapter` or `model`, and optional `effort`.
3. For writers, pass `mode: "worktree_write"` and the registered `worktree`.
4. Take the returned id; call `fabric_status` with `ids: [id]` and `wait_seconds: 55`.
5. Repeat status only while running; inspect the terminal row and copy its `Route:` provenance line.
6. For a question, call `fabric_dispatch` with `resume: id` and the answer in `prompt`.
7. Use `fabric_cancel` with `id` to stop; use `fabric_output` with `id` and `part` for a bounded tail.

## Rules

- Use parallel agents only when the tasks are independent, have stable interfaces and checkable outputs, and save more attention than coordination costs. Keep coupled work serial.
- Partition writers into separate registered worktrees. The chair owns authority, decisions, user communication and the final synthesis. A worker's report is a claim: verify commits, counts and tests against the live tree and transcript.
- Use the [HARNESS.md](../../HARNESS.md) risk ladder for review. It also governs provider choice and claim/ack; do not duplicate those policies here.
- Send answer-bearing external work through Fabric request/reply when available. If unavailable, mark `FABRIC-ROUNDTRIP-UNAVAILABLE` and keep the direct result in a named artifact. Herdr observes and sends steering only.
- Choose an explicit model when the user or task names one. Otherwise use `flagship`, `workhorse` or `scout`. `fabric_adapters` shows the current catalogue and health. Unknown models and unsupported effort should run with a reported note or substitution when executable; see [routing-and-tiers.md](references/routing-and-tiers.md).
- Prefer per-dispatch flags. Editing global provider configuration requires explicit authority.
- Keep full worker output in run files and return only the digest and path. For liveness, use `fabric_status`; see [worker-liveness.md](references/worker-liveness.md) for degraded runs. Size alone proves nothing.
- On a terminal result, record `adapter/model@effort` from the receipt; derive family from it. Native subagents record `claude/<model>@<effort> (anthropic; resolved)` from the Agent tool's model parameter, never from self-report.

## Adaptive loop

Preflight authority, isolation, disclosure and receipts. Dispatch independent waves, reduce their claims against objective evidence, and add only informative repair, verification or review waves. Parallel lanes stop ready to merge; the chair merges serially and reruns checks after the tree changes. Record missing review legs and substitutions. For accepted ready issues, use [autonomous-implementation.md](references/autonomous-implementation.md); run-until-STOP missions use `autopilot`.

## Depth on demand

Use [orchestration-contract.md](references/orchestration-contract.md) for worker contracts, [routing-and-tiers.md](references/routing-and-tiers.md) for catalogue choices, [direct-cli-fallback.md](references/direct-cli-fallback.md) only when Fabric cannot express the task, and [codex-capabilities.md](references/codex-capabilities.md) for Codex capability probing. Other references in `references/` cover paired primary work, Herdr, verification, memory and domain adaptation. Direct CLI is a degraded path; record its route and result artifact.

The portable artifact is [coordination summary schema](portable-workflow.v1.json).
