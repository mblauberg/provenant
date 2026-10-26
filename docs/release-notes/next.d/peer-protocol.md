## Breaking changes

- Every peer request and reply envelope now carries integer `protocol`.
  The host listing reports the protocol observed in the reply envelope. Calls
  use one SSH connection and reject any different version, naming both
  versions; upgrade peers together. Missing reply protocols and released
  peers' decode refusals identify protocol 1. There is no capability
  negotiation or support for older peers. Facade errors use distinct
  `not_terminal` and `operation_conflict` codes locally and on the peer wire,
  instead of `conflict` (#1631, #1571).
- The CLI reports an unavailable run owner as `OWNER_UNAVAILABLE`, replacing
  `UNSUPPORTED` so the refusal round-trips without changing its meaning.
