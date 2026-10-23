# Fabric connection identity

Fabric fixes the project from its server process's working directory. Before
a work claim, prove that the connection belongs to the intended repository or
registered worktree: the active connection's `fabric_whoami` project and
database identities must match the expected ones. Never infer identity from a
path alone; a copied checkout or inherited Git redirect must not silently
select another project.

If identity cannot be verified, including when `provenant fabric mcp` reports
owner-unavailable, fail closed: do not claim, dispatch a writer or land through
that connection.
