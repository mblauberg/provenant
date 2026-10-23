# Landing lease

Fabric owns the landing lease: its semantics, store and command grammar are in
the [Fabric design](../specs/rust-port/design/fabric.md). The root
`provenant lease` synonym and its exit-code contract are gone, and no CLI route
forwards to another owner.

Never treat empty output or an owner-unavailable result (exit 4) as an acquired
lease. A landing tool that needs the lease refuses to land without one. Over
MCP, use `fabric_landing_lease` only after verifying the connection's identity,
as [Fabric connection identity](../../skills/orchestrate/references/fabric-identity.md)
describes.
