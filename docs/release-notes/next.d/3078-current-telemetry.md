## Breaking changes
### Telemetry and providers

- **Telemetry requires a fresh usage ledger and native-session ingest store.** The format-1 reset must discard the previous ledger, ingest segments and ingest cursors; later ingest requests can replay the selected native transcripts. (#3078)
