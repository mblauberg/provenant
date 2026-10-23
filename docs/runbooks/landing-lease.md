# Landing lease during the Rust rewrite

Status: awaiting Fabric owner binding

The parser rewrite removes the `provenant lease` alias and its old exit-code
contract. There is no CLI forwarding route. Fabric's typed owner must supply
its lease subtree and shared store before repository scripts can acquire,
verify, push or release a lease through the rewritten binary.

Do not treat empty output or an unavailable owner as an acquired lease. A
repository landing tool that requires Fabric must refuse to land until its
owner binding is available. An existing MCP connection can use its own
`fabric_landing_lease` contract only after verifying the connection identity;
the rewritten binary does not yet create such a transport.

The [Fabric design](../specs/rust-port/design/fabric.md) owns lease semantics
and the later command grammar. This parser PR does not merge or push a shared
branch; it opens an implementation PR for review.
