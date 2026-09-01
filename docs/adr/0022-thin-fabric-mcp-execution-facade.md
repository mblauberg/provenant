# ADR 0022 — Thin Fabric MCP execution façade

**Status:** Accepted 2026-09-01 (issue [#725](https://github.com/mblauberg/provenant/issues/725))

**Amends:** [ADR 0013](0013-thin-provenant-cli.md), [ADR
0020](0020-retire-the-daemon-fabric.md) and [ADR
0021](0021-configured-workspace-dispatch-boundaries.md).

## Context

Agents already use Fabric MCP for project coordination, while ordinary provider
work requires a separate command-line invocation. That split adds everyday
friction and discourages use of the existing reliable dispatch and batch
owners. Moving their mechanics or receipts into Fabric would instead duplicate
runtime ownership.

## Decision

Fabric MCP exposes exactly two execution tools:

- `fabric_dispatch` starts one ordinary configured-provider task; and
- `fabric_batch` starts a fixed batch of 1–64 tasks with concurrency capped at
  eight.

Both create a run directory automatically and delegate unchanged to
`dispatch_run.py` or `batch_run.py`. The default route is the current provider
seat, the `workhorse` alias and the `worker` role. Same-family and mixed-family
ordinary work are allowed; independence remains a separate assurance claim.

Responses contain compact status, actual route when known and absolute artifact
paths. Prompts, results and diagnostics remain in the existing run files. A
caller may return immediately or wait for at most 55 seconds in one MCP call.
Closing the transport asks any still-active owner started by that process to
terminate; later inspection, retry and cancellation use the existing run
controls.

Fabric does not gain a provider adapter, scheduler, daemon, session database,
transcript copy, fallback policy, delivery receipt, universal hashes or
model-family permission gate. Direct CLI execution remains supported. Persistent
provider sessions and richer task correlation are separate work items.

## Consequences

Agents get one low-friction MCP surface for coordination and ordinary fan-out,
including cheap batches, while the orchestration scripts remain the sole owners
of route resolution, provider processes, attempts and batch evidence. The
façade adds no maintenance service or parallel lifecycle state.
