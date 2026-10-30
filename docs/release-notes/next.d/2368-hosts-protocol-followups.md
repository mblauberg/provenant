## Fixes
### Fabric and MCP

- `provenant hosts` no longer reports the local release's feature ceiling for
  a probed remote peer; the wire carries only the peer's base protocol fence,
  so `features` appears only on the local row (#2368).
- A protocol mismatch frame from a release that omits the expected protocol
  decodes without inventing one, and the message drops the "this host expects
  protocol N" clause instead of naming a level that peer never sent (#2368).
