# Specifications

This directory is Provenant requirements doctrine, not a live work tracker or
globally applied cross-project harness policy. Current specifications use
semantic domain/topic paths. Each linked file is an independent normative
owner; directories exist only for discovery. Git owns content integrity and
history. Requirements and acceptance stay with the spec that owns the
behaviour. GitHub issues and their Project Status fields own current delivery
state, owner, dependencies and user gates.

The Agent Fabric and Console specifications went with the runtime they
described. Fabric is now small enough to be its own documentation: see
[`runtime/fabric/README.md`](../../runtime/fabric/README.md) and the
[MCP registration runbook](../runbooks/fabric-mcp-registration.md).

## Fabric

- [Fabric across the user's own hosts](fabric-hosts.md)
- [Host setup and diagnostics](../runbooks/fabric-hosts.md)
- [Landing lease from a repository script](../runbooks/landing-lease.md)

## Observability

- [Observability layer and native macOS console](observability-console.md)
- [Console and menu bar design](observability-console-design.md)

## Harness

- [Adaptive agent harness lifecycle](harness/lifecycle.md)
- [Progressive-disclosure refactor](harness/disclosure-refactor.md)
- [Configurable multi-provider execution core](harness/provider-execution-core.md)
- [Harness operating model: routing, cache, quota and telemetry](harness/operating-model.md)
- [Graph runner: contract-driven plan graphs (operating model R2)](harness/graph-runner.md)
- [Rust lane supervisor and a single Rust front door](harness/rust-supervisor.md)
- [Rust rewrite plan and rulebook](rust-port/README.md)
- [Harness profiles: cost and quality knobs](harness/profiles.md)
- [Switching and comparing profiles](../runbooks/harness-profiles.md)
- [Held-out replay bench for harness changes](harness/replay-bench.md)
- [Replay bench studies: model-effort frontier and fleet topology](harness/replay-bench-studies.md)
