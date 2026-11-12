## Behaviour changes
### Lanes and settlement

- Writers on trusted routes now run without the macOS sandbox, like the same CLI started by hand, so they share host build caches. Read-only lanes, lanes with a narrowed network, lanes submitted by a sandboxed lane and routes listed in `domains.run.sandboxed_routes` (the OpenCode Zen tier by default) stay sandboxed; `run --sandbox` / `--no-sandbox`, `attempt.sandbox` and the host `lanes.sandbox` knob override the rest. Lanes see `PROVENANT_SANDBOX=on|off`, `status` and receipts record the mode, hosted-cloud keys are stripped from every lane's environment, and `domains.run.sandbox_cache_reads` gives sandboxed lanes read-only host cache paths. See [lane sandbox mode](../../runbooks/configuration.md#lane-sandbox-mode) (#3246).
