## Fixes

### Pipelines and graphs

- A writer-binding failure can no longer turn a node settled as skipped by its own `when` condition into a failed node with a dangling skip reason. The skipped disposition is authoritative at every binding site, and the transition that records a binding failure now preserves it, so the resulting checkpoint still validates on reload. (#4105)
- A checkpoint that decodes cleanly but fails semantic validation is reported once by `tick` and then quarantined by its bytes and plan, instead of repeating the same advance error on every tick. Changed bytes or a changed plan are reported again, and `provenant stop -- NAME` retires the record just as it does for a corrupt checkpoint. (#4105)
