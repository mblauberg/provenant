# Fabric MCP registration

Status: awaiting Rust client cut-over

The parser rewrite removes the implicit no-argument MCP launch. Its legacy
registry editor (`scripts/configure-fabric-mcp.py`) and tests retire with that
route: the editor generated the removed invocation and could no longer create
a working registration. No registration or client configuration is changed by
this rewrite.

`provenant fabric mcp` is the explicit transport command. Until the Fabric
owner is connected, it exits 4 (`OWNER_UNAVAILABLE`); `--json` is invalid for
MCP execution and is rejected before stdin. An empty `provenant` invocation
prints usage and exits 2.

The config crate supplies release and managed-file APIs. Production install
composition, six-client MCP registration and hooks belong to client cut-over.
That work must record each client's real registration behavior in isolated
fixtures, preserve unrelated client configuration and register the explicit
transport. See the [Fabric design](../specs/rust-port/design/fabric.md) and
[state design](../specs/rust-port/design/state.md). No compatibility launcher
is provided.
