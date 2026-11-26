## Fixes

### Lanes and settlement

- `provenant upgrade --verify --host <peer>` no longer leaves the smoke read lane on a stale detached view. A remote reader builds the same deterministic nonce fixture under its own home through the `smoke_fixture` protocol instead of pinning a sender scratch commit the peer has never seen, so a stale peer clone still reads a valid tree. The fixture carries a nonce line absent from the caller checkout and the bundled template, so a reader that reconstructs the answer from source cannot pass the smoke read; no caller path or staged bytes cross the wire. (#4015)
