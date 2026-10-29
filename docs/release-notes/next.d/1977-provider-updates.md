## Additions
### Telemetry and providers

- Idle provider CLIs update automatically, started by quota events. Updates can be turned off per provider, the update command can be overridden, and the interval (default 24 hours) and timeout (default 300 seconds) are configurable (#1977).
- A provider is never updated while its lanes run, and a busy provider does not delay lanes that have other candidates. When every candidate is busy, lane preparation waits at most 30 seconds and honours cancellation (#1977).
