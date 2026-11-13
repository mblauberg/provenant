## Fixes
### Lanes and settlement

- `reconcile --abandon ATTEMPT --reason TEXT` can clear retained queued lanes that never dispatched, preserving their evidence and reason. Attempts with process evidence or writer custody remain protected (#3183).
