# ADR 0005: One delivery kernel, separate from execution

**Status:** Accepted

Changed: 2026-10-26, #1754.

## Context

Duplicating lifecycle policy in coordination, dispatch and delivery produces
competing acceptance authorities. An execution result answers a different
question from a verified delivery receipt.

## Decision

The workflow crate owns the neutral delivery kernel and its canonical receipt.
Make objectively decidable risk floors, authority containment, evidence and
review coverage executable there. The chair and governing skills own
judgement, subject to the constitution's user gates.

Fabric owns coordination and delegates execution to the run owner. Neither
Fabric nor a provider attempt certifies delivery acceptance. Delivery evidence
may refer to execution evidence without replacing it.

Lifecycle evidence, provider launch authority and release evidence are distinct
trust boundaries. A record of one must never claim attestation of another;
local evidence must not claim an independent external authority.

## Consequences

There is one machine owner for delivery minima. Domain profiles may strengthen
those minima; execution and coordination add no parallel lifecycle ledger.
The former external daemon ledger and its receipt ports are retired. Current
module boundaries are mapped in the [architecture](../ARCHITECTURE.md).
