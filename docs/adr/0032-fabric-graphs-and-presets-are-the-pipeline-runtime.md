# ADR 0032: Fabric graphs and presets are the pipeline runtime

**Status:** Accepted

Changed: 2026-10-30, #2002.

## Context

Multi-agent work (scouting, fan-out, review, repair, landing) was assembled by
hand from separate runs. Each chair re-invented sequencing, fallback and
restart behaviour, and a crash could launch a step twice or lose its place.

## Decision

A bounded Fabric graph is the one runtime for multi-step agent work. Nodes are
ordinary run attempts, command gates, decide nodes, plan expansions and land
nodes. A preset is not a second engine: it is data (stages, roles, typed
parameters) that lowers through one path into an ordinary graph, which the graph
owner submits. `--explain` shows the lowered graph.

The owner checkpoints each node under a stable key before it calls run, so
replay returns the existing attempt and a restart never launches a node twice.
Host configuration sets graph limits and budgets; a graph document cannot raise
them. Run settlement advances pending graphs, and `graph resume` is only manual
recovery. Tiers resolve through routing pools
([ADR 0033](0033-weighted-route-pools-are-the-routing-unit.md)).

Graph delivery and landing keep the authority gates: a land node accepts review
evidence only from an agent attempt outside the writer's chain, bound to the
head it merges.

## Consequences

New pipeline shapes are preset or role data, not new code paths. Checkpoints are
versioned, so graphs in flight across a release that changes them must finish
first. The [Fabric spec](../specs/fabric.md#bounded-graphs-and-route-fallback)
owns node semantics; the [preset runbook](../runbooks/presets.md) owns use.
Graph MCP calls stay a thin façade per
[ADR 0022](0022-thin-fabric-mcp-execution-facade.md).
