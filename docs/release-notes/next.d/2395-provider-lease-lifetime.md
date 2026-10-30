## Fixes

- Provider updates no longer wait for running lanes to finish. Dispatch waits for a busy provider only up to `--wait`, capped at 30 seconds, before proceeding with the installed version. ([#2395](https://github.com/mblauberg/provenant/issues/2395))
