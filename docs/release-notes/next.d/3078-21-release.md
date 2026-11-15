## Behaviour changes
### Lanes and settlement

- `wait` shares native filesystem notifications across local graph targets on macOS and Linux. Peer and relay sources without an event subscription use the configured `domains.wait.local_interval_ms` fallback, which remains visible after settlement. (#3078)
