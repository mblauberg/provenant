## Fixes

### Telemetry and providers

- `provenant reconcile --abandon` now settles cloud records created before source pinning, which have no commit and no saved job, and keeps the existing evidence and remote-session warning.
- A cloud submission the provider definitely refused now redacts credentials from the saved failure detail, as the uncertain path already did.
