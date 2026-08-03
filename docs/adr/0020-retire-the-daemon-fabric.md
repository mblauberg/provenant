# ADR 0020 — Retire the daemon fabric for a daemonless SQLite bus

**Status:** Accepted 2026-08-02 (user); supersedes [ADR
0002](0002-capability-compiled-execution-authority.md) and [ADR
0018](0018-accept-portal-stdio-v1-launch-custody.md)

## Context

The coordination runtime had grown to roughly 350,000 lines across five
packages: a daemon, a wire protocol, an operator console, a Herdr adapter and a
Rust review-portal supervisor. Between a fresh directory and its first message
stood eighteen preconditions — workspace trust, seat provisioning, generation
and lease validity, capability compilation, socket liveness, protocol
handshake, adapter activation and the rest.

The reported symptom was not slowness. It was that agents could not use it. In
ordinary personal projects the first call failed, and the failure was a
governance refusal rather than a bug, so retrying never helped.

The trust boundary that justified the daemon does not exist on a single-user
machine. Every agent runs as the same uid and can read every other agent's
capability file, so a capability token can only inconvenience an honest caller.
Once that is accepted, the daemon's remaining technical job is serialising
writes, and SQLite in WAL mode already does that: measured here at sixteen
concurrent processes and 3,200 messages with none lost and a 657 ms contention
window.

## Decision

Delete the five packages. `runtime/fabric` replaces them: one SQLite file, nine
MCP tools, a shell CLI, and identity derived from the working directory rather
than issued to the caller.

Nothing is trusted, bootstrapped, provisioned, renewed or leased. An agent is
`(project, agent_id)`, where the project is the git toplevel of the working
directory or the directory itself, and the agent id comes from the environment
the provider already sets. Nothing expires, so nothing needs renewing.

Provider execution leaves Fabric entirely. There are no in-process adapters:
cross-provider work is dispatched as a direct command-line call, and Fabric
carries the messages, shared tasks and activity log around it. The doctrine
that direct CLIs were a degraded fallback is reversed, because the path they
were a fallback to no longer exists.

## Consequences

The capability-compiled write profiles of ADR 0002 and the `portal-stdio-v1`
launch custody of ADR 0018 have no runtime left to enforce them. Both are
superseded rather than amended: the boundary they protected was the daemon's,
and the daemon is gone.

Provider sessions are no longer held open between calls. Skills already
dispatch `codex exec`, `agy` and `cursor-agent` per lane, so nothing that was
working loses a capability.

The cost is that anything wanting genuine multi-tenant isolation would have to
reintroduce a mediating process. That is accepted deliberately: this is a
personal harness on one machine, and an attacker already able to read another
agent's files has won regardless.

The old tree is preserved on the `legacy/agent-fabric` branch.
