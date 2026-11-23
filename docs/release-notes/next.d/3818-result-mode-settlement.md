## Fixes
### Lanes and settlement

- Settlement no longer stalls when a lane leaves `result.md` group- or
  world-readable in its private attempt directory: settlement's own read
  tightens the regular file through its open descriptor, warns with the path
  and old mode, and settles. A symlinked or escaping result still refuses, the
  stalled reason names the remedy, and every other sealed read still refuses an
  over-broad mode rather than repairing it. (#3818)
