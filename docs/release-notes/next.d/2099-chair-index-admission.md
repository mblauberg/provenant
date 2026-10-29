## Fixes
### Lanes and settlement
Chair-index admission is now enforced inside `ChairIndex::inherit`: a versioned request's run ID must equal the host derivation from its operation, an unsupported version is refused, and a lane's unversioned request must match a retained host seal. Callers can no longer bind a lane-chosen run ID. Host-root continuations of top-level runs, such as `resume`, still inherit their chair.
