# Next release

Every install's source digest changes once with this release. `check install`
reports existing installs as stale until they are reinstalled.

The source digest now covers shipped workspace source (each member's `src/`,
`assets/` and `build.rs`), manifests, the lockfile, the toolchain and Cargo
configuration, the release packager (`examples/package-release.rs`) and the
bundled assets. Edits under a member's `tests/`, `benches/` and other `examples/`
no longer change it.

## Breaking changes

- Every peer request and reply envelope now carries integer `protocol: 2`.
  Health data also reports it. Calls use one SSH connection and reject any
  different version, naming both versions; upgrade peers together. Missing
  reply protocols and released peers' decode refusals identify protocol 1.
  There is no capability negotiation or support for older peers. Facade errors
  use distinct `not_terminal` and `operation_conflict` codes locally and on the
  peer wire, instead of `conflict` (#1631, #1571).
- The CLI reports an unavailable run owner as `OWNER_UNAVAILABLE`, replacing
  `UNSUPPORTED` so the refusal round-trips without changing its meaning.

## Behaviour changes

- Host entries report `protocol` only when known. Mismatched peers remain in
  the list with `available: false`, their protocol and `reason: "peer_version"`.
- `provenant output` on a terminal attempt that has no requested part now
  reports `NOT_FOUND` (exit 2). It used to report `NONTERMINAL` (exit 1)
  locally and `CONFLICT` over a peer. Over MCP the facade code is
  `not_found`. Active attempts still report `NONTERMINAL` (#1673).
