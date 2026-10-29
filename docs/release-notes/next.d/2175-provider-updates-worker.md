## Fixes
### Telemetry and providers

- The detached provider-update worker no longer fails on every run when launched from a directory that is not a Git checkout. Launches are now throttled to one per 15 minutes, and a failed run keeps its one cause in `provider-updates/worker.json` under the state root (#2175).
