# Specifications

This directory is Provenant requirements doctrine, not a live work tracker or
globally applied cross-project harness policy. Current specifications use
semantic domain/topic paths. Each linked file is an independent normative
owner; directories exist only for discovery. Git owns content integrity and
history. Requirements and acceptance stay with the spec that owns the
behaviour. GitHub issues and their Project Status fields own current delivery
state, owner, dependencies and user gates.

The Fabric contract is the [Fabric Rust design](rust-port/design/fabric.md);
registration is in the [MCP registration runbook](../runbooks/fabric-mcp-registration.md).
The old Fabric v2 proposal is retained as [design history](../archive/specs/fabric-v2.md).

## Fabric

- [Fabric across the user's own hosts](fabric-hosts.md)
- [Host setup and diagnostics](../runbooks/fabric-hosts.md)
- [Landing lease from a repository script](../runbooks/landing-lease.md)

## Harness

- [Harness profiles](harness/profiles.md)
- [Adaptive agent harness lifecycle](harness/lifecycle.md)
- [Held-out replay bench](harness/replay-bench.md)
- [Rust run supervision: single Rust front door](harness/rust-supervisor.md)
- [Rust rewrite plan and rulebook](rust-port/README.md)
- [Switching and comparing profiles](../runbooks/harness-profiles.md)
