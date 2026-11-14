# ADR 0026: Federate the user's own hosts over SSH

**Status:** Accepted

## Context

Hosts can sleep or lose connectivity. Files, databases and process identity have local owners, so shared network storage would make custody and liveness depend on the network.

## Decision

Each host owns its runs, output and processes; the project home host owns shared coordination. Federate the user's hosts over authenticated SSH, with typed operations routed to the owner. Only an owning host judges its processes. Explicit host selection does not silently fall back, and an uncertain dispatch must be reconciled before another writer is launched. Credentials remain local.

The [Fabric spec](../specs/fabric.md#peer-hosts) owns the protocol and failure contracts.

## Consequences

Shared claims are unavailable when the home host is unreachable. Remote views incur network round trips, but no shared database or central listener is needed.
