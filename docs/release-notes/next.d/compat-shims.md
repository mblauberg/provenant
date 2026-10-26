## Breaking changes

- `route list --json` no longer reports each pool member's `readiness` and
  `factor_basis_points`. Listing never probes, so they were always
  `unknown_availability` and 10000. Resolve and dispatch receipts still record
  both for each candidate.
- Fabric tasks and messages no longer accept the retired `legacy` and
  `legacy_output_path` members, even as null. A store row or peer reply that
  carries one fails to decode. Migration 7 already strips them from stored
  data, so this affects only a binary from before migration 7 still writing to
  the store, or such a peer. Upgrade every host together.
- Graph checkpoints must record `launched` and `launch_actor` for each
  attempt. A checkpoint without them, written before they existed, is refused
  instead of being treated as launched and resumed under the current actor.
- `hosts doctor HOST` requires the remote report's `peer_agent` field. A
  remote build too old to report it is refused as an invalid report.
