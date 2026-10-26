# ADR 0020: Daemonless project coordination in SQLite

**Status:** Accepted

Changed: 2026-10-26, #1754.

## Context

The former daemon made ordinary coordination depend on provisioning, expiring
capabilities and protocol handshakes. On one user's machine, same-UID agents
could read each other's capability files; the ceremony supplied little
isolation while obstructing normal use.

## Decision

Fabric is a project-scoped SQLite coordination store with CLI and MCP surfaces,
not a provider daemon. The fabric-store crate owns mail, tasks and events;
fabric-runtime owns identity and host coordination. SQLite serialises writes.

Identity derives from the working directory. Registered worktrees share their
primary checkout's project identity; other Git or non-Git roots stand alone.
The agent label can split inboxes. Delivery claims may expire and redeliver an
unacknowledged message; a claim is neither provider liveness nor a provider
session lease.

Provider execution belongs to run, providers and supervisor. Fabric's thin MCP
surface delegates to those owners under
[ADR 0022](0022-thin-fabric-mcp-execution-facade.md). Delivery acceptance belongs
to workflow, separately from execution evidence. There is no daemon capability
compiler, review-portal custody protocol or persistent provider session pool.

## Consequences

Coordination works without provisioning a second trust system. Same-user
coordination does not promise multi-tenant isolation. Cross-host federation
uses the explicit ownership boundary in
[ADR 0026](0026-federate-fabric-across-own-hosts-over-ssh.md), not a shared
network-mounted database.
