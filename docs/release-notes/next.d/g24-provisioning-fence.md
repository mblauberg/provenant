## Fixes
### Pipelines and graphs

- Graph relays retain unresolved writer checkout fences after a launch deadline expires, reconcile them on a fresh bounded attempt, and continue serving unrelated operations.
