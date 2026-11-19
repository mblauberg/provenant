## Fixes

### Git and safety

- Shared-worktree lanes keep separate indexes and refs, so plain commits cannot include a sibling's staging and a new launch cannot reset it. Budget commits report the OID created by their own operation. Staged snapshots and stopped merges stay private while native convergence carries committed trees forward. Divergent host publication retains private work for recovery. (#3451)
