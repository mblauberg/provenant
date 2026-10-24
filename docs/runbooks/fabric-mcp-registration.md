# Fabric MCP registration

`provenant fabric mcp --client NAME` is the explicit stdio transport. Supported
client values are `claude`, `codex`, `cursor`, `agy` and `opencode`.
`--json` is refused before stdin. A bare `provenant` prints usage and exits 2,
so a client registration must name `fabric mcp`.

Installation registers the transport for each selected harness and preserves
unrelated client configuration; there is no separate registry editor. See the
[state design](../specs/rust-port/design/state.md) and the
[Fabric design](../specs/rust-port/design/fabric.md).
