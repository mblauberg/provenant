# ADR 0039: One graph of template nodes

**Status:** Accepted

## Context

Multi-step work needs one recovery and mutation owner. Templates compose workflow shapes without a separate runtime for each shape.

## Decision

Use one graph kernel for attempts, templates, nodes and graphs. A plain lane is a one-node graph; composites are data. All graph edits use one revisioned mutation path, and launched attempts remain immutable. Checkpoint before external launch so recovery can reconcile uncertain effects instead of duplicating them. The checkpoint is recovery authority; the trace explains it.

The kernel owns review and repair, and one declared policy decides what follows a stop. Conditions on nodes allow a template to express alternatives without maintaining parallel workflow shapes. Children use the host's graph service and remain within the user's authority envelope. Hosts retain their own recovery and budget authority.

Observe external facts, commit durable facts, derive projections and attention, then take typed action. An observation such as process presence or an artefact alone does not establish completion or authority.

See the [Fabric spec](../specs/fabric.md#bounded-graphs-and-route-fallback), [stop policy](../specs/run.md#what-follows-a-stop), and [template runbook](../runbooks/routing-and-templates.md#templates).

## Interface

The CLI and lane relay use the same graph service; command syntax belongs to `provenant help`.

## Consequences

One mutation and recovery model serves both small and composed work. Event sourcing with a projected checkpoint was rejected because replacing the recovery authority adds complexity without making recovery easier.
