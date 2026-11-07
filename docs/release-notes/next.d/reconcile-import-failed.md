## Fixes
### Lanes and settlement

- `reconcile` settles failed lane Git imports after the guardian exits, preserving their evidence and retained objects. `status` and `output` warn that commits may not have reached the branch and show the head commit and retained object location when available. Busy records no longer hide progress on other records or remain in the report after settlement.
