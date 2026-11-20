## Changes

### Lanes and settlement

- `provenant gc` now reports each Cargo target's reclaimable bytes as APFS private bytes -- the blocks no other clone shares -- where the filesystem can report them. Seeded worktree targets are clones that share blocks with their seed, so their allocated figure overstated what removal would free. The private figure is measured inside the existing target census walk, so it stays within `gc.sweep_budget`, resumes with the census cursor and adds no separate full-tree pass. It feeds both the reported reclaimable totals and the budget arithmetic, while eviction remains least-recently-used. The budget compares the summed private figure with the configured byte cap, so it can understate volume occupancy where clones share blocks. (#3595)
- Where private bytes are unavailable, GC says so, reports allocated bytes instead and never reports zero. The human and JSON summaries name which figure they carry. (#3595)
