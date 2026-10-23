# Fabric connection identity during the Rust rewrite

Fabric fixes the project from its server process's working directory. Before
a work claim, prove that the connection belongs to the intended repository or
registered worktree; never infer identity from a path alone.

The parser rewrite removes the legacy CLI identity route and implicit MCP
launch. `provenant fabric mcp` returns owner-unavailable until Fabric binds its
transport. No CLI fallback can establish a new connection or verify its
expected identity in this slice. Fail closed: report the connection unavailable
and do not claim, dispatch a writer or land through that connection.

When Fabric's owner lands, restore this procedure using its typed identity
request and the active connection's `fabric_whoami` response. Require matching
project and database identities before claiming; a copied checkout or inherited
Git redirect must not silently select another project.
