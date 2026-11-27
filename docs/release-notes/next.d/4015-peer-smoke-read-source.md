## Fixes

### Lanes and settlement

- The `provenant upgrade --verify --verify-host <peer>` smoke reader no longer reads the peer checkout's own `HEAD`, as 2.10.0 did. The peer builds the same deterministic nonce fixture under its own home through the `smoke_fixture` protocol and reads that, so a stale peer clone still reads a valid tree. The nonce line is absent from the caller checkout and the bundled template, so a reader that reconstructs the answer from source cannot pass; no caller path or staged bytes cross the wire. (#4015)
