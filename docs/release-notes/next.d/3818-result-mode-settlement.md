## Fixes
### Lanes and settlement

- Settlement no longer stalls when a lane leaves `result.md` group- or
  world-readable in its private attempt directory: it tightens the regular
  file through its open descriptor, warns with the path and old mode, and
  settles. A symlinked or escaping result still refuses, and the stalled reason
  names the remedy. (#3818)
