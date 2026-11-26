## Fixes

### Lanes and settlement

- `provenant upgrade --verify --host <peer>` no longer leaves the smoke read lane on a stale detached view. A remote reader now binds the peer clone's own checkout HEAD, and its first parent supplies the review base, instead of pinning a sender scratch commit the peer has never seen. The staged fixture still travels as bytes and is named by the brief. The upgrade smoke fixture now carries a fresh random fact that exists only in staged bytes, so a reader that reconstructs the answer from the checkout or the bundled template cannot pass the smoke read. (#4015)
