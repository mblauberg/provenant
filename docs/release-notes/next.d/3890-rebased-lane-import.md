## Fixes

### Lanes and settlement

- Import now accepts a lane that rebased its own branch onto a moved base and force-pushed it. When the lane's final head is the pushed upstream tip, settlement moves the host branch to that head with a rewritten-history warning instead of merging the displaced tip back into it; the displaced tip is retained at `refs/provenant/recovery/<oid>` in the same ref transaction, so a finished repair settles `succeeded` with the branch equal to the lane head. A host commit made during the lane is not the pushed tip and still refuses with a typed publication conflict. `lanes.history.rebase = "refuse"` retains the diverged history as before. (#3890)
