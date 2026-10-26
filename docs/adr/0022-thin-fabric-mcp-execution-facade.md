# ADR 0022: Thin Fabric MCP execution façade

**Status:** Accepted

Changed: 2026-10-26, #1754.

## Context

Agents already coordinate through MCP. Requiring a separate provider control
plane for fan-out creates friction; moving execution into Fabric would create
a competing run owner.

## Decision

Fabric MCP is a thin typed façade over the existing execution owners. Submit,
inspect, resume and cancel calls delegate to run; graph calls advance bounded,
checkpointed workflows through the same execution stack. The Fabric MCP crate
owns transport and request decoding, not provider launch mechanics.

Return bounded status and paths to run-owned output. Full prompts, results and
diagnostics stay in run files. Transport waits are bounded; later inspection
and control use retained run identity. A transport disconnect alone does not
authorise signalling an unrelated PID.

Writer graph nodes require verified worktree custody and a reservation that
also fences ordinary dispatch. Cancellation persists admission fencing before
asking the run owner to stop attempts. Trusted graph gates are recorded process
groups; they are not a confinement boundary. Unknown launch evidence counts as
launched, so it cannot justify retrying a writer elsewhere.

The typed [Fabric surface](../specs/fabric.md#cli-and-mcp-surfaces) and code own
operation and field inventories. CLI calls remain a path to the same owners.

## Consequences

MCP exposes fan-out and checkpoint control without adding a daemon, provider
session database, transcript copy or delivery acceptance authority. Execution
and access boundaries remain those of
[ADR 0021](0021-configured-workspace-dispatch-boundaries.md).
