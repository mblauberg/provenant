---
name: orchestrate
description: "Use to coordinate bounded work across agents, research lanes or independent reviews. Use diagnose for coupled defects and autopilot for missions that continue until STOP. Not for a small, single-owner task."
---

# Multi-agent orchestration

## Quickstart

One surface per action. Fabric MCP tools cover submit, graph, status, resume, cancel, claim, landing leases and bounded stdout or stderr reads (`fabric_output`). The CLI covers the rest: `provenant lanes` (list and wait), `provenant route list`, `provenant output` (events, tail, result, prompt), `provenant reconcile` and `provenant fabric landing push|reconcile`. A terminal without MCP cancels with `provenant cancel`.

1. [Native first](references/routing-and-tiers.md#native-first): use native subagents for your own provider when the native tool supports the work. Claude callers use native Claude subagents; Claude models remain in the route pools for non-Claude chairs. Use Fabric for other providers, detached work and sandboxed worktree writers.
2. Call `fabric_submit` with its `submission` operation; each task has one route choice. Use `provenant route list --adapter <id>` to inspect model ids, pool membership and cached availability.
3. For a graph workflow, use `fabric_graph` with `op: run|status|cancel|resume`; pass the project-local graph document path to `run` and `resume`, and its `id` to `status` and `cancel`.
4. For writers, pass `access: "worktree_write"` and the registered `worktree`
   as an absolute path or relative to the selected repository's primary root,
   named per `setup-repo`'s branch naming doctrine; the task id defaults to that
   same branch-derived string. Before submission, [confirm the connection's
   project](references/fabric-identity.md), then acquire and verify an advisory
   `fabric_claim` `acquire` (`scope` names the issue or path set, with an
   `operation_id` and `lease_ms`); keep its generation/token proof, renew it while work
   continues and release it when finished. Do not submit against a conflicting live claim; submission does not
   enforce claims. One supervisor writer lease excludes a second writer;
   terminal Git sequencer state fails the lane. If custody is lost, keep the
   worktree reserved and follow [chair recovery](../../docs/worktrees.md#writer-custody-recovery).
5. After submission, keep one `provenant lanes --wait N` running in the background. When it exits, read the printed lanes, act, and re-arm it ([chair-loop.md](references/chair-loop.md#waiting) covers `--all`). A submitted sub-agent blocks in the foreground ([worker-liveness.md](references/worker-liveness.md)).
6. Use `provenant lanes` as the source of truth after compaction or restart; inspect a terminal row with `fabric_run`'s `status` operation. Read retained events with `provenant output RUN/TASK/ATTEMPT --part events` and its byte offset.
7. For a question, call `fabric_run` with its `resume` operation and the answer in `prompt`, targeting `target: { attempt: ATTEMPT }` and providing `expected_generation` and `operation_id`.
8. Use `fabric_run`'s `cancel` operation with an explicit target and operation ID to stop a run; use `fabric_output`'s `read` operation with a target, part, offset and byte limit for a bounded slice.

## Rules

- A rejected batch task returns its `error` and `fix`; other tasks in the same Rust Fabric submission may still run. Claude callers manage native Claude subagents outside the Fabric task list.
- Use parallel agents only when the tasks are independent, have stable interfaces and checkable outputs, and save more attention than coordination costs. Keep coupled work serial.
- Partition writers into separate registered worktrees, and name each writer lane's write scope (files or directories) before fan-out. Lanes with overlapping write scopes run serially, and dependent lanes merge in dependency order. The chair owns authority, decisions, user communication and the final synthesis. A worker's report is a claim: verify commits, counts and tests against the live tree and transcript.
- Use the [HARNESS.md](../../HARNESS.md) risk ladder for review. It also governs provider choice and claim/ack; do not duplicate those policies here.
- Send answer-bearing external work through Fabric request/reply when available. If unavailable, mark `FABRIC-ROUNDTRIP-UNAVAILABLE` and keep the direct result in a named artifact. Herdr observes and sends steering only.
- Choose one exact model or named route for each Rust Fabric task. The bundled `utility`, `junior`, `senior`, `principal`, `writing` and `design` pools are ordered candidate lists; repeated use does not rotate members. Rust Fabric has no rotation, council-size, model-list, weight or synonym selector. For a [tiered council](references/tiered-council.md), submit separate tasks for distinct exact models and synthesise their results in the chair. The CLI's explicit model fallbacks are ordered recovery candidates, not council seats. `task_class` selects configured knob presets only; it does not alias a route. `provenant route list` shows pool members and cached availability. An unknown exact model remains unverified until current provider observation, activation, disclosure and confinement allow it; an unsupported effort is refused.
- Dispatch defaults:
  - Fan out 2-4 independent lanes; go wider only for independent, checkable slices.
  - Before fan-out, run `provenant route list`. A named pool tries members in order and does not spread repeated tasks across them. For family diversity, select distinct exact model IDs from the listing and submit one task per model. Set effort only when the catalogue records that exact control.
  - Set the exact model for each native subagent when its tool allows it. Claude callers use native Claude subagents; non-Claude chairs can dispatch Claude pool members through Fabric. Brief native subagents with paths; end each when its deliverable lands.
  - Brief in five fields: goal, scope/paths, constraints, output shape and length, verification.
  - agy lanes are OS-confined to their `cwd` and `add_dirs`; pass briefs through `prompt` or `prompt_file` (read by Fabric), rather than paths for the lane to open.
  - Keep tool output bounded: output over a bound goes to a file, and the command returns a pointer and a short tail. Avoid unbounded `gh ... --limit 1000` and `rg --files` over run directories. A polling loop prints one line.
  - Wait once: run one `provenant lanes --wait N` in the background, or one status wait. Never sleep or poll loops.
  - Wait with `provenant lanes --wait N` or `gh pr checks --watch` as a background command; never a bare sleep or a manual polling loop.
  - No subagent or lane blocks in a foreground wait longer than about four minutes. Long waits run in the background (the harness notifies on completion) or belong to the chair. Cache lifetimes vary by provider (Anthropic's default is five minutes; others differ), so keep waits short and let receipts record cache reads.
  - Shape briefs for the cache: stable shared preamble and context pack first, task-specific content last. Resume the lane's session for follow-ups rather than dispatching fresh, and let sibling lanes share one context pack. Cached reads cost roughly 5 to 50 times less than fresh input, depending on provider.
  - Size lanes by context, not minutes: one logical unit and one commit per lane. The lane keeps a short progress notes file; near its context budget it stops, and the chair restarts it fresh from the brief, contract and notes rather than letting it work from a rotted context.
  - Research: when a decision rests on external facts (docs, papers, prices, prior art), dispatch research lanes with network access proactively, fanned out across juniors with a senior synthesis.
- Every enabled lane, including junior lanes, receives Provenant's generated Fabric MCP configuration and supported hooks. Nested MCP submission admits child records within `lanes.max_depth` and attempts the shared launch path. Live nested provider launch and utility-model execution remain unverified and subject to provider activation gates; do not assume submission means execution. Dispatch sets `PROVENANT_LANE_DEPTH` and records the parent attempt; `lanes.max_depth` defaults to 2 and refuses deeper nesting. User MCP servers and personal hooks are excluded.
- A dispatched lane has no inbox. Peers message sessions, not lanes; a lane gets new instructions only through `resume` or a new dispatch.
- Prefer per-dispatch flags. Editing global provider configuration requires explicit authority.
- A chair running lanes across many wakes follows [chair-loop.md](references/chair-loop.md), including its reviewer git-verb denylist and decision council.
- Before landing, acquire a work claim if none is live for this session, verify
  it, and call `fabric_landing` `acquire` with the integration
  reference, expected remote OID, candidate OID, `lease_ms` and operation ID. See the
  [landing lease runbook](../../docs/runbooks/landing-lease.md). Verify pinned
  candidate and integration OIDs through Rust confinement before merging. Do
  not infer a protected-tip release from an advisory claim. Renew or release
  the lease with `fabric_landing` and its generation/token proof and an operation ID.
  A project whose landing tool owns the lease leaves it to that tool.
- Keep full worker output in run files and return only the digest and path; brief native subagents to the [~150-word result digest](references/orchestration-contract.md#native-result-digest). For liveness, use `fabric_run`'s `status` operation; see [worker-liveness.md](references/worker-liveness.md) for degraded runs. Size alone proves nothing.
- On a terminal result, record `adapter/model@effort` from the receipt; derive family from it. Native subagents record `claude/<model>@<effort> (anthropic; resolved)` from the Agent tool's model parameter, never from self-report.

## Adaptive loop

Preflight authority, isolation, disclosure and receipts. Dispatch independent waves, reduce their claims against objective evidence, and add only informative repair, verification or review waves. Parallel lanes stop ready to merge; the chair merges serially and reruns checks after the tree changes. Record missing review legs and substitutions. Objective checks and tier-required review meet delivery acceptance; the chair reports the outcome and the user may reject it. Explicit user acceptance is required only for `terminal` work or when the approved scope asks for it; a project may name its landing gate as acceptance. External actions retain their separate authority gates. For accepted ready issues, use [autonomous-implementation.md](references/autonomous-implementation.md); run-until-STOP missions use `autopilot`.

## Depth on demand

Use [orchestration-contract.md](references/orchestration-contract.md) for worker contracts, [routing-and-tiers.md](references/routing-and-tiers.md) for route choices, and [lane-safety.md](references/lane-safety.md) for lane and reviewer safeguards. For non-code work, read [domain adaptation](references/domain-adaptation.md); for scratch handling, read [memory and scratch](references/memory-scratchpad.md). Other references cover provider pairing, Herdr, verification, and evaluation.

Keep the criteria, evidence and outcome in the existing project document or PR.

Reference eval cases (not run by CI): [lifecycle](evals/lifecycle_cases.yaml),
[contracts](evals/contract_cases.yaml), [discipline](evals/discipline_cases.yaml),
and [topology value](evals/topology_value_cases.yaml).
