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

Writer graph nodes take run's ordinary writer lock. Cancellation persists
admission fencing before asking the run owner to stop attempts. Unknown launch
evidence counts as launched, so it cannot justify retrying a writer elsewhere.

The typed [Fabric surface](../specs/fabric.md#cli-and-mcp-surfaces) and code own
operation and field inventories. CLI calls remain a path to the same owners.

## Consequences

MCP exposes fan-out and checkpoint control without adding a daemon, provider
session database, transcript copy or delivery acceptance authority. Execution
and access boundaries remain those of
[ADR 0021](0021-configured-workspace-dispatch-boundaries.md).

## Addendum: opt-in registration (epic #3078)

The facade stays, but no default session or lane carries it. Every tool has a
`provenant` CLI command, so a client with a shell loses nothing. The knob
`fabric.mcp_server` (default `false`) registers the stdio server for clients
without a shell:

- Install and upgrade write the owned `fabric` MCP entry only when the knob is
  on. When it is off they remove the entry they own, if it still matches the
  receipt value; an edited or foreign entry stays.
- Confined lanes pass the server to their provider only when the knob is on.
- `provenant fabric mcp` serves the same tools whenever a client starts it.
