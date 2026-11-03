## Fixes
### Pipelines and graphs

- `fabric graph cancel --reason` records why a graph was cancelled, and cancelled graphs no longer show settled members as pending or running (#2736).
- Cancelling a graph member on a remote host, and `lanes --wait`, now follow a policy retry chain to its live tail, so a successor queued during the lookup is no longer missed (#2736).
