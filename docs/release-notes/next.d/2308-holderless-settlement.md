## Fixes
### Lanes and settlement

- `reconcile` now settles an attempt that is stuck in settlement with no live holder. When the guardian and provider are both gone in this boot, no durable exit was left and nothing holds the writer lock, the attempt ends `lost` with the reason "no live holder", and `cancel` of such an attempt now completes through the reconcile it already queues. Before, only a reboot proved this, so a crashed guardian left the attempt reported as settling and counted as an active lane (#2308).
