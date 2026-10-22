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
`dispatch_run.py` or `batch_run.py`. The routing surface is `adapter`, `alias`
and `mode`, with `worktree` when the mode is `worktree_write`; the schemas are
strict, so an assurance selector is a typed input error rather than a silently
ignored one. The default route is the current provider seat, the `senior`
alias, the `worker` role and `read_only` access. Same-family and mixed-family
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
provider sessions and richer task correlation are separate work items; the
amendment below narrows the session-database exclusion for named sessions only.

## Consequences

Agents get one low-friction MCP surface for coordination and ordinary fan-out,
including cheap batches, while the orchestration scripts remain the sole owners
of route resolution, provider processes, attempts and batch evidence. The
façade adds no maintenance service or parallel lifecycle state.

## Amendment: named sessions

**Accepted 2026-09-30** (issue [#726](https://github.com/mblauberg/provenant/issues/726)).

Cross-agent continuation needs one project-shared mapping from a name to a
provider session, and the existing Fabric SQLite store is already the
project-shared state. It gains one `sessions` table holding only the alias: a
case-sensitive name per project, the actual adapter, the provider-native session
ID, the run, task, attempt and result path of the last clean turn, and the
latest turn's run, attempt, status and launching process ID. It holds no
prompts, results, transcripts or provider processes.

A turn is an ordinary `fabric_dispatch`: a fresh dispatch for a new name, the
existing `resume` path for a known one, or `handoff` when the caller passes
`fresh: true`, each continuing from the alias's last clean attempt. The
dispatch and run owners keep sole ownership of processes, attempts,
cancellation and result files. Only a clean turn (`ok` or `input_required`)
moves the alias, to the attempt that ended the owner invocation. One turn per
name is active; a concurrent caller gets `session_busy`. A turn records its run
before the owner starts, and launches nothing if it cannot. Busy follows the
run's lifecycle: the turn is active while its launching process lives, then
while its run's task is open or the run's owner lives. The run status records
the launcher and the owner it started, so readers close an attempt whose
launcher died before starting an owner, and keep open one whose owner lives
without an owner record. This is reconciled whenever the name is read; there is no lease timer, heartbeat,
queue, expiry or cleanup daemon. A provider without native continuation, or one
that no longer has the session, reports `continuation_unsupported` rather than
falling back to a new session.
`fabric_session` inspects, lists and forgets names; forgetting deletes only the
alias once no turn is active.

## Amendment: dispatch batches

**Accepted 2026-10-21.**

The dedicated `fabric_batch` MCP registration and its `FABRIC_LEGACY_TOOLS`
opt-in are retired. Callers use `fabric_dispatch` with `tasks[]` for finite
batches; the existing batch owner still executes those tasks.

## Amendment: plan graphs

**Accepted 2026-10-22** (graph runner spec, [PR
#1099](https://github.com/mblauberg/provenant/pull/1099); slice 1).

The execution tools are now `fabric_dispatch` and `fabric_graph`.
`fabric_graph` (`action: run|status|cancel|resume`) takes one validated JSON
plan graph whose nodes are ordinary dispatches, and returns at once with a
digest; `status` waits for at most 55 seconds and restarts an owner that died.
The CLI equivalent is `provenant graph`.

Fabric gains a scheduler only in this bounded form: one detached owner process
per running graph, started by the call and ending with the graph. It holds no
authority of its own and calls no model. Each node is a `fabric_dispatch` made
with the creating seat's identity, so the workspace boundaries, capability
rules and own-seat guard apply unchanged, and `dispatch_run.py` remains the
sole owner of provider processes, attempts and results. The graph state lives
in four tables in the existing Fabric store; a node's receipt references its
dispatch receipt by run and task id and copies no provider output. There is
still no daemon: a dead owner is detected on read and restarted by `resume`,
and recovery adopts the lanes the registry still shows.
`provenant graph resume ID --force` audits the caller and replaces stale owner identity under a process lock, retaining lane and gate fences until recovery proves them stopped.

Two mechanisms are new. A writer node's linked worktree is reserved for the
graph in the store and by a file in its Git directory, which `dispatch_run.py`
honours, so a hand dispatch into it is refused while the graph runs. A node may
run trusted fast gates: allowlisted argv run in the node's tree with a minimal
environment, launched as recorded process groups behind a handshake so that no
gate runs unrecorded and cancel terminates the whole group before the
reservation is released. Gates are not confinement; they have the user's
filesystem and network reach.
