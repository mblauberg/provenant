## Fixes

- The detached provider-update worker no longer fails on every run. Quota events
  launched it with `--project` set to their working directory, which the worker
  rejected whenever that directory was not a Git checkout, so it exited before
  writing any backoff record and was launched again on the next event. It now
  starts in that directory without selecting it as a project, launches are
  throttled to one per 15 minutes whatever the outcome, and a failed run keeps
  its one cause in `provider-updates/worker.json` under the state root.
