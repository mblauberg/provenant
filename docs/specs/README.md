# Specifications

This directory is Provenant requirements doctrine, not a live work tracker or
globally applied cross-project harness policy. Current specifications use
semantic domain/topic paths. Each linked file is an independent normative
owner; directories exist only for discovery. Git owns content integrity and
history. Requirements and acceptance stay with the spec that owns the
behaviour. GitHub issues and their Project Status fields own current delivery
state, owner, dependencies and user gates.

The Agent Fabric and Console specifications went with the runtime they
described. The accepted Rust replacement is described in
[Fabric Rust design](rust-port/design/fabric.md) and the
[MCP registration runbook](../runbooks/fabric-mcp-registration.md). The old
Fabric v2 proposal is retained as [design history](../archive/specs/fabric-v2.md).

## Fabric

- [Fabric across the user's own hosts](fabric-hosts.md)
- [Host setup and diagnostics](../runbooks/fabric-hosts.md)
- [Landing lease from a repository script](../runbooks/landing-lease.md)

## Historical specifications

The following pre-rewrite specifications are retained for reference. Their
legacy banners identify them as historical, not current product contracts.

- [Observability layer and native macOS console](observability-console.md)
- [Console and menu bar design](observability-console-design.md)
- [Adaptive agent harness lifecycle](harness/lifecycle.md)
- [Progressive-disclosure refactor](harness/disclosure-refactor.md)
- [Configurable multi-provider execution core](harness/provider-execution-core.md)
- [Harness operating model: routing, cache, quota and telemetry](harness/operating-model.md)
- [Graph runner: contract-driven plan graphs](harness/graph-runner.md)
- [Harness profiles](harness/profiles.md)
- [Held-out replay bench](harness/replay-bench.md)
- [Replay bench studies](harness/replay-bench-studies.md)

## Observability

- Rust run supervision: [single Rust front door](harness/rust-supervisor.md)

## Harness

- [Rust rewrite plan and rulebook](rust-port/README.md)
- [Switching and comparing profiles](../runbooks/harness-profiles.md)
