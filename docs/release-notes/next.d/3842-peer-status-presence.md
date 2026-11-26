## Fixes

### Lanes and settlement

- A review placed on a peer no longer reports `unknown` on the launching host while its writer is live. The peer relays its own census-observed presence with the status result, so the launching host reports the same state instead of treating fresh output as a heartbeat or guessing from event age. A peer-owned lane's reclaim hint now names the peer's graph target and the host where `reconcile` resolves it, and target refusals no longer print `NOT_FOUND: NOT_FOUND:`. (#3842)
