---
name: orchestrate
description: "Use for bounded fan-out, multi-agent research, cross-family reviews, audits, Herdr control and ready-issue automation. Not for tiny work; coupled defects: diagnose; run-until-STOP: autopilot."
---

# Multi-agent orchestration

## Quickstart

1. [Native first](references/routing-and-tiers.md#native-first): run your own provider's models as native subagents (Claude Code: Agent tool `opus`/`sonnet`/`haiku`; Codex: its subagents), not Fabric. Use Fabric for other providers (agy's Claude 4.6 included), long or detached runs, and sandboxed worktree writers.
2. Call `fabric_dispatch`; its tool schema documents the arguments. `model` takes `adapter/id[@effort]`; `route` takes only a [pool name](references/routing-and-tiers.md#route-pools), and an explicit id there is `route_invalid`. `fabric_adapters` with `models: <adapter>` lists live ids.
3. For writers, pass `mode: "worktree_write"` and the registered `worktree`,
   named per `setup-repo`'s branch naming doctrine; the task id defaults to that
   same branch-derived string. Before dispatch, [confirm the connection's
   project](references/fabric-identity.md), then acquire and verify an advisory
   `provenant fabric claim acquire --issue KEY --operation-id ID` (or
   repeat `--path PATH` for a path set); keep its generation/token proof, renew
   it while work continues and release it when
   finished. Do not dispatch against a conflicting live claim; dispatch does not
   enforce claims.
4. After dispatch, keep one `provenant lanes --wait N` running in the background. When it exits, read the printed lanes, act, and re-arm it ([chair-loop.md](references/chair-loop.md#waiting) covers `--all`). A dispatching sub-agent blocks in the foreground ([worker-liveness.md](references/worker-liveness.md)).
5. Use `provenant lanes` as the source of truth after compaction or restart; inspect a terminal row with `fabric_status` and copy its `Route:` provenance line. Read retained events with `provenant output RUN/TASK/ATTEMPT --part events` and its byte offset.
6. For a question, call `fabric_dispatch` with `resume: id` and the answer in `prompt`; pass one `session` name to continue a provider conversation across turns or agents.
7. Use `fabric_cancel` with `id` to stop; use `fabric_output` with `id` and `part` for a bounded tail.

## Rules

- A rejected batch task returns its `error` and `fix`; the other tasks still run, except when a Claude seat explicitly requests its own Claude route. That guard refuses the entire batch before placement, names the offending task id and returns every task as rejected.
- Use parallel agents only when the tasks are independent, have stable interfaces and checkable outputs, and save more attention than coordination costs. Keep coupled work serial.
- Partition writers into separate registered worktrees, and name each writer lane's write scope (files or directories) before fan-out. Lanes with overlapping write scopes run serially, and dependent lanes merge in dependency order. The chair owns authority, decisions, user communication and the final synthesis. A worker's report is a claim: verify commits, counts and tests against the live tree and transcript.
- Use the [HARNESS.md](../../HARNESS.md) risk ladder for review. It also governs provider choice and claim/ack; do not duplicate those policies here.
- Send answer-bearing external work through Fabric request/reply when available. If unavailable, mark `FABRIC-ROUNDTRIP-UNAVAILABLE` and keep the direct result in a named artifact. Herdr observes and sends steering only.
- Prefer a direct model name (`sonnet`, `sol`, `luna`, `deepseek-free`, `glm-free`) whenever you know what the work needs. Otherwise pass a tier, `route: "junior"`, `"senior"` or `"principal"`, or a craft pool, `"design"` or `"writing"`; add `rotate: true` to spread load, `council: N` for N runs across families, or `models: [...]` for an ad-hoc council. Old tier names (`bulk`, `scout`, `workhorse`, `strong`, `review`, `flagship`) are rejected with the new name. With several families available, run substantial and higher work as a [tiered council](references/tiered-council.md). `provenant route list` shows each pool with live availability, and `fabric_adapters` shows the current catalogue and health. An unknown model or unsupported effort runs with a recorded note or substitution when executable; see [routing-and-tiers.md](references/routing-and-tiers.md).
- Dispatch defaults:
  - Fan out 2-4 independent lanes; go wider only for independent, checkable slices.
  - Before fan-out, run `provenant route list`; dispatch by tier alias and spread work across that tier's available members. Set effort where the tool exposes it, otherwise record the limit.
  - Set route or model on every spawn, including native subagents; name Claude members directly (haiku junior, sonnet senior, opus principal) because Agent accepts no tier aliases. Brief native subagents with paths; end each when its deliverable lands.
  - Brief in five fields: goal, scope/paths, constraints, output shape and length, verification.
  - agy lanes are OS-confined to their `cwd` and `add_dirs`; pass briefs through `prompt` or `prompt_file` (read by Fabric), rather than paths for the lane to open.
  - Keep tool output bounded: output over a bound goes to a file, and the command returns a pointer and a short tail. Avoid unbounded `gh ... --limit 1000` and `rg --files` over run directories. A polling loop prints one line.
  - Wait once: run one `provenant lanes --wait N` in the background, or one status wait. Never sleep or poll loops.
  - Wait with `provenant lanes --wait N` or `gh pr checks --watch` as a background command; never a bare sleep or a manual polling loop.
  - No subagent or lane blocks in a foreground wait longer than about four minutes. Long waits run in the background (the harness notifies on completion) or belong to the chair. Cache lifetimes vary by provider (Anthropic's default is five minutes; others differ), so keep waits short and let receipts record cache reads.
  - Shape briefs for the cache: stable shared preamble and context pack first, task-specific content last. Resume the lane's session for follow-ups rather than dispatching fresh, and let sibling lanes share one context pack. Cached reads cost roughly 5 to 50 times less than fresh input, depending on provider.
  - Size lanes by context, not minutes: one logical unit and one commit per lane. The lane keeps a short progress notes file; near its context budget it stops, and the chair restarts it fresh from the brief, contract and notes rather than letting it work from a rotted context.
  - Research: when a decision rests on external facts (docs, papers, prices, prior art), dispatch research lanes with network access proactively, fanned out across juniors with a senior synthesis.
- A dispatched lane has no inbox. Peers message sessions, not lanes; a lane gets new instructions only through `resume` or a new dispatch.
- Prefer per-dispatch flags. Editing global provider configuration requires explicit authority.
- A chair running lanes across many wakes follows [chair-loop.md](references/chair-loop.md), including its reviewer git-verb denylist and decision council.
- Before landing, acquire a work claim if none is live for this session, verify
  it, and acquire `provenant fabric landing acquire` with the integration
  reference, expected remote OID, candidate OID and operation ID. See the
  [landing lease runbook](../../docs/runbooks/landing-lease.md). Verify pinned
  candidate and integration OIDs through Rust confinement before merging. Do
  not infer a protected-tip release from an advisory claim. Renew or release
  the landing lease using its generation/token proof and an operation ID.
  A project whose landing tool owns the lease leaves it to that tool.
- Keep full worker output in run files and return only the digest and path; brief native subagents to the [~150-word result digest](references/orchestration-contract.md#native-result-digest). For liveness, use `fabric_status`; see [worker-liveness.md](references/worker-liveness.md) for degraded runs. Size alone proves nothing.
- On a terminal result, record `adapter/model@effort` from the receipt; derive family from it. Native subagents record `claude/<model>@<effort> (anthropic; resolved)` from the Agent tool's model parameter, never from self-report.

## Adaptive loop

Preflight authority, isolation, disclosure and receipts. Dispatch independent waves, reduce their claims against objective evidence, and add only informative repair, verification or review waves. Parallel lanes stop ready to merge; the chair merges serially and reruns checks after the tree changes. Record missing review legs and substitutions. Objective checks and tier-required review meet delivery acceptance; the chair reports the outcome and the user may reject it. Explicit user acceptance is required only for `terminal` work or when the approved scope asks for it; a project may name its landing gate as acceptance. External actions retain their separate authority gates. For accepted ready issues, use [autonomous-implementation.md](references/autonomous-implementation.md); run-until-STOP missions use `autopilot`.

## Depth on demand

Use [orchestration-contract.md](references/orchestration-contract.md) for worker contracts, [routing-and-tiers.md](references/routing-and-tiers.md) for catalogue choices, [direct-cli-fallback.md](references/direct-cli-fallback.md) only when Fabric cannot express the task, and [codex-capabilities.md](references/codex-capabilities.md) for Codex capability probing. [lane-safety.md](references/lane-safety.md) for lane and reviewer failure modes and false greens. [trigger-boundary.md](references/trigger-boundary.md) for skill activation boundaries, [retrieval-and-tool-routing.md](references/retrieval-and-tool-routing.md) for source and tool selection, and [evaluation-and-observability.md](references/evaluation-and-observability.md) for checks and trace review. Other references in `references/` cover paired primary work, Herdr, verification, memory and domain adaptation. Direct CLI is a degraded path; record its route and result artifact.

Keep the criteria, evidence and outcome in the existing project document or PR.
