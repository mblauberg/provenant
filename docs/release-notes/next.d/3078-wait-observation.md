# Wait observation

## Behaviour changes

### Lanes and settlement

- `wait` shares native filesystem notifications across local graph targets on macOS and Linux. Peer and relay sources without an event subscription use the configured `domains.wait.local_interval_ms` fallback, which remains visible after settlement. (#3078)
- PR and CI observation use conditional requests and configurable fast/slow intervals; recent check-run durations determine when observation accelerates. Conditional PR wait rows report unknown review decisions; one-shot `pr status` retains its detailed review observation. (#3078)
