# Next release

Every install's source digest changes once with this release. `check install`
reports existing installs as stale until they are reinstalled.

The source digest now covers shipped workspace source (each member's `src/`,
`assets/` and `build.rs`), manifests, the lockfile, the toolchain and Cargo
configuration, the release packager (`examples/package-release.rs`) and the
bundled assets. Edits under a member's `tests/`, `benches/` and other `examples/`
no longer change it.

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

## Behaviour changes

- `provenant output` on a terminal attempt that has no requested part now
  reports `NOT_FOUND` (exit 2). It used to report `NONTERMINAL` (exit 1)
  locally and `CONFLICT` over a peer. Over MCP the facade code is
  `not_found`. Active attempts still report `NONTERMINAL` (#1673).
