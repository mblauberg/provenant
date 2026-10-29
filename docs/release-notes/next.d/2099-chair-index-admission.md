## Fixes
### Lanes and settlement
Chair-index admission is now enforced inside `ChairIndex::inherit`: a new association needs a host-derived run ID, or an unversioned request must match a retained host seal. Callers can no longer bind a lane-chosen run ID.
