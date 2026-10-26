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

Fabric MCP exposes `fabric_run` (`op: submit`) for one or more ordinary
configured-provider tasks and `fabric_graph` for checkpointed graph workflows. The typed catalogue
and current request contracts live in the [Rust Fabric design](../specs/fabric.md#cli-and-mcp-surfaces).
Submission uses `op: submit`, a `tasks` list, `concurrency` and an
`operation_id`. Route preferences are carried per task. Writers declare
`access: worktree_write` and a verified registered `worktree`; read-only tasks
use `access: read_only`. The run owner enforces access before provider launch.

Responses contain compact status, actual route when known and absolute artifact
paths. Prompts, results and diagnostics remain in the existing run files. A
caller may return immediately or wait for at most 55 seconds in one MCP call.
Closing the transport asks any still-active owner started by that process to
terminate; later inspection, retry and cancellation use the existing run
controls.

Fabric does not gain a provider adapter, scheduler, daemon, session database,
transcript copy, fallback policy, delivery receipt, universal hashes or
model-family permission gate. Direct CLI execution remains supported. Persistent
provider sessions and richer task correlation are separate work items; the
amendment below narrows the session-database exclusion for named sessions only.

## Consequences

Agents get one low-friction MCP surface for coordination and ordinary fan-out,
including cheap batches, while the orchestration scripts remain the sole owners
of route resolution, provider processes, attempts and batch evidence. The
façade adds no maintenance service or parallel lifecycle state.

## Continuation and batches

Continuation targets an explicit attempt through `fabric_run` with `op: resume`,
`operation_id`, `prompt` and `expected_generation`. Named sessions are retired.
Finite batches use `fabric_run` with `op: submit` and `tasks`.

## Amendment: plan graphs

**Accepted 2026-10-22** (graph runner spec, [PR
#1099](https://github.com/mblauberg/provenant/pull/1099); slice 1).

`fabric_graph` selects `op: run|status|cancel|resume`. Run and resume take a
project-local JSON document path. Status and cancel take its `id`. The CLI uses
`provenant fabric graph run|status|cancel|resume`. Calls advance bounded work
under a checkpoint lock and return typed graph state. They do not start a daemon.

Each node declares `access: read_only|worktree_write`; writers name a registered
worktree by absolute path or relative to the selected repository's primary root.
The execution owner verifies and confines it before launch.
Auth refusals, usage limits and adapter unavailability may advance routes before
provider launch. After launch, only read-only nodes may fall back. Writer
failures preserve the worktree for the chair. Unknown launch evidence is treated
as launched. Each attempt uses the configured graph timeout.

Cancel persists an admission fence and asks the run owner to stop accepted
attempts. Resume checks the document digest and observes the same attempt keys.
Run mutations remain unavailable until the dispatch/control owner is bound;
status and the admission fence remain usable through every MCP harness.

Two mechanisms are new. A writer node's linked worktree is reserved for the
graph in the store and by a file in its Git directory, which `dispatch_run.py`
honours, so a hand dispatch into it is refused while the graph runs. A node may
run trusted fast gates: allowlisted argv run in the node's tree with a minimal
environment, launched as recorded process groups behind a handshake so that no
gate runs unrecorded and cancel terminates the whole group before the
reservation is released. Gates are not confinement; they have the user's
filesystem and network reach.
