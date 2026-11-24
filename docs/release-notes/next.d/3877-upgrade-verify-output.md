## Fixes

### CLI and install

- `provenant upgrade --verify` now names each failing host once in the `UPGRADE_FAILED` message with a bounded, one-line reason, and leads the human view with one `upgraded` / `verified` / `failed - reason` line per host. The verify child is run with `--json`, so a failed host shows its innermost error code and message instead of nested `Command failed` wrappers, and the smoke host prints its graph verdict instead of raw envelopes; repeated capability notices collapse to a count unless `--full` is given. On failure the renderer drops human text, so run `provenant upgrade --verify --json` for the full per-host detail. Running the verification from outside a Git checkout no longer fails the smoke host just because the selected project is not a checkout. (#3877)
