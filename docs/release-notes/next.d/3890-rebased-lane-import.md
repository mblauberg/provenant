## Fixes

### Lanes and settlement

- A lane that rebases its branch can settle with a rewritten-history warning when the host branch still equals its pinned launch tip, including after a private force-push leaves the host tracking ref stale. The displaced tip is retained at `refs/provenant/recovery/<oid>`. A host tip advanced during the lane is merged or refused, even when a remote-tracking ref matches the lane head. `lanes.history.rebase = "refuse"` retains rewritten or diverged history instead. (#3890)
