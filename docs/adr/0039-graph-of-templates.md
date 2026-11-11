# ADR 0039: One graph of template nodes

**Status:** Accepted

Changed: 2026-11-06, #2867.

## Context

Multi-step work needs one recovery and mutation owner. Templates make workflow
shapes composable without adding a separate runtime for each shape; a plain
worker is the same execution mechanism as a node in a larger graph.

## Decision

Keep the graph kernel, its checkpoint-before-launch discipline and the compiled
`GraphNode` as an internal form. Expose four concepts above it.

- **Attempt:** one provider process under custody. Retries and continuations are
  new attempts with lineage.
- **Template:** TOML with parameters and either a leaf (brief, tier, access,
  outputs) or `nodes`. A composite is a
  template with nodes; a dotted name is a flavour (`review.security`). Layers are
  bundled, user, repository and inline, and the same schema serves all of them.
- **Node:** `use` plus overrides (`brief`, `tier`, `after`, `inputs`, `over`,
  `min_ok`, `repeat`, work binding). Its executor is an agent, a deterministic
  action (gate, decide, land) or a composite. Plan and synthesise are briefs, not
  runtime kinds. `over` copies a node per named item with `${item}`, so a swarm
  is one node over named areas.
- **Graph:** a revisioned collection of nodes, changed only by patches.

```toml
# .provenant/templates/map-and-plan.toml
nodes = [
  { id = "map", use = "explore", over = ["crates/run", "crates/fabric-runtime"] },
  { id = "slices", use = "plan", after = ["map"] },
]
```

A dynamic list comes from a patch or from a resolver named `@name` at launch:
`over = "@review-panel"` selects the resolved panel, with the host choosing lenses from
risk tier and tags.

**One mutation path.** `admit_patch(graph, parent, patch)` is called by submission,
an agent's final `patch` block and a live call from a lane. A patch holds nodes,
inline templates and optional `skip` or `replace` of unstarted nodes. It commits
atomically against an expected revision and an idempotency key, and rejects cycles
and conflicting edits, scans for secrets and journals the change. Started attempts
are immutable. Admission applies the configured graph limits.
"Planner" is only `use = "plan"`: any agent may emit any template, loop, worktree
or further planner, so replanning after a wave is ordinary.

**Children.** A lane runs `provenant run` or `fabric_run`; the call crosses
`crates/fabric-mcp/src/lane.rs#HostRelay` and the host writes the journal. Children
join the host's graph, sharing its budget, cancellation and trace, and may be
writers integrating into the parent's branch. Spawning is recorded separately
from completion dependencies, so a parent can yield its slot while it waits. A
composite completes when its dynamic producers have closed and its join condition
holds. Cancellation follows the subtree unless a child was explicitly detached.

**Peer placement.** `tools/front-door/src/fabric/peer.rs#peer_submit_graph` gives
an authenticated submitted attempt its own graph on the receiving host, using
the ordinary scheduler and checkpoint-before-launch discipline. Its local relay
serves child launches, graph edits and slot yields. Children default to that host;
an explicit host uses ordinary placement, so any agent can launch further nodes
on any configured reachable host. A submitted operation replays into the same
receiver graph rather than launching another parent.

The origin retains one node for the remote attempt. The peer returns a
host-and-graph pointer with the original graph/node binding and bounded pages of
redacted subtree metadata in status. `tools/front-door/src/fabric/unified.rs#enrich_status_rows`
exposes that pointer; `tools/front-door/src/fabric/unified.rs#unified_graph_output`
follows those pages' attempt references for graph and node output. Explicit
attempt output remains the ordinary byte reader. Cancelling the origin node
sends the existing peer cancellation request; the receiver fences and stops its
subtree before replying. All communication uses forward SSH requests from the
origin. A collected terminal peer checkpoint is reported explicitly, so the
origin can retain its own status row while the attempt record remains readable.
No reverse connection or replicated journal is required. Budgets and
recovery are owned by each host's graph; this boundary does not provide a shared
cross-host spending ledger.

A child's sandbox roots and capabilities need not be a subset of its parent's:
the host admits the child through ordinary launch policy, rather than inheriting
the parent process's sandbox. This does not widen the user-approved authority
envelope. The chair must keep delegated work within that envelope, as
[ADR 0021](0021-configured-workspace-dispatch-boundaries.md) requires. Credential,
state and protected-root checks apply independently at every launch. The admission
owner is [`relay.rs#admit_child_bound`](../../tools/front-door/crates/fabric-runtime/src/graph/relay.rs).

**A plain lane** is a one-node patch using `task`, with `AttemptSpec`
carrying placement, account and write roots.

**Prompts are content-addressed.** A checkpoint stores digests into a store beside
the journal, collected with its graph, so graph size does not grow with brief size
times node count. Prompts are scanned for secrets before they persist.

## Node library

Bundled templates are owned by
[`assets/templates/`](../../tools/front-door/crates/fabric-runtime/assets/templates/).
The [template runbook](../runbooks/routing-and-templates.md#templates) owns composition and use; template
names, tiers and briefs are not a separate architectural catalogue.

## Trace and recovery

The graph journal retains bounded, redacted identifiers, digests and cause codes
for mutations, attempts and control decisions. The checkpoint is the authority
for recovery; the event trace explains it rather than replacing it. Checkpoint
before launching, and bind replay to stable operation and node identities so a
restart cannot launch a completed effect twice. Recovery reconciles uncertain
launches, reuses finished outputs and resumes only unfinished work.

Landing accepts independent review evidence bound to the exact subject it merges. Run settlement
advances pending graphs without a second scheduler in the CLI or MCP façade
([ADR 0022](0022-thin-fabric-mcp-execution-facade.md)).

## Interface

The [generated command reference](../reference/cli.md) owns commands and flags.
CLI and MCP call the same graph service. Inside a lane, submission adds child
work through the host relay; it does not create a private child store.

## Consequences

Checkpoint recovery around external effects is the critical boundary. Waiting
parents yield capacity so child completion does not depend on a slot the parent
holds. Each host owns its budget and recovery; peer placement does not imply a
cross-host spending ledger.

Checkpoint formats must remain readable across an upgrade or be drained first.
State readability breaks are declared by a compiled-in state format version
compared during staged preflight between the installed and target release;
routine upgrades without state format breaks warn on in-flight work and proceed,
while broken formats block until drained or overridden with `--yes`.
Unreadable records are preserved and reported. Event sourcing with a projected
checkpoint is rejected: it would replace a working recovery authority without
solving a present need.

## Amendment: review and repair are a kernel-owned domain

Review and repair belong to the kernel's domain, not to a template or to the
agent's prompt. The kernel owns panel resolution, the repair loop and the
records they persist. A change to a persisted shape in this domain bumps
`STATE_FORMAT_VERSION` by default; an additive change that an older reader
safely ignores may leave it unchanged only when the change says why. The
ceilings on this domain become defaults a chair can override: panel family
diversity is advisory unless the chair asks for strict, and `max_repair_cycles`
defaults to three with no ceiling. Within the default budget the third review
still asks the principal for design judgement; a budget the chair raises runs
until it is used up and then reaches the principal. `PanelResolution` gains
`diversity` and `warnings` without a version bump: both are serde-defaulted and
the record does not deny unknown fields, so an older reader ignores them.
Separately, outside the kernel, the CI check of skill-negative coverage reports
gaps instead of failing. Part of #3078 (wave 1, #3135).
