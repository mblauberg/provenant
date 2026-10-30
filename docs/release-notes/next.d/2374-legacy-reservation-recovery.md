## Fixes
### Lanes and settlement

- `reconcile` recovers writer lanes left settling behind reservations from older releases, preserving committed work and recovery evidence. Blocked settlement reports its reason, and repeated lane listings stop restarting workers after three failed recovery attempts; explicit reconciliation permits another attempt (#2374).
