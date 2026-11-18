## Fixes

### Pipelines and graphs

- Preserve queued attempts across deferred launch retries and keep repository refusals distinct from I/O failures. #3393
- Reconcile settled attempts before their preparation retry wake without restarting queued work. #3393
