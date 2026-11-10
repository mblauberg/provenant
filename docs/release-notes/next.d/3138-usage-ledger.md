## Breaking changes
### Telemetry and providers

- **Lane usage now lives in a usage ledger that outlives the run store.** After installing 1.8.0, run `provenant telemetry backfill` once on each host, before any later release removes run history; running it again adds nothing. Settlements record their usage in the ledger from now on. (#3138)
