## Fixes
### Lanes and settlement

- Settlement no longer stalls when a lane leaves `result.md`, the `handoff.json`
  snapshot or its progress handoff group- or world-readable in its private
  attempt directory: settlement's own read tightens each provider-written
  settlement input through its open descriptor, warns with the path and old
  mode, and settles. The inputs are a class on `AttemptFile`, not one hard-coded
  name, so another provider-written input is covered by declaring it there.
  Host-written private records and later presentation reads still refuse an
  over-broad mode rather than repairing it. A symlinked or escaping input still
  refuses, and the stalled reason names the remedy. (#3818)
