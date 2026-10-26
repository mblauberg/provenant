# ADR 0026: Federate the user's own hosts over SSH

**Status:** Accepted

Changed: 2026-10-26, #1754.

## Context

Several machines provide useful execution capacity but may sleep or lose
connectivity. Files, SQLite stores and process identity have natural local
owners; sharing a database would make locking and liveness depend on the
network.

## Decision

Each host owns its runs, output, coordination rows and processes. Reach it
through a bounded peer entry point over the user's existing OpenSSH
configuration. Host-qualified identifiers prevent ambiguous writes; only the
owning host judges process liveness.

Views query reachable hosts and mark stale observations with their age.
Mutations route to the owner with operation IDs for idempotent retry. A lost
dispatch response is launch-unknown and is reconciled by operation ID, never
retried locally as a second writer. Explicit host selection never falls back;
default placement may fall back only before sending the request.

Each project names one home host for fenced shared claims and landing leases.
Exclusion must not depend on two stores or clocks agreeing. Message transfer
retains the creation ID and clears pending outbound state only after confirmed
import. Git objects move directly over SSH; writer results bind base and head.

The versioned peer protocol accepts typed requests and fixed operations, never
a caller-built shell command. Each host authenticates its own providers. The
executing host's confinement and protected paths still apply. Federation is
provider-neutral and exposed through the same CLI and MCP owners.

## Consequences

No shared-network SQLite, replicated store, new listener or central workflow
service is introduced. Remote views cost bounded SSH round trips, and home-host
claims are unavailable while that host cannot be reached. The
[Fabric design](../specs/fabric.md) owns current protocol details.
